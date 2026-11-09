//! Dense returns explain's text values.
//!
//! Dense fills a text column by sharing one `Arc<str>` among the rows that
//! hold a value, and quotes a text in an error without copying it until the
//! error is reported. Neither may change what a call returns. For generated
//! programs over text literals, inputs, parameter lookups, cached rules,
//! `if`, `match` without a matching arm, and a derived relation whose
//! predicate reads the household's text, each batch runs through explain (one
//! query per household) and through dense in both numeric modes.
//!
//! Invariant: dense and explain agree. Both succeed with the same value for
//! every output and row, or both fail with the same message.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::JudgmentOutcome;
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec, PeriodSpec,
    ProgramSpec, RelationRecordSpec, ScalarValueSpec,
};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};

/// The texts programs and batches draw from: empty, short, repeated, and
/// ones whose debug quoting escapes characters.
fn texts() -> Vec<String> {
    vec![
        String::new(),
        "a".to_string(),
        "b".to_string(),
        "quote \" and \\ slash".to_string(),
        "line\nbreak é \u{1}".to_string(),
        "x".repeat(300),
    ]
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

/// A program over `rules`, relating households to people: `member` is the
/// base relation and `chosen` the members `chosen` holds for. The text table
/// `t` maps 0 to 2 onto texts.
fn program(rules: Vec<Value>, chosen: Value) -> Value {
    let pair = json!(["Household", "Person"]);
    let texts = texts();
    json!({
        "units": [],
        "relations": [
            { "name": "member", "arity": 2, "slot_entities": pair },
            {
                "name": "chosen", "arity": 2, "slot_entities": pair,
                "derivation": {
                    "source_relation": "member", "current_slot": 0, "related_slot": 1,
                    "slot_entities": pair, "predicate": chosen,
                },
            },
        ],
        "parameters": [
            { "name": "t", "unit": null, "indexed_by": "key", "versions": [{ "effective_from": "2000-01-01", "values": {
                "0": { "kind": "text", "value": texts[1] },
                "1": { "kind": "text", "value": texts[3] },
                "2": { "kind": "text", "value": texts[5] },
            } }] },
        ],
        "derived": rules,
    })
}

#[derive(Clone, Debug, Default)]
struct Scope {
    texts: Vec<String>,
    judgments: Vec<String>,
}

fn union(strategies: Vec<BoxedStrategy<Value>>) -> BoxedStrategy<Value> {
    proptest::strategy::Union::new(strategies).boxed()
}

fn literal() -> BoxedStrategy<Value> {
    select(texts()).prop_map(|value| text(&value)).boxed()
}

fn equality() -> impl Strategy<Value = &'static str> {
    select(vec!["eq", "ne"])
}

fn text_expr(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        literal(),
        Just(input("name")).boxed(),
        select(texts())
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
fn number(scope: &Scope) -> BoxedStrategy<Value> {
    let nickname =
        (equality(), literal()).prop_map(|(op, value)| compare(input("nickname"), op, value));
    union(vec![
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
        (select(vec!["member", "chosen"]), prop::option::of(nickname))
            .prop_map(|(relation, where_clause)| count_related(relation, where_clause))
            .boxed(),
    ])
}

/// A predicate for `chosen`: the household's projected text or judgment,
/// with or without a `match` that fails, or the member's own text.
fn chosen() -> BoxedStrategy<Value> {
    prop_oneof![
        (equality(), literal()).prop_map(|(op, value)| compare(derived("h"), op, value)),
        (equality(), literal()).prop_map(|(op, value)| compare(input("nickname"), op, value)),
        prop::collection::vec(literal(), 1..=2).prop_map(|patterns| {
            compare(
                if_then_else(
                    compare(derived("h"), "eq", text("a")),
                    integer(1),
                    no_match(derived("h"), patterns),
                ),
                "eq",
                integer(1),
            )
        }),
        Just(derived("hj")),
    ]
    .boxed()
}

/// `h` (text) and `hj` (judgment) read no relation, so `chosen` can read
/// them. Then the text `r0`, the judgment `r1` and the number `r2`.
fn rules() -> impl Strategy<Value = (Vec<Value>, Value)> {
    let helpers = Scope {
        texts: vec!["h".to_string()],
        judgments: vec!["hj".to_string()],
    };
    let later = Scope {
        texts: vec!["h".to_string(), "r0".to_string()],
        judgments: vec!["hj".to_string(), "r1".to_string()],
    };
    (
        (
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
            number(&later),
        ),
        chosen(),
    )
        .prop_map(|((h, hj, r0, r1, r2), chosen)| {
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

// ---------------------------------------------------------------------------
// Households
// ---------------------------------------------------------------------------

/// A household's name, its `a`, and its members' nicknames.
#[derive(Clone, Debug)]
struct Household {
    name: String,
    a: i64,
    nicknames: Vec<String>,
}

fn households() -> impl Strategy<Value = Vec<Household>> {
    let household = (
        select(texts()),
        0_i64..=2,
        prop::collection::vec(select(texts()), 0..=3),
    )
        .prop_map(|(name, a, nicknames)| Household { name, a, nicknames });
    prop_oneof![
        prop::collection::vec(household.clone(), 0..=6),
        // Every household the same name, so rows share one value.
        (household, 1_usize..=6).prop_map(|(household, count)| vec![household; count]),
    ]
}

fn member_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member".to_string(),
        current_slot: 0,
        related_slot: 1,
    }
}

fn text_column(values: impl Iterator<Item = String>) -> DenseColumn {
    DenseColumn::Text(values.map(|value| value.as_str().into()).collect())
}

fn dense_batch(households: &[Household]) -> DenseBatchSpec {
    let mut offsets = vec![0];
    for household in households {
        offsets.push(offsets.last().copied().unwrap_or(0) + household.nicknames.len());
    }
    DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([
            (
                "name".to_string(),
                text_column(households.iter().map(|household| household.name.clone())),
            ),
            (
                "a".to_string(),
                DenseColumn::Integer(households.iter().map(|household| household.a).collect()),
            ),
        ]),
        relations: HashMap::from([(
            member_key(),
            DenseRelationBatchSpec {
                offsets,
                inputs: HashMap::from([(
                    "nickname".to_string(),
                    text_column(
                        households
                            .iter()
                            .flat_map(|household| household.nicknames.clone()),
                    ),
                )]),
                ..Default::default()
            },
        )]),
    }
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = period();
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn record(name: &str, entity: &str, entity_id: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: entity_id.to_string(),
        interval: interval(),
        value,
    }
}

/// Explain's dataset for `households`. It holds a record only for an input
/// in `read`, the program's input catalog: explain refuses one for an input
/// the program never reads, which dense ignores.
fn dataset(households: &[Household], read: &[String]) -> DatasetSpec {
    let mut dataset = DatasetSpec::default();
    for (index, household) in households.iter().enumerate() {
        let household_id = format!("household-{index}");
        dataset.inputs.extend(
            [
                record(
                    "name",
                    "Household",
                    &household_id,
                    ScalarValueSpec::Text {
                        value: household.name.clone(),
                    },
                ),
                record(
                    "a",
                    "Household",
                    &household_id,
                    ScalarValueSpec::Integer { value: household.a },
                ),
            ]
            .into_iter()
            .filter(|record| read.contains(&record.name)),
        );
        for (member, nickname) in household.nicknames.iter().enumerate() {
            let person_id = format!("person-{index}-{member}");
            if read.iter().any(|name| name == "nickname") {
                dataset.inputs.push(record(
                    "nickname",
                    "Person",
                    &person_id,
                    ScalarValueSpec::Text {
                        value: nickname.clone(),
                    },
                ));
            }
            dataset.relations.push(RelationRecordSpec {
                name: "member".to_string(),
                tuple: vec![household_id.clone(), person_id],
                interval: interval(),
            });
        }
    }
    dataset
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// One output's value on one row, as both modes report it.
#[derive(Debug, PartialEq)]
enum Cell {
    Text(String),
    Integer(i64),
    Judgment(JudgmentOutcomeSpec),
}

fn explain_cell(value: &OutputValue) -> Cell {
    match value {
        OutputValue::Scalar { value, .. } => match value {
            ScalarValueSpec::Text { value } => Cell::Text(value.clone()),
            ScalarValueSpec::Integer { value } => Cell::Integer(*value),
            other => panic!("unexpected explain scalar {other:?}"),
        },
        OutputValue::Judgment { outcome, .. } => Cell::Judgment(*outcome),
    }
}

fn dense_cell(value: &DenseOutputValue, row: usize) -> Cell {
    match value {
        DenseOutputValue::Scalar(DenseColumn::Text(values)) => Cell::Text(values[row].to_string()),
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => Cell::Integer(values[row]),
        DenseOutputValue::Scalar(other) => panic!("unexpected dense column {other:?}"),
        DenseOutputValue::Judgment(outcomes) => Cell::Judgment(match outcomes[row] {
            JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
            JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
            JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
        }),
    }
}

type ExplainRows = Result<Vec<HashMap<String, OutputValue>>, String>;

fn explain(
    artifact: &CompiledProgramArtifact,
    households: &[Household],
    outputs: &[String],
) -> ExplainRows {
    let read: Vec<String> = artifact
        .metadata
        .input_catalog
        .iter()
        .flat_map(|entry| entry.request_names.clone())
        .collect();
    execute_request(ExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: dataset(households, &read),
        queries: (0..households.len())
            .map(|index| ExecutionQuery {
                assessment_date: None,
                entity_id: format!("household-{index}"),
                period: period(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .map(|response| {
        response
            .results
            .into_iter()
            .map(|result| result.outputs.into_iter().collect())
            .collect()
    })
    .map_err(|error| error.to_string())
}

fn agrees(
    explain: &ExplainRows,
    dense: &Result<DenseExecutionResult, EvalError>,
    outputs: &[String],
) -> Result<(), TestCaseError> {
    match (explain, dense) {
        (Ok(rows), Ok(dense)) => {
            for (row, explained) in rows.iter().enumerate() {
                for output in outputs {
                    prop_assert_eq!(
                        dense_cell(&dense.outputs[output], row),
                        explain_cell(&explained[output]),
                        "`{}` on row {}",
                        output,
                        row
                    );
                }
            }
            Ok(())
        }
        (Err(explain), Err(dense)) => {
            prop_assert_eq!(&dense.to_string(), explain);
            Ok(())
        }
        (explain, dense) => Err(TestCaseError::fail(format!(
            "explain {:?}, dense {:?}",
            explain.as_ref().map(|_| "ok"),
            dense.as_ref().map(|_| "ok")
        ))),
    }
}

#[test]
fn dense_returns_explains_text_values() {
    let strategy = (rules(), households());
    let mut runner = TestRunner::new(Config {
        cases: 256,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&strategy, |((rules, chosen), households)| {
            let outputs: Vec<String> = rules
                .iter()
                .map(|rule| rule["name"].as_str().expect("rule name").to_string())
                .collect();
            let spec: ProgramSpec = serde_json::from_value(program(rules, chosen))
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let artifact = CompiledProgramArtifact::compile(spec)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let explained = explain(&artifact, &households, &outputs);
            let period = period().to_model().expect("period converts");
            agrees(
                &explained,
                &dense.execute(&period, dense_batch(&households), &outputs),
                &outputs,
            )?;
            agrees(
                &explained,
                &dense.execute_f64(&period, dense_batch(&households), &outputs),
                &outputs,
            )
        })
        .expect("dense returns explain's values");
}
