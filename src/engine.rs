// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, JudgmentExpr, JudgmentOutcome, Period,
    Program, RelatedValueRef, ScalarExpr, ScalarValue,
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

pub(crate) fn describe_match_operand(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(value) => describe_scalar_value(value),
        ScalarExpr::Input(name) | ScalarExpr::Derived(name) => name.clone(),
        ScalarExpr::InputOrElse { name, .. } => name.clone(),
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

#[derive(Clone, Debug, Default)]
pub(crate) struct NodeExecutionTrace {
    pub(crate) dependencies: Vec<CacheKey>,
    pub(crate) not_evaluated_dependencies: Vec<SkippedTraceDependency>,
    pub(crate) parameter_reads: Vec<ParameterTraceRead>,
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
    /// Rule evaluations the effects are already recorded on.
    recorded_on: HashSet<CacheKey>,
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
    /// Effects of the relation predicates under evaluation, innermost last,
    /// each with the number of active evaluations when it began: only records
    /// made at that depth belong to the relation rather than to a rule its
    /// predicate evaluates.
    member_captures: Vec<(usize, Vec<TraceEffect>)>,
    /// Derived-relation resolutions running on the native stack, and how
    /// many may ([`RELATION_NESTING_LIMIT`] outside tests).
    relation_depth: usize,
    relation_nesting_limit: usize,
    /// Set when a resolution stopped because it needed these members and
    /// resolving them here would nest past [`RELATION_NESTING_LIMIT`]. The
    /// error that carries the stop out to the resolving driver is a
    /// placeholder; the driver reads this instead.
    suspended_on: Option<DerivedMembersKey>,
}

impl<'a> Engine<'a> {
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
            member_captures: Vec::new(),
            relation_depth: 0,
            relation_nesting_limit: RELATION_NESTING_LIMIT,
            suspended_on: None,
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
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.scalar_cache.get(&key) {
            return Ok(value.clone());
        }

        let derived = self.get_derived(derived_name)?.clone();
        self.validate_unit(&derived)?;
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
            DerivedSemantics::Scalar(expr) => self.eval_scalar_expr(expr, entity_id, period),
            DerivedSemantics::Judgment(_) => {
                Err(EvalError::ExpectedScalar(derived_name.to_string()))
            }
        };
        let finished = self
            .active_evaluations
            .pop()
            .expect("scalar evaluation pushed an active trace key");
        debug_assert_eq!(finished, key);
        let value = evaluated
            .map_err(|error| error.within_rule(derived.id.as_deref().unwrap_or(derived_name)))?;
        // Apply the rule's opt-in output rounding before caching, so the
        // rounded value is what both direct queries and dependent rules
        // (`ScalarExpr::Derived`) observe. Absent `rounding` is a no-op. When
        // rounding actually moves the value, keep the pre-rounding amount so the
        // trace can show the rounding step.
        let rounded = apply_output_rounding(&derived, value.clone());
        if self.tracing && rounded != value {
            self.pre_rounding_cache.insert(key.clone(), value);
        }
        self.scalar_cache.insert(key, rounded.clone());
        Ok(rounded)
    }

    pub fn evaluate_judgment(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<JudgmentOutcome, EvalError> {
        let key = CacheKey {
            derived: derived_name.to_string(),
            entity_id: entity_id.to_string(),
            period: period.clone(),
        };
        if let Some(value) = self.judgment_cache.get(&key) {
            return Ok(*value);
        }

        let derived = self.get_derived(derived_name)?.clone();
        self.validate_unit(&derived)?;
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
            DerivedSemantics::Judgment(expr) => self.eval_judgment_expr(expr, entity_id, period),
            DerivedSemantics::Scalar(_) => {
                Err(EvalError::ExpectedJudgment(derived_name.to_string()))
            }
        };
        let finished = self
            .active_evaluations
            .pop()
            .expect("judgment evaluation pushed an active trace key");
        debug_assert_eq!(finished, key);
        let value = evaluated
            .map_err(|error| error.within_rule(derived.id.as_deref().unwrap_or(derived_name)))?;
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
        if let Some((capture_depth, effects)) = self.member_captures.last_mut()
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

    /// Record on `parent` everything resolving `key` recorded, in order,
    /// expanding the members it read in place. Members already replayed on
    /// `parent` are skipped whole: their records are there already. The walk
    /// keeps its own stack, since a relation chain nests members per link.
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
        self.derived_members
            .get_mut(key)
            .is_some_and(|members| members.recorded_on.insert(parent.clone()))
    }

    fn record_skipped_scalar_dependencies(
        &mut self,
        expr: &ScalarExpr,
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
        expr: &JudgmentExpr,
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

    fn get_derived(&self, name: &str) -> Result<&Derived, EvalError> {
        self.program
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

    pub(crate) fn eval_scalar_expr(
        &mut self,
        expr: &ScalarExpr,
        entity_id: &str,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        self.eval_scalar_expr_inner(expr, entity_id, period, None)
    }

    fn eval_scalar_expr_inner(
        &mut self,
        expr: &ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Result<ScalarValue, EvalError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(value.clone()),
            ScalarExpr::Input(name) => self.lookup_input(name, entity_id, period),
            ScalarExpr::InputOrElse { name, default } => {
                match self.lookup_input(name, entity_id, period) {
                    Ok(value) => Ok(value),
                    Err(EvalError::MissingInput { .. }) => Ok(default.clone()),
                    Err(other) => Err(other),
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
                self.evaluate_scalar(name, target_entity_id, period)
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
                self.lookup_parameter(parameter, lookup_key, period)
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
                    return Err(EvalError::DivisionByZero);
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
                    ));
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
                    ));
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
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let related_ids = self.related_entity_ids(
                    relation,
                    *current_slot,
                    *related_slot,
                    entity_id,
                    period,
                )?;
                let mut count = 0_usize;
                for related_id in related_ids {
                    if let Some(predicate) = where_clause {
                        if !self
                            .eval_judgment_expr(predicate, &related_id, period)?
                            .is_holds()
                        {
                            continue;
                        }
                    }
                    count += 1;
                }
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
                let mut total = Decimal::ZERO;
                for related_id in self.related_entity_ids(
                    relation,
                    *current_slot,
                    *related_slot,
                    entity_id,
                    period,
                )? {
                    if let Some(predicate) = where_clause {
                        if !self
                            .eval_judgment_expr(predicate, &related_id, period)?
                            .is_holds()
                        {
                            continue;
                        }
                    }
                    total =
                        checked_add(total, self.eval_related_value(value, &related_id, period)?)?;
                }
                Ok(ScalarValue::Decimal(total))
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
                Err(no_matching_arm(subject, &value, patterns))
            }
            // Cross-period reductions are only defined when a batch is supplied
            // per period (the dense lifetime surface). The sparse single-period
            // interpreter has no period axis to reduce over.
            ScalarExpr::OverPeriods { kind, .. } => {
                Err(EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()))
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &JudgmentExpr,
        entity_id: &str,
        period: &Period,
    ) -> Result<JudgmentOutcome, EvalError> {
        self.eval_judgment_expr_inner(expr, entity_id, period, None)
    }

    fn eval_judgment_expr_inner(
        &mut self,
        expr: &JudgmentExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Result<JudgmentOutcome, EvalError> {
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
                let derived = self.get_derived(name)?.clone();
                let target_entity_id = relation_context
                    .and_then(|context| context.entity_id_for(&derived.entity))
                    .unwrap_or(entity_id);
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: target_entity_id.to_string(),
                    period: period.clone(),
                });
                self.evaluate_judgment(name, target_entity_id, period)
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

    fn eval_related_value(
        &mut self,
        value: &RelatedValueRef,
        entity_id: &str,
        period: &Period,
    ) -> Result<Decimal, EvalError> {
        let scalar = match value {
            RelatedValueRef::Input(name) => self.lookup_input(name, entity_id, period)?,
            RelatedValueRef::Derived(name) => {
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                });
                self.evaluate_scalar(name, entity_id, period)?
            }
        };
        scalar.as_decimal().ok_or_else(|| {
            EvalError::TypeMismatch("related aggregation requires numeric values".to_string())
        })
    }

    fn eval_decimal(
        &mut self,
        expr: &ScalarExpr,
        entity_id: &str,
        period: &Period,
        relation_context: Option<RelationEvalContext<'_>>,
    ) -> Result<Decimal, EvalError> {
        self.eval_scalar_expr_inner(expr, entity_id, period, relation_context)?
            .as_decimal()
            .ok_or_else(|| EvalError::TypeMismatch("expected numeric scalar".to_string()))
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
    ) -> Result<Vec<String>, EvalError> {
        let schema = self.relation_schema(relation, current_slot, related_slot)?;
        let mut related_ids =
            self.direct_related_ids(relation, current_slot, related_slot, entity_id, period);
        if schema.derivation.is_some() {
            related_ids.extend(
                self.derived_members(relation, entity_id, period)?
                    .iter()
                    .cloned(),
            );
        }
        related_ids.sort();
        related_ids.dedup();
        Ok(related_ids)
    }

    fn relation_contains(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        current_id: &str,
        related_id: &str,
        period: &Period,
    ) -> Result<bool, EvalError> {
        let schema = self.relation_schema(relation, current_slot, related_slot)?;
        let direct = self
            .relation_index
            .get(&(relation.to_string(), current_slot, current_id.to_string()))
            .is_some_and(|records| {
                records.iter().any(|record| {
                    record.interval.contains_period(period)
                        && record
                            .tuple
                            .get(related_slot)
                            .is_some_and(|candidate| candidate == related_id)
                })
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

    /// The ids `relation`'s own tuples relate to `entity_id` in `period`.
    fn direct_related_ids(
        &self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        entity_id: &str,
        period: &Period,
    ) -> Vec<String> {
        self.relation_index
            .get(&(relation.to_string(), current_slot, entity_id.to_string()))
            .into_iter()
            .flat_map(|records| records.iter().copied())
            .filter(|record| record.interval.contains_period(period))
            .filter_map(|record| record.tuple.get(related_slot).cloned())
            .collect()
    }

    /// The members the derived relation `relation` adds for `entity_id` in
    /// `period`. They are resolved once; every later use reads them and
    /// replays the trace records resolving them made.
    fn derived_members(
        &mut self,
        relation: &str,
        entity_id: &str,
        period: &Period,
    ) -> Result<Rc<[String]>, EvalError> {
        let key = members_key(relation, entity_id, period);
        if !self.derived_members.contains_key(&key) {
            if self.relation_depth >= self.relation_nesting_limit {
                self.suspended_on = Some(key);
                return Err(EvalError::TypeMismatch(
                    "derived relation resolution suspended".to_string(),
                ));
            }
            self.resolve_members(key.clone());
        }
        self.record_effect(TraceEffect::Members(key.clone()));
        self.derived_members
            .get(&key)
            .expect("resolution memoizes the members it was asked for")
            .result
            .clone()
    }

    /// Resolve `key` without nesting resolutions on the native stack past
    /// [`RELATION_NESTING_LIMIT`]. An attempt resolves as far as it can. When
    /// it stops for members it needs, those go on this driver's stack, and the
    /// stopped attempt runs again once they are resolved: it rereads what it
    /// evaluated from the caches, and its predicate resumes at the candidate
    /// that stopped it. The order in which rules and relations are first
    /// evaluated is therefore the order recursion would have evaluated them.
    fn resolve_members(&mut self, key: DerivedMembersKey) {
        let mut stack = vec![key.clone()];
        let mut on_stack = HashSet::from([key]);
        while let Some(top) = stack.last().cloned() {
            self.relation_depth = self.relation_depth.saturating_add(1);
            self.fold_members(&top);
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
                self.derived_members.insert(
                    top.clone(),
                    DerivedMembers {
                        result: Err(cyclic_relation_error(&needed.relation)),
                        effects: Vec::new(),
                        recorded_on: HashSet::new(),
                    },
                );
            }
            debug_assert!(self.derived_members.contains_key(&top));
            stack.pop();
            on_stack.remove(&top);
        }
    }

    /// Resolve the members of `key`'s relation, and before them those of each
    /// unresolved derived relation down its chain of source relations,
    /// deepest first, memoizing each. The chain is walked by a loop, not by
    /// recursion. Stops, leaving `suspended_on` set and the stopped
    /// predicate's progress in `pending_members`, when a predicate needs
    /// members that must be resolved first.
    fn fold_members(&mut self, key: &DerivedMembersKey) {
        let program: &'a Program = self.program;
        let (entity_id, period) = (key.entity_id.as_str(), &key.period);
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
                || self
                    .derived_members
                    .contains_key(&members_key(&source.name, entity_id, period))
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
                        recorded_on: HashSet::new(),
                    },
                );
            }
            return;
        }

        for relation in chain.into_iter().rev() {
            let level = members_key(&relation.name, entity_id, period);
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
                                recorded_on: HashSet::new(),
                            },
                        );
                        continue;
                    }
                },
            };
            if !self.filter_members(&level, derivation, pending) {
                return;
            }
        }
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
            effects.push(TraceEffect::Members(source_key));
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
    /// memoize the members that hold. Returns false, with the progress in
    /// `pending_members`, if the predicate stopped for members it needs.
    fn filter_members(
        &mut self,
        level: &DerivedMembersKey,
        derivation: &'a crate::model::RelationDerivation,
        pending: PendingMembers,
    ) -> bool {
        let PendingMembers {
            candidates,
            next,
            mut kept,
            effects,
        } = pending;
        self.member_captures
            .push((self.active_evaluations.len(), effects));
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
            let recorded = self
                .member_captures
                .last()
                .map_or(0, |(_, effects)| effects.len());
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
                Err(_) if self.suspended_on.is_some() => {
                    // This candidate is tested again from the start once the
                    // members it needs are resolved.
                    let (_, mut effects) = self
                        .member_captures
                        .pop()
                        .expect("the capture pushed above");
                    effects.truncate(recorded);
                    self.pending_members.insert(
                        level.clone(),
                        PendingMembers {
                            candidates: Rc::clone(&candidates),
                            next: index,
                            kept,
                            effects,
                        },
                    );
                    return false;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let (_, effects) = self
            .member_captures
            .pop()
            .expect("the capture pushed above");
        self.derived_members.insert(
            level.clone(),
            DerivedMembers {
                result: failure.map_or_else(|| Ok(kept.into()), Err),
                effects,
                recorded_on: HashSet::new(),
            },
        );
        true
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

#[cfg(test)]
mod relation_resolution_tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    use serde_json::{Value, json};

    use super::Engine;
    use crate::model::{DType, Period};
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
                prop::sample::select([HOUSEHOLDS.as_slice(), PEOPLE.as_slice()].concat()),
                prop::sample::select(PEOPLE.to_vec()),
            ),
            0..8,
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
                .map(|(left, right)| {
                    json!({"name": "base", "tuple": [left, right], "interval": interval})
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
        let program = program.to_program().expect("the program lowers");
        let dataset = dataset
            .to_dataset_for_program(&program)
            .expect("the dataset binds");
        let period = Period::month(2026, 1);
        let mut engine = Engine::new(&program, &dataset).with_relation_nesting_limit(limit);
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
                let native = evaluate(&program, &dataset, super::RELATION_NESTING_LIMIT);
                let suspended = evaluate(&program, &dataset, 1);
                if native != suspended {
                    return Err(TestCaseError::fail(format!(
                        "native {native:#?}\nsuspended {suspended:#?}\nprogram {}",
                        serde_json::to_string(&program).expect("serializes")
                    )));
                }
                if native.iter().any(|line| line.contains("Ok(")) {
                    nested.set(nested.get() + 1);
                }
                Ok(())
            })
            .unwrap();
        assert!(nested.get() > 500, "only {} cases evaluated", nested.get());
    }
}
