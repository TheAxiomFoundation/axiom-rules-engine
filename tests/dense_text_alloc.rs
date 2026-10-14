//! Dense execution never copies a text value once per row.
//!
//! A text column holds one value per row, and dense used to copy the value's
//! heap data for every row it filled: a broadcast literal, a root value
//! projected to each of its related rows, a parameter lookup, each read of an
//! input or cached rule, and each row an `if` or a cached stretch merged. The
//! row buffers were reserved fallibly (#256), but each copy was an ordinary
//! allocation. A batch stating N related rows, whose 8-byte owners fit, over a
//! root text of L bytes asked for about N×L bytes in small allocations: a
//! refused one aborted the process, and on a host that overcommits memory the
//! rest could exhaust it.
//!
//! These cases run in their own binary, under the allocator in
//! `tests/support/guarded_alloc.rs`, with texts far longer than any buffer
//! their small batches need:
//!
//! - a cap below a text's length refuses any copy of it, so a regression
//!   aborts this binary;
//! - a numbered refusal refuses each allocation at least as long as the cap
//!   in turn, so every one must be fallible, not only the first.
//!
//! Invariants, checked by the examples and properties below:
//!
//! 1. **Sharing.** For every generated program and small batch, execution
//!    makes no allocation as large as the cap unless its result is an error
//!    that names a long text, and under the cap it returns exactly what it
//!    returns uncapped.
//! 2. **Refusal.** An error naming a long text copies it fallibly: under the
//!    cap, or with any one of its large allocations refused, execution returns
//!    the uncapped result or the refusal naming one of the batch's counts. It
//!    never aborts.
//! 3. **Differential.** When both numeric modes succeed, their text outputs
//!    are equal: text never depends on the numeric mode.

use std::collections::{BTreeMap, HashMap};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::ProgramSpec;
use chrono::NaiveDate;
use proptest::prelude::*;
use proptest::sample::{Index, select};
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};

#[path = "support/guarded_alloc.rs"]
mod guarded_alloc;

use guarded_alloc::{Guarded, capped, refusing_large, serial};

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

/// The cap, and the threshold of a large allocation. Every row buffer of the
/// batches here fits well under it.
const CAP: usize = 1 << 14;

/// The length of a long text: four times the cap.
const LONG: usize = 1 << 16;

/// A text of at least `LONG` bytes starting with `seed`. Its quotes,
/// backslashes, newlines and control characters make an error's rendering of
/// it longer than the text.
fn long_text(seed: char) -> String {
    let unit = format!("{seed}\"\\\n\u{1}é");
    unit.repeat(LONG / unit.len() + 1)
}

/// The texts a batch or program draws from: two short and two long.
fn texts() -> [String; 4] {
    [
        "a".to_string(),
        "b".to_string(),
        long_text('x'),
        long_text('y'),
    ]
}

/// A text column. Each row holds its own copy of its value.
fn text_column(values: &[String]) -> DenseColumn {
    DenseColumn::Text(values.iter().map(|value| value.as_str().into()).collect())
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// The count a refusal names, or `None` for any other result.
fn refused_count(error: &EvalError) -> Option<usize> {
    let EvalError::TypeMismatch(message) = error else {
        return None;
    };
    let head = message.strip_suffix(" rows, more than this process can allocate")?;
    let head = head.strip_suffix(" related").unwrap_or(head);
    head.rsplit(' ').next()?.parse().ok()
}

/// Whether `result` is an error naming a long text.
fn names_long_text(result: &Result<DenseExecutionResult, EvalError>) -> bool {
    matches!(result, Err(error) if error.to_string().len() >= LONG)
}

// ---------------------------------------------------------------------------
// Programs, written as compiled-program JSON on a `Household` root
// ---------------------------------------------------------------------------

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn text(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "text", "value": value } })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn input_or_else(name: &str, default: &str) -> Value {
    json!({ "kind": "input_or_else", "name": name, "default": { "kind": "text", "value": default } })
}

fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn if_then_else(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr })
}

/// A `match` with no arm for `subject`: dense compiles one only as an `if`'s
/// `else`.
fn no_match(subject: Value, patterns: Vec<Value>) -> Value {
    json!({ "kind": "no_match", "subject": subject, "patterns": patterns })
}

fn lookup(parameter: &str, index: Value) -> Value {
    json!({ "kind": "parameter_lookup", "parameter": parameter, "index": index })
}

fn sum_over_periods(value: Value) -> Value {
    json!({ "kind": "over_periods", "over": "sum", "value": value })
}

fn add(items: Vec<Value>) -> Value {
    json!({ "kind": "add", "items": items })
}

fn count_related(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1 });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn scalar_rule(name: &str, dtype: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": "Household", "dtype": dtype, "unit": null, "semantics": "scalar", "expr": expr })
}

fn judgment_rule(name: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": "Household", "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

/// A program over `rules`. With `chosen`, it relates households to people:
/// `member` is the base relation and `chosen` the members its predicate holds
/// for. A derived relation's predicate can read the household's own rules,
/// which it evaluates on the household's rows and projects to its members.
/// The text table `t` holds a long text at key 2.
fn program(rules: Vec<Value>, chosen: Option<Value>) -> Value {
    let relations = match chosen {
        Some(chosen) => {
            let pair = json!(["Household", "Person"]);
            json!([
                { "name": "member", "arity": 2, "slot_entities": pair },
                {
                    "name": "chosen", "arity": 2, "slot_entities": pair,
                    "derivation": {
                        "source_relation": "member", "current_slot": 0, "related_slot": 1,
                        "slot_entities": pair, "predicate": chosen,
                    },
                },
            ])
        }
        None => json!([]),
    };
    let [short_a, short_b, long_x, _] = texts();
    json!({
        "units": [],
        "relations": relations,
        "parameters": [
            { "name": "t", "unit": null, "versions": [{ "effective_from": "2000-01-01", "values": {
                "0": { "kind": "text", "value": short_a },
                "1": { "kind": "text", "value": short_b },
                "2": { "kind": "text", "value": long_x },
            } }] },
        ],
        "derived": rules,
    })
}

fn compile(program: &Value) -> Result<DenseCompiledProgram, String> {
    let spec: ProgramSpec =
        serde_json::from_value(program.clone()).map_err(|error| error.to_string())?;
    let artifact = CompiledProgramArtifact::compile(spec).map_err(|error| error.to_string())?;
    DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .map_err(|error| error.to_string())
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("date")
}

fn month() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: day("2026-01-01"),
        end: day("2026-01-31"),
    }
}

fn years(count: usize) -> Vec<Period> {
    (0..count)
        .map(|index| {
            let year = 2024 + i32::try_from(index).expect("few periods");
            Period {
                kind: PeriodKind::TaxYear,
                start: NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
                end: NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
            }
        })
        .collect()
}

fn member_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member".to_string(),
        current_slot: 0,
        related_slot: 1,
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Decimal,
    Float,
}

const MODES: [Mode; 2] = [Mode::Decimal, Mode::Float];

fn execute(
    program: &DenseCompiledProgram,
    mode: Mode,
    batch: DenseBatchSpec,
    outputs: &[String],
) -> Result<DenseExecutionResult, EvalError> {
    match mode {
        Mode::Decimal => program.execute(&month(), batch, outputs),
        Mode::Float => program.execute_f64(&month(), batch, outputs),
    }
}

fn execute_lifetime(
    program: &DenseCompiledProgram,
    mode: Mode,
    batches: Vec<DenseBatchSpec>,
    outputs: &[String],
) -> Result<DenseExecutionResult, EvalError> {
    let periods = years(batches.len());
    match mode {
        Mode::Decimal => program.execute_lifetime(&periods, batches, outputs),
        Mode::Float => program.execute_lifetime_f64(&periods, batches, outputs),
    }
}

/// A result in comparable form: each output's values, or the error.
fn summary(
    result: &Result<DenseExecutionResult, EvalError>,
) -> Result<BTreeMap<String, String>, String> {
    match result {
        Ok(result) => Ok(result
            .outputs
            .iter()
            .map(|(name, value)| (name.clone(), format!("{value:?}")))
            .collect()),
        Err(error) => Err(format!("{error:?}")),
    }
}

fn names(rules: &[Value]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| rule["name"].as_str().expect("rule name").to_string())
        .collect()
}

/// `rows` households with the root inputs `name` (text) and `a` (integer).
fn root_batch(names: &[String], a: &[i64]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: names.len(),
        inputs: HashMap::from([
            ("name".to_string(), text_column(names)),
            ("a".to_string(), DenseColumn::Integer(a.to_vec())),
        ]),
        relations: HashMap::new(),
    }
}

/// One household named `name`, stating `related` members and supplying no
/// related column.
fn stated_members(name: &str, related: usize) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([("name".to_string(), text_column(&[name.to_string()]))]),
        relations: HashMap::from([(
            member_key(),
            DenseRelationBatchSpec {
                offsets: vec![0, related],
                inputs: HashMap::new(),
                related_row_count: Some(related),
            },
        )]),
    }
}

// ---------------------------------------------------------------------------
// Each place dense filled a text column
// ---------------------------------------------------------------------------

/// Runs `batch` through `program` uncapped and under the cap, in both modes,
/// and checks the capped result is the uncapped one, or, when the uncapped
/// result is an error naming a long text, the refusal naming `count`. Returns
/// the uncapped results.
fn shared_under_cap(
    case: &str,
    program: &DenseCompiledProgram,
    batch: impl Fn() -> DenseBatchSpec,
    outputs: &[&str],
    count: usize,
) -> Vec<Result<DenseExecutionResult, EvalError>> {
    let outputs: Vec<String> = outputs.iter().map(|output| output.to_string()).collect();
    MODES
        .into_iter()
        .map(|mode| {
            let label = format!("{case}, {mode:?}");
            let uncapped = execute(program, mode, batch(), &outputs);
            let input = batch();
            let result = capped(CAP, || execute(program, mode, input, &outputs));
            match &result {
                Err(error) if names_long_text(&uncapped) && refused_count(error).is_some() => {
                    assert_eq!(refused_count(error), Some(count), "{label}: {error:?}");
                }
                _ => assert_eq!(summary(&result), summary(&uncapped), "{label}"),
            }
            uncapped
        })
        .collect()
}

/// The text output `output` of `result`.
fn text_output(result: &Result<DenseExecutionResult, EvalError>, output: &str) -> Vec<String> {
    match result {
        Ok(result) => match result.outputs.get(output) {
            Some(DenseOutputValue::Scalar(DenseColumn::Text(values))) => {
                values.iter().map(|value| value.to_string()).collect()
            }
            other => panic!("{output} is not a text column: {other:?}"),
        },
        Err(error) => panic!("{output} failed: {error:?}"),
    }
}

#[test]
fn a_long_text_literal_is_broadcast_without_a_copy_per_row() {
    let _serial = serial();
    let long = long_text('x');
    let program = compile(&program(
        vec![scalar_rule("out", "text", text(&long))],
        None,
    ))
    .expect("program compiles");
    let rows = 256;
    let batch = || DenseBatchSpec {
        row_count: rows,
        inputs: HashMap::new(),
        relations: HashMap::new(),
    };
    for uncapped in shared_under_cap("text literal", &program, batch, &["out"], rows) {
        assert_eq!(text_output(&uncapped, "out"), vec![long.clone(); rows]);
    }
}

#[test]
fn a_long_root_text_is_projected_to_stated_related_rows_without_a_copy_per_row() {
    let _serial = serial();
    // 512 members whose 8-byte owners and 16- to 24-byte text slots fit under
    // the cap, over a root text four times the cap.
    let related = 512;
    let long = long_text('x');
    for (case, label) in [
        ("root text input", input("name")),
        ("root text literal", text(&long)),
    ] {
        let program = compile(&program(
            vec![
                scalar_rule("label", "text", label),
                scalar_rule("out", "integer", count_related("chosen", None)),
            ],
            Some(compare(derived("label"), "eq", text(&long))),
        ))
        .expect("program compiles");
        for uncapped in shared_under_cap(
            case,
            &program,
            || stated_members(&long, related),
            &["out"],
            related,
        ) {
            assert_eq!(
                summary(&uncapped).expect("counts")["out"],
                format!(
                    "{:?}",
                    DenseOutputValue::Scalar(DenseColumn::Integer(vec![512]))
                )
            );
        }
    }
}

#[test]
fn a_long_text_parameter_is_looked_up_without_a_copy_per_row() {
    let _serial = serial();
    let program = compile(&program(
        vec![scalar_rule("out", "text", lookup("t", input("a")))],
        None,
    ))
    .expect("program compiles");
    let rows = 256;
    let keys: Vec<i64> = (0..rows).map(|row| [2, 0, 2, 1][row % 4]).collect();
    let names = vec!["a".to_string(); rows];
    for uncapped in shared_under_cap(
        "text parameter",
        &program,
        || root_batch(&names, &keys),
        &["out"],
        rows,
    ) {
        let [short_a, short_b, long_x, _] = texts();
        let expected: Vec<String> = keys
            .iter()
            .map(|key| match key {
                0 => short_a.clone(),
                1 => short_b.clone(),
                _ => long_x.clone(),
            })
            .collect();
        assert_eq!(text_output(&uncapped, "out"), expected);
    }
}

#[test]
fn long_texts_are_read_selected_and_merged_without_a_copy_per_row() {
    let _serial = serial();
    let [_, _, long_x, long_y] = texts();
    // `label` copies the input on every read; `picked` selects between a
    // cached rule and a literal; `late` reads `picked` under a partial mask
    // first and every row after, so its cached stretches merge.
    let program = compile(&program(
        vec![
            scalar_rule("label", "text", input("name")),
            scalar_rule(
                "picked",
                "text",
                if_then_else(
                    compare(input("a"), "gt", integer(0)),
                    derived("label"),
                    text(&long_y),
                ),
            ),
            scalar_rule(
                "early",
                "text",
                if_then_else(
                    compare(input("a"), "gt", integer(1)),
                    derived("picked"),
                    input_or_else("missing", &long_y),
                ),
            ),
            scalar_rule("late", "text", derived("picked")),
        ],
        None,
    ))
    .expect("program compiles");
    let rows = 96;
    let a: Vec<i64> = (0..rows).map(|row| (row % 3) as i64).collect();
    let names: Vec<String> = (0..rows)
        .map(|row| {
            if row % 2 == 0 {
                long_x.clone()
            } else {
                "b".to_string()
            }
        })
        .collect();
    // `early` first, so `late` extends the stretch of `picked` it computed.
    let outputs = ["early", "late", "label", "picked"];
    for uncapped in shared_under_cap(
        "read, select, merge",
        &program,
        || root_batch(&names, &a),
        &outputs,
        rows,
    ) {
        let picked: Vec<String> = (0..rows)
            .map(|row| {
                if a[row] > 0 {
                    names[row].clone()
                } else {
                    long_y.clone()
                }
            })
            .collect();
        let early: Vec<String> = (0..rows)
            .map(|row| {
                if a[row] > 1 {
                    picked[row].clone()
                } else {
                    long_y.clone()
                }
            })
            .collect();
        assert_eq!(text_output(&uncapped, "label"), names);
        assert_eq!(text_output(&uncapped, "picked"), picked);
        assert_eq!(text_output(&uncapped, "early"), early);
        assert_eq!(text_output(&uncapped, "late"), picked);
    }
}

#[test]
fn a_match_on_a_long_text_names_it_fallibly() {
    let _serial = serial();
    let long = long_text('x');
    // Every row reaches the `match`, and none of its arms covers the subject:
    // each row fails naming the long text.
    for (case, subject, rules) in [
        ("literal subject", text(&long), vec![]),
        ("input subject", input("name"), vec![]),
        (
            "cached subject",
            derived("label"),
            vec![scalar_rule("label", "text", input("name"))],
        ),
    ] {
        let mut rules = rules;
        rules.push(scalar_rule(
            "out",
            "integer",
            if_then_else(
                compare(input("a"), "gt", integer(5)),
                integer(1),
                no_match(subject, vec![text("a"), text("b")]),
            ),
        ));
        let program = compile(&program(rules, None)).expect("program compiles");
        let rows = 64;
        let names = vec![long.clone(); rows];
        let a = vec![0; rows];
        for uncapped in shared_under_cap(case, &program, || root_batch(&names, &a), &["out"], rows)
        {
            match uncapped {
                Err(EvalError::NoMatchingArm { rule, value, .. }) => {
                    assert_eq!(rule, "out", "{case}");
                    assert_eq!(value, format!("{long:?}"), "{case}");
                }
                other => panic!("{case}: expected the match failure, got {other:?}"),
            }
        }
    }
}

#[test]
fn a_match_on_a_long_related_text_names_it_fallibly() {
    let _serial = serial();
    let long = long_text('x');
    // The derived relation's predicate matches the projected root text, and
    // no arm covers it: every member fails, and the rule counting the
    // relation names the failure.
    let program = compile(&program(
        vec![
            scalar_rule("label", "text", input("name")),
            scalar_rule("out", "integer", count_related("chosen", None)),
        ],
        Some(compare(
            if_then_else(
                compare(derived("label"), "eq", text("a")),
                integer(1),
                no_match(derived("label"), vec![text("a"), text("b")]),
            ),
            "eq",
            integer(1),
        )),
    ))
    .expect("program compiles");
    let related = 512;
    for uncapped in shared_under_cap(
        "related match",
        &program,
        || stated_members(&long, related),
        &["out"],
        related,
    ) {
        match uncapped {
            Err(EvalError::NoMatchingArm { rule, value, .. }) => {
                assert_eq!(rule, "out");
                assert_eq!(value, format!("{long:?}"));
            }
            other => panic!("expected the match failure, got {other:?}"),
        }
    }
}

#[test]
fn a_long_period_varying_text_input_is_named_fallibly() {
    let _serial = serial();
    let [_, _, long_x, long_y] = texts();
    let program = compile(&program(
        vec![scalar_rule(
            "life",
            "decimal",
            add(vec![
                sum_over_periods(integer(5)),
                if_then_else(
                    compare(input("name"), "eq", text(&long_x)),
                    integer(1),
                    integer(0),
                ),
            ]),
        )],
        None,
    ))
    .expect("program compiles");
    let rows = 64;
    let outputs = vec!["life".to_string()];
    for (case, second, varies) in [
        ("invariant", long_x.clone(), false),
        ("varying", long_y.clone(), true),
    ] {
        let batches = || {
            vec![
                root_batch(&vec![long_x.clone(); rows], &vec![0; rows]),
                root_batch(&vec![second.clone(); rows], &vec![0; rows]),
            ]
        };
        for mode in MODES {
            let label = format!("{case}, {mode:?}");
            let uncapped = execute_lifetime(&program, mode, batches(), &outputs);
            let input = batches();
            let result = capped(CAP, || execute_lifetime(&program, mode, input, &outputs));
            if varies {
                match &uncapped {
                    Err(EvalError::LifetimePeriodVaryingInput {
                        first_value,
                        second_value,
                        ..
                    }) => {
                        assert_eq!(first_value, &format!("{long_x:?}"), "{label}");
                        assert_eq!(second_value, &format!("{long_y:?}"), "{label}");
                    }
                    other => panic!("{label}: expected the varying input, got {other:?}"),
                }
                match &result {
                    Err(error) => assert_eq!(refused_count(error), Some(rows), "{label}"),
                    Ok(_) => panic!("{label}: the long labels fit under the cap"),
                }
            } else {
                assert!(uncapped.is_ok(), "{label}: {uncapped:?}");
                assert_eq!(summary(&result), summary(&uncapped), "{label}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Generated programs
// ---------------------------------------------------------------------------

/// The earlier rules a generated formula may read, by kind.
#[derive(Clone, Debug, Default)]
struct Scope {
    texts: Vec<String>,
    judgments: Vec<String>,
}

fn union(strategies: Vec<BoxedStrategy<Value>>) -> BoxedStrategy<Value> {
    proptest::strategy::Union::new(strategies).boxed()
}

fn literal() -> BoxedStrategy<Value> {
    select(texts().to_vec())
        .prop_map(|value| text(&value))
        .boxed()
}

fn equality() -> impl Strategy<Value = &'static str> {
    select(vec!["eq", "ne"])
}

/// A text on the household's rows.
fn text_expr(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        literal(),
        Just(input("name")).boxed(),
        select(texts().to_vec())
            .prop_map(|default| input_or_else("missing", &default))
            .boxed(),
        (0_i64..=2)
            .prop_map(|key| lookup("t", integer(key)))
            .boxed(),
        Just(lookup("t", input("a"))).boxed(),
    ];
    if !scope.texts.is_empty() {
        leaves.push(
            select(scope.texts.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = text_expr(scope, depth - 1);
    let condition = judgment(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        2 => (condition.clone(), inner.clone(), inner.clone())
            .prop_map(|(condition, then_expr, else_expr)| if_then_else(condition, then_expr, else_expr)),
        1 => (condition, inner.clone(), inner, prop::collection::vec(literal(), 1..=2))
            .prop_map(|(condition, then_expr, subject, patterns)| {
                if_then_else(condition, then_expr, no_match(subject, patterns))
            }),
    ]
    .boxed()
}

fn judgment(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        (text_expr(scope, 0), equality(), text_expr(scope, 0))
            .prop_map(|(left, op, right)| compare(left, op, right))
            .boxed(),
        (select(vec!["lt", "gt", "eq"]), 0_i64..=2)
            .prop_map(|(op, value)| compare(input("a"), op, integer(value)))
            .boxed(),
    ];
    if !scope.judgments.is_empty() {
        leaves.push(
            select(scope.judgments.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = judgment(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        1 => (select(vec!["and", "or"]), prop::collection::vec(inner.clone(), 1..=2))
            .prop_map(|(kind, items)| json!({ "kind": kind, "items": items })),
        1 => inner.prop_map(|item| json!({ "kind": "not", "item": item })),
    ]
    .boxed()
}

/// A number that may fail a row on a text `match`, or count the members a
/// text predicate keeps.
fn number(scope: &Scope, relations: bool) -> BoxedStrategy<Value> {
    let mut choices = vec![
        (
            judgment(scope, 1),
            text_expr(scope, 1),
            prop::collection::vec(literal(), 1..=2),
        )
            .prop_map(|(condition, subject, patterns)| {
                if_then_else(condition, integer(1), no_match(subject, patterns))
            })
            .boxed(),
        (judgment(scope, 1), 0_i64..=3, 0_i64..=3)
            .prop_map(|(condition, then_value, else_value)| {
                if_then_else(condition, integer(then_value), integer(else_value))
            })
            .boxed(),
    ];
    if relations {
        let nickname =
            (equality(), literal()).prop_map(|(op, value)| compare(input("nickname"), op, value));
        choices.push(
            (select(vec!["member", "chosen"]), prop::option::of(nickname))
                .prop_map(|(relation, where_clause)| count_related(relation, where_clause))
                .boxed(),
        );
    }
    union(choices)
}

/// A related-row predicate for `chosen`: the projected root text or
/// judgment, or the member's own text.
fn chosen(scope: &Scope) -> BoxedStrategy<Value> {
    let texts = scope.texts.clone();
    prop_oneof![
        (select(texts.clone()), equality(), literal()).prop_map(|(name, op, value)| compare(
            derived(&name),
            op,
            value
        )),
        (equality(), literal()).prop_map(|(op, value)| compare(input("nickname"), op, value)),
        (select(texts), prop::collection::vec(literal(), 1..=2)).prop_map(|(name, patterns)| {
            compare(
                if_then_else(
                    compare(derived(&name), "eq", text("a")),
                    integer(1),
                    no_match(derived(&name), patterns),
                ),
                "eq",
                integer(1),
            )
        }),
        Just(derived("hj")),
    ]
    .boxed()
}

/// A program's rules and, when it has relations, the predicate of `chosen`.
/// `h` (text) and `hj` (judgment) read no relation, so `chosen`'s predicate
/// can read them on the household's rows. Then the text `r0`, the judgment
/// `r1` and the number `r2`, which may count a relation.
fn rules(relations: bool) -> impl Strategy<Value = (Vec<Value>, Option<Value>)> {
    let helpers = Scope {
        texts: vec!["h".to_string()],
        judgments: vec!["hj".to_string()],
    };
    let expressions = (
        text_expr(&Scope::default(), 2),
        judgment(
            &Scope {
                texts: vec!["h".to_string()],
                ..Scope::default()
            },
            1,
        ),
        text_expr(&helpers, 2),
        judgment(
            &Scope {
                texts: vec!["h".to_string(), "r0".to_string()],
                judgments: vec!["hj".to_string()],
            },
            2,
        ),
        number(
            &Scope {
                texts: vec!["h".to_string(), "r0".to_string()],
                judgments: vec!["hj".to_string(), "r1".to_string()],
            },
            relations,
        ),
    );
    let chosen = if relations {
        chosen(&helpers).prop_map(Some).boxed()
    } else {
        Just(None).boxed()
    };
    (expressions, chosen).prop_map(|((h, hj, r0, r1, r2), chosen)| {
        let rules = vec![
            scalar_rule("h", "text", h),
            judgment_rule("hj", hj),
            scalar_rule("r0", "text", r0),
            judgment_rule("r1", r1),
            scalar_rule("r2", "integer", r2),
        ];
        (rules, chosen)
    })
}

/// A lifetime output: a reduction beside the household's text, which binds
/// only when it is the same in every period.
fn lifetime_rules() -> impl Strategy<Value = Vec<Value>> {
    (equality(), literal()).prop_map(|(op, value)| {
        vec![scalar_rule(
            "life",
            "decimal",
            add(vec![
                sum_over_periods(integer(5)),
                if_then_else(compare(input("name"), op, value), integer(1), integer(0)),
            ]),
        )]
    })
}

// ---------------------------------------------------------------------------
// Generated batches
// ---------------------------------------------------------------------------

fn text_values(rows: usize) -> BoxedStrategy<Vec<String>> {
    prop_oneof![
        // Every row the same text.
        select(texts().to_vec()).prop_map(move |value| vec![value; rows]),
        prop::collection::vec(select(texts().to_vec()), rows),
    ]
    .boxed()
}

fn root_rows() -> impl Strategy<Value = usize> {
    prop_oneof![0_usize..=4, 5_usize..=64]
}

/// Root columns: `name`, present or not, and `a`.
fn root_columns(rows: usize) -> BoxedStrategy<HashMap<String, DenseColumn>> {
    (
        prop::option::weighted(0.85, text_values(rows)),
        prop::collection::vec(0_i64..=2, rows),
    )
        .prop_map(|(names, a)| {
            let mut columns = HashMap::from([("a".to_string(), DenseColumn::Integer(a))]);
            if let Some(names) = names {
                columns.insert("name".to_string(), text_column(&names));
            }
            columns
        })
        .boxed()
}

/// A batch of households, each with up to six members, whose nicknames are
/// supplied or not. Without them the related count is stated.
fn batch(relations: bool) -> impl Strategy<Value = DenseBatchSpec> {
    root_rows().prop_flat_map(move |rows| {
        (
            Just(rows),
            root_columns(rows),
            prop::collection::vec(0_usize..=6, rows),
        )
            .prop_flat_map(move |(rows, inputs, members)| {
                let mut offsets = vec![0];
                for count in &members {
                    offsets.push(offsets.last().copied().unwrap_or(0) + count);
                }
                let related = offsets.last().copied().unwrap_or(0);
                (
                    Just(rows),
                    Just(inputs),
                    Just(offsets),
                    prop::option::weighted(0.75, text_values(related)),
                )
                    .prop_map(move |(rows, inputs, offsets, nicknames)| {
                        let related = offsets.last().copied().unwrap_or(0);
                        let relation = DenseRelationBatchSpec {
                            offsets,
                            related_row_count: nicknames.is_none().then_some(related),
                            inputs: nicknames
                                .map(|nicknames| {
                                    HashMap::from([(
                                        "nickname".to_string(),
                                        text_column(&nicknames),
                                    )])
                                })
                                .unwrap_or_default(),
                        };
                        DenseBatchSpec {
                            row_count: rows,
                            inputs,
                            relations: if relations {
                                HashMap::from([(member_key(), relation)])
                            } else {
                                HashMap::new()
                            },
                        }
                    })
            })
    })
}

/// Two periods of the same households, the second with the first's names or
/// its own.
fn lifetime_batches() -> impl Strategy<Value = Vec<DenseBatchSpec>> {
    root_rows().prop_flat_map(|rows| {
        (root_columns(rows), root_columns(rows), any::<bool>()).prop_map(
            move |(first, mut second, invariant)| {
                if invariant && let Some(names) = first.get("name") {
                    second.insert("name".to_string(), names.clone());
                }
                [first, second]
                    .into_iter()
                    .map(|inputs| DenseBatchSpec {
                        row_count: rows,
                        inputs,
                        relations: HashMap::new(),
                    })
                    .collect()
            },
        )
    })
}

/// The counts a batch sizes buffers by: its root rows and, when it has the
/// relation, its related rows.
fn counts(batch: &DenseBatchSpec) -> Vec<usize> {
    let mut counts = vec![batch.row_count];
    counts.extend(
        batch
            .relations
            .values()
            .filter_map(|relation| relation.offsets.last().copied()),
    );
    counts
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    })
}

/// Invariants 1 and 2 for one case: `run` executes a fresh copy of the case.
fn check_case(
    counts: &[usize],
    numbered: Index,
    run: impl Fn() -> Result<DenseExecutionResult, EvalError>,
) -> Result<(), TestCaseError> {
    let (uncapped, large) = refusing_large(CAP, 0, &run);
    if let Err(error) = &uncapped {
        prop_assert!(
            refused_count(error).is_none(),
            "a small batch was refused uncapped: {error:?}"
        );
    }
    let long_error = names_long_text(&uncapped);
    // Invariant 1: no text is copied, so a run whose result names no long
    // text asks for nothing as large as the cap.
    if !long_error {
        prop_assert_eq!(large, 0, "{:?}", summary(&uncapped));
    }
    let agrees = |result: &Result<DenseExecutionResult, EvalError>| -> Result<(), TestCaseError> {
        if long_error
            && let Err(error) = result
            && let Some(count) = refused_count(error)
        {
            prop_assert!(
                counts.contains(&count),
                "refused {count} rows, not a count of {counts:?}"
            );
            return Ok(());
        }
        prop_assert_eq!(summary(result), summary(&uncapped));
        Ok(())
    };
    agrees(&capped(CAP, &run))?;
    // Invariant 2: refuse the first, the last and one other large allocation.
    if large > 0 {
        let mut refused = vec![1, large, numbered.index(large) + 1];
        refused.dedup();
        for number in refused {
            let (result, _) = refusing_large(CAP, number, &run);
            agrees(&result)?;
        }
    }
    Ok(())
}

#[test]
fn no_generated_program_copies_a_text_per_row() {
    let _serial = serial();
    let strategy = any::<bool>().prop_flat_map(|relations| {
        (
            rules(relations),
            batch(relations),
            select(MODES.to_vec()),
            any::<Index>(),
        )
    });
    runner(256)
        .run(&strategy, |((rules, chosen), batch, mode, numbered)| {
            let outputs = names(&rules);
            let program = compile(&program(rules, chosen)).map_err(TestCaseError::fail)?;
            let counts = counts(&batch);
            check_case(&counts, numbered, || {
                execute(&program, mode, batch.clone(), &outputs)
            })
        })
        .expect("no text is copied per row");
}

#[test]
fn no_generated_lifetime_program_copies_a_text_per_row() {
    let _serial = serial();
    let strategy = (
        lifetime_rules(),
        lifetime_batches(),
        select(MODES.to_vec()),
        any::<Index>(),
    );
    runner(128)
        .run(&strategy, |(rules, batches, mode, numbered)| {
            let program = compile(&program(rules, None)).map_err(TestCaseError::fail)?;
            let counts = vec![batches[0].row_count];
            let outputs = vec!["life".to_string()];
            check_case(&counts, numbered, || {
                execute_lifetime(&program, mode, batches.clone(), &outputs)
            })
        })
        .expect("no text is copied per row");
}

#[test]
fn text_results_do_not_depend_on_the_numeric_mode() {
    let _serial = serial();
    let strategy = any::<bool>().prop_flat_map(|relations| (rules(relations), batch(relations)));
    runner(128)
        .run(&strategy, |((rules, chosen), batch)| {
            let outputs = names(&rules);
            let program = compile(&program(rules, chosen)).map_err(TestCaseError::fail)?;
            let exact = execute(&program, Mode::Decimal, batch.clone(), &outputs);
            let fast = execute(&program, Mode::Float, batch, &outputs);
            if let (Ok(exact), Ok(fast)) = (&exact, &fast) {
                for (name, value) in &exact.outputs {
                    if let DenseOutputValue::Scalar(DenseColumn::Text(_)) = value {
                        prop_assert_eq!(
                            format!("{value:?}"),
                            format!("{:?}", fast.outputs[name]),
                            "{}",
                            name
                        );
                    }
                }
            }
            Ok(())
        })
        .expect("text does not depend on the numeric mode");
}
