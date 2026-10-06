#![cfg(feature = "unit-derivation")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/unit_derivation")
        .join(name)
}

fn run_with_stdin(args: &[&str], input: &[u8]) -> Output {
    let mut child = engine()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aggregation CLI");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write request");
    child.wait_with_output().expect("wait for aggregation CLI")
}

#[test]
fn compiled_aggregation_cli_is_gated_registered_deterministic_and_exact() {
    let temp = std::env::temp_dir().join(format!("axiom-stage3-cli-test-{}", std::process::id()));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("remove stale test-owned directory");
    }
    fs::create_dir_all(&temp).expect("create test-owned directory");
    let temp = fs::canonicalize(&temp).expect("resolve the exact test-owned directory");
    let first_artifact = temp.join("first.json");
    let second_artifact = temp.join("second.json");
    let rulespec_root = temp.join("rulespec-nz");
    let source_program =
        rulespec_root.join("nz/statutes/income_tax/family_scheme/tax_credits.yaml");
    fs::create_dir_all(source_program.parent().unwrap()).expect("create canonical source tree");
    fs::copy(
        fixture("nz_best_start_gross.rulespec.yaml"),
        &source_program,
    )
    .expect("copy source fixture into canonical tree");
    let source_artifact = temp.join("source.json");
    let source_compile = engine()
        .args([
            "compile",
            "--program",
            source_program.to_str().unwrap(),
            "--rulespec-root",
            rulespec_root.to_str().unwrap(),
            "--output",
            source_artifact.to_str().unwrap(),
        ])
        .output()
        .expect("compile provenance source artifact");
    assert!(
        source_compile.status.success(),
        "source compile failed: {}",
        String::from_utf8_lossy(&source_compile.stderr)
    );
    let plan = fixture("nz_income_explorer_family.yaml");
    let request = fs::read(fixture("nz_income_explorer_request.json")).unwrap();
    let expected = fs::read(fixture("nz_income_explorer_result.json")).unwrap();

    for artifact in [&first_artifact, &second_artifact] {
        let output = engine()
            .args([
                "compile-unit-aggregation",
                "--plan",
                plan.to_str().unwrap(),
                "--source-artifact",
                source_artifact.to_str().unwrap(),
                "--output",
                artifact.to_str().unwrap(),
            ])
            .output()
            .expect("compile aggregation artifact");
        assert!(
            output.status.success(),
            "compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        fs::read(&first_artifact).unwrap(),
        fs::read(&second_artifact).unwrap(),
        "registered compilation must be byte-deterministic"
    );

    let artifact = first_artifact.to_str().unwrap();
    let disabled = run_with_stdin(&["run-unit-aggregation", "--artifact", artifact], &request);
    assert!(!disabled.status.success());
    assert!(String::from_utf8_lossy(&disabled.stderr).contains("unit derivation is disabled"));

    let raw_plan = run_with_stdin(
        &[
            "run-unit-aggregation",
            "--plan",
            plan.to_str().unwrap(),
            "--enable-experimental-unit-derivation",
        ],
        &request,
    );
    assert!(!raw_plan.status.success());
    assert!(
        String::from_utf8_lossy(&raw_plan.stderr)
            .contains("unknown run-unit-aggregation argument `--plan`")
    );

    let args = [
        "run-unit-aggregation",
        "--artifact",
        artifact,
        "--enable-experimental-unit-derivation",
    ];
    let first = run_with_stdin(&args, &request);
    let second = run_with_stdin(&args, &request);
    assert!(
        first.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stdout, expected, "fixture JSON must match exactly");

    let mut malformed: serde_json::Value = serde_json::from_slice(&request).unwrap();
    malformed["unexpected_request_field"] = serde_json::json!(true);
    let malformed = run_with_stdin(&args, &serde_json::to_vec(&malformed).unwrap());
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("unknown field"));

    let mut unknown_relation: serde_json::Value = serde_json::from_slice(&request).unwrap();
    unknown_relation["relations"][1]["facts"][0]["knowledge"] = serde_json::json!({
        "status": "unknown",
        "evidence": {"id": "cli-child-relation-unknown"}
    });
    let unknown = run_with_stdin(&args, &serde_json::to_vec(&unknown_relation).unwrap());
    assert!(unknown.status.success());
    let unknown: serde_json::Value = serde_json::from_slice(&unknown.stdout).unwrap();
    assert_eq!(unknown["families"]["status"], "indeterminate");

    fs::remove_dir_all(&temp).expect("remove test-owned directory");
}
