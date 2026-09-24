use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionRequest, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{
    DatasetBindingOptions, DatasetSpec, DerivedSemanticsSpec, ScalarExprSpec,
};
use serde_json::{Value, json};

const TYPED_RELATION_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: qualifying_person_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, TaxUnit]
  - name: qualifying_person
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: is_eligible
  - name: tax_unit_marker
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: case_value
  - name: qualifying_person_count
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(qualifying_person_of_tax_unit, qualifying_person)
  - name: credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: qualifying_person_count * 1500
"#;

fn artifact() -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(TYPED_RELATION_RULESPEC)
        .expect("typed relation RuleSpec compiles")
}

fn dataset(reversed: bool) -> Value {
    let interval = json!({"start": "2026-01-01", "end": "2026-12-31"});
    let tuple = if reversed {
        ["tax-unit-1", "person-1"]
    } else {
        ["person-1", "tax-unit-1"]
    };
    json!({
        "inputs": [
            {
                "name": "is_eligible", "entity": "Person", "entity_id": "person-1",
                "interval": interval, "value": {"kind": "bool", "value": true}
            },
            {
                "name": "case_value", "entity": "TaxUnit", "entity_id": "tax-unit-1",
                "interval": interval, "value": {"kind": "integer", "value": 1}
            }
        ],
        "relations": [{
            "name": "qualifying_person_of_tax_unit", "tuple": tuple, "interval": interval
        }]
    })
}

fn request(mode: &str, reversed: bool, compiled: bool) -> Value {
    let mut request = json!({
        "mode": mode,
        "dataset": dataset(reversed),
        "queries": [{
            "entity_id": "tax-unit-1",
            "period": {"period_kind": "tax_year", "start": "2026-01-01", "end": "2026-12-31"},
            "outputs": ["qualifying_person_count", "credit"]
        }]
    });
    if !compiled {
        request["program"] = serde_json::to_value(artifact().program).unwrap();
    }
    request
}

fn execute(request: Value, compiled: bool) -> Result<Value, String> {
    let response = if compiled {
        let request: CompiledExecutionRequest = serde_json::from_value(request).unwrap();
        execute_compiled_request(artifact(), request)
    } else {
        let request: ExecutionRequest = serde_json::from_value(request).unwrap();
        execute_request(request)
    };
    response
        .map(|response| serde_json::to_value(response).unwrap())
        .map_err(|error| error.to_string())
}

fn assert_mismatch(message: &str) {
    for detail in [
        "qualifying_person_of_tax_unit",
        "slot 0",
        "tax-unit-1",
        "expected `Person`",
        "found `TaxUnit`",
    ] {
        assert!(message.contains(detail), "missing {detail:?}: {message}");
    }
    let lower = message.to_lowercase();
    assert!(
        lower.contains("tuple")
            && (lower.contains("reorder") || lower.contains("fix") || lower.contains("supply")),
        "the error must tell the caller how to fix the tuple: {message}"
    );
}

fn assert_credit(response: &Value, count: i64, credit: i64) {
    for (name, expected) in [("qualifying_person_count", count), ("credit", credit)] {
        let value = &response["results"][0]["outputs"][name]["value"]["value"];
        let actual = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()));
        assert_eq!(actual, Some(expected), "unexpected {name}: {response}");
    }
}

fn run_cli(args: &[&str], request: &Value) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules-engine binary");
    child
        .stdin
        .take()
        .expect("stdin available")
        .write_all(&serde_json::to_vec(request).unwrap())
        .expect("request written");
    child.wait_with_output().expect("binary completes")
}

fn artifact_path(test: &str) -> PathBuf {
    let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/lane-scratch/relation-binding-tests");
    std::fs::create_dir_all(&scratch).expect("scratch directory created");
    let path = scratch.join(format!("{test}.compiled.json"));
    artifact().write_json_file(&path).expect("artifact writes");
    std::fs::write(
        scratch.join(format!("{test}.checkpoint.json")),
        serde_json::to_vec_pretty(&json!({"test": test, "artifact": path})).unwrap(),
    )
    .expect("artifact path checkpoint written at generation");
    path
}

fn assert_cli_rejected(output: &Output) {
    assert!(
        !output.status.success(),
        "reversed relation unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty(), "a rejected request has no result");
    assert_mismatch(&String::from_utf8_lossy(&output.stderr));
}

fn assert_cli_lenient(output: &Output, count: i64, credit: i64, mismatch: bool) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    assert_eq!(response["metadata"]["relation_binding"], "lenient");
    assert_credit(&response, count, credit);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lenient"),
        "opt-out must be visible: {stderr}"
    );
    if mismatch {
        assert!(
            stderr.contains("warning[relation_slot_entity_mismatch]"),
            "{stderr}"
        );
        assert_mismatch(&stderr);
    }
}

#[test]
fn dataset_binding_defaults_to_rejecting_known_relation_kind_mismatches() {
    let program = artifact().program.to_program().unwrap();
    let dataset: DatasetSpec = serde_json::from_value(dataset(true)).unwrap();
    let error = dataset
        .to_dataset_for_program(&program)
        .expect_err("default binding must reject a reversed typed tuple");
    assert_mismatch(&error.to_string());
    let error = dataset
        .to_dataset_for_program_with_options(&program, DatasetBindingOptions::default())
        .expect_err("default options must also reject a reversed typed tuple");
    assert_mismatch(&error.to_string());
}

#[test]
fn execute_request_explain_rejects_reversed_relation_by_default() {
    assert_mismatch(&execute(request("explain", true, false), false).unwrap_err());
}

#[test]
fn execute_request_fast_rejects_reversed_relation_by_default() {
    assert_mismatch(&execute(request("fast", true, false), false).unwrap_err());
}

#[test]
fn execute_compiled_request_explain_rejects_reversed_relation_by_default() {
    assert_mismatch(&execute(request("explain", true, true), true).unwrap_err());
}

#[test]
fn execute_compiled_request_fast_rejects_reversed_relation_by_default() {
    assert_mismatch(&execute(request("fast", true, true), true).unwrap_err());
}

#[test]
fn execute_request_lenient_opt_out_echoes_policy_and_preserves_empty_lookup() {
    for mode in ["explain", "fast"] {
        let mut request = request(mode, true, false);
        request["relation_binding"] = json!("lenient");
        let response = execute(request, false).expect("explicit lenient binding succeeds");
        assert_eq!(response["metadata"]["relation_binding"], "lenient");
        assert_credit(&response, 0, 0);
    }
}

#[test]
fn execute_compiled_request_lenient_opt_out_echoes_policy_and_preserves_empty_lookup() {
    for mode in ["explain", "fast"] {
        let mut request = request(mode, true, true);
        request["relation_binding"] = json!("lenient");
        let response = execute(request, true).expect("explicit lenient binding succeeds");
        assert_eq!(response["metadata"]["relation_binding"], "lenient");
        assert_credit(&response, 0, 0);
    }
}

#[test]
fn relation_binding_rejects_unknown_request_policy() {
    for compiled in [false, true] {
        let mut request = request("explain", false, compiled);
        request["relation_binding"] = json!("leninet");
        let error = if compiled {
            serde_json::from_value::<CompiledExecutionRequest>(request).unwrap_err()
        } else {
            serde_json::from_value::<ExecutionRequest>(request).unwrap_err()
        };
        assert!(error.to_string().contains("leninet"), "{error}");
    }
}

#[test]
fn bare_stdin_cli_rejects_reversed_relation_by_default() {
    assert_cli_rejected(&run_cli(&[], &request("explain", true, false)));
}

#[test]
fn run_cli_rejects_reversed_relation_by_default() {
    assert_cli_rejected(&run_cli(&["run"], &request("fast", true, false)));
}

#[test]
fn run_compiled_cli_rejects_reversed_relation_by_default() {
    let path = artifact_path("run_compiled_cli_rejects_reversed_relation_by_default");
    assert_cli_rejected(&run_cli(
        &["run-compiled", "--artifact", path.to_str().unwrap()],
        &request("fast", true, true),
    ));
}

#[test]
fn bare_stdin_cli_echoes_lenient_request_and_warns_about_mismatch() {
    let mut request = request("explain", true, false);
    request["relation_binding"] = json!("lenient");
    assert_cli_lenient(&run_cli(&[], &request), 0, 0, true);
}

#[test]
fn run_cli_lenient_flag_preserves_empty_lookup_and_warns() {
    assert_cli_lenient(
        &run_cli(
            &["run", "--relation-binding", "lenient"],
            &request("fast", true, false),
        ),
        0,
        0,
        true,
    );
}

#[test]
fn run_compiled_cli_lenient_flag_preserves_empty_lookup_and_warns() {
    let path = artifact_path("run_compiled_cli_lenient_flag_preserves_empty_lookup_and_warns");
    assert_cli_lenient(
        &run_cli(
            &[
                "run-compiled",
                "--artifact",
                path.to_str().unwrap(),
                "--relation-binding",
                "lenient",
            ],
            &request("fast", true, true),
        ),
        0,
        0,
        true,
    );
}

#[test]
fn run_compiled_cli_echoes_lenient_request_and_warns_about_mismatch() {
    let path = artifact_path("run_compiled_cli_echoes_lenient_request_and_warns_about_mismatch");
    let mut request = request("explain", true, true);
    request["relation_binding"] = json!("lenient");
    assert_cli_lenient(
        &run_cli(
            &["run-compiled", "--artifact", path.to_str().unwrap()],
            &request,
        ),
        0,
        0,
        true,
    );
}

#[test]
fn lenient_cli_opt_out_is_visible_even_without_mismatches() {
    assert_cli_lenient(
        &run_cli(
            &["run", "--relation-binding", "lenient"],
            &request("explain", false, false),
        ),
        1,
        1500,
        false,
    );
}

#[test]
fn declared_tuple_order_counts_one_person_and_returns_1500_in_both_modes() {
    for mode in ["explain", "fast"] {
        for compiled in [false, true] {
            let response = execute(request(mode, false, compiled), compiled)
                .expect("tuple in declared order succeeds");
            assert_credit(&response, 1, 1500);
        }
    }
}

#[test]
fn unknown_and_ambiguous_id_kinds_remain_unvalidated_in_any_input_order() {
    let program = artifact().program.to_program().unwrap();
    let mut value = dataset(true);
    value["inputs"][0]["entity_id"] = json!("ambiguous");
    value["inputs"][1]["entity_id"] = json!("ambiguous");
    value["relations"][0]["tuple"] = json!(["ambiguous", "unknown"]);
    let dataset: DatasetSpec = serde_json::from_value(value).unwrap();
    for inputs in [
        dataset.inputs.clone(),
        dataset.inputs.iter().cloned().rev().collect(),
    ] {
        let dataset = DatasetSpec {
            inputs,
            relations: dataset.relations.clone(),
        };
        for options in [
            DatasetBindingOptions::strict(),
            DatasetBindingOptions::default(),
        ] {
            let outcome = dataset
                .to_dataset_for_program_with_options(&program, options)
                .expect("neither unknown nor ambiguous kinds prove a mismatch");
            assert!(outcome.diagnostics.is_empty());
            assert_eq!(outcome.dataset.relations[0].tuple, ["ambiguous", "unknown"]);
        }
    }
}

#[test]
fn explicit_lenient_dataset_options_retain_both_mismatch_diagnostics() {
    let program = artifact().program.to_program().unwrap();
    let dataset: DatasetSpec = serde_json::from_value(dataset(true)).unwrap();
    let outcome = dataset
        .to_dataset_for_program_with_options(
            &program,
            DatasetBindingOptions {
                strict_relation_entities: false,
            },
        )
        .expect("explicit lenient binding retains the original tuple");
    assert_eq!(outcome.diagnostics.len(), 2);
    assert_eq!(
        outcome.dataset.relations[0].tuple,
        ["tax-unit-1", "person-1"]
    );
    assert_eq!(outcome.diagnostics[0].expected_entity, "Person");
    assert_eq!(outcome.diagnostics[0].actual_entity, "TaxUnit");
    assert_eq!(outcome.diagnostics[1].expected_entity, "TaxUnit");
    assert_eq!(outcome.diagnostics[1].actual_entity, "Person");
}

#[test]
fn legacy_artifact_binding_respects_its_executable_relation_direction() {
    let mut program = artifact().program;
    for rule in &mut program.derived {
        for semantics in std::iter::once(&mut rule.semantics).chain(
            rule.versions
                .iter_mut()
                .map(|version| &mut version.semantics),
        ) {
            if let DerivedSemanticsSpec::Scalar {
                expr:
                    ScalarExprSpec::CountRelated {
                        current_slot,
                        related_slot,
                        ..
                    },
            } = semantics
            {
                *current_slot = 0;
                *related_slot = 1;
            }
        }
    }
    let artifact = CompiledProgramArtifact::compile(program).expect("legacy artifact compiles");
    assert_eq!(
        artifact.program.relations[0].slot_entities,
        ["Person", "TaxUnit"]
    );
    for mode in ["explain", "fast"] {
        let request: CompiledExecutionRequest =
            serde_json::from_value(request(mode, true, true)).unwrap();
        let response = execute_compiled_request(artifact.clone(), request)
            .expect("strict binding follows the slots that the artifact executes");
        assert_credit(&serde_json::to_value(response).unwrap(), 1, 1500);
    }
}

#[test]
fn default_policy_is_echoed_after_binding() {
    for compiled in [false, true] {
        for mode in ["explain", "fast"] {
            let response = execute(request(mode, false, compiled), compiled).unwrap();
            assert_eq!(response["metadata"]["relation_binding"], "strict");
        }
    }
}

#[test]
fn historical_responses_do_not_claim_strict_binding() {
    let response: axiom_rules_engine::api::ExecutionResponse = serde_json::from_value(json!({
        "metadata": {"requested_mode": "explain", "actual_mode": "explain", "fallback_reason": null},
        "results": []
    })).unwrap();
    let response = serde_json::to_value(response).unwrap();
    assert!(response["metadata"]["relation_binding"].is_null());
}

#[test]
fn cli_strict_flag_overrides_lenient_request() {
    let mut uncompiled = request("explain", true, false);
    uncompiled["relation_binding"] = json!("lenient");
    assert_cli_rejected(&run_cli(
        &["run", "--relation-binding", "strict"],
        &uncompiled,
    ));
    let path = artifact_path("strict-flag-override");
    let mut compiled = request("explain", true, true);
    compiled["relation_binding"] = json!("lenient");
    assert_cli_rejected(&run_cli(
        &[
            "run-compiled",
            "--artifact",
            path.to_str().unwrap(),
            "--relation-binding",
            "strict",
        ],
        &compiled,
    ));
}

#[test]
fn cdcc_owner_first_declaration_rejects_legacy_member_first_tuple() {
    let source = TYPED_RELATION_RULESPEC.replace("[Person, TaxUnit]", "[TaxUnit, Person]");
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
    let count = artifact
        .program
        .derived
        .iter()
        .find(|rule| rule.name == "qualifying_person_count")
        .unwrap();
    assert!(matches!(
        count.semantics,
        DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::CountRelated {
                current_slot: 0,
                related_slot: 1,
                ..
            }
        }
    ));
    for mode in ["explain", "fast"] {
        let correct: CompiledExecutionRequest =
            serde_json::from_value(request(mode, true, true)).unwrap();
        let response = execute_compiled_request(artifact.clone(), correct).unwrap();
        assert_credit(&serde_json::to_value(response).unwrap(), 1, 1500);

        let old_order = request(mode, false, true);
        let reversed: CompiledExecutionRequest = serde_json::from_value(old_order.clone()).unwrap();
        let error = execute_compiled_request(artifact.clone(), reversed)
            .unwrap_err()
            .to_string();
        for detail in [
            "qualifying_person_of_tax_unit",
            "slot 0",
            "person-1",
            "expected `TaxUnit`",
            "found `Person`",
        ] {
            assert!(error.contains(detail), "{error}");
        }
        let mut lenient = old_order;
        lenient["relation_binding"] = json!("lenient");
        let response =
            execute_compiled_request(artifact.clone(), serde_json::from_value(lenient).unwrap())
                .unwrap();
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(response["metadata"]["relation_binding"], "lenient");
        assert_credit(&response, 0, 0);
    }
}
