//! Property tests for the dense batch-shape contract.
//!
//! Invariants, for every batch the generators produce:
//!
//! 1. Totality: `execute` and `execute_lifetime[_f64]` return `Ok` or `Err`
//!    and never panic (proptest reports a panic as a failing case).
//! 2. Agreement with the stated contract: a batch is refused exactly when
//!    `expected_refusal`, the contract written out independently below, finds
//!    a problem, with the error kind it names and a message naming the rule.
//! 3. Differential: an accepted batch's `len(members)` and `sum(members.income)`
//!    equal the values computed directly from its offsets, household flags and
//!    incomes; a missing income fails the first row that reads one, as lazy
//!    evaluation does.
//! 4. Period invariance is total over dtypes: lifetime execution fails exactly
//!    the rows whose base value varies across periods or is not numeric, and
//!    reports the lowest such row with the values it compared.
//!
//! Shapes start as small batches binding accepts (every nonzero related count
//! confirmed), then take at most one corruption. A huge
//! count is only ever written where the contract refuses it before anything
//! is allocated from it, or as a stated count binding must fail to reserve
//! (at least 2^50 owners), so no case asks the allocator for real memory.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{JudgmentOutcome, Period, PeriodKind, ScalarValue};
use chrono::NaiveDate;
use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

/// `len(members)` over a relation filtered by a household flag, which has no
/// related input: only a supplied column or a stated count confirms it.
const ACTIVE_SIZE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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

/// The same filtered relation, also summing a related input it reads.
const ACTIVE_INCOME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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

/// Counts at or above 2^50: no host can allocate one `usize` per row.
const HUGE: [usize; 5] = [
    1 << 50,
    1 << 60,
    isize::MAX as usize,
    usize::MAX - 1,
    usize::MAX,
];

/// The binder's row bound, restated: `isize::MAX` over the widest per-row
/// buffer element.
const MAX_ROWS: usize = {
    let sizes = [
        size_of::<bool>(),
        size_of::<i64>(),
        size_of::<f64>(),
        size_of::<usize>(),
        size_of::<Decimal>(),
        size_of::<String>(),
        size_of::<NaiveDate>(),
        size_of::<JudgmentOutcome>(),
        size_of::<ScalarValue>(),
    ];
    let mut widest = 1;
    let mut index = 0;
    while index < sizes.len() {
        if sizes[index] > widest {
            widest = sizes[index];
        }
        index += 1;
    }
    isize::MAX as usize / widest
};

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

fn only_key(program: &DenseCompiledProgram) -> DenseRelationKey {
    program.relations()[0].key.clone()
}

// ---------------------------------------------------------------------------
// Per-period shapes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Shape {
    row_count: usize,
    active: Vec<bool>,
    /// `None` omits the relation batch.
    offsets: Option<Vec<usize>>,
    related_row_count: Option<usize>,
    /// Supplied related columns. The income program reads `income`; nothing
    /// reads `note`.
    income: Option<Vec<i64>>,
    note: Option<Vec<bool>>,
}

#[derive(Clone, Copy, Debug)]
enum Corruption {
    None,
    RowCount(usize),
    ActiveLength(usize),
    DropRelation,
    OffsetsLength(usize),
    OffsetsStart(usize),
    OffsetsDecrease,
    OffsetsLast(usize),
    StatedCount(usize),
    /// Offsets and stated count agree on a huge value; no column.
    HugeClaim(usize),
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
            // Binding accepts the starting shape: with no column to confirm a
            // nonzero related count, it is stated.
            let state_count = state_count || !(supply_income || supply_note);
            Shape {
                row_count: rows.len(),
                active: rows.iter().map(|(_, active)| *active).collect(),
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
        1 => Just(Corruption::DropRelation),
        1 => small.clone().prop_map(Corruption::OffsetsLength),
        1 => (1_usize..4).prop_map(Corruption::OffsetsStart),
        2 => Just(Corruption::OffsetsDecrease),
        1 => huge.clone().prop_map(Corruption::OffsetsLast),
        1 => small.clone().prop_map(Corruption::StatedCount),
        1 => huge.clone().prop_map(Corruption::StatedCount),
        1 => huge.prop_map(Corruption::HugeClaim),
        1 => small.clone().prop_map(Corruption::IncomeLength),
        1 => small.prop_map(Corruption::NoteLength),
        1 => Just(Corruption::DropRelatedColumns),
    ]
}

fn corrupt(mut shape: Shape, corruption: Corruption) -> Shape {
    match corruption {
        Corruption::None => {}
        Corruption::RowCount(value) => shape.row_count = value,
        Corruption::ActiveLength(length) => shape.active = vec![true; length],
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
            // Raise an interior boundary above the next one, keeping the
            // first offset at 0 and the last unchanged, so only the
            // non-decreasing rule can fire.
            if let Some(offsets) = &mut shape.offsets
                && offsets.len() >= 3
            {
                offsets[1] = offsets[2] + 1;
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
        Corruption::HugeClaim(value) => {
            if let Some(last) = shape
                .offsets
                .as_mut()
                .and_then(|offsets| offsets.last_mut())
            {
                *last = value;
            }
            shape.related_row_count = Some(value);
            shape.income = None;
            shape.note = None;
        }
        Corruption::IncomeLength(length) => shape.income = Some(vec![1; length]),
        Corruption::NoteLength(length) => shape.note = Some(vec![false; length]),
        Corruption::DropRelatedColumns => {
            shape.income = None;
            shape.note = None;
        }
    }
    shape
}

fn batch(program: &DenseCompiledProgram, shape: &Shape) -> DenseBatchSpec {
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
            only_key(program),
            DenseRelationBatchSpec {
                offsets: offsets.clone(),
                inputs: related,
                related_row_count: shape.related_row_count,
            },
        );
    }
    DenseBatchSpec {
        row_count: shape.row_count,
        inputs: HashMap::from([(
            "application_active".to_string(),
            DenseColumn::Bool(shape.active.clone()),
        )]),
        relations,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    TypeMismatch(&'static str),
    MissingInput,
    UnknownRelation,
}

/// The batch contract, written independently of the binder, in the order it
/// reports problems, then the first row lazy evaluation fails.
fn expected_refusal(shape: &Shape, reads_income: bool) -> Option<Refusal> {
    use Refusal::TypeMismatch;
    if shape.row_count > MAX_ROWS {
        return Some(TypeMismatch("exceeds"));
    }
    if shape.active.len() != shape.row_count {
        return Some(TypeMismatch("dense root input `application_active`"));
    }
    let Some(offsets) = &shape.offsets else {
        return Some(Refusal::UnknownRelation);
    };
    if offsets.len() != shape.row_count + 1 {
        return Some(TypeMismatch("offsets must have length"));
    }
    if offsets[0] != 0 {
        return Some(TypeMismatch("offsets must start at 0"));
    }
    if offsets.windows(2).any(|pair| pair[0] > pair[1]) {
        return Some(TypeMismatch("must be non-decreasing"));
    }
    let related = offsets[shape.row_count];
    if shape
        .related_row_count
        .is_some_and(|count| count != related)
    {
        return Some(TypeMismatch("but related_row_count is"));
    }
    let income_length = shape.income.as_ref().map(Vec::len);
    let note_length = shape.note.as_ref().map(Vec::len);
    if reads_income && income_length.is_some_and(|length| length != related) {
        return Some(TypeMismatch("input `income`"));
    }
    let confirmed =
        shape.related_row_count.is_some() || [income_length, note_length].contains(&Some(related));
    if related > 0 && !confirmed {
        // A disagreeing column is named, smallest name first.
        return Some(match (income_length, note_length) {
            (Some(_), _) => TypeMismatch("input `income`"),
            (None, Some(_)) => TypeMismatch("input `note`"),
            (None, None) => TypeMismatch("no related column"),
        });
    }
    if related > MAX_ROWS {
        return Some(TypeMismatch("can hold"));
    }
    // Columns no schema reads are checked after the schema walk, smallest
    // name first.
    let unread = [
        (!reads_income).then_some(("input `income`", income_length)),
        Some(("input `note`", note_length)),
    ];
    for (fragment, length) in unread.into_iter().flatten() {
        if length.is_some_and(|length| length != related) {
            return Some(TypeMismatch(fragment));
        }
    }
    // Then owners are reserved: a stated count of at least 2^50 cannot be.
    if HUGE.contains(&related) {
        return Some(TypeMismatch("more than this process can allocate"));
    }
    // A missing income fails the first row that reads one: an active
    // household with a member.
    if reads_income && shape.income.is_none() {
        let reads =
            (0..shape.row_count).any(|row| shape.active[row] && offsets[row + 1] > offsets[row]);
        if reads {
            return Some(Refusal::MissingInput);
        }
    }
    None
}

fn check_shape(program: &DenseCompiledProgram, shape: &Shape, reads_income: bool) {
    let mut outputs = vec!["active_unit_size".to_string()];
    if reads_income {
        outputs.push("active_unit_income".to_string());
    }
    let result = program.execute(&month(), batch(program, shape), &outputs);
    match (result, expected_refusal(shape, reads_income)) {
        (Ok(result), None) => check_values(&result, shape, reads_income),
        (Err(EvalError::TypeMismatch(message)), Some(Refusal::TypeMismatch(fragment))) => {
            assert!(
                message.contains(fragment),
                "{shape:?}: expected `{fragment}`, got: {message}"
            );
        }
        (Err(EvalError::MissingInput { .. }), Some(Refusal::MissingInput))
        | (Err(EvalError::UnknownRelation(_)), Some(Refusal::UnknownRelation)) => {}
        (Ok(result), Some(refusal)) => panic!(
            "expected {refusal:?}, but {shape:?} was accepted: {:?}",
            result.outputs
        ),
        (Err(error), expected) => panic!("{shape:?}: expected {expected:?}, got {error}"),
    }
}

fn check_values(result: &DenseExecutionResult, shape: &Shape, reads_income: bool) {
    let offsets = shape
        .offsets
        .as_ref()
        .expect("accepted batches have offsets");
    let members = |row: usize| offsets[row]..offsets[row + 1];
    let sizes = (0..shape.row_count)
        .map(|row| {
            if shape.active[row] {
                members(row).len() as i64
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    match result.outputs.get("active_unit_size") {
        Some(DenseOutputValue::Scalar(DenseColumn::Integer(values))) => {
            assert_eq!(values, &sizes, "{shape:?}");
        }
        other => panic!("unexpected size column {other:?}"),
    }
    if reads_income {
        let totals = (0..shape.row_count)
            .map(|row| {
                let total = match (&shape.income, shape.active[row]) {
                    (Some(income), true) => income[members(row)].iter().sum::<i64>(),
                    _ => 0,
                };
                Decimal::from(total)
            })
            .collect::<Vec<_>>();
        match result.outputs.get("active_unit_income") {
            Some(DenseOutputValue::Scalar(DenseColumn::Decimal(values))) => {
                assert_eq!(values, &totals, "{shape:?}");
            }
            other => panic!("unexpected income column {other:?}"),
        }
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
// Lifetime period invariance over every column dtype
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

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2000, 1, day).expect("date")
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
        Values::Date(values) => DenseColumn::Date(values.iter().map(|day| date(*day)).collect()),
    }
}

/// A numeric cell as an exact decimal; `None` for a non-numeric column or a
/// float the decimal type cannot represent (NaN).
fn decimal_cell(values: &Values, row: usize) -> Option<Decimal> {
    match values {
        Values::Integer(values) | Values::Decimal(values) => Some(Decimal::from(values[row])),
        Values::Float(values) => Decimal::from_f64(values[row]),
        _ => None,
    }
}

/// Whether `later` provably carries `first`'s value at `row`: equal values of
/// the same non-numeric dtype, or equal representable numbers across any
/// numeric dtypes. A change to or from a non-numeric dtype always differs.
fn same_at(first: &Values, later: &Values, row: usize) -> bool {
    match (first, later) {
        (Values::Bool(a), Values::Bool(b)) => a[row] == b[row],
        (Values::Text(a), Values::Text(b)) => a[row] == b[row],
        (Values::Date(a), Values::Date(b)) => a[row] == b[row],
        _ => matches!(
            (decimal_cell(first, row), decimal_cell(later, row)),
            (Some(a), Some(b)) if a == b
        ),
    }
}

/// The engine's rendering of a cell in an error message.
fn label(values: &Values, row: usize) -> String {
    match values {
        Values::Bool(values) => values[row].to_string(),
        Values::Integer(values) | Values::Decimal(values) => values[row].to_string(),
        Values::Float(values) => values[row].to_string(),
        Values::Text(values) => format!("{:?}", values[row]),
        Values::Date(values) => date(values[row]).to_string(),
    }
}

/// `+` reads the bound base: a non-numeric dtype fails every row, and a NaN
/// fails its row in Decimal mode only.
fn adds_at(base: &Values, row: usize, f64_mode: bool) -> bool {
    match base {
        Values::Integer(_) | Values::Decimal(_) => true,
        Values::Float(_) => f64_mode || decimal_cell(base, row).is_some(),
        _ => false,
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

        // The lowest failing row decides the error: the first later period
        // that differs from the first, else a base `+` cannot read.
        let first = &bases[0];
        let period_label = |index: usize| {
            let year = 2001 + index;
            format!("in {year}-01-01..{year}-12-31")
        };
        let failure = (0..rows).find_map(|row| {
            if let Some(later) = (1..bases.len()).find(|later| !same_at(first, &bases[*later], row)) {
                Some(Err((label(first, row), period_label(later), label(&bases[later], row))))
            } else if !adds_at(first, row, f64_mode) {
                Some(Ok(()))
            } else {
                None
            }
        });
        match (result, failure) {
            (Ok(result), None) => {
                // One unit of earnings per period, plus the bound base.
                let totals = match result.outputs.get("shifted_total") {
                    Some(DenseOutputValue::Scalar(DenseColumn::Decimal(values))) => values
                        .iter()
                        .map(|value| value.to_string().parse::<f64>().expect("decimal"))
                        .collect::<Vec<_>>(),
                    Some(DenseOutputValue::Scalar(DenseColumn::Float(values))) => values.clone(),
                    other => panic!("unexpected total column {other:?}"),
                };
                prop_assert_eq!(totals.len(), rows);
                for (row, total) in totals.iter().enumerate() {
                    let base = match first {
                        Values::Integer(values) | Values::Decimal(values) => values[row] as f64,
                        Values::Float(values) => values[row],
                        _ => unreachable!("only numeric bases add"),
                    };
                    let expected = bases.len() as f64 + base;
                    prop_assert!(
                        *total == expected || (total.is_nan() && expected.is_nan()),
                        "{:?} (f64 {}): row {} totals {} not {}",
                        bases,
                        f64_mode,
                        row,
                        total,
                        expected
                    );
                }
            }
            (
                Err(EvalError::LifetimePeriodVaryingInput {
                    first_period,
                    first_value,
                    second_period,
                    second_value,
                    ..
                }),
                Some(Err((first_label, later_period, later_label))),
            ) => {
                prop_assert_eq!(first_period, period_label(0), "{:?}", bases);
                prop_assert_eq!(first_value, first_label, "{:?}", bases);
                prop_assert_eq!(second_period, later_period, "{:?}", bases);
                prop_assert_eq!(second_value, later_label, "{:?}", bases);
            }
            (Err(EvalError::TypeMismatch(_)), Some(Ok(()))) => {}
            (result, failure) => prop_assert!(
                false,
                "{:?} (f64 {}): expected {:?}, got {:?}",
                bases,
                f64_mode,
                failure,
                result.map(|result| result.outputs)
            ),
        }
    }
}
