//! Verification probe (candidate dense-generic-date-type-text):
//! when a date function receives a non-date operand, does dense report the
//! generic "expected date dense column" instead of explain's function-specific
//! type-mismatch text?

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval(period: &PeriodSpec) -> IntervalSpec {
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn show(
    result: &Result<DenseExecutionResult, axiom_rules_engine::engine::EvalError>,
    out: &str,
) -> String {
    match result {
        Ok(r) => match r.outputs.get(out) {
            Some(DenseOutputValue::Scalar(col)) => format!("OK {col:?}"),
            other => format!("OK(non-scalar) {other:?}"),
        },
        Err(e) => format!("ERR {e}"),
    }
}

fn run_explain_fast(
    label: &str,
    program: &axiom_rules_engine::spec::ProgramSpec,
    dataset: &DatasetSpec,
    entity_id: &str,
    out: &str,
) -> Vec<String> {
    let period = month_period();
    let mut lines = Vec::new();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let request = ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: period.clone(),
                outputs: vec![out.to_string()],
            }],
        };
        let line = match execute_request(request) {
            Ok(resp) => format!(
                "[{label}] {mode:?}: OK actual_mode={:?} fallback={:?} outputs={:?}",
                resp.metadata.actual_mode, resp.metadata.fallback_reason, resp.results
            ),
            Err(e) => format!("[{label}] {mode:?}: ERR {e}"),
        };
        println!("{line}");
        lines.push(line);
    }
    lines
}

fn root_case(label: &str, formula: &str, dtype: &str, explain_substr: &str) {
    let module = format!(
        r#"
format: rulespec/v1
rules:
  - name: out
    kind: derived
    entity: Person
    dtype: {dtype}
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: {formula}
"#
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&module).expect("module compiles");
    let period = month_period();
    let iv = interval(&period);
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "base".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: iv.clone(),
                value: ScalarValueSpec::Integer { value: 5 },
            },
            InputRecordSpec {
                name: "n".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: iv.clone(),
                value: ScalarValueSpec::Integer { value: 1 },
            },
        ],
        relations: Vec::new(),
    };
    // Only supply inputs the formula references (explain rejects unknown inputs).
    let mut dataset = dataset;
    dataset.inputs.retain(|input| formula.contains(input.name.as_str()));
    let lines = run_explain_fast(label, &artifact.program, &dataset, "p1", "out");

    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("dense compilation succeeds");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("base".to_string(), DenseColumn::Integer(vec![5])),
            ("n".to_string(), DenseColumn::Integer(vec![1])),
        ])
        .into_iter()
        .filter(|(name, _)| formula.contains(name.as_str()))
        .collect(),
        relations: HashMap::new(),
    };
    let model_period = period.to_model().expect("period converts");
    let d = show(
        &dense.execute(&model_period, batch.clone(), &["out".to_string()]),
        "out",
    );
    let f = show(
        &dense.execute_f64(&model_period, batch, &["out".to_string()]),
        "out",
    );
    println!("[{label}] dense execute     => {d}");
    println!("[{label}] dense execute_f64 => {f}");
    let explain_matches = lines[0].contains(explain_substr);
    let dense_generic = d.contains("expected date dense column");
    println!(
        "[{label}] VERDICT explain_has_specific_text={explain_matches} dense_has_generic_text={dense_generic} divergent_text={}",
        explain_matches && dense_generic
    );
}

#[test]
fn root_date_add_days_integer_base() {
    root_case(
        "root date_add_days",
        "date_add_days(base, n)",
        "Date",
        "date_add_days expects a date on the left",
    );
}

#[test]
fn root_date_add_months_integer_base() {
    root_case(
        "root date_add_months",
        "date_add_months(base, n)",
        "Date",
        "date_add_months expects a date on the left",
    );
}

#[test]
fn root_date_add_years_integer_base() {
    root_case(
        "root date_add_years",
        "date_add_years(base, n)",
        "Date",
        "date_add_years expects a date on the left",
    );
}

#[test]
fn root_days_between_integer_from() {
    root_case(
        "root days_between from",
        "days_between(n, period_start)",
        "Integer",
        "days_between expects a date for `from`",
    );
}

#[test]
fn root_days_between_integer_to() {
    root_case(
        "root days_between to",
        "days_between(period_start, n)",
        "Integer",
        "days_between expects a date for `to`",
    );
}

// Related-row executor: a Person-level derived date expression summed over a
// household relation.
#[test]
fn related_days_between_integer_from() {
    let module = r#"
format: rulespec/v1
rules:
  - name: age_days
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: days_between(dob, period_start)
  - name: total_age_days
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: sum(member_of_household.age_days)
"#;
    let label = "related days_between from";
    let artifact = CompiledProgramArtifact::from_rulespec_str(module).expect("module compiles");
    let period = month_period();
    let iv = interval(&period);
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "dob".to_string(),
            entity: "Person".to_string(),
            entity_id: "p1".to_string(),
            interval: iv.clone(),
            value: ScalarValueSpec::Integer { value: 1 },
        }],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["p1".to_string(), "h1".to_string()],
            interval: iv.clone(),
        }],
    };
    let lines = run_explain_fast(label, &artifact.program, &dataset, "h1", "total_age_days");

    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .expect("dense compilation succeeds");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 1],
                inputs: HashMap::from([("dob".to_string(), DenseColumn::Integer(vec![1]))]),
            },
        )]),
    };
    let model_period = period.to_model().expect("period converts");
    let d = show(
        &dense.execute(&model_period, batch.clone(), &["total_age_days".to_string()]),
        "total_age_days",
    );
    let f = show(
        &dense.execute_f64(&model_period, batch, &["total_age_days".to_string()]),
        "total_age_days",
    );
    println!("[{label}] dense execute     => {d}");
    println!("[{label}] dense execute_f64 => {f}");
    let explain_matches = lines[0].contains("days_between expects a date for `from`");
    let dense_generic = d.contains("expected date dense column");
    println!(
        "[{label}] VERDICT explain_has_specific_text={explain_matches} dense_has_generic_text={dense_generic} divergent_text={}",
        explain_matches && dense_generic
    );
}
