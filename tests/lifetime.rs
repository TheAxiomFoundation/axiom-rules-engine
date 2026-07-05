//! Tests for the cross-period reduction surface (issue #67): the four
//! over-periods builtins and `DenseCompiledProgram::execute_lifetime[_f64]`.
//!
//! The reductions run over an entity's own period axis — one positionally
//! aligned input batch per period. Row `i` is the same entity in every period.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A calendar-year period. Only the boundaries matter to parameter selection;
/// the reductions themselves are period-count driven.
fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

/// One single-column input batch of `values` (one row per entity).
fn batch(input: &str, values: Vec<f64>) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: values.len(),
        inputs: HashMap::from([(input.to_string(), DenseColumn::Float(values))]),
        relations: HashMap::new(),
    }
}

fn compile(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("rulespec module compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect("dense compilation succeeds")
}

/// Attempt dense compilation, returning the error string on failure. The
/// rulespec layer itself compiles; the dense compiler is where nesting is
/// rejected.
fn try_compile_error(rulespec: &str, entity: &str) -> String {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("rulespec module compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect_err("dense compilation must fail")
        .to_string()
}

/// Read a single-output scalar result as a `Vec<f64>`, accepting whichever
/// numeric column variant the mode produced (`Decimal` for `execute_lifetime`,
/// `Float` for `execute_lifetime_f64`, `Integer` for `count_over_periods`).
fn scalar_f64(result: &axiom_rules_engine::dense::DenseExecutionResult, name: &str) -> Vec<f64> {
    match result.outputs.get(name).expect("output present") {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
            values.iter().map(|value| value.to_f64().unwrap()).collect()
        }
        DenseOutputValue::Scalar(DenseColumn::Float(values)) => values.clone(),
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
            values.iter().map(|value| *value as f64).collect()
        }
        other => panic!("expected a numeric scalar column, got {other:?}"),
    }
}

// A minimal single-rule module. `{FORMULA}` is spliced into the `aime`-style
// derived rule so each test can vary only the reduction under test.
fn single_rule_module(name: &str, dtype: &str, formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: {name}
    kind: derived
    entity: Worker
    dtype: {dtype}
    period: Year
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          {formula}
"#
    )
}

// ---------------------------------------------------------------------------
// Acceptance test — issue #67: AIME for a synthetic 40-year worker
// ---------------------------------------------------------------------------

/// 42 USC 415(b): AIME is the sum of the highest-35 years of indexed earnings
/// divided by 420 (35 years x 12 months), rounded down.
///
/// Synthetic worker: 40 years of indexed earnings, year k (k = 1..=40) earns
///   12_000 + 1_000 * (k - 1)  =>  12_000, 13_000, ..., 51_000.
/// The top 35 of 40 drops the five smallest (12_000..16_000), keeping the
/// arithmetic series 17_000, 18_000, ..., 51_000 (35 terms).
///   sum(top 35) = 35 * (17_000 + 51_000) / 2 = 35 * 34_000 = 1_190_000
///   AIME        = floor(1_190_000 / 420) = floor(2833.33...) = 2833
const AIME_MODULE: &str = r#"
format: rulespec/v1
module:
  summary: |-
    Synthetic 42 USC 415(b) AIME: highest-35-years selection over a worker's
    own earnings history. Acceptance fixture for cross-period reductions (#67).
rules:
  - name: computation_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1960-01-01'
        formula: '35'
  - name: aime_divisor
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1960-01-01'
        formula: '420'
  - name: aime
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    source: 42 USC 415(b)
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          floor(sum_top_n_over_periods(indexed_earnings, computation_years) / aime_divisor)
"#;

fn aime_worker() -> (Vec<Period>, Vec<DenseBatchSpec>) {
    let periods = (1..=40).map(|k| year(1980 + k)).collect::<Vec<_>>();
    let batches = (1..=40)
        .map(|k| {
            batch(
                "indexed_earnings",
                vec![12_000.0 + 1_000.0 * f64::from(k - 1)],
            )
        })
        .collect::<Vec<_>>();
    (periods, batches)
}

#[test]
fn aime_40_year_worker_decimal() {
    let program = compile(AIME_MODULE, "Worker");
    let (periods, batches) = aime_worker();
    let result = program
        .execute_lifetime(&periods, batches, &["aime".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(result.row_count, 1);
    // Hand-derived above: floor(1_190_000 / 420) = 2833.
    assert_eq!(scalar_f64(&result, "aime"), vec![2833.0]);
}

#[test]
fn aime_40_year_worker_f64_matches_decimal() {
    let program = compile(AIME_MODULE, "Worker");
    let (periods, batches) = aime_worker();
    let result = program
        .execute_lifetime_f64(&periods, batches, &["aime".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "aime"), vec![2833.0]);
}

// ---------------------------------------------------------------------------
// Each builtin
// ---------------------------------------------------------------------------

#[test]
fn sum_over_periods_totals_the_period_axis() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![250.0]),
        batch("earnings", vec![50.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "total"), vec![400.0]);
}

#[test]
fn max_over_periods_takes_the_largest_period() {
    let module = single_rule_module("peak", "Money", "max_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![250.0]),
        batch("earnings", vec![50.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["peak".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "peak"), vec![250.0]);
}

#[test]
fn count_over_periods_counts_supplied_periods() {
    // The inner value is evaluated but ignored; the count is the period count,
    // identical for every row.
    let module = single_rule_module("years", "Integer", "count_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![1.0, 9.0]),
        batch("earnings", vec![2.0, 8.0]),
        batch("earnings", vec![3.0, 7.0]),
        batch("earnings", vec![4.0, 6.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["years".to_string()])
        .expect("lifetime execution succeeds");
    // Two entities (rows), each sees the same period count of 4.
    assert_eq!(scalar_f64(&result, "years"), vec![4.0, 4.0]);
}

// ---------------------------------------------------------------------------
// sum_top_n_over_periods edge cases
// ---------------------------------------------------------------------------

#[test]
fn sum_top_n_selects_the_n_largest() {
    let module = single_rule_module("top2", "Money", "sum_top_n_over_periods(earnings, 2)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![10.0]),
        batch("earnings", vec![40.0]),
        batch("earnings", vec![30.0]),
        batch("earnings", vec![20.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top2".to_string()])
        .expect("lifetime execution succeeds");
    // Two largest: 40 + 30 = 70.
    assert_eq!(scalar_f64(&result, "top2"), vec![70.0]);
}

#[test]
fn sum_top_n_with_ties_sums_the_right_multiplicity() {
    // Four periods valued 50, 50, 50, 10; top 2 must be 50 + 50 = 100 even
    // though the 50s tie. Sorting is by value; ties contribute their own copies.
    let module = single_rule_module("top2", "Money", "sum_top_n_over_periods(earnings, 2)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![50.0]),
        batch("earnings", vec![50.0]),
        batch("earnings", vec![50.0]),
        batch("earnings", vec![10.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top2".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "top2"), vec![100.0]);
}

#[test]
fn sum_top_n_zero_pads_when_fewer_periods_than_n() {
    // n = 5 but only 3 periods supplied: the two missing computation years
    // contribute zero, so the result is just the sum of the three (mirrors
    // 415(b), where computation years count whether or not there were earnings).
    let module = single_rule_module("top5", "Money", "sum_top_n_over_periods(earnings, 5)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![200.0]),
        batch("earnings", vec![300.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top5".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "top5"), vec![600.0]);
}

#[test]
fn sum_top_n_n_from_a_parameter_expression() {
    // `n` may be any scalar expression; here a parameter table. top 3 of
    // {5,4,3,2,1} = 5 + 4 + 3 = 12.
    let module = r#"
format: rulespec/v1
rules:
  - name: keep_n
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: '3'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, keep_n)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004), year(2005)];
    let batches = vec![
        batch("earnings", vec![5.0]),
        batch("earnings", vec![4.0]),
        batch("earnings", vec![3.0]),
        batch("earnings", vec![2.0]),
        batch("earnings", vec![1.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "top"), vec![12.0]);
}

#[test]
fn sum_top_n_truncates_a_non_integer_n() {
    // n = 2.9 truncates to 2, so top 2 of {5,4,3,2,1} = 5 + 4 = 9. Locks the
    // "truncate to integer" contract (not round).
    let module = r#"
format: rulespec/v1
rules:
  - name: keep_n
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2000-01-01'
        formula: '2.9'
  - name: top
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, keep_n)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004), year(2005)];
    let batches = vec![
        batch("earnings", vec![5.0]),
        batch("earnings", vec![4.0]),
        batch("earnings", vec![3.0]),
        batch("earnings", vec![2.0]),
        batch("earnings", vec![1.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "top"), vec![9.0]);
}

#[test]
fn sum_top_n_handles_negative_values() {
    // With negatives present, "largest" still means greatest value. top 2 of
    // {-10, -1, -5, -20} = (-1) + (-5) = -6.
    let module = single_rule_module("top2", "Money", "sum_top_n_over_periods(earnings, 2)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![-10.0]),
        batch("earnings", vec![-1.0]),
        batch("earnings", vec![-5.0]),
        batch("earnings", vec![-20.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["top2".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "top2"), vec![-6.0]);
}

#[test]
fn sum_over_periods_handles_negatives() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![-40.0]),
        batch("earnings", vec![-10.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "total"), vec![50.0]);
}

// ---------------------------------------------------------------------------
// Multi-entity positional alignment
// ---------------------------------------------------------------------------

#[test]
fn reduces_each_row_independently_across_periods() {
    // Two workers (rows), three periods. Each row reduces over its own column
    // slice: row 0 = {1, 3, 5}, row 1 = {10, 20, 30}.
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![1.0, 10.0]),
        batch("earnings", vec![3.0, 20.0]),
        batch("earnings", vec![5.0, 30.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(result.row_count, 2);
    assert_eq!(scalar_f64(&result, "total"), vec![9.0, 60.0]);
}

#[test]
fn outer_formula_combines_reduction_with_parameter_and_literal() {
    // Exercise the outer-formula path: a reduction divided by a parameter, then
    // floored (the AIME shape) but with a simpler hand value.
    // sum = 300 + 300 + 300 = 900; 900 / 400 = 2.25; floor = 2.
    let module = r#"
format: rulespec/v1
rules:
  - name: divisor
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: '400'
  - name: average
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          floor(sum_over_periods(earnings) / divisor)
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![300.0]),
        batch("earnings", vec![300.0]),
        batch("earnings", vec![300.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["average".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "average"), vec![2.0]);
}

// ---------------------------------------------------------------------------
// Validation / error paths
// ---------------------------------------------------------------------------

#[test]
fn errors_when_row_counts_differ_across_periods() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002)];
    // Period 0 has 2 rows, period 1 has 3 — positional alignment is impossible.
    let batches = vec![
        batch("earnings", vec![1.0, 2.0]),
        batch("earnings", vec![1.0, 2.0, 3.0]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect_err("row-count mismatch must error");
    let message = error.to_string();
    assert!(
        message.contains("same entity row count") && message.contains("period 1"),
        "unexpected error: {message}"
    );
}

#[test]
fn errors_when_period_and_batch_counts_differ() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![batch("earnings", vec![1.0]), batch("earnings", vec![2.0])];
    let error = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect_err("period/batch mismatch must error");
    assert!(
        error.to_string().contains("one input batch per period"),
        "unexpected error: {error}"
    );
}

#[test]
fn errors_when_no_periods_supplied() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let error = program
        .execute_lifetime(&[], Vec::new(), &["total".to_string()])
        .expect_err("empty periods must error");
    assert!(
        error.to_string().contains("at least one period"),
        "unexpected error: {error}"
    );
}

#[test]
fn errors_when_output_has_no_over_periods_reduction() {
    // `plain` is a per-period formula with no reduction; lifetime execution must
    // refuse it and point at the per-period entry points.
    let module = single_rule_module("plain", "Money", "earnings * 2");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002)];
    let batches = vec![batch("earnings", vec![1.0]), batch("earnings", vec![2.0])];
    let error = program
        .execute_lifetime(&periods, batches, &["plain".to_string()])
        .expect_err("non-reduction output must error under lifetime execution");
    let message = error.to_string();
    assert!(
        message.contains("over-periods reduction") && message.contains("execute_f64"),
        "unexpected error: {message}"
    );
}

#[test]
fn errors_when_sum_top_n_n_is_less_than_one() {
    let module = single_rule_module("top", "Money", "sum_top_n_over_periods(earnings, 0)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002)];
    let batches = vec![batch("earnings", vec![1.0]), batch("earnings", vec![2.0])];
    let error = program
        .execute_lifetime(&periods, batches, &["top".to_string()])
        .expect_err("n < 1 must error");
    assert!(
        error.to_string().contains("requires n >= 1"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_directly_nested_over_periods_at_compile_time() {
    // A reduction consumes the period axis, so nesting another reduction in its
    // argument is meaningless. The dense compiler rejects it with a precise
    // message naming both the outer and inner reduction.
    let module = single_rule_module(
        "nested",
        "Money",
        "sum_over_periods(max_over_periods(earnings))",
    );
    let message = try_compile_error(&module, "Worker");
    assert!(
        message.contains("cannot be nested")
            && message.contains("sum_over_periods")
            && message.contains("max_over_periods"),
        "unexpected error: {message}"
    );
}

#[test]
fn rejects_nested_over_periods_in_the_n_argument() {
    // The `n` of sum_top_n is also an argument that consumes no period axis;
    // a reduction there is likewise rejected.
    let module = single_rule_module(
        "nested_n",
        "Money",
        "sum_top_n_over_periods(earnings, count_over_periods(earnings))",
    );
    let message = try_compile_error(&module, "Worker");
    assert!(
        message.contains("cannot be nested") && message.contains("count_over_periods"),
        "unexpected error: {message}"
    );
}

// ---------------------------------------------------------------------------
// Per-period execution rejects the builtins
// ---------------------------------------------------------------------------

#[test]
fn per_period_execute_rejects_over_periods_reduction() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let error = program
        .execute(
            &year(2001),
            batch("earnings", vec![1.0]),
            &["total".to_string()],
        )
        .expect_err("per-period execution must reject the reduction");
    let message = error.to_string();
    assert!(
        message.contains("sum_over_periods") && message.contains("lifetime execution"),
        "unexpected error: {message}"
    );
}

#[test]
fn per_period_execute_f64_rejects_over_periods_reduction() {
    let module = single_rule_module("peak", "Money", "max_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let error = program
        .execute_f64(
            &year(2001),
            batch("earnings", vec![1.0]),
            &["peak".to_string()],
        )
        .expect_err("per-period f64 execution must reject the reduction");
    assert!(
        error.to_string().contains("max_over_periods"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------------------------
// Decimal exactness: the canonical mode is exact where f64 would drift
// ---------------------------------------------------------------------------

#[test]
fn decimal_mode_sums_exactly() {
    // 0.1 + 0.2 is 0.3 exactly in Decimal (f64 would give 0.30000000000000004).
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002)];
    let batches = vec![batch("earnings", vec![0.1]), batch("earnings", vec![0.2])];
    let result = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect("lifetime execution succeeds");
    let DenseOutputValue::Scalar(DenseColumn::Decimal(values)) =
        result.outputs.get("total").expect("output present")
    else {
        panic!("decimal mode must return a Decimal column");
    };
    assert_eq!(values, &vec![Decimal::from_str_exact("0.3").unwrap()]);
}
