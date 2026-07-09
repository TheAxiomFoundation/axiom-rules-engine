use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse, OutputValue,
    execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::JudgmentOutcome;
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec,
    IndexedParameterSpec, InputRecordSpec, IntervalSpec, JudgmentExprSpec, JudgmentOutcomeSpec,
    ParameterVersionSpec, PeriodKindSpec, PeriodSpec, ProgramSpec, RelationRecordSpec,
    RelationSpec, ScalarExprSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

const ENTITY: &str = "Household";
const OUTPUT: &str = "result";

#[test]
fn execution_mode_parity_batch_matches_concatenated_singletons() {
    let program = scalar_program(ScalarExprSpec::Add {
        items: vec![
            input("amount"),
            ScalarExprSpec::Literal {
                value: decimal_value("10"),
            },
        ],
    });
    let entity_ids = ["household-1", "household-2", "household-3"];
    let amounts = [decimal("5"), decimal("12.5"), decimal("0")];
    let dataset = DatasetSpec {
        inputs: entity_ids
            .iter()
            .zip(amounts)
            .map(|(entity_id, amount)| input_record("amount", entity_id, amount))
            .collect(),
        relations: Vec::new(),
    };
    let queries = entity_ids
        .iter()
        .map(|entity_id| query(entity_id, &[OUTPUT]))
        .collect::<Vec<_>>();

    let mut sparse_mode_values = Vec::new();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let batch = run_sparse(mode.clone(), &program, &dataset, &queries)
            .unwrap_or_else(|error| panic!("{mode:?} batch succeeds: {error}"));
        if mode == ExecutionMode::Fast {
            assert_fast_path(&batch);
        }

        let mut singleton_results = Vec::new();
        for singleton_query in &queries {
            let singleton = run_sparse(
                mode.clone(),
                &program,
                &dataset,
                std::slice::from_ref(singleton_query),
            )
            .unwrap_or_else(|error| panic!("{mode:?} singleton succeeds: {error}"));
            if mode == ExecutionMode::Fast {
                assert_fast_path(&singleton);
            }
            singleton_results.extend(primary_results(&singleton));
        }

        assert_eq!(primary_results(&batch), singleton_results);
        sparse_mode_values.push(sparse_decimal_values(&batch, OUTPUT));
    }
    assert_eq!(sparse_mode_values[0], sparse_mode_values[1]);

    let dense = compile_dense(&program);
    let dense_batch = dense
        .execute(
            &period().to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: amounts.len(),
                inputs: HashMap::from([(
                    "amount".to_string(),
                    DenseColumn::Decimal(amounts.to_vec()),
                )]),
                relations: HashMap::new(),
            },
            &[OUTPUT.to_string()],
        )
        .expect("dense batch succeeds");
    let dense_batch_values = dense_decimal_values(&dense_batch, OUTPUT);
    let dense_singleton_values = amounts
        .into_iter()
        .flat_map(|amount| {
            let singleton = dense
                .execute(
                    &period().to_model().expect("period converts"),
                    DenseBatchSpec {
                        row_count: 1,
                        inputs: HashMap::from([(
                            "amount".to_string(),
                            DenseColumn::Decimal(vec![amount]),
                        )]),
                        relations: HashMap::new(),
                    },
                    &[OUTPUT.to_string()],
                )
                .expect("dense singleton succeeds");
            dense_decimal_values(&singleton, OUTPUT)
        })
        .collect::<Vec<_>>();

    assert_eq!(dense_batch_values, dense_singleton_values);
    assert_eq!(sparse_mode_values[0], dense_batch_values);
}

// Divergence: Explain skips the inactive `if` branch, while Fast and dense eagerly evaluate it
// and report DivisionByZero even though the condition selects the other branch.
#[test]
#[ignore]
fn execution_mode_parity_short_circuit_if_skips_inactive_error() {
    let program = scalar_program(ScalarExprSpec::If {
        condition: Box::new(comparison(
            input("divisor"),
            ComparisonOpSpec::Eq,
            integer_literal(0),
        )),
        then_expr: Box::new(integer_literal(42)),
        else_expr: Box::new(divide(integer_literal(100), input("divisor"))),
    });
    let dataset = divisor_dataset();
    let queries = vec![query("household-1", &[OUTPUT])];

    let explain = run_sparse(ExecutionMode::Explain, &program, &dataset, &queries)
        .expect("Explain skips the inactive division by zero");
    let fast = run_sparse(ExecutionMode::Fast, &program, &dataset, &queries);
    let dense = run_dense_with_divisor(&program, 0);

    assert_eq!(sparse_decimal_values(&explain, OUTPUT), vec![decimal("42")]);
    assert!(
        fast.is_ok() && dense.is_ok(),
        "Fast and dense must skip the inactive division-by-zero branch; Fast={fast:?}, dense={dense:?}"
    );
    let fast = fast.expect("Fast succeeds");
    let dense = dense.expect("dense succeeds");
    assert_fast_path(&fast);
    assert_eq!(sparse_decimal_values(&fast, OUTPUT), vec![decimal("42")]);
    assert_eq!(dense_decimal_values(&dense, OUTPUT), vec![decimal("42")]);
}

// Divergence: Explain stops `and` after NotHolds, while Fast and dense evaluate the remaining
// operand and report DivisionByZero.
#[test]
#[ignore]
fn execution_mode_parity_short_circuit_and_skips_inactive_error() {
    let program = judgment_program(JudgmentExprSpec::And {
        items: vec![
            comparison(input("divisor"), ComparisonOpSpec::Ne, integer_literal(0)),
            comparison(
                divide(integer_literal(100), input("divisor")),
                ComparisonOpSpec::Gt,
                integer_literal(1),
            ),
        ],
    });
    assert_short_circuit_judgment(program, JudgmentOutcomeSpec::NotHolds);
}

// Divergence: Explain stops `or` after Holds, while Fast and dense evaluate the remaining operand
// and report DivisionByZero.
#[test]
#[ignore]
fn execution_mode_parity_short_circuit_or_skips_inactive_error() {
    let program = judgment_program(JudgmentExprSpec::Or {
        items: vec![
            comparison(input("divisor"), ComparisonOpSpec::Eq, integer_literal(0)),
            comparison(
                divide(integer_literal(100), input("divisor")),
                ComparisonOpSpec::Gt,
                integer_literal(1),
            ),
        ],
    });
    assert_short_circuit_judgment(program, JudgmentOutcomeSpec::Holds);
}

// Divergence: Explain selects the covering input with the greatest interval start, while Fast
// selects the last covering record in DatasetSpec order. Dense accepts pre-resolved columns and
// therefore has no interval-resolution surface to exercise for this fixture.
#[test]
#[ignore]
fn execution_mode_parity_overlapping_inputs_choose_latest_start() {
    let program = scalar_program(input("amount"));
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "amount".to_string(),
                entity: ENTITY.to_string(),
                entity_id: "household-1".to_string(),
                interval: interval("2025-07-01", "2026-12-31"),
                value: decimal_value("20"),
            },
            InputRecordSpec {
                name: "amount".to_string(),
                entity: ENTITY.to_string(),
                entity_id: "household-1".to_string(),
                interval: interval("2025-01-01", "2026-12-31"),
                value: decimal_value("10"),
            },
        ],
        relations: Vec::new(),
    };
    let queries = vec![query("household-1", &[OUTPUT])];

    let explain =
        run_sparse(ExecutionMode::Explain, &program, &dataset, &queries).expect("Explain succeeds");
    let fast =
        run_sparse(ExecutionMode::Fast, &program, &dataset, &queries).expect("Fast succeeds");
    assert_fast_path(&fast);

    let expected = vec![decimal("20")];
    assert_eq!(sparse_decimal_values(&explain, OUTPUT), expected);
    assert_eq!(sparse_decimal_values(&fast, OUTPUT), expected);
}

// Divergence: Explain rejects a fractional table index, while Fast and canonical Decimal dense
// truncate 1.5 toward zero and return the value for key 1.
#[test]
#[ignore]
fn execution_mode_parity_non_integral_table_indices_are_rejected() {
    let program = ProgramSpec {
        parameters: vec![IndexedParameterSpec {
            id: None,
            name: "amounts".to_string(),
            unit: None,
            indexed_by: Some("bracket".to_string()),
            source: None,
            source_url: None,
            corpus_citation_path: None,
            versions: vec![ParameterVersionSpec {
                effective_from: date("2026-01-01"),
                values: BTreeMap::from([(1, ScalarValueSpec::Integer { value: 100 })]),
            }],
        }],
        derived: vec![scalar_derived(
            DTypeSpec::Integer,
            ScalarExprSpec::ParameterLookup {
                parameter: "amounts".to_string(),
                index: Box::new(input("bracket")),
            },
        )],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![input_record("bracket", "household-1", decimal("1.5"))],
        relations: Vec::new(),
    };
    let queries = vec![query("household-1", &[OUTPUT])];

    let explain = run_sparse(ExecutionMode::Explain, &program, &dataset, &queries);
    let fast = run_sparse(ExecutionMode::Fast, &program, &dataset, &queries);
    let dense = compile_dense(&program).execute(
        &period().to_model().expect("period converts"),
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([(
                "bracket".to_string(),
                DenseColumn::Decimal(vec![decimal("1.5")]),
            )]),
            relations: HashMap::new(),
        },
        &[OUTPUT.to_string()],
    );

    assert!(
        api_type_mismatch(&explain, "parameter key for `amounts` must be an integer")
            && api_type_mismatch(&fast, "parameter key for bulk lookup must be integral")
            && dense_type_mismatch(&dense, "parameter key for dense lookup must be integral"),
        "all modes must reject the fractional index with their integral-key TypeMismatch; Explain={explain:?}, Fast={fast:?}, dense={dense:?}"
    );
}

#[test]
fn execution_mode_parity_duplicate_relation_rows_match_sparse_modes() {
    let (program, dataset, queries) = duplicate_relation_fixture();
    let explain =
        run_sparse(ExecutionMode::Explain, &program, &dataset, &queries).expect("Explain succeeds");
    let fast =
        run_sparse(ExecutionMode::Fast, &program, &dataset, &queries).expect("Fast succeeds");
    assert_fast_path(&fast);

    assert_eq!(sparse_integer_values(&explain, OUTPUT), vec![1]);
    assert_eq!(sparse_integer_values(&fast, OUTPUT), vec![1]);
    // DenseBatchSpec has offsets and formula inputs but no relation tuple identities. It cannot
    // distinguish these exact duplicate tuples from two distinct related rows, so dense is not an
    // exercisable comparison surface for this fixture.
}

// Divergence: Fast unions heterogeneous requested outputs and evaluates every output for every
// row, so a combined batch reports a missing input that neither singleton request needs. Explain
// evaluates only each query's outputs. Dense has one output list for the whole batch and cannot
// express per-row output selections.
#[test]
#[ignore]
fn execution_mode_parity_heterogeneous_batch_matches_concatenated_singletons() {
    let program = ProgramSpec {
        derived: vec![
            derived(
                "amount_result",
                DTypeSpec::Decimal,
                DerivedSemanticsSpec::Scalar {
                    expr: input("amount"),
                },
            ),
            derived(
                "flag_result",
                DTypeSpec::Bool,
                DerivedSemanticsSpec::Scalar {
                    expr: input("flag"),
                },
            ),
        ],
        ..ProgramSpec::default()
    };
    let dataset = DatasetSpec {
        inputs: vec![
            input_record("amount", "household-1", decimal("25")),
            InputRecordSpec {
                name: "flag".to_string(),
                entity: ENTITY.to_string(),
                entity_id: "household-2".to_string(),
                interval: period_interval(),
                value: ScalarValueSpec::Bool { value: true },
            },
        ],
        relations: Vec::new(),
    };
    let queries = vec![
        query("household-1", &["amount_result"]),
        query("household-2", &["flag_result"]),
    ];

    let explain_batch = run_sparse(ExecutionMode::Explain, &program, &dataset, &queries)
        .expect("Explain batch succeeds");
    let explain_singletons = run_singletons(ExecutionMode::Explain, &program, &dataset, &queries)
        .expect("Explain singletons succeed");
    assert_eq!(primary_results(&explain_batch), explain_singletons);

    let fast_singletons = run_singletons(ExecutionMode::Fast, &program, &dataset, &queries)
        .expect("Fast singletons succeed");
    let fast_batch = run_sparse(ExecutionMode::Fast, &program, &dataset, &queries);
    assert!(
        fast_batch.is_ok(),
        "Fast batch must not evaluate outputs a row did not request: {fast_batch:?}"
    );
    let fast_batch = fast_batch.expect("Fast batch succeeds");
    assert_fast_path(&fast_batch);
    assert_eq!(primary_results(&fast_batch), fast_singletons);
    assert_eq!(
        primary_results(&explain_batch),
        primary_results(&fast_batch)
    );
}

fn assert_short_circuit_judgment(program: ProgramSpec, expected: JudgmentOutcomeSpec) {
    let dataset = divisor_dataset();
    let queries = vec![query("household-1", &[OUTPUT])];
    let explain = run_sparse(ExecutionMode::Explain, &program, &dataset, &queries)
        .expect("Explain short-circuits");
    let fast = run_sparse(ExecutionMode::Fast, &program, &dataset, &queries);
    let dense = run_dense_with_divisor(&program, 0);

    assert_eq!(sparse_judgment_values(&explain, OUTPUT), vec![expected]);
    assert!(
        fast.is_ok() && dense.is_ok(),
        "Fast and dense must skip the inactive division-by-zero operand; Fast={fast:?}, dense={dense:?}"
    );
    let fast = fast.expect("Fast succeeds");
    let dense = dense.expect("dense succeeds");
    assert_fast_path(&fast);
    assert_eq!(sparse_judgment_values(&fast, OUTPUT), vec![expected]);
    assert_eq!(dense_judgment_values(&dense, OUTPUT), vec![expected]);
}

fn duplicate_relation_fixture() -> (ProgramSpec, DatasetSpec, Vec<ExecutionQuery>) {
    let relation = RelationRecordSpec {
        name: "members".to_string(),
        tuple: vec!["person-1".to_string(), "household-1".to_string()],
        interval: period_interval(),
    };
    (
        ProgramSpec {
            relations: vec![RelationSpec {
                name: "members".to_string(),
                arity: 2,
                derivation: None,
            }],
            derived: vec![scalar_derived(
                DTypeSpec::Integer,
                ScalarExprSpec::CountRelated {
                    relation: "members".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    where_clause: None,
                },
            )],
            ..ProgramSpec::default()
        },
        DatasetSpec {
            inputs: Vec::new(),
            relations: vec![relation.clone(), relation],
        },
        vec![query("household-1", &[OUTPUT])],
    )
}

fn scalar_program(expr: ScalarExprSpec) -> ProgramSpec {
    ProgramSpec {
        derived: vec![scalar_derived(DTypeSpec::Decimal, expr)],
        ..ProgramSpec::default()
    }
}

fn judgment_program(expr: JudgmentExprSpec) -> ProgramSpec {
    ProgramSpec {
        derived: vec![derived(
            OUTPUT,
            DTypeSpec::Judgment,
            DerivedSemanticsSpec::Judgment { expr },
        )],
        ..ProgramSpec::default()
    }
}

fn scalar_derived(dtype: DTypeSpec, expr: ScalarExprSpec) -> DerivedSpec {
    derived(OUTPUT, dtype, DerivedSemanticsSpec::Scalar { expr })
}

fn derived(name: &str, dtype: DTypeSpec, semantics: DerivedSemanticsSpec) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: ENTITY.to_string(),
        dtype,
        unit: None,
        period: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: Vec::new(),
    }
}

fn input(name: &str) -> ScalarExprSpec {
    ScalarExprSpec::Input {
        name: name.to_string(),
    }
}

fn integer_literal(value: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Integer { value },
    }
}

fn divide(left: ScalarExprSpec, right: ScalarExprSpec) -> ScalarExprSpec {
    ScalarExprSpec::Div {
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn comparison(
    left: ScalarExprSpec,
    op: ComparisonOpSpec,
    right: ScalarExprSpec,
) -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

fn divisor_dataset() -> DatasetSpec {
    DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "divisor".to_string(),
            entity: ENTITY.to_string(),
            entity_id: "household-1".to_string(),
            interval: period_interval(),
            value: ScalarValueSpec::Integer { value: 0 },
        }],
        relations: Vec::new(),
    }
}

fn input_record(name: &str, entity_id: &str, value: Decimal) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: ENTITY.to_string(),
        entity_id: entity_id.to_string(),
        interval: period_interval(),
        value: ScalarValueSpec::Decimal {
            value: value.normalize().to_string(),
        },
    }
}

fn query(entity_id: &str, outputs: &[&str]) -> ExecutionQuery {
    ExecutionQuery {
        assessment_date: None,
        entity_id: entity_id.to_string(),
        period: period(),
        outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
    }
}

fn run_sparse(
    mode: ExecutionMode,
    program: &ProgramSpec,
    dataset: &DatasetSpec,
    queries: &[ExecutionQuery],
) -> Result<ExecutionResponse, ApiError> {
    execute_request(ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.to_vec(),
    })
}

fn run_singletons(
    mode: ExecutionMode,
    program: &ProgramSpec,
    dataset: &DatasetSpec,
    queries: &[ExecutionQuery],
) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut results = Vec::new();
    for singleton_query in queries {
        let response = run_sparse(
            mode.clone(),
            program,
            dataset,
            std::slice::from_ref(singleton_query),
        )?;
        if mode == ExecutionMode::Fast {
            assert_fast_path(&response);
        }
        results.extend(primary_results(&response));
    }
    Ok(results)
}

fn compile_dense(program: &ProgramSpec) -> DenseCompiledProgram {
    let artifact = CompiledProgramArtifact::compile(program.clone()).expect("program compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(ENTITY))
        .expect("dense compilation succeeds")
}

fn run_dense_with_divisor(
    program: &ProgramSpec,
    divisor: i64,
) -> Result<DenseExecutionResult, axiom_rules_engine::engine::EvalError> {
    compile_dense(program).execute(
        &period().to_model().expect("period converts"),
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([("divisor".to_string(), DenseColumn::Integer(vec![divisor]))]),
            relations: HashMap::new(),
        },
        &[OUTPUT.to_string()],
    )
}

fn assert_fast_path(response: &ExecutionResponse) {
    assert_eq!(response.metadata.requested_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(response.metadata.fallback_reason, None);
}

fn api_type_mismatch(result: &Result<ExecutionResponse, ApiError>, expected_message: &str) -> bool {
    matches!(
        result,
        Err(ApiError::Eval(EvalError::TypeMismatch(message))) if message == expected_message
    )
}

fn dense_type_mismatch(
    result: &Result<DenseExecutionResult, EvalError>,
    expected_message: &str,
) -> bool {
    matches!(
        result,
        Err(EvalError::TypeMismatch(message)) if message == expected_message
    )
}

fn primary_results(response: &ExecutionResponse) -> Vec<serde_json::Value> {
    response
        .results
        .iter()
        .map(|result| {
            serde_json::json!({
                "entity_id": result.entity_id,
                "period": result.period,
                "assessment_date": result.assessment_date,
                "outputs": result.outputs,
            })
        })
        .collect()
}

fn sparse_decimal_values(response: &ExecutionResponse, output: &str) -> Vec<Decimal> {
    response
        .results
        .iter()
        .map(
            |result| match result.outputs.get(output).expect("output exists") {
                OutputValue::Scalar {
                    value: ScalarValueSpec::Integer { value },
                    ..
                } => Decimal::from(*value),
                OutputValue::Scalar {
                    value: ScalarValueSpec::Decimal { value },
                    ..
                } => decimal(value),
                other => panic!("expected numeric scalar output, got {other:?}"),
            },
        )
        .collect()
}

fn sparse_integer_values(response: &ExecutionResponse, output: &str) -> Vec<i64> {
    response
        .results
        .iter()
        .map(
            |result| match result.outputs.get(output).expect("output exists") {
                OutputValue::Scalar {
                    value: ScalarValueSpec::Integer { value },
                    ..
                } => *value,
                other => panic!("expected integer output, got {other:?}"),
            },
        )
        .collect()
}

fn sparse_judgment_values(response: &ExecutionResponse, output: &str) -> Vec<JudgmentOutcomeSpec> {
    response
        .results
        .iter()
        .map(
            |result| match result.outputs.get(output).expect("output exists") {
                OutputValue::Judgment { outcome, .. } => *outcome,
                other => panic!("expected judgment output, got {other:?}"),
            },
        )
        .collect()
}

fn dense_decimal_values(result: &DenseExecutionResult, output: &str) -> Vec<Decimal> {
    match result.outputs.get(output).expect("dense output exists") {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
            values.iter().copied().map(Decimal::from).collect()
        }
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values.clone(),
        other => panic!("expected dense numeric scalar output, got {other:?}"),
    }
}

fn dense_judgment_values(result: &DenseExecutionResult, output: &str) -> Vec<JudgmentOutcomeSpec> {
    match result.outputs.get(output).expect("dense output exists") {
        DenseOutputValue::Judgment(values) => values
            .iter()
            .map(|value| match value {
                JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
                JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
                JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
            })
            .collect(),
        other => panic!("expected dense judgment output, got {other:?}"),
    }
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: date("2026-01-01"),
        end: date("2026-01-31"),
    }
}

fn period_interval() -> IntervalSpec {
    IntervalSpec {
        start: period().start,
        end: period().end,
    }
}

fn interval(start: &str, end: &str) -> IntervalSpec {
    IntervalSpec {
        start: date(start),
        end: date(end),
    }
}

fn date(value: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").expect("valid fixture date")
}

fn decimal_value(value: &str) -> ScalarValueSpec {
    ScalarValueSpec::Decimal {
        value: value.to_string(),
    }
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid fixture decimal")
}
