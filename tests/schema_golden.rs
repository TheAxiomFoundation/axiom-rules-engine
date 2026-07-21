//! Golden-file and self-consistency tests for the published JSON Schemas.
//!
//! - `schemas_are_current` regenerates every schema in memory and asserts the
//!   checked-in `schemas/*.json` copy is byte-identical, so CI fails on drift
//!   between the Rust types and the published files. Refresh with
//!   `cargo run -- emit-schemas --out schemas`.
//! - `published_schemas_are_valid_draft07` compiles each published schema as a
//!   JSON Schema, catching a malformed hand-written fragment.
//! - `artifact_schema_accepts_a_real_compiled_artifact` compiles an in-repo
//!   RuleSpec fixture to a `CompiledProgramArtifact` and validates the actual
//!   serialized bytes against the derived artifact schema — the round-trip that
//!   proves the schema matches what the engine emits.

#![cfg(feature = "schema")]

use std::path::PathBuf;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::schema::{all_schemas, to_pretty_string};

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas")
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
         (run `cargo run -- emit-schemas --out schemas`):\n{}",
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
fn artifact_schema_accepts_a_real_compiled_artifact() {
    // A real, engine-valid RuleSpec module lives in the fixtures. Compile it
    // the way the CLI does and validate the serialized artifact against the
    // derived schema.
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rulespec/uksi/2013/376/rules.yaml");
    let artifact = CompiledProgramArtifact::from_rulespec_file(&fixture)
        .expect("fixture compiles to an artifact");
    let artifact_json = serde_json::to_value(&artifact).expect("artifact serializes to JSON");

    let schema_value = all_schemas()
        .into_iter()
        .find(|named| named.file_name == "compiled-artifact.v2.schema.json")
        .expect("artifact schema is published")
        .schema;
    let validator = jsonschema::draft7::new(&schema_value).expect("artifact schema compiles");

    let errors: Vec<String> = validator
        .iter_errors(&artifact_json)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "compiled artifact did not validate against its own schema:\n{}",
        errors.join("\n")
    );
}

#[test]
fn artifact_schema_accepts_the_annotated_divergences() {
    // Exercise the manually-annotated spots in the derived artifact schema
    // that a plain schemars derive would get wrong or narrow: `dtype` aliases
    // (`money` for decimal), decimal values as both a bare integer and a
    // string, a date value, module metadata with a flattened
    // `source_verification.extra` field, and a judgment/comparison expression.
    // If any of these regress, this artifact stops validating.
    let artifact = serde_json::json!({
        "artifact_format_version": 2,
        "engine_version": "0.1.0",
        "program": {
            "module": {
                "id": "us:statutes/7/2017/a",
                "source_verification": {
                    "corpus_citation_path": "us/statute/7/2017/a",
                    "source_sha256": "a".repeat(64),
                    "corpus_citation_paths": ["us/statute/7/2017/a"]
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
            "fast_path": {"strategy": "s", "compatible": true, "blockers": []}}
    });

    let schema_value = all_schemas()
        .into_iter()
        .find(|named| named.file_name == "compiled-artifact.v2.schema.json")
        .expect("artifact schema is published")
        .schema;
    let validator = jsonschema::draft7::new(&schema_value).expect("artifact schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(&artifact)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "annotated-divergence artifact did not validate:\n{}",
        errors.join("\n")
    );

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

#[test]
fn current_artifact_schema_rejects_missing_and_v1_versions() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rulespec/uksi/2013/376/rules.yaml");
    let artifact = CompiledProgramArtifact::from_rulespec_file(&fixture)
        .expect("fixture compiles to a v2 artifact");
    let base = serde_json::to_value(&artifact).expect("artifact serializes");
    let schema = all_schemas()
        .into_iter()
        .find(|named| named.file_name == "compiled-artifact.v2.schema.json")
        .expect("current artifact schema is published")
        .schema;
    let validator = jsonschema::draft7::new(&schema).expect("artifact schema compiles");

    let mut missing = base.clone();
    missing
        .as_object_mut()
        .expect("artifact is an object")
        .remove("artifact_format_version");
    let mut v1 = base;
    v1["artifact_format_version"] = serde_json::json!(1);

    assert!(!validator.is_valid(&missing));
    assert!(!validator.is_valid(&v1));
}
