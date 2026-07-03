//! End-to-end tests for the currency output-rounding contract (A8).
//!
//! The contract: a derived rule may declare `rounding: <mode>`. When present
//! AND the rule's `unit` is a declared `Currency { minor_units }`, the rule's
//! output is rounded to `minor_units` under the mode — identically in every
//! execution path (explain scalar interpreter, bulk fast columnar, dense
//! columnar). Absent the declaration, behavior is exactly as before.
//!
//! These tests exercise: cross-path equivalence (explain == fast == dense) on
//! adversarial values (exact .5 midpoints, negatives) for every mode; that
//! rounding composes through a dependent rule; trace visibility of the
//! pre-rounding value and applied mode; the compile-time validations; and the
//! opt-in guarantee that an undeclared rule is untouched.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    DerivedTraceNode, ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse,
    OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::{DType, ScalarValue};
use axiom_rules_engine::spec::{
    DatasetSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RoundingModeSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal")
}

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn period_interval(period: &PeriodSpec) -> IntervalSpec {
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn decimal_of(value: &ScalarValueSpec) -> Decimal {
    match value {
        ScalarValueSpec::Decimal { value } => decimal(value),
        other => panic!("expected a decimal scalar value, got {other:?}"),
    }
}

fn scalar_decimal(output: &OutputValue) -> Decimal {
    match output {
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => decimal(value),
        other => panic!("expected a decimal scalar output, got {other:?}"),
    }
}

fn dense_decimal_at(output: &DenseOutputValue, row: usize) -> Decimal {
    match output {
        DenseOutputValue::Scalar(column) => match column.scalar_value_at(row, &DType::Decimal) {
            ScalarValue::Decimal(value) => value,
            other => panic!("expected a dense decimal value, got {other:?}"),
        },
        other => panic!("expected a dense scalar output, got {other:?}"),
    }
}

/// Run one `entity`-rooted program in all three paths (explain, fast/bulk, and
/// dense decimal) over a single `income` input column, and return the decimal
/// value of `output` for each entity in every path. The three vectors must be
/// equal for the rounding contract to hold across paths.
struct ThreePathValues {
    explain: Vec<Decimal>,
    fast: Vec<Decimal>,
    dense: Vec<Decimal>,
}

fn run_three_paths(
    rulespec: &str,
    entity: &str,
    output: &str,
    incomes: &[(&str, Decimal)],
) -> ThreePathValues {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles");
    let period = month_period();

    let dataset = DatasetSpec {
        inputs: incomes
            .iter()
            .map(|(id, income)| axiom_rules_engine::spec::InputRecordSpec {
                name: "income".to_string(),
                entity: entity.to_string(),
                entity_id: (*id).to_string(),
                interval: period_interval(&period),
                value: ScalarValueSpec::Decimal {
                    value: income.normalize().to_string(),
                },
            })
            .collect(),
        relations: Vec::new(),
    };
    let queries: Vec<ExecutionQuery> = incomes
        .iter()
        .map(|(id, _)| ExecutionQuery {
            entity_id: (*id).to_string(),
            period: period.clone(),
            outputs: vec![output.to_string()],
            assessment_date: None,
        })
        .collect();

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: dataset.clone(),
        queries: queries.clone(),
    })
    .expect("explain execution succeeds");
    assert_eq!(explain.metadata.actual_mode, ExecutionMode::Explain);

    let fast = execute_request(ExecutionRequest {
        mode: ExecutionMode::Fast,
        program: artifact.program.clone(),
        dataset,
        queries,
    })
    .expect("fast execution succeeds");
    assert_eq!(
        fast.metadata.actual_mode,
        ExecutionMode::Fast,
        "fast path fell back to explain: {:?}",
        fast.metadata.fallback_reason
    );

    let dense = DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect("dense compilation succeeds")
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: incomes.len(),
                inputs: HashMap::from([(
                    "income".to_string(),
                    DenseColumn::Decimal(incomes.iter().map(|(_, income)| *income).collect()),
                )]),
                relations: HashMap::new(),
            },
            &[output.to_string()],
        )
        .expect("dense execution succeeds");
    let dense_output = dense.outputs.get(output).expect("dense output present");

    ThreePathValues {
        explain: explain
            .results
            .iter()
            .map(|result| scalar_decimal(result.outputs.get(output).expect("explain output")))
            .collect(),
        fast: fast
            .results
            .iter()
            .map(|result| scalar_decimal(result.outputs.get(output).expect("fast output")))
            .collect(),
        dense: (0..incomes.len())
            .map(|row| dense_decimal_at(dense_output, row))
            .collect(),
    }
}

// A single-entity, single-rule program that rounds `income / 2` to whole
// dollars under `{MODE}`. `income / 2` on an odd dollar amount lands exactly on
// a `.5` midpoint, which is the case that separates the modes. The unit is
// whole-dollar currency (`minor_units: 0`), the SNAP allotment convention.
fn half_dollar_program(mode: &str) -> String {
    format!(
        r#"
format: rulespec/v1
units:
  - name: USD
    kind: currency
    minor_units: 0
rules:
  - name: half_income
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    rounding: {mode}
    effective_from: 2026-01-01
    formula: income / 2
"#
    )
}

// ---------------------------------------------------------------------------
// Cross-path equivalence, per mode, with exact .5 midpoints and negatives.
// ---------------------------------------------------------------------------

fn assert_three_paths_equal(values: &ThreePathValues, expected: &[Decimal]) {
    assert_eq!(values.explain, expected, "explain path");
    assert_eq!(
        values.fast, values.explain,
        "fast (bulk) path diverges from explain"
    );
    assert_eq!(
        values.dense, values.explain,
        "dense path diverges from explain"
    );
}

#[test]
fn half_up_agrees_across_paths_on_midpoints_and_negatives() {
    // income/2 for these incomes: 3/2=1.5, 5/2=2.5, 7/2=3.5, -3/2=-1.5, -5/2=-2.5.
    let incomes = [
        ("a", decimal("3")),
        ("b", decimal("5")),
        ("c", decimal("7")),
        ("d", decimal("-3")),
        ("e", decimal("-5")),
    ];
    let values = run_three_paths(
        &half_dollar_program("half_up"),
        "Person",
        "half_income",
        &incomes,
    );
    // Half-up: away from zero on every .5 midpoint.
    assert_three_paths_equal(
        &values,
        &[
            decimal("2"),
            decimal("3"),
            decimal("4"),
            decimal("-2"),
            decimal("-3"),
        ],
    );
}

#[test]
fn half_even_agrees_across_paths_on_midpoints_and_negatives() {
    let incomes = [
        ("a", decimal("3")),  // 1.5 -> 2 (even)
        ("b", decimal("5")),  // 2.5 -> 2 (even)
        ("c", decimal("7")),  // 3.5 -> 4 (even)
        ("d", decimal("-3")), // -1.5 -> -2 (even)
        ("e", decimal("-5")), // -2.5 -> -2 (even)
    ];
    let values = run_three_paths(
        &half_dollar_program("half_even"),
        "Person",
        "half_income",
        &incomes,
    );
    assert_three_paths_equal(
        &values,
        &[
            decimal("2"),
            decimal("2"),
            decimal("4"),
            decimal("-2"),
            decimal("-2"),
        ],
    );
}

#[test]
fn floor_agrees_across_paths_on_midpoints_and_negatives() {
    let incomes = [
        ("a", decimal("3")),  // 1.5 -> 1
        ("b", decimal("5")),  // 2.5 -> 2
        ("c", decimal("-3")), // -1.5 -> -2 (toward -inf)
        ("d", decimal("-5")), // -2.5 -> -3 (toward -inf)
    ];
    let values = run_three_paths(
        &half_dollar_program("floor"),
        "Person",
        "half_income",
        &incomes,
    );
    assert_three_paths_equal(
        &values,
        &[decimal("1"), decimal("2"), decimal("-2"), decimal("-3")],
    );
}

#[test]
fn ceil_agrees_across_paths_on_midpoints_and_negatives() {
    let incomes = [
        ("a", decimal("3")),  // 1.5 -> 2
        ("b", decimal("5")),  // 2.5 -> 3
        ("c", decimal("-3")), // -1.5 -> -1 (toward +inf)
        ("d", decimal("-5")), // -2.5 -> -2 (toward +inf)
    ];
    let values = run_three_paths(
        &half_dollar_program("ceil"),
        "Person",
        "half_income",
        &incomes,
    );
    assert_three_paths_equal(
        &values,
        &[decimal("2"), decimal("3"), decimal("-1"), decimal("-2")],
    );
}

#[test]
fn dense_f64_mode_also_rounds() {
    // The dense f64 mode is best-effort, not exact, but must still round to the
    // declared scale. On clean midpoints/integers the f64 result equals the
    // exact one. This exercises the `f64: DenseNum::round_to` path, which the
    // Decimal-mode cross-path tests do not cover.
    let artifact = CompiledProgramArtifact::from_rulespec_str(&half_dollar_program("half_up"))
        .expect("compiles");
    let period = month_period();
    let result = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("dense compiles")
        .execute_f64(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: 3,
                inputs: HashMap::from([(
                    "income".to_string(),
                    // income/2: 3/2=1.5->2, 5/2=2.5->3, 8/2=4->4.
                    DenseColumn::Decimal(vec![decimal("3"), decimal("5"), decimal("8")]),
                )]),
                relations: HashMap::new(),
            },
            &["half_income".to_string()],
        )
        .expect("dense f64 execution succeeds");
    match result.outputs.get("half_income").expect("output present") {
        DenseOutputValue::Scalar(DenseColumn::Float(values)) => {
            assert_eq!(values, &[2.0, 3.0, 4.0]);
        }
        other => panic!("expected a dense float column, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Composition: a rounded rule referenced by a downstream rule must propagate
// the ROUNDED value, in every path (the SNAP pattern: net income rounds, then
// the allotment is computed from the rounded figure).
// ---------------------------------------------------------------------------

const COMPOSED_ROUNDING_RULESPEC: &str = r#"
format: rulespec/v1
units:
  - name: USD
    kind: currency
    minor_units: 0
rules:
  # Rounds income/2 to whole dollars, half-up.
  - name: rounded_base
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    rounding: half_up
    effective_from: 2026-01-01
    formula: income / 2
  # Doubles the ROUNDED base. If rounding composed correctly, 2 * round(2.5) =
  # 2 * 3 = 6, not 2 * 2.5 = 5.
  - name: doubled
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    effective_from: 2026-01-01
    formula: rounded_base * 2
"#;

#[test]
fn rounding_composes_into_dependents_across_paths() {
    // income = 5 -> income/2 = 2.5 -> round half-up = 3 -> doubled = 6.
    let incomes = [("a", decimal("5"))];
    let values = run_three_paths(COMPOSED_ROUNDING_RULESPEC, "Person", "doubled", &incomes);
    assert_three_paths_equal(&values, &[decimal("6")]);

    // And the intermediate rounded value itself is 3 in all paths.
    let base = run_three_paths(
        COMPOSED_ROUNDING_RULESPEC,
        "Person",
        "rounded_base",
        &incomes,
    );
    assert_three_paths_equal(&base, &[decimal("3")]);
}

// ---------------------------------------------------------------------------
// Cents-scale rounding (minor_units: 2), the ordinary money case.
// ---------------------------------------------------------------------------

const CENTS_RULESPEC: &str = r#"
format: rulespec/v1
units:
  - name: USD
    kind: currency
    minor_units: 2
rules:
  - name: third
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    rounding: half_up
    effective_from: 2026-01-01
    formula: income / 3
"#;

#[test]
fn cents_scale_rounds_across_paths() {
    // 100/3 = 33.3333... -> 33.33; 200/3 = 66.6666... -> 66.67.
    let incomes = [("a", decimal("100")), ("b", decimal("200"))];
    let values = run_three_paths(CENTS_RULESPEC, "Person", "third", &incomes);
    assert_three_paths_equal(&values, &[decimal("33.33"), decimal("66.67")]);
}

// ---------------------------------------------------------------------------
// Opt-in: an identical program WITHOUT `rounding` is unchanged (no rounding).
// ---------------------------------------------------------------------------

const UNROUNDED_RULESPEC: &str = r#"
format: rulespec/v1
units:
  - name: USD
    kind: currency
    minor_units: 0
rules:
  - name: half_income
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    effective_from: 2026-01-01
    formula: income / 2
"#;

#[test]
fn absent_declaration_leaves_value_unrounded() {
    // Same formula/inputs as the half_up test, but no `rounding:`. The value
    // must stay fractional (2.5), proving rounding is strictly opt-in, in all
    // three paths.
    let incomes = [("a", decimal("5"))];
    let values = run_three_paths(UNROUNDED_RULESPEC, "Person", "half_income", &incomes);
    assert_three_paths_equal(&values, &[decimal("2.5")]);
}

// ---------------------------------------------------------------------------
// Trace visibility: explain-mode trace shows the applied mode and, when the
// value moved, the pre-rounding value.
// ---------------------------------------------------------------------------

#[test]
fn explain_trace_shows_rounding_mode_and_pre_rounding_value() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(&half_dollar_program("half_up"))
        .expect("compiles");
    let period = month_period();
    let response: ExecutionResponse = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![axiom_rules_engine::spec::InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "a".to_string(),
                interval: period_interval(&period),
                value: ScalarValueSpec::Decimal {
                    value: "5".to_string(),
                },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            entity_id: "a".to_string(),
            period: period.clone(),
            outputs: vec!["half_income".to_string()],
            assessment_date: None,
        }],
    })
    .expect("execution succeeds");

    let node = response.results[0]
        .trace
        .get("half_income")
        .expect("trace node present");
    let DerivedTraceNode::Scalar {
        value,
        rounding,
        pre_rounding_value,
        ..
    } = node
    else {
        panic!("expected a scalar trace node");
    };
    // Post-rounding value shown as the value.
    assert_eq!(decimal_of(value), decimal("3"));
    // Applied mode is surfaced.
    assert_eq!(*rounding, Some(RoundingModeSpec::HalfUp));
    // Pre-rounding value (2.5) is surfaced because rounding moved the value.
    let pre = pre_rounding_value
        .as_ref()
        .expect("pre-rounding value present");
    assert_eq!(decimal_of(pre), decimal("2.5"));
}

#[test]
fn explain_trace_omits_pre_rounding_value_when_value_unchanged() {
    // income = 4 -> income/2 = 2 (already whole); rounding is a no-op, so the
    // trace shows the mode but no pre_rounding_value.
    let artifact = CompiledProgramArtifact::from_rulespec_str(&half_dollar_program("half_up"))
        .expect("compiles");
    let period = month_period();
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![axiom_rules_engine::spec::InputRecordSpec {
                name: "income".to_string(),
                entity: "Person".to_string(),
                entity_id: "a".to_string(),
                interval: period_interval(&period),
                value: ScalarValueSpec::Decimal {
                    value: "4".to_string(),
                },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            entity_id: "a".to_string(),
            period: period.clone(),
            outputs: vec!["half_income".to_string()],
            assessment_date: None,
        }],
    })
    .expect("execution succeeds");

    let DerivedTraceNode::Scalar {
        value,
        rounding,
        pre_rounding_value,
        ..
    } = response.results[0]
        .trace
        .get("half_income")
        .expect("trace node present")
    else {
        panic!("expected a scalar trace node");
    };
    assert_eq!(decimal_of(value), decimal("2"));
    assert_eq!(*rounding, Some(RoundingModeSpec::HalfUp));
    assert!(
        pre_rounding_value.is_none(),
        "pre-rounding value should be absent when rounding did not move the value"
    );
}

// ---------------------------------------------------------------------------
// Compile-time validation.
// ---------------------------------------------------------------------------

#[test]
fn rounding_on_non_currency_unit_is_a_compile_error() {
    // `ratio` is not a currency, so `rounding:` on a rule bound to it must be
    // rejected at compile time with a clear message.
    let rulespec = r#"
format: rulespec/v1
units:
  - name: fraction
    kind: ratio
rules:
  - name: some_ratio
    kind: derived
    entity: Person
    dtype: Rate
    period: Month
    unit: fraction
    rounding: half_up
    effective_from: 2026-01-01
    formula: income / 2
"#;
    let error = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect_err("rounding on a non-currency unit must fail to compile");
    let message = error.to_string();
    assert!(
        message.contains("not a declared currency unit") || message.contains("Currency"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("some_ratio"),
        "error should name the offending rule: {message}"
    );
}

#[test]
fn rounding_with_no_unit_is_a_compile_error() {
    // A currency rule must actually declare a currency unit to round to.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: unitless
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    rounding: half_up
    effective_from: 2026-01-01
    formula: income / 2
"#;
    let error = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect_err("rounding with no unit must fail to compile");
    assert!(
        error.to_string().contains("unitless"),
        "error should name the offending rule: {error}"
    );
}

#[test]
fn rounding_on_non_derived_rule_is_a_compile_error() {
    // `rounding:` is an output-rule concern; a parameter declaring it is
    // rejected up front rather than silently ignored.
    let rulespec = r#"
format: rulespec/v1
units:
  - name: USD
    kind: currency
    minor_units: 0
rules:
  - name: some_param
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: bracket
    rounding: half_up
    versions:
      - effective_from: 2026-01-01
        values:
          0: 100
"#;
    let error = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect_err("rounding on a parameter must fail to compile");
    let message = error.to_string();
    assert!(
        message.contains("only to `derived`") || message.contains("derived"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("some_param"),
        "error should name the offending rule: {message}"
    );
}

// ---------------------------------------------------------------------------
// Golden fixture: a small SNAP-style whole-dollar RuleSpec, compiled from the
// checked-in fixture file and executed in explain and dense, exercising
// rounding and a rounded-value dependency in both paths.
// ---------------------------------------------------------------------------

#[test]
fn golden_fixture_rounds_in_explain_and_dense() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rulespec/rounding/snap_allotment.yaml");
    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&fixture).expect("golden fixture compiles");

    // net_income = earned_income - 100 (standard deduction), rounded half-up to
    // whole dollars; allotment = max(0, 200 - net_income * 0.3), also rounded.
    // earned_income = 1005 -> net 905 -> 905*0.3 = 271.5 -> allotment before
    // round = max(0, 200 - 271.5) = 0. Use a smaller income to get a positive
    // rounded allotment: earned_income = 605 -> net 505 -> 505*0.3 = 151.5 ->
    // 200 - 151.5 = 48.5 -> round half-up = 49.
    let people = [("h1", decimal("605")), ("h2", decimal("1005"))];
    let period = month_period();

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: people
                .iter()
                .map(|(id, income)| axiom_rules_engine::spec::InputRecordSpec {
                    name: "earned_income".to_string(),
                    entity: "Household".to_string(),
                    entity_id: (*id).to_string(),
                    interval: period_interval(&period),
                    value: ScalarValueSpec::Decimal {
                        value: income.normalize().to_string(),
                    },
                })
                .collect(),
            relations: Vec::new(),
        },
        queries: people
            .iter()
            .map(|(id, _)| ExecutionQuery {
                entity_id: (*id).to_string(),
                period: period.clone(),
                outputs: vec!["net_income".to_string(), "allotment".to_string()],
                assessment_date: None,
            })
            .collect(),
    })
    .expect("explain succeeds");

    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .expect("dense compiles")
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: people.len(),
                inputs: HashMap::from([(
                    "earned_income".to_string(),
                    DenseColumn::Decimal(people.iter().map(|(_, income)| *income).collect()),
                )]),
                relations: HashMap::new(),
            },
            &["net_income".to_string(), "allotment".to_string()],
        )
        .expect("dense succeeds");

    // h1: net = 605 - 100 = 505 (whole already); allotment = 49 (rounded from 48.5).
    // h2: net = 1005 - 100 = 905; 905*0.3 = 271.5; 200 - 271.5 = -71.5; max(0,..) = 0.
    let expected_net = [decimal("505"), decimal("905")];
    let expected_allotment = [decimal("49"), decimal("0")];

    for row in 0..people.len() {
        let net = scalar_decimal(explain.results[row].outputs.get("net_income").unwrap());
        let allot = scalar_decimal(explain.results[row].outputs.get("allotment").unwrap());
        assert_eq!(net, expected_net[row], "explain net_income row {row}");
        assert_eq!(
            allot, expected_allotment[row],
            "explain allotment row {row}"
        );

        assert_eq!(
            dense_decimal_at(dense.outputs.get("net_income").unwrap(), row),
            expected_net[row],
            "dense net_income row {row}"
        );
        assert_eq!(
            dense_decimal_at(dense.outputs.get("allotment").unwrap(), row),
            expected_allotment[row],
            "dense allotment row {row}"
        );
    }
}
