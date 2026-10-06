//! Verification probe (candidate lifetime-topn-n-f64-before-trunc):
//! does `sum_top_n_over_periods` resolve a different integer `n` in the dense
//! lifetime f64 mode than in the Decimal mode?

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn compile(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("rulespec module compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect("dense compilation succeeds")
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal literal")
}

fn batch_dec(columns: &[(&str, Vec<Decimal>)]) -> DenseBatchSpec {
    let row_count = columns[0].1.len();
    DenseBatchSpec {
        row_count,
        inputs: columns
            .iter()
            .map(|(name, values)| (name.to_string(), DenseColumn::Decimal(values.clone())))
            .collect::<HashMap<_, _>>(),
        relations: HashMap::new(),
    }
}

fn show(result: &Result<DenseExecutionResult, axiom_rules_engine::engine::EvalError>, out: &str) -> String {
    match result {
        Ok(r) => match r.outputs.get(out) {
            Some(DenseOutputValue::Scalar(col)) => format!("OK {col:?}"),
            other => format!("OK(non-scalar) {other:?}"),
        },
        Err(e) => format!("ERR {e}"),
    }
}

fn run_both(
    label: &str,
    program: &DenseCompiledProgram,
    periods: &[Period],
    batches: Vec<DenseBatchSpec>,
    out: &str,
) -> (String, String) {
    let dec_res = program.execute_lifetime(periods, batches.clone(), &[out.to_string()]);
    let f64_res = program.execute_lifetime_f64(periods, batches, &[out.to_string()]);
    let d = show(&dec_res, out);
    let f = show(&f64_res, out);
    println!("[{label}] dense execute_lifetime     (Decimal) => {d}");
    println!("[{label}] dense execute_lifetime_f64 (f64)     => {f}");
    (d, f)
}

// Case A: the exact repro sketch — input k = Decimal 2.9999999999999999999999999999.
#[test]
fn case_a_decimal_input_n_near_three() {
    let module = r#"
format: rulespec/v1
rules:
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, k)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let k = dec("2.9999999999999999999999999999");
    println!("[A] k = {k} ; k.to_f64 = {:?}", rust_decimal::prelude::ToPrimitive::to_f64(&k));
    let batches = vec![
        batch_dec(&[("earnings", vec![dec("10")]), ("k", vec![k])]),
        batch_dec(&[("earnings", vec![dec("9")]), ("k", vec![k])]),
        batch_dec(&[("earnings", vec![dec("8")]), ("k", vec![k])]),
    ];
    run_both("A input k", &program, &periods, batches, "top");
}

// Case B: parameter-sourced n (Rate parameter 2.9999999999999999999999999999).
#[test]
fn case_b_parameter_n_near_three() {
    let module = r#"
format: rulespec/v1
rules:
  - name: keep_n
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2000-01-01'
        formula: '2.9999999999999999999999999999'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, keep_n)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch_dec(&[("earnings", vec![dec("10")])]),
        batch_dec(&[("earnings", vec![dec("9")])]),
        batch_dec(&[("earnings", vec![dec("8")])]),
    ];
    run_both("B param keep_n", &program, &periods, batches, "top");
}

// Case C: f64 arithmetic inside n: 0.3 / 0.1 is exactly 3 in Decimal but
// 2.9999999999999996 in f64.
#[test]
fn case_c_arithmetic_n_division() {
    let module = r#"
format: rulespec/v1
rules:
  - name: a
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2000-01-01'
        formula: '0.3'
  - name: b
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2000-01-01'
        formula: '0.1'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, a / b)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch_dec(&[("earnings", vec![dec("10")])]),
        batch_dec(&[("earnings", vec![dec("9")])]),
        batch_dec(&[("earnings", vec![dec("8")])]),
    ];
    run_both("C n = 0.3/0.1", &program, &periods, batches, "top");
}

// Case D: ceil(k * 0.28) with k = 25 -> Decimal 7, f64 ceil(7.000000000000001) = 8.
#[test]
fn case_d_ceil_of_product() {
    let module = r#"
format: rulespec/v1
rules:
  - name: share
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2000-01-01'
        formula: '0.28'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, ceil(k * share))
"#;
    let program = compile(module, "Worker");
    let periods: Vec<Period> = (2001..=2010).map(year).collect();
    // earnings 10, 9, ..., 1 ; k = 25 in every period.
    let batches: Vec<DenseBatchSpec> = (0..10)
        .map(|i| {
            batch_dec(&[
                ("earnings", vec![Decimal::from(10 - i)]),
                ("k", vec![Decimal::from(25)]),
            ])
        })
        .collect();
    run_both("D n = ceil(25*0.28)", &program, &periods, batches, "top");
}

// Explain reference: over-periods reductions have no explain path at all.
#[test]
fn case_e_explain_and_fast_have_no_lifetime_path() {
    let module = r#"
format: rulespec/v1
rules:
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, k)
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(module).expect("program fixture parses");
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(2003, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2003, 12, 31).unwrap(),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "earnings".to_string(),
                entity: "Worker".to_string(),
                entity_id: "w1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: "10".to_string(),
                },
            },
            InputRecordSpec {
                name: "k".to_string(),
                entity: "Worker".to_string(),
                entity_id: "w1".to_string(),
                interval,
                value: ScalarValueSpec::Decimal {
                    value: "2.9999999999999999999999999999".to_string(),
                },
            },
        ],
        relations: Vec::new(),
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let request = ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "w1".to_string(),
                period: period.clone(),
                outputs: vec!["top".to_string()],
            }],
        };
        match execute_request(request) {
            Ok(resp) => println!(
                "[E] {mode:?}: OK actual_mode={:?} fallback={:?} outputs={:?}",
                resp.metadata.actual_mode, resp.metadata.fallback_reason, resp.results
            ),
            Err(e) => println!("[E] {mode:?}: ERR {e}"),
        }
    }
}
