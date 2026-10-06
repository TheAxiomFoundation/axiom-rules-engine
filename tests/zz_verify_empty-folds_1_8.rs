//! Verification probe (candidate dense-zero-related-rows-parameter-no-version):
//! when an indexed parameter has no version covering the period and the batch
//! has zero related rows, does dense error (with a made-up key 0) where
//! explain returns 0 because SumRelated never evaluates its member value?

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
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};

/// Parameter `rate` only has a version effective from 2030; the probe runs in 2026-01.
const RULESPEC_NO_VERSION: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: age
    versions:
      - effective_from: 2030-01-01
        values:
          1: 5
  - name: member_rate
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2020-01-01
        formula: rate[age]
  - name: total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2020-01-01
        formula: sum(member_of_household.member_rate)
  - name: lifetime_rate
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2020-01-01
        formula: sum_over_periods(rate[age])
"#;

/// Control: identical program but the parameter version covers 2026.
const RULESPEC_WITH_VERSION: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: age
    versions:
      - effective_from: 2020-01-01
        values:
          1: 5
  - name: member_rate
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2020-01-01
        formula: rate[age]
  - name: total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2020-01-01
        formula: sum(member_of_household.member_rate)
"#;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    IntervalSpec {
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("date"),
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

/// `members` = list of (person_id, age) belonging to household h1.
fn dataset(members: &[(&str, i64)]) -> DatasetSpec {
    let mut dataset = DatasetSpec::default();
    for (person_id, age) in members {
        dataset.inputs.push(InputRecordSpec {
            name: "age".to_string(),
            entity: "Person".to_string(),
            entity_id: (*person_id).to_string(),
            interval: interval(),
            value: ScalarValueSpec::Integer { value: *age },
        });
        dataset.relations.push(RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec![(*person_id).to_string(), "h1".to_string()],
            interval: interval(),
        });
    }
    dataset
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    members: &[(&str, i64)],
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset(members),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
            period: month_period(),
            outputs: vec!["total".to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get("total")
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

fn household_batch(ages: &[i64]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, ages.len()],
                inputs: HashMap::from([("age".to_string(), DenseColumn::Integer(ages.to_vec()))]),
            },
        )]),
    }
}

fn run_dense_household(dense: &DenseCompiledProgram, ages: &[i64], f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let outputs = ["total".to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, household_batch(ages), &outputs)
    } else {
        dense.execute(&period, household_batch(ages), &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get("total")),
        Err(error) => format!("ERR {error}"),
    }
}

fn probe(label: &str, rulespec: &str, members: &[(&str, i64)]) {
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    let ages: Vec<i64> = members.iter().map(|(_, age)| *age).collect();
    println!("=== {label} (members={members:?}) ===");
    println!(
        "  explain       : {}",
        run_api(ExecutionMode::Explain, &artifact, members)
    );
    println!(
        "  fast          : {}",
        run_api(ExecutionMode::Fast, &artifact, members)
    );
    match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
        Ok(dense) => {
            println!(
                "  dense decimal : {}",
                run_dense_household(&dense, &ages, false)
            );
            println!(
                "  dense f64     : {}",
                run_dense_household(&dense, &ages, true)
            );
        }
        Err(error) => println!("  dense         : compile ERR {error}"),
    }
}

#[test]
fn verify_dense_zero_related_rows_parameter_no_version() {
    // The candidate: no parameter version at 2026-01, household with zero members.
    probe("NO VERSION, zero members", RULESPEC_NO_VERSION, &[]);
    // Parity reference: no version, one member age 1 -> both should error with key 1.
    probe("NO VERSION, one member age 1", RULESPEC_NO_VERSION, &[("p1", 1)]);
    // Control: version covers 2026, zero members -> both should return 0.
    probe("WITH VERSION, zero members", RULESPEC_WITH_VERSION, &[]);
    // Control: version covers 2026, one member age 1 -> both should return 5.
    probe("WITH VERSION, one member age 1", RULESPEC_WITH_VERSION, &[("p1", 1)]);
}

/// Informational: root (Person) and lifetime executors with row_count 0.
/// Explain has no rows to evaluate here, so there is no explain comparison;
/// this only records what dense does on an empty batch.
#[test]
fn info_dense_root_and_lifetime_zero_rows() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(RULESPEC_NO_VERSION).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("person dense compiles");
    let period = month_period().to_model().expect("period converts");
    let empty = || DenseBatchSpec {
        row_count: 0,
        inputs: HashMap::from([("age".to_string(), DenseColumn::Integer(vec![]))]),
        relations: HashMap::new(),
    };
    let outputs = ["member_rate".to_string()];
    let show = |r: Result<axiom_rules_engine::dense::DenseExecutionResult, _>| match r {
        Ok(result) => format!("OK {:?}", result.outputs.get("member_rate")),
        Err(error) => {
            let error: axiom_rules_engine::engine::EvalError = error;
            format!("ERR {error}")
        }
    };
    println!("=== root Person row_count 0 (NO VERSION) ===");
    println!("  dense decimal : {}", show(dense.execute(&period, empty(), &outputs)));
    println!("  dense f64     : {}", show(dense.execute_f64(&period, empty(), &outputs)));
    let periods = vec![Period {
        kind: PeriodKind::Month,
        start: period.start,
        end: period.end,
    }];
    println!("=== lifetime Person row_count 0 (NO VERSION), output lifetime_rate ===");
    let lifetime_outputs = ["lifetime_rate".to_string()];
    for (tag, r) in [
        ("decimal", dense.execute_lifetime(&periods, vec![empty()], &lifetime_outputs)),
        ("f64    ", dense.execute_lifetime_f64(&periods, vec![empty()], &lifetime_outputs)),
    ] {
        match r {
            Ok(result) => println!("  dense lifetime {tag}: OK {:?}", result.outputs.get("lifetime_rate")),
            Err(error) => println!("  dense lifetime {tag}: ERR {error}"),
        }
    }
}
