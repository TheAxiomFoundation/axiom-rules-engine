//! Verification probe (candidate dense-if-mixed-dtype-branches):
//! dense `if()` evaluates both branches eagerly and `select_dense_scalar_column`
//! rejects any non-numeric mismatched branch dtype pair (Text/Integer,
//! Bool/Integer, Date/Integer) with "dense if() branches must have the same
//! dtype", even when every row selects one branch. Explain evaluates only the
//! selected branch and returns its value.
//! Compares explain / fast / dense (Decimal) / dense (f64) for root, related
//! and lifetime executors.

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

const ROOT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: label_text
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2025-01-01
        formula: 'if x > 0: "yes" else: 0'
  - name: label_bool
    kind: derived
    entity: Person
    dtype: Bool
    versions:
      - effective_from: 2025-01-01
        formula: 'if x > 0: true else: 0'
  - name: label_date
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: 'if x > 0: d else: 0'
  - name: label_control
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2025-01-01
        formula: 'if x > 0: "yes" else: "no"'
"#;

const RELATED_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_total
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: 'sum(member_of_household.(if x > 0: "yes" else: 0))'
"#;

const RELATED_RULESPEC_VIA_DERIVED: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: person_value
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: 'if x > 0: "yes" else: 0'
  - name: household_total
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: sum(member_of_household.person_value)
"#;

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: label
    kind: derived
    entity: Worker
    dtype: Text
    versions:
      - effective_from: 1960-01-01
        formula: 'if sum_over_periods(earnings) > 0: "yes" else: 0'
  - name: label_control
    kind: derived
    entity: Worker
    dtype: Text
    versions:
      - effective_from: 1960-01-01
        formula: 'if sum_over_periods(earnings) > 0: "yes" else: "no"'
"#;

fn month_period() -> PeriodSpec {
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

fn input(name: &str, entity: &str, entity_id: &str, value: ScalarValueSpec) -> InputRecordSpec {
    let period = month_period();
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: entity_id.to_string(),
        interval: IntervalSpec {
            start: period.start,
            end: period.end,
        },
        value,
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
            period: month_period(),
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
    let period = month_period().to_model().expect("period converts");
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
fn verify_dense_root_if_mixed_dtype_branches() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(ROOT_RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");
    let june_1 = chrono::NaiveDate::from_ymd_opt(2025, 6, 1).expect("date");

    for output in ["label_text", "label_bool", "label_date", "label_control"] {
        for x in [1_i64, 0] {
            let dataset = DatasetSpec {
                inputs: vec![
                    input("x", "Person", "person-1", ScalarValueSpec::Integer { value: x }),
                    input("d", "Person", "person-1", ScalarValueSpec::Date { value: june_1 }),
                ],
                relations: vec![],
            };
            let batch = || DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([
                    ("x".to_string(), DenseColumn::Integer(vec![x])),
                    ("d".to_string(), DenseColumn::Date(vec![june_1])),
                ]),
                relations: HashMap::new(),
            };
            println!("=== [root] {output} x={x} (every row takes one branch) ===");
            println!(
                "  explain   : {}",
                run_api(ExecutionMode::Explain, &artifact, dataset.clone(), "person-1", output)
            );
            println!(
                "  fast      : {}",
                run_api(ExecutionMode::Fast, &artifact, dataset, "person-1", output)
            );
            println!("  dense dec : {}", run_dense(&dense, batch(), output, false));
            println!("  dense f64 : {}", run_dense(&dense, batch(), output, true));
        }
    }
}

fn run_related(label: &str, rulespec: &str) {
    let artifact = match CompiledProgramArtifact::from_rulespec_str(rulespec) {
        Ok(artifact) => artifact,
        Err(error) => {
            println!("=== [related:{label}] compile ERR {error}");
            return;
        }
    };
    let dense = match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
        Ok(dense) => dense,
        Err(error) => {
            println!("=== [related:{label}] dense compile ERR {error}");
            return;
        }
    };
    let period = month_period();
    // Two members, both x = 0: every related row takes `else 0`.
    let dataset = DatasetSpec {
        inputs: vec![
            input("x", "Person", "p1", ScalarValueSpec::Integer { value: 0 }),
            input("x", "Person", "p2", ScalarValueSpec::Integer { value: 0 }),
        ],
        relations: ["p1", "p2"]
            .iter()
            .map(|person| RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person.to_string(), "h1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            })
            .collect(),
    };
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
                offsets: vec![0, 2],
                inputs: HashMap::from([("x".to_string(), DenseColumn::Integer(vec![0, 0]))]),
            },
        )]),
    };
    println!("=== [related:{label}] household_total, both members x=0 ===");
    println!(
        "  explain   : {}",
        run_api(ExecutionMode::Explain, &artifact, dataset.clone(), "h1", "household_total")
    );
    println!(
        "  fast      : {}",
        run_api(ExecutionMode::Fast, &artifact, dataset, "h1", "household_total")
    );
    println!("  dense dec : {}", run_dense(&dense, batch(), "household_total", false));
    println!("  dense f64 : {}", run_dense(&dense, batch(), "household_total", true));
}

#[test]
fn verify_dense_related_if_mixed_dtype_branches() {
    run_related("inline", RELATED_RULESPEC);
    run_related("via_derived", RELATED_RULESPEC_VIA_DERIVED);
}

#[test]
fn verify_dense_lifetime_if_mixed_dtype_branches() {
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
    for output in ["label", "label_control"] {
        for f64_mode in [false, true] {
            let batches = vec![
                DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::from([(
                        "earnings".to_string(),
                        DenseColumn::Float(vec![100.0]),
                    )]),
                    relations: HashMap::new(),
                },
                DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::from([(
                        "earnings".to_string(),
                        DenseColumn::Float(vec![50.0]),
                    )]),
                    relations: HashMap::new(),
                },
            ];
            let outputs = [output.to_string()];
            let result = if f64_mode {
                dense.execute_lifetime_f64(&periods, batches, &outputs)
            } else {
                dense.execute_lifetime(&periods, batches, &outputs)
            };
            let text = match result {
                Ok(result) => format!("OK {:?}", result.outputs.get(output)),
                Err(error) => format!("ERR {error}"),
            };
            println!(
                "=== [lifetime] {output} earnings sum 150 > 0 (row takes `then`) f64={f64_mode} : {text}"
            );
        }
    }
}
