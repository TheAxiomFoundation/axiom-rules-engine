//! Property tests for the dense batch-shape contract.
//!
//! Invariants, for every batch the generators produce:
//!
//! 1. Totality: `execute` / `execute_lifetime` return `Ok` or `Err` and never
//!    panic (proptest reports a panic as a failing case).
//! 2. Agreement with the stated contract: a batch is accepted exactly when
//!    `expected_rejection` (the contract written out independently below)
//!    finds nothing wrong with it, and a rejected batch fails with the error
//!    kind the contract names.
//! 3. Differential: an accepted batch's `len` and `sum` outputs equal the
//!    values computed directly from its offsets, root flags and related values.
//!
//! Shapes start valid and small, then take at most one corruption. Every
//! corruption that writes a huge value (at or above 2^50) also makes the batch
//! invalid, so no accepted batch sizes a buffer by one: an evaluator that
//! wrongly accepted it fails its allocation at once instead of paging.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use chrono::NaiveDate;
use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

/// `len(members)` over a relation filtered by a household flag, which has no
/// related input: only the offsets (or a stated count) size its mask.
const ACTIVE_SIZE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_active
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: application_active
  - name: active_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: ActiveUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and household_active
  - name: active_unit_size
    kind: derived
    entity: ActiveUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
"#;

/// The same filtered relation, also summing a required related input.
const ACTIVE_INCOME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_active
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: application_active
  - name: active_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: ActiveUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and household_active
  - name: active_unit_size
    kind: derived
    entity: ActiveUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
  - name: active_unit_income
    kind: derived
    entity: ActiveUnit
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(members.income)
"#;

const LIFETIME_BASE_RULESPEC: &str = r#"
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

/// Values at or above 2^50: no allocation this size can succeed.
const HUGE: [usize; 5] = [
    1 << 50,
    1 << 60,
    isize::MAX as usize,
    usize::MAX - 1,
    usize::MAX,
];

fn compile(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect("dense compilation succeeds")
}

fn month() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn household_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member_of_household".to_string(),
        current_slot: 1,
        related_slot: 0,
    }
}

// ---------------------------------------------------------------------------
// Per-period shapes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Shape {
    row_count: usize,
    /// `None` omits the required root input.
    active: Option<Vec<bool>>,
    /// `None` omits the relation batch.
    offsets: Option<Vec<usize>>,
    related_row_count: Option<usize>,
    /// Supplied related columns. `income` is the one the income program reads;
    /// `note` is never read.
    income: Option<Vec<i64>>,
    note: Option<Vec<bool>>,
}

#[derive(Clone, Copy, Debug)]
enum Corruption {
    None,
    RowCount(usize),
    ActiveLength(usize),
    DropActive,
    DropRelation,
    OffsetsLength(usize),
    OffsetsStart(usize),
    OffsetsDecrease,
    OffsetsLast(usize),
    StatedCount(usize),
    IncomeLength(usize),
    NoteLength(usize),
    DropRelatedColumns,
}

fn valid_shape() -> impl Strategy<Value = Shape> {
    (
        prop::collection::vec((0_usize..4, any::<bool>()), 0..6),
        prop::collection::vec(-1_000_i64..1_000, 24),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(rows, values, state_count, supply_income, supply_note)| {
            let mut offsets = vec![0_usize];
            for (size, _) in &rows {
                offsets.push(offsets.last().copied().unwrap_or(0) + size);
            }
            let related = *offsets.last().expect("offsets are non-empty");
            Shape {
                row_count: rows.len(),
                active: Some(rows.iter().map(|(_, active)| *active).collect()),
                offsets: Some(offsets),
                related_row_count: state_count.then_some(related),
                income: supply_income.then(|| values[..related].to_vec()),
                note: supply_note.then(|| vec![true; related]),
            }
        })
}

fn corruption() -> impl Strategy<Value = Corruption> {
    let small = 0_usize..8;
    let huge = prop::sample::select(HUGE.to_vec());
    prop_oneof![
        6 => Just(Corruption::None),
        1 => small.clone().prop_map(Corruption::RowCount),
        1 => huge.clone().prop_map(Corruption::RowCount),
        1 => small.clone().prop_map(Corruption::ActiveLength),
        1 => Just(Corruption::DropActive),
        1 => Just(Corruption::DropRelation),
        1 => small.clone().prop_map(Corruption::OffsetsLength),
        1 => (1_usize..4).prop_map(Corruption::OffsetsStart),
        1 => Just(Corruption::OffsetsDecrease),
        1 => huge.clone().prop_map(Corruption::OffsetsLast),
        1 => small.clone().prop_map(Corruption::StatedCount),
        1 => huge.prop_map(Corruption::StatedCount),
        1 => small.clone().prop_map(Corruption::IncomeLength),
        1 => small.prop_map(Corruption::NoteLength),
        1 => Just(Corruption::DropRelatedColumns),
    ]
}

fn corrupt(mut shape: Shape, corruption: Corruption) -> Shape {
    match corruption {
        Corruption::None => {}
        Corruption::RowCount(value) => shape.row_count = value,
        Corruption::ActiveLength(length) => {
            shape.active = Some(vec![true; length]);
        }
        Corruption::DropActive => shape.active = None,
        Corruption::DropRelation => shape.offsets = None,
        Corruption::OffsetsLength(length) => {
            if let Some(offsets) = &mut shape.offsets {
                let last = offsets.last().copied().unwrap_or(0);
                offsets.resize(length, last);
            }
        }
        Corruption::OffsetsStart(start) => {
            if let Some(offsets) = &mut shape.offsets {
                for offset in offsets.iter_mut() {
                    *offset += start;
                }
            }
        }
        Corruption::OffsetsDecrease => {
            if let Some(offsets) = &mut shape.offsets {
                if let Some(position) = offsets.windows(2).position(|pair| pair[0] < pair[1]) {
                    offsets.swap(position, position + 1);
                }
            }
        }
        Corruption::OffsetsLast(value) => {
            if let Some(last) = shape
                .offsets
                .as_mut()
                .and_then(|offsets| offsets.last_mut())
            {
                *last = value;
            }
        }
        Corruption::StatedCount(value) => shape.related_row_count = Some(value),
        Corruption::IncomeLength(length) => shape.income = Some(vec![1; length]),
        Corruption::NoteLength(length) => shape.note = Some(vec![false; length]),
        Corruption::DropRelatedColumns => {
            shape.income = None;
            shape.note = None;
        }
    }
    shape
}

fn batch(shape: &Shape) -> DenseBatchSpec {
    let mut inputs = HashMap::new();
    if let Some(active) = &shape.active {
        inputs.insert(
            "application_active".to_string(),
            DenseColumn::Bool(active.clone()),
        );
    }
    let mut relations = HashMap::new();
    if let Some(offsets) = &shape.offsets {
        let mut related = HashMap::new();
        if let Some(income) = &shape.income {
            related.insert("income".to_string(), DenseColumn::Integer(income.clone()));
        }
        if let Some(note) = &shape.note {
            related.insert("note".to_string(), DenseColumn::Bool(note.clone()));
        }
        relations.insert(
            household_key(),
            DenseRelationBatchSpec {
                offsets: offsets.clone(),
                inputs: related,
                related_row_count: shape.related_row_count,
            },
        );
    }
    DenseBatchSpec {
        row_count: shape.row_count,
        inputs,
        relations,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Rejection {
    TypeMismatch,
    MissingInput,
    UnknownRelation,
}

/// The batch contract, written independently of `bind_batch`, in the order
/// the binder reports problems.
fn expected_rejection(shape: &Shape, reads_income: bool) -> Option<Rejection> {
    // Row counts no executor buffer can hold. The binder's exact bound is
    // private; every huge value the generator writes exceeds it or breaks
    // another rule below, so rejecting them all is exact.
    if HUGE.contains(&shape.row_count) {
        return Some(Rejection::TypeMismatch);
    }
    match &shape.active {
        Some(active) if active.len() != shape.row_count => return Some(Rejection::TypeMismatch),
        Some(_) => {}
        None => return Some(Rejection::MissingInput),
    }
    let Some(offsets) = &shape.offsets else {
        return Some(Rejection::UnknownRelation);
    };
    if offsets.len() != shape.row_count + 1
        || offsets[0] != 0
        || offsets.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Some(Rejection::TypeMismatch);
    }
    let related = offsets[shape.row_count];
    if shape
        .related_row_count
        .is_some_and(|count| count != related)
    {
        return Some(Rejection::TypeMismatch);
    }
    let lengths = [
        shape.income.as_ref().map(Vec::len),
        shape.note.as_ref().map(Vec::len),
    ];
    if lengths.iter().flatten().any(|length| *length != related) {
        return Some(Rejection::TypeMismatch);
    }
    if reads_income && shape.income.is_none() {
        return Some(Rejection::MissingInput);
    }
    let supplied_any = lengths.iter().any(Option::is_some);
    if related > 0 && shape.related_row_count.is_none() && !supplied_any {
        return Some(Rejection::TypeMismatch);
    }
    if HUGE.contains(&related) {
        return Some(Rejection::TypeMismatch);
    }
    None
}

fn rejection_kind(error: &EvalError) -> Option<Rejection> {
    match error {
        EvalError::TypeMismatch(_) => Some(Rejection::TypeMismatch),
        EvalError::MissingInput { .. } => Some(Rejection::MissingInput),
        EvalError::UnknownRelation(_) => Some(Rejection::UnknownRelation),
        _ => None,
    }
}

fn integers(result: &DenseExecutionResult, output: &str) -> Vec<i64> {
    match result.outputs.get(output) {
        Some(DenseOutputValue::Scalar(DenseColumn::Integer(values))) => values.clone(),
        other => panic!("expected an integer column for `{output}`, got {other:?}"),
    }
}

fn decimals(result: &DenseExecutionResult, output: &str) -> Vec<Decimal> {
    match result.outputs.get(output) {
        Some(DenseOutputValue::Scalar(DenseColumn::Decimal(values))) => values.clone(),
        other => panic!("expected a decimal column for `{output}`, got {other:?}"),
    }
}

fn check_shape(program: &DenseCompiledProgram, shape: &Shape, reads_income: bool) {
    let mut outputs = vec!["active_unit_size".to_string()];
    if reads_income {
        outputs.push("active_unit_income".to_string());
    }
    let result = program.execute(&month(), batch(shape), &outputs);
    let expected = expected_rejection(shape, reads_income);
    match (&result, &expected) {
        (Ok(result), None) => {
            let offsets = shape
                .offsets
                .as_ref()
                .expect("accepted batches have offsets");
            let active = shape.active.as_ref().expect("accepted batches have flags");
            let sizes = (0..shape.row_count)
                .map(|row| {
                    if active[row] {
                        (offsets[row + 1] - offsets[row]) as i64
                    } else {
                        0
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(integers(result, "active_unit_size"), sizes, "{shape:?}");
            if reads_income {
                let income = shape
                    .income
                    .as_ref()
                    .expect("the income column is required");
                let totals = (0..shape.row_count)
                    .map(|row| {
                        let total = if active[row] {
                            income[offsets[row]..offsets[row + 1]].iter().sum::<i64>()
                        } else {
                            0
                        };
                        Decimal::from_i64(total).expect("small totals are decimal")
                    })
                    .collect::<Vec<_>>();
                assert_eq!(decimals(result, "active_unit_income"), totals, "{shape:?}");
            }
        }
        (Err(error), Some(kind)) => {
            assert_eq!(
                rejection_kind(error).as_ref(),
                Some(kind),
                "{shape:?} failed with {error}"
            );
        }
        (Ok(result), Some(kind)) => {
            panic!(
                "expected {kind:?}, but {shape:?} was accepted: {:?}",
                result.outputs
            )
        }
        (Err(error), None) => panic!("{shape:?} is valid but failed: {error}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn shapes_without_related_reads_agree_with_the_contract(
        shape in (valid_shape(), corruption()).prop_map(|(shape, c)| corrupt(shape, c)),
    ) {
        let program = compile(ACTIVE_SIZE_RULESPEC, "ActiveUnit");
        check_shape(&program, &shape, false);
    }

    #[test]
    fn shapes_with_related_reads_agree_with_the_contract(
        shape in (valid_shape(), corruption()).prop_map(|(shape, c)| corrupt(shape, c)),
    ) {
        let program = compile(ACTIVE_INCOME_RULESPEC, "ActiveUnit");
        check_shape(&program, &shape, true);
    }
}

// ---------------------------------------------------------------------------
// Lifetime period-invariance over every column dtype
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Values {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<i64>),
    Float(Vec<f64>),
    Text(Vec<String>),
    Date(Vec<u32>),
}

fn values(rows: usize) -> impl Strategy<Value = Values> {
    // Draw from tiny domains so equal columns across periods are common.
    let float = prop_oneof![
        4 => (0_i64..3).prop_map(|value| value as f64),
        1 => Just(f64::NAN),
    ];
    prop_oneof![
        prop::collection::vec(any::<bool>(), rows).prop_map(Values::Bool),
        prop::collection::vec(0_i64..3, rows).prop_map(Values::Integer),
        prop::collection::vec(0_i64..3, rows).prop_map(Values::Decimal),
        prop::collection::vec(float, rows).prop_map(Values::Float),
        prop::collection::vec(prop::sample::select(vec!["a", "b"]), rows)
            .prop_map(|values| { Values::Text(values.into_iter().map(str::to_string).collect()) }),
        prop::collection::vec(1_u32..3, rows).prop_map(Values::Date),
    ]
}

fn column(values: &Values) -> DenseColumn {
    match values {
        Values::Bool(values) => DenseColumn::Bool(values.clone()),
        Values::Integer(values) => DenseColumn::Integer(values.clone()),
        Values::Decimal(values) => {
            DenseColumn::Decimal(values.iter().map(|value| Decimal::from(*value)).collect())
        }
        Values::Float(values) => DenseColumn::Float(values.clone()),
        Values::Text(values) => DenseColumn::Text(values.clone()),
        Values::Date(values) => DenseColumn::Date(
            values
                .iter()
                .map(|day| NaiveDate::from_ymd_opt(2000, 1, *day).expect("date"))
                .collect(),
        ),
    }
}

/// Numeric cells as exact decimals; `None` for a non-numeric column, and a
/// `None` cell for a float the decimal type cannot represent (NaN).
fn numeric_cells(values: &Values) -> Option<Vec<Option<Decimal>>> {
    match values {
        Values::Integer(values) | Values::Decimal(values) => Some(
            values
                .iter()
                .map(|value| Some(Decimal::from(*value)))
                .collect(),
        ),
        Values::Float(values) => Some(
            values
                .iter()
                .map(|value| Decimal::from_f64(*value))
                .collect(),
        ),
        _ => None,
    }
}

/// Whether `later` provably carries `first`'s value in every row: same
/// non-numeric dtype and values, or numerically equal representable cells. A
/// dtype change is never invariant, even with no rows.
fn invariant(first: &Values, later: &Values) -> bool {
    match (first, later) {
        (Values::Bool(a), Values::Bool(b)) => a == b,
        (Values::Text(a), Values::Text(b)) => a == b,
        (Values::Date(a), Values::Date(b)) => a == b,
        _ => match (numeric_cells(first), numeric_cells(later)) {
            (Some(a), Some(b)) => a
                .iter()
                .zip(&b)
                .all(|(a, b)| matches!((a, b), (Some(a), Some(b)) if a == b)),
            _ => false,
        },
    }
}

fn lifetime_case() -> impl Strategy<Value = (Vec<Values>, bool)> {
    (0_usize..4, 1_usize..4, any::<bool>()).prop_flat_map(|(rows, periods, f64_mode)| {
        (prop::collection::vec(values(rows), periods), Just(f64_mode))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn period_invariance_is_total_over_dtypes((bases, f64_mode) in lifetime_case()) {
        let program = compile(LIFETIME_BASE_RULESPEC, "Worker");
        let rows = column(&bases[0]).len();
        let periods = (0..bases.len())
            .map(|index| {
                let year = 2001 + index as i32;
                Period {
                    kind: PeriodKind::TaxYear,
                    start: NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
                    end: NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
                }
            })
            .collect::<Vec<_>>();
        let batches = bases
            .iter()
            .map(|base| DenseBatchSpec {
                row_count: rows,
                inputs: HashMap::from([
                    ("earnings".to_string(), DenseColumn::Float(vec![1.0; rows])),
                    ("base".to_string(), column(base)),
                ]),
                relations: HashMap::new(),
            })
            .collect::<Vec<_>>();
        let outputs = ["shifted_total".to_string()];
        let result = if f64_mode {
            program.execute_lifetime_f64(&periods, batches, &outputs)
        } else {
            program.execute_lifetime(&periods, batches, &outputs)
        };

        let invariant = bases[1..].iter().all(|later| invariant(&bases[0], later));
        // `+` needs a numeric base. f64 mode adds NaN like any float; Decimal
        // mode cannot represent it. (With two or more periods a NaN is never
        // invariant, so this only matters for a single period.)
        let numeric = numeric_cells(&bases[0])
            .is_some_and(|cells| f64_mode || cells.iter().all(Option::is_some));
        match result {
            Ok(result) => prop_assert!(
                invariant && numeric,
                "accepted {bases:?}: {:?}",
                result.outputs
            ),
            Err(EvalError::LifetimePeriodVaryingInput { .. }) => {
                prop_assert!(!invariant, "{bases:?} is invariant but was reported as varying")
            }
            // An invariant base still has to be numeric (and representable in
            // Decimal mode) for `+`.
            Err(EvalError::TypeMismatch(_)) => prop_assert!(
                invariant && !numeric,
                "{bases:?} failed with a type mismatch"
            ),
            Err(other) => prop_assert!(false, "{bases:?} failed with {other}"),
        }
    }
}
