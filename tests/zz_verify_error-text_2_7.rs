//! Verification probe (candidate dense-date-or-mixed-parameter-values-error):
//! dense lookup_parameter_dense rejects Date-valued or mixed-type table
//! parameters that explain's lookup_parameter returns unchanged.
//! Compares explain / fast / dense (Decimal) / dense (f64).

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
  - name: deadline
    kind: parameter
    dtype: Date
    indexed_by: k
    versions:
      - effective_from: 2025-01-01
        values:
          0: '2025-04-15'
          1: '2025-10-15'
  - name: status_table
    kind: parameter
    dtype: Text
    indexed_by: k
    versions:
      - effective_from: 2025-01-01
        values:
          0: 5
          1: exempt
  - name: out_date
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: deadline[k]
  - name: out_mixed
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2025-01-01
        formula: status_table[k]
"#;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2025, 1, 31).expect("date"),
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str, k: i64) -> String {
    let period = month_period();
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "k".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Integer { value: k },
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "person-1".to_string(),
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

fn run_dense(dense: &DenseCompiledProgram, output: &str, keys: &[i64], f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: keys.len(),
        inputs: HashMap::from([("k".to_string(), DenseColumn::Integer(keys.to_vec()))]),
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
fn verify_dense_date_or_mixed_parameter_values() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");

    for (output, keys) in [
        ("out_date", vec![0_i64]),
        ("out_date", vec![0, 1]),
        ("out_mixed", vec![0]),
        ("out_mixed", vec![1]),
        ("out_mixed", vec![0, 1]),
    ] {
        println!("=== {output} keys={keys:?} ===");
        for k in &keys {
            println!(
                "  explain k={k}      : {}",
                run_api(ExecutionMode::Explain, &artifact, output, *k)
            );
            println!(
                "  fast    k={k}      : {}",
                run_api(ExecutionMode::Fast, &artifact, output, *k)
            );
        }
        println!("  dense decimal batch: {}", run_dense(&dense, output, &keys, false));
        println!("  dense f64 batch    : {}", run_dense(&dense, output, &keys, true));
    }
}
