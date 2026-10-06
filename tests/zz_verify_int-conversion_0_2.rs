//! Verification probe (candidate "dense-index-error-text"): dense integer-operand
//! failures all report a generic "parameter key for dense lookup must be ..." error,
//! even for date_add offsets and for integral-but-out-of-range values, while explain
//! names the construct. Prints explain / fast / dense (Decimal and f64) outcomes.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn rulespec(dtype: &str, formula: &str) -> String {
    let unit = if dtype == "Money" { "\n    unit: USD" } else { "" };
    format!(
        r#"
format: rulespec/v1
rules:
  - name: table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2026-01-01
        values:
          0: 10
          1: 100
          2: 200
  - name: probe
    kind: derived
    entity: Person
    dtype: {dtype}
    period: Month{unit}
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
"#
    )
}

fn describe_output(value: &OutputValue) -> String {
    format!("{value:?}")
}

fn describe_dense(value: &DenseOutputValue) -> String {
    match value {
        DenseOutputValue::Scalar(DenseColumn::Decimal(v)) => format!("Decimal{v:?}"),
        DenseOutputValue::Scalar(DenseColumn::Float(v)) => format!("Float{v:?}"),
        DenseOutputValue::Scalar(DenseColumn::Date(v)) => format!("Date{v:?}"),
        DenseOutputValue::Scalar(DenseColumn::Integer(v)) => format!("Integer{v:?}"),
        other => format!("{other:?}"),
    }
}

fn run_api(artifact: &CompiledProgramArtifact, mode: ExecutionMode, uses_flag: bool) -> String {
    let p = period();
    let interval = IntervalSpec {
        start: p.start,
        end: p.end,
    };
    let request = ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![if uses_flag {
                InputRecordSpec {
                    name: "flag".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "p1".to_string(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: true },
                }
            } else {
                InputRecordSpec {
                    name: "size".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "p1".to_string(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Integer { value: 25 },
                }
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "p1".to_string(),
            period: p.clone(),
            outputs: vec!["probe".to_string()],
        }],
    };
    match execute_request(request) {
        Ok(response) => format!(
            "OK value={} actual_mode={:?} fallback_reason={:?}",
            describe_output(
                response.results[0]
                    .outputs
                    .get("probe")
                    .expect("probe output")
            ),
            response.metadata.actual_mode,
            response.metadata.fallback_reason
        ),
        Err(error) => format!("ERR {error}"),
    }
}

fn batch() -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("flag".to_string(), DenseColumn::Bool(vec![true])),
            ("size".to_string(), DenseColumn::Integer(vec![25])),
        ]),
        relations: HashMap::new(),
    }
}

fn run_dense(artifact: &CompiledProgramArtifact, f64_mode: bool) -> String {
    let dense = match DenseCompiledProgram::from_artifact(artifact, Some("Person")) {
        Ok(dense) => dense,
        Err(error) => return format!("DENSE-COMPILE-ERR {error}"),
    };
    let model_period = period().to_model().expect("period converts");
    let outputs = ["probe".to_string()];
    let result = if f64_mode {
        dense.execute_f64(&model_period, batch(), &outputs)
    } else {
        dense.execute(&model_period, batch(), &outputs)
    };
    match result {
        Ok(result) => format!(
            "OK value={}",
            describe_dense(result.outputs.get("probe").expect("probe output"))
        ),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_dense_index_error_text() {
    // date_add_days with a 1e20 offset is omitted: in f64 mode the K3 saturation feeds
    // i64::MAX into chrono::Duration::days, which panics (known, PR #198 / K3).
    let cases: [(&str, &str, &str); 7] = [
        ("param_key_bool", "Money", "table[flag]"),
        ("date_add_days_bool", "Date", "date_add_days(period_start, flag)"),
        ("date_add_months_bool", "Date", "date_add_months(period_start, flag)"),
        ("date_add_years_bool", "Date", "date_add_years(period_start, flag)"),
        (
            "param_key_1e20",
            "Money",
            "table[size * 10000000000000000000.0]",
        ),
        (
            "date_add_months_1e20",
            "Date",
            "date_add_months(period_start, size * 10000000000000000000.0)",
        ),
        (
            "date_add_years_1e20",
            "Date",
            "date_add_years(period_start, size * 10000000000000000000.0)",
        ),
    ];
    let mut explain_named = 0;
    let mut dense_generic = 0;
    for (label, dtype, formula) in cases {
        let source = rulespec(dtype, formula);
        let artifact = match CompiledProgramArtifact::from_rulespec_str(&source) {
            Ok(artifact) => artifact,
            Err(error) => {
                println!("[{label}] formula `{formula}` COMPILE-ERR {error}");
                continue;
            }
        };
        let uses_flag = formula.contains("flag");
        let explain = run_api(&artifact, ExecutionMode::Explain, uses_flag);
        let fast = run_api(&artifact, ExecutionMode::Fast, uses_flag);
        let dense_dec = run_dense(&artifact, false);
        let dense_f64 = run_dense(&artifact, true);
        println!("[{label}] formula `{formula}`");
        println!("  explain     : {explain}");
        println!("  fast        : {fast}");
        println!("  dense(dec)  : {dense_dec}");
        println!("  dense(f64)  : {dense_f64}");
        if explain.contains("must be an integer") || explain.contains("expects an integer") {
            explain_named += 1;
        }
        if dense_dec.contains("parameter key for dense lookup") {
            dense_generic += 1;
        }
    }
    println!("SUMMARY explain_named={explain_named} dense_generic_param_key={dense_generic}");
}
