//! Verification probe (k1k3-dense-date-offset-text): dense date_add_days /
//! date_add_months / date_add_years offsets go through the shared
//! DenseColumn::as_index_vec, so non-integer offsets either truncate (K1),
//! saturate (K3), or error with "parameter key for dense lookup ..." text
//! instead of explain's per-function "... expects an integer ... count on the
//! right" message. Compares explain / fast / dense execute / dense execute_f64
//! at the root and on a related (Person-in-Family) row.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;

const ROOT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: days_out
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_days(base, n)
  - name: days_calc
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_days(base, n + 0)
  - name: months_out
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_months(base, n)
  - name: months_calc
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_months(base, n + 0)
  - name: years_out
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_years(base, n)
  - name: years_calc
    kind: derived
    entity: Person
    dtype: Date
    versions:
      - effective_from: 2025-01-01
        formula: date_add_years(base, n + 0)
"#;

const RELATED_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: child_days
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: days_between(period_start, date_add_days(period_start, n))
  - name: child_months
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: days_between(period_start, date_add_months(period_start, n))
  - name: child_years
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: days_between(period_start, date_add_years(period_start, n))
  - name: family_days
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: sum(member_of_family.child_days)
  - name: family_months
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: sum(member_of_family.child_months)
  - name: family_years
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: sum(member_of_family.child_years)
"#;

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2025, 1, 31).unwrap(),
    }
}

fn interval() -> IntervalSpec {
    let p = period();
    IntervalSpec {
        start: p.start,
        end: p.end,
    }
}

fn base_date() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2025, 1, 31).unwrap()
}

#[derive(Clone)]
struct Case {
    label: &'static str,
    spec: ScalarValueSpec,
    dense_col: DenseColumn,
}

fn cases() -> Vec<Case> {
    let dec = |s: &str| Decimal::from_str(s).unwrap();
    vec![
        Case {
            label: "int_1 (control)",
            spec: ScalarValueSpec::Integer { value: 1 },
            dense_col: DenseColumn::Integer(vec![1]),
        },
        Case {
            label: "bool_true",
            spec: ScalarValueSpec::Bool { value: true },
            dense_col: DenseColumn::Bool(vec![true]),
        },
        Case {
            label: "text_x",
            spec: ScalarValueSpec::Text {
                value: "x".to_string(),
            },
            dense_col: DenseColumn::Text(vec!["x".to_string()]),
        },
        Case {
            label: "date_2025-01-01",
            spec: ScalarValueSpec::Date {
                value: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
            },
            dense_col: DenseColumn::Date(vec![chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()]),
        },
        Case {
            label: "decimal_1.5",
            spec: ScalarValueSpec::Decimal {
                value: "1.5".to_string(),
            },
            dense_col: DenseColumn::Decimal(vec![dec("1.5")]),
        },
        Case {
            label: "decimal_1e20",
            spec: ScalarValueSpec::Decimal {
                value: "100000000000000000000".to_string(),
            },
            dense_col: DenseColumn::Decimal(vec![dec("100000000000000000000")]),
        },
    ]
}

fn describe_output(output: &OutputValue) -> String {
    match output {
        OutputValue::Scalar { value, .. } => format!("Scalar({value:?})"),
        OutputValue::Judgment { outcome, .. } => format!("Judgment({outcome:?})"),
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "<non-string panic>".to_string()
    }
}

fn run_api(dataset: &DatasetSpec, artifact: &CompiledProgramArtifact, entity: &str, output: &str, mode: ExecutionMode) -> String {
    let req = ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset.clone(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity.to_string(),
            period: period(),
            outputs: vec![output.to_string()],
        }],
    };
    match catch_unwind(AssertUnwindSafe(|| execute_request(req))) {
        Ok(Ok(response)) => format!(
            "OK {} actual_mode={:?} fallback_reason={:?}",
            response.results[0]
                .outputs
                .get(output)
                .map(describe_output)
                .unwrap_or_else(|| "<missing>".to_string()),
            response.metadata.actual_mode,
            response.metadata.fallback_reason
        ),
        Ok(Err(error)) => format!("ERR {error}"),
        Err(p) => format!("PANIC {}", panic_text(p)),
    }
}

fn run_dense(
    dense: &DenseCompiledProgram,
    batch: impl Fn() -> DenseBatchSpec,
    output: &str,
    f64_mode: bool,
) -> String {
    let model_period = period().to_model().unwrap();
    let outputs = vec![output.to_string()];
    let r = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&model_period, batch(), &outputs)
        } else {
            dense.execute(&model_period, batch(), &outputs)
        }
    }));
    match r {
        Ok(Ok(result)) => format!("OK {:?}", result.outputs.get(output)),
        Ok(Err(error)) => format!("ERR {error}"),
        Err(p) => format!("PANIC {}", panic_text(p)),
    }
}

#[test]
fn root_date_offset_text() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(ROOT_RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");
    let outputs = ["days_out", "days_calc", "months_out", "months_calc", "years_out", "years_calc"];

    let mut all_cases = cases();
    // Float-typed dense inputs too (f64 path, K3).
    all_cases.push(Case {
        label: "float_1.5 (dense only; explain gets decimal 1.5)",
        spec: ScalarValueSpec::Decimal {
            value: "1.5".to_string(),
        },
        dense_col: DenseColumn::Float(vec![1.5]),
    });
    all_cases.push(Case {
        label: "float_1e20 (dense only; explain gets decimal 1e20)",
        spec: ScalarValueSpec::Decimal {
            value: "100000000000000000000".to_string(),
        },
        dense_col: DenseColumn::Float(vec![1e20]),
    });

    for case in &all_cases {
        let dataset = DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name: "base".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "p1".to_string(),
                    interval: interval(),
                    value: ScalarValueSpec::Date { value: base_date() },
                },
                InputRecordSpec {
                    name: "n".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "p1".to_string(),
                    interval: interval(),
                    value: case.spec.clone(),
                },
            ],
            relations: vec![],
        };
        for output in outputs {
            let explain = run_api(&dataset, &artifact, "p1", output, ExecutionMode::Explain);
            let fast = run_api(&dataset, &artifact, "p1", output, ExecutionMode::Fast);
            let col = case.dense_col.clone();
            let batch = || DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([
                    ("base".to_string(), DenseColumn::Date(vec![base_date()])),
                    ("n".to_string(), col.clone()),
                ]),
                relations: HashMap::new(),
            };
            let d_dec = run_dense(&dense, batch, output, false);
            let d_f64 = run_dense(&dense, batch, output, true);
            println!("ROOT n={} {output}", case.label);
            println!("    explain          : {explain}");
            println!("    fast             : {fast}");
            println!("    dense execute    : {d_dec}");
            println!("    dense execute_f64: {d_f64}");
        }
    }
}

#[test]
fn related_date_offset_text() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATED_RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).expect("dense compiles");
    let outputs = ["family_days", "family_months", "family_years"];
    for case in cases() {
        let dataset = DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "n".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: interval(),
                value: case.spec.clone(),
            }],
            relations: vec![RelationRecordSpec {
                name: "member_of_family".to_string(),
                tuple: vec!["p1".to_string(), "f1".to_string()],
                interval: interval(),
            }],
        };
        for output in outputs {
            let explain = run_api(&dataset, &artifact, "f1", output, ExecutionMode::Explain);
            let fast = run_api(&dataset, &artifact, "f1", output, ExecutionMode::Fast);
            let col = case.dense_col.clone();
            let batch = || DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "member_of_family".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: vec![0, 1],
                        inputs: HashMap::from([("n".to_string(), col.clone())]),
                    },
                )]),
            };
            let d_dec = run_dense(&dense, batch, output, false);
            let d_f64 = run_dense(&dense, batch, output, true);
            println!("RELATED n={} {output}", case.label);
            println!("    explain          : {explain}");
            println!("    fast             : {fast}");
            println!("    dense execute    : {d_dec}");
            println!("    dense execute_f64: {d_f64}");
        }
    }
}
