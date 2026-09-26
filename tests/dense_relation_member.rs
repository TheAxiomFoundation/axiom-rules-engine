//! Dense and explain agree on `relation_member` in a related predicate
//! (issue #202).
//!
//! Explain (`src/engine.rs`) evaluates a `relation_member` only with a relation
//! context, which `Engine::related_entity_ids` supplies while it evaluates a
//! derived relation's own predicate for one candidate tuple. A `count`/`sum`
//! `where` clause and every rule body are evaluated without one, so a
//! `relation_member` they reach is an error. Dense compiled every
//! `relation_member` in a related predicate to `true`, so it counted every
//! related entity where explain failed, and kept every source tuple of a
//! derived relation whose predicate tests another relation.
//!
//! Every program here runs through the path the PyO3 extension uses:
//! `CompiledProgramArtifact::compile`, then `DenseCompiledProgram::from_artifact`.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::spec::{JudgmentOutcomeSpec, PeriodSpec, ProgramSpec, ScalarValueSpec};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const RELATION_PREDICATE_OUTSIDE_DERIVED_RELATION: &str =
    "type mismatch: relation predicate `member` can only be evaluated inside a derived relation";

// ---------------------------------------------------------------------------
// Fixture: `member(household, person)`, the household in slot 0.
// ---------------------------------------------------------------------------

struct Household {
    id: &'static str,
    /// Each member's id and `income`.
    members: &'static [(&'static str, &'static str)],
}

/// h1 has three members, h2 none and h3 one. Only first members are
/// residents.
const HOUSEHOLDS: &[Household] = &[
    Household {
        id: "h1",
        members: &[("p1", "100"), ("p2", "0"), ("p3", "50")],
    },
    Household {
        id: "h2",
        members: &[],
    },
    Household {
        id: "h3",
        members: &[("p4", "20")],
    },
];

const RESIDENTS: &[(&str, &str)] = &[("h1", "p1"), ("h3", "p4")];

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn interval() -> Value {
    json!({ "start": "2026-01-01", "end": "2026-01-31" })
}

fn base_relation(name: &str) -> Value {
    json!({ "name": name, "arity": 2, "slot_entities": ["Household", "Person"] })
}

/// A derived relation over `source`, reading it household-to-person.
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

fn member_of(relation: &str) -> Value {
    json!({ "kind": "relation_member", "relation": relation, "current_slot": 0, "related_slot": 1 })
}

fn count(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1 });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn sum(relation: &str, value: Value, where_clause: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "sum_related",
        "relation": relation,
        "current_slot": 0,
        "related_slot": 1,
        "value": value,
    });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn income() -> Value {
    json!({ "kind": "input", "name": "income" })
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

/// A judgment that holds (`1 == 1`) or not (`1 == 2`) without reading data.
fn constant(holds: bool) -> Value {
    compare(integer(1), "eq", integer(if holds { 1 } else { 2 }))
}

fn and(items: Vec<Value>) -> Value {
    json!({ "kind": "and", "items": items })
}

fn or(items: Vec<Value>) -> Value {
    json!({ "kind": "or", "items": items })
}

fn not(item: Value) -> Value {
    json!({ "kind": "not", "item": item })
}

fn if_then_else(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr })
}

fn rule(name: &str, entity: &str, dtype: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": dtype, "unit": null, "semantics": "scalar", "expr": expr })
}

fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

fn derived_ref(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn program(relations: Vec<Value>, derived: Vec<Value>) -> Value {
    json!({ "relations": relations, "derived": derived })
}

/// Whether `value` reads the input `name` anywhere.
fn reads_input(value: &Value, name: &str) -> bool {
    match value {
        Value::Object(object) => {
            (matches!(object.get("kind"), Some(kind) if kind == "input" || kind == "input_or_else")
                && object.get("name").is_some_and(|input| input == name))
                || object.values().any(|value| reads_input(value, name))
        }
        Value::Array(items) => items.iter().any(|value| reads_input(value, name)),
        _ => false,
    }
}

/// The fixture's records that `program` declares: explain refuses an input
/// no rule reads and tuples of an undeclared relation.
fn dataset(program: &Value) -> Value {
    let declares = |relation: &str| {
        program["relations"]
            .as_array()
            .is_some_and(|relations| relations.iter().any(|schema| schema["name"] == relation))
    };
    let reads_income = reads_input(program, "income");
    let mut inputs = Vec::new();
    let mut tuples = Vec::new();
    for household in HOUSEHOLDS {
        for (person, income) in household.members {
            if reads_income {
                inputs.push(json!({
                    "name": "income",
                    "entity": "Person",
                    "entity_id": person,
                    "interval": interval(),
                    "value": { "kind": "decimal", "value": income },
                }));
            }
            tuples.push(json!({ "name": "member", "tuple": [household.id, person], "interval": interval() }));
        }
    }
    if declares("resident") {
        for (household, person) in RESIDENTS {
            tuples.push(
                json!({ "name": "resident", "tuple": [household, person], "interval": interval() }),
            );
        }
    }
    json!({ "inputs": inputs, "relations": tuples })
}

// ---------------------------------------------------------------------------
// Running both modes
// ---------------------------------------------------------------------------

/// One output of one household.
#[derive(Clone, Debug, PartialEq)]
enum Cell {
    Number(Decimal),
    Judgment(JudgmentOutcomeSpec),
}

/// Every household's outputs, in `HOUSEHOLDS` order, or the error.
type Answer = Result<Vec<Vec<Cell>>, String>;

fn explain(program: &Value, outputs: &[&str]) -> Answer {
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain",
        "program": program,
        "dataset": dataset(&program),
        "queries": HOUSEHOLDS
            .iter()
            .map(|household| json!({ "entity_id": household.id, "period": period(), "outputs": outputs }))
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
                    OutputValue::Scalar { value, .. } => Cell::Number(match value {
                        ScalarValueSpec::Integer { value } => Decimal::from(*value),
                        ScalarValueSpec::Decimal { value } => {
                            Decimal::from_str(value).expect("decimal output")
                        }
                        other => panic!("unexpected scalar {other:?}"),
                    }),
                    OutputValue::Judgment { outcome, .. } => Cell::Judgment(*outcome),
                })
                .collect()
        })
        .collect())
}

/// The dense compiler for `program` on the artifact path.
fn compile_dense(program: &Value) -> Result<DenseCompiledProgram, DenseCompileError> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program JSON parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("artifact compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
}

fn dense(program: &Value, outputs: &[&str]) -> Result<Answer, DenseCompileError> {
    let compiled = compile_dense(program)?;
    let mut offsets = vec![0];
    let mut incomes = Vec::new();
    for household in HOUSEHOLDS {
        offsets.push(offsets.last().copied().unwrap_or(0) + household.members.len());
        incomes.extend(
            household
                .members
                .iter()
                .map(|(_, income)| Decimal::from_str(income).expect("income")),
        );
    }
    // Derived relations are keyed to their base relation, so one batch per
    // distinct key.
    let relations = compiled
        .relations()
        .iter()
        .map(|schema| {
            let inputs = schema
                .related_inputs
                .iter()
                .map(|name| {
                    assert_eq!(name, "income", "the fixture only has incomes");
                    (name.clone(), DenseColumn::Decimal(incomes.clone()))
                })
                .collect();
            (
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: offsets.clone(),
                    inputs,
                },
            )
        })
        .collect();
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let outputs = outputs
        .iter()
        .map(|output| output.to_string())
        .collect::<Vec<_>>();
    let result = match compiled.execute(
        &period.to_model().expect("period converts"),
        DenseBatchSpec {
            row_count: HOUSEHOLDS.len(),
            inputs: HashMap::new(),
            relations,
        },
        &outputs,
    ) {
        Ok(result) => result,
        Err(error) => return Ok(Err(error.to_string())),
    };
    Ok(Ok((0..result.row_count)
        .map(|row| {
            outputs
                .iter()
                .map(|output| match &result.outputs[output] {
                    DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
                        Cell::Number(Decimal::from(values[row]))
                    }
                    DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                        Cell::Number(values[row])
                    }
                    DenseOutputValue::Judgment(values) => {
                        Cell::Judgment(JudgmentOutcomeSpec::from(values[row]))
                    }
                    DenseOutputValue::Scalar(other) => panic!("unexpected column {other:?}"),
                })
                .collect()
        })
        .collect()))
}

/// Dense compiles `program` and answers exactly what explain answers, values
/// or error message. Returns that answer.
fn assert_dense_matches_explain(label: &str, program: &Value, outputs: &[&str]) -> Answer {
    let expected = explain(program, outputs);
    let actual = dense(program, outputs)
        .unwrap_or_else(|error| panic!("{label}: dense declined the program: {error}"));
    assert_eq!(actual, expected, "{label}: dense and explain differ");
    expected
}

fn numbers(values: &[i64]) -> Vec<Vec<Cell>> {
    values
        .iter()
        .map(|value| vec![Cell::Number(Decimal::from(*value))])
        .collect()
}

// ---------------------------------------------------------------------------
// `relation_member` outside a derived relation's predicate
// ---------------------------------------------------------------------------

/// A `where` clause is evaluated per related entity with no relation context,
/// so explain fails at the first member whose clause reaches the
/// `relation_member`, and dense must fail the same rows with the same error.
/// Dense answered the first case with 3 and the negated one with 0.
#[test]
fn dense_fails_a_where_clause_relation_member_as_explain_does() {
    let member = member_of("member");
    let has_income = compare(income(), "gt", decimal("0"));
    let household = |expr: Value| rule("n", "Household", "integer", expr);
    let members = || vec![base_relation("member")];
    let cases = vec![
        (
            "count where relation_member (the issue's probe)",
            program(
                members(),
                vec![household(count("member", Some(member.clone())))],
            ),
        ),
        (
            "count where not relation_member",
            program(
                members(),
                vec![household(count("member", Some(not(member.clone()))))],
            ),
        ),
        (
            "sum where relation_member",
            program(
                members(),
                vec![rule(
                    "n",
                    "Household",
                    "decimal",
                    sum("member", income(), Some(member.clone())),
                )],
            ),
        ),
        (
            "after a held `and` item",
            program(
                members(),
                vec![household(count(
                    "member",
                    Some(and(vec![has_income.clone(), member.clone()])),
                ))],
            ),
        ),
        (
            "after an unheld `or` item",
            program(
                members(),
                vec![household(count(
                    "member",
                    Some(or(vec![constant(false), member.clone()])),
                ))],
            ),
        ),
        (
            "through a person judgment rule",
            program(
                members(),
                vec![
                    judgment_rule("is_member", "Person", member.clone()),
                    household(count("member", Some(derived_ref("is_member")))),
                ],
            ),
        ),
        (
            "in the `if` condition of a summed person rule",
            program(
                members(),
                vec![
                    rule(
                        "member_income",
                        "Person",
                        "decimal",
                        if_then_else(member.clone(), income(), decimal("0")),
                    ),
                    rule(
                        "n",
                        "Household",
                        "decimal",
                        json!({
                            "kind": "sum_related",
                            "relation": "member",
                            "current_slot": 0,
                            "related_slot": 1,
                            "value": { "kind": "derived", "name": "member_income" },
                        }),
                    ),
                ],
            ),
        ),
        (
            "under a household judgment",
            program(
                members(),
                vec![judgment_rule(
                    "has_members",
                    "Household",
                    compare(count("member", Some(member.clone())), "gt", integer(0)),
                )],
            ),
        ),
    ];
    for (label, program) in cases {
        let outputs: &[&str] = if label == "under a household judgment" {
            &["has_members"]
        } else {
            &["n"]
        };
        assert_eq!(
            assert_dense_matches_explain(label, &program, outputs),
            Err(RELATION_PREDICATE_OUTSIDE_DERIVED_RELATION.to_string()),
            "{label}"
        );
    }
}

/// A `where` clause no row reaches fails in neither mode: a household with no
/// members, an `and`/`or` an earlier item decides, and an `if` branch no row
/// selects.
#[test]
fn dense_answers_when_no_row_reaches_a_where_clause_relation_member() {
    let member = member_of("member");
    let household = |expr: Value| rule("n", "Household", "integer", expr);
    let members = || vec![base_relation("member")];
    let cases = vec![
        (
            "`or` decided by a held item",
            program(
                members(),
                vec![household(count(
                    "member",
                    Some(or(vec![constant(true), member.clone()])),
                ))],
            ),
            vec![3, 0, 1],
        ),
        (
            "`and` decided by an unheld item",
            program(
                members(),
                vec![household(count(
                    "member",
                    Some(and(vec![constant(false), member.clone()])),
                ))],
            ),
            vec![0, 0, 0],
        ),
        (
            "`and` whose first item no member satisfies",
            program(
                members(),
                vec![rule(
                    "n",
                    "Household",
                    "decimal",
                    sum(
                        "member",
                        income(),
                        Some(and(vec![
                            compare(income(), "gt", decimal("1000")),
                            member.clone(),
                        ])),
                    ),
                )],
            ),
            vec![0, 0, 0],
        ),
        (
            "an `if` branch no row selects",
            program(
                members(),
                vec![household(if_then_else(
                    constant(false),
                    count("member", Some(member.clone())),
                    integer(7),
                ))],
            ),
            vec![7, 7, 7],
        ),
    ];
    for (label, program, expected) in cases {
        assert_eq!(
            assert_dense_matches_explain(label, &program, &["n"]),
            Ok(numbers(&expected)),
            "{label}"
        );
    }

    // Only h2, which has no members, is in the batch: its clause is never
    // evaluated.
    let program = program(members(), vec![household(count("member", Some(member)))]);
    let compiled = compile_dense(&program).expect("dense compiles");
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let result = compiled
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: compiled
                    .relations()
                    .iter()
                    .map(|schema| {
                        (
                            schema.key.clone(),
                            DenseRelationBatchSpec {
                                offsets: vec![0, 0],
                                inputs: HashMap::new(),
                            },
                        )
                    })
                    .collect(),
            },
            &["n".to_string()],
        )
        .expect("a household with no members never reaches the clause");
    assert!(matches!(
        &result.outputs["n"],
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) if values == &[0]
    ));
}

/// A rule is evaluated without a relation context even when a derived
/// relation's predicate reads it, so a `relation_member` in that rule fails
/// in explain. Dense inlined the rule into the predicate and answered.
#[test]
fn dense_fails_a_relation_member_in_a_rule_a_derived_predicate_reads() {
    let member = member_of("member");
    let cases = vec![
        (
            "a person judgment rule",
            vec![judgment_rule("is_member", "Person", member.clone())],
            derived_ref("is_member"),
        ),
        (
            "the `if` condition of a person scalar rule",
            vec![rule(
                "member_flag",
                "Person",
                "integer",
                if_then_else(member.clone(), integer(1), integer(0)),
            )],
            compare(derived_ref("member_flag"), "eq", integer(1)),
        ),
    ];
    for (label, rules, predicate) in cases {
        let mut derived = rules;
        derived.push(rule("n", "Household", "integer", count("flagged", None)));
        let program = program(
            vec![
                base_relation("member"),
                derived_relation("flagged", "member", predicate),
            ],
            derived,
        );
        assert_eq!(
            assert_dense_matches_explain(label, &program, &["n"]),
            Err(RELATION_PREDICATE_OUTSIDE_DERIVED_RELATION.to_string()),
            "{label}"
        );
    }
}

// ---------------------------------------------------------------------------
// `relation_member` inside a derived relation's predicate
// ---------------------------------------------------------------------------

/// A derived relation filters its source's tuples, so a membership test of
/// that source (with the slots the derivation reads it with) holds for every
/// candidate, as does one of a source further up the chain. Dense can test it
/// without the tuples, under `and`/`or`/`not` and inside an `if` condition,
/// where explain keeps the relation context.
#[test]
fn dense_membership_of_a_derived_relations_own_sources_holds_for_every_tuple() {
    let has_income = || compare(income(), "gt", decimal("10"));
    let over = |relation: &str| {
        vec![
            rule("n", "Household", "integer", count(relation, None)),
            rule(
                "total",
                "Household",
                "decimal",
                sum(relation, income(), None),
            ),
        ]
    };
    let single = |label: &'static str, predicate: Value, expected: (Vec<i64>, Vec<i64>)| {
        (
            label,
            program(
                vec![
                    base_relation("member"),
                    derived_relation("earner", "member", predicate),
                ],
                over("earner"),
            ),
            expected,
        )
    };
    let cases = vec![
        single(
            "own source and a comparison",
            and(vec![member_of("member"), has_income()]),
            (vec![2, 0, 1], vec![150, 0, 20]),
        ),
        single(
            "negated own source",
            not(member_of("member")),
            (vec![0, 0, 0], vec![0, 0, 0]),
        ),
        single(
            "own source or a comparison",
            or(vec![has_income(), member_of("member")]),
            (vec![3, 0, 1], vec![150, 0, 20]),
        ),
        single(
            "own source in an `if` condition",
            compare(
                if_then_else(member_of("member"), income(), decimal("0")),
                "gt",
                decimal("10"),
            ),
            (vec![2, 0, 1], vec![150, 0, 20]),
        ),
        (
            "a derived source and the base source above it",
            program(
                vec![
                    base_relation("member"),
                    derived_relation("earner", "member", has_income()),
                    derived_relation(
                        "big_earner",
                        "earner",
                        and(vec![
                            member_of("earner"),
                            member_of("member"),
                            compare(income(), "gt", decimal("60")),
                        ]),
                    ),
                ],
                over("big_earner"),
            ),
            (vec![1, 0, 0], vec![100, 0, 0]),
        ),
    ];
    for (label, program, (counts, totals)) in cases {
        let expected = counts
            .iter()
            .zip(&totals)
            .map(|(count, total)| {
                vec![
                    Cell::Number(Decimal::from(*count)),
                    Cell::Number(Decimal::from(*total)),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            assert_dense_matches_explain(label, &program, &["n", "total"]),
            Ok(expected),
            "{label}"
        );
    }
}

/// Membership of any other relation needs that relation's tuples, which a
/// dense batch does not carry, so dense declines the program. Dense kept every
/// source tuple, answering 3 for h1 where explain filters by the other
/// relation (1 with residents, 0 without).
#[test]
fn dense_declines_a_derived_relation_testing_another_relations_membership() {
    let with_filter = |relations: Vec<Value>, predicate: Value| {
        let mut all = relations;
        all.push(derived_relation("filtered", "member", predicate));
        program(
            all,
            vec![rule("n", "Household", "integer", count("filtered", None))],
        )
    };
    let reversed_member = json!({
        "kind": "relation_member", "relation": "member", "current_slot": 1, "related_slot": 0,
    });
    let cases = vec![
        (
            "a second base relation (the issue's probe)",
            with_filter(
                vec![base_relation("member"), base_relation("resident")],
                member_of("resident"),
            ),
            "resident",
            vec![1, 0, 1],
        ),
        (
            "the source read the other way round",
            with_filter(vec![base_relation("member")], reversed_member),
            "member",
            vec![0, 0, 0],
        ),
        (
            "a sibling derived relation over the same source",
            with_filter(
                vec![
                    base_relation("member"),
                    derived_relation("earner", "member", compare(income(), "gt", decimal("10"))),
                ],
                member_of("earner"),
            ),
            "earner",
            vec![2, 0, 1],
        ),
    ];
    for (label, program, relation, expected) in cases {
        assert_eq!(
            explain(&program, &["n"]),
            Ok(numbers(&expected)),
            "{label}: explain filters by the other relation"
        );
        match dense(&program, &["n"]) {
            Err(DenseCompileError::Unsupported(message)) => assert!(
                message.contains(&format!("`relation_member` of `{relation}`"))
                    && message.contains("derived relation `filtered`"),
                "{label}: {message}"
            ),
            other => panic!("{label}: dense must decline, got {other:?}"),
        }
    }

    // Without resident tuples explain counts nobody; dense counted all three.
    let program = with_filter(
        vec![base_relation("member"), base_relation("resident")],
        member_of("resident"),
    );
    let mut request = json!({
        "mode": "explain",
        "program": program,
        "dataset": dataset(&program),
        "queries": [{ "entity_id": "h1", "period": period(), "outputs": ["n"] }],
    });
    request["dataset"]["relations"]
        .as_array_mut()
        .expect("tuples")
        .retain(|tuple| tuple["name"] != "resident");
    let response = execute_request(serde_json::from_value(request).expect("request parses"))
        .expect("explain answers");
    assert!(matches!(
        &response.results[0].outputs["n"],
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value: 0 },
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Programs explain refuses before evaluating
// ---------------------------------------------------------------------------

/// Explain refuses a rule that depends on itself through a derived relation
/// (#197) even when no queried rule reaches the cycle. Here `flagged`'s
/// predicate reads `is_flagged`, whose `relation_member` names `flagged`, and
/// only `n`, which counts `member`, is queried. Compiling and loading an
/// artifact both refuse it the same way, so the dense path never compiles it.
/// (`DenseCompiledProgram::from_program` takes a lowered program as given,
/// as `Engine::new` does.)
#[test]
fn dense_artifact_path_refuses_an_unused_relation_routed_cycle_as_explain_does() {
    let program = program(
        vec![
            base_relation("member"),
            derived_relation("flagged", "member", derived_ref("is_flagged")),
        ],
        vec![
            judgment_rule("is_flagged", "Person", member_of("flagged")),
            rule("n", "Household", "integer", count("member", None)),
        ],
    );
    let explain_error = explain(&program, &["n"]).expect_err("explain refuses the program");
    assert!(
        explain_error.contains("cycl"),
        "explain refuses the cycle: {explain_error}"
    );

    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program parses");
    assert!(matches!(
        CompiledProgramArtifact::compile(spec),
        Err(CompileError::CyclicDependency { .. })
    ));

    // An artifact whose embedded program gained the cycle after compilation
    // fails to load: loading recomputes the metadata from the program.
    let acyclic = self::program(
        vec![
            base_relation("member"),
            derived_relation("flagged", "member", derived_ref("is_flagged")),
        ],
        vec![
            judgment_rule("is_flagged", "Person", constant(true)),
            rule("n", "Household", "integer", count("member", None)),
        ],
    );
    let spec: ProgramSpec = serde_json::from_value(acyclic).expect("program parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("the acyclic program compiles");
    let mut json = serde_json::to_value(&artifact).expect("artifact serialises");
    let rules = json["program"]["derived"].as_array_mut().expect("rules");
    let is_flagged = rules
        .iter_mut()
        .find(|rule| rule["name"] == "is_flagged")
        .expect("is_flagged");
    is_flagged["expr"] = member_of("flagged");
    assert!(matches!(
        CompiledProgramArtifact::from_json_str(&json.to_string()),
        Err(CompileError::CyclicDependency { .. })
    ));
}
