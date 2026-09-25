//! Dense execution fails on an uncovered `match` subject only where explain
//! does: for a row whose evaluation reaches the `match`.
//!
//! Dense evaluates both branches of every conditional for every row, so a
//! `match` without `_` sits in the plan whether or not a row's conditions select
//! it, and a rule referenced from one branch of another rule is computed for
//! every row. Each test here runs the same batch through explain (one query per
//! row) and through dense in both numeric modes, and requires the same values
//! or the same error.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{JudgmentOutcome, Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, DerivedSemanticsSpec, InputRecordSpec, IntervalSpec, JudgmentExprSpec,
    JudgmentOutcomeSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec, ScalarExprSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

/// Rules over one `TaxUnit` row each. `status` 9 is covered by no `match`.
const TAX_UNIT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: same_rule
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1:
              match status:
                  1 => 10
          else: 0
  - name: by_status
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match status:
              1 => 10
              2 => 20
  - name: cross_rule
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: by_status
          else: 0
  - name: by_other
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match other:
              1 => 3
              2 => 4
  - name: nested_else
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 0: 0
          elif other == 1: by_status
          else: by_other + 100
  - name: both_matches
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: by_status + by_other
          else: 0
  - name: guarded_and
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: eligible == 1 and by_status > 15
  - name: guarded_or
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: eligible == 0 or by_status > 15
  - name: unguarded_judgment
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: by_status > 15
  - name: judgment_guard
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if guarded_and: 1
          else: 0
  - name: condition_on_match
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1:
              if by_status > 15: 1
              else: 2
          else: 0
  - name: status_code
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match other:
              1 => 1
              2 => 2
  - name: pattern_is_a_match
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1:
              match status:
                  status_code => 50
                  9 => 90
          else: 0
  - name: rounded_share
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    rounding: half_up
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: by_status / 3
          else: 0
"#;

/// A zero last arm and a parameter key the table lacks: an uncovered row takes
/// a value that would itself fail, and must fail as explain does instead.
const ARITHMETIC_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: divisor
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match status:
              1 => 5
              3 => 0
  - name: dividend
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match other:
              1 => 100
              2 => 200
  - name: ratio
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: dividend / divisor
          else: 0
  - name: rate_key
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match status:
              1 => 1
              3 => 7
  - name: status_rate
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: rate_key
    versions:
      - effective_from: 2026-01-01
        values:
          1: 250
  - name: looked_up
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: status_rate[rate_key]
          else: 0
  - name: step
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          match status:
              1 => 4
              3 => 5
  - name: half_step
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: step / 2
  - name: half_step_rate
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: half_step
    versions:
      - effective_from: 2026-01-01
        values:
          2: 250
  - name: stepped_rate
    kind: derived
    entity: TaxUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: half_step_rate[half_step]
          else: 0
  - name: stepped_days
    kind: derived
    entity: TaxUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1: days_between(period_start, date_add_days(period_start, half_step))
          else: 0
"#;

/// Household totals over person rows. A person whose `status` is 9 is covered
/// by no `match`.
const HOUSEHOLD_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: person_by_status
    kind: derived
    entity: Person
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          match status:
              1 => 10
              2 => 20
  - name: person_amount
    kind: derived
    entity: Person
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          if eligible == 1:
              match status:
                  1 => 10
                  2 => 20
          else: 0
  - name: eligible_member
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: eligible == 1
  - name: high_status_member
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: person_by_status > 15
  - name: guarded_high_status_member
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: eligible == 1 and person_by_status > 15
  - name: total_amount
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.person_amount)
  - name: total_by_status
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.person_by_status)
  - name: eligible_total_by_status
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_household, person_by_status, eligible_member)
  - name: high_status_count
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, high_status_member)
  - name: guarded_high_status_count
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, guarded_high_status_member)
  - name: household_total
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |
          if household_eligible == 1: total_by_status
          else: 0
"#;

// ---------------------------------------------------------------------------
// The merge-gate repros
// ---------------------------------------------------------------------------

/// `if eligible == 1: (match status: 1 => 10) else: 0` over eligible = [1, 0],
/// status = [1, 9]. The second row never reaches the `match`.
#[test]
fn dense_match_in_an_unselected_branch_of_the_same_rule_is_not_an_error() {
    let result = run_tax_units(&["same_rule"], &[&[1, 1, 1], &[0, 9, 1]])
        .expect("the uncovered row never reaches the match");
    assert_eq!(integers(&result, "same_rule"), vec![10, 0]);
    assert_tax_units_match_explain(&["same_rule"], &[&[1, 1, 1], &[0, 9, 1]]);
}

/// `cross_rule = if eligible == 1: by_status else: 0` with
/// `by_status = match status: ...`. Dense computes `by_status` for both rows;
/// only the first row selects it.
#[test]
fn dense_match_in_a_rule_referenced_from_an_unselected_branch_is_not_an_error() {
    let result = run_tax_units(&["cross_rule"], &[&[1, 1, 1], &[0, 9, 1]])
        .expect("the uncovered row never selects by_status");
    assert_eq!(integers(&result, "cross_rule"), vec![10, 0]);
    assert_tax_units_match_explain(&["cross_rule"], &[&[1, 1, 1], &[0, 9, 1]]);
}

/// The same batches fail, with explain's message and attribution, once the
/// uncovered row does select the `match`.
#[test]
fn dense_match_selected_by_an_uncovered_row_fails_with_explains_error() {
    let same = run_tax_units(&["same_rule"], &[&[1, 1, 1], &[1, 9, 1]])
        .expect_err("the uncovered row reaches the match")
        .to_string();
    assert_eq!(
        same,
        "no `match` arm in `same_rule` covers `status` = 9 (arms: 1); add an arm for it or a final `_ =>` arm"
    );
    let cross = run_tax_units(&["cross_rule"], &[&[1, 1, 1], &[1, 9, 1]])
        .expect_err("the uncovered row selects by_status")
        .to_string();
    assert_eq!(
        cross,
        "no `match` arm in `by_status` covers `status` = 9 (arms: 1, 2); add an arm for it or a final `_ =>` arm"
    );
    assert_tax_units_match_explain(&["same_rule"], &[&[1, 1, 1], &[1, 9, 1]]);
    assert_tax_units_match_explain(&["cross_rule"], &[&[1, 1, 1], &[1, 9, 1]]);
}

/// Requesting the rule that contains the `match` asks for its value on every
/// row, so an uncovered row fails even when another requested output discards
/// it.
#[test]
fn dense_requesting_the_matching_rule_itself_still_fails_on_an_uncovered_row() {
    let error = run_tax_units(&["cross_rule", "by_status"], &[&[1, 1, 1], &[0, 9, 1]])
        .expect_err("by_status is requested for the uncovered row")
        .to_string();
    assert_eq!(
        error,
        "no `match` arm in `by_status` covers `status` = 9 (arms: 1, 2); add an arm for it or a final `_ =>` arm"
    );
    assert_tax_units_match_explain(&["cross_rule", "by_status"], &[&[1, 1, 1], &[0, 9, 1]]);
}

// ---------------------------------------------------------------------------
// Differential coverage against explain
// ---------------------------------------------------------------------------

/// Every one- and two-row batch over eligible in {0, 1}, status in {1, 2, 9}
/// and other in {1, 2, 9}, for every rule in the module, one output at a time
/// and all together.
#[test]
fn dense_matches_explain_for_every_small_tax_unit_batch() {
    let outputs = [
        "same_rule",
        "by_status",
        "cross_rule",
        "by_other",
        "nested_else",
        "both_matches",
        "guarded_and",
        "guarded_or",
        "unguarded_judgment",
        "judgment_guard",
        "condition_on_match",
        "status_code",
        "pattern_is_a_match",
        "rounded_share",
    ];
    let mut rows = Vec::new();
    for eligible in [0, 1] {
        for status in [1, 2, 9] {
            for other in [1, 2, 9] {
                rows.push([eligible, status, other]);
            }
        }
    }
    let mut batches: Vec<Vec<[i64; 3]>> = rows.iter().map(|row| vec![*row]).collect();
    for first in &rows {
        for second in &rows {
            batches.push(vec![*first, *second]);
        }
    }
    let fixture = tax_units();
    let mut failures = 0;
    for batch in &batches {
        let batch: Vec<&[i64]> = batch.iter().map(|row| row.as_slice()).collect();
        for output in outputs {
            failures += usize::from(!rows_match_explain(&fixture, &[output], &batch));
        }
        failures += usize::from(!rows_match_explain(&fixture, &outputs, &batch));
    }
    assert_eq!(failures, 0, "dense and explain disagreed (see stderr)");
}

/// The first failing row decides the error, then the first failing output in
/// the requested order, as explain's query-then-output loop does.
#[test]
fn dense_reports_the_first_failing_row_then_output() {
    // Row 0 fails only in by_other; row 1 fails only in by_status.
    let error = run_tax_units(&["by_status", "by_other"], &[&[1, 1, 9], &[1, 9, 1]])
        .expect_err("both rows fail")
        .to_string();
    assert!(error.contains("`by_other` covers `other` = 9"), "{error}");
    assert_tax_units_match_explain(&["by_status", "by_other"], &[&[1, 1, 9], &[1, 9, 1]]);

    // Both outputs fail on row 0: the first requested output's error wins.
    let error = run_tax_units(&["by_other", "by_status"], &[&[1, 9, 9]])
        .expect_err("row 0 fails")
        .to_string();
    assert!(error.contains("`by_other` covers `other` = 9"), "{error}");
    assert_tax_units_match_explain(&["by_other", "by_status"], &[&[1, 9, 9]]);

    // Within one expression, the left operand is evaluated first.
    let error = run_tax_units(&["both_matches"], &[&[1, 9, 9]])
        .expect_err("both operands fail")
        .to_string();
    assert!(error.contains("`by_status` covers `status` = 9"), "{error}");
    assert_tax_units_match_explain(&["both_matches"], &[&[1, 9, 9]]);
}

/// An uncovered row whose last-arm value would divide by zero or miss a
/// parameter key: when the row is not selected it is not an error, and when it
/// is, explain's `match` error comes first (a divisor is evaluated before the
/// dividend, and both before the division).
#[test]
fn dense_uncovered_rows_do_not_raise_errors_their_placeholder_value_would() {
    let fixture = Fixture::new(ARITHMETIC_RULESPEC, "TaxUnit");
    let rows = [
        [0, 1, 1],
        [0, 9, 1],
        [0, 9, 9],
        [1, 1, 1],
        [1, 1, 9],
        [1, 9, 1],
        [1, 9, 9],
    ];
    let mut batches: Vec<Vec<[i64; 3]>> = Vec::new();
    for first in rows {
        batches.push(vec![first]);
        for second in rows {
            batches.push(vec![first, second]);
        }
    }
    // Status 3 divides by zero and misses the parameter key legitimately.
    // Dense still raises those two errors for the whole batch as soon as it
    // meets them (this PR defers only `match` failures), so a selected
    // status-3 row is paired only with rows explain also accepts.
    for other in [[0, 1, 1], [0, 9, 1], [0, 9, 9], [1, 1, 1]] {
        batches.push(vec![[1, 3, 1]]);
        batches.push(vec![other, [1, 3, 1]]);
        batches.push(vec![[1, 3, 1], other]);
    }
    let mut failures = 0;
    for batch in &batches {
        let batch: Vec<&[i64]> = batch.iter().map(|row| row.as_slice()).collect();
        for outputs in [
            &["ratio"][..],
            &["looked_up"][..],
            &["ratio", "looked_up"][..],
        ] {
            failures += usize::from(!rows_match_explain(&fixture, outputs, &batch));
        }
    }
    assert_eq!(failures, 0, "dense and explain disagreed (see stderr)");

    let error = run_rows(&fixture, &["ratio"], &[&[1, 9, 1]])
        .expect_err("the uncovered divisor is selected")
        .to_string();
    assert!(error.contains("`divisor` covers `status` = 9"), "{error}");
    let error = run_rows(&fixture, &["looked_up"], &[&[1, 9, 1]])
        .expect_err("the uncovered key is selected")
        .to_string();
    assert!(error.contains("`rate_key` covers `status` = 9"), "{error}");
}

#[test]
fn dense_fractional_placeholders_do_not_fail_integral_conversions() {
    // An uncovered row's placeholder `step` is 5, so its `half_step` is 2.5:
    // not a parameter key or a day count. Explain never computes it for a row
    // that does not select it, and it must not fail the f64 run either.
    let fixture = Fixture::new(ARITHMETIC_RULESPEC, "TaxUnit");
    let rows = [[0, 1, 1], [0, 9, 1], [1, 1, 1], [1, 9, 1]];
    let mut failures = 0;
    for first in rows {
        for second in rows {
            for outputs in [
                &["stepped_rate"][..],
                &["stepped_days"][..],
                &["stepped_rate", "stepped_days"][..],
            ] {
                failures += usize::from(!rows_match_explain(&fixture, outputs, &[&first, &second]));
            }
        }
    }
    assert_eq!(failures, 0, "dense and explain disagreed (see stderr)");

    let period = period_spec().to_model().expect("period converts");
    let result = fixture
        .dense
        .execute_f64(
            &period,
            tax_unit_batch(&[&[1, 1, 1], &[0, 9, 1]]),
            &names(&["stepped_rate", "stepped_days"]),
        )
        .expect("the uncovered row never selects its key");
    assert_eq!(
        decimals(&result, "stepped_rate"),
        vec![dec("250"), dec("0")]
    );
    assert_eq!(integers(&result, "stepped_days"), vec![2, 0]);
    let error = fixture
        .dense
        .execute_f64(
            &period,
            tax_unit_batch(&[&[1, 9, 1]]),
            &names(&["stepped_rate"]),
        )
        .expect_err("the uncovered row selects its key")
        .to_string();
    assert!(error.contains("`step` covers `status` = 9"), "{error}");
}

#[test]
fn dense_names_a_failed_rule_by_its_id_as_explain_does() {
    // Rules loaded from a corpus target carry ids; explain names a failed rule
    // by its id, directly and through a related entity's inlined rule.
    let tax_units = with_rule_ids(Fixture::new(ARITHMETIC_RULESPEC, "TaxUnit"), "TaxUnit");
    for outputs in [&["ratio"][..], &["divisor"][..], &["looked_up"][..]] {
        assert!(
            rows_match_explain(&tax_units, outputs, &[&[1, 1, 1], &[1, 9, 1]]),
            "dense and explain disagreed (see stderr)"
        );
    }
    let error = run_rows(&tax_units, &["ratio"], &[&[1, 9, 1]])
        .expect_err("the uncovered divisor is selected")
        .to_string();
    assert!(
        error.contains("`us:policies/probe#divisor` covers `status` = 9"),
        "{error}"
    );

    let households = with_rule_ids(household_fixture(), "Household");
    for outputs in [
        &["total_by_status"][..],
        &["high_status_count"][..],
        &["household_total"][..],
    ] {
        assert!(
            households_match_explain(&households, outputs, &[(1, vec![(1, 1), (1, 9)])]),
            "dense and explain disagreed (see stderr)"
        );
    }
}

#[test]
fn dense_refuses_a_match_fallback_whose_conditions_are_not_its_arms() {
    // `divisor`'s last arm is rewritten to test `other`, not `status`. For
    // status 3 and other 1, explain reaches the fallback and fails, where
    // taking the value of the arm whose pattern equals the subject would
    // silently give 0. Dense cannot evaluate that chain as a `match`, so it
    // refuses the program instead.
    let mut artifact = CompiledProgramArtifact::from_rulespec_str(ARITHMETIC_RULESPEC)
        .expect("RuleSpec module compiles");
    let divisor = artifact
        .program
        .derived
        .iter_mut()
        .find(|derived| derived.name == "divisor")
        .expect("divisor is defined");
    let mut rewritten = 0;
    rewritten += retarget_last_arm(&mut divisor.semantics);
    for version in &mut divisor.versions {
        rewritten += retarget_last_arm(&mut version.semantics);
    }
    assert!(rewritten > 0, "divisor's match chain was not found");

    let dataset = DatasetSpec {
        inputs: [("eligible", 1), ("status", 3), ("other", 1)]
            .into_iter()
            .map(|(name, value)| integer_record(name, "TaxUnit", "unit-0", value))
            .collect(),
        relations: Vec::new(),
    };
    let error = explain(&artifact, dataset, &["unit-0".to_string()], &["divisor"])
        .expect_err("explain reaches the fallback");
    assert!(error.contains("`divisor` covers `status` = 3"), "{error}");

    let error = DenseCompiledProgram::from_artifact(&artifact, Some("TaxUnit"))
        .expect_err("dense refuses the rewritten chain")
        .to_string();
    assert!(
        error.contains("a `match` fallback outside its comparison chain"),
        "{error}"
    );
}

/// Point the condition of the arm before a `match` fallback at `other`.
fn retarget_last_arm(semantics: &mut DerivedSemanticsSpec) -> usize {
    let DerivedSemanticsSpec::Scalar { expr } = semantics else {
        return 0;
    };
    let mut expr = expr;
    while let ScalarExprSpec::If {
        condition,
        else_expr,
        ..
    } = expr
    {
        if matches!(else_expr.as_ref(), ScalarExprSpec::NoMatch { .. }) {
            let JudgmentExprSpec::Comparison { left, .. } = condition.as_mut() else {
                return 0;
            };
            **left = ScalarExprSpec::Input {
                name: "other".to_string(),
            };
            return 1;
        }
        expr = else_expr;
    }
    0
}

/// The fixture with every rule given an id, as a corpus target's rules are.
fn with_rule_ids(fixture: Fixture, entity: &str) -> Fixture {
    let mut artifact = fixture.artifact;
    for derived in &mut artifact.program.derived {
        derived.id = Some(format!("us:policies/probe#{}", derived.name));
    }
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some(entity)).expect("dense compiles");
    Fixture {
        artifact,
        dense,
        target: Some("us:policies/probe"),
    }
}

// ---------------------------------------------------------------------------
// Related rows
// ---------------------------------------------------------------------------

/// A household's members as (eligible, status); a household is (household
/// eligible, members).
type Household = (i64, Vec<(i64, i64)>);

#[test]
fn dense_related_row_match_fails_only_for_members_that_reach_it() {
    // The second member is uncovered but not eligible: person_amount takes
    // its else branch.
    let households: Vec<Household> = vec![(1, vec![(1, 1), (0, 9)]), (1, vec![(1, 2)])];
    let result = run_households(&["total_amount"], &households)
        .expect("the uncovered member never reaches the match");
    assert_eq!(
        decimals(&result, "total_amount"),
        vec![dec("10"), dec("20")]
    );

    // A `where` clause excludes the uncovered member before its value is read.
    let result = run_households(&["eligible_total_by_status"], &households)
        .expect("the where clause excludes the uncovered member");
    assert_eq!(
        decimals(&result, "eligible_total_by_status"),
        vec![dec("10"), dec("20")]
    );

    // An `and` guard short-circuits before the member's match.
    let result = run_households(&["guarded_high_status_count"], &households)
        .expect("the and guard short-circuits for the uncovered member");
    assert_eq!(integers(&result, "guarded_high_status_count"), vec![0, 1]);

    // A household-level conditional discards the uncovered household.
    let result = run_households(
        &["household_total"],
        &[(0, vec![(1, 9)]), (1, vec![(1, 1), (0, 2)])],
    )
    .expect("the uncovered household never selects its total");
    assert_eq!(
        decimals(&result, "household_total"),
        vec![dec("0"), dec("30")]
    );

    // Summing the matching rule directly reads every member.
    let error = run_households(&["total_by_status"], &households)
        .expect_err("the unfiltered sum reads the uncovered member")
        .to_string();
    assert_eq!(
        error,
        "no `match` arm in `person_by_status` covers `status` = 9 (arms: 1, 2); add an arm for it or a final `_ =>` arm"
    );
    // So does a `where` clause that itself reads the match.
    let error = run_households(&["high_status_count"], &households)
        .expect_err("the where clause reads the uncovered member")
        .to_string();
    assert!(
        error.contains("`person_by_status` covers `status` = 9"),
        "{error}"
    );
}

#[test]
fn dense_matches_explain_for_small_household_batches() {
    let outputs = [
        "total_amount",
        "total_by_status",
        "eligible_total_by_status",
        "high_status_count",
        "guarded_high_status_count",
        "household_total",
    ];
    let members = [(0, 1), (0, 9), (1, 2), (1, 9)];
    let mut households: Vec<Household> = Vec::new();
    for household_eligible in [0, 1] {
        households.push((household_eligible, Vec::new()));
        for first in members {
            households.push((household_eligible, vec![first]));
            for second in members {
                households.push((household_eligible, vec![first, second]));
            }
        }
    }
    let fixture = household_fixture();
    let mut failures = 0;
    for first in &households {
        for second in [None, Some(&households[5]), Some(&households[23])] {
            let mut batch = vec![first.clone()];
            batch.extend(second.cloned());
            for output in outputs {
                failures += usize::from(!households_match_explain(&fixture, &[output], &batch));
            }
            failures += usize::from(!households_match_explain(&fixture, &outputs, &batch));
        }
    }
    assert_eq!(failures, 0, "dense and explain disagreed (see stderr)");
}

// ---------------------------------------------------------------------------
// Lifetime execution
// ---------------------------------------------------------------------------

const LIFETIME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: by_status
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          match status:
              1 => 10
              2 => 20
  - name: yearly_amount
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          if eligible == 1: by_status
          else: 0
  - name: lifetime_cross_rule
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          sum_over_periods(yearly_amount)
  - name: lifetime_same_rule
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          sum_over_periods(if eligible == 1: match status: 1 => 10 2 => 20 else: 0)
  - name: lifetime_count
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          count_over_periods(yearly_amount)
  - name: cohort_years
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          match cohort:
              1 => 1
              2 => 2
              3 => 99
  - name: lifetime_outer_match
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          if covered_cohort == 1:
              match cohort:
                  1 => sum_over_periods(earnings)
                  2 => max_over_periods(earnings)
          else: 0
  - name: lifetime_top_n
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          if covered_cohort == 1: sum_top_n_over_periods(earnings, cohort_years)
          else: 0
"#;

/// Two workers over two years. The second worker's status is uncovered only
/// in a year they are not eligible.
#[test]
fn dense_lifetime_match_fails_only_in_periods_and_rows_that_reach_it() {
    let dense = compile(LIFETIME_RULESPEC, "Worker");
    let periods = [year(2024), year(2025)];
    let batches = || {
        vec![
            worker_batch(&with_cohorts(&[("eligible", &[1, 0]), ("status", &[1, 9])])),
            worker_batch(&with_cohorts(&[("eligible", &[1, 1]), ("status", &[2, 1])])),
        ]
    };
    for output in ["lifetime_cross_rule", "lifetime_same_rule"] {
        let result = dense
            .execute_lifetime(&periods, batches(), &[output.to_string()])
            .unwrap_or_else(|error| panic!("{output}: {error}"));
        assert_eq!(decimals(&result, output), vec![dec("30"), dec("10")]);
        let result = dense
            .execute_lifetime_f64(&periods, batches(), &[output.to_string()])
            .unwrap_or_else(|error| panic!("{output}: {error}"));
        assert_eq!(decimals(&result, output), vec![dec("30"), dec("10")]);
    }
    let result = dense
        .execute_lifetime(&periods, batches(), &["lifetime_count".to_string()])
        .expect("count sees only selected values");
    assert_eq!(integers(&result, "lifetime_count"), vec![2, 1]);

    // Once the uncovered year is eligible, the reduction reaches the match.
    let selected = || {
        vec![
            worker_batch(&with_cohorts(&[("eligible", &[1, 1]), ("status", &[1, 9])])),
            worker_batch(&with_cohorts(&[("eligible", &[1, 1]), ("status", &[2, 1])])),
        ]
    };
    for (output, rule, arms) in [
        ("lifetime_cross_rule", "by_status", "1, 2"),
        ("lifetime_same_rule", "lifetime_same_rule", "1, 2"),
        ("lifetime_count", "by_status", "1, 2"),
    ] {
        let expected = format!(
            "no `match` arm in `{rule}` covers `status` = 9 (arms: {arms}); add an arm for it or a final `_ =>` arm"
        );
        let error = dense
            .execute_lifetime(&periods, selected(), &[output.to_string()])
            .expect_err("the uncovered year is selected");
        assert_eq!(error.to_string(), expected, "{output}");
        let error = dense
            .execute_lifetime_f64(&periods, selected(), &[output.to_string()])
            .expect_err("the uncovered year is selected");
        assert_eq!(error.to_string(), expected, "{output}");
    }
}

/// A `match` outside any reduction, on a period-invariant input, and a top-N
/// count that is itself a `match`: an uncovered cohort is an error only for a
/// worker whose conditional selects it, and it pre-empts the top-N range
/// check its placeholder count (99) would fail.
#[test]
fn dense_lifetime_match_outside_a_reduction_is_lazy() {
    let dense = compile(LIFETIME_RULESPEC, "Worker");
    let periods = [year(2024), year(2025)];
    let batches = |covered: [i64; 3], cohort: [i64; 3]| {
        vec![
            worker_batch(&[
                ("covered_cohort", &covered),
                ("cohort", &cohort),
                ("earnings", &[100, 200, 300]),
                ("eligible", &[0, 0, 0]),
                ("status", &[1, 1, 1]),
            ]),
            worker_batch(&[
                ("covered_cohort", &covered),
                ("cohort", &cohort),
                ("earnings", &[50, 400, 600]),
                ("eligible", &[0, 0, 0]),
                ("status", &[1, 1, 1]),
            ]),
        ]
    };
    let outputs = [
        "lifetime_outer_match".to_string(),
        "lifetime_top_n".to_string(),
    ];
    let result = dense
        .execute_lifetime(&periods, batches([1, 1, 0], [1, 2, 9]), &outputs)
        .expect("the uncovered worker is not a covered cohort");
    assert_eq!(
        decimals(&result, "lifetime_outer_match"),
        vec![dec("150"), dec("400"), dec("0")]
    );
    assert_eq!(
        decimals(&result, "lifetime_top_n"),
        vec![dec("100"), dec("600"), dec("0")]
    );

    let error = dense
        .execute_lifetime(
            &periods,
            batches([1, 1, 1], [1, 2, 9]),
            &["lifetime_outer_match".to_string()],
        )
        .expect_err("the uncovered worker selects the match");
    assert_eq!(
        error.to_string(),
        "no `match` arm in `lifetime_outer_match` covers `cohort` = 9 (arms: 1, 2); add an arm for it or a final `_ =>` arm"
    );
    let error = dense
        .execute_lifetime(
            &periods,
            batches([1, 1, 1], [1, 2, 9]),
            &["lifetime_top_n".to_string()],
        )
        .expect_err("the uncovered worker selects the top-N count");
    assert_eq!(
        error.to_string(),
        "no `match` arm in `cohort_years` covers `cohort` = 9 (arms: 1, 2, 3); add an arm for it or a final `_ =>` arm"
    );
    // A covered cohort whose count is out of range still fails the range check.
    let error = dense
        .execute_lifetime(
            &periods,
            batches([1, 1, 1], [1, 2, 3]),
            &["lifetime_top_n".to_string()],
        )
        .expect_err("cohort 3's count of 99 exceeds the two periods");
    assert!(error.to_string().contains("n resolved to 99"), "{error}");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const TAX_UNIT_INPUTS: [&str; 3] = ["eligible", "status", "other"];

fn period_spec() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = period_spec();
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

/// A module compiled once, for explain (the artifact) and for dense.
struct Fixture {
    artifact: CompiledProgramArtifact,
    dense: DenseCompiledProgram,
    /// The module target of the rules' public ids, when they carry them.
    target: Option<&'static str>,
}

impl Fixture {
    fn new(rulespec: &str, entity: &str) -> Self {
        let artifact =
            CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles");
        let dense =
            DenseCompiledProgram::from_artifact(&artifact, Some(entity)).expect("dense compiles");
        Self {
            artifact,
            dense,
            target: None,
        }
    }

    /// Explain's request name for an input.
    fn input(&self, name: &str) -> String {
        match self.target {
            Some(target) => format!("{target}#input.{name}"),
            None => name.to_string(),
        }
    }

    /// Explain's outputs, requested by id when the rules carry one and keyed
    /// by name, as dense keys them.
    fn explain(
        &self,
        dataset: DatasetSpec,
        entity_ids: &[String],
        outputs: &[&str],
    ) -> Result<Vec<HashMap<String, OutputValue>>, String> {
        let Some(target) = self.target else {
            return explain(&self.artifact, dataset, entity_ids, outputs);
        };
        let ids: Vec<String> = outputs
            .iter()
            .map(|output| format!("{target}#{output}"))
            .collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let prefix = format!("{target}#");
        explain(&self.artifact, dataset, entity_ids, &ids).map(|rows| {
            rows.into_iter()
                .map(|outputs| {
                    outputs
                        .into_iter()
                        .map(|(key, value)| {
                            (key.strip_prefix(&prefix).unwrap_or(&key).to_string(), value)
                        })
                        .collect()
                })
                .collect()
        })
    }
}

fn compile(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    Fixture::new(rulespec, entity).dense
}

fn integer_record(name: &str, entity: &str, entity_id: &str, value: i64) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: entity_id.to_string(),
        interval: interval(),
        value: ScalarValueSpec::Integer { value },
    }
}

/// Two workers' columns plus the cohort inputs the module's other rules read,
/// all covered.
fn with_cohorts<'a>(columns: &[(&'a str, &'a [i64])]) -> Vec<(&'a str, &'a [i64])> {
    let mut columns = columns.to_vec();
    columns.extend([
        ("covered_cohort", &[1, 1][..]),
        ("cohort", &[1, 1][..]),
        ("earnings", &[0, 0][..]),
    ]);
    columns
}

fn worker_batch(columns: &[(&str, &[i64])]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: columns[0].1.len(),
        inputs: columns
            .iter()
            .map(|(name, values)| (name.to_string(), DenseColumn::Integer(values.to_vec())))
            .collect(),
        relations: HashMap::new(),
    }
}

fn tax_units() -> Fixture {
    Fixture::new(TAX_UNIT_RULESPEC, "TaxUnit")
}

fn run_tax_units(outputs: &[&str], rows: &[&[i64]]) -> Result<DenseExecutionResult, EvalError> {
    run_rows(&tax_units(), outputs, rows)
}

fn run_rows(
    fixture: &Fixture,
    outputs: &[&str],
    rows: &[&[i64]],
) -> Result<DenseExecutionResult, EvalError> {
    fixture.dense.execute(
        &period_spec().to_model().expect("period converts"),
        tax_unit_batch(rows),
        &names(outputs),
    )
}

fn tax_unit_batch(rows: &[&[i64]]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: rows.len(),
        inputs: TAX_UNIT_INPUTS
            .iter()
            .enumerate()
            .map(|(column, name)| {
                (
                    name.to_string(),
                    DenseColumn::Integer(rows.iter().map(|row| row[column]).collect()),
                )
            })
            .collect(),
        relations: HashMap::new(),
    }
}

fn assert_tax_units_match_explain(outputs: &[&str], rows: &[&[i64]]) {
    assert!(
        rows_match_explain(&tax_units(), outputs, rows),
        "dense and explain disagreed (see stderr)"
    );
}

fn rows_match_explain(fixture: &Fixture, outputs: &[&str], rows: &[&[i64]]) -> bool {
    let dataset = DatasetSpec {
        inputs: rows
            .iter()
            .enumerate()
            .flat_map(|(row, values)| {
                TAX_UNIT_INPUTS
                    .iter()
                    .zip(values.iter())
                    .map(move |(name, value)| {
                        integer_record(
                            &fixture.input(name),
                            "TaxUnit",
                            &format!("unit-{row}"),
                            *value,
                        )
                    })
            })
            .collect(),
        relations: Vec::new(),
    };
    let entity_ids = (0..rows.len())
        .map(|row| format!("unit-{row}"))
        .collect::<Vec<_>>();
    let explain = fixture.explain(dataset, &entity_ids, outputs);
    let period = period_spec().to_model().expect("period converts");
    let decimal = fixture
        .dense
        .execute(&period, tax_unit_batch(rows), &names(outputs));
    let float = fixture
        .dense
        .execute_f64(&period, tax_unit_batch(rows), &names(outputs));
    let label = format!("{outputs:?} over {rows:?}");
    agrees(&label, &explain, &decimal, outputs) & agrees(&label, &explain, &float, outputs)
}

fn household_fixture() -> Fixture {
    Fixture::new(HOUSEHOLD_RULESPEC, "Household")
}

fn run_households(
    outputs: &[&str],
    households: &[Household],
) -> Result<DenseExecutionResult, EvalError> {
    household_fixture().dense.execute(
        &period_spec().to_model().expect("period converts"),
        household_batch(households),
        &names(outputs),
    )
}

fn household_batch(households: &[Household]) -> DenseBatchSpec {
    let mut offsets = vec![0_usize];
    let mut eligible = Vec::new();
    let mut status = Vec::new();
    for (_, members) in households {
        for (member_eligible, member_status) in members {
            eligible.push(*member_eligible);
            status.push(*member_status);
        }
        offsets.push(eligible.len());
    }
    DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([(
            "household_eligible".to_string(),
            DenseColumn::Integer(households.iter().map(|(eligible, _)| *eligible).collect()),
        )]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets,
                inputs: HashMap::from([
                    ("eligible".to_string(), DenseColumn::Integer(eligible)),
                    ("status".to_string(), DenseColumn::Integer(status)),
                ]),
            },
        )]),
    }
}

fn households_match_explain(fixture: &Fixture, outputs: &[&str], households: &[Household]) -> bool {
    let mut dataset = DatasetSpec::default();
    let mut entity_ids = Vec::new();
    for (index, (household_eligible, members)) in households.iter().enumerate() {
        let household_id = format!("household-{index}");
        dataset.inputs.push(integer_record(
            &fixture.input("household_eligible"),
            "Household",
            &household_id,
            *household_eligible,
        ));
        for (member, (eligible, status)) in members.iter().enumerate() {
            let person_id = format!("person-{index}-{member}");
            dataset.inputs.push(integer_record(
                &fixture.input("eligible"),
                "Person",
                &person_id,
                *eligible,
            ));
            dataset.inputs.push(integer_record(
                &fixture.input("status"),
                "Person",
                &person_id,
                *status,
            ));
            dataset.relations.push(RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person_id, household_id.clone()],
                interval: interval(),
            });
        }
        entity_ids.push(household_id);
    }
    let explain = fixture.explain(dataset, &entity_ids, outputs);
    let period = period_spec().to_model().expect("period converts");
    let decimal = fixture
        .dense
        .execute(&period, household_batch(households), &names(outputs));
    let float = fixture
        .dense
        .execute_f64(&period, household_batch(households), &names(outputs));
    let label = format!("{outputs:?} over {households:?}");
    agrees(&label, &explain, &decimal, outputs) & agrees(&label, &explain, &float, outputs)
}

fn explain(
    artifact: &CompiledProgramArtifact,
    dataset: DatasetSpec,
    entity_ids: &[String],
    outputs: &[&str],
) -> Result<Vec<HashMap<String, OutputValue>>, String> {
    execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: entity_ids
            .iter()
            .map(|entity_id| ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.clone(),
                period: period_spec(),
                outputs: names(outputs),
            })
            .collect(),
    })
    .map(|response| {
        response
            .results
            .into_iter()
            .map(|result| result.outputs.into_iter().collect())
            .collect()
    })
    .map_err(|error| error.to_string())
}

/// Do explain and a dense run agree on success, on every value, and on the
/// error message? Prints the disagreement and returns false otherwise.
fn agrees(
    label: &str,
    explain: &Result<Vec<HashMap<String, OutputValue>>, String>,
    dense: &Result<DenseExecutionResult, EvalError>,
    outputs: &[&str],
) -> bool {
    match (explain, dense) {
        (Err(explain), Err(dense)) if *explain == dense.to_string() => true,
        (Ok(rows), Ok(dense)) => {
            let mut same = true;
            for (row, explain_outputs) in rows.iter().enumerate() {
                for output in outputs {
                    let expected = explain_value(&explain_outputs[*output]);
                    let actual = dense_value(&dense.outputs[*output], row);
                    if expected != actual {
                        eprintln!(
                            "{label}: `{output}` row {row}: explain {expected:?}, dense {actual:?}"
                        );
                        same = false;
                    }
                }
            }
            same
        }
        (explain, dense) => {
            eprintln!(
                "{label}: explain {:?}, dense {:?}",
                explain.as_ref().map(|_| "ok"),
                dense.as_ref().map(|_| "ok").map_err(ToString::to_string)
            );
            false
        }
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    Number(Decimal),
    Judgment(JudgmentOutcomeSpec),
}

fn explain_value(value: &OutputValue) -> Value {
    match value {
        OutputValue::Scalar { value, .. } => Value::Number(match value {
            ScalarValueSpec::Integer { value } => Decimal::from(*value),
            ScalarValueSpec::Decimal { value } => dec(value),
            other => panic!("unexpected explain scalar {other:?}"),
        }),
        OutputValue::Judgment { outcome, .. } => Value::Judgment(outcome.clone()),
    }
}

fn dense_value(value: &DenseOutputValue, row: usize) -> Value {
    match value {
        DenseOutputValue::Scalar(column) => Value::Number(number_at(column, row)),
        DenseOutputValue::Judgment(outcomes) => Value::Judgment(match outcomes[row] {
            JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
            JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
            JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
        }),
    }
}

fn number_at(column: &DenseColumn, row: usize) -> Decimal {
    match column {
        DenseColumn::Integer(values) => Decimal::from(values[row]),
        DenseColumn::Decimal(values) => values[row].normalize(),
        // f64 mode: compare at the cent, the precision these fixtures use.
        DenseColumn::Float(values) => Decimal::from_f64(values[row])
            .expect("finite")
            .round_dp(2)
            .normalize(),
        other => panic!("unexpected dense column {other:?}"),
    }
}

fn integers(result: &DenseExecutionResult, output: &str) -> Vec<i64> {
    match &result.outputs[output] {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values.clone(),
        other => panic!("expected an integer column for `{output}`, got {other:?}"),
    }
}

fn decimals(result: &DenseExecutionResult, output: &str) -> Vec<Decimal> {
    match &result.outputs[output] {
        DenseOutputValue::Scalar(column) => (0..column.len())
            .map(|row| number_at(column, row))
            .collect(),
        other => panic!("expected a scalar column for `{output}`, got {other:?}"),
    }
}

fn names(outputs: &[&str]) -> Vec<String> {
    outputs.iter().map(|output| output.to_string()).collect()
}

fn dec(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal").normalize()
}
