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

/// A publisher stamping `requires_engine` and a consumer deciding whether an
/// artifact will load both need the number the loader actually matches. Without
/// this, the only way to discover incompatibility is to attempt a load — which
/// is how v0.1.0 and format-2 artifacts both advertised "0.1.0" while every
/// load failed.
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
        "capabilities must report the version the loader enforces"
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
