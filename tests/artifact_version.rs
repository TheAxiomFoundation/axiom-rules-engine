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
fn legacy_artifact_without_version_fields_still_loads() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut value = serde_json::to_value(&artifact).expect("artifact serialises");
    let object = value.as_object_mut().expect("artifact is a JSON object");
    object.remove("artifact_format_version");
    object.remove("engine_version");
    let legacy_json = serde_json::to_string(&value).expect("legacy JSON serialises");

    let reloaded = CompiledProgramArtifact::from_json_str(&legacy_json)
        .expect("legacy artifact without version fields loads");
    assert_eq!(reloaded.artifact_format_version, 0);
    assert_eq!(reloaded.engine_version, None);
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
