//! Integration test: the built binary reports its package version and rejects
//! stray arguments to the version command.
use std::process::Command;

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
}

#[test]
fn version_flag_prints_package_version() {
    for arg in ["--version", "version"] {
        let out = engine().arg(arg).output().expect("run engine");
        assert!(out.status.success(), "{arg} should exit 0");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            stdout.trim(),
            format!("axiom-rules-engine {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

/// The compat contract a publisher stamps into a manifest gates on the
/// artifact format version, not on semver. This is the only way a consumer can
/// read that number off a binary without attempting a load, so it has to stay
/// machine-readable and exact.
#[test]
fn capabilities_report_the_artifact_format_version() {
    let out = engine().arg("capabilities").output().expect("run engine");
    assert!(out.status.success(), "capabilities should exit 0");
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("capabilities emits JSON");
    assert_eq!(value["engine_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        value["artifact_format_version"],
        serde_json::json!(axiom_rules_engine::compile::ARTIFACT_FORMAT_VERSION),
        "capabilities must report the version the loader actually enforces"
    );
}

#[test]
fn capabilities_rejects_extra_arguments() {
    let out = engine()
        .args(["capabilities", "surprise"])
        .output()
        .expect("run engine");
    assert!(!out.status.success(), "stray arg must be an error");
}

#[test]
fn version_rejects_extra_arguments() {
    let out = engine()
        .args(["version", "surprise"])
        .output()
        .expect("run engine");
    assert!(!out.status.success(), "stray arg must be an error");
}

#[test]
fn unknown_command_is_an_error() {
    let out = engine()
        .arg("definitely-not-a-command")
        .output()
        .expect("run engine");
    assert!(!out.status.success());
}

/// `check-artifact` answers "can this engine load this?" without a request, so a
/// gate never has to infer load-vs-execute from error text. A v2 artifact that
/// violates the v2 contract must come back non-loadable even though its version
/// number matches — that is the case a version comparison cannot see.
#[test]
fn check_artifact_separates_load_failure_from_execution() {
    let dir = std::env::temp_dir().join(format!("axiom-check-artifact-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Build through the real compiler so metadata matches the program; a
    // hand-written artifact fails the derived-metadata consistency check for
    // unrelated reasons and would not test what this test is about.
    let artifact = axiom_rules_engine::compile::CompiledProgramArtifact::compile(
        axiom_rules_engine::spec::ProgramSpec::default(),
    )
    .expect("empty program compiles");
    let good = dir.join("good.compiled.json");
    artifact.write_json_file(&good).unwrap();
    let out = engine()
        .args(["check-artifact", "--artifact", good.to_str().unwrap()])
        .output()
        .expect("run engine");
    assert!(
        out.status.success(),
        "a well-formed artifact must be loadable: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let verdict: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["loadable"], true);
    assert_eq!(
        verdict["artifact_format_version"],
        verdict["engine_artifact_format_version"]
    );

    // Right version, wrong contract: `program.extends` was removed in v2. This
    // is exactly the shape the published rulespec-us artifacts carry, and the
    // shape a version-number comparison calls compatible.
    let bad = dir.join("bad.compiled.json");
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&good).unwrap()).unwrap();
    raw["program"]["extends"] = serde_json::Value::Null;
    std::fs::write(&bad, raw.to_string()).unwrap();
    let out = engine()
        .args(["check-artifact", "--artifact", bad.to_str().unwrap()])
        .output()
        .expect("run engine");
    assert!(
        !out.status.success(),
        "matching artifact_format_version must not imply loadable"
    );

    std::fs::remove_dir_all(&dir).ok();
}
