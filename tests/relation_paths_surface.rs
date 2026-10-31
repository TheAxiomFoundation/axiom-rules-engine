//! Path declaration, artifact compatibility, and kind-checking contracts.
use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{ProgramSpec, RelationDerivationSpec};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde::Deserialize;
use serde_json::{Value, json};

fn truth() -> Value {
    json!({"kind":"comparison","left":{"kind":"literal","value":{"kind":"integer","value":0}},
        "op":"eq","right":{"kind":"literal","value":{"kind":"integer","value":0}}})
}
fn step(relation: &str, from_slot: usize, to_slot: usize) -> Value {
    json!({"relation":relation,"from_slot":from_slot,"to_slot":to_slot})
}
fn path_program() -> Value {
    json!({"relations":[
        {"name":"household","arity":2,"slot_entities":["Person","Household"]},
        {"name":"lives_with","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_path":[step("household",0,1),step("household",1,0)],
            "current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":truth()}}
    ],"derived":[{"name":"total","entity":"Person","dtype":"integer","semantics":"scalar",
        "expr":{"kind":"count_related","relation":"lives_with","current_slot":0,"related_slot":1}}]})
}
fn compile(value: Value) -> Result<CompiledProgramArtifact, String> {
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    CompiledProgramArtifact::compile(spec).map_err(|error| error.to_string())
}
fn module(declaration: &str) -> String {
    format!(
        r#"format: rulespec/v1
rules:
  - name: household
    kind: data_relation
    data_relation: {{arity: 2, arguments: [Person, Household]}}
  - name: resident_count
    kind: derived
    entity: Household
    dtype: Integer
    versions: [{{effective_from: 2026-01-01, formula: 'len(household)'}}]
  - name: total
    kind: derived
    entity: Person
    dtype: Integer
    versions: [{{effective_from: 2026-01-01, formula: 'len(lives_with)'}}]
  - name: lives_with
    kind: derived_relation
    derived_relation:
{declaration}
    versions: [{{effective_from: 2026-01-01, formula: 'true'}}]
"#
    )
}
fn declaration() -> String {
    "      arity: 2\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n      source_path:\n        - {relation: household, from_slot: 0, to_slot: 1}\n        - {relation: household, from_slot: 1, to_slot: 0}".into()
}

#[test]
fn path_artifact_omits_the_legacy_source_and_an_old_engine_shape_refuses_it() {
    // This is the pre-path spec shape: unknown source_path is ignored, making
    // absence of the required source_relation the compatibility boundary.
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct OldRelationDerivationSpec {
        source_relation: String,
        current_slot: usize,
        related_slot: usize,
        #[serde(default)]
        entity: Option<String>,
        #[serde(default)]
        member_relation: Option<String>,
        #[serde(default)]
        slot_entities: Vec<String>,
        predicate: Value,
    }
    let artifact = compile(path_program()).unwrap();
    let encoded = serde_json::to_value(&artifact).unwrap();
    let derivation = &encoded["program"]["relations"][1]["derivation"];
    assert!(derivation.get("source_path").is_some());
    assert!(derivation.get("source_relation").is_none());
    let old = serde_json::from_value::<OldRelationDerivationSpec>(derivation.clone()).unwrap_err();
    assert!(
        old.to_string().contains("missing field `source_relation`"),
        "{old}"
    );
    CompiledProgramArtifact::from_json_str(&encoded.to_string()).unwrap();

    // A single source retains its field ordering and omits the new field.
    let legacy = format!(
        r#"{{"source_relation":"household","current_slot":0,"related_slot":1,"slot_entities":["Person","Household"],"predicate":{}}}"#,
        truth()
    );
    let parsed: RelationDerivationSpec = serde_json::from_str(&legacy).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), legacy);
}

#[test]
fn rulespec_paths_require_explicit_binary_declarations_and_exclusive_sources() {
    let valid = declaration();
    CompiledProgramArtifact::from_rulespec_str(&module(&valid)).unwrap();
    for field in [
        "      arity: 2\n",
        "      slot_entities: [Person, Person]\n",
        "      current_slot: 0\n",
        "      related_slot: 1\n",
    ] {
        let error = CompiledProgramArtifact::from_rulespec_str(&module(&valid.replace(field, "")))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("relation_path_requires_binary_declaration"),
            "{field}: {error}"
        );
    }
    for declaration in [
        valid.replace("arity: 2", "arity: 3"),
        valid.replace("related_slot: 1", "related_slot: 0"),
        valid.replace("current_slot: 0", "current_slot: 2"),
    ] {
        let error = CompiledProgramArtifact::from_rulespec_str(&module(&declaration))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("relation_path_requires_binary_declaration"),
            "{error}"
        );
    }
    for field in ["entity", "member_relation"] {
        let error = CompiledProgramArtifact::from_rulespec_str(&module(&format!(
            "{valid}\n      {field}: forbidden"
        )))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("relation_path_forbidden_field") && error.contains(field),
            "{error}"
        );
    }
    let both = format!("{valid}\n      source_relation: household");
    let error = CompiledProgramArtifact::from_rulespec_str(&module(&both))
        .unwrap_err()
        .to_string();
    assert!(error.contains("relation_path_source_conflict"), "{error}");
    let empty = valid
        .split("      source_path:")
        .next()
        .unwrap()
        .to_string()
        + "      source_path: []";
    let error = CompiledProgramArtifact::from_rulespec_str(&module(&empty))
        .unwrap_err()
        .to_string();
    assert!(error.contains("relation_path_empty"), "{error}");
    let neither = valid.split("      source_path:").next().unwrap();
    let error = CompiledProgramArtifact::from_rulespec_str(&module(neither))
        .unwrap_err()
        .to_string();
    // Keep the existing refusal's exact text for legacy declarations.
    assert_eq!(
        error,
        "failed to load RuleSpec module `<memory>`: RuleSpec derived relation `lives_with` must declare derived_relation.source_relation"
    );
}

#[test]
fn raw_path_sources_refuse_conflicts_empty_paths_and_filtered_entity_fields() {
    for (field, value, code) in [
        (
            "source_relation",
            json!("household"),
            "relation_path_source_conflict",
        ),
        ("source_path", json!([]), "relation_path_empty"),
        ("entity", json!("Filtered"), "relation_path_forbidden_field"),
        (
            "member_relation",
            json!("members"),
            "relation_path_forbidden_field",
        ),
    ] {
        let mut program = path_program();
        program["relations"][1]["derivation"][field] = value;
        let error = compile(program).unwrap_err();
        assert!(error.contains(code), "{field}: {error}");
    }
    let mut neither = path_program();
    neither["relations"][1]["derivation"]
        .as_object_mut()
        .unwrap()
        .remove("source_path");
    assert!(
        compile(neither)
            .unwrap_err()
            .contains("relation_path_source_conflict")
    );
}

#[test]
fn raw_path_sources_require_binary_slots_and_declared_endpoint_kinds() {
    for (field, value) in [
        ("arity", json!(3)),
        ("slot_entities", json!(["Person", "Household"])),
        ("derivation.slot_entities", json!([])),
        ("derivation.slot_entities", json!(["Person"])),
        ("derivation.current_slot", json!(2)),
        ("derivation.related_slot", json!(0)),
    ] {
        let mut program = path_program();
        if field == "arity" {
            // Reach path typing without first violating the legacy schema's
            // independent slot-kind count check.
            program["relations"][1]["slot_entities"] = json!([]);
        }
        if let Some(field) = field.strip_prefix("derivation.") {
            program["relations"][1]["derivation"][field] = value;
        } else {
            program["relations"][1][field] = value;
        }
        let error = compile(program).unwrap_err();
        assert!(
            error.contains("relation_path_requires_binary_declaration"),
            "{field}: {error}"
        );
    }
}

#[test]
fn path_step_and_kind_refusals_name_the_construct() {
    for (path, extra, code) in [
        (
            json!([step("triple", 0, 1)]),
            Some(json!({"name":"triple","arity":3,"slot_entities":["Person","Person","Person"]})),
            "relation_path_step_arity",
        ),
        (
            json!([step("household", 0, 0)]),
            None,
            "relation_path_step_slots",
        ),
        (
            json!([step("household", 0, 2)]),
            None,
            "relation_path_step_slots",
        ),
        (
            json!([step("household", 2, 0)]),
            None,
            "relation_path_step_slots",
        ),
        (
            json!([step("household", 0, 1), step("household", 0, 1)]),
            None,
            "relation_path_kind_mismatch",
        ),
        (
            json!([step("household", 1, 0)]),
            None,
            "relation_path_kind_mismatch",
        ),
        (
            json!([step("household", 0, 1)]),
            None,
            "relation_path_kind_mismatch",
        ),
        (
            json!([step("absent", 0, 1)]),
            None,
            "unknown relation dependency `absent`",
        ),
        (
            json!([step("untyped", 0, 1)]),
            Some(json!({"name":"untyped","arity":2})),
            "untyped_relation",
        ),
    ] {
        let mut program = path_program();
        program["relations"][1]["derivation"]["source_path"] = path;
        if let Some(extra) = extra {
            program["relations"].as_array_mut().unwrap().push(extra);
        }
        let error = compile(program).unwrap_err();
        assert!(error.contains(code), "{code}: {error}");
    }
    let mut inverse = path_program();
    inverse["relations"].as_array_mut().unwrap().push(json!({"name":"forward","arity":2,
        "slot_entities":["Person","Household"],"derivation":{"source_relation":"household",
            "current_slot":0,"related_slot":1,"slot_entities":["Person","Household"],"predicate":truth()}}));
    inverse["relations"][1]["derivation"]["source_path"] =
        json!([step("household", 0, 1), step("forward", 1, 0)]);
    assert!(
        compile(inverse)
            .unwrap_err()
            .contains("inverse_derived_traversal_unsupported")
    );
}

#[test]
fn every_path_step_is_a_cycle_dependency() {
    let mut program = path_program();
    program["relations"][1]["derivation"]["source_path"] =
        json!([step("household", 0, 1), step("return_home", 0, 1)]);
    program["relations"].as_array_mut().unwrap().push(json!({"name":"return_home","arity":2,
        "slot_entities":["Household","Person"],"derivation":{"source_path":[step("household",1,0),step("lives_with",0,1)],
            "current_slot":0,"related_slot":1,"slot_entities":["Household","Person"],"predicate":truth()}}));
    let error = compile(program).unwrap_err();
    assert!(
        error.to_lowercase().contains("cyclic") || error.to_lowercase().contains("cycle"),
        "{error}"
    );
}

struct Source;
impl ModuleSource for Source {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok((target == "us:policies/test/path").then(|| module(&declaration())))
    }
}

#[test]
fn every_path_step_rewrites_its_modules_local_alias() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_with_source("us:policies/test/path", &Source)
            .unwrap();
    let relation = artifact
        .program
        .relations
        .iter()
        .find(|relation| relation.name.ends_with("#relation.lives_with"))
        .unwrap();
    let path = relation
        .derivation
        .as_ref()
        .unwrap()
        .source_path
        .as_ref()
        .unwrap();
    assert_eq!(path.len(), 2);
    for step in path {
        assert_eq!(step.relation, "us:policies/test/path#relation.household");
    }
}

#[test]
fn a_path_alone_opts_into_supplied_tuple_and_lenient_binding_refusals() {
    let program = path_program();
    let model: ProgramSpec = serde_json::from_value(program.clone()).unwrap();
    let model = model.to_program().unwrap();
    assert!(model.uses_relation_roles());
    assert!(model.uses_relation_atoms());
    for (binding, relations, code) in [
        (
            "strict",
            json!([{"name":"lives_with","tuple":["a","b"],"interval":{"start":"2026-01-01","end":"2026-12-31"}}]),
            "supplied_tuples_for_derived_relation",
        ),
        ("lenient", json!([]), "lenient_binding_with_relation_roles"),
    ] {
        let request: ExecutionRequest =
            serde_json::from_value(json!({"mode":"explain","program":program,
            "relation_binding":binding,"dataset":{"inputs":[],"relations":relations},"queries":[]}))
            .unwrap();
        let error = execute_request(request).unwrap_err().to_string();
        assert!(error.contains(code), "{error}");
    }
}

#[test]
fn generated_paths_compile_exactly_when_their_endpoint_and_junction_kinds_agree() {
    use std::cell::Cell;
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 128,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[214; 32]),
    );
    let counts = Cell::new([0usize; 3]);
    runner
        .run(
            &(prop::collection::vec(any::<bool>(), 2..7), 0usize..8),
            |(kinds, fault)| {
                let mut program = path_program();
                let length = kinds.len() - 1;
                let kind = |i: usize| if kinds[i] { "Person" } else { "Household" };
                let mut relations = vec![];
                let mut steps = vec![];
                for index in 0..length {
                    let name = format!("edge_{index}");
                    let from_kind = if (fault == 1 && index == 0) || (fault == 3 && index == 1) {
                        if kinds[index] { "Household" } else { "Person" }
                    } else {
                        kind(index)
                    };
                    let to_kind = if fault == 2 && index == length - 1 {
                        if kinds[length] { "Household" } else { "Person" }
                    } else {
                        kind(index + 1)
                    };
                    relations
                        .push(json!({"name":name,"arity":2,"slot_entities":[from_kind,to_kind]}));
                    steps.push(step(&name, 0, 1));
                }
                let mut selected = program["relations"][1].clone();
                selected["slot_entities"] = json!([kind(0), kind(length)]);
                selected["derivation"]["slot_entities"] = selected["slot_entities"].clone();
                selected["derivation"]["source_path"] = json!(steps);
                relations.push(selected);
                program["relations"] = json!(relations);
                program["derived"][0]["entity"] = json!(kind(0));
                let outcome = compile(program);
                let mut observed = counts.get();
                if fault == 1 || fault == 2 || (fault == 3 && length > 1) {
                    let error = outcome.unwrap_err();
                    prop_assert!(error.contains("relation_path_kind_mismatch"), "{error}");
                    observed[0] += 1;
                } else {
                    let artifact = outcome.unwrap();
                    let compiled_path = artifact
                        .program
                        .relations
                        .iter()
                        .find(|r| r.name == "lives_with")
                        .unwrap()
                        .derivation
                        .as_ref()
                        .unwrap()
                        .source_path
                        .as_ref()
                        .unwrap();
                    prop_assert_eq!(compiled_path.len(), length);
                    observed[1] += 1;
                    observed[2] += usize::from(compiled_path.len() >= 3);
                }
                counts.set(observed);
                Ok(())
            },
        )
        .unwrap();
    let counts = counts.get();
    for (name, count, floor) in [
        ("refused mismatches", counts[0], 20),
        ("compiled paths", counts[1], 60),
        ("compiled paths of 3+ steps", counts[2], 30),
    ] {
        assert!(count >= floor, "{name}: {count}, floor {floor}");
        eprintln!("path kind coverage: {name}={count}, floor={floor}");
    }
}
