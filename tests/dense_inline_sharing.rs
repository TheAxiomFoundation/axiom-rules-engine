//! Dense compiles each rule it inlines once, however many paths reach it
//! (audit finding dense-inline-derived-exponential, 2026-09-26).
//!
//! Dense inlines a rule wherever a `sum` value, a `where` clause or a derived
//! relation's predicate reads it, and inlines the rules that rule reads the
//! same way. It inlined a fresh copy at every read, so a rule graph with shared
//! dependencies compiled to a tree with one node per path: `p_i = p_{i-1} +
//! p_{i-1}` compiled to 2^k nodes at depth k, and evaluation walked all of
//! them. Each inlined body is now compiled once per site (a current-entity
//! rule on root rows, a related rule on one relation's related rows), and a
//! body read by more than one node is evaluated once per row.
//!
//! Invariants tested here:
//! * **Size.** The compiled program grows linearly with the depth of a
//!   diamond chain, through each of the four inlining arms.
//! * **Semantics.** Inlined bodies keep explain's answers: for random rule
//!   graphs with shared dependencies (values, `if`/`and`/`or` laziness,
//!   division by zero), dense answers exactly what explain answers, value or
//!   error.
//! * **Laziness.** A shared body is evaluated only for the rows that reach
//!   it: a row that reaches it through no path never fails in it.
//! * **Labels.** A `match` in a shared body names the rule that owns it,
//!   whichever rule reads the body.
//! * **Cycles.** Rules that inline each other in a cycle are refused with an
//!   error instead of overflowing the stack.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::spec::{PeriodSpec, ProgramSpec, ScalarValueSpec};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use rust_decimal::Decimal;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixture: households with members; `member(household, person)`.
// ---------------------------------------------------------------------------

/// A household's `size` and its members' `income`.
#[derive(Clone, Debug)]
struct Household {
    size: i64,
    incomes: Vec<&'static str>,
}

fn households() -> Vec<Household> {
    vec![
        Household {
            size: 3,
            incomes: vec!["100", "0", "50"],
        },
        Household {
            size: 0,
            incomes: vec![],
        },
        Household {
            size: 1,
            incomes: vec!["20"],
        },
        Household {
            size: 2,
            incomes: vec!["-5", "7.5"],
        },
    ]
}

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn interval() -> Value {
    json!({ "start": "2026-01-01", "end": "2026-01-31" })
}

fn member() -> Value {
    json!({ "name": "member", "arity": 2, "slot_entities": ["Household", "Person"] })
}

fn derived_relation(name: &str, source: &str, predicate: Value) -> Value {
    json!({
        "name": name,
        "arity": 2,
        "slot_entities": ["Household", "Person"],
        "derivation": {
            "source_relation": source,
            "current_slot": 0,
            "related_slot": 1,
            "slot_entities": ["Household", "Person"],
            "predicate": predicate,
        },
    })
}

fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn decimal(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "decimal", "value": value } })
}

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn add(items: Vec<Value>) -> Value {
    json!({ "kind": "add", "items": items })
}

fn sub(left: Value, right: Value) -> Value {
    json!({ "kind": "sub", "left": left, "right": right })
}

fn and(items: Vec<Value>) -> Value {
    json!({ "kind": "and", "items": items })
}

fn or(items: Vec<Value>) -> Value {
    json!({ "kind": "or", "items": items })
}

fn if_then_else(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr })
}

fn count(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1 });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn sum(relation: &str, rule: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "sum_related",
        "relation": relation,
        "current_slot": 0,
        "related_slot": 1,
        "value": { "kind": "derived", "name": rule },
    });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn scalar_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "decimal", "unit": null, "semantics": "scalar", "expr": expr })
}

fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

fn program(relations: Vec<Value>, derived: Vec<Value>) -> Value {
    json!({ "relations": relations, "derived": derived })
}

fn compile_dense(program: &Value) -> Result<DenseCompiledProgram, DenseCompileError> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program JSON parses");
    DenseCompiledProgram::from_program(
        &spec.to_program().expect("program lowers"),
        Some("Household"),
    )
}

// ---------------------------------------------------------------------------
// Running both modes
// ---------------------------------------------------------------------------

/// Every household's outputs, in batch order, or the error.
type Answer = Result<Vec<Vec<Decimal>>, String>;

fn reads(program: &Value, name: &str) -> bool {
    program
        .to_string()
        .contains(&format!(r#""kind":"input","name":"{name}""#))
}

fn explain(households: &[Household], program: &Value, outputs: &[&str]) -> Answer {
    // Explain refuses an input no rule reads.
    let (reads_income, reads_size) = (reads(program, "income"), reads(program, "size"));
    let mut inputs = Vec::new();
    let mut tuples = Vec::new();
    for (index, household) in households.iter().enumerate() {
        let id = format!("h{index}");
        if reads_size {
            inputs.push(json!({
                "name": "size", "entity": "Household", "entity_id": id, "interval": interval(),
                "value": { "kind": "integer", "value": household.size },
            }));
        }
        for (position, income) in household.incomes.iter().enumerate() {
            let person = format!("{id}p{position}");
            if reads_income {
                inputs.push(json!({
                    "name": "income", "entity": "Person", "entity_id": person, "interval": interval(),
                    "value": { "kind": "decimal", "value": income },
                }));
            }
            tuples.push(json!({ "name": "member", "tuple": [id, person], "interval": interval() }));
        }
    }
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain",
        "program": program,
        "dataset": { "inputs": inputs, "relations": tuples },
        "queries": (0..households.len())
            .map(|index| json!({ "entity_id": format!("h{index}"), "period": period(), "outputs": outputs }))
            .collect::<Vec<_>>(),
    }))
    .expect("request JSON parses");
    let response = execute_request(request).map_err(|error| error.to_string())?;
    Ok(response
        .results
        .iter()
        .map(|result| {
            outputs
                .iter()
                .map(|output| match &result.outputs[*output] {
                    OutputValue::Scalar { value, .. } => match value {
                        ScalarValueSpec::Integer { value } => Decimal::from(*value),
                        ScalarValueSpec::Decimal { value } => {
                            Decimal::from_str(value).expect("decimal output")
                        }
                        other => panic!("unexpected scalar {other:?}"),
                    },
                    other => panic!("unexpected output {other:?}"),
                })
                .collect()
        })
        .collect())
}

fn dense(
    compiled: &DenseCompiledProgram,
    households: &[Household],
    outputs: &[&str],
    exact: bool,
) -> Answer {
    let mut offsets = vec![0];
    let mut incomes = Vec::new();
    for household in households {
        offsets.push(offsets.last().copied().unwrap_or(0) + household.incomes.len());
        incomes.extend(
            household
                .incomes
                .iter()
                .map(|income| Decimal::from_str(income).expect("income")),
        );
    }
    // Every relation here is keyed to `member`: one batch carries every
    // column any of them reads.
    let relations = compiled
        .relations()
        .iter()
        .map(|schema| {
            (
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: offsets.clone(),
                    inputs: HashMap::from([(
                        "income".to_string(),
                        DenseColumn::Decimal(incomes.clone()),
                    )]),
                },
            )
        })
        .collect();
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let period = period.to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([(
            "size".to_string(),
            DenseColumn::Integer(households.iter().map(|household| household.size).collect()),
        )]),
        relations,
    };
    let outputs = outputs
        .iter()
        .map(|output| output.to_string())
        .collect::<Vec<_>>();
    let result = if exact {
        compiled.execute(&period, batch, &outputs)
    } else {
        compiled.execute_f64(&period, batch, &outputs)
    }
    .map_err(|error| error.to_string())?;
    Ok((0..result.row_count)
        .map(|row| {
            outputs
                .iter()
                .map(|output| match &result.outputs[output] {
                    DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
                        Decimal::from(values[row])
                    }
                    DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values[row],
                    DenseOutputValue::Scalar(DenseColumn::Float(values)) => {
                        Decimal::from_f64_retain(values[row]).expect("finite")
                    }
                    other => panic!("unexpected output {other:?}"),
                })
                .collect()
        })
        .collect())
}

/// Dense (exact mode) answers exactly what explain answers, value or error.
fn assert_matches_explain(
    label: &str,
    households: &[Household],
    program: &Value,
    outputs: &[&str],
) {
    let expected = explain(households, program, outputs);
    let compiled = compile_dense(program)
        .unwrap_or_else(|error| panic!("{label}: dense declined the program: {error}"));
    let actual = dense(&compiled, households, outputs, true);
    assert_eq!(actual, expected, "{label}: dense and explain differ");
}

// ---------------------------------------------------------------------------
// Diamond chains through each inlining arm
// ---------------------------------------------------------------------------

/// `{prefix}0 = leaf`; `{prefix}i = ({prefix}(i-1) + {prefix}(i-1)) - ({prefix}(i-1) - 1)`,
/// which is `{prefix}(i-1) + 1` reading the rule below three times.
fn scalar_diamond(prefix: &str, entity: &str, leaf: Value, depth: usize) -> Vec<Value> {
    let mut rules = vec![scalar_rule(&format!("{prefix}0"), entity, leaf)];
    for level in 1..=depth {
        let below = || derived(&format!("{prefix}{}", level - 1));
        rules.push(scalar_rule(
            &format!("{prefix}{level}"),
            entity,
            sub(add(vec![below(), below()]), sub(below(), integer(1))),
        ));
    }
    rules
}

/// `{prefix}0 = leaf`; `{prefix}i = {prefix}(i-1) and ({prefix}(i-1) or {prefix}(i-1))`.
fn judgment_diamond(prefix: &str, entity: &str, leaf: Value, depth: usize) -> Vec<Value> {
    let mut rules = vec![judgment_rule(&format!("{prefix}0"), entity, leaf)];
    for level in 1..=depth {
        let below = || derived(&format!("{prefix}{}", level - 1));
        rules.push(judgment_rule(
            &format!("{prefix}{level}"),
            entity,
            and(vec![below(), or(vec![below(), below()])]),
        ));
    }
    rules
}

const ARMS: [&str; 4] = [
    "related scalar",
    "related predicate",
    "current scalar",
    "current judgment",
];

/// A program whose output `total` reads a depth-`depth` diamond through one
/// inlining arm.
fn diamond_program(arm: &str, depth: usize) -> Value {
    let top = |prefix: &str| format!("{prefix}{depth}");
    match arm {
        // A person rule summed into the household.
        "related scalar" => {
            let mut rules = scalar_diamond("p", "Person", input("income"), depth);
            rules.push(scalar_rule(
                "total",
                "Household",
                sum("member", &top("p"), None),
            ));
            program(vec![member()], rules)
        }
        // A person judgment in a `where` clause.
        "related predicate" => {
            let mut rules = judgment_diamond(
                "j",
                "Person",
                compare(input("income"), "gt", decimal("0")),
                depth,
            );
            rules.push(scalar_rule(
                "total",
                "Household",
                count("member", Some(derived(&top("j")))),
            ));
            program(vec![member()], rules)
        }
        // A household rule a derived relation's predicate compares.
        "current scalar" => {
            let mut rules = scalar_diamond("h", "Household", input("size"), depth);
            rules.push(scalar_rule("total", "Household", count("eligible", None)));
            program(
                vec![
                    member(),
                    derived_relation(
                        "eligible",
                        "member",
                        compare(derived(&top("h")), "gt", integer(depth as i64 + 1)),
                    ),
                ],
                rules,
            )
        }
        // A household judgment a derived relation's predicate reads.
        "current judgment" => {
            let mut rules = judgment_diamond(
                "g",
                "Household",
                compare(input("size"), "gt", integer(1)),
                depth,
            );
            rules.push(scalar_rule("total", "Household", count("eligible", None)));
            program(
                vec![
                    member(),
                    derived_relation("eligible", "member", derived(&top("g"))),
                ],
                rules,
            )
        }
        other => panic!("unknown arm {other}"),
    }
}

/// Run `test` on a thread with a large stack. Compiling and evaluating an
/// inlined rule still recurses once per level of the rules it reads (bounded
/// separately), and a debug build's default 2 MB test thread holds about 20
/// levels of these diamonds; sharing is what these tests are about.
fn on_large_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(test)
        .expect("test thread spawns")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

/// The size of `program`'s dense plan, by its `Debug` rendering.
fn plan_size(program: &Value) -> usize {
    format!("{:?}", compile_dense(program).expect("dense compiles")).len()
}

/// The plan for a diamond of depth 12 is at most a few times the plan for
/// depth 6. Inlining a fresh copy per read made it 3^6 (729) times larger:
/// each level reads the level below three times.
fn assert_linear(arm: &str) {
    let (shallow, deep) = (
        plan_size(&diamond_program(arm, 6)),
        plan_size(&diamond_program(arm, 12)),
    );
    assert!(
        deep <= 3 * shallow,
        "{arm}: the plan grew from {shallow} bytes at depth 6 to {deep} at depth 12"
    );
}

#[test]
fn diamonds_compile_to_plans_linear_in_depth() {
    on_large_stack(|| {
        for arm in ARMS {
            assert_linear(arm);
        }
    });
}

/// At depth 48 a copy per read would be 3^48 nodes. Dense compiles and
/// evaluates the shared bodies and answers what explain answers, in both
/// numeric modes.
#[test]
fn deep_diamonds_match_explain() {
    on_large_stack(|| {
        let households = households();
        for arm in ARMS {
            // Fail fast rather than build 3^48 nodes if sharing regresses.
            assert_linear(arm);
            let program = diamond_program(arm, 48);
            assert_matches_explain(arm, &households, &program, &["total"]);
            let compiled = compile_dense(&program).expect("dense compiles");
            assert_eq!(
                dense(&compiled, &households, &["total"], false),
                dense(&compiled, &households, &["total"], true),
                "{arm}: f64 and decimal modes differ"
            );
        }
    });
}

/// The same household judgment read by every link of a relation chain is one
/// shared body, evaluated once per household, not once per link.
#[test]
fn a_rule_every_link_reads_is_shared_across_the_chain() {
    on_large_stack(a_rule_every_link_reads_is_shared);
}

fn a_rule_every_link_reads_is_shared() {
    let links = 24;
    let mut relations = vec![member()];
    for link in 0..links {
        let source = if link + 1 == links {
            "member".to_string()
        } else {
            format!("link{}", link + 1)
        };
        relations.push(derived_relation(
            &format!("link{link}"),
            &source,
            and(vec![derived("g8"), derived("j8")]),
        ));
    }
    let mut rules = judgment_diamond(
        "g",
        "Household",
        compare(input("size"), "gt", integer(0)),
        8,
    );
    rules.extend(judgment_diamond(
        "j",
        "Person",
        compare(input("income"), "gte", decimal("0")),
        8,
    ));
    rules.push(scalar_rule("total", "Household", count("link0", None)));
    let program = program(relations, rules);
    // One body per diamond level for `g` (root rows) and one per level and
    // link for `j` (each link's related rows). A copy per read was 3^8
    // leaves of each per link.
    let size = plan_size(&program);
    println!("chain plan: {size} bytes");
    assert!(size < 400_000, "the chain's plan is {size} bytes");
    assert_matches_explain("chain", &households(), &program, &["total"]);
}

// ---------------------------------------------------------------------------
// Laziness, labels and cycles
// ---------------------------------------------------------------------------

/// `share = 10 / income` fails for a member with no income. `guarded` reads it
/// twice, only for members with income; `unguarded` reads it for every
/// member. Requested alone, `guarded` never evaluates the shared body for the
/// zero-income member, so it answers; with `unguarded` in the same request,
/// that member fails, and the cached rows of one read do not leak into the
/// other.
#[test]
fn a_shared_body_is_evaluated_only_for_rows_that_reach_it() {
    let divided = json!({ "kind": "div", "left": decimal("10"), "right": input("income") });
    let rules = vec![
        scalar_rule("share", "Person", divided),
        scalar_rule(
            "guarded_member",
            "Person",
            if_then_else(
                compare(input("income"), "ne", decimal("0")),
                add(vec![derived("share"), derived("share")]),
                decimal("0"),
            ),
        ),
        scalar_rule(
            "guarded",
            "Household",
            sum("member", "guarded_member", None),
        ),
        scalar_rule(
            "unguarded",
            "Household",
            count(
                "member",
                Some(compare(derived("share"), "gt", decimal("0"))),
            ),
        ),
        scalar_rule(
            "guarded_where",
            "Household",
            sum(
                "member",
                "share",
                Some(compare(input("income"), "ne", decimal("0"))),
            ),
        ),
    ];
    let program = program(vec![member()], rules);
    let households = households();
    for outputs in [
        &["guarded"][..],
        &["guarded_where"],
        &["guarded", "guarded_where"],
        &["unguarded"],
        &["guarded", "unguarded"],
        &["unguarded", "guarded"],
    ] {
        assert_matches_explain(&format!("{outputs:?}"), &households, &program, outputs);
    }
    assert!(
        explain(&households, &program, &["guarded", "guarded_where"]).is_ok(),
        "the guarded reads never divide by zero"
    );
    assert!(
        explain(&households, &program, &["unguarded"]).is_err(),
        "the unguarded read divides by zero"
    );
}

/// `labelled` has a `match` without `_`; `first` and `second` both read it,
/// so its body is shared. An uncovered subject fails naming `labelled`, as
/// explain names it, not whichever rule read the body first.
#[test]
fn a_match_in_a_shared_body_names_its_own_rule() {
    let labelled = json!({
        "kind": "if",
        "condition": compare(input("income"), "eq", decimal("100")),
        "then_expr": decimal("1"),
        "else_expr": { "kind": "no_match", "subject": input("income"), "patterns": [decimal("100")] },
    });
    let rules = vec![
        scalar_rule("labelled", "Person", labelled),
        scalar_rule(
            "first",
            "Person",
            add(vec![derived("labelled"), decimal("1")]),
        ),
        scalar_rule(
            "second",
            "Person",
            add(vec![derived("labelled"), derived("labelled")]),
        ),
        scalar_rule("total_first", "Household", sum("member", "first", None)),
        scalar_rule("total_second", "Household", sum("member", "second", None)),
    ];
    let program = program(vec![member()], rules);
    let households = households();
    for outputs in [
        &["total_first"][..],
        &["total_second"],
        &["total_second", "total_first"],
    ] {
        assert_matches_explain(&format!("{outputs:?}"), &households, &program, outputs);
    }
    let error = explain(&households, &program, &["total_second"]).expect_err("0 is uncovered");
    assert!(error.contains("labelled"), "{error}");
}

/// Rules that inline each other, through a related or a current-entity arm.
/// Compiling an artifact refuses them; a raw `Program` used to overflow the
/// stack in dense.
#[test]
fn rules_that_inline_each_other_are_refused() {
    let cases = [
        (
            "related scalar",
            program(
                vec![member()],
                vec![
                    scalar_rule("p", "Person", add(vec![derived("q"), decimal("1")])),
                    scalar_rule("q", "Person", add(vec![derived("p"), decimal("1")])),
                    scalar_rule("total", "Household", sum("member", "p", None)),
                ],
            ),
        ),
        (
            "related predicate",
            program(
                vec![member()],
                vec![
                    judgment_rule("p", "Person", and(vec![derived("q")])),
                    judgment_rule("q", "Person", or(vec![derived("p")])),
                    scalar_rule("total", "Household", count("member", Some(derived("p")))),
                ],
            ),
        ),
        (
            "current scalar",
            program(
                vec![
                    member(),
                    derived_relation(
                        "eligible",
                        "member",
                        compare(derived("h"), "gt", integer(0)),
                    ),
                ],
                vec![
                    scalar_rule("h", "Household", add(vec![derived("k"), integer(1)])),
                    scalar_rule("k", "Household", add(vec![derived("h"), integer(1)])),
                    scalar_rule("total", "Household", count("eligible", None)),
                ],
            ),
        ),
        (
            "current judgment",
            program(
                vec![
                    member(),
                    derived_relation("eligible", "member", derived("g")),
                ],
                vec![
                    judgment_rule("g", "Household", and(vec![derived("f")])),
                    judgment_rule("f", "Household", or(vec![derived("g")])),
                    scalar_rule("total", "Household", count("eligible", None)),
                ],
            ),
        ),
    ];
    for (arm, program) in cases {
        let error = compile_dense(&program).expect_err("the rules form a cycle");
        let message = error.to_string();
        assert!(
            message.contains("cyclic dense compilation dependency involving"),
            "{arm}: {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// Random rule graphs with shared dependencies
// ---------------------------------------------------------------------------

/// One generated rule: its kind and the choices that pick its operands.
#[derive(Clone, Debug)]
struct Choice {
    judgment: bool,
    op: u8,
    operands: [u8; 3],
}

fn choice() -> impl Strategy<Value = Choice> {
    (any::<bool>(), any::<u8>(), any::<[u8; 3]>()).prop_map(|(judgment, op, operands)| Choice {
        judgment,
        op,
        operands,
    })
}

const LITERALS: [&str; 5] = ["0", "1", "2", "-1", "0.5"];
const COMPARISONS: [&str; 6] = ["lt", "lte", "gt", "gte", "eq", "ne"];

/// A rule graph for one entity: each rule reads earlier rules (often the same
/// one more than once), the entity's input, or literals.
struct Graph {
    rules: Vec<Value>,
    scalars: Vec<String>,
    judgments: Vec<String>,
}

fn graph(prefix: &str, entity: &str, leaf: &str, choices: &[Choice]) -> Graph {
    let mut graph = Graph {
        rules: Vec::new(),
        scalars: Vec::new(),
        judgments: Vec::new(),
    };
    let scalar = |graph: &Graph, pick: u8| -> Value {
        match pick % 6 {
            0 => input(leaf),
            1 => decimal(LITERALS[usize::from(pick / 6) % LITERALS.len()]),
            _ if graph.scalars.is_empty() => input(leaf),
            _ => derived(&graph.scalars[usize::from(pick) % graph.scalars.len()]),
        }
    };
    let judgment = |graph: &Graph, pick: u8| -> Value {
        if graph.judgments.is_empty() || pick % 5 == 0 {
            compare(scalar(graph, pick / 5), "gt", decimal("0"))
        } else {
            derived(&graph.judgments[usize::from(pick) % graph.judgments.len()])
        }
    };
    for (index, choice) in choices.iter().enumerate() {
        let [a, b, c] = choice.operands;
        let name = format!("{prefix}{index}");
        if choice.judgment {
            let expr = match choice.op % 4 {
                0 => compare(
                    scalar(&graph, a),
                    COMPARISONS[usize::from(b) % COMPARISONS.len()],
                    scalar(&graph, c),
                ),
                1 => and(vec![judgment(&graph, a), judgment(&graph, b)]),
                2 => or(vec![judgment(&graph, a), judgment(&graph, b)]),
                _ => json!({ "kind": "not", "item": judgment(&graph, a) }),
            };
            graph.rules.push(judgment_rule(&name, entity, expr));
            graph.judgments.push(name);
        } else {
            let expr = match choice.op % 6 {
                0 => add(vec![scalar(&graph, a), scalar(&graph, b)]),
                1 => sub(scalar(&graph, a), scalar(&graph, b)),
                2 => json!({ "kind": "max", "items": [scalar(&graph, a), scalar(&graph, b)] }),
                3 => json!({ "kind": "min", "items": [scalar(&graph, a), scalar(&graph, b)] }),
                // The divisor is often zero; `if` guards some reads of it.
                4 => {
                    json!({ "kind": "div", "left": scalar(&graph, a), "right": scalar(&graph, b) })
                }
                _ => if_then_else(judgment(&graph, c), scalar(&graph, a), scalar(&graph, b)),
            };
            graph.rules.push(scalar_rule(&name, entity, expr));
            graph.scalars.push(name);
        }
    }
    if graph.scalars.is_empty() {
        let name = format!("{prefix}_leaf");
        graph.rules.push(scalar_rule(&name, entity, input(leaf)));
        graph.scalars.push(name);
    }
    if graph.judgments.is_empty() {
        let name = format!("{prefix}_positive");
        graph.rules.push(judgment_rule(
            &name,
            entity,
            compare(input(leaf), "gt", decimal("0")),
        ));
        graph.judgments.push(name);
    }
    graph
}

/// Household outputs reading the graphs through every inlining arm: a `sum`
/// value and `where` clause (person rules on related rows), a derived
/// relation's predicate (household rules on root rows, person rules on
/// related rows), a two-link chain whose links read the same rules, and a
/// root rule reading a household rule directly and through `sum`.
fn random_program(people: &Graph, homes: &Graph) -> Value {
    let last = |names: &Vec<String>| names.last().expect("non-empty").clone();
    let first = |names: &Vec<String>| names[0].clone();
    let (p_scalar, p_judgment) = (last(&people.scalars), last(&people.judgments));
    let (h_scalar, h_judgment) = (last(&homes.scalars), last(&homes.judgments));
    let eligible = and(vec![
        derived(&h_judgment),
        compare(derived(&h_scalar), "gte", decimal("0")),
        derived(&first(&people.judgments)),
    ]);
    let mut rules = people.rules.clone();
    rules.extend(homes.rules.clone());
    rules.extend([
        scalar_rule(
            "total",
            "Household",
            sum("member", &p_scalar, Some(derived(&p_judgment))),
        ),
        scalar_rule("counted", "Household", count("eligible", None)),
        scalar_rule(
            "chained",
            "Household",
            count("link0", Some(derived(&first(&people.judgments)))),
        ),
        scalar_rule(
            "direct",
            "Household",
            add(vec![
                derived(&h_scalar),
                sum("member", &first(&people.scalars), None),
            ]),
        ),
    ]);
    program(
        vec![
            member(),
            derived_relation("eligible", "member", eligible),
            derived_relation(
                "link1",
                "member",
                or(vec![derived(&h_judgment), derived(&p_judgment)]),
            ),
            derived_relation(
                "link0",
                "link1",
                and(vec![derived(&h_judgment), derived(&p_judgment)]),
            ),
        ],
        rules,
    )
}

fn households_strategy() -> impl Strategy<Value = Vec<Household>> {
    const INCOMES: [&str; 6] = ["0", "0", "1", "2.5", "-1", "10"];
    prop::collection::vec(
        (
            prop::sample::select(vec![0_i64, 0, 1, 2, -1, 3]),
            prop::collection::vec(prop::sample::select(INCOMES.to_vec()), 0..4),
        )
            .prop_map(|(size, incomes)| Household { size, incomes }),
        1..5,
    )
}

/// For random rule graphs, full of shared dependencies, read through every
/// inlining arm, and random households, dense answers exactly what explain
/// answers: the same values, or the same error.
#[test]
fn random_shared_rule_graphs_match_explain() {
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 256,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    let outputs = ["total", "counted", "chained", "direct"];
    // How many cases answered, and how many shared a body, so the property
    // cannot pass by comparing only errors or unshared plans.
    let answered = std::cell::Cell::new(0_usize);
    let shared = std::cell::Cell::new(0_usize);
    runner
        .run(
            &(
                prop::collection::vec(choice(), 1..8),
                prop::collection::vec(choice(), 1..6),
                households_strategy(),
            ),
            |(person_choices, household_choices, households)| {
                let people = graph("p", "Person", "income", &person_choices);
                let homes = graph("h", "Household", "size", &household_choices);
                let program = random_program(&people, &homes);
                let expected = explain(&households, &program, &outputs);
                let compiled = compile_dense(&program).expect("dense compiles");
                let actual = dense(&compiled, &households, &outputs, true);
                answered.set(answered.get() + usize::from(expected.is_ok()));
                shared.set(shared.get() + usize::from(shares_a_body(&compiled)));
                prop_assert_eq!(actual, expected, "program {}", program);
                Ok(())
            },
        )
        .expect("dense matches explain");
    println!(
        "{} of 256 cases answered, {} shared a body",
        answered.get(),
        shared.get()
    );
    assert!(
        answered.get() >= 64,
        "only {} cases answered",
        answered.get()
    );
    assert!(
        shared.get() >= 64,
        "only {} cases shared a body",
        shared.get()
    );
}

/// Whether any inlined body of `compiled` is read by more than one node, by
/// its `Debug` rendering.
fn shares_a_body(compiled: &DenseCompiledProgram) -> bool {
    let plan = format!("{compiled:?}");
    plan.match_indices("references: ").any(|(at, prefix)| {
        plan[at + prefix.len()..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|count| count.parse::<usize>().ok())
            .is_some_and(|count| count > 1)
    })
}
