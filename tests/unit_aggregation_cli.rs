#![cfg(feature = "unit-derivation")]

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Set to `1` to rewrite the exact result fixture from the CLI's current output
/// instead of failing on drift. Refused under CI, where the test only compares.
const UPDATE_FIXTURE_ENV: &str = "AXIOM_UPDATE_UNIT_AGGREGATION_FIXTURE";

/// Drifted JSON paths listed before the report truncates (it says how many more).
const MAX_REPORTED_PATHS: usize = 20;

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

/// Compares CLI output with the result fixture at `path` byte for byte.
///
/// The fixture pins `plan_digest` and `trace_root`. Both commit to the SHA-256
/// of the whole serialized source artifact, which records `engine_version` and
/// the unit table seeded into this program, so a version cut or a newly seeded
/// currency moves both digests while every family value stays the same. They
/// also hash inputs the result never shows, such as the canonical plan
/// (citations included), the request, and the constitution trace. On drift the
/// failure lists the JSON paths that moved, so a digest-only change is easy to
/// tell apart from a change to the result body.
fn check_result_fixture(actual: &[u8], path: &Path) {
    let expected = fs::read(path).expect("read result fixture");
    if actual == expected {
        return;
    }
    let moved = moved_json_paths(&expected, actual);
    if std::env::var_os(UPDATE_FIXTURE_ENV).is_some_and(|value| value == "1") {
        assert!(
            std::env::var_os("CI").is_none(),
            "{UPDATE_FIXTURE_ENV} is refused under CI"
        );
        fs::write(path, actual).expect("rewrite result fixture");
        // The harness captures `eprintln!` from passing tests; write to the real
        // stderr so the person regenerating sees what moved.
        writeln!(
            std::io::stderr(),
            "rewrote {} (moved: {moved})",
            path.display()
        )
        .expect("report the rewrite");
        return;
    }
    panic!(
        "run-unit-aggregation output no longer matches {} exactly.\n\
         Moved JSON paths: {moved}\n\
         If only /plan_digest and/or /trace_root moved, something they hash changed \
         without changing any family value: the compiled source artifact (for \
         example engine_version or the seeded unit table), the plan or request \
         fixture (citations and evidence ids included), the constitution trace, or \
         the digest code. Confirm which one and that the change is intended, then \
         regenerate the fixture with:\n  \
         {UPDATE_FIXTURE_ENV}=1 cargo test --features \"schema unit-derivation\" \
         --test unit_aggregation_cli\n\
         Any other moved path changes the result body: investigate it first.",
        path.display()
    );
}

fn moved_json_paths(expected: &[u8], actual: &[u8]) -> String {
    let (Ok(expected), Ok(actual)) = (
        serde_json::from_slice::<serde_json::Value>(expected),
        serde_json::from_slice::<serde_json::Value>(actual),
    ) else {
        return "(the output or the fixture is not JSON)".to_string();
    };
    let mut paths = Vec::new();
    collect_moved_paths(&expected, &actual, String::new(), &mut paths);
    if paths.is_empty() {
        return "(none: same JSON value, different bytes)".to_string();
    }
    let total = paths.len();
    paths.truncate(MAX_REPORTED_PATHS);
    let mut report = paths.join(", ");
    if total > MAX_REPORTED_PATHS {
        report.push_str(&format!(", and {} more", total - MAX_REPORTED_PATHS));
    }
    report
}

/// Appends the JSON Pointer of every leaf, key, or array whose value differs.
fn collect_moved_paths(
    expected: &serde_json::Value,
    actual: &serde_json::Value,
    path: String,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let keys: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
            for key in keys {
                let child = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (expected.get(key), actual.get(key)) {
                    (Some(expected), Some(actual)) => {
                        collect_moved_paths(expected, actual, child, out);
                    }
                    _ => out.push(child),
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) if expected.len() == actual.len() => {
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                collect_moved_paths(expected, actual, format!("{path}/{index}"), out);
            }
        }
        _ if expected != actual => out.push(if path.is_empty() {
            "(root)".to_string()
        } else {
            path
        }),
        _ => {}
    }
}

#[test]
fn fixture_drift_report_names_every_moved_path() {
    let expected = br#"{"plan_digest":"a","trace_root":"b","families":{"value":[{"id":"f","members":["p1"]}]}}"#;
    let digest_only = br#"{"plan_digest":"c","trace_root":"d","families":{"value":[{"id":"f","members":["p1"]}]}}"#;
    assert_eq!(
        moved_json_paths(expected, digest_only),
        "/plan_digest, /trace_root"
    );
    let value_moved = br#"{"plan_digest":"a","trace_root":"b","families":{"value":[{"id":"f","members":["p2"]}],"a/b~":1}}"#;
    assert_eq!(
        moved_json_paths(expected, value_moved),
        "/families/a~1b~0, /families/value/0/members/0"
    );
    let reformatted = b"{\"trace_root\": \"b\", \"plan_digest\": \"a\", \"families\": {\"value\": [{\"id\": \"f\", \"members\": [\"p1\"]}]}}";
    assert_eq!(
        moved_json_paths(expected, reformatted),
        "(none: same JSON value, different bytes)"
    );
    assert_eq!(
        moved_json_paths(expected, b"not json"),
        "(the output or the fixture is not JSON)"
    );
    assert_eq!(moved_json_paths(br#"{"a":1,"b":2}"#, br#"{"a":1}"#), "/b");
    assert_eq!(moved_json_paths(br#"{"m":[1]}"#, br#"{"m":[1,2]}"#), "/m");
    assert_eq!(moved_json_paths(br#"{"m":[1]}"#, br#"{"m":{"0":1}}"#), "/m");
    assert_eq!(moved_json_paths(b"1", b"2"), "(root)");
    let many_expected = serde_json::to_vec(&(0..25).collect::<Vec<_>>()).unwrap();
    let many_actual = serde_json::to_vec(&(100..125).collect::<Vec<_>>()).unwrap();
    let report = moved_json_paths(&many_expected, &many_actual);
    assert!(report.starts_with("/0, /1, "), "{report}");
    assert!(report.ends_with("/19, and 5 more"), "{report}");
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
    check_result_fixture(&first.stdout, &fixture("nz_income_explorer_result.json"));

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
