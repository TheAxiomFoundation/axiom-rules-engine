//! `single_related` (the general `only(r.f)`) against main's `only_related`
//! (#261), which projects a Boolean input only.
//!
//! The two are separate kinds because `ScalarExprSpec` does not deny unknown
//! fields: an engine that knows only `only_related` would load a
//! `single_related` with a `where` clause or a derived value and drop them.
//! Where both apply, a Boolean input and no `where` clause, they must be the
//! same function. This checks that on generated data in explain, fast and
//! dense (Decimal and f64): the same value, or the same error with the same
//! fields.
//!
//! One difference is intended and not generated here: `only_related` refuses
//! a non-Boolean value with `TypeMismatch`, while `single_related` returns it
//! with its own kind.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec,
};
use axiom_rules_engine::rulespec::lower_rulespec_str;
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
use serde_json::{Value, json};

const HOUSEHOLDS: usize = 3;

/// One `decision(person, household)` tuple. A partial tuple covers only the
/// first half of the month, so it does not cover the queried period.
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
}

fn expr(kind: &str) -> Value {
    let mut expr = json!({
        "kind": kind,
        "relation": "decision",
        "current_slot": 0,
        "related_slot": 1,
        "value": {"kind": "input", "name": "refused"},
    });
    if kind == "single_related" {
        expr["where"] = Value::Null;
    }
    expr
}

fn program(kind: &str) -> Value {
    json!({
        "relations": [{"name": "decision", "arity": 2, "slot_entities": ["Person", "Household"]}],
        "derived": [{
            "name": "projected",
            "entity": "Person",
            "dtype": "boolean",
            "semantics": "scalar",
            "expr": expr(kind),
        }],
    })
}

fn request(kind: &str, mode: &str, case: &Case, person: usize) -> ExecutionRequest {
    let inputs: Vec<Value> = case
        .refused
        .iter()
        .enumerate()
        .filter_map(|(household, value)| {
            value.map(|value| {
                json!({
                    "name": "refused",
                    "entity": "Household",
                    "entity_id": format!("h{household}"),
                    "interval": {"start": "2026-01-01", "end": "2026-01-31"},
                    "value": {"kind": "bool", "value": value},
                })
            })
        })
        .collect();
    let relations: Vec<Value> = case
        .edges
        .iter()
        .map(|edge| {
            json!({
                "name": "decision",
                "tuple": [format!("p{}", edge.person), format!("h{}", edge.household)],
                "interval": {
                    "start": "2026-01-01",
                    "end": if edge.partial { "2026-01-15" } else { "2026-01-31" },
                },
            })
        })
        .collect();
    serde_json::from_value(json!({
        "mode": mode,
        "program": program(kind),
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

/// Dense rows are the people; each person's related rows are their full
/// tuples in order (dense has no intervals, so partial tuples are left out).
/// `refused` gives each related row its household's value; when a reached
/// household has no record the column is left out, which dense reports as a
/// missing input.
fn dense(kind: &str, case: &Case, f64_mode: bool) -> String {
    let spec: ProgramSpec = serde_json::from_value(program(kind)).expect("program parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("program compiles");
    let compiled =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");
    let mut offsets = vec![0];
    let mut values = Vec::new();
    let mut complete = true;
    for person in 0..case.people {
        for edge in case
            .edges
            .iter()
            .filter(|edge| edge.person == person && !edge.partial)
        {
            match case.refused[edge.household] {
                Some(value) => values.push(value),
                None => {
                    complete = false;
                    values.push(false);
                }
            }
        }
        offsets.push(values.len());
    }
    let mut inputs = HashMap::new();
    if complete {
        inputs.insert("refused".to_string(), DenseColumn::Bool(values));
    }
    let key = compiled.relations()[0].key.clone();
    let batch = DenseBatchSpec {
        row_count: case.people,
        inputs: HashMap::new(),
        relations: HashMap::from([(key, DenseRelationBatchSpec { offsets, inputs })]),
    };
    let period = request(kind, "explain", case, 0).queries[0]
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
        Ok(result) => format!("{:?}", result.outputs),
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
        )
            .prop_map(|(people, edges, refused)| Case {
                people,
                edges,
                refused,
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn single_related_equals_only_related_on_a_boolean_input(case in cases()) {
        for mode in ["explain", "fast"] {
            for person in 0..case.people {
                prop_assert_eq!(
                    sparse("single_related", mode, &case, person),
                    sparse("only_related", mode, &case, person),
                    "{} mode, person p{}", mode, person
                );
            }
        }
        for f64_mode in [false, true] {
            prop_assert_eq!(
                dense("single_related", &case, f64_mode),
                dense("only_related", &case, f64_mode),
                "dense, f64 {}", f64_mode
            );
        }
    }
}

/// The generator reaches every outcome the property compares: a value, no
/// member, several members, a repeated tuple, a member with no record and a
/// tuple that does not cover the period.
#[test]
fn the_examples_reach_every_outcome() {
    let edge = |person, household, partial| Edge {
        person,
        household,
        partial,
    };
    let case = Case {
        people: 3,
        edges: vec![
            edge(0, 0, false),
            edge(1, 0, false),
            edge(1, 1, false),
            edge(2, 2, true),
        ],
        refused: vec![Some(true), Some(false), None],
    };
    let outcomes: Vec<String> = (0..3)
        .map(|person| sparse("only_related", "explain", &case, person))
        .collect();
    assert_eq!(outcomes[0], r#"{"kind":"bool","value":true}"#);
    assert!(outcomes[1].contains("AmbiguousRelation"), "{}", outcomes[1]);
    assert!(outcomes[2].contains("MissingRelation"), "{}", outcomes[2]);
    let missing_input = Case {
        people: 1,
        edges: vec![edge(0, 2, false)],
        refused: vec![Some(true), Some(false), None],
    };
    for kind in ["only_related", "single_related"] {
        assert!(sparse(kind, "explain", &missing_input, 0).contains("MissingInput"));
        assert!(dense(kind, &missing_input, false).contains("MissingInput"));
    }
    let several = Case {
        people: 1,
        edges: vec![edge(0, 0, false), edge(0, 1, false)],
        refused: vec![Some(true), Some(false), None],
    };
    for kind in ["only_related", "single_related"] {
        assert!(dense(kind, &several, false).contains("AmbiguousRelation"));
        assert!(dense(kind, &case, false).contains("AmbiguousRelation"));
    }
    // A repeated tuple: explain reads the distinct ids, so one household;
    // dense rows are the tuples, so two (docs/only-related.md). Both kinds
    // agree within each mode.
    let repeated = Case {
        people: 1,
        edges: vec![edge(0, 0, false), edge(0, 0, false)],
        refused: vec![Some(true), Some(false), None],
    };
    for kind in ["only_related", "single_related"] {
        assert_eq!(
            sparse(kind, "explain", &repeated, 0),
            r#"{"kind":"bool","value":true}"#
        );
        assert!(dense(kind, &repeated, false).contains("AmbiguousRelation"));
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
