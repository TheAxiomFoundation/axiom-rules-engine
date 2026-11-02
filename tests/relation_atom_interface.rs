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

/// A derived atom resolves members for its current-slot term and tests the
/// related term against them, as a membership test does, so the interface
/// records the same traversal for both.
#[test]
fn derived_atoms_record_the_current_slot_like_membership_tests() {
    for current in [0, 1] {
        let lookups = |predicate: Value| {
            let mut spec = serde_json::to_value(program(predicate)).unwrap();
            assert_eq!(spec["relations"][3]["name"], "otherwise_unused");
            let derivation = &mut spec["relations"][3]["derivation"];
            derivation["current_slot"] = json!(current);
            derivation["related_slot"] = json!(1 - current);
            let interface =
                derive_interface(&serde_json::from_value::<ProgramSpec>(spec).unwrap()).unwrap();
            relation(&interface.relations, "otherwise_unused")
                .lookup_slots
                .clone()
        };
        let atom = lookups(json!({
            "kind": "relation_atom", "relation": "otherwise_unused", "terms": ["anchor", "candidate"]
        }));
        assert_eq!(atom, [current]);
        assert_eq!(
            atom,
            lookups(json!({
                "kind": "relation_member", "relation": "otherwise_unused",
                "current_slot": current, "related_slot": 1 - current
            }))
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

fn distinct_kind_rulespec(target_kinds: bool, owner_kinds: bool, predicate: &str) -> String {
    let predicate = serde_json::to_string(predicate).unwrap();
    let kinds = "      slot_entities: [Person, Household]\n";
    let target = if target_kinds { kinds } else { "" };
    let owner = if owner_kinds { kinds } else { "" };
    format!(
        r#"
format: rulespec/v1
rules:
  - name: base
    kind: data_relation
    data_relation: {{arity: 2, arguments: [Person, Household]}}
  - name: inherited
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: base
      current_slot: 1
      related_slot: 0
{target}    versions:
      - effective_from: 2026-01-01
        formula: true
  - name: unrelated
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: base
      current_slot: 1
      related_slot: 0
    versions:
      - effective_from: 2026-01-01
        formula: candidate.age > 0
  - name: selected
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: base
      current_slot: 1
      related_slot: 0
{owner}    versions:
      - effective_from: 2026-01-01
        formula: {predicate}
  - name: total
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(selected)
  - name: age_value
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age
"#
    )
}

#[test]
fn rulespec_derived_atom_targets_cannot_gain_declared_kinds_from_direction_stamping() {
    let source = distinct_kind_rulespec(false, true, "inherited(candidate, anchor)");
    let error = CompiledProgramArtifact::from_rulespec_str(&source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("relation_atom_needs_typed_relation"),
        "{error}"
    );
}

#[test]
fn rulespec_atom_and_comparison_owners_require_declared_kinds_before_stamping() {
    for predicate in [
        "base(candidate, anchor)",
        "candidate == candidate",
        "(if candidate == candidate: 1 else: 0) == 1",
    ] {
        let source = distinct_kind_rulespec(true, false, predicate);
        let error = CompiledProgramArtifact::from_rulespec_str(&source).unwrap_err();
        assert!(
            error.to_string().contains("relation_roles_need_slot_kinds"),
            "{predicate}: {error}"
        );
    }
}

#[test]
fn rulespec_keeps_qualified_only_and_unrelated_distinct_kind_stamping() {
    for (target_kinds, owner_kinds, predicate) in [
        (false, false, "candidate.age > 0"),
        (true, true, "inherited(candidate, anchor)"),
    ] {
        let source = distinct_kind_rulespec(target_kinds, owner_kinds, predicate);
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
        let value = serde_json::to_value(&artifact.program).unwrap();
        for name in ["inherited", "unrelated", "selected"] {
            let relation = value["relations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|relation| relation["name"] == name)
                .unwrap();
            assert_eq!(relation["slot_entities"], json!(["Person", "Household"]));
            assert_eq!(
                relation["derivation"]["slot_entities"],
                json!(["Person", "Household"])
            );
        }
        let encoded = serde_json::to_string(&artifact).unwrap();
        CompiledProgramArtifact::from_json_str(&encoded).unwrap();
    }
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
