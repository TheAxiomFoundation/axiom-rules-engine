//! Verification probe (candidate dense-missing-input-placeholder-period):
//! dense `bind_batch` reports a missing required input with the entity KIND
//! (root) or RELATION NAME (related) and a 1900-01-01..1900-01-01 placeholder
//! period, whereas explain reports the entity id and the real query period.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
};

const ROOT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: out
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: 2025-01-01
        formula: x + 1
"#;

const RELATED_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: earned_income_total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    versions:
      - effective_from: 2025-01-01
        formula: sum(member_of_household.earned_income)
"#;

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: total
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: 1960-01-01
        formula: sum_over_periods(earnings)
"#;

fn june_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2025, 6, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2025, 6, 30).expect("date"),
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
    dataset: DatasetSpec,
    entity_id: &str,
    output: &str,
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: june_period(),
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

fn run_dense(
    dense: &DenseCompiledProgram,
    batch: DenseBatchSpec,
    output: &str,
    f64_mode: bool,
) -> String {
    let period = june_period().to_model().expect("period converts");
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
fn verify_dense_root_missing_input_text() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(ROOT_RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");

    let explain = run_api(
        ExecutionMode::Explain,
        &artifact,
        DatasetSpec::default(),
        "p1",
        "out",
    );
    let fast = run_api(
        ExecutionMode::Fast,
        &artifact,
        DatasetSpec::default(),
        "p1",
        "out",
    );
    let empty_batch = || DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::new(),
    };
    let dense_dec = run_dense(&dense, empty_batch(), "out", false);
    let dense_f64 = run_dense(&dense, empty_batch(), "out", true);

    println!("[root] explain   : {explain}");
    println!("[root] fast      : {fast}");
    println!("[root] dense dec : {dense_dec}");
    println!("[root] dense f64 : {dense_f64}");
}

#[test]
fn verify_dense_related_missing_input_text() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATED_RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles");

    let period = june_period();
    let dataset = || DatasetSpec {
        inputs: vec![],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["p1".to_string(), "h1".to_string()],
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
        }],
    };

    let explain = run_api(
        ExecutionMode::Explain,
        &artifact,
        dataset(),
        "h1",
        "earned_income_total",
    );
    let fast = run_api(
        ExecutionMode::Fast,
        &artifact,
        dataset(),
        "h1",
        "earned_income_total",
    );
    let batch = || DenseBatchSpec {
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
                inputs: HashMap::new(),
            },
        )]),
    };
    let dense_dec = run_dense(&dense, batch(), "earned_income_total", false);
    let dense_f64 = run_dense(&dense, batch(), "earned_income_total", true);

    println!("[related] explain   : {explain}");
    println!("[related] fast      : {fast}");
    println!("[related] dense dec : {dense_dec}");
    println!("[related] dense f64 : {dense_f64}");
}

#[test]
fn verify_dense_lifetime_missing_input_text() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(LIFETIME_RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).expect("dense compiles");
    let year = |y: i32| Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    };
    let periods = vec![year(2001), year(2002)];
    let batches = vec![
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([("earnings".to_string(), DenseColumn::Float(vec![100.0]))]),
            relations: HashMap::new(),
        },
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::new(),
        },
    ];
    let result = match dense.execute_lifetime(&periods, batches, &["total".to_string()]) {
        Ok(result) => format!("OK {:?}", result.outputs.get("total")),
        Err(error) => format!("ERR {error}"),
    };
    println!("[lifetime, period 2002 batch lacks `earnings`] dense : {result}");
}
