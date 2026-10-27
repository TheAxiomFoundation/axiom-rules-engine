//! Atom programs order rules through the complete relation dependency graph.
//! Earlier artifacts keep their frozen order, including qualified-read and
//! term-comparison programs which do not use an atom.

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

fn integer(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn always() -> Value {
    json!({"kind": "comparison", "left": integer(1), "op": "eq", "right": integer(1)})
}

fn atom(relation: &str) -> Value {
    json!({"kind": "relation_atom", "relation": relation, "terms": ["anchor", "candidate"]})
}

fn relation(name: &str, source: &str, predicate: Value) -> Value {
    json!({"name": name, "arity": 2, "slot_entities": ["Person", "Person"],
        "derivation": {"source_relation": source, "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Person", "Person"], "predicate": predicate}})
}

fn score_test() -> Value {
    json!({"kind": "comparison", "op": "gt", "right": integer(0),
        "left": {"kind": "role_derived", "term": "candidate", "name": "z_score"}})
}

fn program(shape: &str) -> Value {
    let mut relations = vec![
        json!({"name": "base", "arity": 2, "slot_entities": ["Person", "Person"]}),
        relation("selected", "base", atom("inner")),
    ];
    match shape {
        "target" => relations.push(relation("inner", "base", score_test())),
        "source" => {
            relations.push(relation("inner", "inner_source", always()));
            relations.push(relation("inner_source", "base", score_test()));
        }
        "nested" => {
            relations.push(relation("inner", "base", atom("nested")));
            relations.push(relation("nested", "base", score_test()));
        }
        other => panic!("unknown test shape {other}"),
    }
    json!({"relations": relations, "derived": [
        {"name": "a_total", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "selected", "current_slot": 0, "related_slot": 1}},
        {"name": "b_rule", "entity": "Person", "dtype": "integer", "semantics": "scalar", "expr": integer(0)},
        {"name": "z_score", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "input", "name": "score"}},
    ]})
}

fn compile(value: Value) -> Result<CompiledProgramArtifact, CompileError> {
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    CompiledProgramArtifact::compile(spec)
}

#[test]
fn atom_order_follows_target_source_and_nested_atom_predicate_rules() {
    for shape in ["target", "source", "nested"] {
        let original = program(shape);
        for permutation in 0..6 {
            let mut value = original.clone();
            let rules = value["derived"].as_array_mut().unwrap();
            rules.rotate_left(permutation % 3);
            if permutation >= 3 {
                rules.reverse();
            }
            let relations = value["relations"].as_array_mut().unwrap();
            let len = relations.len();
            relations.rotate_left(permutation % len);
            if permutation % 2 == 0 {
                relations.reverse();
            }
            let artifact = compile(value).unwrap();
            assert_eq!(
                artifact.metadata.evaluation_order,
                ["b_rule", "z_score", "a_total"],
                "{shape}, permutation {permutation}"
            );
            let encoded = serde_json::to_string(&artifact).unwrap();
            let loaded = CompiledProgramArtifact::from_json_str(&encoded).unwrap();
            assert_eq!(serde_json::to_string(&loaded).unwrap(), encoded);
        }
    }
}

#[test]
fn programs_without_atoms_keep_their_frozen_order() {
    let membership = json!({"kind": "relation_member", "relation": "inner", "current_slot": 0, "related_slot": 1});
    let qualified = json!({"kind": "comparison", "op": "gt", "right": integer(0),
        "left": {"kind": "role_input", "term": "candidate", "name": "age"}});
    let comparison =
        json!({"kind": "same_entity", "left": "candidate", "right": "anchor", "negated": true});
    for predicate in [
        membership.clone(),
        json!({"kind": "and", "items": [membership.clone(), qualified]}),
        json!({"kind": "and", "items": [membership, comparison]}),
    ] {
        let mut value = program("target");
        value["relations"][1]["derivation"]["predicate"] = predicate;
        let artifact = compile(value).unwrap();
        assert_eq!(
            artifact.metadata.evaluation_order,
            ["a_total", "b_rule", "z_score"]
        );
    }
}

#[test]
fn atom_cycles_include_relations_read_by_only_related_rules() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "role_derived", "term": "candidate", "name": "z_score"
    });
    value["derived"][2] = json!({
        "name": "z_score", "entity": "Person", "dtype": "judgment", "semantics": "judgment",
        "expr": {"kind": "comparison", "op": "eq",
            "left": {"kind": "only_related", "relation": "selected", "current_slot": 0, "related_slot": 1,
                "value": {"kind": "input", "name": "flag"}},
            "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}}
    });
    let error = compile(value).unwrap_err();
    assert!(
        matches!(error, CompileError::CyclicDependency { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("z_score"), "{error}");
}

#[test]
fn atom_order_includes_only_related_reads_inside_predicates() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "comparison", "op": "eq",
        "left": {"kind": "only_related", "relation": "inner_source", "current_slot": 0, "related_slot": 1,
            "value": {"kind": "input", "name": "flag"}},
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    value["relations"]
        .as_array_mut()
        .unwrap()
        .push(relation("inner_source", "base", score_test()));
    let artifact = compile(value).unwrap();
    assert_eq!(
        artifact.metadata.evaluation_order,
        ["b_rule", "z_score", "a_total"]
    );
}

#[test]
fn unused_atom_cycles_through_only_related_are_refused() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "comparison", "op": "eq",
        "left": {"kind": "only_related", "relation": "selected", "current_slot": 0, "related_slot": 1,
            "value": {"kind": "input", "name": "flag"}},
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    // Neither remaining rule reads either relation: this must be refused
    // even when there is no unfinished rule node for the cycle to block.
    value["derived"].as_array_mut().unwrap().remove(0);
    let error = compile(value).unwrap_err();
    assert!(
        matches!(error, CompileError::CyclicRelationDependency { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("inner"), "{error}");
    assert!(error.to_string().contains("selected"), "{error}");
}
