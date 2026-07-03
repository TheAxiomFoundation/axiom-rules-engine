//! Bidirectional fidelity tests: the published module schema must agree with
//! the engine's serde *deserialization* layer — a document serde accepts
//! validates, and a document serde rejects at deserialization does not.
//!
//! These pin the fidelity claims the schema makes, especially the ones a
//! naive `#[derive(JsonSchema)]` would get wrong:
//!
//! - `kind` accepts ANY string (`RuleKind::Unsupported`), so an unknown kind
//!   *validates* even though lowering later rejects it.
//! - `module.encoding_provenance` is `deny_unknown_fields`, so an unknown
//!   subfield is *rejected*.
//! - closed enums (`validation[].status`, `source_relation.type`) reject
//!   unknown variants.
//! - explicit `null` is accepted for defaulted `Vec`/`Option` fields.
//!
//! The distinction the schema encodes is *deserialization*, not full lowering:
//! a document can validate and still fail `lower_rulespec_str` for semantic
//! reasons (unknown kind, top-level relations, missing effective_from). That
//! boundary is asserted here too.

#![cfg(feature = "schema")]

use axiom_rules_engine::schema::all_schemas;
use jsonschema::Validator;
use serde_json::Value;

fn module_validator() -> Validator {
    let schema = all_schemas()
        .into_iter()
        .find(|n| n.file_name == "rulespec-module.v1.schema.json")
        .expect("module schema published")
        .schema;
    jsonschema::draft7::new(&schema).expect("module schema compiles")
}

/// Does the engine *deserialize* this YAML? We can only observe the combined
/// deserialize+lower entry point, so we treat "lowers OK" and "failed for a
/// non-YAML (i.e. semantic/lowering) reason" both as "deserialized", and a
/// `yaml parse error:` prefix as "did not deserialize". The engine's
/// deserialization errors are the ones `serde_yaml` raises, which
/// `RuleSpecError::Yaml` renders with that prefix.
fn engine_deserializes(yaml: &str) -> bool {
    match axiom_rules_engine::rulespec::lower_rulespec_str(yaml) {
        Ok(_) => true,
        Err(error) => !format!("{error}").starts_with("yaml parse error:"),
    }
}

fn schema_validates(validator: &Validator, yaml: &str) -> bool {
    match serde_yaml::from_str::<Value>(yaml) {
        Ok(value) => validator.is_valid(&value),
        Err(_) => false,
    }
}

/// The core agreement assertion: for a document that is at least a
/// discriminated RuleSpec, schema-validity and serde-deserializability match.
fn assert_agrees(validator: &Validator, label: &str, yaml: &str) {
    let de = engine_deserializes(yaml);
    let val = schema_validates(validator, yaml);
    assert_eq!(
        de, val,
        "schema/serde disagree for `{label}`: engine_deserializes={de}, schema_validates={val}\n{yaml}"
    );
}

#[test]
fn schema_rejects_what_serde_rejects_at_deserialization() {
    let v = module_validator();
    // Each of these fails serde deserialization; the schema must reject too.
    assert_agrees(
        &v,
        "rules-is-string",
        "format: rulespec/v1\nrules: not-a-list\n",
    );
    assert_agrees(
        &v,
        "module-is-string",
        "format: rulespec/v1\nmodule: hello\nrules: []\n",
    );
    assert_agrees(
        &v,
        "provenance-unknown-field",
        "format: rulespec/v1\nmodule:\n  encoding_provenance:\n    bogus: x\nrules: []\n",
    );
    assert_agrees(
        &v,
        "validation-bad-status",
        "format: rulespec/v1\nmodule:\n  validation:\n    - oracle: taxsim\n      status: sideways\nrules: []\n",
    );
    assert_agrees(
        &v,
        "source-relation-type-bad",
        "format: rulespec/v1\nrules:\n  - name: r\n    kind: source_relation\n    source_relation:\n      type: bogus\n      target: 'us:x#y'\n",
    );
    assert_agrees(
        &v,
        "rule-missing-name",
        "format: rulespec/v1\nrules:\n  - kind: parameter\n    formula: '1'\n",
    );
}

#[test]
fn schema_accepts_what_serde_accepts() {
    let v = module_validator();
    // Explicit null on defaulted Vec / Option fields.
    assert_agrees(
        &v,
        "imports-null",
        "format: rulespec/v1\nimports:\nrules: []\n",
    );
    assert_agrees(&v, "rules-null", "format: rulespec/v1\nrules:\n");
    assert_agrees(
        &v,
        "validation-null",
        "format: rulespec/v1\nmodule:\n  validation:\nrules: []\n",
    );
    // String-like coercion: a numeric unit.
    assert_agrees(
        &v,
        "unit-number",
        "format: rulespec/v1\nrules:\n  - name: p\n    kind: parameter\n    unit: 5\n    formula: '1'\n    effective_from: '2020-01-01'\n",
    );
    // A parameter table with integer-keyed bare scalar values.
    assert_agrees(
        &v,
        "indexed-values",
        "format: rulespec/v1\nrules:\n  - name: p\n    kind: parameter\n    indexed_by: household_size\n    versions:\n      - effective_from: '2020-01-01'\n        values:\n          1: 100\n          2: '200.50'\n",
    );
}

#[test]
fn unknown_rule_kind_validates_but_does_not_lower() {
    // The load-bearing RuleKind fidelity case. `kind: <anything>` deserializes
    // into RuleKind::Unsupported, so the schema (which keeps `kind` an open
    // string) MUST accept it — but lowering rejects it. A derived schema with
    // a closed enum would wrongly reject the file that serde accepts.
    let v = module_validator();
    let yaml = "format: rulespec/v1\nrules:\n  - name: r\n    kind: totally_new_kind\n    formula: '1'\n    effective_from: '2020-01-01'\n";

    let value: Value = serde_yaml::from_str(yaml).expect("valid yaml");
    assert!(
        v.is_valid(&value),
        "unknown kind must validate against the schema (RuleKind accepts any string)"
    );

    let lowered = axiom_rules_engine::rulespec::lower_rulespec_str(yaml);
    let error = lowered.expect_err("unknown kind must fail lowering");
    assert!(
        format!("{error}").contains("unsupported kind"),
        "expected an unsupported-kind lowering error, got: {error}"
    );
}

#[test]
fn known_rule_kinds_validate() {
    let v = module_validator();
    for kind in [
        "parameter",
        "derived",
        "data_relation",
        "derived_relation",
        "source_relation",
    ] {
        let yaml = format!("format: rulespec/v1\nrules:\n  - name: r\n    kind: {kind}\n");
        let value: Value = serde_yaml::from_str(&yaml).expect("valid yaml");
        assert!(
            v.is_valid(&value),
            "known kind `{kind}` must validate structurally"
        );
    }
}

#[test]
fn bad_source_sha256_is_rejected_by_pattern() {
    // The engine rejects a non-64-hex sha256 in `validate_module_metadata`
    // (a post-deserialize check). The schema mirrors it with a `pattern`, so
    // the schema catches it too — a rare case where structural validation
    // covers a semantic engine check.
    let v = module_validator();
    let yaml = "format: rulespec/v1\nmodule:\n  source_verification:\n    source_sha256: 'abc'\nrules: []\n";
    let value: Value = serde_yaml::from_str(yaml).expect("valid yaml");
    assert!(
        !v.is_valid(&value),
        "a non-64-hex source_sha256 must fail the schema pattern"
    );
    let good = format!(
        "format: rulespec/v1\nmodule:\n  source_verification:\n    source_sha256: '{}'\nrules: []\n",
        "a".repeat(64)
    );
    let good_value: Value = serde_yaml::from_str(&good).expect("valid yaml");
    assert!(v.is_valid(&good_value), "a 64-hex source_sha256 must pass");
}
