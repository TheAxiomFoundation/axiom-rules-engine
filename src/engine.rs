// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::Determination::{self, Determined, Indeterminate};
use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, JudgmentExpr, JudgmentOutcome,
    MissingFacts, Need, Needs, Period, Program, RelatedValueRef, ScalarExpr, ScalarValue,
};

/// Shift by calendar months, clamping a missing target day to month end.
/// Legal rules decide their own inclusive/exclusive boundaries around this
/// date operation. Overflow is an evaluation error, never a panic.
pub(crate) fn shift_calendar_months(
    base: chrono::NaiveDate,
    offset: i64,
) -> Result<chrono::NaiveDate, EvalError> {
    let result = u32::try_from(offset.unsigned_abs()).ok().and_then(|count| {
        let months = chrono::Months::new(count);
        if offset < 0 {
            base.checked_sub_months(months)
        } else {
            base.checked_add_months(months)
        }
    });
    result.ok_or_else(|| {
        EvalError::TypeMismatch(
            "date_add_months result is outside the supported date range".to_string(),
        )
    })
}

pub(crate) fn shift_calendar_years(
    base: chrono::NaiveDate,
    offset: i64,
) -> Result<chrono::NaiveDate, EvalError> {
    offset
        .checked_mul(12)
        .and_then(|months| shift_calendar_months(base, months).ok())
        .ok_or_else(|| {
            EvalError::TypeMismatch(
                "date_add_years result is outside the supported date range".to_string(),
            )
        })
}

pub(crate) fn shift_calendar_days(
    base: chrono::NaiveDate,
    offset: i64,
) -> Result<chrono::NaiveDate, EvalError> {
    chrono::TimeDelta::try_days(offset)
        .and_then(|days| base.checked_add_signed(days))
        .ok_or_else(|| {
            EvalError::TypeMismatch(
                "date_add_days result is outside the supported date range".to_string(),
            )
        })
}

// Decimal arithmetic for every evaluator (explain, bulk, dense). rust_decimal's
// operators panic when a result leaves the 96-bit range; an input or parameter
// large enough to overflow is an evaluation error, never a panic, and all
// three paths report it with the same message.

/// How a checked Decimal operation fails. It is small and `Copy`, so the
/// column loops pay nothing for it on success; `?` turns it into the
/// matching `EvalError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArithmeticError {
    Overflow(&'static str),
    DivisionByZero,
}

impl From<ArithmeticError> for EvalError {
    fn from(error: ArithmeticError) -> Self {
        match error {
            ArithmeticError::Overflow(operation) => EvalError::ArithmeticOverflow(operation),
            ArithmeticError::DivisionByZero => EvalError::DivisionByZero,
        }
    }
}

#[inline]
pub(crate) fn checked_add(left: Decimal, right: Decimal) -> Result<Decimal, ArithmeticError> {
    left.checked_add(right)
        .ok_or(ArithmeticError::Overflow("addition"))
}

#[inline]
pub(crate) fn checked_sub(left: Decimal, right: Decimal) -> Result<Decimal, ArithmeticError> {
    left.checked_sub(right)
        .ok_or(ArithmeticError::Overflow("subtraction"))
}

#[inline]
pub(crate) fn checked_mul(left: Decimal, right: Decimal) -> Result<Decimal, ArithmeticError> {
    left.checked_mul(right)
        .ok_or(ArithmeticError::Overflow("multiplication"))
}

/// A zero divisor is `DivisionByZero`; a quotient beyond the range (a large
/// dividend over a divisor below one) is an overflow.
#[inline]
pub(crate) fn checked_div(left: Decimal, right: Decimal) -> Result<Decimal, ArithmeticError> {
    if right.is_zero() {
        return Err(ArithmeticError::DivisionByZero);
    }
    left.checked_div(right)
        .ok_or(ArithmeticError::Overflow("division"))
}

#[derive(Clone, Debug, Error)]
pub enum EvalError {
    #[error("unknown derived output: {0}")]
    UnknownDerived(String),
    #[error("unknown parameter: {0}")]
    UnknownParameter(String),
    #[error("unknown relation: {0}")]
    UnknownRelation(String),
    #[error("missing input `{name}` for entity `{entity_id}` over {period_start}..{period_end}")]
    MissingInput {
        name: String,
        entity_id: String,
        period_start: chrono::NaiveDate,
        period_end: chrono::NaiveDate,
    },
    /// An evaluation under `MissingFacts::Indeterminate` ended unknown, at an
    /// entry point whose result cannot say so ([`Engine::evaluate_scalar`]).
    #[error("result is indeterminate: it needs {needs}")]
    Indeterminate { needs: Needs },
    #[error(
        "ambiguous input `{name}` for entity `{entity_id}`: records effective from {effective_from} have conflicting values; merge or split the spells so one value applies at each start date"
    )]
    AmbiguousInput {
        name: String,
        entity_id: String,
        effective_from: chrono::NaiveDate,
    },
    #[error(
        "no `match` arm in `{rule}` covers `{subject}` = {value} (arms: {patterns}); add an arm for it or a final `_ =>` arm"
    )]
    NoMatchingArm {
        rule: String,
        subject: String,
        value: String,
        patterns: String,
    },
    #[error("unit `{0}` was not declared")]
    UnknownUnit(String),
    #[error("type mismatch: {0}")]
    TypeMismatch(String),
    #[error("parameter `{parameter}` has no value for key `{key}` at {at}")]
    MissingParameterValue {
        parameter: String,
        key: i64,
        at: chrono::NaiveDate,
    },
    #[error("derived `{derived}` has no formula version at {at}")]
    MissingDerivedFormulaVersion {
        derived: String,
        at: chrono::NaiveDate,
    },
    /// Reported only for a hand-built `Program`: checked programs are acyclic.
    #[error("derived `{0}` depends on itself, so its evaluation cannot finish")]
    DependencyCycle(String),
    #[error("derived `{0}` is scalar, but a judgment was requested")]
    ExpectedJudgment(String),
    #[error("derived `{0}` is judgment, but a scalar was requested")]
    ExpectedScalar(String),
    #[error("division by zero")]
    DivisionByZero,
    #[error("arithmetic overflow: {0} result is outside the representable decimal range")]
    ArithmeticOverflow(&'static str),
    #[error(
        "over-periods reduction `{0}` is valid only under lifetime execution (execute_lifetime); it has no meaning in per-period execution"
    )]
    OverPeriodsOutsideLifetime(&'static str),
    #[error(
        "lifetime execution requires one input batch per period: got {periods} periods and {batches} batches"
    )]
    LifetimePeriodBatchMismatch { periods: usize, batches: usize },
    #[error("lifetime execution requires at least one period")]
    LifetimeNoPeriods,
    #[error(
        "lifetime execution requires every period's batch to have the same entity row count (positional alignment): period {period} has {row_count} rows but period 0 has {expected}"
    )]
    LifetimeRowCountMismatch {
        period: usize,
        row_count: usize,
        expected: usize,
    },
    #[error(
        "lifetime execution only supports outputs whose formula contains an over-periods reduction; `{0}` does not — use the per-period execute / execute_f64 entry points instead"
    )]
    LifetimeOutputWithoutReduction(String),
    #[error(
        "lifetime execution cannot evaluate `{0}` outside an over-periods reduction because it is period-specific; wrap it in a reduction (e.g. sum_over_periods) so its period is defined"
    )]
    LifetimeAmbiguousLeaf(String),
    #[error(
        "lifetime execution cannot evaluate input `{input}` outside an over-periods reduction: its value is not period-invariant for at least one entity — {first_period} it is {first_value} but {second_period} it is {second_value}; wrap it in a reduction (e.g. sum_over_periods) so its period is defined, or supply the same value for every period"
    )]
    LifetimePeriodVaryingInput {
        input: String,
        first_period: String,
        first_value: String,
        second_period: String,
        second_value: String,
    },
    #[error(
        "lifetime execution requires supplied periods in strictly ascending order by start date, but period {earlier_index} ({earlier}) does not start before period {later_index} ({later})"
    )]
    LifetimePeriodsNotAscending {
        earlier_index: usize,
        earlier: String,
        later_index: usize,
        later: String,
    },
    #[error(
        "{reduction} requires an integer n with 1 <= n <= the {period_count} supplied periods, but n resolved to {n} for at least one entity; a top-N sum over more periods than exist only pads with zeros (a no-op), so this masks a data error — supply an n within range or pad the period history explicitly"
    )]
    OverPeriodsTopNOutOfRange {
        reduction: &'static str,
        n: String,
        period_count: usize,
    },
    #[error(
        "{reduction}'s n is not period-invariant for at least one entity — {first_period} it is {first_value} but {second_period} it is {second_value}; n must resolve to the same value in every supplied period (parameter- and input-sourced n are held to the same contract)"
    )]
    OverPeriodsTopNPeriodVarying {
        reduction: &'static str,
        first_period: String,
        first_value: String,
        second_period: String,
        second_value: String,
    },
}
impl EvalError {
    /// Name the rule a `match` failure occurred in. The evaluator of the rule
    /// that contains the `match` fills this in as the error leaves it; rules
    /// further up the dependency chain leave it unchanged.
    pub(crate) fn within_rule(self, rule: &str) -> Self {
        match self {
            Self::NoMatchingArm {
                rule: existing,
                subject,
                value,
                patterns,
            } if existing.is_empty() => Self::NoMatchingArm {
                rule: rule.to_string(),
                subject,
                value,
                patterns,
            },
            other => other,
        }
    }
}

/// The error for a `match` subject that none of `patterns` covers.
pub(crate) fn no_matching_arm(
    subject: &ScalarExpr,
    value: &ScalarValue,
    patterns: &[ScalarExpr],
) -> EvalError {
    no_matching_arm_written(subject, value, patterns, false)
}

/// [`no_matching_arm`], with every decimal written normalized when
/// `normalize` is set. Under `MissingFacts::Indeterminate` it is: two records
/// with the same start whose decimals differ only in scale (`1.0`, `1.00`)
/// are both admitted, and which one a lookup reads depends on record order,
/// so a message that showed the scale would too.
fn no_matching_arm_written(
    subject: &ScalarExpr,
    value: &ScalarValue,
    patterns: &[ScalarExpr],
    normalize: bool,
) -> EvalError {
    EvalError::NoMatchingArm {
        rule: String::new(),
        subject: match_operand_text(subject, normalize),
        value: scalar_value_text(value, normalize),
        patterns: patterns
            .iter()
            .map(|pattern| match_operand_text(pattern, normalize))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The error for a `relation_member` evaluated without a relation context:
/// only a derived relation's predicate supplies one, so a `count`/`sum`
/// `where` clause or a rule body that reaches it fails.
pub(crate) fn relation_member_outside_derived_relation(relation: &str) -> EvalError {
    EvalError::TypeMismatch(format!(
        "relation predicate `{relation}` can only be evaluated inside a derived relation"
    ))
}

pub(crate) fn describe_match_operand(expr: &ScalarExpr) -> String {
    match_operand_text(expr, false)
}

fn match_operand_text(expr: &ScalarExpr, normalize: bool) -> String {
    match expr {
        ScalarExpr::Literal(value) => scalar_value_text(value, normalize),
        ScalarExpr::Input(name) | ScalarExpr::Derived(name) => name.clone(),
        ScalarExpr::InputOrElse { name, .. } => name.clone(),
        _ => "the match subject".to_string(),
    }
}

fn scalar_value_text(value: &ScalarValue, normalize: bool) -> String {
    match value {
        ScalarValue::Bool(value) => value.to_string(),
        ScalarValue::Integer(value) => value.to_string(),
        ScalarValue::Decimal(value) if normalize => value.normalize().to_string(),
        ScalarValue::Decimal(value) => value.to_string(),
        ScalarValue::Text(value) => format!("{value:?}"),
        ScalarValue::Date(value) => value.to_string(),
    }
}

/// The refusal, under `MissingFacts::Indeterminate`, of a construct the #215
/// series has not yet given three-valued semantics
/// (`docs/three-valued-values.md`, section 10); each later PR of the series
/// removes the uses it lifts. A refusal is not an evaluation failure: it is
/// its own interrupt, which no `and`/`or` item catches, so the evaluation that
/// reaches the construct is refused whatever the item's siblings hold, and
/// nothing reads an unknown there as `not_holds`, zero or an absent member.
/// The drive returns it as the `TypeMismatch` it carries.
fn unsupported_under_indeterminate(construct: &str) -> Interrupt {
    Interrupt::Refused(EvalError::TypeMismatch(format!(
        "missing_facts=indeterminate does not yet support {construct}"
    )))
}

/// A determined value where a construct reads one that this series has not
/// lifted yet. Only an evaluation under `MissingFacts::Indeterminate` can be
/// indeterminate, and there it is refused.
fn require_determined<T>(determination: Determination<T>, construct: &str) -> Eval<T> {
    match determination {
        Determined(value) => Ok(value),
        Indeterminate(_) => Err(unsupported_under_indeterminate(construct)),
    }
}

/// The order in which an `and`/`or`, and the program checks, choose among
/// failures: by message, then by variant and every field, so that the choice
/// depends on the errors and not on the position they were raised at. Two
/// errors can render the same message and differ in a field: a `match`
/// failure gains its rule's name only as it leaves that rule.
fn failure_order(error: &EvalError) -> (String, String) {
    (error.to_string(), format!("{error:?}"))
}

/// Keep the least of the failures seen so far under [`failure_order`].
fn keep_least_failure(least: &mut Option<EvalError>, error: EvalError) {
    let replace = match least {
        Some(kept) => failure_order(&error) < failure_order(kept),
        None => true,
    };
    if replace {
        *least = Some(error);
    }
}

fn relation_slots_out_of_range(
    relation: &str,
    arity: usize,
    current_slot: usize,
    related_slot: usize,
) -> EvalError {
    EvalError::TypeMismatch(format!(
        "relation `{relation}` has arity {arity}, but slots {current_slot} and {related_slot} were requested"
    ))
}

/// Validate the one unresolvable tie in the input-spell precedence contract.
///
/// Covering records are resolved by latest `interval.start`. Two values for the
/// same canonical fact, entity, and start date have equal precedence, so
/// choosing either would reintroduce dataset-order semantics. Equal duplicates
/// are harmless; conflicting values are rejected before either execution mode
/// runs.
pub(crate) fn validate_input_spells(data: &DataSet) -> Result<(), EvalError> {
    let mut values_by_start: HashMap<(&str, &str, chrono::NaiveDate), &ScalarValue> =
        HashMap::new();
    for record in &data.inputs {
        let key = (
            record.name.as_str(),
            record.entity_id.as_str(),
            record.interval.start,
        );
        if let Some(existing) = values_by_start.get(&key) {
            if *existing != &record.value {
                return Err(EvalError::AmbiguousInput {
                    name: record.name.clone(),
                    entity_id: record.entity_id.clone(),
                    effective_from: record.interval.start,
                });
            }
        } else {
            values_by_start.insert(key, &record.value);
        }
    }
    Ok(())
}

/// Build the order-independent, single-period input view consumed by Fast.
///
/// Bulk execution handles one shared query period and otherwise falls back to
/// Explain. Resolve every fact (including facts on related entities) once in a
/// single O(N) pass so the bulk evaluator cannot overwrite a newer covering
/// spell with an older record that happens to appear later in the dataset.
pub(crate) fn resolve_inputs_for_period(data: &DataSet, period: &Period) -> DataSet {
    let mut selected_by_fact: HashMap<(&str, &str), usize> = HashMap::new();
    let mut inputs = Vec::new();

    for record in &data.inputs {
        if !record.interval.contains_period(period) {
            continue;
        }
        let key = (record.name.as_str(), record.entity_id.as_str());
        if let Some(&selected_index) = selected_by_fact.get(&key) {
            let selected: &crate::model::InputRecord = &inputs[selected_index];
            if record.interval.start > selected.interval.start {
                inputs[selected_index] = record.clone();
            }
        } else {
            selected_by_fact.insert(key, inputs.len());
            inputs.push(record.clone());
        }
    }

    DataSet {
        inputs,
        relations: data.relations.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CacheKey {
    pub(crate) derived: String,
    pub(crate) entity_id: String,
    pub(crate) period: Period,
}

/// A derived-rule evaluation the interpreter deferred to its driver instead of
/// recursing into it (see [`crate::depth`]). Whether a scalar or a judgment
/// was asked for is part of it: a reference of the wrong kind fails where one
/// of the right kind would not. The drive's record of rule evaluations that
/// failed is keyed the same way, deferred or not (see [`Engine::failed`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Deferred {
    Scalar(CacheKey),
    Judgment(CacheKey),
}

impl Deferred {
    fn rule(&self) -> &str {
        match self {
            Self::Scalar(key) | Self::Judgment(key) => &key.derived,
        }
    }
}

/// Why an evaluation inside the interpreter stopped before producing a value:
/// an evaluation error; the refusal of a construct `MissingFacts::Indeterminate`
/// does not support yet (see [`unsupported_under_indeterminate`]), which
/// nothing catches; or a rule deferred to the driver, which evaluates it and
/// retries. Only the drivers see `Defer`; the public entry points return
/// `EvalError`, and a refusal as the `TypeMismatch` it carries.
#[derive(Clone, Debug)]
enum Interrupt {
    Error(EvalError),
    Refused(EvalError),
    Defer(Deferred),
}

impl Interrupt {
    fn within_rule(self, rule: &str) -> Self {
        match self {
            Self::Error(error) => Self::Error(error.within_rule(rule)),
            other => other,
        }
    }
}

impl From<EvalError> for Interrupt {
    fn from(error: EvalError) -> Self {
        Self::Error(error)
    }
}

impl From<ArithmeticError> for Interrupt {
    fn from(error: ArithmeticError) -> Self {
        Self::Error(error.into())
    }
}

type Eval<T> = Result<T, Interrupt>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraceSkipReason {
    ShortCircuit,
    BranchNotSelected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SkippedTraceDependency {
    Derived {
        key: CacheKey,
        reason: TraceSkipReason,
    },
    Parameter {
        parameter: String,
        reason: TraceSkipReason,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParameterTraceRead {
    pub(crate) parameter: String,
    pub(crate) index: i64,
    pub(crate) value: ScalarValue,
    pub(crate) effective_from: chrono::NaiveDate,
    pub(crate) effective_to: Option<chrono::NaiveDate>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NodeExecutionTrace {
    pub(crate) dependencies: Vec<CacheKey>,
    pub(crate) not_evaluated_dependencies: Vec<SkippedTraceDependency>,
    pub(crate) parameter_reads: Vec<ParameterTraceRead>,
}

#[derive(Clone, Debug)]
pub(crate) enum EvaluatedTraceValue {
    Scalar {
        value: ScalarValue,
        pre_rounding_value: Option<ScalarValue>,
    },
    Judgment(JudgmentOutcome),
}

#[derive(Clone, Debug)]
pub(crate) struct EvaluatedTraceInstance {
    pub(crate) key: CacheKey,
    pub(crate) value: EvaluatedTraceValue,
    pub(crate) execution: NodeExecutionTrace,
}

#[derive(Clone, Copy)]
struct RelationEvalContext<'a> {
    current_id: &'a str,
    related_id: &'a str,
    current_entity: Option<&'a str>,
    related_entity: Option<&'a str>,
}

impl<'a> RelationEvalContext<'a> {
    fn entity_id_for(self, entity: &str) -> Option<&'a str> {
        if self.current_entity == Some(entity) {
            return Some(self.current_id);
        }
        if self.related_entity == Some(entity) {
            return Some(self.related_id);
        }
        None
    }
}

/// How an [`Engine`] evaluates. The default is what [`Engine::new`] does.
///
/// Incomplete until the #215 series lands (`docs/three-valued-values.md`,
/// section 14), and not yet reachable from a request.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvaluationOptions {
    pub missing_facts: MissingFacts,
}

/// Something the program checks walk: a rule's formula for the period, or a
/// derived relation's source and predicate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Checked {
    Rule(String),
    Relation(String),
}

/// What a reference to a rule asks for.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Scalar,
    Judgment,
}

pub struct Engine<'a> {
    program: &'a Program,
    options: EvaluationOptions,
    /// Whether a public entry point runs the program checks under
    /// `MissingFacts::Indeterminate`. Only a unit test turns them off, to
    /// reach a wrong-kind reference the checks would refuse.
    program_checks: bool,
    /// What the program checks have passed, per period: each rule and
    /// derived relation is walked once per period.
    checked: HashMap<Period, HashSet<Checked>>,
    input_index: HashMap<(String, String), Vec<&'a crate::model::InputRecord>>,
    relation_index: HashMap<(String, usize, String), Vec<&'a crate::model::RelationRecord>>,
    scalar_cache: HashMap<CacheKey, ScalarValue>,
    /// Pre-rounding value of a currency rule whose declared rounding changed it,
    /// keyed like `scalar_cache`. Only populated when rounding actually moved
    /// the value; the trace uses it to show the rounding step for audit.
    pre_rounding_cache: HashMap<CacheKey, ScalarValue>,
    judgment_cache: HashMap<CacheKey, JudgmentOutcome>,
    /// Scalar rule evaluations that ended indeterminate, with their needs,
    /// keyed like `scalar_cache`. Only `MissingFacts::Indeterminate` fills it.
    indeterminate_scalars: HashMap<CacheKey, Needs>,
    /// Judgment rule evaluations that ended unknown, with their needs, keyed
    /// like `judgment_cache`. Only `MissingFacts::Indeterminate` fills it.
    undetermined_judgments: HashMap<CacheKey, Needs>,
    /// Rule evaluations that failed or were refused in the current drive,
    /// under `MissingFacts::Indeterminate`, keyed by what was asked for. Each
    /// is raised again at every later reference in the drive instead of being
    /// evaluated again (see [`Engine::remember_failure`] and
    /// [`Engine::run_tasks`]). It holds no deferral.
    failed: HashMap<Deferred, Interrupt>,
    execution_trace: HashMap<CacheKey, NodeExecutionTrace>,
    active_evaluations: Vec<CacheKey>,
    /// Whether to record the per-node trace. The bulk evaluator borrows this
    /// interpreter for per-entity relation work and never reads a trace.
    tracing: bool,
    /// Expression levels on the stack since the driver's current task began.
    depth: usize,
    /// The level at which a reference to a rule not yet evaluated is deferred
    /// to the driver (see [`crate::depth`]).
    suspend_depth: usize,
    /// How far each `count`/`sum` over related entities has got in the
    /// current drive (see [`Step`]).
    folds: HashMap<Step, Progress<FoldResume, (usize, Decimal)>>,
    /// How far each resolution of a relation's members has got in the current
    /// drive (see [`Step`]).
    members: HashMap<Step, Progress<MembersResume, Vec<String>>>,
    /// Expression levels evaluated, members a `count`/`sum` walked and
    /// relation tuples read since the driver last reported them (see
    /// [`crate::depth::count_visits`]).
    visits: usize,
}

/// A loop over data that a deferral can interrupt: a `count`/`sum` over one
/// entity's related entities, or the resolution of one entity's members of a
/// relation, which tests a derived relation's predicate on each candidate.
/// A deferral inside one retries the rule evaluation it runs in, which reaches
/// the loop again. Without a record of where it stood, the retry would walk
/// every member before the one that deferred again, and every loop the
/// evaluation had already finished, so a household whose members each defer
/// would cost a pass per member.
///
/// So the drive records each loop's progress under its `Step`, and the retry
/// resumes an interrupted loop at the member that deferred and reuses a
/// finished loop's result. A step is keyed by the rule evaluation it runs in
/// (`parent`) as well as by what it evaluates: a loop's trace records go to
/// that evaluation, and the retry of the same evaluation already holds them
/// (records are deduplicated by first occurrence), while any other evaluation
/// walks the loop itself and records them there, as recursion does.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Step {
    site: Site,
    entity_id: String,
    period: Period,
    parent: Option<CacheKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Site {
    /// A `count`/`sum`, identified by its node's address. That is sound
    /// because every expression the engine evaluates is borrowed from the
    /// program for the engine's whole life: the evaluation functions take
    /// `&'a` expressions, so the compiler refuses to evaluate a copy, whose
    /// address another expression could reuse.
    Fold(usize),
    /// A relation's members, read from one slot to another.
    Members {
        relation: String,
        current_slot: usize,
        related_slot: usize,
    },
}

/// A loop's progress in the current drive.
#[derive(Debug)]
enum Progress<P, D> {
    /// Interrupted by a deferral; the retry resumes here.
    Pending(P),
    /// Finished; the retry reuses the result.
    Done(D),
}

/// An interrupted aggregation: its related entities, the member that
/// deferred (evaluated again, from its `where` clause, on the retry), and the
/// count and total of the members before it.
#[derive(Debug)]
struct FoldResume {
    related_ids: Vec<String>,
    next: usize,
    count: usize,
    total: Decimal,
}

/// An interrupted resolution of a derived relation's members: the candidates
/// its source relation gives, the candidate whose predicate deferred, and the
/// candidates before it that the predicate selected. The relation's own
/// tuples are read once the resolution finishes, so no retry reads them.
#[derive(Debug)]
struct MembersResume {
    candidates: Vec<String>,
    next: usize,
    selected: Vec<String>,
}

impl<'a> Engine<'a> {
    /// Evaluate `program` over `data` as given. This low-level constructor
    /// performs no relation entity typing or dataset binding checks: callers
    /// that build a `Program` themselves must run
    /// [`crate::relation_typing::check_program`] first, or go through
    /// `execute_request`, which does.
    pub fn new(program: &'a Program, data: &'a DataSet) -> Self {
        Self::with_tracing(program, data, true)
    }

    /// An interpreter that computes exactly what [`Engine::new`] computes but
    /// records no trace. The bulk evaluator uses it for per-entity work
    /// (relation aggregations) so that work follows the reference semantics
    /// by construction.
    pub(crate) fn new_untraced(program: &'a Program, data: &'a DataSet) -> Self {
        Self::with_tracing(program, data, false)
    }

    fn with_tracing(program: &'a Program, data: &'a DataSet, tracing: bool) -> Self {
        let mut input_index: HashMap<(String, String), Vec<&'a crate::model::InputRecord>> =
            HashMap::new();
        for record in &data.inputs {
            input_index
                .entry((record.name.clone(), record.entity_id.clone()))
                .or_default()
                .push(record);
        }
        for records in input_index.values_mut() {
            records.sort_by_key(|record| std::cmp::Reverse(record.interval.start));
        }

        let mut relation_index: HashMap<
            (String, usize, String),
            Vec<&'a crate::model::RelationRecord>,
        > = HashMap::new();
        for record in &data.relations {
            for (slot, value) in record.tuple.iter().enumerate() {
                relation_index
                    .entry((record.name.clone(), slot, value.clone()))
                    .or_default()
                    .push(record);
            }
        }

        Self {
            program,
            options: EvaluationOptions::default(),
            program_checks: true,
            checked: HashMap::new(),
            input_index,
            relation_index,
            scalar_cache: HashMap::new(),
            pre_rounding_cache: HashMap::new(),
            judgment_cache: HashMap::new(),
            indeterminate_scalars: HashMap::new(),
            undetermined_judgments: HashMap::new(),
            failed: HashMap::new(),
            execution_trace: HashMap::new(),
            active_evaluations: Vec::new(),
            tracing,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            folds: HashMap::new(),
            members: HashMap::new(),
            visits: 0,
        }
    }

    /// An interpreter that evaluates under `options`. Under
    /// `MissingFacts::Error`, the default, it computes exactly what
    /// [`Engine::new`] computes. Under `MissingFacts::Indeterminate`, an
    /// input no record covers makes the result that reads it unknown, naming
    /// the input (`docs/three-valued-values.md`, section 5), and each public
    /// evaluation first checks the program for defects no dataset can change
    /// (section 4).
    ///
    /// Fallible because later PRs of the #215 series validate the dataset
    /// here; for now it always succeeds. Incomplete until that series lands,
    /// and not yet reachable from a request.
    #[doc(hidden)]
    pub fn with_options(
        program: &'a Program,
        data: &'a DataSet,
        options: EvaluationOptions,
    ) -> Result<Self, EvalError> {
        let mut engine = Self::with_tracing(program, data, true);
        engine.options = options;
        Ok(engine)
    }

    fn three_valued(&self) -> bool {
        self.options.missing_facts == MissingFacts::Indeterminate
    }

    /// The value of a scalar rule. A result that is indeterminate, which only
    /// an engine built with [`Engine::with_options`] can reach, is
    /// [`EvalError::Indeterminate`]; [`Engine::scalar_determination`] returns
    /// it as a value.
    pub fn evaluate_scalar(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        match self.scalar_determination(derived_name, entity_id, period)? {
            Determined(value) => Ok(value),
            Indeterminate(needs) => Err(EvalError::Indeterminate { needs }),
        }
    }

    /// The outcome of a judgment rule; an unknown one is
    /// [`JudgmentOutcome::Undetermined`].
    pub fn evaluate_judgment(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<JudgmentOutcome, EvalError> {
        Ok(
            match self.judgment_determination(derived_name, entity_id, period)? {
                Determined(outcome) => outcome,
                Indeterminate(_) => JudgmentOutcome::Undetermined,
            },
        )
    }

    /// The value of a scalar rule, or the inputs it needs when it is
    /// indeterminate.
    pub fn scalar_determination(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<Determination<ScalarValue>, EvalError> {
        self.check_rule(derived_name, Kind::Scalar, period)?;
        self.drive(|engine| engine.scalar_rule(derived_name, entity_id, period))
    }

    /// The outcome of a judgment rule, or the inputs it needs when it is
    /// unknown.
    pub fn judgment_determination(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<Determination<JudgmentOutcome>, EvalError> {
        self.check_rule(derived_name, Kind::Judgment, period)?;
        self.drive(|engine| engine.judgment_rule(derived_name, entity_id, period))
    }

    /// Run `root` to completion. When it defers a rule (see [`crate::depth`]),
    /// evaluate that rule as a task of its own, from level zero, and retry.
    /// Tasks form a stack: a task can defer a rule it reaches, which runs
    /// first. The first error ends the evaluation, as it would the recursive
    /// one: an error propagates unchanged from the rule it occurs in to the
    /// root, so a task's error is the root's. Under
    /// `MissingFacts::Indeterminate` an error can instead be caught on its
    /// way, by an `and`/`or` item, so a rule's error is recorded and raised
    /// again wherever the rule is referenced later in the drive (see
    /// [`Engine::remember_failure`] and [`Engine::run_tasks`]).
    fn drive<T>(&mut self, root: impl FnMut(&mut Self) -> Eval<T>) -> Result<T, EvalError> {
        let result = self.run_tasks(root);
        // Every retry reaches the loops its first attempt was interrupted in,
        // and resumes them; only an error leaves one pending.
        debug_assert!(
            result.is_err()
                || (self
                    .folds
                    .values()
                    .all(|progress| matches!(progress, Progress::Done(_)))
                    && self
                        .members
                        .values()
                        .all(|progress| matches!(progress, Progress::Done(_))))
        );
        self.folds.clear();
        self.members.clear();
        self.failed.clear();
        crate::depth::add_visits(std::mem::take(&mut self.visits));
        result
    }

    fn run_tasks<T>(&mut self, mut root: impl FnMut(&mut Self) -> Eval<T>) -> Result<T, EvalError> {
        let mut tasks: Vec<Deferred> = Vec::new();
        let mut waiting: HashSet<Deferred> = HashSet::new();
        loop {
            self.depth = 0;
            let interrupt = match tasks.last().cloned() {
                None => match root(self) {
                    Ok(value) => return Ok(value),
                    Err(interrupt) => interrupt,
                },
                Some(task) => {
                    let evaluated = match &task {
                        Deferred::Scalar(key) => self
                            .scalar_rule(&key.derived, &key.entity_id, &key.period)
                            .map(|_| ()),
                        Deferred::Judgment(key) => self
                            .judgment_rule(&key.derived, &key.entity_id, &key.period)
                            .map(|_| ()),
                    };
                    match evaluated {
                        // A task that ends unknown is finished too: its
                        // result is cached.
                        Ok(()) => {
                            tasks.pop();
                            waiting.remove(&task);
                            continue;
                        }
                        // The task cannot tell whether the reference that
                        // deferred it sits in an `and`/`or` item, which
                        // catches a failure under the mode. So the failure is
                        // recorded against the deferred request and the
                        // interrupted evaluation retried: it reaches the
                        // reference again, finds the record instead of
                        // deferring, and fails there, where an item can catch
                        // it. A refusal is recorded the same way, as a
                        // refusal, so that it is caught nowhere and ends the
                        // evaluation as an error does. The rule has already
                        // recorded a failure of its formula (see
                        // `remember_failure`); this also records one raised
                        // before the formula runs, such as a missing formula
                        // version, which the retry would otherwise defer
                        // again.
                        Err(failure @ (Interrupt::Error(_) | Interrupt::Refused(_)))
                            if self.three_valued() =>
                        {
                            tasks.pop();
                            waiting.remove(&task);
                            self.failed.insert(task, failure);
                            continue;
                        }
                        Err(interrupt) => interrupt,
                    }
                }
            };
            match interrupt {
                Interrupt::Error(error) | Interrupt::Refused(error) => return Err(error),
                // A rule deferred while a task waiting on it is still open
                // reaches itself.
                Interrupt::Defer(deferred) if waiting.contains(&deferred) => {
                    return Err(EvalError::DependencyCycle(deferred.rule().to_string()));
                }
                Interrupt::Defer(deferred) => {
                    waiting.insert(deferred.clone());
                    tasks.push(deferred);
                }
            }
        }
    }

    /// Evaluate one expression level: count it for the deferral threshold.
    fn nested<T>(&mut self, evaluate: impl FnOnce(&mut Self) -> Eval<T>) -> Eval<T> {
        self.visits += 1;
        self.depth += 1;
        let result = evaluate(self);
        self.depth -= 1;
        result
    }

    fn scalar_rule(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Determination<ScalarValue>> {
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.scalar_cache.get(&key) {
            return Ok(Determined(value.clone()));
        }
        if let Some(needs) = self.indeterminate_scalars.get(&key) {
            return Ok(Indeterminate(needs.clone()));
        }
        // A failure this request met earlier in the drive is raised again
        // here, where the reference sits (see `remember_failure`).
        if !self.failed.is_empty() {
            if let Some(failure) = self.failed.get(&Deferred::Scalar(key.clone())) {
                return Err(failure.clone());
            }
        }
        if self.depth >= self.suspend_depth {
            return Err(Interrupt::Defer(Deferred::Scalar(key)));
        }

        let derived = self.get_derived(derived_name)?;
        self.validate_unit(derived)?;
        let semantics = derived.semantics_at(period).ok_or_else(|| {
            EvalError::MissingDerivedFormulaVersion {
                derived: derived_name.to_string(),
                at: period.start,
            }
        })?;
        if self.tracing {
            self.execution_trace.entry(key.clone()).or_default();
        }
        self.active_evaluations.push(key.clone());
        let evaluated = match semantics {
            DerivedSemantics::Scalar(expr) => {
                self.eval_scalar_expr_inner(expr, entity_id, period, None)
            }
            DerivedSemantics::Judgment(_) => {
                Err(EvalError::ExpectedScalar(derived_name.to_string()).into())
            }
        };
        let finished = self
            .active_evaluations
            .pop()
            .expect("scalar evaluation pushed an active trace key");
        debug_assert_eq!(finished, key);
        let value = match evaluated.map_err(|interrupt| {
            interrupt.within_rule(derived.id.as_deref().unwrap_or(derived_name))
        }) {
            Ok(Determined(value)) => value,
            Ok(Indeterminate(needs)) => {
                self.indeterminate_scalars.insert(key, needs.clone());
                return Ok(Indeterminate(needs));
            }
            Err(interrupt) => {
                return Err(self.remember_failure(Deferred::Scalar(key), interrupt));
            }
        };
        // Apply the rule's opt-in output rounding before caching, so the
        // rounded value is what both direct queries and dependent rules
        // (`ScalarExpr::Derived`) observe. Absent `rounding` is a no-op. When
        // rounding actually moves the value, keep the pre-rounding amount so the
        // trace can show the rounding step.
        let rounded = apply_output_rounding(derived, value.clone());
        if self.tracing && rounded != value {
            self.pre_rounding_cache.insert(key.clone(), value);
        }
        self.scalar_cache.insert(key, rounded.clone());
        Ok(Determined(rounded))
    }

    fn judgment_rule(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Determination<JudgmentOutcome>> {
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.judgment_cache.get(&key) {
            return Ok(Determined(*value));
        }
        if let Some(needs) = self.undetermined_judgments.get(&key) {
            return Ok(Indeterminate(needs.clone()));
        }
        if !self.failed.is_empty() {
            if let Some(failure) = self.failed.get(&Deferred::Judgment(key.clone())) {
                return Err(failure.clone());
            }
        }
        if self.depth >= self.suspend_depth {
            return Err(Interrupt::Defer(Deferred::Judgment(key)));
        }

        let derived = self.get_derived(derived_name)?;
        self.validate_unit(derived)?;
        let semantics = derived.semantics_at(period).ok_or_else(|| {
            EvalError::MissingDerivedFormulaVersion {
                derived: derived_name.to_string(),
                at: period.start,
            }
        })?;
        if self.tracing {
            self.execution_trace.entry(key.clone()).or_default();
        }
        self.active_evaluations.push(key.clone());
        let evaluated = match semantics {
            DerivedSemantics::Judgment(expr) => {
                self.eval_judgment_expr_inner(expr, entity_id, period, None)
            }
            DerivedSemantics::Scalar(_) => {
                Err(EvalError::ExpectedJudgment(derived_name.to_string()).into())
            }
        };
        let finished = self
            .active_evaluations
            .pop()
            .expect("judgment evaluation pushed an active trace key");
        debug_assert_eq!(finished, key);
        let value = match evaluated.map_err(|interrupt| {
            interrupt.within_rule(derived.id.as_deref().unwrap_or(derived_name))
        }) {
            Ok(Determined(value)) => value,
            Ok(Indeterminate(needs)) => {
                self.undetermined_judgments.insert(key, needs.clone());
                return Ok(Indeterminate(needs));
            }
            Err(interrupt) => {
                return Err(self.remember_failure(Deferred::Judgment(key), interrupt));
            }
        };
        self.judgment_cache.insert(key, value);
        Ok(Determined(value))
    }

    /// Under `MissingFacts::Indeterminate`, remember for the rest of the drive
    /// that rule evaluation `request` failed or was refused, and return the
    /// interrupt. Every later reference raises it again from the record (see
    /// [`Engine::scalar_rule`]) instead of evaluating the rule again. An
    /// `and`/`or` item catches a failure, so without the record a rule that
    /// fails would be evaluated once per reference, and a chain whose every
    /// level references the level below twice would double its work per
    /// level (`docs/three-valued-values.md`, 5.9). Like a cached value, the
    /// failure is intrinsic to the rule, entity and period: the rule's body is
    /// evaluated the same way wherever it is referenced from. A deferral is
    /// not a failure and is not kept. Without the mode the first failure ends
    /// the drive, so nothing is kept.
    fn remember_failure(&mut self, request: Deferred, failure: Interrupt) -> Interrupt {
        if self.three_valued() && !matches!(failure, Interrupt::Defer(_)) {
            self.failed.insert(request, failure.clone());
        }
        failure
    }

    pub(crate) fn evaluated_trace_instances(
        &self,
        period: &Period,
        root_entity_id: &str,
    ) -> Vec<EvaluatedTraceInstance> {
        let mut instances = Vec::with_capacity(self.scalar_cache.len() + self.judgment_cache.len());
        for (key, value) in &self.scalar_cache {
            if key.period != *period {
                continue;
            }
            instances.push(EvaluatedTraceInstance {
                key: key.clone(),
                value: EvaluatedTraceValue::Scalar {
                    value: value.clone(),
                    pre_rounding_value: self.pre_rounding_cache.get(key).cloned(),
                },
                execution: self.execution_trace.get(key).cloned().unwrap_or_default(),
            });
        }
        for (key, outcome) in &self.judgment_cache {
            if key.period != *period {
                continue;
            }
            instances.push(EvaluatedTraceInstance {
                key: key.clone(),
                value: EvaluatedTraceValue::Judgment(*outcome),
                execution: self.execution_trace.get(key).cloned().unwrap_or_default(),
            });
        }
        let mut reachable = HashSet::new();
        let mut pending: Vec<_> = instances
            .iter()
            .filter(|instance| instance.key.entity_id == root_entity_id)
            .map(|instance| instance.key.clone())
            .collect();
        while let Some(key) = pending.pop() {
            if !reachable.insert(key.clone()) {
                continue;
            }
            if let Some(execution) = self.execution_trace.get(&key) {
                pending.extend(execution.dependencies.iter().cloned());
            }
        }
        instances.retain(|instance| reachable.contains(&instance.key));
        instances
    }

    pub fn cached_scalar(
        &self,
        derived: &str,
        entity_id: &str,
        period: &Period,
    ) -> Option<ScalarValue> {
        self.scalar_cache
            .get(&CacheKey {
                derived: derived.to_string(),
                entity_id: entity_id.to_string(),
                period: period.clone(),
            })
            .cloned()
    }

    /// The pre-rounding value of a derived output, present only when the rule
    /// declared rounding AND rounding changed the value. Lets the trace show the
    /// value before the statutory rounding step was applied.
    pub fn cached_pre_rounding(
        &self,
        derived: &str,
        entity_id: &str,
        period: &Period,
    ) -> Option<ScalarValue> {
        self.pre_rounding_cache
            .get(&CacheKey {
                derived: derived.to_string(),
                entity_id: entity_id.to_string(),
                period: period.clone(),
            })
            .cloned()
    }

    pub fn cached_judgment(
        &self,
        derived: &str,
        entity_id: &str,
        period: &Period,
    ) -> Option<JudgmentOutcome> {
        self.judgment_cache
            .get(&CacheKey {
                derived: derived.to_string(),
                entity_id: entity_id.to_string(),
                period: period.clone(),
            })
            .copied()
    }

    /// The needs of a rule evaluation that ended unknown, scalar or judgment.
    pub fn needs(&self, derived: &str, entity_id: &str, period: &Period) -> Option<Needs> {
        let key = CacheKey {
            derived: derived.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        self.indeterminate_scalars
            .get(&key)
            .or_else(|| self.undetermined_judgments.get(&key))
            .cloned()
    }

    fn record_evaluated_dependency(&mut self, key: CacheKey) {
        if !self.tracing {
            return;
        }
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        let trace = self.execution_trace.entry(parent).or_default();
        if !trace.dependencies.contains(&key) {
            trace.dependencies.push(key);
        }
    }

    fn record_skipped_dependency(&mut self, dependency: SkippedTraceDependency) {
        if !self.tracing {
            return;
        }
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        let trace = self.execution_trace.entry(parent).or_default();
        if !trace.not_evaluated_dependencies.contains(&dependency) {
            trace.not_evaluated_dependencies.push(dependency);
        }
    }

    fn record_parameter_read(&mut self, read: ParameterTraceRead) {
        if !self.tracing {
            return;
        }
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        let trace = self.execution_trace.entry(parent).or_default();
        if !trace.parameter_reads.contains(&read) {
            trace.parameter_reads.push(read);
        }
    }

    fn record_skipped_scalar_dependencies(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
        reason: TraceSkipReason,
    ) {
        if !self.tracing {
            return;
        }
        let mut derived = Vec::new();
        let mut parameters = Vec::new();
        collect_scalar_trace_references(expr, &mut derived, &mut parameters);
        for name in derived {
            let target_entity_id = self
                .program
                .derived
                .get(&name)
                .and_then(|dependency| {
                    relation_context.and_then(|context| context.entity_id_for(&dependency.entity))
                })
                .unwrap_or(entity_id)
                .to_string();
            self.record_skipped_dependency(SkippedTraceDependency::Derived {
                key: CacheKey {
                    derived: name,
                    entity_id: target_entity_id,
                    period: period.clone(),
                },
                reason,
            });
        }
        for parameter in parameters {
            self.record_skipped_dependency(SkippedTraceDependency::Parameter { parameter, reason });
        }
    }

    fn record_skipped_judgment_dependencies(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
        reason: TraceSkipReason,
    ) {
        if !self.tracing {
            return;
        }
        let mut derived = Vec::new();
        let mut parameters = Vec::new();
        collect_judgment_trace_references(expr, &mut derived, &mut parameters);
        for name in derived {
            let target_entity_id = self
                .program
                .derived
                .get(&name)
                .and_then(|dependency| {
                    relation_context.and_then(|context| context.entity_id_for(&dependency.entity))
                })
                .unwrap_or(entity_id)
                .to_string();
            self.record_skipped_dependency(SkippedTraceDependency::Derived {
                key: CacheKey {
                    derived: name,
                    entity_id: target_entity_id,
                    period: period.clone(),
                },
                reason,
            });
        }
        for parameter in parameters {
            self.record_skipped_dependency(SkippedTraceDependency::Parameter { parameter, reason });
        }
    }

    /// The rule `name`, borrowed from the program rather than copied: an
    /// aggregation's address in it identifies the aggregation (see
    /// [`Site::Fold`]).
    fn get_derived(&self, name: &str) -> Result<&'a Derived, EvalError> {
        let program: &'a Program = self.program;
        program
            .derived
            .get(name)
            .ok_or_else(|| EvalError::UnknownDerived(name.to_string()))
    }

    fn validate_unit(&self, derived: &Derived) -> Result<(), EvalError> {
        if let Some(unit) = &derived.unit {
            if !self.program.units.contains_key(unit) {
                return Err(EvalError::UnknownUnit(unit.clone()));
            }
        }
        Ok(())
    }

    /// Evaluate an expression outside any rule, for the bulk evaluator's
    /// per-entity relation aggregations. Like [`Engine::evaluate_scalar`], an
    /// indeterminate result is [`EvalError::Indeterminate`].
    pub(crate) fn eval_scalar_expr(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        self.check_expression(expr, period)?;
        match self.drive(|engine| engine.eval_scalar_expr_inner(expr, entity_id, period, None))? {
            Determined(value) => Ok(value),
            Indeterminate(needs) => Err(EvalError::Indeterminate { needs }),
        }
    }

    fn eval_scalar_expr_inner(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<ScalarValue>> {
        self.nested(|engine| engine.scalar_node(expr, entity_id, period, relation_context))
    }

    /// One scalar node. Operands, conversions and checked arithmetic run in
    /// the reference order. Once an operand is indeterminate the node still
    /// evaluates and converts the rest, raising what they raise, but does no
    /// more arithmetic: the result is indeterminate with the needs of every
    /// indeterminate operand (`Determination::zip`). Only
    /// `MissingFacts::Indeterminate` makes an operand indeterminate, so
    /// without it each node computes what it always has.
    fn scalar_node(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<ScalarValue>> {
        match expr {
            ScalarExpr::Literal(value) => Ok(Determined(value.clone())),
            ScalarExpr::Input(name) => Ok(self.read_input(name, entity_id, period)?),
            ScalarExpr::InputOrElse { name, default } => {
                // Under the mode a missing cell is indeterminate, never the
                // default: a default would turn absence into a value.
                match self.read_input(name, entity_id, period) {
                    Err(EvalError::MissingInput { .. }) => Ok(Determined(default.clone())),
                    read => Ok(read?),
                }
            }
            ScalarExpr::Derived(name) => {
                let derived = self.get_derived(name)?;
                let target_entity_id = relation_context
                    .and_then(|context| context.entity_id_for(&derived.entity))
                    .unwrap_or(entity_id);
                let value = self.scalar_rule(name, target_entity_id, period)?;
                // Recorded once the rule returns, so a rule whose failure an
                // `and`/`or` item catches has no edge. Without the mode this
                // changes nothing: a failure is never caught, and a parent
                // records nothing while the rule runs.
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: target_entity_id.to_string(),
                    period: period.clone(),
                });
                Ok(value)
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                let lookup_key = self
                    .eval_scalar_expr_inner(index, entity_id, period, relation_context)?
                    .try_map(|key| {
                        key.as_index().ok_or_else(|| {
                            EvalError::TypeMismatch(format!(
                                "parameter key for `{parameter}` must be an integer"
                            ))
                        })
                    })?;
                Ok(lookup_key.try_map(|key| self.lookup_parameter(parameter, key, period))?)
            }
            ScalarExpr::Add(items) => {
                let mut total = Determined(Decimal::ZERO);
                for item in items {
                    let operand = self.eval_decimal(item, entity_id, period, relation_context)?;
                    total = total
                        .zip(operand)
                        .try_map(|(total, operand)| checked_add(total, operand))?;
                }
                Ok(total.map(ScalarValue::Decimal))
            }
            ScalarExpr::Sub(left, right) => {
                let left = self.eval_decimal(left, entity_id, period, relation_context)?;
                let right = self.eval_decimal(right, entity_id, period, relation_context)?;
                Ok(left
                    .zip(right)
                    .try_map(|(left, right)| checked_sub(left, right))?
                    .map(ScalarValue::Decimal))
            }
            ScalarExpr::Mul(left, right) => {
                let left = self.eval_decimal(left, entity_id, period, relation_context)?;
                let right = self.eval_decimal(right, entity_id, period, relation_context)?;
                Ok(left
                    .zip(right)
                    .try_map(|(left, right)| checked_mul(left, right))?
                    .map(ScalarValue::Decimal))
            }
            ScalarExpr::Div(left, right) => {
                let divisor = self.eval_decimal(right, entity_id, period, relation_context)?;
                if matches!(divisor, Determined(divisor) if divisor.is_zero()) {
                    return Err(EvalError::DivisionByZero.into());
                }
                let dividend = self.eval_decimal(left, entity_id, period, relation_context)?;
                Ok(dividend
                    .zip(divisor)
                    .try_map(|(dividend, divisor)| checked_div(dividend, divisor))?
                    .map(ScalarValue::Decimal))
            }
            ScalarExpr::Max(items) => {
                let mut iter = items.iter();
                let Some(first) = iter.next() else {
                    return Err(EvalError::TypeMismatch(
                        "max() requires at least one operand".to_string(),
                    )
                    .into());
                };
                let mut best = self.eval_decimal(first, entity_id, period, relation_context)?;
                for item in iter {
                    let candidate = self.eval_decimal(item, entity_id, period, relation_context)?;
                    best = best.zip(candidate).map(
                        |(best, candidate)| {
                            if candidate > best { candidate } else { best }
                        },
                    );
                }
                Ok(best.map(ScalarValue::Decimal))
            }
            ScalarExpr::Min(items) => {
                let mut iter = items.iter();
                let Some(first) = iter.next() else {
                    return Err(EvalError::TypeMismatch(
                        "min() requires at least one operand".to_string(),
                    )
                    .into());
                };
                let mut best = self.eval_decimal(first, entity_id, period, relation_context)?;
                for item in iter {
                    let candidate = self.eval_decimal(item, entity_id, period, relation_context)?;
                    best = best.zip(candidate).map(
                        |(best, candidate)| {
                            if candidate < best { candidate } else { best }
                        },
                    );
                }
                Ok(best.map(ScalarValue::Decimal))
            }
            ScalarExpr::Ceil(value) => Ok(self
                .eval_decimal(value, entity_id, period, relation_context)?
                .map(|value| ScalarValue::Decimal(value.ceil()))),
            ScalarExpr::Floor(value) => Ok(self
                .eval_decimal(value, entity_id, period, relation_context)?
                .map(|value| ScalarValue::Decimal(value.floor()))),
            ScalarExpr::PeriodStart => Ok(Determined(ScalarValue::Date(period.start))),
            ScalarExpr::PeriodEnd => Ok(Determined(ScalarValue::Date(period.end))),
            ScalarExpr::DateAddDays { date, days } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .try_map(|base| {
                        base.as_date().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_days expects a date on the left".to_string(),
                            )
                        })
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(days, entity_id, period, relation_context)?
                    .try_map(|offset| {
                        offset.as_index().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_days expects an integer day count on the right"
                                    .to_string(),
                            )
                        })
                    })?;
                Ok(base.zip(offset).try_map(|(base, offset)| {
                    shift_calendar_days(base, offset).map(ScalarValue::Date)
                })?)
            }
            ScalarExpr::DateAddMonths { date, months } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .try_map(|base| {
                        base.as_date().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_months expects a date on the left".to_string(),
                            )
                        })
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(months, entity_id, period, relation_context)?
                    .try_map(|offset| {
                        offset.as_index().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_months expects an integer month count on the right"
                                    .to_string(),
                            )
                        })
                    })?;
                Ok(base.zip(offset).try_map(|(base, offset)| {
                    shift_calendar_months(base, offset).map(ScalarValue::Date)
                })?)
            }
            ScalarExpr::DateAddYears { date, years } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .try_map(|base| {
                        base.as_date().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_years expects a date on the left".to_string(),
                            )
                        })
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(years, entity_id, period, relation_context)?
                    .try_map(|offset| {
                        offset.as_index().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "date_add_years expects an integer year count on the right"
                                    .to_string(),
                            )
                        })
                    })?;
                Ok(base.zip(offset).try_map(|(base, offset)| {
                    shift_calendar_years(base, offset).map(ScalarValue::Date)
                })?)
            }
            ScalarExpr::DaysBetween { from, to } => {
                let a = self
                    .eval_scalar_expr_inner(from, entity_id, period, relation_context)?
                    .try_map(|from| {
                        from.as_date().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "days_between expects a date for `from`".to_string(),
                            )
                        })
                    })?;
                let b = self
                    .eval_scalar_expr_inner(to, entity_id, period, relation_context)?
                    .try_map(|to| {
                        to.as_date().ok_or_else(|| {
                            EvalError::TypeMismatch(
                                "days_between expects a date for `to`".to_string(),
                            )
                        })
                    })?;
                Ok(a.zip(b)
                    .map(|(a, b)| ScalarValue::Integer(b.signed_duration_since(a).num_days())))
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                if self.three_valued() {
                    return Err(unsupported_under_indeterminate("`count` over a relation"));
                }
                let (count, _) = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    None,
                    entity_id,
                    period,
                )?;
                // A count of collected ids fits in i64 on every supported target.
                Ok(Determined(ScalarValue::Integer(count as i64)))
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                if self.three_valued() {
                    return Err(unsupported_under_indeterminate("`sum` over a relation"));
                }
                let (_, total) = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    Some(value),
                    entity_id,
                    period,
                )?;
                Ok(Determined(ScalarValue::Decimal(total)))
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition =
                    self.eval_judgment_expr_inner(condition, entity_id, period, relation_context)?;
                if require_determined(condition, "an `if` whose condition is undetermined")?
                    .is_holds()
                {
                    self.record_skipped_scalar_dependencies(
                        else_expr,
                        entity_id,
                        period,
                        relation_context,
                        TraceSkipReason::BranchNotSelected,
                    );
                    self.eval_scalar_expr_inner(then_expr, entity_id, period, relation_context)
                } else {
                    self.record_skipped_scalar_dependencies(
                        then_expr,
                        entity_id,
                        period,
                        relation_context,
                        TraceSkipReason::BranchNotSelected,
                    );
                    self.eval_scalar_expr_inner(else_expr, entity_id, period, relation_context)
                }
            }
            ScalarExpr::NoMatch { subject, patterns } => {
                // The patterns only label the error; they are never evaluated,
                // and an indeterminate subject never reaches the error.
                let value =
                    self.eval_scalar_expr_inner(subject, entity_id, period, relation_context)?;
                match value {
                    Determined(value) => {
                        Err(
                            no_matching_arm_written(subject, &value, patterns, self.three_valued())
                                .into(),
                        )
                    }
                    Indeterminate(needs) => Ok(Indeterminate(needs)),
                }
            }
            // Cross-period reductions are only defined when a batch is supplied
            // per period (the dense lifetime surface). The sparse single-period
            // interpreter has no period axis to reduce over.
            ScalarExpr::OverPeriods { kind, .. } => {
                Err(EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()).into())
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Determination<JudgmentOutcome>> {
        self.eval_judgment_expr_inner(expr, entity_id, period, None)
    }

    fn eval_judgment_expr_inner(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<JudgmentOutcome>> {
        self.nested(|engine| engine.judgment_node(expr, entity_id, period, relation_context))
    }

    fn judgment_node(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<JudgmentOutcome>> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                let left_value =
                    self.eval_scalar_expr_inner(left, entity_id, period, relation_context)?;
                let right_value =
                    self.eval_scalar_expr_inner(right, entity_id, period, relation_context)?;
                Ok(left_value.zip(right_value).try_map(|(left, right)| {
                    self.compare_scalar_values(&left, *op, &right).map(|holds| {
                        if holds {
                            JudgmentOutcome::Holds
                        } else {
                            JudgmentOutcome::NotHolds
                        }
                    })
                })?)
            }
            JudgmentExpr::Derived(name) => {
                let derived = self.get_derived(name)?;
                let target_entity_id = relation_context
                    .and_then(|context| context.entity_id_for(&derived.entity))
                    .unwrap_or(entity_id);
                let outcome = self.judgment_rule(name, target_entity_id, period)?;
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: target_entity_id.to_string(),
                    period: period.clone(),
                });
                Ok(outcome)
            }
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => {
                if self.three_valued() {
                    return Err(unsupported_under_indeterminate("`relation_member`"));
                }
                let context = relation_context
                    .ok_or_else(|| relation_member_outside_derived_relation(relation))?;
                Ok(Determined(
                    if self.relation_contains(
                        relation,
                        *current_slot,
                        *related_slot,
                        context.current_id,
                        context.related_id,
                        period,
                    )? {
                        JudgmentOutcome::Holds
                    } else {
                        JudgmentOutcome::NotHolds
                    },
                ))
            }
            JudgmentExpr::And(items) => self.connective(
                items,
                JudgmentOutcome::NotHolds,
                entity_id,
                period,
                relation_context,
            ),
            JudgmentExpr::Or(items) => self.connective(
                items,
                JudgmentOutcome::Holds,
                entity_id,
                period,
                relation_context,
            ),
            JudgmentExpr::Not(item) => Ok(self
                .eval_judgment_expr_inner(item, entity_id, period, relation_context)?
                .map(|outcome| match outcome {
                    JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
                    JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
                    JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
                })),
        }
    }

    /// `and` (`decisive` is `NotHolds`) or `or` (`decisive` is `Holds`). The
    /// items run left to right and stop at the first decisive one, whose
    /// skipped siblings the trace records.
    ///
    /// Under `MissingFacts::Indeterminate` they also run past an unknown item
    /// and past one that fails (`docs/three-valued-values.md` 5.4): an item's
    /// evaluation error is caught here, the only place this series catches
    /// one, so a decisive item decides whatever its siblings did, in either
    /// order. With no decisive item the result is unknown with every unknown
    /// item's needs; else the least caught failure (see [`failure_order`]),
    /// raised again from here; else the other outcome. A deferral and a
    /// refusal pass through uncaught. Nothing about catching reaches a
    /// referenced rule's body, so a rule's result does not depend on where
    /// it is referenced from. Without the mode no error is caught, so this
    /// short-circuits and fails exactly as it always has.
    fn connective(
        &mut self,
        items: &'a [JudgmentExpr],
        decisive: JudgmentOutcome,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<JudgmentOutcome>> {
        let mut needs: Option<Needs> = None;
        let mut failure: Option<EvalError> = None;
        let mut saw_undetermined = false;
        for (index, item) in items.iter().enumerate() {
            match self.eval_judgment_expr_inner(item, entity_id, period, relation_context) {
                Ok(Determined(outcome)) if outcome == decisive => {
                    for skipped in &items[index + 1..] {
                        self.record_skipped_judgment_dependencies(
                            skipped,
                            entity_id,
                            period,
                            relation_context,
                            TraceSkipReason::ShortCircuit,
                        );
                    }
                    return Ok(Determined(decisive));
                }
                Ok(Determined(JudgmentOutcome::Undetermined)) => saw_undetermined = true,
                Ok(Determined(_)) => {}
                Ok(Indeterminate(item_needs)) => match &mut needs {
                    Some(needs) => needs.extend(item_needs),
                    None => needs = Some(item_needs),
                },
                Err(Interrupt::Error(error)) if self.three_valued() => {
                    keep_least_failure(&mut failure, error);
                }
                Err(interrupt) => return Err(interrupt),
            }
        }
        if let Some(needs) = needs {
            return Ok(Indeterminate(needs));
        }
        if let Some(error) = failure {
            return Err(error.into());
        }
        Ok(Determined(if saw_undetermined {
            JudgmentOutcome::Undetermined
        } else if decisive == JudgmentOutcome::NotHolds {
            JudgmentOutcome::Holds
        } else {
            JudgmentOutcome::NotHolds
        }))
    }

    /// Count the related entities of `entity_id` that `where_clause` selects,
    /// and sum `value` over them, in order: a `count` passes no `value` and
    /// reads only the count, a `sum` reads only the total.
    ///
    /// When a member defers a rule (see [`crate::depth`]), the fold records
    /// where it stopped before unwinding, and the retry resumes at that member.
    /// The members before it are not evaluated again: they read only cached
    /// rules, and their trace records are already made. So an aggregation
    /// whose every member defers costs one pass, not one pass per member.
    fn fold_related(
        &mut self,
        node: &'a ScalarExpr,
        (relation, current_slot, related_slot): (&str, usize, usize),
        where_clause: Option<&'a JudgmentExpr>,
        value: Option<&'a RelatedValueRef>,
        entity_id: &str,
        period: &Period,
    ) -> Eval<(usize, Decimal)> {
        let key = self.step(
            Site::Fold(std::ptr::from_ref(node) as usize),
            entity_id,
            period,
        );
        let FoldResume {
            related_ids,
            mut next,
            mut count,
            mut total,
        } = match self.folds.remove(&key) {
            Some(Progress::Done(result)) => {
                self.folds.insert(key, Progress::Done(result));
                return Ok(result);
            }
            Some(Progress::Pending(resume)) => resume,
            None => FoldResume {
                related_ids: self.related_entity_ids(
                    relation,
                    current_slot,
                    related_slot,
                    entity_id,
                    period,
                )?,
                next: 0,
                count: 0,
                total: Decimal::ZERO,
            },
        };
        while let Some(related_id) = related_ids.get(next) {
            // Each member counts as a visit, so a fold walked again shows in
            // `count_visits` even when its members read nothing.
            self.visits += 1;
            match self.fold_member(where_clause, value, related_id, period) {
                Ok(None) => {}
                Ok(Some(amount)) => {
                    count += 1;
                    total = checked_add(total, amount)?;
                }
                Err(Interrupt::Defer(deferred)) => {
                    self.folds.insert(
                        key,
                        Progress::Pending(FoldResume {
                            related_ids,
                            next,
                            count,
                            total,
                        }),
                    );
                    return Err(Interrupt::Defer(deferred));
                }
                Err(error) => return Err(error),
            }
            next += 1;
        }
        self.folds.insert(key, Progress::Done((count, total)));
        Ok((count, total))
    }

    /// The key of a loop at `site` for this entity and period, in the rule
    /// evaluation now running (see [`Step`]).
    fn step(&self, site: Site, entity_id: &str, period: &Period) -> Step {
        Step {
            site,
            entity_id: entity_id.to_string(),
            period: period.clone(),
            parent: self.active_evaluations.last().cloned(),
        }
    }

    /// One related entity's part in a `count` or `sum`: `None` when the
    /// `where` clause does not select it, else its value (zero for a count).
    fn fold_member(
        &mut self,
        where_clause: Option<&'a JudgmentExpr>,
        value: Option<&'a RelatedValueRef>,
        related_id: &str,
        period: &Period,
    ) -> Eval<Option<Decimal>> {
        if let Some(predicate) = where_clause {
            let selected = self.eval_judgment_expr(predicate, related_id, period)?;
            if !require_determined(selected, "an undetermined `where` clause")?.is_holds() {
                return Ok(None);
            }
        }
        match value {
            Some(value) => {
                let value = self.eval_related_value(value, related_id, period)?;
                Ok(Some(require_determined(
                    value,
                    "an indeterminate value in a `sum`",
                )?))
            }
            None => Ok(Some(Decimal::ZERO)),
        }
    }

    fn eval_related_value(
        &mut self,
        value: &'a RelatedValueRef,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Determination<Decimal>> {
        let scalar = match value {
            RelatedValueRef::Input(name) => self.read_input(name, entity_id, period)?,
            RelatedValueRef::Derived(name) => {
                let value = self.scalar_rule(name, entity_id, period)?;
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                });
                value
            }
        };
        Ok(scalar.try_map(|scalar| {
            scalar.as_decimal().ok_or_else(|| {
                EvalError::TypeMismatch("related aggregation requires numeric values".to_string())
            })
        })?)
    }

    fn eval_decimal(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Determination<Decimal>> {
        Ok(self
            .eval_scalar_expr_inner(expr, entity_id, period, relation_context)?
            .try_map(|value| {
                value
                    .as_decimal()
                    .ok_or_else(|| EvalError::TypeMismatch("expected numeric scalar".to_string()))
            })?)
    }

    /// Read an input cell. Under `MissingFacts::Indeterminate` a cell no
    /// record covers is indeterminate, with that cell as its need; without
    /// the mode it is `MissingInput`, as [`Engine::lookup_input`] reports it.
    fn read_input(
        &self,
        name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<Determination<ScalarValue>, EvalError> {
        match self.lookup_input(name, entity_id, period) {
            Ok(value) => Ok(Determined(value)),
            Err(EvalError::MissingInput { .. }) if self.three_valued() => {
                Ok(Indeterminate(Needs::one(Need::Input {
                    name: name.to_string(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                })))
            }
            Err(error) => Err(error),
        }
    }

    fn lookup_input(
        &self,
        name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        let mut covering = self
            .input_index
            .get(&(name.to_string(), entity_id.to_string()))
            .into_iter()
            .flat_map(|records| records.iter().copied())
            .filter(|record| record.interval.contains_period(period));
        let selected = covering.next().ok_or_else(|| EvalError::MissingInput {
            name: name.to_string(),
            entity_id: entity_id.to_string(),
            period_start: period.start,
            period_end: period.end,
        })?;

        // Records are sorted by descending start in `new`. Any immediately
        // following covering records with the same start have equal
        // precedence. Reject a conflicting tie rather than recovering dataset
        // order as a hidden tie-breaker for direct Engine callers.
        for tied in covering.take_while(|record| record.interval.start == selected.interval.start) {
            if tied.value != selected.value {
                return Err(EvalError::AmbiguousInput {
                    name: name.to_string(),
                    entity_id: entity_id.to_string(),
                    effective_from: selected.interval.start,
                });
            }
        }

        Ok(selected.value.clone())
    }

    /// Evaluate a non-indexed parameter at a period for a direct query
    /// output. Indexed parameters need a key expression, so they stay
    /// reachable only through derived formulas.
    pub fn evaluate_parameter(
        &mut self,
        name: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        let indexed_by = {
            let parameter = self
                .program
                .parameters
                .get(name)
                .ok_or_else(|| EvalError::UnknownParameter(name.to_string()))?;
            parameter.indexed_by.clone()
        };
        if indexed_by.is_some() {
            return Err(EvalError::TypeMismatch(format!(
                "parameter `{name}` is indexed; query it through a derived rule"
            )));
        }
        self.lookup_parameter(name, 0, period)
    }

    fn lookup_parameter(
        &mut self,
        name: &str,
        key: i64,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        let (value, effective_from, effective_to) = {
            let parameter = self
                .program
                .parameters
                .get(name)
                .ok_or_else(|| EvalError::UnknownParameter(name.to_string()))?;
            let version = parameter
                .versions
                .iter()
                .filter(|version| version.applies_at(period.start))
                .max_by_key(|version| version.effective_from)
                .ok_or_else(|| EvalError::MissingParameterValue {
                    parameter: name.to_string(),
                    key,
                    at: period.start,
                })?;
            let value = version.values.get(&key).cloned().ok_or_else(|| {
                EvalError::MissingParameterValue {
                    parameter: name.to_string(),
                    key,
                    at: period.start,
                }
            })?;
            (value, version.effective_from, version.effective_to)
        };
        self.record_parameter_read(ParameterTraceRead {
            parameter: name.to_string(),
            index: key,
            value: value.clone(),
            effective_from,
            effective_to,
        });
        Ok(value)
    }

    fn related_entity_ids(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Vec<String>> {
        // Relations are closed-world here: an absent tuple is no member. The
        // mode must never read one that way, so until the series gives
        // relations open-world semantics every read is refused.
        if self.three_valued() {
            return Err(unsupported_under_indeterminate(
                "reading a relation's members",
            ));
        }
        self.nested(|engine| {
            engine.related_entity_ids_at_level(
                relation,
                current_slot,
                related_slot,
                entity_id,
                period,
            )
        })
    }

    fn related_entity_ids_at_level(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Vec<String>> {
        // Borrowed from the program, not copied: a `count`/`sum` in the
        // predicate is identified by its address (see [`Site::Fold`]).
        let program: &'a Program = self.program;
        let schema = program
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_string()))?;
        if current_slot >= schema.arity || related_slot >= schema.arity {
            return Err(relation_slots_out_of_range(
                relation,
                schema.arity,
                current_slot,
                related_slot,
            )
            .into());
        }
        let key = self.step(
            Site::Members {
                relation: relation.to_string(),
                current_slot,
                related_slot,
            },
            entity_id,
            period,
        );
        let resume = match self.members.remove(&key) {
            Some(Progress::Done(related_ids)) => {
                self.members
                    .insert(key, Progress::Done(related_ids.clone()));
                return Ok(related_ids);
            }
            Some(Progress::Pending(resume)) => Some(resume),
            None => None,
        };

        let MembersResume {
            candidates,
            mut next,
            mut selected,
        } = match resume {
            Some(resume) => resume,
            None => {
                // Resolving the source's members can defer, and a retry must
                // not read this relation's own tuples again, so they are read
                // once the resolution finishes.
                let candidates = match &schema.derivation {
                    Some(derivation) => self.related_entity_ids(
                        &derivation.source_relation,
                        derivation.current_slot,
                        derivation.related_slot,
                        entity_id,
                        period,
                    )?,
                    None => Vec::new(),
                };
                MembersResume {
                    candidates,
                    next: 0,
                    selected: Vec::new(),
                }
            }
        };

        if let Some(derivation) = &schema.derivation {
            while let Some(related_id) = candidates.get(next) {
                let context = RelationEvalContext {
                    current_id: entity_id,
                    related_id,
                    current_entity: derivation
                        .slot_entities
                        .get(derivation.current_slot)
                        .map(String::as_str),
                    related_entity: derivation
                        .slot_entities
                        .get(derivation.related_slot)
                        .map(String::as_str),
                };
                match self.eval_judgment_expr_inner(
                    &derivation.predicate,
                    related_id,
                    period,
                    Some(context),
                ) {
                    Ok(outcome) => {
                        if require_determined(outcome, "an undetermined relation predicate")?
                            .is_holds()
                        {
                            selected.push(related_id.clone());
                        }
                    }
                    Err(Interrupt::Defer(deferred)) => {
                        self.members.insert(
                            key,
                            Progress::Pending(MembersResume {
                                candidates,
                                next,
                                selected,
                            }),
                        );
                        return Err(Interrupt::Defer(deferred));
                    }
                    Err(error) => return Err(error),
                }
                next += 1;
            }
        }

        // Reading the relation's own tuples makes no trace record and cannot
        // fail, and the members are sorted, so reading them last changes
        // nothing recursion would show.
        let mut related_ids =
            self.direct_related_ids(relation, current_slot, related_slot, entity_id, period);
        related_ids.extend(selected);
        related_ids.sort();
        related_ids.dedup();
        self.members
            .insert(key, Progress::Done(related_ids.clone()));
        Ok(related_ids)
    }

    /// The ids `relation`'s own tuples relate to `entity_id` in `period`.
    /// Each tuple read counts as a visit (see [`crate::depth::count_visits`]),
    /// so work over a relation's tuples is measured like work over rules.
    fn direct_related_ids(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        entity_id: &str,
        period: &Period,
    ) -> Vec<String> {
        let records = self
            .relation_index
            .get(&(relation.to_string(), current_slot, entity_id.to_string()))
            .map_or(&[][..], Vec::as_slice);
        self.visits += records.len();
        records
            .iter()
            .filter(|record| record.interval.contains_period(period))
            .filter_map(|record| record.tuple.get(related_slot).cloned())
            .collect()
    }

    fn relation_contains(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        current_id: &str,
        related_id: &str,
        period: &Period,
    ) -> Eval<bool> {
        Ok(self
            .related_entity_ids(relation, current_slot, related_slot, current_id, period)?
            .iter()
            .any(|candidate| candidate == related_id))
    }

    fn compare_scalar_values(
        &self,
        left: &ScalarValue,
        op: ComparisonOp,
        right: &ScalarValue,
    ) -> Result<bool, EvalError> {
        compare_scalar_values(left, op, right)
    }

    /// Check what a reference to rule `name`, asking for `kind`, can reach
    /// (see [`Engine::check_program`]).
    fn check_rule(&mut self, name: &str, kind: Kind, period: &Period) -> Result<(), EvalError> {
        self.check_program(period, |walk| walk.rule_reference(name, kind))
    }

    /// Check what `expr`, evaluated outside any rule, can reach (see
    /// [`Engine::check_program`]).
    fn check_expression(&mut self, expr: &ScalarExpr, period: &Period) -> Result<(), EvalError> {
        self.check_program(period, |walk| walk.scalar(expr, false))
    }

    /// Under `MissingFacts::Indeterminate`, refuse before anything is
    /// evaluated a program defect that no dataset can change
    /// (`docs/three-valued-values.md`, section 4), with the error evaluation
    /// would raise for it. Today such a defect fails only an evaluation that
    /// reaches it; under the mode an `and`/`or` item catches failures, so
    /// one behind a decisive item would otherwise disappear. A refusal here
    /// is not an evaluation failure, and nothing catches it.
    ///
    /// The checks cover everything `root` can reach by reference, using the
    /// formula each rule has for `period`. What they pass is remembered per
    /// period, so each rule is walked once; a run that finds a defect
    /// remembers nothing, and the next call finds the same one.
    fn check_program(
        &mut self,
        period: &Period,
        root: impl FnOnce(&mut ProgramWalk<'_>),
    ) -> Result<(), EvalError> {
        if !self.three_valued() || !self.program_checks {
            return Ok(());
        }
        let mut walk = ProgramWalk {
            program: self.program,
            period,
            checked: self.checked.get(period),
            seen: HashSet::new(),
            pending: Vec::new(),
            defect: None,
        };
        root(&mut walk);
        while let Some(item) = walk.pending.pop() {
            walk.walk(&item);
        }
        if let Some(defect) = walk.defect {
            return Err(defect);
        }
        let seen = walk.seen;
        self.checked.entry(period.clone()).or_default().extend(seen);
        Ok(())
    }
}

/// One run of the program checks (see [`Engine::check_program`]). It walks
/// what evaluation can execute, and only that: a `no_match`'s patterns only
/// label its error and are never evaluated, so they are not walked. Rules
/// and derived relations are walked from a worklist rather than by recursion
/// across references, so a long chain of rules does not deepen the stack;
/// within one formula the walk recurses, as evaluation does.
struct ProgramWalk<'p> {
    program: &'p Program,
    period: &'p Period,
    /// What earlier runs passed for this period.
    checked: Option<&'p HashSet<Checked>>,
    /// What this run has queued.
    seen: HashSet<Checked>,
    pending: Vec<Checked>,
    /// The least defect found (see [`failure_order`]), so that the one
    /// reported does not depend on the order of the walk.
    defect: Option<EvalError>,
}

impl ProgramWalk<'_> {
    fn defect(&mut self, error: EvalError) {
        keep_least_failure(&mut self.defect, error);
    }

    fn queue(&mut self, item: Checked) {
        if self.checked.is_some_and(|checked| checked.contains(&item)) {
            return;
        }
        if self.seen.insert(item.clone()) {
            self.pending.push(item);
        }
    }

    /// A reference to rule `name` asking for `kind`, which fails as evaluating
    /// it would: for an unknown rule, an undeclared unit, then, when the rule
    /// has a formula for the period, the wrong kind. A rule with no formula
    /// for the period is no defect: it fails if evaluation reaches it, as it
    /// always has.
    fn rule_reference(&mut self, name: &str, kind: Kind) {
        let Some(derived) = self.program.derived.get(name) else {
            return self.defect(EvalError::UnknownDerived(name.to_string()));
        };
        if let Some(unit) = &derived.unit {
            if !self.program.units.contains_key(unit) {
                return self.defect(EvalError::UnknownUnit(unit.clone()));
            }
        }
        let Some(semantics) = derived.semantics_at(self.period) else {
            return;
        };
        match (kind, semantics) {
            (Kind::Scalar, DerivedSemantics::Judgment(_)) => {
                self.defect(EvalError::ExpectedScalar(name.to_string()))
            }
            (Kind::Judgment, DerivedSemantics::Scalar(_)) => {
                self.defect(EvalError::ExpectedJudgment(name.to_string()))
            }
            _ => self.queue(Checked::Rule(name.to_string())),
        }
    }

    /// A read of `relation` from `current_slot` to `related_slot`, which
    /// reaches a derived relation's source and predicate.
    fn relation_read(&mut self, relation: &str, current_slot: usize, related_slot: usize) {
        let Some(schema) = self.program.relations.get(relation) else {
            return self.defect(EvalError::UnknownRelation(relation.to_string()));
        };
        if current_slot >= schema.arity || related_slot >= schema.arity {
            return self.defect(relation_slots_out_of_range(
                relation,
                schema.arity,
                current_slot,
                related_slot,
            ));
        }
        if schema.derivation.is_some() {
            self.queue(Checked::Relation(relation.to_string()));
        }
    }

    fn walk(&mut self, item: &Checked) {
        let program = self.program;
        match item {
            Checked::Rule(name) => {
                match program
                    .derived
                    .get(name)
                    .and_then(|derived| derived.semantics_at(self.period))
                {
                    Some(DerivedSemantics::Scalar(expr)) => self.scalar(expr, false),
                    Some(DerivedSemantics::Judgment(expr)) => self.judgment(expr, false),
                    // Queued only for a rule with a formula for the period.
                    None => {}
                }
            }
            Checked::Relation(name) => {
                if let Some(derivation) = program
                    .relations
                    .get(name)
                    .and_then(|schema| schema.derivation.as_ref())
                {
                    self.relation_read(
                        &derivation.source_relation,
                        derivation.current_slot,
                        derivation.related_slot,
                    );
                    self.judgment(&derivation.predicate, true);
                }
            }
        }
    }

    /// `in_predicate`: whether `expr` is part of a derived relation's own
    /// predicate, the only place evaluation gives a `relation_member` the
    /// relation context it needs. A rule the predicate references, and a
    /// `count`/`sum` `where` clause or summed value, are evaluated without it.
    fn scalar(&mut self, expr: &ScalarExpr, in_predicate: bool) {
        match expr {
            ScalarExpr::Literal(_)
            | ScalarExpr::Input(_)
            | ScalarExpr::InputOrElse { .. }
            | ScalarExpr::PeriodStart
            | ScalarExpr::PeriodEnd => {}
            ScalarExpr::Derived(name) => self.rule_reference(name, Kind::Scalar),
            ScalarExpr::ParameterLookup { parameter, index } => {
                self.scalar(index, in_predicate);
                if !self.program.parameters.contains_key(parameter) {
                    self.defect(EvalError::UnknownParameter(parameter.clone()));
                }
            }
            ScalarExpr::Add(items) => {
                for item in items {
                    self.scalar(item, in_predicate);
                }
            }
            ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
                if items.is_empty() {
                    let call = if matches!(expr, ScalarExpr::Max(_)) {
                        "max"
                    } else {
                        "min"
                    };
                    self.defect(EvalError::TypeMismatch(format!(
                        "{call}() requires at least one operand"
                    )));
                }
                for item in items {
                    self.scalar(item, in_predicate);
                }
            }
            ScalarExpr::Sub(left, right)
            | ScalarExpr::Mul(left, right)
            | ScalarExpr::Div(left, right) => {
                self.scalar(left, in_predicate);
                self.scalar(right, in_predicate);
            }
            ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => self.scalar(value, in_predicate),
            ScalarExpr::DateAddDays { date, days: offset }
            | ScalarExpr::DateAddMonths {
                date,
                months: offset,
            }
            | ScalarExpr::DateAddYears {
                date,
                years: offset,
            } => {
                self.scalar(date, in_predicate);
                self.scalar(offset, in_predicate);
            }
            ScalarExpr::DaysBetween { from, to } => {
                self.scalar(from, in_predicate);
                self.scalar(to, in_predicate);
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                self.relation_read(relation, *current_slot, *related_slot);
                if let Some(where_clause) = where_clause {
                    self.judgment(where_clause, false);
                }
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                self.relation_read(relation, *current_slot, *related_slot);
                if let Some(where_clause) = where_clause {
                    self.judgment(where_clause, false);
                }
                if let RelatedValueRef::Derived(name) = value {
                    self.rule_reference(name, Kind::Scalar);
                }
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, in_predicate);
                self.scalar(then_expr, in_predicate);
                self.scalar(else_expr, in_predicate);
            }
            ScalarExpr::NoMatch { subject, .. } => self.scalar(subject, in_predicate),
            ScalarExpr::OverPeriods { kind, .. } => {
                self.defect(EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()))
            }
        }
    }

    fn judgment(&mut self, expr: &JudgmentExpr, in_predicate: bool) {
        match expr {
            JudgmentExpr::Comparison { left, right, .. } => {
                self.scalar(left, in_predicate);
                self.scalar(right, in_predicate);
            }
            JudgmentExpr::Derived(name) => self.rule_reference(name, Kind::Judgment),
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => {
                if in_predicate {
                    self.relation_read(relation, *current_slot, *related_slot);
                } else {
                    self.defect(relation_member_outside_derived_relation(relation));
                }
            }
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, in_predicate);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, in_predicate),
        }
    }
}

/// Compare two scalars under the reference semantics: booleans and text
/// support only `==`/`!=`, dates compare chronologically, and anything else
/// compares numerically or fails with a type error. The columnar evaluators
/// call this for every row whose operands are not a vectorised shape, so a
/// comparison means the same thing in every mode.
pub(crate) fn compare_scalar_values(
    left: &ScalarValue,
    op: ComparisonOp,
    right: &ScalarValue,
) -> Result<bool, EvalError> {
    match (left, right) {
        (ScalarValue::Bool(left), ScalarValue::Bool(right)) => match op {
            ComparisonOp::Eq => Ok(left == right),
            ComparisonOp::Ne => Ok(left != right),
            _ => Err(EvalError::TypeMismatch(
                "boolean comparisons only support == and !=".to_string(),
            )),
        },
        (ScalarValue::Text(left), ScalarValue::Text(right)) => match op {
            ComparisonOp::Eq => Ok(left == right),
            ComparisonOp::Ne => Ok(left != right),
            _ => Err(EvalError::TypeMismatch(
                "text comparisons only support == and !=".to_string(),
            )),
        },
        (ScalarValue::Date(left), ScalarValue::Date(right)) => Ok(match op {
            ComparisonOp::Lt => left < right,
            ComparisonOp::Lte => left <= right,
            ComparisonOp::Gt => left > right,
            ComparisonOp::Gte => left >= right,
            ComparisonOp::Eq => left == right,
            ComparisonOp::Ne => left != right,
        }),
        _ => {
            let left = left.as_decimal().ok_or_else(|| {
                EvalError::TypeMismatch("left side of comparison is not numeric".to_string())
            })?;
            let right = right.as_decimal().ok_or_else(|| {
                EvalError::TypeMismatch("right side of comparison is not numeric".to_string())
            })?;
            Ok(match op {
                ComparisonOp::Lt => left < right,
                ComparisonOp::Lte => left <= right,
                ComparisonOp::Gt => left > right,
                ComparisonOp::Gte => left >= right,
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
            })
        }
    }
}

fn collect_scalar_trace_references(
    expr: &ScalarExpr,
    derived: &mut Vec<String>,
    parameters: &mut Vec<String>,
) {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
        ScalarExpr::Derived(name) => derived.push(name.clone()),
        ScalarExpr::ParameterLookup { parameter, index } => {
            parameters.push(parameter.clone());
            collect_scalar_trace_references(index, derived, parameters);
        }
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_trace_references(item, derived, parameters);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right) => {
            collect_scalar_trace_references(left, derived, parameters);
            collect_scalar_trace_references(right, derived, parameters);
        }
        ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => {
            collect_scalar_trace_references(value, derived, parameters);
        }
        ScalarExpr::DateAddDays { date, days } => {
            collect_scalar_trace_references(date, derived, parameters);
            collect_scalar_trace_references(days, derived, parameters);
        }
        ScalarExpr::DateAddMonths { date, months } => {
            collect_scalar_trace_references(date, derived, parameters);
            collect_scalar_trace_references(months, derived, parameters);
        }
        ScalarExpr::DateAddYears { date, years } => {
            collect_scalar_trace_references(date, derived, parameters);
            collect_scalar_trace_references(years, derived, parameters);
        }
        ScalarExpr::DaysBetween { from, to } => {
            collect_scalar_trace_references(from, derived, parameters);
            collect_scalar_trace_references(to, derived, parameters);
        }
        ScalarExpr::CountRelated { where_clause, .. } => {
            if let Some(predicate) = where_clause {
                collect_judgment_trace_references(predicate, derived, parameters);
            }
        }
        ScalarExpr::SumRelated {
            value,
            where_clause,
            ..
        } => {
            if let RelatedValueRef::Derived(name) = value {
                derived.push(name.clone());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_trace_references(predicate, derived, parameters);
            }
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_trace_references(condition, derived, parameters);
            collect_scalar_trace_references(then_expr, derived, parameters);
            collect_scalar_trace_references(else_expr, derived, parameters);
        }
        ScalarExpr::NoMatch { subject, patterns } => {
            collect_scalar_trace_references(subject, derived, parameters);
            for pattern in patterns {
                collect_scalar_trace_references(pattern, derived, parameters);
            }
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_trace_references(value, derived, parameters);
            if let Some(n) = n {
                collect_scalar_trace_references(n, derived, parameters);
            }
        }
    }
}

fn collect_judgment_trace_references(
    expr: &JudgmentExpr,
    derived: &mut Vec<String>,
    parameters: &mut Vec<String>,
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_trace_references(left, derived, parameters);
            collect_scalar_trace_references(right, derived, parameters);
        }
        JudgmentExpr::Derived(name) => derived.push(name.clone()),
        JudgmentExpr::RelationMember { .. } => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_trace_references(item, derived, parameters);
            }
        }
        JudgmentExpr::Not(item) => {
            collect_judgment_trace_references(item, derived, parameters);
        }
    }
}

/// Apply a derived rule's opt-in currency rounding to a just-computed scalar
/// value. Rounding is defined only for decimal (currency) outputs; a rule with
/// no `rounding` declared, or a non-decimal value, passes through unchanged.
/// This is the sparse/explain counterpart of the columnar rounding the bulk and
/// dense paths apply, and both call the same [`crate::model::Rounding::apply`].
pub fn apply_output_rounding(derived: &Derived, value: ScalarValue) -> ScalarValue {
    match (derived.rounding, value) {
        (Some(rounding), ScalarValue::Decimal(amount)) => {
            ScalarValue::Decimal(rounding.apply(amount))
        }
        (_, value) => value,
    }
}

pub fn expect_decimal(value: ScalarValue) -> Result<Decimal, EvalError> {
    value
        .as_decimal()
        .ok_or_else(|| EvalError::TypeMismatch("expected decimal-compatible scalar".to_string()))
}

pub fn expect_integer(value: ScalarValue) -> Result<i64, EvalError> {
    match value {
        ScalarValue::Integer(value) => Ok(value),
        _ => Err(EvalError::TypeMismatch(
            "expected integer scalar".to_string(),
        )),
    }
}

pub fn expect_dtype(derived: &Derived, expected: DType) -> Result<(), EvalError> {
    if derived.dtype == expected {
        Ok(())
    } else {
        Err(EvalError::TypeMismatch(format!(
            "derived `{}` has dtype {:?}, expected {:?}",
            derived.name, derived.dtype, expected
        )))
    }
}

/// Cases of `MissingFacts::Indeterminate` that only the engine's internals
/// reach; `tests/three_valued.rs` has the rest.
#[cfg(test)]
mod three_valued_tests {
    use super::*;
    use crate::depth::with_suspend_depth;
    use crate::model::{DerivedVersion, Interval};

    fn period() -> Period {
        Period::month(2026, 1)
    }

    fn rule(name: &str, dtype: DType, semantics: DerivedSemantics) -> Derived {
        Derived {
            id: None,
            name: name.to_string(),
            entity: "Case".to_string(),
            dtype,
            unit: None,
            rounding: None,
            source: None,
            source_url: None,
            corpus_citation_path: None,
            semantics,
            versions: Vec::<DerivedVersion>::new(),
        }
    }

    fn program(rules: Vec<Derived>) -> Program {
        let mut program = Program::default();
        for rule in rules {
            program.add_derived(rule).expect("distinct rule names");
        }
        program
    }

    fn int(value: i64) -> ScalarExpr {
        ScalarExpr::Literal(ScalarValue::Integer(value))
    }

    fn comparison(left: ScalarExpr, op: ComparisonOp, right: ScalarExpr) -> JudgmentExpr {
        JudgmentExpr::Comparison { left, op, right }
    }

    fn three_valued<'a>(program: &'a Program, data: &'a DataSet) -> Engine<'a> {
        Engine::with_options(
            program,
            data,
            EvaluationOptions {
                missing_facts: MissingFacts::Indeterminate,
            },
        )
        .expect("the options are accepted")
    }

    /// The failure the drive records for a rule is keyed by the kind of value
    /// asked for as well as by rule, entity and period. The program checks
    /// refuse a wrong-kind reference, so only an engine that skips them can
    /// show the key matters: the judgment reference to the scalar rule `s`
    /// fails and is recorded, by the rule itself and, at threshold 1, where it
    /// defers, by the driver too, and the scalar reference to `s` later in the
    /// same drive still evaluates it.
    #[test]
    fn a_failed_wrong_kind_reference_does_not_fail_a_right_kind_one() {
        let program = program(vec![
            rule("s", DType::Integer, DerivedSemantics::Scalar(int(5))),
            rule(
                "root",
                DType::Judgment,
                DerivedSemantics::Judgment(JudgmentExpr::Or(vec![
                    JudgmentExpr::And(vec![
                        JudgmentExpr::Derived("s".to_string()),
                        comparison(int(0), ComparisonOp::Eq, int(1)),
                    ]),
                    comparison(
                        ScalarExpr::Derived("s".to_string()),
                        ComparisonOp::Gt,
                        int(0),
                    ),
                ])),
            ),
        ]);
        let data = DataSet::default();

        let refused = three_valued(&program, &data).judgment_determination("root", "c1", &period());
        assert!(
            matches!(&refused, Err(EvalError::ExpectedJudgment(name)) if name == "s"),
            "the program checks refuse the wrong-kind reference: {refused:?}"
        );

        for threshold in [usize::MAX, 1, 2] {
            with_suspend_depth(threshold, || {
                let mut engine = three_valued(&program, &data);
                engine.program_checks = false;
                let result = engine.judgment_determination("root", "c1", &period());
                assert!(
                    matches!(result, Ok(Determined(JudgmentOutcome::Holds))),
                    "threshold {threshold}: {result:?}"
                );
            });
        }

        // The record is what made the difference: drive the same evaluation
        // by hand and look at it before the drive clears it.
        for threshold in [usize::MAX, 1] {
            with_suspend_depth(threshold, || {
                let mut engine = three_valued(&program, &data);
                engine.program_checks = false;
                let result =
                    engine.run_tasks(|engine| engine.judgment_rule("root", "c1", &period()));
                assert!(matches!(result, Ok(Determined(JudgmentOutcome::Holds))));
                let key = CacheKey {
                    derived: "s".to_string(),
                    entity_id: "c1".to_string(),
                    period: period(),
                };
                assert!(
                    matches!(
                        engine.failed.get(&Deferred::Judgment(key.clone())),
                        Some(Interrupt::Error(EvalError::ExpectedJudgment(_)))
                    ),
                    "threshold {threshold}: the wrong-kind reference failed: {:?}",
                    engine.failed
                );
                assert!(!engine.failed.contains_key(&Deferred::Scalar(key)));
                assert_eq!(
                    engine.cached_scalar("s", "c1", &period()),
                    Some(ScalarValue::Integer(5))
                );
            });
        }
    }

    /// Two `match` failures with the same message, one raised in the
    /// expression and one inside a rule whose name makes the messages
    /// coincide, are chosen between by their fields, so the choice is the
    /// same in either order before the error leaves any rule.
    /// `tests/three_valued.rs` checks the same pair after it leaves one.
    #[test]
    fn equal_messages_are_chosen_between_by_their_fields_before_leaving_a_rule() {
        let inner = "` covers `a";
        let subject = "a` covers `x";
        let program = program(vec![rule(
            inner,
            DType::Integer,
            DerivedSemantics::Scalar(ScalarExpr::NoMatch {
                subject: Box::new(ScalarExpr::Input("x".to_string())),
                patterns: vec![int(2)],
            }),
        )]);
        let mut data = DataSet::default();
        for slot in ["x", subject] {
            data.add_input(
                slot,
                "Case",
                "c1",
                Interval::covering(&period()),
                ScalarValue::Integer(1),
            );
        }
        let inline = comparison(
            ScalarExpr::NoMatch {
                subject: Box::new(ScalarExpr::Input(subject.to_string())),
                patterns: vec![int(2)],
            },
            ComparisonOp::Gt,
            int(0),
        );
        let referenced = comparison(
            ScalarExpr::Derived(inner.to_string()),
            ComparisonOp::Gt,
            int(0),
        );
        let expressions = [
            JudgmentExpr::And(vec![inline.clone(), referenced.clone()]),
            JudgmentExpr::And(vec![referenced, inline]),
        ]
        .map(|condition| ScalarExpr::If {
            condition: Box::new(condition),
            then_expr: Box::new(int(1)),
            else_expr: Box::new(int(0)),
        });
        let errors: Vec<EvalError> = expressions
            .iter()
            .map(|expr| {
                three_valued(&program, &data)
                    .eval_scalar_expr(expr, "c1", &period())
                    .expect_err("both items fail and neither decides")
            })
            .collect();
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages[0],
            "no `match` arm in `` covers `a` covers `x` = 1 (arms: 2); add an arm for it or a final `_ =>` arm"
        );
        assert_eq!(format!("{:?}", errors[0]), format!("{:?}", errors[1]));
        assert!(
            matches!(&errors[0], EvalError::NoMatchingArm { rule, subject: chosen, .. }
                if rule.is_empty() && chosen == subject),
            "the error raised in the expression is the least: {:?}",
            errors[0]
        );
    }

    /// What the program checks pass is remembered per period; a run that
    /// finds a defect remembers nothing.
    #[test]
    fn program_checks_remember_only_what_passed() {
        let program = program(vec![
            rule("a", DType::Integer, DerivedSemantics::Scalar(int(1))),
            rule(
                "b",
                DType::Decimal,
                DerivedSemantics::Scalar(ScalarExpr::Add(vec![
                    ScalarExpr::Derived("a".to_string()),
                    int(1),
                ])),
            ),
            rule(
                "bad",
                DType::Decimal,
                DerivedSemantics::Scalar(ScalarExpr::Add(vec![
                    ScalarExpr::Derived("b".to_string()),
                    ScalarExpr::Derived("missing_rule".to_string()),
                ])),
            ),
        ]);
        let data = DataSet::default();
        let mut engine = three_valued(&program, &data);
        let refused = engine.scalar_determination("bad", "c1", &period());
        assert!(matches!(refused, Err(EvalError::UnknownDerived(_))));
        assert!(engine.checked.is_empty());
        assert!(matches!(
            engine.scalar_determination("b", "c1", &period()),
            Ok(Determined(_))
        ));
        let passed = &engine.checked[&period()];
        assert_eq!(passed.len(), 2);
        assert!(passed.contains(&Checked::Rule("a".to_string())));
        assert!(passed.contains(&Checked::Rule("b".to_string())));
    }
}
