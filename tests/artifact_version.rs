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
        formula: "10"
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount + base_amount
"#;

#[test]
fn compile_stamps_format_and_engine_versions() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
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
