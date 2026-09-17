use std::{collections::HashMap, str::FromStr};

use axiom_rules_engine::{
    api::{ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request},
    compile::CompiledProgramArtifact,
    dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue},
    spec::{
        DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
    },
};
use chrono::NaiveDate;

fn date(value: &str) -> NaiveDate {
    NaiveDate::from_str(value).unwrap()
}

fn artifact(function: &str) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(&format!(
        r#"format: rulespec/v1
module:
  summary: Synthetic calendar arithmetic, without statutory age or expiry semantics.
rules:
  - name: shifted_date
    kind: derived
    entity: Person
    dtype: Date
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: {function}(base_date, offset)
"#
    ))
    .unwrap()
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: date("2025-06-01"),
        end: date("2025-06-30"),
    }
}

fn request(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    base: ScalarValueSpec,
    offset: ScalarValueSpec,
) -> ExecutionRequest {
    let period = period();
    ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: [("base_date", base), ("offset", offset)]
                .into_iter()
                .map(|(name, value)| InputRecordSpec {
                    name: name.to_string(),
                    entity: "Person".to_string(),
                    entity_id: "p".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value,
                })
                .collect(),
            ..Default::default()
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "p".to_string(),
            period,
            outputs: vec!["shifted_date".to_string()],
        }],
    }
}

#[test]
fn calendar_shifts_match_exact_dates_in_explain_fast_fallback_and_dense() {
    for (function, cases) in [
        (
            "date_add_months",
            vec![
                ("2025-01-31", 1, "2025-02-28"),
                ("2024-01-31", 1, "2024-02-29"),
                ("2025-03-31", -1, "2025-02-28"),
                ("2025-08-31", 6, "2026-02-28"),
                ("2025-01-31", 2, "2025-03-31"),
                ("2025-01-15", -13, "2023-12-15"),
                ("2025-01-31", 0, "2025-01-31"),
            ],
        ),
        (
            "date_add_years",
            vec![
                ("2024-02-29", 1, "2025-02-28"),
                ("2024-02-29", 4, "2028-02-29"),
                ("2000-02-29", 100, "2100-02-28"),
                ("2000-02-29", -100, "1900-02-28"),
                ("2025-06-01", -18, "2007-06-01"),
                ("2024-02-29", 0, "2024-02-29"),
            ],
        ),
    ] {
        let artifact = artifact(function);
        let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).unwrap();
        for (base, offset, expected) in cases {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let response = execute_request(request(
                    &artifact,
                    mode,
                    ScalarValueSpec::Date { value: date(base) },
                    ScalarValueSpec::Integer { value: offset },
                ))
                .unwrap();
                match &response.results[0].outputs["shifted_date"] {
                    OutputValue::Scalar {
                        value: ScalarValueSpec::Date { value },
                        ..
                    } => assert_eq!(*value, date(expected), "{function}({base}, {offset})"),
                    other => panic!("unexpected output {other:?}"),
                }
            }
            let response = dense
                .execute(
                    &period().to_model().unwrap(),
                    DenseBatchSpec {
                        row_count: 1,
                        inputs: HashMap::from([
                            ("base_date".to_string(), DenseColumn::Date(vec![date(base)])),
                            ("offset".to_string(), DenseColumn::Integer(vec![offset])),
                        ]),
                        relations: HashMap::new(),
                    },
                    &["shifted_date".to_string()],
                )
                .unwrap();
            match &response.outputs["shifted_date"] {
                DenseOutputValue::Scalar(DenseColumn::Date(values)) => {
                    assert_eq!(values, &[date(expected)])
                }
                other => panic!("unexpected dense output {other:?}"),
            }
        }
        // The new expression variants survive serde artifact roundtrips.
        let serialized = serde_json::to_string(&artifact.program).unwrap();
        let roundtrip = serde_json::from_str(&serialized).unwrap();
        CompiledProgramArtifact::compile(roundtrip).unwrap();
    }
}

#[test]
fn calendar_shifts_reject_fractional_offsets_and_overflow_without_panicking() {
    for function in ["date_add_months", "date_add_years"] {
        let artifact = artifact(function);
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            for (base, offset) in [
                (
                    date("2025-01-31"),
                    ScalarValueSpec::Decimal {
                        value: "0.5".to_string(),
                    },
                ),
                (
                    date("2025-01-31"),
                    ScalarValueSpec::Integer { value: i64::MAX },
                ),
                (
                    date("2025-01-31"),
                    ScalarValueSpec::Integer { value: i64::MIN },
                ),
                (NaiveDate::MAX, ScalarValueSpec::Integer { value: 1 }),
                (NaiveDate::MIN, ScalarValueSpec::Integer { value: -1 }),
            ] {
                assert!(
                    execute_request(request(
                        &artifact,
                        mode.clone(),
                        ScalarValueSpec::Date { value: base },
                        offset
                    ))
                    .is_err()
                );
            }
        }
        let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).unwrap();
        for offset in [i64::MIN, i64::MAX] {
            assert!(
                dense
                    .execute(
                        &period().to_model().unwrap(),
                        DenseBatchSpec {
                            row_count: 1,
                            inputs: HashMap::from([
                                (
                                    "base_date".to_string(),
                                    DenseColumn::Date(vec![date("2025-01-31")])
                                ),
                                ("offset".to_string(), DenseColumn::Integer(vec![offset])),
                            ]),
                            relations: HashMap::new(),
                        },
                        &["shifted_date".to_string()]
                    )
                    .is_err()
            );
        }
    }
}

#[test]
fn calendar_shifts_inside_related_rules_match_explain_and_dense() {
    use axiom_rules_engine::dense::{DenseRelationBatchSpec, DenseRelationKey};
    use axiom_rules_engine::spec::RelationRecordSpec;
    for (function, offset) in [("date_add_months", 12), ("date_add_years", 1)] {
        let source = format!(
            r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: shifted_date
    kind: derived
    entity: Person
    dtype: Date
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: {function}(base_date, {offset})
  - name: shifted_day_count
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: days_between(base_date, shifted_date)
  - name: family_day_count
    kind: derived
    entity: Family
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: sum(member_of_family.shifted_day_count)
"#
        );
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
        let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
        let interval = IntervalSpec {
            start: period().start,
            end: period().end,
        };
        let dates = [date("2024-02-29"), date("2025-01-31")];
        let dataset = DatasetSpec {
            inputs: dates
                .iter()
                .enumerate()
                .map(|(i, value)| InputRecordSpec {
                    name: "base_date".into(),
                    entity: "Person".into(),
                    entity_id: format!("p{i}"),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Date { value: *value },
                })
                .collect(),
            relations: (0..2)
                .map(|i| RelationRecordSpec {
                    name: "member_of_family".into(),
                    tuple: vec![format!("p{i}"), "f".into()],
                    interval: interval.clone(),
                })
                .collect(),
            ..Default::default()
        };
        let response = execute_request(ExecutionRequest {
            mode: ExecutionMode::Explain,
            program: artifact.program.clone(),
            dataset,
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "f".into(),
                period: period(),
                outputs: vec!["family_day_count".into()],
            }],
        })
        .unwrap();
        match &response.results[0].outputs["family_day_count"] {
            OutputValue::Scalar {
                value: ScalarValueSpec::Decimal { value },
                ..
            } => assert_eq!(value, "730"),
            other => panic!("unexpected related output {other:?}"),
        }
        let response = dense
            .execute(
                &period().to_model().unwrap(),
                DenseBatchSpec {
                    row_count: 2,
                    inputs: HashMap::new(),
                    relations: HashMap::from([(
                        DenseRelationKey {
                            name: "member_of_family".into(),
                            current_slot: 1,
                            related_slot: 0,
                        },
                        DenseRelationBatchSpec {
                            offsets: vec![0, 2, 2],
                            inputs: HashMap::from([(
                                "base_date".into(),
                                DenseColumn::Date(dates.to_vec()),
                            )]),
                        },
                    )]),
                },
                &["family_day_count".into()],
            )
            .unwrap();
        match &response.outputs["family_day_count"] {
            DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                assert_eq!(
                    values.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    ["730", "0"]
                );
            }
            other => panic!("unexpected related dense output {other:?}"),
        }
    }
}
