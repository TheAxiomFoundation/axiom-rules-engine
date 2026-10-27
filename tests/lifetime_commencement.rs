//! Lifetime rule-date checks agree with the traced reference evaluator and
//! per-period dense execution, including rows that never reach a dated rule.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::engine::{Engine, EvalError};
use axiom_rules_engine::model::{
    DataSet, Interval, JudgmentOutcome, Period, PeriodKind, Program, ScalarValue,
};
use chrono::{Duration, NaiveDate};
use proptest::prelude::*;
use rust_decimal::prelude::ToPrimitive;

type Outcome = Result<Vec<f64>, String>;

fn day(start: NaiveDate) -> Period {
    Period {
        kind: PeriodKind::Custom("Day".to_string()),
        start,
        end: start,
    }
}

fn year(year: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
    }
}

fn module(effective_from: NaiveDate) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: commenced_scalar
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '{effective_from}'
        formula: amount + 1
  - name: commenced_judgment
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: '{effective_from}'
        formula: amount > 0
  - name: guarded_scalar
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: |
          if enabled == 1: commenced_scalar
          else: 0
  - name: guarded_judgment_scalar
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: |
          if enabled == 1 and commenced_judgment: amount + 1
          else: 0
  - name: total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: sum_over_periods(guarded_scalar)
  - name: unconditional_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: sum_over_periods(commenced_scalar)
  - name: outer_guarded_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: |
          if enabled == 1: sum_over_periods(commenced_scalar)
          else: 0
  - name: judgment_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: sum_over_periods(guarded_judgment_scalar)
  - name: direct_scalar_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: sum_over_periods(0) + guarded_scalar
  - name: direct_judgment_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '1900-01-01'
        formula: |
          if enabled == 1 and commenced_judgment: sum_over_periods(0) + amount + 1
          else: sum_over_periods(0)
  - name: commenced_scalar_total
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '{effective_from}'
        formula: sum_over_periods(amount)
  - name: commenced_judgment_total
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: '{effective_from}'
        formula: sum_over_periods(amount) > 0
"#
    )
}

fn programs(effective_from: NaiveDate) -> (DenseCompiledProgram, Program) {
    let artifact = CompiledProgramArtifact::from_rulespec_str(&module(effective_from))
        .expect("commencement fixture compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("single open-ended versions compile densely");
    (dense, artifact.program.to_program().unwrap())
}

fn batch(rows: &[(i64, bool)]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: rows.len(),
        inputs: HashMap::from([
            (
                "amount".to_string(),
                DenseColumn::Integer(rows.iter().map(|(amount, _)| *amount).collect()),
            ),
            (
                "enabled".to_string(),
                DenseColumn::Integer(
                    rows.iter()
                        .map(|(_, enabled)| i64::from(*enabled))
                        .collect(),
                ),
            ),
        ]),
        relations: HashMap::new(),
    }
}

fn outcome(result: Result<DenseExecutionResult, EvalError>, output: &str) -> Outcome {
    result
        .map(|result| match &result.outputs[output] {
            DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
                values.iter().map(|value| *value as f64).collect()
            }
            DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                values.iter().map(|value| value.to_f64().unwrap()).collect()
            }
            DenseOutputValue::Scalar(DenseColumn::Float(values)) => values.clone(),
            DenseOutputValue::Judgment(values) => values
                .iter()
                .map(|value| {
                    if *value == JudgmentOutcome::Holds {
                        1.0
                    } else {
                        0.0
                    }
                })
                .collect(),
            other => panic!("unexpected output: {other:?}"),
        })
        .map_err(|error| error.to_string())
}

fn lifetime(
    dense: &DenseCompiledProgram,
    periods: &[Period],
    rows: &[Vec<(i64, bool)>],
    output: &str,
    float: bool,
) -> Outcome {
    let batches = rows.iter().map(|rows| batch(rows)).collect();
    let outputs = [output.to_string()];
    outcome(
        if float {
            dense.execute_lifetime_f64(periods, batches, &outputs)
        } else {
            dense.execute_lifetime(periods, batches, &outputs)
        },
        output,
    )
}

fn explain(program: &Program, period: &Period, row: (i64, bool), output: &str) -> Outcome {
    let mut dataset = DataSet::default();
    for (name, value) in [("amount", row.0), ("enabled", i64::from(row.1))] {
        dataset.add_input(
            name,
            "Person",
            "p0",
            Interval::covering(period),
            ScalarValue::Integer(value),
        );
    }
    Engine::new(program, &dataset)
        .evaluate_scalar(output, "p0", period)
        .map(|value| vec![value.as_decimal().unwrap().to_f64().unwrap()])
        .map_err(|error| error.to_string())
}

fn missing(rule: &str, period: &Period) -> Outcome {
    Err(EvalError::MissingDerivedFormulaVersion {
        derived: rule.to_string(),
        at: period.start,
    }
    .to_string())
}

fn reduction_parity(periods: Vec<Period>, effective_from: NaiveDate) {
    let (dense, program) = programs(effective_from);
    for (output, per_period) in [
        ("total", "guarded_scalar"),
        ("outer_guarded_total", "guarded_scalar"),
        ("judgment_total", "guarded_judgment_scalar"),
    ] {
        // Compare individual rows because the public dense entry points return
        // the first row's error for the whole batch.
        for row in [(7, false), (-2, true), (0, true), (7, true)] {
            let mut expected_total = Ok(vec![0.0]);
            for period in &periods {
                let expected = explain(&program, period, row, per_period);
                for float in [false, true] {
                    let outputs = [per_period.to_string()];
                    assert_eq!(
                        outcome(
                            if float {
                                dense.execute_f64(period, batch(&[row]), &outputs)
                            } else {
                                dense.execute(period, batch(&[row]), &outputs)
                            },
                            per_period,
                        ),
                        expected
                    );
                    assert_eq!(
                        lifetime(
                            &dense,
                            std::slice::from_ref(period),
                            &[vec![row]],
                            output,
                            float
                        ),
                        expected,
                        "{output}, period {}, row {row:?}, float={float}",
                        period.start
                    );
                }
                expected_total = expected_total.and_then(|mut sum| {
                    expected.map(|value| {
                        sum[0] += value[0];
                        sum
                    })
                });
            }
            let rows = vec![vec![row]; periods.len()];
            for float in [false, true] {
                assert_eq!(
                    lifetime(&dense, &periods, &rows, output, float),
                    expected_total
                );
            }
        }
        // An entire skipped batch remains valid, and either row ordering must
        // preserve the same error when only one row reaches the dated rule.
        let rows = vec![vec![(4, false), (9, false)]; periods.len()];
        assert_eq!(
            lifetime(&dense, &periods, &rows, output, false),
            Ok(vec![0.0, 0.0])
        );
        for rows in [vec![(4, false), (9, true)], vec![(9, true), (4, false)]] {
            let rows = vec![rows; periods.len()];
            let rule = if output == "judgment_total" {
                "commenced_judgment"
            } else {
                "commenced_scalar"
            };
            for float in [false, true] {
                assert_eq!(
                    lifetime(&dense, &periods, &rows, output, float),
                    missing(rule, &periods[0])
                );
            }
        }
        // With only commenced periods, mixed rows retain their values.
        let commenced: Vec<_> = periods
            .iter()
            .filter(|period| period.start >= effective_from)
            .cloned()
            .collect();
        let rows = vec![vec![(4, false), (9, true)]; commenced.len()];
        for float in [false, true] {
            assert_eq!(
                lifetime(&dense, &commenced, &rows, output, float),
                Ok(vec![0.0, 10.0 * commenced.len() as f64])
            );
        }
    }
}

#[test]
fn lifetime_reductions_match_dense_and_explain_at_day_boundaries() {
    let date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
    reduction_parity(
        (-1..=1)
            .map(|offset| day(date + Duration::days(offset)))
            .collect(),
        date,
    );
}

#[test]
fn lifetime_reductions_match_dense_and_explain_at_year_boundaries() {
    reduction_parity(vec![year(2025), year(2026), year(2027)], year(2026).start);
}

#[test]
fn lifetime_direct_scalar_and_judgment_use_the_last_period() {
    let date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
    let (dense, program) = programs(date);
    for last_offset in [-1, 0, 1] {
        let periods = vec![
            day(date - Duration::days(2)),
            day(date + Duration::days(last_offset)),
        ];
        let reference = periods.last().unwrap();
        for (output, per_period) in [
            ("direct_scalar_total", "guarded_scalar"),
            ("direct_judgment_total", "guarded_judgment_scalar"),
        ] {
            for row in [(7, false), (-2, true), (0, true), (7, true)] {
                let expected = explain(&program, reference, row, per_period);
                for float in [false, true] {
                    let per_period_result = if float {
                        dense.execute_f64(reference, batch(&[row]), &[per_period.to_string()])
                    } else {
                        dense.execute(reference, batch(&[row]), &[per_period.to_string()])
                    };
                    assert_eq!(outcome(per_period_result, per_period), expected);
                    assert_eq!(
                        lifetime(
                            &dense,
                            &periods,
                            &vec![vec![row]; periods.len()],
                            output,
                            float
                        ),
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn lifetime_scalar_output_checks_its_own_commencement_at_the_reference_period() {
    root_output_commencement("commenced_scalar_total");
}

#[test]
fn lifetime_judgment_output_checks_its_own_commencement_at_the_reference_period() {
    root_output_commencement("commenced_judgment_total");
}

fn root_output_commencement(output: &str) {
    let date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
    let (dense, _) = programs(date);
    for last_offset in [-1, 0, 1] {
        let periods = vec![
            day(date - Duration::days(2)),
            day(date + Duration::days(last_offset)),
        ];
        let rows = vec![vec![(3, false), (0, true)]; periods.len()];
        let expected = if last_offset < 0 {
            missing(output, periods.last().unwrap())
        } else if output == "commenced_judgment_total" {
            Ok(vec![1.0, 0.0])
        } else {
            Ok(vec![6.0, 0.0])
        };
        for float in [false, true] {
            assert_eq!(lifetime(&dense, &periods, &rows, output, float), expected);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn lifetime_commencement_properties(
        commencement_offset in 0_i64..10_000,
        generated in prop::collection::vec((-40_i64..=40, -10_000_i64..=10_000), 1..=6),
    ) {
        let date = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap() + Duration::days(commencement_offset);
        // Sorting and deduplication produce a strictly ascending 1–6-period
        // list while retaining a random input for each distinct start date.
        let generated: std::collections::BTreeMap<_, _> = generated.into_iter().collect();
        let periods: Vec<_> = generated.keys().map(|offset| day(date + Duration::days(*offset))).collect();
        let active: Vec<_> = generated.values().map(|amount| vec![(*amount, true)]).collect();
        let skipped: Vec<_> = generated.values().map(|amount| vec![(*amount, false)]).collect();
        let (dense, program) = programs(date);
        let decimal = lifetime(&dense, &periods, &active, "unconditional_total", false);
        let float = lifetime(&dense, &periods, &active, "unconditional_total", true);
        let all_commenced = periods.iter().all(|period| period.start >= date);
        prop_assert_eq!(decimal.is_ok(), all_commenced);
        prop_assert_eq!(decimal.is_ok(), float.is_ok());
        prop_assert_eq!(&decimal, &float);

        let mut per_period_sum = 0.0;
        for (period, rows) in periods.iter().zip(&active) {
            let expected = explain(&program, period, rows[0], "guarded_scalar");
            for float in [false, true] {
                let result = if float {
                    dense.execute_f64(period, batch(rows), &["guarded_scalar".to_string()])
                } else {
                    dense.execute(period, batch(rows), &["guarded_scalar".to_string()])
                };
                prop_assert_eq!(outcome(result, "guarded_scalar"), expected.clone());
            }
            if let Ok(value) = expected {
                per_period_sum += value[0];
            }
        }
        if all_commenced {
            prop_assert_eq!(decimal, Ok(vec![per_period_sum]));
        } else {
            prop_assert_eq!(decimal, missing("commenced_scalar", &periods[0]));
        }
        // The same false input in every period must suppress both the dated
        // inner expression and a whole reduction guarded at lifetime level.
        for output in ["total", "outer_guarded_total"] {
            for float in [false, true] {
                prop_assert_eq!(lifetime(&dense, &periods, &skipped, output, float), Ok(vec![0.0]));
            }
        }
    }
}
