// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::{
    ArithmeticError, EvalError, checked_add, checked_div, checked_mul, checked_sub,
};
use crate::lazy::{RowErrors, RowMask};
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
) -> DenseColumn {
    match column {
        DenseColumn::Bool(_) | DenseColumn::Text(_) | DenseColumn::Date(_) => column,
        numeric => N::into_column(
            numeric_values::<N>(&numeric, live, errors)
                .into_iter()
                .map(|value| value.round_to(rounding))
                .collect(),
        ),
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
    /// The root row that owns each related row.
    owners: Vec<usize>,
    /// `None` is a column the caller did not supply: a missing input on every
    /// related row, an error only for the rows that read it.
    inputs: Vec<Option<DenseColumn>>,
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
    rule: String,
    subject: String,
    patterns: String,
}

impl MatchLabels {
    /// Explain's error for the row whose subject value is at `row` of `subject`.
    fn failure(&self, subject: &DenseColumn, row: usize) -> EvalError {
        EvalError::NoMatchingArm {
            rule: self.rule.clone(),
            subject: self.subject.clone(),
            value: dense_value_label(subject, row),
            patterns: self.patterns.clone(),
        }
    }
}

const NO_MATCH_OUTSIDE_IF: &str =
    "a `match` fallback (`no_match`) that is not the `else` branch of an `if`";

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
    relations: Vec<DenseRelationSchema>,
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
        let mut executor: DenseExecutor<'_, N> = DenseExecutor::new(self, period, batch, true);
        let all = RowMask::all(row_count);
        let mut evaluated = Vec::with_capacity(requested.len());
        for (output, derived_index) in requested {
            let (value, errors) = match &self.derived[derived_index].semantics {
                CompiledSemantics::Scalar(_) => {
                    let (column, errors) = executor.evaluate_scalar(derived_index, &all)?;
                    (DenseOutputValue::Scalar(column), errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let (values, errors) = executor.evaluate_judgment(derived_index, &all)?;
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
        let all = RowMask::all(row_count);
        let mut evaluated = Vec::with_capacity(outputs.len());
        for output in outputs {
            let derived_index = self.derived_index[output];
            let (value, errors) = match &self.derived[derived_index].semantics {
                CompiledSemantics::Scalar(_) => {
                    let (column, errors) = executor.evaluate_scalar(derived_index, &all)?;
                    (DenseOutputValue::Scalar(column), errors)
                }
                CompiledSemantics::Judgment(_) => {
                    let (values, errors) = executor.evaluate_judgment(derived_index, &all)?;
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
            CompiledScalarExpr::NoMatch {
                subject,
                placeholder,
                ..
            } => {
                self.scalar_reduces_over_periods(subject, visiting)
                    || self.scalar_reduces_over_periods(placeholder, visiting)
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

        // An absent input column is a missing input on every row. It fails
        // only the rows whose evaluation reads it, as a missing record does on
        // the explain path; a column read only on branches no row takes may be
        // omitted.
        let bound_inputs = self
            .root_inputs
            .iter()
            .map(|name| batch.inputs.get(name).cloned())
            .collect::<Vec<Option<DenseColumn>>>();

        let mut bound_relations = Vec::with_capacity(self.relations.len());
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
            let bound_inputs = relation
                .related_inputs
                .iter()
                .map(|name| {
                    let column = relation_batch.inputs.get(name).cloned();
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

            let mut owners = Vec::with_capacity(related_count);
            for (row, pair) in relation_batch.offsets.windows(2).enumerate() {
                owners.extend(std::iter::repeat_n(row, pair[1] - pair[0]));
            }
            bound_relations.push(DenseRelationBatch {
                offsets: relation_batch.offsets.clone(),
                related_count,
                owners,
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
    relations: Vec<DenseRelationSchema>,
    relation_index: HashMap<DenseRelationKey, usize>,
    relation_input_index: HashMap<(usize, String), usize>,
    parameters: Vec<CompiledParameter>,
    parameter_index: HashMap<String, usize>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
    visiting: HashSet<String>,
    /// The rules whose formulas are being compiled, innermost last. Related
    /// expressions inline a related entity's rules, so a `match` in one names
    /// the innermost rule here, as explain does.
    related_rules: Vec<String>,
}

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
        })
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
        MatchLabels {
            rule: self.rule_label(rule),
            subject: crate::engine::describe_match_operand(subject),
            patterns: patterns
                .iter()
                .map(crate::engine::describe_match_operand)
                .collect::<Vec<_>>()
                .join(", "),
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

    fn compile_scalar_expr(
        &mut self,
        derived_name: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name),
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
                let value = match value {
                    RelatedValueRef::Input(name) => {
                        let input_index = self.related_input(relation_index, name);
                        CompiledRelatedScalarExpr::Input(input_index)
                    }
                    RelatedValueRef::Derived(name) => self.compile_related_scalar(
                        relation_index,
                        &ScalarExpr::Derived(name.clone()),
                    )?,
                };
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
            ScalarExpr::NoMatch { .. } => Err(DenseCompileError::Unsupported(
                NO_MATCH_OUTSIDE_IF.to_string(),
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

    fn compile_related_predicate(
        &mut self,
        relation_index: usize,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        match expr {
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
                        self.related_rules.push(name.clone());
                        let compiled = self.compile_related_predicate(relation_index, expr);
                        self.related_rules.pop();
                        compiled
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

    fn compile_related_scalar(
        &mut self,
        relation_index: usize,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledRelatedScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => {
                let input_index = self.related_input(relation_index, name);
                Ok(CompiledRelatedScalarExpr::Input(input_index))
            }
            ScalarExpr::InputOrElse { name, default } => {
                let input_index = self.related_input(relation_index, name);
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
                        self.related_rules.push(name.clone());
                        let compiled = self.compile_related_scalar(relation_index, expr);
                        self.related_rules.pop();
                        compiled
                    }
                    DerivedSemantics::Judgment(_) => Err(DenseCompileError::Unsupported(format!(
                        "related scalar expressions cannot reference judgment derived values (`{name}`)"
                    ))),
                }
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
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name),
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
                self.relation_index.insert(lookup_key, index);
                // The predicate is compiled once per relation, so a `match` in
                // it is named by whichever rule evaluates the relation.
                self.related_rules.push(String::new());
                let filter = self.compile_related_predicate(index, &derivation.predicate);
                self.related_rules.pop();
                let filter = filter?;
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
                self.relation_index.insert(lookup_key, index);
                Ok(index)
            }
        }
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
        self.parameters.push(CompiledParameter {
            parameter: parameter.clone(),
        });
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

/// A derived rule's column, computed so far for the `computed` rows.
struct DerivedColumn<T> {
    values: T,
    errors: RowErrors,
    computed: RowMask,
}

/// The rows of `mask` a cached derived column has not computed yet. A rule
/// with no cache entry is computed at least once, possibly under an empty
/// mask, to fix its dtype.
fn pending_rows<T>(cached: Option<&DerivedColumn<T>>, mask: &RowMask) -> RowMask {
    match cached {
        Some(cached) => mask.difference(&cached.computed),
        None => mask.clone(),
    }
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
    let existing = cached.computed.without(&cached.errors);
    let added = pending.without(&errors);
    let values = merge_dense_rows::<N>(&existing, cached.values, &added, values, &mut errors)?;
    let mut merged_errors = cached.errors;
    merged_errors.absorb(errors);
    Ok(DerivedColumn {
        values,
        errors: merged_errors,
        computed: cached.computed.union(&pending),
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
            assign_rows(&mut target, &source, added_rows);
            DenseColumn::Decimal(target)
        }
        (DenseColumn::Float(mut target), DenseColumn::Float(source)) => {
            assign_rows(&mut target, &source, added_rows);
            DenseColumn::Float(target)
        }
        (existing, added) => {
            return select_dense::<N>(existing_rows, existing, added_rows, added, errors);
        }
    })
}

fn merge_judgment(
    cached: Option<DerivedColumn<Vec<JudgmentOutcome>>>,
    pending: RowMask,
    (values, errors): JudgmentEval,
) -> DerivedColumn<Vec<JudgmentOutcome>> {
    let Some(mut cached) = cached else {
        return DerivedColumn {
            values,
            errors,
            computed: pending,
        };
    };
    for row in pending.without(&errors).rows() {
        cached.values[row] = values[row];
    }
    cached.errors.absorb(errors);
    cached.computed = cached.computed.union(&pending);
    cached
}

/// A column of placeholders of `N`'s dtype with `error` on every row of
/// `mask`.
fn fail_numeric<N: DenseNum>(len: usize, mask: &RowMask, error: EvalError) -> DenseEval {
    fail_all(N::into_column(vec![N::ZERO; len]), mask, error)
}

fn fail_all(placeholder: DenseColumn, mask: &RowMask, error: EvalError) -> DenseEval {
    let mut errors = RowErrors::new();
    errors.record_all(mask, &error);
    (placeholder, errors)
}

/// The request's error: the lowest failing row, and for that row the first
/// failing output in request order, as the explain path reports it.
fn first_row_error<'e>(outputs: impl Iterator<Item = &'e RowErrors>) -> Result<(), EvalError> {
    let mut first: Option<(usize, &EvalError)> = None;
    for errors in outputs {
        if let Some((row, error)) = errors.first()
            && first.is_none_or(|(best, _)| row < best)
        {
            first = Some((row, error));
        }
    }
    match first {
        Some((_, error)) => Err(error.clone()),
        None => Ok(()),
    }
}

/// A columnar evaluator over one row axis: a batch's root rows, one
/// relation's related rows, or a lifetime executor's rows. The arithmetic
/// every axis shares is written once against this trait.
trait ColumnAxis<N: DenseNum, E> {
    fn axis_len(&self) -> usize;
    fn eval_column(&mut self, expr: &E, mask: &RowMask) -> Result<DenseEval, EvalError>;
}

/// Judgment evaluation over root or lifetime rows, for the shared
/// short-circuit of `and`/`or`.
trait JudgmentAxis {
    fn judgment_len(&self) -> usize;
    fn eval_judgment_column(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError>;
}

/// Evaluate `expr` for the rows of `mask` and read it as numbers. Returns the
/// numbers and the rows still live; failures land in `errors`.
fn numeric_operand<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    expr: &E,
    mask: &RowMask,
    errors: &mut RowErrors,
) -> Result<(Vec<N>, RowMask), EvalError> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let mut live = mask.without(&operand_errors);
    errors.absorb(operand_errors);
    let mut conversion = RowErrors::new();
    let values = numeric_values::<N>(&column, &live, &mut conversion);
    if !conversion.is_empty() {
        live = live.without(&conversion);
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
) -> Result<(Vec<NaiveDate>, RowMask), EvalError> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let live = mask.without(&operand_errors);
    errors.absorb(operand_errors);
    match column {
        DenseColumn::Date(values) => Ok((values, live)),
        other => {
            let mut wrong = RowErrors::new();
            wrong.record_all(&live, &EvalError::TypeMismatch(message.to_string()));
            let live = live.without(&wrong);
            errors.absorb(wrong);
            Ok((vec![NaiveDate::default(); other.len()], live))
        }
    }
}

fn index_operand<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    expr: &E,
    mask: &RowMask,
    errors: &mut RowErrors,
    message: &str,
) -> Result<(Vec<i64>, RowMask), EvalError> {
    let (column, operand_errors) = axis.eval_column(expr, mask)?;
    let live = mask.without(&operand_errors);
    errors.absorb(operand_errors);
    let mut indices = vec![0; column.len()];
    let mut wrong = RowErrors::new();
    for row in live.rows() {
        match index_at(&column, row) {
            Some(index) => indices[row] = index,
            None => wrong.record(row, EvalError::TypeMismatch(message.to_string())),
        }
    }
    let live = live.without(&wrong);
    errors.absorb(wrong);
    Ok((indices, live))
}

fn eval_add<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    items: &[E],
    mask: &RowMask,
) -> Result<DenseEval, EvalError> {
    let mut errors = RowErrors::new();
    let mut live = mask.clone();
    let mut total = vec![N::ZERO; axis.axis_len()];
    for item in items {
        let (values, next) = numeric_operand::<N, E, A>(axis, item, &live, &mut errors)?;
        let mut overflow = RowErrors::new();
        for row in next.rows() {
            match total[row].try_add(values[row]) {
                Ok(sum) => total[row] = sum,
                Err(error) => overflow.record(row, error.into()),
            }
        }
        live = next.without(&overflow);
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
) -> Result<DenseEval, EvalError> {
    let mut errors = RowErrors::new();
    let (left, live) = numeric_operand::<N, E, A>(axis, left, mask, &mut errors)?;
    let (right, live) = numeric_operand::<N, E, A>(axis, right, &live, &mut errors)?;
    let mut values = vec![N::ZERO; axis.axis_len()];
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
) -> Result<DenseEval, EvalError> {
    let mut errors = RowErrors::new();
    let (dividends, divisors, live) = if divisor_first {
        let (divisors, live) = numeric_operand::<N, E, A>(axis, right, mask, &mut errors)?;
        let mut zero = RowErrors::new();
        for row in live.rows() {
            if divisors[row] == N::ZERO {
                zero.record(row, EvalError::DivisionByZero);
            }
        }
        let live = live.without(&zero);
        errors.absorb(zero);
        let (dividends, live) = numeric_operand::<N, E, A>(axis, left, &live, &mut errors)?;
        (dividends, divisors, live)
    } else {
        let (dividends, live) = numeric_operand::<N, E, A>(axis, left, mask, &mut errors)?;
        let (divisors, live) = numeric_operand::<N, E, A>(axis, right, &live, &mut errors)?;
        (dividends, divisors, live)
    };
    let mut quotients = vec![N::ZERO; axis.axis_len()];
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
) -> Result<DenseEval, EvalError> {
    let Some((first, rest)) = items.split_first() else {
        return Ok(fail_numeric::<N>(
            axis.axis_len(),
            mask,
            EvalError::TypeMismatch(format!("{function}() requires at least one operand")),
        ));
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

fn eval_unary<N: DenseNum, E, A: ColumnAxis<N, E>>(
    axis: &mut A,
    value: &E,
    mask: &RowMask,
    operation: impl Fn(N) -> N,
) -> Result<DenseEval, EvalError> {
    let mut errors = RowErrors::new();
    let (values, live) = numeric_operand::<N, E, A>(axis, value, mask, &mut errors)?;
    let mut result = vec![N::ZERO; axis.axis_len()];
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
) -> Result<DenseEval, EvalError> {
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
    let mut shifted = vec![NaiveDate::default(); axis.axis_len()];
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
) -> Result<DenseEval, EvalError> {
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
    let mut days = vec![0; axis.axis_len()];
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
) -> Result<JudgmentEval, EvalError> {
    let exhausted = match decisive {
        JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
        _ => JudgmentOutcome::NotHolds,
    };
    let len = axis.judgment_len();
    let mut outcomes = vec![exhausted; len];
    let mut undetermined = vec![false; len];
    let mut errors = RowErrors::new();
    let mut pending = mask.clone();
    for item in items {
        if pending.is_empty() {
            break;
        }
        let (values, item_errors) = axis.eval_judgment_column(item, &pending)?;
        let live = pending.without(&item_errors);
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
        });
    }
    for row in pending.rows() {
        if undetermined[row] {
            outcomes[row] = JudgmentOutcome::Undetermined;
        }
    }
    Ok((outcomes, errors))
}

fn negate(values: Vec<JudgmentOutcome>) -> Vec<JudgmentOutcome> {
    values
        .into_iter()
        .map(|value| match value {
            JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
            JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
            JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
        })
        .collect()
}

struct DenseExecutor<'a, N: DenseNum> {
    program: &'a DenseCompiledProgram,
    period: &'a Period,
    batch: DenseBoundBatch,
    /// Enforce each rule's commencement date (#84): a row that reaches a rule
    /// before it commences fails. The per-period entry points do; the
    /// lifetime executor's per-period executors never have.
    enforce_commencement: bool,
    scalar_cache: Vec<Option<DerivedColumn<DenseColumn>>>,
    judgment_cache: Vec<Option<DerivedColumn<Vec<JudgmentOutcome>>>>,
    _numeric_mode: std::marker::PhantomData<N>,
}

impl<N: DenseNum> ColumnAxis<N, CompiledScalarExpr> for DenseExecutor<'_, N> {
    fn axis_len(&self) -> usize {
        self.batch.row_count
    }

    fn eval_column(
        &mut self,
        expr: &CompiledScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
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
    ) -> Result<JudgmentEval, EvalError> {
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

    fn eval_column(
        &mut self,
        expr: &CompiledRelatedScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        self.executor
            .resolve_related_scalar(self.relation, expr, mask)
    }
}

impl<'a, N: DenseNum> DenseExecutor<'a, N> {
    fn new(
        program: &'a DenseCompiledProgram,
        period: &'a Period,
        batch: DenseBoundBatch,
        enforce_commencement: bool,
    ) -> Self {
        Self {
            program,
            period,
            scalar_cache: (0..program.derived.len()).map(|_| None).collect(),
            judgment_cache: (0..program.derived.len()).map(|_| None).collect(),
            batch,
            enforce_commencement,
            _numeric_mode: std::marker::PhantomData,
        }
    }

    /// A derived scalar's column for the rows in `mask`, computing it only
    /// for rows no earlier reference asked for.
    fn evaluate_scalar(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let pending = pending_rows(self.scalar_cache[derived_index].as_ref(), mask);
        let cached = match self.scalar_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = self.compute_scalar(derived_index, &pending)?;
                merge_scalar::<N>(cached, pending, computed)?
            }
        };
        let result = (cached.values.clone(), cached.errors.restricted_to(mask));
        self.scalar_cache[derived_index] = Some(cached);
        Ok(result)
    }

    fn compute_scalar(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let CompiledSemantics::Scalar(expr) = &derived.semantics else {
            return Ok(fail_numeric::<N>(
                self.batch.row_count,
                mask,
                EvalError::ExpectedScalar(derived.name.clone()),
            ));
        };
        let (live, mut errors) = self.commenced_rows(derived_index, mask);
        let (column, formula_errors) = self.eval_scalar_expr(expr, &live)?;
        errors.absorb(formula_errors.within_rule(&derived.label));
        // Opt-in output rounding, applied before caching so dependents and
        // direct outputs see the same rounded values, as on the explain path.
        // Rounds in the executor's numeric mode (exact for Decimal,
        // best-effort for f64).
        let column = match derived.rounding {
            Some(rounding) => {
                round_dense_column::<N>(column, rounding, &live.without(&errors), &mut errors)
            }
            None => column,
        };
        Ok((column, errors))
    }

    /// A rule before its commencement date has no lawful value (#84). Every
    /// row that reaches it fails, and its formula is only typed, under an
    /// empty mask.
    fn commenced_rows(&self, derived_index: usize, mask: &RowMask) -> (RowMask, RowErrors) {
        let derived = &self.program.derived[derived_index];
        if self.enforce_commencement
            && let Some(effective_from) = derived.effective_from
            && self.period.start < effective_from
        {
            let mut errors = RowErrors::new();
            errors.record_all(
                mask,
                &EvalError::MissingDerivedFormulaVersion {
                    derived: derived.name.clone(),
                    at: self.period.start,
                },
            );
            return (RowMask::none(mask.len()), errors);
        }
        (mask.clone(), RowErrors::new())
    }

    fn evaluate_judgment(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError> {
        let pending = pending_rows(self.judgment_cache[derived_index].as_ref(), mask);
        let cached = match self.judgment_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = self.compute_judgment(derived_index, &pending)?;
                merge_judgment(cached, pending, computed)
            }
        };
        let result = (cached.values.clone(), cached.errors.restricted_to(mask));
        self.judgment_cache[derived_index] = Some(cached);
        Ok(result)
    }

    fn compute_judgment(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let placeholder = vec![JudgmentOutcome::NotHolds; self.batch.row_count];
        let CompiledSemantics::Judgment(expr) = &derived.semantics else {
            let (_, errors) = fail_numeric::<N>(
                self.batch.row_count,
                mask,
                EvalError::ExpectedJudgment(derived.name.clone()),
            );
            return Ok((placeholder, errors));
        };
        let (live, mut errors) = self.commenced_rows(derived_index, mask);
        let (values, formula_errors) = self.eval_judgment_expr(expr, &live)?;
        errors.absorb(formula_errors.within_rule(&derived.label));
        Ok((values, errors))
    }

    fn eval_scalar_expr(
        &mut self,
        expr: &CompiledScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let len = self.batch.row_count;
        match expr {
            CompiledScalarExpr::Literal(value) => {
                Ok((broadcast_scalar_literal::<N>(value, len), RowErrors::new()))
            }
            CompiledScalarExpr::Input(index) => Ok(match &self.batch.inputs[*index] {
                Some(column) => (column.clone(), RowErrors::new()),
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
                ),
            }),
            CompiledScalarExpr::InputOrElse { input, default } => Ok((
                match &self.batch.inputs[*input] {
                    Some(column) => column.clone(),
                    None => broadcast_scalar_literal::<N>(default, len),
                },
                RowErrors::new(),
            )),
            CompiledScalarExpr::Derived(index) => self.evaluate_scalar(*index, mask),
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                let (keys, mut errors) = self.eval_scalar_expr(index, mask)?;
                let live = mask.without(&errors);
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    &keys,
                    &live,
                    self.period,
                    &mut errors,
                );
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
                DenseColumn::Date(vec![self.period.start; len]),
                RowErrors::new(),
            )),
            CompiledScalarExpr::PeriodEnd => Ok((
                DenseColumn::Date(vec![self.period.end; len]),
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
            CompiledScalarExpr::CountRelated {
                relation,
                predicate,
            } => self.eval_count_related(*relation, predicate.as_ref(), mask),
            CompiledScalarExpr::SumRelated {
                relation,
                value,
                predicate,
            } => self.eval_sum_related(*relation, value, predicate.as_ref(), mask),
            CompiledScalarExpr::NoMatch {
                subject,
                placeholder,
                labels,
            } => {
                let (placeholder, _) =
                    self.eval_scalar_expr(placeholder, &RowMask::none(mask.len()))?;
                let (subject, mut errors) = self.eval_scalar_expr(subject, mask)?;
                for row in mask.without(&errors).rows() {
                    errors.record(row, labels.failure(&subject, row));
                }
                Ok((placeholder, errors))
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) = self.eval_judgment_expr(condition, mask)?;
                let live = mask.without(&errors);
                // An undetermined condition takes the else branch.
                let then_rows = live.filter(|row| condition[row].is_holds());
                let else_rows = live.difference(&then_rows);
                let (then_values, then_errors) = self.eval_scalar_expr(then_expr, &then_rows)?;
                let (else_values, else_errors) = self.eval_scalar_expr(else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors);
                let else_live = else_rows.without(&else_errors);
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
            )),
        }
    }

    /// Related rows owned by the root rows of `mask`.
    fn related_rows_of(&self, relation: usize, mask: &RowMask) -> RowMask {
        let batch = &self.batch.relations[relation];
        if mask.is_all() {
            return RowMask::all(batch.related_count);
        }
        let mut bits = vec![false; batch.related_count];
        for row in mask.rows() {
            bits[batch.offsets[row]..batch.offsets[row + 1]].fill(true);
        }
        RowMask::from_bits(bits)
    }

    /// Root rows owning at least one related row of `related`.
    fn root_rows_of(&self, relation: usize, related: &RowMask) -> RowMask {
        let owners = &self.batch.relations[relation].owners;
        let mut bits = vec![false; self.batch.row_count];
        for related in related.rows() {
            bits[owners[related]] = true;
        }
        RowMask::from_bits(bits)
    }

    /// Related-row errors lifted to the root rows that own them. A root row
    /// takes its first failing related row's error.
    fn lift_errors(&self, relation: usize, related_errors: &RowErrors) -> RowErrors {
        let owners = &self.batch.relations[relation].owners;
        let mut errors = RowErrors::new();
        for (related, error) in related_errors.iter() {
            errors.record(owners[related], error.clone());
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
                errors.record(related, error.clone());
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
    ) -> RowMask {
        if root_errors.is_empty() {
            return related.clone();
        }
        let owners = &self.batch.relations[relation].owners;
        related.filter(|related| !root_errors.contains(owners[related]))
    }

    /// The related rows of `candidates` that are members of `relation`: a
    /// derived relation's parent filter first, then its own filter, each only
    /// for the rows the previous stage kept (the explain order). Errors are
    /// returned on the root rows that own the failing related rows.
    fn relation_members(
        &mut self,
        relation: usize,
        candidates: &RowMask,
    ) -> Result<(RowMask, RowErrors), EvalError> {
        let program = self.program;
        let schema = &program.relations[relation];
        let mut root_errors = RowErrors::new();
        let mut members = candidates.clone();
        if let Some(parent) = schema.parent_relation {
            let (kept, parent_errors) = self.relation_members(parent, &members)?;
            root_errors.absorb(parent_errors);
            members = self.without_failed_roots(relation, &kept, &root_errors);
        }
        if let Some(filter) = &schema.filter {
            let (holds, filter_errors) = self.eval_related_predicate(relation, filter, &members)?;
            let live = members.without(&filter_errors);
            root_errors.absorb(self.lift_errors(relation, &filter_errors));
            members = live.filter(|related| holds[related]);
            members = self.without_failed_roots(relation, &members, &root_errors);
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
    ) -> Result<(RowMask, RowErrors, RowErrors), EvalError> {
        let candidates = self.related_rows_of(relation, mask);
        let (members, root_errors) = self.relation_members(relation, &candidates)?;
        let Some(predicate) = predicate else {
            return Ok((members, RowErrors::new(), root_errors));
        };
        let (holds, where_errors) = self.eval_related_predicate(relation, predicate, &members)?;
        let kept = members
            .without(&where_errors)
            .filter(|related| holds[related]);
        Ok((kept, where_errors, root_errors))
    }

    fn eval_count_related(
        &mut self,
        relation: usize,
        predicate: Option<&CompiledRelatedJudgmentExpr>,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let (members, where_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        errors.absorb(self.lift_errors(relation, &where_errors));
        let owners = &self.batch.relations[relation].owners;
        let mut counts = vec![0_usize; self.batch.row_count];
        for related in members.rows() {
            counts[owners[related]] += 1;
        }
        Ok((
            DenseColumn::Integer(
                counts
                    .into_iter()
                    .map(|count| i64::try_from(count).unwrap_or(i64::MAX))
                    .collect(),
            ),
            errors,
        ))
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
    ) -> Result<DenseEval, EvalError> {
        let (members, mut related_errors, mut errors) =
            self.filtered_members(relation, predicate, mask)?;
        let (values, value_errors) = self.resolve_related_scalar(relation, value, &members)?;
        let live = members.without(&value_errors);
        related_errors.absorb(value_errors);
        let numbers = numeric_values::<N>(&values, &live, &mut related_errors);
        let live = live.without(&related_errors);
        errors.absorb(self.lift_errors(relation, &related_errors));
        let owners = &self.batch.relations[relation].owners;
        let mut totals = vec![N::ZERO; self.batch.row_count];
        let mut overflow = RowErrors::new();
        for related in live.rows() {
            let owner = owners[related];
            if errors.contains(owner) || overflow.contains(owner) {
                continue;
            }
            match totals[owner].try_add(numbers[related]) {
                Ok(total) => totals[owner] = total,
                Err(error) => overflow.record(owner, error.into()),
            }
        }
        errors.absorb(overflow);
        Ok((N::into_column(totals), errors))
    }

    fn eval_related_predicate(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedJudgmentExpr,
        mask: &RowMask,
    ) -> Result<(Vec<bool>, RowErrors), EvalError> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedJudgmentExpr::Literal(value) => {
                Ok((vec![*value; length], RowErrors::new()))
            }
            CompiledRelatedJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.resolve_related_scalar(relation, left, mask)?;
                let live = mask.without(&errors);
                let (right, right_errors) = self.resolve_related_scalar(relation, right, &live)?;
                let live = live.without(&right_errors);
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors);
                Ok((
                    outcomes
                        .into_iter()
                        .map(|outcome| outcome.is_holds())
                        .collect(),
                    errors,
                ))
            }
            CompiledRelatedJudgmentExpr::RootJudgment(expr) => {
                let roots = self.root_rows_of(relation, mask);
                let (values, root_errors) = self.eval_judgment_expr(expr, &roots)?;
                let projected = project_root_judgment_to_related(
                    &values,
                    &self.batch.relations[relation].offsets,
                )?;
                Ok((projected, self.lower_errors(relation, &root_errors, mask)))
            }
            CompiledRelatedJudgmentExpr::And(items) | CompiledRelatedJudgmentExpr::Or(items) => {
                // Short-circuit per related row: `and` stops at the first
                // false item, `or` at the first true one.
                let decisive = matches!(expr, CompiledRelatedJudgmentExpr::Or(_));
                let mut result = vec![!decisive; length];
                let mut errors = RowErrors::new();
                let mut pending = mask.clone();
                for item in items {
                    if pending.is_empty() {
                        break;
                    }
                    let (values, item_errors) =
                        self.eval_related_predicate(relation, item, &pending)?;
                    let live = pending.without(&item_errors);
                    errors.absorb(item_errors);
                    pending = live.filter(|related| {
                        if values[related] == decisive {
                            result[related] = decisive;
                            false
                        } else {
                            true
                        }
                    });
                }
                Ok((result, errors))
            }
            CompiledRelatedJudgmentExpr::Not(item) => {
                let (values, errors) = self.eval_related_predicate(relation, item, mask)?;
                Ok((values.into_iter().map(|keep| !keep).collect(), errors))
            }
        }
    }

    fn resolve_related_scalar(
        &mut self,
        relation: usize,
        expr: &CompiledRelatedScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let length = self.batch.relations[relation].related_count;
        match expr {
            CompiledRelatedScalarExpr::Literal(value) => Ok((
                broadcast_scalar_literal::<N>(value, length),
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::Input(index) => {
                Ok(match &self.batch.relations[relation].inputs[*index] {
                    Some(column) => (column.clone(), RowErrors::new()),
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
                        )
                    }
                })
            }
            CompiledRelatedScalarExpr::InputOrElse { input, default } => Ok((
                match &self.batch.relations[relation].inputs[*input] {
                    Some(column) => column.clone(),
                    None => broadcast_scalar_literal::<N>(default, length),
                },
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::RootScalar(expr) => {
                let roots = self.root_rows_of(relation, mask);
                let (values, root_errors) = self.eval_scalar_expr(expr, &roots)?;
                let projected = project_root_column_to_related(
                    &values,
                    &self.batch.relations[relation].offsets,
                )?;
                Ok((projected, self.lower_errors(relation, &root_errors, mask)))
            }
            CompiledRelatedScalarExpr::ParameterLookup { parameter, index } => {
                let (keys, mut errors) = self.resolve_related_scalar(relation, index, mask)?;
                let live = mask.without(&errors);
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    &keys,
                    &live,
                    self.period,
                    &mut errors,
                );
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
                DenseColumn::Date(vec![self.period.start; length]),
                RowErrors::new(),
            )),
            CompiledRelatedScalarExpr::PeriodEnd => Ok((
                DenseColumn::Date(vec![self.period.end; length]),
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
                for related in mask.without(&errors).rows() {
                    errors.record(related, labels.failure(&subject, related));
                }
                Ok((placeholder, errors))
            }
            CompiledRelatedScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) =
                    self.eval_related_predicate(relation, condition, mask)?;
                let live = mask.without(&errors);
                let then_rows = live.filter(|related| condition[related]);
                let else_rows = live.difference(&then_rows);
                let (then_values, then_errors) =
                    self.resolve_related_scalar(relation, then_expr, &then_rows)?;
                let (else_values, else_errors) =
                    self.resolve_related_scalar(relation, else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors);
                let else_live = else_rows.without(&else_errors);
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
    ) -> Result<JudgmentEval, EvalError> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.eval_scalar_expr(left, mask)?;
                let live = mask.without(&errors);
                let (right, right_errors) = self.eval_scalar_expr(right, &live)?;
                let live = live.without(&right_errors);
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors);
                Ok((outcomes, errors))
            }
            CompiledJudgmentExpr::Derived(index) => self.evaluate_judgment(*index, mask),
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
}

impl<N: DenseNum> ColumnAxis<N, CompiledScalarExpr> for LifetimeExecutor<'_, N> {
    fn axis_len(&self) -> usize {
        self.row_count
    }

    fn eval_column(
        &mut self,
        expr: &CompiledScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
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
    ) -> Result<JudgmentEval, EvalError> {
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
            .map(|(period, batch)| DenseExecutor::new(program, period, batch, false))
            .collect();
        Self {
            program,
            period_executors,
            reference_period: periods.len() - 1,
            row_count,
            scalar_cache: (0..program.derived.len()).map(|_| None).collect(),
            judgment_cache: (0..program.derived.len()).map(|_| None).collect(),
        }
    }

    fn evaluate_scalar(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let pending = pending_rows(self.scalar_cache[derived_index].as_ref(), mask);
        let cached = match self.scalar_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = self.compute_scalar(derived_index, &pending)?;
                merge_scalar::<N>(cached, pending, computed)?
            }
        };
        let result = (cached.values.clone(), cached.errors.restricted_to(mask));
        self.scalar_cache[derived_index] = Some(cached);
        Ok(result)
    }

    fn compute_scalar(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        let CompiledSemantics::Scalar(expr) = &derived.semantics else {
            return Ok(fail_numeric::<N>(
                self.row_count,
                mask,
                EvalError::ExpectedScalar(derived.name.clone()),
            ));
        };
        let (column, errors) = self.eval_scalar(expr, mask)?;
        let mut errors = errors.within_rule(&derived.label);
        let column = match derived.rounding {
            Some(rounding) => {
                round_dense_column::<N>(column, rounding, &mask.without(&errors), &mut errors)
            }
            None => column,
        };
        Ok((column, errors))
    }

    fn evaluate_judgment(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError> {
        let pending = pending_rows(self.judgment_cache[derived_index].as_ref(), mask);
        let cached = match self.judgment_cache[derived_index].take() {
            Some(cached) if pending.is_empty() => cached,
            cached => {
                let computed = self.compute_judgment(derived_index, &pending)?;
                merge_judgment(cached, pending, computed)
            }
        };
        let result = (cached.values.clone(), cached.errors.restricted_to(mask));
        self.judgment_cache[derived_index] = Some(cached);
        Ok(result)
    }

    fn compute_judgment(
        &mut self,
        derived_index: usize,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError> {
        let program = self.program;
        let derived = &program.derived[derived_index];
        match &derived.semantics {
            CompiledSemantics::Judgment(expr) => self
                .eval_judgment(expr, mask)
                .map(|(values, errors)| (values, errors.within_rule(&derived.label))),
            CompiledSemantics::Scalar(_) => {
                let (_, errors) = fail_numeric::<N>(
                    self.row_count,
                    mask,
                    EvalError::ExpectedJudgment(derived.name.clone()),
                );
                Ok((vec![JudgmentOutcome::NotHolds; self.row_count], errors))
            }
        }
    }

    fn eval_scalar(
        &mut self,
        expr: &CompiledScalarExpr,
        mask: &RowMask,
    ) -> Result<DenseEval, EvalError> {
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
                Ok((broadcast_scalar_literal::<N>(value, len), RowErrors::new()))
            }
            CompiledScalarExpr::Derived(index) => self.evaluate_scalar(*index, mask),
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                // Period-specific: resolve at the reference period. The index
                // expression is itself lifetime-evaluated (typically a literal
                // or derived), then used as integer keys.
                let (keys, mut errors) = self.eval_scalar(index, mask)?;
                let live = mask.without(&errors);
                let column = lookup_parameter_dense::<N>(
                    &self.program.parameters[*parameter].parameter,
                    &keys,
                    &live,
                    self.period_executors[self.reference_period].period,
                    &mut errors,
                );
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
                for row in mask.without(&errors).rows() {
                    errors.record(row, labels.failure(&subject, row));
                }
                Ok((placeholder, errors))
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let (condition, mut errors) = self.eval_judgment(condition, mask)?;
                let live = mask.without(&errors);
                let then_rows = live.filter(|row| condition[row].is_holds());
                let else_rows = live.difference(&then_rows);
                let (then_values, then_errors) = self.eval_scalar(then_expr, &then_rows)?;
                let (else_values, else_errors) = self.eval_scalar(else_expr, &else_rows)?;
                let then_live = then_rows.without(&then_errors);
                let else_live = else_rows.without(&else_errors);
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
                DenseColumn::Date(vec![NaiveDate::default(); len]),
                "period_start",
            )),
            CompiledScalarExpr::PeriodEnd => Ok(ambiguous(
                DenseColumn::Date(vec![NaiveDate::default(); len]),
                "period_end",
            )),
            CompiledScalarExpr::DateAddDays { .. } => Ok(ambiguous(
                DenseColumn::Date(vec![NaiveDate::default(); len]),
                "date_add_days",
            )),
            CompiledScalarExpr::DateAddMonths { .. } => Ok(ambiguous(
                DenseColumn::Date(vec![NaiveDate::default(); len]),
                "date_add_months",
            )),
            CompiledScalarExpr::DateAddYears { .. } => Ok(ambiguous(
                DenseColumn::Date(vec![NaiveDate::default(); len]),
                "date_add_years",
            )),
            CompiledScalarExpr::DaysBetween { .. } => Ok(ambiguous(
                DenseColumn::Integer(vec![0; len]),
                "days_between",
            )),
            CompiledScalarExpr::CountRelated { .. } => Ok(ambiguous(
                DenseColumn::Integer(vec![0; len]),
                "count/len over a relation",
            )),
            CompiledScalarExpr::SumRelated { .. } => Ok(ambiguous(
                N::into_column(vec![N::ZERO; len]),
                "sum over a relation",
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
    ) -> Result<DenseEval, EvalError> {
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
            let mut counts = vec![0_i64; self.row_count];
            for executor in &mut self.period_executors {
                let (column, period_errors) = executor.eval_scalar_expr(value, &live)?;
                live = live.without(&period_errors);
                errors.absorb(period_errors);
                let mut count_errors = RowErrors::new();
                accumulate_nonzero_counts(&column, &live, &mut counts, &mut count_errors)?;
                live = live.without(&count_errors);
                errors.absorb(count_errors);
            }
            return Ok((DenseColumn::Integer(counts), errors));
        }

        // period-major: per_period[p][r] is entity r's inner value in period p.
        let mut per_period: Vec<Vec<N>> = Vec::with_capacity(period_count);
        for executor in &mut self.period_executors {
            let (column, period_errors) = executor.eval_scalar_expr(value, &live)?;
            live = live.without(&period_errors);
            errors.absorb(period_errors);
            let mut conversion = RowErrors::new();
            per_period.push(numeric_values::<N>(&column, &live, &mut conversion));
            live = live.without(&conversion);
            errors.absorb(conversion);
        }

        let mut result = vec![N::ZERO; self.row_count];
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
                live = live.without(&count_errors);
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
    ) -> Result<Vec<usize>, EvalError> {
        let mut counts = vec![0; self.row_count];
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
            let (column, period_errors) = executor.eval_scalar_expr(n, &live)?;
            live = live.without(&period_errors);
            errors.absorb(period_errors);
            per_period.push(column);
        }
        let (reference, others) = per_period
            .split_last()
            .expect("lifetime execution guarantees at least one period");
        // Reject a period-varying n: compare every earlier period against the
        // reference (chronologically-last) period, in each column's own dtype.
        let mut varying = RowErrors::new();
        for (index, column) in others.iter().enumerate() {
            for row in live.rows() {
                if !varying.contains(row) && values_differ(reference, column, row) {
                    varying.record(
                        row,
                        EvalError::OverPeriodsTopNPeriodVarying {
                            reduction,
                            first_period: period_label(self.period_executors[index].period),
                            first_value: dense_value_label(column, row),
                            second_period: period_label(
                                self.period_executors[self.reference_period].period,
                            ),
                            second_value: dense_value_label(reference, row),
                        },
                    );
                }
            }
        }
        live = live.without(&varying);
        errors.absorb(varying);

        // n is period-invariant: truncate the reference column toward zero to an
        // exact i64 and enforce 1 <= n <= period_count per row.
        let mut conversion = RowErrors::new();
        let raw = numeric_values::<N>(reference, &live, &mut conversion);
        live = live.without(&conversion);
        errors.absorb(conversion);
        for row in live.rows() {
            // Non-finite or beyond the i64 range: a garbage n, reported as out
            // of range rather than saturated to a clamp.
            let Some(as_i64) = raw[row].try_to_i64_trunc() else {
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
    ) -> Result<DenseEval, EvalError> {
        let mut errors = RowErrors::new();
        let mut live = mask.clone();
        let mut per_period: Vec<DenseColumn> = Vec::with_capacity(self.period_executors.len());
        for executor in &mut self.period_executors {
            let (column, period_errors) = executor.eval_scalar_expr(expr, &live)?;
            live = live.without(&period_errors);
            errors.absorb(period_errors);
            per_period.push(column);
        }
        // A single period is invariant by definition; nothing to compare.
        let (first, rest) = per_period
            .split_first()
            .expect("lifetime execution guarantees at least one period");
        for (offset, column) in rest.iter().enumerate() {
            for row in live.rows() {
                if !errors.contains(row) && values_differ(first, column, row) {
                    // `offset` indexes `rest`, so the diverging period is `offset + 1`.
                    errors.record(
                        row,
                        EvalError::LifetimePeriodVaryingInput {
                            input: input_name.to_string(),
                            first_period: period_label(self.period_executors[0].period),
                            first_value: dense_value_label(first, row),
                            second_period: period_label(self.period_executors[offset + 1].period),
                            second_value: dense_value_label(column, row),
                        },
                    );
                }
            }
        }
        // Every live row agrees across all periods: bind the (identical) first
        // period's column, preserving its original dtype for the caller.
        Ok((first.clone(), errors))
    }

    fn eval_judgment(
        &mut self,
        expr: &CompiledJudgmentExpr,
        mask: &RowMask,
    ) -> Result<JudgmentEval, EvalError> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.eval_scalar(left, mask)?;
                let live = mask.without(&errors);
                let (right, right_errors) = self.eval_scalar(right, &live)?;
                let live = live.without(&right_errors);
                errors.absorb(right_errors);
                let outcomes = compare_dense::<N>(&left, *op, &right, &live, &mut errors);
                Ok((outcomes, errors))
            }
            CompiledJudgmentExpr::Derived(index) => self.evaluate_judgment(*index, mask),
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
) -> Vec<N> {
    match column {
        DenseColumn::Integer(values) => {
            values.iter().map(|value| N::from_integer(*value)).collect()
        }
        DenseColumn::Decimal(values) => values.iter().map(N::from_decimal).collect(),
        DenseColumn::Float(values) => {
            let mut numbers = vec![N::ZERO; values.len()];
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
            errors.record_all(
                live,
                &EvalError::TypeMismatch("expected decimal-compatible dense column".to_string()),
            );
            vec![N::ZERO; column.len()]
        }
    }
}

/// A row's value as an exact integer key or count, under the explain path's
/// rule: an integer, or a decimal (or finite `f64`) with no fractional part
/// that fits in `i64`. Anything else, including a non-numeric value, is none.
fn index_at(column: &DenseColumn, row: usize) -> Option<i64> {
    match column {
        DenseColumn::Integer(values) => Some(values[row]),
        DenseColumn::Decimal(values) if values[row].fract().is_zero() => values[row].to_i64(),
        DenseColumn::Float(values) => {
            let value = values[row];
            // 2^63 is exactly representable; `value < 2^63` keeps the cast
            // exact rather than saturating.
            (value.fract() == 0.0
                && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value))
            .then_some(value as i64)
        }
        _ => None,
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
    parameter: &IndexedParameter,
    keys: &DenseColumn,
    live: &RowMask,
    period: &Period,
    errors: &mut RowErrors,
) -> DenseColumn {
    let len = keys.len();
    let version = parameter
        .versions
        .iter()
        .filter(|version| version.applies_at(period.start))
        .max_by_key(|version| version.effective_from);
    let mut lookup = |row: usize| -> Option<&ScalarValue> {
        let Some(key) = index_at(keys, row) else {
            errors.record(
                row,
                EvalError::TypeMismatch(format!(
                    "parameter key for `{}` must be an integer",
                    parameter.name
                )),
            );
            return None;
        };
        let value = version.and_then(|version| version.values.get(&key));
        if value.is_none() {
            errors.record(
                row,
                EvalError::MissingParameterValue {
                    parameter: parameter.name.clone(),
                    key,
                    at: period.start,
                },
            );
        }
        value
    };
    match version.map_or(TableKind::Numeric, |version| table_kind(&version.values)) {
        TableKind::Integer => {
            let mut values = vec![0; len];
            for row in live.rows() {
                if let Some(ScalarValue::Integer(value)) = lookup(row) {
                    values[row] = *value;
                }
            }
            DenseColumn::Integer(values)
        }
        TableKind::Bool => {
            let mut values = vec![false; len];
            for row in live.rows() {
                if let Some(ScalarValue::Bool(value)) = lookup(row) {
                    values[row] = *value;
                }
            }
            DenseColumn::Bool(values)
        }
        TableKind::Text => {
            let mut values = vec![String::new(); len];
            for row in live.rows() {
                if let Some(ScalarValue::Text(value)) = lookup(row) {
                    values[row] = value.clone();
                }
            }
            DenseColumn::Text(values)
        }
        TableKind::Date => {
            let mut values = vec![NaiveDate::default(); len];
            for row in live.rows() {
                if let Some(ScalarValue::Date(value)) = lookup(row) {
                    values[row] = *value;
                }
            }
            DenseColumn::Date(values)
        }
        TableKind::Numeric => {
            let mut values = vec![N::ZERO; len];
            let mut not_numeric = Vec::new();
            for row in live.rows() {
                if let Some(value) = lookup(row) {
                    match value.as_decimal() {
                        Some(number) => values[row] = N::from_decimal(&number),
                        None => not_numeric.push(row),
                    }
                }
            }
            for row in not_numeric {
                errors.record(
                    row,
                    EvalError::TypeMismatch(
                        "parameter values must be numeric in dense mode".to_string(),
                    ),
                );
            }
            N::into_column(values)
        }
    }
}

fn assign_rows<T: Clone>(target: &mut [T], source: &[T], rows: &RowMask) {
    for row in rows.rows() {
        target[row] = source[row].clone();
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
            assign_rows(&mut target, &source, else_rows);
            DenseColumn::Bool(target)
        }
        (DenseColumn::Integer(mut target), DenseColumn::Integer(source)) => {
            assign_rows(&mut target, &source, else_rows);
            DenseColumn::Integer(target)
        }
        (DenseColumn::Text(mut target), DenseColumn::Text(source)) => {
            assign_rows(&mut target, &source, else_rows);
            DenseColumn::Text(target)
        }
        (DenseColumn::Date(mut target), DenseColumn::Date(source)) => {
            assign_rows(&mut target, &source, else_rows);
            DenseColumn::Date(target)
        }
        (then_values, else_values) if is_numeric(&then_values) && is_numeric(&else_values) => {
            let mut numbers = numeric_values::<N>(&then_values, then_rows, errors);
            let others = numeric_values::<N>(&else_values, else_rows, errors);
            assign_rows(&mut numbers, &others, else_rows);
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
) -> Vec<JudgmentOutcome> {
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
    let mut outcomes = vec![JudgmentOutcome::NotHolds; left.len()];
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
            let left = numeric_values::<N>(left, live, &mut conversion);
            let right = numeric_values::<N>(right, live, &mut conversion);
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
    outcomes
}
