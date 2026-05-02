use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

use axiom_rules::api::{
    CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse,
    OutputValue, execute_compiled_request, execute_request,
};
use axiom_rules::compile::CompiledProgramArtifact;
use axiom_rules::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec, InputRecordSpec,
    IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    RelatedValueRefSpec, RelationRecordSpec, ScalarExprSpec, ScalarValueSpec,
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
    let program =
        axiom_rules::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("program fixture parses");
    let request = simple_execution_request(ExecutionMode::Fast, program);

    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules binary");

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
    let program =
        axiom_rules::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("program fixture parses");
    let period = simple_period();
    let queries = simple_queries(&period);
    let dataset = simple_dataset(&period);

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
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
            source: None,
            period: None,
            source_url: None,
            semantics: DerivedSemanticsSpec::Scalar {
                expr: ScalarExprSpec::If {
                    condition: Box::new(axiom_rules::spec::JudgmentExprSpec::Comparison {
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
        relations: vec![axiom_rules::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
        }],
        derived: vec![
            DerivedSpec {
                id: None,
                name: "person_income".to_string(),
                entity: "Person".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
                source: None,
                period: None,
                source_url: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::Input {
                        name: "income".to_string(),
                    },
                },
            },
            DerivedSpec {
                id: None,
                name: "household_income".to_string(),
                entity: "Household".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
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
        mode: ExecutionMode::Explain,
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
        relations: vec![axiom_rules::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
        }],
        derived: vec![DerivedSpec {
            id: None,
            name: "has_elderly_or_disabled_member".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Judgment,
            unit: None,
            source: None,
            period: None,
            source_url: None,
            semantics: DerivedSemanticsSpec::Judgment {
                expr: axiom_rules::spec::JudgmentExprSpec::Comparison {
                    left: Box::new(ScalarExprSpec::CountRelated {
                        relation: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        where_clause: Some(Box::new(
                            axiom_rules::spec::JudgmentExprSpec::Comparison {
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
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Explain);
    assert!(
        response
            .metadata
            .fallback_reason
            .as_deref()
            .unwrap_or_default()
            .contains("count_related where-clauses"),
        "unexpected fallback reason: {:?}",
        response.metadata.fallback_reason
    );
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
    let temp_root =
        std::env::temp_dir().join(format!("axiom-rules-compile-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp_root).expect("temp dir created");
    let program_path = temp_root.join("rules.yaml");
    let artifact_path = temp_root.join("rules.compiled.json");
    std::fs::write(&program_path, SIMPLE_RULESPEC).expect("RuleSpec module written");

    let compile_output = Command::new(env!("CARGO_BIN_EXE_axiom-rules"))
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules"))
        .args([
            "run-compiled",
            "--artifact",
            artifact_path.to_str().expect("utf8 path"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn axiom-rules binary");

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

fn judgment_output(output: &OutputValue) -> JudgmentOutcomeSpec {
    match output {
        OutputValue::Judgment { outcome, .. } => *outcome,
        other => panic!("expected judgment output, got {other:?}"),
    }
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal literal")
}
