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
