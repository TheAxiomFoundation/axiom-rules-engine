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
use proptest::prelude::*;
use serde_json::Value;
use std::sync::LazyLock;

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
    assert_agrees(
        &v,
        "upstream-source-check-null",
        "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/guidance/treasury/rate\n    upstream_source_check:\nrules: []\n",
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
    let yaml = "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/statute/7/2017/a\n    source_sha256: 'abc'\nrules: []\n";
    let value: Value = serde_yaml::from_str(yaml).expect("valid yaml");
    assert!(
        !v.is_valid(&value),
        "a non-64-hex source_sha256 must fail the schema pattern"
    );
    let good = format!(
        "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/guidance/treasury/rate\n    source_sha256: '{}'\n    upstream_source_check:\n      status: official_parameter_source\n      checked_paths: [us/statute/7/2017/a]\n      rationale: Guidance supplies the current annual parameter.\nrules: []\n",
        "a".repeat(64)
    );
    let good_value: Value = serde_yaml::from_str(&good).expect("valid yaml");
    assert!(v.is_valid(&good_value), "a 64-hex source_sha256 must pass");

    for invalid in [
        "format: rulespec/v1\nmodule:\n  source_verification:\n    source_sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nrules: []\n",
        "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_paths: [us/statute/7/2017/a]\nrules: []\n",
        "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/guidance/treasury/rate\n    upstream_source_check:\n      status: official_parameter_source\n      checked_paths: [us/statute/7/2017/a]\n      rationale: checked\n      note: unknown\nrules: []\n",
    ] {
        let value: Value = serde_yaml::from_str(invalid).expect("valid yaml");
        assert!(!v.is_valid(&value), "invalid source verification must fail");
    }
}

#[test]
fn schema_requires_exact_discriminator_and_rejects_removed_directives() {
    let validator = module_validator();
    for yaml in [
        "schema: axiom.rules.module.v1\nrules: []\n",
        "format: rulespec/v1\nschema: axiom.rules.module.v1\nrules: []\n",
        "format: rulespec/v1\nextends: us:policies/base\nrules: []\n",
        "format: wrong/v1\nrules: []\n",
        "format: rulespec/v1\nimports: [us:policies/base.YAML]\nrules: []\n",
    ] {
        let value: Value = serde_yaml::from_str(yaml).expect("valid YAML fixture");
        assert!(
            !validator.is_valid(&value),
            "removed alias must fail: {yaml}"
        );
    }
}

fn interpretation_document(status: &str) -> Value {
    let mut entry = serde_json::json!({
        "id": "daily-twenty-per-cent",
        "question": "Does the percentage apply once a week or each day?",
        "texts": [{
            "corpus_citation_path": "uk-babergh/manual/council-tax-reduction-scheme-2026-2027/page-87",
            "excerpt": "The prescribed daily percentage"
        }],
        "reading": "Apply 20% of the weekly excess once a week.",
        "alternatives": [{"reading": "Apply 20% of the weekly excess each day.", "effect": "Support ends sooner."}],
        "basis": "The scheme's weekly table supplies the intended taper.",
        "status": status,
        "affects": ["p"]
    });
    if status != "open" {
        entry["ruling"] = serde_json::json!({
            "by": "Axiom Foundation",
            "date": "2026-10-10",
            "record": "https://github.com/TheAxiomFoundation/rulespec-uk/pull/449"
        });
    }
    if matches!(status, "confirmed" | "contradicted") {
        entry["confirmation"] = serde_json::json!({
            "by": "Babergh District Council",
            "date": "2026-10-20",
            "excerpt": "The percentage applies once a week.",
            "record": "https://example.org/publisher-reply"
        });
    }
    serde_json::json!({
        "format": "rulespec/v1",
        "module": {"interpretations": [entry]},
        "rules": [{"name": "p", "kind": "parameter", "formula": "1", "effective_from": "2020-01-01"}]
    })
}

fn interpretation_validators() -> Vec<Validator> {
    all_schemas()
        .into_iter()
        .filter(|named| {
            matches!(
                named.file_name,
                "rulespec-module.v1.schema.json"
                    | "compiled-artifact.v2.schema.json"
                    | "execution-request.v1.schema.json"
            )
        })
        .map(|named| {
            jsonschema::draft7::options()
                .should_validate_formats(true)
                .build(&named.schema)
                .expect("schema compiles with date format validation")
        })
        .collect()
}

fn interpretation_schema_surfaces(document: &Value) -> [Value; 3] {
    let valid = interpretation_document("open");
    let artifact = axiom_rules_engine::compile::CompiledProgramArtifact::from_rulespec_str(
        &serde_yaml::to_string(&valid).unwrap(),
    )
    .expect("base module compiles");
    let mut compiled = serde_json::to_value(artifact).unwrap();
    compiled["program"]["module"] = document["module"].clone();
    let request = serde_json::json!({
        "mode": "explain",
        "program": compiled["program"],
        "dataset": {"inputs": [], "relations": []},
        "queries": []
    });
    [document.clone(), compiled, request]
}

static PUBLISHED_INTERPRETATION_VALIDATORS: LazyLock<[Validator; 3]> = LazyLock::new(|| {
    [
        include_str!("../schemas/rulespec-module.v1.schema.json"),
        include_str!("../schemas/compiled-artifact.v2.schema.json"),
        include_str!("../schemas/execution-request.v1.schema.json"),
    ]
    .map(|schema| {
        jsonschema::draft7::options()
            .should_validate_formats(true)
            .build(&serde_json::from_str::<Value>(schema).expect("published schema is JSON"))
            .expect("published schema compiles with date format validation")
    })
});

static GENERATED_INTERPRETATION_VALIDATORS: LazyLock<Vec<Validator>> =
    LazyLock::new(interpretation_validators);

const INTERPRETATION_NONBLANK_FIELDS: [&str; 8] = [
    "/module/interpretations/0/question",
    "/module/interpretations/0/reading",
    "/module/interpretations/0/basis",
    "/module/interpretations/0/texts/0/excerpt",
    "/module/interpretations/0/alternatives/0/reading",
    "/module/interpretations/0/ruling/by",
    "/module/interpretations/0/confirmation/by",
    "/module/interpretations/0/confirmation/excerpt",
];

const UNICODE_WHITESPACE: [char; 25] = [
    '\u{0009}', '\u{000a}', '\u{000b}', '\u{000c}', '\u{000d}', '\u{0020}', '\u{0085}', '\u{00a0}',
    '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
    '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}',
    '\u{3000}',
];

const NON_WHITESPACE_LOOKALIKES: [char; 8] = [
    '\u{001c}', '\u{001d}', '\u{001e}', '\u{001f}', '\u{180e}', '\u{200b}', '\u{2060}', '\u{feff}',
];

fn assert_interpretation_nonblank_agrees_with_trim(text: &str, field: &str) {
    let expected = !text.trim().is_empty();
    let mut document = interpretation_document("confirmed");
    *document.pointer_mut(field).unwrap() = serde_json::json!(text);
    assert_eq!(
        axiom_rules_engine::rulespec::lower_rulespec_str(
            &serde_yaml::to_string(&document).unwrap()
        )
        .is_ok(),
        expected,
        "loader: {field}, text {text:?}"
    );
    let surfaces = interpretation_schema_surfaces(&document);
    for (kind, validators) in [
        ("generated", GENERATED_INTERPRETATION_VALIDATORS.as_slice()),
        ("published", PUBLISHED_INTERPRETATION_VALIDATORS.as_slice()),
    ] {
        for (index, (validator, surface)) in validators.iter().zip(&surfaces).enumerate() {
            assert_eq!(
                validator.is_valid(surface),
                expected,
                "{kind} schema {index}: {field}, text {text:?}"
            );
        }
    }
}

#[test]
fn interpretation_unicode_next_line_is_blank() {
    for field in INTERPRETATION_NONBLANK_FIELDS {
        assert_interpretation_nonblank_agrees_with_trim("\u{0085}", field);
    }
}

#[test]
fn interpretation_unicode_byte_order_mark_is_nonblank() {
    for field in INTERPRETATION_NONBLANK_FIELDS {
        assert_interpretation_nonblank_agrees_with_trim("\u{feff}", field);
    }
}

#[test]
fn interpretation_unicode_whitespace_matches_trim() {
    for character in UNICODE_WHITESPACE
        .into_iter()
        .chain(NON_WHITESPACE_LOOKALIKES)
    {
        let text = character.to_string();
        for field in INTERPRETATION_NONBLANK_FIELDS {
            assert_interpretation_nonblank_agrees_with_trim(&text, field);
            assert_interpretation_nonblank_agrees_with_trim(&format!("{text}x{text}"), field);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn interpretation_unicode_whitespace_property_matches_trim(
        whitespace in prop::collection::vec(prop::sample::select(UNICODE_WHITESPACE.to_vec()), 0..24),
        content in prop::option::of(prop_oneof![
            prop::sample::select(NON_WHITESPACE_LOOKALIKES.to_vec()),
            any::<char>(),
        ]),
        field_index in 0usize..INTERPRETATION_NONBLANK_FIELDS.len(),
    ) {
        let mut text: String = whitespace.iter().collect();
        if let Some(content) = content {
            text.push(content);
        }
        text.extend(whitespace);
        assert_interpretation_nonblank_agrees_with_trim(
            &text,
            INTERPRETATION_NONBLANK_FIELDS[field_index],
        );
    }

    #[test]
    fn published_interpretation_schemas_agree_with_loader_for_generated_entries(
        id in "[a-z0-9]{1,10}(-[a-z0-9]{1,8}){0,3}",
        prose in "[A-Za-z][A-Za-z0-9 ]{0,40}",
        status_index in 0usize..4,
        year in 1900u16..2101,
        month in 1u8..13,
        day in 1u8..29,
        mutation_index in 0usize..128,
    ) {
        let status = ["open", "ruled", "confirmed", "contradicted"][status_index];
        let mut valid = interpretation_document(status);
        let entry = &mut valid["module"]["interpretations"][0];
        entry["id"] = serde_json::json!(id);
        for field in ["question", "reading", "basis"] {
            entry[field] = serde_json::json!(prose);
        }
        entry["texts"][0]["corpus_citation_path"] =
            serde_json::json!(format!("uk/manual/scheme/{id}"));
        entry["texts"][0]["excerpt"] = serde_json::json!(prose);
        entry["alternatives"][0]["reading"] = serde_json::json!(prose);
        for field in ["ruling", "confirmation"] {
            if let Some(block) = entry.get_mut(field) {
                block["by"] = serde_json::json!(prose);
                block["date"] = serde_json::json!(format!("{year:04}-{month:02}-{day:02}"));
                block["record"] = serde_json::json!(format!("https://example.org/{id}"));
                if field == "confirmation" {
                    block["excerpt"] = serde_json::json!(prose);
                }
            }
        }

        // Each candidate changes one field of this valid entry. Restrict
        // nested-block mutations to blocks present for its generated status.
        let root = "/module/interpretations/0";
        let mut mutations = Vec::new();
        for field in ["id", "question", "texts", "reading", "alternatives", "basis", "status", "affects"] {
            mutations.push((format!("{root}/{field}"), None));
        }
        for field in ["texts", "alternatives", "affects"] {
            mutations.push((format!("{root}/{field}"), Some(serde_json::json!([]))));
        }
        for field in ["question", "reading", "basis", "texts/0/excerpt", "alternatives/0/reading"] {
            mutations.push((format!("{root}/{field}"), Some(serde_json::json!(" \n\t"))));
        }
        for field in ["unknown", "texts/0/unknown", "alternatives/0/unknown"] {
            mutations.push((format!("{root}/{field}"), Some(serde_json::json!("extra"))));
        }
        mutations.push((format!("{root}/id"), Some(serde_json::json!("Invalid_ID"))));
        mutations.push((format!("{root}/status"), Some(serde_json::json!("uncertain"))));
        mutations.push((format!("{root}/texts/0/corpus_citation_path"), Some(serde_json::json!("uk/manual"))));
        let supplied = interpretation_document("confirmed");
        for field in ["ruling", "confirmation"] {
            let block = entry.get(field);
            let replacement = if block.is_some() {
                None
            } else {
                Some(supplied["module"]["interpretations"][0][field].clone())
            };
            mutations.push((format!("{root}/{field}"), replacement));
            if block.is_some() {
                for (nested, replacement) in [
                    ("date", "2026-2-01"),
                    ("date", "2026-02-29"),
                    ("record", "http://example.org/record"),
                    ("by", " \n\t"),
                    ("unknown", "extra"),
                ] {
                    mutations.push((format!("{root}/{field}/{nested}"), Some(serde_json::json!(replacement))));
                }
                if field == "confirmation" {
                    mutations.push((format!("{root}/{field}/excerpt"), Some(serde_json::json!(" \n\t"))));
                }
            }
        }
        let (path, replacement) = &mutations[mutation_index % mutations.len()];
        let mut invalid = valid.clone();
        if let Some(replacement) = replacement {
            if let Some(value) = invalid.pointer_mut(path) {
                *value = replacement.clone();
            } else {
                let (parent, field) = path.rsplit_once('/').unwrap();
                invalid.pointer_mut(parent).unwrap()[field] = replacement.clone();
            }
        } else {
            let (parent, field) = path.rsplit_once('/').unwrap();
            invalid.pointer_mut(parent).unwrap().as_object_mut().unwrap().remove(field);
        }

        for (document, expected) in [(&valid, true), (&invalid, false)] {
            prop_assert_eq!(axiom_rules_engine::rulespec::lower_rulespec_str(
                &serde_yaml::to_string(document).unwrap()
            ).is_ok(), expected, "loader: status {}, mutation {}", status, path);
            for (index, (validator, surface)) in PUBLISHED_INTERPRETATION_VALIDATORS
                .iter().zip(interpretation_schema_surfaces(document)).enumerate()
            {
                prop_assert_eq!(validator.is_valid(&surface), expected,
                    "published schema {}: status {}, mutation {}", index, status, path);
            }
        }
    }
}

#[test]
fn interpretations_validate_in_module_and_current_artifact_schemas() {
    let validators = interpretation_validators();
    assert_eq!(validators.len(), 3);
    for status in ["open", "ruled", "confirmed", "contradicted"] {
        let document = interpretation_document(status);
        let source = serde_yaml::to_string(&document).unwrap();
        let artifact =
            axiom_rules_engine::compile::CompiledProgramArtifact::from_rulespec_str(&source)
                .expect("valid interpretations compile");
        let compiled = serde_json::to_value(artifact).unwrap();
        let request = serde_json::json!({
            "mode": "explain",
            "program": compiled["program"],
            "dataset": {"inputs": [], "relations": []},
            "queries": []
        });
        let surfaces = [document, compiled, request];
        for (validator, surface) in validators.iter().zip(surfaces) {
            let errors = validator
                .iter_errors(&surface)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            assert!(errors.is_empty(), "status {status}: {}", errors.join("\n"));
        }
    }
}

#[test]
fn interpretation_schema_rejects_unknown_keys_missing_fields_and_wrong_types() {
    let validators = interpretation_validators();
    for status in ["open", "ruled", "confirmed"] {
        let base = interpretation_document(status);
        let mut object_paths = vec![
            "/module/interpretations/0",
            "/module/interpretations/0/texts/0",
            "/module/interpretations/0/alternatives/0",
        ];
        if status != "open" {
            object_paths.push("/module/interpretations/0/ruling");
        }
        if status == "confirmed" {
            object_paths.push("/module/interpretations/0/confirmation");
        }
        for object_path in object_paths {
            let mut unknown = base.clone();
            unknown.pointer_mut(object_path).unwrap()["unknown"] = serde_json::json!("value");
            assert!(!engine_deserializes(
                &serde_yaml::to_string(&unknown).unwrap()
            ));
            for (validator, surface) in validators
                .iter()
                .zip(interpretation_schema_surfaces(&unknown))
            {
                assert!(
                    !validator.is_valid(&surface),
                    "unknown key at {object_path} must fail"
                );
            }
        }
        let entry = base
            .pointer("/module/interpretations/0")
            .unwrap()
            .as_object()
            .unwrap();
        for field in [
            "id",
            "question",
            "texts",
            "reading",
            "alternatives",
            "basis",
            "status",
            "affects",
        ] {
            assert!(entry.contains_key(field));
            let mut missing = base.clone();
            missing["module"]["interpretations"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(!engine_deserializes(
                &serde_yaml::to_string(&missing).unwrap()
            ));
            for (validator, surface) in validators
                .iter()
                .zip(interpretation_schema_surfaces(&missing))
            {
                assert!(!validator.is_valid(&surface), "missing {field} must fail");
            }
        }
    }
    for field in [
        "/module/interpretations/0/id",
        "/module/interpretations/0/question",
        "/module/interpretations/0/texts/0/corpus_citation_path",
        "/module/interpretations/0/texts/0/excerpt",
        "/module/interpretations/0/reading",
        "/module/interpretations/0/alternatives/0/reading",
        "/module/interpretations/0/alternatives/0/effect",
        "/module/interpretations/0/basis",
        "/module/interpretations/0/ruling/by",
        "/module/interpretations/0/ruling/date",
        "/module/interpretations/0/ruling/record",
        "/module/interpretations/0/confirmation/by",
        "/module/interpretations/0/confirmation/date",
        "/module/interpretations/0/confirmation/excerpt",
        "/module/interpretations/0/confirmation/record",
        "/module/interpretations/0/affects/0",
    ] {
        let mut wrong_type = interpretation_document("confirmed");
        *wrong_type.pointer_mut(field).unwrap() = serde_json::json!(42);
        assert!(
            !engine_deserializes(&serde_yaml::to_string(&wrong_type).unwrap()),
            "non-string {field} must fail serde"
        );
        for (validator, surface) in validators
            .iter()
            .zip(interpretation_schema_surfaces(&wrong_type))
        {
            assert!(
                !validator.is_valid(&surface),
                "non-string {field} must fail schema"
            );
        }
    }
}

#[test]
fn interpretation_schemas_enforce_load_time_metadata_constraints() {
    let validators = interpretation_validators();
    let invalid_values = [
        (
            "/module/interpretations/0/id",
            serde_json::json!("Daily_Percentage"),
        ),
        (
            "/module/interpretations/0/question",
            serde_json::json!(" \n\t"),
        ),
        ("/module/interpretations/0/reading", serde_json::json!("")),
        ("/module/interpretations/0/basis", serde_json::json!("  ")),
        ("/module/interpretations/0/texts", serde_json::json!([])),
        (
            "/module/interpretations/0/texts/0/excerpt",
            serde_json::json!(" "),
        ),
        (
            "/module/interpretations/0/texts/0/corpus_citation_path",
            serde_json::json!("uk/manual"),
        ),
        (
            "/module/interpretations/0/alternatives",
            serde_json::json!([]),
        ),
        (
            "/module/interpretations/0/alternatives/0/reading",
            serde_json::json!(" "),
        ),
        ("/module/interpretations/0/affects", serde_json::json!([])),
        (
            "/module/interpretations/0/status",
            serde_json::json!("uncertain"),
        ),
        (
            "/module/interpretations/0/ruling/by",
            serde_json::json!(" "),
        ),
        (
            "/module/interpretations/0/ruling/date",
            serde_json::json!("2026-2-01"),
        ),
        (
            "/module/interpretations/0/ruling/date",
            serde_json::json!("2026-02-29"),
        ),
        (
            "/module/interpretations/0/ruling/record",
            serde_json::json!("http://example.org/ruling"),
        ),
        (
            "/module/interpretations/0/confirmation/by",
            serde_json::json!(" "),
        ),
        (
            "/module/interpretations/0/confirmation/date",
            serde_json::json!("2026-04-31"),
        ),
        (
            "/module/interpretations/0/confirmation/excerpt",
            serde_json::json!(" "),
        ),
        (
            "/module/interpretations/0/confirmation/record",
            serde_json::json!("http://example.org/reply"),
        ),
    ];
    for (field, value) in invalid_values {
        let mut document = interpretation_document("confirmed");
        *document.pointer_mut(field).unwrap() = value;
        assert!(
            axiom_rules_engine::rulespec::lower_rulespec_str(
                &serde_yaml::to_string(&document).unwrap()
            )
            .is_err(),
            "invalid {field} must fail load"
        );
        for (validator, surface) in validators
            .iter()
            .zip(interpretation_schema_surfaces(&document))
        {
            assert!(
                !validator.is_valid(&surface),
                "invalid {field} must fail schema"
            );
        }
    }
}

#[test]
fn interpretation_schemas_enforce_status_dependent_blocks_and_reject_nulls() {
    let validators = interpretation_validators();
    for status in ["open", "ruled", "confirmed", "contradicted"] {
        for field in ["ruling", "confirmation"] {
            let mut document = interpretation_document(status);
            let entry = document["module"]["interpretations"][0]
                .as_object_mut()
                .unwrap();
            if entry.remove(field).is_none() {
                let supplied = interpretation_document("confirmed");
                entry.insert(
                    field.to_string(),
                    supplied["module"]["interpretations"][0][field].clone(),
                );
            }
            for (validator, surface) in validators
                .iter()
                .zip(interpretation_schema_surfaces(&document))
            {
                assert!(
                    !validator.is_valid(&surface),
                    "status {status} with wrong {field} presence must fail"
                );
            }
        }
    }
    for field in [
        "/module/interpretations",
        "/module/interpretations/0/ruling",
        "/module/interpretations/0/confirmation",
        "/module/interpretations/0/confirmation/record",
        "/module/interpretations/0/alternatives/0/effect",
    ] {
        let mut document = interpretation_document("confirmed");
        *document.pointer_mut(field).unwrap() = Value::Null;
        assert!(
            !engine_deserializes(&serde_yaml::to_string(&document).unwrap()),
            "null {field} must fail serde"
        );
        for (validator, surface) in validators
            .iter()
            .zip(interpretation_schema_surfaces(&document))
        {
            assert!(
                !validator.is_valid(&surface),
                "null {field} must fail schema"
            );
        }
    }
}
