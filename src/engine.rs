// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, Extreme, JudgmentExpr,
    JudgmentOutcome, Period, Program, RelatedValueRef, ScalarExpr, ScalarValue, Term,
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
    #[error(
        "missing related entity for `{relation}` from `{entity_id}` over {period_start}..{period_end}"
    )]
    MissingRelation {
        relation: String,
        entity_id: String,
        period_start: chrono::NaiveDate,
        period_end: chrono::NaiveDate,
    },
    #[error(
        "ambiguous relation `{relation}` from `{entity_id}` over {period_start}..{period_end}: {count} distinct targets"
    )]
    AmbiguousRelation {
        relation: String,
        entity_id: String,
        period_start: chrono::NaiveDate,
        period_end: chrono::NaiveDate,
        count: usize,
    },
    #[error("missing input `{name}` for entity `{entity_id}` over {period_start}..{period_end}")]
    MissingInput {
        name: String,
        entity_id: String,
        period_start: chrono::NaiveDate,
        period_end: chrono::NaiveDate,
    },
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
    /// A `min`/`max` over related entities that selects none. The law gives
    /// no value for the least of nothing, so the engine invents none; guard
    /// the call with a count of the same members.
    #[error(
        "{function} over relation `{relation}` has no related entity for entity `{entity_id}` over {period_start}..{period_end}, so it has no value; guard it with a count of the same members (for example `if len({relation}) > 0 then ... else ...`)"
    )]
    EmptyRelatedAggregate {
        function: &'static str,
        relation: String,
        entity_id: String,
        period_start: chrono::NaiveDate,
        period_end: chrono::NaiveDate,
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
    EvalError::NoMatchingArm {
        rule: String::new(),
        subject: describe_match_operand(subject),
        value: describe_scalar_value(value),
        patterns: patterns
            .iter()
            .map(describe_match_operand)
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

/// The error for a `min`/`max` over related entities whose member value it
/// cannot compare. Every evaluator words it the same way.
pub(crate) fn extreme_value_type_error(function: &str, relation: &str, found: &str) -> EvalError {
    EvalError::TypeMismatch(format!(
        "{function} over relation `{relation}` compares numbers or dates, but a related value is {found}"
    ))
}

/// Whether `candidate` replaces an equal `best` as a `min`/`max` result.
/// Equal decimals can differ in scale (`2.5`, `2.50`) or in the sign of zero,
/// so the order the members are walked in would otherwise pick the result's
/// representation. The larger scale wins, then the non-negative sign, so
/// every evaluator and every member order returns the same value.
pub(crate) fn decimal_tie_prefers(candidate: &Decimal, best: &Decimal) -> bool {
    (candidate.scale(), !candidate.is_sign_negative()) > (best.scale(), !best.is_sign_negative())
}

/// The id `term` names in `relation_context`, or the refusal of a read
/// through it (`term.name`) when no relation context binds it.
fn term_id<'c>(
    term: Term,
    name: &str,
    relation_context: Option<RelationEvalContext<'c>>,
) -> Result<&'c str, EvalError> {
    relation_context
        .and_then(|context| context.id_of(term))
        .ok_or_else(|| relation_role_outside_derived_relation(term, name))
}

/// The error for a qualified read (`anchor.name`, `candidate.name`)
/// evaluated without a relation context. Compilation refuses such a read
/// (`relation_role_outside_scope`); this guards hand-built programs, which
/// [`Engine::new`] does not check.
pub(crate) fn relation_role_outside_derived_relation(term: Term, name: &str) -> EvalError {
    EvalError::TypeMismatch(format!(
        "`{}.{name}` reads a term of a derived relation's predicate, so it can only be evaluated inside one, outside any aggregation's `where` clause or aggregated value",
        term.as_str()
    ))
}

/// Guard role-only judgments in hand-built programs, whose constructors do
/// not run the compiler's `relation_role_outside_scope` check.
pub(crate) fn relation_construct_outside_derived_relation(construct: &str) -> EvalError {
    EvalError::TypeMismatch(format!(
        "relation_role_outside_scope: {construct} can only be evaluated inside a derived relation's predicate, outside any aggregation's `where` clause or aggregated value"
    ))
}

pub(crate) fn describe_match_operand(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(value) => describe_scalar_value(value),
        ScalarExpr::Input(name) | ScalarExpr::Derived(name) => name.clone(),
        ScalarExpr::InputOrElse { name, .. } => name.clone(),
        ScalarExpr::RoleInput { term, name } | ScalarExpr::RoleDerived { term, name } => {
            format!("{}.{name}", term.as_str())
        }
        _ => "the match subject".to_string(),
    }
}

pub(crate) fn describe_scalar_value(value: &ScalarValue) -> String {
    match value {
        ScalarValue::Bool(value) => value.to_string(),
        ScalarValue::Integer(value) => value.to_string(),
        ScalarValue::Decimal(value) => value.to_string(),
        ScalarValue::Text(value) => format!("{value:?}"),
        ScalarValue::Date(value) => value.to_string(),
    }
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
/// recursing into it (see [`crate::depth`]).
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
/// an evaluation error, or a rule deferred to the driver, which evaluates it
/// and retries. Only the drivers see `Defer`; the public entry points return
/// `EvalError`.
#[derive(Debug)]
enum Interrupt {
    Error(EvalError),
    Defer(Deferred),
}

impl Interrupt {
    fn within_rule(self, rule: &str) -> Self {
        match self {
            Self::Error(error) => Self::Error(error.within_rule(rule)),
            deferred => deferred,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TraceSkipReason {
    ShortCircuit,
    BranchNotSelected,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectedYearTraceRead {
    pub(crate) input: String,
    pub(crate) entity_id: String,
    pub(crate) selected_period: Period,
    pub(crate) value: ScalarValue,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NodeExecutionTrace {
    pub(crate) dependencies: Vec<CacheKey>,
    pub(crate) not_evaluated_dependencies: Vec<SkippedTraceDependency>,
    pub(crate) parameter_reads: Vec<ParameterTraceRead>,
    pub(crate) selected_year_reads: Vec<SelectedYearTraceRead>,
}

/// What a [`NodeExecutionTrace`] already holds, so a record is deduplicated
/// in constant time rather than by scanning a list that grows with every
/// link of a relation chain. The lists keep first-occurrence order.
#[derive(Debug, Default)]
struct NodeTraceIndex {
    dependencies: HashSet<CacheKey>,
    not_evaluated_dependencies: HashSet<SkippedTraceDependency>,
    /// Positions in `parameter_reads` by parameter and key. A read's value
    /// is compared by equality only, so reads sharing a parameter and key
    /// are compared in full.
    parameter_reads: HashMap<(String, i64), Vec<usize>>,
}

/// The members a derived relation adds for one current entity and period:
/// its source relation's related ids that satisfy its predicate. They do not
/// depend on the slots a caller reads the relation at, so every use of the
/// relation for that entity and period shares them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DerivedMembersKey {
    relation: String,
    entity_id: String,
    period: Period,
}

/// A trace record that evaluating relation predicates makes on the rule
/// whose evaluation asked for the relation.
#[derive(Clone, Debug)]
enum TraceEffect {
    Evaluated(CacheKey),
    Skipped(SkippedTraceDependency),
    ParameterRead(ParameterTraceRead),
    SelectedYearRead(SelectedYearTraceRead),
    /// Every record resolving these derived members made, in its place.
    Members(DerivedMembersKey),
}

#[derive(Debug)]
struct DerivedMembers {
    /// Sorted and deduplicated.
    result: Result<Rc<[String]>, EvalError>,
    /// The records resolving these members made, in order, for replay on
    /// each rule evaluation that asks for them.
    effects: Vec<TraceEffect>,
    /// The most recent rule evaluation these effects were recorded on.
    /// Keeping one caller bounds replay bookkeeping independently of the
    /// number of callers. Older callers already deduplicate the records in
    /// their trace index if they ask for these effects again.
    recorded_on: Vec<CacheKey>,
}

/// A derived relation's predicate part-way through its candidates, stopped
/// because a candidate needed a relation that must be resolved first.
#[derive(Debug)]
struct PendingMembers {
    candidates: Rc<[String]>,
    next: usize,
    kept: Vec<String>,
    effects: Vec<TraceEffect>,
}

/// Progress through a path's candidate generation. A completed lookup advances
/// `next`; a suspended lookup retains that id and every completed step.
#[derive(Debug)]
struct PathResume {
    step: usize,
    frontier: Vec<String>,
    next: usize,
    reached: Vec<String>,
    effects: Vec<TraceEffect>,
}

/// Counts increment only after a lookup or a complete candidate generation.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct PathCoverage {
    resumed: usize,
    empty: usize,
    nonempty: usize,
    long: usize,
    derived_positions: [usize; 4],
}

#[cfg(test)]
mod actual_path_work {
    use std::cell::Cell;
    type Counts = (usize, usize, usize, usize, [usize; 4]);
    thread_local! { static COUNTS: Cell<Counts> = const { Cell::new((0, 0, 0, 0, [0; 4])) }; }

    pub(super) fn lookup(resumed: bool, derived_position: Option<usize>) {
        COUNTS.with(|counts| {
            let mut value = counts.get();
            value.0 += usize::from(resumed);
            if let Some(position) = derived_position {
                value.4[position.min(3)] += 1;
            }
            counts.set(value);
        });
    }

    pub(super) fn completed(frontier: &[String], length: usize) {
        assert!(
            frontier.windows(2).all(|pair| pair[0] < pair[1]),
            "path candidates must be distinct and sorted: {frontier:?}"
        );
        let empty = frontier.is_empty();
        COUNTS.with(|counts| {
            let mut value = counts.get();
            value.1 += usize::from(empty);
            value.2 += usize::from(!empty);
            value.3 += usize::from(length >= 3);
            counts.set(value);
        });
    }

    pub(super) fn take() -> Option<Counts> {
        Some(COUNTS.with(|counts| counts.replace((0, 0, 0, 0, [0; 4]))))
    }
}

#[cfg(test)]
#[path = "../tests/support/relation_paths.rs"]
mod relation_path_properties;

/// How many derived-relation resolutions may nest on the native stack. A
/// relation's predicate can read another derived relation, whose predicate
/// reads the next, so nesting natively would grow the stack with the length
/// of such a chain. Past this depth a resolution stops where it needs the
/// next relation, the driver below it resolves that relation, and the
/// stopped resolution then runs again; see [`Engine::resolve_members`].
/// Debug frames are much larger: a count-related predicate chain needed
/// 2048 KiB at depth 32, but 576 KiB at depth 8 (aarch64-apple-darwin,
/// rustc 1.94.1, 16 KiB resolution), so debug builds stop earlier.
#[cfg(debug_assertions)]
const RELATION_NESTING_LIMIT: usize = 8;
#[cfg(not(debug_assertions))]
const RELATION_NESTING_LIMIT: usize = 32;

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

/// The binding environment of a derived relation's predicate: the id and
/// kind of each of its terms. `anchor` is the id the relation is read from
/// (its current slot) and `candidate` the id being tested (its related
/// slot); each kind is the derivation's `slot_entities` at that slot.
///
/// Unqualified reads route through [`Self::entity_id_for`], exactly as
/// before terms existed. A qualified read `t.name` reads [`Self::id_of`] its
/// term. Variables bound by `exists` (docs/relation-roles.md, section 3.6)
/// extend this environment with a borrowed chain of frames; unqualified
/// routing never reads them.
#[derive(Clone, Copy)]
struct VariableFrame<'a> {
    id: &'a str,
    parent: Option<&'a VariableFrame<'a>>,
}

#[derive(Clone, Copy)]
struct RelationEvalContext<'a> {
    current_id: &'a str,
    related_id: &'a str,
    current_entity: Option<&'a String>,
    related_entity: Option<&'a String>,
    variables: Option<&'a VariableFrame<'a>>,
}

impl<'a> RelationEvalContext<'a> {
    /// The id an unqualified rule read of `entity` routes to: the current id
    /// when the current slot has that kind, else the related id when the
    /// related slot has it, else none (the caller falls back to the id the
    /// predicate is evaluated on, the candidate).
    fn entity_id_for(self, entity: &str) -> Option<&'a str> {
        if self.current_entity.is_some_and(|kind| kind == entity) {
            return Some(self.current_id);
        }
        if self.related_entity.is_some_and(|kind| kind == entity) {
            return Some(self.related_id);
        }
        None
    }

    /// The id `term` names.
    fn id_of(self, term: Term) -> Option<&'a str> {
        match term {
            Term::Anchor => Some(self.current_id),
            Term::Candidate => Some(self.related_id),
            Term::Var(mut index) => {
                let mut frame = self.variables?;
                while index > 0 {
                    frame = frame.parent?;
                    index -= 1;
                }
                Some(frame.id)
            }
            #[cfg(test)]
            Term::Id(id) => Some(id),
        }
    }
}

pub struct Engine<'a> {
    program: &'a Program,
    input_index: HashMap<(String, String), Vec<&'a crate::model::InputRecord>>,
    relation_index: HashMap<(String, usize, String), Vec<&'a crate::model::RelationRecord>>,
    relation_records: HashMap<String, Vec<&'a crate::model::RelationRecord>>,
    scalar_cache: HashMap<CacheKey, ScalarValue>,
    /// Pre-rounding value of a currency rule whose declared rounding changed it,
    /// keyed like `scalar_cache`. Only populated when rounding actually moved
    /// the value; the trace uses it to show the rounding step for audit.
    pre_rounding_cache: HashMap<CacheKey, ScalarValue>,
    judgment_cache: HashMap<CacheKey, JudgmentOutcome>,
    execution_trace: HashMap<CacheKey, NodeExecutionTrace>,
    trace_index: HashMap<CacheKey, NodeTraceIndex>,
    active_evaluations: Vec<CacheKey>,
    /// Whether to record the per-node trace. The bulk evaluator borrows this
    /// interpreter for per-entity relation work and never reads a trace.
    tracing: bool,
    derived_members: HashMap<DerivedMembersKey, DerivedMembers>,
    pending_members: HashMap<DerivedMembersKey, PendingMembers>,
    pending_paths: HashMap<DerivedMembersKey, PathResume>,
    #[cfg(test)]
    path_coverage: PathCoverage,
    /// Validated source prefixes that a predicate interruption has not yet
    /// finished. Retaining them prevents a retry from walking the prefix again.
    member_chains: HashMap<DerivedMembersKey, Vec<&'a crate::model::RelationSchema>>,
    #[cfg(test)]
    source_walks: usize,
    #[cfg(test)]
    relation_suspensions: usize,
    /// Effects of the relation predicates under evaluation, innermost last,
    /// each with the number of active evaluations when it began: only records
    /// made at that depth belong to the relation rather than to a rule its
    /// predicate evaluates.
    member_captures: Vec<(DerivedMembersKey, usize, Vec<TraceEffect>)>,
    /// Derived-relation resolutions running on the native stack, and how
    /// many may ([`RELATION_NESTING_LIMIT`] outside tests).
    relation_depth: usize,
    relation_nesting_limit: usize,
    /// Set when a resolution stopped because it needed these members and
    /// resolving them here would nest past [`RELATION_NESTING_LIMIT`]. The
    /// error that carries the stop out to the resolving driver is a
    /// placeholder; the driver reads this instead.
    suspended_on: Option<DerivedMembersKey>,
    /// Expression levels on the stack since the driver's current task began.
    depth: usize,
    /// The level at which a reference to a rule not yet evaluated is deferred
    /// to the driver (see [`crate::depth`]).
    suspend_depth: usize,
    /// How far each aggregation over related entities has got in the
    /// current drive (see [`Step`]).
    folds: HashMap<Step, Progress<FoldResume, Fold>>,
    exists: HashMap<ExistsKey, Progress<ExistsResume, JudgmentOutcome>>,
    #[cfg(test)]
    exists_coverage: ExistsCoverage,
    /// How far each resolution of a relation's members has got in the current
    /// drive (see [`Step`]).
    #[cfg(test)]
    members: HashMap<Step, Progress<MembersResume, Vec<String>>>,
    #[cfg(test)]
    recursive_relation_oracle: bool,
    /// Expression levels evaluated, members an aggregation walked and
    /// relation tuples read since the driver last reported them (see
    /// [`crate::depth::count_visits`]).
    visits: usize,
    #[cfg(test)]
    maximum_depth: usize,
}

/// A loop over data that a deferral can interrupt: an aggregation (`count`,
/// `sum`, `min`/`max`, `only`) over one entity's related entities, or the
/// resolution of one entity's members of a relation, which tests a derived relation's predicate on each candidate.
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
/// walks the loop itself and records them there, as recursion does. A fold
/// inside a relation predicate is also keyed by the owning resolution, since
/// separate owners must each capture a shared candidate's trace effects.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Step {
    site: Site,
    entity_id: String,
    period: Period,
    parent: Option<CacheKey>,
    /// A predicate's folds belong to the relation resolution capturing their
    /// effects. Two owners can test the same candidate in the same rule.
    owner: Option<DerivedMembersKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Site {
    /// An aggregation over related entities, identified by its node's
    /// address. That is sound because every expression the engine evaluates
    /// is borrowed from the program for the engine's whole life: the
    /// evaluation functions take `&'a` expressions, so the compiler refuses
    /// to evaluate a copy, whose address another expression could reuse.
    Fold(usize),
    /// A relation's members, read from one slot to another.
    #[cfg(test)]
    Members {
        relation: String,
        current_slot: usize,
        related_slot: usize,
    },
}

/// Each binding environment gets its own resumable disjunction. The owner
/// separates relation-effect captures even when they share a parent rule.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ExistsKey {
    node: usize,
    anchor: String,
    candidate: String,
    variables: Vec<String>,
    period: Period,
    parent: Option<CacheKey>,
    owner: Option<DerivedMembersKey>,
}

#[derive(Debug)]
struct ExistsResume {
    domain: Vec<String>,
    next: usize,
    saw_undetermined: bool,
}

/// Incremented after the claimed work occurs, not on entry to an arm.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct ExistsCoverage {
    resumed: usize,
    early_stop: usize,
    empty: usize,
    nonempty: usize,
    nested: usize,
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
/// deferred (evaluated again, from its `where` clause, on the retry), and
/// what the members before it contributed.
#[derive(Debug)]
struct FoldResume {
    related_ids: Vec<String>,
    next: usize,
    fold: Fold,
}

/// What an aggregation over related entities has gathered from the members
/// walked so far. A fold keeps the kind it starts with.
#[derive(Clone, Debug)]
enum Fold {
    /// `count` and `sum`: the members selected and the total of their values
    /// (zero for a count, which reads none).
    Total { count: usize, total: Decimal },
    /// `min` / `max`: the extreme value so far, `None` before the first
    /// member. Numbers are held as `Decimal`, as `sum` holds them.
    Extreme {
        extreme: Extreme,
        best: Option<ScalarValue>,
    },
    /// `only`: the ids of the members selected. Its value is read once the
    /// selection finishes, and only for a single member, so a selection of
    /// none or several reads no value.
    Select(Vec<String>),
}

/// One related entity's part in an aggregation.
enum MemberPart {
    /// The `where` clause does not select it.
    Excluded,
    /// Selected; the aggregation reads no value from it (`count`, `only`).
    Selected,
    /// Selected, with its value.
    Valued(ScalarValue),
}

impl Fold {
    /// Take in one selected member. Fails on a value the aggregation cannot
    /// combine: a non-number for `sum`, an overflowing total, or for
    /// `min`/`max` a value that is neither a number nor a date, or whose kind
    /// differs from the members before it.
    fn add(
        &mut self,
        related_id: &str,
        value: Option<ScalarValue>,
        relation: &str,
        filtered: bool,
    ) -> Result<(), EvalError> {
        match self {
            Self::Total { count, total } => {
                *count += 1;
                if let Some(value) = value {
                    let amount = value.as_decimal().ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "related aggregation requires numeric values".to_string(),
                        )
                    })?;
                    *total = checked_add(*total, amount)?;
                }
            }
            Self::Extreme { extreme, best } => {
                let function = extreme.call_name(filtered);
                let candidate = match value {
                    Some(value @ (ScalarValue::Integer(_) | ScalarValue::Decimal(_))) => {
                        ScalarValue::Decimal(value.as_decimal().expect("a number is a decimal"))
                    }
                    Some(ScalarValue::Date(date)) => ScalarValue::Date(date),
                    Some(ScalarValue::Bool(_)) => {
                        return Err(extreme_value_type_error(function, relation, "a boolean"));
                    }
                    Some(ScalarValue::Text(_)) => {
                        return Err(extreme_value_type_error(function, relation, "text"));
                    }
                    None => unreachable!("min/max reads every member's value"),
                };
                let replace = match (&candidate, best.as_ref()) {
                    (_, None) => true,
                    (ScalarValue::Decimal(candidate), Some(ScalarValue::Decimal(current))) => {
                        extreme.improves(candidate, current)
                            || (candidate == current && decimal_tie_prefers(candidate, current))
                    }
                    (ScalarValue::Date(candidate), Some(ScalarValue::Date(current))) => {
                        extreme.improves(candidate, current)
                    }
                    (ScalarValue::Date(_), Some(_)) => {
                        return Err(extreme_value_type_error(
                            function,
                            relation,
                            "a date where earlier members are numbers",
                        ));
                    }
                    (_, Some(_)) => {
                        return Err(extreme_value_type_error(
                            function,
                            relation,
                            "a number where earlier members are dates",
                        ));
                    }
                };
                if replace {
                    *best = Some(candidate);
                }
            }
            Self::Select(selected) => selected.push(related_id.to_string()),
        }
        Ok(())
    }
}

/// An interrupted resolution of a derived relation's members: the candidates
/// its source relation gives, the candidate whose predicate deferred, and the
/// candidates before it that the predicate selected. The relation's own
/// tuples are read once the resolution finishes, so no retry reads them.
#[cfg(test)]
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
        let mut relation_records: HashMap<String, Vec<&'a crate::model::RelationRecord>> =
            HashMap::new();
        let uses_relation_roles = program.uses_relation_roles();
        for record in &data.relations {
            // Binding refuses these records. Low-level Rust callers skip
            // binding, so exclude them from every lookup as well: programs
            // using relation roles derive their members exclusively.
            if uses_relation_roles
                && program
                    .relations
                    .get(&record.name)
                    .is_some_and(|schema| schema.derivation.is_some())
            {
                continue;
            }
            if program
                .relations
                .get(&record.name)
                .is_some_and(|schema| schema.arity == 1)
            {
                relation_records
                    .entry(record.name.clone())
                    .or_default()
                    .push(record);
            }
            for (slot, value) in record.tuple.iter().enumerate() {
                relation_index
                    .entry((record.name.clone(), slot, value.clone()))
                    .or_default()
                    .push(record);
            }
        }

        Self {
            program,
            input_index,
            relation_index,
            relation_records,
            scalar_cache: HashMap::new(),
            pre_rounding_cache: HashMap::new(),
            judgment_cache: HashMap::new(),
            execution_trace: HashMap::new(),
            trace_index: HashMap::new(),
            active_evaluations: Vec::new(),
            tracing,
            derived_members: HashMap::new(),
            pending_members: HashMap::new(),
            pending_paths: HashMap::new(),
            #[cfg(test)]
            path_coverage: PathCoverage::default(),
            member_chains: HashMap::new(),
            #[cfg(test)]
            source_walks: 0,
            #[cfg(test)]
            relation_suspensions: 0,
            member_captures: Vec::new(),
            relation_depth: 0,
            relation_nesting_limit: RELATION_NESTING_LIMIT,
            suspended_on: None,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            folds: HashMap::new(),
            exists: HashMap::new(),
            #[cfg(test)]
            exists_coverage: ExistsCoverage::default(),
            #[cfg(test)]
            members: HashMap::new(),
            #[cfg(test)]
            recursive_relation_oracle: false,
            visits: 0,
            #[cfg(test)]
            maximum_depth: 0,
        }
    }

    /// The same interpreter, resolving at most `limit` derived relations on
    /// the native stack at once, so tests can force every nested resolution
    /// through the suspend-and-resume driver.
    #[cfg(test)]
    fn with_relation_nesting_limit(mut self, limit: usize) -> Self {
        self.relation_nesting_limit = limit;
        self
    }

    pub fn evaluate_scalar(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        self.drive(|engine| engine.scalar_rule(derived_name, entity_id, period))
    }

    pub fn evaluate_judgment(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<JudgmentOutcome, EvalError> {
        self.drive(|engine| engine.judgment_rule(derived_name, entity_id, period))
    }

    /// Run `root` to completion. When it defers a rule (see [`crate::depth`]),
    /// evaluate that rule as a task of its own, from level zero, and retry.
    /// Tasks form a stack: a task can defer a rule it reaches, which runs
    /// first. The first error ends the evaluation, as it would the recursive
    /// one: an error propagates unchanged from the rule it occurs in to the
    /// root, so a task's error is the root's.
    fn drive<T>(&mut self, root: impl FnMut(&mut Self) -> Eval<T>) -> Result<T, EvalError> {
        let result = self.run_tasks(root);
        // Every retry reaches the loops its first attempt was interrupted in,
        // and resumes them; only an error leaves one pending.
        debug_assert!(
            result.is_err()
                || self
                    .folds
                    .values()
                    .all(|progress| matches!(progress, Progress::Done(_)))
        );
        debug_assert!(
            result.is_err()
                || self
                    .exists
                    .values()
                    .all(|progress| matches!(progress, Progress::Done(_)))
        );
        debug_assert!(self.member_captures.is_empty());
        self.folds.clear();
        self.exists.clear();
        self.pending_members.clear();
        self.pending_paths.clear();
        self.member_chains.clear();
        #[cfg(test)]
        self.members.clear();
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
                        Ok(()) => {
                            tasks.pop();
                            waiting.remove(&task);
                            continue;
                        }
                        Err(interrupt) => interrupt,
                    }
                }
            };
            match interrupt {
                Interrupt::Error(error) => return Err(error),
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
        #[cfg(test)]
        {
            self.maximum_depth = self.maximum_depth.max(self.depth);
        }
        let result = evaluate(self);
        self.depth -= 1;
        result
    }

    fn scalar_rule(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Eval<ScalarValue> {
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.scalar_cache.get(&key) {
            return Ok(value.clone());
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
        let value = evaluated.map_err(|interrupt| {
            interrupt.within_rule(derived.id.as_deref().unwrap_or(derived_name))
        })?;
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
        Ok(rounded)
    }

    fn judgment_rule(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Eval<JudgmentOutcome> {
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.judgment_cache.get(&key) {
            return Ok(*value);
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
        let value = evaluated.map_err(|interrupt| {
            interrupt.within_rule(derived.id.as_deref().unwrap_or(derived_name))
        })?;
        self.judgment_cache.insert(key, value);
        Ok(value)
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

    fn record_evaluated_dependency(&mut self, key: CacheKey) {
        self.record_effect(TraceEffect::Evaluated(key));
    }

    /// A read through a term: an input read at the term's id, or a scalar
    /// rule evaluated there.
    fn role_scalar(
        &mut self,
        expr: &ScalarExpr,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<ScalarValue> {
        match expr {
            ScalarExpr::RoleInput { term, name } => {
                Ok(self.role_input(*term, name, period, relation_context)?)
            }
            ScalarExpr::RoleDerived { term, name } => {
                self.role_scalar_rule(*term, name, period, relation_context)
            }
            _ => unreachable!("only reads through a term"),
        }
    }

    /// The input `name` read at `term`'s id.
    fn role_input(
        &self,
        term: Term,
        name: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Result<ScalarValue, EvalError> {
        self.lookup_input(name, term_id(term, name, relation_context)?, period)
    }

    /// The scalar rule `name` evaluated at `term`'s id.
    fn role_scalar_rule(
        &mut self,
        term: Term,
        name: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<ScalarValue> {
        let target_entity_id = self.role_dependency(term, name, period, relation_context)?;
        self.scalar_rule(name, target_entity_id, period)
    }

    /// The judgment rule `name` evaluated at `term`'s id.
    fn role_judgment_rule(
        &mut self,
        term: Term,
        name: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        let target_entity_id = self.role_dependency(term, name, period, relation_context)?;
        self.judgment_rule(name, target_entity_id, period)
    }

    /// The judgments only a relation role can evaluate, behind one call site
    /// in `judgment_node`. Debug builds reserve every call's arguments and
    /// result in the caller's frame, so a call site per construct grows every
    /// judgment frame, including the frames of deeply nested legacy relation
    /// predicates: a separate site for atoms and term comparisons pushed a
    /// 20,000-link counted chain (`tests/relation_chains.rs`) past a 2 MiB
    /// thread.
    fn relation_role_judgment(
        &mut self,
        expr: &'a JudgmentExpr,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        match expr {
            JudgmentExpr::RoleDerived { term, name } => {
                self.role_judgment_rule(*term, name, period, relation_context)
            }
            JudgmentExpr::RelationAtom { relation, terms } => {
                self.atom_judgment(relation, terms, period, relation_context)
            }
            JudgmentExpr::Exists { .. } => self.exists_judgment(expr, period, relation_context),
            JudgmentExpr::SameEntity {
                left,
                right,
                negated,
            } => self.same_entity_judgment(*left, *right, *negated, relation_context),
            _ => unreachable!("only relation-role judgments"),
        }
    }

    // Keep these arms' binding and allocation locals outside `judgment_node`:
    // debug builds otherwise reserve them in every judgment frame, including
    // the frames of deeply nested legacy relation predicates.
    fn atom_judgment(
        &mut self,
        relation: &str,
        terms: &[Term],
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        let context = relation_context
            .ok_or_else(|| relation_construct_outside_derived_relation("relation atom"))?;
        let ids = terms
            .iter()
            .map(|term| term_id(*term, relation, Some(context)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if self.relation_atom_contains(relation, &ids, period)? {
            JudgmentOutcome::Holds
        } else {
            JudgmentOutcome::NotHolds
        })
    }

    /// Evaluate the finite, sorted disjunction represented by an `exists`.
    /// Keep this frame outside `judgment_node`, whose stack size affects every
    /// legacy relation predicate as well.
    #[inline(never)]
    fn exists_judgment(
        &mut self,
        node: &'a JudgmentExpr,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        let JudgmentExpr::Exists {
            relation,
            terms,
            variable_slot,
            body,
        } = node
        else {
            unreachable!("only exists nodes reach this helper");
        };
        let context = relation_context
            .ok_or_else(|| relation_construct_outside_derived_relation("exists"))?;
        let mut variables = Vec::new();
        let mut frame = context.variables;
        while let Some(bound) = frame {
            variables.push(bound.id.to_string());
            frame = bound.parent;
        }
        let key = ExistsKey {
            node: std::ptr::from_ref(node) as usize,
            anchor: context.current_id.to_string(),
            candidate: context.related_id.to_string(),
            variables,
            period: period.clone(),
            parent: self.active_evaluations.last().cloned(),
            owner: self.member_captures.last().and_then(|(owner, depth, _)| {
                (*depth == self.active_evaluations.len()).then(|| owner.clone())
            }),
        };
        #[cfg(test)]
        let mut resumed = false;
        let ExistsResume {
            domain,
            mut next,
            mut saw_undetermined,
        } = match self.exists.remove(&key) {
            Some(Progress::Done(outcome)) => {
                self.exists.insert(key, Progress::Done(outcome));
                return Ok(outcome);
            }
            Some(Progress::Pending(resume)) => {
                #[cfg(test)]
                {
                    resumed = true;
                }
                resume
            }
            None => {
                let domain =
                    self.exists_domain(relation, terms, *variable_slot, period, context)?;
                #[cfg(test)]
                if domain.is_empty() {
                    self.exists_coverage.empty += 1;
                } else {
                    self.exists_coverage.nonempty += 1;
                }
                ExistsResume {
                    domain,
                    next: 0,
                    saw_undetermined: false,
                }
            }
        };
        while let Some(id) = domain.get(next) {
            self.visits += 1;
            let bound = VariableFrame {
                id,
                parent: context.variables,
            };
            let bound_context = RelationEvalContext {
                variables: Some(&bound),
                ..context
            };
            let evaluated = self.eval_judgment_expr_inner(
                body,
                context.related_id,
                period,
                Some(bound_context),
            );
            #[cfg(test)]
            {
                // Count a resumed site only after its bound body actually ran.
                self.exists_coverage.resumed += usize::from(resumed);
                resumed = false;
                self.exists_coverage.nested += usize::from(context.variables.is_some());
            }
            match evaluated {
                Ok(JudgmentOutcome::Holds) => {
                    for skipped in &domain[next + 1..] {
                        let bound = VariableFrame {
                            id: skipped,
                            parent: context.variables,
                        };
                        self.record_skipped_judgment_dependencies(
                            body,
                            context.related_id,
                            period,
                            Some(RelationEvalContext {
                                variables: Some(&bound),
                                ..context
                            }),
                            TraceSkipReason::ShortCircuit,
                        );
                    }
                    #[cfg(test)]
                    {
                        self.exists_coverage.early_stop += usize::from(next + 1 < domain.len());
                    }
                    self.exists
                        .insert(key, Progress::Done(JudgmentOutcome::Holds));
                    return Ok(JudgmentOutcome::Holds);
                }
                Ok(JudgmentOutcome::NotHolds) => {}
                Ok(JudgmentOutcome::Undetermined) => saw_undetermined = true,
                Err(interrupt)
                    if self.suspended_on.is_some() || matches!(interrupt, Interrupt::Defer(_)) =>
                {
                    self.exists.insert(
                        key,
                        Progress::Pending(ExistsResume {
                            domain,
                            next,
                            saw_undetermined,
                        }),
                    );
                    return Err(interrupt);
                }
                Err(error) => return Err(error),
            }
            next += 1;
        }
        let outcome = if saw_undetermined {
            JudgmentOutcome::Undetermined
        } else {
            JudgmentOutcome::NotHolds
        };
        self.exists.insert(key, Progress::Done(outcome));
        Ok(outcome)
    }

    /// A data domain uses its lowest bound slot's existing tuple index; a
    /// unary generator, which has no bound slots, reads its relation's tuples.
    /// Derived domains read forward members, never supplied derived tuples.
    fn exists_domain(
        &mut self,
        relation: &str,
        terms: &[Term],
        variable_slot: usize,
        period: &Period,
        context: RelationEvalContext<'_>,
    ) -> Eval<Vec<String>> {
        let schema = self
            .program
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_string()))?;
        if terms.len() != schema.arity || variable_slot >= schema.arity {
            return Err(EvalError::TypeMismatch(format!(
                "exists generator `{relation}` has invalid arity or variable slot"
            ))
            .into());
        }
        if let Some(derivation) = &schema.derivation {
            if schema.arity != 2 || variable_slot != derivation.related_slot {
                return Err(EvalError::TypeMismatch(format!("inverse_derived_traversal_unsupported: exists generator `{relation}` must bind its binary derived relation's related slot")).into());
            }
            let anchor = term_id(terms[derivation.current_slot], relation, Some(context))?;
            return self.related_entity_ids(
                relation,
                derivation.current_slot,
                derivation.related_slot,
                anchor,
                period,
            );
        }
        let bound = terms
            .iter()
            .enumerate()
            .filter(|(slot, _)| *slot != variable_slot)
            .map(|(slot, term)| Ok((slot, term_id(*term, relation, Some(context))?)))
            .collect::<Result<Vec<_>, EvalError>>()?;
        let records = if let Some((slot, id)) = bound.first() {
            self.relation_index
                .get(&(relation.to_string(), *slot, (*id).to_string()))
        } else {
            self.relation_records.get(relation)
        }
        .map_or(&[][..], Vec::as_slice);
        self.visits += records.len();
        let mut domain = records
            .iter()
            .filter(|record| {
                record.tuple.len() == terms.len()
                    && record.interval.contains_period(period)
                    && bound.iter().all(|(slot, id)| record.tuple[*slot] == *id)
            })
            .map(|record| record.tuple[variable_slot].clone())
            .collect::<Vec<_>>();
        domain.sort();
        domain.dedup();
        Ok(domain)
    }

    fn same_entity_judgment(
        &self,
        left: Term,
        right: Term,
        negated: bool,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        let context = relation_context
            .ok_or_else(|| relation_construct_outside_derived_relation("term comparison"))?;
        Ok(
            if (term_id(left, "term comparison", Some(context))?
                == term_id(right, "term comparison", Some(context))?)
                != negated
            {
                JudgmentOutcome::Holds
            } else {
                JudgmentOutcome::NotHolds
            },
        )
    }

    /// The id a rule read through `term` is evaluated on, recorded as a
    /// dependency there. This and the reads above live outside
    /// `scalar_node` and `judgment_node`: every nesting level pays for those
    /// two frames, and a debug build does not overlap its match arms' slots.
    fn role_dependency<'c>(
        &mut self,
        term: Term,
        name: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'c>>,
    ) -> Result<&'c str, EvalError> {
        let target_entity_id = term_id(term, name, relation_context)?;
        self.get_derived(name)?;
        self.record_evaluated_dependency(CacheKey {
            derived: name.to_string(),
            entity_id: target_entity_id.to_string(),
            period: period.clone(),
        });
        Ok(target_entity_id)
    }

    fn record_skipped_dependency(&mut self, dependency: SkippedTraceDependency) {
        self.record_effect(TraceEffect::Skipped(dependency));
    }

    fn record_parameter_read(&mut self, read: ParameterTraceRead) {
        self.record_effect(TraceEffect::ParameterRead(read));
    }

    /// Record `effect` on the rule under evaluation, or, while a relation
    /// predicate is being evaluated for that rule, on the relation's
    /// resolution, which replays it on every rule that asks for the relation.
    fn record_effect(&mut self, effect: TraceEffect) {
        if !self.tracing {
            return;
        }
        let depth = self.active_evaluations.len();
        if let Some((_, capture_depth, effects)) = self.member_captures.last_mut()
            && *capture_depth == depth
        {
            effects.push(effect);
            return;
        }
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        match effect {
            TraceEffect::Members(key) => self.replay_members(&parent, key),
            effect => self.record_on(&parent, effect),
        }
    }

    fn record_on(&mut self, parent: &CacheKey, effect: TraceEffect) {
        let trace = self.execution_trace.entry(parent.clone()).or_default();
        let index = self.trace_index.entry(parent.clone()).or_default();
        match effect {
            TraceEffect::Evaluated(key) => {
                if index.dependencies.insert(key.clone()) {
                    trace.dependencies.push(key);
                }
            }
            TraceEffect::Skipped(dependency) => {
                if index.not_evaluated_dependencies.insert(dependency.clone()) {
                    trace.not_evaluated_dependencies.push(dependency);
                }
            }
            TraceEffect::SelectedYearRead(read) => {
                if !trace.selected_year_reads.contains(&read) {
                    trace.selected_year_reads.push(read);
                }
            }
            TraceEffect::ParameterRead(read) => {
                let positions = index
                    .parameter_reads
                    .entry((read.parameter.clone(), read.index))
                    .or_default();
                if !positions
                    .iter()
                    .any(|&position| trace.parameter_reads[position] == read)
                {
                    positions.push(trace.parameter_reads.len());
                    trace.parameter_reads.push(read);
                }
            }
            TraceEffect::Members(key) => self.replay_members(parent, key),
        }
    }

    /// A compact reference to these members' trace effects. Empty resolutions
    /// contribute nothing; a resolution that only forwards its source shares
    /// that source's summary directly. Source edges are canonicalized as they
    /// are captured, so arbitrarily long forwarding chains take one lookup.
    fn member_effect(&self, key: &DerivedMembersKey) -> Option<TraceEffect> {
        match self.derived_members.get(key)?.effects.as_slice() {
            [] => None,
            [TraceEffect::Members(source)] => Some(TraceEffect::Members(source.clone())),
            _ => Some(TraceEffect::Members(key.clone())),
        }
    }

    /// Record on `parent` everything resolving `key` recorded, in order,
    /// expanding the members it read in place. A summary most recently
    /// replayed on `parent` is skipped whole. The walk keeps its own stack,
    /// since an effectful relation chain can nest summaries per link.
    fn replay_members(&mut self, parent: &CacheKey, key: DerivedMembersKey) {
        if !self.mark_replayed(&key, parent) {
            return;
        }
        let mut stack = vec![(key, 0_usize)];
        while let Some((key, next)) = stack.last_mut() {
            let effect = self
                .derived_members
                .get(key)
                .and_then(|members| members.effects.get(*next))
                .cloned();
            *next = next.saturating_add(1);
            match effect {
                None => {
                    stack.pop();
                }
                Some(TraceEffect::Members(child)) => {
                    if self.mark_replayed(&child, parent) {
                        stack.push((child, 0));
                    }
                }
                Some(effect) => self.record_on(parent, effect),
            }
        }
    }

    fn mark_replayed(&mut self, key: &DerivedMembersKey, parent: &CacheKey) -> bool {
        let Some(members) = self.derived_members.get_mut(key) else {
            return false;
        };
        if members.recorded_on.first() == Some(parent) {
            return false;
        }
        members.recorded_on.clear();
        members.recorded_on.push(parent.clone());
        true
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
        self.record_skipped_references(
            derived,
            parameters,
            entity_id,
            period,
            relation_context,
            reason,
        );
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
        self.record_skipped_references(
            derived,
            parameters,
            entity_id,
            period,
            relation_context,
            reason,
        );
    }

    /// Record the rules and parameters a skipped expression would have read.
    /// An unqualified rule read is recorded at the id it routes to; a
    /// qualified read at its term's id. A qualified read with no relation
    /// context names no id, so it is not recorded: evaluating it would fail.
    fn record_skipped_references(
        &mut self,
        derived: Vec<TraceReference>,
        parameters: Vec<String>,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
        reason: TraceSkipReason,
    ) {
        for reference in derived {
            let (name, target_entity_id) = match reference {
                TraceReference::Routed(name) => {
                    let target_entity_id = self
                        .program
                        .derived
                        .get(&name)
                        .and_then(|dependency| {
                            relation_context
                                .and_then(|context| context.entity_id_for(&dependency.entity))
                        })
                        .unwrap_or(entity_id)
                        .to_string();
                    (name, target_entity_id)
                }
                TraceReference::Term(term, name) => {
                    let Some(context) = relation_context else {
                        continue;
                    };
                    let Some(id) = context.id_of(term) else {
                        continue;
                    };
                    (name, id.to_string())
                }
            };
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
    /// per-entity relation aggregations.
    pub(crate) fn eval_scalar_expr(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        self.drive(|engine| engine.eval_scalar_expr_inner(expr, entity_id, period, None))
    }

    fn eval_scalar_expr_inner(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<ScalarValue> {
        self.nested(|engine| engine.scalar_node(expr, entity_id, period, relation_context))
    }

    fn scalar_node(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<ScalarValue> {
        match expr {
            ScalarExpr::Literal(value) => Ok(value.clone()),
            ScalarExpr::InputForYear { input, year } => {
                let declaration =
                    self.program
                        .selected_year_inputs
                        .get(input)
                        .ok_or_else(|| {
                            EvalError::TypeMismatch(format!(
                                "missing selected-year contract {input}"
                            ))
                        })?;
                let selected =
                    crate::selected_year::period(*year).map_err(EvalError::TypeMismatch)?;
                let value = self.lookup_input(&declaration.slot, entity_id, &selected)?;
                let records = self
                    .input_index
                    .get(&(declaration.slot.clone(), entity_id.to_string()));
                let mut covering = records
                    .into_iter()
                    .flat_map(|items| items.iter())
                    .filter(|record| record.interval.contains_period(&selected));
                let first = covering.next();
                let invalid_owner = first.is_none_or(|record| record.entity != declaration.entity)
                    || first.is_some_and(|first| {
                        covering
                            .take_while(|record| record.interval.start == first.interval.start)
                            .any(|record| record.entity != declaration.entity)
                    });
                if invalid_owner {
                    return Err(EvalError::TypeMismatch(format!(
                        "{} requires owner {}",
                        input, declaration.entity
                    ))
                    .into());
                }
                declaration
                    .check_value(&value)
                    .map_err(EvalError::TypeMismatch)?;
                self.record_effect(TraceEffect::SelectedYearRead(SelectedYearTraceRead {
                    input: input.clone(),
                    entity_id: entity_id.to_string(),
                    selected_period: selected,
                    value: value.clone(),
                }));
                Ok(value)
            }
            ScalarExpr::Input(name) => Ok(self.lookup_input(name, entity_id, period)?),
            ScalarExpr::InputOrElse { name, default } => {
                match self.lookup_input(name, entity_id, period) {
                    Ok(value) => Ok(value),
                    Err(EvalError::MissingInput { .. }) => Ok(default.clone()),
                    Err(other) => Err(other.into()),
                }
            }
            ScalarExpr::Derived(name) => {
                let derived = self.get_derived(name)?;
                let target_entity_id = relation_context
                    .and_then(|context| context.entity_id_for(&derived.entity))
                    .unwrap_or(entity_id);
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: target_entity_id.to_string(),
                    period: period.clone(),
                });
                self.scalar_rule(name, target_entity_id, period)
            }
            ScalarExpr::RoleInput { .. } | ScalarExpr::RoleDerived { .. } => {
                self.role_scalar(expr, period, relation_context)
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                let lookup_key = self
                    .eval_scalar_expr_inner(index, entity_id, period, relation_context)?
                    .as_index()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(format!(
                            "parameter key for `{parameter}` must be an integer"
                        ))
                    })?;
                Ok(self.lookup_parameter(parameter, lookup_key, period)?)
            }
            ScalarExpr::Add(items) => {
                let mut total = Decimal::ZERO;
                for item in items {
                    total = checked_add(
                        total,
                        self.eval_decimal(item, entity_id, period, relation_context)?,
                    )?;
                }
                Ok(ScalarValue::Decimal(total))
            }
            ScalarExpr::Sub(left, right) => Ok(ScalarValue::Decimal(checked_sub(
                self.eval_decimal(left, entity_id, period, relation_context)?,
                self.eval_decimal(right, entity_id, period, relation_context)?,
            )?)),
            ScalarExpr::Mul(left, right) => Ok(ScalarValue::Decimal(checked_mul(
                self.eval_decimal(left, entity_id, period, relation_context)?,
                self.eval_decimal(right, entity_id, period, relation_context)?,
            )?)),
            ScalarExpr::Div(left, right) => {
                let divisor = self.eval_decimal(right, entity_id, period, relation_context)?;
                if divisor.is_zero() {
                    return Err(EvalError::DivisionByZero.into());
                }
                Ok(ScalarValue::Decimal(checked_div(
                    self.eval_decimal(left, entity_id, period, relation_context)?,
                    divisor,
                )?))
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
                    if candidate > best {
                        best = candidate;
                    }
                }
                Ok(ScalarValue::Decimal(best))
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
                    if candidate < best {
                        best = candidate;
                    }
                }
                Ok(ScalarValue::Decimal(best))
            }
            ScalarExpr::Ceil(value) => Ok(ScalarValue::Decimal(
                self.eval_decimal(value, entity_id, period, relation_context)?
                    .ceil(),
            )),
            ScalarExpr::Floor(value) => Ok(ScalarValue::Decimal(
                self.eval_decimal(value, entity_id, period, relation_context)?
                    .floor(),
            )),
            ScalarExpr::PeriodStart => Ok(ScalarValue::Date(period.start)),
            ScalarExpr::PeriodEnd => Ok(ScalarValue::Date(period.end)),
            ScalarExpr::DateAddDays { date, days } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_days expects a date on the left".to_string(),
                        )
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(days, entity_id, period, relation_context)?
                    .as_index()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_days expects an integer day count on the right".to_string(),
                        )
                    })?;
                Ok(ScalarValue::Date(shift_calendar_days(base, offset)?))
            }
            ScalarExpr::DateAddMonths { date, months } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_months expects a date on the left".to_string(),
                        )
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(months, entity_id, period, relation_context)?
                    .as_index()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_months expects an integer month count on the right"
                                .to_string(),
                        )
                    })?;
                Ok(ScalarValue::Date(shift_calendar_months(base, offset)?))
            }
            ScalarExpr::DateAddYears { date, years } => {
                let base = self
                    .eval_scalar_expr_inner(date, entity_id, period, relation_context)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_years expects a date on the left".to_string(),
                        )
                    })?;
                let offset = self
                    .eval_scalar_expr_inner(years, entity_id, period, relation_context)?
                    .as_index()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_years expects an integer year count on the right".to_string(),
                        )
                    })?;
                Ok(ScalarValue::Date(shift_calendar_years(base, offset)?))
            }
            ScalarExpr::DaysBetween { from, to } => {
                let a = self
                    .eval_scalar_expr_inner(from, entity_id, period, relation_context)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "days_between expects a date for `from`".to_string(),
                        )
                    })?;
                let b = self
                    .eval_scalar_expr_inner(to, entity_id, period, relation_context)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch("days_between expects a date for `to`".to_string())
                    })?;
                Ok(ScalarValue::Integer(b.signed_duration_since(a).num_days()))
            }
            ScalarExpr::OnlyRelated {
                relation,
                current_slot,
                related_slot,
                value,
            } => {
                let ids = self.related_entity_ids(
                    relation,
                    *current_slot,
                    *related_slot,
                    entity_id,
                    period,
                )?;
                let related = match ids.as_slice() {
                    [] => {
                        return Err(EvalError::MissingRelation {
                            relation: relation.clone(),
                            entity_id: entity_id.into(),
                            period_start: period.start,
                            period_end: period.end,
                        }
                        .into());
                    }
                    [id] => id,
                    _ => {
                        return Err(EvalError::AmbiguousRelation {
                            relation: relation.clone(),
                            entity_id: entity_id.into(),
                            period_start: period.start,
                            period_end: period.end,
                            count: ids.len(),
                        }
                        .into());
                    }
                };
                let RelatedValueRef::Input(name) = value else {
                    return Err(EvalError::TypeMismatch(
                        "only_related requires a Boolean input".into(),
                    )
                    .into());
                };
                let result = self.lookup_input(name, related, period)?;
                if !matches!(result, ScalarValue::Bool(_)) {
                    return Err(EvalError::TypeMismatch(
                        "only_related requires a Boolean input".into(),
                    )
                    .into());
                }
                Ok(result)
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let fold = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    None,
                    Fold::Total {
                        count: 0,
                        total: Decimal::ZERO,
                    },
                    entity_id,
                    period,
                )?;
                let Fold::Total { count, .. } = fold else {
                    unreachable!("a fold keeps its kind");
                };
                // A count of collected ids fits in i64 on every supported target.
                Ok(ScalarValue::Integer(count as i64))
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let fold = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    Some(value),
                    Fold::Total {
                        count: 0,
                        total: Decimal::ZERO,
                    },
                    entity_id,
                    period,
                )?;
                let Fold::Total { total, .. } = fold else {
                    unreachable!("a fold keeps its kind");
                };
                Ok(ScalarValue::Decimal(total))
            }
            ScalarExpr::ExtremeRelated {
                extreme,
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let fold = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    Some(value),
                    Fold::Extreme {
                        extreme: *extreme,
                        best: None,
                    },
                    entity_id,
                    period,
                )?;
                let Fold::Extreme { best, .. } = fold else {
                    unreachable!("a fold keeps its kind");
                };
                // The least of nothing has no value; the encoder guards the
                // call with a count rather than the engine inventing one.
                best.ok_or_else(|| {
                    EvalError::EmptyRelatedAggregate {
                        function: extreme.call_name(where_clause.is_some()),
                        relation: relation.clone(),
                        entity_id: entity_id.to_string(),
                        period_start: period.start,
                        period_end: period.end,
                    }
                    .into()
                })
            }
            ScalarExpr::SingleRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let fold = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    None,
                    Fold::Select(Vec::new()),
                    entity_id,
                    period,
                )?;
                let Fold::Select(selected) = fold else {
                    unreachable!("a fold keeps its kind");
                };
                match selected.as_slice() {
                    [member] => self.eval_related_value(value, member, period),
                    [] => Err(EvalError::MissingRelation {
                        relation: relation.clone(),
                        entity_id: entity_id.to_string(),
                        period_start: period.start,
                        period_end: period.end,
                    }
                    .into()),
                    members => Err(EvalError::AmbiguousRelation {
                        relation: relation.clone(),
                        entity_id: entity_id.to_string(),
                        period_start: period.start,
                        period_end: period.end,
                        count: members.len(),
                    }
                    .into()),
                }
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                if self
                    .eval_judgment_expr_inner(condition, entity_id, period, relation_context)?
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
                let value =
                    self.eval_scalar_expr_inner(subject, entity_id, period, relation_context)?;
                Err(no_matching_arm(subject, &value, patterns).into())
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
    ) -> Eval<JudgmentOutcome> {
        self.eval_judgment_expr_inner(expr, entity_id, period, None)
    }

    fn eval_judgment_expr_inner(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        self.nested(|engine| engine.judgment_node(expr, entity_id, period, relation_context))
    }

    fn judgment_node(
        &mut self,
        expr: &'a JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<JudgmentOutcome> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                let left_value =
                    self.eval_scalar_expr_inner(left, entity_id, period, relation_context)?;
                let right_value =
                    self.eval_scalar_expr_inner(right, entity_id, period, relation_context)?;
                Ok(
                    if self.compare_scalar_values(&left_value, *op, &right_value)? {
                        JudgmentOutcome::Holds
                    } else {
                        JudgmentOutcome::NotHolds
                    },
                )
            }
            JudgmentExpr::Derived(name) => {
                let derived = self.get_derived(name)?;
                let target_entity_id = relation_context
                    .and_then(|context| context.entity_id_for(&derived.entity))
                    .unwrap_or(entity_id);
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: target_entity_id.to_string(),
                    period: period.clone(),
                });
                self.judgment_rule(name, target_entity_id, period)
            }
            JudgmentExpr::RoleDerived { .. }
            | JudgmentExpr::RelationAtom { .. }
            | JudgmentExpr::SameEntity { .. }
            | JudgmentExpr::Exists { .. } => {
                self.relation_role_judgment(expr, period, relation_context)
            }
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => {
                let context = relation_context
                    .ok_or_else(|| relation_member_outside_derived_relation(relation))?;
                Ok(
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
                )
            }
            JudgmentExpr::And(items) => {
                let mut saw_undetermined = false;
                for (index, item) in items.iter().enumerate() {
                    match self.eval_judgment_expr_inner(
                        item,
                        entity_id,
                        period,
                        relation_context,
                    )? {
                        JudgmentOutcome::Holds => {}
                        JudgmentOutcome::NotHolds => {
                            for skipped in &items[index + 1..] {
                                self.record_skipped_judgment_dependencies(
                                    skipped,
                                    entity_id,
                                    period,
                                    relation_context,
                                    TraceSkipReason::ShortCircuit,
                                );
                            }
                            return Ok(JudgmentOutcome::NotHolds);
                        }
                        JudgmentOutcome::Undetermined => saw_undetermined = true,
                    }
                }
                Ok(if saw_undetermined {
                    JudgmentOutcome::Undetermined
                } else {
                    JudgmentOutcome::Holds
                })
            }
            JudgmentExpr::Or(items) => {
                let mut saw_undetermined = false;
                for (index, item) in items.iter().enumerate() {
                    match self.eval_judgment_expr_inner(
                        item,
                        entity_id,
                        period,
                        relation_context,
                    )? {
                        JudgmentOutcome::Holds => {
                            for skipped in &items[index + 1..] {
                                self.record_skipped_judgment_dependencies(
                                    skipped,
                                    entity_id,
                                    period,
                                    relation_context,
                                    TraceSkipReason::ShortCircuit,
                                );
                            }
                            return Ok(JudgmentOutcome::Holds);
                        }
                        JudgmentOutcome::NotHolds => {}
                        JudgmentOutcome::Undetermined => saw_undetermined = true,
                    }
                }
                Ok(if saw_undetermined {
                    JudgmentOutcome::Undetermined
                } else {
                    JudgmentOutcome::NotHolds
                })
            }
            JudgmentExpr::Not(item) => Ok(
                match self.eval_judgment_expr_inner(item, entity_id, period, relation_context)? {
                    JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
                    JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
                    JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
                },
            ),
        }
    }

    /// Walk the related entities of `entity_id` in order, and fold into
    /// `fold` each one `where_clause` selects, with its `value` when one is
    /// given: a `count` passes no `value` and reads only the count, a `sum`
    /// reads only the total, a `min`/`max` reads the extreme, and an `only`
    /// passes no `value` and reads the members selected. The first member
    /// whose clause or value fails, or that the fold cannot take in, ends the
    /// walk with its error.
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
        fold: Fold,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Fold> {
        let key = self.step(
            Site::Fold(std::ptr::from_ref(node) as usize),
            entity_id,
            period,
        );
        let FoldResume {
            related_ids,
            mut next,
            mut fold,
        } = match self.folds.remove(&key) {
            Some(Progress::Done(result)) => {
                self.folds.insert(key, Progress::Done(result.clone()));
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
                fold,
            },
        };
        while let Some(related_id) = related_ids.get(next) {
            // Each member counts as a visit, so a fold walked again shows in
            // `count_visits` even when its members read nothing.
            self.visits += 1;
            match self.fold_member(where_clause, value, related_id, period) {
                Ok(MemberPart::Excluded) => {}
                Ok(MemberPart::Selected) => {
                    fold.add(related_id, None, relation, where_clause.is_some())?;
                }
                Ok(MemberPart::Valued(member_value)) => {
                    fold.add(
                        related_id,
                        Some(member_value),
                        relation,
                        where_clause.is_some(),
                    )?;
                }
                Err(interrupt)
                    if self.suspended_on.is_some() || matches!(interrupt, Interrupt::Defer(_)) =>
                {
                    self.folds.insert(
                        key,
                        Progress::Pending(FoldResume {
                            related_ids,
                            next,
                            fold,
                        }),
                    );
                    return Err(interrupt);
                }
                Err(error) => return Err(error),
            }
            next += 1;
        }
        self.folds.insert(key, Progress::Done(fold.clone()));
        Ok(fold)
    }

    /// The key of a loop at `site` for this entity and period, in the rule
    /// evaluation now running (see [`Step`]).
    fn step(&self, site: Site, entity_id: &str, period: &Period) -> Step {
        Step {
            site,
            entity_id: entity_id.to_string(),
            period: period.clone(),
            parent: self.active_evaluations.last().cloned(),
            owner: self.member_captures.last().and_then(|(owner, depth, _)| {
                (*depth == self.active_evaluations.len()).then(|| owner.clone())
            }),
        }
    }

    /// One related entity's part in an aggregation: the `where` clause
    /// first, then, for a selected member, its value when the aggregation
    /// reads one.
    fn fold_member(
        &mut self,
        where_clause: Option<&'a JudgmentExpr>,
        value: Option<&'a RelatedValueRef>,
        related_id: &str,
        period: &Period,
    ) -> Eval<MemberPart> {
        if let Some(predicate) = where_clause {
            if !self
                .eval_judgment_expr(predicate, related_id, period)?
                .is_holds()
            {
                return Ok(MemberPart::Excluded);
            }
        }
        match value {
            Some(value) => Ok(MemberPart::Valued(
                self.eval_related_value(value, related_id, period)?,
            )),
            None => Ok(MemberPart::Selected),
        }
    }

    fn eval_related_value(
        &mut self,
        value: &'a RelatedValueRef,
        entity_id: &str,
        period: &Period,
    ) -> Eval<ScalarValue> {
        match value {
            RelatedValueRef::Input(name) => Ok(self.lookup_input(name, entity_id, period)?),
            RelatedValueRef::Derived(name) => {
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                });
                self.scalar_rule(name, entity_id, period)
            }
        }
    }

    fn eval_decimal(
        &mut self,
        expr: &'a ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Eval<Decimal> {
        Ok(self
            .eval_scalar_expr_inner(expr, entity_id, period, relation_context)?
            .as_decimal()
            .ok_or_else(|| EvalError::TypeMismatch("expected numeric scalar".to_string()))?)
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
        self.nested(|engine| {
            #[cfg(test)]
            if engine.recursive_relation_oracle {
                return engine.recursive_related_entity_ids(
                    relation,
                    current_slot,
                    related_slot,
                    entity_id,
                    period,
                );
            }
            let schema = engine.relation_schema(relation, current_slot, related_slot)?;
            let derived = if schema.derivation.is_some() {
                Some(engine.derived_members(relation, entity_id, period)?)
            } else {
                None
            };
            let mut ids =
                engine.direct_related_ids(relation, current_slot, related_slot, entity_id, period);
            if let Some(derived) = derived {
                ids.extend(derived.iter().cloned());
            }
            ids.sort();
            ids.dedup();
            Ok(ids)
        })
    }

    #[cfg(test)]
    fn recursive_related_entity_ids(
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
            return Err(EvalError::TypeMismatch(format!(
                "relation `{relation}` has arity {}, but slots {current_slot} and {related_slot} were requested",
                schema.arity
            ))
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
                    Some(derivation) => {
                        if let Some(path) = &derivation.source_path {
                            let mut frontier =
                                std::collections::BTreeSet::from([entity_id.to_string()]);
                            for step in path {
                                let mut reached = std::collections::BTreeSet::new();
                                for id in frontier {
                                    for next in self.related_entity_ids(
                                        &step.relation,
                                        step.from_slot,
                                        step.to_slot,
                                        &id,
                                        period,
                                    )? {
                                        reached.insert(next);
                                    }
                                }
                                frontier = reached;
                            }
                            frontier.into_iter().collect()
                        } else {
                            self.related_entity_ids(
                                derivation
                                    .source_relation
                                    .as_deref()
                                    .expect("a single source"),
                                derivation.current_slot,
                                derivation.related_slot,
                                entity_id,
                                period,
                            )?
                        }
                    }
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
                    current_entity: derivation.slot_entities.get(derivation.current_slot),
                    related_entity: derivation.slot_entities.get(derivation.related_slot),
                    variables: None,
                };
                match self.eval_judgment_expr_inner(
                    &derivation.predicate,
                    related_id,
                    period,
                    Some(context),
                ) {
                    Ok(outcome) => {
                        if outcome.is_holds() {
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
        #[cfg(test)]
        if self.recursive_relation_oracle {
            return Ok(self
                .related_entity_ids(relation, current_slot, related_slot, current_id, period)?
                .iter()
                .any(|id| id == related_id));
        }
        let schema = self.relation_schema(relation, current_slot, related_slot)?;
        let records = self
            .relation_index
            .get(&(relation.to_string(), current_slot, current_id.to_string()))
            .map_or(&[][..], Vec::as_slice);
        self.visits += records.len();
        let direct = records.iter().any(|record| {
            record.interval.contains_period(period)
                && record
                    .tuple
                    .get(related_slot)
                    .is_some_and(|candidate| candidate == related_id)
        });
        if schema.derivation.is_none() {
            return Ok(direct);
        }
        // Resolve the derived members even when a tuple already answers, so
        // the trace records the predicates as it always has.
        let derived = self.derived_members(relation, current_id, period)?;
        Ok(direct
            || derived
                .binary_search_by(|candidate| candidate.as_str().cmp(related_id))
                .is_ok())
    }

    /// An atom fixes every slot, rather than selecting only the two slots
    /// used by a bare membership judgment. A derived atom reads its own
    /// forward members for the anchor named in its current slot; that anchor
    /// can differ from the enclosing predicate's anchor.
    fn relation_atom_contains(
        &mut self,
        relation: &str,
        ids: &[&str],
        period: &Period,
    ) -> Eval<bool> {
        let program: &'a Program = self.program;
        let schema = program
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_string()))?;
        if ids.len() != schema.arity {
            return Err(EvalError::TypeMismatch(format!(
                "relation atom `{relation}` has {} terms, but the relation has arity {}",
                ids.len(),
                schema.arity
            ))
            .into());
        }
        if let Some(derivation) = &schema.derivation {
            if schema.arity != 2 {
                return Err(EvalError::TypeMismatch(format!(
                    "relation atom `{relation}` uses a derived relation of arity {}; only arity 2 is supported",
                    schema.arity
                ))
                .into());
            }
            let current_id = ids.get(derivation.current_slot).ok_or_else(|| {
                EvalError::TypeMismatch(format!(
                    "relation atom `{relation}` has no current slot {}",
                    derivation.current_slot
                ))
            })?;
            let related_id = ids.get(derivation.related_slot).ok_or_else(|| {
                EvalError::TypeMismatch(format!(
                    "relation atom `{relation}` has no related slot {}",
                    derivation.related_slot
                ))
            })?;
            return Ok(self
                .related_entity_ids(
                    relation,
                    derivation.current_slot,
                    derivation.related_slot,
                    current_id,
                    period,
                )?
                .binary_search_by(|candidate| candidate.as_str().cmp(related_id))
                .is_ok());
        }
        let Some(first_id) = ids.first() else {
            return Err(EvalError::TypeMismatch(format!(
                "relation atom `{relation}` needs at least one term"
            ))
            .into());
        };
        let records = self
            .relation_index
            .get(&(relation.to_string(), 0, (*first_id).to_string()))
            .map_or(&[][..], Vec::as_slice);
        self.visits += records.len();
        Ok(records.iter().any(|record| {
            record.interval.contains_period(period)
                && record
                    .tuple
                    .iter()
                    .map(String::as_str)
                    .eq(ids.iter().copied())
        }))
    }

    /// `relation`'s schema, when it exists and has both slots.
    fn relation_schema(
        &self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
    ) -> Result<&'a crate::model::RelationSchema, EvalError> {
        let program: &'a Program = self.program;
        let schema = program
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_string()))?;
        if current_slot >= schema.arity || related_slot >= schema.arity {
            return Err(EvalError::TypeMismatch(format!(
                "relation `{relation}` has arity {}, but slots {current_slot} and {related_slot} were requested",
                schema.arity
            )));
        }
        Ok(schema)
    }

    /// The members the derived relation `relation` adds for `entity_id` in
    /// `period`. They are resolved once; every later use reads them and
    /// replays the trace records resolving them made.
    fn derived_members(
        &mut self,
        relation: &str,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Rc<[String]>> {
        let key = members_key(relation, entity_id, period);
        if !self.derived_members.contains_key(&key) {
            if self.relation_depth >= self.relation_nesting_limit {
                self.suspended_on = Some(key);
                return Err(EvalError::TypeMismatch(
                    "derived relation resolution suspended".to_string(),
                )
                .into());
            }
            self.resolve_members(key.clone())?;
        }
        if let Some(effect) = self.member_effect(&key) {
            self.record_effect(effect);
        }
        self.derived_members
            .get(&key)
            .expect("resolution memoizes the members it was asked for")
            .result
            .clone()
            .map_err(Into::into)
    }

    /// Resolve `key` without nesting resolutions on the native stack past
    /// [`RELATION_NESTING_LIMIT`]. An attempt resolves as far as it can. When
    /// it stops for members it needs, those go on this driver's stack, and the
    /// stopped attempt runs again once they are resolved: it rereads what it
    /// evaluated from the caches, and its predicate resumes at the candidate
    /// that stopped it. The order in which rules and relations are first
    /// evaluated is therefore the order recursion would have evaluated them.
    fn resolve_members(&mut self, key: DerivedMembersKey) -> Eval<()> {
        let mut stack = vec![key.clone()];
        let mut on_stack = HashSet::from([key]);
        while let Some(top) = stack.last().cloned() {
            self.relation_depth = self.relation_depth.saturating_add(1);
            let resolved = self.fold_members(&top);
            self.relation_depth = self.relation_depth.saturating_sub(1);
            if let Some(needed) = self.suspended_on.take() {
                if on_stack.insert(needed.clone()) {
                    #[cfg(test)]
                    {
                        self.relation_suspensions += 1;
                    }
                    stack.push(needed);
                    continue;
                }
                // `top` needs members that wait on `top`: a cycle, which
                // program validation refuses. The recursive resolution this
                // replaces overflowed the stack on one.
                self.pending_members.remove(&top);
                self.pending_paths.remove(&top);
                self.member_chains.remove(&top);
                self.derived_members.insert(
                    top.clone(),
                    DerivedMembers {
                        result: Err(cyclic_relation_error(&needed.relation)),
                        effects: Vec::new(),
                        recorded_on: Vec::new(),
                    },
                );
            } else {
                resolved?;
            }
            debug_assert!(self.derived_members.contains_key(&top));
            stack.pop();
            on_stack.remove(&top);
        }
        Ok(())
    }

    /// Resolve the members of `key`'s relation, and before them those of each
    /// unresolved derived relation down its chain of source relations,
    /// deepest first, memoizing each. The chain is walked by a loop, not by
    /// recursion. Stops, leaving `suspended_on` set and the stopped
    /// predicate's progress in `pending_members`, when a predicate needs
    /// members that must be resolved first.
    fn fold_members(&mut self, key: &DerivedMembersKey) -> Eval<()> {
        let program: &'a Program = self.program;
        let (entity_id, period) = (key.entity_id.as_str(), &key.period);
        let mut chain = match self.member_chains.remove(key) {
            Some(chain) => chain,
            None => {
                // Down the chain, check each source relation before evaluating any
                // predicate, as resolution always has: it exists and has the slots
                // the derivation reads.
                let mut chain = Vec::new();
                let mut on_chain = HashSet::new();
                let mut relation = program
                    .relations
                    .get(&key.relation)
                    .expect("members are resolved only for a known derived relation");
                let failure = loop {
                    let derivation = relation
                        .derivation
                        .as_ref()
                        .expect("members are resolved only for a derived relation");
                    #[cfg(test)]
                    {
                        self.source_walks += 1;
                    }
                    chain.push(relation);
                    on_chain.insert(relation.name.as_str());
                    // Paths branch and change the lookup id at every step.
                    // Their resumable frontier is handled at this chain level.
                    if derivation.source_path.is_some() {
                        break None;
                    }
                    let source = match self.relation_schema(
                        derivation
                            .source_relation
                            .as_deref()
                            .expect("a single source"),
                        derivation.current_slot,
                        derivation.related_slot,
                    ) {
                        Ok(source) => source,
                        Err(error) => break Some(error),
                    };
                    if source.derivation.is_none()
                        || self.derived_members.contains_key(&members_key(
                            &source.name,
                            entity_id,
                            period,
                        ))
                    {
                        break None;
                    }
                    if on_chain.contains(source.name.as_str()) {
                        break Some(cyclic_relation_error(&source.name));
                    }
                    relation = source;
                };
                if let Some(error) = failure {
                    // Resolution fails at every level above the failed check, before
                    // any predicate runs.
                    for relation in chain {
                        let level = members_key(&relation.name, entity_id, period);
                        self.pending_members.remove(&level);
                        self.derived_members.insert(
                            level,
                            DerivedMembers {
                                result: Err(error.clone()),
                                effects: Vec::new(),
                                recorded_on: Vec::new(),
                            },
                        );
                    }
                    return Ok(());
                }

                chain
            }
        };
        while let Some(relation) = chain.last().copied() {
            let level = members_key(&relation.name, entity_id, period);
            if self.derived_members.contains_key(&level) {
                chain.pop();
                continue;
            }
            let derivation = relation
                .derivation
                .as_ref()
                .expect("the chain holds derived relations");
            if derivation.source_path.is_some() {
                if let Err(interrupt) = self.fold_path_members(&level, derivation) {
                    self.member_chains.insert(key.clone(), chain);
                    return Err(interrupt);
                }
                chain.pop();
                continue;
            }
            let pending = match self.pending_members.remove(&level) {
                Some(pending) => pending,
                None => match self.member_candidates(derivation, entity_id, period) {
                    Ok(pending) => pending,
                    Err((error, effects)) => {
                        self.derived_members.insert(
                            level,
                            DerivedMembers {
                                result: Err(error),
                                effects,
                                recorded_on: Vec::new(),
                            },
                        );
                        chain.pop();
                        continue;
                    }
                },
            };
            if let Err(interrupt) = self.filter_members(&level, derivation, pending) {
                self.member_chains.insert(key.clone(), chain);
                return Err(interrupt);
            }
            chain.pop();
        }
        Ok(())
    }

    /// The candidates a derived relation's predicate tests: its source
    /// relation's related ids, read at the derivation's slots, whose own
    /// members are already resolved.
    fn member_candidates(
        &mut self,
        derivation: &'a crate::model::RelationDerivation,
        entity_id: &str,
        period: &Period,
    ) -> Result<PendingMembers, (EvalError, Vec<TraceEffect>)> {
        let program: &'a Program = self.program;
        let source = derivation
            .source_relation
            .as_deref()
            .expect("a single source");
        let mut candidates = self.direct_related_ids(
            source,
            derivation.current_slot,
            derivation.related_slot,
            entity_id,
            period,
        );
        let mut effects = Vec::new();
        if program
            .relations
            .get(source)
            .is_some_and(|source| source.derivation.is_some())
        {
            let source_key = members_key(source, entity_id, period);
            let result = self
                .derived_members
                .get(&source_key)
                .expect("a source's members resolve before the relation's")
                .result
                .clone();
            if let Some(effect) = self.member_effect(&source_key) {
                effects.push(effect);
            }
            match result {
                Ok(ids) => candidates.extend(ids.iter().cloned()),
                Err(error) => return Err((error, effects)),
            }
        }
        candidates.sort();
        candidates.dedup();
        Ok(PendingMembers {
            candidates: candidates.into(),
            next: 0,
            kept: Vec::new(),
            effects,
        })
    }

    /// Generate a path's candidates before filtering them. Keep the path state
    /// separate from a predicate's state so legacy source-chain frames stay
    /// small. Captures carry a derived step's effects across either kind of stop.
    fn fold_path_members(
        &mut self,
        level: &DerivedMembersKey,
        derivation: &'a crate::model::RelationDerivation,
    ) -> Eval<()> {
        if let Some(pending) = self.pending_members.remove(level) {
            return self.filter_members(level, derivation, pending);
        }
        let steps = derivation.source_path.as_ref().expect("a path source");
        let saved = self.pending_paths.remove(level);
        #[cfg(test)]
        let mut resumed = saved.is_some();
        let mut progress = saved.unwrap_or_else(|| PathResume {
            step: 0,
            frontier: vec![level.entity_id.clone()],
            next: 0,
            reached: Vec::new(),
            effects: Vec::new(),
        });
        self.member_captures.push((
            level.clone(),
            self.active_evaluations.len(),
            std::mem::take(&mut progress.effects),
        ));
        while let Some(step) = steps.get(progress.step) {
            while let Some(id) = progress.frontier.get(progress.next) {
                match self.related_entity_ids(
                    &step.relation,
                    step.from_slot,
                    step.to_slot,
                    id,
                    &level.period,
                ) {
                    Ok(ids) => {
                        progress.reached.extend(ids);
                        progress.next += 1;
                        #[cfg(test)]
                        {
                            actual_path_work::lookup(
                                resumed,
                                self.program.relations[&step.relation]
                                    .derivation
                                    .is_some()
                                    .then_some(progress.step),
                            );
                            if resumed {
                                self.path_coverage.resumed += 1;
                                resumed = false;
                            }
                            if self.program.relations[&step.relation].derivation.is_some() {
                                self.path_coverage.derived_positions[progress.step.min(3)] += 1;
                            }
                        }
                    }
                    Err(interrupt) => {
                        let (_, _, effects) = self.member_captures.pop().expect("path capture");
                        progress.effects = effects;
                        if self.suspended_on.is_some() || matches!(interrupt, Interrupt::Defer(_)) {
                            self.pending_paths.insert(level.clone(), progress);
                            return Err(interrupt);
                        }
                        let Interrupt::Error(error) = interrupt else {
                            unreachable!("deferral saved above")
                        };
                        self.derived_members.insert(
                            level.clone(),
                            DerivedMembers {
                                result: Err(error),
                                effects: progress.effects,
                                recorded_on: Vec::new(),
                            },
                        );
                        return Ok(());
                    }
                }
            }
            progress.reached.sort();
            progress.reached.dedup();
            progress.frontier = std::mem::take(&mut progress.reached);
            progress.next = 0;
            progress.step += 1;
        }
        let (_, _, effects) = self.member_captures.pop().expect("path capture");
        #[cfg(test)]
        {
            actual_path_work::completed(&progress.frontier, steps.len());
            if progress.frontier.is_empty() {
                self.path_coverage.empty += 1;
            } else {
                self.path_coverage.nonempty += 1;
            }
            if steps.len() >= 3 {
                self.path_coverage.long += 1;
            }
        }
        self.filter_members(
            level,
            derivation,
            PendingMembers {
                candidates: progress.frontier.into(),
                next: 0,
                kept: Vec::new(),
                effects,
            },
        )
    }

    /// Test the remaining candidates against the derivation's predicate and
    /// memoize the members that hold. On a relation suspension or rule
    /// deferral, keep the progress in `pending_members` for the retry.
    fn filter_members(
        &mut self,
        level: &DerivedMembersKey,
        derivation: &'a crate::model::RelationDerivation,
        pending: PendingMembers,
    ) -> Eval<()> {
        let PendingMembers {
            candidates,
            next,
            mut kept,
            effects,
        } = pending;
        self.member_captures
            .push((level.clone(), self.active_evaluations.len(), effects));
        let current_entity = derivation.slot_entities.get(derivation.current_slot);
        let related_entity = derivation.slot_entities.get(derivation.related_slot);
        let mut failure = None;
        for (index, candidate) in candidates.iter().enumerate().skip(next) {
            let context = RelationEvalContext {
                current_id: &level.entity_id,
                related_id: candidate,
                current_entity,
                related_entity,
                variables: None,
            };
            match self.eval_judgment_expr_inner(
                &derivation.predicate,
                candidate,
                &level.period,
                Some(context),
            ) {
                Ok(outcome) => {
                    if outcome.is_holds() {
                        kept.push(candidate.clone());
                    }
                }
                Err(interrupt)
                    if self.suspended_on.is_some() || matches!(interrupt, Interrupt::Defer(_)) =>
                {
                    // This candidate is tested again from the start once the
                    // members it needs are resolved.
                    // A finished aggregation is reused on retry and makes
                    // no trace records again. Keep the records it already
                    // made; replay deduplicates repeated records in order.
                    let (_, _, effects) = self
                        .member_captures
                        .pop()
                        .expect("the capture pushed above");
                    self.pending_members.insert(
                        level.clone(),
                        PendingMembers {
                            candidates: Rc::clone(&candidates),
                            next: index,
                            kept,
                            effects,
                        },
                    );
                    return Err(interrupt);
                }
                Err(Interrupt::Error(error)) => {
                    failure = Some(error);
                    break;
                }
                Err(Interrupt::Defer(_)) => unreachable!("deferrals are saved for retry above"),
            }
        }
        let (_, _, effects) = self
            .member_captures
            .pop()
            .expect("the capture pushed above");
        self.derived_members.insert(
            level.clone(),
            DerivedMembers {
                result: failure.map_or_else(|| Ok(kept.into()), Err),
                effects,
                recorded_on: Vec::new(),
            },
        );
        Ok(())
    }

    fn compare_scalar_values(
        &self,
        left: &ScalarValue,
        op: ComparisonOp,
        right: &ScalarValue,
    ) -> Result<bool, EvalError> {
        compare_scalar_values(left, op, right)
    }
}

fn members_key(relation: &str, entity_id: &str, period: &Period) -> DerivedMembersKey {
    DerivedMembersKey {
        relation: relation.to_string(),
        entity_id: entity_id.to_string(),
        period: period.clone(),
    }
}

/// The error for a derived relation whose members depend on themselves,
/// which program validation refuses before execution.
fn cyclic_relation_error(relation: &str) -> EvalError {
    EvalError::TypeMismatch(format!(
        "derived relation `{relation}` depends on its own members"
    ))
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

/// A rule a skipped expression would have read: unqualified, so routed by
/// the rule's entity, or qualified by a term, so read at that term's id.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceReference {
    Routed(String),
    Term(Term, String),
}

fn collect_scalar_trace_references(
    expr: &ScalarExpr,
    derived: &mut Vec<TraceReference>,
    parameters: &mut Vec<String>,
) {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::InputForYear { .. }
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::RoleInput { .. }
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
        ScalarExpr::Derived(name) => derived.push(TraceReference::Routed(name.clone())),
        ScalarExpr::RoleDerived { term, name } => {
            derived.push(TraceReference::Term(*term, name.clone()));
        }
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
        ScalarExpr::OnlyRelated { .. } => {}
        ScalarExpr::CountRelated { where_clause, .. } => {
            if let Some(predicate) = where_clause {
                collect_judgment_trace_references(predicate, derived, parameters);
            }
        }
        ScalarExpr::SumRelated {
            value,
            where_clause,
            ..
        }
        | ScalarExpr::ExtremeRelated {
            value,
            where_clause,
            ..
        }
        | ScalarExpr::SingleRelated {
            value,
            where_clause,
            ..
        } => {
            if let RelatedValueRef::Derived(name) = value {
                derived.push(TraceReference::Routed(name.clone()));
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
    derived: &mut Vec<TraceReference>,
    parameters: &mut Vec<String>,
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_trace_references(left, derived, parameters);
            collect_scalar_trace_references(right, derived, parameters);
        }
        JudgmentExpr::Derived(name) => derived.push(TraceReference::Routed(name.clone())),
        JudgmentExpr::RoleDerived { term, name } => {
            derived.push(TraceReference::Term(*term, name.clone()));
        }
        JudgmentExpr::Exists { body, .. } => {
            // No domain is evaluated on a skipped path. This binder has no
            // id; outer frames remain available after removing its index.
            let mut body_references = Vec::new();
            collect_judgment_trace_references(body, &mut body_references, parameters);
            derived.extend(
                body_references
                    .into_iter()
                    .filter_map(|reference| match reference {
                        TraceReference::Term(Term::Var(0), _) => None,
                        TraceReference::Term(Term::Var(index), name) => {
                            Some(TraceReference::Term(Term::Var(index - 1), name))
                        }
                        reference => Some(reference),
                    }),
            );
        }
        // Relation membership contributes no rule or parameter dependency.
        // A comparison reads only the ids already in the binding context.
        JudgmentExpr::RelationMember { .. }
        | JudgmentExpr::RelationAtom { .. }
        | JudgmentExpr::SameEntity { .. } => {}
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

#[cfg(test)]
mod relation_resolution_tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    use serde_json::{Value, json};

    use super::Engine;
    use crate::model::{
        ComparisonOp, DType, DataSet, Derived, DerivedSemantics, Interval, JudgmentExpr, Period,
        Program, RelationDerivation, RelationSchema, ScalarExpr, ScalarValue, Term,
    };
    use crate::spec::{DatasetSpec, ProgramSpec};

    const HOUSEHOLDS: [&str; 2] = ["h1", "h2"];
    const PEOPLE: [&str; 3] = ["p1", "p2", "p3"];
    const RELATIONS: usize = 6;

    /// How a case spends its relations. Source chains grow long when sources
    /// and reads range over every later relation, and resolutions nest deep
    /// when they favour the next one, so no one shape covers both.
    #[derive(Clone, Copy)]
    struct Shape {
        /// Weight of a relation read against each other predicate leaf.
        read: u32,
        /// Weight of reading the next relation against any later one.
        next: u32,
        /// Weight of `base` against any relation, as a source and for a tuple.
        base: u32,
        /// Weight of a household rule reading `r0` against any relation.
        root: u32,
        /// One past the most tuples a dataset holds.
        tuples: usize,
        exists: bool,
        paths: bool,
    }

    const BROAD: Shape = Shape {
        read: 1,
        next: 0,
        base: 0,
        root: 0,
        tuples: 12,
        exists: false,
        paths: false,
    };
    const DEEP: Shape = Shape {
        read: 4,
        next: 3,
        base: 3,
        root: 2,
        tuples: 32,
        exists: false,
        paths: false,
    };
    const EXISTS: Shape = Shape {
        exists: true,
        ..DEEP
    };
    const PATHS: Shape = Shape {
        paths: true,
        ..EXISTS
    };

    fn literal(value: i64) -> Value {
        json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
    }

    fn person_rule(index: usize) -> Value {
        json!({"kind": "derived", "name": format!("p{index}")})
    }

    /// A predicate for derived relation `index`: it may read only relations
    /// after it and person rules, so programs stay acyclic.
    fn predicate(index: usize, shape: Shape) -> BoxedStrategy<Value> {
        let later = (index + 1..RELATIONS)
            .map(|later| format!("r{later}"))
            .collect::<Vec<_>>();
        let mut leaves = vec![
            (
                1,
                Just(json!({"kind": "comparison", "left": literal(1), "op": "eq", "right": literal(1)}))
                    .boxed(),
            ),
            (
                1,
                (0_usize..2)
                    .prop_map(|rule| {
                        json!({"kind": "comparison", "left": person_rule(rule), "op": "gte", "right": literal(10)})
                    })
                    .boxed(),
            ),
            // A parameter read and a division that can fail.
            (
                1,
                (0_i64..3)
                    .prop_map(|key| {
                        json!({"kind": "comparison",
                            "left": {"kind": "div", "left": {"kind": "parameter_lookup", "parameter": "rate", "index": literal(key)}, "right": {"kind": "input", "name": "income"}},
                            "op": "gt", "right": literal(0)})
                    })
                    .boxed(),
            ),
        ];
        if shape.exists {
            leaves.push((3, prop::bool::ANY.prop_map(|nested| {
                let read = json!({"kind": "comparison", "left": {
                    "kind": "role_derived", "term": {"var": 0}, "name": "p0"
                }, "op": "gte", "right": literal(10)});
                let body = if nested {
                    json!({"kind": "exists", "relation": "base", "terms": ["anchor", {"var": 0}], "variable_slot": 1,
                        "body": {"kind": "and", "items": [read, {"kind": "same_entity", "left": {"var": 0}, "right": {"var": 1}, "negated": false}]}})
                } else { read };
                json!({"kind": "exists", "relation": "base", "terms": ["anchor", {"var": 0}], "variable_slot": 1, "body": body})
            }).boxed()));
        }
        if let Some(next) = later.first().cloned() {
            let read = prop_oneof![shape.next => Just(next), 1 => prop::sample::select(later)];
            leaves.push((
                shape.read,
                (read.clone(), 0_usize..2)
                    .prop_map(|(relation, orientation)| {
                        json!({"kind": "relation_member", "relation": relation,
                            "current_slot": orientation, "related_slot": 1 - orientation})
                    })
                    .boxed(),
            ));
            if shape.exists {
                leaves.push((shape.read, read.clone().prop_map(|relation| {
                    json!({"kind": "exists", "relation": "base", "terms": ["anchor", {"var": 0}], "variable_slot": 1,
                        "body": {"kind": "relation_atom", "relation": relation, "terms": ["anchor", {"var": 0}]}})
                }).boxed()));
            }
            leaves.push((
                shape.read,
                read.prop_map(|relation| {
                        json!({"kind": "comparison",
                            "left": {"kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1},
                            "op": "gte", "right": literal(1)})
                    })
                    .boxed(),
            ));
        }
        prop::strategy::Union::new_weighted(leaves)
            .prop_recursive(2, 6, 3, |inner| {
                prop_oneof![
                    vec(inner.clone(), 1..3)
                        .prop_map(|items| json!({"kind": "and", "items": items})),
                    vec(inner.clone(), 1..3)
                        .prop_map(|items| json!({"kind": "or", "items": items})),
                    inner.prop_map(|item| json!({"kind": "not", "item": item})),
                ]
            })
            .boxed()
    }

    fn relation(index: usize, shape: Shape) -> BoxedStrategy<Value> {
        let sources = std::iter::once("base".to_string())
            .chain((index + 1..RELATIONS).map(|later| format!("r{later}")))
            .collect::<Vec<_>>();
        let source =
            prop_oneof![shape.base => Just("base".to_string()), 1 => prop::sample::select(sources)];
        (
            source.clone(),
            prop::sample::select(vec![(0_usize, 1_usize), (1, 0)]),
            prop::bool::ANY,
            predicate(index, shape),
            vec(source, 1..5),
        )
            .prop_map(
                move |(source, (current_slot, related_slot), typed, predicate, path)| {
                    let mut derivation = json!({
                        "source_relation": source, "current_slot": current_slot,
                        "related_slot": related_slot, "predicate": predicate,
                    });
                    if shape.paths {
                        derivation
                            .as_object_mut()
                            .unwrap()
                            .remove("source_relation");
                        derivation["source_path"] = json!(
                            path.into_iter()
                                .map(|relation| json!({
                                    "relation":relation, "from_slot":0, "to_slot":1
                                }))
                                .collect::<Vec<_>>()
                        );
                        derivation["current_slot"] = json!(0);
                        derivation["related_slot"] = json!(1);
                    }
                    if typed || shape.paths {
                        derivation["slot_entities"] = if shape.paths {
                            json!(["Person", "Person"])
                        } else {
                            json!(["Household", "Person"])
                        };
                    }
                    json!({"name": format!("r{index}"), "arity": 2, "derivation": derivation})
                },
            )
            .boxed()
    }

    fn household_rule(index: usize, shape: Shape) -> BoxedStrategy<Value> {
        let relation = prop_oneof![
            shape.root => Just("r0".to_string()),
            1 => (0..RELATIONS).prop_map(|relation| format!("r{relation}")),
        ];
        let where_clause = prop::option::of((0_usize..2).prop_map(|rule| {
            json!({"kind": "comparison", "left": person_rule(rule), "op": "lt", "right": literal(30)})
        }));
        prop_oneof![
            (relation.clone(), where_clause.clone()).prop_map(|(relation, where_clause)| {
                let mut expr = json!({"kind": "count_related", "relation": relation,
                    "current_slot": 0, "related_slot": 1});
                if let Some(where_clause) = where_clause {
                    expr["where"] = where_clause;
                }
                expr
            }),
            (relation, 0_usize..2, where_clause).prop_map(|(relation, rule, where_clause)| {
                let mut expr = json!({"kind": "sum_related", "relation": relation,
                    "current_slot": 0, "related_slot": 1,
                    "value": {"kind": "derived", "name": format!("p{rule}")}});
                if let Some(where_clause) = where_clause {
                    expr["where"] = where_clause;
                }
                expr
            }),
        ]
        .prop_map(move |expr| {
            json!({"name": format!("h{index}"), "entity": "Household", "dtype": "decimal",
                "unit": null, "semantics": "scalar", "expr": expr})
        })
        .boxed()
    }

    /// A program of person rules over one input, a data relation `base`,
    /// derived relations reading later ones through sources and predicates,
    /// and household aggregations over them; with tuples and incomes, some
    /// missing so predicates fail.
    fn case_of(shape: Shape) -> impl Strategy<Value = (ProgramSpec, DatasetSpec)> {
        let relations = (0..RELATIONS)
            .map(|index| relation(index, shape))
            .collect::<Vec<_>>();
        let rules = (0..3)
            .map(|index| household_rule(index, shape))
            .collect::<Vec<_>>();
        let tuples = vec(
            (
                prop_oneof![
                    shape.base => Just("base".to_string()),
                    1 => prop::sample::select(
                        std::iter::once("base".to_string())
                            .chain((0..RELATIONS).map(|index| format!("r{index}")))
                            .collect::<Vec<_>>(),
                    ),
                ],
                prop::sample::select([HOUSEHOLDS.as_slice(), PEOPLE.as_slice()].concat()),
                prop::sample::select(PEOPLE.to_vec()),
            ),
            0..shape.tuples,
        );
        let incomes = vec(prop::option::of(0_i64..40), PEOPLE.len());
        (relations, rules, tuples, incomes).prop_map(move |(relations, rules, tuples, incomes)| {
            let mut derived = vec![
                json!({"name": "p0", "entity": "Person", "dtype": "decimal", "unit": null,
                    "semantics": "scalar", "expr": {"kind": "input", "name": "income"}}),
                json!({"name": "p1", "entity": "Person", "dtype": "decimal", "unit": null,
                    "semantics": "scalar", "expr": {"kind": "add", "items": [person_rule(0), literal(5)]}}),
            ];
            derived.extend(rules.into_iter().map(|mut rule| {
                if shape.paths { rule["entity"] = json!("Person"); }
                rule
            }));
            let mut base = json!({"name": "base", "arity": 2});
            if shape.paths { base["slot_entities"] = json!(["Person", "Person"]); }
            let relations = std::iter::once(base)
                .chain(relations)
                .collect::<Vec<_>>();
            let program = json!({
                "relations": relations,
                "parameters": [{"name": "rate", "unit": null, "versions": [
                    {"effective_from": "2026-01-01", "values": {"0": {"kind": "integer", "value": 2}, "1": {"kind": "integer", "value": 0}}}
                ]}],
                "derived": derived,
            });
            let interval = json!({"start": "2026-01-01", "end": "2026-12-31"});
            let inputs = PEOPLE
                .iter()
                .zip(incomes)
                .filter_map(|(person, income)| {
                    income.map(|income| {
                        json!({"name": "income", "entity": "Person", "entity_id": person,
                            "interval": interval, "value": {"kind": "integer", "value": income}})
                    })
                })
                .collect::<Vec<_>>();
            let tuples = tuples
                .into_iter()
                .map(|(relation, left, right)| {
                    json!({"name": relation, "tuple": [left, right], "interval": interval})
                })
                .collect::<Vec<_>>();
            let dataset = json!({"inputs": inputs, "relations": tuples});
            (
                serde_json::from_value(program).expect("a valid program"),
                serde_json::from_value(dataset).expect("a valid dataset"),
            )
        })
    }

    /// Broad and deep cases in equal parts.
    fn case() -> impl Strategy<Value = (ProgramSpec, DatasetSpec)> {
        prop_oneof![2 => case_of(BROAD), 2 => case_of(DEEP), 1 => case_of(EXISTS), 2 => case_of(PATHS)]
    }

    fn evaluate_with_oracle(
        program: &ProgramSpec,
        dataset: &DatasetSpec,
        limit: usize,
        recursive: bool,
        suspend_depth: usize,
    ) -> Vec<String> {
        evaluate_counting(program, dataset, limit, recursive, suspend_depth).0
    }

    /// Every household output's value or error, and each household's trace,
    /// as an engine resolving at most `limit` relations natively gives them,
    /// with the number of times that limit stopped a resolution.
    fn evaluate_counting(
        program: &ProgramSpec,
        dataset: &DatasetSpec,
        limit: usize,
        recursive: bool,
        suspend_depth: usize,
    ) -> (Vec<String>, usize) {
        let (outcomes, stops, _, _, _) =
            evaluate_detailed(program, dataset, limit, recursive, suspend_depth);
        (outcomes, stops)
    }

    fn evaluate_detailed(
        program: &ProgramSpec,
        dataset: &DatasetSpec,
        limit: usize,
        recursive: bool,
        suspend_depth: usize,
    ) -> (
        Vec<String>,
        usize,
        super::ExistsCoverage,
        usize,
        super::PathCoverage,
    ) {
        let program = program.to_program().expect("the program lowers");
        // These are direct evaluator comparisons. Keep every generated
        // caller-slot orientation and direct tuple, including combinations
        // that strict dataset admission rejects for entity-kind mismatches.
        let dataset = dataset.to_dataset().expect("a valid raw dataset");
        evaluate_detailed_model(&program, &dataset, limit, recursive, suspend_depth)
    }

    fn evaluate_detailed_model(
        program: &Program,
        dataset: &DataSet,
        limit: usize,
        recursive: bool,
        suspend_depth: usize,
    ) -> (
        Vec<String>,
        usize,
        super::ExistsCoverage,
        usize,
        super::PathCoverage,
    ) {
        let period = Period::month(2026, 1);
        let mut engine = Engine::new(program, dataset).with_relation_nesting_limit(limit);
        engine.recursive_relation_oracle = recursive;
        engine.suspend_depth = suspend_depth;
        let mut outcomes = Vec::new();
        for household in HOUSEHOLDS {
            for rule in ["h0", "h1", "h2"] {
                let outcome = match program.derived[rule].dtype {
                    DType::Judgment => {
                        format!("{:?}", engine.evaluate_judgment(rule, household, &period))
                    }
                    _ => format!("{:?}", engine.evaluate_scalar(rule, household, &period)),
                };
                outcomes.push(format!("{household} {rule} {outcome}"));
            }
            let mut trace = engine
                .evaluated_trace_instances(&period, household)
                .iter()
                .map(|instance| format!("{instance:?}"))
                .collect::<Vec<_>>();
            trace.sort();
            outcomes.extend(trace);
        }
        (
            outcomes,
            engine.relation_suspensions,
            engine.exists_coverage,
            engine.maximum_depth,
            engine.path_coverage,
        )
    }

    #[test]
    fn suspended_resolution_matches_native_resolution() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 3000,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[23; 32]),
        );
        let nested = std::cell::Cell::new(0_usize);
        let stopped = std::cell::RefCell::new([0_usize; RELATIONS + 1]);
        let paths = std::cell::RefCell::new(super::PathCoverage::default());
        runner
            .run(&case(), |(program, dataset)| {
                // Convert once per case; every limit/depth still gets a fresh
                // interpreter and the same independent recursive oracle.
                let model = program.to_program().expect("the program lowers");
                let data = dataset.to_dataset().expect("a valid raw dataset");
                // Relations read only later ones, so the deepest request sits
                // at depth RELATIONS - 1. That bounds the limits that can stop
                // a resolution, and no limit can stop this reference.
                let (native, _, _, maximum_depth, _) = evaluate_detailed_model(&model, &data, usize::MAX, true, usize::MAX);
                for limit in 1..=RELATIONS {
                    let mut stopped_here = false;
                    for suspend_depth in 1..=maximum_depth {
                        let (suspended, stops, _, _, coverage) = evaluate_detailed_model(&model, &data, limit, false, suspend_depth);
                        if native != suspended {
                            return Err(TestCaseError::fail(format!(
                                "relation nesting {limit}, rule suspend depth {suspend_depth}\nnative {native:#?}\nsuspended {suspended:#?}\nprogram {}",
                                serde_json::to_string(&program).expect("serializes")
                            )));
                        }
                        stopped_here |= stops > 0;
                        // These counters come from successful resumed lookups
                        // and completed candidate generations in the evaluator.
                        let mut paths = paths.borrow_mut();
                        paths.resumed += coverage.resumed;
                        paths.empty += coverage.empty;
                        paths.nonempty += coverage.nonempty;
                        paths.long += coverage.long;
                        for (total, count) in paths.derived_positions.iter_mut().zip(coverage.derived_positions) {
                            *total += count;
                        }
                    }
                    stopped.borrow_mut()[limit] += usize::from(stopped_here);
                }
                if native.iter().any(|line| line.contains("Ok(")) {
                    nested.set(nested.get() + 1);
                }
                Ok(())
            })
            .unwrap();
        assert!(nested.get() > 500, "only {} cases evaluated", nested.get());
        let paths = paths.into_inner();
        assert!(
            paths.resumed >= 25,
            "resumed candidate generation: {paths:?}"
        );
        assert!(
            paths.empty >= 100 && paths.nonempty >= 25,
            "empty/nonempty paths: {paths:?}"
        );
        assert!(paths.long >= 100, "three-or-more steps: {paths:?}");
        assert!(
            paths.derived_positions.iter().all(|count| *count >= 10),
            "derived step positions: {paths:?}"
        );
        eprintln!(
            "PATH COVERAGE {paths:?}; floors resumed25 empty100 nonempty25 long100 each-position10"
        );
        let stopped = stopped.into_inner();
        for limit in 1..RELATIONS {
            let cases = stopped[limit];
            assert!(cases >= 10, "limit {limit} stopped only {cases} cases");
        }
        assert_eq!(stopped[RELATIONS], 0, "a limit past the deepest request");
        eprintln!(
            "COVERAGE stopped per limit: {stopped:?} nested {}",
            nested.get()
        );
    }

    /// The old recursive resolver is an independent oracle: source traversal,
    /// predicate evaluation and direct tuples follow their original order,
    /// without the derived-member cache or effect replay under test.
    #[test]
    fn iterative_resolution_matches_recursive_semantics_on_small_dags() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 3000,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[41; 32]),
        );
        runner
            .run(&case(), |(program, dataset)| {
                let recursive = evaluate_with_oracle(&program, &dataset, 32, true, usize::MAX);
                for (limit, suspend_depth) in [(32, usize::MAX), (1, usize::MAX), (1, 1), (32, 2)] {
                    let iterative =
                        evaluate_with_oracle(&program, &dataset, limit, false, suspend_depth);
                    prop_assert_eq!(
                        &iterative,
                        &recursive,
                        "relation nesting {}, rule deferral {}, program {}",
                        limit,
                        suspend_depth,
                        serde_json::to_string(&program).expect("serializes")
                    );
                }
                Ok(())
            })
            .expect("iterative results, errors and ordered trace records match recursion");
    }

    const IDS: [&str; 3] = ["a", "b", "c"];

    fn typed_relation(
        program: &mut Program,
        name: &str,
        arity: usize,
        derivation: Option<RelationDerivation>,
    ) {
        program.relations.insert(
            name.to_string(),
            RelationSchema {
                name: name.to_string(),
                arity,
                slot_entities: vec!["Node".to_string(); arity],
                derivation,
            },
        );
    }

    fn derivation(source: &str, predicate: JudgmentExpr) -> RelationDerivation {
        RelationDerivation {
            source_relation: Some(source.to_string()),
            source_path: None,
            current_slot: 0,
            related_slot: 1,
            entity: None,
            member_relation: None,
            slot_entities: vec!["Node".to_string(), "Node".to_string()],
            predicate,
        }
    }

    fn score(term: Term, threshold: i64) -> JudgmentExpr {
        JudgmentExpr::Comparison {
            left: ScalarExpr::RoleDerived {
                term,
                name: "score".to_string(),
            },
            op: ComparisonOp::Gte,
            right: ScalarExpr::Literal(ScalarValue::Integer(threshold)),
        }
    }

    fn combination(term: Term) -> JudgmentExpr {
        JudgmentExpr::RelationAtom {
            relation: "required_combination".to_string(),
            terms: vec![term, Term::Candidate],
        }
    }

    /// The expansion oracle computes tuple domains from the raw records, with
    /// its own BTreeSet, then builds an ordinary disjunction with literal-id
    /// terms. `Id` is cfg(test), and neither serialized enum has such a kind.
    fn exists_case() -> impl Strategy<Value = (Program, Program, DataSet)> {
        (
            prop::bool::ANY,
            prop::bool::ANY,
            0_usize..3,
            0_usize..3,
            vec((0_usize..3, 0_usize..3, 0_usize..3, prop::bool::ANY), 0..24),
            vec((0_usize..3, 0_usize..3), 0..14),
            vec(prop::option::of(-2_i64..4), 3),
            -1_i64..3,
        )
            .prop_map(
                |(derived, nested, variable_slot, candidate, tuples, pairs, scores, threshold)| {
                    let period = Period::month(2026, 1);
                    let interval = Interval::covering(&period);
                    let inactive = Interval::covering(&Period::month(2026, 2));
                    let mut program = Program::default();
                    let mut data = DataSet::default();
                    for (name, arity) in [
                        ("roster", 2),
                        ("domain_data", if derived { 2 } else { 3 }),
                        ("pairs", 2),
                        ("inner", 2),
                    ] {
                        typed_relation(&mut program, name, arity, None);
                    }
                    typed_relation(
                        &mut program,
                        "required_combination",
                        2,
                        Some(derivation("pairs", holds())),
                    );
                    if derived {
                        typed_relation(
                            &mut program,
                            "generated",
                            2,
                            Some(derivation(
                                "domain_data",
                                JudgmentExpr::SameEntity {
                                    left: Term::Anchor,
                                    right: Term::Candidate,
                                    negated: true,
                                },
                            )),
                        );
                    }
                    data.add_relation(
                        "roster",
                        vec!["a".to_string(), IDS[candidate].to_string()],
                        interval.clone(),
                    );
                    let mut domain = std::collections::BTreeSet::new();
                    let mut terms = vec![Term::Anchor, Term::Candidate, Term::Candidate];
                    let generator;
                    let variable_slot = if derived {
                        generator = "generated";
                        terms = vec![Term::Anchor, Term::Var(0)];
                        1
                    } else {
                        generator = "domain_data";
                        let mut bound = [Term::Anchor, Term::Candidate].into_iter();
                        for (slot, term) in terms.iter_mut().enumerate() {
                            *term = if slot == variable_slot {
                                Term::Var(0)
                            } else {
                                bound.next().unwrap()
                            };
                        }
                        variable_slot
                    };
                    for (a, b, c, active) in tuples {
                        let tuple = if derived {
                            vec![IDS[a].to_string(), IDS[b].to_string()]
                        } else {
                            vec![IDS[a].to_string(), IDS[b].to_string(), IDS[c].to_string()]
                        };
                        if active {
                            let matches = if derived {
                                a == 0 && b != 0
                            } else {
                                terms.iter().enumerate().all(|(slot, term)| {
                                    slot == variable_slot
                                        || tuple[slot]
                                            == match term {
                                                Term::Anchor => "a",
                                                Term::Candidate => IDS[candidate],
                                                Term::Var(_) | Term::Id(_) => unreachable!(),
                                            }
                                })
                            };
                            if matches {
                                let id = if derived {
                                    IDS[b]
                                } else {
                                    [IDS[a], IDS[b], IDS[c]][variable_slot]
                                };
                                domain.insert(id);
                            }
                        }
                        data.add_relation(
                            "domain_data",
                            tuple,
                            if active {
                                interval.clone()
                            } else {
                                inactive.clone()
                            },
                        );
                    }
                    for &(left, right) in &pairs {
                        for name in ["pairs", "inner"] {
                            data.add_relation(
                                name,
                                vec![IDS[left].to_string(), IDS[right].to_string()],
                                interval.clone(),
                            );
                        }
                    }
                    for (id, value) in IDS.into_iter().zip(scores) {
                        if let Some(value) = value {
                            data.add_input(
                                "score",
                                "Node",
                                id,
                                interval.clone(),
                                ScalarValue::Integer(value),
                            );
                        }
                    }
                    program.derived.insert(
                        "score".to_string(),
                        rule("score", ScalarExpr::Input("score".to_string())),
                    );
                    program.derived.insert(
                        "head".to_string(),
                        rule("head", count("selected".to_string())),
                    );
                    let body = if nested {
                        JudgmentExpr::Exists {
                            relation: "inner".to_string(),
                            terms: vec![Term::Var(0), Term::Var(0)],
                            variable_slot: 1,
                            body: Box::new(JudgmentExpr::And(vec![
                                JudgmentExpr::SameEntity {
                                    left: Term::Var(1),
                                    right: Term::Var(0),
                                    negated: true,
                                },
                                score(Term::Var(0), threshold),
                                combination(Term::Var(1)),
                            ])),
                        }
                    } else {
                        JudgmentExpr::And(vec![
                            score(Term::Var(0), threshold),
                            combination(Term::Var(0)),
                        ])
                    };
                    typed_relation(
                        &mut program,
                        "selected",
                        2,
                        Some(derivation(
                            "roster",
                            JudgmentExpr::Exists {
                                relation: generator.to_string(),
                                terms,
                                variable_slot,
                                body: Box::new(body),
                            },
                        )),
                    );
                    let mut expanded = program.clone();
                    let items = domain
                        .into_iter()
                        .map(|id| {
                            if nested {
                                let inner = pairs
                                    .iter()
                                    .filter(|(left, _)| IDS[*left] == id)
                                    .map(|(_, right)| IDS[*right])
                                    .collect::<std::collections::BTreeSet<_>>();
                                JudgmentExpr::Or(
                                    inner
                                        .into_iter()
                                        .map(|other| {
                                            JudgmentExpr::And(vec![
                                                JudgmentExpr::SameEntity {
                                                    left: Term::Id(id),
                                                    right: Term::Id(other),
                                                    negated: true,
                                                },
                                                score(Term::Id(other), threshold),
                                                combination(Term::Id(id)),
                                            ])
                                        })
                                        .collect(),
                                )
                            } else {
                                JudgmentExpr::And(vec![
                                    score(Term::Id(id), threshold),
                                    combination(Term::Id(id)),
                                ])
                            }
                        })
                        .collect();
                    expanded
                        .relations
                        .get_mut("selected")
                        .unwrap()
                        .derivation
                        .as_mut()
                        .unwrap()
                        .predicate = JudgmentExpr::Or(items);
                    (program, expanded, data)
                },
            )
    }

    fn model_outcome(
        program: &Program,
        data: &DataSet,
        limit: usize,
        depth: usize,
    ) -> (Result<String, String>, Vec<String>, super::ExistsCoverage) {
        let period = Period::month(2026, 1);
        let mut engine = Engine::new(program, data).with_relation_nesting_limit(limit);
        engine.suspend_depth = depth;
        let result = engine
            .evaluate_scalar("head", "a", &period)
            .map(|value| {
                serde_json::to_string(&crate::spec::ScalarValueSpec::from_model(value)).unwrap()
            })
            .map_err(|error| error.to_string());
        let mut trace = engine
            .evaluated_trace_instances(&period, "a")
            .iter()
            .map(|instance| format!("{instance:?}"))
            .collect::<Vec<_>>();
        trace.sort();
        (result, trace, engine.exists_coverage)
    }

    fn fast_model_outcome(program: &Program, data: &DataSet) -> Result<String, String> {
        let query = crate::api::ExecutionQuery {
            entity_id: "a".to_string(),
            outputs: vec!["head".to_string()],
            assessment_date: None,
            period: crate::spec::PeriodSpec {
                kind: crate::spec::PeriodKindSpec::Month,
                start: Period::month(2026, 1).start,
                end: Period::month(2026, 1).end,
            },
        };
        match crate::bulk::try_execute(program, data, &[query])
            .map_err(|error| error.to_string())?
        {
            crate::bulk::FastPathResult::Executed(response) => {
                assert_eq!(
                    response.metadata.actual_mode,
                    crate::api::ExecutionMode::Fast
                );
                assert_eq!(response.results.len(), 1, "bulk executed the queried row");
                match &response.results[0].outputs["head"] {
                    crate::api::OutputValue::Scalar { value, .. } => {
                        Ok(serde_json::to_string(value).unwrap())
                    }
                    _ => panic!("a scalar head"),
                }
            }
            crate::bulk::FastPathResult::Unsupported { reason } => {
                // Public Fast falls back to Explain to choose the first
                // evaluation failure. Literal-id models cannot go through
                // serialization, so reproduce that routing on this model.
                let explained = model_outcome(program, data, usize::MAX, usize::MAX).0;
                let Err(error) = explained else {
                    panic!("successful exists must execute in bulk: {reason}");
                };
                assert_eq!(
                    reason,
                    format!("bulk evaluation failed ({error}); explain decides the outcome"),
                    "fallback must be the evaluation failure, not an unsupported construct"
                );
                Err(error)
            }
        }
    }

    fn role_judgment(term: Term, name: &str) -> JudgmentExpr {
        JudgmentExpr::RoleDerived {
            term,
            name: name.to_string(),
        }
    }

    fn quantified(body: JudgmentExpr) -> JudgmentExpr {
        JudgmentExpr::Exists {
            relation: "domain".to_string(),
            terms: vec![Term::Anchor, Term::Var(0)],
            variable_slot: 1,
            body: Box::new(body),
        }
    }

    fn trace_probe(node: &JudgmentExpr, skipped: bool) -> (Vec<String>, Vec<String>) {
        let mut program = Program::default();
        typed_relation(&mut program, "domain", 2, None);
        let mut eligible = rule("eligible", ScalarExpr::Literal(ScalarValue::Integer(1)));
        eligible.dtype = DType::Judgment;
        eligible.semantics = DerivedSemantics::Judgment(holds());
        program.derived.insert("eligible".to_string(), eligible);
        let period = Period::month(2026, 1);
        let mut data = DataSet::default();
        for id in ["c", "a", "b", "a"] {
            data.add_relation(
                "domain",
                vec!["root".to_string(), id.to_string()],
                Interval::covering(&period),
            );
        }
        let mut engine = Engine::new(&program, &data);
        engine.suspend_depth = usize::MAX;
        let parent = super::CacheKey {
            derived: "parent".to_string(),
            entity_id: "root".to_string(),
            period: period.clone(),
        };
        engine.active_evaluations.push(parent.clone());
        let context = super::RelationEvalContext {
            current_id: "root",
            related_id: "candidate",
            current_entity: None,
            related_entity: None,
            variables: None,
        };
        if skipped {
            engine.record_skipped_judgment_dependencies(
                node,
                "candidate",
                &period,
                Some(context),
                super::TraceSkipReason::ShortCircuit,
            );
        } else {
            engine
                .drive(|engine| {
                    engine.eval_judgment_expr_inner(node, "candidate", &period, Some(context))
                })
                .unwrap();
        }
        engine.active_evaluations.pop();
        let trace = engine
            .execution_trace
            .get(&parent)
            .cloned()
            .unwrap_or_default();
        let evaluated = trace
            .dependencies
            .iter()
            .map(|key| format!("{}:{}", key.derived, key.entity_id))
            .collect();
        let skipped = trace
            .not_evaluated_dependencies
            .iter()
            .filter_map(|dependency| match dependency {
                super::SkippedTraceDependency::Derived { key, .. } => {
                    Some(format!("{}:{}", key.derived, key.entity_id))
                }
                super::SkippedTraceDependency::Parameter { .. } => None,
            })
            .collect();
        (evaluated, skipped)
    }

    #[test]
    fn exists_skipped_reads_use_evaluated_outer_bindings() {
        let node = quantified(JudgmentExpr::And(vec![
            JudgmentExpr::Not(Box::new(holds())),
            role_judgment(Term::Var(0), "eligible"),
        ]));
        assert_eq!(
            trace_probe(&node, false),
            (
                vec![],
                vec![
                    "eligible:a".to_string(),
                    "eligible:b".to_string(),
                    "eligible:c".to_string()
                ]
            )
        );
    }

    #[test]
    fn exists_unevaluated_inner_bindings_produce_no_skip_records() {
        let node = quantified(JudgmentExpr::And(vec![
            JudgmentExpr::Not(Box::new(holds())),
            quantified(role_judgment(Term::Var(0), "eligible")),
        ]));
        assert_eq!(trace_probe(&node, false), (vec![], vec![]));
    }

    #[test]
    fn exists_wholly_skipped_records_anchor_and_candidate_once() {
        let node = quantified(JudgmentExpr::And(vec![
            role_judgment(Term::Anchor, "eligible"),
            role_judgment(Term::Candidate, "eligible"),
            role_judgment(Term::Var(0), "eligible"),
            quantified(role_judgment(Term::Anchor, "eligible")),
        ]));
        assert_eq!(
            trace_probe(&node, true),
            (
                vec![],
                vec![
                    "eligible:root".to_string(),
                    "eligible:candidate".to_string()
                ]
            )
        );
    }

    #[test]
    fn exists_skipped_nested_frames_preserve_enclosing_variable_indices() {
        let node = quantified(JudgmentExpr::And(vec![
            JudgmentExpr::Not(Box::new(holds())),
            quantified(quantified(JudgmentExpr::And(vec![
                role_judgment(Term::Var(0), "inner"),
                role_judgment(Term::Var(1), "middle"),
                role_judgment(Term::Var(2), "outer"),
            ]))),
        ]));
        assert_eq!(
            trace_probe(&node, false),
            (
                vec![],
                vec![
                    "outer:a".to_string(),
                    "outer:b".to_string(),
                    "outer:c".to_string()
                ]
            )
        );
    }

    #[test]
    fn exists_early_stop_records_each_remaining_sorted_distinct_id() {
        let node = quantified(role_judgment(Term::Var(0), "eligible"));
        assert_eq!(
            trace_probe(&node, false),
            (
                vec!["eligible:a".to_string()],
                vec!["eligible:b".to_string(), "eligible:c".to_string()]
            )
        );
    }

    #[test]
    fn exists_resume_key_separates_every_binding_period_node_and_parent() {
        let period = Period::month(2026, 1);
        let february = Period::month(2026, 2);
        let mut program = Program::default();
        typed_relation(&mut program, "domain", 5, None);
        typed_relation(&mut program, "pairs", 2, None);
        typed_relation(
            &mut program,
            "required_combination",
            2,
            Some(derivation("pairs", holds())),
        );
        program.derived.insert(
            "score".to_string(),
            rule("score", ScalarExpr::Input("score".to_string())),
        );
        let mut data = DataSet::default();
        data.add_relation(
            "domain",
            ["a", "b", "x", "y", "z"].map(str::to_string).to_vec(),
            Interval::covering(&period),
        );
        data.add_input(
            "score",
            "Node",
            "z",
            Interval::covering(&period),
            ScalarValue::Integer(1),
        );
        data.add_relation(
            "pairs",
            vec!["z".to_string(), "b".to_string()],
            Interval::covering(&period),
        );
        let node = JudgmentExpr::Exists {
            relation: "domain".to_string(),
            terms: vec![
                Term::Anchor,
                Term::Candidate,
                Term::Var(0),
                Term::Var(1),
                Term::Var(0),
            ],
            variable_slot: 4,
            body: Box::new(JudgmentExpr::And(vec![
                score(Term::Var(0), 1),
                combination(Term::Var(0)),
            ])),
        };
        let mut other_node = node.clone();
        if let JudgmentExpr::Exists { body, .. } = &mut other_node {
            *body = Box::new(JudgmentExpr::Not(body.clone()));
        }
        // Parent evaluation stays in January even when the expression reads
        // February, isolating ExistsKey.period from CacheKey.period.
        let parent_period = period.clone();
        for limit in 1..=2 {
            for depth in 1..=8 {
                let mut engine = Engine::new(&program, &data).with_relation_nesting_limit(limit);
                engine.suspend_depth = depth;
                let relation_stops = std::cell::Cell::new(0_usize);
                let resumed_after_relation_stop = std::cell::Cell::new(0_usize);
                let run = engine
                    .drive(|engine| {
                        let mut outcomes = Vec::new();
                        for (site, anchor, candidate, inner, outer, period, parent) in [
                            (&node, "a", "b", "x", "y", &period, "first"),
                            (&node, "other", "b", "x", "y", &period, "first"),
                            (&node, "a", "other", "x", "y", &period, "first"),
                            (&node, "a", "b", "other", "y", &period, "first"),
                            (&node, "a", "b", "x", "other", &period, "first"),
                            (&node, "a", "b", "x", "y", &february, "first"),
                            (&other_node, "a", "b", "x", "y", &period, "first"),
                            (&node, "a", "b", "x", "y", &period, "second"),
                        ] {
                            let outer = super::VariableFrame {
                                id: outer,
                                parent: None,
                            };
                            let inner = super::VariableFrame {
                                id: inner,
                                parent: Some(&outer),
                            };
                            let context = super::RelationEvalContext {
                                current_id: anchor,
                                related_id: candidate,
                                current_entity: None,
                                related_entity: None,
                                variables: Some(&inner),
                            };
                            engine.active_evaluations.push(super::CacheKey {
                                derived: parent.to_string(),
                                entity_id: "root".to_string(),
                                period: parent_period.clone(),
                            });
                            // Simulate the enclosing relation resolution that
                            // normally calls a predicate. Keep owner=None so
                            // the owner key cannot hide an omitted anchor or
                            // period from this key-dimension matrix. The parent
                            // period above is also fixed for that same reason.
                            let mut resumed_before_retry = None;
                            let outcome = loop {
                                assert_eq!(engine.relation_depth, 0);
                                engine.relation_depth = 1;
                                let outcome = engine.exists_judgment(site, period, Some(context));
                                engine.relation_depth = 0;
                                let Some(needed) = engine.suspended_on.take() else {
                                    break outcome;
                                };
                                // This is a real bound hit in derived_members.
                                // The ordinary outer resolver consumes it by
                                // resolving the needed relation, then retries
                                // the saved predicate. Reproduce that handoff
                                // here and count only completed resolutions.
                                match engine.resolve_members(needed) {
                                    Ok(()) => {
                                        relation_stops.set(relation_stops.get() + 1);
                                        // Snapshot after the handoff: a rule
                                        // resume before this relation stop
                                        // cannot satisfy the resumed floor.
                                        resumed_before_retry = Some(engine.exists_coverage.resumed);
                                    }
                                    Err(interrupt) => break Err(interrupt),
                                }
                            };
                            if let (Some(resumed_before), true) =
                                (resumed_before_retry, outcome.is_ok())
                            {
                                assert!(
                                    engine.exists_coverage.resumed > resumed_before,
                                    "the relation stop must resume the bound exists body"
                                );
                                resumed_after_relation_stop
                                    .set(resumed_after_relation_stop.get() + 1);
                            }
                            engine.active_evaluations.pop();
                            outcomes.push(outcome?);
                        }
                        Ok(outcomes)
                    })
                    .unwrap();
                assert_eq!(
                    run,
                    vec![
                        crate::model::JudgmentOutcome::Holds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::NotHolds,
                        crate::model::JudgmentOutcome::Holds
                    ],
                    "relation limit {limit}, depth {depth}"
                );
                if limit == 1 {
                    assert!(
                        relation_stops.get() >= 1,
                        "relation limit {limit}, depth {depth} must stop a resolution"
                    );
                    assert!(
                        resumed_after_relation_stop.get() >= 1,
                        "relation limit {limit}, depth {depth} must resume exists after that stop"
                    );
                } else {
                    assert_eq!(
                        relation_stops.get(),
                        0,
                        "limit {limit} is past this matrix's one enclosing resolution"
                    );
                }
                for parent in ["first", "second"] {
                    let trace = &engine.execution_trace[&super::CacheKey {
                        derived: parent.to_string(),
                        entity_id: "root".to_string(),
                        period: parent_period.clone(),
                    }];
                    assert!(
                        trace
                            .dependencies
                            .iter()
                            .any(|dependency| dependency.derived == "score"
                                && dependency.entity_id == "z"),
                        "{parent} owns its effects at depth {depth}"
                    );
                }
            }
        }
    }

    #[test]
    fn exists_expands_and_defers_with_observed_paths() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 256,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[71; 32]),
        );
        let coverage = std::cell::Cell::new([0_usize; 5]);
        let failures = std::cell::Cell::new(0_usize);
        let bulk_successes = std::cell::Cell::new(0_usize);
        let independently_compared_traces = std::cell::Cell::new(0_usize);
        runner
            .run(&exists_case(), |(program, expanded, data)| {
                let (oracle, expanded_trace, _) = model_outcome(&expanded, &data, usize::MAX, usize::MAX);
                let (native, native_trace, native_counts) =
                    model_outcome(&program, &data, usize::MAX, usize::MAX);
                prop_assert_eq!(&native, &oracle, "independent sorted-domain disjunction");
                let flat_body = matches!(
                    &program.relations["selected"].derivation.as_ref().unwrap().predicate,
                    JudgmentExpr::Exists { body, .. } if matches!(body.as_ref(), JudgmentExpr::And(_))
                );
                if flat_body {
                    // A flat body's literal-id expansion has the same skip
                    // records, including early stops and empty domains. A
                    // wholly skipped nested binder deliberately differs (§5).
                    prop_assert_eq!(&native_trace, &expanded_trace, "independent flat expansion trace");
                    independently_compared_traces.set(independently_compared_traces.get() + 1);
                }

                let (repeated, repeated_trace, _) =
                    model_outcome(&program, &data, usize::MAX, usize::MAX);
                prop_assert_eq!(
                    &repeated,
                    &oracle,
                    "deterministic value against independent oracle"
                );
                prop_assert_eq!(&repeated_trace, &native_trace, "deterministic trace");
                let mut permuted = data.clone();
                permuted.inputs.reverse();
                permuted.relations.reverse();
                let (reordered, reordered_trace, _) =
                    model_outcome(&program, &permuted, usize::MAX, usize::MAX);
                prop_assert_eq!(
                    &reordered,
                    &oracle,
                    "input, tuple and roster order against independent oracle"
                );
                prop_assert_eq!(
                    &reordered_trace,
                    &native_trace,
                    "input, tuple and roster trace order"
                );
                prop_assert_eq!(
                    fast_model_outcome(&program, &permuted),
                    oracle.clone(),
                    "fast order against independent oracle"
                );

                let fast = fast_model_outcome(&program, &data);
                // A successful helper result requires an actual Executed
                // bulk response; success through fallback panics above.
                bulk_successes.set(bulk_successes.get() + usize::from(fast.is_ok()));
                prop_assert_eq!(fast, oracle.clone(), "fast finite quantifier");
                prop_assert_eq!(
                    fast_model_outcome(&expanded, &data),
                    oracle.clone(),
                    "fast literal-id expansion"
                );
                failures.set(failures.get() + usize::from(oracle.is_err()));
                let mut observed = [
                    native_counts.resumed > 0,
                    native_counts.early_stop > 0,
                    native_counts.empty > 0,
                    native_counts.nonempty > 0,
                    native_counts.nested > 0,
                ];
                // The deepest derived resolution here is selected -> generated or
                // selected -> required_combination. Expression paths contain at
                // most twelve levels; greater depths cannot defer another rule.
                for limit in 1..=2 {
                    for depth in 1..=12 {
                        let (outcome, trace, counts) = model_outcome(&program, &data, limit, depth);
                        prop_assert_eq!(&outcome, &oracle, "limit {}, depth {}", limit, depth);
                        prop_assert_eq!(
                            &trace,
                            &native_trace,
                            "trace limit {}, depth {}",
                            limit,
                            depth
                        );
                        for (seen, count) in observed.iter_mut().zip([
                            counts.resumed,
                            counts.early_stop,
                            counts.empty,
                            counts.nonempty,
                            counts.nested,
                        ]) {
                            *seen |= count > 0;
                        }
                    }
                }
                let mut counts = coverage.get();
                for (count, observed) in counts.iter_mut().zip(observed) {
                    *count += usize::from(observed);
                }
                coverage.set(counts);
                Ok(())
            })
            .unwrap();
        let counts = coverage.get();
        for (name, count, floor) in [
            ("resumed exists", counts[0], 20),
            ("early stop", counts[1], 2),
            ("empty domain", counts[2], 20),
            ("nonempty domain", counts[3], 100),
            ("nested frame", counts[4], 20),
        ] {
            assert!(count >= floor, "{name}: {count} cases, floor {floor}");
        }
        assert!(
            failures.get() >= 10,
            "missing-input failures must reach the oracle"
        );
        assert!(
            bulk_successes.get() >= 100,
            "actual successful bulk cases: {}",
            bulk_successes.get()
        );
        assert!(
            independently_compared_traces.get() >= 100,
            "actual independent flat trace comparisons: {}",
            independently_compared_traces.get()
        );
        eprintln!(
            "EXISTS COVERAGE cases [resumed, early_stop, empty, nonempty, nested] = {counts:?}; failures {}; bulk successes {}; independent flat traces {}",
            failures.get(),
            bulk_successes.get(),
            independently_compared_traces.get()
        );
    }

    /// The positive fragment excludes derived relation reads, aggregating
    /// rule reads, count comparisons and bare membership judgments. Counting
    /// selected members below only observes the predicate's exact value.
    #[test]
    fn exists_positive_base_domains_are_monotone_with_observed_paths() {
        let strategy = (
            prop::bool::ANY,
            0_usize..3,
            0_usize..3,
            vec(0_usize..3, 0..8),
            vec(0_usize..3, 0..8),
            vec((0_usize..3, 0_usize..3), 0..12),
        );
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 256,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[83; 32]),
        );
        let coverage = std::cell::Cell::new([0_usize; 4]);
        runner
            .run(
                &strategy,
                |(nested, candidate, even_nots, before, added, edges)| {
                    let mut program = Program::default();
                    for name in ["domain", "roster", "inner"] {
                        typed_relation(&mut program, name, 2, None);
                    }
                    let body = if nested {
                        JudgmentExpr::Exists {
                            relation: "inner".to_string(),
                            terms: vec![Term::Var(0), Term::Var(0)],
                            variable_slot: 1,
                            body: Box::new(JudgmentExpr::SameEntity {
                                left: Term::Var(0),
                                right: Term::Candidate,
                                negated: false,
                            }),
                        }
                    } else {
                        JudgmentExpr::SameEntity {
                            left: Term::Var(0),
                            right: Term::Candidate,
                            negated: false,
                        }
                    };
                    let mut predicate = quantified(body);
                    for _ in 0..even_nots {
                        predicate =
                            JudgmentExpr::Not(Box::new(JudgmentExpr::Not(Box::new(predicate))));
                    }
                    typed_relation(
                        &mut program,
                        "selected",
                        2,
                        Some(derivation("roster", predicate)),
                    );
                    program.derived.insert(
                        "head".to_string(),
                        rule("head", count("selected".to_string())),
                    );
                    let period = Period::month(2026, 1);
                    let interval = Interval::covering(&period);
                    let mut data = DataSet::default();
                    data.add_relation(
                        "roster",
                        vec!["a".to_string(), IDS[candidate].to_string()],
                        interval.clone(),
                    );
                    for (left, right) in &edges {
                        data.add_relation(
                            "inner",
                            vec![IDS[*left].to_string(), IDS[*right].to_string()],
                            interval.clone(),
                        );
                    }
                    for id in &before {
                        data.add_relation(
                            "domain",
                            vec!["a".to_string(), IDS[*id].to_string()],
                            interval.clone(),
                        );
                    }
                    let mut extended = data.clone();
                    for id in &added {
                        extended.add_relation(
                            "domain",
                            vec!["a".to_string(), IDS[*id].to_string()],
                            interval.clone(),
                        );
                    }
                    let expected = |domain: &[usize]| {
                        domain.iter().any(|id| {
                            if nested {
                                edges
                                    .iter()
                                    .any(|(left, right)| left == id && *right == candidate)
                            } else {
                                *id == candidate
                            }
                        })
                    };
                    let before_expected = expected(&before);
                    let all = before.iter().chain(&added).copied().collect::<Vec<_>>();
                    let after_expected = expected(&all);
                    prop_assert!(!before_expected || after_expected);
                    let mut observed = [false; 4];
                    for (records, expected) in
                        [(&data, before_expected), (&extended, after_expected)]
                    {
                        let oracle = Ok(serde_json::to_string(
                            &crate::spec::ScalarValueSpec::from_model(ScalarValue::Integer(
                                i64::from(expected),
                            )),
                        )
                        .unwrap());
                        let (value, _, counts) =
                            model_outcome(&program, records, usize::MAX, usize::MAX);
                        prop_assert_eq!(
                            &value,
                            &oracle,
                            "exact membership under positive tuple extension"
                        );
                        prop_assert_eq!(
                            fast_model_outcome(&program, records),
                            oracle,
                            "fast exact positive value"
                        );
                        for (seen, count) in observed.iter_mut().zip([
                            counts.early_stop,
                            counts.empty,
                            counts.nonempty,
                            counts.nested,
                        ]) {
                            *seen |= count > 0;
                        }
                    }
                    let mut counts = coverage.get();
                    for (count, seen) in counts.iter_mut().zip(observed) {
                        *count += usize::from(seen);
                    }
                    coverage.set(counts);
                    Ok(())
                },
            )
            .unwrap();
        let counts = coverage.get();
        for (name, count, floor) in [
            ("early stop", counts[0], 20),
            ("empty", counts[1], 20),
            ("nonempty", counts[2], 150),
            ("nested", counts[3], 50),
        ] {
            assert!(
                count >= floor,
                "monotone {name}: {count} cases, floor {floor}"
            );
        }
        eprintln!(
            "EXISTS MONOTONE COVERAGE cases [early_stop, empty, nonempty, nested] = {counts:?}"
        );
    }

    #[derive(Clone, Copy)]
    enum DeepLink {
        Source,
        Member,
        Counted,
    }

    fn count(relation: String) -> ScalarExpr {
        ScalarExpr::CountRelated {
            relation,
            current_slot: 0,
            related_slot: 1,
            where_clause: None,
        }
    }

    fn holds() -> JudgmentExpr {
        JudgmentExpr::Comparison {
            left: ScalarExpr::Literal(ScalarValue::Integer(1)),
            op: ComparisonOp::Eq,
            right: ScalarExpr::Literal(ScalarValue::Integer(1)),
        }
    }

    fn rule(name: &str, expr: ScalarExpr) -> Derived {
        Derived {
            id: None,
            name: name.to_string(),
            entity: "Node".to_string(),
            dtype: DType::Integer,
            unit: None,
            rounding: None,
            source: None,
            source_url: None,
            corpus_citation_path: None,
            semantics: DerivedSemantics::Scalar(expr),
            versions: Vec::new(),
        }
    }

    /// Build the model directly, so these tests isolate evaluation from
    /// RuleSpec lowering, relation typing and dataset binding.
    fn deep_chain(links: usize, kind: DeepLink, cycle: bool) -> (Program, DataSet) {
        let mut program = Program::default();
        program.relations.insert(
            "base".to_string(),
            RelationSchema {
                name: "base".to_string(),
                arity: 2,
                slot_entities: vec!["Node".to_string(), "Node".to_string()],
                derivation: None,
            },
        );
        for index in 0..links {
            let name = format!("r{index}");
            let next = if index + 1 < links {
                Some(format!("r{}", index + 1))
            } else {
                cycle.then(|| "r0".to_string())
            };
            let (source, predicate) = match (kind, next) {
                (DeepLink::Source, Some(next)) => (next, holds()),
                (DeepLink::Member, Some(next)) => (
                    "base".to_string(),
                    JudgmentExpr::RelationMember {
                        relation: next,
                        current_slot: 0,
                        related_slot: 1,
                    },
                ),
                (DeepLink::Counted, Some(next)) => (
                    "base".to_string(),
                    JudgmentExpr::Comparison {
                        left: count(next),
                        op: ComparisonOp::Gte,
                        right: ScalarExpr::Literal(ScalarValue::Integer(0)),
                    },
                ),
                (_, None) => ("base".to_string(), holds()),
            };
            program.relations.insert(
                name.clone(),
                RelationSchema {
                    name,
                    arity: 2,
                    slot_entities: vec!["Node".to_string(), "Node".to_string()],
                    derivation: Some(RelationDerivation {
                        source_relation: Some(source),
                        source_path: None,
                        current_slot: 0,
                        related_slot: 1,
                        entity: None,
                        member_relation: None,
                        slot_entities: vec!["Node".to_string(), "Node".to_string()],
                        predicate,
                    }),
                },
            );
        }
        program
            .derived
            .insert("head".to_string(), rule("head", count("r0".to_string())));
        let mut dataset = DataSet::default();
        let interval = Interval::covering(&Period::month(2026, 1));
        for (left, right) in [("a", "a"), ("a", "b"), ("b", "a"), ("b", "b")] {
            dataset.add_relation(
                "base",
                vec![left.to_string(), right.to_string()],
                interval.clone(),
            );
        }
        (program, dataset)
    }

    #[test]
    fn review_shared_candidate_fold_preserves_cached_relation_trace() {
        let (mut program, _) = deep_chain(1, DeepLink::Source, false);
        for name in ["owners", "items"] {
            program.relations.insert(
                name.to_string(),
                RelationSchema {
                    name: name.to_string(),
                    arity: 2,
                    slot_entities: vec!["Node".to_string(), "Node".to_string()],
                    derivation: None,
                },
            );
        }
        program.derived.insert(
            "p".to_string(),
            rule("p", ScalarExpr::Literal(ScalarValue::Integer(1))),
        );
        program
            .derived
            .insert("other".to_string(), rule("other", count("r0".to_string())));
        program.derived.get_mut("head").unwrap().semantics =
            DerivedSemantics::Scalar(ScalarExpr::CountRelated {
                relation: "owners".to_string(),
                current_slot: 0,
                related_slot: 1,
                where_clause: Some(Box::new(JudgmentExpr::Comparison {
                    left: count("r0".to_string()),
                    op: ComparisonOp::Gt,
                    right: ScalarExpr::Literal(ScalarValue::Integer(0)),
                })),
            });
        program
            .relations
            .get_mut("r0")
            .unwrap()
            .derivation
            .as_mut()
            .unwrap()
            .predicate = JudgmentExpr::Comparison {
            left: ScalarExpr::CountRelated {
                relation: "items".to_string(),
                current_slot: 0,
                related_slot: 1,
                where_clause: Some(Box::new(JudgmentExpr::Comparison {
                    left: ScalarExpr::Derived("p".to_string()),
                    op: ComparisonOp::Eq,
                    right: ScalarExpr::Literal(ScalarValue::Integer(1)),
                })),
            },
            op: ComparisonOp::Eq,
            right: ScalarExpr::Literal(ScalarValue::Integer(1)),
        };
        let period = Period::month(2026, 1);
        let interval = Interval::covering(&period);
        let mut dataset = DataSet::default();
        for (name, left, right) in [
            ("owners", "root", "A"),
            ("owners", "root", "B"),
            ("base", "A", "X"),
            ("base", "B", "X"),
            ("items", "X", "Y"),
        ] {
            dataset.add_relation(
                name,
                vec![left.to_string(), right.to_string()],
                interval.clone(),
            );
        }
        let run = |recursive| {
            let mut engine = Engine::new(&program, &dataset);
            engine.recursive_relation_oracle = recursive;
            engine.suspend_depth = usize::MAX;
            assert_eq!(
                engine.evaluate_scalar("head", "root", &period).unwrap(),
                ScalarValue::Integer(2)
            );
            assert_eq!(
                engine.evaluate_scalar("other", "B", &period).unwrap(),
                ScalarValue::Integer(1)
            );
            let key = super::CacheKey {
                derived: "other".to_string(),
                entity_id: "B".to_string(),
                period: period.clone(),
            };
            engine
                .execution_trace
                .get(&key)
                .unwrap()
                .dependencies
                .clone()
        };
        let recursive = run(true);
        let iterative = run(false);
        assert_eq!(recursive.len(), 1, "oracle must read p(Y)");
        assert_eq!(
            iterative, recursive,
            "cached relation r0(B) must retain the p(Y) dependency its fold reused"
        );
    }

    #[test]
    fn review_empty_chain_trace_replay_bookkeeping_is_linear() {
        let mut measurements = Vec::new();
        for links in [64, 256] {
            let (mut program, dataset) = deep_chain(links, DeepLink::Source, false);
            for index in 0..links {
                let name = format!("out{index}");
                program
                    .derived
                    .insert(name.clone(), rule(&name, count(format!("r{index}"))));
            }
            let period = Period::month(2026, 1);
            let mut engine = Engine::new(&program, &dataset);
            for index in 0..links {
                assert_eq!(
                    engine
                        .evaluate_scalar(&format!("out{index}"), "a", &period)
                        .unwrap(),
                    ScalarValue::Integer(2)
                );
            }
            assert!(
                engine
                    .execution_trace
                    .values()
                    .all(|trace| trace.dependencies.is_empty()
                        && trace.not_evaluated_dependencies.is_empty()
                        && trace.parameter_reads.is_empty())
            );
            let marks: usize = engine
                .derived_members
                .values()
                .map(|members| members.recorded_on.len())
                .sum();
            let effects: usize = engine
                .derived_members
                .values()
                .map(|members| members.effects.len())
                .sum();
            eprintln!(
                "review empty trace chain: {links} links/output rules, {marks} retained replay marks, {effects} retained effects"
            );
            measurements.push((links, marks, effects));
        }
        for (links, marks, effects) in measurements {
            assert_eq!(marks, 0, "no replay marks for {links} effect-free links");
            assert_eq!(effects, 0, "no replay edges for {links} effect-free links");
        }
    }

    #[test]
    fn many_callers_share_effectful_chain_trace_summaries() {
        let mut measurements = Vec::new();
        for links in [64, 256] {
            let (mut program, dataset) = deep_chain(links, DeepLink::Source, false);
            program.derived.insert(
                "p".to_string(),
                rule("p", ScalarExpr::Literal(ScalarValue::Integer(1))),
            );
            program.derived.get_mut("p").unwrap().entity = "Candidate".to_string();
            for relation in program.relations.values_mut() {
                relation.slot_entities[1] = "Candidate".to_string();
                if let Some(derivation) = relation.derivation.as_mut() {
                    derivation.slot_entities[1] = "Candidate".to_string();
                }
            }
            program
                .relations
                .get_mut(&format!("r{}", links - 1))
                .unwrap()
                .derivation
                .as_mut()
                .unwrap()
                .predicate = JudgmentExpr::Comparison {
                left: ScalarExpr::Derived("p".to_string()),
                op: ComparisonOp::Eq,
                right: ScalarExpr::Literal(ScalarValue::Integer(1)),
            };
            for index in 0..links {
                let name = format!("out{index}");
                program
                    .derived
                    .insert(name.clone(), rule(&name, count(format!("r{index}"))));
            }
            let period = Period::month(2026, 1);
            let mut engine = Engine::new(&program, &dataset);
            for index in 0..links {
                let output = format!("out{index}");
                assert_eq!(
                    engine.evaluate_scalar(&output, "a", &period).unwrap(),
                    ScalarValue::Integer(2)
                );
                let key = super::CacheKey {
                    derived: output,
                    entity_id: "a".to_string(),
                    period: period.clone(),
                };
                let dependencies = &engine.execution_trace[&key].dependencies;
                assert_eq!(
                    dependencies
                        .iter()
                        .map(|key| (key.derived.as_str(), key.entity_id.as_str()))
                        .collect::<Vec<_>>(),
                    [("p", "a"), ("p", "b")]
                );
            }
            let marks: usize = engine
                .derived_members
                .values()
                .map(|members| members.recorded_on.len())
                .sum();
            let effects: usize = engine
                .derived_members
                .values()
                .map(|members| members.effects.len())
                .sum();
            eprintln!(
                "effectful trace chain: {links} links/callers, {marks} retained replay marks, {effects} retained effects"
            );
            measurements.push((links, marks, effects));
        }
        for (links, marks, effects) in measurements {
            assert_eq!(
                marks, 1,
                "all forwarding links share one summary with one recent caller ({links} links)"
            );
            assert!(
                effects <= 2 * links,
                "{effects} retained effects for {links} links"
            );
        }
    }

    #[test]
    fn deep_direct_relation_evaluation_uses_bounded_stack() {
        for kind in [DeepLink::Source, DeepLink::Member, DeepLink::Counted] {
            let (program, dataset) = deep_chain(20_000, kind, false);
            for traced in [true, false] {
                let mut engine = Engine::with_tracing(&program, &dataset, traced);
                assert_eq!(
                    engine
                        .evaluate_scalar("head", "a", &Period::month(2026, 1))
                        .expect("chain evaluates"),
                    ScalarValue::Integer(2)
                );
            }
        }
    }

    #[test]
    fn deep_direct_relation_cycles_are_rejected_without_recursing() {
        for kind in [DeepLink::Source, DeepLink::Member, DeepLink::Counted] {
            let (program, dataset) = deep_chain(20_000, kind, true);
            let mut engine = Engine::new(&program, &dataset);
            let error = engine
                .evaluate_scalar("head", "a", &Period::month(2026, 1))
                .expect_err("a relation depending on its own members is cyclic");
            assert!(
                error.to_string().contains("depends on its own members"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_deferred_rule_in_a_relation_predicate_resumes_without_caching_the_interrupt() {
        let (mut program, dataset) = deep_chain(3, DeepLink::Source, false);
        program.derived.insert(
            "predicate_value".to_string(),
            rule(
                "predicate_value",
                ScalarExpr::Literal(ScalarValue::Integer(1)),
            ),
        );
        program
            .relations
            .get_mut("r2")
            .expect("tail")
            .derivation
            .as_mut()
            .expect("derived")
            .predicate = JudgmentExpr::Comparison {
            left: ScalarExpr::Derived("predicate_value".to_string()),
            op: ComparisonOp::Eq,
            right: ScalarExpr::Literal(ScalarValue::Integer(1)),
        };
        let mut engine = Engine::new(&program, &dataset);
        engine.suspend_depth = 1;
        assert_eq!(
            engine
                .evaluate_scalar("head", "a", &Period::month(2026, 1))
                .expect("chain evaluates"),
            ScalarValue::Integer(2)
        );
        assert!(engine.pending_members.is_empty());
        assert!(engine.member_captures.is_empty());
        assert!(
            engine
                .derived_members
                .values()
                .all(|members| members.result.is_ok())
        );
    }

    #[test]
    fn interrupted_source_chains_validate_each_source_once() {
        for deferred_rule in [false, true] {
            let links = 512;
            let (mut program, dataset) = deep_chain(links, DeepLink::Source, false);
            for index in 0..links {
                let predicate = if deferred_rule {
                    let dependency = format!("p{index}");
                    program.derived.insert(
                        dependency.clone(),
                        rule(&dependency, ScalarExpr::Literal(ScalarValue::Integer(1))),
                    );
                    JudgmentExpr::Comparison {
                        left: ScalarExpr::Derived(dependency),
                        op: ComparisonOp::Eq,
                        right: ScalarExpr::Literal(ScalarValue::Integer(1)),
                    }
                } else {
                    let auxiliary = format!("aux{index}");
                    program.relations.insert(
                        auxiliary.clone(),
                        RelationSchema {
                            name: auxiliary.clone(),
                            arity: 2,
                            slot_entities: vec!["Node".to_string(), "Node".to_string()],
                            derivation: Some(RelationDerivation {
                                source_relation: Some("base".to_string()),
                                source_path: None,
                                current_slot: 0,
                                related_slot: 1,
                                entity: None,
                                member_relation: None,
                                slot_entities: vec!["Node".to_string(), "Node".to_string()],
                                predicate: holds(),
                            }),
                        },
                    );
                    JudgmentExpr::RelationMember {
                        relation: auxiliary,
                        current_slot: 0,
                        related_slot: 1,
                    }
                };
                program
                    .relations
                    .get_mut(&format!("r{index}"))
                    .expect("source link")
                    .derivation
                    .as_mut()
                    .expect("derived")
                    .predicate = predicate;
            }
            let mut engine = Engine::new(&program, &dataset).with_relation_nesting_limit(1);
            engine.suspend_depth = 1;
            assert_eq!(
                engine
                    .evaluate_scalar("head", "a", &Period::month(2026, 1))
                    .expect("chain evaluates"),
                ScalarValue::Integer(2)
            );
            assert!(
                engine.source_walks <= 2 * links,
                "{} source checks for {links} links (rule deferral: {deferred_rule})",
                engine.source_walks
            );
            assert!(engine.member_chains.is_empty());
        }
    }

    #[test]
    fn a_completed_predicate_fold_keeps_its_trace_when_a_later_rule_defers() {
        let (mut program, mut dataset) = deep_chain(1, DeepLink::Source, false);
        dataset.relations.truncate(1);
        for dependency in ["first", "later"] {
            program.derived.insert(
                dependency.to_string(),
                rule(dependency, ScalarExpr::Literal(ScalarValue::Integer(1))),
            );
        }
        let read = |dependency: &str| JudgmentExpr::Comparison {
            left: ScalarExpr::Derived(dependency.to_string()),
            op: ComparisonOp::Eq,
            right: ScalarExpr::Literal(ScalarValue::Integer(1)),
        };
        program
            .relations
            .get_mut("r0")
            .expect("relation")
            .derivation
            .as_mut()
            .expect("derived")
            .predicate = JudgmentExpr::And(vec![
            JudgmentExpr::Comparison {
                left: ScalarExpr::CountRelated {
                    relation: "base".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    where_clause: Some(Box::new(read("first"))),
                },
                op: ComparisonOp::Eq,
                right: ScalarExpr::Literal(ScalarValue::Integer(1)),
            },
            read("later"),
        ]);
        let period = Period::month(2026, 1);
        let mut engine = Engine::new(&program, &dataset);
        engine.suspend_depth = 1;
        assert_eq!(
            engine
                .evaluate_scalar("head", "a", &period)
                .expect("chain evaluates"),
            ScalarValue::Integer(1)
        );
        let head = super::CacheKey {
            derived: "head".to_string(),
            entity_id: "a".to_string(),
            period,
        };
        let dependencies = engine.execution_trace[&head]
            .dependencies
            .iter()
            .map(|key| key.derived.as_str())
            .collect::<Vec<_>>();
        assert_eq!(dependencies, ["first", "later"]);
    }

    #[test]
    fn a_predicate_fold_suspended_for_each_member_walks_members_once() {
        let mut previous = None;
        for members in [128_usize, 512] {
            let (mut program, _) = deep_chain(1, DeepLink::Source, false);
            for name in ["fan", "items"] {
                program.relations.insert(
                    name.to_string(),
                    RelationSchema {
                        name: name.to_string(),
                        arity: 2,
                        slot_entities: vec!["Node".to_string(), "Node".to_string()],
                        derivation: None,
                    },
                );
            }
            program.relations.insert(
                "selected".to_string(),
                RelationSchema {
                    name: "selected".to_string(),
                    arity: 2,
                    slot_entities: vec!["Node".to_string(), "Node".to_string()],
                    derivation: Some(RelationDerivation {
                        source_relation: Some("items".to_string()),
                        source_path: None,
                        current_slot: 0,
                        related_slot: 1,
                        entity: None,
                        member_relation: None,
                        slot_entities: vec!["Node".to_string(), "Node".to_string()],
                        predicate: holds(),
                    }),
                },
            );
            program
                .relations
                .get_mut("r0")
                .expect("relation")
                .derivation
                .as_mut()
                .expect("derived")
                .predicate = JudgmentExpr::Comparison {
                left: ScalarExpr::CountRelated {
                    relation: "fan".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    where_clause: Some(Box::new(JudgmentExpr::Comparison {
                        left: count("selected".to_string()),
                        op: ComparisonOp::Gte,
                        right: ScalarExpr::Literal(ScalarValue::Integer(0)),
                    })),
                },
                op: ComparisonOp::Eq,
                right: ScalarExpr::Literal(ScalarValue::Integer(
                    i64::try_from(members).expect("fits"),
                )),
            };
            let period = Period::month(2026, 1);
            let interval = Interval::covering(&period);
            let mut dataset = DataSet::default();
            dataset.add_relation(
                "base",
                vec!["root".to_string(), "owner".to_string()],
                interval.clone(),
            );
            for member in 0..members {
                let entity = format!("member{member}");
                dataset.add_relation(
                    "fan",
                    vec!["owner".to_string(), entity.clone()],
                    interval.clone(),
                );
                dataset.add_relation("items", vec![entity, "leaf".to_string()], interval.clone());
            }
            let mut engine = Engine::new(&program, &dataset).with_relation_nesting_limit(1);
            let (result, visits) =
                crate::depth::count_visits(|| engine.evaluate_scalar("head", "root", &period));
            assert_eq!(
                result.expect("predicate fold completes"),
                ScalarValue::Integer(1)
            );
            assert!(
                visits <= 24 * members + 100,
                "{visits} visits for {members} members; completed prefixes must not be replayed"
            );
            if let Some(smaller) = previous {
                assert!(
                    visits <= 5 * smaller,
                    "{visits} visits after {smaller} at one quarter the members"
                );
            }
            previous = Some(visits);
        }
    }
}
