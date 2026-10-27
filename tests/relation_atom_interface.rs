//! Interface derivation for relation atoms and term comparisons. Legacy
//! programs keep their tuple contract; relation-role programs make every
//! derived relation's tuple input unavailable.

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::interface::{InterfaceRelation, RelationProvenance, derive_interface};
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

fn program(predicate: Value) -> ProgramSpec {
    serde_json::from_value(json!({
        "relations": [
            {"name": "roster", "arity": 2, "slot_entities": ["Person", "Person"]},
            {"name": "triple", "arity": 3, "slot_entities": ["Person", "Person", "Person"]},
            {"name": "selected", "arity": 2, "slot_entities": ["Person", "Person"], "derivation": {
                "source_relation": "roster", "current_slot": 0, "related_slot": 1,
                "slot_entities": ["Person", "Person"], "predicate": predicate
            }},
            {"name": "otherwise_unused", "arity": 2, "slot_entities": ["Person", "Person"], "derivation": {
                "source_relation": "roster", "current_slot": 0, "related_slot": 1,
                "slot_entities": ["Person", "Person"], "predicate": always()
            }}
        ],
        "derived": [{
            "name": "count", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "selected", "current_slot": 0, "related_slot": 1}
        }]
    })).unwrap()
}

fn always() -> Value {
    json!({
        "kind": "comparison", "op": "eq",
        "left": {"kind": "literal", "value": {"kind": "integer", "value": 1}},
        "right": {"kind": "literal", "value": {"kind": "integer", "value": 1}}
    })
}

fn relation<'a>(relations: &'a [InterfaceRelation], name: &str) -> &'a InterfaceRelation {
    relations.iter().find(|entry| entry.name == name).unwrap()
}

#[test]
fn relation_atoms_read_relations_and_all_bound_slots() {
    let interface = derive_interface(&program(json!({
        "kind": "relation_atom", "relation": "triple", "terms": ["candidate", "anchor", "candidate"]
    })))
    .unwrap();
    assert!(interface.inputs.is_empty());
    assert_eq!(
        relation(&interface.relations, "triple").lookup_slots,
        [0, 1, 2]
    );
    let RelationProvenance::Derived { reads, .. } =
        &relation(&interface.relations, "selected").provenance
    else {
        panic!("derived relation")
    };
    assert_eq!(reads.relations, ["triple"]);
    assert!(reads.inputs.is_empty());
    assert!(reads.rules.is_empty());
    for entry in &interface.relations {
        assert_eq!(
            entry.accepts_supplied_tuples,
            matches!(entry.provenance, RelationProvenance::Supplied)
        );
    }
}

#[test]
fn term_comparisons_read_no_inputs_rules_or_relations() {
    let interface = derive_interface(&program(json!({
        "kind": "same_entity", "left": "candidate", "right": "anchor", "negated": true
    })))
    .unwrap();
    assert!(interface.inputs.is_empty());
    let RelationProvenance::Derived { reads, .. } =
        &relation(&interface.relations, "selected").provenance
    else {
        panic!("derived relation")
    };
    assert!(reads.inputs.is_empty());
    assert!(reads.rules.is_empty());
    assert!(reads.relations.is_empty());
    assert!(
        relation(&interface.relations, "triple")
            .lookup_slots
            .is_empty()
    );
    assert!(!relation(&interface.relations, "otherwise_unused").accepts_supplied_tuples);
}

#[test]
fn qualified_read_only_program_disables_all_derived_tuple_inputs() {
    let interface = derive_interface(&program(json!({
        "kind": "comparison", "op": "gt",
        "left": {"kind": "role_input", "term": "candidate", "name": "age"},
        "right": {"kind": "literal", "value": {"kind": "integer", "value": 18}}
    })))
    .unwrap();
    assert_eq!(interface.inputs[0].name, "age");
    assert_eq!(interface.inputs[0].entities, ["Person"]);
    assert!(!relation(&interface.relations, "selected").accepts_supplied_tuples);
    assert!(!relation(&interface.relations, "otherwise_unused").accepts_supplied_tuples);
    assert!(relation(&interface.relations, "roster").accepts_supplied_tuples);
}

#[test]
fn legacy_program_retains_tuple_acceptance() {
    let interface = derive_interface(&program(always())).unwrap();
    assert!(
        interface
            .relations
            .iter()
            .all(|relation| relation.accepts_supplied_tuples)
    );
}

#[test]
fn atoms_and_comparisons_require_the_containing_derivations_declared_kinds() {
    for predicate in [
        json!({"kind": "relation_atom", "relation": "triple", "terms": ["anchor", "candidate", "anchor"]}),
        json!({"kind": "same_entity", "left": "anchor", "right": "candidate", "negated": true}),
    ] {
        let mut value = serde_json::to_value(program(predicate)).unwrap();
        let selected = value["relations"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|relation| relation["name"] == "selected")
            .unwrap();
        selected["derivation"]
            .as_object_mut()
            .unwrap()
            .remove("slot_entities");
        let spec: ProgramSpec = serde_json::from_value(value).unwrap();
        // The relation and its source still establish effective kinds. Terms
        // specifically require the containing derivation's declaration.
        let model = spec.to_program().unwrap();
        assert_eq!(
            axiom_rules_engine::relation_typing::effective_slot_entities(&model, "selected"),
            Some(vec!["Person".to_string(), "Person".to_string()]),
        );
        let error = CompiledProgramArtifact::compile(spec).unwrap_err();
        assert!(
            error.to_string().contains("relation_roles_need_slot_kinds"),
            "{error}"
        );
    }
}

#[test]
fn derived_atom_targets_require_declared_kinds_even_when_their_source_is_typed() {
    let mut value = serde_json::to_value(program(json!({
        "kind": "relation_atom", "relation": "inherited", "terms": ["anchor", "candidate"]
    })))
    .unwrap();
    value["relations"].as_array_mut().unwrap().push(json!({
        "name": "inherited", "arity": 2, "derivation": {
            "source_relation": "roster", "current_slot": 0, "related_slot": 1,
            "predicate": always()
        }
    }));
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    let model = spec.to_program().unwrap();
    assert_eq!(
        axiom_rules_engine::relation_typing::effective_slot_entities(&model, "inherited"),
        Some(vec!["Person".to_string(), "Person".to_string()]),
    );
    let error = CompiledProgramArtifact::compile(spec).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("relation_atom_needs_typed_relation"),
        "{error}"
    );
}

#[test]
fn relation_role_interface_round_trips_and_rejects_tampering() {
    let artifact = CompiledProgramArtifact::compile(program(json!({
        "kind": "relation_atom", "relation": "triple", "terms": ["anchor", "candidate", "anchor"]
    })))
    .unwrap();
    let encoded = serde_json::to_string(&artifact).unwrap();
    let loaded = CompiledProgramArtifact::from_json_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&loaded).unwrap(), encoded);

    for (name, field, replacement) in [
        ("triple", "lookup_slots", json!([0])),
        ("selected", "accepts_supplied_tuples", json!(true)),
    ] {
        let mut tampered = serde_json::to_value(&artifact).unwrap();
        let relations = tampered["program"]["interface"]["relations"]
            .as_array_mut()
            .unwrap();
        let entry = relations
            .iter_mut()
            .find(|entry| entry["name"] == name)
            .unwrap();
        entry[field] = replacement;
        let error = CompiledProgramArtifact::from_json_str(&tampered.to_string()).unwrap_err();
        assert!(error.to_string().contains("interface"), "{error}");
    }
    let mut tampered = serde_json::to_value(&artifact).unwrap();
    let relations = tampered["program"]["interface"]["relations"]
        .as_array_mut()
        .unwrap();
    let entry = relations
        .iter_mut()
        .find(|entry| entry["name"] == "selected")
        .unwrap();
    entry["provenance"]["reads"]["relations"] = json!([]);
    let error = CompiledProgramArtifact::from_json_str(&tampered.to_string()).unwrap_err();
    assert!(error.to_string().contains("interface"), "{error}");
}
