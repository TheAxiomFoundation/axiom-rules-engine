//! Verification probe: dense f64 mode rounds before integer-key conversion.
//! Compares explain / fast / dense-Decimal / dense-f64 for parameter keys and
//! date_add_days offsets computed as `Integer * Decimal literal`.

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
  - name: table
    kind: parameter
    dtype: Money
    indexed_by: size
    versions:
      - effective_from: 2026-01-01
        values:
          7: 700
          8: 800
          114: 114000
          115: 115000
  - name: k_ceil
    kind: derived
    entity: Household
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: table[ceil(size * 0.28)]
  - name: k_floor
    kind: derived
    entity: Household
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: table[floor(size * 4.6)]
  - name: k_raw
    kind: derived
    entity: Household
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: table[size * 0.28]
  - name: shifted_f
    kind: derived
    entity: Household
    dtype: Date
    versions:
      - effective_from: 2026-01-01
        formula: date_add_days(period_start, size * 0.28)
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

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    output: &str,
    size: i64,
) -> String {
    let period = month_period();
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "size".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Integer { value: size },
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
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

fn run_dense(dense: &DenseCompiledProgram, output: &str, size: i64, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([("size".to_string(), DenseColumn::Integer(vec![size]))]),
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
fn verify_dense_f64_key_rounding() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles");

    let size = 25;
    for output in ["k_ceil", "k_floor", "k_raw", "shifted_f"] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output, size);
        let fast = run_api(ExecutionMode::Fast, &artifact, output, size);
        let dense_dec = run_dense(&dense, output, size, false);
        let dense_f64 = run_dense(&dense, output, size, true);
        println!("=== {output} (size={size}) ===");
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        println!("  dense decimal: {dense_dec}");
        println!("  dense f64    : {dense_f64}");
    }
    println!(
        "f64 check: 25.0*0.28 = {:?}, 25.0*4.6 = {:?}",
        25.0_f64 * 0.28,
        25.0_f64 * 4.6
    );
}
