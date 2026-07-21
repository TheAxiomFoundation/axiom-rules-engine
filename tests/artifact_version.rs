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
    let v1_json = serde_json::to_string(&value).expect("v1 JSON serialises");

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
fn artifact_loader_rejects_an_inverted_effective_range() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("bounded RuleSpec compiles");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    value["program"]["parameters"][0]["versions"][0]["effective_to"] =
        serde_json::json!("2025-12-31");
    let json = serde_json::to_string(&value).expect("mutated artifact serialises");

    let error = CompiledProgramArtifact::from_json_str(&json)
        .expect_err("inverted artifact range must fail before execution");
    assert!(matches!(error, CompileError::Spec(_)));
}
