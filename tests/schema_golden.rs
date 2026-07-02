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
        .find(|named| named.file_name == "compiled-artifact.v1.schema.json")
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
