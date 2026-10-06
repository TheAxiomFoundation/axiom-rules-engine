//! Verification probe (candidate bulk-bool-text-ordering-value): bulk
//! compare_columns answers <, <=, >, >= on Bool/Text operands with NotHolds
//! where explain errors, and fast mode does not fall back.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: flag_order
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_a < claim_b
  - name: flag_order_amount
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if flag_order: 100
          else: 0
  - name: flag_order_gte
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_b >= claim_a
  - name: flag_text
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: status > "m"
  - name: flag_text_amount
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if flag_text: 100
          else: 0
"#;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn input(name: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: "p1".to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("date"),
        },
        value,
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    let period = month_period();
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![
                input("claim_a", ScalarValueSpec::Bool { value: false }),
                input("claim_b", ScalarValueSpec::Bool { value: true }),
                input(
                    "status",
                    ScalarValueSpec::Text {
                        value: "z".to_string(),
                    },
                ),
            ],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "p1".to_string(),
            period: period.clone(),
            outputs: vec![output.to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get(output)
                .map(fmt_output)
                .unwrap_or_else(|| "<missing>".to_string());
            format!(
                "OK {value} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("claim_a".to_string(), DenseColumn::Bool(vec![false])),
            ("claim_b".to_string(), DenseColumn::Bool(vec![true])),
            ("status".to_string(), DenseColumn::Text(vec!["z".to_string()])),
        ]),
        relations: HashMap::new(),
    };
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, batch, &outputs)
    } else {
        dense.execute(&period, batch, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_bulk_bool_text_ordering() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"));
    for output in [
        "flag_order",
        "flag_order_amount",
        "flag_order_gte",
        "flag_text",
        "flag_text_amount",
    ] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        println!("=== {output} ===");
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        match &dense {
            Ok(dense) => {
                println!("  dense decimal: {}", run_dense(dense, output, false));
                println!("  dense f64    : {}", run_dense(dense, output, true));
            }
            Err(error) => println!("  dense        : compile ERR {error}"),
        }
    }
}
