// rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
// uses the checked helpers in engine.rs instead (see clippy.toml).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};

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
    /// Expression levels evaluated since the driver last reported them (see
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

/// An interrupted resolution of a derived relation's members: the members
/// the relation's own tuples give, the candidates its source relation gives,
/// the candidate whose predicate deferred, and the candidates before it that
/// the predicate selected.
#[derive(Debug)]
struct MembersResume {
    direct: Vec<String>,
    candidates: Vec<String>,
    next: usize,
    selected: Vec<String>,
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
            active_evaluations: Vec::new(),
            tracing,
            depth: 0,
            suspend_depth: crate::depth::suspend_depth(),
            folds: HashMap::new(),
            members: HashMap::new(),
            visits: 0,
        }
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
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let (count, _) = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    None,
                    entity_id,
                    period,
                )?;
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
                let (_, total) = self.fold_related(
                    expr,
                    (relation, *current_slot, *related_slot),
                    where_clause.as_deref(),
                    Some(value),
                    entity_id,
                    period,
                )?;
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
            if !self
                .eval_judgment_expr(predicate, related_id, period)?
                .is_holds()
            {
                return Ok(None);
            }
        }
        match value {
            Some(value) => Ok(Some(self.eval_related_value(value, related_id, period)?)),
            None => Ok(Some(Decimal::ZERO)),
        }
    }

    fn eval_related_value(
        &mut self,
        value: &'a RelatedValueRef,
        entity_id: &str,
        period: &Period,
    ) -> Eval<Decimal> {
        let scalar = match value {
            RelatedValueRef::Input(name) => self.lookup_input(name, entity_id, period)?,
            RelatedValueRef::Derived(name) => {
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                });
                self.scalar_rule(name, entity_id, period)?
            }
        };
        Ok(scalar.as_decimal().ok_or_else(|| {
            EvalError::TypeMismatch("related aggregation requires numeric values".to_string())
        })?)
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
            direct,
            candidates,
            mut next,
            mut selected,
        } = match resume {
            Some(resume) => resume,
            None => {
                let direct = self
                    .relation_index
                    .get(&(relation.to_string(), current_slot, entity_id.to_string()))
                    .into_iter()
                    .flat_map(|records| records.iter().copied())
                    .filter(|record| record.interval.contains_period(period))
                    .filter_map(|record| record.tuple.get(related_slot).cloned())
                    .collect::<Vec<String>>();
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
                    direct,
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
                                direct,
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

        let mut related_ids = direct;
        related_ids.extend(selected);
        related_ids.sort();
        related_ids.dedup();
        self.members
            .insert(key, Progress::Done(related_ids.clone()));
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
