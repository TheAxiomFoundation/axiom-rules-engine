//! Model, kind, dependency and execution-mode boundaries for bounded exists.
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::spec::{ProgramSpec, TermSpec};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};
use std::cell::Cell;

fn integer(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn always() -> Value {
    json!({"kind": "comparison", "left": integer(1), "op": "eq", "right": integer(1)})
}

fn exists(relation: &str, body: Value) -> Value {
    json!({"kind": "exists", "relation": relation,
        "terms": ["anchor", {"var": 0}], "variable_slot": 1, "body": body})
}

fn relation(name: &str, predicate: Value) -> Value {
    json!({"name": name, "arity": 2, "slot_entities": ["Person", "Person"],
        "derivation": {"source_relation": "roster", "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Person", "Person"], "predicate": predicate}})
}

fn program(predicate: Value) -> Value {
    json!({"relations": [
        {"name": "roster", "arity": 2, "slot_entities": ["Person", "Person"]},
        {"name": "ties", "arity": 2, "slot_entities": ["Person", "Person"]},
        relation("selected", predicate)
    ], "derived": [
        {"name": "a_total", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "selected", "current_slot": 0, "related_slot": 1}},
        {"name": "z_score", "entity": "Person", "dtype": "integer", "semantics": "scalar", "expr": integer(1)}
    ]})
}

fn compile(value: Value) -> Result<CompiledProgramArtifact, String> {
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    CompiledProgramArtifact::compile(spec).map_err(|error| error.to_string())
}

fn score(term: Value) -> Value {
    json!({"kind": "comparison", "left": {"kind": "role_derived", "name": "z_score", "term": term},
        "op": "eq", "right": integer(1)})
}

#[test]
fn variable_artifact_form_keeps_legacy_terms_and_round_trips() {
    assert_eq!(
        serde_json::to_value(TermSpec::Anchor).unwrap(),
        json!("anchor")
    );
    assert_eq!(
        serde_json::to_value(TermSpec::Candidate).unwrap(),
        json!("candidate")
    );
    assert_eq!(
        serde_json::to_value(TermSpec::Var(3)).unwrap(),
        json!({"var": 3})
    );
    for value in [json!({"id": "literal"}), json!("id")] {
        assert!(serde_json::from_value::<TermSpec>(value).is_err());
    }
    let compiled = compile(program(exists("ties", score(json!({"var": 0}))))).unwrap();
    let encoded = serde_json::to_string(&compiled).unwrap();
    let loaded = CompiledProgramArtifact::from_json_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&loaded).unwrap(), encoded);
}

#[test]
fn raw_variable_indices_and_generator_marker_are_checked() {
    let out_of_scope = [
        program(score(json!({"var": 0}))),
        program(exists("ties", score(json!({"var": 1})))),
        program(exists("ties", exists("ties", score(json!({"var": 2}))))),
    ];
    for value in out_of_scope {
        let error = compile(value).unwrap_err();
        assert!(error.contains("relation_variable_out_of_scope"), "{error}");
    }
    for slot in [0, 2, usize::MAX] {
        let mut predicate = exists("ties", always());
        predicate["variable_slot"] = json!(slot);
        let error = compile(program(predicate)).unwrap_err();
        assert!(error.contains("relation_exists_variable_count"), "{error}");
    }
}

#[test]
fn exists_adds_generator_and_body_edges_to_evaluation_order() {
    let mut value = program(exists("inner", always()));
    value["relations"]
        .as_array_mut()
        .unwrap()
        .push(relation("inner", score(json!("candidate"))));
    let artifact = compile(value).unwrap();
    assert_eq!(artifact.metadata.evaluation_order, ["z_score", "a_total"]);
    let artifact = compile(program(exists("ties", score(json!({"var": 0}))))).unwrap();
    assert_eq!(artifact.metadata.evaluation_order, ["z_score", "a_total"]);
}

#[test]
fn exists_refuses_generator_and_body_cycles() {
    for predicate in [
        exists("selected", always()),
        exists(
            "ties",
            json!({"kind": "comparison", "op": "gt", "right": integer(0),
            "left": {"kind": "role_derived", "name": "a_total", "term": {"var": 0}}}),
        ),
    ] {
        let error = compile(program(predicate)).unwrap_err();
        assert!(error.to_lowercase().contains("cycl"), "{error}");
    }
}

#[test]
fn dense_refuses_exists_by_name_and_issue() {
    let compiled = compile(program(exists("ties", always()))).unwrap();
    let error = DenseCompiledProgram::from_artifact(&compiled, Some("Person"))
        .err()
        .expect("dense refusal");
    assert!(matches!(error, DenseCompileError::Unsupported(_)));
    let message = error.to_string();
    assert!(
        message.contains("exists") && message.contains("#218"),
        "{message}"
    );
}

fn fixed_runner(seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases: 96,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[test]
fn variable_kind_comes_from_its_generator_slot() {
    let accepted = Cell::new(0);
    let refused = Cell::new(0);
    let nested = Cell::new(0);
    fixed_runner(0x39)
        .run(&(any::<bool>(), 1usize..5), |(same_kind, depth)| {
            let mut predicate = score(json!({"var": depth - 1}));
            for index in 0..depth {
                predicate = exists(if index + 1 == depth { "outer" } else { "ties" }, predicate);
            }
            let mut value = program(predicate);
            value["relations"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name": "outer", "arity": 2,
            "slot_entities": ["Person", if same_kind { "Person" } else { "Household" }]}));
            // Inner generators use the predicate's original anchor; the rule
            // read addresses the outermost frame independently of those frames.
            match compile(value) {
                Ok(_) => {
                    accepted.set(accepted.get() + 1);
                    if depth > 1 {
                        nested.set(nested.get() + 1);
                    }
                    prop_assert!(same_kind);
                }
                Err(error) => {
                    prop_assert!(!same_kind);
                    prop_assert!(error.contains("relation_role_entity_mismatch"), "{}", error);
                    refused.set(refused.get() + 1);
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(
        accepted.get() >= 20,
        "matching kinds reached {} compilations",
        accepted.get()
    );
    assert!(
        refused.get() >= 20,
        "mismatching kinds reached {} refusals",
        refused.get()
    );
    assert!(
        nested.get() >= 16,
        "nested frames reached {} successful compilations",
        nested.get()
    );
}

#[test]
fn variable_scope_is_exactly_the_enclosing_frame_count() {
    let accepted = Cell::new(0);
    let refused = Cell::new(0);
    let nested = Cell::new(0);
    fixed_runner(0x61)
        .run(&(0usize..5, 0usize..6), |(depth, index)| {
            let mut predicate = score(json!({"var": index}));
            for _ in 0..depth {
                predicate = exists("ties", predicate);
            }
            match compile(program(predicate)) {
                Ok(_) => {
                    prop_assert!(index < depth);
                    accepted.set(accepted.get() + 1);
                    if depth > 1 {
                        nested.set(nested.get() + 1);
                    }
                }
                Err(error) => {
                    prop_assert!(index >= depth);
                    prop_assert!(
                        error.contains("relation_variable_out_of_scope"),
                        "{}",
                        error
                    );
                    refused.set(refused.get() + 1);
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(
        accepted.get() >= 12,
        "in-scope reads reached {} compilations",
        accepted.get()
    );
    assert!(
        refused.get() >= 30,
        "out-of-scope reads reached {} refusals",
        refused.get()
    );
    assert!(
        nested.get() >= 8,
        "nested frames reached {} successful compilations",
        nested.get()
    );
}

#[test]
fn imported_relations_used_only_by_generator_and_body_survive_and_are_rewritten() {
    struct Modules(std::collections::HashMap<String, String>);
    impl axiom_rules_engine::source::ModuleSource for Modules {
        fn load(
            &self,
            target: &str,
        ) -> Result<Option<String>, axiom_rules_engine::source::SourceError> {
            Ok(self.0.get(target).cloned())
        }
    }
    let imported = r#"
format: rulespec/v1
rules:
  - name: imported_pair
    kind: data_relation
    data_relation: {arity: 2, arguments: [Person, Person]}
  - name: imported_body
    kind: data_relation
    data_relation: {arity: 2, arguments: [Person, Person]}
"#;
    let root = r#"
format: rulespec/v1
imports:
  - us:policies/exists-pair
rules:
  - name: roster
    kind: data_relation
    data_relation: {arity: 2, arguments: [Person, Person]}
  - name: selected
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: roster
      current_slot: 0
      related_slot: 1
      slot_entities: [Person, Person]
    versions:
      - effective_from: 2026-01-01
        formula: exists(imported_pair(anchor, p), imported_body(p, candidate))
  - name: total
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(selected)
"#;
    let source = Modules(std::collections::HashMap::from([
        ("us:policies/exists-pair".into(), imported.into()),
        ("us:policies/exists-root".into(), root.into()),
    ]));
    let artifact =
        CompiledProgramArtifact::from_rulespec_with_source("us:policies/exists-root", &source)
            .unwrap();
    let model = serde_json::to_value(&artifact.program).unwrap();
    let relations = model["relations"].as_array().unwrap();
    for short in ["imported_pair", "imported_body"] {
        let qualified = format!("us:policies/exists-pair#relation.{short}");
        assert!(
            relations
                .iter()
                .any(|relation| relation["name"] == qualified)
        );
    }
    let selected = relations
        .iter()
        .find(|relation| relation["name"] == "us:policies/exists-root#relation.selected")
        .unwrap();
    let predicate = &selected["derivation"]["predicate"];
    assert_eq!(
        predicate["relation"],
        "us:policies/exists-pair#relation.imported_pair"
    );
    assert_eq!(
        predicate["body"]["relation"],
        "us:policies/exists-pair#relation.imported_body"
    );
}

#[test]
fn derived_generator_of_arity_other_than_two_refuses_by_name() {
    let mut value = program(json!({"kind": "exists", "relation": "triple_derived",
        "terms": ["anchor", "candidate", {"var": 0}], "variable_slot": 2, "body": always()}));
    value["relations"].as_array_mut().unwrap().extend([
        json!({"name": "triple_source", "arity": 3, "slot_entities": ["Person", "Person", "Person"]}),
        json!({"name": "triple_derived", "arity": 3, "slot_entities": ["Person", "Person", "Person"],
            "derivation": {"source_relation": "triple_source", "current_slot": 0, "related_slot": 2,
                "slot_entities": ["Person", "Person", "Person"], "predicate": always()}}),
    ]);
    let error = compile(value).unwrap_err();
    assert!(
        error.contains("derived_relation_atom_arity_unsupported"),
        "{error}"
    );
}
