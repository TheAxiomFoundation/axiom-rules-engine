use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionRequest,
    ExecutionResponse, OutputValue, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::engine::EvalError;
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

const TYPED_RELATION_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: person_marker
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: person_value
  - name: household_marker
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: household_value
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
fn explain_and_fast_are_differentially_equivalent_on_generated_programs() {
    // Deterministic property-style coverage without a random dependency. Each
    // seed varies the arithmetic program, the two overlapping input values,
    // and their dataset order. The newer spell must win in both modes.
    for seed in 0_u64..128 {
        let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        };
        let operation = next() % 4;
        let literal = i64::try_from(next() % 9 + 1).expect("small generated literal");
        let newer_value = i64::try_from(next() % 2_000 + 1).expect("small generated value");
        let older_value =
            newer_value + i64::try_from(next() % 2_000 + 1).expect("small generated delta");
        let newer_first = next() % 2 == 0;

        let expression = match operation {
            0 => ScalarExprSpec::Add {
                items: vec![
                    ScalarExprSpec::Input {
                        name: "amount".to_string(),
                    },
                    decimal_literal(literal),
                ],
            },
            1 => ScalarExprSpec::Sub {
                left: Box::new(ScalarExprSpec::Input {
                    name: "amount".to_string(),
                }),
                right: Box::new(decimal_literal(literal)),
            },
            2 => ScalarExprSpec::Mul {
                left: Box::new(ScalarExprSpec::Input {
                    name: "amount".to_string(),
                }),
                right: Box::new(decimal_literal(literal)),
            },
            _ => ScalarExprSpec::Div {
                left: Box::new(ScalarExprSpec::Input {
                    name: "amount".to_string(),
                }),
                right: Box::new(decimal_literal(literal)),
            },
        };
        let (program, dataset, query) =
            generated_overlap_case(expression, newer_value, older_value, newer_first);

        let explain = execute_request(ExecutionRequest {
            mode: ExecutionMode::Explain,
            program: program.clone(),
            dataset: dataset.clone(),
            queries: vec![query.clone()],
        })
        .expect("generated Explain request succeeds");
        let fast = execute_request(ExecutionRequest {
            mode: ExecutionMode::Fast,
            program,
            dataset,
            queries: vec![query],
        })
        .expect("generated Fast request succeeds");

        assert_eq!(explain.metadata.actual_mode, ExecutionMode::Explain);
        assert_eq!(
            fast.metadata.actual_mode,
            ExecutionMode::Fast,
            "seed {seed} unexpectedly fell back: {:?}",
            fast.metadata.fallback_reason
        );
        assert_eq!(
            serde_json::to_value(&explain.results[0].outputs).expect("Explain outputs serialise"),
            serde_json::to_value(&fast.results[0].outputs).expect("Fast outputs serialise"),
            "execution modes diverged for generated seed {seed}"
        );
    }
}

#[test]
fn overlapping_covering_inputs_use_latest_start_in_every_mode_and_order() {
    let expression = ScalarExprSpec::If {
        condition: Box::new(axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
            left: Box::new(ScalarExprSpec::Input {
                name: "amount".to_string(),
            }),
            op: ComparisonOpSpec::Gt,
            right: Box::new(decimal_literal(3_000)),
        }),
        then_expr: Box::new(decimal_literal(0)),
        else_expr: Box::new(decimal_literal(650)),
    };

    for newer_first in [true, false] {
        let (program, dataset, query) =
            generated_overlap_case(expression.clone(), 2_000, 4_000, newer_first);
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let response = execute_request(ExecutionRequest {
                mode: mode.clone(),
                program: program.clone(),
                dataset: dataset.clone(),
                queries: vec![query.clone()],
            })
            .expect("overlapping-input request succeeds");

            assert_eq!(response.metadata.actual_mode, mode);
            assert_eq!(
                decimal_output(
                    response.results[0]
                        .outputs
                        .get("benefit")
                        .expect("benefit output")
                ),
                decimal("650"),
                "latest-start input did not win with newer_first={newer_first}"
            );
        }
    }
}

#[test]
fn equal_start_conflicting_inputs_are_ambiguous_in_every_mode_and_order() {
    let expression = ScalarExprSpec::Input {
        name: "amount".to_string(),
    };

    for newer_first in [true, false] {
        let (program, mut dataset, query) =
            generated_overlap_case(expression.clone(), 2_000, 4_000, newer_first);
        // Give both conflicting records equal precedence while leaving their
        // ends different. Dataset order and interval length are not authority
        // to choose one asserted fact over another.
        dataset.inputs[0].interval.start =
            chrono::NaiveDate::from_ymd_opt(2025, 7, 1).expect("valid date");
        dataset.inputs[1].interval.start =
            chrono::NaiveDate::from_ymd_opt(2025, 7, 1).expect("valid date");
        dataset.inputs[0].interval.end =
            chrono::NaiveDate::from_ymd_opt(2027, 12, 31).expect("valid date");

        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let error = execute_request(ExecutionRequest {
                mode,
                program: program.clone(),
                dataset: dataset.clone(),
                queries: vec![query.clone()],
            })
            .expect_err("equal-precedence conflicting facts must be rejected");

            assert!(
                matches!(
                    error,
                    ApiError::Eval(EvalError::AmbiguousInput {
                        ref name,
                        ref entity_id,
                        effective_from,
                    }) if name == "amount"
                        && entity_id == "household-1"
                        && effective_from
                            == chrono::NaiveDate::from_ymd_opt(2025, 7, 1)
                                .expect("valid date")
                ),
                "unexpected ambiguity error: {error}"
            );
        }
    }
}

#[test]
fn newer_non_covering_input_does_not_displace_older_covering_input() {
    let expression = ScalarExprSpec::Input {
        name: "amount".to_string(),
    };
    let (program, mut dataset, query) = generated_overlap_case(expression, 2_000, 4_000, true);
    dataset.inputs[0].interval.start =
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15).expect("valid date");

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: vec![query.clone()],
        })
        .expect("request with a non-covering newer spell succeeds");

        assert_eq!(response.metadata.actual_mode, mode);
        assert_eq!(
            decimal_output(
                response.results[0]
                    .outputs
                    .get("benefit")
                    .expect("benefit output")
            ),
            decimal("4000")
        );
    }
}

#[test]
fn related_inputs_use_latest_covering_start_in_every_mode_and_order() {
    let period = simple_period();
    let program = ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![DerivedSpec {
            id: None,
            name: "household_amount".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Decimal,
            unit: None,
            rounding: None,
            source: None,
            period: None,
            source_url: None,
            corpus_citation_path: None,
            semantics: DerivedSemanticsSpec::Scalar {
                expr: ScalarExprSpec::SumRelated {
                    relation: "member_of_household".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    value: RelatedValueRefSpec::Input {
                        name: "amount".to_string(),
                    },
                    where_clause: None,
                },
            },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let newer = InputRecordSpec {
        name: "amount".to_string(),
        entity: "Person".to_string(),
        entity_id: "person-1".to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2025, 7, 1).expect("valid date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("valid date"),
        },
        value: decimal_value("2000"),
    };
    let older = InputRecordSpec {
        name: "amount".to_string(),
        entity: "Person".to_string(),
        entity_id: "person-1".to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("valid date"),
        },
        value: decimal_value("4000"),
    };
    let relation = RelationRecordSpec {
        name: "member_of_household".to_string(),
        tuple: vec!["person-1".to_string(), "household-1".to_string()],
        interval: IntervalSpec {
            start: period.start,
            end: period.end,
        },
    };
    let query = ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: vec!["household_amount".to_string()],
    };

    for inputs in [vec![newer.clone(), older.clone()], vec![older, newer]] {
        let dataset = DatasetSpec {
            inputs,
            relations: vec![relation.clone()],
        };
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let response = execute_request(ExecutionRequest {
                mode: mode.clone(),
                program: program.clone(),
                dataset: dataset.clone(),
                queries: vec![query.clone()],
            })
            .expect("related-input request succeeds");

            assert_eq!(response.metadata.actual_mode, mode);
            assert_eq!(
                decimal_output(
                    response.results[0]
                        .outputs
                        .get("household_amount")
                        .expect("household amount output")
                ),
                decimal("2000")
            );
        }
    }
}

#[test]
fn explain_trace_closes_short_circuited_dependency_edges() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: gate_1
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_1_value
  - name: gate_2
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_2_value
  - name: gate_3
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_3_value
  - name: gate_4
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_4_value
  - name: gate_5
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_5_value
  - name: gate_6
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_6_value
  - name: gate_7
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_7_value
  - name: snap_eligible
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: gate_1 and gate_2 and gate_3 and gate_4 and gate_5 and gate_6 and gate_7
"#;
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let dataset = DatasetSpec {
        inputs: [
            ("gate_1_value", true),
            ("gate_2_value", true),
            ("gate_3_value", false),
        ]
        .into_iter()
        .map(|(name, value)| InputRecordSpec {
            name: name.to_string(),
            entity: "Household".to_string(),
            entity_id: "household-1".to_string(),
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
            value: ScalarValueSpec::Bool { value },
        })
        .collect(),
        relations: vec![],
    };
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["snap_eligible".to_string()],
        }],
    })
    .expect("short-circuited request succeeds without skipped inputs");
    let result = &response.results[0];
    assert_eq!(
        judgment_output(
            result
                .outputs
                .get("snap_eligible")
                .expect("eligibility output")
        ),
        JudgmentOutcomeSpec::NotHolds
    );

    let node = serde_json::to_value(
        result
            .trace
            .get("snap_eligible")
            .expect("eligibility trace node"),
    )
    .expect("trace node serialises");
    let actual = node["dependencies"]
        .as_array()
        .expect("actual dependencies are an array");
    assert_eq!(actual.len(), 3, "only traversed gates are actual edges");
    for dependency in actual {
        let dependency = dependency.as_str().expect("dependency key is text");
        assert!(
            result.trace.contains_key(dependency),
            "actual dependency `{dependency}` must resolve to a trace node"
        );
    }
    let skipped = node["not_evaluated_dependencies"]
        .as_array()
        .expect("skipped dependency edges are explicit");
    assert_eq!(skipped.len(), 4);
    assert!(skipped.iter().all(|dependency| {
        dependency["reason"]
            .as_str()
            .is_some_and(|reason| reason == "short_circuit")
    }));
}

#[test]
fn trace_skip_status_is_parent_specific_even_when_child_is_cached() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: first_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: false
  - name: cached_but_skipped_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: true
  - name: eligible
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: first_gate and cached_but_skipped_gate
"#;
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            // Warm the would-be skipped child before evaluating its parent.
            outputs: vec![
                "cached_but_skipped_gate".to_string(),
                "eligible".to_string(),
            ],
        }],
    })
    .expect("request succeeds");
    assert!(
        response.results[0]
            .trace
            .contains_key("cached_but_skipped_gate"),
        "separately requested child has an evaluated node"
    );

    let parent = serde_json::to_value(
        response.results[0]
            .trace
            .get("eligible")
            .expect("parent trace node"),
    )
    .expect("trace node serialises");
    assert_eq!(
        parent["dependencies"],
        serde_json::json!(["first_gate"]),
        "a warm cache must not turn an untraversed parent edge into an actual edge"
    );
    assert_eq!(
        parent["not_evaluated_dependencies"][0]["dependency"],
        "cached_but_skipped_gate"
    );
}

#[test]
fn trace_execution_projection_uses_selected_parameter_not_authored_source() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: weeks_per_year
    kind: parameter
    dtype: Number
    source: 52 benefit weeks
    versions:
      - effective_from: 2026-01-01
        formula: 52
  - name: weekly_value
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    versions:
      - effective_from: 2026-01-01
        formula: supplied_weekly_value
  - name: annual_entitlement
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    source: Authored prose incorrectly says payment spans 365/7 calendar weeks.
    versions:
      - effective_from: 2026-01-01
        formula: weekly_value * weeks_per_year
"#;
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "supplied_weekly_value".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: decimal_value("8.50"),
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "person-1".to_string(),
            period,
            outputs: vec!["annual_entitlement".to_string()],
        }],
    })
    .expect("annual entitlement request succeeds");
    assert_eq!(
        decimal_output(
            response.results[0]
                .outputs
                .get("annual_entitlement")
                .expect("annual entitlement output")
        ),
        decimal("442")
    );

    let node = serde_json::to_value(
        response.results[0]
            .trace
            .get("annual_entitlement")
            .expect("annual entitlement trace node"),
    )
    .expect("trace node serialises");
    let executed_expression = node["executed_expression"]
        .as_str()
        .expect("executed expression is present");
    assert!(executed_expression.contains("weekly_value"));
    assert!(executed_expression.contains("weeks_per_year"));
    assert!(
        !executed_expression.contains("365/7"),
        "authored prose cannot become the calculation narrative"
    );
    let reads = node["parameter_reads"]
        .as_array()
        .expect("parameter reads are present");
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["parameter"], "weeks_per_year");
    assert_eq!(reads[0]["index"], 0);
    assert_eq!(reads[0]["value"]["value"], serde_json::json!(52));
}

#[test]
fn trace_records_or_and_if_skips_with_closed_actual_edges() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: true_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: true
  - name: skipped_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: missing_gate_input
  - name: any_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: true_gate or skipped_gate
  - name: selected_amount
    kind: derived
    entity: Household
    dtype: Number
    versions:
      - effective_from: 2026-01-01
        formula: 10
  - name: skipped_amount
    kind: derived
    entity: Household
    dtype: Number
    versions:
      - effective_from: 2026-01-01
        formula: missing_amount
  - name: conditional_amount
    kind: derived
    entity: Household
    dtype: Number
    versions:
      - effective_from: 2026-01-01
        formula: "if true_gate: selected_amount else: skipped_amount"
"#;
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["any_gate".to_string(), "conditional_amount".to_string()],
        }],
    })
    .expect("unselected branches do not require their missing inputs");
    let trace = &response.results[0].trace;

    let or_node =
        serde_json::to_value(trace.get("any_gate").expect("Or trace node")).expect("serialises");
    assert_eq!(or_node["dependencies"], serde_json::json!(["true_gate"]));
    assert_eq!(
        or_node["not_evaluated_dependencies"],
        serde_json::json!([{
            "dependency": "skipped_gate",
            "reason": "short_circuit"
        }])
    );

    let if_node = serde_json::to_value(
        trace
            .get("conditional_amount")
            .expect("conditional trace node"),
    )
    .expect("serialises");
    assert_eq!(
        if_node["dependencies"],
        serde_json::json!(["true_gate", "selected_amount"])
    );
    assert_eq!(
        if_node["not_evaluated_dependencies"],
        serde_json::json!([{
            "dependency": "skipped_amount",
            "reason": "branch_not_selected"
        }])
    );

    for (key, node) in trace {
        let node = serde_json::to_value(node).expect("trace node serialises");
        for dependency in node["dependencies"]
            .as_array()
            .expect("dependencies are an array")
        {
            let dependency = dependency.as_str().expect("dependency key is text");
            assert!(
                trace.contains_key(dependency),
                "trace node `{key}` has dangling dependency `{dependency}`"
            );
        }
    }
}

#[test]
fn trace_closure_includes_related_entity_instances() {
    let period = simple_period();
    let program = ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![
            DerivedSpec {
                id: None,
                name: "person_amount".to_string(),
                entity: "Person".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
                rounding: None,
                source: None,
                period: None,
                source_url: None,
                corpus_citation_path: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::Input {
                        name: "amount".to_string(),
                    },
                },
                versions: vec![],
            },
            DerivedSpec {
                id: None,
                name: "household_amount".to_string(),
                entity: "Household".to_string(),
                dtype: DTypeSpec::Decimal,
                unit: None,
                rounding: None,
                source: None,
                period: None,
                source_url: None,
                corpus_citation_path: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::SumRelated {
                        relation: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        value: RelatedValueRefSpec::Derived {
                            name: "person_amount".to_string(),
                        },
                        where_clause: None,
                    },
                },
                versions: vec![],
            },
        ],
        ..ProgramSpec::default()
    };
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "amount".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: decimal_value("25"),
            }],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["household_amount".to_string()],
        }],
    })
    .expect("related derived request succeeds");
    let trace = &response.results[0].trace;
    let parent = serde_json::to_value(trace.get("household_amount").expect("parent trace node"))
        .expect("serialises");
    let dependency = parent["dependencies"][0]
        .as_str()
        .expect("related instance dependency key");
    assert!(
        trace.contains_key(dependency),
        "related instance edge must resolve"
    );
    let child = serde_json::to_value(trace.get(dependency).expect("related child trace node"))
        .expect("serialises");
    assert_eq!(child["name"], "person_amount");
    assert_eq!(child["entity_id"], "person-1");
}

#[test]
fn old_trace_json_deserializes_without_execution_projection_fields() {
    let old_wire_shape = serde_json::json!({
        "kind": "judgment",
        "name": "eligible",
        "id": null,
        "unit": null,
        "outcome": "holds",
        "source": null,
        "source_url": null,
        "dependencies": []
    });
    let node: axiom_rules_engine::api::DerivedTraceNode =
        serde_json::from_value(old_wire_shape).expect("old trace JSON remains readable");
    assert!(matches!(
        node,
        axiom_rules_engine::api::DerivedTraceNode::Judgment { .. }
    ));
}

#[test]
fn trace_excludes_cached_nodes_from_prior_query_entities() {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let response = execute_request(simple_execution_request(ExecutionMode::Explain, program))
        .expect("multi-entity Explain request succeeds");

    assert_eq!(response.results.len(), 2);
    for result in &response.results {
        assert_eq!(
            result.trace.len(),
            1,
            "a query trace must not include nodes cached while evaluating another entity"
        );
        let node = serde_json::to_value(
            result
                .trace
                .get("adjusted_amount")
                .expect("current entity trace node"),
        )
        .expect("trace node serialises");
        assert_eq!(node["entity_id"], result.entity_id);
        assert!(
            result.trace.keys().all(|key| !key.contains("@entity:")),
            "unrelated cached entity instances must not leak into this query trace"
        );
    }
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
            corpus_citation_path: None,
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
            corpus_citation_path: None,
            semantics: true_semantics.clone(),
            versions: vec![
                DerivedVersionSpec {
                    effective_from: chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                        .expect("valid date"),
                    effective_to: None,
                    semantics: false_semantics,
                },
                DerivedVersionSpec {
                    effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                        .expect("valid date"),
                    effective_to: None,
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
fn parameter_versions_expire_and_leave_gaps_in_all_execution_modes() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: bounded_rate
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        effective_to: 2025-12-31
        formula: "1"
      - effective_from: 2027-01-01
        effective_to: 2027-12-31
        formula: "3"
  - name: amount_using_bounded_rate
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2020-01-01
        formula: bounded_rate
"#;
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect("bounded parameter RuleSpec lowers");

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        assert_eq!(
            integer_result(
                &program,
                mode.clone(),
                2025,
                12,
                31,
                "amount_using_bounded_rate"
            )
            .expect("effective_to is inclusive"),
            1
        );
        assert!(matches!(
            integer_result(&program, mode.clone(), 2026, 1, 1, "amount_using_bounded_rate"),
            Err(ApiError::Eval(EvalError::MissingParameterValue { parameter, .. }))
                if parameter == "bounded_rate"
        ));
        assert_eq!(
            integer_result(
                &program,
                mode.clone(),
                2027,
                1,
                1,
                "amount_using_bounded_rate"
            )
            .expect("later parameter version begins after the gap"),
            3
        );
        assert!(matches!(
            integer_result(&program, mode, 2028, 1, 1, "amount_using_bounded_rate"),
            Err(ApiError::Eval(EvalError::MissingParameterValue { parameter, .. }))
                if parameter == "bounded_rate"
        ));
    }
}

#[test]
fn derived_versions_expire_and_leave_gaps_in_all_execution_modes() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: bounded_amount
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2025-01-01
        effective_to: 2025-12-31
        formula: "1"
      - effective_from: 2027-01-01
        effective_to: 2027-12-31
        formula: "3"
"#;
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect("bounded derived RuleSpec lowers");

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        assert_eq!(
            integer_result(&program, mode.clone(), 2025, 12, 31, "bounded_amount")
                .expect("effective_to is inclusive"),
            1
        );
        assert!(matches!(
            integer_result(&program, mode.clone(), 2026, 1, 1, "bounded_amount"),
            Err(ApiError::Eval(EvalError::MissingDerivedFormulaVersion { derived, .. }))
                if derived == "bounded_amount"
        ));
        assert_eq!(
            integer_result(&program, mode.clone(), 2027, 1, 1, "bounded_amount")
                .expect("later derived version begins after the gap"),
            3
        );
        assert!(matches!(
            integer_result(&program, mode, 2028, 1, 1, "bounded_amount"),
            Err(ApiError::Eval(EvalError::MissingDerivedFormulaVersion { derived, .. }))
                if derived == "bounded_amount"
        ));
    }
}

#[test]
fn single_unbounded_derived_version_does_not_apply_before_its_effective_date() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: dated_belgian_amount
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2022-01-01
        formula: "17"
"#;
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect("single-version derived RuleSpec lowers");

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        assert!(matches!(
            integer_result(
                &program,
                mode.clone(),
                2019,
                1,
                1,
                "dated_belgian_amount"
            ),
            Err(ApiError::Eval(EvalError::MissingDerivedFormulaVersion { derived, .. }))
                if derived == "dated_belgian_amount"
        ));
        assert_eq!(
            integer_result(&program, mode, 2022, 1, 1, "dated_belgian_amount")
                .expect("derived applies on its effective date"),
            17
        );
    }
}

#[test]
fn exhaustive_match_uses_wildcard_only_for_unmatched_subjects() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: filing_credit
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2026-01-01
        formula: |
          match filing_status:
              "single" => 10
              "joint" => 20
              _ => 99
"#;
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect("exhaustive match lowers");
    let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Custom {
            name: "Day".to_string(),
        },
        start: date,
        end: date,
    };

    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode,
            program: program.clone(),
            dataset: DatasetSpec {
                inputs: [
                    ("single-filer", "single"),
                    ("joint-filer", "joint"),
                    ("widowed-filer", "widowed"),
                ]
                .into_iter()
                .map(|(entity_id, filing_status)| InputRecordSpec {
                    name: "filing_status".to_string(),
                    entity: "TaxUnit".to_string(),
                    entity_id: entity_id.to_string(),
                    interval: IntervalSpec {
                        start: date,
                        end: date,
                    },
                    value: ScalarValueSpec::Text {
                        value: filing_status.to_string(),
                    },
                })
                .collect(),
                relations: Vec::new(),
            },
            queries: ["single-filer", "joint-filer", "widowed-filer"]
                .into_iter()
                .map(|entity_id| ExecutionQuery {
                    assessment_date: None,
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                    outputs: vec!["filing_credit".to_string()],
                })
                .collect(),
        })
        .expect("match executes");

        let values = response
            .results
            .iter()
            .map(|result| {
                integer_output(
                    result
                        .outputs
                        .get("filing_credit")
                        .expect("filing_credit output"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![10, 20, 99]);
    }
}

fn integer_result(
    program: &ProgramSpec,
    mode: ExecutionMode,
    year: i32,
    month: u32,
    day: u32,
    output: &str,
) -> Result<i64, ApiError> {
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day).expect("valid test date");
    let response = execute_request(ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period: PeriodSpec {
                kind: PeriodKindSpec::Custom {
                    name: "Day".to_string(),
                },
                start: date,
                end: date,
            },
            outputs: vec![output.to_string()],
        }],
    })?;
    let value = response.results[0]
        .outputs
        .get(output)
        .expect("requested output is returned");
    match value {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => Ok(*value),
        other => panic!("expected integer output, got {other:?}"),
    }
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
            slot_entities: Vec::new(),
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
                corpus_citation_path: None,
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
                corpus_citation_path: None,
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
            slot_entities: Vec::new(),
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
            corpus_citation_path: None,
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
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-compile-test-{}",
            std::process::id()
        ));
    let rulespec_root = temp_root.join("rulespec-us");
    let program_path = rulespec_root.join("us/policies/tests/simple.yaml");
    let artifact_path = temp_root.join("rules.compiled.json");
    std::fs::create_dir_all(program_path.parent().expect("program parent"))
        .expect("temp dir created");
    std::fs::write(&program_path, SIMPLE_RULESPEC).expect("RuleSpec module written");

    let compile_output = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([
            "compile",
            "--program",
            program_path.to_str().expect("utf8 path"),
            "--rulespec-root",
            rulespec_root.to_str().expect("utf8 root"),
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
    let mut dataset = simple_dataset(&period);
    for input in &mut dataset.inputs {
        input.name = "us:policies/tests/simple#input.amount".to_string();
    }
    let mut queries = simple_queries(&period);
    for query in &mut queries {
        query.outputs = vec!["us:policies/tests/simple#adjusted_amount".to_string()];
    }
    let request = CompiledExecutionRequest {
        mode: ExecutionMode::Fast,
        dataset,
        queries,
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
                .get("us:policies/tests/simple#adjusted_amount")
                .expect("adjusted amount output")
        ),
        decimal("25")
    );

    std::fs::remove_dir_all(temp_root).ok();
}

#[test]
fn run_compiled_emits_relation_slot_entity_warning_to_stderr() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(TYPED_RELATION_RULESPEC)
        .expect("typed relation RuleSpec compiles");
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-relation-warning-{}",
            std::process::id()
        ));
    std::fs::create_dir_all(&temp_root).expect("temp dir created");
    let artifact_path = temp_root.join("typed-relation.compiled.json");
    artifact
        .write_json_file(&artifact_path)
        .expect("artifact writes");

    let interval = IntervalSpec {
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-12-31".parse().expect("valid date"),
    };
    let request = CompiledExecutionRequest {
        mode: ExecutionMode::Explain,
        dataset: DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name: "person_value".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "person-1".to_string(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Integer { value: 1 },
                },
                InputRecordSpec {
                    name: "household_value".to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Integer { value: 1 },
                },
            ],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["household-1".to_string(), "person-1".to_string()],
                interval,
            }],
        },
        queries: Vec::new(),
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([
            "run-compiled",
            "--artifact",
            artifact_path.to_str().expect("utf8 path"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    serde_json::from_slice::<ExecutionResponse>(&output.stdout)
        .expect("warning does not contaminate JSON stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning[relation_slot_entity_mismatch]"),
        "{stderr}"
    );
    assert!(stderr.contains("member_of_household"), "{stderr}");
    assert!(stderr.contains("expected `Person`"), "{stderr}");
    assert!(stderr.contains("found `Household`"), "{stderr}");

    std::fs::remove_dir_all(temp_root).ok();
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

fn generated_overlap_case(
    expression: ScalarExprSpec,
    newer_value: i64,
    older_value: i64,
    newer_first: bool,
) -> (ProgramSpec, DatasetSpec, ExecutionQuery) {
    let period = simple_period();
    let newer = InputRecordSpec {
        name: "amount".to_string(),
        entity: "Household".to_string(),
        entity_id: "household-1".to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2025, 7, 1).expect("valid date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("valid date"),
        },
        value: decimal_value(&newer_value.to_string()),
    };
    let older = InputRecordSpec {
        name: "amount".to_string(),
        entity: "Household".to_string(),
        entity_id: "household-1".to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("valid date"),
        },
        value: decimal_value(&older_value.to_string()),
    };
    let inputs = if newer_first {
        vec![newer, older]
    } else {
        vec![older, newer]
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
            corpus_citation_path: None,
            semantics: DerivedSemanticsSpec::Scalar { expr: expression },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let query = ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: vec!["benefit".to_string()],
    };

    (
        program,
        DatasetSpec {
            inputs,
            relations: vec![],
        },
        query,
    )
}

fn decimal_literal(value: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: decimal_value(&value.to_string()),
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
