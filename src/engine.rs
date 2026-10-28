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
        .map(|context| context.id_of(term))
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

/// How many derived-relation resolutions may nest on the native stack. A
/// relation's predicate can read another derived relation, whose predicate
/// reads the next, so nesting natively would grow the stack with the length
/// of such a chain. Past this depth a resolution stops where it needs the
/// next relation, the driver below it resolves that relation, and the
/// stopped resolution then runs again; see [`Engine::resolve_members`].
/// Debug frames are much larger: a count-related predicate chain needed
/// 2048 KiB at depth 32, but 576 KiB at depth 8 (aarch64-apple-darwin,
/// rustc 1.94.1, 16 KiB resolution). Debug builds therefore
/// stop earlier; this also exercises suspended resolution throughout tests.
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
struct RelationEvalContext<'a> {
    current_id: &'a str,
    related_id: &'a str,
    current_entity: Option<&'a str>,
    related_entity: Option<&'a str>,
}

impl<'a> RelationEvalContext<'a> {
    /// The id an unqualified rule read of `entity` routes to: the current id
    /// when the current slot has that kind, else the related id when the
    /// related slot has it, else none (the caller falls back to the id the
    /// predicate is evaluated on, the candidate).
    fn entity_id_for(self, entity: &str) -> Option<&'a str> {
        if self.current_entity == Some(entity) {
            return Some(self.current_id);
        }
        if self.related_entity == Some(entity) {
            return Some(self.related_id);
        }
        None
    }

    /// The id `term` names.
    fn id_of(self, term: Term) -> &'a str {
        match term {
            Term::Anchor => self.current_id,
            Term::Candidate => self.related_id,
        }
    }
}

pub struct Engine<'a> {
    program: &'a Program,
    input_index: HashMap<(String, String), Vec<&'a crate::model::InputRecord>>,
    relation_index: HashMap<(String, usize, String), Vec<&'a crate::model::RelationRecord>>,
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
    /// Validated source prefixes that a predicate interruption has not yet
    /// finished. Retaining them prevents a retry from walking the prefix again.
    member_chains: HashMap<DerivedMembersKey, Vec<&'a crate::model::RelationSchema>>,
    #[cfg(test)]
    source_walks: usize,
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
            scalar_cache: HashMap::new(),
            pre_rounding_cache: HashMap::new(),
            judgment_cache: HashMap::new(),
            execution_trace: HashMap::new(),
            trace_index: HashMap::new(),
            active_evaluations: Vec::new(),
            tracing,
            derived_members: HashMap::new(),
            pending_members: HashMap::new(),
            member_chains: HashMap::new(),
            #[cfg(test)]
            source_walks: 0,
            member_captures: Vec::new(),
            relation_depth: 0,
            relation_nesting_limit: RELATION_NESTING_LIMIT,
            suspended_on: None,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            folds: HashMap::new(),
            #[cfg(test)]
            members: HashMap::new(),
            #[cfg(test)]
            recursive_relation_oracle: false,
            visits: 0,
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
        debug_assert!(self.member_captures.is_empty());
        self.folds.clear();
        self.pending_members.clear();
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
            .map(|term| context.id_of(*term))
            .collect::<Vec<_>>();
        Ok(if self.relation_atom_contains(relation, &ids, period)? {
            JudgmentOutcome::Holds
        } else {
            JudgmentOutcome::NotHolds
        })
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
            if (context.id_of(left) == context.id_of(right)) != negated {
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
                    (name, context.id_of(term).to_string())
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
            | JudgmentExpr::SameEntity { .. } => {
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
                    stack.push(needed);
                    continue;
                }
                // `top` needs members that wait on `top`: a cycle, which
                // program validation refuses. The recursive resolution this
                // replaces overflowed the stack on one.
                self.pending_members.remove(&top);
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
                    let source = match self.relation_schema(
                        &derivation.source_relation,
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
        let source = &derivation.source_relation;
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
        let current_entity = derivation
            .slot_entities
            .get(derivation.current_slot)
            .map(String::as_str);
        let related_entity = derivation
            .slot_entities
            .get(derivation.related_slot)
            .map(String::as_str);
        let mut failure = None;
        for (index, candidate) in candidates.iter().enumerate().skip(next) {
            let context = RelationEvalContext {
                current_id: &level.entity_id,
                related_id: candidate,
                current_entity,
                related_entity,
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
        Program, RelationDerivation, RelationSchema, ScalarExpr, ScalarValue,
    };
    use crate::spec::{DatasetSpec, ProgramSpec};

    const HOUSEHOLDS: [&str; 2] = ["h1", "h2"];
    const PEOPLE: [&str; 3] = ["p1", "p2", "p3"];
    const RELATIONS: usize = 6;

    fn literal(value: i64) -> Value {
        json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
    }

    fn person_rule(index: usize) -> Value {
        json!({"kind": "derived", "name": format!("p{index}")})
    }

    /// A predicate for derived relation `index`: it may read only relations
    /// after it and person rules, so programs stay acyclic.
    fn predicate(index: usize) -> BoxedStrategy<Value> {
        let later = (index + 1..RELATIONS)
            .map(|later| format!("r{later}"))
            .collect::<Vec<_>>();
        let mut leaves = vec![
            Just(json!({"kind": "comparison", "left": literal(1), "op": "eq", "right": literal(1)}))
                .boxed(),
            (0_usize..2)
                .prop_map(|rule| {
                    json!({"kind": "comparison", "left": person_rule(rule), "op": "gte", "right": literal(10)})
                })
                .boxed(),
            // A parameter read and a division that can fail.
            (0_i64..3)
                .prop_map(|key| {
                    json!({"kind": "comparison",
                        "left": {"kind": "div", "left": {"kind": "parameter_lookup", "parameter": "rate", "index": literal(key)}, "right": {"kind": "input", "name": "income"}},
                        "op": "gt", "right": literal(0)})
                })
                .boxed(),
        ];
        if !later.is_empty() {
            let members = later.clone();
            leaves.push(
                (prop::sample::select(members), 0_usize..2)
                    .prop_map(|(relation, orientation)| {
                        json!({"kind": "relation_member", "relation": relation,
                            "current_slot": orientation, "related_slot": 1 - orientation})
                    })
                    .boxed(),
            );
            leaves.push(
                prop::sample::select(later)
                    .prop_map(|relation| {
                        json!({"kind": "comparison",
                            "left": {"kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1},
                            "op": "gte", "right": literal(1)})
                    })
                    .boxed(),
            );
        }
        prop::strategy::Union::new(leaves)
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

    fn relation(index: usize) -> BoxedStrategy<Value> {
        let sources = std::iter::once("base".to_string())
            .chain((index + 1..RELATIONS).map(|later| format!("r{later}")))
            .collect::<Vec<_>>();
        (
            prop::sample::select(sources),
            prop::sample::select(vec![(0_usize, 1_usize), (1, 0)]),
            prop::bool::ANY,
            predicate(index),
        )
            .prop_map(
                move |(source, (current_slot, related_slot), typed, predicate)| {
                    let mut derivation = json!({
                        "source_relation": source, "current_slot": current_slot,
                        "related_slot": related_slot, "predicate": predicate,
                    });
                    if typed {
                        derivation["slot_entities"] = json!(["Household", "Person"]);
                    }
                    json!({"name": format!("r{index}"), "arity": 2, "derivation": derivation})
                },
            )
            .boxed()
    }

    fn household_rule(index: usize) -> BoxedStrategy<Value> {
        let relation = (0..RELATIONS).prop_map(|relation| format!("r{relation}"));
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
    fn case() -> impl Strategy<Value = (ProgramSpec, DatasetSpec)> {
        let relations = (0..RELATIONS).map(relation).collect::<Vec<_>>();
        let rules = (0..3).map(household_rule).collect::<Vec<_>>();
        let tuples = vec(
            (
                prop::sample::select(
                    std::iter::once("base".to_string())
                        .chain((0..RELATIONS).map(|index| format!("r{index}")))
                        .collect::<Vec<_>>(),
                ),
                prop::sample::select([HOUSEHOLDS.as_slice(), PEOPLE.as_slice()].concat()),
                prop::sample::select(PEOPLE.to_vec()),
            ),
            0..12,
        );
        let incomes = vec(prop::option::of(0_i64..40), PEOPLE.len());
        (relations, rules, tuples, incomes).prop_map(|(relations, rules, tuples, incomes)| {
            let mut derived = vec![
                json!({"name": "p0", "entity": "Person", "dtype": "decimal", "unit": null,
                    "semantics": "scalar", "expr": {"kind": "input", "name": "income"}}),
                json!({"name": "p1", "entity": "Person", "dtype": "decimal", "unit": null,
                    "semantics": "scalar", "expr": {"kind": "add", "items": [person_rule(0), literal(5)]}}),
            ];
            derived.extend(rules);
            let relations = std::iter::once(json!({"name": "base", "arity": 2}))
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

    /// Every household output's value or error, and each household's trace,
    /// as an engine resolving at most `limit` relations natively gives them.
    fn evaluate(program: &ProgramSpec, dataset: &DatasetSpec, limit: usize) -> Vec<String> {
        evaluate_with_oracle(
            program,
            dataset,
            limit,
            false,
            crate::depth::suspend_depth(),
        )
    }

    fn evaluate_with_oracle(
        program: &ProgramSpec,
        dataset: &DatasetSpec,
        limit: usize,
        recursive: bool,
        suspend_depth: usize,
    ) -> Vec<String> {
        let program = program.to_program().expect("the program lowers");
        // These are direct evaluator comparisons. Keep every generated
        // caller-slot orientation and direct tuple, including combinations
        // that strict dataset admission rejects for entity-kind mismatches.
        let dataset = dataset.to_dataset().expect("a valid raw dataset");
        let period = Period::month(2026, 1);
        let mut engine = Engine::new(&program, &dataset).with_relation_nesting_limit(limit);
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
        outcomes
    }

    #[test]
    fn suspended_resolution_matches_native_resolution() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 1500,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[23; 32]),
        );
        let nested = std::cell::Cell::new(0_usize);
        runner
            .run(&case(), |(program, dataset)| {
                // Keep this reference unsuspended regardless of the runtime
                // default: these acyclic cases have only RELATIONS links.
                let native = evaluate(&program, &dataset, usize::MAX);
                for limit in [1, 2, super::RELATION_NESTING_LIMIT, 32, usize::MAX] {
                    let suspended = evaluate(&program, &dataset, limit);
                    if native != suspended {
                        return Err(TestCaseError::fail(format!(
                            "relation nesting {limit}\nnative {native:#?}\nsuspended {suspended:#?}\nprogram {}",
                            serde_json::to_string(&program).expect("serializes")
                        )));
                    }
                }
                if native.iter().any(|line| line.contains("Ok(")) {
                    nested.set(nested.get() + 1);
                }
                Ok(())
            })
            .unwrap();
        assert!(nested.get() > 500, "only {} cases evaluated", nested.get());
    }

    /// The old recursive resolver is an independent oracle: source traversal,
    /// predicate evaluation and direct tuples follow their original order,
    /// without the derived-member cache or effect replay under test.
    #[test]
    fn iterative_resolution_matches_recursive_semantics_on_small_dags() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 1500,
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
                        source_relation: source,
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
                                source_relation: "base".to_string(),
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
                        source_relation: "items".to_string(),
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
