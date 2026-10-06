//! Verification probe (lifetime-topn-n-label): does dense lifetime
//! `sum_top_n_over_periods` report the truncated / saturated n instead of the
//! value the formula supplied, in its OverPeriodsTopNOutOfRange error text?
//! Also records what explain and fast do for the same formula.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn batch(input: &str, values: Vec<f64>) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: values.len(),
        inputs: HashMap::from([(input.to_string(), DenseColumn::Float(values))]),
        relations: HashMap::new(),
    }
}

fn single_rule_module(formula: &str) -> String {
    format!(
        r#"
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
          {formula}
"#
    )
}

fn param_module(param_dtype: &str, param_value: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: keep_n
    kind: parameter
    dtype: {param_dtype}
    versions:
      - effective_from: '1960-01-01'
        formula: '{param_value}'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, keep_n)
"#
    )
}

fn describe(result: Result<axiom_rules_engine::dense::DenseExecutionResult, impl std::fmt::Display>) -> String {
    match result {
        Ok(result) => match result.outputs.get("top") {
            Some(DenseOutputValue::Scalar(column)) => format!("VALUE {column:?}"),
            other => format!("VALUE (non-scalar) {other:?}"),
        },
        Err(error) => format!("ERROR {error}"),
    }
}

/// Extract just the "n resolved to X" fragment for compact comparison.
fn n_label(text: &str) -> String {
    match text.find("n resolved to ") {
        Some(start) => {
            let rest = &text[start + "n resolved to ".len()..];
            let end = rest.find(" for at least").unwrap_or(rest.len());
            rest[..end].to_string()
        }
        None => "<no 'n resolved to' fragment>".to_string(),
    }
}

fn run_dense(label: &str, module: &str) -> (String, String) {
    let artifact = match CompiledProgramArtifact::from_rulespec_str(module) {
        Ok(artifact) => artifact,
        Err(error) => {
            println!("[{label}] rulespec compile error: {error}");
            return (String::new(), String::new());
        }
    };
    let program = match DenseCompiledProgram::from_artifact(&artifact, Some("Worker")) {
        Ok(program) => program,
        Err(error) => {
            println!("[{label}] dense compile error: {error}");
            return (String::new(), String::new());
        }
    };
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = || {
        vec![
            batch("earnings", vec![100.0]),
            batch("earnings", vec![200.0]),
            batch("earnings", vec![300.0]),
        ]
    };
    let decimal = describe(program.execute_lifetime(&periods, batches(), &["top".to_string()]));
    let f64_mode = describe(program.execute_lifetime_f64(&periods, batches(), &["top".to_string()]));
    println!("[{label}] execute_lifetime     (Decimal): {decimal}");
    println!("[{label}] execute_lifetime_f64 (f64)    : {f64_mode}");
    println!(
        "[{label}] n label: Decimal='{}'  f64='{}'",
        n_label(&decimal),
        n_label(&f64_mode)
    );
    (decimal, f64_mode)
}

fn run_explain_fast(label: &str, module: &str) {
    let program = match axiom_rules_engine::rulespec::lower_rulespec_str(module) {
        Ok(program) => program,
        Err(error) => {
            println!("[{label}] lower_rulespec_str error: {error}");
            return;
        }
    };
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(2003, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2003, 12, 31).expect("date"),
    };
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "earnings".to_string(),
            entity: "Worker".to_string(),
            entity_id: "w1".to_string(),
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
            value: ScalarValueSpec::Decimal {
                value: "300".to_string(),
            },
        }],
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
            Ok(response) => println!(
                "[{label}] {mode:?}: OK actual_mode={:?} fallback_reason={:?} results={:?}",
                response.metadata.actual_mode,
                response.metadata.fallback_reason,
                response.results
            ),
            Err(error) => println!("[{label}] {mode:?}: ERROR {error}"),
        }
    }
}

#[test]
fn lifetime_topn_n_label_probe() {
    println!("=== controls ===");
    run_dense("lit 0", &single_rule_module("sum_top_n_over_periods(earnings, 0)"));
    run_dense("lit 4", &single_rule_module("sum_top_n_over_periods(earnings, 4)"));
    run_dense("lit 2.9", &single_rule_module("sum_top_n_over_periods(earnings, 2.9)"));

    println!("=== fractional n truncated to 0 ===");
    let (d_half, f_half) =
        run_dense("lit 0.5", &single_rule_module("sum_top_n_over_periods(earnings, 0.5)"));
    let (d_neg, f_neg) = run_dense(
        "lit 0 - 0.9",
        &single_rule_module("sum_top_n_over_periods(earnings, 0 - 0.9)"),
    );
    let (d_p, f_p) = run_dense("param Rate 0.5", &param_module("Rate", "0.5"));
    let (d_p35, f_p35) = run_dense("lit 3.5", &single_rule_module("sum_top_n_over_periods(earnings, 3.5)"));
    let (d_p45, f_p45) = run_dense("lit 4.5", &single_rule_module("sum_top_n_over_periods(earnings, 4.5)"));

    println!("=== 2^63 (saturation at i64::MAX in f64 mode) ===");
    let (d_big, f_big) = run_dense(
        "lit 9223372036854775808.0",
        &single_rule_module("sum_top_n_over_periods(earnings, 9223372036854775808.0)"),
    );
    let (d_big2, f_big2) = run_dense(
        "lit 9223372036854775808",
        &single_rule_module("sum_top_n_over_periods(earnings, 9223372036854775808)"),
    );

    println!("=== 2^53+1 Integer (f64 cast) ===");
    let (d_53, f_53) = run_dense(
        "lit 9007199254740993",
        &single_rule_module("sum_top_n_over_periods(earnings, 9007199254740993)"),
    );
    let (d_53p, f_53p) = run_dense("param Integer 9007199254740993", &param_module("Integer", "9007199254740993"));

    println!("=== explain / fast for the same formulas ===");
    run_explain_fast("lit 0.5", &single_rule_module("sum_top_n_over_periods(earnings, 0.5)"));
    run_explain_fast(
        "lit 9223372036854775808.0",
        &single_rule_module("sum_top_n_over_periods(earnings, 9223372036854775808.0)"),
    );

    println!("=== summary ===");
    for (name, d, f) in [
        ("0.5", &d_half, &f_half),
        ("0 - 0.9", &d_neg, &f_neg),
        ("param 0.5", &d_p, &f_p),
        ("3.5", &d_p35, &f_p35),
        ("4.5", &d_p45, &f_p45),
        ("9223372036854775808.0", &d_big, &f_big),
        ("9223372036854775808", &d_big2, &f_big2),
        ("9007199254740993", &d_53, &f_53),
        ("param Integer 9007199254740993", &d_53p, &f_53p),
    ] {
        println!(
            "SUMMARY supplied={name:<32} decimal_label={:<28} f64_label={}",
            n_label(d),
            n_label(f)
        );
    }
}
