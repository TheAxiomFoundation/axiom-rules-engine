//! Explain, fast and dense agree on integer operands and on `max()` / `min()`.
//!
//! Explain (`src/engine.rs`) is the reference. Fast mode (`src/bulk.rs`) must
//! answer with explain's values or fail with explain's error; any bulk error
//! sends the request to explain, so the danger is bulk answering where explain
//! fails. Dense (`src/dense.rs`) has no fallback: its error reaches the caller,
//! so it must be explain's error, text included.
//!
//! Three divergences are pinned here:
//!
//! 1. An integer operand (a parameter-table key, a `date_add_*` count) that is
//!    a Decimal with a fractional part. Explain refuses it; bulk and dense used
//!    `Decimal::to_i64`, which truncates, so key 2.5 read row 2 and
//!    `date_add_days(d, 2.5)` added two days.
//! 2. A dense `f64` key that is integral but outside the `i64` range. Dense
//!    cast it with `as i64`, which saturates, so 1e20 read row `i64::MAX`.
//! 3. `max()` / `min()` with no operand. Explain refuses it; bulk and dense
//!    seeded their fold with the type's extreme and returned that sentinel.
//!
//! Dense evaluates both branches of a conditional for every row (until #180),
//! so a bad operand in a branch no row takes can fail a dense batch that
//! explain answers. Every case here puts the operand on the taken path.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMetadata, ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue,
    execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::Engine;
use axiom_rules_engine::model::{
    DataSet, DerivedSemantics, Period, PeriodKind, Program, ScalarExpr, ScalarValue,
};
use axiom_rules_engine::spec::{
    DatasetSpec, DerivedSemanticsSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec,
    ProgramSpec, RelationRecordSpec, ScalarExprSpec, ScalarValueSpec,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A batch's outcome, comparable across modes: every row's value as a
/// canonical string, or the error's display text.
type Outcome = Result<Vec<String>, String>;

fn month() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: date("2026-01-01"),
        end: date("2026-01-31"),
    }
}

fn model_month() -> Period {
    month().to_model().expect("period converts")
}

fn interval() -> IntervalSpec {
    IntervalSpec {
        start: month().start,
        end: month().end,
    }
}

fn date(value: &str) -> NaiveDate {
    NaiveDate::from_str(value).expect("valid date")
}

fn decimal(value: &str) -> ScalarValueSpec {
    ScalarValueSpec::Decimal {
        value: value.to_string(),
    }
}

fn canonical_decimal(value: Decimal) -> String {
    value.normalize().to_string()
}

fn canonical_f64(value: f64) -> String {
    // `f64`'s Display is the shortest string that round-trips, so a table
    // value of 0.3 reads back as "0.3", matching explain's Decimal.
    canonical_decimal(Decimal::from_str(&value.to_string()).expect("finite f64 output"))
}

fn canonical_spec(value: &ScalarValueSpec) -> String {
    match value {
        ScalarValueSpec::Decimal { value } => {
            canonical_decimal(Decimal::from_str(value).expect("decimal output"))
        }
        ScalarValueSpec::Integer { value } => value.to_string(),
        ScalarValueSpec::Date { value } => value.to_string(),
        other => panic!("unexpected output value {other:?}"),
    }
}

fn canonical_column(column: &DenseColumn) -> Vec<String> {
    match column {
        DenseColumn::Decimal(values) => values.iter().copied().map(canonical_decimal).collect(),
        DenseColumn::Float(values) => values.iter().copied().map(canonical_f64).collect(),
        DenseColumn::Integer(values) => values.iter().map(ToString::to_string).collect(),
        DenseColumn::Date(values) => values.iter().map(ToString::to_string).collect(),
        other => panic!("unexpected dense column {other:?}"),
    }
}

/// One entity per row, ids `p0`, `p1`, ...; each row lists its inputs.
type Rows<'a> = &'a [Vec<(&'a str, ScalarValueSpec)>];

fn request(
    program: &ProgramSpec,
    mode: ExecutionMode,
    rows: Rows,
    output: &str,
) -> ExecutionRequest {
    ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: DatasetSpec {
            inputs: rows
                .iter()
                .enumerate()
                .flat_map(|(row, inputs)| {
                    inputs.iter().map(move |(name, value)| InputRecordSpec {
                        name: (*name).to_string(),
                        entity: "Person".to_string(),
                        entity_id: format!("p{row}"),
                        interval: interval(),
                        value: value.clone(),
                    })
                })
                .collect(),
            ..Default::default()
        },
        queries: (0..rows.len())
            .map(|row| ExecutionQuery {
                assessment_date: None,
                entity_id: format!("p{row}"),
                period: month(),
                outputs: vec![output.to_string()],
            })
            .collect(),
    }
}

/// Run a request and reduce it to an [`Outcome`], keeping fast's metadata.
fn run(request: ExecutionRequest, output: &str) -> (Outcome, Option<ExecutionMetadata>) {
    match execute_request(request) {
        Ok(response) => {
            let values = response
                .results
                .iter()
                .map(|result| match &result.outputs[output] {
                    OutputValue::Scalar { value, .. } => canonical_spec(value),
                    other => panic!("expected a scalar output, got {other:?}"),
                })
                .collect();
            (Ok(values), Some(response.metadata))
        }
        Err(error) => (Err(error.to_string()), None),
    }
}

fn explain(program: &ProgramSpec, rows: Rows, output: &str) -> Outcome {
    run(
        request(program, ExecutionMode::Explain, rows, output),
        output,
    )
    .0
}

/// Fast's outcome. When fast answers, it must be explain's answer; when bulk
/// could not answer, the metadata must say explain decided.
fn fast(program: &ProgramSpec, rows: Rows, output: &str) -> Outcome {
    run(request(program, ExecutionMode::Fast, rows, output), output).0
}

fn dense_outcome(result: Result<DenseExecutionResult, impl ToString>, output: &str) -> Outcome {
    match result {
        Ok(result) => match &result.outputs[output] {
            DenseOutputValue::Scalar(column) => Ok(canonical_column(column)),
            other => panic!("expected a scalar dense output, got {other:?}"),
        },
        Err(error) => Err(error.to_string()),
    }
}

fn dense_batch(row_count: usize, inputs: &[(&str, DenseColumn)]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count,
        inputs: inputs
            .iter()
            .map(|(name, column)| ((*name).to_string(), column.clone()))
            .collect(),
        relations: HashMap::new(),
    }
}

/// A batch for [`RATE_TABLE_RULESPEC`]. Dense binds every root input its plan
/// reads, so the inputs a case does not exercise are filled with zeros.
fn rate_batch(row_count: usize, inputs: &[(&str, DenseColumn)]) -> DenseBatchSpec {
    let mut batch = dense_batch(row_count, inputs);
    for name in ["bracket", "household_size"] {
        batch
            .inputs
            .entry(name.to_string())
            .or_insert_with(|| DenseColumn::Integer(vec![0; row_count]));
    }
    batch
}

/// Dense in both arithmetic modes over the same batch.
fn dense_both(dense: &DenseCompiledProgram, batch: &DenseBatchSpec, output: &str) -> [Outcome; 2] {
    let outputs = [output.to_string()];
    [
        dense_outcome(
            dense.execute(&model_month(), batch.clone(), &outputs),
            output,
        ),
        dense_outcome(
            dense.execute_f64(&model_month(), batch.clone(), &outputs),
            output,
        ),
    ]
}

fn compile(rulespec: &str) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles")
}

fn dense(artifact: &CompiledProgramArtifact, entity: &str) -> DenseCompiledProgram {
    DenseCompiledProgram::from_artifact(artifact, Some(entity)).expect("dense compiles")
}

const KEY_ERROR: &str = "type mismatch: parameter key for `rate_table` must be an integer";

/// A rate table keyed by small integers and by both ends of the `i64` range,
/// so a truncated key (2.5 -> 2) or a saturated key (1e20 -> i64::MAX) would
/// find a row and silently answer.
const RATE_TABLE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rate_table
    kind: parameter
    dtype: Rate
    indexed_by: bracket
    versions:
      - effective_from: 2026-01-01
        values:
          -9223372036854775808: 0.01
          -1: 0.05
          0: 0.1
          1: 0.2
          2: 0.3
          3: 0.4
          9223372036854775807: 0.99
  - name: rate
    kind: derived
    entity: Person
    dtype: Rate
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: rate_table[bracket]
  - name: half_size_rate
    kind: derived
    entity: Person
    dtype: Rate
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: rate_table[household_size / 2]
"#;

// ---------------------------------------------------------------------------
// 1. Fractional integer operands
// ---------------------------------------------------------------------------

#[test]
fn a_fractional_parameter_key_fails_with_explains_error_in_every_mode() {
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");

    for key in [
        "2.5",
        "0.5",
        "-0.5",
        "2.000001",
        "3.9999999999999999999999999",
    ] {
        let rows = [vec![("bracket", decimal(key))]];
        assert_eq!(
            explain(&artifact.program, &rows, "rate"),
            Err(KEY_ERROR.to_string()),
            "explain, key {key}"
        );
        assert_eq!(
            fast(&artifact.program, &rows, "rate"),
            Err(KEY_ERROR.to_string()),
            "fast must not truncate key {key}"
        );
        let batch = rate_batch(
            1,
            &[(
                "bracket",
                DenseColumn::Decimal(vec![Decimal::from_str(key).expect("key")]),
            )],
        );
        for (mode, outcome) in ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "rate"))
        {
            assert_eq!(
                outcome,
                Err(KEY_ERROR.to_string()),
                "dense {mode} must not truncate Decimal key {key}"
            );
        }
    }

    // The same key as an f64 column: dense already refused it, but with its
    // own text rather than explain's.
    let batch = rate_batch(1, &[("bracket", DenseColumn::Float(vec![2.5]))]);
    for (mode, outcome) in ["decimal", "f64"]
        .iter()
        .zip(dense_both(&dense, &batch, "rate"))
    {
        assert_eq!(
            outcome,
            Err(KEY_ERROR.to_string()),
            "dense {mode}, f64 key 2.5"
        );
    }
}

#[test]
fn a_computed_fractional_key_fails_with_explains_error_in_every_mode() {
    // household_size / 2 is 2.5 for a household of five: a plausible way for a
    // fractional key to arise from integral inputs.
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");
    let rows = [vec![(
        "household_size",
        ScalarValueSpec::Integer { value: 5 },
    )]];
    assert_eq!(
        explain(&artifact.program, &rows, "half_size_rate"),
        Err(KEY_ERROR.to_string())
    );
    assert_eq!(
        fast(&artifact.program, &rows, "half_size_rate"),
        Err(KEY_ERROR.to_string())
    );
    let batch = rate_batch(1, &[("household_size", DenseColumn::Integer(vec![5]))]);
    for (mode, outcome) in
        ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "half_size_rate"))
    {
        assert_eq!(outcome, Err(KEY_ERROR.to_string()), "dense {mode}");
    }

    // A household of four halves to exactly 2 and reads row 2 everywhere.
    let rows = [vec![(
        "household_size",
        ScalarValueSpec::Integer { value: 4 },
    )]];
    let expected = Ok(vec!["0.3".to_string()]);
    assert_eq!(
        explain(&artifact.program, &rows, "half_size_rate"),
        expected
    );
    assert_eq!(fast(&artifact.program, &rows, "half_size_rate"), expected);
    let batch = rate_batch(1, &[("household_size", DenseColumn::Integer(vec![4]))]);
    for outcome in dense_both(&dense, &batch, "half_size_rate") {
        assert_eq!(outcome, expected);
    }
}

#[test]
fn one_fractional_key_fails_the_whole_batch_in_every_mode() {
    // Explain fails a request when any query fails; fast used to answer both
    // rows, reading row 2 for the 2.5 household.
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");
    let rows = [
        vec![("bracket", decimal("1"))],
        vec![("bracket", decimal("2.5"))],
    ];
    assert_eq!(
        explain(&artifact.program, &rows, "rate"),
        Err(KEY_ERROR.to_string())
    );
    assert_eq!(
        fast(&artifact.program, &rows, "rate"),
        Err(KEY_ERROR.to_string())
    );
    let batch = rate_batch(
        2,
        &[(
            "bracket",
            DenseColumn::Decimal(vec![Decimal::ONE, Decimal::from_str("2.5").expect("key")]),
        )],
    );
    for outcome in dense_both(&dense, &batch, "rate") {
        assert_eq!(outcome, Err(KEY_ERROR.to_string()));
    }
}

#[test]
fn integral_keys_in_any_numeric_form_read_the_same_row_in_every_mode() {
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");
    let cases: [(ScalarValueSpec, DenseColumn, &str); 7] = [
        (
            ScalarValueSpec::Integer { value: 2 },
            DenseColumn::Integer(vec![2]),
            "0.3",
        ),
        (decimal("2"), DenseColumn::Float(vec![2.0]), "0.3"),
        (decimal("2.000"), DenseColumn::Float(vec![2.0]), "0.3"),
        (decimal("-0"), DenseColumn::Float(vec![-0.0]), "0.1"),
        (decimal("-1"), DenseColumn::Float(vec![-1.0]), "0.05"),
        (
            ScalarValueSpec::Integer { value: i64::MAX },
            DenseColumn::Integer(vec![i64::MAX]),
            "0.99",
        ),
        // -2^63 is exactly representable as f64 and is i64::MIN: a valid key.
        (
            decimal("-9223372036854775808"),
            DenseColumn::Float(vec![-9_223_372_036_854_775_808.0]),
            "0.01",
        ),
    ];
    for (key, column, expected) in cases {
        let expected = Ok(vec![expected.to_string()]);
        let rows = [vec![("bracket", key.clone())]];
        assert_eq!(
            explain(&artifact.program, &rows, "rate"),
            expected,
            "{key:?}"
        );
        let (fast_outcome, metadata) = run(
            request(&artifact.program, ExecutionMode::Fast, &rows, "rate"),
            "rate",
        );
        assert_eq!(fast_outcome, expected, "fast {key:?}");
        assert_eq!(
            metadata.expect("fast answered").actual_mode,
            ExecutionMode::Fast,
            "an integral key stays in fast mode: {key:?}"
        );
        let decimal_column = match &key {
            ScalarValueSpec::Integer { value } => DenseColumn::Integer(vec![*value]),
            ScalarValueSpec::Decimal { value } => {
                DenseColumn::Decimal(vec![Decimal::from_str(value).expect("key")])
            }
            other => panic!("unexpected key {other:?}"),
        };
        for column in [decimal_column, column.clone()] {
            let batch = rate_batch(1, &[("bracket", column.clone())]);
            for (mode, outcome) in ["decimal", "f64"]
                .iter()
                .zip(dense_both(&dense, &batch, "rate"))
            {
                assert_eq!(outcome, expected, "dense {mode}, {column:?}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Out-of-range keys: refused, never saturated
// ---------------------------------------------------------------------------

#[test]
fn an_integral_key_beyond_i64_is_explains_error_in_every_mode_never_a_saturated_row() {
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");

    // Decimal keys: every mode already refused these, dense and bulk with
    // their own text.
    for key in [
        "100000000000000000000",
        "-100000000000000000000",
        "9223372036854775808",
    ] {
        let rows = [vec![("bracket", decimal(key))]];
        assert_eq!(
            explain(&artifact.program, &rows, "rate"),
            Err(KEY_ERROR.to_string()),
            "explain {key}"
        );
        assert_eq!(
            fast(&artifact.program, &rows, "rate"),
            Err(KEY_ERROR.to_string()),
            "fast {key}"
        );
        let batch = rate_batch(
            1,
            &[(
                "bracket",
                DenseColumn::Decimal(vec![Decimal::from_str(key).expect("key")]),
            )],
        );
        for outcome in dense_both(&dense, &batch, "rate") {
            assert_eq!(outcome, Err(KEY_ERROR.to_string()), "dense Decimal {key}");
        }
    }

    // f64 keys: `as i64` saturated 1e20 and 2^63 to i64::MAX and read the
    // 0.99 row; the non-finite ones were already refused.
    for key in [
        1e20,
        -1e20,
        9_223_372_036_854_775_808.0,
        f64::MAX,
        f64::MIN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        let batch = rate_batch(1, &[("bracket", DenseColumn::Float(vec![key]))]);
        for (mode, outcome) in ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "rate"))
        {
            assert_eq!(
                outcome,
                Err(KEY_ERROR.to_string()),
                "dense {mode} must refuse f64 key {key:e}, not saturate it"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Fractional calendar offsets (dense root and related-row executors)
// ---------------------------------------------------------------------------

const DATE_FUNCTIONS: [(&str, &str); 3] = [
    ("date_add_days", "day"),
    ("date_add_months", "month"),
    ("date_add_years", "year"),
];

fn offset_error(function: &str, unit: &str) -> String {
    format!("type mismatch: {function} expects an integer {unit} count on the right")
}

fn shifted_date_rulespec(function: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: shifted_date
    kind: derived
    entity: Person
    dtype: Date
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: {function}(base_date, offset)
  - name: shifted_day_count
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: days_between(base_date, shifted_date)
  - name: family_day_count
    kind: derived
    entity: Family
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_family.shifted_day_count)
"#
    )
}

#[test]
fn a_fractional_calendar_offset_fails_with_explains_error_in_every_mode() {
    for (function, unit) in DATE_FUNCTIONS {
        let artifact = compile(&shifted_date_rulespec(function));
        let dense = dense(&artifact, "Person");
        let expected = Err(offset_error(function, unit));
        for offset in ["2.5", "-0.5", "100000000000000000000"] {
            let rows = [vec![
                (
                    "base_date",
                    ScalarValueSpec::Date {
                        value: date("2026-01-10"),
                    },
                ),
                ("offset", decimal(offset)),
            ]];
            assert_eq!(
                explain(&artifact.program, &rows, "shifted_date"),
                expected,
                "explain {function}({offset})"
            );
            assert_eq!(
                fast(&artifact.program, &rows, "shifted_date"),
                expected,
                "fast {function}({offset})"
            );
            let batch = dense_batch(
                1,
                &[
                    ("base_date", DenseColumn::Date(vec![date("2026-01-10")])),
                    (
                        "offset",
                        DenseColumn::Decimal(vec![Decimal::from_str(offset).expect("offset")]),
                    ),
                ],
            );
            for (mode, outcome) in
                ["decimal", "f64"]
                    .iter()
                    .zip(dense_both(&dense, &batch, "shifted_date"))
            {
                assert_eq!(outcome, expected, "dense {mode} {function}({offset})");
            }
        }
        for offset in [2.5, 1e20, f64::NAN] {
            let batch = dense_batch(
                1,
                &[
                    ("base_date", DenseColumn::Date(vec![date("2026-01-10")])),
                    ("offset", DenseColumn::Float(vec![offset])),
                ],
            );
            for (mode, outcome) in
                ["decimal", "f64"]
                    .iter()
                    .zip(dense_both(&dense, &batch, "shifted_date"))
            {
                assert_eq!(outcome, expected, "dense {mode} {function}(f64 {offset:e})");
            }
        }
    }
}

#[test]
fn a_fractional_calendar_offset_inside_a_related_rule_fails_with_explains_error() {
    // family_day_count sums a Person rule, so dense evaluates date_add_* in its
    // related-row executor rather than the root one.
    for (function, unit) in DATE_FUNCTIONS {
        let artifact = compile(&shifted_date_rulespec(function));
        let dense = dense(&artifact, "Family");
        let expected = Err(offset_error(function, unit));
        let offsets = [decimal("1"), decimal("2.5")];
        let dataset = DatasetSpec {
            inputs: offsets
                .iter()
                .enumerate()
                .flat_map(|(member, offset)| {
                    [
                        InputRecordSpec {
                            name: "base_date".to_string(),
                            entity: "Person".to_string(),
                            entity_id: format!("p{member}"),
                            interval: interval(),
                            value: ScalarValueSpec::Date {
                                value: date("2026-01-10"),
                            },
                        },
                        InputRecordSpec {
                            name: "offset".to_string(),
                            entity: "Person".to_string(),
                            entity_id: format!("p{member}"),
                            interval: interval(),
                            value: offset.clone(),
                        },
                    ]
                })
                .collect(),
            relations: (0..offsets.len())
                .map(|member| RelationRecordSpec {
                    name: "member_of_family".to_string(),
                    tuple: vec![format!("p{member}"), "f".to_string()],
                    interval: interval(),
                })
                .collect(),
            ..Default::default()
        };
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let outcome = run(
                ExecutionRequest {
                    mode: mode.clone(),
                    program: artifact.program.clone(),
                    dataset: dataset.clone(),
                    queries: vec![ExecutionQuery {
                        assessment_date: None,
                        entity_id: "f".to_string(),
                        period: month(),
                        outputs: vec!["family_day_count".to_string()],
                    }],
                },
                "family_day_count",
            )
            .0;
            assert_eq!(outcome, expected, "{mode:?} {function}");
        }
        for offsets in [
            DenseColumn::Decimal(vec![
                Decimal::ONE,
                Decimal::from_str("2.5").expect("offset"),
            ]),
            DenseColumn::Float(vec![1.0, 2.5]),
        ] {
            let batch = DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "member_of_family".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: vec![0, 2],
                        inputs: HashMap::from([
                            (
                                "base_date".to_string(),
                                DenseColumn::Date(vec![date("2026-01-10"); 2]),
                            ),
                            ("offset".to_string(), offsets.clone()),
                        ]),
                    },
                )]),
            };
            for (mode, outcome) in
                ["decimal", "f64"]
                    .iter()
                    .zip(dense_both(&dense, &batch, "family_day_count"))
            {
                assert_eq!(
                    outcome, expected,
                    "dense {mode} related {function} {offsets:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter keys inside related and lifetime rules
// ---------------------------------------------------------------------------

#[test]
fn a_fractional_key_inside_a_related_rule_fails_with_explains_error() {
    let rulespec = format!(
        "{RATE_TABLE_RULESPEC}{}",
        r#"
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: family_rate_total
    kind: derived
    entity: Family
    dtype: Rate
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_family.rate)
"#
    );
    let artifact = compile(&rulespec);
    let dense = dense(&artifact, "Family");
    let brackets = [decimal("1"), decimal("2.5")];
    let dataset = DatasetSpec {
        inputs: brackets
            .iter()
            .enumerate()
            .map(|(member, bracket)| InputRecordSpec {
                name: "bracket".to_string(),
                entity: "Person".to_string(),
                entity_id: format!("p{member}"),
                interval: interval(),
                value: bracket.clone(),
            })
            .collect(),
        relations: (0..brackets.len())
            .map(|member| RelationRecordSpec {
                name: "member_of_family".to_string(),
                tuple: vec![format!("p{member}"), "f".to_string()],
                interval: interval(),
            })
            .collect(),
        ..Default::default()
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let outcome = run(
            ExecutionRequest {
                mode: mode.clone(),
                program: artifact.program.clone(),
                dataset: dataset.clone(),
                queries: vec![ExecutionQuery {
                    assessment_date: None,
                    entity_id: "f".to_string(),
                    period: month(),
                    outputs: vec!["family_rate_total".to_string()],
                }],
            },
            "family_rate_total",
        )
        .0;
        assert_eq!(outcome, Err(KEY_ERROR.to_string()), "{mode:?}");
    }
    for brackets in [
        DenseColumn::Decimal(vec![Decimal::ONE, Decimal::from_str("2.5").expect("key")]),
        DenseColumn::Float(vec![1.0, 2.5]),
        DenseColumn::Float(vec![1.0, 1e20]),
    ] {
        let batch = DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::from([(
                DenseRelationKey {
                    name: "member_of_family".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: vec![0, 2],
                    inputs: HashMap::from([("bracket".to_string(), brackets.clone())]),
                },
            )]),
        };
        for (mode, outcome) in
            ["decimal", "f64"]
                .iter()
                .zip(dense_both(&dense, &batch, "family_rate_total"))
        {
            assert_eq!(
                outcome,
                Err(KEY_ERROR.to_string()),
                "dense {mode} {brackets:?}"
            );
        }
    }
}

fn tax_year(year: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
        end: NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
    }
}

#[test]
fn a_fractional_key_in_a_lifetime_rule_fails_with_explains_parameter_key_error() {
    // Explain has no lifetime path; the lifetime executor's parameter lookup
    // reports the error explain reports for the same lookup in one period.
    let rulespec = format!(
        "{RATE_TABLE_RULESPEC}{}",
        r#"
  - name: lifetime_charge
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(earnings) * rate_table[bracket]
"#
    );
    let artifact = compile(&rulespec);
    let dense = dense(&artifact, "Person");
    let periods = [tax_year(2026), tax_year(2027)];
    let outputs = ["lifetime_charge".to_string()];
    let lifetime = |bracket: DenseColumn, f64_mode: bool| {
        let batches = periods
            .iter()
            .map(|_| {
                rate_batch(
                    1,
                    &[
                        ("earnings", DenseColumn::Decimal(vec![Decimal::from(100)])),
                        ("bracket", bracket.clone()),
                    ],
                )
            })
            .collect::<Vec<_>>();
        let result = if f64_mode {
            dense.execute_lifetime_f64(&periods, batches, &outputs)
        } else {
            dense.execute_lifetime(&periods, batches, &outputs)
        };
        dense_outcome(result, "lifetime_charge")
    };
    for f64_mode in [false, true] {
        assert_eq!(
            lifetime(DenseColumn::Integer(vec![2]), f64_mode),
            Ok(vec!["60".to_string()]),
            "integral key, f64 mode {f64_mode}"
        );
        for bracket in [
            DenseColumn::Decimal(vec![Decimal::from_str("2.5").expect("key")]),
            DenseColumn::Float(vec![2.5]),
            DenseColumn::Float(vec![1e20]),
        ] {
            assert_eq!(
                lifetime(bracket.clone(), f64_mode),
                Err(KEY_ERROR.to_string()),
                "{bracket:?}, f64 mode {f64_mode}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. max() and min() with no operand
// ---------------------------------------------------------------------------

fn extremum_error(function: &str) -> String {
    format!("{function}() requires at least one operand")
}

fn single_rule(formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: amount
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: |-
          {formula}
"#
    )
}

#[test]
fn a_rulespec_formula_with_an_empty_max_or_min_does_not_compile() {
    for (formula, function) in [
        ("max()", "max"),
        ("min()", "min"),
        ("max(income, min())", "min"),
        ("min(max(), income)", "max"),
        ("if income > max(): 1 else: 0", "max"),
    ] {
        let error = CompiledProgramArtifact::from_rulespec_str(&single_rule(formula))
            .err()
            .unwrap_or_else(|| panic!("`{formula}` must not compile"))
            .to_string();
        assert!(
            error.contains(&extremum_error(function)),
            "`{formula}`: {error}"
        );
    }
    // One operand is enough.
    compile(&single_rule("max(income)"));
    compile(&single_rule("min(income)"));
}

/// A program spec whose `amount` rule is `max(income)` or `min(income)`, with
/// the operand list then emptied, as a hand-written ProgramSpec could be.
fn empty_extremum_spec(function: &str) -> ProgramSpec {
    let mut program = axiom_rules_engine::rulespec::lower_rulespec_str(&single_rule(&format!(
        "{function}(income)"
    )))
    .expect("RuleSpec lowers");
    let empty = if function == "max" {
        ScalarExprSpec::Max { items: vec![] }
    } else {
        ScalarExprSpec::Min { items: vec![] }
    };
    let derived = program
        .derived
        .iter_mut()
        .find(|derived| derived.name == "amount")
        .expect("amount rule");
    derived.semantics = DerivedSemanticsSpec::Scalar {
        expr: empty.clone(),
    };
    for version in &mut derived.versions {
        version.semantics = DerivedSemanticsSpec::Scalar {
            expr: empty.clone(),
        };
    }
    program
}

#[test]
fn a_program_spec_with_an_empty_max_or_min_is_refused_on_load_in_every_mode() {
    for function in ["max", "min"] {
        let program = empty_extremum_spec(function);
        let error = CompiledProgramArtifact::compile(program.clone())
            .err()
            .unwrap_or_else(|| panic!("an empty {function}() must not compile"))
            .to_string();
        assert!(error.contains(&extremum_error(function)), "{error}");

        // The JSON form a caller would send is refused the same way.
        let json = serde_json::to_string(&program).expect("spec serializes");
        let reparsed: ProgramSpec = serde_json::from_str(&json).expect("spec deserializes");
        let rows = [vec![("income", decimal("10"))]];
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            let outcome = run(request(&reparsed, mode.clone(), &rows, "amount"), "amount").0;
            let error = outcome.expect_err("an empty extremum is refused");
            assert!(
                error.contains(&extremum_error(function)),
                "{mode:?}: {error}"
            );
        }
    }
}

/// Replace `rule`'s expression, in its unversioned semantics and every
/// version, in a model program built by hand (bypassing `ProgramSpec`).
fn set_rule_expr(program: &mut Program, rule: &str, expr: ScalarExpr) {
    let derived = program.derived.get_mut(rule).expect("rule exists");
    derived.semantics = DerivedSemantics::Scalar(expr.clone());
    for version in &mut derived.versions {
        version.semantics = DerivedSemantics::Scalar(expr.clone());
    }
}

fn empty_extremum(function: &str) -> ScalarExpr {
    if function == "max" {
        ScalarExpr::Max(vec![])
    } else {
        ScalarExpr::Min(vec![])
    }
}

#[test]
fn a_hand_built_empty_max_or_min_fails_with_explains_error_in_dense_never_a_sentinel() {
    // A `model::Program` built in Rust skips `ProgramSpec::to_program`, so the
    // evaluators must still agree: explain's error, never Decimal::MIN/MAX or
    // f64::MIN/MAX.
    for function in ["max", "min"] {
        let expected = Err(format!("type mismatch: {}", extremum_error(function)));
        let artifact = compile(&single_rule(&format!("{function}(income)")));
        let mut program = artifact.program.to_program().expect("program converts");
        set_rule_expr(&mut program, "amount", empty_extremum(function));

        // With its operands gone the rule reads no input, so `income` is no
        // longer an input slot and the dataset is empty.
        let dataset = DataSet::default();
        let explain = Engine::new(&program, &dataset)
            .evaluate_scalar("amount", "p0", &model_month())
            .map(|value| match value {
                ScalarValue::Decimal(value) => vec![canonical_decimal(value)],
                other => panic!("unexpected value {other:?}"),
            })
            .map_err(|error| error.to_string());
        assert_eq!(explain, expected, "explain {function}()");

        let dense =
            DenseCompiledProgram::from_program(&program, Some("Person")).expect("dense compiles");
        let batch = dense_batch(1, &[("income", DenseColumn::Decimal(vec![Decimal::TEN]))]);
        for (mode, outcome) in ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "amount"))
        {
            assert_eq!(outcome, expected, "dense {mode} {function}()");
        }
    }
}

#[test]
fn a_hand_built_empty_max_or_min_in_a_related_or_lifetime_rule_fails_with_explains_error() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: member_amount
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: max(income)
  - name: family_amount
    kind: derived
    entity: Family
    dtype: Money
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_family.member_amount)
  - name: floor_amount
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: max(bonus)
  - name: lifetime_amount
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(earnings) + floor_amount
"#;
    let artifact = compile(rulespec);
    for function in ["max", "min"] {
        let expected = Err(format!("type mismatch: {}", extremum_error(function)));
        let mut program = artifact.program.to_program().expect("program converts");
        set_rule_expr(&mut program, "member_amount", empty_extremum(function));
        set_rule_expr(&mut program, "floor_amount", empty_extremum(function));

        // Related-row executor: the Family root sums a Person rule.
        let dense = DenseCompiledProgram::from_program(&program, Some("Family"))
            .expect("dense compiles for Family");
        let batch = DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::from([(
                DenseRelationKey {
                    name: "member_of_family".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: vec![0, 2],
                    inputs: HashMap::from([(
                        "income".to_string(),
                        DenseColumn::Decimal(vec![Decimal::ONE, Decimal::TWO]),
                    )]),
                },
            )]),
        };
        for (mode, outcome) in
            ["decimal", "f64"]
                .iter()
                .zip(dense_both(&dense, &batch, "family_amount"))
        {
            assert_eq!(outcome, expected, "dense {mode} related {function}()");
        }

        // Lifetime executor: `floor_amount` is inlined outside the reduction.
        let dense = DenseCompiledProgram::from_program(&program, Some("Worker"))
            .expect("dense compiles for Worker");
        let periods = [tax_year(2026), tax_year(2027)];
        let batches = || {
            periods
                .iter()
                .map(|_| {
                    dense_batch(
                        1,
                        &[
                            ("earnings", DenseColumn::Decimal(vec![Decimal::TEN])),
                            ("bonus", DenseColumn::Decimal(vec![Decimal::ONE])),
                        ],
                    )
                })
                .collect::<Vec<_>>()
        };
        let outputs = ["lifetime_amount".to_string()];
        assert_eq!(
            dense_outcome(
                dense.execute_lifetime(&periods, batches(), &outputs),
                "lifetime_amount"
            ),
            expected,
            "dense lifetime decimal {function}()"
        );
        assert_eq!(
            dense_outcome(
                dense.execute_lifetime_f64(&periods, batches(), &outputs),
                "lifetime_amount"
            ),
            expected,
            "dense lifetime f64 {function}()"
        );
    }
}

// ---------------------------------------------------------------------------
// Property: every mode reads a key the same way
// ---------------------------------------------------------------------------

/// The reference reading of a numeric key, written independently of the
/// engine: a key is its exact integer value when that value is an integer in
/// the i64 range, and is refused otherwise.
fn reference_key(key: Decimal) -> Option<i64> {
    if !key.fract().is_zero() {
        return None;
    }
    let integer = i128::from_str(&key.trunc().to_string()).ok()?;
    i64::try_from(integer).ok()
}

fn reference_float_key(key: f64) -> Option<i64> {
    if !key.is_finite() || key.fract() != 0.0 {
        return None;
    }
    // Every finite integral f64 below 2^127 in magnitude converts to i128
    // exactly; larger ones saturate, which lies outside i64 either way.
    i64::try_from(key as i128).ok()
}

fn table_rate(key: i64) -> Option<&'static str> {
    match key {
        i64::MIN => Some("0.01"),
        -1 => Some("0.05"),
        0 => Some("0.1"),
        1 => Some("0.2"),
        2 => Some("0.3"),
        3 => Some("0.4"),
        i64::MAX => Some("0.99"),
        _ => None,
    }
}

fn missing_row_error(key: i64) -> String {
    format!("parameter `rate_table` has no value for key `{key}` at 2026-01-01")
}

#[test]
fn generated_keys_are_read_identically_by_every_mode_and_match_the_reference() {
    // Invariants, over generated Decimal and f64 keys:
    // 1. A key is accepted exactly when it is an integer in the i64 range;
    //    an accepted key reads exactly that row (no truncation, no
    //    saturation); a refused key is explain's error in every mode.
    // 2. Explain, fast, dense decimal and dense f64 give the same outcome.
    // Deterministic, seeded like the repo's other generated tests, so it adds
    // no dependency.
    let artifact = compile(RATE_TABLE_RULESPEC);
    let dense = dense(&artifact, "Person");
    let edges = [
        "0",
        "2",
        "-1",
        "2.5",
        "-2.5",
        "0.0000000000000000000000000001",
        "9223372036854775807",
        "9223372036854775807.5",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "79228162514264337593543950335",
        "-79228162514264337593543950335",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };
    let mut decimal_keys = edges
        .iter()
        .map(|key| Decimal::from_str(key).expect("edge key"))
        .collect::<Vec<_>>();
    for _ in 0..200 {
        // Small magnitudes, scale 0-3, so roughly a quarter are integral and
        // many of those hit a table row.
        let mantissa = i64::try_from(next() % 10_000).expect("small") - 5_000;
        let scale = u32::try_from(next() % 4).expect("small scale");
        decimal_keys.push(Decimal::new(mantissa, scale));
    }
    let (mut accepted, mut refused) = (0, 0);
    for key in decimal_keys {
        let expected: Outcome = match reference_key(key) {
            None => {
                refused += 1;
                Err(KEY_ERROR.to_string())
            }
            Some(row) => {
                accepted += 1;
                match table_rate(row) {
                    Some(rate) => Ok(vec![rate.to_string()]),
                    None => Err(missing_row_error(row)),
                }
            }
        };
        let rows = [vec![("bracket", decimal(&key.to_string()))]];
        assert_eq!(
            explain(&artifact.program, &rows, "rate"),
            expected,
            "explain {key}"
        );
        assert_eq!(
            fast(&artifact.program, &rows, "rate"),
            expected,
            "fast {key}"
        );
        let batch = rate_batch(1, &[("bracket", DenseColumn::Decimal(vec![key]))]);
        for (mode, outcome) in ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "rate"))
        {
            assert_eq!(outcome, expected, "dense {mode} Decimal {key}");
        }
    }
    assert!(
        accepted > 20 && refused > 20,
        "{accepted} accepted, {refused} refused"
    );

    let mut float_keys = vec![
        0.0,
        -0.0,
        2.0,
        2.5,
        -1.0,
        9_223_372_036_854_775_807.0, // rounds to 2^63
        9_223_372_036_854_775_808.0,
        -9_223_372_036_854_775_808.0,
        -9_223_372_036_854_777_856.0, // next f64 below -2^63
        4_611_686_018_427_387_904.0,  // 2^62, in range and absent
        1e19,
        -1e19,
        1e300,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    for _ in 0..200 {
        // Halves and whole numbers around the table's rows.
        let whole = i32::try_from(next() % 12).expect("small") - 6;
        let half = if next() % 2 == 0 { 0.0 } else { 0.5 };
        float_keys.push(f64::from(whole) + half);
    }
    for key in float_keys {
        let expected: Outcome = match reference_float_key(key) {
            None => Err(KEY_ERROR.to_string()),
            Some(row) => match table_rate(row) {
                Some(rate) => Ok(vec![rate.to_string()]),
                None => Err(missing_row_error(row)),
            },
        };
        let batch = rate_batch(1, &[("bracket", DenseColumn::Float(vec![key]))]);
        for (mode, outcome) in ["decimal", "f64"]
            .iter()
            .zip(dense_both(&dense, &batch, "rate"))
        {
            assert_eq!(outcome, expected, "dense {mode} f64 {key:e}");
        }
    }
}
