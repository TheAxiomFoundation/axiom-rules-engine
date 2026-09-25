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
fn division_reports_the_same_error_as_explain_in_every_mode() {
    // Explain evaluates the divisor first and rejects zero before evaluating
    // the dividend; fast reports explain's error, and dense follows the same
    // order, so both failing operands give one answer everywhere.
    for (formula, expected) in [
        ("(amount + 1) / 0", EvalError::DivisionByZero.to_string()),
        (
            "(amount + 1) / (amount * 2)",
            overflow_message("multiplication"),
        ),
        (
            "(amount * 2) / (amount - amount)",
            EvalError::DivisionByZero.to_string(),
        ),
    ] {
        let rulespec = household_formula(formula);
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let error = execute_request(household_request(
                mode.clone(),
                &rulespec,
                &[("household-a", &[("amount", MAX)])],
            ))
            .expect_err("the division fails");
            assert_eq!(error.to_string(), expected, "{formula} in {mode:?}");
        }
        let error =
            dense_households(&rulespec, &[("amount", &[MAX])]).expect_err("the division fails");
        assert_eq!(error.to_string(), expected, "{formula} in dense");
    }
}

#[test]
fn division_order_in_member_predicates_and_lifetime_formulas() {
    // Related-row division follows explain's order too.
    let rulespec = member_predicate_count("(income + 1) / (income - income) > 0");
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let error = execute_request(member_request(
            mode.clone(),
            &rulespec,
            &[("household-a", &[MAX])],
        ))
        .expect_err("the division fails");
        assert_eq!(error.to_string(), "division by zero", "{mode:?}");
    }
    let error = dense_members(&rulespec, &[&[MAX]]).expect_err("the division fails");
    assert_eq!(error.to_string(), "division by zero");

    // Lifetime formulas have no explain counterpart and keep evaluating the
    // dividend first, so a `sum_top_n` dividend reports its n-contract error
    // ahead of the divisor's (python/tests/test_dense_lifetime.py).
    for formula in [
        "(sum_over_periods(earnings) + 1) / 0",
        "(sum_over_periods(earnings) + 1) / (sum_over_periods(earnings) * 2)",
    ] {
        let error = lifetime_total(formula, &[MAX]).expect_err("the division fails");
        assert_eq!(error.to_string(), overflow_message("addition"), "{formula}");
    }
}

#[test]
fn filtered_aggregation_checks_the_filter_before_the_value_in_every_mode() {
    // Explain checks each member's predicate before reading its value, and
    // dense computes the filter before the values, so a member for which
    // both fail reports the predicate's error everywhere.
    const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: earning_member
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: income - (0 - income) > 0
  - name: doubled
    kind: derived
    entity: Person
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: income * 2
  - name: result
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_household, doubled, earning_member)
"#;
    assert_overflows_in_explain_and_fast(
        |mode| member_request(mode, RULESPEC, &[("household-a", &[MAX])]),
        "subtraction",
    );
    assert_dense_overflow(dense_members(RULESPEC, &[&[MAX]]), "subtraction");
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

fn member_predicate_count(predicate: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: counted_member
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: {predicate}
  - name: result
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, counted_member)
"#
    )
}

#[test]
fn arithmetic_in_a_related_members_predicate_reports_overflow_in_every_mode() {
    // Bulk evaluates the member predicate through its related-row evaluator,
    // dense through its related-column evaluator.
    for (predicate, operation) in [
        ("income + income > 0", "addition"),
        ("income - (0 - income) > 0", "subtraction"),
        ("income * 2 > 0", "multiplication"),
        ("income / 0.5 > 0", "division"),
    ] {
        let rulespec = member_predicate_count(predicate);
        assert_overflows_in_explain_and_fast(
            |mode| member_request(mode, &rulespec, &[("household-a", &["1", MAX])]),
            operation,
        );
        assert_dense_overflow(dense_members(&rulespec, &[&["1", MAX]]), operation);

        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let response = execute_request(member_request(
                mode,
                &rulespec,
                &[("household-a", &["1", "-1", "2"])],
            ))
            .expect("count answers");
            assert_eq!(results(&response), [decimal("2")], "{predicate}");
        }
        assert_eq!(
            dense_members(&rulespec, &[&["1", "-1", "2"]]).expect("dense answers"),
            [decimal("2")],
            "{predicate}"
        );
    }
}

#[test]
fn filtered_related_aggregation_overflow_is_an_error_in_every_mode() {
    // sum_where sums only the members its predicate selects; dense takes a
    // separate, masked path for it.
    const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: earning_member
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: income > 0
  - name: result
    kind: derived
    entity: Household
    dtype: Decimal
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_household, income, earning_member)
"#;
    assert_overflows_in_explain_and_fast(
        |mode| member_request(mode, RULESPEC, &[("household-a", &[MAX, "-5", "1"])]),
        "addition",
    );
    assert_dense_overflow(dense_members(RULESPEC, &[&[MAX, "-5", "1"]]), "addition");

    // A member the predicate excludes is not summed, so its value cannot
    // overflow the total.
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(member_request(
            mode,
            RULESPEC,
            &[("household-a", &[MAX, "-1"])],
        ))
        .expect("the filtered sum answers");
        assert_eq!(results(&response), [decimal(MAX)]);
    }
    assert_eq!(
        dense_members(RULESPEC, &[&[MAX, "-1"]]).expect("dense answers"),
        [decimal(MAX)]
    );
}

/// Dense decimal lifetime execution of `total = <formula>` for one worker
/// whose `earnings` in consecutive years are given.
fn lifetime_total(formula: &str, earnings: &[&str]) -> Result<Vec<Decimal>, EvalError> {
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
    let periods = (2001..)
        .take(earnings.len())
        .map(|year| Period {
            kind: PeriodKind::TaxYear,
            start: chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
            end: chrono::NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
        })
        .collect::<Vec<_>>();
    let batches = earnings
        .iter()
        .map(|value| DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([(
                "earnings".to_string(),
                DenseColumn::Decimal(vec![decimal(value)]),
            )]),
            relations: HashMap::new(),
        })
        .collect();
    let result = program.execute_lifetime(&periods, batches, &["total".to_string()])?;
    Ok(dense_decimals(&result.outputs["total"]))
}

#[test]
fn lifetime_reductions_report_overflow() {
    for formula in [
        "sum_over_periods(earnings)",
        "sum_top_n_over_periods(earnings, 2)",
    ] {
        assert_dense_overflow(lifetime_total(formula, &[MAX, "1"]), "addition");
        assert_eq!(
            lifetime_total(formula, &[MAX, "-1"]).expect("an in-range sum answers"),
            [decimal(MAX) - Decimal::ONE],
            "{formula}"
        );
    }
}

#[test]
fn arithmetic_on_lifetime_reductions_reports_overflow() {
    // The lifetime executor evaluates the arithmetic around its reductions
    // itself, apart from the per-period executors.
    for (formula, operation, in_range) in [
        (
            "sum_over_periods(earnings) + sum_over_periods(earnings)",
            "addition",
            "8",
        ),
        (
            "0 - sum_over_periods(earnings) - sum_over_periods(earnings)",
            "subtraction",
            "-8",
        ),
        ("sum_over_periods(earnings) * 2", "multiplication", "8"),
        ("sum_over_periods(earnings) / 0.5", "division", "8"),
    ] {
        // The reduction itself stays in range (MAX - 1); the operation on it
        // does not.
        assert_dense_overflow(lifetime_total(formula, &[MAX, "-1"]), operation);
        assert_eq!(
            lifetime_total(formula, &["3", "1"]).expect("in range"),
            [decimal(in_range)],
            "{formula}"
        );
    }
}

/// A generated arithmetic expression over the household inputs `a` and `b`.
enum Expr {
    Input(&'static str),
    Literal(&'static str),
    Binary(char, Box<Expr>, Box<Expr>),
}

impl Expr {
    fn formula(&self) -> String {
        match self {
            Expr::Input(name) => name.to_string(),
            Expr::Literal(value) => value.to_string(),
            Expr::Binary(op, left, right) => {
                format!("({} {op} {})", left.formula(), right.formula())
            }
        }
    }

    fn inputs(&self, used: &mut Vec<&'static str>) {
        match self {
            Expr::Input(name) => {
                if !used.contains(name) {
                    used.push(name);
                }
            }
            Expr::Literal(_) => {}
            Expr::Binary(_, left, right) => {
                left.inputs(used);
                right.inputs(used);
            }
        }
    }

    /// The reference: rust_decimal's own checked operations, independent of
    /// the engine's helpers. `None` means the operation has no Decimal result
    /// (an overflow or a zero divisor), where the raw operator panics.
    fn reference(&self, a: Decimal, b: Decimal) -> Option<Decimal> {
        match self {
            Expr::Input("a") => Some(a),
            Expr::Input(_) => Some(b),
            Expr::Literal(value) => Some(decimal(value)),
            Expr::Binary(op, left, right) => {
                let (left, right) = (left.reference(a, b)?, right.reference(a, b)?);
                match op {
                    '+' => left.checked_add(right),
                    '-' => left.checked_sub(right),
                    '*' => left.checked_mul(right),
                    _ => left.checked_div(right),
                }
            }
        }
    }
}

/// Invariants over generated formulas and operands, including the extremes
/// of the Decimal range:
/// 1. No evaluator panics.
/// 2. Explain equals the reference: the same value where every operation on
///    the selected branch has a Decimal result, and an arithmetic error
///    (ArithmeticOverflow or DivisionByZero) where one does not.
/// 3. Fast mode returns explain's values, or explain's exact error.
/// 4. Dense equals the reference, except that it evaluates both branches of
///    a conditional for every row: it may instead report an arithmetic error,
///    and only when some household's unselected branch has no Decimal result.
///    That divergence is intended until dense evaluates branches per row
///    (#180).
#[test]
fn generated_arithmetic_matches_the_reference_in_every_mode() {
    const OPERANDS: [&str; 11] = [
        "0",
        "1",
        "-1",
        "0.5",
        "7",
        "1000000000000000",
        "0.0000000000000000000000000001",
        "39614081257132168796771975168",
        "-39614081257132168796771975168",
        MAX,
        "-79228162514264337593543950335",
    ];
    const LITERALS: [&str; 5] = ["0", "1", "2", "0.5", "3"];
    fn generate(next: &mut impl FnMut() -> u64, depth: u32) -> Expr {
        if depth == 0 || next() % 3 == 0 {
            return match next() % 3 {
                0 => Expr::Input("a"),
                1 => Expr::Input("b"),
                _ => Expr::Literal(LITERALS[(next() % 5) as usize]),
            };
        }
        let op = ['+', '-', '*', '/'][(next() % 4) as usize];
        Expr::Binary(
            op,
            Box::new(generate(next, depth - 1)),
            Box::new(generate(next, depth - 1)),
        )
    }
    let is_arithmetic_error = |error: &EvalError| {
        matches!(
            error,
            EvalError::ArithmeticOverflow(_) | EvalError::DivisionByZero
        )
    };

    // Answered, an arithmetic error, and answered although an unselected
    // branch fails.
    let mut outcomes = [0_usize; 3];
    for seed in 0_u64..400 {
        let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let conditional = next() % 2 == 0;
        let then_expr = generate(&mut next, 3);
        let else_expr = generate(&mut next, 3);
        let households = [
            (
                OPERANDS[(next() % 11) as usize],
                OPERANDS[(next() % 11) as usize],
            ),
            (
                OPERANDS[(next() % 11) as usize],
                OPERANDS[(next() % 11) as usize],
            ),
        ];

        let mut used = Vec::new();
        then_expr.inputs(&mut used);
        let body = if conditional {
            else_expr.inputs(&mut used);
            for name in ["a", "b"] {
                if !used.contains(&name) {
                    used.push(name);
                }
            }
            format!(
                "|-\n          if a > b: {}\n          else: {}",
                then_expr.formula(),
                else_expr.formula()
            )
        } else {
            then_expr.formula()
        };
        if used.is_empty() {
            // A formula of literals alone has no rows to evaluate in dense.
            continue;
        }
        let rulespec = household_formula(&body);
        let expected = households
            .iter()
            .map(|(a, b)| {
                let (a, b) = (decimal(a), decimal(b));
                if !conditional || a > b {
                    then_expr.reference(a, b)
                } else {
                    else_expr.reference(a, b)
                }
            })
            .collect::<Option<Vec<Decimal>>>();
        let context = format!("seed {seed}: {body} over {households:?}");

        let inputs = households
            .iter()
            .map(|(a, b)| {
                used.iter()
                    .map(|name| (*name, if *name == "a" { *a } else { *b }))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let request = |mode| {
            household_request(
                mode,
                &rulespec,
                &[("household-a", &inputs[0]), ("household-b", &inputs[1])],
            )
        };
        let explain = execute_request(request(ExecutionMode::Explain));
        let fast = execute_request(request(ExecutionMode::Fast));
        let columns = used
            .iter()
            .map(|name| {
                let values = households
                    .iter()
                    .map(|(a, b)| if *name == "a" { *a } else { *b })
                    .collect::<Vec<_>>();
                (*name, values)
            })
            .collect::<Vec<_>>();
        let columns = columns
            .iter()
            .map(|(name, values)| (*name, values.as_slice()))
            .collect::<Vec<_>>();
        let dense = dense_households(&rulespec, &columns);
        // A conditional whose unselected branch has no Decimal result for
        // some household: explain and fast must not evaluate it.
        let unselected_branch_fails = conditional
            && households.iter().any(|(a, b)| {
                let (a, b) = (decimal(a), decimal(b));
                let unselected = if a > b { &else_expr } else { &then_expr };
                unselected.reference(a, b).is_none()
            });

        match &expected {
            Some(values) => {
                let explain = explain.unwrap_or_else(|error| panic!("{context}: explain {error}"));
                assert_eq!(&results(&explain), values, "{context}: explain");
                let fast = fast.unwrap_or_else(|error| panic!("{context}: fast {error}"));
                assert_eq!(&results(&fast), values, "{context}: fast");
                match dense {
                    Ok(dense) => assert_eq!(&dense, values, "{context}: dense"),
                    // Dense evaluates both branches for every row, so it may
                    // fail only on a branch some household does not take,
                    // and only with an arithmetic error.
                    Err(error) => assert!(
                        unselected_branch_fails && is_arithmetic_error(&error),
                        "{context}: dense {error:?}"
                    ),
                }
                outcomes[if unselected_branch_fails { 2 } else { 0 }] += 1;
            }
            None => {
                let explain = explain.expect_err(&format!("{context}: explain answered"));
                assert!(
                    matches!(&explain, ApiError::Eval(error) if is_arithmetic_error(error)),
                    "{context}: explain {explain:?}"
                );
                let fast = fast.expect_err(&format!("{context}: fast answered"));
                assert_eq!(fast.to_string(), explain.to_string(), "{context}: fast");
                let dense = dense.expect_err(&format!("{context}: dense answered"));
                assert!(is_arithmetic_error(&dense), "{context}: dense {dense:?}");
                outcomes[1] += 1;
            }
        }

        // Row by row, an unconditional formula fails in dense with exactly
        // explain's error: both evaluate operands left to right, except that
        // division evaluates the divisor first and rejects zero before the
        // dividend. (With several rows, or several members of one household,
        // dense may report another row's or member's error, since it
        // evaluates a whole column before the next operation.)
        if !conditional {
            for (index, household) in inputs.iter().enumerate() {
                let explain = execute_request(household_request(
                    ExecutionMode::Explain,
                    &rulespec,
                    &[("household-a", household)],
                ));
                let columns = household
                    .iter()
                    .map(|(name, value)| (*name, std::slice::from_ref(value)))
                    .collect::<Vec<_>>();
                let dense = dense_households(&rulespec, &columns);
                match (explain, dense) {
                    (Ok(explain), Ok(dense)) => {
                        assert_eq!(results(&explain), dense, "{context}: row {index}")
                    }
                    (Err(explain), Err(dense)) => assert_eq!(
                        explain.to_string(),
                        dense.to_string(),
                        "{context}: row {index}"
                    ),
                    (explain, dense) => {
                        panic!("{context}: row {index}: explain {explain:?}, dense {dense:?}")
                    }
                }
            }
        }
    }
    // Keep the generator honest: every outcome must actually occur.
    assert!(outcomes.iter().all(|count| *count > 0), "{outcomes:?}");
}
