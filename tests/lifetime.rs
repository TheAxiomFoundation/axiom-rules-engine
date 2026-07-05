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

/// A batch with several named float inputs, all describing the same rows in the
/// same order. Every column must have `row_count` entries.
fn batch_multi(row_count: usize, columns: &[(&str, Vec<f64>)]) -> DenseBatchSpec {
    let inputs = columns
        .iter()
        .map(|(name, values)| {
            assert_eq!(values.len(), row_count, "column `{name}` has wrong length");
            (name.to_string(), DenseColumn::Float(values.clone()))
        })
        .collect();
    DenseBatchSpec {
        row_count,
        inputs,
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
fn count_over_periods_counts_nonzero_periods_per_row() {
    // count_over_periods evaluates its argument per period and counts, PER ROW,
    // the periods whose value is nonzero — it is not a bare supplied-period
    // count. Review reproduction: mixed zero/nonzero earnings per row.
    // Row 0 earnings: [0, 20, 0, 40] -> 2 nonzero years.
    // Row 1 earnings: [9, 0, 7, 6]   -> 3 nonzero years.
    let module = single_rule_module("years", "Integer", "count_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![0.0, 9.0]),
        batch("earnings", vec![20.0, 0.0]),
        batch("earnings", vec![0.0, 7.0]),
        batch("earnings", vec![40.0, 6.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["years".to_string()])
        .expect("lifetime execution succeeds");
    assert_eq!(scalar_f64(&result, "years"), vec![2.0, 3.0]);
}

#[test]
fn count_over_periods_all_zero_is_zero() {
    // A row whose value is zero in every supplied period counts zero — the
    // count is over nonzero periods, so an all-zero history is 0, not the
    // period count.
    let module = single_rule_module("years", "Integer", "count_over_periods(earnings)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![0.0, 5.0]),
        batch("earnings", vec![0.0, 0.0]),
        batch("earnings", vec![0.0, 8.0]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["years".to_string()])
        .expect("lifetime execution succeeds");
    // Row 0 is zero in all three periods -> 0; row 1 has two nonzero periods.
    assert_eq!(scalar_f64(&result, "years"), vec![0.0, 2.0]);
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
fn sum_top_n_errors_when_n_exceeds_the_period_count() {
    // Strict n contract: n = 5 with only 3 supplied periods is out of range.
    // Padding the two missing slots with zeros would be an arithmetic no-op, so
    // an over-length n is rejected as a likely data error rather than silently
    // summing every period.
    let module = single_rule_module("top5", "Money", "sum_top_n_over_periods(earnings, 5)");
    let program = compile(&module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![200.0]),
        batch("earnings", vec![300.0]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["top5".to_string()])
        .expect_err("n > period count must error");
    let message = error.to_string();
    assert!(
        message.contains("1 <= n <=") && message.contains("3 supplied periods"),
        "unexpected error: {message}"
    );
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
    // Strict n contract: n outside 1..=period_count is an out-of-range error.
    let message = error.to_string();
    assert!(
        message.contains("1 <= n <=") && message.contains("supplied periods"),
        "unexpected error: {message}"
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

// ---------------------------------------------------------------------------
// Period-invariant binding of bare inputs outside a reduction
//
// A bare input evaluated outside any reduction has no single period in general.
// But the derived counts real statutes build (42 USC 415(b)'s computation-year
// count) bottom out in per-person-constant inputs supplied identically in every
// period. Lifetime execution binds such an input when — and only when — its
// value is verified identical across all supplied periods for each row; a value
// that actually varies across periods still errors, naming the input.
// ---------------------------------------------------------------------------

/// The 415(b) benefit-computation-year count shape, as a standalone module: a
/// derived integer count built from two per-person-constant inputs (the years a
/// worker attains 21 and 62), then that count is used both as the `n` of
/// `sum_top_n_over_periods` AND as a divisor in the outer formula. The count is
/// reached only through a derived chain and sits outside every reduction — the
/// exact pattern the amendment legalizes.
const DERIVED_N_MODULE: &str = r#"
format: rulespec/v1
module:
  summary: |-
    Shape-mirror of 42 USC 415(b): a benefit-computation-year count derived from
    per-person-constant inputs feeds sum_top_n_over_periods as a person-varying n
    and divides the total to an AIME.
rules:
  - name: dropout_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '5'
  - name: minimum_computation_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '2'
  - name: months_per_year
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '12'
  - name: elapsed_years
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          year_attained_62 - max(1950, year_attained_21)
  - name: computation_year_count
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          max(minimum_computation_years, elapsed_years - dropout_years)
  - name: earnings_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          sum_top_n_over_periods(indexed_earnings, computation_year_count)
  - name: aime
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          floor(earnings_total / (months_per_year * computation_year_count))
"#;

#[test]
fn per_person_constant_input_binds_outside_a_reduction() {
    // A single per-person-constant input used directly in the outer formula
    // (added to a reduction). The input is the same in every period, so it binds
    // to that value. base_year = 2000 (constant); sum = 10 + 20 + 30 = 60;
    // result = 60 + 2000 = 2060.
    let module = r#"
format: rulespec/v1
rules:
  - name: shifted_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_over_periods(earnings) + base_year
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch_multi(1, &[("earnings", vec![10.0]), ("base_year", vec![2000.0])]),
        batch_multi(1, &[("earnings", vec![20.0]), ("base_year", vec![2000.0])]),
        batch_multi(1, &[("earnings", vec![30.0]), ("base_year", vec![2000.0])]),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["shifted_total".to_string()])
        .expect("period-invariant input must bind");
    assert_eq!(scalar_f64(&result, "shifted_total"), vec![2060.0]);
}

#[test]
fn per_row_varying_but_per_period_constant_input_binds_per_row() {
    // Two workers with DIFFERENT constant inputs. Each row's input is invariant
    // across the three periods, but the rows differ from each other — the bind
    // is per row. Row 0: base 100, sum 6 -> 106. Row 1: base 500, sum 60 -> 560.
    let module = r#"
format: rulespec/v1
rules:
  - name: shifted_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_over_periods(earnings) + base
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = vec![
        batch_multi(
            2,
            &[("earnings", vec![1.0, 10.0]), ("base", vec![100.0, 500.0])],
        ),
        batch_multi(
            2,
            &[("earnings", vec![2.0, 20.0]), ("base", vec![100.0, 500.0])],
        ),
        batch_multi(
            2,
            &[("earnings", vec![3.0, 30.0]), ("base", vec![100.0, 500.0])],
        ),
    ];
    let result = program
        .execute_lifetime(&periods, batches, &["shifted_total".to_string()])
        .expect("per-row-constant input must bind per row");
    assert_eq!(result.row_count, 2);
    assert_eq!(scalar_f64(&result, "shifted_total"), vec![106.0, 560.0]);
}

#[test]
fn period_varying_input_still_errors_naming_the_input() {
    // The same input `base` is supplied a DIFFERENT value in period 1 than in
    // period 0 for the (single) row. That is genuinely period-ambiguous outside
    // a reduction, so it must error, and the message must name the input.
    let module = r#"
format: rulespec/v1
rules:
  - name: shifted_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_over_periods(earnings) + base
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002)];
    let batches = vec![
        batch_multi(1, &[("earnings", vec![1.0]), ("base", vec![100.0])]),
        // base changes from 100 to 200 across periods -> ambiguous.
        batch_multi(1, &[("earnings", vec![2.0]), ("base", vec![200.0])]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["shifted_total".to_string()])
        .expect_err("a period-varying input must error");
    let message = error.to_string();
    assert!(
        message.contains("base")
            && message.contains("not period-invariant")
            && message.contains("100")
            && message.contains("200"),
        "unexpected error: {message}"
    );
}

/// Two workers with genuinely different derived `n` (36 and 31) in one batch,
/// mirroring the Python acceptance fixture and rulespec-us #541's 415(b) shape.
/// The count is derived from per-person-constant inputs and used as the `n` of
/// `sum_top_n_over_periods` and as an outer divisor. Hand-derived:
///   Worker A: year21=1985, year62=2026 -> elapsed 41, count max(2,41-5)=36.
///     Earnings: 96_000 x36 then 12_000, 6_000, 0, 0, 0 (41 periods).
///     top-36 sum = 36*96_000 = 3_456_000.
///     aime = floor(3_456_000 / (12*36=432)) = floor(8000.0) = 8000.
///   Worker B: year21=1990, year62=2026 -> elapsed 36, count max(2,36-5)=31.
///     Earnings: flat 62_000 (41 periods). top-31 sum = 31*62_000 = 1_922_000.
///     aime = floor(1_922_000 / (12*31=372)) = floor(5166.666...) = 5166.
///       (Statutory floor per 42 USC 415(b)(2)(A) / 20 CFR 404.211(d); NOT the
///        round-half-up 5167 — no integer months divisor even yields 5167.)
#[test]
fn derived_n_from_constant_inputs_end_to_end() {
    let program = compile(DERIVED_N_MODULE, "Worker");
    let n_periods = 41;
    let periods = (0..n_periods)
        .map(|k| year(1985 + k as i32))
        .collect::<Vec<_>>();

    let mut a_earn = vec![96_000.0; 36];
    a_earn.extend([12_000.0, 6_000.0, 0.0, 0.0, 0.0]);
    let b_earn = vec![62_000.0; n_periods];

    let batches = (0..n_periods)
        .map(|k| {
            batch_multi(
                2,
                &[
                    ("indexed_earnings", vec![a_earn[k], b_earn[k]]),
                    ("year_attained_21", vec![1985.0, 1990.0]),
                    ("year_attained_62", vec![2026.0, 2026.0]),
                ],
            )
        })
        .collect::<Vec<_>>();

    let result = program
        .execute_lifetime(
            &periods,
            batches,
            &["earnings_total".to_string(), "aime".to_string()],
        )
        .expect("derived-n lifetime execution succeeds");
    assert_eq!(result.row_count, 2);
    assert_eq!(
        scalar_f64(&result, "earnings_total"),
        vec![3_456_000.0, 1_922_000.0]
    );
    assert_eq!(scalar_f64(&result, "aime"), vec![8_000.0, 5_166.0]);
}

// ---------------------------------------------------------------------------
// Review reproductions: the exact failing cases from PR #68's review, now
// pinned to their FIXED (post-review) semantics.
// ---------------------------------------------------------------------------

/// A named derived reused both inside a reduction and bare outside it means the
/// same thing in both positions: its body inlined in the lifetime context.
/// Review's live case — `credit = rate + earnings` (rate a period-keyed
/// parameter, 100 from 2000 and 200 from 2003), and the reuse
/// `combined = sum_over_periods(credit) plus credit` over 2001-2003 — used to
/// return 600 (400 from the per-period reduction and 200 from a
/// reference-period-only outside evaluation), a value no consistent semantics
/// produces. Under referential transparency the bare `credit` inlines
/// `rate + earnings`; `earnings` is a period-VARYING input outside every
/// reduction, so the whole output errors loudly instead of silently computing
/// 600.
#[test]
fn derived_reused_inside_and_outside_reduction_errors_on_period_varying_input() {
    let module = r#"
format: rulespec/v1
rules:
  - name: rate
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: '100'
      - effective_from: '2003-01-01'
        formula: '200'
  - name: credit
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          rate + earnings
  - name: combined
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '2000-01-01'
        formula: |-
          sum_over_periods(credit) + credit
"#;
    let program = compile(module, "Worker");
    let periods = vec![year(2001), year(2002), year(2003)];
    // earnings varies by period (10, 20, 30), so the bare `credit` outside the
    // reduction is period-ambiguous and must error rather than silently pinning.
    let batches = vec![
        batch("earnings", vec![10.0]),
        batch("earnings", vec![20.0]),
        batch("earnings", vec![30.0]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["combined".to_string()])
        .expect_err("a period-varying input reached outside a reduction must error");
    let message = error.to_string();
    assert!(
        message.contains("earnings") && message.contains("not period-invariant"),
        "unexpected error: {message}"
    );
}

/// Supplied periods must be strictly ascending by start date; a descending pair
/// is rejected naming the offending adjacent periods, rather than silently
/// resolving parameters (and any top-N n) at the wrong — positionally last but
/// chronologically earlier — year.
#[test]
fn errors_when_periods_are_not_strictly_ascending() {
    let module = single_rule_module("total", "Money", "sum_over_periods(earnings)");
    let program = compile(&module, "Worker");
    // Descending: 2020 then 2019.
    let periods = vec![year(2020), year(2019)];
    let batches = vec![
        batch("earnings", vec![100.0]),
        batch("earnings", vec![200.0]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["total".to_string()])
        .expect_err("descending periods must error");
    let message = error.to_string();
    assert!(
        message.contains("strictly ascending")
            && message.contains("2020")
            && message.contains("2019"),
        "unexpected error: {message}"
    );
}

/// The strict n contract holds identically in the Decimal path: n = 45 with 41
/// supplied periods is out of range (padding the four missing slots with zeros
/// is a no-op), so `execute_lifetime` errors rather than silently summing every
/// period into a plausible-but-wrong AIME.
#[test]
fn sum_top_n_n_exceeds_period_count_errors_in_decimal_path() {
    let module = single_rule_module("top", "Money", "sum_top_n_over_periods(earnings, 45)");
    let program = compile(&module, "Worker");
    let periods = (0..41).map(|k| year(1985 + k)).collect::<Vec<_>>();
    let batches = (0..41)
        .map(|_| batch("earnings", vec![1_000.0]))
        .collect::<Vec<_>>();
    let error = program
        .execute_lifetime(&periods, batches, &["top".to_string()])
        .expect_err("n=45 over 41 periods must error");
    let message = error.to_string();
    assert!(
        message.contains("1 <= n <=") && message.contains("41 supplied periods"),
        "unexpected error: {message}"
    );
}

/// The same n=45-over-41-periods case errors identically in the f64 throughput
/// path — the contract is enforced in both dtype paths, not just Decimal.
#[test]
fn sum_top_n_n_exceeds_period_count_errors_in_f64_path() {
    let module = single_rule_module("top", "Money", "sum_top_n_over_periods(earnings, 45)");
    let program = compile(&module, "Worker");
    let periods = (0..41).map(|k| year(1985 + k)).collect::<Vec<_>>();
    let batches = (0..41)
        .map(|_| batch("earnings", vec![1_000.0]))
        .collect::<Vec<_>>();
    let error = program
        .execute_lifetime_f64(&periods, batches, &["top".to_string()])
        .expect_err("n=45 over 41 periods must error (f64 path)");
    let message = error.to_string();
    assert!(
        message.contains("1 <= n <=") && message.contains("41 supplied periods"),
        "unexpected error: {message}"
    );
}

/// A parameter-sourced n that varies by period is a data error under the strict
/// n contract — held to the same period-invariance requirement as an
/// input-sourced n — not silently pinned to the reference period.
#[test]
fn sum_top_n_period_varying_parameter_n_errors() {
    let module = r#"
format: rulespec/v1
rules:
  - name: keep_n
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: '2'
      - effective_from: '2003-01-01'
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
    // keep_n is 2 in 2001/2002 and 3 from 2003 -> not period-invariant.
    let periods = vec![year(2001), year(2002), year(2003), year(2004)];
    let batches = vec![
        batch("earnings", vec![5.0]),
        batch("earnings", vec![4.0]),
        batch("earnings", vec![3.0]),
        batch("earnings", vec![2.0]),
    ];
    let error = program
        .execute_lifetime(&periods, batches, &["top".to_string()])
        .expect_err("a period-varying parameter n must error");
    let message = error.to_string();
    assert!(
        message.contains("not period-invariant") && message.contains("sum_top_n_over_periods"),
        "unexpected error: {message}"
    );
}

/// An unknown `*_over_periods` reduction name fails to PARSE (the rulespec /
/// formula layer), not at some later stage: the builtin match is exhaustive with
/// an explicit error arm, so a typo or not-yet-implemented reduction cannot be
/// mistaken for the period count or a plain call.
#[test]
fn unknown_over_periods_reduction_fails_to_parse() {
    let module = single_rule_module("bad", "Money", "avg_over_periods(earnings)");
    let error = CompiledProgramArtifact::from_rulespec_str(&module)
        .expect_err("an unknown over-periods reduction must fail to parse");
    let message = error.to_string();
    assert!(
        message.contains("avg_over_periods") && message.contains("over-periods reduction"),
        "unexpected error: {message}"
    );
}
