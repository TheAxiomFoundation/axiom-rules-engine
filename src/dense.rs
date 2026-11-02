// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::{
    ArithmeticError, EvalError, checked_add, checked_div, checked_mul, checked_sub,
    decimal_tie_prefers, extreme_value_type_error, relation_member_outside_derived_relation,
};
use crate::lazy::{
    Quoted, QuotedError, QuotedValue, RowError, RowErrors, RowMask, copied_rows, filled_rows,
    row_buffer, share_quoted,
};
use crate::model::{
    ComparisonOp, DType, Derived, DerivedSemantics, Extreme, IndexedParameter, JudgmentExpr,
    JudgmentOutcome, OverPeriodsKind, Period, Program, RelatedValueRef, Rounding, RoundingMode,
    SCALAR_ENTITY, ScalarExpr, ScalarValue, Term,
};

#[cfg(test)]
#[path = "dense_relation_chain_tests.rs"]
mod relation_chain_tests;

#[derive(Clone, Debug)]
pub enum DenseColumn {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<Decimal>),
    /// f64 column for the fast (`execute_f64`) execution mode. Decimal mode
    /// also accepts these as inputs, converting losslessly where possible.
    Float(Vec<f64>),
    /// One shared text per row. Rows holding the same value share one
    /// allocation: dense fills a text column by cloning the `Arc`, never the
    /// text, so a broadcast, projection, lookup or read costs a pointer a row
    /// however long the text is.
    Text(Vec<Arc<str>>),
    Date(Vec<chrono::NaiveDate>),
}

impl DenseColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::Integer(values) => values.len(),
            Self::Decimal(values) => values.len(),
            Self::Float(values) => values.len(),
            Self::Text(values) => values.len(),
            Self::Date(values) => values.len(),
        }
    }

    pub fn scalar_value_at(&self, index: usize, dtype: &DType) -> ScalarValue {
        match (self, dtype) {
            (Self::Bool(values), _) => ScalarValue::Bool(values[index]),
            (Self::Integer(values), DType::Integer) => ScalarValue::Integer(values[index]),
            (Self::Integer(values), _) => ScalarValue::Decimal(Decimal::from(values[index])),
            (Self::Decimal(values), _) => ScalarValue::Decimal(values[index]),
            (Self::Float(values), _) => {
                ScalarValue::Decimal(Decimal::from_f64(values[index]).unwrap_or_default())
            }
            (Self::Text(values), _) => ScalarValue::Text(values[index].to_string()),
            (Self::Date(values), _) => ScalarValue::Date(values[index]),
        }
    }
}

/// Numeric type the dense executor evaluates arithmetic in. `Decimal` is the
/// canonical mode (exact, matches the sparse engine); `f64` trades exactness
/// for roughly an order of magnitude in throughput for bulk microsimulation.
trait DenseNum: Copy + PartialOrd {
    const ZERO: Self;

    /// Arithmetic. The trait deliberately has no `std::ops` bounds, so generic
    /// code cannot reach rust_decimal's panicking operators. `Decimal` reports
    /// a result outside its range as `ArithmeticOverflow` and a zero divisor as
    /// `DivisionByZero`, the same errors as the explain and bulk paths; `f64`
    /// follows IEEE 754 except that a zero divisor is `DivisionByZero` here too.
    fn try_add(self, other: Self) -> Result<Self, ArithmeticError>;
    fn try_sub(self, other: Self) -> Result<Self, ArithmeticError>;
    fn try_mul(self, other: Self) -> Result<Self, ArithmeticError>;
    fn try_div(self, other: Self) -> Result<Self, ArithmeticError>;
    fn from_decimal(value: &Decimal) -> Self;
    fn from_integer(value: i64) -> Self;
    /// An `f64` column value in this mode, or `None` when it has no value here
    /// (a non-finite or out-of-range float in `Decimal` mode).
    fn from_float(value: f64) -> Option<Self>;
    fn ceil(self) -> Self;
    fn floor(self) -> Self;
    /// Round to a currency scale under a declared mode. `Decimal` rounds
    /// exactly (identical to the sparse and bulk paths); `f64` is best-effort,
    /// consistent with this mode being for throughput, not exact legal
    /// determinations.
    fn round_to(self, rounding: Rounding) -> Self;
    /// Truncate toward zero to an exact `i64`, or `None` when the value is
    /// non-finite or lies beyond the `i64` range. Used to read the `n` of
    /// `sum_top_n_over_periods`: `None` (and any out-of-range integer) is a hard
    /// error under the strict n contract, never a silent saturation.
    fn try_to_i64_trunc(self) -> Option<i64>;
    /// A total order for ranking period values (`sum_top_n_over_periods`).
    /// `f64` ranks NaN above every number, so a NaN value is always selected
    /// and poisons the sum, as it does `sum_over_periods`; ordering it with
    /// `partial_cmp` made the sort's comparator inconsistent, which lets
    /// `sort_by` panic.
    fn rank_cmp(&self, other: &Self) -> std::cmp::Ordering;
    /// Whether `self` replaces an equal `other` as a `min`/`max` over related
    /// entities, so the result does not depend on member order: explain's
    /// rule for decimals (larger scale, then non-negative zero), and
    /// non-negative zero for `f64`.
    fn tie_prefers(&self, other: &Self) -> bool;
    /// Wrap evaluated values in the column variant for this mode.
    fn into_column(values: Vec<Self>) -> DenseColumn;
}

impl DenseNum for Decimal {
    const ZERO: Self = Decimal::ZERO;

    #[inline]
    fn try_add(self, other: Self) -> Result<Self, ArithmeticError> {
        checked_add(self, other)
    }

    #[inline]
    fn try_sub(self, other: Self) -> Result<Self, ArithmeticError> {
        checked_sub(self, other)
    }

    #[inline]
    fn try_mul(self, other: Self) -> Result<Self, ArithmeticError> {
        checked_mul(self, other)
    }

    #[inline]
    fn try_div(self, other: Self) -> Result<Self, ArithmeticError> {
        checked_div(self, other)
    }

    fn from_decimal(value: &Decimal) -> Self {
        *value
    }

    fn from_integer(value: i64) -> Self {
        Decimal::from(value)
    }

    fn from_float(value: f64) -> Option<Self> {
        Decimal::from_f64(value)
    }

    fn ceil(self) -> Self {
        Decimal::ceil(&self)
    }

    fn floor(self) -> Self {
        Decimal::floor(&self)
    }

    fn round_to(self, rounding: Rounding) -> Self {
        rounding.apply(self)
    }

    fn rank_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cmp(other)
    }

    fn tie_prefers(&self, other: &Self) -> bool {
        decimal_tie_prefers(self, other)
    }

    fn try_to_i64_trunc(self) -> Option<i64> {
        // Truncate toward zero; `to_i64` returns None when the truncated value
        // is beyond the i64 range, which the strict n contract treats as a hard
        // error (never a saturation to i64::MAX).
        self.trunc().to_i64()
    }

    fn into_column(values: Vec<Self>) -> DenseColumn {
        DenseColumn::Decimal(values)
    }
}

impl DenseNum for f64 {
    const ZERO: Self = 0.0;

    #[inline]
    fn try_add(self, other: Self) -> Result<Self, ArithmeticError> {
        Ok(self + other)
    }

    #[inline]
    fn try_sub(self, other: Self) -> Result<Self, ArithmeticError> {
        Ok(self - other)
    }

    #[inline]
    fn try_mul(self, other: Self) -> Result<Self, ArithmeticError> {
        Ok(self * other)
    }

    #[inline]
    fn try_div(self, other: Self) -> Result<Self, ArithmeticError> {
        if other == 0.0 {
            return Err(ArithmeticError::DivisionByZero);
        }
        Ok(self / other)
    }

    fn from_decimal(value: &Decimal) -> Self {
        value.to_f64().unwrap_or(f64::NAN)
    }

    fn from_integer(value: i64) -> Self {
        value as f64
    }

    fn from_float(value: f64) -> Option<Self> {
        Some(value)
    }

    fn ceil(self) -> Self {
        f64::ceil(self)
    }

    fn floor(self) -> Self {
        f64::floor(self)
    }

    fn round_to(self, rounding: Rounding) -> Self {
        // Best-effort f64 rounding at the currency scale. f64 cannot represent
        // most decimal fractions exactly, so this is intentionally not the
        // exact Decimal path; the f64 mode is documented as throughput-oriented,
        // not for exact legal determinations.
        let scale = 10_f64.powi(i32::from(rounding.minor_units));
        let scaled = self * scale;
        let rounded = match rounding.mode {
            // round_ties_even is half-to-even; the others match the Decimal
            // strategies (away-from-zero on .5, toward ±infinity).
            RoundingMode::HalfUp => scaled.round(),
            RoundingMode::HalfEven => scaled.round_ties_even(),
            RoundingMode::Floor => scaled.floor(),
            RoundingMode::Ceil => scaled.ceil(),
        };
        rounded / scale
    }

    fn rank_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.is_nan(), other.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => self.total_cmp(other),
        }
    }

    fn tie_prefers(&self, other: &Self) -> bool {
        !self.is_sign_negative() && other.is_sign_negative()
    }

    fn try_to_i64_trunc(self) -> Option<i64> {
        // Reject non-finite and out-of-range values instead of saturating: the
        // strict n contract errors on a garbage n rather than silently reading
        // it as 0 (NaN) or a clamped extreme. Comparing against
        // `i64::MAX as f64` let exactly 2^63 through, because that cast rounds
        // up to 2^63; `f64_as_index` checks the half-open range instead.
        f64_as_index(self.trunc())
    }

    fn into_column(values: Vec<Self>) -> DenseColumn {
        DenseColumn::Float(values)
    }
}

/// Round a dense column's numeric values under `rounding`, in numeric mode `N`.
/// Currency formulas evaluate to numeric columns (`Integer`/`Decimal`/`Float`),
/// which are read as `N`, rounded elementwise, and rebuilt in `N`'s column
/// variant; non-numeric columns are impossible for a currency rule and pass
/// through unchanged.
fn round_dense_column<N: DenseNum>(
    column: DenseColumn,
    rounding: Rounding,
    live: &RowMask,
    errors: &mut RowErrors,
) -> Result<DenseColumn, EvalError> {
    Ok(match column {
        DenseColumn::Bool(_) | DenseColumn::Text(_) | DenseColumn::Date(_) => column,
        numeric => {
            let mut values = numeric_values::<N>(&numeric, live, errors)?;
            for row in live.without(errors)?.rows() {
                values[row] = values[row].round_to(rounding);
            }
            N::into_column(values)
        }
    })
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct DenseRelationKey {
    pub name: String,
    pub current_slot: usize,
    pub related_slot: usize,
}

#[derive(Clone, Debug)]
pub struct DenseRelationSchema {
    pub key: DenseRelationKey,
    pub related_inputs: Vec<String>,
    current_entity: Option<String>,
    related_entity: Option<String>,
    parent_relation: Option<usize>,
    filter: Option<CompiledRelatedJudgmentExpr>,
}

/// One relation's rows for a batch: root row `r`'s related rows are
/// `offsets[r]..offsets[r + 1]` of every related column.
#[derive(Clone, Debug, Default)]
pub struct DenseRelationBatchSpec {
    /// `row_count + 1` non-decreasing boundaries starting at 0. The last is the
    /// relation's related row count.
    pub offsets: Vec<usize>,
    /// Related columns by input name, one value per related row, whether or
    /// not the program reads them.
    pub inputs: HashMap<String, DenseColumn>,
    /// The related row count, stated by the caller. When given it must equal
    /// the last offset. It is required when the offsets claim related rows but
    /// no related column is supplied (a `len` over the relation, or a filter
    /// that reads only the current entity), because nothing else confirms the
    /// offsets. It is the caller's assertion, not a check: count the related
    /// rows the offsets index, one per root and member pair (a related entity
    /// two roots share counts twice), independently of the offsets, rather
    /// than copying the last offset.
    pub related_row_count: Option<usize>,
    /// Related columns supplied by length alone, without their values. Each
    /// is checked like a column in `inputs` (it must have the related row
    /// count, and one that does confirms the count), but no rule can read
    /// one. A binding layer that does not convert the columns no rule reads,
    /// as the PyO3 extension does not, passes their lengths here.
    /// A length that equals the last offset confirms the count as a stated
    /// `related_row_count` would, so it is trusted the same way.
    pub input_lengths: HashMap<String, usize>,
}

/// One period's dense input: `row_count` root rows, root columns of that
/// length, and a batch for every relation the program reads. Batches under
/// other relation keys are ignored.
#[derive(Clone, Debug)]
pub struct DenseBatchSpec {
    pub row_count: usize,
    pub inputs: HashMap<String, DenseColumn>,
    pub relations: HashMap<DenseRelationKey, DenseRelationBatchSpec>,
}

/// The most rows a dense batch or relation may claim. A dense buffer holds one
/// element per row and a `Vec` holds at most `isize::MAX` bytes, so a larger
/// count would panic with "capacity overflow" when a buffer of the widest
/// element is allocated; binding refuses it instead.
const MAX_DENSE_ROWS: usize = isize::MAX as usize / WIDEST_DENSE_ELEMENT;

/// The size of the widest element a dense buffer holds per row. List any new
/// per-row element type here.
const WIDEST_DENSE_ELEMENT: usize = {
    let sizes = [
        size_of::<bool>(),
        size_of::<i64>(),
        size_of::<f64>(),
        size_of::<usize>(),
        size_of::<Decimal>(),
        size_of::<String>(),
        size_of::<Arc<str>>(),
        size_of::<NaiveDate>(),
        size_of::<JudgmentOutcome>(),
        size_of::<ScalarValue>(),
        // `min`/`max` over a relation keep each root's best value so far.
        size_of::<Option<Decimal>>(),
        size_of::<Option<f64>>(),
        size_of::<Option<NaiveDate>>(),
    ];
    let mut widest = 1;
    let mut index = 0;
    while index < sizes.len() {
        if sizes[index] > widest {
            widest = sizes[index];
        }
        index += 1;
    }
    widest
};

/// One relation schema's view of its batch. Every link of a derived-relation
/// chain is keyed to the chain's base relation, so the schemas that share a
/// key share its offsets, owners and columns rather than each holding a copy.
#[derive(Clone, Debug)]
struct DenseRelationBatch {
    /// Shared, like `owners`, without copying the caller's `Vec`.
    offsets: Rc<Vec<usize>>,
    related_count: usize,
    /// The root row that owns each related row. Sharing the Vec preserves its
    /// allocation without copying the rows into an Rc slice.
    owners: Rc<Vec<usize>>,
    /// This schema's `related_inputs`, index-aligned. `None` is a column the
    /// caller did not supply: a missing input on every related row, an error
    /// only for the rows that read it.
    inputs: Vec<Option<Rc<DenseColumn>>>,
}

/// A relation key's batch, bound once for every schema keyed to it.
struct BoundRelationKey {
    offsets: Rc<Vec<usize>>,
    related_count: usize,
    owners: Rc<Vec<usize>>,
    /// Supplied columns stay here until a schema first reads them.
    unbound_inputs: HashMap<String, DenseColumn>,
    /// Validated columns, shared by every schema that reads them.
    inputs: HashMap<String, Rc<DenseColumn>>,
}

#[derive(Clone, Debug)]
struct DenseBoundBatch {
    row_count: usize,
    /// `None` is a root input the caller did not supply: `input_or_else`
    /// reads its default, and a plain read is a missing input on every row,
    /// an error only for the rows that reach it.
    inputs: Vec<Option<DenseColumn>>,
    relations: Vec<DenseRelationBatch>,
}

#[derive(Clone, Debug)]
pub enum DenseOutputValue {
    Scalar(DenseColumn),
    Judgment(Vec<JudgmentOutcome>),
}

#[derive(Clone, Debug)]
pub struct DenseExecutionResult {
    pub row_count: usize,
    pub outputs: HashMap<String, DenseOutputValue>,
}

#[derive(Debug, Error)]
pub enum DenseCompileError {
    #[error(transparent)]
    Eval(#[from] EvalError),
    #[error("relation entity typing failed:\n{0}")]
    RelationTyping(crate::relation_typing::RelationTypingReport),
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
    #[error(
        "dense compilation requires an explicit entity because the RuleSpec module defines multiple derived entities"
    )]
    AmbiguousRootEntity,
    #[error("dense compilation could not find derived outputs for entity `{0}`")]
    UnknownEntity(String),
    #[error("dense compilation does not yet support {0}")]
    Unsupported(String),
    #[error(
        "over-periods reductions cannot be nested: `{outer}` contains `{inner}` in its argument — reduce over the period axis once"
    )]
    NestedOverPeriods {
        outer: &'static str,
        inner: &'static str,
    },
    #[error(
        "dense compilation only supports dependencies within the same root entity; `{dependency}` from `{derived}` crosses into `{entity}`"
    )]
    CrossEntityDependency {
        derived: String,
        dependency: String,
        entity: String,
    },
}

/// A literal as dense holds it: a text is shared, built once at compile time,
/// so broadcasting it to every row clones a pointer, not the text.
#[derive(Clone, Debug)]
enum DenseLiteral {
    Bool(bool),
    Integer(i64),
    Decimal(Decimal),
    Text(Arc<str>),
    Date(NaiveDate),
}

impl From<&ScalarValue> for DenseLiteral {
    fn from(value: &ScalarValue) -> Self {
        match value {
            ScalarValue::Bool(value) => Self::Bool(*value),
            ScalarValue::Integer(value) => Self::Integer(*value),
            ScalarValue::Decimal(value) => Self::Decimal(*value),
            ScalarValue::Text(value) => Self::Text(Arc::from(value.as_str())),
            ScalarValue::Date(value) => Self::Date(*value),
        }
    }
}

#[derive(Clone, Debug)]
enum CompiledScalarExpr {
    Literal(DenseLiteral),
    Input(usize),
    InputOrElse {
        input: usize,
        default: DenseLiteral,
    },
    Derived(usize),
    /// A rule body inlined on root rows: `inline_scalars[index]`.
    Inline(usize),
    /// Commencement of a rule inlined on the current-record axis.
    Commenced {
        rule: RuleCommencement,
        value: Box<CompiledScalarExpr>,
    },
    /// An inlined rule's output rounding, applied before its dependent reads it.
    Round {
        value: Box<CompiledScalarExpr>,
        rounding: Rounding,
    },
    ParameterLookup {
        parameter: usize,
        index: Box<CompiledScalarExpr>,
    },
    Add(Vec<CompiledScalarExpr>),
    Sub(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Mul(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Div(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Max(Vec<CompiledScalarExpr>),
    Min(Vec<CompiledScalarExpr>),
    Ceil(Box<CompiledScalarExpr>),
    Floor(Box<CompiledScalarExpr>),
    PeriodStart,
    PeriodEnd,
    DateAddDays {
        date: Box<CompiledScalarExpr>,
        days: Box<CompiledScalarExpr>,
    },
    DateAddMonths {
        date: Box<CompiledScalarExpr>,
        months: Box<CompiledScalarExpr>,
    },
    DateAddYears {
        date: Box<CompiledScalarExpr>,
        years: Box<CompiledScalarExpr>,
    },
    DaysBetween {
        from: Box<CompiledScalarExpr>,
        to: Box<CompiledScalarExpr>,
    },
    /// `only_related`; `relation_name` as for `ExtremeRelated`.
    OnlyRelated {
        relation: usize,
        relation_name: String,
        input: usize,
    },
    CountRelated {
        relation: usize,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    SumRelated {
        relation: usize,
        value: Box<CompiledRelatedScalarExpr>,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    /// `min`/`max` over related entities. `relation_name` is the relation as
    /// the formula names it, for explain's errors: a derived relation's schema
    /// is keyed to its chain's base relation.
    ExtremeRelated {
        extreme: Extreme,
        relation: usize,
        relation_name: String,
        value: Box<CompiledRelatedScalarExpr>,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    /// `only` over related entities; `relation_name` as for `ExtremeRelated`.
    SingleRelated {
        relation: usize,
        relation_name: String,
        value: Box<CompiledRelatedScalarExpr>,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    If {
        condition: Box<CompiledJudgmentExpr>,
        then_expr: Box<CompiledScalarExpr>,
        else_expr: Box<CompiledScalarExpr>,
    },
    /// The fallback of a `match` without `_`: every row that reaches it fails
    /// with explain's error naming the subject's value. `placeholder` is the
    /// last arm's value, evaluated for no rows, so the column has the arms'
    /// dtype whichever rows are live.
    NoMatch {
        subject: Box<CompiledScalarExpr>,
        placeholder: Box<CompiledScalarExpr>,
        labels: MatchLabels,
    },
    /// Cross-period reduction, evaluated only by the lifetime executor. `value`
    /// is compiled as an ordinary per-period scalar (evaluated once per supplied
    /// period); `n` (SumTopN only) is compiled likewise, evaluated in every
    /// period and required to be period-invariant before its reference-period
    /// value is used. The per-period executor rejects this node.
    OverPeriods {
        kind: OverPeriodsKind,
        value: Box<CompiledScalarExpr>,
        n: Option<Box<CompiledScalarExpr>>,
    },
}

#[derive(Clone, Debug)]
enum CompiledRelatedScalarExpr {
    Literal(DenseLiteral),
    Input(usize),
    InputOrElse {
        input: usize,
        default: DenseLiteral,
    },
    RootScalar(Box<CompiledScalarExpr>),
    /// A related rule's body inlined on this relation's related rows:
    /// `related_inline_scalars[index]`.
    Inline(usize),
    /// An inlined dependency has its own commencement, independently of the
    /// aggregate rule that reads it.
    Commenced {
        rule: RuleCommencement,
        value: Box<CompiledRelatedScalarExpr>,
    },
    /// An inlined related rule's output rounding, under the related-row mask.
    Round {
        value: Box<CompiledRelatedScalarExpr>,
        rounding: Rounding,
    },
    ParameterLookup {
        parameter: usize,
        index: Box<CompiledRelatedScalarExpr>,
    },
    Add(Vec<CompiledRelatedScalarExpr>),
    Sub(
        Box<CompiledRelatedScalarExpr>,
        Box<CompiledRelatedScalarExpr>,
    ),
    Mul(
        Box<CompiledRelatedScalarExpr>,
        Box<CompiledRelatedScalarExpr>,
    ),
    Div(
        Box<CompiledRelatedScalarExpr>,
        Box<CompiledRelatedScalarExpr>,
    ),
    Max(Vec<CompiledRelatedScalarExpr>),
    Min(Vec<CompiledRelatedScalarExpr>),
    Ceil(Box<CompiledRelatedScalarExpr>),
    Floor(Box<CompiledRelatedScalarExpr>),
    PeriodStart,
    PeriodEnd,
    DateAddDays {
        date: Box<CompiledRelatedScalarExpr>,
        days: Box<CompiledRelatedScalarExpr>,
    },
    DateAddMonths {
        date: Box<CompiledRelatedScalarExpr>,
        months: Box<CompiledRelatedScalarExpr>,
    },
    DateAddYears {
        date: Box<CompiledRelatedScalarExpr>,
        years: Box<CompiledRelatedScalarExpr>,
    },
    DaysBetween {
        from: Box<CompiledRelatedScalarExpr>,
        to: Box<CompiledRelatedScalarExpr>,
    },
    If {
        condition: Box<CompiledRelatedJudgmentExpr>,
        then_expr: Box<CompiledRelatedScalarExpr>,
        else_expr: Box<CompiledRelatedScalarExpr>,
    },
    /// Related-row form of [`CompiledScalarExpr::NoMatch`].
    NoMatch {
        subject: Box<CompiledRelatedScalarExpr>,
        placeholder: Box<CompiledRelatedScalarExpr>,
        labels: MatchLabels,
    },
}

/// How a failed `match` names its rule, subject and arms, rendered at compile
/// time from the model expressions the dense plan does not keep.
#[derive(Clone, Debug)]
struct MatchLabels {
    /// The rule whose formula contains the `match`, as explain names it: by
    /// its id when it has one. Dense inlines a related entity's rules into the
    /// aggregation that reads them, so the name is fixed here. It is empty for
    /// a `match` directly in a derived relation's membership predicate, which
    /// the rule that evaluates the relation names.
    ///
    /// Each label is shared: a subject or pattern can quote a long text
    /// literal, and a failing row's error refers to the labels rather than
    /// copying them.
    rule: Arc<str>,
    subject: Arc<str>,
    patterns: Arc<str>,
}

impl MatchLabels {
    /// Record explain's error on each row of `rows`, whose subject value is
    /// at that row of `subject`. The error quotes the value without copying
    /// it, and consecutive rows failing on the same value share one error.
    fn record_failures(&self, subject: &DenseColumn, rows: &RowMask, errors: &mut RowErrors) {
        let mut last: Option<Rc<Quoted>> = None;
        for row in rows.rows() {
            let quoted = Quoted {
                rows: subject.len(),
                error: QuotedError::NoMatchingArm {
                    rule: Arc::clone(&self.rule),
                    subject: Arc::clone(&self.subject),
                    value: quoted_value(subject, row),
                    patterns: Arc::clone(&self.patterns),
                },
            };
            errors.record_error(row, share_quoted(&mut last, quoted));
        }
    }
}

const NO_MATCH_OUTSIDE_IF: &str =
    "a `match` fallback (`no_match`) that is not the `else` branch of an `if`";

/// The refusal of a qualified read (`anchor.name`, `candidate.name`). Dense
/// compiles no relation roles yet (#218): a program whose reachable
/// predicates use one runs in explain or fast mode.
fn relation_role_unsupported(term: Term, name: &str) -> DenseCompileError {
    DenseCompileError::Unsupported(format!(
        "the qualified read `{}.{name}` in a derived relation's predicate; relation roles run in explain and fast modes, and dense support is #218",
        term.as_str()
    ))
}

fn relation_atom_unsupported(relation: &str) -> DenseCompileError {
    DenseCompileError::Unsupported(format!(
        "the relation atom `{relation}` in a derived relation's predicate; relation atoms run in explain and fast modes, and dense support is #218"
    ))
}

fn exists_unsupported() -> DenseCompileError {
    DenseCompileError::Unsupported(
        "bounded exists is not supported in dense execution (#218)".to_string(),
    )
}

fn path_source_unsupported(relation: &str) -> DenseCompileError {
    DenseCompileError::Unsupported(format!(
        "the path source of derived relation `{relation}`; path sources run in explain and fast modes, and dense support is #218"
    ))
}

/// The relation at or below `relation`, down its single sources, that takes
/// a path source.
fn path_source_below<'a>(program: &'a Program, relation: &'a str) -> Option<&'a str> {
    let mut on_chain = HashSet::new();
    let mut name = relation;
    while on_chain.insert(name) {
        let derivation = program.relations.get(name)?.derivation.as_ref()?;
        if derivation.source_path.is_some() {
            return Some(name);
        }
        name = derivation.source_relation.as_deref()?;
    }
    None
}

fn term_comparison_unsupported() -> DenseCompileError {
    DenseCompileError::Unsupported(
        "a term comparison in a derived relation's predicate; term comparisons run in explain and fast modes, and dense support is #218".to_string(),
    )
}

#[derive(Clone, Debug)]
enum CompiledRelatedJudgmentExpr {
    Literal(bool),
    Comparison {
        left: CompiledRelatedScalarExpr,
        op: ComparisonOp,
        right: CompiledRelatedScalarExpr,
    },
    RootJudgment(Box<CompiledJudgmentExpr>),
    /// A related rule's body inlined on this relation's related rows:
    /// `related_inline_judgments[index]`.
    Inline(usize),
    Commenced {
        rule: RuleCommencement,
        value: Box<CompiledRelatedJudgmentExpr>,
    },
    /// A `relation_member` outside a derived relation's own predicate: in a
    /// `count`/`sum` `where` clause, or in a related rule a predicate reads.
    /// Explain evaluates both without a relation context, so every related
    /// row that reaches it fails with explain's error.
    RelationMemberWithoutContext {
        relation: String,
    },
    And(Vec<CompiledRelatedJudgmentExpr>),
    Or(Vec<CompiledRelatedJudgmentExpr>),
    Not(Box<CompiledRelatedJudgmentExpr>),
}

#[derive(Clone, Debug)]
enum CompiledJudgmentExpr {
    Comparison {
        left: CompiledScalarExpr,
        op: ComparisonOp,
        right: CompiledScalarExpr,
    },
    Derived(usize),
    /// A rule body inlined on root rows: `inline_judgments[index]`.
    Inline(usize),
    Commenced {
        rule: RuleCommencement,
        value: Box<CompiledJudgmentExpr>,
    },
    And(Vec<CompiledJudgmentExpr>),
    Or(Vec<CompiledJudgmentExpr>),
    Not(Box<CompiledJudgmentExpr>),
}

#[derive(Clone, Debug)]
struct RuleCommencement {
    derived: String,
    effective_from: NaiveDate,
}

impl RuleCommencement {
    fn rows(&self, period: &Period, mask: &RowMask) -> (RowMask, RowErrors) {
        if period.start < self.effective_from {
            let mut errors = RowErrors::new();
            errors.record_all(
                mask,
                &EvalError::MissingDerivedFormulaVersion {
                    derived: self.derived.clone(),
                    at: period.start,
                },
            );
            (RowMask::none(mask.len()), errors)
        } else {
            (mask.clone(), RowErrors::new())
        }
    }
}

/// Inlined rules obey the same supported version shape as root rules: one
/// unbounded version, evaluated only after its commencement. A related rule
/// can change independently of the aggregate that reads it.
fn inlined_rule_semantics(
    derived: &Derived,
) -> Result<(&DerivedSemantics, Option<RuleCommencement>), DenseCompileError> {
    match derived.versions.as_slice() {
        [] => Ok((&derived.semantics, None)),
        [only] if only.effective_to.is_none() => Ok((
            &only.semantics,
            Some(RuleCommencement {
                derived: derived.name.clone(),
                effective_from: only.effective_from,
            }),
        )),
        _ => Err(DenseCompileError::Unsupported(format!(
            "versioned inlined derived formulas in `{}`; use generic API execution",
            derived.name
        ))),
    }
}

fn named_aggregate_error(
    function: &str,
    relation: &str,
    error: DenseCompileError,
) -> DenseCompileError {
    match error {
        DenseCompileError::Unsupported(message) => DenseCompileError::Unsupported(format!(
            "{message} (while compiling {function} over relation `{relation}`)"
        )),
        other => other,
    }
}

#[derive(Clone, Debug)]
enum CompiledSemantics {
    Scalar(CompiledScalarExpr),
    Judgment(CompiledJudgmentExpr),
}

#[derive(Clone, Debug)]
struct CompiledDerived {
    name: String,
    /// How explain names the rule when an error leaves it: its id when it has
    /// one, else its name.
    label: String,
    semantics: CompiledSemantics,
    /// Opt-in output rounding, copied from the model `Derived` at compile time.
    /// Applied to the whole evaluated column before it is cached, so dependents
    /// and direct outputs observe the same rounded values as the other paths.
    rounding: Option<Rounding>,
    /// Commencement date of the single unbounded version this was compiled from,
    /// when the source rule was versioned. Dense compiles once and executes for
    /// many periods, so the date cannot be resolved at compile time; it is
    /// enforced per execution instead. `None` for unversioned rules.
    effective_from: Option<NaiveDate>,
}

#[derive(Clone, Debug)]
struct CompiledParameter {
    parameter: IndexedParameter,
    /// Each version's text values by key, in `parameter.versions` order,
    /// shared at compile time so a lookup clones a pointer, not the text.
    texts: Vec<BTreeMap<i64, Arc<str>>>,
}

impl CompiledParameter {
    fn new(parameter: &IndexedParameter) -> Self {
        let texts = parameter
            .versions
            .iter()
            .map(|version| {
                version
                    .values
                    .iter()
                    .filter_map(|(key, value)| match value {
                        ScalarValue::Text(text) => Some((*key, Arc::from(text.as_str()))),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        Self {
            parameter: parameter.clone(),
            texts,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DenseCompiledProgram {
    root_entity: String,
    root_inputs: Vec<String>,
    relations: Vec<DenseRelationSchema>,
    parameters: Vec<CompiledParameter>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
    inline_scalars: Vec<InlineBody<CompiledScalarExpr>>,
    inline_judgments: Vec<InlineBody<CompiledJudgmentExpr>>,
    related_inline_scalars: Vec<InlineBody<CompiledRelatedScalarExpr>>,
    related_inline_judgments: Vec<InlineBody<CompiledRelatedJudgmentExpr>>,
}

/// A rule body dense inlines, compiled once and read through `Inline(index)`
/// nodes. Dense inlines a rule wherever an aggregation's value, a `where`
/// clause or a derived relation's predicate reads it, and inlines the rules
/// that rule reads the same way. Inlining each copy afresh made a rule graph
/// with shared dependencies (`p_i = p_{i-1} + p_{i-1}`) compile to a tree with
/// one node per path, 2^k for k levels. Each body is now compiled once per
/// place it is evaluated: a current-entity rule's body on root rows, and a
/// related rule's body on one relation's related rows.
///
/// A body is the formula [`inlined_rule_semantics`] selects for the rule, as
/// the inlined copy was. Its commencement check and its rounding wrap each
/// read, outside the body, and a `match` in it names the rule that owns it.
#[derive(Clone, Debug)]
struct InlineBody<E> {
    body: E,
    /// How many compiled nodes read this body. The executors cache the column
    /// of a body read by more than one node, so a row is computed once however
    /// many paths reach it; a body read by one node is evaluated in place, as
    /// the inlined copy was.
    references: usize,
    /// The inlining levels compiling the body took (see [`MAX_INLINE_DEPTH`]),
    /// its own and those of every body it reads. A read from deeper in an
    /// aggregation would have compiled it that much deeper, so a shared read
    /// is checked against the bound with this height.
    height: usize,
}

/// Where dense inlines a rule's body: on root rows, or on one relation's
/// related rows. A rule's body compiles to the same expression at every read
/// from one site, so the site keys the shared body.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum InlineSite {
    Current(String),
    Related(usize, String),
}

/// The side table an inlined body is in.
#[derive(Clone, Copy, Debug)]
enum InlineTable {
    Scalars,
    Judgments,
    RelatedScalars,
    RelatedJudgments,
}

impl DenseCompiledProgram {
    pub fn from_artifact(
        artifact: &CompiledProgramArtifact,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
        Self::from_program(&artifact.program.to_program()?, entity)
    }

    /// Compile `program` as given. Like `Engine::new`, this does not check the
    /// dependency graph: an artifact is checked when it is compiled or
    /// loaded, and `execute_request` checks an inline program, both refusing a
    /// cycle routed through a derived relation even where no rule reaches it.
    /// To check a program first, compile it into an artifact
    /// (`CompiledProgramArtifact::compile`, or load one with `from_json_*`)
    /// and pass that to [`Self::from_artifact`], which does not check again.
    ///
    /// Relation entity typing is checked here, as at the request and artifact
    /// entry points, because an artifact deserialized directly (rather than
    /// through `from_json_*`) or a program built in Rust has not been checked.
    /// (`Engine::new` alone evaluates a `Program` as given.)
    pub fn from_program(
        program: &Program,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
        crate::relation_typing::check_program(program)
            .map_err(DenseCompileError::RelationTyping)?;
        let root_entity = match entity {
            Some(entity) => entity.to_string(),
            None => {
                let entities = program
                    .derived
                    .values()
                    .map(|derived| derived.entity.clone())
                    .filter(|entity| entity != SCALAR_ENTITY)
                    .collect::<HashSet<String>>();
                if entities.len() > 1 {
                    return Err(DenseCompileError::AmbiguousRootEntity);
                }
                // A module whose derived rules are all scalar formula
                // parameters compiles with the scalar pseudo-entity as its
                // root and executes as a broadcast.
                entities
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| SCALAR_ENTITY.to_string())
            }
        };

        let available = program
            .derived
            .values()
            .filter(|derived| derived.entity == root_entity)
            .map(|derived| derived.name.clone())
            .collect::<Vec<String>>();
        if available.is_empty() {
            return Err(DenseCompileError::UnknownEntity(root_entity));
        }

        let mut compiler = DenseCompiler::new(program, root_entity.clone())?;
        for name in available {
            compiler.compile_derived(&name)?;
        }
        Ok(compiler.finish())
    }

    pub fn root_entity(&self) -> &str {
        &self.root_entity
    }

    pub fn root_inputs(&self) -> &[String] {
        &self.root_inputs
    }

    pub fn relations(&self) -> &[DenseRelationSchema] {
        &self.relations
    }

    pub fn output_names(&self) -> Vec<String> {
        self.derived
            .iter()
            .map(|derived| derived.name.clone())
            .collect()
    }

    /// Execute in canonical `Decimal` arithmetic.
    pub fn execute(
        &self,
        period: &Period,
        batch: DenseBatchSpec,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        self.execute_with::<Decimal>(period, batch, outputs)
    }

    /// Execute in `f64` arithmetic. Numeric outputs come back as
    /// `DenseColumn::Float`. Substantially faster than `execute` for large
    /// batches, at the cost of floating-point rounding; intended for
    /// microsimulation-style workloads rather than exact legal determinations.
    pub fn execute_f64(
        &self,
        period: &Period,
        batch: DenseBatchSpec,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        self.execute_with::<f64>(period, batch, outputs)
    }

    /// Evaluate `outputs` for every row, lazily per row: each node only for the
    /// rows whose reference evaluation reaches it (see
    /// `docs/execution-semantics.md`). A rule before its commencement date
    /// fails the rows that reach it (#84), as the generic executor reports
    /// `MissingDerivedFormulaVersion` per rule. The call fails with the first
    /// failing row's error, and for that row the first failing output's.
    fn execute_with<N: DenseNum>(
        &self,
        period: &Period,
        batch: DenseBatchSpec,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        let requested = self.resolve_outputs(outputs)?;
        let batch = self.bind_batch(batch)?;
        let row_count = batch.row_count;
        let mut executor: DenseExecutor<'_, N> = DenseExecutor::new(self, period, batch);
        let all = RowMask::all(row_count);
        let mut evaluated = Vec::with_capacity(requested.len());
        for (output, derived_index) in requested {
            let (value, errors) = match &self.derived[derived_index].semantics {
                CompiledSemantics::Scalar(_) => {
                    let (column, errors) = drive(
                        &mut executor,
                        |executor| executor.evaluate_scalar(derived_index, &all),
                        |index| self.derived[index].name.clone(),
                    )?;
                    (DenseOutputValue::Scalar(column), errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let (values, errors) = drive(
                        &mut executor,
                        |executor| executor.evaluate_judgment(derived_index, &all),
                        |index| self.derived[index].name.clone(),
                    )?;
                    (DenseOutputValue::Judgment(values), errors)
                }
            };
            evaluated.push((output, value, errors));
        }
        first_row_error(evaluated.iter().map(|(_, _, errors)| errors))?;
        Ok(DenseExecutionResult {
            row_count,
            outputs: evaluated
                .into_iter()
                .map(|(output, value, _)| (output, value))
                .collect(),
        })
    }

    fn resolve_outputs(&self, outputs: &[String]) -> Result<Vec<(String, usize)>, EvalError> {
        outputs
            .iter()
            .map(|output| {
                self.derived_index
                    .get(output)
                    .map(|&index| (output.clone(), index))
                    .ok_or_else(|| EvalError::UnknownDerived(output.clone()))
            })
            .collect()
    }

    /// Execute over an entity's lifetime — one positionally aligned input batch
    /// per period — in canonical `Decimal` arithmetic. See
    /// [`Self::execute_lifetime_f64`] for the full contract.
    pub fn execute_lifetime(
        &self,
        periods: &[Period],
        batches: Vec<DenseBatchSpec>,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        self.execute_lifetime_with::<Decimal>(periods, batches, outputs)
    }

    /// Execute over an entity's lifetime in `f64` arithmetic (throughput mode).
    ///
    /// `periods` and `batches` must have equal length (one batch per period),
    /// the periods must be strictly ascending by start date, and every batch
    /// must describe the SAME entity rows in the SAME order and count (v1
    /// positional alignment — row `i` is the same entity in every period). Each
    /// requested output's formula must contain at least one over-periods
    /// reduction (`sum_over_periods`, `max_over_periods`, `count_over_periods`,
    /// `sum_top_n_over_periods`); outputs with no reduction are period-specific
    /// and should use the per-period [`Self::execute_f64`] entry point instead.
    ///
    /// Each reduction's inner expression is evaluated once per period with the
    /// ordinary per-period executor; the per-period vectors are stacked and
    /// reduced row-wise. Non-reduction scalars combined with a reduction
    /// resolve at the reference period — the chronologically-last supplied
    /// period (parameters like a bend point index to the determination year);
    /// a derived referenced outside a reduction means its body inlined in this
    /// same lifetime context, and a bare period-invariant input binds its
    /// common value.
    ///
    /// Reached rules must have commenced at their evaluation period's start:
    /// each individual period inside reductions (including top-N counts), and
    /// the reference period for lifetime-level rules and requested outputs.
    /// Earlier periods fail with `EvalError::MissingDerivedFormulaVersion`,
    /// as on the per-period entry points; untaken branches remain unaffected.
    pub fn execute_lifetime_f64(
        &self,
        periods: &[Period],
        batches: Vec<DenseBatchSpec>,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        self.execute_lifetime_with::<f64>(periods, batches, outputs)
    }

    fn execute_lifetime_with<N: DenseNum>(
        &self,
        periods: &[Period],
        batches: Vec<DenseBatchSpec>,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        if periods.is_empty() {
            return Err(EvalError::LifetimeNoPeriods);
        }
        if periods.len() != batches.len() {
            return Err(EvalError::LifetimePeriodBatchMismatch {
                periods: periods.len(),
                batches: batches.len(),
            });
        }

        // Periods must be strictly ascending by start date. The reference period
        // for period-specific scalars (parameters, and the `n` of
        // `sum_top_n_over_periods`) is the chronologically-last supplied period;
        // an unsorted or descending list would otherwise resolve a bend point or
        // COLA at the wrong year with no signal. Reject rather than silently sort
        // so the caller's period/batch pairing stays authoritative.
        for (earlier_index, window) in periods.windows(2).enumerate() {
            let (earlier, later) = (&window[0], &window[1]);
            if earlier.start >= later.start {
                return Err(EvalError::LifetimePeriodsNotAscending {
                    earlier_index,
                    earlier: format!("{}..{}", earlier.start, earlier.end),
                    later_index: earlier_index + 1,
                    later: format!("{}..{}", later.start, later.end),
                });
            }
        }

        // Bind every period's batch and require identical row counts (positional
        // alignment). Row `i` is the same entity across every period.
        let mut bound = Vec::with_capacity(batches.len());
        let mut expected_row_count = None;
        for (index, batch) in batches.into_iter().enumerate() {
            let bound_batch = self.bind_batch(batch)?;
            match expected_row_count {
                None => expected_row_count = Some(bound_batch.row_count),
                Some(expected) if bound_batch.row_count != expected => {
                    return Err(EvalError::LifetimeRowCountMismatch {
                        period: index,
                        row_count: bound_batch.row_count,
                        expected,
                    });
                }
                Some(_) => {}
            }
            bound.push(bound_batch);
        }
        let row_count = expected_row_count.unwrap_or(0);

        // Every requested output must reduce over the period axis; a purely
        // per-period output has no single-column lifetime meaning.
        let reduces = self.rules_reducing_over_periods();
        for output in outputs {
            let Some(&derived_index) = self.derived_index.get(output) else {
                return Err(EvalError::UnknownDerived(output.clone()));
            };
            if !reduces[derived_index] {
                return Err(EvalError::LifetimeOutputWithoutReduction(output.clone()));
            }
        }

        let mut executor: LifetimeExecutor<'_, N> =
            LifetimeExecutor::new(self, periods, bound, row_count);
        let all = RowMask::all(row_count);
        let mut evaluated = Vec::with_capacity(outputs.len());
        for output in outputs {
            let derived_index = self.derived_index[output];
            let (value, errors) = match &self.derived[derived_index].semantics {
                CompiledSemantics::Scalar(_) => {
                    let (column, errors) = drive(
                        &mut executor,
                        |executor| executor.evaluate_scalar(derived_index, &all),
                        |index| self.derived[index].name.clone(),
                    )?;
                    (DenseOutputValue::Scalar(column), errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let (values, errors) = drive(
                        &mut executor,
                        |executor| executor.evaluate_judgment(derived_index, &all),
                        |index| self.derived[index].name.clone(),
                    )?;
                    (DenseOutputValue::Judgment(values), errors)
                }
            };
            evaluated.push((output.clone(), value, errors));
        }
        first_row_error(evaluated.iter().map(|(_, _, errors)| errors))?;
        Ok(DenseExecutionResult {
            row_count,
            outputs: evaluated
                .into_iter()
                .map(|(output, value, _)| (output, value))
                .collect(),
        })
    }

    /// For each compiled rule, whether its formula contains an over-periods
    /// reduction, directly or through the rules it reads. Used to gate
    /// lifetime execution to reduction outputs only. A rule is compiled after
    /// every rule it reads, so one pass in index order sees each dependency's
    /// answer before it is needed, however long the chain.
    fn rules_reducing_over_periods(&self) -> Vec<bool> {
        let mut reduces = Vec::with_capacity(self.derived.len());
        for derived in &self.derived {
            let reduction = match &derived.semantics {
                CompiledSemantics::Scalar(expr) => self.scalar_reduces_over_periods(expr, &reduces),
                CompiledSemantics::Judgment(expr) => {
                    self.judgment_reduces_over_periods(expr, &reduces)
                }
            };
            reduces.push(reduction);
        }
        reduces
    }

    fn scalar_reduces_over_periods(&self, expr: &CompiledScalarExpr, reduces: &[bool]) -> bool {
        match expr {
            CompiledScalarExpr::OverPeriods { .. } => true,
            CompiledScalarExpr::Literal(_)
            | CompiledScalarExpr::Input(_)
            | CompiledScalarExpr::InputOrElse { .. }
            | CompiledScalarExpr::PeriodStart
            | CompiledScalarExpr::PeriodEnd => false,
            CompiledScalarExpr::Derived(index) => {
                // Earlier in index order, so already decided: a rule is
                // compiled after every rule it reads.
                debug_assert!(*index < reduces.len(), "a rule reads a later rule");
                reduces.get(*index).copied().unwrap_or(false)
            }
            // A body inlined on root rows compiles as a current-entity
            // expression, which refuses an over-periods reduction, and only an
            // aggregation or a relation's predicate reads one.
            CompiledScalarExpr::Inline(_) => false,
            CompiledScalarExpr::ParameterLookup { index, .. } => {
                self.scalar_reduces_over_periods(index, reduces)
            }
            CompiledScalarExpr::Add(items)
            | CompiledScalarExpr::Max(items)
            | CompiledScalarExpr::Min(items) => items
                .iter()
                .any(|item| self.scalar_reduces_over_periods(item, reduces)),
            CompiledScalarExpr::Sub(left, right)
            | CompiledScalarExpr::Mul(left, right)
            | CompiledScalarExpr::Div(left, right) => {
                self.scalar_reduces_over_periods(left, reduces)
                    || self.scalar_reduces_over_periods(right, reduces)
            }
            CompiledScalarExpr::Ceil(value)
            | CompiledScalarExpr::Floor(value)
            | CompiledScalarExpr::Commenced { value, .. }
            | CompiledScalarExpr::Round { value, .. } => {
                self.scalar_reduces_over_periods(value, reduces)
            }
            CompiledScalarExpr::DateAddDays { date, days } => {
                self.scalar_reduces_over_periods(date, reduces)
                    || self.scalar_reduces_over_periods(days, reduces)
            }
            CompiledScalarExpr::DateAddMonths { date, months } => {
                self.scalar_reduces_over_periods(date, reduces)
                    || self.scalar_reduces_over_periods(months, reduces)
            }
            CompiledScalarExpr::DateAddYears { date, years } => {
                self.scalar_reduces_over_periods(date, reduces)
                    || self.scalar_reduces_over_periods(years, reduces)
            }
            CompiledScalarExpr::DaysBetween { from, to } => {
                self.scalar_reduces_over_periods(from, reduces)
                    || self.scalar_reduces_over_periods(to, reduces)
            }
            CompiledScalarExpr::OnlyRelated { .. }
            | CompiledScalarExpr::CountRelated { .. }
            | CompiledScalarExpr::SumRelated { .. }
            | CompiledScalarExpr::ExtremeRelated { .. }
            | CompiledScalarExpr::SingleRelated { .. } => false,
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment_reduces_over_periods(condition, reduces)
                    || self.scalar_reduces_over_periods(then_expr, reduces)
                    || self.scalar_reduces_over_periods(else_expr, reduces)
            }
            CompiledScalarExpr::NoMatch {
                subject,
                placeholder,
                ..
            } => {
                self.scalar_reduces_over_periods(subject, reduces)
                    || self.scalar_reduces_over_periods(placeholder, reduces)
            }
        }
    }

    fn judgment_reduces_over_periods(&self, expr: &CompiledJudgmentExpr, reduces: &[bool]) -> bool {
        match expr {
            CompiledJudgmentExpr::Comparison { left, right, .. } => {
                self.scalar_reduces_over_periods(left, reduces)
                    || self.scalar_reduces_over_periods(right, reduces)
            }
            CompiledJudgmentExpr::Derived(index) => {
                // Earlier in index order, so already decided: a rule is
                // compiled after every rule it reads.
                debug_assert!(*index < reduces.len(), "a rule reads a later rule");
                reduces.get(*index).copied().unwrap_or(false)
            }
            // See `scalar_reduces_over_periods`.
            CompiledJudgmentExpr::Inline(_) => false,
            CompiledJudgmentExpr::And(items) | CompiledJudgmentExpr::Or(items) => items
                .iter()
                .any(|item| self.judgment_reduces_over_periods(item, reduces)),
            CompiledJudgmentExpr::Not(item)
            | CompiledJudgmentExpr::Commenced { value: item, .. } => {
                self.judgment_reduces_over_periods(item, reduces)
            }
        }
    }

    /// Check a batch against this program, then bind its columns. Every count
    /// binding or execution sizes a buffer by is justified before any buffer
    /// of that size is allocated (error messages and schema metadata still
    /// allocate):
    ///
    /// - `row_count` is at most [`MAX_DENSE_ROWS`], and every supplied root
    ///   column has that length.
    /// - Each relation's offsets hold `row_count + 1` non-decreasing entries
    ///   starting at 0; the last is its related row count, which a stated
    ///   `related_row_count` must equal and each read column must have.
    /// - A nonzero related count is confirmed by a supplied related column of
    ///   that length or by a stated count, and is at most [`MAX_DENSE_ROWS`];
    ///   every supplied related column, read or not, must have it.
    ///
    /// Relations are checked in two passes. The first walks the schemas in
    /// order and reports what a per-schema binding reports first: a key's
    /// offsets when its first schema reaches it, then each schema's read
    /// columns in its input order. The second runs the checks above that a
    /// per-schema binding never made (confirmation, the bound, and columns no
    /// schema reads), key by key in the order the program first reads them.
    /// Root columns, and the columns no schema reads within a key, are reported
    /// smallest name first, so a malformed batch reports the same error every
    /// run.
    fn bind_batch(&self, mut batch: DenseBatchSpec) -> Result<DenseBoundBatch, EvalError> {
        let row_count = batch.row_count;
        if row_count > MAX_DENSE_ROWS {
            return Err(EvalError::TypeMismatch(format!(
                "dense batch row_count {row_count} exceeds {MAX_DENSE_ROWS}, the most rows a dense batch can hold"
            )));
        }
        if let Some((name, column)) =
            first_by_name(&batch.inputs, |column| column.len() != row_count)
        {
            return Err(EvalError::TypeMismatch(format!(
                "dense root input `{name}` has length {} but row_count is {row_count}",
                column.len()
            )));
        }

        // Every link of a derived-relation chain is keyed to the chain's base
        // relation, so many schemas can share one key. A key's offsets are
        // checked when the first schema keyed to it is reached, and every
        // schema length-checks the columns it reads. A column's length and a
        // key's related count never change, so checking a shared column again
        // for a later schema reports nothing new; the first error is the one a
        // per-schema binding reports.
        let mut related_counts: HashMap<&DenseRelationKey, usize> = HashMap::new();
        let mut key_order = Vec::new();
        for relation in &self.relations {
            let relation_batch = batch.relations.get(&relation.key).ok_or_else(|| {
                EvalError::UnknownRelation(format!(
                    "{}::{}/{}/{}",
                    relation.key.name,
                    relation.key.current_slot,
                    relation.key.related_slot,
                    self.root_entity
                ))
            })?;
            let related_count = match related_counts.get(&relation.key) {
                Some(&related_count) => related_count,
                None => {
                    let related_count =
                        Self::check_offsets(&relation.key, relation_batch, row_count)?;
                    related_counts.insert(&relation.key, related_count);
                    key_order.push(&relation.key);
                    related_count
                }
            };
            for input in &relation.related_inputs {
                if let Some(column) = relation_batch.inputs.get(input)
                    && column.len() != related_count
                {
                    return Err(related_length_mismatch(
                        input,
                        &relation.key.name,
                        column.len(),
                        related_count,
                    ));
                }
            }
        }
        for &key in &key_order {
            Self::confirm_related_count(key, &batch.relations[key], related_counts[key])?;
        }

        // The batch is valid. An absent input column is a missing input on
        // every row. It fails only the rows whose evaluation reads it, as a
        // missing record does on the explain path; a column read only on
        // branches no row takes may be omitted. Root input names are distinct,
        // so each column is moved out of the batch rather than copied.
        let bound_inputs = self
            .root_inputs
            .iter()
            .map(|name| batch.inputs.remove(name))
            .collect::<Vec<Option<DenseColumn>>>();

        // A key's owners are built once and every schema keyed to it shares
        // them, its offsets and its columns; a column is wrapped for sharing
        // when a schema first reads it.
        let mut bound_keys: HashMap<&DenseRelationKey, BoundRelationKey> = HashMap::new();
        for &key in &key_order {
            let relation_batch = batch
                .relations
                .remove(key)
                .expect("every key was checked above");
            bound_keys.insert(
                key,
                Self::bind_relation_key(key, relation_batch, related_counts[key])?,
            );
        }
        let mut bound_relations = Vec::with_capacity(self.relations.len());
        for relation in &self.relations {
            let key = bound_keys
                .get_mut(&relation.key)
                .expect("every key was bound above");
            let inputs = relation
                .related_inputs
                .iter()
                .map(|name| {
                    if let Some(column) = key.inputs.get(name) {
                        return Some(Rc::clone(column));
                    }
                    let (name, column) = key.unbound_inputs.remove_entry(name)?;
                    let column = Rc::new(column);
                    key.inputs.insert(name, Rc::clone(&column));
                    Some(column)
                })
                .collect();
            bound_relations.push(DenseRelationBatch {
                offsets: Rc::clone(&key.offsets),
                related_count: key.related_count,
                owners: Rc::clone(&key.owners),
                inputs,
            });
        }

        Ok(DenseBoundBatch {
            row_count,
            inputs: bound_inputs,
            relations: bound_relations,
        })
    }

    /// Check a key's offsets and stated count, returning its related count.
    fn check_offsets(
        key: &DenseRelationKey,
        relation_batch: &DenseRelationBatchSpec,
        row_count: usize,
    ) -> Result<usize, EvalError> {
        let name = &key.name;
        let offsets = &relation_batch.offsets;
        // `bind_batch` bounds `row_count` by `MAX_DENSE_ROWS`, so this cannot
        // overflow; it is checked rather than left to wrap to zero.
        let expected = row_count.checked_add(1).ok_or_else(|| {
            EvalError::TypeMismatch(format!(
                "dense batch row_count {row_count} leaves no room for relation `{name}`'s offsets, which need row_count + 1 entries"
            ))
        })?;
        if offsets.len() != expected {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` offsets must have length {expected} (row_count + 1) but has {}",
                offsets.len()
            )));
        }
        // `expected` is at least 1, so the offsets are non-empty from here on.
        if offsets[0] != 0 {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` offsets must start at 0 but starts at {}",
                offsets[0]
            )));
        }
        if let Some(index) = offsets.windows(2).position(|pair| pair[0] > pair[1]) {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` offsets must be non-decreasing, but offsets[{index}] = {} is greater than offsets[{}] = {}",
                offsets[index],
                index + 1,
                offsets[index + 1]
            )));
        }
        let related_count = offsets[row_count];
        if let Some(stated) = relation_batch.related_row_count
            && stated != related_count
        {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` offsets end at {related_count} but related_row_count is {stated}"
            )));
        }
        Ok(related_count)
    }

    /// The offsets alone could claim any number of related rows, and binding
    /// allocates one owner per related row. So a nonzero count must be
    /// confirmed by a supplied column with one value per related row, or by
    /// the caller's `related_row_count`, and must fit [`MAX_DENSE_ROWS`].
    /// Every column the schemas read was length-checked already, so any
    /// supplied column that disagrees here is one no schema reads.
    fn confirm_related_count(
        key: &DenseRelationKey,
        relation_batch: &DenseRelationBatchSpec,
        related_count: usize,
    ) -> Result<(), EvalError> {
        let name = &key.name;
        // Every supplied column's length: the columns with values, then the
        // ones supplied by length alone.
        let lengths = || {
            relation_batch
                .inputs
                .iter()
                .map(|(input, column)| (input, column.len()))
                .chain(
                    relation_batch
                        .input_lengths
                        .iter()
                        .map(|(input, length)| (input, *length)),
                )
        };
        let disagreeing = lengths()
            .filter(|(_, length)| *length != related_count)
            .min_by_key(|(input, _)| *input);
        let confirmed = related_count == 0
            || relation_batch.related_row_count.is_some()
            || lengths().any(|(_, length)| length == related_count);
        if !confirmed && disagreeing.is_none() {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` offsets end at {related_count}, but no related column was supplied to confirm {related_count} related rows; supply the relation's related columns or set related_row_count to the number of related rows the offsets index (one per root and member pair)"
            )));
        }
        if confirmed && related_count > MAX_DENSE_ROWS {
            return Err(EvalError::TypeMismatch(format!(
                "dense relation `{name}` claims {related_count} related rows, more than the {MAX_DENSE_ROWS} a dense batch can hold"
            )));
        }
        // A supplied column that disagrees confirms nothing and is malformed.
        if let Some((input, length)) = disagreeing {
            return Err(related_length_mismatch(input, name, length, related_count));
        }
        Ok(())
    }

    /// Build a checked key's owners, reserved with `try_reserve_exact`: a
    /// stated count is trusted up to [`MAX_DENSE_ROWS`], and a reservation the
    /// allocator reports as failed is an error rather than an abort. A
    /// reservation that succeeds says nothing about later allocations.
    fn bind_relation_key(
        key: &DenseRelationKey,
        relation_batch: DenseRelationBatchSpec,
        related_count: usize,
    ) -> Result<BoundRelationKey, EvalError> {
        let offsets = relation_batch.offsets;
        let mut owners = Vec::new();
        owners.try_reserve_exact(related_count).map_err(|_| {
            EvalError::TypeMismatch(format!(
                "dense relation `{}` claims {related_count} related rows, more than this process can allocate",
                key.name
            ))
        })?;
        for (row, pair) in offsets.windows(2).enumerate() {
            owners.extend(std::iter::repeat_n(row, pair[1] - pair[0]));
        }
        Ok(BoundRelationKey {
            offsets: Rc::new(offsets),
            related_count,
            owners: Rc::new(owners),
            unbound_inputs: relation_batch.inputs,
            inputs: HashMap::new(),
        })
    }
}

/// Among the columns `pick` selects, the one with the smallest name, found in
/// one pass without allocating, so a malformed batch reports the same column
/// whatever the map's iteration order.
fn first_by_name<'a>(
    columns: &'a HashMap<String, DenseColumn>,
    pick: impl Fn(&DenseColumn) -> bool,
) -> Option<(&'a String, &'a DenseColumn)> {
    columns
        .iter()
        .filter(|(_, column)| pick(column))
        .min_by_key(|(name, _)| *name)
}

fn related_length_mismatch(
    input: &str,
    relation: &str,
    length: usize,
    related_count: usize,
) -> EvalError {
    EvalError::TypeMismatch(format!(
        "dense relation input `{input}` for `{relation}` has length {length} but related row count is {related_count}"
    ))
}

/// The refusal of a rule or derived relation reached again while it is being
/// compiled.
fn cyclic_dependency(name: &str) -> DenseCompileError {
    DenseCompileError::Unsupported(format!(
        "cyclic dense compilation dependency involving `{name}`"
    ))
}

/// Dense declines an aggregation nested past [`MAX_INLINE_DEPTH`].
fn inlined_too_deep() -> DenseCompileError {
    DenseCompileError::Unsupported(format!(
        "a relation aggregation whose inlined rules nest more than {MAX_INLINE_DEPTH} levels deep; use generic API execution"
    ))
}

/// A source graph is a forest directed from each source to its derived
/// relations. DFS intervals let a membership query find the first edge below
/// its tested ancestor without walking or retaining every intervening pair.
struct CandidateAncestry<'a> {
    scopes: HashMap<&'a str, CandidateScope>,
    #[cfg(test)]
    visits: usize,
}

struct CandidateScope {
    start: usize,
    end: usize,
    /// Direct children, in DFS order; their intervals are disjoint.
    children: Vec<CandidateBranch>,
}

struct CandidateBranch {
    start: usize,
    end: usize,
    current_slot: usize,
    related_slot: usize,
}

impl<'a> CandidateAncestry<'a> {
    fn new(program: &'a Program) -> Self {
        let mut children: HashMap<&str, Vec<(&str, usize, usize)>> = HashMap::new();
        let mut derived = HashSet::new();
        for (name, schema) in &program.relations {
            children.entry(name).or_default();
            // Paths are refused when compilation reaches them. They do not
            // belong to this single-source forest, including when unused.
            if let Some(derivation) = &schema.derivation
                && derivation.source_path.is_none()
                && let Some(source) = &derivation.source_relation
            {
                derived.insert(name.as_str());
                children.entry(source).or_default().push((
                    name,
                    derivation.current_slot,
                    derivation.related_slot,
                ));
            }
        }
        let mut intervals: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut next = 0;
        for root in children
            .keys()
            .copied()
            .filter(|name| !derived.contains(name))
        {
            let mut stack = vec![(root, false)];
            while let Some((name, exiting)) = stack.pop() {
                if exiting {
                    intervals.get_mut(name).expect("entered before exiting").1 = next;
                } else {
                    intervals.insert(name, (next, next));
                    next += 1;
                    stack.push((name, true));
                    for &(child, _, _) in children[name].iter().rev() {
                        stack.push((child, false));
                    }
                }
            }
        }
        // Cyclic components have no root and receive no interval. The
        // compilation walk still refuses them with its existing diagnostic.
        let scopes = intervals
            .iter()
            .map(|(&name, &(start, end))| {
                let branches = children[name]
                    .iter()
                    .filter_map(|&(child, current_slot, related_slot)| {
                        let &(start, end) = intervals.get(child)?;
                        Some(CandidateBranch {
                            start,
                            end,
                            current_slot,
                            related_slot,
                        })
                    })
                    .collect();
                (
                    name,
                    CandidateScope {
                        start,
                        end,
                        children: branches,
                    },
                )
            })
            .collect();
        Self {
            scopes,
            #[cfg(test)]
            visits: next,
        }
    }

    fn holds(
        &mut self,
        scope: &str,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
    ) -> bool {
        #[cfg(test)]
        {
            self.visits += 1;
        }
        let (Some(scope), Some(relation)) = (self.scopes.get(scope), self.scopes.get(relation))
        else {
            return false;
        };
        if scope.start <= relation.start || scope.start >= relation.end {
            return false;
        }
        let child = relation.children.partition_point(|child| {
            #[cfg(test)]
            {
                self.visits += 1;
            }
            child.start <= scope.start
        });
        child.checked_sub(1).is_some_and(|child| {
            let child = &relation.children[child];
            scope.start < child.end
                && child.current_slot == current_slot
                && child.related_slot == related_slot
        })
    }
}

struct DenseCompiler<'a> {
    program: &'a Program,
    root_entity: String,
    root_inputs: Vec<String>,
    root_input_index: HashMap<String, usize>,
    relations: Vec<DenseRelationSchema>,
    relation_index: HashMap<DenseRelationKey, usize>,
    relation_input_index: HashMap<(usize, String), usize>,
    parameters: Vec<CompiledParameter>,
    parameter_index: HashMap<String, usize>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
    /// The rules on the current compilation path: each root rule being
    /// compiled into its own column and each rule being inlined into a related
    /// or current-entity expression. Reaching one again means the rules form a
    /// cycle, which compiling an artifact refuses but a raw `Program` does
    /// not, so compiling it again would recurse until the stack overflowed.
    visiting: HashSet<String>,
    /// The rules whose formulas are being compiled, innermost last. Related
    /// expressions inline a related entity's rules, so a `match` in one names
    /// the innermost rule here, as explain does.
    related_rules: Vec<String>,
    /// The derived relation whose own predicate is being compiled, if any.
    /// Explain evaluates a `relation_member` there with the candidate tuple in
    /// scope, and selects a referenced rule's record by its declared entity,
    /// including under `if` conditions and other operands. It is `None` in
    /// `where` clauses, in `sum` values and in the rules a predicate reads,
    /// which explain evaluates without that scope.
    membership_scope: Option<String>,
    /// Only explicitly requested membership answers, rather than every
    /// intermediate scope/target pair along their source paths.
    candidate_membership: HashMap<(String, String, usize, usize), bool>,
    candidate_ancestry: Option<CandidateAncestry<'a>>,
    /// Expression levels on the stack since the driver's current task began.
    depth: usize,
    /// The level at which a rule not yet compiled is deferred to the driver
    /// (see [`crate::depth`]).
    suspend_depth: usize,
    /// Nesting of the expression being inlined into a relation aggregation
    /// (see [`MAX_INLINE_DEPTH`]).
    inline_depth: usize,
    /// The deepest `inline_depth` reached, counting a shared body's height,
    /// since the body being compiled began (see [`InlineBody::height`]).
    inline_peak: usize,
    inline_scalars: Vec<InlineBody<CompiledScalarExpr>>,
    inline_judgments: Vec<InlineBody<CompiledJudgmentExpr>>,
    related_inline_scalars: Vec<InlineBody<CompiledRelatedScalarExpr>>,
    related_inline_judgments: Vec<InlineBody<CompiledRelatedJudgmentExpr>>,
    /// Each inlined body's index in the table its site and kind select.
    inline_index: HashMap<InlineSite, usize>,
    /// The reads of inlined bodies (each a `references` increment) made by
    /// rules, bodies and derived relations' predicates still compiling. A
    /// rule that finishes keeps its reads. So does a body once it is
    /// recorded, and a predicate once its relation's filter is set, since the
    /// retry shares the body and finds the relation registered rather than
    /// compiling either again. When a task defers, the reads still here,
    /// those the unwound rules made themselves, are taken back, since the
    /// retry makes them again (see [`Self::compile_tasks`]).
    inline_reads: Vec<(InlineTable, usize)>,
    /// Expression levels evaluated since compiling last reported them (see
    /// [`crate::depth::count_visits`]).
    visits: usize,
}

/// How deep an expression inlined into a relation aggregation may nest.
///
/// A `where` clause or a summed value inlines the rules it reads, and a rule
/// of the current entity read there inlines the same way, so the compiled
/// expression nests as deep as the chain of rules it inlines. Inlined rules
/// have no compiled index to defer to (see [`crate::depth`]), so compiling
/// and evaluating them recurse through the whole chain. Past this bound dense
/// declines the program; the generic API evaluates it. The bound is a property
/// of the program alone, so every build and host declines the same programs.
/// It also caps the stack inlining uses: measured in a release build, a
/// chain at the bound compiles and evaluates on a 1 MiB thread, not on
/// 512 KiB. Debug frames are far larger.
pub const MAX_INLINE_DEPTH: usize = 512;

/// Why compiling a rule stopped: an error, or a dependency deferred to the
/// driver, which compiles it and retries.
enum CompileStop {
    Error(DenseCompileError),
    Defer(String),
}

impl From<DenseCompileError> for CompileStop {
    fn from(error: DenseCompileError) -> Self {
        Self::Error(error)
    }
}

type Compiling<T> = Result<T, CompileStop>;

impl<'a> DenseCompiler<'a> {
    fn new(program: &'a Program, root_entity: String) -> Result<Self, DenseCompileError> {
        Ok(Self {
            program,
            root_entity,
            root_inputs: Vec::new(),
            root_input_index: HashMap::new(),
            relations: Vec::new(),
            relation_index: HashMap::new(),
            relation_input_index: HashMap::new(),
            parameters: Vec::new(),
            parameter_index: HashMap::new(),
            derived: Vec::new(),
            derived_index: HashMap::new(),
            visiting: HashSet::new(),
            related_rules: Vec::new(),
            membership_scope: None,
            candidate_membership: HashMap::new(),
            candidate_ancestry: None,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            inline_depth: 0,
            inline_peak: 0,
            inline_scalars: Vec::new(),
            inline_judgments: Vec::new(),
            related_inline_scalars: Vec::new(),
            related_inline_judgments: Vec::new(),
            inline_index: HashMap::new(),
            inline_reads: Vec::new(),
            visits: 0,
        })
    }

    /// How explain names `rule` when an error leaves it: by its id when it has
    /// one. An empty `rule` (a derived relation's predicate) stays empty.
    /// Compile one level of an expression inlined into a relation
    /// aggregation, declining past [`MAX_INLINE_DEPTH`].
    fn inlined<T>(
        &mut self,
        compile: impl FnOnce(&mut Self) -> Result<T, DenseCompileError>,
    ) -> Result<T, DenseCompileError> {
        if self.inline_depth >= MAX_INLINE_DEPTH {
            return Err(inlined_too_deep());
        }
        self.inline_depth += 1;
        self.inline_peak = self.inline_peak.max(self.inline_depth);
        let compiled = compile(self);
        self.inline_depth -= 1;
        compiled
    }

    fn rule_label(&self, rule: &str) -> String {
        self.program
            .derived
            .get(rule)
            .and_then(|derived| derived.id.clone())
            .unwrap_or_else(|| rule.to_string())
    }

    fn match_labels(
        &self,
        rule: &str,
        subject: &ScalarExpr,
        patterns: &[ScalarExpr],
    ) -> MatchLabels {
        MatchLabels {
            rule: Arc::from(self.rule_label(rule)),
            subject: Arc::from(crate::engine::describe_match_operand(subject)),
            patterns: Arc::from(
                patterns
                    .iter()
                    .map(crate::engine::describe_match_operand)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }

    fn related_rule(&self) -> String {
        self.related_rules.last().cloned().unwrap_or_default()
    }

    fn finish(self) -> DenseCompiledProgram {
        DenseCompiledProgram {
            root_entity: self.root_entity,
            root_inputs: self.root_inputs,
            relations: self.relations,
            parameters: self.parameters,
            derived: self.derived,
            derived_index: self.derived_index,
            inline_scalars: self.inline_scalars,
            inline_judgments: self.inline_judgments,
            related_inline_scalars: self.related_inline_scalars,
            related_inline_judgments: self.related_inline_judgments,
        }
    }

    /// Compile `name` and the rules it depends on. When compiling defers a
    /// dependency (see [`crate::depth`]), compile that dependency as a task of
    /// its own, from level zero, and retry. A retry reaches the same
    /// references in the same order, so rules, inputs, relations and
    /// parameters are registered in the order the recursion registers them.
    fn compile_derived(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        let compiled = self.compile_tasks(name);
        crate::depth::add_visits(std::mem::take(&mut self.visits));
        compiled
    }

    fn compile_tasks(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        let mut tasks: Vec<String> = Vec::new();
        loop {
            // A deferral unwinds every rule in progress; nothing is being
            // compiled or inlined when a task starts.
            self.depth = 0;
            self.visiting.clear();
            self.related_rules.clear();
            self.membership_scope = None;
            let target = tasks.last().cloned().unwrap_or_else(|| name.to_string());
            let compiled = self.compile_rule(&target);
            if let Err(CompileStop::Defer(_)) = compiled {
                // The retry makes again each read the unwound rules made
                // themselves. It shares the bodies they compiled and finds the
                // relations they registered, whose reads were kept when each
                // body was recorded and each predicate compiled: take back the
                // former, so a body counts the reads of the compiled program.
                for (table, index) in std::mem::take(&mut self.inline_reads) {
                    *self.inline_references(table, index) -= 1;
                }
            }
            match compiled {
                Ok(index) => {
                    if tasks.pop().is_none() {
                        return Ok(index);
                    }
                }
                Err(CompileStop::Error(error)) => return Err(error),
                Err(CompileStop::Defer(dependency)) => {
                    if dependency == name || tasks.contains(&dependency) {
                        return Err(cyclic_dependency(&dependency));
                    }
                    tasks.push(dependency);
                }
            }
        }
    }

    fn compile_rule(&mut self, name: &str) -> Compiling<usize> {
        let reads = self.inline_reads.len();
        let compiled = self.compile_rule_formula(name);
        if compiled.is_ok() {
            // The rule is compiled for good, and keeps the reads it made.
            self.inline_reads.truncate(reads);
        }
        compiled
    }

    fn compile_rule_formula(&mut self, name: &str) -> Compiling<usize> {
        if let Some(&index) = self.derived_index.get(name) {
            return Ok(index);
        }
        if self.visiting.contains(name) {
            return Err(cyclic_dependency(name).into());
        }
        if self.depth >= self.suspend_depth {
            return Err(CompileStop::Defer(name.to_string()));
        }

        let derived =
            self.program.derived.get(name).ok_or_else(|| {
                DenseCompileError::Unsupported(format!("unknown derived `{name}`"))
            })?;
        // Since #84 stopped discarding commencement dates, every declared derived rule carries
        // at least one version, so rejecting all versioned rules here would reject the whole
        // corpus. The single unbounded version — the shape #84 restored — is compiled directly
        // and its commencement enforced at execution time. Genuinely multi-version or
        // end-bounded rules still need per-period selection the dense plan cannot express, and
        // are still routed to the generic API.
        let versioned_semantics = match derived.versions.as_slice() {
            [] => None,
            [only] if only.effective_to.is_none() => Some((&only.semantics, only.effective_from)),
            _ => {
                return Err(DenseCompileError::Unsupported(format!(
                    "versioned derived formulas in `{name}`; use generic API execution"
                ))
                .into());
            }
        };
        if derived.entity != self.root_entity && derived.entity != SCALAR_ENTITY {
            return Err(DenseCompileError::CrossEntityDependency {
                derived: name.to_string(),
                dependency: name.to_string(),
                entity: derived.entity.clone(),
            }
            .into());
        }

        let (source_semantics, effective_from) = match versioned_semantics {
            Some((semantics, from)) => (semantics, Some(from)),
            None => (&derived.semantics, None),
        };

        self.visiting.insert(name.to_string());
        self.related_rules.push(name.to_string());
        let compiled_semantics = match source_semantics {
            DerivedSemantics::Scalar(expr) => {
                CompiledSemantics::Scalar(self.compile_scalar_expr(name, expr)?)
            }
            DerivedSemantics::Judgment(expr) => {
                CompiledSemantics::Judgment(self.compile_judgment_expr(name, expr)?)
            }
        };
        self.related_rules.pop();
        self.visiting.remove(name);

        let index = self.derived.len();
        self.derived.push(CompiledDerived {
            name: derived.name.clone(),
            label: self.rule_label(name),
            semantics: compiled_semantics,
            rounding: derived.rounding,
            effective_from,
        });
        self.derived_index.insert(name.to_string(), index);
        Ok(index)
    }

    /// Run `compile`, which inlines `rule`'s formula, with `rule` on the
    /// compilation path. A body is recorded for sharing only once it has
    /// compiled (see [`Self::inline_body`]), so a rule that reads itself
    /// would inline itself again, without end; a rule already on the path is
    /// refused.
    fn inline_rule<T>(
        &mut self,
        rule: &str,
        compile: impl FnOnce(&mut Self) -> Result<T, DenseCompileError>,
    ) -> Result<T, DenseCompileError> {
        if !self.visiting.insert(rule.to_string()) {
            return Err(cyclic_dependency(rule));
        }
        let compiled = compile(self);
        self.visiting.remove(rule);
        compiled
    }

    fn inline_references(&mut self, table: InlineTable, index: usize) -> &mut usize {
        match table {
            InlineTable::Scalars => &mut self.inline_scalars[index].references,
            InlineTable::Judgments => &mut self.inline_judgments[index].references,
            InlineTable::RelatedScalars => &mut self.related_inline_scalars[index].references,
            InlineTable::RelatedJudgments => &mut self.related_inline_judgments[index].references,
        }
    }

    fn inline_height(&self, table: InlineTable, index: usize) -> usize {
        match table {
            InlineTable::Scalars => self.inline_scalars[index].height,
            InlineTable::Judgments => self.inline_judgments[index].height,
            InlineTable::RelatedScalars => self.related_inline_scalars[index].height,
            InlineTable::RelatedJudgments => self.related_inline_judgments[index].height,
        }
    }

    /// Record a read of the body at `index`.
    fn read_inline(&mut self, table: InlineTable, index: usize) {
        *self.inline_references(table, index) += 1;
        self.inline_reads.push((table, index));
    }

    /// The index of `rule`'s body at `site`, compiling it with `compile` on
    /// the first read and sharing it after (see [`InlineBody`]).
    ///
    /// A shared read refuses or accepts exactly what compiling the body again
    /// there would have. A rule on the compilation path is a cycle, checked
    /// first, as [`Self::inline_rule`] checks it. A body already compiled
    /// compiled without error, and compiles to the same expression from every
    /// read at its site, so the one other way compiling it again could fail is
    /// [`MAX_INLINE_DEPTH`]: the read is refused when the body's height,
    /// from the read's depth, passes the bound.
    fn inline_body<E>(
        &mut self,
        table: InlineTable,
        site: InlineSite,
        rule: &str,
        compile: impl FnOnce(&mut Self) -> Result<E, DenseCompileError>,
        bodies: fn(&mut Self) -> &mut Vec<InlineBody<E>>,
    ) -> Result<usize, DenseCompileError> {
        if self.visiting.contains(rule) {
            return Err(cyclic_dependency(rule));
        }
        if let Some(&index) = self.inline_index.get(&site) {
            let reached = self.inline_depth + self.inline_height(table, index);
            if reached > MAX_INLINE_DEPTH {
                return Err(inlined_too_deep());
            }
            self.inline_peak = self.inline_peak.max(reached);
            self.read_inline(table, index);
            return Ok(index);
        }
        let start = self.inline_depth;
        let outer_peak = std::mem::replace(&mut self.inline_peak, start);
        let reads = self.inline_reads.len();
        let body = self.inline_rule(rule, compile);
        let height = self.inline_peak - start;
        self.inline_peak = outer_peak.max(self.inline_peak);
        let body = body?;
        // The body is recorded for good, and its nodes are final: it keeps
        // the reads its compile made, as a finished rule does (see
        // `compile_rule`). A deferral that unwinds its reader takes back only
        // the reader's own read of it, which the retry makes again.
        self.inline_reads.truncate(reads);
        let table_entries = bodies(self);
        let index = table_entries.len();
        table_entries.push(InlineBody {
            body,
            references: 0,
            height,
        });
        self.inline_index.insert(site, index);
        self.read_inline(table, index);
        Ok(index)
    }

    /// `rule`'s scalar body on root rows, compiled once.
    fn inline_current_scalar(
        &mut self,
        rule: &str,
        entity: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        let index = self.inline_body(
            InlineTable::Scalars,
            InlineSite::Current(rule.to_string()),
            rule,
            |compiler| compiler.compile_current_scalar_expr(rule, entity, expr),
            |compiler| &mut compiler.inline_scalars,
        )?;
        Ok(CompiledScalarExpr::Inline(index))
    }

    /// `rule`'s judgment body on root rows, compiled once.
    fn inline_current_judgment(
        &mut self,
        rule: &str,
        entity: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledJudgmentExpr, DenseCompileError> {
        let index = self.inline_body(
            InlineTable::Judgments,
            InlineSite::Current(rule.to_string()),
            rule,
            |compiler| compiler.compile_current_judgment_expr(rule, entity, expr),
            |compiler| &mut compiler.inline_judgments,
        )?;
        Ok(CompiledJudgmentExpr::Inline(index))
    }

    /// `rule`'s scalar body on `relation_index`'s related rows, compiled
    /// once. It compiles with `rule` as the innermost rule, which names a
    /// `match` in it, and outside any derived relation's own predicate, as
    /// explain evaluates a rule body.
    fn inline_related_scalar(
        &mut self,
        relation_index: usize,
        rule: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        let index = self.inline_body(
            InlineTable::RelatedScalars,
            InlineSite::Related(relation_index, rule.to_string()),
            rule,
            |compiler| {
                compiler.related_rules.push(rule.to_string());
                let scope = compiler.membership_scope.take();
                let compiled = compiler.compile_related_scalar(relation_index, expr);
                compiler.membership_scope = scope;
                compiler.related_rules.pop();
                compiled
            },
            |compiler| &mut compiler.related_inline_scalars,
        )?;
        Ok(CompiledRelatedScalarExpr::Inline(index))
    }

    /// `rule`'s judgment body on `relation_index`'s related rows, compiled
    /// once, as [`Self::inline_related_scalar`] compiles a scalar body.
    fn inline_related_judgment(
        &mut self,
        relation_index: usize,
        rule: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        let index = self.inline_body(
            InlineTable::RelatedJudgments,
            InlineSite::Related(relation_index, rule.to_string()),
            rule,
            |compiler| {
                compiler.related_rules.push(rule.to_string());
                let scope = compiler.membership_scope.take();
                let compiled = compiler.compile_related_predicate(relation_index, expr);
                compiler.membership_scope = scope;
                compiler.related_rules.pop();
                compiled
            },
            |compiler| &mut compiler.related_inline_judgments,
        )?;
        Ok(CompiledRelatedJudgmentExpr::Inline(index))
    }

    fn compile_scalar_expr(
        &mut self,
        derived_name: &str,
        expr: &ScalarExpr,
    ) -> Compiling<CompiledScalarExpr> {
        self.visits += 1;
        self.depth += 1;
        let compiled = self.compile_scalar_node(derived_name, expr);
        self.depth -= 1;
        compiled
    }

    fn compile_scalar_node(
        &mut self,
        derived_name: &str,
        expr: &ScalarExpr,
    ) -> Compiling<CompiledScalarExpr> {
        match expr {
            ScalarExpr::RoleInput { term, name } | ScalarExpr::RoleDerived { term, name } => {
                Err(relation_role_unsupported(*term, name).into())
            }
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.into())),
            ScalarExpr::InputForYear { .. } => Err(DenseCompileError::Unsupported(
                "selected-year input requires period-aware Explain execution".into(),
            )
            .into()),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name),
                default: default.into(),
            }),
            ScalarExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown scalar dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != self.root_entity && dependency.entity != SCALAR_ENTITY {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    }
                    .into());
                }
                Ok(CompiledScalarExpr::Derived(self.compile_rule(name)?))
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                Ok(CompiledScalarExpr::ParameterLookup {
                    parameter: self.parameter(parameter)?,
                    index: Box::new(self.compile_scalar_expr(derived_name, index)?),
                })
            }
            ScalarExpr::Add(items) => Ok(CompiledScalarExpr::Add(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Compiling<Vec<CompiledScalarExpr>>>()?,
            )),
            ScalarExpr::Sub(left, right) => Ok(CompiledScalarExpr::Sub(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Mul(left, right) => Ok(CompiledScalarExpr::Mul(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Div(left, right) => Ok(CompiledScalarExpr::Div(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Max(items) => Ok(CompiledScalarExpr::Max(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Compiling<Vec<CompiledScalarExpr>>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Compiling<Vec<CompiledScalarExpr>>>()?,
            )),
            ScalarExpr::Ceil(value) => Ok(CompiledScalarExpr::Ceil(Box::new(
                self.compile_scalar_expr(derived_name, value)?,
            ))),
            ScalarExpr::Floor(value) => Ok(CompiledScalarExpr::Floor(Box::new(
                self.compile_scalar_expr(derived_name, value)?,
            ))),
            ScalarExpr::PeriodStart => Ok(CompiledScalarExpr::PeriodStart),
            ScalarExpr::PeriodEnd => Ok(CompiledScalarExpr::PeriodEnd),
            ScalarExpr::DateAddDays { date, days } => Ok(CompiledScalarExpr::DateAddDays {
                date: Box::new(self.compile_scalar_expr(derived_name, date)?),
                days: Box::new(self.compile_scalar_expr(derived_name, days)?),
            }),
            ScalarExpr::DateAddMonths { date, months } => Ok(CompiledScalarExpr::DateAddMonths {
                date: Box::new(self.compile_scalar_expr(derived_name, date)?),
                months: Box::new(self.compile_scalar_expr(derived_name, months)?),
            }),
            ScalarExpr::DateAddYears { date, years } => Ok(CompiledScalarExpr::DateAddYears {
                date: Box::new(self.compile_scalar_expr(derived_name, date)?),
                years: Box::new(self.compile_scalar_expr(derived_name, years)?),
            }),
            ScalarExpr::DaysBetween { from, to } => Ok(CompiledScalarExpr::DaysBetween {
                from: Box::new(self.compile_scalar_expr(derived_name, from)?),
                to: Box::new(self.compile_scalar_expr(derived_name, to)?),
            }),
            ScalarExpr::OnlyRelated {
                relation,
                current_slot,
                related_slot,
                value,
            } => {
                let RelatedValueRef::Input(name) = value else {
                    return Err(DenseCompileError::Unsupported(
                        "only_related requires a Boolean input".into(),
                    )
                    .into());
                };
                let relation_name = relation.clone();
                let relation = self.relation(relation, *current_slot, *related_slot)?;
                let input = self.related_input(relation, name);
                Ok(CompiledScalarExpr::OnlyRelated {
                    relation,
                    relation_name,
                    input,
                })
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let relation_index = self.relation(relation, *current_slot, *related_slot)?;
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| self.compile_related_predicate(relation_index, inner))
                    .transpose()?;
                Ok(CompiledScalarExpr::CountRelated {
                    relation: relation_index,
                    predicate,
                })
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let relation_index = self.relation(relation, *current_slot, *related_slot)?;
                let value = self.compile_related_value(relation_index, value)?;
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| self.compile_related_predicate(relation_index, inner))
                    .transpose()?;
                Ok(CompiledScalarExpr::SumRelated {
                    relation: relation_index,
                    value: Box::new(value),
                    predicate,
                })
            }
            ScalarExpr::ExtremeRelated {
                extreme,
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let function = extreme.call_name(where_clause.is_some());
                // A relation schema may be shared by several aggregations.
                // Its own unsupported construct determines the error, so a
                // different first reader cannot change the refusal text.
                let relation_index = self.relation(relation, *current_slot, *related_slot)?;
                let value = self
                    .compile_related_value(relation_index, value)
                    .map_err(|error| named_aggregate_error(function, relation, error))?;
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| self.compile_related_predicate(relation_index, inner))
                    .transpose()
                    .map_err(|error| named_aggregate_error(function, relation, error))?;
                Ok(CompiledScalarExpr::ExtremeRelated {
                    extreme: *extreme,
                    relation: relation_index,
                    relation_name: relation.clone(),
                    value: Box::new(value),
                    predicate,
                })
            }
            ScalarExpr::SingleRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let relation_index = self.relation(relation, *current_slot, *related_slot)?;
                let value = self
                    .compile_related_value(relation_index, value)
                    .map_err(|error| named_aggregate_error("only", relation, error))?;
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| self.compile_related_predicate(relation_index, inner))
                    .transpose()
                    .map_err(|error| named_aggregate_error("only", relation, error))?;
                Ok(CompiledScalarExpr::SingleRelated {
                    relation: relation_index,
                    relation_name: relation.clone(),
                    value: Box::new(value),
                    predicate,
                })
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(CompiledScalarExpr::If {
                condition: Box::new(self.compile_judgment_expr(derived_name, condition)?),
                then_expr: Box::new(self.compile_scalar_expr(derived_name, then_expr)?),
                else_expr: Box::new(match else_expr.as_ref() {
                    ScalarExpr::NoMatch { subject, patterns } => CompiledScalarExpr::NoMatch {
                        subject: Box::new(self.compile_scalar_expr(derived_name, subject)?),
                        placeholder: Box::new(self.compile_scalar_expr(derived_name, then_expr)?),
                        labels: self.match_labels(derived_name, subject, patterns),
                    },
                    else_expr => self.compile_scalar_expr(derived_name, else_expr)?,
                }),
            }),
            ScalarExpr::NoMatch { .. } => {
                Err(DenseCompileError::Unsupported(NO_MATCH_OUTSIDE_IF.to_string()).into())
            }
            ScalarExpr::OverPeriods { kind, value, n } => {
                let value = self.compile_scalar_expr(derived_name, value)?;
                let n = n
                    .as_ref()
                    .map(|inner| self.compile_scalar_expr(derived_name, inner).map(Box::new))
                    .transpose()?;
                // A reduction reduces the period axis away, so nesting another
                // reduction directly in its argument is meaningless. Reject it at
                // compile time with a precise message rather than letting the
                // inner reduction surface an opaque per-period error at run time.
                if let Some(inner) = nested_over_periods_kind(&value)
                    .or_else(|| n.as_deref().and_then(nested_over_periods_kind))
                {
                    return Err(DenseCompileError::NestedOverPeriods {
                        outer: kind.as_call_name(),
                        inner: inner.as_call_name(),
                    }
                    .into());
                }
                Ok(CompiledScalarExpr::OverPeriods {
                    kind: *kind,
                    value: Box::new(value),
                    n,
                })
            }
        }
    }

    fn compile_judgment_expr(
        &mut self,
        derived_name: &str,
        expr: &JudgmentExpr,
    ) -> Compiling<CompiledJudgmentExpr> {
        self.visits += 1;
        self.depth += 1;
        let compiled = self.compile_judgment_node(derived_name, expr);
        self.depth -= 1;
        compiled
    }

    fn compile_judgment_node(
        &mut self,
        derived_name: &str,
        expr: &JudgmentExpr,
    ) -> Compiling<CompiledJudgmentExpr> {
        match expr {
            JudgmentExpr::RoleDerived { term, name } => {
                Err(relation_role_unsupported(*term, name).into())
            }
            JudgmentExpr::RelationAtom { relation, .. } => {
                Err(relation_atom_unsupported(relation).into())
            }
            JudgmentExpr::Exists { .. } => Err(exists_unsupported().into()),
            JudgmentExpr::SameEntity { .. } => Err(term_comparison_unsupported().into()),
            JudgmentExpr::Comparison { left, op, right } => Ok(CompiledJudgmentExpr::Comparison {
                left: self.compile_scalar_expr(derived_name, left)?,
                op: *op,
                right: self.compile_scalar_expr(derived_name, right)?,
            }),
            JudgmentExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown judgment dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != self.root_entity && dependency.entity != SCALAR_ENTITY {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    }
                    .into());
                }
                Ok(CompiledJudgmentExpr::Derived(self.compile_rule(name)?))
            }
            JudgmentExpr::RelationMember { relation, .. } => Err(DenseCompileError::Unsupported(
                format!("relation predicate `{relation}`"),
            )
            .into()),
            JudgmentExpr::And(items) => Ok(CompiledJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Compiling<Vec<CompiledJudgmentExpr>>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Compiling<Vec<CompiledJudgmentExpr>>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledJudgmentExpr::Not(Box::new(
                self.compile_judgment_expr(derived_name, item)?,
            ))),
        }
    }

    fn compile_related_predicate(
        &mut self,
        relation_index: usize,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        self.inlined(|compiler| compiler.compile_related_predicate_node(relation_index, expr))
    }

    fn compile_related_predicate_node(
        &mut self,
        relation_index: usize,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        match expr {
            JudgmentExpr::RoleDerived { term, name } => Err(relation_role_unsupported(*term, name)),
            JudgmentExpr::RelationAtom { relation, .. } => Err(relation_atom_unsupported(relation)),
            JudgmentExpr::Exists { .. } => Err(exists_unsupported()),
            JudgmentExpr::SameEntity { .. } => Err(term_comparison_unsupported()),
            JudgmentExpr::Comparison { left, op, right } => {
                Ok(CompiledRelatedJudgmentExpr::Comparison {
                    left: self.compile_related_scalar(relation_index, left)?,
                    op: *op,
                    right: self.compile_related_scalar(relation_index, right)?,
                })
            }
            JudgmentExpr::Derived(name) => {
                let derived = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown related judgment dependency `{name}`"
                    ))
                })?;
                let (semantics, commencement) = inlined_rule_semantics(derived)?;
                let compiled = if self.reads_current_record(
                    relation_index,
                    "predicate",
                    name,
                    &derived.entity,
                )? {
                    match semantics {
                        DerivedSemantics::Judgment(expr) => {
                            Ok(CompiledRelatedJudgmentExpr::RootJudgment(Box::new(
                                self.inline_current_judgment(name, &derived.entity, expr)?,
                            )))
                        }
                        DerivedSemantics::Scalar(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "where-clause predicates cannot reference scalar derived values (`{name}`)"
                            )))
                        }
                    }
                } else {
                    match semantics {
                        DerivedSemantics::Judgment(expr) => {
                            self.inline_related_judgment(relation_index, name, expr)
                        }
                        DerivedSemantics::Scalar(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "where-clause predicates cannot reference scalar derived values (`{name}`)"
                            )))
                        }
                    }
                }?;
                Ok(match commencement {
                    Some(rule) => CompiledRelatedJudgmentExpr::Commenced {
                        rule,
                        value: Box::new(compiled),
                    },
                    None => compiled,
                })
            }
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => match self.membership_scope.clone() {
                None => Ok(CompiledRelatedJudgmentExpr::RelationMemberWithoutContext {
                    relation: relation.clone(),
                }),
                Some(scope)
                    if self.holds_for_every_candidate(
                        &scope,
                        relation,
                        *current_slot,
                        *related_slot,
                    ) =>
                {
                    Ok(CompiledRelatedJudgmentExpr::Literal(true))
                }
                Some(scope) => Err(match path_source_below(self.program, relation) {
                    Some(path) => path_source_unsupported(path),
                    None => DenseCompileError::Unsupported(format!(
                        "`relation_member` of `{relation}` (slots {current_slot}, {related_slot}) in the predicate of derived relation `{scope}`; dense can test only a source of that relation, read with the slots its derivation reads it with"
                    )),
                }),
            },
            JudgmentExpr::And(items) => Ok(CompiledRelatedJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledRelatedJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledRelatedJudgmentExpr::Not(Box::new(
                self.compile_related_predicate(relation_index, item)?,
            ))),
        }
    }

    /// The value an aggregation reads on each related row: the related input,
    /// or the related rule inlined for the related rows.
    fn compile_related_value(
        &mut self,
        relation_index: usize,
        value: &RelatedValueRef,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        match value {
            RelatedValueRef::Input(name) => Ok(CompiledRelatedScalarExpr::Input(
                self.related_input(relation_index, name),
            )),
            RelatedValueRef::Derived(name) => {
                self.compile_related_scalar(relation_index, &ScalarExpr::Derived(name.clone()))
            }
        }
    }

    fn compile_related_scalar(
        &mut self,
        relation_index: usize,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        self.inlined(|compiler| compiler.compile_related_scalar_node(relation_index, expr))
    }

    fn compile_related_scalar_node(
        &mut self,
        relation_index: usize,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::RoleInput { term, name } | ScalarExpr::RoleDerived { term, name } => {
                Err(relation_role_unsupported(*term, name))
            }
            ScalarExpr::Literal(value) => Ok(CompiledRelatedScalarExpr::Literal(value.into())),
            ScalarExpr::InputForYear { .. } => Err(DenseCompileError::Unsupported(
                "selected-year input requires period-aware Explain execution".into(),
            )
            .into()),
            ScalarExpr::Input(name) => {
                let input_index = self.related_input(relation_index, name);
                Ok(CompiledRelatedScalarExpr::Input(input_index))
            }
            ScalarExpr::InputOrElse { name, default } => {
                let input_index = self.related_input(relation_index, name);
                Ok(CompiledRelatedScalarExpr::InputOrElse {
                    input: input_index,
                    default: default.into(),
                })
            }
            ScalarExpr::Derived(name) => {
                let derived = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown related scalar dependency `{name}`"
                    ))
                })?;
                let (semantics, commencement) = inlined_rule_semantics(derived)?;
                let compiled = if self.reads_current_record(
                    relation_index,
                    "scalar",
                    name,
                    &derived.entity,
                )? {
                    match semantics {
                        DerivedSemantics::Scalar(expr) => {
                            let value = self.inline_current_scalar(name, &derived.entity, expr)?;
                            let value = match derived.rounding {
                                Some(rounding) => CompiledScalarExpr::Round {
                                    value: Box::new(value),
                                    rounding,
                                },
                                None => value,
                            };
                            Ok(CompiledRelatedScalarExpr::RootScalar(Box::new(value)))
                        }
                        DerivedSemantics::Judgment(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "related scalar expressions cannot reference judgment derived values (`{name}`)"
                            )))
                        }
                    }
                } else {
                    match semantics {
                        DerivedSemantics::Scalar(expr) => {
                            let value = self.inline_related_scalar(relation_index, name, expr)?;
                            Ok(match derived.rounding {
                                Some(rounding) => CompiledRelatedScalarExpr::Round {
                                    value: Box::new(value),
                                    rounding,
                                },
                                None => value,
                            })
                        }
                        DerivedSemantics::Judgment(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "related scalar expressions cannot reference judgment derived values (`{name}`)"
                            )))
                        }
                    }
                }?;
                Ok(match commencement {
                    Some(rule) => CompiledRelatedScalarExpr::Commenced {
                        rule,
                        value: Box::new(compiled),
                    },
                    None => compiled,
                })
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                Ok(CompiledRelatedScalarExpr::ParameterLookup {
                    parameter: self.parameter(parameter)?,
                    index: Box::new(self.compile_related_scalar(relation_index, index)?),
                })
            }
            ScalarExpr::Add(items) => Ok(CompiledRelatedScalarExpr::Add(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Sub(left, right) => Ok(CompiledRelatedScalarExpr::Sub(
                Box::new(self.compile_related_scalar(relation_index, left)?),
                Box::new(self.compile_related_scalar(relation_index, right)?),
            )),
            ScalarExpr::Mul(left, right) => Ok(CompiledRelatedScalarExpr::Mul(
                Box::new(self.compile_related_scalar(relation_index, left)?),
                Box::new(self.compile_related_scalar(relation_index, right)?),
            )),
            ScalarExpr::Div(left, right) => Ok(CompiledRelatedScalarExpr::Div(
                Box::new(self.compile_related_scalar(relation_index, left)?),
                Box::new(self.compile_related_scalar(relation_index, right)?),
            )),
            ScalarExpr::Max(items) => Ok(CompiledRelatedScalarExpr::Max(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledRelatedScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Ceil(value) => Ok(CompiledRelatedScalarExpr::Ceil(Box::new(
                self.compile_related_scalar(relation_index, value)?,
            ))),
            ScalarExpr::Floor(value) => Ok(CompiledRelatedScalarExpr::Floor(Box::new(
                self.compile_related_scalar(relation_index, value)?,
            ))),
            ScalarExpr::PeriodStart => Ok(CompiledRelatedScalarExpr::PeriodStart),
            ScalarExpr::PeriodEnd => Ok(CompiledRelatedScalarExpr::PeriodEnd),
            ScalarExpr::DateAddDays { date, days } => Ok(CompiledRelatedScalarExpr::DateAddDays {
                date: Box::new(self.compile_related_scalar(relation_index, date)?),
                days: Box::new(self.compile_related_scalar(relation_index, days)?),
            }),
            ScalarExpr::DateAddMonths { date, months } => {
                Ok(CompiledRelatedScalarExpr::DateAddMonths {
                    date: Box::new(self.compile_related_scalar(relation_index, date)?),
                    months: Box::new(self.compile_related_scalar(relation_index, months)?),
                })
            }
            ScalarExpr::DateAddYears { date, years } => {
                Ok(CompiledRelatedScalarExpr::DateAddYears {
                    date: Box::new(self.compile_related_scalar(relation_index, date)?),
                    years: Box::new(self.compile_related_scalar(relation_index, years)?),
                })
            }
            ScalarExpr::DaysBetween { from, to } => Ok(CompiledRelatedScalarExpr::DaysBetween {
                from: Box::new(self.compile_related_scalar(relation_index, from)?),
                to: Box::new(self.compile_related_scalar(relation_index, to)?),
            }),
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(CompiledRelatedScalarExpr::If {
                condition: Box::new(self.compile_related_predicate(relation_index, condition)?),
                then_expr: Box::new(self.compile_related_scalar(relation_index, then_expr)?),
                else_expr: Box::new(match else_expr.as_ref() {
                    ScalarExpr::NoMatch { subject, patterns } => {
                        CompiledRelatedScalarExpr::NoMatch {
                            subject: Box::new(
                                self.compile_related_scalar(relation_index, subject)?,
                            ),
                            placeholder: Box::new(
                                self.compile_related_scalar(relation_index, then_expr)?,
                            ),
                            labels: self.match_labels(&self.related_rule(), subject, patterns),
                        }
                    }
                    else_expr => self.compile_related_scalar(relation_index, else_expr)?,
                }),
            }),
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_IF.to_string(),
            )),
            ScalarExpr::OnlyRelated { relation, .. }
            | ScalarExpr::CountRelated { relation, .. }
            | ScalarExpr::SumRelated { relation, .. } => {
                Err(DenseCompileError::Unsupported(format!(
                    "aggregation over relation `{relation}` nested inside a related expression"
                )))
            }
            ScalarExpr::ExtremeRelated {
                extreme,
                relation,
                where_clause,
                ..
            } => Err(DenseCompileError::Unsupported(format!(
                "{} over relation `{relation}` nested inside a related expression",
                extreme.call_name(where_clause.is_some())
            ))),
            ScalarExpr::SingleRelated { relation, .. } => Err(DenseCompileError::Unsupported(
                format!("only over relation `{relation}` nested inside a related expression"),
            )),
            ScalarExpr::OverPeriods { kind, .. } => Err(DenseCompileError::Unsupported(format!(
                "over-periods reduction `{}` nested inside a related expression",
                kind.as_call_name()
            ))),
        }
    }

    fn compile_current_scalar_expr(
        &mut self,
        derived_name: &str,
        entity: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        self.inlined(|compiler| {
            compiler.compile_current_scalar_expr_node(derived_name, entity, expr)
        })
    }

    fn compile_current_scalar_expr_node(
        &mut self,
        derived_name: &str,
        entity: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::RoleInput { term, name } | ScalarExpr::RoleDerived { term, name } => {
                Err(relation_role_unsupported(*term, name))
            }
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.into())),
            ScalarExpr::InputForYear { .. } => Err(DenseCompileError::Unsupported(
                "selected-year input requires period-aware Explain execution".into(),
            )
            .into()),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name),
                default: default.into(),
            }),
            ScalarExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown scalar dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != entity
                    && dependency.entity != self.root_entity
                    && dependency.entity != SCALAR_ENTITY
                {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    });
                }
                let (semantics, commencement) = inlined_rule_semantics(dependency)?;
                let compiled = match semantics {
                    DerivedSemantics::Scalar(expr) => {
                        let value = self.inline_current_scalar(name, &dependency.entity, expr)?;
                        Ok(match dependency.rounding {
                            Some(rounding) => CompiledScalarExpr::Round {
                                value: Box::new(value),
                                rounding,
                            },
                            None => value,
                        })
                    }
                    DerivedSemantics::Judgment(_) => Err(DenseCompileError::Unsupported(format!(
                        "scalar expression cannot reference judgment derived value (`{name}`)"
                    ))),
                }?;
                Ok(match commencement {
                    Some(rule) => CompiledScalarExpr::Commenced {
                        rule,
                        value: Box::new(compiled),
                    },
                    None => compiled,
                })
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                Ok(CompiledScalarExpr::ParameterLookup {
                    parameter: self.parameter(parameter)?,
                    index: Box::new(self.compile_current_scalar_expr(
                        derived_name,
                        entity,
                        index,
                    )?),
                })
            }
            ScalarExpr::Add(items) => Ok(CompiledScalarExpr::Add(
                items
                    .iter()
                    .map(|item| self.compile_current_scalar_expr(derived_name, entity, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Sub(left, right) => Ok(CompiledScalarExpr::Sub(
                Box::new(self.compile_current_scalar_expr(derived_name, entity, left)?),
                Box::new(self.compile_current_scalar_expr(derived_name, entity, right)?),
            )),
            ScalarExpr::Mul(left, right) => Ok(CompiledScalarExpr::Mul(
                Box::new(self.compile_current_scalar_expr(derived_name, entity, left)?),
                Box::new(self.compile_current_scalar_expr(derived_name, entity, right)?),
            )),
            ScalarExpr::Div(left, right) => Ok(CompiledScalarExpr::Div(
                Box::new(self.compile_current_scalar_expr(derived_name, entity, left)?),
                Box::new(self.compile_current_scalar_expr(derived_name, entity, right)?),
            )),
            ScalarExpr::Max(items) => Ok(CompiledScalarExpr::Max(
                items
                    .iter()
                    .map(|item| self.compile_current_scalar_expr(derived_name, entity, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_current_scalar_expr(derived_name, entity, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Ceil(value) => Ok(CompiledScalarExpr::Ceil(Box::new(
                self.compile_current_scalar_expr(derived_name, entity, value)?,
            ))),
            ScalarExpr::Floor(value) => Ok(CompiledScalarExpr::Floor(Box::new(
                self.compile_current_scalar_expr(derived_name, entity, value)?,
            ))),
            ScalarExpr::PeriodStart => Ok(CompiledScalarExpr::PeriodStart),
            ScalarExpr::PeriodEnd => Ok(CompiledScalarExpr::PeriodEnd),
            ScalarExpr::DateAddDays { date, days } => Ok(CompiledScalarExpr::DateAddDays {
                date: Box::new(self.compile_current_scalar_expr(derived_name, entity, date)?),
                days: Box::new(self.compile_current_scalar_expr(derived_name, entity, days)?),
            }),
            ScalarExpr::DateAddMonths { date, months } => Ok(CompiledScalarExpr::DateAddMonths {
                date: Box::new(self.compile_current_scalar_expr(derived_name, entity, date)?),
                months: Box::new(self.compile_current_scalar_expr(derived_name, entity, months)?),
            }),
            ScalarExpr::DateAddYears { date, years } => Ok(CompiledScalarExpr::DateAddYears {
                date: Box::new(self.compile_current_scalar_expr(derived_name, entity, date)?),
                years: Box::new(self.compile_current_scalar_expr(derived_name, entity, years)?),
            }),
            ScalarExpr::DaysBetween { from, to } => Ok(CompiledScalarExpr::DaysBetween {
                from: Box::new(self.compile_current_scalar_expr(derived_name, entity, from)?),
                to: Box::new(self.compile_current_scalar_expr(derived_name, entity, to)?),
            }),
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(CompiledScalarExpr::If {
                condition: Box::new(self.compile_current_judgment_expr(
                    derived_name,
                    entity,
                    condition,
                )?),
                then_expr: Box::new(self.compile_current_scalar_expr(
                    derived_name,
                    entity,
                    then_expr,
                )?),
                else_expr: Box::new(match else_expr.as_ref() {
                    ScalarExpr::NoMatch { subject, patterns } => CompiledScalarExpr::NoMatch {
                        subject: Box::new(self.compile_current_scalar_expr(
                            derived_name,
                            entity,
                            subject,
                        )?),
                        placeholder: Box::new(self.compile_current_scalar_expr(
                            derived_name,
                            entity,
                            then_expr,
                        )?),
                        labels: self.match_labels(derived_name, subject, patterns),
                    },
                    else_expr => {
                        self.compile_current_scalar_expr(derived_name, entity, else_expr)?
                    }
                }),
            }),
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_IF.to_string(),
            )),
            ScalarExpr::OnlyRelated { .. }
            | ScalarExpr::CountRelated { .. }
            | ScalarExpr::SumRelated { .. } => Err(DenseCompileError::Unsupported(
                "current-entity derived relation predicates cannot aggregate another relation"
                    .to_string(),
            )),
            ScalarExpr::ExtremeRelated {
                extreme,
                relation,
                where_clause,
                ..
            } => Err(DenseCompileError::Unsupported(format!(
                "current-entity derived relation predicates cannot use {} over relation `{relation}`",
                extreme.call_name(where_clause.is_some())
            ))),
            ScalarExpr::SingleRelated { relation, .. } => {
                Err(DenseCompileError::Unsupported(format!(
                    "current-entity derived relation predicates cannot use only over relation `{relation}`"
                )))
            }
            ScalarExpr::OverPeriods { kind, .. } => Err(DenseCompileError::Unsupported(format!(
                "over-periods reduction `{}` nested inside a current-entity related expression",
                kind.as_call_name()
            ))),
        }
    }

    fn compile_current_judgment_expr(
        &mut self,
        derived_name: &str,
        entity: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledJudgmentExpr, DenseCompileError> {
        self.inlined(|compiler| {
            compiler.compile_current_judgment_expr_node(derived_name, entity, expr)
        })
    }

    fn compile_current_judgment_expr_node(
        &mut self,
        derived_name: &str,
        entity: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledJudgmentExpr, DenseCompileError> {
        match expr {
            JudgmentExpr::RoleDerived { term, name } => Err(relation_role_unsupported(*term, name)),
            JudgmentExpr::RelationAtom { relation, .. } => Err(relation_atom_unsupported(relation)),
            JudgmentExpr::Exists { .. } => Err(exists_unsupported()),
            JudgmentExpr::SameEntity { .. } => Err(term_comparison_unsupported()),
            JudgmentExpr::Comparison { left, op, right } => Ok(CompiledJudgmentExpr::Comparison {
                left: self.compile_current_scalar_expr(derived_name, entity, left)?,
                op: *op,
                right: self.compile_current_scalar_expr(derived_name, entity, right)?,
            }),
            JudgmentExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown judgment dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != entity
                    && dependency.entity != self.root_entity
                    && dependency.entity != SCALAR_ENTITY
                {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    });
                }
                let (semantics, commencement) = inlined_rule_semantics(dependency)?;
                let compiled = match semantics {
                    DerivedSemantics::Judgment(expr) => {
                        self.inline_current_judgment(name, &dependency.entity, expr)
                    }
                    DerivedSemantics::Scalar(_) => Err(DenseCompileError::Unsupported(format!(
                        "judgment expression cannot reference scalar derived value (`{name}`)"
                    ))),
                }?;
                Ok(match commencement {
                    Some(rule) => CompiledJudgmentExpr::Commenced {
                        rule,
                        value: Box::new(compiled),
                    },
                    None => compiled,
                })
            }
            JudgmentExpr::RelationMember { relation, .. } => Err(DenseCompileError::Unsupported(
                format!("current-entity relation predicate `{relation}`"),
            )),
            JudgmentExpr::And(items) => Ok(CompiledJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_current_judgment_expr(derived_name, entity, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_current_judgment_expr(derived_name, entity, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledJudgmentExpr::Not(Box::new(
                self.compile_current_judgment_expr(derived_name, entity, item)?,
            ))),
        }
    }

    fn root_input(&mut self, name: &str) -> usize {
        if let Some(&index) = self.root_input_index.get(name) {
            return index;
        }
        let index = self.root_inputs.len();
        self.root_inputs.push(name.to_string());
        self.root_input_index.insert(name.to_string(), index);
        index
    }

    fn relation(
        &mut self,
        name: &str,
        current_slot: usize,
        related_slot: usize,
    ) -> Result<usize, DenseCompileError> {
        let lookup_key = DenseRelationKey {
            name: name.to_string(),
            current_slot,
            related_slot,
        };
        if let Some(&index) = self.relation_index.get(&lookup_key) {
            return Ok(index);
        }
        let program = self.program;
        let Some(derivation) = program
            .relations
            .get(name)
            .and_then(|relation| relation.derivation.as_ref())
        else {
            let index = self.relations.len();
            self.relations.push(DenseRelationSchema {
                key: lookup_key.clone(),
                related_inputs: Vec::new(),
                current_entity: None,
                related_entity: None,
                parent_relation: None,
                filter: None,
            });
            self.relation_index.insert(lookup_key, index);
            return Ok(index);
        };

        // Down the chain of source relations, collect the derived links not
        // compiled yet, with a loop rather than one call per link. A link's
        // source is looked up at the slots the link's derivation reads.
        let mut links = vec![(lookup_key, derivation)];
        let mut on_chain = HashSet::from([name]);
        let mut parent = None;
        loop {
            let (key, derivation) = links.last().expect("the chain starts with one link");
            if derivation.source_path.is_some() {
                return Err(path_source_unsupported(&key.name));
            }
            let source = derivation
                .source_relation
                .as_deref()
                .expect("validated single source");
            let Some(source_derivation) = program
                .relations
                .get(source)
                .and_then(|relation| relation.derivation.as_ref())
            else {
                break;
            };
            let source_key = DenseRelationKey {
                name: source.to_string(),
                current_slot: derivation.current_slot,
                related_slot: derivation.related_slot,
            };
            if let Some(&index) = self.relation_index.get(&source_key) {
                parent = Some(index);
                break;
            }
            if !on_chain.insert(source) {
                return Err(cyclic_dependency(source));
            }
            links.push((source_key, source_derivation));
        }

        // Compile the deepest link first, as each link's parent must exist
        // before it: push its schema, register it, then compile its filter.
        for (lookup_key, derivation) in links.into_iter().rev() {
            let key = parent.map_or_else(
                || DenseRelationKey {
                    name: derivation
                        .source_relation
                        .clone()
                        .expect("validated single source"),
                    current_slot: derivation.current_slot,
                    related_slot: derivation.related_slot,
                },
                |parent: usize| self.relations[parent].key.clone(),
            );
            let index = self.relations.len();
            self.relations.push(DenseRelationSchema {
                key,
                related_inputs: Vec::new(),
                current_entity: derivation
                    .slot_entities
                    .get(derivation.current_slot)
                    .cloned(),
                related_entity: derivation
                    .slot_entities
                    .get(derivation.related_slot)
                    .cloned(),
                parent_relation: parent,
                filter: None,
            });
            let scope = self.membership_scope.replace(lookup_key.name.clone());
            self.relation_index.insert(lookup_key, index);
            // The predicate is compiled once per relation, so a `match` in
            // it is named by whichever rule evaluates the relation.
            self.related_rules.push(String::new());
            let reads = self.inline_reads.len();
            let filter = self.compile_related_predicate(index, &derivation.predicate);
            self.membership_scope = scope;
            self.related_rules.pop();
            self.relations[index].filter = Some(filter?);
            // The relation stays registered, so a retry after a deferral finds
            // it and does not compile its predicate again: the predicate keeps
            // the reads it made, as a recorded body does.
            self.inline_reads.truncate(reads);
            parent = Some(index);
        }
        Ok(parent.expect("the chain compiled at least one link"))
    }

    /// Whether a rule that a related expression references is evaluated on the
    /// current record (compiled for the root row and projected to its related
    /// rows) rather than inlined on the related rows.
    ///
    /// Explain reads a referenced rule's declared entity only in a derived
    /// relation's own predicate, where it selects the current record when the
    /// entity is the relation's current slot entity and the related record
    /// otherwise (`RelationEvalContext::entity_id_for`, falling back to the
    /// related id). A `where` clause, a `sum`'s value and every rule body run
    /// with no relation context, so explain evaluates each rule they reference
    /// for the related entity, whatever entity the rule declares: a
    /// `Household` rule in a `where` clause over household members is
    /// evaluated for each member, and a `Person` rule in a `where` clause over
    /// a person-to-person relation is evaluated for each related person, not
    /// for the root person.
    ///
    /// In a predicate of a derived relation whose related slot entity is
    /// declared, a rule of an entity that is neither slot's (other than an
    /// entity-free `Scalar` rule) is declined rather than evaluated on the
    /// related record as explain's fallback does.
    fn reads_current_record(
        &self,
        relation_index: usize,
        kind: &str,
        name: &str,
        entity: &str,
    ) -> Result<bool, DenseCompileError> {
        if self.membership_scope.is_none() {
            return Ok(false);
        }
        let relation = &self.relations[relation_index];
        if relation.current_entity.as_deref() == Some(entity) {
            return Ok(true);
        }
        if entity != SCALAR_ENTITY
            && relation
                .related_entity
                .as_deref()
                .is_some_and(|related| related != entity)
        {
            return Err(DenseCompileError::Unsupported(format!(
                "related {kind} `{name}` has entity `{entity}`, which is neither current nor related for relation `{}`",
                relation.key.name
            )));
        }
        Ok(false)
    }

    /// Whether dense accepts a `relation_member` of `relation` (with these
    /// slots) in the predicate of derived relation `scope`, as a test that
    /// holds for every tuple `scope` filters. Explain filters `scope`'s
    /// source's related entities for its slots (engine.rs
    /// `related_entity_ids`), and a source that is itself derived filters its
    /// own source the same way, so each relation up that chain, tested with
    /// the slots the chain reads it with, contains every candidate. That holds
    /// because a dense batch keys a derived relation to its base relation and
    /// so carries no tuples named by a derived relation itself. This is a
    /// sufficient condition, not the only case that holds: a derived ancestor
    /// read with other slots holds too, through explain ignoring the requested
    /// slots of a derived relation, and dense declines it rather than rely on
    /// that.
    ///
    /// The source forest is indexed once. A query checks the target's direct
    /// child whose subtree contains `scope`, then that child's source slots.
    /// This takes constant work per query on chains, including when every
    /// link tests a different ancestor, and retains only linear forest state.
    fn holds_for_every_candidate(
        &mut self,
        scope: &str,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
    ) -> bool {
        let key = (
            scope.to_string(),
            relation.to_string(),
            current_slot,
            related_slot,
        );
        if let Some(&holds) = self.candidate_membership.get(&key) {
            return holds;
        }
        let ancestry = self
            .candidate_ancestry
            .get_or_insert_with(|| CandidateAncestry::new(self.program));
        let holds = ancestry.holds(scope, relation, current_slot, related_slot);
        self.candidate_membership.insert(key, holds);
        holds
    }

    fn related_input(&mut self, relation: usize, name: &str) -> usize {
        if let Some(&index) = self.relation_input_index.get(&(relation, name.to_string())) {
            return index;
        }
        let index = self.relations[relation].related_inputs.len();
        self.relations[relation]
            .related_inputs
            .push(name.to_string());
        self.relation_input_index
            .insert((relation, name.to_string()), index);
        index
    }

    fn parameter(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        if let Some(&index) = self.parameter_index.get(name) {
            return Ok(index);
        }
        let parameter =
            self.program.parameters.get(name).ok_or_else(|| {
                DenseCompileError::Unsupported(format!("unknown parameter `{name}`"))
            })?;
        let index = self.parameters.len();
        self.parameters.push(CompiledParameter::new(parameter));
        self.parameter_index.insert(name.to_string(), index);
        Ok(index)
    }
}

// ---------------------------------------------------------------------------
// Lazy columnar evaluation
//
// Every node is evaluated under a `RowMask` of the rows whose reference
// (explain) evaluation reaches it, and errors are recorded per row instead of
// aborting the batch; see `crate::lazy` and `docs/execution-semantics.md`. A
// node under an empty mask does no per-row work and cannot fail, but it still
// produces a column of its usual dtype, so a column's dtype never depends on
// which rows happen to be live.
// ---------------------------------------------------------------------------

/// Values of one node for every row, and the rows whose evaluation failed.
type DenseEval = (DenseColumn, RowErrors);
type JudgmentEval = (Vec<JudgmentOutcome>, RowErrors);

/// A derived rule's rows an executor deferred to its driver instead of
/// recursing into them (see [`crate::depth`]).
#[derive(Clone, Debug)]
struct Deferred {
    derived: usize,
    judgment: bool,
    rows: RowMask,
}

/// Why a column evaluation stopped before producing a column: an error that
/// fails the whole call, or a rule deferred to the driver, which evaluates it
/// and retries. Row-level errors are values in [`RowErrors`], never a `Stop`.
#[derive(Debug)]
enum Stop {
    Error(EvalError),
    Defer(Deferred),
}

impl From<EvalError> for Stop {
    fn from(error: EvalError) -> Self {
        Self::Error(error)
    }
}

type Flow<T> = Result<T, Stop>;

/// An executor that can defer a rule's rows to [`drive`].
trait Deferring {
    /// Start a driver task: no expression levels are on the stack.
    fn reset_depth(&mut self);
    /// Evaluate a deferred rule's rows, from level zero.
    fn run_deferred(&mut self, task: &Deferred) -> Flow<()>;
    /// The expression levels evaluated since the last call (see
    /// [`crate::depth::count_visits`]).
    fn take_visits(&mut self) -> usize;
}

/// Run `root` to completion on `executor`. When it defers a rule (see
/// [`crate::depth`]), evaluate that rule's rows as a task of its own and
/// retry. Tasks form a stack: a task can defer a rule it reaches, which runs
/// first. A rule deferred while a task waiting on it is still open reaches
/// itself; compiled dense programs are acyclic, so that is reported as a cycle
/// rather than deferred forever.
fn drive<X: Deferring, T>(
    executor: &mut X,
    root: impl FnMut(&mut X) -> Flow<T>,
    rule_name: impl Fn(usize) -> String,
) -> Result<T, EvalError> {
    let result = run_tasks(executor, root, rule_name);
    crate::depth::add_visits(executor.take_visits());
    result
}

fn run_tasks<X: Deferring, T>(
    executor: &mut X,
    mut root: impl FnMut(&mut X) -> Flow<T>,
    rule_name: impl Fn(usize) -> String,
) -> Result<T, EvalError> {
    let mut tasks: Vec<Deferred> = Vec::new();
    let mut waiting: HashSet<(usize, bool)> = HashSet::new();
    loop {
        executor.reset_depth();
        let stop = match tasks.last() {
            None => match root(executor) {
                Ok(value) => return Ok(value),
                Err(stop) => stop,
            },
            Some(task) => match executor.run_deferred(task) {
                Ok(()) => {
                    let done = tasks.pop().expect("a task was running");
                    waiting.remove(&(done.derived, done.judgment));
                    continue;
                }
                Err(stop) => stop,
            },
        };
        match stop {
            Stop::Error(error) => return Err(error),
            Stop::Defer(deferred) => {
                if !waiting.insert((deferred.derived, deferred.judgment)) {
                    return Err(EvalError::DependencyCycle(rule_name(deferred.derived)));
                }
                tasks.push(deferred);
            }
        }
    }
}

/// A derived rule's column, computed so far for the `computed` rows.
struct DerivedColumn<T> {
    values: T,
    errors: RowErrors,
    computed: RowMask,
}

/// The rows of `mask` a cached derived column has not computed yet. A rule
/// with no cache entry is computed at least once, possibly under an empty
/// mask, to fix its dtype.
fn pending_rows<T>(
    cached: Option<&DerivedColumn<T>>,
    mask: &RowMask,
) -> Result<RowMask, EvalError> {
    match cached {
        Some(cached) => mask.difference(&cached.computed),
        None => Ok(mask.clone()),
    }
}

/// Whether evaluating a rule computes it: when rows are pending, or when it
/// has never been computed, which it is even under an empty mask to fix its
/// dtype. Deferral must cover both. A rule no row reaches still reads the
/// rules its formula references, so a deep chain behind an untaken branch,
/// or in an empty batch, recursed through every link when only pending rows
/// deferred.
fn computes<T>(cached: Option<&DerivedColumn<T>>, pending: &RowMask) -> bool {
    cached.is_none() || !pending.is_empty()
}

fn merge_scalar<N: DenseNum>(
    cached: Option<DerivedColumn<DenseColumn>>,
    pending: RowMask,
    (values, mut errors): DenseEval,
) -> Result<DerivedColumn<DenseColumn>, EvalError> {
    let Some(cached) = cached else {
        return Ok(DerivedColumn {
            values,
            errors,
            computed: pending,
        });
    };
    let existing = cached.computed.without(&cached.errors)?;
    let added = pending.without(&errors)?;
    let values = merge_dense_rows::<N>(&existing, cached.values, &added, values, &mut errors)?;
    let mut merged_errors = cached.errors;
    merged_errors.absorb(errors);
    Ok(DerivedColumn {
        values,
        errors: merged_errors,
        computed: cached.computed.union(&pending)?,
    })
}

/// Extend a cached column with newly computed rows. Both stretches come from
/// the same formula, so a pair of one dtype keeps it, decimal and `f64`
/// included: a rule that passes a decimal input through stays decimal in
/// `f64` mode however its rows were reached. Only a formula whose dtype
/// depends on which branch its rows took goes through `if`'s combination
/// rule.
fn merge_dense_rows<N: DenseNum>(
    existing_rows: &RowMask,
    existing: DenseColumn,
    added_rows: &RowMask,
    added: DenseColumn,
    errors: &mut RowErrors,
) -> Result<DenseColumn, EvalError> {
    Ok(match (existing, added) {
        (DenseColumn::Decimal(mut target), DenseColumn::Decimal(source)) => {
            assign_rows(&mut target, source, added_rows);
            DenseColumn::Decimal(target)
        }
        (DenseColumn::Float(mut target), DenseColumn::Float(source)) => {
            assign_rows(&mut target, source, added_rows);
            DenseColumn::Float(target)
        }
        (existing, added) => {
            return select_dense::<N>(existing_rows, existing, added_rows, added, errors);
        }
    })
}

fn merge_judgment<T: Copy>(
    cached: Option<DerivedColumn<Vec<T>>>,
    pending: RowMask,
    (values, errors): (Vec<T>, RowErrors),
) -> Result<DerivedColumn<Vec<T>>, EvalError> {
    let Some(mut cached) = cached else {
        return Ok(DerivedColumn {
            values,
            errors,
            computed: pending,
        });
    };
    for row in pending.without(&errors)?.rows() {
        cached.values[row] = values[row];
    }
    cached.errors.absorb(errors);
    cached.computed = cached.computed.union(&pending)?;
    Ok(cached)
}

/// The rows of `mask` of a shared inlined body's column, cached per row as
/// [`DenseExecutor::evaluate_scalar`] caches a rule's: `compute` evaluates
/// only the rows of `mask` not computed yet (under an empty mask on the first
/// read, to fix the dtype), and `merge` adds them to the cache. A deferral
/// leaves the cache as it was, so the retry extends it. The column returned
/// is a `copy` of the cache's, which reserves its rows fallibly, as
/// `evaluate_scalar` and `evaluate_judgment` copy a rule's.
fn cached_rows<T>(
    cache: &mut Option<DerivedColumn<T>>,
    mask: &RowMask,
    compute: impl FnOnce(&RowMask) -> Flow<(T, RowErrors)>,
    merge: impl FnOnce(
        Option<DerivedColumn<T>>,
        RowMask,
        (T, RowErrors),
    ) -> Result<DerivedColumn<T>, EvalError>,
    copy: impl FnOnce(&T) -> Result<T, EvalError>,
) -> Flow<(T, RowErrors)> {
    let pending = pending_rows(cache.as_ref(), mask)?;
    if cache.is_none() || !pending.is_empty() {
        let computed = compute(&pending)?;
        *cache = Some(merge(cache.take(), pending, computed)?);
    }
    let cached = cache.as_ref().expect("computed above");
    Ok((copy(&cached.values)?, cached.errors.restricted_to(mask)))
}

/// Per-executor columns of the inlined bodies read by more than one node
/// (see [`InlineBody`]), computed so far.
struct InlineCaches {
    scalars: Vec<Option<DerivedColumn<DenseColumn>>>,
    judgments: Vec<Option<DerivedColumn<Vec<JudgmentOutcome>>>>,
    related_scalars: Vec<Option<DerivedColumn<DenseColumn>>>,
    related_judgments: Vec<Option<DerivedColumn<Vec<bool>>>>,
}

impl InlineCaches {
    fn new(program: &DenseCompiledProgram) -> Self {
        fn empty<T>(len: usize) -> Vec<Option<T>> {
            (0..len).map(|_| None).collect()
        }
        Self {
            scalars: empty(program.inline_scalars.len()),
            judgments: empty(program.inline_judgments.len()),
            related_scalars: empty(program.related_inline_scalars.len()),
            related_judgments: empty(program.related_inline_judgments.len()),
        }
    }
}

/// A column of placeholders of `N`'s dtype with `error` on every row of
/// `mask`. The column is allocated before any row's error is recorded, so a
/// count no buffer fits fails before a pass over its rows.
fn fail_numeric<N: DenseNum>(
    len: usize,
    mask: &RowMask,
    error: EvalError,
) -> Result<DenseEval, EvalError> {
    Ok(fail_all(
        N::into_column(filled_rows(len, N::ZERO)?),
        mask,
        error,
    ))
}

fn fail_all(placeholder: DenseColumn, mask: &RowMask, error: EvalError) -> DenseEval {
    let mut errors = RowErrors::new();
    errors.record_all(mask, &error);
    (placeholder, errors)
}

/// A scalar rule read as a judgment: placeholders, and the error on every
/// row of `mask`.
fn expected_judgment(len: usize, mask: &RowMask, rule: &str) -> Result<JudgmentEval, EvalError> {
    let placeholder = filled_rows(len, JudgmentOutcome::NotHolds)?;
    let mut errors = RowErrors::new();
    errors.record_all(mask, &EvalError::ExpectedJudgment(rule.to_string()));
    Ok((placeholder, errors))
}

/// The rows a rule's formula is evaluated for: `mask`, or none before the
/// rule commences, when the formula is only typed.
fn commenced_rows(mask: &RowMask, not_commenced: Option<&EvalError>) -> RowMask {
    match not_commenced {
        Some(_) => RowMask::none(mask.len()),
        None => mask.clone(),
    }
}

/// The commencement error on every row of `mask`. Recorded only after the
/// formula has been typed, which allocates its column, so a count no buffer
/// fits fails before a pass over its rows.
fn not_commenced_errors(mask: &RowMask, not_commenced: Option<&EvalError>) -> RowErrors {
    let mut errors = RowErrors::new();
    if let Some(error) = not_commenced {
        errors.record_all(mask, error);
    }
    errors
}

/// The request's error: the lowest failing row, and for that row the first
/// failing output in request order, as the explain path reports it.
fn first_row_error<'e>(outputs: impl Iterator<Item = &'e RowErrors>) -> Result<(), EvalError> {
    let mut first: Option<(usize, &RowError)> = None;
    for errors in outputs {
        if let Some((row, error)) = errors.first()
            && first.is_none_or(|(best, _)| row < best)
        {
            first = Some((row, error));
        }
    }
    match first {
        // A quoted error's message is built only here, for the one error
        // reported, and fallibly.
        Some((_, error)) => Err(error.to_eval()),
        None => Ok(()),
    }
}

/// A columnar evaluator over one row axis: a batch's root rows, one
/// relation's related rows, or a lifetime executor's rows. The arithmetic
/// every axis shares is written once against this trait.
trait ColumnAxis<N: DenseNum, E> {
    fn axis_len(&self) -> usize;
    fn eval_column(&mut self, expr: &E, mask: &RowMask) -> Flow<DenseEval>;
}

/// Judgment evaluation over root or lifetime rows, for the shared
/// short-circuit of `and`/`or`.
trait JudgmentAxis {
    fn judgment_len(&self) -> usize;
    fn eval_judgment_column(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Flow<JudgmentEval>;
}

/// Evaluate `expr` for the rows of `mask` and read it as numbers. Returns the
/// numbers and the rows still live; failures land in `errors`.
fn numeric_operand<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    expr: &E,
    mask: &RowMask,
    errors: &mut RowErrors,
) -> Flow<(Vec<N>, RowMask)> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let mut live = mask.without(&operand_errors)?;
    errors.absorb(operand_errors);
    let mut conversion = RowErrors::new();
    let values = numeric_values::<N>(&column, &live, &mut conversion)?;
    if !conversion.is_empty() {
        live = live.without(&conversion)?;
        errors.absorb(conversion);
    }
    Ok((values, live))
}

fn date_operand<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    expr: &E,
    mask: &RowMask,
    errors: &mut RowErrors,
    message: &str,
) -> Flow<(Vec<NaiveDate>, RowMask)> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let live = mask.without(&operand_errors)?;
    errors.absorb(operand_errors);
    match column {
        DenseColumn::Date(values) => Ok((values, live)),
        other => {
            let placeholder = filled_rows(other.len(), NaiveDate::default())?;
            let mut wrong = RowErrors::new();
            wrong.record_all(&live, &EvalError::TypeMismatch(message.to_string()));
            let live = live.without(&wrong)?;
            errors.absorb(wrong);
            Ok((placeholder, live))
        }
    }
}

fn index_operand<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    expr: &E,
    mask: &RowMask,
    errors: &mut RowErrors,
    message: &str,
) -> Flow<(Vec<i64>, RowMask)> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let live = mask.without(&operand_errors)?;
    errors.absorb(operand_errors);
    let mut indices = filled_rows(column.len(), 0)?;
    let mut wrong = RowErrors::new();
    for row in live.rows() {
        match index_at(&column, row) {
            Some(index) => indices[row] = index,
            None => wrong.record(row, EvalError::TypeMismatch(message.to_string())),
        }
    }
    let live = live.without(&wrong)?;
    errors.absorb(wrong);
    Ok((indices, live))
}

fn eval_add<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    items: &[E],
    mask: &RowMask,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let mut live = mask.clone();
    let mut total = filled_rows(axis.axis_len(), N::ZERO)?;
    for item in items {
        let (values, next) = numeric_operand::<N, E, A>(axis, item, &live, &mut errors)?;
        let mut overflow = RowErrors::new();
        for row in next.rows() {
            match total[row].try_add(values[row]) {
                Ok(sum) => total[row] = sum,
                Err(error) => overflow.record(row, error.into()),
            }
        }
        live = next.without(&overflow)?;
        errors.absorb(overflow);
    }
    Ok((N::into_column(total), errors))
}

fn eval_binary<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    left: &E,
    right: &E,
    mask: &RowMask,
    operation: impl Fn(N, N) -> Result<N, ArithmeticError>,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let (left, live) = numeric_operand::<N, E, A>(axis, left, mask, &mut errors)?;
    let (right, live) = numeric_operand::<N, E, A>(axis, right, &live, &mut errors)?;
    let mut values = filled_rows(axis.axis_len(), N::ZERO)?;
    for row in live.rows() {
        match operation(left[row], right[row]) {
            Ok(value) => values[row] = value,
            Err(error) => errors.record(row, error.into()),
        }
    }
    Ok((N::into_column(values), errors))
}

/// Division. With `divisor_first` (the explain order, used on root and
/// related rows) the divisor is evaluated and checked for zero before the
/// dividend, so a row with a zero divisor never reads the dividend. The
/// lifetime executor, which has no explain counterpart, keeps its
/// dividend-first order (#198).
fn eval_div<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    left: &E,
    right: &E,
    mask: &RowMask,
    divisor_first: bool,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let (dividends, divisors, live) = if divisor_first {
        let (divisors, live) = numeric_operand::<N, E, A>(axis, right, mask, &mut errors)?;
        let mut zero = RowErrors::new();
        for row in live.rows() {
            if divisors[row] == N::ZERO {
                zero.record(row, EvalError::DivisionByZero);
            }
        }
        let live = live.without(&zero)?;
        errors.absorb(zero);
        let (dividends, live) = numeric_operand::<N, E, A>(axis, left, &live, &mut errors)?;
        (dividends, divisors, live)
    } else {
        let (dividends, live) = numeric_operand::<N, E, A>(axis, left, mask, &mut errors)?;
        let (divisors, live) = numeric_operand::<N, E, A>(axis, right, &live, &mut errors)?;
        (dividends, divisors, live)
    };
    let mut quotients = filled_rows(axis.axis_len(), N::ZERO)?;
    for row in live.rows() {
        match dividends[row].try_div(divisors[row]) {
            Ok(quotient) => quotients[row] = quotient,
            Err(error) => errors.record(row, error.into()),
        }
    }
    Ok((N::into_column(quotients), errors))
}

fn eval_extremum<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    items: &[E],
    mask: &RowMask,
    function: &str,
    replaces: impl Fn(N, N) -> bool,
) -> Flow<DenseEval> {
    let Some((first, rest)) = items.split_first() else {
        return Ok(fail_numeric::<N>(
            axis.axis_len(),
            mask,
            EvalError::TypeMismatch(format!("{function}() requires at least one operand")),
        )?);
    };
    let mut errors = RowErrors::new();
    let (mut best, mut live) = numeric_operand::<N, E, A>(axis, first, mask, &mut errors)?;
    for item in rest {
        let (candidates, next) = numeric_operand::<N, E, A>(axis, item, &live, &mut errors)?;
        live = next;
        for row in live.rows() {
            if replaces(candidates[row], best[row]) {
                best[row] = candidates[row];
            }
        }
    }
    Ok((N::into_column(best), errors))
}

fn eval_round<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    value: &E,
    rounding: Rounding,
    mask: &RowMask,
) -> Flow<DenseEval> {
    let (column, mut errors) = axis.eval_column(value, mask)?;
    let live = mask.without(&errors)?;
    let column = round_dense_column::<N>(column, rounding, &live, &mut errors)?;
    Ok((column, errors))
}

fn eval_unary<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    value: &E,
    mask: &RowMask,
    operation: impl Fn(N) -> N,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let (values, live) = numeric_operand::<N, E, A>(axis, value, mask, &mut errors)?;
    let mut result = filled_rows(axis.axis_len(), N::ZERO)?;
    for row in live.rows() {
        result[row] = operation(values[row]);
    }
    Ok((N::into_column(result), errors))
}

/// `date_add_days`/`_months`/`_years`: the date, then the count, then the
/// checked shift, as on the explain path.
fn eval_date_shift<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    date: &E,
    count: &E,
    mask: &RowMask,
    function: &str,
    unit: &str,
    shift: fn(NaiveDate, i64) -> Result<NaiveDate, EvalError>,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let (dates, live) = date_operand::<N, E, A>(
        axis,
        date,
        mask,
        &mut errors,
        &format!("{function} expects a date on the left"),
    )?;
    let (counts, live) = index_operand::<N, E, A>(
        axis,
        count,
        &live,
        &mut errors,
        &format!("{function} expects an integer {unit} count on the right"),
    )?;
    let mut shifted = filled_rows(axis.axis_len(), NaiveDate::default())?;
    for row in live.rows() {
        match shift(dates[row], counts[row]) {
            Ok(date) => shifted[row] = date,
            Err(error) => errors.record(row, error),
        }
    }
    Ok((DenseColumn::Date(shifted), errors))
}

fn eval_days_between<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    from: &E,
    to: &E,
    mask: &RowMask,
) -> Flow<DenseEval> {
    let mut errors = RowErrors::new();
    let (from, live) = date_operand::<N, E, A>(
        axis,
        from,
        mask,
        &mut errors,
        "days_between expects a date for `from`",
    )?;
    let (to, live) = date_operand::<N, E, A>(
        axis,
        to,
        &live,
        &mut errors,
        "days_between expects a date for `to`",
    )?;
    let mut days = filled_rows(axis.axis_len(), 0)?;
    for row in live.rows() {
        days[row] = to[row].signed_duration_since(from[row]).num_days();
    }
    Ok((DenseColumn::Integer(days), errors))
}

/// `and` (`decisive` = not_holds) or `or` (`decisive` = holds): each item is
/// evaluated only for the rows no earlier item decided. A row with an
/// undetermined item and no decisive one is undetermined.
fn eval_short_circuit<A: JudgmentAxis>(
    axis: &mut A,
    items: &[CompiledJudgmentExpr],
    mask: &RowMask,
    decisive: JudgmentOutcome,
) -> Flow<JudgmentEval> {
    let exhausted = match decisive {
        JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
        _ => JudgmentOutcome::NotHolds,
    };
    let len = axis.judgment_len();
    let mut outcomes = filled_rows(len, exhausted)?;
    let mut undetermined = filled_rows(len, false)?;
    let mut errors = RowErrors::new();
    let mut pending = mask.clone();
    for item in items {
        if pending.is_empty() {
            break;
        }
        let (values, item_errors) = axis.eval_judgment_column(item, &pending)?;
        let live = pending.without(&item_errors)?;
        errors.absorb(item_errors);
        pending = live.filter(|row| {
            let value = values[row];
            if value == decisive {
                outcomes[row] = decisive;
                false
            } else {
                if value == JudgmentOutcome::Undetermined {
                    undetermined[row] = true;
                }
                true
            }
        })?;
    }
    for row in pending.rows() {
        if undetermined[row] {
            outcomes[row] = JudgmentOutcome::Undetermined;
        }
    }
    Ok((outcomes, errors))
}

/// Negate in place, so `not` needs no second buffer.
fn negate(mut values: Vec<JudgmentOutcome>) -> Vec<JudgmentOutcome> {
    for value in &mut values {
        *value = match *value {
            JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
            JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
            JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
        };
    }
    values
}

struct DenseExecutor<'a, N: DenseNum> {
    program: &'a DenseCompiledProgram,
    period: &'a Period,
    batch: DenseBoundBatch,
    scalar_cache: Vec<Option<DerivedColumn<DenseColumn>>>,
    judgment_cache: Vec<Option<DerivedColumn<Vec<JudgmentOutcome>>>>,
    /// Each relation's members for the candidates it was last resolved for,
    /// with the root-row errors resolving them found. Aggregations over the
    /// links of one chain share these instead of re-filtering every link
    /// below each.
    relation_members: Vec<Option<ResolvedMembers>>,
    inline_cache: InlineCaches,
    /// Expression levels on the stack since the driver's current task began.
    depth: usize,
    /// The level at which a rule's pending rows are deferred to the driver
    /// (see [`crate::depth`]).
    suspend_depth: usize,
    /// Expression levels evaluated since the driver last reported them (see
    /// [`crate::depth::count_visits`]).
    visits: usize,
    _numeric_mode: std::marker::PhantomData<N>,
}

#[derive(Clone, Debug)]
struct ResolvedMembers {
    candidates: RowMask,
    members: RowMask,
    root_errors: RowErrors,
}

impl<N: DenseNum> Deferring for DenseExecutor<'_, N> {
    fn reset_depth(&mut self) {
        self.depth = 0;
    }

    fn take_visits(&mut self) -> usize {
        std::mem::take(&mut self.visits)
    }

    fn run_deferred(&mut self, task: &Deferred) -> Flow<()> {
        if task.judgment {
            self.evaluate_judgment(task.derived, &task.rows).map(|_| ())
        } else {
            self.evaluate_scalar(task.derived, &task.rows).map(|_| ())
        }
    }
}

impl<N: DenseNum> ColumnAxis<N, CompiledScalarExpr> for DenseExecutor<'_, N> {
    fn axis_len(&self) -> usize {
        self.batch.row_count
    }

    fn eval_column(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        self.eval_scalar_expr(expr, mask)
    }
}

impl<N: DenseNum> JudgmentAxis for DenseExecutor<'_, N> {
    fn judgment_len(&self) -> usize {
        self.batch.row_count
    }

    fn eval_judgment_column(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Flow<JudgmentEval> {
        self.eval_judgment_expr(expr, mask)
    }
}

/// One relation's related rows, as an evaluation axis.
struct RelatedAxis<'x, 'a, N: DenseNum> {
    executor: &'x mut DenseExecutor<'a, N>,
    relation: usize,
}

impl<N: DenseNum> ColumnAxis<N, CompiledRelatedScalarExpr> for RelatedAxis<'_, '_, N> {
    fn axis_len(&self) -> usize {
        self.executor.batch.relations[self.relation].related_count
    }

    fn eval_column(&mut self, expr: &CompiledRelatedScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        self.executor
            .resolve_related_scalar(self.relation, expr, mask)
    }
}

impl<'a, N: DenseNum> DenseExecutor<'a, N> {
    fn new(program: &'a DenseCompiledProgram, period: &'a Period, batch: DenseBoundBatch) -> Self {
        Self {
            program,
            period,
            scalar_cache: (0..program.derived.len()).map(|_| None).collect(),
            judgment_cache: (0..program.derived.len()).map(|_| None).collect(),
            relation_members: vec![None; program.relations.len()],
            inline_cache: InlineCaches::new(program),
            batch,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            visits: 0,
            _numeric_mode: std::marker::PhantomData,
        }
    }

    /// Evaluate an expression outside any rule to completion, deferring as
    /// [`drive`] does. The lifetime executor evaluates a reduction's operand
    /// in each period this way.
    fn drive_expr(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        let program = self.program;
        Ok(drive(
            self,
            |executor| executor.eval_scalar_expr(expr, mask),
            |index| program.derived[index].name.clone(),
        )?)
    }

    /// A derived scalar's column for the rows in `mask`, computing it only
    /// for rows no earlier reference asked for.
    fn evaluate_scalar(&mut self, derived_index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let pending = pending_rows(self.scalar_cache[derived_index].as_ref(), mask)?;
        if computes(self.scalar_cache[derived_index].as_ref(), &pending)
            && self.depth >= self.suspend_depth
        {
            return Err(Stop::Defer(Deferred {
                derived: derived_index,
                judgment: false,
                rows: pending,
            }));
        }
        let cached = match self.scalar_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = match self.compute_scalar(derived_index, &pending) {
                    Ok(computed) => computed,
                    Err(stop) => {
                        // A deferral retries this rule for the same rows;
                        // keep the rows it computed earlier, as recursion
                        // would have, so the retry extends them.
                        self.scalar_cache[derived_index] = cached;
                        return Err(stop);
                    }
                };
                merge_scalar::<N>(cached, pending, computed)?
            }
        };
        let values = copy_column(&cached.values);
        let errors = cached.errors.restricted_to(mask);
        self.scalar_cache[derived_index] = Some(cached);
        Ok((values?, errors))
    }

    fn compute_scalar(&mut self, derived_index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let CompiledSemantics::Scalar(expr) = &derived.semantics else {
            return Ok(fail_numeric::<N>(
                self.batch.row_count,
                mask,
                EvalError::ExpectedScalar(derived.name.clone()),
            )?);
        };
        let not_commenced = self.commencement_error(derived_index);
        let live = commenced_rows(mask, not_commenced.as_ref());
        let (column, formula_errors) = self.eval_scalar_expr(expr, &live)?;
        let mut errors = not_commenced_errors(mask, not_commenced.as_ref());
        errors.absorb(formula_errors.within_rule(&derived.label));
        // Opt-in output rounding, applied before caching so dependents and
        // direct outputs see the same rounded values, as on the explain path.
        // Rounds in the executor's numeric mode (exact for Decimal,
        // best-effort for f64).
        let column = match derived.rounding {
            Some(rounding) => {
                round_dense_column::<N>(column, rounding, &live.without(&errors)?, &mut errors)?
            }
            None => column,
        };
        Ok((column, errors))
    }

    /// A rule before its commencement date has no lawful value (#84): every
    /// row that reaches it fails with this error, and its formula is only
    /// typed, under an empty mask.
    fn commencement_error(&self, derived_index: usize) -> Option<EvalError> {
        let derived = &self.program.derived[derived_index];
        if let Some(effective_from) = derived.effective_from
            && self.period.start < effective_from
        {
            return Some(EvalError::MissingDerivedFormulaVersion {
                derived: derived.name.clone(),
                at: self.period.start,
            });
        }
        None
    }

    fn evaluate_judgment(&mut self, derived_index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let pending = pending_rows(self.judgment_cache[derived_index].as_ref(), mask)?;
        if computes(self.judgment_cache[derived_index].as_ref(), &pending)
            && self.depth >= self.suspend_depth
        {
            return Err(Stop::Defer(Deferred {
                derived: derived_index,
                judgment: true,
                rows: pending,
            }));
        }
        let cached = match self.judgment_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = match self.compute_judgment(derived_index, &pending) {
                    Ok(computed) => computed,
                    Err(stop) => {
                        // A deferral retries this rule for the same rows;
                        // keep the rows it computed earlier, as recursion
                        // would have, so the retry extends them.
                        self.judgment_cache[derived_index] = cached;
                        return Err(stop);
                    }
                };
                merge_judgment(cached, pending, computed)?
            }
        };
        let values = copied_rows(&cached.values);
        let errors = cached.errors.restricted_to(mask);
        self.judgment_cache[derived_index] = Some(cached);
        Ok((values?, errors))
    }

    fn compute_judgment(&mut self, derived_index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let CompiledSemantics::Judgment(expr) = &derived.semantics else {
            return Ok(expected_judgment(
                self.batch.row_count,
                mask,
                &derived.name,
            )?);
        };
        let not_commenced = self.commencement_error(derived_index);
        let live = commenced_rows(mask, not_commenced.as_ref());
        let (values, formula_errors) = self.eval_judgment_expr(expr, &live)?;
        let mut errors = not_commenced_errors(mask, not_commenced.as_ref());
        errors.absorb(formula_errors.within_rule(&derived.label));
        Ok((values, errors))
    }

    /// An inlined body's column for the rows of `mask`. A body read by one
    /// node is evaluated in place; a shared one is computed once per row.
    fn evaluate_inline_scalar(&mut self, index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let program = self.program;
        let inline = &program.inline_scalars[index];
        if inline.references < 2 {
            return self.eval_scalar_expr(&inline.body, mask);
        }
        let mut cache = self.inline_cache.scalars[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.eval_scalar_expr(&inline.body, pending),
            merge_scalar::<N>,
            |values| copy_column(values),
        );
        self.inline_cache.scalars[index] = cache;
        result
    }

    fn evaluate_inline_judgment(&mut self, index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let program = self.program;
        let inline = &program.inline_judgments[index];
        if inline.references < 2 {
            return self.eval_judgment_expr(&inline.body, mask);
        }
        let mut cache = self.inline_cache.judgments[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.eval_judgment_expr(&inline.body, pending),
            merge_judgment,
            |values| copied_rows(values),
        );
        self.inline_cache.judgments[index] = cache;
        result
    }

    /// A related rule's inlined body on `relation`'s related rows, for the
    /// rows of `mask`.
    fn evaluate_related_inline_scalar(
        &mut self,
        relation: usize,
        index: usize,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let program = self.program;
        let inline = &program.related_inline_scalars[index];
        if inline.references < 2 {
            return self.resolve_related_scalar(relation, &inline.body, mask);
        }
        let mut cache = self.inline_cache.related_scalars[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.resolve_related_scalar(relation, &inline.body, pending),
            merge_scalar::<N>,
            |values| copy_column(values),
        );
        self.inline_cache.related_scalars[index] = cache;
        result
    }

    fn evaluate_related_inline_judgment(
        &mut self,
        relation: usize,
        index: usize,
        mask: &RowMask,
    ) -> Flow<(Vec<bool>, RowErrors)> {
        let program = self.program;
        let inline = &program.related_inline_judgments[index];
        if inline.references < 2 {
            return self.eval_related_predicate(relation, &inline.body, mask);
        }
        let mut cache = self.inline_cache.related_judgments[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.eval_related_predicate(relation, &inline.body, pending),
            merge_judgment,
            |values| copied_rows(values),
        );
        self.inline_cache.related_judgments[index] = cache;
        result
    }

    fn eval_scalar_expr(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        self.visits += 1;
        self.depth += 1;
        let result = self.scalar_node(expr, mask);
        self.depth -= 1;
        result
    }

    fn scalar_node(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        let len = self.batch.row_count;
        match expr {
            CompiledScalarExpr::Literal(value) => {
                Ok((broadcast_scalar_literal::<N>(value, len)?, RowErrors::new()))
            }
            CompiledScalarExpr::Input(index) => Ok(match &self.batch.inputs[*index] {
                Some(column) => (copy_column(column)?, RowErrors::new()),
                // An absent column is a missing input on every row; it fails
                // only the rows that read it.
                None => fail_numeric::<N>(
                    len,
                    mask,
                    EvalError::MissingInput {
                        name: self.program.root_inputs[*index].clone(),
                        entity_id: self.program.root_entity.clone(),
                        period_start: self.period.start,
                        period_end: self.period.end,
                    },
                )?,
            }),
            CompiledScalarExpr::InputOrElse { input, default } => Ok((
                match &self.batch.inputs[*input] {
                    Some(column) => copy_column(column)?,
                    None => broadcast_scalar_literal::<N>(default, len)?,
                },
                RowErrors::new(),
            )),
            CompiledScalarExpr::Derived(index) => self.evaluate_scalar(*index, mask),
            CompiledScalarExpr::Inline(index) => self.evaluate_inline_scalar(*index, mask),
            CompiledScalarExpr::Commenced { rule, value } => {
                let (live, mut errors) = rule.rows(self.period, mask);
                let (values, formula_errors) = self.eval_scalar_expr(value, &live)?;
                errors.absorb(formula_errors);
                Ok((values, errors))
            }
            CompiledScalarExpr::Round { value, rounding } => {
                eval_round::<N, _, _>(self, value.as_ref(), *rounding, mask)
            }
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                let (keys, mut errors) = self.eval_scalar_expr(index, mask)?;
                let live = mask.without(&errors)?;
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter],
                    &keys,
                    &live,
                    self.period,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
            CompiledScalarExpr::Add(items) => eval_add::<N, _, _>(self, items, mask),
            CompiledScalarExpr::Sub(left, right) => {
                eval_binary::<N, _, _>(self, left, right, mask, N::try_sub)
            }
            CompiledScalarExpr::Mul(left, right) => {
                eval_binary::<N, _, _>(self, left, right, mask, N::try_mul)
            }
            CompiledScalarExpr::Div(left, right) => {
                eval_div::<N, _, _>(self, left, right, mask, true)
            }
            CompiledScalarExpr::Max(items) => {
                eval_extremum::<N, _, _>(self, items, mask, "max", |candidate, best| {
                    candidate > best
                })
            }
            CompiledScalarExpr::Min(items) => {
                eval_extremum::<N, _, _>(self, items, mask, "min", |candidate, best| {
                    candidate < best
                })
            }
            CompiledScalarExpr::Ceil(value) => {
                eval_unary::<N, _, _>(self, value, mask, |value| value.ceil())
            }
            CompiledScalarExpr::Floor(value) => {
                eval_unary::<N, _, _>(self, value, mask, |value| value.floor())
            }
            CompiledScalarExpr::PeriodStart => Ok((
                DenseColumn::Date(filled_rows(len, self.period.start)?),
                RowErrors::new(),
            )),
            CompiledScalarExpr::PeriodEnd => Ok((
                DenseColumn::Date(filled_rows(len, self.period.end)?),
                RowErrors::new(),
            )),
            CompiledScalarExpr::DateAddDays { date, days } => eval_date_shift::<N, _, _>(
                self,
                date,
                days,
                mask,
                "date_add_days",
                "day",
                crate::engine::shift_calendar_days,
            ),
            CompiledScalarExpr::DateAddMonths { date, months } => eval_date_shift::<N, _, _>(
                self,
                date,
                months,
                mask,
                "date_add_months",
                "month",
                crate::engine::shift_calendar_months,
            ),
            CompiledScalarExpr::DateAddYears { date, years } => eval_date_shift::<N, _, _>(
                self,
                date,
                years,
                mask,
                "date_add_years",
                "year",
                crate::engine::shift_calendar_years,
            ),
            CompiledScalarExpr::DaysBetween { from, to } => {
                eval_days_between::<N, _, _>(self, from, to, mask)
            }
            CompiledScalarExpr::OnlyRelated {
                relation,
                relation_name,
                input,
            } => self.eval_only_related((*relation, relation_name), *input, mask),
            CompiledScalarExpr::CountRelated {
                relation,
                predicate,
            } => self.eval_count_related(*relation, predicate.as_ref(), mask),
            CompiledScalarExpr::SumRelated {
                relation,
                value,
                predicate,
            } => self.eval_sum_related(*relation, value, predicate.as_ref(), mask),
            CompiledScalarExpr::ExtremeRelated {
                extreme,
                relation,
                relation_name,
                value,
                predicate,
            } => self.eval_extreme_related(
                *extreme,
                (*relation, relation_name),
                value,
                predicate.as_ref(),
                mask,
            ),
            CompiledScalarExpr::SingleRelated {
                relation,
                relation_name,
                value,
                predicate,
            } => self.eval_single_related(
                (*relation, relation_name),
                value,
                predicate.as_ref(),
                mask,
            ),
            CompiledScalarExpr::NoMatch {
                subject,
                placeholder,
                labels,
            } => {
                let (placeholder, _) =
                    self.eval_scalar_expr(placeholder, &RowMask::none(mask.len()))?;
                let (subject, mut errors) = self.eval_scalar_expr(subject, mask)?;
                labels.record_failures(&subject, &mask.without(&errors)?, &mut errors);
                Ok((placeholder, errors))
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) = self.eval_judgment_expr(condition, mask)?;
                let live = mask.without(&errors)?;
                // An undetermined condition takes the else branch.
                let then_rows = live.filter(|row| condition[row].is_holds())?;
                let else_rows = live.difference(&then_rows)?;
                let (then_values, then_errors) = self.eval_scalar_expr(then_expr, &then_rows)?;
                let (else_values, else_errors) = self.eval_scalar_expr(else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors)?;
                let else_live = else_rows.without(&else_errors)?;
                errors.absorb(then_errors);
                errors.absorb(else_errors);
                let column = select_dense::<N>(
                    &then_live,
                    then_values,
                    &else_live,
                    else_values,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
            // Cross-period reductions require a batch per period; they are
            // evaluated by the lifetime executor, never here.
            CompiledScalarExpr::OverPeriods { kind, .. } => Ok(fail_numeric::<N>(
                len,
                mask,
                EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()),
            )?),
        }
    }

    /// Related rows owned by the root rows of `mask`.
    fn related_rows_of(&self, relation: usize, mask: &RowMask) -> Result<RowMask, EvalError> {
        let batch = &self.batch.relations[relation];
        if mask.is_all() {
            return Ok(RowMask::all(batch.related_count));
        }
        if mask.is_empty() {
            return Ok(RowMask::none(batch.related_count));
        }
        let mut bits = filled_rows(batch.related_count, false)?;
        for row in mask.rows() {
            bits[batch.offsets[row]..batch.offsets[row + 1]].fill(true);
        }
        Ok(RowMask::from_bits(bits))
    }

    /// Root rows owning at least one related row of `related`.
    fn root_rows_of(&self, relation: usize, related: &RowMask) -> Result<RowMask, EvalError> {
        if related.is_empty() {
            return Ok(RowMask::none(self.batch.row_count));
        }
        let owners = &self.batch.relations[relation].owners;
        let mut bits = filled_rows(self.batch.row_count, false)?;
        for related in related.rows() {
            bits[owners[related]] = true;
        }
        Ok(RowMask::from_bits(bits))
    }

    /// Related-row errors lifted to the root rows that own them. A root row
    /// takes its first failing related row's error.
    fn lift_errors(&self, relation: usize, related_errors: &RowErrors) -> RowErrors {
        let owners = &self.batch.relations[relation].owners;
        let mut errors = RowErrors::new();
        for (related, error) in related_errors.iter() {
            errors.record_error(owners[related], error.clone());
        }
        errors
    }

    /// Root-row errors pushed down to the related rows of `related` they own.
    fn lower_errors(
        &self,
        relation: usize,
        root_errors: &RowErrors,
        related: &RowMask,
    ) -> RowErrors {
        let mut errors = RowErrors::new();
        if root_errors.is_empty() {
            return errors;
        }
        let owners = &self.batch.relations[relation].owners;
        for related in related.rows() {
            if let Some(error) = root_errors.get(owners[related]) {
                errors.record_error(related, error.clone());
            }
        }
        errors
    }

    /// Drop the related rows whose root row already failed: the reference
    /// evaluation of that row stopped there.
    fn without_failed_roots(
        &self,
        relation: usize,
        related: &RowMask,
        root_errors: &RowErrors,
    ) -> Result<RowMask, EvalError> {
        if root_errors.is_empty() {
            return Ok(related.clone());
        }
        let owners = &self.batch.relations[relation].owners;
        related.filter(|related| !root_errors.contains(owners[related]))
    }

    /// The members of `relation` among `candidates`, and the root-row errors
    /// found resolving them. A derived relation's parent is resolved first,
    /// then its own filter runs on the parent's members. The chain of parents
    /// is walked by a loop, deepest link first, and stops early at a link
    /// already resolved for the same candidates.
    fn relation_members(
        &mut self,
        relation: usize,
        candidates: &RowMask,
    ) -> Flow<(RowMask, RowErrors)> {
        let program = self.program;
        let resolved_for = |resolved: &Option<ResolvedMembers>| {
            resolved
                .as_ref()
                .is_some_and(|resolved| resolved.candidates.same_rows(candidates))
        };
        let mut chain = Vec::new();
        let mut link = Some(relation);
        let mut resolved = None;
        while let Some(current) = link {
            if resolved_for(&self.relation_members[current]) {
                resolved = self.relation_members[current].clone();
                break;
            }
            chain.push(current);
            link = program.relations[current].parent_relation;
        }
        let (mut members, mut root_errors) = resolved.map_or_else(
            || (candidates.clone(), RowErrors::new()),
            |resolved| (resolved.members, resolved.root_errors),
        );
        for link in chain.into_iter().rev() {
            let schema = &program.relations[link];
            if schema.parent_relation.is_some() {
                members = self.without_failed_roots(link, &members, &root_errors)?;
            }
            if let Some(filter) = &schema.filter
                && !members.is_empty()
            {
                let (holds, filter_errors) = self.eval_related_predicate(link, filter, &members)?;
                let live = members.without(&filter_errors)?;
                root_errors.absorb(self.lift_errors(link, &filter_errors));
                members = live.filter(|related| holds[related])?;
                members = self.without_failed_roots(link, &members, &root_errors)?;
            }
            self.relation_members[link] = Some(ResolvedMembers {
                candidates: candidates.clone(),
                members: members.clone(),
                root_errors: root_errors.clone(),
            });
        }
        Ok((members, root_errors))
    }

    /// Members of `relation` owned by the rows of `mask` that pass the
    /// `where` clause. Returns them, the where clause's related-row errors
    /// and the membership stage's root-row errors.
    fn filtered_members(
        &mut self,
        relation: usize,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Flow<(RowMask, RowErrors, RowErrors)> {
        let candidates = self.related_rows_of(relation, mask)?;
        let (members, root_errors) = self.relation_members(relation, &candidates)?;
        let Some(predicate) = predicate else {
            return Ok((members, RowErrors::new(), root_errors));
        };
        let (holds, where_errors) = self.eval_related_predicate(relation, predicate, &members)?;
        let kept = members
            .without(&where_errors)?
            .filter(|related| holds[related])?;
        Ok((kept, where_errors, root_errors))
    }

    fn eval_only_related(
        &mut self,
        (relation, relation_name): (usize, &str),
        input: usize,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let (members, _, mut errors) = self.filtered_members(relation, None, mask)?;
        let owners = self.batch.relations[relation].owners.clone();
        let mut counts = filled_rows(self.batch.row_count, 0_usize)?;
        for related in members.rows() {
            counts[owners[related]] += 1;
        }
        for row in mask.without(&errors)?.rows() {
            let error = match counts[row] {
                0 => Some(EvalError::MissingRelation {
                    relation: relation_name.to_string(),
                    entity_id: row.to_string(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                }),
                1 => None,
                count => Some(EvalError::AmbiguousRelation {
                    relation: relation_name.to_string(),
                    entity_id: row.to_string(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                    count,
                }),
            };
            if let Some(error) = error {
                errors.record(row, error);
            }
        }
        let selected = members.filter(|related| !errors.contains(owners[related]))?;
        let (values, value_errors) = self.resolve_related_scalar(
            relation,
            &CompiledRelatedScalarExpr::Input(input),
            &selected,
        )?;
        errors.absorb(self.lift_errors(relation, &value_errors));
        let mut result = filled_rows(self.batch.row_count, false)?;
        for related in selected.without(&value_errors)?.rows() {
            let row = owners[related];
            if let DenseColumn::Bool(values) = &values {
                result[row] = values[related];
            } else {
                errors.record(
                    row,
                    EvalError::TypeMismatch("only_related requires a Boolean input".into()),
                );
            }
        }
        Ok((DenseColumn::Bool(result), errors))
    }

    fn eval_count_related(
        &mut self,
        relation: usize,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let (members, where_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        errors.absorb(self.lift_errors(relation, &where_errors));
        let owners = &self.batch.relations[relation].owners;
        let mut counts = filled_rows(self.batch.row_count, 0_usize)?;
        for related in members.rows() {
            counts[owners[related]] += 1;
        }
        // Binding bounds every related count by `MAX_DENSE_ROWS`, far below
        // `i64::MAX`, so no count fails to convert; one that did would be an
        // error on its row, not a clamped count.
        let mut values = filled_rows(counts.len(), 0_i64)?;
        for (row, count) in counts.into_iter().enumerate() {
            match i64::try_from(count) {
                Ok(count) => values[row] = count,
                Err(_) => errors.record(
                    row,
                    EvalError::TypeMismatch(format!(
                        "dense relation count {count} does not fit in an integer column"
                    )),
                ),
            }
        }
        Ok((DenseColumn::Integer(values), errors))
    }

    /// For each member (in batch order) the `where` clause and then the
    /// summed value, so a value is never read for a member the clause
    /// excludes.
    fn eval_sum_related(
        &mut self,
        relation: usize,
        value: &CompiledRelatedScalarExpr,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let (members, mut related_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        let (values, value_errors) = self.resolve_related_scalar(relation, value, &members)?;
        let live = members.without(&value_errors)?;
        related_errors.absorb(value_errors);
        let numbers = numeric_values::<N>(&values, &live, &mut related_errors)?;
        let live = live.without(&related_errors)?;
        // Explain adds each member in turn and stops a root at its first
        // member whose `where` clause or value fails, or whose addition
        // overflows, whichever comes first; relation-filter errors (already in
        // `errors`) come before all of them. So walk the live and failed
        // members together in batch order.
        let owners = &self.batch.relations[relation].owners;
        let mut totals = filled_rows(self.batch.row_count, N::ZERO)?;
        let mut stopped = RowErrors::new();
        let mut failures = related_errors.iter().peekable();
        for related in live.rows() {
            while let Some((failed, error)) = failures.next_if(|(failed, _)| *failed < related) {
                stopped.record_error(owners[failed], error.clone());
            }
            let owner = owners[related];
            if errors.contains(owner) || stopped.contains(owner) {
                continue;
            }
            match totals[owner].try_add(numbers[related]) {
                Ok(total) => totals[owner] = total,
                Err(error) => stopped.record(owner, error.into()),
            }
        }
        for (failed, error) in failures {
            stopped.record_error(owners[failed], error.clone());
        }
        errors.absorb(stopped);
        Ok((N::into_column(totals), errors))
    }

    /// `min`/`max` over each root row's members, walked as `sum` walks them:
    /// for each member (in batch order) the `where` clause and then the value,
    /// so a root fails with its first failing member's error. Numbers compare
    /// in mode `N` and dates chronologically; a boolean or text value is
    /// explain's type error. A root left with no member fails with explain's
    /// empty-aggregate error, which names the root entity kind where explain
    /// names the id (dense rows carry none), as `MissingInput` does.
    fn eval_extreme_related(
        &mut self,
        extreme: Extreme,
        (relation, relation_name): (usize, &str),
        value: &CompiledRelatedScalarExpr,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let function = extreme.call_name(predicate.is_some());
        let (members, mut related_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        let (values, value_errors) = self.resolve_related_scalar(relation, value, &members)?;
        let live = members.without(&value_errors)?;
        related_errors.absorb(value_errors);
        let owners = Rc::clone(&self.batch.relations[relation].owners);
        let row_count = self.batch.row_count;
        let mut extremes = match &values {
            DenseColumn::Date(dates) => Extremes::Dates {
                values: dates,
                best: filled_rows(row_count, None)?,
            },
            DenseColumn::Bool(_) | DenseColumn::Text(_) => {
                let found = if matches!(values, DenseColumn::Bool(_)) {
                    "a boolean"
                } else {
                    "text"
                };
                related_errors.record_all(
                    &live,
                    &extreme_value_type_error(function, relation_name, found),
                );
                Extremes::Numbers {
                    values: Vec::new(),
                    best: filled_rows(row_count, None)?,
                }
            }
            numeric => Extremes::Numbers {
                values: numeric_values::<N>(numeric, &live, &mut related_errors)?,
                best: filled_rows(row_count, None)?,
            },
        };
        // Membership errors (already in `errors`) come before every member's;
        // then a root takes its first failing member's error, in batch order.
        // No member's value can fail the comparison itself, so that order is
        // the whole of explain's.
        for (failed, error) in related_errors.iter() {
            errors.record_error(owners[failed], error.clone());
        }
        let live = live.without(&related_errors)?;
        for related in live.rows() {
            let owner = owners[related];
            if !errors.contains(owner) {
                extremes.take(extreme, owner, related);
            }
        }
        let empty = |errors: &mut RowErrors, row: usize| {
            errors.record(
                row,
                EvalError::EmptyRelatedAggregate {
                    function,
                    relation: relation_name.to_string(),
                    entity_id: self.program.root_entity.clone(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                },
            );
        };
        let column = match extremes {
            Extremes::Numbers { best, .. } => {
                let mut column = filled_rows(row_count, N::ZERO)?;
                for row in mask.without(&errors)?.rows() {
                    match best[row] {
                        Some(value) => column[row] = value,
                        None => empty(&mut errors, row),
                    }
                }
                N::into_column(column)
            }
            Extremes::Dates { best, .. } => {
                let mut column = filled_rows(row_count, NaiveDate::default())?;
                for row in mask.without(&errors)?.rows() {
                    match best[row] {
                        Some(value) => column[row] = value,
                        None => empty(&mut errors, row),
                    }
                }
                DenseColumn::Date(column)
            }
        };
        Ok((column, errors))
    }

    /// `only` over each root row's members: the `where` clause for every
    /// member, then the value of the one member a root selects. A root that
    /// selects none or several fails with explain's error and reads no value;
    /// the error names the row, as `eval_only_related` does, so each of these
    /// errors means one thing in dense whichever expression raised it. The
    /// value keeps its column's type, as an input read does.
    fn eval_single_related(
        &mut self,
        (relation, relation_name): (usize, &str),
        value: &CompiledRelatedScalarExpr,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let (members, where_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        errors.absorb(self.lift_errors(relation, &where_errors));
        let owners = Rc::clone(&self.batch.relations[relation].owners);
        let row_count = self.batch.row_count;
        let mut counts = filled_rows(row_count, 0_usize)?;
        let mut single = filled_rows(row_count, 0_usize)?;
        for related in members.rows() {
            counts[owners[related]] += 1;
            single[owners[related]] = related;
        }
        let mut chosen = filled_rows(self.batch.relations[relation].related_count, false)?;
        for row in mask.without(&errors)?.rows() {
            let error = match counts[row] {
                1 => {
                    chosen[single[row]] = true;
                    continue;
                }
                0 => EvalError::MissingRelation {
                    relation: relation_name.to_string(),
                    entity_id: row.to_string(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                },
                count => EvalError::AmbiguousRelation {
                    relation: relation_name.to_string(),
                    entity_id: row.to_string(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                    count,
                },
            };
            errors.record(row, error);
        }
        let chosen = RowMask::from_bits(chosen);
        let (values, value_errors) = self.resolve_related_scalar(relation, value, &chosen)?;
        let read = chosen.without(&value_errors)?;
        errors.absorb(self.lift_errors(relation, &value_errors));
        Ok((
            gather_to_owners(&values, &owners, &read, row_count)?,
            errors,
        ))
    }

    fn eval_related_predicate(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedJudgmentExpr,
        mask: &RowMask,
    ) -> Flow<(Vec<bool>, RowErrors)> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedJudgmentExpr::Commenced { rule, value } => {
                let (live, mut errors) = rule.rows(self.period, mask);
                let (values, formula_errors) =
                    self.eval_related_predicate(relation, value, &live)?;
                errors.absorb(formula_errors);
                Ok((values, errors))
            }
            CompiledRelatedJudgmentExpr::Literal(value) => {
                Ok((filled_rows(length, *value)?, RowErrors::new()))
            }
            CompiledRelatedJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.resolve_related_scalar(relation, left, mask)?;
                let live = mask.without(&errors)?;
                let (right, right_errors) = self.resolve_related_scalar(relation, right, &live)?;
                let live = live.without(&right_errors)?;
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors)?;
                let mut holds = row_buffer(outcomes.len())?;
                holds.extend(outcomes.iter().map(|outcome| outcome.is_holds()));
                Ok((holds, errors))
            }
            CompiledRelatedJudgmentExpr::Inline(index) => {
                self.evaluate_related_inline_judgment(relation, *index, mask)
            }
            CompiledRelatedJudgmentExpr::RootJudgment(expr) => {
                let roots = self.root_rows_of(relation, mask)?;
                let (values, root_errors) = self.eval_judgment_expr(expr, &roots)?;
                let projected = project_root_judgment_to_related(
                    &values,
                    &self.batch.relations[relation].offsets,
                )?;
                Ok((projected, self.lower_errors(relation, &root_errors, mask)))
            }
            CompiledRelatedJudgmentExpr::RelationMemberWithoutContext { relation: name } => {
                let placeholder = filled_rows(length, false)?;
                let mut errors = RowErrors::new();
                errors.record_all(mask, &relation_member_outside_derived_relation(name));
                Ok((placeholder, errors))
            }
            CompiledRelatedJudgmentExpr::And(items) | CompiledRelatedJudgmentExpr::Or(items) => {
                // Short-circuit per related row: `and` stops at the first
                // false item, `or` at the first true one.
                let decisive = matches!(expr, CompiledRelatedJudgmentExpr::Or(_));
                let mut result = filled_rows(length, !decisive)?;
                let mut errors = RowErrors::new();
                let mut pending = mask.clone();
                for item in items {
                    if pending.is_empty() {
                        break;
                    }
                    let (values, item_errors) =
                        self.eval_related_predicate(relation, item, &pending)?;
                    let live = pending.without(&item_errors)?;
                    errors.absorb(item_errors);
                    pending = live.filter(|related| {
                        if values[related] == decisive {
                            result[related] = decisive;
                            false
                        } else {
                            true
                        }
                    })?;
                }
                Ok((result, errors))
            }
            CompiledRelatedJudgmentExpr::Not(item) => {
                let (mut values, errors) = self.eval_related_predicate(relation, item, mask)?;
                for keep in &mut values {
                    *keep = !*keep;
                }
                Ok((values, errors))
            }
        }
    }

    fn resolve_related_scalar(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedScalarExpr,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedScalarExpr::Commenced { rule, value } => {
                let (live, mut errors) = rule.rows(self.period, mask);
                let (values, formula_errors) =
                    self.resolve_related_scalar(relation, value, &live)?;
                errors.absorb(formula_errors);
                Ok((values, errors))
            }
            CompiledRelatedScalarExpr::Literal(value) => Ok((
                broadcast_scalar_literal::<N>(value, length)?,
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::Input(index) => {
                Ok(match &self.batch.relations[relation].inputs[*index] {
                    Some(column) => (copy_column(column)?, RowErrors::new()),
                    None => {
                        let schema = &self.program.relations[relation];
                        fail_numeric::<N>(
                            length,
                            mask,
                            EvalError::MissingInput {
                                name: schema.related_inputs[*index].clone(),
                                entity_id: schema.key.name.clone(),
                                period_start: self.period.start,
                                period_end: self.period.end,
                            },
                        )?
                    }
                })
            }
            CompiledRelatedScalarExpr::InputOrElse { input, default } => Ok((
                match &self.batch.relations[relation].inputs[*input] {
                    Some(column) => copy_column(column)?,
                    None => broadcast_scalar_literal::<N>(default, length)?,
                },
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::Round { value, rounding } => eval_round::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                value.as_ref(),
                *rounding,
                mask,
            ),
            CompiledRelatedScalarExpr::Inline(index) => {
                self.evaluate_related_inline_scalar(relation, *index, mask)
            }
            CompiledRelatedScalarExpr::RootScalar(expr) => {
                let roots = self.root_rows_of(relation, mask)?;
                let (values, root_errors) = self.eval_scalar_expr(expr, &roots)?;
                let projected = project_root_column_to_related(
                    &values,
                    &self.batch.relations[relation].offsets,
                )?;
                Ok((projected, self.lower_errors(relation, &root_errors, mask)))
            }
            CompiledRelatedScalarExpr::ParameterLookup { parameter, index } => {
                let (keys, mut errors) = self.resolve_related_scalar(relation, index, mask)?;
                let live = mask.without(&errors)?;
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter],
                    &keys,
                    &live,
                    self.period,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
            CompiledRelatedScalarExpr::Add(items) => eval_add::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                items,
                mask,
            ),
            CompiledRelatedScalarExpr::Sub(left, right) => eval_binary::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                left,
                right,
                mask,
                N::try_sub,
            ),
            CompiledRelatedScalarExpr::Mul(left, right) => eval_binary::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                left,
                right,
                mask,
                N::try_mul,
            ),
            CompiledRelatedScalarExpr::Div(left, right) => eval_div::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                left,
                right,
                mask,
                true,
            ),
            CompiledRelatedScalarExpr::Max(items) => eval_extremum::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                items,
                mask,
                "max",
                |candidate, best| candidate > best,
            ),
            CompiledRelatedScalarExpr::Min(items) => eval_extremum::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                items,
                mask,
                "min",
                |candidate, best| candidate < best,
            ),
            CompiledRelatedScalarExpr::Ceil(value) => eval_unary::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                value,
                mask,
                |value| value.ceil(),
            ),
            CompiledRelatedScalarExpr::Floor(value) => eval_unary::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                value,
                mask,
                |value| value.floor(),
            ),
            CompiledRelatedScalarExpr::PeriodStart => Ok((
                DenseColumn::Date(filled_rows(length, self.period.start)?),
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::PeriodEnd => Ok((
                DenseColumn::Date(filled_rows(length, self.period.end)?),
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::DateAddDays { date, days } => eval_date_shift::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                date,
                days,
                mask,
                "date_add_days",
                "day",
                crate::engine::shift_calendar_days,
            ),
            CompiledRelatedScalarExpr::DateAddMonths { date, months } => {
                eval_date_shift::<N, _, _>(
                    &mut RelatedAxis {
                        executor: self,
                        relation,
                    },
                    date,
                    months,
                    mask,
                    "date_add_months",
                    "month",
                    crate::engine::shift_calendar_months,
                )
            }
            CompiledRelatedScalarExpr::DateAddYears { date, years } => eval_date_shift::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                date,
                years,
                mask,
                "date_add_years",
                "year",
                crate::engine::shift_calendar_years,
            ),
            CompiledRelatedScalarExpr::DaysBetween { from, to } => eval_days_between::<N, _, _>(
                &mut RelatedAxis {
                    executor: self,
                    relation,
                },
                from,
                to,
                mask,
            ),
            CompiledRelatedScalarExpr::NoMatch {
                subject,
                placeholder,
                labels,
            } => {
                let (placeholder, _) =
                    self.resolve_related_scalar(relation, placeholder, &RowMask::none(mask.len()))?;
                let (subject, mut errors) = self.resolve_related_scalar(relation, subject, mask)?;
                labels.record_failures(&subject, &mask.without(&errors)?, &mut errors);
                Ok((placeholder, errors))
            }
            CompiledRelatedScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) =
                    self.eval_related_predicate(relation, condition, mask)?;
                let live = mask.without(&errors)?;
                let then_rows = live.filter(|related| condition[related])?;
                let else_rows = live.difference(&then_rows)?;
                let (then_values, then_errors) =
                    self.resolve_related_scalar(relation, then_expr, &then_rows)?;
                let (else_values, else_errors) =
                    self.resolve_related_scalar(relation, else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors)?;
                let else_live = else_rows.without(&else_errors)?;
                errors.absorb(then_errors);
                errors.absorb(else_errors);
                let column = select_dense::<N>(
                    &then_live,
                    then_values,
                    &else_live,
                    else_values,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Flow<JudgmentEval> {
        self.visits += 1;
        self.depth += 1;
        let result = self.judgment_node(expr, mask);
        self.depth -= 1;
        result
    }

    fn judgment_node(&mut self, expr: &CompiledJudgmentExpr, mask: &RowMask) -> Flow<JudgmentEval> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.eval_scalar_expr(left, mask)?;
                let live = mask.without(&errors)?;
                let (right, right_errors) = self.eval_scalar_expr(right, &live)?;
                let live = live.without(&right_errors)?;
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors)?;
                Ok((outcomes, errors))
            }
            CompiledJudgmentExpr::Derived(index) => self.evaluate_judgment(*index, mask),
            CompiledJudgmentExpr::Inline(index) => self.evaluate_inline_judgment(*index, mask),
            CompiledJudgmentExpr::Commenced { rule, value } => {
                let (live, mut errors) = rule.rows(self.period, mask);
                let (values, formula_errors) = self.eval_judgment_expr(value, &live)?;
                errors.absorb(formula_errors);
                Ok((values, errors))
            }
            CompiledJudgmentExpr::And(items) => {
                eval_short_circuit(self, items, mask, JudgmentOutcome::NotHolds)
            }
            CompiledJudgmentExpr::Or(items) => {
                eval_short_circuit(self, items, mask, JudgmentOutcome::Holds)
            }
            CompiledJudgmentExpr::Not(item) => {
                let (values, errors) = self.eval_judgment_expr(item, mask)?;
                Ok((negate(values), errors))
            }
        }
    }
}

/// Executor for the lifetime surface: it owns one bound batch (and one
/// [`DenseExecutor`]) per period, all describing the same entity rows in the
/// same order. It evaluates a formula once, row-wise, collapsing each
/// over-periods reduction to a per-entity column by evaluating the reduction's
/// inner expression across every period's executor and reducing down the period
/// axis. Scalars outside any reduction that need a period (parameters) resolve
/// at the reference period — the chronologically-last supplied period, which
/// the `execute_lifetime` entry points validate the period list to end with. A
/// derived referenced outside a reduction is evaluated by inlining its body in
/// this same lifetime context (so it means exactly its definition), and a bare
/// period-invariant input binds its common value.
///
/// Evaluation is lazy per row, as on the per-period surface: a reduction, a
/// period-invariant input check and a top-N count see only the rows that
/// reach them, and a row stops at its first failing period.
struct LifetimeExecutor<'a, N: DenseNum> {
    program: &'a DenseCompiledProgram,
    /// One per-period executor, index-aligned with `periods`. Each carries its
    /// own per-period scalar/judgment memoization.
    period_executors: Vec<DenseExecutor<'a, N>>,
    /// Index of the reference period — the chronologically-last supplied period
    /// (the entry points validate the list is strictly ascending, so this is the
    /// final index) — for period-specific scalars combined with a reduction.
    reference_period: usize,
    row_count: usize,
    /// Lifetime-level memoization of derived values (keyed by derived index),
    /// so a derived referenced from several places reduces once. Rounding is
    /// applied before caching, mirroring the per-period path.
    scalar_cache: Vec<Option<DerivedColumn<DenseColumn>>>,
    judgment_cache: Vec<Option<DerivedColumn<Vec<JudgmentOutcome>>>>,
    /// Lifetime-level columns of shared inlined bodies. Only an aggregation
    /// or a relation's predicate reads an inlined body, and the lifetime
    /// executor evaluates those in its per-period executors, so these stay
    /// empty in practice.
    inline_cache: InlineCaches,
    /// Expression levels on the stack since the driver's current task began.
    depth: usize,
    /// The level at which a rule's pending rows are deferred to the driver
    /// (see [`crate::depth`]).
    suspend_depth: usize,
    /// Expression levels evaluated since the driver last reported them (see
    /// [`crate::depth::count_visits`]).
    visits: usize,
}

impl<N: DenseNum> Deferring for LifetimeExecutor<'_, N> {
    fn reset_depth(&mut self) {
        self.depth = 0;
    }

    fn take_visits(&mut self) -> usize {
        std::mem::take(&mut self.visits)
    }

    fn run_deferred(&mut self, task: &Deferred) -> Flow<()> {
        if task.judgment {
            self.evaluate_judgment(task.derived, &task.rows).map(|_| ())
        } else {
            self.evaluate_scalar(task.derived, &task.rows).map(|_| ())
        }
    }
}

impl<N: DenseNum> ColumnAxis<N, CompiledScalarExpr> for LifetimeExecutor<'_, N> {
    fn axis_len(&self) -> usize {
        self.row_count
    }

    fn eval_column(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        self.eval_scalar(expr, mask)
    }
}

impl<N: DenseNum> JudgmentAxis for LifetimeExecutor<'_, N> {
    fn judgment_len(&self) -> usize {
        self.row_count
    }

    fn eval_judgment_column(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Flow<JudgmentEval> {
        self.eval_judgment(expr, mask)
    }
}

impl<'a, N: DenseNum> LifetimeExecutor<'a, N> {
    fn new(
        program: &'a DenseCompiledProgram,
        periods: &'a [Period],
        bound: Vec<DenseBoundBatch>,
        row_count: usize,
    ) -> Self {
        let period_executors = periods
            .iter()
            .zip(bound)
            .map(|(period, batch)| DenseExecutor::new(program, period, batch))
            .collect();
        Self {
            program,
            period_executors,
            reference_period: periods.len() - 1,
            row_count,
            scalar_cache: (0..program.derived.len()).map(|_| None).collect(),
            judgment_cache: (0..program.derived.len()).map(|_| None).collect(),
            inline_cache: InlineCaches::new(program),
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            visits: 0,
        }
    }

    fn evaluate_scalar(&mut self, derived_index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let pending = pending_rows(self.scalar_cache[derived_index].as_ref(), mask)?;
        if computes(self.scalar_cache[derived_index].as_ref(), &pending)
            && self.depth >= self.suspend_depth
        {
            return Err(Stop::Defer(Deferred {
                derived: derived_index,
                judgment: false,
                rows: pending,
            }));
        }
        let cached = match self.scalar_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = match self.compute_scalar(derived_index, &pending) {
                    Ok(computed) => computed,
                    Err(stop) => {
                        // A deferral retries this rule for the same rows;
                        // keep the rows it computed earlier, as recursion
                        // would have, so the retry extends them.
                        self.scalar_cache[derived_index] = cached;
                        return Err(stop);
                    }
                };
                merge_scalar::<N>(cached, pending, computed)?
            }
        };
        let values = copy_column(&cached.values);
        let errors = cached.errors.restricted_to(mask);
        self.scalar_cache[derived_index] = Some(cached);
        Ok((values?, errors))
    }

    fn compute_scalar(&mut self, derived_index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let CompiledSemantics::Scalar(expr) = &derived.semantics else {
            return Ok(fail_numeric::<N>(
                self.row_count,
                mask,
                EvalError::ExpectedScalar(derived.name.clone()),
            )?);
        };
        let executor = &self.period_executors[self.reference_period];
        let not_commenced = executor.commencement_error(derived_index);
        let live = commenced_rows(mask, not_commenced.as_ref());
        let (column, formula_errors) = self.eval_scalar(expr, &live)?;
        let mut errors = not_commenced_errors(mask, not_commenced.as_ref());
        errors.absorb(formula_errors.within_rule(&derived.label));
        let column = match derived.rounding {
            Some(rounding) => {
                round_dense_column::<N>(column, rounding, &live.without(&errors)?, &mut errors)?
            }
            None => column,
        };
        Ok((column, errors))
    }

    fn evaluate_judgment(&mut self, derived_index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let pending = pending_rows(self.judgment_cache[derived_index].as_ref(), mask)?;
        if computes(self.judgment_cache[derived_index].as_ref(), &pending)
            && self.depth >= self.suspend_depth
        {
            return Err(Stop::Defer(Deferred {
                derived: derived_index,
                judgment: true,
                rows: pending,
            }));
        }
        let cached = match self.judgment_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = match self.compute_judgment(derived_index, &pending) {
                    Ok(computed) => computed,
                    Err(stop) => {
                        // A deferral retries this rule for the same rows;
                        // keep the rows it computed earlier, as recursion
                        // would have, so the retry extends them.
                        self.judgment_cache[derived_index] = cached;
                        return Err(stop);
                    }
                };
                merge_judgment(cached, pending, computed)?
            }
        };
        let values = copied_rows(&cached.values);
        let errors = cached.errors.restricted_to(mask);
        self.judgment_cache[derived_index] = Some(cached);
        Ok((values?, errors))
    }

    fn compute_judgment(&mut self, derived_index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let executor = &self.period_executors[self.reference_period];
        let not_commenced = executor.commencement_error(derived_index);
        let live = commenced_rows(mask, not_commenced.as_ref());
        let (values, formula_errors) = match &derived.semantics {
            CompiledSemantics::Judgment(expr) => self.eval_judgment(expr, &live)?,
            CompiledSemantics::Scalar(_) => {
                return Ok(expected_judgment(self.row_count, mask, &derived.name)?);
            }
        };
        let mut errors = not_commenced_errors(mask, not_commenced.as_ref());
        errors.absorb(formula_errors.within_rule(&derived.label));
        Ok((values, errors))
    }

    /// An inlined body's column for the rows of `mask`. A body read by one
    /// node is evaluated in place; a shared one is computed once per row.
    fn evaluate_inline_scalar(&mut self, index: usize, mask: &RowMask) -> Flow<DenseEval> {
        let program = self.program;
        let inline = &program.inline_scalars[index];
        if inline.references < 2 {
            return self.eval_scalar(&inline.body, mask);
        }
        let mut cache = self.inline_cache.scalars[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.eval_scalar(&inline.body, pending),
            merge_scalar::<N>,
            |values| copy_column(values),
        );
        self.inline_cache.scalars[index] = cache;
        result
    }

    fn evaluate_inline_judgment(&mut self, index: usize, mask: &RowMask) -> Flow<JudgmentEval> {
        let program = self.program;
        let inline = &program.inline_judgments[index];
        if inline.references < 2 {
            return self.eval_judgment(&inline.body, mask);
        }
        let mut cache = self.inline_cache.judgments[index].take();
        let result = cached_rows(
            &mut cache,
            mask,
            |pending| self.eval_judgment(&inline.body, pending),
            merge_judgment,
            |values| copied_rows(values),
        );
        self.inline_cache.judgments[index] = cache;
        result
    }

    fn eval_scalar(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        self.visits += 1;
        self.depth += 1;
        let result = self.scalar_node(expr, mask);
        self.depth -= 1;
        result
    }

    fn scalar_node(&mut self, expr: &CompiledScalarExpr, mask: &RowMask) -> Flow<DenseEval> {
        let len = self.row_count;
        let ambiguous = |placeholder: DenseColumn, leaf: &str| {
            fail_all(
                placeholder,
                mask,
                EvalError::LifetimeAmbiguousLeaf(leaf.to_string()),
            )
        };
        match expr {
            CompiledScalarExpr::OverPeriods { kind, value, n } => {
                self.eval_over_periods(*kind, value, n.as_deref(), mask)
            }
            CompiledScalarExpr::Literal(value) => {
                Ok((broadcast_scalar_literal::<N>(value, len)?, RowErrors::new()))
            }
            CompiledScalarExpr::Derived(index) => self.evaluate_scalar(*index, mask),
            CompiledScalarExpr::Inline(index) => self.evaluate_inline_scalar(*index, mask),
            CompiledScalarExpr::Commenced { rule, value } => {
                let (live, mut errors) =
                    rule.rows(self.period_executors[self.reference_period].period, mask);
                let (column, formula_errors) = self.eval_scalar(value, &live)?;
                errors.absorb(formula_errors);
                Ok((column, errors))
            }
            CompiledScalarExpr::Round { value, rounding } => {
                eval_round::<N, _, _>(self, value.as_ref(), *rounding, mask)
            }
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                // Period-specific: resolve at the reference period. The index
                // expression is itself lifetime-evaluated (typically a literal
                // or derived), then used as integer keys.
                let (keys, mut errors) = self.eval_scalar(index, mask)?;
                let live = mask.without(&errors)?;
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter],
                    &keys,
                    &live,
                    self.period_executors[self.reference_period].period,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
            CompiledScalarExpr::Add(items) => eval_add::<N, _, _>(self, items, mask),
            CompiledScalarExpr::Sub(left, right) => {
                eval_binary::<N, _, _>(self, left, right, mask, N::try_sub)
            }
            CompiledScalarExpr::Mul(left, right) => {
                eval_binary::<N, _, _>(self, left, right, mask, N::try_mul)
            }
            CompiledScalarExpr::Div(left, right) => {
                eval_div::<N, _, _>(self, left, right, mask, false)
            }
            CompiledScalarExpr::Max(items) => {
                eval_extremum::<N, _, _>(self, items, mask, "max", |candidate, best| {
                    candidate > best
                })
            }
            CompiledScalarExpr::Min(items) => {
                eval_extremum::<N, _, _>(self, items, mask, "min", |candidate, best| {
                    candidate < best
                })
            }
            CompiledScalarExpr::Ceil(value) => {
                eval_unary::<N, _, _>(self, value, mask, |value| value.ceil())
            }
            CompiledScalarExpr::Floor(value) => {
                eval_unary::<N, _, _>(self, value, mask, |value| value.floor())
            }
            CompiledScalarExpr::NoMatch {
                subject,
                placeholder,
                labels,
            } => {
                let (placeholder, _) = self.eval_scalar(placeholder, &RowMask::none(mask.len()))?;
                let (subject, mut errors) = self.eval_scalar(subject, mask)?;
                labels.record_failures(&subject, &mask.without(&errors)?, &mut errors);
                Ok((placeholder, errors))
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) = self.eval_judgment(condition, mask)?;
                let live = mask.without(&errors)?;
                let then_rows = live.filter(|row| condition[row].is_holds())?;
                let else_rows = live.difference(&then_rows)?;
                let (then_values, then_errors) = self.eval_scalar(then_expr, &then_rows)?;
                let (else_values, else_errors) = self.eval_scalar(else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors)?;
                let else_live = else_rows.without(&else_errors)?;
                errors.absorb(then_errors);
                errors.absorb(else_errors);
                let column = select_dense::<N>(
                    &then_live,
                    then_values,
                    &else_live,
                    else_values,
                    &mut errors,
                )?;
                Ok((column, errors))
            }
            // A bare input outside a reduction has no single period in general,
            // but a per-person-constant input (a birth / age-attainment year —
            // the shape every statutory computation-year count bottoms out in)
            // carries the same value in every supplied period. Bind it per row
            // when it is verified period-invariant; error naming the input and
            // the first divergence when it is not. This is reached directly and
            // through a derived chain (e.g. an elapsed-years count built from
            // `year_attained_62 - year_attained_21`), and it may sit in scalar
            // or in the `n` position of `sum_top_n_over_periods`.
            CompiledScalarExpr::Input(index) => {
                let name = self.program.root_inputs[*index].clone();
                self.eval_period_invariant_input(expr, &name, mask)
            }
            CompiledScalarExpr::InputOrElse { input, .. } => {
                let name = self.program.root_inputs[*input].clone();
                self.eval_period_invariant_input(expr, &name, mask)
            }
            CompiledScalarExpr::PeriodStart => Ok(ambiguous(
                DenseColumn::Date(filled_rows(len, NaiveDate::default())?),
                "period_start",
            )),
            CompiledScalarExpr::PeriodEnd => Ok(ambiguous(
                DenseColumn::Date(filled_rows(len, NaiveDate::default())?),
                "period_end",
            )),
            CompiledScalarExpr::DateAddDays { .. } => Ok(ambiguous(
                DenseColumn::Date(filled_rows(len, NaiveDate::default())?),
                "date_add_days",
            )),
            CompiledScalarExpr::DateAddMonths { .. } => Ok(ambiguous(
                DenseColumn::Date(filled_rows(len, NaiveDate::default())?),
                "date_add_months",
            )),
            CompiledScalarExpr::DateAddYears { .. } => Ok(ambiguous(
                DenseColumn::Date(filled_rows(len, NaiveDate::default())?),
                "date_add_years",
            )),
            CompiledScalarExpr::DaysBetween { .. } => Ok(ambiguous(
                DenseColumn::Integer(filled_rows(len, 0)?),
                "days_between",
            )),
            CompiledScalarExpr::OnlyRelated { .. } => Ok(ambiguous(
                DenseColumn::Bool(filled_rows(len, false)?),
                "only_related over a relation",
            )),
            CompiledScalarExpr::CountRelated { .. } => Ok(ambiguous(
                DenseColumn::Integer(filled_rows(len, 0)?),
                "count/len over a relation",
            )),
            CompiledScalarExpr::SumRelated { .. } => Ok(ambiguous(
                N::into_column(filled_rows(len, N::ZERO)?),
                "sum over a relation",
            )),
            CompiledScalarExpr::ExtremeRelated { extreme, .. } => Ok(ambiguous(
                N::into_column(filled_rows(len, N::ZERO)?),
                match extreme {
                    Extreme::Min => "min over a relation",
                    Extreme::Max => "max over a relation",
                },
            )),
            CompiledScalarExpr::SingleRelated { .. } => Ok(ambiguous(
                N::into_column(filled_rows(len, N::ZERO)?),
                "only over a relation",
            )),
        }
    }

    /// Collapse an over-periods reduction to a per-entity column: evaluate the
    /// inner `value` under every period's executor, then reduce down the period
    /// axis for each row. A row that fails in one period is not evaluated in
    /// later ones.
    fn eval_over_periods(
        &mut self,
        kind: OverPeriodsKind,
        value: &CompiledScalarExpr,
        n: Option<&CompiledScalarExpr>,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let period_count = self.period_executors.len();
        let mut errors = RowErrors::new();
        let mut live = mask.clone();

        // Count evaluates its argument per period (same leaf rules as the other
        // reductions — period-specific leaves are legal inside a reduction) and
        // counts, per row, the periods whose value is nonzero. This is
        // referentially consistent with the sibling reductions rather than a
        // special-cased period count: `count_over_periods(x)` means "how many
        // periods had a nonzero `x`". A Bool/judgment-shaped inner value counts
        // `true`; every numeric variant counts `!= 0`.
        if kind == OverPeriodsKind::Count {
            let mut counts = filled_rows(self.row_count, 0_i64)?;
            for executor in &mut self.period_executors {
                let (column, period_errors) = executor.drive_expr(value, &live)?;
                live = live.without(&period_errors)?;
                errors.absorb(period_errors);
                let mut count_errors = RowErrors::new();
                accumulate_nonzero_counts(&column, &live, &mut counts, &mut count_errors)?;
                live = live.without(&count_errors)?;
                errors.absorb(count_errors);
            }
            return Ok((DenseColumn::Integer(counts), errors));
        }

        // period-major: per_period[p][r] is entity r's inner value in period p.
        let mut per_period: Vec<Vec<N>> = Vec::with_capacity(period_count);
        for executor in &mut self.period_executors {
            let (column, period_errors) = executor.drive_expr(value, &live)?;
            live = live.without(&period_errors)?;
            errors.absorb(period_errors);
            let mut conversion = RowErrors::new();
            per_period.push(numeric_values::<N>(&column, &live, &mut conversion)?);
            live = live.without(&conversion)?;
            errors.absorb(conversion);
        }

        let mut result = filled_rows(self.row_count, N::ZERO)?;
        match kind {
            // Handled above: count does not build the numeric period matrix.
            OverPeriodsKind::Count => unreachable!("count is handled above"),
            OverPeriodsKind::Sum => {
                let mut overflow = RowErrors::new();
                for row in live.rows() {
                    match per_period
                        .iter()
                        .try_fold(N::ZERO, |total, period| total.try_add(period[row]))
                    {
                        Ok(total) => result[row] = total,
                        Err(error) => overflow.record(row, error.into()),
                    }
                }
                errors.absorb(overflow);
            }
            OverPeriodsKind::Max => {
                for row in live.rows() {
                    // period_count >= 1 is guaranteed by the caller.
                    let mut best = per_period[0][row];
                    for period in &per_period[1..] {
                        if period[row] > best {
                            best = period[row];
                        }
                    }
                    result[row] = best;
                }
            }
            OverPeriodsKind::SumTopN => {
                // `eval_top_n_counts` enforces 1 <= n <= period_count for every
                // row, so `take` below never exceeds the number of periods: a
                // top-N sum is a mathematical no-op past the period count (extra
                // slots would only add zeros), so over-length n is rejected as a
                // likely data error rather than silently padded.
                let mut count_errors = RowErrors::new();
                let counts = self.eval_top_n_counts(n, period_count, &live, &mut count_errors)?;
                live = live.without(&count_errors)?;
                errors.absorb(count_errors);
                let mut overflow = RowErrors::new();
                for row in live.rows() {
                    let mut values: Vec<N> = per_period.iter().map(|period| period[row]).collect();
                    // Descending, under a total order: `rank_cmp` ranks an
                    // f64 NaN above every number, so it is always selected
                    // and poisons the sum, as it does `sum_over_periods`.
                    values.sort_by(|a, b| b.rank_cmp(a));
                    // n is bounded by the period count, so this takes a
                    // genuine prefix of the sorted values (no zero padding).
                    match values[..counts[row]]
                        .iter()
                        .try_fold(N::ZERO, |total, value| total.try_add(*value))
                    {
                        Ok(total) => result[row] = total,
                        Err(error) => overflow.record(row, error.into()),
                    }
                }
                errors.absorb(overflow);
            }
        }
        Ok((N::into_column(result), errors))
    }

    /// Resolve the `n` of `sum_top_n_over_periods` into a per-row count under the
    /// strict n contract. `n` is evaluated under EVERY period's executor and must
    /// resolve to the same value in every period (parameter- and input-sourced n
    /// are held to the identical contract — a period-varying parameter n is a
    /// data error, not a silent pin to the reference period). The reference
    /// period's column is then truncated toward zero to an exact `i64`; a
    /// non-finite or out-of-range value (`try_to_i64_trunc` -> `None`) and any
    /// `n` outside `1 <= n <= period_count` both raise typed errors — never a
    /// clamp to `i64::MAX`, never a silent pin, never a pad past the period count
    /// (which would be an arithmetic no-op masking the bad count). Each check
    /// fails only the rows it applies to.
    fn eval_top_n_counts(
        &mut self,
        n: Option<&CompiledScalarExpr>,
        period_count: usize,
        mask: &RowMask,
        errors: &mut RowErrors,
    ) -> Flow<Vec<usize>> {
        let mut counts = filled_rows(self.row_count, 0)?;
        let Some(n) = n else {
            errors.record_all(
                mask,
                &EvalError::TypeMismatch(
                    "sum_top_n_over_periods requires an n argument".to_string(),
                ),
            );
            return Ok(counts);
        };
        let reduction = OverPeriodsKind::SumTopN.as_call_name();

        // Evaluate `n` under each period's executor: one column per period,
        // positionally aligned by row. A parameter-sourced n indexes each
        // period's date, so a year-varying parameter yields different columns and
        // is caught below; an n derived from period-invariant inputs (the
        // 42 USC 415(b) computation-year count) is identical in every period and
        // passes.
        let mut live = mask.clone();
        let mut per_period: Vec<DenseColumn> = Vec::with_capacity(self.period_executors.len());
        for executor in &mut self.period_executors {
            let (column, period_errors) = executor.drive_expr(n, &live)?;
            live = live.without(&period_errors)?;
            errors.absorb(period_errors);
            per_period.push(column);
        }
        let (reference, others) = per_period
            .split_last()
            .expect("lifetime execution guarantees at least one period");
        // Reject a period-varying n: compare every earlier period against the
        // reference (chronologically-last) period, in each column's own dtype.
        // Each error quotes both values without copying them (`n` can be a
        // text); consecutive rows with the same pair share one.
        let mut varying = RowErrors::new();
        let second_period: Arc<str> = Arc::from(period_label(
            self.period_executors[self.reference_period].period,
        ));
        for (index, column) in others.iter().enumerate() {
            let first_period: Arc<str> =
                Arc::from(period_label(self.period_executors[index].period));
            record_top_n_varying(
                reduction,
                (&first_period, column),
                (&second_period, reference),
                &live,
                self.row_count,
                &mut varying,
            );
        }
        live = live.without(&varying)?;
        errors.absorb(varying);

        // n is period-invariant: truncate the reference column toward zero to an
        // exact i64 and enforce 1 <= n <= period_count per row.
        let mut conversion = RowErrors::new();
        let raw = numeric_values::<N>(reference, &live, &mut conversion)?;
        live = live.without(&conversion)?;
        errors.absorb(conversion);
        for row in live.rows() {
            // Non-finite or beyond the i64 range: a garbage n, reported as out
            // of range rather than saturated to a clamp.
            let Some(as_i64) = raw[row].try_to_i64_trunc() else {
                // `raw` holds only rows `numeric_values` converted, so `n` is
                // a number here and its label is short; a text `n` failed
                // that conversion above.
                errors.record(
                    row,
                    EvalError::OverPeriodsTopNOutOfRange {
                        reduction,
                        n: dense_value_label(reference, row),
                        period_count,
                    },
                );
                continue;
            };
            if as_i64 < 1 || (as_i64 as u64) > period_count as u64 {
                errors.record(
                    row,
                    EvalError::OverPeriodsTopNOutOfRange {
                        reduction,
                        n: as_i64.to_string(),
                        period_count,
                    },
                );
            } else {
                counts[row] = as_i64 as usize;
            }
        }
        Ok(counts)
    }

    /// Bind a bare input evaluated OUTSIDE any reduction, when — and only when —
    /// its supplied value is verified identical across every period for a given
    /// row.
    ///
    /// A reduction defines the period axis explicitly; a bare input does not, so
    /// it is period-ambiguous in general. But the derived counts real statutes
    /// build (42 USC 415(b)'s benefit-computation-year count, from the year a
    /// worker attains 21 and 62) bottom out in inputs that are constant across
    /// the whole history by construction. Rather than reject every such input, we
    /// evaluate `expr` under each period's ordinary executor and check PER ROW
    /// that the value never changes. If a row is invariant across all periods,
    /// that single value is bound for it; if a live row diverges, it fails,
    /// naming the input and the first two differing period labels and values.
    /// Truly period-varying inputs therefore still fail loudly — the check only
    /// legalizes provably unambiguous bindings.
    ///
    /// Values are compared in each column's own dtype (numeric, bool, text or
    /// date), so the invariance test is exact and never rounds. `expr` is the
    /// original `Input` / `InputOrElse` node, replayed identically in every
    /// period; an absent `InputOrElse` broadcasts the same default in each and so
    /// is trivially invariant, binding that default.
    fn eval_period_invariant_input(
        &mut self,
        expr: &CompiledScalarExpr,
        input_name: &str,
        mask: &RowMask,
    ) -> Flow<DenseEval> {
        let mut errors = RowErrors::new();
        let mut live = mask.clone();
        let mut per_period: Vec<DenseColumn> = Vec::with_capacity(self.period_executors.len());
        for executor in &mut self.period_executors {
            let (column, period_errors) = executor.drive_expr(expr, &live)?;
            live = live.without(&period_errors)?;
            errors.absorb(period_errors);
            per_period.push(column);
        }
        // A single period is invariant by definition; nothing to compare.
        let (first, rest) = per_period
            .split_first()
            .expect("lifetime execution guarantees at least one period");
        let input: Arc<str> = Arc::from(input_name);
        let first_period: Arc<str> = Arc::from(period_label(self.period_executors[0].period));
        for (offset, column) in rest.iter().enumerate() {
            // `offset` indexes `rest`, so the diverging period is `offset + 1`.
            let second_period: Arc<str> =
                Arc::from(period_label(self.period_executors[offset + 1].period));
            record_varying_input(
                &input,
                (&first_period, first),
                (&second_period, column),
                &live,
                self.row_count,
                &mut errors,
            );
        }
        // Every live row agrees across all periods: bind the (identical) first
        // period's column, preserving its original dtype for the caller.
        Ok((per_period.swap_remove(0), errors))
    }

    fn eval_judgment(&mut self, expr: &CompiledJudgmentExpr, mask: &RowMask) -> Flow<JudgmentEval> {
        self.visits += 1;
        self.depth += 1;
        let result = self.judgment_node(expr, mask);
        self.depth -= 1;
        result
    }

    fn judgment_node(&mut self, expr: &CompiledJudgmentExpr, mask: &RowMask) -> Flow<JudgmentEval> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.eval_scalar(left, mask)?;
                let live = mask.without(&errors)?;
                let (right, right_errors) = self.eval_scalar(right, &live)?;
                let live = live.without(&right_errors)?;
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors)?;
                Ok((outcomes, errors))
            }
            CompiledJudgmentExpr::Derived(index) => self.evaluate_judgment(*index, mask),
            CompiledJudgmentExpr::Inline(index) => self.evaluate_inline_judgment(*index, mask),
            CompiledJudgmentExpr::Commenced { rule, value } => {
                let (live, mut errors) =
                    rule.rows(self.period_executors[self.reference_period].period, mask);
                let (values, formula_errors) = self.eval_judgment(value, &live)?;
                errors.absorb(formula_errors);
                Ok((values, errors))
            }
            CompiledJudgmentExpr::And(items) => {
                eval_short_circuit(self, items, mask, JudgmentOutcome::NotHolds)
            }
            CompiledJudgmentExpr::Or(items) => {
                eval_short_circuit(self, items, mask, JudgmentOutcome::Holds)
            }
            CompiledJudgmentExpr::Not(item) => {
                let (values, errors) = self.eval_judgment(item, mask)?;
                Ok((negate(values), errors))
            }
        }
    }
}

/// Record `LifetimePeriodVaryingInput` on each row of `rows` not yet failed
/// whose value differs between the two periods' columns. Each error quotes
/// both values without copying them; consecutive rows with the same pair
/// share one (`share_quoted`).
fn record_varying_input(
    input: &Arc<str>,
    (first_period, first): (&Arc<str>, &DenseColumn),
    (second_period, second): (&Arc<str>, &DenseColumn),
    rows: &RowMask,
    row_count: usize,
    errors: &mut RowErrors,
) {
    let mut last: Option<Rc<Quoted>> = None;
    for row in rows.rows() {
        if !errors.contains(row) && values_differ(first, second, row) {
            let quoted = Quoted {
                rows: row_count,
                error: QuotedError::LifetimePeriodVaryingInput {
                    input: Arc::clone(input),
                    first_period: Arc::clone(first_period),
                    first_value: quoted_value(first, row),
                    second_period: Arc::clone(second_period),
                    second_value: quoted_value(second, row),
                },
            };
            errors.record_error(row, share_quoted(&mut last, quoted));
        }
    }
}

/// Record `OverPeriodsTopNPeriodVarying` on each row of `rows` not yet failed
/// whose `n` differs between an earlier period's column and the reference
/// period's. `n` can be a text, so each error quotes both values without
/// copying them; consecutive rows with the same pair share one.
fn record_top_n_varying(
    reduction: &'static str,
    (first_period, first): (&Arc<str>, &DenseColumn),
    (second_period, reference): (&Arc<str>, &DenseColumn),
    rows: &RowMask,
    row_count: usize,
    errors: &mut RowErrors,
) {
    let mut last: Option<Rc<Quoted>> = None;
    for row in rows.rows() {
        if !errors.contains(row) && values_differ(reference, first, row) {
            let quoted = Quoted {
                rows: row_count,
                error: QuotedError::OverPeriodsTopNPeriodVarying {
                    reduction,
                    first_period: Arc::clone(first_period),
                    first_value: quoted_value(first, row),
                    second_period: Arc::clone(second_period),
                    second_value: quoted_value(reference, row),
                },
            };
            errors.record_error(row, share_quoted(&mut last, quoted));
        }
    }
}

/// A human-readable label for a period, used in period-invariance error
/// messages. The `start..end` boundaries identify the period unambiguously and
/// match how periods are otherwise surfaced to callers.
fn period_label(period: &Period) -> String {
    format!("in {}..{}", period.start, period.end)
}

/// The value at `row` of a dense column as an error quotes it: a text shared,
/// anything else as its label.
fn quoted_value(column: &DenseColumn, row: usize) -> QuotedValue {
    match column {
        DenseColumn::Text(values) if row < values.len() => {
            QuotedValue::Text(Arc::clone(&values[row]))
        }
        _ => QuotedValue::Label(dense_value_label(column, row)),
    }
}

/// Format the value at `row` of a dense column for an error message, in the
/// column's own dtype (so an integer year reads `1985`, not `1985.0`). A row
/// the column lacks is described by the column's dtype and length instead, so
/// building an error never panics.
fn dense_value_label(column: &DenseColumn, row: usize) -> String {
    let value = match column {
        DenseColumn::Bool(values) => values.get(row).map(ToString::to_string),
        DenseColumn::Integer(values) => values.get(row).map(ToString::to_string),
        DenseColumn::Decimal(values) => values.get(row).map(ToString::to_string),
        DenseColumn::Float(values) => values.get(row).map(ToString::to_string),
        DenseColumn::Text(values) => values.get(row).map(|value| format!("{value:?}")),
        DenseColumn::Date(values) => values.get(row).map(ToString::to_string),
    };
    value.unwrap_or_else(|| {
        let dtype = match column {
            DenseColumn::Bool(_) => "bool",
            DenseColumn::Integer(_) => "integer",
            DenseColumn::Decimal(_) => "decimal",
            DenseColumn::Float(_) => "float",
            DenseColumn::Text(_) => "text",
            DenseColumn::Date(_) => "date",
        };
        format!(
            "no value (row {row} of a {}-row {dtype} column)",
            column.len()
        )
    })
}

/// Find a directly-nested over-periods reduction inside a compiled scalar
/// expression, returning its kind. Used at compile time to reject
/// `sum_over_periods(max_over_periods(x))` and similar: a reduction consumes the
/// period axis, so nesting another in its argument is meaningless. This walks
/// the compiled tree structurally; a reduction reached only through a `Derived`
/// reference is not flagged here (it still errors safely at run time), because
/// that transitive check needs the whole compiled program, not one expression.
fn nested_over_periods_kind(expr: &CompiledScalarExpr) -> Option<OverPeriodsKind> {
    match expr {
        CompiledScalarExpr::OverPeriods { kind, .. } => Some(*kind),
        CompiledScalarExpr::Literal(_)
        | CompiledScalarExpr::Input(_)
        | CompiledScalarExpr::InputOrElse { .. }
        | CompiledScalarExpr::Derived(_)
        | CompiledScalarExpr::Inline(_)
        | CompiledScalarExpr::PeriodStart
        | CompiledScalarExpr::PeriodEnd
        | CompiledScalarExpr::OnlyRelated { .. }
        | CompiledScalarExpr::CountRelated { .. }
        | CompiledScalarExpr::SumRelated { .. }
        | CompiledScalarExpr::ExtremeRelated { .. }
        | CompiledScalarExpr::SingleRelated { .. } => None,
        CompiledScalarExpr::ParameterLookup { index, .. } => nested_over_periods_kind(index),
        CompiledScalarExpr::Add(items)
        | CompiledScalarExpr::Max(items)
        | CompiledScalarExpr::Min(items) => items.iter().find_map(nested_over_periods_kind),
        CompiledScalarExpr::Sub(left, right)
        | CompiledScalarExpr::Mul(left, right)
        | CompiledScalarExpr::Div(left, right) => {
            nested_over_periods_kind(left).or_else(|| nested_over_periods_kind(right))
        }
        CompiledScalarExpr::Ceil(value)
        | CompiledScalarExpr::Floor(value)
        | CompiledScalarExpr::Commenced { value, .. }
        | CompiledScalarExpr::Round { value, .. } => nested_over_periods_kind(value),
        CompiledScalarExpr::DateAddDays { date, days } => {
            nested_over_periods_kind(date).or_else(|| nested_over_periods_kind(days))
        }
        CompiledScalarExpr::DateAddMonths { date, months } => {
            nested_over_periods_kind(date).or_else(|| nested_over_periods_kind(months))
        }
        CompiledScalarExpr::DateAddYears { date, years } => {
            nested_over_periods_kind(date).or_else(|| nested_over_periods_kind(years))
        }
        CompiledScalarExpr::DaysBetween { from, to } => {
            nested_over_periods_kind(from).or_else(|| nested_over_periods_kind(to))
        }
        CompiledScalarExpr::If {
            then_expr,
            else_expr,
            ..
        } => nested_over_periods_kind(then_expr).or_else(|| nested_over_periods_kind(else_expr)),
        CompiledScalarExpr::NoMatch {
            subject,
            placeholder,
            ..
        } => nested_over_periods_kind(subject).or_else(|| nested_over_periods_kind(placeholder)),
    }
}

fn project_root_judgment_to_related(
    values: &[JudgmentOutcome],
    offsets: &[usize],
) -> Result<Vec<bool>, EvalError> {
    let row_count = offsets.len().saturating_sub(1);
    if values.len() != row_count {
        return Err(EvalError::TypeMismatch(format!(
            "dense root judgment has length {} but relation offsets describe {} rows",
            values.len(),
            row_count
        )));
    }

    let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
    for row in 0..row_count {
        for _ in offsets[row]..offsets[row + 1] {
            projected.push(values[row].is_holds());
        }
    }
    Ok(projected)
}

fn project_root_column_to_related(
    column: &DenseColumn,
    offsets: &[usize],
) -> Result<DenseColumn, EvalError> {
    let row_count = offsets.len().saturating_sub(1);
    if column.len() != row_count {
        return Err(EvalError::TypeMismatch(format!(
            "dense root scalar has length {} but relation offsets describe {} rows",
            column.len(),
            row_count
        )));
    }

    Ok(match column {
        DenseColumn::Bool(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Bool(projected)
        }
        DenseColumn::Integer(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Integer(projected)
        }
        DenseColumn::Decimal(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Decimal(projected)
        }
        DenseColumn::Float(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Float(projected)
        }
        DenseColumn::Text(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row].clone());
                }
            }
            DenseColumn::Text(projected)
        }
        DenseColumn::Date(values) => {
            let mut projected = row_buffer(*offsets.last().unwrap_or(&0))?;
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Date(projected)
        }
    })
}

/// Add one to `counts[row]` for each live row whose value in `column` is
/// nonzero, used by `count_over_periods` to count the periods with a nonzero
/// inner value. Numeric variants count `!= 0`; a `Bool` column counts `true`;
/// `Date`/`Text` columns have no zero and cannot be produced by an arithmetic
/// inner value, so every live row fails rather than being silently counted.
fn accumulate_nonzero_counts(
    column: &DenseColumn,
    live: &RowMask,
    counts: &mut [i64],
    errors: &mut RowErrors,
) -> Result<(), EvalError> {
    if column.len() != counts.len() {
        return Err(EvalError::TypeMismatch(format!(
            "count_over_periods inner value produced {} rows but the batch has {}",
            column.len(),
            counts.len()
        )));
    }
    for row in live.rows() {
        let nonzero = match column {
            DenseColumn::Bool(values) => values[row],
            DenseColumn::Integer(values) => values[row] != 0,
            DenseColumn::Decimal(values) => !values[row].is_zero(),
            DenseColumn::Float(values) => values[row] != 0.0,
            DenseColumn::Text(_) | DenseColumn::Date(_) => {
                errors.record_all(
                    live,
                    &EvalError::TypeMismatch(
                        "count_over_periods requires a numeric or boolean inner value (text and date have no zero to count against)".to_string(),
                    ),
                );
                return Ok(());
            }
        };
        if nonzero {
            counts[row] = counts[row].saturating_add(1);
        }
    }
    Ok(())
}

/// Whether two positionally aligned dense columns disagree at `row`.
/// Comparison is exact and in each column's dtype; two numeric columns of
/// different variants (e.g. an `Integer` and a `Float`) are compared by
/// numeric value, so an input supplied as `1985` in one period and `1985.0` in
/// another is still period-invariant. A value not representable as a decimal,
/// mismatched non-numeric shapes and mismatched lengths all count as differing
/// (this errors loudly, the safe outcome for a structurally different value).
fn values_differ(left: &DenseColumn, right: &DenseColumn, row: usize) -> bool {
    if left.len() != right.len() {
        return true;
    }
    match (left, right) {
        (DenseColumn::Bool(a), DenseColumn::Bool(b)) => a[row] != b[row],
        (DenseColumn::Text(a), DenseColumn::Text(b)) => a[row] != b[row],
        (DenseColumn::Date(a), DenseColumn::Date(b)) => a[row] != b[row],
        _ => match (decimal_at(left, row), decimal_at(right, row)) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        },
    }
}

fn decimal_at(column: &DenseColumn, row: usize) -> Option<Decimal> {
    match column {
        DenseColumn::Integer(values) => Some(Decimal::from(values[row])),
        DenseColumn::Decimal(values) => Some(values[row]),
        DenseColumn::Float(values) => Decimal::from_f64(values[row]),
        _ => None,
    }
}

fn is_numeric(column: &DenseColumn) -> bool {
    matches!(
        column,
        DenseColumn::Integer(_) | DenseColumn::Decimal(_) | DenseColumn::Float(_)
    )
}

/// Read a numeric column in mode `N`. A row that is live and not numeric
/// (a non-numeric column, or an `f64` with no decimal value) fails with a type
/// error; other rows hold zero.
fn numeric_values<N: DenseNum>(
    column: &DenseColumn,
    live: &RowMask,
    errors: &mut RowErrors,
) -> Result<Vec<N>, EvalError> {
    Ok(match column {
        DenseColumn::Integer(values) => {
            let mut numbers = row_buffer(values.len())?;
            numbers.extend(values.iter().map(|value| N::from_integer(*value)));
            numbers
        }
        DenseColumn::Decimal(values) => {
            let mut numbers = row_buffer(values.len())?;
            numbers.extend(values.iter().map(N::from_decimal));
            numbers
        }
        DenseColumn::Float(values) => {
            let mut numbers = filled_rows(values.len(), N::ZERO)?;
            for (row, value) in values.iter().enumerate() {
                match N::from_float(*value) {
                    Some(number) => numbers[row] = number,
                    None if live.contains(row) => errors.record(
                        row,
                        EvalError::TypeMismatch(
                            "float column value is not representable as decimal".to_string(),
                        ),
                    ),
                    None => {}
                }
            }
            numbers
        }
        DenseColumn::Bool(_) | DenseColumn::Text(_) | DenseColumn::Date(_) => {
            let placeholder = filled_rows(column.len(), N::ZERO)?;
            errors.record_all(
                live,
                &EvalError::TypeMismatch("expected decimal-compatible dense column".to_string()),
            );
            placeholder
        }
    })
}

/// A row's value as an exact integer key or count, under the explain path's
/// rule: an integer, or a decimal (or finite `f64`) with no fractional part
/// that fits in `i64`. Anything else, including a non-numeric value, is none.
fn index_at(column: &DenseColumn, row: usize) -> Option<i64> {
    match column {
        DenseColumn::Integer(values) => Some(values[row]),
        DenseColumn::Decimal(values) if values[row].fract().is_zero() => values[row].to_i64(),
        DenseColumn::Float(values) => f64_as_index(values[row]),
        _ => None,
    }
}

/// An `f64` as an exact `i64`: `None` unless it is integral and in range.
fn f64_as_index(value: f64) -> Option<i64> {
    // 2^63 is exactly representable; `value < 2^63` keeps the cast exact
    // rather than saturating, and NaN and the infinities fail both tests.
    (value.fract() == 0.0
        && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value))
    .then_some(value as i64)
}

fn broadcast_scalar_literal<N: DenseNum>(
    value: &DenseLiteral,
    length: usize,
) -> Result<DenseColumn, EvalError> {
    Ok(match value {
        DenseLiteral::Bool(value) => DenseColumn::Bool(filled_rows(length, *value)?),
        DenseLiteral::Integer(value) => DenseColumn::Integer(filled_rows(length, *value)?),
        DenseLiteral::Decimal(value) => {
            N::into_column(filled_rows(length, N::from_decimal(value))?)
        }
        // Each row shares the compiled literal's text.
        DenseLiteral::Text(value) => DenseColumn::Text(filled_rows(length, Arc::clone(value))?),
        DenseLiteral::Date(value) => DenseColumn::Date(filled_rows(length, *value)?),
    })
}

/// A copy of `column`, reserved fallibly: an input or a cached rule's column,
/// which each read takes its own copy of.
fn copy_column(column: &DenseColumn) -> Result<DenseColumn, EvalError> {
    Ok(match column {
        DenseColumn::Bool(values) => DenseColumn::Bool(copied_rows(values)?),
        DenseColumn::Integer(values) => DenseColumn::Integer(copied_rows(values)?),
        DenseColumn::Decimal(values) => DenseColumn::Decimal(copied_rows(values)?),
        DenseColumn::Float(values) => DenseColumn::Float(copied_rows(values)?),
        DenseColumn::Text(values) => DenseColumn::Text(copied_rows(values)?),
        DenseColumn::Date(values) => DenseColumn::Date(copied_rows(values)?),
    })
}

/// The text a row holds before it is filled: one shared empty text, so a
/// placeholder column costs a pointer a row.
fn empty_text() -> Arc<str> {
    Arc::from("")
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum TableKind {
    Integer,
    Bool,
    Text,
    Date,
    Numeric,
}

/// The dtype of a parameter lookup column. It is a property of the selected
/// table, not of which keys a batch happens to look up, so a column's dtype
/// never depends on which rows are live.
fn table_kind(values: &std::collections::BTreeMap<i64, ScalarValue>) -> TableKind {
    let kinds = values
        .values()
        .map(|value| match value {
            ScalarValue::Integer(_) => TableKind::Integer,
            ScalarValue::Bool(_) => TableKind::Bool,
            ScalarValue::Text(_) => TableKind::Text,
            ScalarValue::Date(_) => TableKind::Date,
            ScalarValue::Decimal(_) => TableKind::Numeric,
        })
        .collect::<HashSet<_>>();
    match kinds.into_iter().collect::<Vec<_>>().as_slice() {
        [kind] => *kind,
        _ => TableKind::Numeric,
    }
}

/// Look up `parameter` at `period` for the live rows' keys. A non-integral
/// key and a missing cell fail their row, as on the explain path.
fn lookup_parameter_dense<N: DenseNum>(
    compiled: &CompiledParameter,
    keys: &DenseColumn,
    live: &RowMask,
    period: &Period,
    errors: &mut RowErrors,
) -> Result<DenseColumn, EvalError> {
    let parameter = &compiled.parameter;
    let len = keys.len();
    let selected = parameter
        .versions
        .iter()
        .enumerate()
        .filter(|(_, version)| version.applies_at(period.start))
        .max_by_key(|(_, version)| version.effective_from);
    let version = selected.map(|(_, version)| version);
    let texts = selected.map(|(index, _)| &compiled.texts[index]);
    let lookup = |row: usize| -> Result<&ScalarValue, EvalError> {
        let Some(key) = index_at(keys, row) else {
            return Err(EvalError::TypeMismatch(format!(
                "parameter key for `{}` must be an integer",
                parameter.name
            )));
        };
        version
            .and_then(|version| version.values.get(&key))
            .ok_or_else(|| EvalError::MissingParameterValue {
                parameter: parameter.name.clone(),
                key,
                at: period.start,
            })
    };
    // Each row records its own failure: a lookup's, or a value of the wrong
    // kind for the table.
    Ok(
        match version.map_or(TableKind::Numeric, |version| table_kind(&version.values)) {
            TableKind::Integer => {
                let mut values = filled_rows(len, 0)?;
                for row in live.rows() {
                    match lookup(row) {
                        Ok(ScalarValue::Integer(value)) => values[row] = *value,
                        Ok(_) => {}
                        Err(error) => errors.record(row, error),
                    }
                }
                DenseColumn::Integer(values)
            }
            TableKind::Bool => {
                let mut values = filled_rows(len, false)?;
                for row in live.rows() {
                    match lookup(row) {
                        Ok(ScalarValue::Bool(value)) => values[row] = *value,
                        Ok(_) => {}
                        Err(error) => errors.record(row, error),
                    }
                }
                DenseColumn::Bool(values)
            }
            TableKind::Text => {
                let mut values = filled_rows(len, empty_text())?;
                for row in live.rows() {
                    match lookup(row) {
                        // The compiled table's shared text for the same key.
                        Ok(ScalarValue::Text(_)) => {
                            if let Some(text) = index_at(keys, row)
                                .and_then(|key| texts.and_then(|texts| texts.get(&key)))
                            {
                                values[row] = Arc::clone(text);
                            }
                        }
                        Ok(_) => {}
                        Err(error) => errors.record(row, error),
                    }
                }
                DenseColumn::Text(values)
            }
            TableKind::Date => {
                let mut values = filled_rows(len, NaiveDate::default())?;
                for row in live.rows() {
                    match lookup(row) {
                        Ok(ScalarValue::Date(value)) => values[row] = *value,
                        Ok(_) => {}
                        Err(error) => errors.record(row, error),
                    }
                }
                DenseColumn::Date(values)
            }
            TableKind::Numeric => {
                let mut values = filled_rows(len, N::ZERO)?;
                for row in live.rows() {
                    match lookup(row).map(ScalarValue::as_decimal) {
                        Ok(Some(number)) => values[row] = N::from_decimal(&number),
                        Ok(None) => errors.record(
                            row,
                            EvalError::TypeMismatch(
                                "parameter values must be numeric in dense mode".to_string(),
                            ),
                        ),
                        Err(error) => errors.record(row, error),
                    }
                }
                N::into_column(values)
            }
        },
    )
}

/// The running `min`/`max` of each root row over its related rows' values:
/// numbers in mode `N`, or dates.
enum Extremes<'v, N: DenseNum> {
    Numbers {
        values: Vec<N>,
        best: Vec<Option<N>>,
    },
    Dates {
        values: &'v [NaiveDate],
        best: Vec<Option<NaiveDate>>,
    },
}

impl<N: DenseNum> Extremes<'_, N> {
    /// Fold `related`'s value into `owner`'s extreme. Ties keep the first
    /// unless `tie_prefers` says otherwise, so member order never shows.
    fn take(&mut self, extreme: Extreme, owner: usize, related: usize) {
        match self {
            Self::Numbers { values, best } => {
                let candidate = values[related];
                best[owner] = Some(match best[owner] {
                    Some(current)
                        if !(extreme.improves(&candidate, &current)
                            || (candidate == current && candidate.tie_prefers(&current))) =>
                    {
                        current
                    }
                    _ => candidate,
                });
            }
            Self::Dates { values, best } => {
                let candidate = values[related];
                best[owner] = Some(match best[owner] {
                    Some(current) if !extreme.improves(&candidate, &current) => current,
                    _ => candidate,
                });
            }
        }
    }
}

/// A root-row column holding, for each related row of `rows`, its value of
/// `values` at the root row that owns it. Rows of no related row hold the
/// column type's default.
fn gather_to_owners(
    values: &DenseColumn,
    owners: &[usize],
    rows: &RowMask,
    row_count: usize,
) -> Result<DenseColumn, EvalError> {
    fn gather<T: Clone>(
        values: &[T],
        default: T,
        owners: &[usize],
        rows: &RowMask,
        row_count: usize,
    ) -> Result<Vec<T>, EvalError> {
        let mut gathered = filled_rows(row_count, default)?;
        for related in rows.rows() {
            gathered[owners[related]] = values[related].clone();
        }
        Ok(gathered)
    }
    Ok(match values {
        DenseColumn::Bool(values) => {
            DenseColumn::Bool(gather(values, false, owners, rows, row_count)?)
        }
        DenseColumn::Integer(values) => {
            DenseColumn::Integer(gather(values, 0, owners, rows, row_count)?)
        }
        DenseColumn::Decimal(values) => {
            DenseColumn::Decimal(gather(values, Decimal::ZERO, owners, rows, row_count)?)
        }
        DenseColumn::Float(values) => {
            DenseColumn::Float(gather(values, 0.0, owners, rows, row_count)?)
        }
        DenseColumn::Text(values) => {
            DenseColumn::Text(gather(values, empty_text(), owners, rows, row_count)?)
        }
        DenseColumn::Date(values) => DenseColumn::Date(gather(
            values,
            NaiveDate::default(),
            owners,
            rows,
            row_count,
        )?),
    })
}

/// Move `source`'s values on `rows` into `target`. Each value moves rather
/// than being cloned, so a text row hands over its `Arc` without touching the
/// count, and no value is copied.
fn assign_rows<T>(target: &mut [T], mut source: Vec<T>, rows: &RowMask) {
    for row in rows.rows() {
        std::mem::swap(&mut target[row], &mut source[row]);
    }
}

/// The per-row choice of an `if` (or the merge of two computed stretches of a
/// cached column): `then_values` on `then_rows`, `else_values` on `else_rows`.
/// Integer, boolean, text and date columns of one dtype keep it; any other
/// numeric pair (decimal or `f64` on either side) is read in mode `N`, whether
/// or not both sides have live rows, so the dtype does not depend on the
/// data. Dtypes that cannot combine are an error only when both sides
/// have live rows; a side no row selects contributes nothing.
fn select_dense<N: DenseNum>(
    then_rows: &RowMask,
    then_values: DenseColumn,
    else_rows: &RowMask,
    else_values: DenseColumn,
    errors: &mut RowErrors,
) -> Result<DenseColumn, EvalError> {
    Ok(match (then_values, else_values) {
        (DenseColumn::Bool(mut target), DenseColumn::Bool(source)) => {
            assign_rows(&mut target, source, else_rows);
            DenseColumn::Bool(target)
        }
        (DenseColumn::Integer(mut target), DenseColumn::Integer(source)) => {
            assign_rows(&mut target, source, else_rows);
            DenseColumn::Integer(target)
        }
        (DenseColumn::Text(mut target), DenseColumn::Text(source)) => {
            assign_rows(&mut target, source, else_rows);
            DenseColumn::Text(target)
        }
        (DenseColumn::Date(mut target), DenseColumn::Date(source)) => {
            assign_rows(&mut target, source, else_rows);
            DenseColumn::Date(target)
        }
        (then_values, else_values) if is_numeric(&then_values) && is_numeric(&else_values) => {
            let mut numbers = numeric_values::<N>(&then_values, then_rows, errors)?;
            let others = numeric_values::<N>(&else_values, else_rows, errors)?;
            assign_rows(&mut numbers, others, else_rows);
            N::into_column(numbers)
        }
        (then_values, else_values) => {
            if then_rows.is_empty() {
                else_values
            } else if else_rows.is_empty() {
                then_values
            } else {
                return Err(EvalError::TypeMismatch(
                    "dense if() branches must have the same dtype".to_string(),
                ));
            }
        }
    })
}

/// Compare two columns on the live rows with the explain path's rules:
/// booleans and text support only `==`/`!=`, dates compare
/// chronologically, numbers compare in mode `N`, and anything else fails the
/// row with explain's type error.
fn compare_dense<N: DenseNum>(
    left: &DenseColumn,
    op: ComparisonOp,
    right: &DenseColumn,
    live: &RowMask,
    errors: &mut RowErrors,
) -> Result<Vec<JudgmentOutcome>, EvalError> {
    fn outcome(holds: bool) -> JudgmentOutcome {
        if holds {
            JudgmentOutcome::Holds
        } else {
            JudgmentOutcome::NotHolds
        }
    }
    fn ordered<T: PartialOrd>(op: ComparisonOp, left: &T, right: &T) -> bool {
        match op {
            ComparisonOp::Lt => left < right,
            ComparisonOp::Lte => left <= right,
            ComparisonOp::Gt => left > right,
            ComparisonOp::Gte => left >= right,
            ComparisonOp::Eq => left == right,
            ComparisonOp::Ne => left != right,
        }
    }
    let mut outcomes = filled_rows(left.len(), JudgmentOutcome::NotHolds)?;
    let mut equality = |holds: &dyn Fn(usize) -> bool, message: &str| match op {
        ComparisonOp::Eq | ComparisonOp::Ne => {
            for row in live.rows() {
                outcomes[row] = outcome(holds(row) == (op == ComparisonOp::Eq));
            }
        }
        _ => errors.record_all(live, &EvalError::TypeMismatch(message.to_string())),
    };
    match (left, right) {
        (DenseColumn::Bool(left), DenseColumn::Bool(right)) => equality(
            &|row| left[row] == right[row],
            "boolean comparisons only support == and !=",
        ),
        (DenseColumn::Text(left), DenseColumn::Text(right)) => equality(
            &|row| left[row] == right[row],
            "text comparisons only support == and !=",
        ),
        (DenseColumn::Date(left), DenseColumn::Date(right)) => {
            for row in live.rows() {
                outcomes[row] = outcome(ordered(op, &left[row], &right[row]));
            }
        }
        (left, right) if is_numeric(left) && is_numeric(right) => {
            let mut conversion = RowErrors::new();
            let left = numeric_values::<N>(left, live, &mut conversion)?;
            let right = numeric_values::<N>(right, live, &mut conversion)?;
            for row in live.rows() {
                if !conversion.contains(row) {
                    outcomes[row] = outcome(ordered(op, &left[row], &right[row]));
                }
            }
            errors.absorb(conversion);
        }
        (left, _) => {
            let side = if is_numeric(left) { "right" } else { "left" };
            errors.record_all(
                live,
                &EvalError::TypeMismatch(format!("{side} side of comparison is not numeric")),
            );
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod quoted_failure_tests {
    use super::*;

    /// The value the error recorded for `row` quotes, rendered.
    fn reported_value(errors: &RowErrors, row: usize) -> String {
        match errors.get(row).map(RowError::to_eval) {
            Some(EvalError::NoMatchingArm { value, .. }) => value,
            other => panic!("row {row}: expected a match failure, got {other:?}"),
        }
    }

    /// Invariant: each row's match failure quotes that row's own value.
    /// Rows share an error only while their values are the same text, so a
    /// later row in a run never reports an earlier row's value.
    #[test]
    fn each_row_of_a_run_quotes_its_own_value() {
        let labels = MatchLabels {
            rule: Arc::from("out"),
            subject: Arc::from("label"),
            patterns: Arc::from("\"z\""),
        };
        let a: Arc<str> = Arc::from("a");
        let b: Arc<str> = Arc::from("b");
        let column = DenseColumn::Text(vec![
            Arc::clone(&a),
            Arc::clone(&a),
            Arc::clone(&b),
            Arc::clone(&b),
            Arc::clone(&a),
        ]);
        let mut errors = RowErrors::new();
        labels.record_failures(&column, &RowMask::all(5), &mut errors);
        let values: Vec<String> = (0..5).map(|row| reported_value(&errors, row)).collect();
        assert_eq!(values, ["\"a\"", "\"a\"", "\"b\"", "\"b\"", "\"a\""]);
        // Rows 0 and 1 share one error; rows 1 and 2 do not.
        let shared = |left: usize, right: usize| match (errors.get(left), errors.get(right)) {
            (Some(RowError::Quoted(left)), Some(RowError::Quoted(right))) => {
                Rc::ptr_eq(left, right)
            }
            other => panic!("expected quoted errors, got {other:?}"),
        };
        assert!(shared(0, 1) && shared(2, 3));
        assert!(!shared(1, 2) && !shared(3, 4));

        // A non-text subject quotes each row's label the same way.
        let mut errors = RowErrors::new();
        let integers = DenseColumn::Integer(vec![7, 7, 8]);
        labels.record_failures(&integers, &RowMask::all(3), &mut errors);
        let values: Vec<String> = (0..3).map(|row| reported_value(&errors, row)).collect();
        assert_eq!(values, ["7", "7", "8"]);
    }

    fn texts(values: &[&str]) -> DenseColumn {
        // Equal values share one `Arc`, as a column filled by sharing does.
        let mut seen: Vec<Arc<str>> = Vec::new();
        DenseColumn::Text(
            values
                .iter()
                .map(|value| {
                    if let Some(shared) = seen.iter().find(|shared| &***shared == *value) {
                        return Arc::clone(shared);
                    }
                    let shared: Arc<str> = Arc::from(*value);
                    seen.push(Arc::clone(&shared));
                    shared
                })
                .collect(),
        )
    }

    /// The pair each failing row's period-varying error quotes, rendered;
    /// `None` for a row with no error.
    fn reported_pairs(errors: &RowErrors, rows: usize) -> Vec<Option<(String, String)>> {
        (0..rows)
            .map(|row| match errors.get(row).map(RowError::to_eval) {
                None => None,
                Some(EvalError::LifetimePeriodVaryingInput {
                    first_value,
                    second_value,
                    ..
                })
                | Some(EvalError::OverPeriodsTopNPeriodVarying {
                    first_value,
                    second_value,
                    ..
                }) => Some((first_value, second_value)),
                Some(other) => panic!("row {row}: unexpected {other:?}"),
            })
            .collect()
    }

    /// The two columns each recorder compares: the first period's values,
    /// then the second's. Row 2 agrees, and every varying row after row 0
    /// changes one of its pair, so a later row that reused an earlier row's
    /// error would report the wrong pair.
    fn varying_periods() -> (DenseColumn, DenseColumn, Vec<Option<(String, String)>>) {
        let first = texts(&["a", "a", "a", "a", "b", "b"]);
        let second = texts(&["x", "x", "a", "y", "y", "y"]);
        let pair = |first: &str, second: &str| Some((format!("{first:?}"), format!("{second:?}")));
        let expected = vec![
            pair("a", "x"),
            pair("a", "x"),
            None,
            pair("a", "y"),
            pair("b", "y"),
            pair("b", "y"),
        ];
        (first, second, expected)
    }

    /// Invariant: each varying row of a lifetime input reports its own pair
    /// of values, however its run is shared.
    #[test]
    fn each_varying_input_row_quotes_its_own_pair() {
        let (first, second, expected) = varying_periods();
        let mut errors = RowErrors::new();
        record_varying_input(
            &Arc::from("name"),
            (&Arc::from("in 2024"), &first),
            (&Arc::from("in 2025"), &second),
            &RowMask::all(6),
            6,
            &mut errors,
        );
        assert_eq!(reported_pairs(&errors, 6), expected);
    }

    /// Invariant: each varying row of `sum_top_n`'s `n` reports its own
    /// pair of values, however its run is shared.
    #[test]
    fn each_varying_top_n_row_quotes_its_own_pair() {
        let (first, reference, expected) = varying_periods();
        let mut errors = RowErrors::new();
        record_top_n_varying(
            "sum_top_n",
            (&Arc::from("in 2024"), &first),
            (&Arc::from("in 2025"), &reference),
            &RowMask::all(6),
            6,
            &mut errors,
        );
        assert_eq!(reported_pairs(&errors, 6), expected);
    }
}
