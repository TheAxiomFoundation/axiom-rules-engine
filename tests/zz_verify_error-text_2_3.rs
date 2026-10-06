//! Verification probe: fast (bulk) root compare_columns answers ordering
//! operators (<, <=, >, >=) on Bool/Bool and Text/Text with NotHolds, where
//! explain rejects them with a TypeMismatch. Also prints dense for reference.

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
  - name: bool_if_out
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if ba < bb: 1
          else: 2
  - name: bool_judgment
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: ba >= bb
  - name: text_judgment
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: ta < tb
  - name: text_if_out
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if ta > tb: 1
          else: 2
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
    let period = month_period();
    InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: "person-1".to_string(),
        interval: IntervalSpec {
            start: period.start,
            end: period.end,
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
                input("ba", ScalarValueSpec::Bool { value: false }),
                input("bb", ScalarValueSpec::Bool { value: true }),
                input(
                    "ta",
                    ScalarValueSpec::Text {
                        value: "a".to_string(),
                    },
                ),
                input(
                    "tb",
                    ScalarValueSpec::Text {
                        value: "b".to_string(),
                    },
                ),
            ],
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

fn run_dense(dense: &DenseCompiledProgram, output: &str) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("ba".to_string(), DenseColumn::Bool(vec![false])),
            ("bb".to_string(), DenseColumn::Bool(vec![true])),
            ("ta".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ("tb".to_string(), DenseColumn::Text(vec!["b".to_string()])),
        ]),
        relations: HashMap::new(),
    };
    let outputs = [output.to_string()];
    match dense.execute(&period, batch, &outputs) {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_bulk_bool_text_ordering_silent_notholds() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"));
    let mut divergences = 0;
    for output in ["bool_if_out", "bool_judgment", "text_judgment", "text_if_out"] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        let dense_result = match &dense {
            Ok(dense) => run_dense(dense, output),
            Err(error) => format!("DENSE-COMPILE-ERR {error}"),
        };
        println!("[{output}] explain: {explain}");
        println!("[{output}] fast:    {fast}");
        println!("[{output}] dense:   {dense_result}");
        if explain.starts_with("ERR") && fast.starts_with("OK") && fast.contains("actual_mode=Fast")
        {
            divergences += 1;
            println!("[{output}] DIVERGENCE: explain errors, fast returns a value without fallback");
        }
    }
    println!("divergences: {divergences}");
}
