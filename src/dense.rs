use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::EvalError;
use crate::model::{
    ComparisonOp, DType, DerivedSemantics, IndexedParameter, JudgmentExpr, JudgmentOutcome,
    OverPeriodsKind, Period, Program, RelatedValueRef, Rounding, RoundingMode, SCALAR_ENTITY,
    ScalarExpr, ScalarValue,
};

#[derive(Clone, Debug)]
pub enum DenseColumn {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<Decimal>),
    /// f64 column for the fast (`execute_f64`) execution mode. Decimal mode
    /// also accepts these as inputs, converting losslessly where possible.
    Float(Vec<f64>),
    Text(Vec<String>),
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

    /// Integral keys or day counts, one per row; a row `failed` marks reads 0.
    fn as_index_vec(&self, failed: &[bool]) -> Result<Vec<i64>, EvalError> {
        let failed = |row: usize| failed.get(row).copied().unwrap_or(false);
        match self {
            Self::Integer(values) => Ok(values
                .iter()
                .enumerate()
                .map(|(row, value)| if failed(row) { 0 } else { *value })
                .collect()),
            Self::Decimal(values) => values
                .iter()
                .enumerate()
                .map(|(row, value)| {
                    if failed(row) {
                        return Ok(0);
                    }
                    value.to_i64().ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "parameter key for dense lookup must be integral".to_string(),
                        )
                    })
                })
                .collect(),
            Self::Float(values) => values
                .iter()
                .enumerate()
                .map(|(row, value)| {
                    if failed(row) {
                        Ok(0)
                    } else if value.is_finite() && value.fract() == 0.0 {
                        Ok(*value as i64)
                    } else {
                        Err(EvalError::TypeMismatch(
                            "parameter key for dense lookup must be integral".to_string(),
                        ))
                    }
                })
                .collect(),
            _ => Err(EvalError::TypeMismatch(
                "parameter key for dense lookup must be numeric".to_string(),
            )),
        }
    }

    fn as_date_vec(&self) -> Result<Vec<chrono::NaiveDate>, EvalError> {
        match self {
            Self::Date(values) => Ok(values.clone()),
            _ => Err(EvalError::TypeMismatch(
                "expected date dense column".to_string(),
            )),
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
            (Self::Text(values), _) => ScalarValue::Text(values[index].clone()),
            (Self::Date(values), _) => ScalarValue::Date(values[index]),
        }
    }
}

/// Numeric type the dense executor evaluates arithmetic in. `Decimal` is the
/// canonical mode (exact, matches the sparse engine); `f64` trades exactness
/// for roughly an order of magnitude in throughput for bulk microsimulation.
trait DenseNum:
    Copy
    + PartialOrd
    + std::ops::Add<Output = Self>
    + std::ops::AddAssign
    + std::ops::Sub<Output = Self>
    + std::ops::Mul<Output = Self>
    + std::ops::Div<Output = Self>
{
    const ZERO: Self;
    const MIN: Self;
    const MAX: Self;

    fn from_decimal(value: &Decimal) -> Self;
    fn ceil(self) -> Self;
    fn floor(self) -> Self;
    /// Round to a currency scale under a declared mode. `Decimal` rounds
    /// exactly (identical to the sparse and bulk paths); `f64` is best-effort,
    /// consistent with this mode being for throughput, not exact legal
    /// determinations.
    fn round_to(self, rounding: Rounding) -> Self;
    fn is_zero(self) -> bool;
    /// Truncate toward zero to an exact `i64`, or `None` when the value is
    /// non-finite or lies beyond the `i64` range. Used to read the `n` of
    /// `sum_top_n_over_periods`: `None` (and any out-of-range integer) is a hard
    /// error under the strict n contract, never a silent saturation.
    fn try_to_i64_trunc(self) -> Option<i64>;
    /// Wrap evaluated values in the column variant for this mode.
    fn into_column(values: Vec<Self>) -> DenseColumn;
    /// Read any numeric column as this mode's working vector.
    fn vec_from_column(column: &DenseColumn) -> Result<Vec<Self>, EvalError>;
}

impl DenseNum for Decimal {
    const ZERO: Self = Decimal::ZERO;
    const MIN: Self = Decimal::MIN;
    const MAX: Self = Decimal::MAX;

    fn from_decimal(value: &Decimal) -> Self {
        *value
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

    fn is_zero(self) -> bool {
        self == Decimal::ZERO
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

    fn vec_from_column(column: &DenseColumn) -> Result<Vec<Self>, EvalError> {
        match column {
            DenseColumn::Integer(values) => {
                Ok(values.iter().map(|value| Decimal::from(*value)).collect())
            }
            DenseColumn::Decimal(values) => Ok(values.clone()),
            DenseColumn::Float(values) => values
                .iter()
                .map(|value| {
                    Decimal::from_f64(*value).ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "float column value is not representable as decimal".to_string(),
                        )
                    })
                })
                .collect(),
            _ => Err(EvalError::TypeMismatch(
                "expected decimal-compatible dense column".to_string(),
            )),
        }
    }
}

impl DenseNum for f64 {
    const ZERO: Self = 0.0;
    const MIN: Self = f64::MIN;
    const MAX: Self = f64::MAX;

    fn from_decimal(value: &Decimal) -> Self {
        value.to_f64().unwrap_or(f64::NAN)
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

    fn is_zero(self) -> bool {
        self == 0.0
    }

    fn try_to_i64_trunc(self) -> Option<i64> {
        // Reject non-finite and out-of-range values instead of saturating: the
        // strict n contract errors on a garbage n rather than silently reading
        // it as 0 (NaN) or a clamped extreme.
        let truncated = self.trunc();
        if !truncated.is_finite() || truncated < i64::MIN as f64 || truncated > i64::MAX as f64 {
            return None;
        }
        Some(truncated as i64)
    }

    fn into_column(values: Vec<Self>) -> DenseColumn {
        DenseColumn::Float(values)
    }

    fn vec_from_column(column: &DenseColumn) -> Result<Vec<Self>, EvalError> {
        match column {
            DenseColumn::Integer(values) => Ok(values.iter().map(|value| *value as f64).collect()),
            DenseColumn::Decimal(values) => Ok(values
                .iter()
                .map(|value| value.to_f64().unwrap_or(f64::NAN))
                .collect()),
            DenseColumn::Float(values) => Ok(values.clone()),
            _ => Err(EvalError::TypeMismatch(
                "expected decimal-compatible dense column".to_string(),
            )),
        }
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
) -> Result<DenseColumn, EvalError> {
    match column {
        DenseColumn::Bool(_) | DenseColumn::Text(_) | DenseColumn::Date(_) => Ok(column),
        numeric => {
            let values = N::vec_from_column(&numeric)?;
            Ok(N::into_column(
                values
                    .into_iter()
                    .map(|value| value.round_to(rounding))
                    .collect(),
            ))
        }
    }
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

#[derive(Clone, Debug)]
pub struct DenseRelationBatchSpec {
    pub offsets: Vec<usize>,
    pub inputs: HashMap<String, DenseColumn>,
}

#[derive(Clone, Debug)]
pub struct DenseBatchSpec {
    pub row_count: usize,
    pub inputs: HashMap<String, DenseColumn>,
    pub relations: HashMap<DenseRelationKey, DenseRelationBatchSpec>,
}

#[derive(Clone, Debug)]
struct DenseRelationBatch {
    offsets: Vec<usize>,
    related_count: usize,
    inputs: Vec<Option<DenseColumn>>,
}

#[derive(Clone, Debug)]
struct DenseBoundBatch {
    row_count: usize,
    /// None entries indicate an optional root input that the caller did not
    /// supply; the executor will fall back to its per-reference default.
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

#[derive(Clone, Debug)]
enum CompiledScalarExpr {
    Literal(ScalarValue),
    Input(usize),
    InputOrElse {
        input: usize,
        default: ScalarValue,
    },
    Derived(usize),
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
    CountRelated {
        relation: usize,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    SumRelated {
        relation: usize,
        value: Box<CompiledRelatedScalarExpr>,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    If {
        condition: Box<CompiledJudgmentExpr>,
        then_expr: Box<CompiledScalarExpr>,
        else_expr: Box<CompiledScalarExpr>,
    },
    /// A `match` without `_`, fused from the comparison chain the formula
    /// lowering writes for it (see [`match_chain`]): each row takes the value
    /// of the first `(pattern, value)` arm whose pattern equals its subject,
    /// and a row no arm takes gets explain's error, deferred to the rows that
    /// keep it.
    Match {
        subject: Box<CompiledScalarExpr>,
        arms: Vec<(CompiledScalarExpr, CompiledScalarExpr)>,
        labels: MatchLabels,
    },
    /// Cross-period reduction, evaluated only by the lifetime executor. `value`
    /// is compiled as an ordinary per-period scalar (evaluated once per supplied
    /// period); `n` (SumTopN only) is compiled likewise and read at the
    /// reference period. The per-period executor rejects this node.
    OverPeriods {
        kind: OverPeriodsKind,
        value: Box<CompiledScalarExpr>,
        n: Option<Box<CompiledScalarExpr>>,
    },
}

#[derive(Clone, Debug)]
enum CompiledRelatedScalarExpr {
    Literal(ScalarValue),
    Input(usize),
    InputOrElse {
        input: usize,
        default: ScalarValue,
    },
    RootScalar(Box<CompiledScalarExpr>),
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
    /// Related-row form of [`CompiledScalarExpr::Match`].
    Match {
        subject: Box<CompiledRelatedScalarExpr>,
        arms: Vec<(CompiledRelatedScalarExpr, CompiledRelatedScalarExpr)>,
        labels: MatchLabels,
    },
}

/// How a failed `match` names its rule, subject and arms, rendered at compile
/// time from the model expressions the dense plan no longer carries.
#[derive(Clone, Debug)]
struct MatchLabels {
    /// The rule whose formula contains the `match`, as explain names it when
    /// the error leaves that rule: by its id when it has one, else its name.
    /// Dense inlines a related entity's rules into the aggregation that reads
    /// them, so the name is fixed here. It is empty for a `match` in a derived
    /// relation's membership predicate, which belongs to whichever rule
    /// evaluates the relation.
    rule: String,
    subject: String,
    patterns: String,
}

impl MatchLabels {
    fn new(rule: &str, subject: &ScalarExpr, patterns: &[ScalarExpr]) -> Self {
        Self {
            rule: rule.to_string(),
            subject: crate::engine::describe_match_operand(subject),
            patterns: patterns
                .iter()
                .map(crate::engine::describe_match_operand)
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Explain's error for every row of `subject` that no pattern covers.
    fn uncovered(&self, subject: DenseColumn, covered: &[bool]) -> RowErrors {
        if covered.iter().all(|covered| *covered) {
            return RowErrors::default();
        }
        let failure = Rc::new(MatchFailure {
            labels: self.clone(),
            subject,
        });
        RowErrors(
            covered
                .iter()
                .enumerate()
                .filter(|(_, covered)| !**covered)
                .map(|(row, _)| {
                    (
                        row,
                        RowError {
                            failure: Rc::clone(&failure),
                            row,
                        },
                    )
                })
                .collect(),
        )
    }
}

/// One evaluation of a `match` whose subject some rows' arms do not cover.
#[derive(Clone, Debug)]
struct MatchFailure {
    labels: MatchLabels,
    /// The evaluated subject, to name a failing row's value.
    subject: DenseColumn,
}

/// Why one row has no value: its `match` subject at `row` of the failure's
/// subject column. Rows move when a root value is spread over related rows or a
/// related row's error reaches its root row, so the subject row travels with it.
#[derive(Clone, Debug)]
struct RowError {
    failure: Rc<MatchFailure>,
    row: usize,
}

impl RowError {
    fn to_eval_error(&self) -> EvalError {
        let labels = &self.failure.labels;
        EvalError::NoMatchingArm {
            rule: labels.rule.clone(),
            subject: labels.subject.clone(),
            value: dense_value_label(&self.failure.subject, self.row),
            patterns: labels.patterns.clone(),
        }
    }
}

/// The rows of a dense value whose evaluation failed, in ascending row order.
///
/// Dense evaluates every branch of a conditional for every row, and a rule for
/// every row even when only some rows' conditions reach it. Explain evaluates a
/// row only as far as its own conditions lead and fails only if that path
/// fails. So a row's failure is recorded here rather than raised: the row keeps
/// a placeholder value, a conditional that does not select the row drops the
/// error with the value, and the error fails the call only if a requested output
/// keeps it. Empty, and free, unless some `match` subject is uncovered.
#[derive(Clone, Debug, Default)]
struct RowErrors(Vec<(usize, RowError)>);

impl RowErrors {
    fn contains(&self, row: usize) -> bool {
        self.0
            .binary_search_by_key(&row, |(failed, _)| *failed)
            .is_ok()
    }

    /// These errors, then `later`'s for rows that have none: a row stops at the
    /// first failure in explain's evaluation order.
    fn or(self, later: RowErrors) -> RowErrors {
        if later.0.is_empty() {
            return self;
        }
        if self.0.is_empty() {
            return later;
        }
        let mut merged = Vec::with_capacity(self.0.len() + later.0.len());
        let mut later = later.0.into_iter().peekable();
        for (row, error) in self.0 {
            while let Some(entry) = later.next_if(|(later_row, _)| *later_row < row) {
                merged.push(entry);
            }
            later.next_if(|(later_row, _)| *later_row == row);
            merged.push((row, error));
        }
        merged.extend(later);
        RowErrors(merged)
    }

    /// The errors of rows that evaluation reaches.
    fn reached(self, reaches: impl Fn(usize) -> bool) -> RowErrors {
        if self.0.is_empty() {
            return self;
        }
        RowErrors(
            self.0
                .into_iter()
                .filter(|(row, _)| reaches(*row))
                .collect(),
        )
    }

    /// The errors of `if condition then .. else ..`: a row's condition error,
    /// else the error of the branch the row selects.
    fn select(
        condition: RowErrors,
        holds: impl Fn(usize) -> bool,
        then_errors: RowErrors,
        else_errors: RowErrors,
    ) -> RowErrors {
        condition.or(then_errors
            .reached(&holds)
            .or(else_errors.reached(|row| !holds(row))))
    }

    /// Spread root rows' errors over their related rows.
    fn spread(&self, offsets: &[usize]) -> RowErrors {
        RowErrors(
            self.0
                .iter()
                .flat_map(|(row, error)| {
                    (offsets[*row]..offsets[*row + 1]).map(move |related| (related, error.clone()))
                })
                .collect(),
        )
    }

    /// Each root row's first related-row error, in related-row order. Explain
    /// also stops at a root row's first failing related entity, so the root row
    /// fails exactly when explain's does. When several related entities fail,
    /// explain can name a different one: it visits them in id order, and for a
    /// derived relation it resolves every membership stage before any `where`
    /// clause or value.
    fn gather(self, offsets: &[usize]) -> RowErrors {
        let mut gathered: Vec<(usize, RowError)> = Vec::new();
        // `offsets` starts at 0 and never decreases, and related rows arrive in
        // ascending order, so the owning root row only moves forward: it is the
        // last one that starts at or before `related`.
        let mut row = 0;
        for (related, error) in self.0 {
            while offsets[row + 1] <= related {
                row += 1;
            }
            if gathered.last().is_none_or(|(last, _)| *last != row) {
                gathered.push((row, error));
            }
        }
        RowErrors(gathered)
    }

    /// Name `rule` in failures that do not name one yet (a `match` in a derived
    /// relation's predicate): explain names the rule the error leaves first.
    fn within_rule(self, rule: &str) -> RowErrors {
        if self
            .0
            .iter()
            .all(|(_, error)| !error.failure.labels.rule.is_empty())
        {
            return self;
        }
        let mut named: Vec<(Rc<MatchFailure>, Rc<MatchFailure>)> = Vec::new();
        RowErrors(
            self.0
                .into_iter()
                .map(|(row, mut error)| {
                    if error.failure.labels.rule.is_empty() {
                        let existing = named
                            .iter()
                            .find(|(unnamed, _)| Rc::ptr_eq(unnamed, &error.failure))
                            .map(|(_, named)| Rc::clone(named));
                        let renamed = existing.unwrap_or_else(|| {
                            let mut failure = (*error.failure).clone();
                            failure.labels.rule = rule.to_string();
                            let failure = Rc::new(failure);
                            named.push((Rc::clone(&error.failure), Rc::clone(&failure)));
                            failure
                        });
                        error.failure = renamed;
                    }
                    (row, error)
                })
                .collect(),
        )
    }
}

/// A dense value, one entry per row, and the rows whose evaluation failed.
#[derive(Clone, Debug)]
struct Evaluated<T> {
    values: T,
    errors: RowErrors,
}

impl<T> Evaluated<T> {
    fn ok(values: T) -> Self {
        Self {
            values,
            errors: RowErrors::default(),
        }
    }

    fn try_map<U>(
        self,
        map: impl FnOnce(T) -> Result<U, EvalError>,
    ) -> Result<Evaluated<U>, EvalError> {
        Ok(Evaluated {
            values: map(self.values)?,
            errors: self.errors,
        })
    }
}

impl Evaluated<DenseColumn> {
    /// Integral keys or day counts, one per row. A row that already failed
    /// reads 0 instead of its placeholder: explain never computes that value,
    /// so it must not fail the conversion, or overflow a date shift, in the
    /// row's place.
    fn into_index_vec(self) -> Result<Evaluated<Vec<i64>>, EvalError> {
        let mut failed = Vec::new();
        if !self.errors.0.is_empty() {
            failed = vec![false; self.values.len()];
            for (row, _) in &self.errors.0 {
                failed[*row] = true;
            }
        }
        Ok(Evaluated {
            values: self.values.as_index_vec(&failed)?,
            errors: self.errors,
        })
    }
}

impl<N: DenseNum> Evaluated<Vec<N>> {
    fn into_column(self) -> Evaluated<DenseColumn> {
        Evaluated {
            values: N::into_column(self.values),
            errors: self.errors,
        }
    }
}

/// Read an evaluated column as numeric mode `N`.
fn numeric<N: DenseNum>(column: Evaluated<DenseColumn>) -> Result<Evaluated<Vec<N>>, EvalError> {
    column.try_map(|values| N::vec_from_column(&values))
}

/// Fold one more operand of `add`, `max` or `min` into `acc`. Explain evaluates
/// operands left to right, so a row keeps the first operand's failure.
fn fold_numeric<N: DenseNum>(
    acc: &mut Evaluated<Vec<N>>,
    item: Evaluated<DenseColumn>,
    fold: impl Fn(&mut N, N),
) -> Result<(), EvalError> {
    let item = numeric::<N>(item)?;
    for (acc, value) in acc.values.iter_mut().zip(item.values) {
        fold(acc, value);
    }
    acc.errors = std::mem::take(&mut acc.errors).or(item.errors);
    Ok(())
}

/// Combine two operands row by row; a row keeps the left operand's failure
/// first, as explain evaluates left to right.
fn zip_numeric<N: DenseNum>(
    left: Evaluated<Vec<N>>,
    right: Evaluated<Vec<N>>,
    combine: impl Fn(N, N) -> N,
) -> Evaluated<DenseColumn> {
    Evaluated {
        values: N::into_column(
            left.values
                .into_iter()
                .zip(right.values)
                .map(|(left, right)| combine(left, right))
                .collect(),
        ),
        errors: left.errors.or(right.errors),
    }
}

/// `dividend / divisor`. Explain evaluates the divisor first and fails on a
/// zero divisor before it evaluates the dividend, so the divisor's failure
/// comes first, and a row whose divisor failed does not also divide by its
/// placeholder.
fn divide_numeric<N: DenseNum>(
    dividend: Evaluated<Vec<N>>,
    divisor: Evaluated<Vec<N>>,
) -> Result<Evaluated<DenseColumn>, EvalError> {
    let divisor_errors = divisor.errors;
    let values = dividend
        .values
        .into_iter()
        .zip(divisor.values)
        .enumerate()
        .map(|(row, (dividend, divisor))| {
            if !divisor.is_zero() {
                Ok(dividend / divisor)
            } else if divisor_errors.contains(row) {
                Ok(N::ZERO)
            } else {
                Err(EvalError::DivisionByZero)
            }
        })
        .collect::<Result<Vec<N>, EvalError>>()?;
    Ok(Evaluated {
        values: N::into_column(values),
        errors: divisor_errors.or(dividend.errors),
    })
}

fn map_numeric<N: DenseNum>(
    value: Evaluated<Vec<N>>,
    map: impl Fn(N) -> N,
) -> Evaluated<DenseColumn> {
    Evaluated {
        values: N::into_column(value.values.into_iter().map(map).collect()),
        errors: value.errors,
    }
}

fn add_days(date: Evaluated<Vec<NaiveDate>>, days: Evaluated<Vec<i64>>) -> Evaluated<DenseColumn> {
    Evaluated {
        values: DenseColumn::Date(
            date.values
                .into_iter()
                .zip(days.values)
                .map(|(date, days)| date + chrono::Duration::days(days))
                .collect(),
        ),
        errors: date.errors.or(days.errors),
    }
}

/// Shift each date by a calendar offset. A row whose date or offset already
/// failed keeps that failure instead of a range error for its placeholder.
fn shift_dates(
    date: Evaluated<Vec<NaiveDate>>,
    offset: Evaluated<Vec<i64>>,
    shift: fn(NaiveDate, i64) -> Result<NaiveDate, EvalError>,
) -> Result<Evaluated<DenseColumn>, EvalError> {
    let errors = date.errors.or(offset.errors);
    let values = date
        .values
        .into_iter()
        .zip(offset.values)
        .enumerate()
        .map(|(row, (date, offset))| match shift(date, offset) {
            Err(_) if errors.contains(row) => Ok(date),
            shifted => shifted,
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Evaluated {
        values: DenseColumn::Date(values),
        errors,
    })
}

fn days_between(
    from: Evaluated<Vec<NaiveDate>>,
    to: Evaluated<Vec<NaiveDate>>,
) -> Evaluated<DenseColumn> {
    Evaluated {
        values: DenseColumn::Integer(
            from.values
                .into_iter()
                .zip(to.values)
                .map(|(from, to)| (to - from).num_days())
                .collect(),
        ),
        errors: from.errors.or(to.errors),
    }
}

/// `if condition then .. else ..` over a batch: each row takes the branch its
/// condition selects, with that branch's failure (after the condition's own).
fn select_dense<N: DenseNum>(
    condition: Evaluated<Vec<JudgmentOutcome>>,
    then_values: Evaluated<DenseColumn>,
    else_values: Evaluated<DenseColumn>,
) -> Result<Evaluated<DenseColumn>, EvalError> {
    let Evaluated {
        values: condition,
        errors: condition_errors,
    } = condition;
    let errors = RowErrors::select(
        condition_errors,
        |row| condition[row].is_holds(),
        then_values.errors,
        else_values.errors,
    );
    Ok(Evaluated {
        values: select_dense_scalar_column::<N>(condition, then_values.values, else_values.values)?,
        errors,
    })
}

fn all_hold(row_count: usize) -> Evaluated<Vec<JudgmentOutcome>> {
    Evaluated::ok(vec![JudgmentOutcome::Holds; row_count])
}

fn none_hold(row_count: usize) -> Evaluated<Vec<JudgmentOutcome>> {
    Evaluated::ok(vec![JudgmentOutcome::NotHolds; row_count])
}

/// Fold one more item into an `and`. Explain stops at the first item that does
/// not hold, so a row that already fails the `and` never reaches this item's
/// failure.
fn and_judgments(
    combined: Evaluated<Vec<JudgmentOutcome>>,
    item: Evaluated<Vec<JudgmentOutcome>>,
) -> Evaluated<Vec<JudgmentOutcome>> {
    let Evaluated {
        values: mut results,
        errors,
    } = combined;
    let errors = errors.or(item
        .errors
        .reached(|row| results[row] != JudgmentOutcome::NotHolds));
    for (result, value) in results.iter_mut().zip(item.values) {
        *result = match (*result, value) {
            (JudgmentOutcome::NotHolds, _) | (_, JudgmentOutcome::NotHolds) => {
                JudgmentOutcome::NotHolds
            }
            (JudgmentOutcome::Undetermined, _) | (_, JudgmentOutcome::Undetermined) => {
                JudgmentOutcome::Undetermined
            }
            _ => JudgmentOutcome::Holds,
        };
    }
    Evaluated {
        values: results,
        errors,
    }
}

/// Fold one more item into an `or`, which explain stops at the first item that
/// holds.
fn or_judgments(
    combined: Evaluated<Vec<JudgmentOutcome>>,
    item: Evaluated<Vec<JudgmentOutcome>>,
) -> Evaluated<Vec<JudgmentOutcome>> {
    let Evaluated {
        values: mut results,
        errors,
    } = combined;
    let errors = errors.or(item
        .errors
        .reached(|row| results[row] != JudgmentOutcome::Holds));
    for (result, value) in results.iter_mut().zip(item.values) {
        *result = match (*result, value) {
            (JudgmentOutcome::Holds, _) | (_, JudgmentOutcome::Holds) => JudgmentOutcome::Holds,
            (JudgmentOutcome::Undetermined, _) | (_, JudgmentOutcome::Undetermined) => {
                JudgmentOutcome::Undetermined
            }
            _ => JudgmentOutcome::NotHolds,
        };
    }
    Evaluated {
        values: results,
        errors,
    }
}

fn not_judgment(item: Evaluated<Vec<JudgmentOutcome>>) -> Evaluated<Vec<JudgmentOutcome>> {
    Evaluated {
        values: item
            .values
            .into_iter()
            .map(|value| match value {
                JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
                JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
                JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
            })
            .collect(),
        errors: item.errors,
    }
}

/// The requested outputs, or the error explain reports first if a requested
/// output keeps a failed row: the lowest failing row (explain's query order),
/// then the first such output in the requested order.
fn dense_result(
    row_count: usize,
    outputs: Vec<(&String, (DenseOutputValue, RowErrors))>,
) -> Result<DenseExecutionResult, EvalError> {
    let mut first: Option<&(usize, RowError)> = None;
    for (_, (_, errors)) in &outputs {
        if let Some(entry) = errors.0.first()
            && first.is_none_or(|(row, _)| entry.0 < *row)
        {
            first = Some(entry);
        }
    }
    if let Some((_, error)) = first {
        return Err(error.to_eval_error());
    }
    Ok(DenseExecutionResult {
        row_count,
        outputs: outputs
            .into_iter()
            .map(|(name, (value, _))| (name.clone(), value))
            .collect(),
    })
}

/// The innermost arm of a `match` without `_`, given the evaluated subject, the
/// errors of its patterns, which rows some pattern covers, and the last arm's
/// value. A covered row takes `value` (rows covered by an outer arm discard
/// it); an uncovered row fails as explain's does.
/// The arms of a `match` without `_` whose comparison chain starts at the `if`
/// with this `condition`, `then_expr` and `else_expr`: `if s == p1: v1 else if
/// s == p2: v2 .. else no_match(s, [p1, p2, ..])`, as the formula lowering
/// writes it. Dense evaluates the chain as one node that gives each row the
/// value of the first pattern equal to its subject. That is the chain's
/// meaning only when every condition compares the fallback's subject with its
/// pattern, in order, so any other shape is `None`.
fn match_chain<'a>(
    condition: &'a JudgmentExpr,
    then_expr: &'a ScalarExpr,
    else_expr: &'a ScalarExpr,
) -> Option<MatchChain<'a>> {
    let mut arms = vec![(condition, then_expr)];
    let mut rest = else_expr;
    while let ScalarExpr::If {
        condition,
        then_expr,
        else_expr,
    } = rest
    {
        arms.push((condition, then_expr));
        rest = else_expr;
    }
    let ScalarExpr::NoMatch { subject, patterns } = rest else {
        return None;
    };
    if arms.len() != patterns.len() {
        return None;
    }
    let values = arms
        .into_iter()
        .zip(patterns)
        .map(|((condition, value), pattern)| match condition {
            JudgmentExpr::Comparison {
                left,
                op: ComparisonOp::Eq,
                right,
            } if left == subject.as_ref() && right == pattern => Some(value),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(MatchChain {
        subject,
        patterns,
        values,
    })
}

struct MatchChain<'a> {
    subject: &'a ScalarExpr,
    patterns: &'a [ScalarExpr],
    /// Each pattern's value, in pattern order.
    values: Vec<&'a ScalarExpr>,
}

/// A `match` without `_` over a batch, from its evaluated subject and
/// `(pattern, value)` arms: each row takes the value of the first arm whose
/// pattern equals its subject. A row's error follows explain down the
/// comparison chain: the subject's, then each pattern's while no earlier arm
/// has taken the row, then explain's no-arm error if no arm takes it, else the
/// taken value's. A row no arm takes keeps the last arm's value as a
/// placeholder.
fn select_match_arm<N: DenseNum>(
    labels: &MatchLabels,
    subject: Evaluated<DenseColumn>,
    arms: Vec<(Evaluated<DenseColumn>, Evaluated<DenseColumn>)>,
) -> Result<Evaluated<DenseColumn>, EvalError> {
    let mut taken: Vec<Option<usize>> = vec![None; subject.values.len()];
    let mut errors = subject.errors;
    let mut values = Vec::with_capacity(arms.len());
    for (arm, (pattern, value)) in arms.into_iter().enumerate() {
        errors = errors.or(pattern.errors.reached(|row| taken[row].is_none()));
        let matched =
            compare_related_columns::<N>(&subject.values, ComparisonOp::Eq, &pattern.values)?;
        for (taken, matched) in taken.iter_mut().zip(matched) {
            if matched && taken.is_none() {
                *taken = Some(arm);
            }
        }
        values.push(value);
    }
    let covered: Vec<bool> = taken.iter().map(Option::is_some).collect();
    let mut errors = errors.or(labels.uncovered(subject.values, &covered));
    let mut values = values.into_iter().enumerate().rev();
    let (last, last_value) = values.next().expect("a match chain has an arm");
    errors = errors.or(last_value.errors.reached(|row| taken[row] == Some(last)));
    let mut column = last_value.values;
    for (arm, value) in values {
        // Rows take one arm each, so the arms' value errors never share a row.
        errors = errors.or(value.errors.reached(|row| taken[row] == Some(arm)));
        let takes = taken
            .iter()
            .map(|taken| {
                if *taken == Some(arm) {
                    JudgmentOutcome::Holds
                } else {
                    JudgmentOutcome::NotHolds
                }
            })
            .collect();
        column = select_dense_scalar_column::<N>(takes, value.values, column)?;
    }
    Ok(Evaluated {
        values: column,
        errors,
    })
}

/// Narrow a relation mask by the next stage of relation membership. Explain
/// evaluates a stage only for the related entities that passed the ones before
/// it, so a related row's error in `next` counts only if the row passed `mask`.
fn narrow_relation_mask(
    mask: Evaluated<Option<Vec<bool>>>,
    next: Evaluated<Option<Vec<bool>>>,
) -> Evaluated<Option<Vec<bool>>> {
    let Evaluated {
        values: mask,
        errors,
    } = mask;
    let errors = errors.or(next
        .errors
        .reached(|row| mask.as_ref().is_none_or(|mask| mask[row])));
    let values = match (mask, next.values) {
        (Some(mut mask), Some(next)) => {
            for (keep, next) in mask.iter_mut().zip(next) {
                *keep &= next;
            }
            Some(mask)
        }
        (mask, None) => mask,
        (None, next) => next,
    };
    Evaluated { values, errors }
}

const NO_MATCH_OUTSIDE_CHAIN: &str =
    "a `match` fallback outside its comparison chain is not supported in dense execution";

#[derive(Clone, Debug)]
enum CompiledRelatedJudgmentExpr {
    Literal(bool),
    Comparison {
        left: CompiledRelatedScalarExpr,
        op: ComparisonOp,
        right: CompiledRelatedScalarExpr,
    },
    RootJudgment(Box<CompiledJudgmentExpr>),
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
    And(Vec<CompiledJudgmentExpr>),
    Or(Vec<CompiledJudgmentExpr>),
    Not(Box<CompiledJudgmentExpr>),
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
}

#[derive(Clone, Debug)]
pub struct DenseCompiledProgram {
    root_entity: String,
    root_inputs: Vec<String>,
    /// Set of root input indices that are only ever referenced via
    /// `input_or_else`. These may be omitted at execution time — the
    /// per-reference default is inlined by the executor.
    optional_root_inputs: HashSet<usize>,
    relations: Vec<DenseRelationSchema>,
    /// Per-relation: indices of related inputs that are only ever referenced
    /// via `input_or_else` inside a `where` predicate.
    optional_related_inputs: Vec<HashSet<usize>>,
    parameters: Vec<CompiledParameter>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
}

impl DenseCompiledProgram {
    pub fn from_artifact(
        artifact: &CompiledProgramArtifact,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
        Self::from_program(&artifact.program.to_program()?, entity)
    }

    pub fn from_program(
        program: &Program,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
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

    /// Every rule in a dense plan is reachable from the compiled roots, so a plan executed
    /// before any of their commencement dates has no lawful answer. The generic executor
    /// reports that per rule as `MissingDerivedFormulaVersion`; report it identically here
    /// rather than computing a pre-commencement number (#84).
    fn check_commencement(&self, period: &Period) -> Result<(), EvalError> {
        for derived in &self.derived {
            if let Some(effective_from) = derived.effective_from
                && period.start < effective_from
            {
                return Err(EvalError::MissingDerivedFormulaVersion {
                    derived: derived.name.clone(),
                    at: period.start,
                });
            }
        }
        Ok(())
    }

    fn execute_with<N: DenseNum>(
        &self,
        period: &Period,
        batch: DenseBatchSpec,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        self.check_commencement(period)?;
        let batch = self.bind_batch(batch)?;
        let mut executor: DenseExecutor<'_, N> = DenseExecutor::new(self, period, batch);
        let mut evaluated = Vec::with_capacity(outputs.len());
        for output in outputs {
            let Some(&derived_index) = self.derived_index.get(output) else {
                return Err(EvalError::UnknownDerived(output.clone()));
            };
            let derived = &self.derived[derived_index];
            let value = match &derived.semantics {
                CompiledSemantics::Scalar(_) => {
                    let column = executor.evaluate_scalar(derived_index)?.clone();
                    (DenseOutputValue::Scalar(column.values), column.errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let values = executor.evaluate_judgment(derived_index)?.clone();
                    (DenseOutputValue::Judgment(values.values), values.errors)
                }
            };
            evaluated.push((output, value));
        }
        dense_result(executor.batch.row_count, evaluated)
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
        for output in outputs {
            let Some(&derived_index) = self.derived_index.get(output) else {
                return Err(EvalError::UnknownDerived(output.clone()));
            };
            if !self.derived_reduces_over_periods(derived_index) {
                return Err(EvalError::LifetimeOutputWithoutReduction(output.clone()));
            }
        }

        let mut executor: LifetimeExecutor<'_, N> =
            LifetimeExecutor::new(self, periods, bound, row_count);
        let mut evaluated = Vec::with_capacity(outputs.len());
        for output in outputs {
            let derived_index = self.derived_index[output];
            let value = match &self.derived[derived_index].semantics {
                CompiledSemantics::Scalar(_) => {
                    let column = executor.evaluate_scalar(derived_index)?.clone();
                    (DenseOutputValue::Scalar(column.values), column.errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let values = executor.evaluate_judgment(derived_index)?.clone();
                    (DenseOutputValue::Judgment(values.values), values.errors)
                }
            };
            evaluated.push((output, value));
        }
        dense_result(row_count, evaluated)
    }

    /// Does this derived's compiled formula contain an over-periods reduction,
    /// directly or through the derived values it depends on? Used to gate
    /// lifetime execution to reduction outputs only.
    fn derived_reduces_over_periods(&self, derived_index: usize) -> bool {
        let mut visiting = HashSet::new();
        self.derived_reduces_over_periods_inner(derived_index, &mut visiting)
    }

    fn derived_reduces_over_periods_inner(
        &self,
        derived_index: usize,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        if !visiting.insert(derived_index) {
            return false;
        }
        let result = match &self.derived[derived_index].semantics {
            CompiledSemantics::Scalar(expr) => self.scalar_reduces_over_periods(expr, visiting),
            CompiledSemantics::Judgment(expr) => self.judgment_reduces_over_periods(expr, visiting),
        };
        visiting.remove(&derived_index);
        result
    }

    fn scalar_reduces_over_periods(
        &self,
        expr: &CompiledScalarExpr,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        match expr {
            CompiledScalarExpr::OverPeriods { .. } => true,
            CompiledScalarExpr::Literal(_)
            | CompiledScalarExpr::Input(_)
            | CompiledScalarExpr::InputOrElse { .. }
            | CompiledScalarExpr::PeriodStart
            | CompiledScalarExpr::PeriodEnd => false,
            CompiledScalarExpr::Derived(index) => {
                self.derived_reduces_over_periods_inner(*index, visiting)
            }
            CompiledScalarExpr::ParameterLookup { index, .. } => {
                self.scalar_reduces_over_periods(index, visiting)
            }
            CompiledScalarExpr::Add(items)
            | CompiledScalarExpr::Max(items)
            | CompiledScalarExpr::Min(items) => items
                .iter()
                .any(|item| self.scalar_reduces_over_periods(item, visiting)),
            CompiledScalarExpr::Sub(left, right)
            | CompiledScalarExpr::Mul(left, right)
            | CompiledScalarExpr::Div(left, right) => {
                self.scalar_reduces_over_periods(left, visiting)
                    || self.scalar_reduces_over_periods(right, visiting)
            }
            CompiledScalarExpr::Ceil(value) | CompiledScalarExpr::Floor(value) => {
                self.scalar_reduces_over_periods(value, visiting)
            }
            CompiledScalarExpr::DateAddDays { date, days } => {
                self.scalar_reduces_over_periods(date, visiting)
                    || self.scalar_reduces_over_periods(days, visiting)
            }
            CompiledScalarExpr::DateAddMonths { date, months } => {
                self.scalar_reduces_over_periods(date, visiting)
                    || self.scalar_reduces_over_periods(months, visiting)
            }
            CompiledScalarExpr::DateAddYears { date, years } => {
                self.scalar_reduces_over_periods(date, visiting)
                    || self.scalar_reduces_over_periods(years, visiting)
            }
            CompiledScalarExpr::DaysBetween { from, to } => {
                self.scalar_reduces_over_periods(from, visiting)
                    || self.scalar_reduces_over_periods(to, visiting)
            }
            CompiledScalarExpr::CountRelated { .. } | CompiledScalarExpr::SumRelated { .. } => {
                false
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment_reduces_over_periods(condition, visiting)
                    || self.scalar_reduces_over_periods(then_expr, visiting)
                    || self.scalar_reduces_over_periods(else_expr, visiting)
            }
            CompiledScalarExpr::Match { subject, arms, .. } => {
                self.scalar_reduces_over_periods(subject, visiting)
                    || arms.iter().any(|(pattern, value)| {
                        self.scalar_reduces_over_periods(pattern, visiting)
                            || self.scalar_reduces_over_periods(value, visiting)
                    })
            }
        }
    }

    fn judgment_reduces_over_periods(
        &self,
        expr: &CompiledJudgmentExpr,
        visiting: &mut HashSet<usize>,
    ) -> bool {
        match expr {
            CompiledJudgmentExpr::Comparison { left, right, .. } => {
                self.scalar_reduces_over_periods(left, visiting)
                    || self.scalar_reduces_over_periods(right, visiting)
            }
            CompiledJudgmentExpr::Derived(index) => {
                self.derived_reduces_over_periods_inner(*index, visiting)
            }
            CompiledJudgmentExpr::And(items) | CompiledJudgmentExpr::Or(items) => items
                .iter()
                .any(|item| self.judgment_reduces_over_periods(item, visiting)),
            CompiledJudgmentExpr::Not(item) => self.judgment_reduces_over_periods(item, visiting),
        }
    }

    fn bind_batch(&self, batch: DenseBatchSpec) -> Result<DenseBoundBatch, EvalError> {
        for (name, column) in &batch.inputs {
            if column.len() != batch.row_count {
                return Err(EvalError::TypeMismatch(format!(
                    "dense root input `{name}` has length {} but row_count is {}",
                    column.len(),
                    batch.row_count
                )));
            }
        }

        let bound_inputs = self
            .root_inputs
            .iter()
            .enumerate()
            .map(|(index, name)| match batch.inputs.get(name).cloned() {
                Some(column) => Ok(Some(column)),
                None if self.optional_root_inputs.contains(&index) => Ok(None),
                None => Err(EvalError::MissingInput {
                    name: name.clone(),
                    entity_id: self.root_entity.clone(),
                    period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                    period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                }),
            })
            .collect::<Result<Vec<Option<DenseColumn>>, EvalError>>()?;

        let mut bound_relations = Vec::with_capacity(self.relations.len());
        for (relation_index, relation) in self.relations.iter().enumerate() {
            let relation_batch = batch.relations.get(&relation.key).ok_or_else(|| {
                EvalError::UnknownRelation(format!(
                    "{}::{}/{}/{}",
                    relation.key.name,
                    relation.key.current_slot,
                    relation.key.related_slot,
                    self.root_entity
                ))
            })?;

            if relation_batch.offsets.len() != batch.row_count + 1 {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must have length {}",
                    relation.key.name,
                    batch.row_count + 1
                )));
            }
            if relation_batch.offsets.first().copied().unwrap_or_default() != 0 {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must start at 0",
                    relation.key.name
                )));
            }
            if !relation_batch
                .offsets
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
            {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must be non-decreasing",
                    relation.key.name
                )));
            }

            let related_count = *relation_batch.offsets.last().unwrap_or(&0);
            let optional_for_relation = &self.optional_related_inputs[relation_index];
            let bound_inputs = relation
                .related_inputs
                .iter()
                .enumerate()
                .map(|(input_index, name)| {
                    let column = match relation_batch.inputs.get(name).cloned() {
                        Some(column) => Some(column),
                        None if optional_for_relation.contains(&input_index) => None,
                        None => {
                            return Err(EvalError::MissingInput {
                                name: name.clone(),
                                entity_id: relation.key.name.clone(),
                                period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1)
                                    .expect("date"),
                                period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1)
                                    .expect("date"),
                            });
                        }
                    };
                    if let Some(column) = &column {
                        if column.len() != related_count {
                            return Err(EvalError::TypeMismatch(format!(
                                "dense relation input `{}` for `{}` has length {} but related row count is {}",
                                name,
                                relation.key.name,
                                column.len(),
                                related_count
                            )));
                        }
                    }
                    Ok(column)
                })
                .collect::<Result<Vec<Option<DenseColumn>>, EvalError>>()?;

            bound_relations.push(DenseRelationBatch {
                offsets: relation_batch.offsets.clone(),
                related_count,
                inputs: bound_inputs,
            });
        }

        Ok(DenseBoundBatch {
            row_count: batch.row_count,
            inputs: bound_inputs,
            relations: bound_relations,
        })
    }
}

struct DenseCompiler<'a> {
    program: &'a Program,
    root_entity: String,
    root_inputs: Vec<String>,
    root_input_index: HashMap<String, usize>,
    /// Root input indices that have only ever been referenced via
    /// `input_or_else`. If a bare `input` reference lands later, the index
    /// is evicted.
    optional_root_inputs: HashSet<usize>,
    relations: Vec<DenseRelationSchema>,
    relation_index: HashMap<DenseRelationKey, usize>,
    relation_input_index: HashMap<(usize, String), usize>,
    /// Per-relation, related-input indices that have only ever been referenced
    /// via `input_or_else` inside a `where` predicate.
    optional_related_inputs: Vec<HashSet<usize>>,
    parameters: Vec<CompiledParameter>,
    parameter_index: HashMap<String, usize>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
    visiting: HashSet<String>,
}

impl<'a> DenseCompiler<'a> {
    fn new(program: &'a Program, root_entity: String) -> Result<Self, DenseCompileError> {
        Ok(Self {
            program,
            root_entity,
            root_inputs: Vec::new(),
            root_input_index: HashMap::new(),
            optional_root_inputs: HashSet::new(),
            relations: Vec::new(),
            relation_index: HashMap::new(),
            relation_input_index: HashMap::new(),
            optional_related_inputs: Vec::new(),
            parameters: Vec::new(),
            parameter_index: HashMap::new(),
            derived: Vec::new(),
            derived_index: HashMap::new(),
            visiting: HashSet::new(),
        })
    }

    fn finish(self) -> DenseCompiledProgram {
        DenseCompiledProgram {
            root_entity: self.root_entity,
            root_inputs: self.root_inputs,
            optional_root_inputs: self.optional_root_inputs,
            relations: self.relations,
            optional_related_inputs: self.optional_related_inputs,
            parameters: self.parameters,
            derived: self.derived,
            derived_index: self.derived_index,
        }
    }

    fn compile_derived(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        if let Some(&index) = self.derived_index.get(name) {
            return Ok(index);
        }
        if self.visiting.contains(name) {
            return Err(DenseCompileError::Unsupported(format!(
                "cyclic dense compilation dependency involving `{name}`"
            )));
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
                )));
            }
        };
        if derived.entity != self.root_entity && derived.entity != SCALAR_ENTITY {
            return Err(DenseCompileError::CrossEntityDependency {
                derived: name.to_string(),
                dependency: name.to_string(),
                entity: derived.entity.clone(),
            });
        }

        let (source_semantics, effective_from) = match versioned_semantics {
            Some((semantics, from)) => (semantics, Some(from)),
            None => (&derived.semantics, None),
        };

        self.visiting.insert(name.to_string());
        let compiled_semantics = match source_semantics {
            DerivedSemantics::Scalar(expr) => {
                CompiledSemantics::Scalar(self.compile_scalar_expr(name, expr)?)
            }
            DerivedSemantics::Judgment(expr) => {
                CompiledSemantics::Judgment(self.compile_judgment_expr(name, expr)?)
            }
        };
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

    /// How explain names `rule` when an error leaves it: by its id when it has
    /// one. An empty `rule` (a derived relation's predicate) stays empty.
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
        MatchLabels::new(&self.rule_label(rule), subject, patterns)
    }

    fn compile_scalar_expr(
        &mut self,
        derived_name: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name, false))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name, true),
                default: default.clone(),
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
                    });
                }
                Ok(CompiledScalarExpr::Derived(self.compile_derived(name)?))
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
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
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
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
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
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let relation_index = self.relation(relation, *current_slot, *related_slot)?;
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| {
                        self.compile_related_predicate(relation_index, derived_name, inner)
                    })
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
                let value = match value {
                    RelatedValueRef::Input(name) => {
                        let input_index = self.related_input(relation_index, name, false);
                        CompiledRelatedScalarExpr::Input(input_index)
                    }
                    RelatedValueRef::Derived(name) => self.compile_related_scalar(
                        relation_index,
                        derived_name,
                        &ScalarExpr::Derived(name.clone()),
                    )?,
                };
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| {
                        self.compile_related_predicate(relation_index, derived_name, inner)
                    })
                    .transpose()?;
                Ok(CompiledScalarExpr::SumRelated {
                    relation: relation_index,
                    value: Box::new(value),
                    predicate,
                })
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                if let Some(chain) = match_chain(condition, then_expr, else_expr) {
                    let mut arms = Vec::with_capacity(chain.patterns.len());
                    for (pattern, value) in chain.patterns.iter().zip(chain.values) {
                        arms.push((
                            self.compile_scalar_expr(derived_name, pattern)?,
                            self.compile_scalar_expr(derived_name, value)?,
                        ));
                    }
                    return Ok(CompiledScalarExpr::Match {
                        subject: Box::new(self.compile_scalar_expr(derived_name, chain.subject)?),
                        arms,
                        labels: self.match_labels(derived_name, chain.subject, chain.patterns),
                    });
                }
                if matches!(else_expr.as_ref(), ScalarExpr::NoMatch { .. }) {
                    return Err(DenseCompileError::Unsupported(
                        NO_MATCH_OUTSIDE_CHAIN.to_string(),
                    ));
                }
                Ok(CompiledScalarExpr::If {
                    condition: Box::new(self.compile_judgment_expr(derived_name, condition)?),
                    then_expr: Box::new(self.compile_scalar_expr(derived_name, then_expr)?),
                    else_expr: Box::new(self.compile_scalar_expr(derived_name, else_expr)?),
                })
            }
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_CHAIN.to_string(),
            )),
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
                    });
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
    ) -> Result<CompiledJudgmentExpr, DenseCompileError> {
        match expr {
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
                    });
                }
                Ok(CompiledJudgmentExpr::Derived(self.compile_derived(name)?))
            }
            JudgmentExpr::RelationMember { relation, .. } => Err(DenseCompileError::Unsupported(
                format!("relation predicate `{relation}`"),
            )),
            JudgmentExpr::And(items) => Ok(CompiledJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledJudgmentExpr>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledJudgmentExpr>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledJudgmentExpr::Not(Box::new(
                self.compile_judgment_expr(derived_name, item)?,
            ))),
        }
    }

    /// Compile a predicate over a relation's related rows. `rule` is the rule
    /// whose formula (after inlining) contains `expr`, which a `match` inside
    /// names when it fails.
    fn compile_related_predicate(
        &mut self,
        relation_index: usize,
        rule: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                Ok(CompiledRelatedJudgmentExpr::Comparison {
                    left: self.compile_related_scalar(relation_index, rule, left)?,
                    op: *op,
                    right: self.compile_related_scalar(relation_index, rule, right)?,
                })
            }
            JudgmentExpr::Derived(name) => {
                let derived = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown related judgment dependency `{name}`"
                    ))
                })?;
                let relation = &self.relations[relation_index];
                if relation.current_entity.as_deref() == Some(derived.entity.as_str())
                    || derived.entity == self.root_entity
                {
                    return match &derived.semantics {
                        DerivedSemantics::Judgment(expr) => {
                            Ok(CompiledRelatedJudgmentExpr::RootJudgment(Box::new(
                                self.compile_current_judgment_expr(name, &derived.entity, expr)?,
                            )))
                        }
                        DerivedSemantics::Scalar(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "where-clause predicates cannot reference scalar derived values (`{name}`)"
                            )))
                        }
                    };
                }
                if relation.related_entity.is_some()
                    && relation.related_entity.as_deref() != Some(derived.entity.as_str())
                {
                    return Err(DenseCompileError::Unsupported(format!(
                        "related predicate `{name}` has entity `{}`, which is neither current nor related for relation `{}`",
                        derived.entity, relation.key.name
                    )));
                }
                match &derived.semantics {
                    DerivedSemantics::Judgment(expr) => {
                        self.compile_related_predicate(relation_index, name, expr)
                    }
                    DerivedSemantics::Scalar(_) => Err(DenseCompileError::Unsupported(format!(
                        "where-clause predicates cannot reference scalar derived values (`{name}`)"
                    ))),
                }
            }
            JudgmentExpr::RelationMember { .. } => Ok(CompiledRelatedJudgmentExpr::Literal(true)),
            JudgmentExpr::And(items) => Ok(CompiledRelatedJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, rule, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledRelatedJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, rule, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledRelatedJudgmentExpr::Not(Box::new(
                self.compile_related_predicate(relation_index, rule, item)?,
            ))),
        }
    }

    /// Compile a scalar over a relation's related rows; `rule` is as for
    /// [`Self::compile_related_predicate`].
    fn compile_related_scalar(
        &mut self,
        relation_index: usize,
        rule: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledRelatedScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => {
                let input_index = self.related_input(relation_index, name, false);
                Ok(CompiledRelatedScalarExpr::Input(input_index))
            }
            ScalarExpr::InputOrElse { name, default } => {
                let input_index = self.related_input(relation_index, name, true);
                Ok(CompiledRelatedScalarExpr::InputOrElse {
                    input: input_index,
                    default: default.clone(),
                })
            }
            ScalarExpr::Derived(name) => {
                let derived = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown related scalar dependency `{name}`"
                    ))
                })?;
                let relation = &self.relations[relation_index];
                if relation.current_entity.as_deref() == Some(derived.entity.as_str())
                    || derived.entity == self.root_entity
                    || derived.entity == SCALAR_ENTITY
                {
                    return match &derived.semantics {
                        DerivedSemantics::Scalar(expr) => {
                            Ok(CompiledRelatedScalarExpr::RootScalar(Box::new(
                                self.compile_current_scalar_expr(name, &derived.entity, expr)?,
                            )))
                        }
                        DerivedSemantics::Judgment(_) => {
                            Err(DenseCompileError::Unsupported(format!(
                                "related scalar expressions cannot reference judgment derived values (`{name}`)"
                            )))
                        }
                    };
                }
                if relation.related_entity.is_some()
                    && relation.related_entity.as_deref() != Some(derived.entity.as_str())
                {
                    return Err(DenseCompileError::Unsupported(format!(
                        "related scalar `{name}` has entity `{}`, which is neither current nor related for relation `{}`",
                        derived.entity, relation.key.name
                    )));
                }
                match &derived.semantics {
                    DerivedSemantics::Scalar(expr) => {
                        self.compile_related_scalar(relation_index, name, expr)
                    }
                    DerivedSemantics::Judgment(_) => Err(DenseCompileError::Unsupported(format!(
                        "related scalar expressions cannot reference judgment derived values (`{name}`)"
                    ))),
                }
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                Ok(CompiledRelatedScalarExpr::ParameterLookup {
                    parameter: self.parameter(parameter)?,
                    index: Box::new(self.compile_related_scalar(relation_index, rule, index)?),
                })
            }
            ScalarExpr::Add(items) => Ok(CompiledRelatedScalarExpr::Add(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, rule, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Sub(left, right) => Ok(CompiledRelatedScalarExpr::Sub(
                Box::new(self.compile_related_scalar(relation_index, rule, left)?),
                Box::new(self.compile_related_scalar(relation_index, rule, right)?),
            )),
            ScalarExpr::Mul(left, right) => Ok(CompiledRelatedScalarExpr::Mul(
                Box::new(self.compile_related_scalar(relation_index, rule, left)?),
                Box::new(self.compile_related_scalar(relation_index, rule, right)?),
            )),
            ScalarExpr::Div(left, right) => Ok(CompiledRelatedScalarExpr::Div(
                Box::new(self.compile_related_scalar(relation_index, rule, left)?),
                Box::new(self.compile_related_scalar(relation_index, rule, right)?),
            )),
            ScalarExpr::Max(items) => Ok(CompiledRelatedScalarExpr::Max(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, rule, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledRelatedScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_related_scalar(relation_index, rule, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            ScalarExpr::Ceil(value) => Ok(CompiledRelatedScalarExpr::Ceil(Box::new(
                self.compile_related_scalar(relation_index, rule, value)?,
            ))),
            ScalarExpr::Floor(value) => Ok(CompiledRelatedScalarExpr::Floor(Box::new(
                self.compile_related_scalar(relation_index, rule, value)?,
            ))),
            ScalarExpr::PeriodStart => Ok(CompiledRelatedScalarExpr::PeriodStart),
            ScalarExpr::PeriodEnd => Ok(CompiledRelatedScalarExpr::PeriodEnd),
            ScalarExpr::DateAddDays { date, days } => Ok(CompiledRelatedScalarExpr::DateAddDays {
                date: Box::new(self.compile_related_scalar(relation_index, rule, date)?),
                days: Box::new(self.compile_related_scalar(relation_index, rule, days)?),
            }),
            ScalarExpr::DateAddMonths { date, months } => {
                Ok(CompiledRelatedScalarExpr::DateAddMonths {
                    date: Box::new(self.compile_related_scalar(relation_index, rule, date)?),
                    months: Box::new(self.compile_related_scalar(relation_index, rule, months)?),
                })
            }
            ScalarExpr::DateAddYears { date, years } => {
                Ok(CompiledRelatedScalarExpr::DateAddYears {
                    date: Box::new(self.compile_related_scalar(relation_index, rule, date)?),
                    years: Box::new(self.compile_related_scalar(relation_index, rule, years)?),
                })
            }
            ScalarExpr::DaysBetween { from, to } => Ok(CompiledRelatedScalarExpr::DaysBetween {
                from: Box::new(self.compile_related_scalar(relation_index, rule, from)?),
                to: Box::new(self.compile_related_scalar(relation_index, rule, to)?),
            }),
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                if let Some(chain) = match_chain(condition, then_expr, else_expr) {
                    let mut arms = Vec::with_capacity(chain.patterns.len());
                    for (pattern, value) in chain.patterns.iter().zip(chain.values) {
                        arms.push((
                            self.compile_related_scalar(relation_index, rule, pattern)?,
                            self.compile_related_scalar(relation_index, rule, value)?,
                        ));
                    }
                    return Ok(CompiledRelatedScalarExpr::Match {
                        subject: Box::new(self.compile_related_scalar(
                            relation_index,
                            rule,
                            chain.subject,
                        )?),
                        arms,
                        labels: self.match_labels(rule, chain.subject, chain.patterns),
                    });
                }
                if matches!(else_expr.as_ref(), ScalarExpr::NoMatch { .. }) {
                    return Err(DenseCompileError::Unsupported(
                        NO_MATCH_OUTSIDE_CHAIN.to_string(),
                    ));
                }
                Ok(CompiledRelatedScalarExpr::If {
                    condition: Box::new(self.compile_related_predicate(
                        relation_index,
                        rule,
                        condition,
                    )?),
                    then_expr: Box::new(self.compile_related_scalar(
                        relation_index,
                        rule,
                        then_expr,
                    )?),
                    else_expr: Box::new(self.compile_related_scalar(
                        relation_index,
                        rule,
                        else_expr,
                    )?),
                })
            }
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_CHAIN.to_string(),
            )),
            ScalarExpr::CountRelated { relation, .. } | ScalarExpr::SumRelated { relation, .. } => {
                Err(DenseCompileError::Unsupported(format!(
                    "aggregation over relation `{relation}` nested inside a related expression"
                )))
            }
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
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name, false))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name, true),
                default: default.clone(),
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
                match &dependency.semantics {
                    DerivedSemantics::Scalar(expr) => {
                        self.compile_current_scalar_expr(name, &dependency.entity, expr)
                    }
                    DerivedSemantics::Judgment(_) => Err(DenseCompileError::Unsupported(format!(
                        "scalar expression cannot reference judgment derived value (`{name}`)"
                    ))),
                }
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
            } => {
                if let Some(chain) = match_chain(condition, then_expr, else_expr) {
                    let mut arms = Vec::with_capacity(chain.patterns.len());
                    for (pattern, value) in chain.patterns.iter().zip(chain.values) {
                        arms.push((
                            self.compile_current_scalar_expr(derived_name, entity, pattern)?,
                            self.compile_current_scalar_expr(derived_name, entity, value)?,
                        ));
                    }
                    return Ok(CompiledScalarExpr::Match {
                        subject: Box::new(self.compile_current_scalar_expr(
                            derived_name,
                            entity,
                            chain.subject,
                        )?),
                        arms,
                        labels: self.match_labels(derived_name, chain.subject, chain.patterns),
                    });
                }
                if matches!(else_expr.as_ref(), ScalarExpr::NoMatch { .. }) {
                    return Err(DenseCompileError::Unsupported(
                        NO_MATCH_OUTSIDE_CHAIN.to_string(),
                    ));
                }
                Ok(CompiledScalarExpr::If {
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
                    else_expr: Box::new(self.compile_current_scalar_expr(
                        derived_name,
                        entity,
                        else_expr,
                    )?),
                })
            }
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_CHAIN.to_string(),
            )),
            ScalarExpr::CountRelated { .. } | ScalarExpr::SumRelated { .. } => {
                Err(DenseCompileError::Unsupported(
                    "current-entity derived relation predicates cannot aggregate another relation"
                        .to_string(),
                ))
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
        match expr {
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
                match &dependency.semantics {
                    DerivedSemantics::Judgment(expr) => {
                        self.compile_current_judgment_expr(name, &dependency.entity, expr)
                    }
                    DerivedSemantics::Scalar(_) => Err(DenseCompileError::Unsupported(format!(
                        "judgment expression cannot reference scalar derived value (`{name}`)"
                    ))),
                }
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

    fn root_input(&mut self, name: &str, optional: bool) -> usize {
        if let Some(&index) = self.root_input_index.get(name) {
            if !optional {
                self.optional_root_inputs.remove(&index);
            }
            return index;
        }
        let index = self.root_inputs.len();
        self.root_inputs.push(name.to_string());
        self.root_input_index.insert(name.to_string(), index);
        if optional {
            self.optional_root_inputs.insert(index);
        }
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
            Ok(index)
        } else {
            let relation = self.program.relations.get(name);
            if let Some(derivation) = relation.and_then(|relation| relation.derivation.as_ref()) {
                let source_key = DenseRelationKey {
                    name: derivation.source_relation.clone(),
                    current_slot: derivation.current_slot,
                    related_slot: derivation.related_slot,
                };
                let parent_relation = self
                    .program
                    .relations
                    .get(&derivation.source_relation)
                    .and_then(|relation| relation.derivation.as_ref())
                    .map(|_| {
                        self.relation(
                            &derivation.source_relation,
                            derivation.current_slot,
                            derivation.related_slot,
                        )
                    })
                    .transpose()?;
                let key = parent_relation
                    .map(|parent| self.relations[parent].key.clone())
                    .unwrap_or(source_key);
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
                    parent_relation,
                    filter: None,
                });
                self.optional_related_inputs.push(HashSet::new());
                self.relation_index.insert(lookup_key, index);
                let filter = self.compile_related_predicate(index, "", &derivation.predicate)?;
                self.relations[index].filter = Some(filter);
                return Ok(index);
            } else {
                let index = self.relations.len();
                let key = lookup_key.clone();
                self.relations.push(DenseRelationSchema {
                    key,
                    related_inputs: Vec::new(),
                    current_entity: None,
                    related_entity: None,
                    parent_relation: None,
                    filter: None,
                });
                self.optional_related_inputs.push(HashSet::new());
                self.relation_index.insert(lookup_key, index);
                Ok(index)
            }
        }
    }

    fn related_input(&mut self, relation: usize, name: &str, optional: bool) -> usize {
        if let Some(&index) = self.relation_input_index.get(&(relation, name.to_string())) {
            if !optional {
                self.optional_related_inputs[relation].remove(&index);
            }
            return index;
        }
        let index = self.relations[relation].related_inputs.len();
        self.relations[relation]
            .related_inputs
            .push(name.to_string());
        self.relation_input_index
            .insert((relation, name.to_string()), index);
        if optional {
            self.optional_related_inputs[relation].insert(index);
        }
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
        self.parameters.push(CompiledParameter {
            parameter: parameter.clone(),
        });
        self.parameter_index.insert(name.to_string(), index);
        Ok(index)
    }
}

struct DenseExecutor<'a, N: DenseNum> {
    program: &'a DenseCompiledProgram,
    period: &'a Period,
    batch: DenseBoundBatch,
    scalar_cache: Vec<Option<Evaluated<DenseColumn>>>,
    judgment_cache: Vec<Option<Evaluated<Vec<JudgmentOutcome>>>>,
    _numeric_mode: std::marker::PhantomData<N>,
}

impl<'a, N: DenseNum> DenseExecutor<'a, N> {
    fn new(program: &'a DenseCompiledProgram, period: &'a Period, batch: DenseBoundBatch) -> Self {
        Self {
            program,
            period,
            scalar_cache: vec![None; program.derived.len()],
            judgment_cache: vec![None; program.derived.len()],
            batch,
            _numeric_mode: std::marker::PhantomData,
        }
    }

    fn evaluate_scalar(
        &mut self,
        derived_index: usize,
    ) -> Result<&Evaluated<DenseColumn>, EvalError> {
        if self.scalar_cache[derived_index].is_none() {
            // Borrow the expression tree through the program reference (not
            // through `self`) so evaluation can take `&mut self` without
            // cloning the tree.
            let program = self.program;
            let derived = &program.derived[derived_index];
            let mut column = match &derived.semantics {
                CompiledSemantics::Scalar(expr) => self
                    .eval_scalar_expr(expr)
                    .map_err(|error| error.within_rule(&derived.label))?,
                CompiledSemantics::Judgment(_) => {
                    return Err(EvalError::ExpectedScalar(derived.name.clone()));
                }
            };
            // Opt-in output rounding, applied to the evaluated column before
            // caching so both direct outputs and dependent rules see rounded
            // values — the columnar mirror of the explain path. Rounds in the
            // executor's numeric mode `N` (exact for Decimal, best-effort for
            // f64), matching the arithmetic the column was computed in.
            if let Some(rounding) = derived.rounding {
                column.values = round_dense_column::<N>(column.values, rounding)?;
            }
            column.errors = column.errors.within_rule(&derived.label);
            self.scalar_cache[derived_index] = Some(column);
        }
        Ok(self.scalar_cache[derived_index].as_ref().expect("cached"))
    }

    fn evaluate_judgment(
        &mut self,
        derived_index: usize,
    ) -> Result<&Evaluated<Vec<JudgmentOutcome>>, EvalError> {
        if self.judgment_cache[derived_index].is_none() {
            let program = self.program;
            let derived = &program.derived[derived_index];
            let mut values = match &derived.semantics {
                CompiledSemantics::Judgment(expr) => self
                    .eval_judgment_expr(expr)
                    .map_err(|error| error.within_rule(&derived.label))?,
                CompiledSemantics::Scalar(_) => {
                    return Err(EvalError::ExpectedJudgment(derived.name.clone()));
                }
            };
            values.errors = values.errors.within_rule(&derived.label);
            self.judgment_cache[derived_index] = Some(values);
        }
        Ok(self.judgment_cache[derived_index].as_ref().expect("cached"))
    }

    fn eval_scalar_expr(
        &mut self,
        expr: &CompiledScalarExpr,
    ) -> Result<Evaluated<DenseColumn>, EvalError> {
        match expr {
            CompiledScalarExpr::Literal(value) => Ok(Evaluated::ok(broadcast_scalar_literal::<N>(
                value,
                self.batch.row_count,
            ))),
            CompiledScalarExpr::Input(index) => self.batch.inputs[*index]
                .clone()
                .map(Evaluated::ok)
                .ok_or_else(|| EvalError::MissingInput {
                    name: self.program.root_inputs[*index].clone(),
                    entity_id: self.program.root_entity.clone(),
                    period_start: self.period.start,
                    period_end: self.period.end,
                }),
            CompiledScalarExpr::InputOrElse { input, default } => {
                Ok(Evaluated::ok(match &self.batch.inputs[*input] {
                    Some(column) => column.clone(),
                    None => broadcast_scalar_literal::<N>(default, self.batch.row_count),
                }))
            }
            CompiledScalarExpr::Derived(index) => Ok(self.evaluate_scalar(*index)?.clone()),
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                let keys = self.eval_scalar_expr(index)?.into_index_vec()?;
                lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    keys,
                    self.period,
                )
            }
            CompiledScalarExpr::Add(items) => {
                let mut total = Evaluated::ok(vec![N::ZERO; self.batch.row_count]);
                for item in items {
                    fold_numeric(&mut total, self.eval_scalar_expr(item)?, |total, value| {
                        *total += value
                    })?;
                }
                Ok(total.into_column())
            }
            CompiledScalarExpr::Sub(left, right) => {
                let left = numeric::<N>(self.eval_scalar_expr(left)?)?;
                let right = numeric::<N>(self.eval_scalar_expr(right)?)?;
                Ok(zip_numeric(left, right, |left, right| left - right))
            }
            CompiledScalarExpr::Mul(left, right) => {
                let left = numeric::<N>(self.eval_scalar_expr(left)?)?;
                let right = numeric::<N>(self.eval_scalar_expr(right)?)?;
                Ok(zip_numeric(left, right, |left, right| left * right))
            }
            CompiledScalarExpr::Div(left, right) => {
                let left = numeric::<N>(self.eval_scalar_expr(left)?)?;
                let right = numeric::<N>(self.eval_scalar_expr(right)?)?;
                divide_numeric(left, right)
            }
            CompiledScalarExpr::Max(items) => {
                let mut values = Evaluated::ok(vec![N::MIN; self.batch.row_count]);
                for item in items {
                    fold_numeric(&mut values, self.eval_scalar_expr(item)?, |best, value| {
                        if value > *best {
                            *best = value;
                        }
                    })?;
                }
                Ok(values.into_column())
            }
            CompiledScalarExpr::Min(items) => {
                let mut values = Evaluated::ok(vec![N::MAX; self.batch.row_count]);
                for item in items {
                    fold_numeric(&mut values, self.eval_scalar_expr(item)?, |best, value| {
                        if value < *best {
                            *best = value;
                        }
                    })?;
                }
                Ok(values.into_column())
            }
            CompiledScalarExpr::Ceil(value) => Ok(map_numeric(
                numeric::<N>(self.eval_scalar_expr(value)?)?,
                |value| value.ceil(),
            )),
            CompiledScalarExpr::Floor(value) => Ok(map_numeric(
                numeric::<N>(self.eval_scalar_expr(value)?)?,
                |value| value.floor(),
            )),
            CompiledScalarExpr::PeriodStart => Ok(Evaluated::ok(DenseColumn::Date(vec![
                self.period.start;
                self.batch.row_count
            ]))),
            CompiledScalarExpr::PeriodEnd => Ok(Evaluated::ok(DenseColumn::Date(vec![
                self.period.end;
                self.batch.row_count
            ]))),
            CompiledScalarExpr::DateAddDays { date, days } => {
                let date = self
                    .eval_scalar_expr(date)?
                    .try_map(|date| date.as_date_vec())?;
                let days = self.eval_scalar_expr(days)?.into_index_vec()?;
                Ok(add_days(date, days))
            }
            CompiledScalarExpr::DateAddMonths { date, months } => {
                let date = self
                    .eval_scalar_expr(date)?
                    .try_map(|date| date.as_date_vec())?;
                let months = self.eval_scalar_expr(months)?.into_index_vec()?;
                shift_dates(date, months, crate::engine::shift_calendar_months)
            }
            CompiledScalarExpr::DateAddYears { date, years } => {
                let date = self
                    .eval_scalar_expr(date)?
                    .try_map(|date| date.as_date_vec())?;
                let years = self.eval_scalar_expr(years)?.into_index_vec()?;
                shift_dates(date, years, crate::engine::shift_calendar_years)
            }
            CompiledScalarExpr::DaysBetween { from, to } => {
                let from = self
                    .eval_scalar_expr(from)?
                    .try_map(|from| from.as_date_vec())?;
                let to = self.eval_scalar_expr(to)?.try_map(|to| to.as_date_vec())?;
                Ok(days_between(from, to))
            }
            CompiledScalarExpr::CountRelated {
                relation,
                predicate,
            } => {
                let offsets = self.batch.relations[*relation].offsets.clone();
                let Evaluated {
                    values: mask,
                    errors,
                } = self.relation_mask(*relation, predicate.as_ref())?;
                let counts = if let Some(mask) = mask {
                    let mut counts = Vec::with_capacity(self.batch.row_count);
                    for row in 0..self.batch.row_count {
                        let start = offsets[row];
                        let end = offsets[row + 1];
                        let matched = mask[start..end].iter().filter(|keep| **keep).count() as i64;
                        counts.push(matched);
                    }
                    counts
                } else {
                    offsets
                        .windows(2)
                        .map(|pair| (pair[1] - pair[0]) as i64)
                        .collect()
                };
                Ok(Evaluated {
                    values: DenseColumn::Integer(counts),
                    errors: errors.gather(&offsets),
                })
            }
            CompiledScalarExpr::SumRelated {
                relation,
                value,
                predicate,
            } => {
                let offsets = self.batch.relations[*relation].offsets.clone();
                let values = numeric::<N>(self.resolve_related_scalar(*relation, value)?)?;
                let Evaluated {
                    values: mask,
                    errors: mask_errors,
                } = self.relation_mask(*relation, predicate.as_ref())?;
                let mut totals = Vec::with_capacity(self.batch.row_count);
                for row in 0..self.batch.row_count {
                    let start = offsets[row];
                    let end = offsets[row + 1];
                    let mut total = N::ZERO;
                    match &mask {
                        Some(mask) => {
                            for (offset, value) in values.values[start..end].iter().enumerate() {
                                if mask[start + offset] {
                                    total += *value;
                                }
                            }
                        }
                        None => {
                            for value in &values.values[start..end] {
                                total += *value;
                            }
                        }
                    }
                    totals.push(total);
                }
                // Explain reads a related entity's value only once it has
                // passed the relation's membership and `where` stages.
                let related_errors = mask_errors.or(values
                    .errors
                    .reached(|related| mask.as_ref().is_none_or(|mask| mask[related])));
                Ok(Evaluated {
                    values: N::into_column(totals),
                    errors: related_errors.gather(&offsets),
                })
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.eval_judgment_expr(condition)?;
                let then_values = self.eval_scalar_expr(then_expr)?;
                let else_values = self.eval_scalar_expr(else_expr)?;
                select_dense::<N>(condition, then_values, else_values)
            }
            CompiledScalarExpr::Match {
                subject,
                arms,
                labels,
            } => {
                let subject = self.eval_scalar_expr(subject)?;
                let mut evaluated = Vec::with_capacity(arms.len());
                for (pattern, value) in arms {
                    evaluated.push((
                        self.eval_scalar_expr(pattern)?,
                        self.eval_scalar_expr(value)?,
                    ));
                }
                select_match_arm::<N>(labels, subject, evaluated)
            }
            // Cross-period reductions require a batch per period; they are
            // evaluated by the lifetime executor, never here.
            CompiledScalarExpr::OverPeriods { kind, .. } => {
                Err(EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()))
            }
        }
    }

    fn relation_mask(
        &mut self,
        relation: usize,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
    ) -> Result<Evaluated<Option<Vec<bool>>>, EvalError> {
        let program = self.program;
        let parent_relation = program.relations[relation].parent_relation;
        let parent_mask = parent_relation
            .map(|parent| self.relation_mask(parent, None))
            .transpose()?;
        let base_mask = program.relations[relation]
            .filter
            .as_ref()
            .map(|predicate| self.eval_related_predicate(relation, predicate))
            .transpose()?;
        let predicate_mask = predicate
            .map(|predicate| self.eval_related_predicate(relation, predicate))
            .transpose()?;

        // The stages narrow in explain's order: the source relation's
        // membership, this relation's own filter, then the `where` clause.
        let mut mask = parent_mask.unwrap_or_else(|| Evaluated::ok(None));
        for stage in [base_mask, predicate_mask].into_iter().flatten() {
            mask = narrow_relation_mask(
                mask,
                Evaluated {
                    values: Some(stage.values),
                    errors: stage.errors,
                },
            );
        }
        Ok(mask)
    }

    fn eval_related_predicate(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedJudgmentExpr,
    ) -> Result<Evaluated<Vec<bool>>, EvalError> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedJudgmentExpr::Literal(value) => Ok(Evaluated::ok(vec![*value; length])),
            CompiledRelatedJudgmentExpr::Comparison { left, op, right } => {
                let left = self.resolve_related_scalar(relation, left)?;
                let right = self.resolve_related_scalar(relation, right)?;
                Ok(Evaluated {
                    values: compare_related_columns::<N>(&left.values, *op, &right.values)?,
                    errors: left.errors.or(right.errors),
                })
            }
            CompiledRelatedJudgmentExpr::RootJudgment(expr) => {
                let offsets = self.batch.relations[relation].offsets.clone();
                let values = self.eval_judgment_expr(expr)?;
                Ok(Evaluated {
                    values: project_root_judgment_to_related(&values.values, &offsets)?,
                    errors: values.errors.spread(&offsets),
                })
            }
            CompiledRelatedJudgmentExpr::And(items) => {
                let mut result = vec![true; length];
                let mut errors = RowErrors::default();
                for item in items {
                    let item = self.eval_related_predicate(relation, item)?;
                    // Explain stops at the first item that does not hold.
                    errors = errors.or(item.errors.reached(|row| result[row]));
                    for (index, keep) in item.values.into_iter().enumerate() {
                        result[index] &= keep;
                    }
                }
                Ok(Evaluated {
                    values: result,
                    errors,
                })
            }
            CompiledRelatedJudgmentExpr::Or(items) => {
                let mut result = vec![false; length];
                let mut errors = RowErrors::default();
                for item in items {
                    let item = self.eval_related_predicate(relation, item)?;
                    // Explain stops at the first item that holds.
                    errors = errors.or(item.errors.reached(|row| !result[row]));
                    for (index, keep) in item.values.into_iter().enumerate() {
                        result[index] |= keep;
                    }
                }
                Ok(Evaluated {
                    values: result,
                    errors,
                })
            }
            CompiledRelatedJudgmentExpr::Not(item) => {
                let item = self.eval_related_predicate(relation, item)?;
                Ok(Evaluated {
                    values: item.values.into_iter().map(|keep| !keep).collect(),
                    errors: item.errors,
                })
            }
        }
    }

    fn resolve_related_scalar(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedScalarExpr,
    ) -> Result<Evaluated<DenseColumn>, EvalError> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedScalarExpr::Literal(value) => {
                Ok(Evaluated::ok(broadcast_scalar_literal::<N>(value, length)))
            }
            CompiledRelatedScalarExpr::Input(index) => self.batch.relations[relation].inputs
                [*index]
                .clone()
                .map(Evaluated::ok)
                .ok_or_else(|| EvalError::MissingInput {
                    name: format!("related_input[{index}]"),
                    entity_id: String::new(),
                    period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                    period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                }),
            CompiledRelatedScalarExpr::InputOrElse { input, default } => Ok(Evaluated::ok(
                match &self.batch.relations[relation].inputs[*input] {
                    Some(column) => column.clone(),
                    None => broadcast_scalar_literal::<N>(default, length),
                },
            )),
            CompiledRelatedScalarExpr::RootScalar(expr) => {
                let offsets = self.batch.relations[relation].offsets.clone();
                let values = self.eval_scalar_expr(expr)?;
                Ok(Evaluated {
                    values: project_root_column_to_related(&values.values, &offsets)?,
                    errors: values.errors.spread(&offsets),
                })
            }
            CompiledRelatedScalarExpr::ParameterLookup { parameter, index } => {
                let keys = self
                    .resolve_related_scalar(relation, index)?
                    .into_index_vec()?;
                lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    keys,
                    self.period,
                )
            }
            CompiledRelatedScalarExpr::Add(items) => {
                let mut total = Evaluated::ok(vec![N::ZERO; length]);
                for item in items {
                    fold_numeric(
                        &mut total,
                        self.resolve_related_scalar(relation, item)?,
                        |total, value| *total += value,
                    )?;
                }
                Ok(total.into_column())
            }
            CompiledRelatedScalarExpr::Sub(left, right) => {
                let left = numeric::<N>(self.resolve_related_scalar(relation, left)?)?;
                let right = numeric::<N>(self.resolve_related_scalar(relation, right)?)?;
                Ok(zip_numeric(left, right, |left, right| left - right))
            }
            CompiledRelatedScalarExpr::Mul(left, right) => {
                let left = numeric::<N>(self.resolve_related_scalar(relation, left)?)?;
                let right = numeric::<N>(self.resolve_related_scalar(relation, right)?)?;
                Ok(zip_numeric(left, right, |left, right| left * right))
            }
            CompiledRelatedScalarExpr::Div(left, right) => {
                let left = numeric::<N>(self.resolve_related_scalar(relation, left)?)?;
                let right = numeric::<N>(self.resolve_related_scalar(relation, right)?)?;
                divide_numeric(left, right)
            }
            CompiledRelatedScalarExpr::Max(items) => {
                let mut values = Evaluated::ok(vec![N::MIN; length]);
                for item in items {
                    fold_numeric(
                        &mut values,
                        self.resolve_related_scalar(relation, item)?,
                        |best, value| {
                            if value > *best {
                                *best = value;
                            }
                        },
                    )?;
                }
                Ok(values.into_column())
            }
            CompiledRelatedScalarExpr::Min(items) => {
                let mut values = Evaluated::ok(vec![N::MAX; length]);
                for item in items {
                    fold_numeric(
                        &mut values,
                        self.resolve_related_scalar(relation, item)?,
                        |best, value| {
                            if value < *best {
                                *best = value;
                            }
                        },
                    )?;
                }
                Ok(values.into_column())
            }
            CompiledRelatedScalarExpr::Ceil(value) => Ok(map_numeric(
                numeric::<N>(self.resolve_related_scalar(relation, value)?)?,
                |value| value.ceil(),
            )),
            CompiledRelatedScalarExpr::Floor(value) => Ok(map_numeric(
                numeric::<N>(self.resolve_related_scalar(relation, value)?)?,
                |value| value.floor(),
            )),
            CompiledRelatedScalarExpr::PeriodStart => Ok(Evaluated::ok(DenseColumn::Date(vec![
                    self.period.start;
                    length
                ]))),
            CompiledRelatedScalarExpr::PeriodEnd => Ok(Evaluated::ok(DenseColumn::Date(vec![
                    self.period.end;
                    length
                ]))),
            CompiledRelatedScalarExpr::DateAddDays { date, days } => {
                let date = self
                    .resolve_related_scalar(relation, date)?
                    .try_map(|date| date.as_date_vec())?;
                let days = self
                    .resolve_related_scalar(relation, days)?
                    .into_index_vec()?;
                Ok(add_days(date, days))
            }
            CompiledRelatedScalarExpr::DateAddMonths { date, months } => {
                let date = self
                    .resolve_related_scalar(relation, date)?
                    .try_map(|date| date.as_date_vec())?;
                let months = self
                    .resolve_related_scalar(relation, months)?
                    .into_index_vec()?;
                shift_dates(date, months, crate::engine::shift_calendar_months)
            }
            CompiledRelatedScalarExpr::DateAddYears { date, years } => {
                let date = self
                    .resolve_related_scalar(relation, date)?
                    .try_map(|date| date.as_date_vec())?;
                let years = self
                    .resolve_related_scalar(relation, years)?
                    .into_index_vec()?;
                shift_dates(date, years, crate::engine::shift_calendar_years)
            }
            CompiledRelatedScalarExpr::DaysBetween { from, to } => {
                let from = self
                    .resolve_related_scalar(relation, from)?
                    .try_map(|from| from.as_date_vec())?;
                let to = self
                    .resolve_related_scalar(relation, to)?
                    .try_map(|to| to.as_date_vec())?;
                Ok(days_between(from, to))
            }
            CompiledRelatedScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let Evaluated {
                    values: condition,
                    errors: condition_errors,
                } = self.eval_related_predicate(relation, condition)?;
                let then_values = self.resolve_related_scalar(relation, then_expr)?;
                let else_values = self.resolve_related_scalar(relation, else_expr)?;
                let errors = RowErrors::select(
                    condition_errors,
                    |row| condition[row],
                    then_values.errors,
                    else_values.errors,
                );
                Ok(Evaluated {
                    values: select_related_scalar_column::<N>(
                        &condition,
                        then_values.values,
                        else_values.values,
                    )?,
                    errors,
                })
            }
            CompiledRelatedScalarExpr::Match {
                subject,
                arms,
                labels,
            } => {
                let subject = self.resolve_related_scalar(relation, subject)?;
                let mut evaluated = Vec::with_capacity(arms.len());
                for (pattern, value) in arms {
                    evaluated.push((
                        self.resolve_related_scalar(relation, pattern)?,
                        self.resolve_related_scalar(relation, value)?,
                    ));
                }
                select_match_arm::<N>(labels, subject, evaluated)
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &CompiledJudgmentExpr,
    ) -> Result<Evaluated<Vec<JudgmentOutcome>>, EvalError> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let left = self.eval_scalar_expr(left)?;
                let right = self.eval_scalar_expr(right)?;
                Ok(Evaluated {
                    values: compare_dense_columns::<N>(left.values, *op, right.values)?,
                    errors: left.errors.or(right.errors),
                })
            }
            CompiledJudgmentExpr::Derived(index) => Ok(self.evaluate_judgment(*index)?.clone()),
            CompiledJudgmentExpr::And(items) => {
                let mut combined = all_hold(self.batch.row_count);
                for item in items {
                    combined = and_judgments(combined, self.eval_judgment_expr(item)?);
                }
                Ok(combined)
            }
            CompiledJudgmentExpr::Or(items) => {
                let mut combined = none_hold(self.batch.row_count);
                for item in items {
                    combined = or_judgments(combined, self.eval_judgment_expr(item)?);
                }
                Ok(combined)
            }
            CompiledJudgmentExpr::Not(item) => Ok(not_judgment(self.eval_judgment_expr(item)?)),
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
    scalar_cache: Vec<Option<Evaluated<DenseColumn>>>,
    judgment_cache: Vec<Option<Evaluated<Vec<JudgmentOutcome>>>>,
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
            scalar_cache: vec![None; program.derived.len()],
            judgment_cache: vec![None; program.derived.len()],
        }
    }

    fn evaluate_scalar(
        &mut self,
        derived_index: usize,
    ) -> Result<&Evaluated<DenseColumn>, EvalError> {
        if self.scalar_cache[derived_index].is_none() {
            let program = self.program;
            let derived = &program.derived[derived_index];
            let mut column = match &derived.semantics {
                CompiledSemantics::Scalar(expr) => self
                    .eval_scalar(expr)
                    .map_err(|error| error.within_rule(&derived.label))?,
                CompiledSemantics::Judgment(_) => {
                    return Err(EvalError::ExpectedScalar(derived.name.clone()));
                }
            };
            if let Some(rounding) = derived.rounding {
                column.values = round_dense_column::<N>(column.values, rounding)?;
            }
            column.errors = column.errors.within_rule(&derived.label);
            self.scalar_cache[derived_index] = Some(column);
        }
        Ok(self.scalar_cache[derived_index].as_ref().expect("cached"))
    }

    fn evaluate_judgment(
        &mut self,
        derived_index: usize,
    ) -> Result<&Evaluated<Vec<JudgmentOutcome>>, EvalError> {
        if self.judgment_cache[derived_index].is_none() {
            let program = self.program;
            let derived = &program.derived[derived_index];
            let mut values = match &derived.semantics {
                CompiledSemantics::Judgment(expr) => self
                    .eval_judgment(expr)
                    .map_err(|error| error.within_rule(&derived.label))?,
                CompiledSemantics::Scalar(_) => {
                    return Err(EvalError::ExpectedJudgment(derived.name.clone()));
                }
            };
            values.errors = values.errors.within_rule(&derived.label);
            self.judgment_cache[derived_index] = Some(values);
        }
        Ok(self.judgment_cache[derived_index].as_ref().expect("cached"))
    }

    fn eval_scalar(
        &mut self,
        expr: &CompiledScalarExpr,
    ) -> Result<Evaluated<DenseColumn>, EvalError> {
        match expr {
            CompiledScalarExpr::OverPeriods { kind, value, n } => {
                self.eval_over_periods(*kind, value, n.as_deref())
            }
            CompiledScalarExpr::Literal(value) => Ok(Evaluated::ok(broadcast_scalar_literal::<N>(
                value,
                self.row_count,
            ))),
            CompiledScalarExpr::Derived(index) => Ok(self.evaluate_scalar(*index)?.clone()),
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                // Period-specific: resolve at the reference period. The index
                // expression is itself lifetime-evaluated (typically a literal
                // or derived), then used as integer keys.
                let keys = self.eval_scalar(index)?.into_index_vec()?;
                lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    keys,
                    self.period_executors[self.reference_period].period,
                )
            }
            CompiledScalarExpr::Add(items) => {
                let mut total = Evaluated::ok(vec![N::ZERO; self.row_count]);
                for item in items {
                    fold_numeric(&mut total, self.eval_scalar(item)?, |total, value| {
                        *total += value
                    })?;
                }
                Ok(total.into_column())
            }
            CompiledScalarExpr::Sub(left, right) => {
                let left = numeric::<N>(self.eval_scalar(left)?)?;
                let right = numeric::<N>(self.eval_scalar(right)?)?;
                Ok(zip_numeric(left, right, |l, r| l - r))
            }
            CompiledScalarExpr::Mul(left, right) => {
                let left = numeric::<N>(self.eval_scalar(left)?)?;
                let right = numeric::<N>(self.eval_scalar(right)?)?;
                Ok(zip_numeric(left, right, |l, r| l * r))
            }
            CompiledScalarExpr::Div(left, right) => {
                let left = numeric::<N>(self.eval_scalar(left)?)?;
                let right = numeric::<N>(self.eval_scalar(right)?)?;
                divide_numeric(left, right)
            }
            CompiledScalarExpr::Max(items) => {
                let mut values = Evaluated::ok(vec![N::MIN; self.row_count]);
                for item in items {
                    fold_numeric(&mut values, self.eval_scalar(item)?, |best, value| {
                        if value > *best {
                            *best = value;
                        }
                    })?;
                }
                Ok(values.into_column())
            }
            CompiledScalarExpr::Min(items) => {
                let mut values = Evaluated::ok(vec![N::MAX; self.row_count]);
                for item in items {
                    fold_numeric(&mut values, self.eval_scalar(item)?, |best, value| {
                        if value < *best {
                            *best = value;
                        }
                    })?;
                }
                Ok(values.into_column())
            }
            CompiledScalarExpr::Ceil(value) => Ok(map_numeric(
                numeric::<N>(self.eval_scalar(value)?)?,
                |value| value.ceil(),
            )),
            CompiledScalarExpr::Floor(value) => Ok(map_numeric(
                numeric::<N>(self.eval_scalar(value)?)?,
                |value| value.floor(),
            )),
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.eval_judgment(condition)?;
                let then_values = self.eval_scalar(then_expr)?;
                let else_values = self.eval_scalar(else_expr)?;
                select_dense::<N>(condition, then_values, else_values)
            }
            CompiledScalarExpr::Match {
                subject,
                arms,
                labels,
            } => {
                let subject = self.eval_scalar(subject)?;
                let mut evaluated = Vec::with_capacity(arms.len());
                for (pattern, value) in arms {
                    evaluated.push((self.eval_scalar(pattern)?, self.eval_scalar(value)?));
                }
                select_match_arm::<N>(labels, subject, evaluated)
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
                self.eval_period_invariant_input(expr, &name)
                    .map(Evaluated::ok)
            }
            CompiledScalarExpr::InputOrElse { input, .. } => {
                let name = self.program.root_inputs[*input].clone();
                self.eval_period_invariant_input(expr, &name)
                    .map(Evaluated::ok)
            }
            CompiledScalarExpr::PeriodStart => {
                Err(EvalError::LifetimeAmbiguousLeaf("period_start".to_string()))
            }
            CompiledScalarExpr::PeriodEnd => {
                Err(EvalError::LifetimeAmbiguousLeaf("period_end".to_string()))
            }
            CompiledScalarExpr::DateAddDays { .. } => Err(EvalError::LifetimeAmbiguousLeaf(
                "date_add_days".to_string(),
            )),
            CompiledScalarExpr::DateAddMonths { .. } => Err(EvalError::LifetimeAmbiguousLeaf(
                "date_add_months".to_string(),
            )),
            CompiledScalarExpr::DateAddYears { .. } => Err(EvalError::LifetimeAmbiguousLeaf(
                "date_add_years".to_string(),
            )),
            CompiledScalarExpr::DaysBetween { .. } => {
                Err(EvalError::LifetimeAmbiguousLeaf("days_between".to_string()))
            }
            CompiledScalarExpr::CountRelated { .. } => Err(EvalError::LifetimeAmbiguousLeaf(
                "count/len over a relation".to_string(),
            )),
            CompiledScalarExpr::SumRelated { .. } => Err(EvalError::LifetimeAmbiguousLeaf(
                "sum over a relation".to_string(),
            )),
        }
    }

    /// Collapse an over-periods reduction to a per-entity column: evaluate the
    /// inner `value` under every period's executor, then reduce down the period
    /// axis for each row.
    ///
    /// A row fails if its inner value failed in any period (the reduction reads
    /// every period), with the error of the earliest such period.
    fn eval_over_periods(
        &mut self,
        kind: OverPeriodsKind,
        value: &CompiledScalarExpr,
        n: Option<&CompiledScalarExpr>,
    ) -> Result<Evaluated<DenseColumn>, EvalError> {
        let period_count = self.period_executors.len();

        // Count evaluates its argument per period (same leaf rules as the other
        // reductions — period-specific leaves are legal inside a reduction) and
        // counts, per row, the periods whose value is nonzero. This is
        // referentially consistent with the sibling reductions rather than a
        // special-cased period count: `count_over_periods(x)` means "how many
        // periods had a nonzero `x`". A Bool/judgment-shaped inner value counts
        // `true`; every numeric variant counts `!= 0`.
        if kind == OverPeriodsKind::Count {
            let mut counts = vec![0_i64; self.row_count];
            let mut errors = RowErrors::default();
            for executor in &mut self.period_executors {
                let column = executor.eval_scalar_expr(value)?;
                accumulate_nonzero_counts(&column.values, &mut counts)?;
                errors = errors.or(column.errors);
            }
            return Ok(Evaluated {
                values: DenseColumn::Integer(counts),
                errors,
            });
        }

        // period-major: per_period[p][r] is entity r's inner value in period p.
        let mut per_period: Vec<Vec<N>> = Vec::with_capacity(period_count);
        let mut errors = RowErrors::default();
        for executor in &mut self.period_executors {
            let column = executor.eval_scalar_expr(value)?;
            per_period.push(N::vec_from_column(&column.values)?);
            errors = errors.or(column.errors);
        }

        let result = match kind {
            // Handled above: count does not build the numeric period matrix.
            OverPeriodsKind::Count => unreachable!("count is handled above"),
            OverPeriodsKind::Sum => (0..self.row_count)
                .map(|row| {
                    let mut total = N::ZERO;
                    for period in &per_period {
                        total += period[row];
                    }
                    total
                })
                .collect::<Vec<N>>(),
            OverPeriodsKind::Max => (0..self.row_count)
                .map(|row| {
                    // period_count >= 1 is guaranteed by the caller.
                    let mut best = per_period[0][row];
                    for period in &per_period[1..] {
                        if period[row] > best {
                            best = period[row];
                        }
                    }
                    best
                })
                .collect::<Vec<N>>(),
            OverPeriodsKind::SumTopN => {
                // `eval_top_n_counts` enforces 1 <= n <= period_count for every
                // row, so `take` below never exceeds the number of periods: a
                // top-N sum is a mathematical no-op past the period count (extra
                // slots would only add zeros), so over-length n is rejected as a
                // likely data error rather than silently padded.
                let counts = self.eval_top_n_counts(n, period_count)?;
                errors = errors.or(counts.errors);
                let counts = counts.values;
                (0..self.row_count)
                    .map(|row| {
                        let mut values: Vec<N> =
                            per_period.iter().map(|period| period[row]).collect();
                        // Descending sort. N is Decimal or f64; per-period inner
                        // values are finite in practice, so a total order via
                        // partial_cmp is safe here.
                        values
                            .sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                        // n is bounded by the period count, so this takes a
                        // genuine prefix of the sorted values (no zero padding).
                        let take = counts[row];
                        let mut total = N::ZERO;
                        for value in &values[..take] {
                            total += *value;
                        }
                        total
                    })
                    .collect::<Vec<N>>()
            }
        };
        Ok(Evaluated {
            values: N::into_column(result),
            errors,
        })
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
    /// (which would be an arithmetic no-op masking the bad count).
    ///
    /// A row whose `n` failed in some period has no count to check: it keeps
    /// that failure (the earliest period's) and a placeholder count of 1.
    fn eval_top_n_counts(
        &mut self,
        n: Option<&CompiledScalarExpr>,
        period_count: usize,
    ) -> Result<Evaluated<Vec<usize>>, EvalError> {
        let n = n.ok_or_else(|| {
            EvalError::TypeMismatch("sum_top_n_over_periods requires an n argument".to_string())
        })?;
        let reduction = OverPeriodsKind::SumTopN.as_call_name();

        // Evaluate `n` under each period's executor: one column per period,
        // positionally aligned by row. A parameter-sourced n indexes each
        // period's date, so a year-varying parameter yields different columns and
        // is caught below; an n derived from period-invariant inputs (the
        // 42 USC 415(b) computation-year count) is identical in every period and
        // passes.
        let mut per_period: Vec<DenseColumn> = Vec::with_capacity(self.period_executors.len());
        let mut errors = RowErrors::default();
        for executor in &mut self.period_executors {
            let column = executor.eval_scalar_expr(n)?;
            per_period.push(column.values);
            errors = errors.or(column.errors);
        }
        let (reference, others) = per_period
            .split_last()
            .expect("lifetime execution guarantees at least one period");
        // Reject a period-varying n: compare every earlier period against the
        // reference (chronologically-last) period, in each column's own dtype.
        for (index, column) in others.iter().enumerate() {
            if let Some(row) = first_differing_row(reference, column, |row| errors.contains(row)) {
                return Err(EvalError::OverPeriodsTopNPeriodVarying {
                    reduction,
                    first_period: period_label(self.period_executors[index].period),
                    first_value: dense_value_label(column, row),
                    second_period: period_label(
                        self.period_executors[self.reference_period].period,
                    ),
                    second_value: dense_value_label(reference, row),
                });
            }
        }

        // n is period-invariant: truncate the reference column toward zero to an
        // exact i64 and enforce 1 <= n <= period_count per row.
        let raw = N::vec_from_column(reference)?;
        let counts = raw
            .into_iter()
            .enumerate()
            .map(|(row, value)| {
                if errors.contains(row) {
                    return Ok(1);
                }
                let as_i64 = value.try_to_i64_trunc().ok_or_else(|| {
                    // Non-finite or beyond the i64 range: a garbage n, reported
                    // as out of range rather than saturated to a clamp.
                    EvalError::OverPeriodsTopNOutOfRange {
                        reduction,
                        n: dense_value_label(reference, row),
                        period_count,
                    }
                })?;
                if as_i64 < 1 || (as_i64 as u64) > period_count as u64 {
                    Err(EvalError::OverPeriodsTopNOutOfRange {
                        reduction,
                        n: as_i64.to_string(),
                        period_count,
                    })
                } else {
                    Ok(as_i64 as usize)
                }
            })
            .collect::<Result<Vec<usize>, EvalError>>()?;
        Ok(Evaluated {
            values: counts,
            errors,
        })
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
    /// that single value is bound for it; if any row diverges, we error, naming
    /// the input and the first two differing period labels and values. Truly
    /// period-varying inputs therefore still fail loudly — the check only
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
    ) -> Result<DenseColumn, EvalError> {
        // Evaluate the leaf under every period's executor: one column per
        // period, each of length `row_count` and positionally aligned by row.
        let mut per_period: Vec<DenseColumn> = Vec::with_capacity(self.period_executors.len());
        for executor in &mut self.period_executors {
            // A bare input never fails per row, so only its values matter.
            per_period.push(executor.eval_scalar_expr(expr)?.values);
        }
        // A single period is invariant by definition; nothing to compare.
        let (first, rest) = per_period
            .split_first()
            .expect("lifetime execution guarantees at least one period");
        for (offset, column) in rest.iter().enumerate() {
            if let Some(row) = first_differing_row(first, column, |_| false) {
                // `offset` indexes `rest`, so the diverging period is `offset + 1`.
                let later_period = offset + 1;
                return Err(EvalError::LifetimePeriodVaryingInput {
                    input: input_name.to_string(),
                    first_period: period_label(self.period_executors[0].period),
                    first_value: dense_value_label(first, row),
                    second_period: period_label(self.period_executors[later_period].period),
                    second_value: dense_value_label(column, row),
                });
            }
        }
        // Every row agrees across all periods: bind the (identical) first
        // period's column, preserving its original dtype for the caller.
        Ok(first.clone())
    }

    fn eval_judgment(
        &mut self,
        expr: &CompiledJudgmentExpr,
    ) -> Result<Evaluated<Vec<JudgmentOutcome>>, EvalError> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let left = self.eval_scalar(left)?;
                let right = self.eval_scalar(right)?;
                Ok(Evaluated {
                    values: compare_dense_columns::<N>(left.values, *op, right.values)?,
                    errors: left.errors.or(right.errors),
                })
            }
            CompiledJudgmentExpr::Derived(index) => Ok(self.evaluate_judgment(*index)?.clone()),
            CompiledJudgmentExpr::And(items) => {
                let mut combined = all_hold(self.row_count);
                for item in items {
                    combined = and_judgments(combined, self.eval_judgment(item)?);
                }
                Ok(combined)
            }
            CompiledJudgmentExpr::Or(items) => {
                let mut combined = none_hold(self.row_count);
                for item in items {
                    combined = or_judgments(combined, self.eval_judgment(item)?);
                }
                Ok(combined)
            }
            CompiledJudgmentExpr::Not(item) => Ok(not_judgment(self.eval_judgment(item)?)),
        }
    }
}

/// A human-readable label for a period, used in period-invariance error
/// messages. The `start..end` boundaries identify the period unambiguously and
/// match how periods are otherwise surfaced to callers.
fn period_label(period: &Period) -> String {
    format!("in {}..{}", period.start, period.end)
}

/// Format the value at `row` of a dense column for an error message, in the
/// column's own dtype (so an integer year reads `1985`, not `1985.0`).
fn dense_value_label(column: &DenseColumn, row: usize) -> String {
    match column {
        DenseColumn::Bool(values) => values[row].to_string(),
        DenseColumn::Integer(values) => values[row].to_string(),
        DenseColumn::Decimal(values) => values[row].to_string(),
        DenseColumn::Float(values) => values[row].to_string(),
        DenseColumn::Text(values) => format!("{:?}", values[row]),
        DenseColumn::Date(values) => values[row].to_string(),
    }
}

/// Add one to `counts[row]` for each row whose value in `column` is nonzero,
/// used by `count_over_periods` to count the periods with a nonzero inner value.
/// Numeric variants count `!= 0`; a `Bool` column counts `true`; `Date`/`Text`
/// columns have no zero and cannot be produced by an arithmetic inner value, so
/// they are rejected rather than silently counted.
fn accumulate_nonzero_counts(column: &DenseColumn, counts: &mut [i64]) -> Result<(), EvalError> {
    if column.len() != counts.len() {
        return Err(EvalError::TypeMismatch(format!(
            "count_over_periods inner value produced {} rows but the batch has {}",
            column.len(),
            counts.len()
        )));
    }
    match column {
        DenseColumn::Bool(values) => {
            for (row, value) in values.iter().enumerate() {
                if *value {
                    counts[row] += 1;
                }
            }
        }
        DenseColumn::Integer(values) => {
            for (row, value) in values.iter().enumerate() {
                if *value != 0 {
                    counts[row] += 1;
                }
            }
        }
        DenseColumn::Decimal(values) => {
            for (row, value) in values.iter().enumerate() {
                if !value.is_zero() {
                    counts[row] += 1;
                }
            }
        }
        DenseColumn::Float(values) => {
            for (row, value) in values.iter().enumerate() {
                if *value != 0.0 {
                    counts[row] += 1;
                }
            }
        }
        DenseColumn::Text(_) | DenseColumn::Date(_) => {
            return Err(EvalError::TypeMismatch(
                "count_over_periods requires a numeric or boolean inner value (text and date have no zero to count against)".to_string(),
            ));
        }
    }
    Ok(())
}

/// Return the first row index at which two positionally aligned dense columns
/// disagree, or `None` if every row is identical. Comparison is exact and in
/// each column's dtype; two numeric columns of different variants (e.g. an
/// `Integer` and a `Float`) are compared by numeric value so an input supplied
/// as `1985` in one period and `1985.0` in another is still period-invariant.
/// Columns whose lengths differ, or whose types are non-numeric and mismatched,
/// are treated as differing at the first row (this errors loudly, which is the
/// safe outcome for a period that supplied a structurally different value).
/// Rows for which `skip` holds are not compared.
fn first_differing_row(
    left: &DenseColumn,
    right: &DenseColumn,
    skip: impl Fn(usize) -> bool,
) -> Option<usize> {
    // Length mismatch cannot arise under positional lifetime alignment (every
    // period's batch has the same row count), but guard it: report row 0.
    if left.len() != right.len() {
        return Some(0);
    }
    match (left, right) {
        (DenseColumn::Bool(a), DenseColumn::Bool(b)) => {
            (0..a.len()).find(|&row| !skip(row) && a[row] != b[row])
        }
        (DenseColumn::Text(a), DenseColumn::Text(b)) => {
            (0..a.len()).find(|&row| !skip(row) && a[row] != b[row])
        }
        (DenseColumn::Date(a), DenseColumn::Date(b)) => {
            (0..a.len()).find(|&row| !skip(row) && a[row] != b[row])
        }
        // Any pairing of numeric variants (Integer / Decimal / Float) compares
        // by numeric value. `vec_from_column::<Decimal>` promotes both sides to
        // Decimal for an exact comparison when representable; a value not
        // representable as Decimal — an f64 that is non-finite or beyond
        // Decimal's magnitude (~7.9e28), neither of which a per-person constant
        // year or money amount ever is — falls back to differing.
        (
            DenseColumn::Integer(_) | DenseColumn::Decimal(_) | DenseColumn::Float(_),
            DenseColumn::Integer(_) | DenseColumn::Decimal(_) | DenseColumn::Float(_),
        ) => match (
            <Decimal as DenseNum>::vec_from_column(left),
            <Decimal as DenseNum>::vec_from_column(right),
        ) {
            (Ok(a), Ok(b)) => (0..a.len()).find(|&row| !skip(row) && a[row] != b[row]),
            // Non-representable value on at least one side (non-finite or out of
            // Decimal range): cannot prove invariance, so treat as differing at
            // the first row.
            _ => Some(0),
        },
        // Genuinely different, non-numeric column shapes: not provably
        // invariant.
        _ => Some(0),
    }
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
        | CompiledScalarExpr::PeriodStart
        | CompiledScalarExpr::PeriodEnd
        | CompiledScalarExpr::CountRelated { .. }
        | CompiledScalarExpr::SumRelated { .. } => None,
        CompiledScalarExpr::ParameterLookup { index, .. } => nested_over_periods_kind(index),
        CompiledScalarExpr::Add(items)
        | CompiledScalarExpr::Max(items)
        | CompiledScalarExpr::Min(items) => items.iter().find_map(nested_over_periods_kind),
        CompiledScalarExpr::Sub(left, right)
        | CompiledScalarExpr::Mul(left, right)
        | CompiledScalarExpr::Div(left, right) => {
            nested_over_periods_kind(left).or_else(|| nested_over_periods_kind(right))
        }
        CompiledScalarExpr::Ceil(value) | CompiledScalarExpr::Floor(value) => {
            nested_over_periods_kind(value)
        }
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
        CompiledScalarExpr::Match { subject, arms, .. } => nested_over_periods_kind(subject)
            .or_else(|| {
                arms.iter().find_map(|(pattern, value)| {
                    nested_over_periods_kind(pattern).or_else(|| nested_over_periods_kind(value))
                })
            }),
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

    let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
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
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Bool(projected)
        }
        DenseColumn::Integer(values) => {
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Integer(projected)
        }
        DenseColumn::Decimal(values) => {
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Decimal(projected)
        }
        DenseColumn::Float(values) => {
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Float(projected)
        }
        DenseColumn::Text(values) => {
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row].clone());
                }
            }
            DenseColumn::Text(projected)
        }
        DenseColumn::Date(values) => {
            let mut projected = Vec::with_capacity(*offsets.last().unwrap_or(&0));
            for row in 0..row_count {
                for _ in offsets[row]..offsets[row + 1] {
                    projected.push(values[row]);
                }
            }
            DenseColumn::Date(projected)
        }
    })
}

fn compare_related_columns<N: DenseNum>(
    left: &DenseColumn,
    op: ComparisonOp,
    right: &DenseColumn,
) -> Result<Vec<bool>, EvalError> {
    match (left, right) {
        (DenseColumn::Bool(left), DenseColumn::Bool(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
                _ => false,
            })
            .collect()),
        (DenseColumn::Text(left), DenseColumn::Text(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
                _ => false,
            })
            .collect()),
        (DenseColumn::Date(left), DenseColumn::Date(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Lt => left < right,
                ComparisonOp::Lte => left <= right,
                ComparisonOp::Gt => left > right,
                ComparisonOp::Gte => left >= right,
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
            })
            .collect()),
        (left, right) => {
            let left = N::vec_from_column(left)?;
            let right = N::vec_from_column(right)?;
            Ok(left
                .into_iter()
                .zip(right)
                .map(|(left, right)| match op {
                    ComparisonOp::Lt => left < right,
                    ComparisonOp::Lte => left <= right,
                    ComparisonOp::Gt => left > right,
                    ComparisonOp::Gte => left >= right,
                    ComparisonOp::Eq => left == right,
                    ComparisonOp::Ne => left != right,
                })
                .collect())
        }
    }
}

fn broadcast_scalar_literal<N: DenseNum>(value: &ScalarValue, length: usize) -> DenseColumn {
    match value {
        ScalarValue::Bool(value) => DenseColumn::Bool(vec![*value; length]),
        ScalarValue::Integer(value) => DenseColumn::Integer(vec![*value; length]),
        ScalarValue::Decimal(value) => N::into_column(vec![N::from_decimal(value); length]),
        ScalarValue::Text(value) => DenseColumn::Text(vec![value.clone(); length]),
        ScalarValue::Date(value) => DenseColumn::Date(vec![*value; length]),
    }
}

/// Look up `parameter` at each row's key. A row whose key already failed keeps
/// that failure rather than a missing-cell error for its placeholder key, since
/// explain never reaches the lookup for it; it reads any cell as a placeholder.
fn lookup_parameter_dense<N: DenseNum>(
    parameter: &IndexedParameter,
    keys: Evaluated<Vec<i64>>,
    period: &Period,
) -> Result<Evaluated<DenseColumn>, EvalError> {
    let Evaluated {
        values: keys,
        errors,
    } = keys;
    // Failed rows are sorted and distinct, so all of them failed exactly when
    // there are as many as there are rows.
    let every_row_failed = !keys.is_empty() && errors.0.len() == keys.len();
    let version = parameter
        .versions
        .iter()
        .filter(|version| version.applies_at(period.start))
        .max_by_key(|version| version.effective_from);
    let version = match version {
        Some(version) => version,
        None if every_row_failed => {
            return Ok(Evaluated {
                values: DenseColumn::Integer(vec![0; keys.len()]),
                errors,
            });
        }
        None => {
            let first_reached = (0..keys.len()).find(|row| !errors.contains(*row));
            return Err(EvalError::MissingParameterValue {
                parameter: parameter.name.clone(),
                key: first_reached.map(|row| keys[row]).unwrap_or_default(),
                at: period.start,
            });
        }
    };
    let placeholder = version
        .values
        .values()
        .next()
        .cloned()
        .unwrap_or(ScalarValue::Integer(0));

    let values = keys
        .iter()
        .enumerate()
        .map(|(row, key)| match version.values.get(key) {
            Some(value) => Ok(value.clone()),
            None if errors.contains(row) => Ok(placeholder.clone()),
            None => Err(EvalError::MissingParameterValue {
                parameter: parameter.name.clone(),
                key: *key,
                at: period.start,
            }),
        })
        .collect::<Result<Vec<ScalarValue>, EvalError>>()?;
    Ok(Evaluated {
        values: parameter_column::<N>(values)?,
        errors,
    })
}

/// A column of looked-up parameter cells, in the cells' own dtype.
fn parameter_column<N: DenseNum>(values: Vec<ScalarValue>) -> Result<DenseColumn, EvalError> {
    if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Integer(_)))
    {
        Ok(DenseColumn::Integer(
            values
                .into_iter()
                .map(|value| match value {
                    ScalarValue::Integer(value) => Ok(value),
                    _ => Err(EvalError::TypeMismatch(
                        "mixed parameter dtypes are not supported".to_string(),
                    )),
                })
                .collect::<Result<Vec<i64>, EvalError>>()?,
        ))
    } else if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Bool(_)))
    {
        Ok(DenseColumn::Bool(
            values
                .into_iter()
                .map(|value| match value {
                    ScalarValue::Bool(value) => Ok(value),
                    _ => Err(EvalError::TypeMismatch(
                        "mixed parameter dtypes are not supported".to_string(),
                    )),
                })
                .collect::<Result<Vec<bool>, EvalError>>()?,
        ))
    } else if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Text(_)))
    {
        Ok(DenseColumn::Text(
            values
                .into_iter()
                .map(|value| match value {
                    ScalarValue::Text(value) => Ok(value),
                    _ => Err(EvalError::TypeMismatch(
                        "mixed parameter dtypes are not supported".to_string(),
                    )),
                })
                .collect::<Result<Vec<String>, EvalError>>()?,
        ))
    } else {
        Ok(N::into_column(
            values
                .into_iter()
                .map(|value| {
                    value
                        .as_decimal()
                        .map(|value| N::from_decimal(&value))
                        .ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "parameter values must be numeric in dense mode".to_string(),
                            )
                        })
                })
                .collect::<Result<Vec<N>, EvalError>>()?,
        ))
    }
}

fn select_related_scalar_column<N: DenseNum>(
    condition: &[bool],
    then_values: DenseColumn,
    else_values: DenseColumn,
) -> Result<DenseColumn, EvalError> {
    let condition = condition
        .iter()
        .map(|holds| {
            if *holds {
                JudgmentOutcome::Holds
            } else {
                JudgmentOutcome::NotHolds
            }
        })
        .collect();
    select_dense_scalar_column::<N>(condition, then_values, else_values)
}

fn select_dense_scalar_column<N: DenseNum>(
    condition: Vec<JudgmentOutcome>,
    then_values: DenseColumn,
    else_values: DenseColumn,
) -> Result<DenseColumn, EvalError> {
    match (then_values, else_values) {
        (DenseColumn::Integer(then_values), DenseColumn::Integer(else_values)) => {
            Ok(DenseColumn::Integer(
                condition
                    .into_iter()
                    .zip(then_values)
                    .zip(else_values)
                    .map(|((condition, then_value), else_value)| {
                        if condition.is_holds() {
                            then_value
                        } else {
                            else_value
                        }
                    })
                    .collect(),
            ))
        }
        (DenseColumn::Bool(then_values), DenseColumn::Bool(else_values)) => Ok(DenseColumn::Bool(
            condition
                .into_iter()
                .zip(then_values)
                .zip(else_values)
                .map(|((condition, then_value), else_value)| {
                    if condition.is_holds() {
                        then_value
                    } else {
                        else_value
                    }
                })
                .collect(),
        )),
        (DenseColumn::Text(then_values), DenseColumn::Text(else_values)) => Ok(DenseColumn::Text(
            condition
                .into_iter()
                .zip(then_values)
                .zip(else_values)
                .map(|((condition, then_value), else_value)| {
                    if condition.is_holds() {
                        then_value
                    } else {
                        else_value
                    }
                })
                .collect(),
        )),
        (DenseColumn::Date(then_values), DenseColumn::Date(else_values)) => Ok(DenseColumn::Date(
            condition
                .into_iter()
                .zip(then_values)
                .zip(else_values)
                .map(|((condition, then_value), else_value)| {
                    if condition.is_holds() {
                        then_value
                    } else {
                        else_value
                    }
                })
                .collect(),
        )),
        (then_values, else_values) => {
            let then_values = N::vec_from_column(&then_values).map_err(|_| {
                EvalError::TypeMismatch("dense if() branches must have the same dtype".to_string())
            })?;
            let else_values = N::vec_from_column(&else_values).map_err(|_| {
                EvalError::TypeMismatch("dense if() branches must have the same dtype".to_string())
            })?;
            Ok(N::into_column(
                condition
                    .into_iter()
                    .zip(then_values)
                    .zip(else_values)
                    .map(|((condition, then_value), else_value)| {
                        if condition.is_holds() {
                            then_value
                        } else {
                            else_value
                        }
                    })
                    .collect(),
            ))
        }
    }
}

fn compare_dense_columns<N: DenseNum>(
    left: DenseColumn,
    op: ComparisonOp,
    right: DenseColumn,
) -> Result<Vec<JudgmentOutcome>, EvalError> {
    match (left, right) {
        (DenseColumn::Bool(left), DenseColumn::Bool(right)) => Ok(left
            .into_iter()
            .zip(right)
            .map(|(left, right)| {
                let outcome = match op {
                    ComparisonOp::Eq => left == right,
                    ComparisonOp::Ne => left != right,
                    _ => false,
                };
                if outcome {
                    JudgmentOutcome::Holds
                } else {
                    JudgmentOutcome::NotHolds
                }
            })
            .collect()),
        (DenseColumn::Text(left), DenseColumn::Text(right)) => Ok(left
            .into_iter()
            .zip(right)
            .map(|(left, right)| {
                let outcome = match op {
                    ComparisonOp::Eq => left == right,
                    ComparisonOp::Ne => left != right,
                    _ => false,
                };
                if outcome {
                    JudgmentOutcome::Holds
                } else {
                    JudgmentOutcome::NotHolds
                }
            })
            .collect()),
        (DenseColumn::Date(left), DenseColumn::Date(right)) => Ok(left
            .into_iter()
            .zip(right)
            .map(|(left, right)| {
                let outcome = match op {
                    ComparisonOp::Lt => left < right,
                    ComparisonOp::Lte => left <= right,
                    ComparisonOp::Gt => left > right,
                    ComparisonOp::Gte => left >= right,
                    ComparisonOp::Eq => left == right,
                    ComparisonOp::Ne => left != right,
                };
                if outcome {
                    JudgmentOutcome::Holds
                } else {
                    JudgmentOutcome::NotHolds
                }
            })
            .collect()),
        (left, right) => {
            let left = N::vec_from_column(&left)?;
            let right = N::vec_from_column(&right)?;
            Ok(left
                .into_iter()
                .zip(right)
                .map(|(left, right)| {
                    let outcome = match op {
                        ComparisonOp::Lt => left < right,
                        ComparisonOp::Lte => left <= right,
                        ComparisonOp::Gt => left > right,
                        ComparisonOp::Gte => left >= right,
                        ComparisonOp::Eq => left == right,
                        ComparisonOp::Ne => left != right,
                    };
                    if outcome {
                        JudgmentOutcome::Holds
                    } else {
                        JudgmentOutcome::NotHolds
                    }
                })
                .collect())
        }
    }
}
