//! Path provenance and the relation slots candidate generation actually reads.
use std::cell::Cell;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::interface::{RelationProvenance, derive_interface};
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use serde_json::{Value, json};

fn yes() -> Value {
    json!({"kind":"comparison", "op":"eq",
        "left":{"kind":"literal","value":{"kind":"integer","value":1}},
        "right":{"kind":"literal","value":{"kind":"integer","value":1}}})
}

fn count(relation: &str) -> Value {
    json!({"name":"total", "entity":"Person", "dtype":"integer", "semantics":"scalar",
        "expr":{"kind":"count_related","relation":relation,"current_slot":0,"related_slot":1}})
}

fn fixture() -> ProgramSpec {
    serde_json::from_value(json!({"relations":[
        {"name":"home", "arity":2, "slot_entities":["Person","Household"]},
        {"name":"residents", "arity":2, "slot_entities":["Person","Household"], "derivation":{
            "source_relation":"home", "current_slot":1, "related_slot":0,
            "slot_entities":["Person","Household"], "predicate":yes()}},
        {"name":"selected", "arity":2, "slot_entities":["Person","Person"], "derivation":{
            "source_path":[
                {"relation":"home", "from_slot":0, "to_slot":1},
                {"relation":"residents", "from_slot":1, "to_slot":0}],
            "current_slot":0, "related_slot":1, "slot_entities":["Person","Person"], "predicate":yes()}}
    ], "derived":[count("selected")]})).unwrap()
}

#[test]
fn path_records_every_relation_and_data_or_derived_lookup_slot() {
    let interface = derive_interface(&fixture()).unwrap();
    let relation = |name: &str| interface.relations.iter().find(|r| r.name == name).unwrap();
    assert_eq!(relation("home").lookup_slots, [0, 1]);
    assert_eq!(relation("residents").lookup_slots, [1]);
    assert!(!relation("selected").accepts_supplied_tuples);
    let RelationProvenance::SourcePath {
        rule,
        source_path,
        reads,
    } = &relation("selected").provenance
    else {
        panic!("path provenance")
    };
    assert_eq!(rule, "selected");
    assert_eq!(
        source_path
            .iter()
            .map(|s| (s.relation.as_str(), s.from_slot, s.to_slot))
            .collect::<Vec<_>>(),
        [("home", 0, 1), ("residents", 1, 0)]
    );
    assert_eq!(reads.relations, ["home", "residents"]);
    assert!(reads.inputs.is_empty());
    assert!(reads.rules.is_empty());

    let artifact = CompiledProgramArtifact::compile(fixture()).unwrap();
    let encoded = serde_json::to_string(&artifact).unwrap();
    assert_eq!(artifact.artifact_format_version, 3);
    CompiledProgramArtifact::from_json_str(&encoded).unwrap();
    let mut tampered: Value = serde_json::from_str(&encoded).unwrap();
    let home = tampered["program"]["interface"]["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["name"] == "home")
        .unwrap();
    home["lookup_slots"] = json!([1]);
    assert!(
        CompiledProgramArtifact::from_json_str(&tampered.to_string())
            .unwrap_err()
            .to_string()
            .contains("interface")
    );
}

#[test]
fn path_provenance_and_steps_require_exactly_their_keys() {
    let interface = derive_interface(&fixture()).unwrap();
    let provenance = &interface
        .relations
        .iter()
        .find(|r| r.name == "selected")
        .unwrap()
        .provenance;
    let encoded = serde_json::to_value(provenance).unwrap();
    assert_eq!(encoded["kind"], "source_path");
    assert!(encoded.get("source_relation").is_none());
    let decoded: RelationProvenance = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(&decoded, provenance);
    for key in ["kind", "rule", "source_path", "reads"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<RelationProvenance>(missing).is_err(),
            "missing {key}"
        );
    }
    for key in ["unknown", "source_relation"] {
        let mut extra = encoded.clone();
        extra[key] = json!("home");
        assert!(
            serde_json::from_value::<RelationProvenance>(extra).is_err(),
            "extra {key}"
        );
    }
    for key in ["relation", "from_slot", "to_slot"] {
        let mut missing = encoded.clone();
        missing["source_path"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            serde_json::from_value::<RelationProvenance>(missing).is_err(),
            "missing step {key}"
        );
    }
    let mut extra = encoded.clone();
    extra["source_path"][0]["unknown"] = json!(true);
    assert!(serde_json::from_value::<RelationProvenance>(extra).is_err());

    // The previous interface only knows supplied and derived provenance.
    #[derive(Debug, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum OldProvenance {
        Supplied,
        Derived { source_relation: String },
    }
    assert!(serde_json::from_value::<OldProvenance>(encoded).is_err());
    // Keep the old shape's fields used without weakening the historical type.
    let old: OldProvenance =
        serde_json::from_value(json!({"kind":"derived", "source_relation":"home"})).unwrap();
    assert!(matches!(old, OldProvenance::Derived { source_relation } if source_relation == "home"));
}

#[test]
fn dense_refuses_a_reachable_path_source_by_name() {
    let program = fixture().to_program().unwrap();
    let error = DenseCompiledProgram::from_program(&program, Some("Person")).unwrap_err();
    assert!(
        matches!(error, DenseCompileError::Unsupported(ref message)
        if message.contains("path source") && message.contains("selected") && message.contains("#218")),
        "{error}"
    );
}

#[test]
fn dense_refuses_a_path_reached_through_a_single_source_chain() {
    let mut spec = serde_json::to_value(fixture()).unwrap();
    spec["relations"].as_array_mut().unwrap().push(json!({
        "name":"wrapper","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_relation":"selected","current_slot":0,"related_slot":1,
            "slot_entities":["Person","Person"],"predicate":yes()}}));
    spec["derived"] = json!([count("wrapper")]);
    let program = serde_json::from_value::<ProgramSpec>(spec)
        .unwrap()
        .to_program()
        .unwrap();
    let error = DenseCompiledProgram::from_program(&program, Some("Person")).unwrap_err();
    assert!(
        matches!(error, DenseCompileError::Unsupported(ref message)
        if message.contains("path source") && message.contains("selected") && message.contains("#218")),
        "{error}"
    );
}

#[test]
fn an_unused_path_does_not_enter_denses_single_source_ancestry() {
    let mut spec = serde_json::to_value(fixture()).unwrap();
    spec["relations"].as_array_mut().unwrap().extend([
        json!({"name":"peers","arity":2,"slot_entities":["Person","Person"]}),
        json!({"name":"eligible","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_relation":"peers","current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],
            "predicate":{"kind":"relation_member","relation":"peers","current_slot":0,"related_slot":1}}}),
    ]);
    spec["derived"] = json!([count("eligible")]);
    let program = serde_json::from_value::<ProgramSpec>(spec)
        .unwrap()
        .to_program()
        .unwrap();
    DenseCompiledProgram::from_program(&program, Some("Person")).unwrap();
}

#[test]
pub(crate) fn generated_paths_record_all_edges_and_slots() {
    let mut runner = TestRunner::new(Config {
        cases: 128,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(0x214_4_155),
        ..Config::default()
    });
    let checked_steps = Cell::new(0);
    let long_paths = Cell::new(0);
    let derived_positions = [Cell::new(0), Cell::new(0), Cell::new(0)];
    runner.run(&prop::collection::vec((any::<bool>(), 0_usize..2), 1..6), |steps| {
        let mut relations = Vec::new();
        let mut path = Vec::new();
        for (index, &(derived, from)) in steps.iter().enumerate() {
            let name = format!("step{index}");
            let mut relation = json!({"name":name, "arity":2, "slot_entities":["Person","Person"]});
            if derived {
                let base = format!("base{index}");
                relations.push(json!({"name":base,"arity":2,"slot_entities":["Person","Person"]}));
                relation["derivation"] = json!({"source_relation":base,"current_slot":from,"related_slot":1-from,
                    "slot_entities":["Person","Person"],"predicate":yes()});
            }
            relations.push(relation);
            path.push(json!({"relation":name,"from_slot":from,"to_slot":1-from}));
        }
        relations.push(json!({"name":"selected","arity":2,"slot_entities":["Person","Person"], "derivation":{
            "source_path":path,"current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":yes()}}));
        let spec: ProgramSpec = serde_json::from_value(json!({"relations":relations,"derived":[count("selected")]})).unwrap();
        let interface = derive_interface(&spec).unwrap();
        let selected = interface.relations.iter().find(|r| r.name == "selected").unwrap();
        let RelationProvenance::SourcePath { reads, .. } = &selected.provenance else { panic!("path provenance"); };
        prop_assert_eq!(&reads.relations, &(0..steps.len()).map(|i| format!("step{i}")).collect::<Vec<_>>());
        for (index, &(derived, from)) in steps.iter().enumerate() {
            let relation = interface.relations.iter().find(|r| r.name == format!("step{index}")).unwrap();
            prop_assert_eq!(&relation.lookup_slots, &vec![from]);
            checked_steps.set(checked_steps.get() + 1);
            if derived && index < 3 { derived_positions[index].set(derived_positions[index].get() + 1); }
        }
        long_paths.set(long_paths.get() + usize::from(steps.len() >= 3));
        Ok(())
    }).unwrap();
    assert!(
        checked_steps.get() >= 300,
        "interface path steps went dark: {}",
        checked_steps.get()
    );
    assert!(
        long_paths.get() >= 50,
        "long interface paths went dark: {}",
        long_paths.get()
    );
    for (position, cases) in derived_positions.iter().enumerate() {
        assert!(
            cases.get() >= 20,
            "derived interface position {position} went dark: {}",
            cases.get()
        );
    }
    eprintln!(
        "interface paths: steps={} (floor 300), long={} (floor 50), derived positions={:?} (floor 20 each)",
        checked_steps.get(),
        long_paths.get(),
        derived_positions.map(|count| count.get())
    );
}
