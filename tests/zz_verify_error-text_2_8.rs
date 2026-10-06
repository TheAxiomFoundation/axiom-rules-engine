//! Verification probe (dense-if-branch-dtype-error): dense errors
//! "dense if() branches must have the same dtype" when an if() mixes a
//! non-numeric branch with a numeric one, while explain evaluates only the
//! selected branch and returns its value. Compares explain / fast / dense
//! (Decimal and f64) on the same compiled program.

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
  - name: out
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if c: flag
          else: 1
  - name: out_text
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if c: label
          else: 1
  - name: out_same_bool
    kind: derived
    entity: Person
    dtype: Bool
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if c: flag
          else: other_flag
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
        OutputValue::Scalar { value, dtype, .. } => format!("{value:?} (dtype={dtype:?})"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn input(name: &str, entity_id: &str, value: ScalarValueSpec) -> InputRecordSpec {
    let period = month_period();
    InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: entity_id.to_string(),
        interval: IntervalSpec {
            start: period.start,
            end: period.end,
        },
        value,
    }
}

/// Runs one query per row; rows are (c, flag, other_flag, label).
fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    output: &str,
    rows: &[(bool, bool, bool, &str)],
) -> String {
    let period = month_period();
    let mut inputs = Vec::new();
    let mut queries = Vec::new();
    for (index, (c, flag, other_flag, label)) in rows.iter().enumerate() {
        let id = format!("person-{index}");
        inputs.push(input("c", &id, ScalarValueSpec::Bool { value: *c }));
        inputs.push(input("flag", &id, ScalarValueSpec::Bool { value: *flag }));
        inputs.push(input(
            "other_flag",
            &id,
            ScalarValueSpec::Bool { value: *other_flag },
        ));
        inputs.push(input(
            "label",
            &id,
            ScalarValueSpec::Text {
                value: label.to_string(),
            },
        ));
        queries.push(ExecutionQuery {
            assessment_date: None,
            entity_id: id,
            period: period.clone(),
            outputs: vec![output.to_string()],
        });
    }
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs,
            relations: vec![],
        },
        queries,
    });
    match result {
        Ok(response) => {
            let values: Vec<String> = response
                .results
                .iter()
                .map(|result| {
                    result
                        .outputs
                        .get(output)
                        .map(fmt_output)
                        .unwrap_or_else(|| "<missing>".to_string())
                })
                .collect();
            format!(
                "OK {values:?} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(
    dense: &DenseCompiledProgram,
    output: &str,
    rows: &[(bool, bool, bool, &str)],
    f64_mode: bool,
) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: rows.len(),
        inputs: HashMap::from([
            (
                "c".to_string(),
                DenseColumn::Bool(rows.iter().map(|row| row.0).collect()),
            ),
            (
                "flag".to_string(),
                DenseColumn::Bool(rows.iter().map(|row| row.1).collect()),
            ),
            (
                "other_flag".to_string(),
                DenseColumn::Bool(rows.iter().map(|row| row.2).collect()),
            ),
            (
                "label".to_string(),
                DenseColumn::Text(rows.iter().map(|row| row.3.to_string()).collect()),
            ),
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
fn verify_dense_if_branch_dtype_error() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");

    let cases: Vec<(&str, &str, Vec<(bool, bool, bool, &str)>)> = vec![
        ("out", "c=true (selects Bool flag)", vec![(true, true, false, "x")]),
        ("out", "c=false (selects literal 1)", vec![(false, true, false, "x")]),
        (
            "out",
            "2 rows disagree c=[true,false]",
            vec![(true, true, false, "x"), (false, true, false, "x")],
        ),
        (
            "out_text",
            "c=true (selects Text label)",
            vec![(true, true, false, "hello")],
        ),
        (
            "out_text",
            "c=false (selects literal 1)",
            vec![(false, true, false, "hello")],
        ),
        (
            "out_same_bool",
            "control: both branches Bool, c=true",
            vec![(true, true, false, "x")],
        ),
    ];

    for (output, label, rows) in &cases {
        let explain = run_api(ExecutionMode::Explain, &artifact, output, rows);
        let fast = run_api(ExecutionMode::Fast, &artifact, output, rows);
        let dense_dec = run_dense(&dense, output, rows, false);
        let dense_f64 = run_dense(&dense, output, rows, true);
        println!("=== {output} [{label}] ===");
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        println!("  dense decimal: {dense_dec}");
        println!("  dense f64    : {dense_f64}");
    }
}
