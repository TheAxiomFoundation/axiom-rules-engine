//! `single_related` (the general `only(r.f)`) against main's `only_related`
//! (#261), which projects a Boolean input only.
//!
//! The two are separate kinds because `ScalarExprSpec` does not deny unknown
//! fields: an engine that knows only `only_related` would load one carrying
//! a `where` clause and ignore the clause, answering over every member with
//! no error for the dropped clause. (It refuses a derived value or any slot
//! pair other than 0 and 1 at load, and a non-Boolean input at evaluation.)
//! Where both apply, a Boolean input and no
//! `where` clause, they must be the same function. This checks that on
//! generated data in explain, fast and dense (Decimal and f64): the same
//! value, or the same error with the same fields. Each query reads one
//! person. A dense batch stops at its first failing row, so dense runs each
//! person alone and then all of them in one batch.
//!
//! One difference is intended and not generated here: `only_related` refuses
//! a non-Boolean value with `TypeMismatch`, while `single_related` returns it
//! with its own kind.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
};
use axiom_rules_engine::rulespec::lower_rulespec_str;
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
use serde_json::{Value, json};

const HOUSEHOLDS: usize = 3;

/// One `decision` tuple between a person and a household. A partial tuple
/// covers only the first half of the month, so it does not cover the queried
/// period.
#[derive(Clone, Debug)]
struct Edge {
    person: usize,
    household: usize,
    partial: bool,
}

#[derive(Clone, Debug)]
struct Case {
    people: usize,
    edges: Vec<Edge>,
    /// Each household's `refused`, or `None` when it has no record.
    refused: Vec<Option<bool>>,
    /// Whether `decision` is written household first and read at slots
    /// (1, 0), rather than person first at (0, 1).
    reversed: bool,
    /// When set, the query reads `kept`, derived from `decision`, which keeps
    /// the households whose `listed` is true; these are the households'
    /// `listed`. When `None` the query reads `decision` itself.
    listed: Option<Vec<bool>>,
}

impl Case {
    fn slots(&self) -> (usize, usize) {
        if self.reversed { (1, 0) } else { (0, 1) }
    }

    fn slot_entities(&self) -> Value {
        if self.reversed {
            json!(["Household", "Person"])
        } else {
            json!(["Person", "Household"])
        }
    }

    fn relation(&self) -> &'static str {
        if self.listed.is_some() {
            "kept"
        } else {
            "decision"
        }
    }

    /// The households of `person`'s tuples that cover the period, in order.
    fn full_households(&self, person: usize) -> Vec<usize> {
        self.edges
            .iter()
            .filter(|edge| edge.person == person && !edge.partial)
            .map(|edge| edge.household)
            .collect()
    }
}

fn expr(kind: &str, case: &Case) -> Value {
    let (current_slot, related_slot) = case.slots();
    let mut expr = json!({
        "kind": kind,
        "relation": case.relation(),
        "current_slot": current_slot,
        "related_slot": related_slot,
        "value": {"kind": "input", "name": "refused"},
    });
    if kind == "single_related" {
        expr["where"] = Value::Null;
    }
    expr
}

fn program(kind: &str, case: &Case) -> Value {
    let (current_slot, related_slot) = case.slots();
    let mut relations = vec![json!({
        "name": "decision",
        "arity": 2,
        "slot_entities": case.slot_entities(),
    })];
    if case.listed.is_some() {
        relations.push(json!({
            "name": "kept",
            "arity": 2,
            "slot_entities": case.slot_entities(),
            "derivation": {
                "source_relation": "decision",
                "current_slot": current_slot,
                "related_slot": related_slot,
                "slot_entities": case.slot_entities(),
                "predicate": {
                    "kind": "comparison",
                    "left": {"kind": "input", "name": "listed"},
                    "op": "eq",
                    "right": {"kind": "literal", "value": {"kind": "bool", "value": true}},
                },
            },
        }));
    }
    json!({
        "relations": relations,
        "derived": [{
            "name": "projected",
            "entity": "Person",
            "dtype": "boolean",
            "semantics": "scalar",
            "expr": expr(kind, case),
        }],
    })
}

fn request(kind: &str, mode: &str, case: &Case, person: usize) -> ExecutionRequest {
    let interval = json!({"start": "2026-01-01", "end": "2026-01-31"});
    let record = |name: &str, household: usize, value: bool| {
        json!({
            "name": name,
            "entity": "Household",
            "entity_id": format!("h{household}"),
            "interval": interval,
            "value": {"kind": "bool", "value": value},
        })
    };
    let mut inputs: Vec<Value> = case
        .refused
        .iter()
        .enumerate()
        .filter_map(|(household, value)| value.map(|value| record("refused", household, value)))
        .collect();
    if let Some(listed) = &case.listed {
        inputs.extend(
            listed
                .iter()
                .enumerate()
                .map(|(household, value)| record("listed", household, *value)),
        );
    }
    let relations: Vec<Value> = case
        .edges
        .iter()
        .map(|edge| {
            let person = format!("p{}", edge.person);
            let household = format!("h{}", edge.household);
            json!({
                "name": "decision",
                "tuple": if case.reversed { [household, person] } else { [person, household] },
                "interval": {
                    "start": "2026-01-01",
                    "end": if edge.partial { "2026-01-15" } else { "2026-01-31" },
                },
            })
        })
        .collect();
    serde_json::from_value(json!({
        "mode": mode,
        "program": program(kind, case),
        "dataset": {"inputs": inputs, "relations": relations},
        "queries": [{
            "entity_id": format!("p{person}"),
            "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
            "outputs": ["projected"],
        }],
    }))
    .expect("request parses")
}

/// The output value, or the error, of one query. Traces are left out: they
/// spell the expression's kind.
fn sparse(kind: &str, mode: &str, case: &Case, person: usize) -> String {
    match execute_request(request(kind, mode, case, person)) {
        Ok(response) => {
            let response = serde_json::to_value(response).expect("response serializes");
            response["results"][0]["outputs"]["projected"]["value"].to_string()
        }
        Err(error) => format!("{error:?}"),
    }
}

fn dense_program(kind: &str, case: &Case) -> DenseCompiledProgram {
    let spec: ProgramSpec = serde_json::from_value(program(kind, case)).expect("program parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("program compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles")
}

/// One batch whose rows are `people`, in order. A batch stops at its first
/// failing row, so the property runs each person alone as well as all of
/// them together. A row's related rows are its full tuples in order (dense
/// has no intervals, so partial tuples are left out, and a repeated tuple is
/// two rows). When a related row's household has no `refused` record the
/// column is left out, which dense reports as a missing input; the batch
/// states its related row count, since a left-out column cannot confirm it.
fn dense(compiled: &DenseCompiledProgram, case: &Case, people: &[usize], f64_mode: bool) -> String {
    let mut offsets = vec![0];
    let mut households = Vec::new();
    for &person in people {
        households.extend(case.full_households(person));
        offsets.push(households.len());
    }
    let mut relations: HashMap<_, DenseRelationBatchSpec> = HashMap::new();
    for schema in compiled.relations() {
        let batch = relations
            .entry(schema.key.clone())
            .or_insert_with(|| DenseRelationBatchSpec {
                offsets: offsets.clone(),
                inputs: HashMap::new(),
                related_row_count: Some(households.len()),
                ..Default::default()
            });
        for name in &schema.related_inputs {
            let column: Option<Vec<bool>> = households
                .iter()
                .map(|&household| match name.as_str() {
                    "refused" => case.refused[household],
                    "listed" => case.listed.as_ref().map(|listed| listed[household]),
                    other => panic!("unexpected related input `{other}`"),
                })
                .collect();
            if let Some(values) = column {
                batch.inputs.insert(name.clone(), DenseColumn::Bool(values));
            }
        }
    }
    let batch = DenseBatchSpec {
        row_count: people.len(),
        inputs: HashMap::new(),
        relations,
    };
    let period = request("only_related", "explain", case, 0).queries[0]
        .period
        .to_model()
        .expect("period");
    let outputs = ["projected".to_string()];
    let result = if f64_mode {
        compiled.execute_f64(&period, batch, &outputs)
    } else {
        compiled.execute(&period, batch, &outputs)
    };
    match result {
        Ok(result) => match result.outputs.get("projected") {
            Some(DenseOutputValue::Scalar(DenseColumn::Bool(values))) => format!("{values:?}"),
            other => panic!("`projected` is a Boolean column, got {other:?}"),
        },
        Err(error) => format!("{error:?}"),
    }
}

fn cases() -> impl Strategy<Value = Case> {
    (1..=3_usize).prop_flat_map(|people| {
        let edge = (0..people, 0..HOUSEHOLDS, proptest::bool::weighted(0.2)).prop_map(
            |(person, household, partial)| Edge {
                person,
                household,
                partial,
            },
        );
        (
            Just(people),
            proptest::collection::vec(edge, 0..=6),
            proptest::collection::vec(proptest::option::weighted(0.85, any::<bool>()), HOUSEHOLDS),
            any::<bool>(),
            proptest::option::of(proptest::collection::vec(any::<bool>(), HOUSEHOLDS)),
        )
            .prop_map(|(people, edges, refused, reversed, listed)| Case {
                people,
                edges,
                refused,
                reversed,
                listed,
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn single_related_equals_only_related_on_a_boolean_input(case in cases()) {
        let single = dense_program("single_related", &case);
        let only = dense_program("only_related", &case);
        for person in 0..case.people {
            for mode in ["explain", "fast"] {
                prop_assert_eq!(
                    sparse("single_related", mode, &case, person),
                    sparse("only_related", mode, &case, person),
                    "{} mode, person p{}", mode, person
                );
            }
            for f64_mode in [false, true] {
                prop_assert_eq!(
                    dense(&single, &case, &[person], f64_mode),
                    dense(&only, &case, &[person], f64_mode),
                    "dense, f64 {}, person p{}", f64_mode, person
                );
            }
        }
        // All people in one batch: the first failing row's error, which
        // names that row's index.
        let everyone: Vec<usize> = (0..case.people).collect();
        for f64_mode in [false, true] {
            prop_assert_eq!(
                dense(&single, &case, &everyone, f64_mode),
                dense(&only, &case, &everyone, f64_mode),
                "dense, f64 {}, every person", f64_mode
            );
        }
    }
}

fn edge(person: usize, household: usize, partial: bool) -> Edge {
    Edge {
        person,
        household,
        partial,
    }
}

fn case(people: usize, edges: Vec<Edge>) -> Case {
    Case {
        people,
        edges,
        refused: vec![Some(true), Some(false), None],
        reversed: false,
        listed: None,
    }
}

/// Every outcome the property compares, in each mode, for both kinds.
#[test]
fn the_examples_reach_every_outcome() {
    let mixed = case(
        3,
        vec![
            edge(0, 0, false),
            edge(1, 0, false),
            edge(1, 1, false),
            edge(2, 2, true),
        ],
    );
    let missing_input = case(1, vec![edge(0, 2, false)]);
    // A repeated tuple: explain reads the distinct ids, so one household;
    // dense rows are the tuples, so two (docs/only-related.md).
    let repeated = case(1, vec![edge(0, 0, false), edge(0, 0, false)]);
    // Read household first through `kept`: of p0's two households only h0 is
    // listed, so the read is h0's where `decision` itself is ambiguous.
    let kept = Case {
        reversed: true,
        listed: Some(vec![true, false, false]),
        ..case(1, vec![edge(0, 0, false), edge(0, 1, false)])
    };
    let unkept = Case {
        listed: Some(vec![false, false, true]),
        ..kept.clone()
    };
    for kind in ["only_related", "single_related"] {
        for mode in ["explain", "fast"] {
            let expect = |case: &Case, person: usize, outcome: &str| {
                let got = sparse(kind, mode, case, person);
                assert!(got.contains(outcome), "{kind} {mode} p{person}: {got}");
            };
            expect(&mixed, 0, r#"{"kind":"bool","value":true}"#);
            expect(&mixed, 1, "AmbiguousRelation");
            expect(&mixed, 2, "MissingRelation");
            expect(&missing_input, 0, "MissingInput");
            expect(&repeated, 0, r#"{"kind":"bool","value":true}"#);
            expect(&kept, 0, r#"{"kind":"bool","value":true}"#);
            expect(&unkept, 0, r#"MissingRelation { relation: "kept""#);
        }
        for f64_mode in [false, true] {
            let expect = |case: &Case, person: usize, outcome: &str| {
                let got = dense(&dense_program(kind, case), case, &[person], f64_mode);
                assert!(
                    got.contains(outcome),
                    "{kind} dense f64 {f64_mode} p{person}: {got}"
                );
            };
            expect(&mixed, 0, "[true]");
            expect(&mixed, 1, "AmbiguousRelation");
            expect(&mixed, 2, "MissingRelation");
            expect(&missing_input, 0, "MissingInput");
            expect(&repeated, 0, "AmbiguousRelation");
            expect(&kept, 0, "[true]");
            // The relation as the formula names it, not `decision`, the base
            // relation its dense batch is keyed to.
            expect(&unkept, 0, r#"MissingRelation { relation: "kept""#);
            // All three people in one batch: p0 reads a value and p1 is the
            // first to fail, so the error names row 1.
            let got = dense(&dense_program(kind, &mixed), &mixed, &[0, 1, 2], f64_mode);
            assert!(
                got.contains(r#"AmbiguousRelation { relation: "decision", entity_id: "1""#),
                "{kind} dense f64 {f64_mode} every person: {got}"
            );
        }
    }
}

/// RuleSpec spells the general form `only(r.f)` and main's form
/// `only_related(r, f)`; each lowers to its own kind.
#[test]
fn rulespec_lowers_each_spelling_to_its_own_kind() {
    let source = |formula: &str| {
        format!(
            r#"
format: rulespec/v1
inputs:
  - name: refused
    entity: Household
    dtype: Boolean
rules:
  - name: decision
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: projected
    kind: derived
    entity: Person
    dtype: Boolean
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: {formula}
"#
        )
    };
    let general = serde_json::to_string(
        &lower_rulespec_str(&source("only(decision.refused)")).expect("only lowers"),
    )
    .expect("serializes");
    let boolean = serde_json::to_string(
        &lower_rulespec_str(&source("only_related(decision, refused)"))
            .expect("only_related lowers"),
    )
    .expect("serializes");
    assert!(general.contains(r#""kind":"single_related""#), "{general}");
    assert!(!general.contains(r#""kind":"only_related""#), "{general}");
    assert!(boolean.contains(r#""kind":"only_related""#), "{boolean}");
    assert!(!boolean.contains(r#""kind":"single_related""#), "{boolean}");
}
