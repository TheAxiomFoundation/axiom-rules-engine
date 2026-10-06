//! Verification probe (candidate lifetime-top-n-fractional-n-truncated):
//! dense `sum_top_n_over_periods` reads `n` through `try_to_i64_trunc`, so a
//! fractional n (2.5, 3.9, 0.5) is truncated rather than rejected the way
//! explain's `ScalarValue::as_index` rejects a fractional integer operand.
//! Explain and fast (bulk) have no lifetime surface: both reject OverPeriods.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::{Period, PeriodKind, ScalarValue};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;
use std::str::FromStr;

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: top
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: 2025-01-01
        formula: sum_top_n_over_periods(earn, n)
"#;

fn month(m: u32) -> Period {
    let start = chrono::NaiveDate::from_ymd_opt(2026, m, 1).expect("date");
    let end = chrono::NaiveDate::from_ymd_opt(2026, m + 1, 1)
        .expect("date")
        .pred_opt()
        .expect("date");
    Period {
        kind: PeriodKind::Month,
        start,
        end,
    }
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal")
}

fn batch_dec(earn: &str, n: &str) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("earn".to_string(), DenseColumn::Decimal(vec![dec(earn)])),
            ("n".to_string(), DenseColumn::Decimal(vec![dec(n)])),
        ]),
        relations: HashMap::new(),
    }
}

fn batch_f64(earn: f64, n: f64) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("earn".to_string(), DenseColumn::Float(vec![earn])),
            ("n".to_string(), DenseColumn::Float(vec![n])),
        ]),
        relations: HashMap::new(),
    }
}

fn describe(result: Result<axiom_rules_engine::dense::DenseExecutionResult, axiom_rules_engine::engine::EvalError>) -> String {
    match result {
        Ok(result) => match result.outputs.get("top") {
            Some(DenseOutputValue::Scalar(column)) => format!("VALUE {column:?}"),
            other => format!("VALUE(other) {other:?}"),
        },
        Err(error) => format!("ERROR {error}"),
    }
}

fn dense_program() -> DenseCompiledProgram {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles")
}

#[test]
fn explain_and_fast_have_no_lifetime_surface() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(RULESPEC).expect("lowers");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "earn".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: "10".to_string(),
                },
            },
            InputRecordSpec {
                name: "n".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval,
                value: ScalarValueSpec::Decimal {
                    value: "2.5".to_string(),
                },
            },
        ],
        relations: vec![],
    };
    let query = ExecutionQuery {
        assessment_date: None,
        entity_id: "p1".to_string(),
        period,
        outputs: vec!["top".to_string()],
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: vec![query.clone()],
        });
        match response {
            Ok(response) => println!(
                "PROBE {mode:?}: OK actual_mode={:?} fallback_reason={:?} outputs={:?}",
                response.metadata.actual_mode,
                response.metadata.fallback_reason,
                response.results[0].outputs
            ),
            Err(error) => println!("PROBE {mode:?}: ERROR {error}"),
        }
    }
}

#[test]
fn explain_as_index_reference_contract() {
    for s in ["2.5", "3.9", "0.5", "2"] {
        println!(
            "PROBE as_index(Decimal {s}) = {:?}",
            ScalarValue::Decimal(dec(s)).as_index()
        );
    }
}

#[test]
fn dense_lifetime_fractional_n() {
    let program = dense_program();
    let periods = vec![month(1), month(2), month(3)];
    let outputs = vec!["top".to_string()];
    for n in ["2", "2.5", "3.9", "0.5"] {
        let batches = vec![batch_dec("10", n), batch_dec("20", n), batch_dec("30", n)];
        let result = program.execute_lifetime(&periods, batches, &outputs);
        println!("PROBE dense-lifetime Decimal n={n}: {}", describe(result));
    }
    for n in [2.0_f64, 2.5, 3.9, 0.5] {
        let batches = vec![batch_f64(10.0, n), batch_f64(20.0, n), batch_f64(30.0, n)];
        let result = program.execute_lifetime_f64(&periods, batches, &outputs);
        println!("PROBE dense-lifetime f64 n={n}: {}", describe(result));
    }
}
