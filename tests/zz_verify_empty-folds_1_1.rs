//! Verification probe (candidate dense-root-bool-text-ordering-value):
//! dense root (`DenseExecutor::eval_judgment_expr` Comparison) and lifetime
//! (`LifetimeExecutor::eval_judgment` Comparison) route through
//! `compare_dense_columns`, whose Bool and Text arms answer the ordering
//! operators (<, <=, >, >=) with NotHolds, where explain's
//! `compare_scalar_values` errors with TypeMismatch.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

const ROOT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: flag_order
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_a < claim_b
  - name: flag_order_lte
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_a <= claim_b
  - name: flag_order_gt
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_b > claim_a
  - name: flag_order_gte
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: claim_b >= claim_a
  - name: flag_order_amount
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if flag_order: 100
          else: 0
  - name: text_order
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: label_a < label_b
  - name: text_order_amount
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if label_a <= label_b: 100
          else: 0
"#;

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: lifetime_bool_amount
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: 1960-01-01
        formula: |-
          if claim_a < claim_b: sum_over_periods(earnings)
          else: 0
  - name: lifetime_text_amount
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: 1960-01-01
        formula: |-
          if label_a <= label_b: sum_over_periods(earnings)
          else: 0
  - name: lifetime_inner_bool_amount
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: 1960-01-01
        formula: |-
          sum_over_periods(if claim_a < claim_b: earnings else: 0)
"#;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
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
                input("claim_a", ScalarValueSpec::Bool { value: false }),
                input("claim_b", ScalarValueSpec::Bool { value: true }),
                input(
                    "label_a",
                    ScalarValueSpec::Text {
                        value: "a".to_string(),
                    },
                ),
                input(
                    "label_b",
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

fn root_batch() -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("claim_a".to_string(), DenseColumn::Bool(vec![false])),
            ("claim_b".to_string(), DenseColumn::Bool(vec![true])),
            ("label_a".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ("label_b".to_string(), DenseColumn::Text(vec!["b".to_string()])),
        ]),
        relations: HashMap::new(),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, root_batch(), &outputs)
    } else {
        dense.execute(&period, root_batch(), &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

fn lifetime_batch(earnings: f64) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("earnings".to_string(), DenseColumn::Float(vec![earnings])),
            ("claim_a".to_string(), DenseColumn::Bool(vec![false])),
            ("claim_b".to_string(), DenseColumn::Bool(vec![true])),
            ("label_a".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ("label_b".to_string(), DenseColumn::Text(vec!["b".to_string()])),
        ]),
        relations: HashMap::new(),
    }
}

fn run_lifetime(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let periods = vec![year(2024), year(2025), year(2026)];
    let batches = vec![
        lifetime_batch(1000.0),
        lifetime_batch(2000.0),
        lifetime_batch(3000.0),
    ];
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_lifetime_f64(&periods, batches, &outputs)
    } else {
        dense.execute_lifetime(&periods, batches, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_dense_root_and_lifetime_bool_text_ordering() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(ROOT_RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("dense compiles");
    let mut root_divergences = 0;
    for output in [
        "flag_order",
        "flag_order_lte",
        "flag_order_gt",
        "flag_order_gte",
        "flag_order_amount",
        "text_order",
        "text_order_amount",
    ] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        let dense_dec = run_dense(&dense, output, false);
        let dense_f64 = run_dense(&dense, output, true);
        println!("[root {output}] explain:     {explain}");
        println!("[root {output}] fast:        {fast}");
        println!("[root {output}] dense:       {dense_dec}");
        println!("[root {output}] dense_f64:   {dense_f64}");
        if explain.starts_with("ERR") && dense_dec.starts_with("OK") {
            root_divergences += 1;
            println!("[root {output}] DIVERGENCE: explain errors, dense returns a value");
        }
    }
    println!("root divergences: {root_divergences}");

    let lifetime_artifact =
        CompiledProgramArtifact::from_rulespec_str(LIFETIME_RULESPEC).expect("compiles");
    let lifetime_dense = DenseCompiledProgram::from_artifact(&lifetime_artifact, Some("Worker"))
        .expect("dense compiles");
    let mut lifetime_values = 0;
    for output in [
        "lifetime_bool_amount",
        "lifetime_text_amount",
        "lifetime_inner_bool_amount",
    ] {
        let dec = run_lifetime(&lifetime_dense, output, false);
        let f64r = run_lifetime(&lifetime_dense, output, true);
        println!("[lifetime {output}] dense_lifetime:     {dec}");
        println!("[lifetime {output}] dense_lifetime_f64: {f64r}");
        if dec.starts_with("OK") {
            lifetime_values += 1;
            println!(
                "[lifetime {output}] DIVERGENCE: ordering on Bool/Text answered without the TypeMismatch explain raises"
            );
        }
    }
    println!("lifetime value-returning outputs: {lifetime_values}");
}
