//! Verification probe (k1k3-dense-param-key-text): dense parameter-key errors vs
//! explain's `parameter key for `rate` must be an integer`, plus Decimal
//! truncation (K1) and Float saturation (K3) in DenseColumn::as_index_vec,
//! across dense root / related / lifetime executors.

use std::collections::HashMap;
use std::str::FromStr;

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
use rust_decimal::Decimal;

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Decimal
    indexed_by: k
    versions:
      - effective_from: 2020-01-01
        values:
          0: 10
          1: 20
          2: 30
  - name: out
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2020-01-01
        formula: rate[k]
  - name: out_calc
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2020-01-01
        formula: rate[k + 0]
"#;

const RELATED_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Decimal
    indexed_by: k
    versions:
      - effective_from: 2020-01-01
        values:
          0: 10
          1: 20
          2: 30
  - name: person_rate
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2020-01-01
        formula: rate[k]
  - name: hh_total
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2020-01-01
        formula: sum(member_of_household.person_rate)
"#;

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Decimal
    indexed_by: k
    versions:
      - effective_from: 2020-01-01
        values:
          0: 10
          1: 20
          2: 30
  - name: total
    kind: derived
    entity: Worker
    dtype: Decimal
    period: Year
    versions:
      - effective_from: 2020-01-01
        formula: sum_over_periods(earn) + rate[k]
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

#[derive(Clone)]
enum Key {
    Dec(&'static str),
    Bool(bool),
    Text(&'static str),
    Date(chrono::NaiveDate),
    Float(f64),
}

impl Key {
    fn label(&self) -> String {
        match self {
            Key::Dec(v) => format!("Decimal({v})"),
            Key::Bool(v) => format!("Bool({v})"),
            Key::Text(v) => format!("Text({v:?})"),
            Key::Date(v) => format!("Date({v})"),
            Key::Float(v) => format!("Float({v:e})"),
        }
    }

    /// ScalarValueSpec for explain/fast; Float has no explain counterpart.
    fn spec(&self) -> Option<ScalarValueSpec> {
        match self {
            Key::Dec(v) => Some(ScalarValueSpec::Decimal {
                value: Decimal::from_str(v)
                    .or_else(|_| Decimal::from_scientific(v))
                    .expect("decimal")
                    .normalize()
                    .to_string(),
            }),
            Key::Bool(v) => Some(ScalarValueSpec::Bool { value: *v }),
            Key::Text(v) => Some(ScalarValueSpec::Text {
                value: (*v).to_string(),
            }),
            Key::Date(v) => Some(ScalarValueSpec::Date { value: *v }),
            Key::Float(_) => None,
        }
    }

    fn column(&self, n: usize) -> DenseColumn {
        match self {
            Key::Dec(v) => DenseColumn::Decimal(vec![
                Decimal::from_str(v)
                    .or_else(|_| Decimal::from_scientific(v))
                    .expect("decimal");
                n
            ]),
            Key::Bool(v) => DenseColumn::Bool(vec![*v; n]),
            Key::Text(v) => DenseColumn::Text(vec![(*v).to_string(); n]),
            Key::Date(v) => DenseColumn::Date(vec![*v; n]),
            Key::Float(v) => DenseColumn::Float(vec![*v; n]),
        }
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str, key: &Key) -> String {
    let Some(value) = key.spec() else {
        return "n/a (no Float ScalarValue)".to_string();
    };
    let period = month_period();
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "k".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value,
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "p1".to_string(),
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

fn run_dense(dense: &DenseCompiledProgram, output: &str, key: &Key, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([("k".to_string(), key.column(1))]),
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

fn run_api_related(mode: ExecutionMode, artifact: &CompiledProgramArtifact, key: &Key) -> String {
    let Some(value) = key.spec() else {
        return "n/a (no Float ScalarValue)".to_string();
    };
    let period = month_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "k".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: interval.clone(),
                value,
            }],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["p1".to_string(), "h1".to_string()],
                interval,
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
            period: period.clone(),
            outputs: vec!["hh_total".to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get("hh_total")
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

fn run_dense_related(dense: &DenseCompiledProgram, key: &Key, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
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
                inputs: HashMap::from([("k".to_string(), key.column(1))]),
            },
        )]),
    };
    let outputs = ["hh_total".to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, batch, &outputs)
    } else {
        dense.execute(&period, batch, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get("hh_total")),
        Err(error) => format!("ERR {error}"),
    }
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn run_dense_lifetime(dense: &DenseCompiledProgram, key: &Key, f64_mode: bool) -> String {
    let periods = vec![year(2024), year(2025)];
    let batches = periods
        .iter()
        .map(|_| DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([
                ("earn".to_string(), DenseColumn::Decimal(vec![Decimal::from(100)])),
                ("k".to_string(), key.column(1)),
            ]),
            relations: HashMap::new(),
        })
        .collect::<Vec<_>>();
    let outputs = ["total".to_string()];
    let result = if f64_mode {
        dense.execute_lifetime_f64(&periods, batches, &outputs)
    } else {
        dense.execute_lifetime(&periods, batches, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get("total")),
        Err(error) => format!("ERR {error}"),
    }
}

fn keys() -> Vec<Key> {
    vec![
        Key::Dec("1"),
        Key::Dec("1.5"),
        Key::Dec("-0.5"),
        Key::Dec("100000000000000000000"),
        Key::Bool(true),
        Key::Text("1"),
        Key::Date(chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("date")),
        Key::Float(1.5),
        Key::Float(1e20),
        Key::Float(f64::NAN),
        Key::Float(f64::INFINITY),
    ]
}

#[test]
fn verify_dense_param_key_text_root() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");
    for key in keys() {
        for output in ["out", "out_calc"] {
            println!("=== ROOT {output} k={} ===", key.label());
            println!("  explain      : {}", run_api(ExecutionMode::Explain, &artifact, output, &key));
            println!("  fast         : {}", run_api(ExecutionMode::Fast, &artifact, output, &key));
            println!("  dense decimal: {}", run_dense(&dense, output, &key, false));
            println!("  dense f64    : {}", run_dense(&dense, output, &key, true));
        }
    }
}

#[test]
fn verify_dense_param_key_text_related() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATED_RULESPEC).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .expect("dense compiles");
    for key in keys() {
        println!("=== RELATED hh_total k={} ===", key.label());
        println!("  explain      : {}", run_api_related(ExecutionMode::Explain, &artifact, &key));
        println!("  fast         : {}", run_api_related(ExecutionMode::Fast, &artifact, &key));
        println!("  dense decimal: {}", run_dense_related(&dense, &key, false));
        println!("  dense f64    : {}", run_dense_related(&dense, &key, true));
    }
}

#[test]
fn verify_dense_param_key_text_lifetime() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(LIFETIME_RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).expect("dense compiles");
    for key in keys() {
        println!("=== LIFETIME total k={} ===", key.label());
        println!("  dense lifetime decimal: {}", run_dense_lifetime(&dense, &key, false));
        println!("  dense lifetime f64    : {}", run_dense_lifetime(&dense, &key, true));
    }
}
