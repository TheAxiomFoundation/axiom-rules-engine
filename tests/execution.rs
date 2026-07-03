use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

use axiom_rules_engine::api::{
    execute_compiled_request, execute_request, CompiledExecutionRequest, ExecutionMode,
    ExecutionQuery, ExecutionRequest, ExecutionResponse, OutputValue,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec,
    DerivedVersionSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec,
    PeriodSpec, ProgramSpec, RelatedValueRefSpec, RelationRecordSpec, ScalarExprSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;

const SIMPLE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount + base_amount
"#;

#[test]
fn cli_round_trip_returns_json() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let request = simple_execution_request(ExecutionMode::Fast, program);

    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules-engine binary");

    child
        .stdin
        .take()
        .expect("stdin available")
        .write_all(
            serde_json::to_string(&request)
                .expect("request serialises")
                .as_bytes(),
        )
        .expect("request written");

    let output = child.wait_with_output().expect("binary completes");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let response: ExecutionResponse =
        serde_json::from_slice(&output.stdout).expect("response parses");
    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(
        response.metadata.actual_mode,
        ExecutionMode::Fast,
        "unexpected fallback reason: {:?}",
        response.metadata.fallback_reason
    );
    let result = &response.results[0];
    assert_eq!(
        decimal_output(
            result
                .outputs
                .get("adjusted_amount")
                .expect("adjusted amount output")
        ),
        decimal("25")
    );
}

#[test]
fn fast_mode_matches_explain_mode_on_batch() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let period = simple_period();
    let queries = simple_queries(&period);
    let dataset = simple_dataset(&period);

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.clone(),
    })
    .expect("explain request succeeds");

    let fast = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries,
    })
    .expect("fast request succeeds");

    assert_eq!(fast.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.fallback_reason, None);
    // fast mode emits no trace; compare only primary outputs here.
    let explain_outputs: Vec<_> = explain
        .results
        .iter()
        .map(|result| {
            (
                result.entity_id.clone(),
                result.period.clone(),
                result.outputs.clone(),
            )
        })
        .collect();
    let fast_outputs: Vec<_> = fast
        .results
        .iter()
        .map(|result| {
            (
                result.entity_id.clone(),
                result.period.clone(),
                result.outputs.clone(),
            )
        })
        .collect();
    assert_eq!(
        serde_json::to_value(&explain_outputs).expect("explain outputs serialise"),
        serde_json::to_value(&fast_outputs).expect("fast outputs serialise")
    );
}

#[test]
fn fast_mode_coerces_integer_and_decimal_if_branches() {
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let program = ProgramSpec {
        derived: vec![DerivedSpec {
            id: None,
            name: "benefit".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Decimal,
            unit: None,
            rounding: None,
            source: None,
            period: None,
            source_url: None,
            semantics: DerivedSemanticsSpec::Scalar {
                expr: ScalarExprSpec::If {
                    condition: Box::new(axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Input {
                            name: "amount".to_string(),
                        }),
                        op: ComparisonOpSpec::Gt,
                        right: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Integer { value: 0 },
                        }),
                    }),
                    then_expr: Box::new(ScalarExprSpec::Input {
                        name: "amount".to_string(),
                    }),
                    else_expr: Box::new(ScalarExprSpec::Literal {
                        value: ScalarValueSpec::Integer { value: 0 },
                    }),
                },
            },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "amount".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval.clone(),
                value: decimal_value("12.5"),
            },
            InputRecordSpec {
                name: "amount".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-2".to_string(),
                interval,
                value: decimal_value("0"),
            },
        ],
        relations: vec![],
    };
    let queries = ["household-1", "household-2"]
        .into_iter()
        .map(|entity_id| ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: period.clone(),
            outputs: vec!["benefit".to_string()],
        })
        .collect();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries,
    })
    .expect("fast request succeeds");

    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("benefit")
                .expect("benefit output")
        ),
        decimal("12.5")
    );
    assert_eq!(
        decimal_output(
            response.results[1]
                .outputs
                .get("benefit")
                .expect("benefit output")
        ),
        decimal("0")
    );
}

#[test]
fn derived_formula_versions_select_by_query_period() {
    let false_semantics = DerivedSemanticsSpec::Judgment {
        expr: axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
            left: Box::new(ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value: 0 },
            }),
            op: ComparisonOpSpec::Eq,
            right: Box::new(ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value: 1 },
            }),
        },
    };
    let true_semantics = DerivedSemanticsSpec::Judgment {
        expr: axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
            left: Box::new(ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value: 1 },
            }),
            op: ComparisonOpSpec::Eq,
            right: Box::new(ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value: 1 },
            }),
        },
    };
    let program = ProgramSpec {
        derived: vec![DerivedSpec {
            id: None,
            name: "eligible".to_string(),
            entity: "Person".to_string(),
            dtype: DTypeSpec::Judgment,
            unit: None,
            rounding: None,
            source: None,
            period: None,
            source_url: None,
            semantics: true_semantics.clone(),
            versions: vec![
                DerivedVersionSpec {
                    effective_from: chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                        .expect("valid date"),
                    semantics: false_semantics,
                },
                DerivedVersionSpec {
                    effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                        .expect("valid date"),
                    semantics: true_semantics,
                },
            ],
        }],
        ..ProgramSpec::default()
    };
    let period_2024 = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2024, 1, 31).expect("valid date"),
    };
    let period_2026 = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };

    let response_2024 = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: program.clone(),
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "person-1".to_string(),
            period: period_2024,
            outputs: vec!["eligible".to_string()],
        }],
    })
    .expect("2024 versioned derived formula request succeeds");
    let response_2026 = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "person-1".to_string(),
            period: period_2026,
            outputs: vec!["eligible".to_string()],
        }],
    })
    .expect("2026 versioned derived formula request succeeds");

    assert_eq!(response_2024.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(response_2026.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        judgment_output(
            response_2024.results[0]
                .outputs
                .get("eligible")
                .expect("2024 output")
        ),
        JudgmentOutcomeSpec::NotHolds
    );
    assert_eq!(
        judgment_output(
            response_2026.results[0]
                .outputs
                .get("eligible")
                .expect("2026 output")
        ),
        JudgmentOutcomeSpec::Holds
    );
}

#[test]
fn fast_mode_falls_back_to_explain_when_bulk_support_is_missing() {
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let program = ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            derivation: None,
        }],
        derived: vec![
            DerivedSpec {
                id: None,
                name: "person_income".to_string(),
                entity: "Person".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
                rounding: None,
                source: None,
                period: None,
                source_url: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::Input {
                        name: "income".to_string(),
                    },
                },
                versions: vec![],
            },
            DerivedSpec {
                id: None,
                name: "household_income".to_string(),
                entity: "Household".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
                rounding: None,
                source: None,
                period: None,
                source_url: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::SumRelated {
                        relation: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        value: RelatedValueRefSpec::Derived {
                            name: "person_income".to_string(),
                        },
                        where_clause: None,
                    },
                },
                versions: vec![],
            },
        ],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: decimal_value("100"),
            },
            InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: decimal_value("50"),
            },
        ],
        relations: vec![
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-2".to_string(), "household-1".to_string()],
                interval,
            },
        ],
    };
    let queries = vec![ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: vec!["household_income".to_string()],
    }];

    let fast = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.clone(),
    })
    .expect("fast request succeeds");
    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries,
    })
    .expect("explain request succeeds");

    assert_eq!(fast.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
    assert!(
        fast.metadata
            .fallback_reason
            .as_deref()
            .unwrap_or_default()
            .contains("bulk execution does not yet support"),
        "unexpected fallback reason: {:?}",
        fast.metadata.fallback_reason
    );
    assert_eq!(
        serde_json::to_value(&fast.results).expect("fast results serialise"),
        serde_json::to_value(&explain.results).expect("explain results serialise")
    );
}

#[test]
fn fast_mode_falls_back_for_filtered_relation_counts() {
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let program = ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            derivation: None,
        }],
        derived: vec![DerivedSpec {
            id: None,
            name: "has_elderly_or_disabled_member".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Judgment,
            unit: None,
            rounding: None,
            source: None,
            period: None,
            source_url: None,
            semantics: DerivedSemanticsSpec::Judgment {
                expr: axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
                    left: Box::new(ScalarExprSpec::CountRelated {
                        relation: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        where_clause: Some(Box::new(
                            axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
                                left: Box::new(ScalarExprSpec::Input {
                                    name: "is_elderly_or_disabled".to_string(),
                                }),
                                op: ComparisonOpSpec::Eq,
                                right: Box::new(ScalarExprSpec::Literal {
                                    value: ScalarValueSpec::Bool { value: true },
                                }),
                            },
                        )),
                    }),
                    op: ComparisonOpSpec::Gt,
                    right: Box::new(ScalarExprSpec::Literal {
                        value: ScalarValueSpec::Integer { value: 0 },
                    }),
                },
            },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "is_elderly_or_disabled".to_string(),
            entity: "Person".to_string(),
            entity_id: "person-1".to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Bool { value: true },
        }],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["person-1".to_string(), "household-1".to_string()],
            interval,
        }],
    };
    let queries = vec![ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: vec!["has_elderly_or_disabled_member".to_string()],
    }];

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries,
    })
    .expect("fast request falls back");

    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.fallback_reason, None);
    assert_eq!(
        judgment_output(
            response.results[0]
                .outputs
                .get("has_elderly_or_disabled_member")
                .expect("elderly/disabled output")
        ),
        JudgmentOutcomeSpec::Holds
    );
}

#[test]
fn derived_relation_filters_structural_members_at_runtime() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn and not student_ineligible
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and snap_member_eligible
  - name: snap_unit_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(snap_unit)
  - name: snap_unit_income
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(snap_unit.income)
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let mut inputs = Vec::new();
    for (person, has_ssn, student_ineligible, income) in [
        ("person-1", true, false, "100"),
        ("person-2", false, false, "250"),
        ("person-3", true, true, "400"),
    ] {
        inputs.push(InputRecordSpec {
            name: "has_ssn".to_string(),
            entity: "Person".to_string(),
            entity_id: person.to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Bool { value: has_ssn },
        });
        inputs.push(InputRecordSpec {
            name: "student_ineligible".to_string(),
            entity: "Person".to_string(),
            entity_id: person.to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Bool {
                value: student_ineligible,
            },
        });
        inputs.push(InputRecordSpec {
            name: "income".to_string(),
            entity: "Person".to_string(),
            entity_id: person.to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Decimal {
                value: income.to_string(),
            },
        });
    }
    let dataset = DatasetSpec {
        inputs,
        relations: vec![
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-2".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-3".to_string(), "household-1".to_string()],
                interval,
            },
        ],
    };

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["snap_unit_size".to_string(), "snap_unit_income".to_string()],
        }],
    })
    .expect("request succeeds");

    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(
        response.metadata.actual_mode,
        ExecutionMode::Fast,
        "unexpected fallback reason: {:?}",
        response.metadata.fallback_reason
    );
    assert_eq!(response.metadata.fallback_reason, None);
    assert_eq!(
        integer_output(
            response.results[0]
                .outputs
                .get("snap_unit_size")
                .expect("snap unit size output")
        ),
        1
    );
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("snap_unit_income")
                .expect("snap unit income output")
        ),
        decimal("100")
    );
}

#[test]
fn filtered_entity_scope_aggregates_over_member_alias() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and snap_member_eligible
  - name: snap_unit_size
    kind: derived
    entity: SnapUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
  - name: snap_unit_income
    kind: derived
    entity: SnapUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(members.income)
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
            InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: "100".to_string(),
                },
            },
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: false },
            },
            InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: "500".to_string(),
                },
            },
        ],
        relations: vec![
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-2".to_string(), "household-1".to_string()],
                interval,
            },
        ],
    };

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["snap_unit_size".to_string(), "snap_unit_income".to_string()],
        }],
    })
    .expect("filtered entity request succeeds");

    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.fallback_reason, None);
    assert_eq!(
        integer_output(
            response.results[0]
                .outputs
                .get("snap_unit_size")
                .expect("snap unit size output")
        ),
        1
    );
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("snap_unit_income")
                .expect("snap unit income output")
        ),
        decimal("100")
    );
}

#[test]
fn derived_relation_membership_can_depend_on_current_entity_predicates() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_accepts_snap_members
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: snap_application_active
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and household_accepts_snap_members and snap_member_eligible
  - name: snap_unit_size
    kind: derived
    entity: SnapUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "snap_application_active".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
            InputRecordSpec {
                name: "snap_application_active".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: false },
            },
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
        ],
        relations: vec![
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-2".to_string(), "household-2".to_string()],
                interval,
            },
        ],
    };

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries: vec![
            ExecutionQuery {
                assessment_date: None,
                entity_id: "household-1".to_string(),
                period: period.clone(),
                outputs: vec!["snap_unit_size".to_string()],
            },
            ExecutionQuery {
                assessment_date: None,
                entity_id: "household-2".to_string(),
                period,
                outputs: vec!["snap_unit_size".to_string()],
            },
        ],
    })
    .expect("cross-scope derived relation request succeeds");

    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        integer_output(
            response.results[0]
                .outputs
                .get("snap_unit_size")
                .expect("first snap unit size output")
        ),
        1
    );
    assert_eq!(
        integer_output(
            response.results[1]
                .outputs
                .get("snap_unit_size")
                .expect("second snap unit size output")
        ),
        0
    );
}

#[test]
fn derived_relations_can_filter_other_derived_relations() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn
  - name: adult_member
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: snap_member_eligible
  - name: adult_snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: snap_unit
      entity: AdultSnapUnit
      member_relation: adult_members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: adult_member
  - name: adult_snap_unit_size
    kind: derived
    entity: AdultSnapUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(adult_members)
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: true },
            },
            InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-3".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: false },
            },
            InputRecordSpec {
                name: "age".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer { value: 30 },
            },
            InputRecordSpec {
                name: "age".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-2".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer { value: 12 },
            },
            InputRecordSpec {
                name: "age".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-3".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer { value: 40 },
            },
        ],
        relations: vec![
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-2".to_string(), "household-1".to_string()],
                interval: interval.clone(),
            },
            RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-3".to_string(), "household-1".to_string()],
                interval,
            },
        ],
    };

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program,
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["adult_snap_unit_size".to_string()],
        }],
    })
    .expect("composed derived relation request succeeds");

    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        integer_output(
            response.results[0]
                .outputs
                .get("adult_snap_unit_size")
                .expect("adult snap unit size output")
        ),
        1
    );
}

#[test]
fn compiled_program_artifact_round_trips_and_executes() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let period = simple_period();

    let response = execute_compiled_request(
        artifact,
        CompiledExecutionRequest {
            mode: ExecutionMode::Fast,
            dataset: simple_dataset(&period),
            queries: simple_queries(&period),
        },
    )
    .expect("compiled request succeeds");

    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("adjusted_amount")
                .expect("adjusted amount output")
        ),
        decimal("25")
    );
}

#[test]
fn cli_compile_and_run_compiled_round_trip() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-compile-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp_root).expect("temp dir created");
    let program_path = temp_root.join("rules.yaml");
    let artifact_path = temp_root.join("rules.compiled.json");
    std::fs::write(&program_path, SIMPLE_RULESPEC).expect("RuleSpec module written");

    let compile_output = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([
            "compile",
            "--program",
            program_path.to_str().expect("utf8 path"),
            "--output",
            artifact_path.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("compile command runs");

    assert!(
        compile_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&compile_output.stderr)
    );
    assert!(
        artifact_path.exists(),
        "compiled artefact should be written"
    );

    let period = simple_period();
    let request = CompiledExecutionRequest {
        mode: ExecutionMode::Fast,
        dataset: simple_dataset(&period),
        queries: simple_queries(&period),
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([
            "run-compiled",
            "--artifact",
            artifact_path.to_str().expect("utf8 path"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules-engine binary");

    child
        .stdin
        .take()
        .expect("stdin available")
        .write_all(
            serde_json::to_string(&request)
                .expect("request serialises")
                .as_bytes(),
        )
        .expect("request written");

    let output = child.wait_with_output().expect("binary completes");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let response: ExecutionResponse =
        serde_json::from_slice(&output.stdout).expect("response parses");
    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("adjusted_amount")
                .expect("adjusted amount output")
        ),
        decimal("25")
    );

    std::fs::remove_file(program_path).ok();
    std::fs::remove_file(artifact_path).ok();
    std::fs::remove_dir(temp_root).ok();
}

// `assessment_date` is a reserved bitemporal field (see docs/bitemporal.md):
// it is parsed, validated, and echoed, but must not affect evaluation yet.
#[test]
fn assessment_date_round_trips_and_evaluates_identically() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let period = simple_period();
    let assessment_date = chrono::NaiveDate::from_ymd_opt(2026, 3, 15).expect("valid date");

    let without = simple_execution_request(ExecutionMode::Explain, program.clone());
    let mut with = without.clone();
    for query in &mut with.queries {
        query.assessment_date = Some(assessment_date);
    }

    // Wire shape: the field is omitted when unset, so existing request JSON
    // is unchanged, and legacy JSON without the field still deserializes.
    let without_json = serde_json::to_value(&without).expect("request serialises");
    assert!(
        without_json["queries"][0].get("assessment_date").is_none(),
        "unset assessment_date must not appear on the wire"
    );
    let with_json = serde_json::to_string(&with).expect("request serialises");
    let reparsed: ExecutionRequest =
        serde_json::from_str(&with_json).expect("request with assessment_date parses");
    assert_eq!(reparsed.queries[0].assessment_date, Some(assessment_date));
    let legacy: ExecutionQuery = serde_json::from_value(without_json["queries"][0].clone())
        .expect("legacy query without assessment_date parses");
    assert_eq!(legacy.assessment_date, None);

    // Evaluation is identical with and without the field, in both modes.
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let mut without_request = without.clone();
        without_request.mode = mode.clone();
        let mut with_request = with.clone();
        with_request.mode = mode;

        let without_response =
            execute_request(without_request).expect("request without assessment_date succeeds");
        let with_response =
            execute_request(with_request).expect("request with assessment_date succeeds");

        assert_eq!(
            serde_json::to_value(&without_response.metadata).expect("metadata serialises"),
            serde_json::to_value(&with_response.metadata).expect("metadata serialises"),
        );
        assert_eq!(without_response.results.len(), with_response.results.len());
        for (without_result, with_result) in without_response
            .results
            .iter()
            .zip(with_response.results.iter())
        {
            assert_eq!(
                serde_json::to_value(&without_result.outputs).expect("outputs serialise"),
                serde_json::to_value(&with_result.outputs).expect("outputs serialise"),
            );
            assert_eq!(
                serde_json::to_value(&without_result.trace).expect("trace serialises"),
                serde_json::to_value(&with_result.trace).expect("trace serialises"),
            );
            // The response echoes the assessment the result was computed under.
            assert_eq!(without_result.assessment_date, None);
            assert_eq!(with_result.assessment_date, Some(assessment_date));
        }
    }

    // The compiled-request path accepts and echoes the field identically.
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let mut compiled_queries = simple_queries(&period);
    for query in &mut compiled_queries {
        query.assessment_date = Some(assessment_date);
    }
    let compiled_response = execute_compiled_request(
        artifact,
        CompiledExecutionRequest {
            mode: ExecutionMode::Fast,
            dataset: simple_dataset(&period),
            queries: compiled_queries,
        },
    )
    .expect("compiled request with assessment_date succeeds");
    assert_eq!(
        compiled_response.results[0].assessment_date,
        Some(assessment_date)
    );
    assert_eq!(
        decimal_output(
            compiled_response.results[0]
                .outputs
                .get("adjusted_amount")
                .expect("adjusted amount output")
        ),
        decimal("25")
    );

    // Boundary: an assessment on the first day of the period is allowed.
    let mut boundary = without.clone();
    for query in &mut boundary.queries {
        query.assessment_date = Some(period.start);
    }
    execute_request(boundary).expect("assessment on the period start date is valid");
}

#[test]
fn assessment_date_before_period_start_errors() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let before_period = chrono::NaiveDate::from_ymd_opt(2025, 12, 31).expect("valid date");

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let mut request = simple_execution_request(mode, program.clone());
        request.queries[1].assessment_date = Some(before_period);

        let error = execute_request(request)
            .expect_err("assessment_date before the period start must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("assessment_date 2025-12-31")
                && message.contains("period start 2026-01-01"),
            "unexpected error message: {message}"
        );
    }
}

fn simple_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    }
}

fn simple_dataset(period: &PeriodSpec) -> DatasetSpec {
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "amount".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval.clone(),
                value: decimal_value("15"),
            },
            InputRecordSpec {
                name: "amount".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-2".to_string(),
                interval,
                value: decimal_value("20"),
            },
        ],
        relations: Vec::new(),
    }
}

fn simple_queries(period: &PeriodSpec) -> Vec<ExecutionQuery> {
    ["household-1", "household-2"]
        .into_iter()
        .map(|entity_id| ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: period.clone(),
            outputs: vec!["adjusted_amount".to_string()],
        })
        .collect()
}

fn simple_execution_request(mode: ExecutionMode, program: ProgramSpec) -> ExecutionRequest {
    let period = simple_period();
    ExecutionRequest {
        mode,
        program,
        dataset: simple_dataset(&period),
        queries: simple_queries(&period),
    }
}

fn decimal_value(value: &str) -> ScalarValueSpec {
    ScalarValueSpec::Decimal {
        value: value.to_string(),
    }
}

fn decimal_output(output: &OutputValue) -> Decimal {
    match output {
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => decimal(value),
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => Decimal::from(*value),
        other => panic!("expected decimal scalar output, got {other:?}"),
    }
}

fn integer_output(output: &OutputValue) -> i64 {
    match output {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => *value,
        other => panic!("expected integer scalar output, got {other:?}"),
    }
}

fn judgment_output(output: &OutputValue) -> JudgmentOutcomeSpec {
    match output {
        OutputValue::Judgment { outcome, .. } => *outcome,
        other => panic!("expected judgment output, got {other:?}"),
    }
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal literal")
}
