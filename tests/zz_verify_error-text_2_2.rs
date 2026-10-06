//! Verification probe (k2-dense-empty-max-min-text): `max()` / `min()` with
//! no operands. Explain errors "max() requires at least one operand"; this
//! probe prints what fast (bulk) and dense (root, related-row, lifetime; both
//! Decimal and f64 modes) return for the same formulas, including "plausible
//! value" variants where the empty call is nested inside a non-empty one.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: out_max
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: max()
  - name: out_min
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: min()
  - name: plaus_max
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: max(income, max())
  - name: plaus_min
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: min(income, min())
  - name: fam_sum_max
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_family.out_max)
  - name: fam_sum_plaus
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_family.plaus_max)
"#;

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: life_plus_min
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: sum_over_periods(earnings) + min()
  - name: life_plaus
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: max(sum_over_periods(earnings), max())
"#;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let p = month_period();
    IntervalSpec {
        start: p.start,
        end: p.end,
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn dataset() -> DatasetSpec {
    // family-1 has two children with income 100 and 250.
    let mut dataset = DatasetSpec::default();
    for (child, income) in [("child-1", "100"), ("child-2", "250")] {
        dataset.inputs.push(InputRecordSpec {
            name: "income".to_string(),
            entity: "Person".to_string(),
            entity_id: child.to_string(),
            interval: interval(),
            value: ScalarValueSpec::Decimal {
                value: income.to_string(),
            },
        });
        dataset.relations.push(RelationRecordSpec {
            name: "member_of_family".to_string(),
            tuple: vec![child.to_string(), "family-1".to_string()],
            interval: interval(),
        });
    }
    dataset
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    entity_id: &str,
    output: &str,
) -> String {
    let result = catch_unwind(AssertUnwindSafe(|| {
        execute_request(ExecutionRequest {
            mode,
            program: artifact.program.clone(),
            dataset: dataset(),
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: month_period(),
                outputs: vec![output.to_string()],
            }],
        })
    }));
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(response)) => {
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
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

fn fmt_dense(
    result: std::thread::Result<
        Result<DenseExecutionResult, axiom_rules_engine::engine::EvalError>,
    >,
    output: &str,
) -> String {
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(result)) => format!("OK {:?}", result.outputs.get(output)),
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

fn run_dense_root(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([(
            "income".to_string(),
            DenseColumn::Decimal(vec![Decimal::from(100)]),
        )]),
        relations: HashMap::new(),
    };
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&period, batch, &outputs)
        } else {
            dense.execute(&period, batch, &outputs)
        }
    }));
    fmt_dense(result, output)
}

fn run_dense_related(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_family".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2],
                inputs: HashMap::from([(
                    "income".to_string(),
                    DenseColumn::Decimal(vec![Decimal::from(100), Decimal::from(250)]),
                )]),
            },
        )]),
    };
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&period, batch, &outputs)
        } else {
            dense.execute(&period, batch, &outputs)
        }
    }));
    fmt_dense(result, output)
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn run_dense_lifetime(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = [100.0, 250.0, 50.0]
        .into_iter()
        .map(|v| DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([("earnings".to_string(), DenseColumn::Float(vec![v]))]),
            relations: HashMap::new(),
        })
        .collect::<Vec<_>>();
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_lifetime_f64(&periods, batches, &outputs)
        } else {
            dense.execute_lifetime(&periods, batches, &outputs)
        }
    }));
    fmt_dense(result, output)
}

#[test]
fn verify_empty_max_min_parity() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense_person =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense person");
    let dense_family =
        DenseCompiledProgram::from_artifact(&artifact, Some("Family")).expect("dense family");

    for output in ["out_max", "out_min", "plaus_max", "plaus_min"] {
        println!("=== ROOT {output} (Person child-1, income=100) ===");
        println!(
            "  explain      : {}",
            run_api(ExecutionMode::Explain, &artifact, "child-1", output)
        );
        println!(
            "  fast         : {}",
            run_api(ExecutionMode::Fast, &artifact, "child-1", output)
        );
        println!("  dense decimal: {}", run_dense_root(&dense_person, output, false));
        println!("  dense f64    : {}", run_dense_root(&dense_person, output, true));
    }

    for output in ["fam_sum_max", "fam_sum_plaus"] {
        println!("=== RELATED {output} (family-1, child incomes 100+250) ===");
        println!(
            "  explain      : {}",
            run_api(ExecutionMode::Explain, &artifact, "family-1", output)
        );
        println!(
            "  fast         : {}",
            run_api(ExecutionMode::Fast, &artifact, "family-1", output)
        );
        println!("  dense decimal: {}", run_dense_related(&dense_family, output, false));
        println!("  dense f64    : {}", run_dense_related(&dense_family, output, true));
    }

    let life_artifact =
        CompiledProgramArtifact::from_rulespec_str(LIFETIME_RULESPEC).expect("lifetime compiles");
    let dense_worker = DenseCompiledProgram::from_artifact(&life_artifact, Some("Worker"))
        .expect("dense worker");
    for output in ["life_plus_min", "life_plaus"] {
        println!("=== LIFETIME {output} (earnings 100,250,50) ===");
        println!("  (explain has no lifetime mode; reference = explain's empty max/min error)");
        println!(
            "  dense decimal: {}",
            run_dense_lifetime(&dense_worker, output, false)
        );
        println!(
            "  dense f64    : {}",
            run_dense_lifetime(&dense_worker, output, true)
        );
    }
}
