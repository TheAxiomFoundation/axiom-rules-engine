//! Verification probe (dense-bool-text-ordering-silent-false): ordering
//! comparisons (`<`, `<=`, `>`, `>=`) on bool/bool or text/text operands.
//! Explain errors ("boolean/text comparisons only support == and !="); the
//! candidate claim is that dense answers NotHolds/false instead, in the root,
//! related (sum_where/count_where filter) and lifetime executors.

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
use rust_decimal::Decimal;

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
    period: &PeriodSpec,
    output: &str,
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
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

// ---------------------------------------------------------------------------
// Root executor
// ---------------------------------------------------------------------------

const ROOT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: flag_low
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag < true
  - name: pick
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if flag_low: 1
          else: 2
  - name: text_low
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: name_a < name_b
  - name: pick_text
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if text_low: 1
          else: 2
"#;

fn root_dataset(period: &PeriodSpec) -> DatasetSpec {
    let interval = interval(period);
    let input = |name: &str, value: ScalarValueSpec| InputRecordSpec {
        name: name.to_string(),
        entity: "Household".to_string(),
        entity_id: "household-1".to_string(),
        interval: interval.clone(),
        value,
    };
    DatasetSpec {
        inputs: vec![
            input("flag", ScalarValueSpec::Bool { value: false }),
            input(
                "name_a",
                ScalarValueSpec::Text {
                    value: "a".to_string(),
                },
            ),
            input(
                "name_b",
                ScalarValueSpec::Text {
                    value: "b".to_string(),
                },
            ),
        ],
        relations: vec![],
    }
}

fn root_batch() -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("flag".to_string(), DenseColumn::Bool(vec![false])),
            ("name_a".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ("name_b".to_string(), DenseColumn::Text(vec!["b".to_string()])),
        ]),
        relations: HashMap::new(),
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
fn verify_root_bool_text_ordering() {
    let period = month_period();
    let artifact = CompiledProgramArtifact::from_rulespec_str(ROOT_RULESPEC).expect("compiles");
    let dense = match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
        Ok(dense) => Some(dense),
        Err(error) => {
            println!("ROOT dense compile ERR {error}");
            None
        }
    };
    for output in ["flag_low", "pick", "text_low", "pick_text"] {
        let explain = run_api(
            ExecutionMode::Explain,
            &artifact,
            root_dataset(&period),
            "household-1",
            &period,
            output,
        );
        let fast = run_api(
            ExecutionMode::Fast,
            &artifact,
            root_dataset(&period),
            "household-1",
            &period,
            output,
        );
        println!("=== ROOT {output} (flag=false, name_a=\"a\", name_b=\"b\") ===");
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        if let Some(dense) = &dense {
            println!(
                "  dense decimal: {}",
                run_dense(dense, root_batch(), output, false)
            );
            println!(
                "  dense f64    : {}",
                run_dense(dense, root_batch(), output, true)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Related-row executor (sum_where / count_where filters)
// ---------------------------------------------------------------------------

const RELATED_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: flag_low
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag < true
  - name: text_low
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: name_a < name_b
  - name: total
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_family, amount, flag_low)
  - name: total_text
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_family, amount, text_low)
  - name: n_low
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_family, flag_low)
  - name: n_low_text
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_family, text_low)
"#;

fn related_dataset(period: &PeriodSpec) -> DatasetSpec {
    let interval = interval(period);
    let input = |name: &str, value: ScalarValueSpec| InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: "child-1".to_string(),
        interval: interval.clone(),
        value,
    };
    DatasetSpec {
        inputs: vec![
            input("flag", ScalarValueSpec::Bool { value: false }),
            input(
                "amount",
                ScalarValueSpec::Decimal {
                    value: "7".to_string(),
                },
            ),
            input(
                "name_a",
                ScalarValueSpec::Text {
                    value: "a".to_string(),
                },
            ),
            input(
                "name_b",
                ScalarValueSpec::Text {
                    value: "b".to_string(),
                },
            ),
        ],
        relations: vec![RelationRecordSpec {
            name: "member_of_family".to_string(),
            tuple: vec!["child-1".to_string(), "family-1".to_string()],
            interval: interval.clone(),
        }],
    }
}

fn related_batch() -> DenseBatchSpec {
    DenseBatchSpec {
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
                inputs: HashMap::from([
                    ("flag".to_string(), DenseColumn::Bool(vec![false])),
                    (
                        "amount".to_string(),
                        DenseColumn::Decimal(vec![Decimal::from(7)]),
                    ),
                    ("name_a".to_string(), DenseColumn::Text(vec!["a".to_string()])),
                    ("name_b".to_string(), DenseColumn::Text(vec!["b".to_string()])),
                ]),
            },
        )]),
    }
}

#[test]
fn verify_related_bool_text_ordering() {
    let period = month_period();
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(RELATED_RULESPEC).expect("compiles");
    let dense = match DenseCompiledProgram::from_artifact(&artifact, Some("Family")) {
        Ok(dense) => Some(dense),
        Err(error) => {
            println!("RELATED dense compile ERR {error}");
            None
        }
    };
    for output in ["total", "total_text", "n_low", "n_low_text"] {
        let explain = run_api(
            ExecutionMode::Explain,
            &artifact,
            related_dataset(&period),
            "family-1",
            &period,
            output,
        );
        let fast = run_api(
            ExecutionMode::Fast,
            &artifact,
            related_dataset(&period),
            "family-1",
            &period,
            output,
        );
        println!(
            "=== RELATED {output} (child flag=false, amount=7, name_a=\"a\", name_b=\"b\") ==="
        );
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        if let Some(dense) = &dense {
            println!(
                "  dense decimal: {}",
                run_dense(dense, related_batch(), output, false)
            );
            println!(
                "  dense f64    : {}",
                run_dense(dense, related_batch(), output, true)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Lifetime executor
// ---------------------------------------------------------------------------

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: life_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          if flag < true: 0
          else: sum_over_periods(earnings)
  - name: life_total_text
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          if name_a < name_b: 0
          else: sum_over_periods(earnings)
"#;

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn lifetime_batch() -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([
            ("flag".to_string(), DenseColumn::Bool(vec![false])),
            ("earnings".to_string(), DenseColumn::Float(vec![100.0])),
            ("name_a".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ("name_b".to_string(), DenseColumn::Text(vec!["b".to_string()])),
        ]),
        relations: HashMap::new(),
    }
}

fn year_spec(y: i32) -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn lifetime_explain_dataset(period: &PeriodSpec) -> DatasetSpec {
    let interval = interval(period);
    let input = |name: &str, value: ScalarValueSpec| InputRecordSpec {
        name: name.to_string(),
        entity: "Worker".to_string(),
        entity_id: "worker-1".to_string(),
        interval: interval.clone(),
        value,
    };
    DatasetSpec {
        inputs: vec![
            input("flag", ScalarValueSpec::Bool { value: false }),
            input(
                "earnings",
                ScalarValueSpec::Decimal {
                    value: "100".to_string(),
                },
            ),
            input(
                "name_a",
                ScalarValueSpec::Text {
                    value: "a".to_string(),
                },
            ),
            input(
                "name_b",
                ScalarValueSpec::Text {
                    value: "b".to_string(),
                },
            ),
        ],
        relations: vec![],
    }
}

#[test]
fn verify_lifetime_bool_text_ordering() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(LIFETIME_RULESPEC).expect("compiles");
    let dense = match DenseCompiledProgram::from_artifact(&artifact, Some("Worker")) {
        Ok(dense) => Some(dense),
        Err(error) => {
            println!("LIFETIME dense compile ERR {error}");
            None
        }
    };
    let periods = vec![year(2001), year(2002)];
    let explain_period = year_spec(2002);
    for output in ["life_total", "life_total_text"] {
        // Explain has no lifetime entry point; run it at a single period to
        // show which error its lazy evaluation reaches first (the condition).
        let explain = run_api(
            ExecutionMode::Explain,
            &artifact,
            lifetime_explain_dataset(&explain_period),
            "worker-1",
            &explain_period,
            output,
        );
        let fast = run_api(
            ExecutionMode::Fast,
            &artifact,
            lifetime_explain_dataset(&explain_period),
            "worker-1",
            &explain_period,
            output,
        );
        println!(
            "=== LIFETIME {output} (flag=false, name_a=\"a\", name_b=\"b\", earnings=100 x 2 years) ==="
        );
        println!("  explain (single period): {explain}");
        println!("  fast (single period)   : {fast}");
        if let Some(dense) = &dense {
            let outputs = [output.to_string()];
            let decimal = match dense.execute_lifetime(
                &periods,
                vec![lifetime_batch(), lifetime_batch()],
                &outputs,
            ) {
                Ok(result) => format!("OK {:?}", result.outputs.get(output)),
                Err(error) => format!("ERR {error}"),
            };
            let f64_result = match dense.execute_lifetime_f64(
                &periods,
                vec![lifetime_batch(), lifetime_batch()],
                &outputs,
            ) {
                Ok(result) => format!("OK {:?}", result.outputs.get(output)),
                Err(error) => format!("ERR {error}"),
            };
            println!("  dense lifetime decimal : {decimal}");
            println!("  dense lifetime f64     : {f64_result}");
        }
    }
}
