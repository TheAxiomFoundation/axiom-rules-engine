use axiom_rules_engine::compile::{ARTIFACT_FORMAT_VERSION, CompileError, CompiledProgramArtifact};

const SIMPLE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        effective_to: 2026-12-31
        formula: "10"
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        effective_to: 2026-12-31
        formula: amount + base_amount
"#;

const RELATION_ARGUMENT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: person_marker
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "1"
  - name: household_marker
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "1"
"#;

#[test]
fn compile_stamps_format_and_engine_versions() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    assert_eq!(ARTIFACT_FORMAT_VERSION, 2);
    assert_eq!(artifact.artifact_format_version, ARTIFACT_FORMAT_VERSION);
    assert_eq!(
        artifact.engine_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let json = serde_json::to_string(&artifact).expect("artifact serialises");
    let reloaded = CompiledProgramArtifact::from_json_str(&json)
        .expect("stamped artifact round-trips through JSON");
    assert_eq!(reloaded.artifact_format_version, ARTIFACT_FORMAT_VERSION);
    assert_eq!(
        reloaded.engine_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn v2_engine_rejects_a_v1_artifact() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles to v2");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    value["artifact_format_version"] = serde_json::json!(1);
    let v1_json = serde_json::to_string(&value).expect("v1-shaped JSON serialises");

    let error = CompiledProgramArtifact::from_json_str(&v1_json)
        .expect_err("the v2 engine must reject a v1 artifact");
    assert!(matches!(
        error,
        CompileError::UnsupportedArtifactFormatVersion {
            found: 1,
            supported: 2,
            ..
        }
    ));
}

#[test]
fn missing_and_prelaunch_artifact_versions_are_rejected() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    value
        .as_object_mut()
        .expect("artifact is a JSON object")
        .remove("artifact_format_version");
    let missing_json = serde_json::to_string(&value).expect("missing-version JSON serialises");
    assert!(
        CompiledProgramArtifact::from_json_str(&missing_json).is_err(),
        "unstamped artifacts must fail closed"
    );

    value
        .as_object_mut()
        .expect("artifact is a JSON object")
        .insert("artifact_format_version".to_string(), serde_json::json!(0));
    let v0_json = serde_json::to_string(&value).expect("v0 JSON serialises");
    let error = CompiledProgramArtifact::from_json_str(&v0_json)
        .expect_err("prelaunch v0 artifact must fail");
    assert!(matches!(
        error,
        CompileError::UnsupportedArtifactFormatVersion { found: 0, .. }
    ));
}

#[test]
fn artifact_from_newer_format_is_rejected() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    let object = value.as_object_mut().expect("artifact is a JSON object");
    object.insert(
        "artifact_format_version".to_string(),
        serde_json::json!(ARTIFACT_FORMAT_VERSION + 1),
    );
    let future_json = serde_json::to_string(&value).expect("future JSON serialises");

    let error = CompiledProgramArtifact::from_json_str(&future_json)
        .expect_err("artifact from a newer format version is rejected");
    match error {
        CompileError::UnsupportedArtifactFormatVersion {
            found, supported, ..
        } => {
            assert_eq!(found, ARTIFACT_FORMAT_VERSION + 1);
            assert_eq!(supported, ARTIFACT_FORMAT_VERSION);
        }
        other => panic!("expected UnsupportedArtifactFormatVersion, got {other:?}"),
    }
}

#[test]
fn artifact_file_round_trip_preserves_versions() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let dir = std::env::temp_dir().join(format!(
        "axiom-rules-engine-artifact-version-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let path = dir.join("program.compiled.json");
    artifact.write_json_file(&path).expect("artifact writes");

    let reloaded =
        CompiledProgramArtifact::from_json_file(&path).expect("artifact loads from file");
    assert_eq!(reloaded.artifact_format_version, ARTIFACT_FORMAT_VERSION);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn legacy_artifact_without_relation_slot_entities_still_loads() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATION_ARGUMENT_RULESPEC)
        .expect("RuleSpec relation arguments compile");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    let relation = value["program"]["relations"][0]
        .as_object_mut()
        .expect("relation is an object");
    assert_eq!(
        relation.remove("slot_entities"),
        Some(serde_json::json!(["Person", "Household"])),
        "the new artifact must carry the field before this test makes it legacy-shaped"
    );

    let loaded = CompiledProgramArtifact::from_json_str(
        &serde_json::to_string(&value).expect("legacy-shaped artifact serialises"),
    )
    .expect("an artifact without optional relation slot entities still loads");
    let round_tripped = serde_json::to_value(loaded).expect("loaded artifact serialises");
    assert!(
        round_tripped["program"]["relations"][0]
            .get("slot_entities")
            .is_none(),
        "missing slot entities default to undeclared and remain omitted"
    );
}

#[test]
fn artifact_loader_retains_unknown_tolerated_relation_fields() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    value["program"]["relations"] = serde_json::json!([{
        "name": "member_of_household",
        "arity": 2,
        "slot_entities": ["Person", "Household"]
    }]);

    let loaded = CompiledProgramArtifact::from_json_str(
        &serde_json::to_string(&value).expect("probe artifact serialises"),
    )
    .expect("format-2 readers tolerate the additive relation field");
    let round_tripped = serde_json::to_value(loaded).expect("probe artifact reserialises");
    assert_eq!(
        round_tripped["program"]["relations"][0]["slot_entities"],
        serde_json::json!(["Person", "Household"]),
        "new readers retain the field older readers safely ignore"
    );
}

#[test]
fn artifact_rejects_relation_slot_entity_count_that_differs_from_arity() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATION_ARGUMENT_RULESPEC)
        .expect("RuleSpec relation arguments compile");
    let mut value = serde_json::to_value(artifact).expect("artifact serialises");
    value["program"]["relations"][0]["slot_entities"] = serde_json::json!(["Person"]);

    let error = CompiledProgramArtifact::from_json_str(
        &serde_json::to_string(&value).expect("malformed artifact serialises"),
    )
    .expect_err("artifact relation slot kinds must match arity");
    let message = error.to_string();
    assert!(message.contains("member_of_household"), "{message}");
    assert!(message.contains("arity 2"), "{message}");
    assert!(message.contains("1 slot"), "{message}");
}

#[test]
fn artifact_loader_rejects_inconsistent_metadata_and_removed_fields() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles");
    let base = serde_json::to_value(&artifact).expect("artifact serialises");

    let mut cases = Vec::new();
    for verification in [
        serde_json::json!({"corpus_citation_path": ""}),
        serde_json::json!({"corpus_citation_path": "us/statute"}),
        serde_json::json!({"corpus_citation_path": "us/statute/26/62", "source_sha256": "bad"}),
        serde_json::json!({"corpus_citation_paths": ["us/statute/26/62"]}),
        serde_json::json!({"corpus_citation_path": "us/statute/26/62", "extra": true}),
    ] {
        let mut value = base.clone();
        value["program"]["module"] = serde_json::json!({"source_verification": verification});
        cases.push(value);
    }

    let mut plural_rule = base.clone();
    plural_rule["program"]["parameters"][0]["corpus_citation_paths"] =
        serde_json::json!(["us/statute/26/62"]);
    cases.push(plural_rule);

    let mut bad_rule_path = base.clone();
    bad_rule_path["program"]["parameters"][0]["corpus_citation_path"] =
        serde_json::json!("us/statute");
    cases.push(bad_rule_path);

    let mut bad_rule_id = base.clone();
    bad_rule_id["program"]["parameters"][0]["id"] =
        serde_json::json!("us:policies/fake#wrong_name");
    cases.push(bad_rule_id);

    let mut bad_catalog = base.clone();
    bad_catalog["metadata"]["input_catalog"] = serde_json::json!([{
        "slot": "amount",
        "canonical_request_name": "us:policies/fake#input.amount",
        "request_names": ["us:policies/fake#input.amount"]
    }]);
    cases.push(bad_catalog);

    let mut bad_order = base.clone();
    bad_order["metadata"]["evaluation_order"] = serde_json::json!([]);
    cases.push(bad_order);

    let mut bad_fast_path = base.clone();
    bad_fast_path["metadata"]["fast_path"]["strategy"] = serde_json::json!("tampered");
    cases.push(bad_fast_path);

    let mut removed_id = base.clone();
    removed_id["program"]["module"] = serde_json::json!({"id": "us:policies/base"});
    cases.push(removed_id);

    let mut removed_extends = base;
    removed_extends["program"]["extends"] = serde_json::json!("us:policies/base");
    cases.push(removed_extends);

    for value in cases {
        let json = serde_json::to_string(&value).expect("inconsistent artifact serialises");
        assert!(
            CompiledProgramArtifact::from_json_str(&json).is_err(),
            "inconsistent v1 artifact must fail: {json}"
        );
    }
}

#[test]
fn direct_program_compile_rejects_invalid_carried_citation() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles");
    let mut program = artifact.program;
    program.parameters[0].corpus_citation_path = Some("us/statute".to_string());
    assert!(CompiledProgramArtifact::compile(program).is_err());
}

/// Artifacts built on the v0.1 maintenance line serialize `extends`
/// unconditionally, so every one of them says `"extends": null` while being
/// fully composed. Released v0.2.0 rejects on the key's presence and therefore
/// cannot load a single published artifact.
#[test]
fn a_null_extends_is_not_unresolved_inheritance() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    value["program"]["extends"] = serde_json::Value::Null;

    CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect("a null extends carries no inheritance and must load");

    // A real inheritance reference is still refused.
    value["program"]["extends"] = serde_json::json!("us:policies/base");
    let error = CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect_err("a non-null extends must still be rejected");
    assert!(
        error.to_string().contains("compose before compilation"),
        "unexpected error: {error}"
    );
}

/// Absence and emptiness are different claims. An artifact predating the
/// catalog omits the field and is readable; one asserting `[]` over a program
/// that has inputs is stating something false and must not be waved through.
#[test]
fn an_absent_input_catalog_is_filled_but_an_empty_one_is_checked() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let expected = artifact.metadata.input_catalog.clone();
    assert!(
        !expected.is_empty(),
        "this program must have inputs for the test to mean anything"
    );
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");

    // Omitted entirely: readable, and the catalog is recomputed from the program.
    value["metadata"]
        .as_object_mut()
        .expect("metadata is an object")
        .remove("input_catalog");
    let loaded = CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect("an artifact predating the catalog still loads");
    assert_eq!(
        loaded.metadata.input_catalog, expected,
        "the filled catalog must match what the program actually declares"
    );

    // Explicitly empty: a false claim, and still a mismatch.
    value["metadata"]["input_catalog"] = serde_json::json!([]);
    let error = CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect_err("an explicitly empty catalog must not be accepted");
    assert!(
        error.to_string().contains("metadata does not match"),
        "unexpected error: {error}"
    );
}

/// Published us-tn-snap carries compatible duplicate parameters that load-time
/// normalization collapses. Reconstructing an absent input catalog runs
/// `to_program()`, which rejects those duplicates — so the fill must happen
/// AFTER normalization. Doing it before turned that artifact into the only
/// load failure in the release (33/34).
#[test]
fn absent_catalog_fill_happens_after_duplicate_normalization() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    // Reshape to the v0.1-maintenance emission: null extends, no catalog, and
    // the same parameter declared twice with compatible (mergeable) versions.
    value["program"]["extends"] = serde_json::Value::Null;
    value["metadata"]
        .as_object_mut()
        .expect("metadata is an object")
        .remove("input_catalog");
    let parameters = value["program"]["parameters"]
        .as_array_mut()
        .expect("parameters is an array");
    // Clone the declaration and shift its version window so the two entries
    // are compatible-mergeable rather than conflicting; reusing the serialized
    // version shape keeps the fixture honest about what engines actually emit.
    let mut duplicate = parameters[0].clone();
    duplicate["versions"][0]["effective_from"] = serde_json::json!("2027-01-01");
    duplicate["versions"][0]["effective_to"] = serde_json::json!("2027-12-31");
    parameters.push(duplicate);

    let loaded = CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect("duplicates must normalize before the catalog fill runs to_program()");
    assert_eq!(
        loaded.program.parameters.len(),
        1,
        "the compatible duplicates collapse into one declaration"
    );
    assert!(
        loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "duplicate_parameter_name"),
        "the merge is diagnosed, not silent"
    );
}
