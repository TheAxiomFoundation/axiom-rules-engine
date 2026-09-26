use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionRequest,
    ExecutionResponse, OutputValue, RulePin, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetBindingOptions, DatasetSpec, DerivedSemanticsSpec,
    DerivedSpec, DerivedVersionSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec,
    PeriodKindSpec, PeriodSpec, ProgramSpec, RelatedValueRefSpec, RelationRecordSpec,
    ScalarExprSpec, ScalarValueSpec,
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
    let queries: Vec<ExecutionQuery> = ["household-1", "household-2"]
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

    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    // Each row keeps the kind of the branch it selected, exactly as explain
    // reports it: the decimal input on the first row, the integer literal on
    // the second.
    assert_eq!(
        results_without_trace(&response),
        results_without_trace(&explain)
    );
    assert!(matches!(
        response.results[1].outputs.get("benefit"),
        Some(OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value: 0 },
            ..
        })
    ));
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

const NON_EXHAUSTIVE_MATCH_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: filing_credit
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match filing_status:
              1 => 10
              2 => 20
"#;

fn filing_status_request(mode: ExecutionMode, filers: &[(&str, i64)]) -> CompiledExecutionRequest {
    let period = simple_period();
    CompiledExecutionRequest {
        mode,
        dataset: DatasetSpec {
            inputs: filers
                .iter()
                .map(|(entity_id, filing_status)| InputRecordSpec {
                    name: "filing_status".to_string(),
                    entity: "TaxUnit".to_string(),
                    entity_id: entity_id.to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Integer {
                        value: *filing_status,
                    },
                })
                .collect(),
            relations: Vec::new(),
        },
        queries: filers
            .iter()
            .map(|(entity_id, _)| ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: period.clone(),
                outputs: vec!["filing_credit".to_string()],
            })
            .collect(),
        pins: Vec::new(),
    }
}

/// A `match` without `_` used to give an uncovered subject its last arm's
/// value (filing status 9 got the joint amount). It is now an error naming
/// the rule, the subject and its value, in explain and fast mode alike, while
/// covered subjects keep their arm's value. The artifact goes through its JSON
/// form, as a served artifact does.
#[test]
fn non_exhaustive_match_rejects_an_uncovered_subject_in_every_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(NON_EXHAUSTIVE_MATCH_RULESPEC)
        .expect("a match without `_` still compiles");
    let json = serde_json::to_string(&artifact).expect("artifact serialises");
    assert!(json.contains(r#""kind":"no_match""#), "{json}");
    let artifact: CompiledProgramArtifact =
        serde_json::from_str(&json).expect("artifact deserialises");

    let mut errors = Vec::new();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        // Rows take different arms, so the batch's innermost comparison
        // (`== 2`) fails for a row that matched the outer arm: that row is
        // covered and must not be reported.
        let covered = execute_compiled_request(
            artifact.clone(),
            filing_status_request(mode.clone(), &[("filer-1", 1), ("filer-2", 2)]),
        )
        .unwrap_or_else(|error| panic!("covered subjects fail in {mode:?}: {error}"));
        assert_eq!(covered.metadata.actual_mode, mode);
        let values: Vec<i64> = covered
            .results
            .iter()
            .map(|result| integer_output(&result.outputs["filing_credit"]))
            .collect();
        assert_eq!(values, vec![10, 20], "{mode:?}");

        let error = execute_compiled_request(
            artifact.clone(),
            filing_status_request(
                mode.clone(),
                &[("filer-1", 1), ("filer-9", 9), ("filer-2", 2)],
            ),
        )
        .expect_err("an uncovered subject is an error, not the last arm");
        assert!(
            matches!(
                &error,
                ApiError::Eval(EvalError::NoMatchingArm { rule, subject, value, patterns })
                    if rule == "filing_credit"
                        && subject == "filing_status"
                        && value == "9"
                        && patterns == "1, 2"
            ),
            "{mode:?}: {error:?}"
        );
        errors.push(error.to_string());
    }
    assert_eq!(errors[0], errors[1]);
    assert!(errors[0].contains("add an arm for it or a final `_ =>` arm"));
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
fn fast_mode_aggregates_related_derived_values_like_explain() {
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
        mode: ExecutionMode::Explain,
        program,
        dataset,
        queries,
    })
    .expect("explain request succeeds");

    // Aggregating a related derived value runs each row's aggregation on the
    // reference interpreter, so fast mode answers it without falling back.
    assert_eq!(fast.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.fallback_reason, None);
    assert_eq!(explain.metadata.actual_mode, ExecutionMode::Explain);
    assert_eq!(
        results_without_trace(&fast),
        results_without_trace(&explain)
    );
}

#[test]
fn fast_mode_falls_back_to_explain_when_bulk_support_is_missing() {
    let (program, dataset, queries) = date_arithmetic_case(&[false, false]);

    let fast = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.clone(),
    })
    .expect("fast request falls back");
    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset,
        queries,
    })
    .expect("explain request succeeds");

    assert_eq!(fast.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
    assert_eq!(
        fast.metadata.fallback_reason.as_deref(),
        Some("bulk fast mode does not yet support days_between")
    );
    assert_eq!(explain.metadata.actual_mode, ExecutionMode::Explain);
    assert_eq!(
        results_without_trace(&fast),
        results_without_trace(&explain)
    );
}

#[test]
fn a_construct_only_a_dead_branch_contains_never_forces_a_fallback() {
    // Every row selects the supported branch: the date arithmetic is dead code
    // for this batch, so fast mode answers it.
    let (program, dataset, queries) = date_arithmetic_case(&[true, true]);
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
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.fallback_reason, None);
    assert_eq!(
        results_without_trace(&fast),
        results_without_trace(&explain)
    );

    // One live row reaches the date arithmetic: fast declines and falls back.
    let (program, dataset, queries) = date_arithmetic_case(&[true, false]);
    let fast = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.clone(),
    })
    .expect("fast request falls back");
    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset,
        queries,
    })
    .expect("explain request succeeds");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
    assert!(fast.metadata.fallback_reason.is_some());
    assert_eq!(
        results_without_trace(&fast),
        results_without_trace(&explain)
    );
}

/// `benefit = if use_amount: amount else: days_between(period_start,
/// period_end)`, one household per flag.
fn date_arithmetic_case(flags: &[bool]) -> (ProgramSpec, DatasetSpec, Vec<ExecutionQuery>) {
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let program = ProgramSpec {
        derived: vec![DerivedSpec {
            id: None,
            name: "benefit".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Integer,
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
                            name: "use_amount".to_string(),
                        }),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Bool { value: true },
                        }),
                    }),
                    then_expr: Box::new(ScalarExprSpec::Input {
                        name: "amount".to_string(),
                    }),
                    else_expr: Box::new(ScalarExprSpec::DaysBetween {
                        from: Box::new(ScalarExprSpec::PeriodStart),
                        to: Box::new(ScalarExprSpec::PeriodEnd),
                    }),
                },
            },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let mut inputs = Vec::new();
    let mut queries = Vec::new();
    for (index, use_amount) in flags.iter().enumerate() {
        let entity_id = format!("household-{index}");
        inputs.push(InputRecordSpec {
            name: "use_amount".to_string(),
            entity: "Household".to_string(),
            entity_id: entity_id.clone(),
            interval: interval.clone(),
            value: ScalarValueSpec::Bool { value: *use_amount },
        });
        inputs.push(InputRecordSpec {
            name: "amount".to_string(),
            entity: "Household".to_string(),
            entity_id: entity_id.clone(),
            interval: interval.clone(),
            value: ScalarValueSpec::Integer { value: 7 },
        });
        queries.push(ExecutionQuery {
            assessment_date: None,
            entity_id,
            period: period.clone(),
            outputs: vec!["benefit".to_string()],
        });
    }
    (
        program,
        DatasetSpec {
            inputs,
            relations: vec![],
        },
        queries,
    )
}

/// A response's results as JSON with the explain-only trace removed, for
/// comparing a fast response against an explain response exactly.
fn results_without_trace(response: &ExecutionResponse) -> serde_json::Value {
    let mut results = serde_json::to_value(&response.results).expect("results serialise");
    for result in results.as_array_mut().expect("results are an array") {
        result
            .as_object_mut()
            .expect("each result is an object")
            .remove("trace");
    }
    results
}

/// A household program whose outputs fail in fast mode in different ways:
/// `days_in_period` counts days between dates and `period_start_date` reads
/// the period start, two constructs bulk does not support (fallback, each
/// with its own reason); `household_income` aggregates a related derived
/// value, which bulk answers on the reference interpreter; `guarded_amount` never takes its `else` branch,
/// which reads the missing `absent_amount` (bulk skips it too, since every row
/// agrees on the condition); `reads_first` and `reads_second` read genuinely
/// missing inputs, and both `shares_failure` rules depend on `reads_first`.
fn fast_outcome_program() -> ProgramSpec {
    let household_rule = |name: &str, expr: ScalarExprSpec| DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype: DTypeSpec::Decimal,
        unit: None,
        rounding: None,
        source: None,
        period: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemanticsSpec::Scalar { expr },
        versions: vec![],
    };
    let input = |name: &str| ScalarExprSpec::Input {
        name: name.to_string(),
    };
    ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![
            DerivedSpec {
                entity: "Person".to_string(),
                ..household_rule("person_income", input("income"))
            },
            household_rule(
                "household_income",
                ScalarExprSpec::SumRelated {
                    relation: "member_of_household".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    value: RelatedValueRefSpec::Derived {
                        name: "person_income".to_string(),
                    },
                    where_clause: None,
                },
            ),
            household_rule(
                "guarded_amount",
                ScalarExprSpec::If {
                    condition: Box::new(axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
                        left: Box::new(decimal_literal(0)),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(decimal_literal(0)),
                    }),
                    then_expr: Box::new(decimal_literal(7)),
                    else_expr: Box::new(input("absent_amount")),
                },
            ),
            DerivedSpec {
                dtype: DTypeSpec::Integer,
                ..household_rule(
                    "days_in_period",
                    ScalarExprSpec::DaysBetween {
                        from: Box::new(ScalarExprSpec::PeriodStart),
                        to: Box::new(ScalarExprSpec::PeriodEnd),
                    },
                )
            },
            household_rule("reads_first", input("absent_first")),
            household_rule("reads_second", input("absent_second")),
            DerivedSpec {
                dtype: DTypeSpec::Date,
                ..household_rule("period_start_date", ScalarExprSpec::PeriodStart)
            },
            household_rule(
                "shares_failure_a",
                ScalarExprSpec::Add {
                    items: vec![
                        ScalarExprSpec::Derived {
                            name: "reads_first".to_string(),
                        },
                        decimal_literal(1),
                    ],
                },
            ),
            household_rule(
                "shares_failure_b",
                ScalarExprSpec::Add {
                    items: vec![
                        ScalarExprSpec::Derived {
                            name: "reads_first".to_string(),
                        },
                        decimal_literal(2),
                    ],
                },
            ),
        ],
        ..ProgramSpec::default()
    }
}

fn fast_outcome_request(mode: ExecutionMode, outputs: &[&str]) -> ExecutionRequest {
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    ExecutionRequest {
        mode,
        program: fast_outcome_program(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: interval.clone(),
                value: decimal_value("100"),
            }],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval,
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: outputs.iter().map(|output| output.to_string()).collect(),
        }],
    }
}

/// Fast mode's outcome is a function of the request, not of the process.
/// Each run below builds fresh hash sets, each with its own hasher keys, so an
/// outcome that depended on hash iteration order would vary across the 64
/// runs of each output order. When any requested output
/// needs explain, the whole request falls back, whichever output is listed
/// first, and answers `guarded_amount` as explain does (7).
#[test]
fn fast_mode_falls_back_deterministically_when_any_output_needs_explain() {
    let explain = execute_request(fast_outcome_request(
        ExecutionMode::Explain,
        &["period_start_date", "guarded_amount"],
    ))
    .expect("explain answers both outputs");
    let explain_results = serde_json::to_value(&explain.results).expect("results serialise");
    assert_eq!(
        decimal_output(&explain.results[0].outputs["guarded_amount"]),
        decimal("7")
    );

    for outputs in [
        ["period_start_date", "guarded_amount"],
        ["guarded_amount", "period_start_date"],
    ] {
        for run in 0..64 {
            let fast = execute_request(fast_outcome_request(ExecutionMode::Fast, &outputs))
                .unwrap_or_else(|error| {
                    panic!(
                        "run {run} with outputs {outputs:?} failed instead of falling back: {error}"
                    )
                });
            assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
            assert_eq!(
                fast.metadata.fallback_reason.as_deref(),
                Some("bulk fast mode does not yet support period_start / period_end"),
                "run {run} with outputs {outputs:?}"
            );
            assert_eq!(
                serde_json::to_value(&fast.results).expect("results serialise"),
                explain_results,
                "run {run} with outputs {outputs:?}"
            );
        }
    }

    // Without an output that needs explain, the same guarded output and a
    // related-derived aggregation are answered in fast mode on every run.
    let explain = execute_request(fast_outcome_request(
        ExecutionMode::Explain,
        &["household_income", "guarded_amount"],
    ))
    .expect("explain answers both outputs");
    assert_eq!(
        decimal_output(&explain.results[0].outputs["household_income"]),
        decimal("100")
    );
    for outputs in [
        ["household_income", "guarded_amount"],
        ["guarded_amount", "household_income"],
    ] {
        for run in 0..16 {
            let fast = execute_request(fast_outcome_request(ExecutionMode::Fast, &outputs))
                .unwrap_or_else(|error| panic!("run {run} with outputs {outputs:?}: {error}"));
            assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
            assert_eq!(fast.metadata.fallback_reason, None);
            let fast_request_explain =
                execute_request(fast_outcome_request(ExecutionMode::Explain, &outputs))
                    .expect("explain answers both outputs");
            assert_eq!(
                results_without_trace(&fast),
                results_without_trace(&fast_request_explain),
                "run {run} with outputs {outputs:?}"
            );
        }
    }
}

/// When several outputs need explain for different reasons, the first of
/// them in request order supplies the fallback reason, in either order, also
/// after an output bulk answers.
#[test]
fn fast_mode_fallback_reason_is_the_first_in_request_order() {
    const DAYS: &str = "bulk fast mode does not yet support days_between";
    const PERIOD: &str = "bulk fast mode does not yet support period_start / period_end";
    for (outputs, reason) in [
        (["days_in_period", "period_start_date"], DAYS),
        (["period_start_date", "days_in_period"], PERIOD),
        (["guarded_amount", "period_start_date"], PERIOD),
        (["household_income", "days_in_period"], DAYS),
    ] {
        for run in 0..16 {
            let fast = execute_request(fast_outcome_request(ExecutionMode::Fast, &outputs))
                .unwrap_or_else(|error| panic!("run {run} with outputs {outputs:?}: {error}"));
            assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
            assert_eq!(
                fast.metadata.fallback_reason.as_deref(),
                Some(reason),
                "run {run} with outputs {outputs:?}"
            );
        }
    }
}

/// With no output needing explain, fast mode reports the first failing output
/// in request order on every run, the same error explain reports.
#[test]
fn fast_mode_reports_the_first_failing_output_in_request_order() {
    for (outputs, missing) in [
        (["reads_first", "reads_second"], "`absent_first`"),
        (["reads_second", "reads_first"], "`absent_second`"),
        // Two outputs sharing one failing dependency report its error.
        (["shares_failure_a", "shares_failure_b"], "`absent_first`"),
        // An unknown output is an error in its place in request order.
        (["reads_first", "no_such_output"], "`absent_first`"),
        (["no_such_output", "reads_first"], "no_such_output"),
    ] {
        let explain = execute_request(fast_outcome_request(ExecutionMode::Explain, &outputs))
            .expect_err("explain fails on the first missing input");
        assert!(
            explain.to_string().contains(missing),
            "explain reported {explain}"
        );
        for run in 0..64 {
            let fast = execute_request(fast_outcome_request(ExecutionMode::Fast, &outputs))
                .expect_err("fast fails on the first missing input");
            assert_eq!(
                fast.to_string(),
                explain.to_string(),
                "run {run} with outputs {outputs:?}"
            );
        }
    }
}

/// A household rule reading one input, and a batch request over several
/// households that supplies `inputs` as (input name, household, value).
fn batch_request(
    mode: ExecutionMode,
    rules: Vec<(&str, ScalarExprSpec)>,
    inputs: &[(&str, &str, &str)],
    queries: &[(&str, &[&str])],
) -> ExecutionRequest {
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    ExecutionRequest {
        mode,
        program: ProgramSpec {
            derived: rules
                .into_iter()
                .map(|(name, expr)| DerivedSpec {
                    id: None,
                    name: name.to_string(),
                    entity: "Household".to_string(),
                    dtype: DTypeSpec::Decimal,
                    unit: None,
                    rounding: None,
                    source: None,
                    period: None,
                    source_url: None,
                    corpus_citation_path: None,
                    semantics: DerivedSemanticsSpec::Scalar { expr },
                    versions: vec![],
                })
                .collect(),
            ..ProgramSpec::default()
        },
        dataset: DatasetSpec {
            inputs: inputs
                .iter()
                .map(|(name, entity_id, value)| InputRecordSpec {
                    name: name.to_string(),
                    entity: "Household".to_string(),
                    entity_id: entity_id.to_string(),
                    interval: interval.clone(),
                    value: decimal_value(value),
                })
                .collect(),
            relations: Vec::new(),
        },
        queries: queries
            .iter()
            .map(|(entity_id, outputs)| ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: period.clone(),
                outputs: outputs.iter().map(|output| output.to_string()).collect(),
            })
            .collect(),
    }
}

/// Bulk warms each output over every row at once, so across a multi-query
/// batch its first error can belong to a later query than explain's (which
/// works query by query). Fast mode therefore lets explain decide any request
/// bulk fails on, and reports explain's error.
#[test]
fn fast_mode_reports_explains_error_across_a_multi_query_batch() {
    let input = |name: &str| ScalarExprSpec::Input {
        name: name.to_string(),
    };
    let rules = || {
        vec![
            ("amount_output", input("amount")),
            ("other_output", input("absent_other")),
        ]
    };
    // `amount` is supplied for household-a only, so warming `amount_output`
    // fails on household-b before either later output of household-a is
    // reached.
    for first_query_outputs in [
        &["amount_output", "no_such_output"][..],
        &["amount_output", "other_output"][..],
    ] {
        let request = |mode| {
            batch_request(
                mode,
                rules(),
                &[("amount", "household-a", "5")],
                &[
                    ("household-a", first_query_outputs),
                    ("household-b", &["amount_output"]),
                ],
            )
        };
        let explain = execute_request(request(ExecutionMode::Explain))
            .expect_err("explain fails on household-a's second output");
        assert!(
            !explain.to_string().contains("household-b"),
            "explain reported {explain}"
        );
        for run in 0..32 {
            let fast = execute_request(request(ExecutionMode::Fast))
                .expect_err("fast fails as explain does");
            assert_eq!(
                fast.to_string(),
                explain.to_string(),
                "run {run} with first query outputs {first_query_outputs:?}"
            );
        }
    }
}

/// When the rows disagree on a conditional, each branch is evaluated only
/// for the rows that select it. The branch household-a does not take reads an
/// input household-a lacks, which explain never evaluates; fast mode answers
/// the batch itself, as explain does.
#[test]
fn fast_mode_answers_a_batch_whose_rows_take_different_branches() {
    let input = |name: &str| ScalarExprSpec::Input {
        name: name.to_string(),
    };
    let rules = || {
        vec![(
            "flagged_or_amount",
            ScalarExprSpec::If {
                condition: Box::new(axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
                    left: Box::new(input("flag")),
                    op: ComparisonOpSpec::Eq,
                    right: Box::new(decimal_literal(1)),
                }),
                then_expr: Box::new(decimal_literal(7)),
                else_expr: Box::new(input("amount")),
            },
        )]
    };
    // household-a takes the `then` branch and has no `amount`; household-b
    // takes the `else` branch and has one.
    let request = |mode| {
        batch_request(
            mode,
            rules(),
            &[
                ("flag", "household-a", "1"),
                ("flag", "household-b", "0"),
                ("amount", "household-b", "5"),
            ],
            &[
                ("household-a", &["flagged_or_amount"]),
                ("household-b", &["flagged_or_amount"]),
            ],
        )
    };
    let explain = execute_request(request(ExecutionMode::Explain)).expect("explain answers");
    assert_eq!(
        decimal_output(&explain.results[0].outputs["flagged_or_amount"]),
        decimal("7")
    );
    assert_eq!(
        decimal_output(&explain.results[1].outputs["flagged_or_amount"]),
        decimal("5")
    );
    for run in 0..16 {
        let fast = execute_request(request(ExecutionMode::Fast))
            .unwrap_or_else(|error| panic!("run {run} failed: {error}"));
        assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast, "run {run}");
        assert_eq!(fast.metadata.fallback_reason, None, "run {run}");
        assert_eq!(
            results_without_trace(&fast),
            results_without_trace(&explain),
            "run {run}"
        );
    }
}

/// A parameter output anywhere in the request sends it to explain before
/// bulk evaluates anything. Bulk evaluates both branches of `guarded` for
/// every row, and household-a's untaken branch overflows, so warming
/// `guarded` before seeing `rate` would name that overflow as the fallback
/// reason (and, before evaluator arithmetic was checked, panicked).
#[test]
fn fast_mode_sends_a_parameter_request_to_explain_before_evaluating_anything() {
    const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: "3"
  - name: guarded
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if flag == 1: 0
          else: amount * 2
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(RULESPEC).expect("RuleSpec lowers");
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let input = |name: &str, entity_id: &str, value: &str| InputRecordSpec {
        name: name.to_string(),
        entity: "Household".to_string(),
        entity_id: entity_id.to_string(),
        interval: interval.clone(),
        value: decimal_value(value),
    };
    let request = |mode, first_outputs: &[&str]| ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: DatasetSpec {
            inputs: vec![
                input("flag", "household-a", "1"),
                input("amount", "household-a", &Decimal::MAX.to_string()),
                input("flag", "household-b", "0"),
                input("amount", "household-b", "1"),
            ],
            relations: Vec::new(),
        },
        queries: [
            ("household-a", first_outputs),
            ("household-b", &["guarded"][..]),
        ]
        .into_iter()
        .map(|(entity_id, outputs)| ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: period.clone(),
            outputs: outputs.iter().map(|output| output.to_string()).collect(),
        })
        .collect(),
    };
    for first_outputs in [&["guarded", "rate"][..], &["rate", "guarded"][..]] {
        let explain = execute_request(request(ExecutionMode::Explain, first_outputs))
            .expect("explain answers");
        assert_eq!(
            decimal_output(&explain.results[0].outputs["guarded"]),
            decimal("0")
        );
        assert_eq!(
            decimal_output(&explain.results[1].outputs["guarded"]),
            decimal("2")
        );
        let fast = execute_request(request(ExecutionMode::Fast, first_outputs))
            .expect("fast answers through explain");
        assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
        assert!(
            fast.metadata
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("parameter output `rate`")),
            "{:?}",
            fast.metadata.fallback_reason
        );
        assert_eq!(
            serde_json::to_value(&fast.results).expect("results serialise"),
            serde_json::to_value(&explain.results).expect("results serialise"),
            "outputs {first_outputs:?}"
        );
    }
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
fn pinned_versioned_rule_evaluates_to_the_pin_in_every_mode() {
    // adjusted_amount is a VERSIONED rule: its formula lives in
    // versions[0], not in a top-level expression. The pin must therefore
    // land in the version the engine actually selects for the dated query
    // — pinning only a top-level expression is exactly the silent-baseline
    // bug this feature exists to prevent.
    let period = simple_period();
    for mode in [ExecutionMode::Fast, ExecutionMode::Explain] {
        let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
            .expect("RuleSpec module compiles from YAML");
        let response = execute_compiled_request(
            artifact,
            CompiledExecutionRequest {
                mode: mode.clone(),
                dataset: simple_dataset(&period),
                queries: simple_queries(&period),
                pins: vec![RulePin {
                    rule: "adjusted_amount".to_string(),
                    value: ScalarValueSpec::Decimal {
                        value: "99".to_string(),
                    },
                }],
            },
        )
        .expect("pinned compiled request succeeds");
        assert_eq!(
            decimal_output(
                response.results[0]
                    .outputs
                    .get("adjusted_amount")
                    .expect("adjusted amount output")
            ),
            decimal("99"),
            "pin must override the versioned formula in {mode:?} mode"
        );
    }
}

#[test]
fn pinned_rule_needs_none_of_its_original_inputs_in_any_mode() {
    // The pin replaces adjusted_amount's value, so its formula's `amount`
    // input need not be supplied. Fast mode used to evaluate the original
    // formula anyway and fail with a missing input.
    let period = simple_period();
    for mode in [ExecutionMode::Fast, ExecutionMode::Explain] {
        let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
            .expect("RuleSpec module compiles from YAML");
        let response = execute_compiled_request(
            artifact,
            CompiledExecutionRequest {
                mode: mode.clone(),
                dataset: DatasetSpec {
                    inputs: Vec::new(),
                    relations: Vec::new(),
                },
                queries: simple_queries(&period),
                pins: vec![RulePin {
                    rule: "adjusted_amount".to_string(),
                    value: decimal_value("99"),
                }],
            },
        )
        .unwrap_or_else(|error| panic!("pinned {mode:?} request failed: {error}"));
        assert_eq!(response.metadata.actual_mode, mode);
        assert_eq!(response.metadata.fallback_reason, None);
        for result in &response.results {
            assert_eq!(
                decimal_output(&result.outputs["adjusted_amount"]),
                decimal("99")
            );
        }
    }
}

fn household_decimal_rule(name: &str, expr: ScalarExprSpec) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype: DTypeSpec::Decimal,
        unit: None,
        rounding: None,
        source: None,
        period: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemanticsSpec::Scalar { expr },
        versions: vec![],
    }
}

fn input_expr(name: &str) -> ScalarExprSpec {
    ScalarExprSpec::Input {
        name: name.to_string(),
    }
}

fn amount_compared(op: ComparisonOpSpec, value: i64) -> axiom_rules_engine::spec::JudgmentExprSpec {
    axiom_rules_engine::spec::JudgmentExprSpec::Comparison {
        left: Box::new(input_expr("amount")),
        op,
        right: Box::new(decimal_literal(value)),
    }
}

/// Ask each query for `output` alone, in explain and in fast mode, and return
/// each mode's values, asserting fast answered natively rather than by
/// falling back to explain.
fn explain_and_fast_values(
    program: &ProgramSpec,
    dataset: &DatasetSpec,
    queries: &[ExecutionQuery],
    output: &str,
) -> [Vec<Decimal>; 2] {
    let queries: Vec<ExecutionQuery> = queries
        .iter()
        .map(|query| ExecutionQuery {
            outputs: vec![output.to_string()],
            ..query.clone()
        })
        .collect();
    [ExecutionMode::Explain, ExecutionMode::Fast].map(|mode| {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: queries.clone(),
        })
        .unwrap_or_else(|error| panic!("{mode:?} request failed: {error}"));
        assert_eq!(response.metadata.actual_mode, mode, "{mode:?} fell back");
        response
            .results
            .iter()
            .map(|result| decimal_output(&result.outputs[output]))
            .collect()
    })
}

/// A request may query one entity more than once. Bulk used to fill each
/// entity's inputs into its last query row only, so an earlier row read an
/// optional input as absent: fast answered [0, 15] where explain answers
/// [15, 15].
#[test]
fn fast_mode_gives_every_query_of_an_entity_its_inputs() {
    let program = ProgramSpec {
        derived: vec![household_decimal_rule(
            "optional_amount",
            ScalarExprSpec::InputOrElse {
                name: "amount".to_string(),
                default: decimal_value("0"),
            },
        )],
        ..ProgramSpec::default()
    };
    let period = simple_period();
    let query = simple_queries(&period).remove(0);
    let values = explain_and_fast_values(
        &program,
        &simple_dataset(&period),
        &[query.clone(), query],
        "optional_amount",
    );
    assert_eq!(values[0], vec![decimal("15"), decimal("15")]);
    assert_eq!(values[1], values[0]);
}

/// With a relation of more than two slots, one related entity can appear in
/// several tuples. Explain counts and sums each related entity once; bulk
/// used to count tuples, so fast answered 2 and 10 where explain answers 1
/// and 5.
#[test]
fn fast_mode_counts_and_sums_each_related_entity_once() {
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let program = ProgramSpec {
        relations: vec![axiom_rules_engine::spec::RelationSpec {
            name: "household_member_role".to_string(),
            arity: 3,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![
            household_decimal_rule(
                "member_count",
                ScalarExprSpec::CountRelated {
                    relation: "household_member_role".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    where_clause: None,
                },
            ),
            household_decimal_rule(
                "member_income",
                ScalarExprSpec::SumRelated {
                    relation: "household_member_role".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    value: RelatedValueRefSpec::Input {
                        name: "income".to_string(),
                    },
                    where_clause: None,
                },
            ),
        ],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "income".to_string(),
            entity: "Person".to_string(),
            entity_id: "person-1".to_string(),
            interval: interval.clone(),
            value: decimal_value("5"),
        }],
        relations: ["head", "earner"]
            .into_iter()
            .map(|role| RelationRecordSpec {
                name: "household_member_role".to_string(),
                tuple: vec![
                    "household-1".to_string(),
                    "person-1".to_string(),
                    role.to_string(),
                ],
                interval: interval.clone(),
            })
            .collect(),
    };
    let queries = [ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: vec!["member_count".to_string(), "member_income".to_string()],
    }];
    for (output, expected) in [("member_count", "1"), ("member_income", "5")] {
        let values = explain_and_fast_values(&program, &dataset, &queries, output);
        assert_eq!(values[0], vec![decimal(expected)], "{output}");
        assert_eq!(values[1], values[0], "{output}");
    }
}

/// The mirror of the uniformly true case below: when no row selects the
/// `then` branch, fast mode must not evaluate it either.
#[test]
fn fast_mode_skips_a_then_branch_no_row_selects() {
    let program = ProgramSpec {
        derived: vec![household_decimal_rule(
            "guarded_ratio",
            ScalarExprSpec::If {
                condition: Box::new(amount_compared(ComparisonOpSpec::Gt, 100)),
                then_expr: Box::new(ScalarExprSpec::Div {
                    left: Box::new(input_expr("amount")),
                    right: Box::new(decimal_literal(0)),
                }),
                else_expr: Box::new(input_expr("amount")),
            },
        )],
        ..ProgramSpec::default()
    };
    let period = simple_period();
    let values = explain_and_fast_values(
        &program,
        &simple_dataset(&period),
        &simple_queries(&period),
        "guarded_ratio",
    );
    assert_eq!(values[0], vec![decimal("15"), decimal("20")]);
    assert_eq!(values[1], values[0]);
}

/// Repeated queries of one household also see its relations: bulk used to
/// give only the last row the household's tuples, so fast counted 0 for the
/// first row where explain counts the two members with income.
#[test]
fn fast_mode_gives_every_query_of_an_entity_its_relations() {
    use axiom_rules_engine::spec::JudgmentExprSpec;

    let period = simple_period();
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
        derived: vec![household_decimal_rule(
            "earning_members",
            ScalarExprSpec::CountRelated {
                relation: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
                where_clause: Some(Box::new(JudgmentExprSpec::Comparison {
                    left: Box::new(input_expr("income")),
                    op: ComparisonOpSpec::Gt,
                    right: Box::new(decimal_literal(0)),
                })),
            },
        )],
        ..ProgramSpec::default()
    };
    let people = [("person-1", "100"), ("person-2", "0"), ("person-3", "50")];
    let dataset = DatasetSpec {
        inputs: people
            .iter()
            .map(|(person, income)| InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: person.to_string(),
                interval: interval.clone(),
                value: decimal_value(income),
            })
            .collect(),
        relations: people
            .iter()
            .map(|(person, _)| RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person.to_string(), "household-1".to_string()],
                interval: interval.clone(),
            })
            .collect(),
    };
    let query = ExecutionQuery {
        assessment_date: None,
        entity_id: "household-1".to_string(),
        period,
        outputs: Vec::new(),
    };
    let values = explain_and_fast_values(
        &program,
        &dataset,
        &[query.clone(), query],
        "earning_members",
    );
    assert_eq!(values[0], vec![decimal("2"), decimal("2")]);
    assert_eq!(values[1], values[0]);
}

/// A derived relation lists the source relation's related entities in the
/// derivation's own slots. RuleSpec lowers `len(snap_unit)` to
/// `count_related(snap_unit, 1, 0)` while the derivation reads its source in
/// slots 0 and 1, so fast mode must take related entities from the
/// derivation, not re-project source tuples with the call's slots.
#[test]
fn fast_mode_counts_a_derived_relation_as_explain_does() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: household_member
    kind: data_relation
    data_relation:
      arity: 2
  - name: eligible_member
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
      source_relation: household_member
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: eligible_member
  - name: snap_unit_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(snap_unit)
"#,
    )
    .expect("derived relation program compiles");
    let period = simple_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let members = [
        ("household-1", "person-1", true),
        ("household-1", "person-2", true),
        ("household-1", "person-3", false),
        ("household-2", "person-4", true),
    ];
    let dataset = DatasetSpec {
        inputs: members
            .iter()
            .map(|(_, person, has_ssn)| InputRecordSpec {
                name: "has_ssn".to_string(),
                entity: "Person".to_string(),
                entity_id: person.to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool { value: *has_ssn },
            })
            .collect(),
        relations: members
            .iter()
            .map(|(household, person, _)| RelationRecordSpec {
                name: "household_member".to_string(),
                tuple: vec![household.to_string(), person.to_string()],
                interval: interval.clone(),
            })
            .collect(),
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_compiled_request(
            artifact.clone(),
            CompiledExecutionRequest {
                mode: mode.clone(),
                dataset: dataset.clone(),
                queries: ["household-1", "household-2"]
                    .into_iter()
                    .map(|household| ExecutionQuery {
                        assessment_date: None,
                        entity_id: household.to_string(),
                        period: period.clone(),
                        outputs: vec!["snap_unit_size".to_string()],
                    })
                    .collect(),
                pins: Vec::new(),
            },
        )
        .unwrap_or_else(|error| panic!("{mode:?} request failed: {error}"));
        assert_eq!(response.metadata.actual_mode, mode, "{mode:?} fell back");
        let sizes: Vec<i64> = response
            .results
            .iter()
            .map(|result| integer_output(&result.outputs["snap_unit_size"]))
            .collect();
        assert_eq!(sizes, vec![2, 1], "{mode:?}");
    }
}

/// Explain evaluates only the branch a row selects and stops `and` / `or` at
/// the first operand that decides the row. Fast mode matches it when every
/// row in the batch decides the same way: branches and operands that no row
/// reaches are not evaluated, so their errors cannot fail the batch.
#[test]
fn fast_mode_skips_branches_and_operands_no_row_reaches() {
    use axiom_rules_engine::spec::JudgmentExprSpec;

    let program = ProgramSpec {
        derived: vec![
            // Guarded division: every household has amount 15 or 20, so no
            // row takes the branch that divides by a zero input.
            household_decimal_rule(
                "guarded_ratio",
                ScalarExprSpec::If {
                    condition: Box::new(amount_compared(ComparisonOpSpec::Gt, 0)),
                    then_expr: Box::new(input_expr("amount")),
                    else_expr: Box::new(ScalarExprSpec::Div {
                        left: Box::new(input_expr("amount")),
                        right: Box::new(input_expr("zero")),
                    }),
                },
            ),
            // Every row fails the first operand, so neither mode reads
            // `absent_flag`.
            household_decimal_rule(
                "and_short_circuit",
                ScalarExprSpec::If {
                    condition: Box::new(JudgmentExprSpec::And {
                        items: vec![
                            amount_compared(ComparisonOpSpec::Gt, 100),
                            JudgmentExprSpec::Comparison {
                                left: Box::new(input_expr("absent_flag")),
                                op: ComparisonOpSpec::Gt,
                                right: Box::new(decimal_literal(0)),
                            },
                        ],
                    }),
                    then_expr: Box::new(decimal_literal(1)),
                    else_expr: Box::new(decimal_literal(2)),
                },
            ),
            // Every row satisfies the first operand.
            household_decimal_rule(
                "or_short_circuit",
                ScalarExprSpec::If {
                    condition: Box::new(JudgmentExprSpec::Or {
                        items: vec![
                            amount_compared(ComparisonOpSpec::Gt, 10),
                            JudgmentExprSpec::Comparison {
                                left: Box::new(input_expr("absent_flag")),
                                op: ComparisonOpSpec::Gt,
                                right: Box::new(decimal_literal(0)),
                            },
                        ],
                    }),
                    then_expr: Box::new(decimal_literal(3)),
                    else_expr: Box::new(decimal_literal(4)),
                },
            ),
        ],
        ..ProgramSpec::default()
    };
    let period = simple_period();
    let mut dataset = simple_dataset(&period);
    for entity_id in ["household-1", "household-2"] {
        dataset.inputs.push(InputRecordSpec {
            name: "zero".to_string(),
            entity: "Household".to_string(),
            entity_id: entity_id.to_string(),
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
            value: decimal_value("0"),
        });
    }
    let queries: Vec<ExecutionQuery> = simple_queries(&period)
        .into_iter()
        .map(|query| ExecutionQuery {
            outputs: vec![
                "guarded_ratio".to_string(),
                "and_short_circuit".to_string(),
                "or_short_circuit".to_string(),
            ],
            ..query
        })
        .collect();

    let mut responses = Vec::new();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: dataset.clone(),
            queries: queries.clone(),
        })
        .unwrap_or_else(|error| panic!("{mode:?} request failed: {error}"));
        assert_eq!(response.metadata.actual_mode, mode);
        let values: Vec<[Decimal; 3]> = response
            .results
            .iter()
            .map(|result| {
                [
                    decimal_output(&result.outputs["guarded_ratio"]),
                    decimal_output(&result.outputs["and_short_circuit"]),
                    decimal_output(&result.outputs["or_short_circuit"]),
                ]
            })
            .collect();
        assert_eq!(
            values,
            vec![
                [decimal("15"), decimal("2"), decimal("3")],
                [decimal("20"), decimal("2"), decimal("3")],
            ],
            "{mode:?}"
        );
        responses.push(values);
    }
    assert_eq!(responses[0], responses[1]);
}

#[test]
fn pinning_an_unknown_rule_is_an_error_not_a_silent_no_op() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SIMPLE_RULESPEC)
        .expect("RuleSpec module compiles from YAML");
    let period = simple_period();
    let error = execute_compiled_request(
        artifact,
        CompiledExecutionRequest {
            mode: ExecutionMode::Fast,
            dataset: simple_dataset(&period),
            queries: simple_queries(&period),
            pins: vec![RulePin {
                rule: "no_such_rule".to_string(),
                value: ScalarValueSpec::Integer { value: 1 },
            }],
        },
    )
    .expect_err("unknown pinned rule must fail loudly");
    assert!(
        matches!(&error, ApiError::UnknownPinnedRule { rule } if rule == "no_such_rule"),
        "got {error:?}"
    );
}

#[test]
fn dataset_binding_rejects_derived_ids_in_every_request_path_and_mode() {
    for compiled in [false, true] {
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let mut request = dataset_binding_request(mode, true);
            let reference = "us:statutes/26/24#adjusted_amount";
            add_dataset_binding_input(&mut request, reference);

            let error = execute_dataset_binding_request(request, compiled)
                .expect_err("a derived legal id is not a dataset input");
            assert_derived_dataset_input_error(&error.to_string(), reference);
        }
    }
}

#[test]
fn dataset_binding_rejects_parameter_ids_in_every_request_path_and_mode() {
    for compiled in [false, true] {
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let mut request = dataset_binding_request(mode, true);
            let reference = "us:statutes/26/24#base_amount";
            add_dataset_binding_input(&mut request, reference);

            let error = execute_dataset_binding_request(request, compiled)
                .expect_err("a parameter legal id is not a dataset input");
            assert_parameter_dataset_input_error(&error.to_string(), reference);
        }
    }
}

#[test]
fn dataset_binding_identifies_bare_derived_names_with_and_without_public_ids() {
    for public_ids in [false, true] {
        for compiled in [false, true] {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let mut request = dataset_binding_request(mode, public_ids);
                add_dataset_binding_input(&mut request, "adjusted_amount");

                let error = execute_dataset_binding_request(request, compiled)
                    .expect_err("a bare derived name is not a dataset input");
                assert_derived_dataset_input_error(&error.to_string(), "adjusted_amount");
            }
        }
    }
}

#[test]
fn dataset_binding_identifies_bare_parameter_names_with_and_without_public_ids() {
    for public_ids in [false, true] {
        for compiled in [false, true] {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let mut request = dataset_binding_request(mode, public_ids);
                add_dataset_binding_input(&mut request, "base_amount");

                let error = execute_dataset_binding_request(request, compiled)
                    .expect_err("a bare parameter name is not a dataset input");
                assert_parameter_dataset_input_error(&error.to_string(), "base_amount");
            }
        }
    }
}

#[test]
fn dataset_binding_refuses_computed_inputs_with_default_and_strict_options() {
    for strict_relation_entities in [false, true] {
        for (reference, derived) in [
            ("us:statutes/26/24#adjusted_amount", true),
            ("us:statutes/26/24#base_amount", false),
        ] {
            let mut request = dataset_binding_request(ExecutionMode::Explain, true);
            add_dataset_binding_input(&mut request, reference);
            let program = request
                .program
                .to_program()
                .expect("fixture model converts");
            let error = request
                .dataset
                .to_dataset_for_program_with_options(
                    &program,
                    DatasetBindingOptions {
                        strict_relation_entities,
                    },
                )
                .expect_err("binding options cannot allow a computed input");
            if derived {
                assert_derived_dataset_input_error(&error.to_string(), reference);
            } else {
                assert_parameter_dataset_input_error(&error.to_string(), reference);
            }
        }
    }
}

#[test]
fn dataset_binding_pins_do_not_allow_derived_dataset_inputs() {
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let mut request = dataset_binding_request(mode, true);
        let reference = "us:statutes/26/24#adjusted_amount";
        add_dataset_binding_input(&mut request, reference);
        let artifact =
            CompiledProgramArtifact::compile(request.program).expect("fixture program compiles");
        let error = execute_compiled_request(
            artifact,
            CompiledExecutionRequest {
                mode: request.mode,
                dataset: request.dataset,
                queries: request.queries,
                pins: vec![RulePin {
                    rule: "adjusted_amount".to_string(),
                    value: decimal_value("99"),
                }],
            },
        )
        .expect_err("a pin does not make a derived rule a dataset input");
        assert_derived_dataset_input_error(&error.to_string(), reference);
    }
}

#[test]
fn dataset_binding_resolver_does_not_expose_computed_ids_as_input_slots() {
    let request = dataset_binding_request(ExecutionMode::Explain, true);
    let program = request
        .program
        .to_program()
        .expect("fixture model converts");
    for reference in [
        "us:statutes/26/24#adjusted_amount",
        "us:statutes/26/24#base_amount",
    ] {
        assert_eq!(
            program.resolve_input_name(reference),
            None,
            "computed reference {reference} must not resolve to an input slot"
        );
    }
    assert_eq!(
        program.resolve_input_name("us:statutes/26/24#input.amount"),
        Some("amount".to_string())
    );
}

#[test]
fn dataset_binding_accepts_real_canonical_inputs_in_every_request_path_and_mode() {
    for compiled in [false, true] {
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let request = dataset_binding_request(mode.clone(), true);
            let response = execute_dataset_binding_request(request, compiled)
                .expect("a canonical catalog input remains accepted");
            assert_eq!(response.metadata.actual_mode, mode);
            for (result, expected) in response.results.iter().zip(["25", "30"]) {
                assert_eq!(
                    decimal_output(
                        result
                            .outputs
                            .get("us:statutes/26/24#adjusted_amount")
                            .expect("derived output exists")
                    ),
                    decimal(expected)
                );
            }
        }
    }
}

#[test]
fn dataset_binding_unknown_inputs_and_derived_relation_names_remain_invalid() {
    for compiled in [false, true] {
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            for reference in ["no_such_input", "us:statutes/26/24#input.no_such_input"] {
                let mut request = dataset_binding_request(mode.clone(), true);
                add_dataset_binding_input(&mut request, reference);
                let error = execute_dataset_binding_request(request, compiled)
                    .expect_err("unknown input is rejected");
                assert!(
                    matches!(error, ApiError::Spec(axiom_rules_engine::spec::SpecError::InvalidDatasetInputReference { reference: rejected }) if rejected == reference)
                );
            }

            let mut request = dataset_binding_request(mode, true);
            let reference = "us:statutes/26/24#adjusted_amount";
            request.dataset.relations.push(RelationRecordSpec {
                name: reference.to_string(),
                tuple: vec!["household-1".to_string()],
                interval: request.dataset.inputs[0].interval.clone(),
            });
            let error = execute_dataset_binding_request(request, compiled)
                .expect_err("a derived id is not a relation");
            assert!(
                matches!(error, ApiError::Spec(axiom_rules_engine::spec::SpecError::InvalidDatasetRelationReference { reference: rejected }) if rejected == reference)
            );
        }
    }
}

#[test]
fn dataset_binding_accepts_explicit_input_slots_sharing_computed_rule_names() {
    for public_ids in [false, true] {
        for compiled in [false, true] {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let mut request = dataset_binding_request(mode.clone(), public_ids);
                let expression = DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::Add {
                        items: vec![
                            ScalarExprSpec::Input {
                                name: "adjusted_amount".to_string(),
                            },
                            ScalarExprSpec::Input {
                                name: "base_amount".to_string(),
                            },
                        ],
                    },
                };
                let derived = &mut request.program.derived[0];
                derived.semantics = expression.clone();
                for version in &mut derived.versions {
                    version.semantics = expression.clone();
                }
                request.dataset.inputs.clear();
                let period = simple_period();
                for (name, value) in [("adjusted_amount", "90"), ("base_amount", "9")] {
                    request.dataset.inputs.push(InputRecordSpec {
                        name: if public_ids {
                            format!("us:statutes/26/24#input.{name}")
                        } else {
                            name.to_string()
                        },
                        entity: "Household".to_string(),
                        entity_id: "household-1".to_string(),
                        interval: IntervalSpec {
                            start: period.start,
                            end: period.end,
                        },
                        value: decimal_value(value),
                    });
                }
                request.queries.truncate(1);
                let output_name = request.queries[0].outputs[0].clone();
                let response = execute_dataset_binding_request(request, compiled)
                    .expect("explicit input slots take precedence over computed names");
                assert_eq!(response.metadata.actual_mode, mode);
                assert_eq!(
                    decimal_output(
                        response.results[0]
                            .outputs
                            .get(&output_name)
                            .expect("derived output exists")
                    ),
                    decimal("99")
                );
            }
        }
    }
}

fn dataset_binding_request(mode: ExecutionMode, public_ids: bool) -> ExecutionRequest {
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC)
        .expect("program fixture parses");
    let mut request = simple_execution_request(mode, program);
    if public_ids {
        for derived in &mut request.program.derived {
            derived.id = Some(format!("us:statutes/26/24#{}", derived.name));
        }
        for parameter in &mut request.program.parameters {
            parameter.id = Some(format!("us:statutes/26/24#{}", parameter.name));
        }
        for input in &mut request.dataset.inputs {
            input.name = format!("us:statutes/26/24#input.{}", input.name);
        }
        for query in &mut request.queries {
            query.outputs = vec!["us:statutes/26/24#adjusted_amount".to_string()];
        }
    }
    request
}

fn add_dataset_binding_input(request: &mut ExecutionRequest, reference: &str) {
    let mut input = request.dataset.inputs[0].clone();
    input.name = reference.to_string();
    input.value = decimal_value("999");
    request.dataset.inputs.push(input);
}

fn execute_dataset_binding_request(
    request: ExecutionRequest,
    compiled: bool,
) -> Result<ExecutionResponse, ApiError> {
    if compiled {
        let artifact =
            CompiledProgramArtifact::compile(request.program).expect("fixture program compiles");
        execute_compiled_request(
            artifact,
            CompiledExecutionRequest {
                mode: request.mode,
                dataset: request.dataset,
                queries: request.queries,
                pins: Vec::new(),
            },
        )
    } else {
        execute_request(request)
    }
}

fn assert_derived_dataset_input_error(message: &str, reference: &str) {
    assert!(
        message.contains(&format!("dataset input `{reference}`")),
        "{message}"
    );
    assert!(
        message.contains("derived rule `adjusted_amount`"),
        "{message}"
    );
    assert!(message.contains("pins"), "{message}");
    assert!(
        message.contains("\"rule\": \"adjusted_amount\""),
        "{message}"
    );
}

fn assert_parameter_dataset_input_error(message: &str, reference: &str) {
    assert!(
        message.contains(&format!("dataset input `{reference}`")),
        "{message}"
    );
    assert!(message.contains("parameter `base_amount`"), "{message}");
    assert!(message.contains("law"), "{message}");
    assert!(message.contains("program parameter"), "{message}");
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
            pins: Vec::new(),
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
        pins: Vec::new(),
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
        pins: Vec::new(),
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
            pins: Vec::new(),
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

#[test]
fn non_indexed_parameters_are_queryable_outputs_in_every_mode() {
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("RuleSpec lowers");
    let period = simple_period();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: DatasetSpec {
                inputs: vec![],
                relations: vec![],
            },
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "household-1".to_string(),
                period: period.clone(),
                outputs: vec!["base_amount".to_string()],
            }],
        })
        .expect("parameter query succeeds");
        let output = response.results[0]
            .outputs
            .get("base_amount")
            .expect("parameter output");
        assert_eq!(decimal_output(output), decimal("10"));
        let OutputValue::Scalar {
            name,
            id,
            dtype,
            unit,
            ..
        } = output
        else {
            panic!("parameter output is a scalar");
        };
        assert_eq!(name, "base_amount");
        assert_eq!(id.as_deref(), None);
        // Whole-number literals lower to integers; the output reports the
        // runtime dtype of the selected value.
        assert_eq!(*dtype, DTypeSpec::Integer);
        assert_eq!(unit.as_deref(), Some("USD"));
        assert_eq!(response.metadata.actual_mode, ExecutionMode::Explain);
        if mode == ExecutionMode::Fast {
            let reason = response
                .metadata
                .fallback_reason
                .clone()
                .expect("fast mode reports the parameter fallback");
            assert!(reason.contains("parameter output"));
        }
    }
}

#[test]
fn parameter_outputs_resolve_by_canonical_id_when_present() {
    let mut program =
        axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("RuleSpec lowers");
    let parameter = program
        .parameters
        .iter_mut()
        .find(|parameter| parameter.name == "base_amount")
        .expect("base amount parameter");
    parameter.id = Some("us:statutes/example/1#base_amount".to_string());
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: program.clone(),
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: simple_period(),
            outputs: vec!["us:statutes/example/1#base_amount".to_string()],
        }],
    })
    .expect("id-addressed parameter query succeeds");
    let output = response.results[0]
        .outputs
        .get("us:statutes/example/1#base_amount")
        .expect("output keyed by canonical id");
    assert_eq!(decimal_output(output), decimal("10"));
    let OutputValue::Scalar { id, .. } = output else {
        panic!("parameter output is a scalar");
    };
    assert_eq!(id.as_deref(), Some("us:statutes/example/1#base_amount"));

    // With an id present the bare name is no longer addressable, matching
    // derived-rule resolution.
    let error = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: simple_period(),
            outputs: vec!["base_amount".to_string()],
        }],
    })
    .expect_err("bare name is not addressable once an id exists");
    assert_eq!(error.to_string(), "unknown derived output: base_amount");
}

#[test]
fn indexed_parameters_are_not_directly_queryable() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: phase_in_rates
    kind: parameter
    dtype: Rate
    indexed_by: qualifying_child_count
    versions:
      - effective_from: 2026-01-01
        values:
          0: 0.0765
          1: 0.34
"#;
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let error = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: simple_period(),
            outputs: vec!["phase_in_rates".to_string()],
        }],
    })
    .expect_err("indexed parameters need a key expression");
    assert!(
        error
            .to_string()
            .contains("is indexed; query it through a derived rule"),
        "unexpected error: {error}"
    );
}

#[test]
fn parameter_outputs_without_a_covering_version_error() {
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("RuleSpec lowers");
    let error = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: PeriodSpec {
                kind: PeriodKindSpec::Month,
                start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
                end: chrono::NaiveDate::from_ymd_opt(2025, 1, 31).expect("valid date"),
            },
            outputs: vec!["base_amount".to_string()],
        }],
    })
    .expect_err("no version covers 2025");
    assert!(
        error.to_string().contains("base_amount"),
        "unexpected error: {error}"
    );
}

#[test]
fn unknown_query_outputs_still_error_with_parameters_present() {
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("RuleSpec lowers");
    let error = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: simple_period(),
            outputs: vec!["no_such_output".to_string()],
        }],
    })
    .expect_err("unknown outputs are rejected");
    assert_eq!(error.to_string(), "unknown derived output: no_such_output");
}

#[test]
fn mixed_parameter_and_derived_outputs_answer_in_one_query() {
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(SIMPLE_RULESPEC).expect("RuleSpec lowers");
    let period = simple_period();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            mode: mode.clone(),
            program: program.clone(),
            dataset: DatasetSpec {
                inputs: vec![InputRecordSpec {
                    name: "amount".to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: decimal_value("5"),
                }],
                relations: vec![],
            },
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "household-1".to_string(),
                period: period.clone(),
                outputs: vec!["adjusted_amount".to_string(), "base_amount".to_string()],
            }],
        })
        .expect("mixed query succeeds");
        let outputs = &response.results[0].outputs;
        assert_eq!(
            decimal_output(outputs.get("base_amount").expect("parameter")),
            decimal("10")
        );
        assert_eq!(
            decimal_output(outputs.get("adjusted_amount").expect("derived")),
            decimal("15")
        );
        assert_eq!(response.metadata.actual_mode, ExecutionMode::Explain);
    }
}

#[test]
fn parameter_output_serializes_the_exact_amount_row_shape() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: monthly_kindergeld_per_child
    kind: parameter
    dtype: Money
    unit: EUR
    versions:
      - effective_from: 2025-01-01
        formula: 255
      - effective_from: 2026-01-01
        formula: 259
"#;
    let mut program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    program
        .parameters
        .iter_mut()
        .find(|parameter| parameter.name == "monthly_kindergeld_per_child")
        .expect("amount parameter")
        .id = Some("de:statutes/estg/66#monthly_kindergeld_per_child".to_string());

    let query_for = |year: i32| ExecutionQuery {
        assessment_date: None,
        entity_id: "case-0::tax_unit".to_string(),
        period: PeriodSpec {
            kind: PeriodKindSpec::Month,
            start: chrono::NaiveDate::from_ymd_opt(year, 6, 1).expect("valid date"),
            end: chrono::NaiveDate::from_ymd_opt(year, 6, 30).expect("valid date"),
        },
        outputs: vec!["de:statutes/estg/66#monthly_kindergeld_per_child".to_string()],
    };

    // 2025 period selects the 255 version; the serialized row is the exact
    // shape the DE Kindergeld certificate premise validates.
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: program.clone(),
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![query_for(2025)],
    })
    .expect("2025 parameter query succeeds");
    assert!(response.results[0].trace.is_empty());
    let row = serde_json::to_value(
        response.results[0]
            .outputs
            .get("de:statutes/estg/66#monthly_kindergeld_per_child")
            .expect("amount output"),
    )
    .expect("output serializes");
    assert_eq!(
        row,
        serde_json::json!({
            "kind": "scalar",
            "name": "monthly_kindergeld_per_child",
            "id": "de:statutes/estg/66#monthly_kindergeld_per_child",
            "dtype": "integer",
            "unit": "EUR",
            "value": {"kind": "integer", "value": 255},
        })
    );

    // A 2026 period selects the later version: effective dating, not key
    // shape, drives the answer.
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![query_for(2026)],
    })
    .expect("2026 parameter query succeeds");
    let row = serde_json::to_value(
        response.results[0]
            .outputs
            .get("de:statutes/estg/66#monthly_kindergeld_per_child")
            .expect("amount output"),
    )
    .expect("output serializes");
    assert_eq!(
        row["value"],
        serde_json::json!({"kind": "integer", "value": 259})
    );
}

// ===========================================================================
// Fast/explain parity regressions from the PR #195 review (2026-09-25)
//
// An independent review of #195 found two requests on main at b16ced0 where
// fast mode answered and explain did not agree: a `relation_member` judgment
// inside a `where` clause (explain errors, fast counted every related entity),
// and a `count` in a decimal rule (explain reports an integer value, fast
// converted it to a decimal). Both still reproduced at 2840b57 and stopped at
// 5a29e03 (#201), which runs every relation aggregation on the reference
// interpreter and keeps each row's value in the kind explain computes. The
// tests below pin explain's semantics for both and for their neighbours, and
// assert that fast mode agrees: on its own path, without falling back, where
// explain answers, and with explain's exact error (which fast reports by
// handing a failing request to explain) where explain fails.
//
// Requests are written as the JSON the CLI reads, so each one can be replayed
// with `axiom-rules-engine < request.json` after setting `mode`.
// ===========================================================================

/// A request over `dataset`'s relations and the inputs `program` reads. The
/// datasets below are shared across programs, and a request may not supply an
/// input its program never reads.
fn review_request(
    program: serde_json::Value,
    dataset: serde_json::Value,
    queries: serde_json::Value,
) -> serde_json::Value {
    fn read_inputs(value: &serde_json::Value, names: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let (Some(kind), Some(name)) = (object.get("kind"), object.get("name"))
                    && (kind == "input" || kind == "input_or_else")
                {
                    names.insert(name.as_str().expect("input name").to_string());
                }
                object.values().for_each(|value| read_inputs(value, names));
            }
            serde_json::Value::Array(items) => {
                items.iter().for_each(|value| read_inputs(value, names))
            }
            _ => {}
        }
    }
    let mut names = std::collections::BTreeSet::new();
    read_inputs(&program, &mut names);
    let mut dataset = dataset;
    if let Some(inputs) = dataset["inputs"].as_array_mut() {
        inputs.retain(|input| names.contains(input["name"].as_str().expect("input name")));
    }
    serde_json::json!({ "program": program, "dataset": dataset, "queries": queries })
}

fn review_period() -> serde_json::Value {
    serde_json::json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn review_interval() -> serde_json::Value {
    serde_json::json!({ "start": "2026-01-01", "end": "2026-01-31" })
}

fn review_query(entity_id: &str, outputs: &[&str]) -> serde_json::Value {
    serde_json::json!({ "entity_id": entity_id, "period": review_period(), "outputs": outputs })
}

fn review_rule(
    name: &str,
    entity: &str,
    dtype: &str,
    expr: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "entity": entity,
        "dtype": dtype,
        "unit": null,
        "semantics": "scalar",
        "expr": expr,
    })
}

fn review_judgment_rule(name: &str, entity: &str, expr: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "entity": entity,
        "dtype": "judgment",
        "unit": null,
        "semantics": "judgment",
        "expr": expr,
    })
}

fn review_input(
    name: &str,
    entity: &str,
    entity_id: &str,
    value: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "entity": entity,
        "entity_id": entity_id,
        "interval": review_interval(),
        "value": value,
    })
}

fn review_tuple(relation: &str, tuple: &[&str]) -> serde_json::Value {
    serde_json::json!({ "name": relation, "tuple": tuple, "interval": review_interval() })
}

fn review_decimal(value: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "decimal", "value": value })
}

fn review_integer(value: i64) -> serde_json::Value {
    serde_json::json!({ "kind": "integer", "value": value })
}

fn review_literal(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "kind": "literal", "value": value })
}

fn review_comparison(
    left: serde_json::Value,
    op: &str,
    right: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

/// `member(household, person)`: the household is slot 0, the person slot 1.
fn review_member_predicate() -> serde_json::Value {
    serde_json::json!({
        "kind": "relation_member",
        "relation": "member",
        "current_slot": 0,
        "related_slot": 1,
    })
}

fn review_count_members(where_clause: Option<serde_json::Value>) -> serde_json::Value {
    let mut count = serde_json::json!({
        "kind": "count_related",
        "relation": "member",
        "current_slot": 0,
        "related_slot": 1,
    });
    if let Some(where_clause) = where_clause {
        count["where"] = where_clause;
    }
    count
}

/// A judgment that holds (`1 == 1`) or not (`1 == 2`) without reading data.
fn review_constant_judgment(holds: bool) -> serde_json::Value {
    review_comparison(
        review_literal(review_integer(1)),
        "eq",
        review_literal(review_integer(if holds { 1 } else { 2 })),
    )
}

fn review_mode_request(mode: ExecutionMode, request: &serde_json::Value) -> ExecutionRequest {
    let mut request = request.clone();
    request["mode"] = serde_json::to_value(&mode).expect("mode serialises");
    serde_json::from_value(request).expect("request JSON parses")
}

/// Explain and fast agree on `request`: when explain answers, fast answers
/// the same results (value kinds included) on its own path, without falling
/// back; when explain fails, fast fails with the same error. Returns explain's
/// results without traces, or its error message.
fn assert_fast_agrees_with_explain(
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let explain = execute_request(review_mode_request(ExecutionMode::Explain, request));
    let fast = execute_request(review_mode_request(ExecutionMode::Fast, request));
    match (explain, fast) {
        (Ok(explain), Ok(fast)) => {
            assert_eq!(
                (&fast.metadata.actual_mode, &fast.metadata.fallback_reason),
                (&ExecutionMode::Fast, &None),
                "fast mode fell back to explain on {request}"
            );
            let explain = results_without_trace(&explain);
            assert_eq!(
                results_without_trace(&fast),
                explain,
                "fast and explain differ on {request}"
            );
            Ok(explain)
        }
        (Err(explain), Err(fast)) => {
            assert_eq!(
                fast.to_string(),
                explain.to_string(),
                "fast and explain fail differently on {request}"
            );
            Err(explain.to_string())
        }
        (Ok(explain), Err(fast)) => panic!(
            "explain answered {} but fast failed with `{fast}` on {request}",
            results_without_trace(&explain)
        ),
        (Err(explain), Ok(fast)) => panic!(
            "explain failed with `{explain}` but fast answered {} on {request}",
            results_without_trace(&fast)
        ),
    }
}

/// Three members, all adults with income; enough for every where clause below
/// to reach a related entity.
fn review_members_dataset() -> serde_json::Value {
    let people = [("p1", "100"), ("p2", "0"), ("p3", "50")];
    serde_json::json!({
        "inputs": people
            .iter()
            .map(|(person, income)| review_input("income", "Person", person, review_decimal(income)))
            .collect::<Vec<_>>(),
        "relations": people
            .iter()
            .map(|(person, _)| review_tuple("member", &["h1", person]))
            .collect::<Vec<_>>(),
    })
}

fn review_member_relations() -> serde_json::Value {
    serde_json::json!([{ "name": "member", "arity": 2 }])
}

const RELATION_PREDICATE_OUTSIDE_DERIVED_RELATION: &str =
    "type mismatch: relation predicate `member` can only be evaluated inside a derived relation";

/// Explain evaluates a `count`/`sum` `where` clause once per related entity
/// with no relation context (engine.rs `ScalarExpr::CountRelated` and
/// `SumRelated` call `eval_judgment_expr`), and `JudgmentExpr::RelationMember`
/// fails without one: only a derived relation's predicate supplies it. So a
/// `relation_member` a `where` clause reaches is an error in explain, and fast
/// mode must fail the same way. At b16ced0 fast answered the first request
/// with a count of 3.
#[test]
fn relation_member_in_an_aggregation_where_clause_fails_in_fast_as_in_explain() {
    let member = review_member_predicate();
    let count_where = |where_clause: serde_json::Value| review_count_members(Some(where_clause));
    let income_above_zero = review_comparison(
        serde_json::json!({ "kind": "input", "name": "income" }),
        "gt",
        review_literal(review_decimal("0")),
    );
    let household = |expr: serde_json::Value| review_rule("n", "Household", "integer", expr);
    let cases = vec![
        // The reviewer's probe (t7.py, "PRE relation_member in where clause").
        (
            "count where relation_member",
            vec![household(count_where(member.clone()))],
            vec![review_query("h1", &["n"])],
        ),
        (
            "sum where relation_member",
            vec![review_rule(
                "n",
                "Household",
                "decimal",
                serde_json::json!({
                    "kind": "sum_related",
                    "relation": "member",
                    "current_slot": 0,
                    "related_slot": 1,
                    "value": { "kind": "input", "name": "income" },
                    "where": member.clone(),
                }),
            )],
            vec![review_query("h1", &["n"])],
        ),
        (
            "relation_member after a held `and` item",
            vec![household(count_where(serde_json::json!({
                "kind": "and",
                "items": [income_above_zero.clone(), member.clone()],
            })))],
            vec![review_query("h1", &["n"])],
        ),
        (
            "relation_member after an unheld `or` item",
            vec![household(count_where(serde_json::json!({
                "kind": "or",
                "items": [review_constant_judgment(false), member.clone()],
            })))],
            vec![review_query("h1", &["n"])],
        ),
        (
            "negated relation_member",
            vec![household(count_where(
                serde_json::json!({ "kind": "not", "item": member.clone() }),
            ))],
            vec![review_query("h1", &["n"])],
        ),
        (
            "relation_member inside exactly_one",
            vec![household(count_where(serde_json::json!({
                "kind": "exactly_one",
                "items": [member.clone(), income_above_zero.clone()],
            })))],
            vec![review_query("h1", &["n"])],
        ),
        (
            "relation_member through a person judgment rule",
            vec![
                review_judgment_rule("is_member", "Person", member.clone()),
                household(count_where(serde_json::json!({
                    "kind": "derived",
                    "name": "is_member",
                }))),
            ],
            vec![review_query("h1", &["n"])],
        ),
        (
            "relation_member under a household judgment",
            vec![review_judgment_rule(
                "has_members",
                "Household",
                review_comparison(
                    count_where(member.clone()),
                    "gt",
                    review_literal(review_integer(0)),
                ),
            )],
            vec![review_query("h1", &["has_members"])],
        ),
        (
            "relation_member queried directly",
            vec![review_judgment_rule("is_member", "Person", member.clone())],
            vec![review_query("p1", &["is_member"])],
        ),
        (
            // Only the second row reaches the where clause (h2 has no
            // members), so explain fails there and fast must fail too.
            "one row of a batch reaches it",
            vec![household(count_where(member.clone()))],
            vec![
                review_query("h2", &["n"]),
                review_query("h1", &["n"]),
                review_query("h2", &["n"]),
            ],
        ),
    ];
    for (label, derived, queries) in cases {
        let request = review_request(
            serde_json::json!({ "relations": review_member_relations(), "derived": derived }),
            review_members_dataset(),
            serde_json::Value::Array(queries),
        );
        let error = assert_fast_agrees_with_explain(&request).expect_err(&format!(
            "{label}: explain must reject the relation predicate"
        ));
        assert_eq!(
            error, RELATION_PREDICATE_OUTSIDE_DERIVED_RELATION,
            "{label}"
        );
    }
}

/// The error is a property of evaluation, not of the program: a `where` clause
/// explain never evaluates (no related entities, a decided `or`, an `if` branch
/// no row selects) cannot fail, and fast mode answers those requests itself.
#[test]
fn relation_member_a_where_clause_never_reaches_fails_neither_mode() {
    let member = review_member_predicate();
    let cases = vec![
        (
            "no related entities",
            review_count_members(Some(member.clone())),
            vec![
                review_query("h2", &["n"]),
                review_query("h3", &["n"]),
                review_query("h2", &["n"]),
            ],
            vec![0, 0, 0],
        ),
        (
            "an `or` decided before it",
            review_count_members(Some(serde_json::json!({
                "kind": "or",
                "items": [review_constant_judgment(true), member.clone()],
            }))),
            vec![review_query("h1", &["n"]), review_query("h2", &["n"])],
            vec![3, 0],
        ),
        (
            "an `if` branch no row selects",
            serde_json::json!({
                "kind": "if",
                "condition": review_constant_judgment(false),
                "then_expr": review_count_members(Some(member.clone())),
                "else_expr": review_count_members(None),
            }),
            vec![review_query("h1", &["n"]), review_query("h2", &["n"])],
            vec![3, 0],
        ),
    ];
    for (label, expr, queries, expected) in cases {
        let request = review_request(
            serde_json::json!({
                "relations": review_member_relations(),
                "derived": [review_rule("n", "Household", "integer", expr)],
            }),
            review_members_dataset(),
            serde_json::Value::Array(queries),
        );
        let results = assert_fast_agrees_with_explain(&request)
            .unwrap_or_else(|error| panic!("{label}: explain failed: {error}"));
        let values = results
            .as_array()
            .expect("results are an array")
            .iter()
            .map(|result| result["outputs"]["n"]["value"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            expected.into_iter().map(review_integer).collect::<Vec<_>>(),
            "{label}"
        );
    }
}

/// Inside a derived relation's predicate `relation_member` has its context and
/// is evaluated: `adult_resident` keeps the members that are also residents
/// and adults. Fast mode answers counts and sums over it, under integer,
/// decimal and judgment rules, exactly as explain does.
#[test]
fn relation_member_in_a_derived_relation_predicate_is_answered_natively_like_explain() {
    let adult_resident = |slot_entities: bool| {
        let mut relation = serde_json::json!({
            "name": "adult_resident",
            "arity": 2,
            "derivation": {
                "source_relation": "member",
                "current_slot": 0,
                "related_slot": 1,
                "predicate": {
                    "kind": "and",
                    "items": [
                        {
                            "kind": "relation_member",
                            "relation": "resident",
                            "current_slot": 0,
                            "related_slot": 1,
                        },
                        { "kind": "derived", "name": "is_adult" },
                    ],
                },
            },
        });
        if slot_entities {
            relation["slot_entities"] = serde_json::json!(["Household", "Person"]);
            relation["derivation"]["slot_entities"] = serde_json::json!(["Household", "Person"]);
        }
        relation
    };
    let over_adult_residents = |kind: &str| {
        let mut expr = serde_json::json!({
            "kind": kind,
            "relation": "adult_resident",
            "current_slot": 0,
            "related_slot": 1,
        });
        if kind == "sum_related" {
            expr["value"] = serde_json::json!({ "kind": "input", "name": "income" });
        }
        expr
    };
    let derived = serde_json::json!([
        review_judgment_rule(
            "is_adult",
            "Person",
            review_comparison(
                serde_json::json!({
                    "kind": "input_or_else",
                    "name": "age",
                    "default": review_integer(0),
                }),
                "gte",
                review_literal(review_integer(18)),
            ),
        ),
        review_rule(
            "n",
            "Household",
            "integer",
            over_adult_residents("count_related")
        ),
        review_rule(
            "n_decimal",
            "Household",
            "decimal",
            over_adult_residents("count_related")
        ),
        review_rule(
            "income",
            "Household",
            "decimal",
            over_adult_residents("sum_related")
        ),
        review_judgment_rule(
            "any",
            "Household",
            review_comparison(
                over_adult_residents("count_related"),
                "gt",
                review_literal(review_integer(0)),
            ),
        ),
    ]);
    // p1 is an adult resident; p2 an adult who is not a resident; p3 a
    // resident child; p4 an adult resident of h2 with no age on record.
    let dataset = serde_json::json!({
        "inputs": [
            review_input("age", "Person", "p1", review_integer(30)),
            review_input("age", "Person", "p2", review_integer(40)),
            review_input("age", "Person", "p3", review_integer(10)),
            review_input("income", "Person", "p1", review_decimal("100")),
            review_input("income", "Person", "p2", review_decimal("200")),
            review_input("income", "Person", "p3", review_decimal("5")),
            review_input("income", "Person", "p4", review_decimal("7")),
        ],
        "relations": [
            review_tuple("member", &["h1", "p1"]),
            review_tuple("member", &["h1", "p2"]),
            review_tuple("member", &["h1", "p3"]),
            review_tuple("member", &["h2", "p4"]),
            review_tuple("resident", &["h1", "p1"]),
            review_tuple("resident", &["h1", "p3"]),
            review_tuple("resident", &["h2", "p4"]),
        ],
    });
    let outputs = ["n", "n_decimal", "income", "any"];
    for slot_entities in [false, true] {
        let request = review_request(
            serde_json::json!({
                "relations": [
                    { "name": "member", "arity": 2 },
                    { "name": "resident", "arity": 2 },
                    adult_resident(slot_entities),
                ],
                "derived": derived,
            }),
            dataset.clone(),
            serde_json::json!([
                review_query("h1", &outputs),
                review_query("h2", &outputs),
                review_query("h3", &outputs),
                review_query("h1", &["n"]),
            ]),
        );
        let results = assert_fast_agrees_with_explain(&request)
            .unwrap_or_else(|error| panic!("explain failed: {error}"));
        let h1 = &results[0]["outputs"];
        assert_eq!(h1["n"]["value"], review_integer(1));
        assert_eq!(h1["n_decimal"]["dtype"], "decimal");
        assert_eq!(h1["n_decimal"]["value"], review_integer(1));
        assert_eq!(h1["income"]["value"], review_decimal("100"));
        assert_eq!(h1["any"]["outcome"], "holds");
        // p4 has no age, so `is_adult` reads the default and does not hold.
        assert_eq!(results[1]["outputs"]["n"]["value"], review_integer(0));
        assert_eq!(results[2]["outputs"]["any"]["outcome"], "not_holds");
    }
}

/// The reviewer's probe (t9.py): a `count` in a rule declared `decimal`.
/// Explain reports the declared dtype and the value its expression computes,
/// an integer; at b16ced0 fast converted the value to a decimal.
#[test]
fn count_related_in_a_decimal_rule_reports_explains_integer_value_in_fast_mode() {
    let people = [("person-1", "100"), ("person-2", "0"), ("person-3", "50")];
    let request = review_request(
        serde_json::json!({
            "relations": [{ "name": "member_of_household", "arity": 2 }],
            "derived": [review_rule(
                "earning_members",
                "Household",
                "decimal",
                serde_json::json!({
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": review_comparison(
                        serde_json::json!({ "kind": "input", "name": "income" }),
                        "gt",
                        review_literal(review_decimal("0")),
                    ),
                }),
            )],
        }),
        serde_json::json!({
            "inputs": people
                .iter()
                .map(|(person, income)| review_input("income", "Person", person, review_decimal(income)))
                .collect::<Vec<_>>(),
            "relations": people
                .iter()
                .map(|(person, _)| review_tuple("member_of_household", &[person, "household-1"]))
                .collect::<Vec<_>>(),
        }),
        serde_json::json!([
            review_query("household-1", &["earning_members"]),
            review_query("household-1", &["earning_members"]),
        ]),
    );
    let results = assert_fast_agrees_with_explain(&request).expect("explain answers");
    for result in results.as_array().expect("results are an array") {
        assert_eq!(
            result["outputs"]["earning_members"],
            serde_json::json!({
                "kind": "scalar",
                "name": "earning_members",
                "dtype": "decimal",
                "unit": null,
                "value": { "kind": "integer", "value": 2 },
            })
        );
    }
}

/// Explain never converts a rule's value to its declared dtype
/// (`Engine::evaluate_scalar` caches what the expression returns and
/// `execute_explain` serialises it as is), so the value kind is a function of
/// the expression and, for an `if`, of the branch each row takes. Fast mode
/// must report the same kind for every declared dtype, every row.
#[test]
fn fast_reports_explains_value_kind_under_every_declared_dtype() {
    let count = review_count_members(None);
    let income = serde_json::json!({ "kind": "input", "name": "income" });
    let sum = |value: serde_json::Value| {
        serde_json::json!({
            "kind": "sum_related",
            "relation": "member",
            "current_slot": 0,
            "related_slot": 1,
            "value": value,
        })
    };
    let expressions = vec![
        ("count", count.clone()),
        (
            "count where",
            review_count_members(Some(review_comparison(
                income.clone(),
                "gt",
                review_literal(review_decimal("0")),
            ))),
        ),
        ("sum of decimals", sum(income.clone())),
        (
            "sum of integers",
            sum(serde_json::json!({ "kind": "input", "name": "units" })),
        ),
        ("integer literal", review_literal(review_integer(7))),
        ("decimal literal", review_literal(review_decimal("7"))),
        (
            "integer input",
            serde_json::json!({ "kind": "input", "name": "size" }),
        ),
        (
            "decimal input",
            serde_json::json!({ "kind": "input", "name": "rent" }),
        ),
        (
            "absent input's integer default",
            serde_json::json!({
                "kind": "input_or_else",
                "name": "absent",
                "default": review_integer(0),
            }),
        ),
        (
            "counts added",
            serde_json::json!({ "kind": "add", "items": [count.clone(), count.clone()] }),
        ),
        (
            "max of a count",
            serde_json::json!({ "kind": "max", "items": [count.clone()] }),
        ),
        (
            "integer table keyed by a count",
            serde_json::json!({
                "kind": "parameter_lookup",
                "parameter": "per_member",
                "index": count.clone(),
            }),
        ),
        (
            // h1 (three members) takes the integer branch; h2 and h3 (none)
            // take the decimal one.
            "if with an integer and a decimal branch",
            serde_json::json!({
                "kind": "if",
                "condition": review_comparison(
                    count.clone(),
                    "gt",
                    review_literal(review_integer(1)),
                ),
                "then_expr": count.clone(),
                "else_expr": review_literal(review_decimal("0.5")),
            }),
        ),
        (
            "reference to an integer count rule",
            serde_json::json!({ "kind": "derived", "name": "member_count" }),
        ),
    ];
    let households = ["h1", "h2", "h3"];
    let mut inputs = vec![
        review_input("income", "Person", "p1", review_decimal("100")),
        review_input("income", "Person", "p2", review_decimal("0")),
        review_input("income", "Person", "p3", review_decimal("50")),
    ];
    for person in ["p1", "p2", "p3"] {
        inputs.push(review_input("units", "Person", person, review_integer(2)));
    }
    for household in households {
        inputs.push(review_input(
            "size",
            "Household",
            household,
            review_integer(3),
        ));
        inputs.push(review_input(
            "rent",
            "Household",
            household,
            review_decimal("12.5"),
        ));
    }
    let members = ["p1", "p2", "p3"]
        .iter()
        .map(|person| review_tuple("member", &["h1", person]))
        .collect::<Vec<_>>();
    let dataset = serde_json::json!({ "inputs": inputs, "relations": members });
    let parameters = serde_json::json!([{
        "name": "per_member",
        "unit": null,
        "indexed_by": "members",
        "versions": [{
            "effective_from": "2020-01-01",
            "values": { "0": review_integer(0), "3": review_integer(30) },
        }],
    }]);
    let mut kinds_seen = std::collections::BTreeSet::new();
    for dtype in ["integer", "decimal", "bool", "text", "date", "judgment"] {
        for rounding in [None, Some("half_up")] {
            for (label, expr) in &expressions {
                let mut rule = review_rule("value", "Household", dtype, expr.clone());
                if let Some(rounding) = rounding {
                    // Rounding applies only to a currency unit, and rounds
                    // decimal values only; an integer passes through.
                    rule["rounding"] = serde_json::json!(rounding);
                    rule["unit"] = serde_json::json!("USD");
                }
                let request = review_request(
                    serde_json::json!({
                        "units": [{ "name": "USD", "kind": "currency", "minor_units": 0 }],
                        "relations": review_member_relations(),
                        "parameters": parameters,
                        "derived": [
                            rule,
                            review_rule("member_count", "Household", "integer", count.clone()),
                            // Referenced on other rows, so the batch reads
                            // `value` both as an output and as a dependency.
                            review_rule(
                                "value_again",
                                "Household",
                                "decimal",
                                serde_json::json!({ "kind": "derived", "name": "value" }),
                            ),
                        ],
                    }),
                    dataset.clone(),
                    serde_json::json!([
                        review_query("h1", &["value"]),
                        review_query("h2", &["value_again"]),
                        review_query("h3", &["value", "value_again"]),
                        review_query("h1", &["value_again", "value"]),
                    ]),
                );
                let results = assert_fast_agrees_with_explain(&request)
                    .unwrap_or_else(|error| panic!("{dtype} {label}: explain failed: {error}"));
                for result in results.as_array().expect("results are an array") {
                    for output in result["outputs"].as_object().expect("outputs").values() {
                        kinds_seen.insert((
                            output["dtype"].as_str().unwrap_or_default().to_string(),
                            output["value"]["kind"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        ));
                    }
                }
            }
        }
    }
    // The matrix exercises both directions of the dropped conversion: integer
    // values under non-integer dtypes and decimal values under `integer`.
    for (dtype, kind) in [
        ("decimal", "integer"),
        ("bool", "integer"),
        ("text", "integer"),
        ("integer", "decimal"),
    ] {
        assert!(
            kinds_seen.contains(&(dtype.to_string(), kind.to_string())),
            "no {kind} value was reported under dtype {dtype}: {kinds_seen:?}"
        );
    }
}
