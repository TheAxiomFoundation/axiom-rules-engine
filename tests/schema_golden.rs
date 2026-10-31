//! Golden-file and self-consistency tests for the published JSON Schemas.
//!
//! - `schemas_are_current` regenerates every schema in memory and asserts the
//!   checked-in `schemas/*.json` copy is byte-identical, so CI fails on drift
//!   between the Rust types and the published files. The four experimental
//!   stage-3 schemas exist only with `unit-derivation` as well, so they are
//!   checked only under `--features "schema unit-derivation"`. Refresh all of
//!   them with `cargo run --features "schema unit-derivation" -- emit-schemas
//!   --out schemas`.
//! - `published_schemas_are_valid_draft07` compiles each published schema as a
//!   JSON Schema, catching a malformed hand-written fragment.
//! - `artifact_schema_accepts_a_real_compiled_artifact` compiles an in-repo
//!   RuleSpec fixture to a `CompiledProgramArtifact` and validates the actual
//!   serialized bytes against the derived artifact schema — the round-trip that
//!   proves the schema matches what the engine emits.
//!
//! The derived artifact schema is `compiled-artifact.v3`. The v2 and v1
//! schemas are frozen archives; format-2 artifacts are checked against the v2
//! archive and the format-2 compatibility loader.

#![cfg(feature = "schema")]

use std::path::PathBuf;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::schema::{all_schemas, to_pretty_string};

/// The derived schema for the format the engine writes.
const CURRENT_ARTIFACT_SCHEMA: &str = "compiled-artifact.v3.schema.json";
/// The frozen schema for format 2, which the compatibility loader reads.
const FORMAT_2_ARTIFACT_SCHEMA: &str = "compiled-artifact.v2.schema.json";

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas")
}

fn published_schema(file_name: &str) -> serde_json::Value {
    all_schemas()
        .into_iter()
        .find(|named| named.file_name == file_name)
        .unwrap_or_else(|| panic!("{file_name} is published"))
        .schema
}

fn validator(file_name: &str) -> jsonschema::Validator {
    jsonschema::draft7::new(&published_schema(file_name))
        .unwrap_or_else(|error| panic!("{file_name} compiles: {error}"))
}

fn schema_errors(validator: &jsonschema::Validator, value: &serde_json::Value) -> Vec<String> {
    validator
        .iter_errors(value)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect()
}

/// The in-repo fixture the artifact-schema tests compile.
fn compiled_fixture() -> CompiledProgramArtifact {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rulespec/uksi/2013/376/rules.yaml");
    let source = std::fs::read_to_string(&fixture).expect("fixture is readable");
    CompiledProgramArtifact::from_rulespec_str(&source).expect("fixture compiles to an artifact")
}

/// The format-2 artifact of the same program, as engines before format 3
/// wrote it: format 3 without `engine_build`, `source_closure` and
/// `program.interface`, stamped 2.
fn as_format_2(artifact: &CompiledProgramArtifact) -> serde_json::Value {
    let mut value = serde_json::to_value(artifact).expect("artifact serializes");
    let object = value.as_object_mut().unwrap();
    object.remove("engine_build");
    object.remove("source_closure");
    object.insert("artifact_format_version".to_string(), serde_json::json!(2));
    value["program"]
        .as_object_mut()
        .unwrap()
        .remove("interface");
    value
}

#[cfg(feature = "unit-derivation")]
struct NzGrossModuleSource;

#[cfg(feature = "unit-derivation")]
impl axiom_rules_engine::source::ModuleSource for NzGrossModuleSource {
    fn load(
        &self,
        target: &str,
    ) -> Result<Option<String>, axiom_rules_engine::source::SourceError> {
        Ok(
            (target == "nz:statutes/income_tax/family_scheme/tax_credits").then(|| {
                include_str!("fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml")
                    .to_string()
            }),
        )
    }
}

#[test]
fn schemas_are_current() {
    let dir = schemas_dir();
    let mut stale = Vec::new();
    for named in all_schemas() {
        let path = dir.join(named.file_name);
        let expected = to_pretty_string(&named.schema);
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual == expected => {}
            Ok(_) => stale.push(format!("{} is out of date", named.file_name)),
            Err(error) => stale.push(format!("{} could not be read: {error}", named.file_name)),
        }
    }
    assert!(
        stale.is_empty(),
        "checked-in schemas differ from the generated ones \
         (run `cargo run --features \"schema unit-derivation\" -- emit-schemas --out schemas`):\n{}",
        stale.join("\n")
    );
}

#[test]
fn published_schemas_are_valid_draft07() {
    for named in all_schemas() {
        jsonschema::draft7::new(&named.schema).unwrap_or_else(|error| {
            panic!(
                "{} is not a valid draft-07 schema: {error}",
                named.file_name
            )
        });
    }
}

#[test]
fn module_schema_and_loader_agree_on_a_null_source_beside_a_path() {
    let validator = validator("rulespec-module.v1.schema.json");
    for field in ["source_relation", "source"] {
        let module = serde_json::json!({
            "format": "rulespec/v1",
            "rules": [
                {"name": "household", "kind": "data_relation",
                    "data_relation": {"arity": 2, "arguments": ["Person", "Household"]}},
                {"name": "resident_count", "kind": "derived", "entity": "Household",
                    "dtype": "Integer",
                    "versions": [{"effective_from": "2026-01-01", "formula": "len(household)"}]},
                {"name": "lives_with", "kind": "derived_relation",
                    "derived_relation": {
                        "arity": 2, "slot_entities": ["Person", "Person"],
                        "current_slot": 0, "related_slot": 1, field: null,
                        "source_path": [
                            {"relation": "household", "from_slot": 0, "to_slot": 1},
                            {"relation": "household", "from_slot": 1, "to_slot": 0},
                        ],
                    },
                    "versions": [{"effective_from": "2026-01-01", "formula": "true"}]},
            ],
        });
        assert_eq!(
            schema_errors(&validator, &module),
            Vec::<String>::new(),
            "{field}"
        );
        CompiledProgramArtifact::from_rulespec_str(&module.to_string())
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }
}

#[test]
fn artifact_schema_accepts_a_real_compiled_artifact() {
    // A real, engine-valid RuleSpec module lives in the fixtures. Compile it
    // the way the CLI does and validate the serialized artifact against the
    // derived schema.
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rulespec/uksi/2013/376/rules.yaml");
    let source = std::fs::read_to_string(&fixture).expect("fixture is readable");
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source)
        .expect("fixture compiles to an artifact");
    let artifact_json = serde_json::to_value(&artifact).expect("artifact serializes to JSON");

    let errors = schema_errors(&validator(CURRENT_ARTIFACT_SCHEMA), &artifact_json);
    assert!(
        errors.is_empty(),
        "compiled artifact did not validate against its own schema:\n{}",
        errors.join("\n")
    );

    // The same module under a canonical target, so its source closure names
    // the module rather than an originless root.
    struct OneModule(String);
    impl axiom_rules_engine::source::ModuleSource for OneModule {
        fn load(
            &self,
            target: &str,
        ) -> Result<Option<String>, axiom_rules_engine::source::SourceError> {
            Ok((target == "uk:legislation/uksi/2013/376").then(|| self.0.clone()))
        }
    }
    let targeted = CompiledProgramArtifact::from_rulespec_with_source(
        "uk:legislation/uksi/2013/376",
        &OneModule(source),
    )
    .expect("fixture compiles under a canonical target");
    let targeted = serde_json::to_value(&targeted).unwrap();
    assert_eq!(
        targeted["source_closure"]["root"],
        "uk:legislation/uksi/2013/376"
    );
    let errors = schema_errors(&validator(CURRENT_ARTIFACT_SCHEMA), &targeted);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// The artifact's schema is the wire struct's, so the generated schema
/// describes the interface block at `program.interface`, required and never
/// null, beside the unchanged `ProgramSpec` fields.
#[test]
fn artifact_schema_describes_the_interface_at_program_interface() {
    let schema = published_schema(CURRENT_ARTIFACT_SCHEMA);
    assert_eq!(
        schema["properties"]["program"]["$ref"],
        "#/$defs/ArtifactProgram"
    );
    let program = &schema["$defs"]["ArtifactProgram"];
    assert_eq!(
        program["properties"]["interface"]["$ref"],
        "#/$defs/ProgramInterface"
    );
    assert!(
        program["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("interface")),
        "{program}"
    );
    for field in ["module", "units", "relations", "parameters", "derived"] {
        assert!(
            program["properties"].get(field).is_some(),
            "ArtifactProgram lists `{field}`"
        );
    }
    for definition in [
        "ProgramInterface",
        "InterfaceEntity",
        "EntityInstances",
        "InterfaceInput",
        "InterfaceRelation",
        "RelationProvenance",
        "RelationReads",
        "EngineBuild",
        "SourceClosure",
        "SourceClosureModule",
    ] {
        assert!(
            schema["$defs"].get(definition).is_some(),
            "the schema defines `{definition}`"
        );
    }
    // The raw request's program is the `ProgramSpec`, which has no interface.
    let request = published_schema("execution-request.v1.schema.json");
    assert!(
        request["$defs"]["ProgramSpec"]["properties"]
            .get("interface")
            .is_none()
    );
}

#[test]
fn artifact_schema_accepts_min_max_and_only_over_related_entities() {
    // Every form of the related aggregates added for #232: a bare and a
    // filtered extreme of each kind, and `only`.
    let rule = |name: &str, formula: &str| {
        format!(
            "  - name: {name}\n    kind: derived\n    entity: Family\n    dtype: Decimal\n    period: Month\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {formula}\n"
        )
    };
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}{}{}{}{}",
        rule("youngest", "min(member_of_family.age)"),
        rule("oldest", "max(member_of_family.age)"),
        rule(
            "youngest_eligible",
            "min_where(member_of_family, age, eligible)"
        ),
        rule(
            "oldest_eligible",
            "max_where(member_of_family, age, eligible)"
        ),
        rule("only_age", "only(member_of_family.age)"),
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).expect("module compiles");
    let artifact_json = serde_json::to_value(&artifact).expect("artifact serializes to JSON");
    let serialized = artifact_json.to_string();
    for kind in [
        r#""extreme":"min""#,
        r#""extreme":"max""#,
        r#""kind":"single_related""#,
    ] {
        assert!(serialized.contains(kind), "the artifact uses {kind}");
    }
    let schema_value = all_schemas()
        .into_iter()
        .find(|named| named.file_name == CURRENT_ARTIFACT_SCHEMA)
        .expect("artifact schema is published")
        .schema;
    let validator = jsonschema::draft7::new(&schema_value).expect("artifact schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(&artifact_json)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "the artifact did not validate against its schema:\n{}",
        errors.join("\n")
    );
}

#[cfg(feature = "unit-derivation")]
#[test]
fn experimental_unit_aggregation_schemas_accept_the_exact_cli_fixtures() {
    use axiom_rules_engine::unit_derivation::{
        AggregationPlan, AggregationRequest, UnitDerivationConfig, UnitDerivationDocumentRegistry,
    };

    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unit_derivation");
    let source = std::fs::read_to_string(fixture_dir.join("nz_income_explorer_family.yaml"))
        .expect("aggregation plan fixture is readable");
    let request_source =
        std::fs::read_to_string(fixture_dir.join("nz_income_explorer_request.json"))
            .expect("aggregation request fixture is readable");
    let plan: AggregationPlan = serde_yaml::from_str(&source).unwrap();
    let request: AggregationRequest = serde_json::from_str(&request_source).unwrap();
    let source_artifact = CompiledProgramArtifact::from_rulespec_with_source(
        "nz:statutes/income_tax/family_scheme/tax_credits",
        &NzGrossModuleSource,
    )
    .unwrap();
    let mut registry = UnitDerivationDocumentRegistry::default();
    let (plan_id, artifact_json) = {
        let artifact = registry
            .register_aggregation_source(&source, &source_artifact)
            .unwrap();
        (
            artifact.plan_id().to_string(),
            serde_json::to_value(artifact).unwrap(),
        )
    };
    let result = registry
        .execute_aggregation(
            &plan_id,
            &request,
            &UnitDerivationConfig {
                enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
    let fixtures = [
        (
            "experimental-unit-aggregation-plan.stage3.schema.json",
            serde_json::to_value(plan).unwrap(),
        ),
        (
            "compiled-unit-aggregation.stage3.schema.json",
            artifact_json,
        ),
        (
            "unit-aggregation-request.stage3.schema.json",
            serde_json::to_value(request).unwrap(),
        ),
        (
            "unit-aggregation-result.stage3.schema.json",
            serde_json::to_value(result).unwrap(),
        ),
    ];
    let schemas = all_schemas();
    for (name, fixture) in fixtures {
        let schema = schemas
            .iter()
            .find(|schema| schema.file_name == name)
            .unwrap();
        let validator = jsonschema::draft7::new(&schema.schema).unwrap();
        let errors = validator
            .iter_errors(&fixture)
            .map(|error| format!("{} at {}", error, error.instance_path()))
            .collect::<Vec<_>>();
        assert!(
            errors.is_empty(),
            "{name} rejected fixture:\n{}",
            errors.join("\n")
        );
    }
}

#[test]
fn artifact_schema_accepts_the_annotated_divergences() {
    // Exercise the manually-annotated spots in the derived artifact schema
    // that a plain schemars derive would get wrong or narrow: `dtype` aliases
    // (`money` for decimal), decimal values as both a bare integer and a
    // string, a date value, exact module source metadata, and a
    // judgment/comparison expression.
    // If any of these regress, this artifact stops validating. It is written
    // as format 2 and checked against the frozen v2 schema, then given format
    // 3's additions (an unknown build, and the interface this engine derives
    // from the program) and checked against the derived v3 schema.
    let format_2 = serde_json::json!({
        "artifact_format_version": 2,
        "engine_version": "0.1.0",
        "program": {
            "module": {
                "source_verification": {
                    "corpus_citation_path": "us/statute/7/2017/a",
                    "source_sha256": "a".repeat(64),
                    "upstream_source_check": {
                        "status": "official_parameter_source",
                        "checked_paths": ["us/statute/7/2017/a"],
                        "rationale": "The official guidance supplies the current parameter."
                    }
                },
                "encoding_provenance": {"encoder": "axiom-encode/0.2"},
                "validation": [{"oracle": "taxsim", "status": "matches", "last_run": "2026-06-01"}]
            },
            "units": [{"name": "USD", "kind": "currency", "minor_units": 2}],
            "relations": [],
            "parameters": [{
                "id": "us:statutes/7/2017/a#p",
                "name": "p", "unit": "USD", "indexed_by": "household_size",
                "versions": [{"effective_from": "2020-01-01", "effective_to": "2020-12-31", "values": {
                    "1": {"kind": "decimal", "value": 200},
                    "2": {"kind": "decimal", "value": "250.50"},
                    "3": {"kind": "integer", "value": 3},
                    "4": {"kind": "date", "value": "2026-01-01"},
                    "5": {"kind": "bool", "value": true},
                    "6": {"kind": "text", "value": "hi"}
                }}]
            }],
            "derived": [{
                "id": "us:statutes/7/2017/a#d",
                "name": "d", "entity": "Household",
                // `money` is a serde alias for `decimal`; the schema must accept it.
                "dtype": "money", "unit": "USD",
                "semantics": "scalar",
                "expr": {"kind": "if",
                    "condition": {"kind": "comparison",
                        "left": {"kind": "input", "name": "x"}, "op": "gte",
                        "right": {"kind": "literal", "value": {"kind": "decimal", "value": 0}}},
                    "then_expr": {"kind": "parameter_lookup", "parameter": "p",
                        "index": {"kind": "input", "name": "household_size"}},
                    "else_expr": {"kind": "literal", "value": {"kind": "integer", "value": 0}}},
                "versions": []
            }]
        },
        "metadata": {"evaluation_order": ["d"],
            "fast_path": {"strategy": "s", "compatible": true, "blockers": []},
            "input_catalog": []}
    });

    let program: axiom_rules_engine::spec::ProgramSpec =
        serde_json::from_value(format_2["program"].clone()).expect("the program deserializes");
    let interface =
        axiom_rules_engine::interface::derive_interface(&program).expect("the program derives");
    let mut format_3 = format_2.clone();
    format_3["artifact_format_version"] = serde_json::json!(3);
    format_3["engine_build"] = serde_json::json!({"commit": null, "dirty": null});
    format_3["program"]["interface"] = serde_json::to_value(interface).unwrap();

    for (schema, artifact) in [
        (FORMAT_2_ARTIFACT_SCHEMA, &format_2),
        (CURRENT_ARTIFACT_SCHEMA, &format_3),
    ] {
        let errors = schema_errors(&validator(schema), artifact);
        assert!(
            errors.is_empty(),
            "annotated-divergence artifact did not validate against {schema}:\n{}",
            errors.join("\n")
        );
    }

    // And the reverse: everything the schema advertises for `dtype` must
    // actually deserialize into a DTypeSpec, so schema and serde agree.
    for dtype in [
        "judgment", "Judgment", "bool", "Bool", "Boolean", "boolean", "integer", "Integer",
        "decimal", "Decimal", "Money", "money", "Rate", "rate", "text", "Text", "date", "Date",
    ] {
        let json = serde_json::json!(dtype);
        serde_json::from_value::<axiom_rules_engine::spec::DTypeSpec>(json).unwrap_or_else(|e| {
            panic!("schema advertises dtype `{dtype}` but serde rejects it: {e}")
        });
    }
}

/// Every case here is refused by the derived schema and by the default
/// loader alike.
#[test]
fn artifact_schema_rejects_wrong_version_and_invalid_provenance() {
    let base = serde_json::to_value(compiled_fixture()).expect("artifact serializes");
    let validator = validator(CURRENT_ARTIFACT_SCHEMA);

    let mut cases = Vec::new();
    for version in [1, 2, 4] {
        let mut stamped = base.clone();
        stamped["artifact_format_version"] = serde_json::json!(version);
        cases.push(stamped);
    }

    for verification in [
        serde_json::json!({"corpus_citation_path": ""}),
        serde_json::json!({"corpus_citation_path": "us/statute"}),
        serde_json::json!({"corpus_citation_path": "us/statute/26/62", "source_sha256": "bad"}),
        serde_json::json!({"corpus_citation_paths": ["us/statute/26/62"]}),
        serde_json::json!({
            "corpus_citation_path": "us/guidance/treasury/rate",
            "upstream_source_check": {
                "status": "official_parameter_source",
                "checked_paths": ["us/statute/26/62"],
                "rationale": "checked",
                "note": "unknown"
            }
        }),
        serde_json::json!({
            "corpus_citation_path": "us/guidance/treasury/rate",
            "upstream_source_check": {
                "status": "official_parameter_source",
                "checked_paths": ["us/statute/26/62"]
            }
        }),
    ] {
        let mut value = base.clone();
        value["program"]["module"] = serde_json::json!({"source_verification": verification});
        cases.push(value);
    }

    let mut bad_rule = base.clone();
    bad_rule["program"]["parameters"][0]["corpus_citation_path"] = serde_json::json!("us/statute");
    cases.push(bad_rule);

    let mut plural_rule = base.clone();
    plural_rule["program"]["parameters"][0]["corpus_citation_paths"] =
        serde_json::json!(["us/statute/26/62"]);
    cases.push(plural_rule);

    let mut removed_id = base.clone();
    removed_id["program"]["module"] = serde_json::json!({"id": "us:policies/base"});
    cases.push(removed_id);

    // An operand-less max() / min() deserializes, but `ProgramSpec::to_program`
    // refuses it, so the schema does too.
    for function in ["max", "min"] {
        let mut empty = base.clone();
        *first_max_expr(&mut empty["program"]).expect("fixture has a max() rule") =
            serde_json::json!({"kind": function, "items": []});
        cases.push(empty);
    }

    // Format 3 refuses the removed composition directive in any form, a
    // null included.
    for extends in [
        serde_json::json!("us:policies/base"),
        serde_json::Value::Null,
    ] {
        let mut removed_extends = base.clone();
        removed_extends["program"]["extends"] = extends;
        cases.push(removed_extends);
    }

    // The keys format 3 closes or requires, and the identities' spelling.
    let mut unknown_key = base.clone();
    unknown_key["unexpected"] = serde_json::json!(true);
    cases.push(unknown_key);
    for key in ["engine_version", "engine_build"] {
        for absent in [false, true] {
            let mut missing = base.clone();
            if absent {
                missing.as_object_mut().unwrap().remove(key);
            } else {
                missing[key] = serde_json::Value::Null;
            }
            cases.push(missing);
        }
    }
    let mut null_closure = base.clone();
    null_closure["source_closure"] = serde_json::Value::Null;
    cases.push(null_closure);
    for absent in [false, true] {
        let mut missing = base.clone();
        if absent {
            missing["program"]
                .as_object_mut()
                .unwrap()
                .remove("interface");
        } else {
            missing["program"]["interface"] = serde_json::Value::Null;
        }
        cases.push(missing);
    }
    let mut no_catalog = base.clone();
    no_catalog["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("input_catalog");
    cases.push(no_catalog);
    for build in [
        serde_json::json!({"commit": "5D63E8F3", "dirty": false}),
        serde_json::json!({"commit": null, "dirty": true}),
        serde_json::json!({"commit": null}),
        serde_json::json!({"commit": null, "dirty": null, "host": "ci"}),
    ] {
        let mut wrong = base.clone();
        wrong["engine_build"] = build;
        cases.push(wrong);
    }
    for (pointer, replacement) in [
        ("/source_closure/digest", serde_json::json!("sha256:short")),
        ("/source_closure/modules/0/sha256", serde_json::json!("ABC")),
        ("/source_closure/root", serde_json::json!("uksi/2013/376")),
    ] {
        let mut wrong = base.clone();
        *wrong
            .pointer_mut(pointer)
            .expect("the fixture's artifact has a source closure") = replacement;
        cases.push(wrong);
    }
    // A closure must list its root, and under a canonical root no module is
    // originless.
    let mut no_modules = base.clone();
    no_modules["source_closure"]["modules"] = serde_json::json!([]);
    cases.push(no_modules);
    let mut originless_under_a_root = base.clone();
    originless_under_a_root["source_closure"]["root"] =
        serde_json::json!("uk:legislation/uksi/2013/376");
    cases.push(originless_under_a_root);
    let mut no_template = base.clone();
    no_template["program"]["interface"]["entities"][0]
        .as_object_mut()
        .expect("the fixture's interface lists an entity")
        .remove("id_template");
    cases.push(no_template);

    for case in cases {
        assert!(
            !validator.is_valid(&case),
            "invalid format-3 artifact must fail schema: {case}"
        );
        assert!(
            CompiledProgramArtifact::from_json_str(&case.to_string()).is_err(),
            "the loader must refuse what the schema refuses: {case}"
        );
    }
}

/// The first `max(...)` expression node anywhere under `value`.
fn first_max_expr(value: &mut serde_json::Value) -> Option<&mut serde_json::Value> {
    if value.get("kind").and_then(serde_json::Value::as_str) == Some("max")
        && value.get("items").is_some()
    {
        return Some(value);
    }
    match value {
        serde_json::Value::Object(fields) => fields.values_mut().find_map(first_max_expr),
        serde_json::Value::Array(items) => items.iter_mut().find_map(first_max_expr),
        _ => None,
    }
}

#[test]
fn v1_and_v2_artifact_schemas_reject_the_other_generation() {
    let v2_artifact = as_format_2(&compiled_fixture());
    let mut v1_artifact = v2_artifact.clone();
    v1_artifact["artifact_format_version"] = serde_json::json!(1);

    let v1_validator = validator("compiled-artifact.v1.schema.json");
    let v2_validator = validator(FORMAT_2_ARTIFACT_SCHEMA);

    assert!(v1_validator.is_valid(&v1_artifact));
    assert!(v2_validator.is_valid(&v2_artifact));
    assert!(!v1_validator.is_valid(&v2_artifact));
    assert!(!v2_validator.is_valid(&v1_artifact));
}

/// The v3 schema and the frozen v1 and v2 schemas each refuse the other
/// generations' artifacts, as the loaders do: a format-2 artifact is the
/// format-3 one without its three additions, stamped 2.
#[test]
fn v3_artifact_schema_and_the_archives_reject_each_others_generation() {
    let artifact = compiled_fixture();
    let v3_artifact = serde_json::to_value(&artifact).unwrap();
    let v2_artifact = as_format_2(&artifact);
    let mut v1_artifact = v2_artifact.clone();
    v1_artifact["artifact_format_version"] = serde_json::json!(1);
    // Format 3 with only its stamp lowered, and format 2 with only its stamp
    // raised.
    let mut v3_stamped_2 = v3_artifact.clone();
    v3_stamped_2["artifact_format_version"] = serde_json::json!(2);
    let mut v2_stamped_3 = v2_artifact.clone();
    v2_stamped_3["artifact_format_version"] = serde_json::json!(3);

    let v1 = validator("compiled-artifact.v1.schema.json");
    let v2 = validator(FORMAT_2_ARTIFACT_SCHEMA);
    let v3 = validator(CURRENT_ARTIFACT_SCHEMA);
    assert!(v3.is_valid(&v3_artifact));
    assert!(v2.is_valid(&v2_artifact));
    // The v2 archive leaves the top-level keys open, so it accepts
    // `v3_stamped_2`, which only the compatibility loader refuses (below).
    for (validator, name, refused) in [
        (
            &v3,
            "v3",
            vec![&v2_artifact, &v1_artifact, &v3_stamped_2, &v2_stamped_3],
        ),
        (&v2, "v2", vec![&v3_artifact, &v2_stamped_3]),
        (&v1, "v1", vec![&v3_artifact]),
    ] {
        for artifact in refused {
            assert!(
                !validator.is_valid(artifact),
                "the {name} schema must refuse {artifact}"
            );
        }
    }
    for artifact in [&v2_artifact, &v3_stamped_2, &v2_stamped_3] {
        CompiledProgramArtifact::from_json_str(&artifact.to_string())
            .expect_err("the default loader reads only a real format-3 artifact");
    }
    CompiledProgramArtifact::from_legacy_format_2_json_str(&v2_artifact.to_string())
        .expect("the compatibility loader reads the format-2 artifact");
    CompiledProgramArtifact::from_legacy_format_2_json_str(&v3_stamped_2.to_string())
        .expect_err("the compatibility loader refuses format 3's additions");
}

/// The schema and the loader must agree on the v0.1-maintenance serialization.
///
/// Engines on that line write `"extends": null` unconditionally and predate
/// `metadata.input_catalog`; every published artifact has this shape. It is
/// format 2, so the format-2 compatibility loader accepts it, and a
/// schema-first consumer reading the frozen v2 schema must reach the same
/// verdict: a schema that forbids the `extends` KEY (rather than a non-null
/// value) rejects every artifact the engine loads, which is the compat lie
/// this change set exists to remove. The default loader and the v3 schema
/// both refuse it.
#[test]
fn artifact_schema_and_loader_agree_on_maintenance_line_artifacts() {
    let mut value = as_format_2(&compiled_fixture());
    // Reshape to what a v0.1-maintenance engine emits.
    value["program"]["extends"] = serde_json::Value::Null;
    value["metadata"]
        .as_object_mut()
        .expect("metadata is an object")
        .remove("input_catalog");

    let validator = validator(FORMAT_2_ARTIFACT_SCHEMA);
    let errors = schema_errors(&validator, &value);
    assert!(
        errors.is_empty(),
        "schema rejects the maintenance-line shape the loader accepts:\n{}",
        errors.join("\n")
    );
    CompiledProgramArtifact::from_legacy_format_2_json_str(&value.to_string())
        .expect("the loader accepts the same shape the schema accepts");

    assert!(
        !self::validator(CURRENT_ARTIFACT_SCHEMA).is_valid(&value),
        "the v3 schema refuses the format-2 shape"
    );
    CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect_err("the default loader refuses the format-2 shape");

    // And both refuse a real (non-null) composition directive.
    value["program"]["extends"] = serde_json::json!("us:policies/base");
    assert!(
        validator.iter_errors(&value).next().is_some(),
        "schema must reject a non-null extends"
    );
    CompiledProgramArtifact::from_legacy_format_2_json_str(&value.to_string())
        .expect_err("the loader must reject a non-null extends");
}
