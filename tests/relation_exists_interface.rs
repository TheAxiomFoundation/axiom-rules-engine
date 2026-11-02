//! Existential generator lookups and lexical kind flow in format 3.
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::interface::{RelationProvenance, derive_interface};
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

fn positive(term: Value, name: &str) -> Value {
    json!({"kind":"comparison","op":"gt","left":{"kind":"role_input","term":term,"name":name},
        "right":{"kind":"literal","value":{"kind":"integer","value":0}}})
}
fn exists(relation: &str, terms: Vec<Value>, slot: usize, body: Value) -> Value {
    json!({"kind":"exists","relation":relation,"terms":terms,"variable_slot":slot,"body":body})
}
fn spec(predicate: Value) -> ProgramSpec {
    serde_json::from_value(json!({"relations":[
        {"name":"roster","arity":2,"slot_entities":["Person","Person"]},
        {"name":"home","arity":3,"slot_entities":["Person","Household","Person"]},
        {"name":"residents","arity":2,"slot_entities":["Household","Person"]},
        {"name":"selected","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_relation":"roster","current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":predicate}},
    ],"derived":[{"name":"total","entity":"Person","dtype":"integer","semantics":"scalar",
        "expr":{"kind":"count_related","relation":"selected","current_slot":0,"related_slot":1}}]})).unwrap()
}

#[test]
fn generator_records_bound_slots_and_body_reads_follow_lexical_kinds() {
    let body = json!({"kind":"and","items":[
        positive(json!({"var":0}),"household_size"),
        exists("residents",vec![json!({"var":0}),json!({"var":0})],1,
            json!({"kind":"and","items":[positive(json!({"var":0}),"age"),positive(json!({"var":1}),"rent")]}))
    ]});
    let spec = spec(exists(
        "home",
        vec![json!("anchor"), json!({"var":0}), json!("candidate")],
        1,
        body,
    ));
    let interface = derive_interface(&spec).unwrap();
    let relation = |name: &str| interface.relations.iter().find(|r| r.name == name).unwrap();
    assert_eq!(relation("home").lookup_slots, [0, 2]);
    assert_eq!(relation("residents").lookup_slots, [0]);
    let RelationProvenance::Derived { reads, .. } = &relation("selected").provenance else {
        panic!("derived")
    };
    assert_eq!(reads.relations, ["home", "residents"]);
    assert_eq!(reads.inputs, ["age", "household_size", "rent"]);
    for (name, kinds) in [
        ("age", vec!["Person"]),
        ("household_size", vec!["Household"]),
        ("rent", vec!["Household"]),
    ] {
        assert_eq!(
            interface
                .inputs
                .iter()
                .find(|input| input.name == name)
                .unwrap()
                .entities,
            kinds
        );
    }
    let artifact = CompiledProgramArtifact::compile(spec).unwrap();
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
    home["lookup_slots"] = json!([0, 1, 2]);
    assert!(
        CompiledProgramArtifact::from_json_str(&tampered.to_string())
            .unwrap_err()
            .to_string()
            .contains("interface")
    );
}

#[test]
fn derived_generator_records_only_its_current_slot_in_either_direction() {
    for current in [0, 1] {
        let terms = if current == 0 {
            vec![json!("anchor"), json!({"var":0})]
        } else {
            vec![json!({"var":0}), json!("anchor")]
        };
        let mut value = serde_json::to_value(spec(exists(
            "members",
            terms,
            1 - current,
            positive(json!({"var":0}), "age"),
        )))
        .unwrap();
        value["relations"].as_array_mut().unwrap().push(json!({"name":"members","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_relation":"roster","current_slot":current,"related_slot":1-current,"slot_entities":["Person","Person"],
            "predicate":{"kind":"same_entity","left":"candidate","right":"candidate","negated":false}}}));
        let spec: ProgramSpec = serde_json::from_value(value).unwrap();
        let interface = derive_interface(&spec).unwrap();
        let members = interface
            .relations
            .iter()
            .find(|r| r.name == "members")
            .unwrap();
        assert_eq!(members.lookup_slots, [current]);
    }
}

#[test]
fn unary_generator_records_no_bound_lookup_slot() {
    let mut value = serde_json::to_value(spec(exists(
        "unary",
        vec![json!({"var":0})],
        0,
        positive(json!({"var":0}), "age"),
    )))
    .unwrap();
    value["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"unary","arity":1,"slot_entities":["Person"]}));
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    let interface = derive_interface(&spec).unwrap();
    assert!(
        interface
            .relations
            .iter()
            .find(|r| r.name == "unary")
            .unwrap()
            .lookup_slots
            .is_empty()
    );
    assert_eq!(interface.inputs[0].entities, ["Person"]);
}
