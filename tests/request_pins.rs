use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use serde_json::{Value, json};

const SIMPLE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount + 10
"#;

fn run_request(mode: &str, compiled: bool, extra_fields: Value) -> Output {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec fixture compiles");
    let mut request = json!({
        "mode": mode,
        "dataset": {
            "inputs": [{
                "name": "amount",
                "entity": "Household",
                "entity_id": "household-1",
                "interval": {"start": "2026-01-01", "end": "2026-01-31"},
                "value": {"kind": "decimal", "value": "15"}
            }],
            "relations": []
        },
        "queries": [{
            "entity_id": "household-1",
            "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
            "outputs": ["adjusted_amount"]
        }]
    });
    request
        .as_object_mut()
        .expect("request is an object")
        .extend(
            extra_fields
                .as_object()
                .expect("extra fields are an object")
                .clone(),
        );

    let mut command = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"));
    let artifact_path = if compiled {
        static NEXT_ARTIFACT: AtomicUsize = AtomicUsize::new(0);
        let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/lane-scratch/request-pins");
        std::fs::create_dir_all(&scratch).expect("scratch directory creates");
        let path = scratch.join(format!(
            "{}-{}.compiled.json",
            std::process::id(),
            NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed)
        ));
        artifact.write_json_file(&path).expect("artifact writes");
        command.args(["run-compiled", "--artifact"]).arg(&path);
        Some(path)
    } else {
        request["program"] = serde_json::to_value(&artifact.program).expect("program serialises");
        None
    };

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules-engine binary");
    child
        .stdin
        .take()
        .expect("stdin available")
        .write_all(&serde_json::to_vec(&request).expect("request serialises"))
        .expect("request written");
    let output = child.wait_with_output().expect("binary completes");
    if let Some(path) = artifact_path {
        std::fs::remove_file(path).expect("temporary artifact removes");
    }
    output
}

fn assert_pins_rejected(compiled: bool, pins: Value) {
    for mode in ["explain", "fast"] {
        let output = run_request(mode, compiled, json!({"pins": pins}));
        assert!(
            !output.status.success(),
            "mode={mode}, compiled={compiled}: pins must fail, stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "rejected requests emit no response"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("does not support pins"), "{stderr}");
        assert!(stderr.contains("remove `pins`"), "{stderr}");
        assert!(stderr.contains("engine with pin support"), "{stderr}");
    }
}

fn populated_pins() -> Value {
    json!([{
        "rule": "adjusted_amount",
        "value": {"kind": "decimal", "value": "99"}
    }])
}

#[test]
fn inline_request_rejects_populated_pins_in_both_modes() {
    assert_pins_rejected(false, populated_pins());
}

#[test]
fn compiled_request_rejects_populated_pins_in_both_modes() {
    assert_pins_rejected(true, populated_pins());
}

#[test]
fn inline_request_rejects_empty_pins_in_both_modes() {
    assert_pins_rejected(false, json!([]));
}

#[test]
fn compiled_request_rejects_empty_pins_in_both_modes() {
    assert_pins_rejected(true, json!([]));
}

#[test]
fn inline_request_rejects_null_pins_in_both_modes() {
    assert_pins_rejected(false, Value::Null);
}

#[test]
fn compiled_request_rejects_null_pins_in_both_modes() {
    assert_pins_rejected(true, Value::Null);
}

#[test]
fn unpinned_requests_keep_accepting_other_unknown_fields() {
    for compiled in [false, true] {
        for mode in ["explain", "fast"] {
            let output = run_request(mode, compiled, json!({"request_id": "caller-metadata"}));
            assert!(
                output.status.success(),
                "mode={mode}, compiled={compiled}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let response: Value = serde_json::from_slice(&output.stdout).expect("response parses");
            assert_eq!(response["metadata"]["actual_mode"], mode);
            assert_eq!(
                response["results"][0]["outputs"]["adjusted_amount"]["value"],
                json!({"kind": "decimal", "value": "25"})
            );
        }
    }
}
