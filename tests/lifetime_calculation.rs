//! Invented fixed-law histories through the actual Decimal plan and CLI.
//! No statutory model or external population is read by these tests.
use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseOutputValue, DenseRelationBatchSpec, DenseRelationKey, SelectedVersion,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::lifetime_api::{
    execute_lifetime_wire_request, parse_lifetime_request, parse_lifetime_wire_request,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{DerivedSemanticsSpec, ScalarExprSpec, ScalarValueSpec};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const OUTPUT: &str = "us:statutes/99/1#history_total";
const FACTOR: &str = "us:statutes/99/1#factor";
const AMOUNT: &str = "us:statutes/99/1#input.amount";
const KEY: &str = "us:statutes/99/1#input.history_key";

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: format!("{y}-01-01").parse().unwrap(),
        end: format!("{y}-12-31").parse().unwrap(),
    }
}
fn wire_year(y: i32) -> Value {
    json!({"period_kind":"tax_year","start":format!("{y}-01-01"),"end":format!("{y}-12-31")})
}
fn fixture() -> CompiledProgramArtifact {
    let source = r#"
format: rulespec/v1
rules:
  - name: factor
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2020-01-01'
        effective_to: '2021-12-31'
        formula: '2'
      - effective_from: '2026-01-01'
        formula: '3'
  - name: history_total
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2020-01-01'
        effective_to: '2021-12-31'
        formula: 'sum_over_periods(amount * factor[history_key])'
      - effective_from: '2026-01-01'
        formula: '2 * sum_over_periods(amount * factor[history_key])'
"#;
    let mut program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap()
        .program;
    program.derived[0].id = Some(OUTPUT.into());
    let factor = &mut program.parameters[0];
    factor.id = Some(FACTOR.into());
    factor.indexed_by = Some("historical_year".into());
    for (version, values) in factor.versions.iter_mut().zip([[2, 4], [3, 5]]) {
        version.values = [1990, 1991]
            .into_iter()
            .zip(values)
            .map(|(key, value)| (key, ScalarValueSpec::Integer { value }))
            .collect();
    }
    CompiledProgramArtifact::compile(program).unwrap()
}
fn batches() -> Vec<DenseBatchSpec> {
    [(1990, ["1.25", "10"]), (1991, ["2.50", "20"])]
        .into_iter()
        .map(|(key, values)| DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::from([
                (
                    "amount".into(),
                    DenseColumn::Decimal(values.map(|s| s.parse().unwrap()).to_vec()),
                ),
                ("history_key".into(), DenseColumn::Integer(vec![key, key])),
            ]),
            relations: HashMap::new(),
        })
        .collect()
}
fn request(y: i32) -> Value {
    json!({
        "schema":"axiom-rules-engine/lifetime-request/v2", "entity":"Worker",
        "periods":[wire_year(1990),wire_year(1991)],
        "calculation_period":wire_year(y), "output_period":wire_year(y), "outputs":[OUTPUT],
        "batches":[
            {"row_count":2,"entity_ids":["0001","person-2"],"inputs":{
                AMOUNT:{"kind":"decimal","values":["1.25","10"]},
                KEY:{"kind":"integer","values":[1990,1990]}}},
            {"row_count":2,"entity_ids":["0001","person-2"],"inputs":{
                AMOUNT:{"kind":"decimal","values":["2.50","20"]},
                KEY:{"kind":"integer","values":[1991,1991]}}}
        ]
    })
}
fn run(artifact: CompiledProgramArtifact, request: Value) -> Result<Value, String> {
    let parsed = parse_lifetime_wire_request(&request.to_string()).map_err(|e| e.to_string())?;
    let response = execute_lifetime_wire_request(artifact, parsed).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(response).unwrap())
}
fn numeric(plan: &CalculationLifetimePlan) -> Vec<Decimal> {
    let result = plan
        .execute(
            &[year(1990), year(1991)],
            batches(),
            &["history_total".into()],
        )
        .unwrap();
    match &result.outputs["history_total"] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values.clone(),
        other => panic!("expected Decimal: {other:?}"),
    }
}
fn set_formula(artifact: CompiledProgramArtifact, formula: &str) -> CompiledProgramArtifact {
    // Parse the expression using the real compiler, then retain the toy source's identities/ranges.
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: factor\n    kind: parameter\n    dtype: Integer\n    versions:\n      - effective_from: '2020-01-01'\n        formula: '2'\n  - name: scratch\n    kind: derived\n    entity: Worker\n    dtype: Money\n    versions:\n      - effective_from: '2020-01-01'\n        formula: '{formula}'\n"
    );
    let replacement = CompiledProgramArtifact::from_rulespec_str(&source)
        .unwrap()
        .program
        .derived
        .remove(0)
        .semantics;
    let mut program = artifact.program;
    program.derived[0].semantics = replacement.clone();
    for version in &mut program.derived[0].versions {
        version.semantics = replacement.clone();
    }
    CompiledProgramArtifact::compile(program).unwrap()
}

#[test]
fn actual_plan_selects_law_and_tables_independently_of_observation_dates() {
    let artifact = fixture();
    let before = serde_json::to_string(&artifact).unwrap();
    let old = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2021)).unwrap();
    let new = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)).unwrap();
    for _ in 0..2 {
        assert_eq!(
            numeric(&old),
            vec![Decimal::new(125, 1), Decimal::from(100)]
        );
        assert_eq!(
            numeric(&new),
            vec![Decimal::new(325, 1), Decimal::from(260)]
        );
    }
    assert_eq!(serde_json::to_string(&artifact).unwrap(), before);
    assert_eq!(old.calculation_period(), &year(2021));
    assert!(
        matches!(&old.selected_versions()[0], SelectedVersion::Derived { id:Some(id), version_index:0,effective_to:Some(end),.. } if id==OUTPUT && end.to_string()=="2021-12-31")
    );
    assert!(
        matches!(&new.selected_versions()[1], SelectedVersion::Parameter { id:Some(id),version_index:1,effective_to:None,.. } if id==FACTOR)
    );
}

#[test]
fn original_artifact_and_caller_mutations_cannot_change_a_prepared_plan() {
    let mut artifact = fixture();
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2021)).unwrap();
    artifact.program.parameters[0].versions[0].values.clear();
    artifact.program.derived[0].versions.clear();
    assert_eq!(numeric(&plan)[0], Decimal::new(125, 1));
    let mut serialized = serde_json::to_value(plan.selected_versions()).unwrap();
    serialized[0]["version_index"] = json!(999);
    assert!(matches!(
        plan.selected_versions()[0],
        SelectedVersion::Derived {
            version_index: 0,
            ..
        }
    ));
}

#[test]
fn missing_selected_table_key_never_borrows_from_an_older_version() {
    let mut program = fixture().program;
    program.parameters[0].versions[1].values.remove(&1991);
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)).unwrap();
    let error = plan
        .execute(
            &[year(1990), year(1991)],
            batches(),
            &["history_total".into()],
        )
        .unwrap_err();
    assert!(
        matches!(error,DenseCompileError::Eval(EvalError::MissingParameterValue { key:1991,at,.. }) if at==year(2026).start)
    );
}

#[test]
fn formula_gaps_and_expiry_fail_at_calculation_without_base_fallback() {
    let mut program = fixture().program;
    program.derived[0].semantics = DerivedSemanticsSpec::Scalar {
        expr: ScalarExprSpec::Literal {
            value: ScalarValueSpec::Integer { value: 999 },
        },
    };
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    for y in [2019, 2022, 2024] {
        assert!(
            matches!(CalculationLifetimePlan::from_artifact(&artifact,"Worker",year(y)),Err(DenseCompileError::Eval(EvalError::MissingDerivedFormulaVersion { at,.. })) if at==year(y).start)
        );
    }
    let mut last = year(2021);
    last.start = last.end;
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", last).unwrap();
    assert_eq!(numeric(&plan)[0], Decimal::new(125, 1));
}

#[test]
fn missing_parameter_version_is_distinct_from_a_missing_key() {
    let mut program = fixture().program;
    program.parameters[0].versions[1].effective_from = year(2027).start;
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    assert!(
        matches!(CalculationLifetimePlan::from_artifact(&artifact,"Worker",year(2026)),Err(DenseCompileError::MissingParameterVersion {at,..}) if at==year(2026).start)
    );
}

#[test]
fn overlapping_equal_start_versions_preserve_existing_last_document_selection() {
    let mut program = fixture().program;
    let mut extra = program.derived[0].versions[1].clone();
    extra.semantics = program.derived[0].versions[0].semantics.clone();
    program.derived[0].versions.push(extra);
    let mut extra = program.parameters[0].versions[1].clone();
    extra.values = program.parameters[0].versions[0].values.clone();
    program.parameters[0].versions.push(extra);
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)).unwrap();
    assert_eq!(numeric(&plan)[0], Decimal::new(125, 1));
    assert!(plan.selected_versions().iter().all(|s| matches!(
        s,
        SelectedVersion::Derived {
            version_index: 2,
            ..
        } | SelectedVersion::Parameter {
            version_index: 2,
            ..
        }
    )));
}

#[test]
fn outer_and_nested_parameter_lookup_both_use_the_calculation_version() {
    let artifact = set_formula(
        fixture(),
        "sum_over_periods(amount * factor[history_key]) + factor[1990]",
    );
    assert_eq!(
        run(artifact.clone(), request(2021)).unwrap()["outputs"][OUTPUT]["column"]["values"],
        json!(["14.5", "102"])
    );
    assert_eq!(
        run(artifact, request(2026)).unwrap()["outputs"][OUTPUT]["column"]["values"],
        json!(["19.25", "133"])
    );
}

#[test]
fn historical_date_expressions_keep_the_actual_observation_dates() {
    let artifact = set_formula(
        fixture(),
        "sum_over_periods(days_between(period_start, period_end))",
    );
    let mut input = request(2026);
    input["periods"] = json!([wire_year(1991), wire_year(1992)]);
    for batch in input["batches"].as_array_mut().unwrap() {
        batch["inputs"] = json!({});
    }
    let output = run(artifact.clone(), input.clone()).unwrap();
    assert_eq!(
        output["outputs"][OUTPUT]["column"]["values"],
        json!(["729", "729"])
    );
    input["calculation_period"] = wire_year(2021);
    input["output_period"] = wire_year(2021);
    assert_eq!(run(artifact, input).unwrap()["outputs"], output["outputs"]);
}

#[test]
fn outer_date_operations_still_refuse_ambiguous_lifetime_meaning() {
    let artifact = set_formula(
        fixture(),
        "sum_over_periods(amount) + days_between(period_start, period_end)",
    );
    let mut input = request(2026);
    for batch in input["batches"].as_array_mut().unwrap() {
        batch["inputs"].as_object_mut().unwrap().remove(KEY);
    }
    assert!(run(artifact, input).unwrap_err().contains("days_between"));
}

#[test]
fn missing_inputs_are_reported_at_the_observation_date() {
    let plan = CalculationLifetimePlan::from_artifact(&fixture(), "Worker", year(2026)).unwrap();
    let mut inputs = batches();
    inputs[0].inputs.remove("amount");
    let error = plan
        .execute(&[year(1990), year(1991)], inputs, &["history_total".into()])
        .unwrap_err();
    assert!(error.to_string().contains("1990"), "{error}");
}

#[test]
fn completed_history_boundary_and_kinds_are_enforced_by_core_and_wire() {
    let artifact = fixture();
    let mut good = request(2026);
    good["calculation_period"] =
        json!({"period_kind":"month","start":"2026-01-01","end":"2026-01-31"});
    good["output_period"] = good["calculation_period"].clone();
    good["periods"][1]["end"] = json!("2025-12-31");
    assert!(run(artifact.clone(), good.clone()).is_ok());
    good["periods"][1]["end"] = json!("2026-01-01");
    assert!(
        run(artifact.clone(), good)
            .unwrap_err()
            .contains("end before calculation")
    );
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)).unwrap();
    let mut periods = [year(1990), year(1991)];
    periods[1].kind = PeriodKind::Month;
    assert!(
        plan.execute(&periods, batches(), &["history_total".into()])
            .unwrap_err()
            .to_string()
            .contains("same kind")
    );
}

#[test]
fn core_refuses_float_unknown_inputs_and_relation_batches() {
    let plan = CalculationLifetimePlan::from_artifact(&fixture(), "Worker", year(2026)).unwrap();
    let mut related = batches();
    related[0].relations.insert(
        DenseRelationKey {
            name: "undeclared".into(),
            current_slot: 0,
            related_slot: 1,
        },
        DenseRelationBatchSpec {
            offsets: vec![0, 0, 0],
            inputs: HashMap::new(),
        },
    );
    assert!(
        plan.execute(
            &[year(1990), year(1991)],
            related,
            &["history_total".into()]
        )
        .unwrap_err()
        .to_string()
        .contains("relation batches")
    );
    let mut inputs = batches();
    inputs[0]
        .inputs
        .insert("amount".into(), DenseColumn::Float(vec![1.25, 10.0]));
    assert!(
        plan.execute(&[year(1990), year(1991)], inputs, &["history_total".into()])
            .unwrap_err()
            .to_string()
            .contains("Float")
    );
    let mut inputs = batches();
    inputs[0]
        .inputs
        .insert("ignored".into(), DenseColumn::Integer(vec![1, 1]));
    assert!(
        plan.execute(&[year(1990), year(1991)], inputs, &["history_total".into()])
            .unwrap_err()
            .to_string()
            .contains("unknown input")
    );
}

#[test]
fn v1_retains_schema_selection_and_commencement_refusals() {
    assert!(parse_lifetime_request(&request(2026).to_string()).is_err());
    let mut input = request(2026);
    input["schema"] = json!("axiom-rules-engine/lifetime-request/v1");
    assert!(parse_lifetime_wire_request(&input.to_string()).is_err());
    input.as_object_mut().unwrap().remove("calculation_period");
    input["output_period"] = wire_year(1991);
    assert!(
        run(fixture(), input.clone())
            .unwrap_err()
            .contains("versioned derived")
    );
    let mut program = fixture().program;
    program.derived[0].versions.remove(0);
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    assert!(run(artifact.clone(), input).unwrap_err().contains("1990"));
    assert!(
        DenseCompiledProgram::from_artifact(&artifact, Some("Worker"))
            .unwrap()
            .execute_lifetime(
                &[year(1990), year(1991)],
                batches(),
                &["history_total".into()]
            )
            .is_err()
    );
}

#[test]
fn wire_refuses_incomplete_alias_misaligned_and_unbounded_requests() {
    for mutate in [0, 1, 2, 3, 4, 5, 6, 7] {
        let mut input = request(2026);
        match mutate {
            0 => {
                input.as_object_mut().unwrap().remove("calculation_period");
            }
            1 => {
                input["output_period"] = wire_year(2027);
            }
            2 => {
                input["batches"][1]["entity_ids"] = json!(["person-2", "0001"]);
            }
            3 => {
                input["batches"][0]["inputs"]["amount"] = json!({"kind":"integer","values":[1,2]});
            }
            4 => {
                input["calculation_period"]["assessment_date"] = json!("2026-01-01");
            }
            5 => {
                input["batches"][0]["inputs"][AMOUNT]["values"] = json!([1.25, 10.0]);
            }
            6 => {
                input["calculation_period"]["start"] = json!("2027-01-01");
            }
            _ => {
                input["periods"] = json!([]);
                input["batches"] = json!([]);
            }
        }
        assert!(run(fixture(), input).is_err(), "mutation {mutate}");
    }
    assert!(parse_lifetime_wire_request("{\"schema\":\"axiom-rules-engine/lifetime-request/v2\",\"schema\":\"axiom-rules-engine/lifetime-request/v1\"}").is_err());
}

#[test]
fn zero_rows_and_unversioned_nodes_keep_explicit_identity_and_no_invented_data() {
    let mut program = fixture().program;
    program.derived[0].semantics = program.derived[0].versions[0].semantics.clone();
    program.derived[0].versions.clear();
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    let mut input = request(2026);
    for batch in input["batches"].as_array_mut().unwrap() {
        batch["row_count"] = json!(0);
        batch["entity_ids"] = json!([]);
        for column in batch["inputs"].as_object_mut().unwrap().values_mut() {
            column["values"] = json!([]);
        }
    }
    let result = run(artifact, input).unwrap();
    assert_eq!(result["outputs"][OUTPUT]["column"]["values"], json!([]));
    assert_eq!(
        result["selected_versions"][0],
        json!({"kind":"unversioned_derived","name":"history_total","id":OUTPUT})
    );
}

#[test]
fn stale_or_malformed_artifact_contracts_are_readmitted_before_preparation() {
    let mut artifact = fixture();
    artifact.artifact_format_version = 999;
    assert!(matches!(
        CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)),
        Err(DenseCompileError::Artifact(_))
    ));
    let mut artifact = fixture();
    artifact.program.derived[0].versions[0].effective_to = Some(year(2010).start);
    assert!(CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2026)).is_err());
}

#[cfg(feature = "fs")]
#[test]
fn actual_cli_v2_preserves_exact_results_and_original_version_provenance() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let artifact = fixture();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("calculation-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("artifact.json");
    std::fs::write(&path, serde_json::to_vec(&artifact).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    for y in [2021, 2026, 2021, 2026] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
            .args(["run-lifetime", "--artifact"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request(y).to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(actual, run(artifact.clone(), request(y)).unwrap());
        assert_eq!(
            actual["outputs"][OUTPUT]["column"]["values"][0],
            if y == 2021 { "12.5" } else { "32.5" }
        );
        assert_eq!(actual["schema"], "axiom-rules-engine/lifetime-response/v2");
        assert_eq!(actual["reference_period"], wire_year(y));
        assert_eq!(actual["periods"], json!([wire_year(1990), wire_year(1991)]));
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "schema")]
#[test]
fn v2_schemas_accept_actual_wire_and_remain_disjoint_from_v1() {
    use axiom_rules_engine::schema::{
        calculation_lifetime_request_schema, calculation_lifetime_response_schema,
        lifetime_request_schema, lifetime_response_schema,
    };
    let input = request(2026);
    let output = run(fixture(), input.clone()).unwrap();
    let request_schema = jsonschema::draft7::new(&calculation_lifetime_request_schema()).unwrap();
    let response_schema = jsonschema::draft7::new(&calculation_lifetime_response_schema()).unwrap();
    assert!(request_schema.is_valid(&input));
    assert!(response_schema.is_valid(&output));
    assert!(
        !jsonschema::draft7::new(&lifetime_request_schema())
            .unwrap()
            .is_valid(&input)
    );
    assert!(
        !jsonschema::draft7::new(&lifetime_response_schema())
            .unwrap()
            .is_valid(&output)
    );
    let mut bad = input.clone();
    bad.as_object_mut().unwrap().remove("calculation_period");
    assert!(!request_schema.is_valid(&bad));
    let mut bad = output;
    bad["selected_versions"][0]["ignored"] = json!(true);
    assert!(!response_schema.is_valid(&bad));
}

#[test]
fn fixed_law_top_n_and_period_varying_counts_keep_distinct_semantics() {
    let mut program =
        set_formula(fixture(), "sum_top_n_over_periods(amount, factor[1990])").program;
    for (i, version) in program.parameters[0].versions.iter_mut().enumerate() {
        version.values.insert(
            1990,
            ScalarValueSpec::Integer {
                value: i as i64 + 1,
            },
        );
    }
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    for (year, expected) in [(2021, "2.5"), (2026, "3.75")] {
        let mut input = request(year);
        for batch in input["batches"].as_array_mut().unwrap() {
            batch["inputs"].as_object_mut().unwrap().remove(KEY);
        }
        assert_eq!(
            run(artifact.clone(), input).unwrap()["outputs"][OUTPUT]["column"]["values"][0],
            expected
        );
    }
    let artifact = set_formula(fixture(), "sum_top_n_over_periods(amount, count)");
    let mut input = request(2026);
    for (index, batch) in input["batches"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        batch["inputs"].as_object_mut().unwrap().remove(KEY);
        batch["inputs"]["us:statutes/99/1#input.count"] =
            json!({"kind":"integer","values":[index+1,1]});
    }
    let error = execute_lifetime_wire_request(
        artifact,
        parse_lifetime_wire_request(&input.to_string()).unwrap(),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            axiom_rules_engine::lifetime_api::LifetimeApiError::Calculation(
                DenseCompileError::Eval(EvalError::OverPeriodsTopNPeriodVarying { .. })
            )
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn all_entity_roots_remain_conservatively_selected_even_if_not_requested() {
    let mut program = fixture().program;
    let mut unused = program.derived[0].clone();
    unused.name = "future_total".into();
    unused.id = Some("us:statutes/99/1#future_total".into());
    unused.versions = vec![unused.versions[1].clone()];
    unused.versions[0].effective_from = year(2030).start;
    program.derived.push(unused);
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    assert!(
        run(artifact, request(2026))
            .unwrap_err()
            .contains("future_total")
    );
}

#[test]
fn v2_failure_categories_preserve_typed_calculation_causes() {
    use axiom_rules_engine::lifetime_api::LifetimeApiError;
    let missing_law = execute_lifetime_wire_request(
        fixture(),
        parse_lifetime_wire_request(&request(2024).to_string()).unwrap(),
    )
    .unwrap_err();
    assert_eq!(missing_law.diagnostic()["category"], "evaluation");
    assert!(matches!(
        missing_law,
        LifetimeApiError::Calculation(DenseCompileError::Eval(
            EvalError::MissingDerivedFormulaVersion { .. }
        ))
    ));
    let mut input = request(2026);
    input["periods"][1]["end"] = json!("2026-01-01");
    let incomplete = execute_lifetime_wire_request(
        fixture(),
        parse_lifetime_wire_request(&input.to_string()).unwrap(),
    )
    .unwrap_err();
    assert_eq!(incomplete.diagnostic()["category"], "invalid_request");
    assert!(matches!(
        incomplete,
        LifetimeApiError::Calculation(DenseCompileError::CalculationHistory(_))
    ));
}
