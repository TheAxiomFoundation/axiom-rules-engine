//! Arithmetic outside the Decimal range is an evaluation error in every
//! execution mode, never a panic. rust_decimal's operators panic on overflow,
//! so the explain, bulk (fast) and dense evaluators use checked arithmetic and
//! report `EvalError::ArithmeticOverflow` with the same message.
//!
//! Fast mode evaluates both branches of a conditional its rows disagree on, so
//! an overflow in a branch some row does not take falls back to explain, which
//! evaluates only the branch each row takes.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse, OutputValue,
    execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;

/// The repro from the #183 merge-gate review: household-a takes the `then`
/// branch, so its `amount * 2` never runs in explain.
const GUARDED: &str = r#"
format: rulespec/v1
rules:
  - name: result
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

const GUARDED_DIVISION: &str = r#"
format: rulespec/v1
rules:
  - name: result
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if divisor == 0: 0
          else: amount / divisor
"#;

const MEMBER_INCOME_TOTAL: &str = r#"
format: rulespec/v1
rules:
  - name: result
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.income)
"#;

const MEMBER_PREDICATE_COUNT: &str = r#"
format: rulespec/v1
rules:
  - name: doubled_income_is_positive
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: income * 2 > 0
  - name: result
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, doubled_income_is_positive)
"#;

const MAX: &str = "79228162514264337593543950335";

fn household_formula(formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: result
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
"#
    )
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = period();
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal")
}

/// One query per household for `result`, with each household's own inputs.
fn household_request(
    mode: ExecutionMode,
    rulespec: &str,
    households: &[(&str, &[(&str, &str)])],
) -> ExecutionRequest {
    ExecutionRequest {
        mode,
        program: axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
            .expect("RuleSpec lowers"),
        dataset: DatasetSpec {
            inputs: households
                .iter()
                .flat_map(|(entity_id, inputs)| {
                    inputs.iter().map(move |(name, value)| InputRecordSpec {
                        name: name.to_string(),
                        entity: "Household".to_string(),
                        entity_id: entity_id.to_string(),
                        interval: interval(),
                        value: ScalarValueSpec::Decimal {
                            value: value.to_string(),
                        },
                    })
                })
                .collect(),
            relations: Vec::new(),
        },
        queries: households
            .iter()
            .map(|(entity_id, _)| ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: period(),
                outputs: vec!["result".to_string()],
            })
            .collect(),
    }
}

/// One household per entry, each with members whose `income` is given.
fn member_request(
    mode: ExecutionMode,
    rulespec: &str,
    households: &[(&str, &[&str])],
) -> ExecutionRequest {
    let mut dataset = DatasetSpec::default();
    for (household_id, incomes) in households {
        for (index, income) in incomes.iter().enumerate() {
            let person_id = format!("{household_id}-person-{index}");
            dataset.inputs.push(InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: person_id.clone(),
                interval: interval(),
                value: ScalarValueSpec::Decimal {
                    value: income.to_string(),
                },
            });
            dataset.relations.push(RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person_id, household_id.to_string()],
                interval: interval(),
            });
        }
    }
    ExecutionRequest {
        mode,
        program: axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
            .expect("RuleSpec lowers"),
        dataset,
        queries: households
            .iter()
            .map(|(household_id, _)| ExecutionQuery {
                assessment_date: None,
                entity_id: household_id.to_string(),
                period: period(),
                outputs: vec!["result".to_string()],
            })
            .collect(),
    }
}

fn dense_program(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity)).expect("dense compiles")
}

/// Dense decimal execution of `result` over one household per row.
fn dense_households(
    rulespec: &str,
    columns: &[(&str, &[&str])],
) -> Result<Vec<Decimal>, EvalError> {
    let row_count = columns.first().map_or(0, |(_, values)| values.len());
    let result = dense_program(rulespec, "Household").execute(
        &period().to_model().expect("period converts"),
        DenseBatchSpec {
            row_count,
            inputs: columns
                .iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        DenseColumn::Decimal(values.iter().map(|value| decimal(value)).collect()),
                    )
                })
                .collect(),
            relations: HashMap::new(),
        },
        &["result".to_string()],
    )?;
    Ok(dense_decimals(&result.outputs["result"]))
}

/// Dense decimal execution of `result` over households whose members' `income`
/// is given.
fn dense_members(rulespec: &str, households: &[&[&str]]) -> Result<Vec<Decimal>, EvalError> {
    let mut offsets = vec![0_usize];
    let mut incomes = Vec::new();
    for members in households {
        incomes.extend(members.iter().map(|income| decimal(income)));
        offsets.push(incomes.len());
    }
    let result = dense_program(rulespec, "Household").execute(
        &period().to_model().expect("period converts"),
        DenseBatchSpec {
            row_count: households.len(),
            inputs: HashMap::new(),
            relations: HashMap::from([(
                DenseRelationKey {
                    name: "member_of_household".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets,
                    inputs: HashMap::from([("income".to_string(), DenseColumn::Decimal(incomes))]),
                },
            )]),
        },
        &["result".to_string()],
    )?;
    Ok(dense_decimals(&result.outputs["result"]))
}

fn dense_decimals(output: &DenseOutputValue) -> Vec<Decimal> {
    match output {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values.clone(),
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
            values.iter().copied().map(Decimal::from).collect()
        }
        other => panic!("expected a numeric dense column, got {other:?}"),
    }
}

fn results(response: &ExecutionResponse) -> Vec<Decimal> {
    response
        .results
        .iter()
        .map(|result| match &result.outputs["result"] {
            OutputValue::Scalar {
                value: ScalarValueSpec::Decimal { value },
                ..
            } => decimal(value),
            OutputValue::Scalar {
                value: ScalarValueSpec::Integer { value },
                ..
            } => Decimal::from(*value),
            other => panic!("expected a numeric output, got {other:?}"),
        })
        .collect()
}

fn overflow_message(operation: &str) -> String {
    format!("arithmetic overflow: {operation} result is outside the representable decimal range")
}

/// Explain and fast both fail with `ArithmeticOverflow(operation)`, under the
/// same message; fast reports explain's error after falling back.
fn assert_overflows_in_explain_and_fast(
    request: impl Fn(ExecutionMode) -> ExecutionRequest,
    operation: &str,
) {
    let explain =
        execute_request(request(ExecutionMode::Explain)).expect_err("explain reports the overflow");
    assert!(
        matches!(&explain, ApiError::Eval(EvalError::ArithmeticOverflow(op)) if *op == operation),
        "explain: {explain:?}"
    );
    assert_eq!(explain.to_string(), overflow_message(operation));
    let fast =
        execute_request(request(ExecutionMode::Fast)).expect_err("fast reports the overflow");
    assert!(
        matches!(&fast, ApiError::Eval(EvalError::ArithmeticOverflow(op)) if *op == operation),
        "fast: {fast:?}"
    );
    assert_eq!(fast.to_string(), explain.to_string());
}

fn assert_dense_overflow(result: Result<Vec<Decimal>, EvalError>, operation: &str) {
    let error = result.expect_err("dense reports the overflow");
    assert!(
        matches!(&error, EvalError::ArithmeticOverflow(op) if *op == operation),
        "dense: {error:?}"
    );
    assert_eq!(error.to_string(), overflow_message(operation));
}

#[test]
fn fast_mode_answers_through_explain_when_only_an_untaken_branch_overflows() {
    let households: &[(&str, &[(&str, &str)])] = &[
        ("household-a", &[("flag", "1"), ("amount", MAX)]),
        ("household-b", &[("flag", "0"), ("amount", "1")]),
    ];
    let explain = execute_request(household_request(
        ExecutionMode::Explain,
        GUARDED,
        households,
    ))
    .expect("explain evaluates only the branch each household takes");
    assert_eq!(results(&explain), [decimal("0"), decimal("2")]);

    // The households disagree on the condition, so bulk evaluates `amount * 2`
    // for household-a too. That overflow sends the request to explain.
    let fast = execute_request(household_request(ExecutionMode::Fast, GUARDED, households))
        .expect("fast answers through explain");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
    assert_eq!(
        fast.metadata.fallback_reason.as_deref(),
        Some(
            format!(
                "bulk evaluation failed ({}); explain decides the outcome",
                overflow_message("multiplication")
            )
            .as_str()
        )
    );
    assert_eq!(
        serde_json::to_value(&fast.results).expect("results serialise"),
        serde_json::to_value(&explain.results).expect("results serialise")
    );

    // Alone, household-a takes the `then` branch in every row, so bulk never
    // evaluates `amount * 2` and fast mode answers without falling back.
    let fast = execute_request(household_request(
        ExecutionMode::Fast,
        GUARDED,
        &households[..1],
    ))
    .expect("fast answers");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(results(&fast), [decimal("0")]);

    // Dense evaluates both branches of a conditional over the whole batch, so
    // until it evaluates each branch only for the rows selecting it, the
    // mixed batch is an overflow error there. Either way it must not panic.
    match dense_households(GUARDED, &[("flag", &["1", "0"]), ("amount", &[MAX, "1"])]) {
        Ok(values) => assert_eq!(values, [decimal("0"), decimal("2")]),
        Err(error) => assert!(
            matches!(error, EvalError::ArithmeticOverflow("multiplication")),
            "dense: {error:?}"
        ),
    }
}

#[test]
fn a_taken_branch_overflow_is_the_same_error_in_every_mode() {
    assert_overflows_in_explain_and_fast(
        |mode| {
            household_request(
                mode,
                GUARDED,
                &[("household-a", &[("flag", "0"), ("amount", MAX)])],
            )
        },
        "multiplication",
    );
    // Household-a's taken branch overflows while household-b's does not:
    // bulk falls back, and explain reports household-a's overflow.
    assert_overflows_in_explain_and_fast(
        |mode| {
            household_request(
                mode,
                GUARDED,
                &[
                    ("household-a", &[("flag", "0"), ("amount", MAX)]),
                    ("household-b", &[("flag", "1"), ("amount", "1")]),
                ],
            )
        },
        "multiplication",
    );
    assert_dense_overflow(
        dense_households(GUARDED, &[("flag", &["0"]), ("amount", &[MAX])]),
        "multiplication",
    );
}

#[test]
fn every_decimal_operation_reports_overflow_in_every_mode() {
    for (formula, inputs, operation) in [
        ("amount + amount", &[("amount", MAX)][..], "addition"),
        (
            "amount - offset",
            &[("amount", MAX), ("offset", "-1")][..],
            "subtraction",
        ),
        ("amount * 2", &[("amount", MAX)][..], "multiplication"),
        ("amount / 0.5", &[("amount", MAX)][..], "division"),
    ] {
        let rulespec = household_formula(formula);
        assert_overflows_in_explain_and_fast(
            |mode| household_request(mode, &rulespec, &[("household-a", inputs)]),
            operation,
        );
        let columns = inputs
            .iter()
            .map(|(name, value)| (*name, std::slice::from_ref(value)))
            .collect::<Vec<_>>();
        assert_dense_overflow(dense_households(&rulespec, &columns), operation);
    }
}

#[test]
fn arithmetic_in_range_is_unchanged_by_checking() {
    // The checked operations agree with the unchecked ones wherever those did
    // not panic, including results at the edge of the range and products that
    // round away fractional digits rather than overflow.
    for (formula, inputs, expected) in [
        (
            "amount + amount",
            &[("amount", "39614081257132168796771975167")][..],
            "79228162514264337593543950334",
        ),
        (
            "amount - offset",
            &[("amount", MAX), ("offset", "1")][..],
            "79228162514264337593543950334",
        ),
        (
            "amount * 2",
            &[("amount", "-39614081257132168796771975167")][..],
            "-79228162514264337593543950334",
        ),
        (
            "amount / 3",
            &[("amount", "1")][..],
            "0.3333333333333333333333333333",
        ),
        (
            "amount * amount",
            &[("amount", "0.0000000000000000000000000001")][..],
            "0",
        ),
    ] {
        let rulespec = household_formula(formula);
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let response = execute_request(household_request(
                mode.clone(),
                &rulespec,
                &[("household-a", inputs)],
            ))
            .unwrap_or_else(|error| panic!("{formula} in {mode:?}: {error}"));
            assert_eq!(response.metadata.actual_mode, mode, "{formula}");
            assert_eq!(results(&response), [decimal(expected)], "{formula}");
        }
        let columns = inputs
            .iter()
            .map(|(name, value)| (*name, std::slice::from_ref(value)))
            .collect::<Vec<_>>();
        assert_eq!(
            dense_households(&rulespec, &columns).expect("dense answers"),
            [decimal(expected)],
            "{formula}"
        );
    }
}

#[test]
fn division_by_zero_is_an_error_in_a_taken_branch_and_skipped_in_an_untaken_one() {
    let explain = execute_request(household_request(
        ExecutionMode::Explain,
        &household_formula("amount / divisor"),
        &[("household-a", &[("amount", "8"), ("divisor", "0")])],
    ))
    .expect_err("explain reports the zero divisor");
    assert!(matches!(explain, ApiError::Eval(EvalError::DivisionByZero)));
    let fast = execute_request(household_request(
        ExecutionMode::Fast,
        &household_formula("amount / divisor"),
        &[("household-a", &[("amount", "8"), ("divisor", "0")])],
    ))
    .expect_err("fast reports the zero divisor");
    assert!(matches!(fast, ApiError::Eval(EvalError::DivisionByZero)));
    assert_eq!(fast.to_string(), explain.to_string());
    assert!(matches!(
        dense_households(
            &household_formula("amount / divisor"),
            &[("amount", &["8"]), ("divisor", &["0"])]
        ),
        Err(EvalError::DivisionByZero)
    ));

    let households: &[(&str, &[(&str, &str)])] = &[
        ("household-a", &[("divisor", "0"), ("amount", "8")]),
        ("household-b", &[("divisor", "4"), ("amount", "8")]),
    ];
    let explain = execute_request(household_request(
        ExecutionMode::Explain,
        GUARDED_DIVISION,
        households,
    ))
    .expect("explain skips household-a's division");
    assert_eq!(results(&explain), [decimal("0"), decimal("2")]);
    let fast = execute_request(household_request(
        ExecutionMode::Fast,
        GUARDED_DIVISION,
        households,
    ))
    .expect("fast answers through explain");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Explain);
    assert_eq!(
        fast.metadata.fallback_reason.as_deref(),
        Some("bulk evaluation failed (division by zero); explain decides the outcome")
    );
    assert_eq!(results(&fast), results(&explain));
    // As for the overflow above: dense evaluates both branches for the whole
    // batch, so the mixed batch may fail there, but never panics.
    match dense_households(
        GUARDED_DIVISION,
        &[("divisor", &["0", "4"]), ("amount", &["8", "8"])],
    ) {
        Ok(values) => assert_eq!(values, [decimal("0"), decimal("2")]),
        Err(error) => assert!(
            matches!(error, EvalError::DivisionByZero),
            "dense: {error:?}"
        ),
    }
}

#[test]
fn related_aggregation_overflow_is_an_error_in_every_mode() {
    assert_overflows_in_explain_and_fast(
        |mode| member_request(mode, MEMBER_INCOME_TOTAL, &[("household-a", &[MAX, "1"])]),
        "addition",
    );
    assert_dense_overflow(
        dense_members(MEMBER_INCOME_TOTAL, &[&[MAX, "1"]]),
        "addition",
    );

    // In range, the aggregation answers in every mode.
    let households: &[(&str, &[&str])] = &[("household-a", &[MAX, "-1"])];
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(member_request(
            mode.clone(),
            MEMBER_INCOME_TOTAL,
            households,
        ))
        .expect("aggregation answers");
        assert_eq!(response.metadata.actual_mode, mode);
        assert_eq!(results(&response), [decimal(MAX) - Decimal::ONE]);
    }
    assert_eq!(
        dense_members(MEMBER_INCOME_TOTAL, &[&[MAX, "-1"]]).expect("dense answers"),
        [decimal(MAX) - Decimal::ONE]
    );
}

#[test]
fn arithmetic_in_a_related_members_predicate_reports_overflow_in_every_mode() {
    // Bulk evaluates the member predicate through its related-row evaluator,
    // dense through its related-column evaluator.
    assert_overflows_in_explain_and_fast(
        |mode| {
            member_request(
                mode,
                MEMBER_PREDICATE_COUNT,
                &[("household-a", &["1", MAX])],
            )
        },
        "multiplication",
    );
    assert_dense_overflow(
        dense_members(MEMBER_PREDICATE_COUNT, &[&["1", MAX]]),
        "multiplication",
    );
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(member_request(
            mode,
            MEMBER_PREDICATE_COUNT,
            &[("household-a", &["1", "-1", "2"])],
        ))
        .expect("count answers");
        assert_eq!(results(&response), [decimal("2")]);
    }
    assert_eq!(
        dense_members(MEMBER_PREDICATE_COUNT, &[&["1", "-1", "2"]]).expect("dense answers"),
        [decimal("2")]
    );
}

#[test]
fn lifetime_reductions_report_overflow() {
    let year = |y: i32| Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    };
    let batch = |value: &str| DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([(
            "earnings".to_string(),
            DenseColumn::Decimal(vec![decimal(value)]),
        )]),
        relations: HashMap::new(),
    };
    for formula in [
        "sum_over_periods(earnings)",
        "sum_top_n_over_periods(earnings, 2)",
    ] {
        let program = dense_program(
            &format!(
                r#"
format: rulespec/v1
rules:
  - name: total
    kind: derived
    entity: Worker
    dtype: Decimal
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: {formula}
"#
            ),
            "Worker",
        );
        let error = program
            .execute_lifetime(
                &[year(2001), year(2002)],
                vec![batch(MAX), batch("1")],
                &["total".to_string()],
            )
            .expect_err("the lifetime sum overflows");
        assert!(
            matches!(error, EvalError::ArithmeticOverflow("addition")),
            "{formula}: {error:?}"
        );
        let result = program
            .execute_lifetime(
                &[year(2001), year(2002)],
                vec![batch(MAX), batch("-1")],
                &["total".to_string()],
            )
            .expect("an in-range lifetime sum answers");
        assert_eq!(
            dense_decimals(&result.outputs["total"]),
            [decimal(MAX) - Decimal::ONE],
            "{formula}"
        );
    }
}
