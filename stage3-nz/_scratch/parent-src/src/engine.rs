use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, JudgmentExpr, JudgmentOutcome, Period,
    Program, RelatedValueRef, ScalarExpr, ScalarValue,
};

#[derive(Debug, Error)]
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
}

impl<'a> Engine<'a> {
    pub fn new(program: &'a Program, data: &'a DataSet) -> Self {
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
        }
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
        self.execution_trace.entry(key.clone()).or_default();
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
        let value = evaluated?;
        // Apply the rule's opt-in output rounding before caching, so the
        // rounded value is what both direct queries and dependent rules
        // (`ScalarExpr::Derived`) observe. Absent `rounding` is a no-op. When
        // rounding actually moves the value, keep the pre-rounding amount so the
        // trace can show the rounding step.
        let rounded = apply_output_rounding(&derived, value.clone());
        if rounded != value {
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
        self.execution_trace.entry(key.clone()).or_default();
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
        let value = evaluated?;
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
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        let trace = self.execution_trace.entry(parent).or_default();
        if !trace.dependencies.contains(&key) {
            trace.dependencies.push(key);
        }
    }

    fn record_skipped_dependency(&mut self, dependency: SkippedTraceDependency) {
        let Some(parent) = self.active_evaluations.last().cloned() else {
            return;
        };
        let trace = self.execution_trace.entry(parent).or_default();
        if !trace.not_evaluated_dependencies.contains(&dependency) {
            trace.not_evaluated_dependencies.push(dependency);
        }
    }

    fn record_parameter_read(&mut self, read: ParameterTraceRead) {
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
        expr: &ScalarExpr,
        entity_id: &str,
        period: &Period,
        reason: TraceSkipReason,
    ) {
        let mut derived = Vec::new();
        let mut parameters = Vec::new();
        collect_scalar_trace_references(expr, &mut derived, &mut parameters);
        for name in derived {
            self.record_skipped_dependency(SkippedTraceDependency::Derived {
                key: CacheKey {
                    derived: name,
                    entity_id: entity_id.to_string(),
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

    fn eval_scalar_expr(
        &mut self,
        expr: &ScalarExpr,
        entity_id: &str,
        period: &Period,
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
                self.record_evaluated_dependency(CacheKey {
                    derived: name.clone(),
                    entity_id: entity_id.to_string(),
                    period: period.clone(),
                });
                self.evaluate_scalar(name, entity_id, period)
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                let lookup_key = self
                    .eval_scalar_expr(index, entity_id, period)?
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
                    total += self.eval_decimal(item, entity_id, period)?;
                }
                Ok(ScalarValue::Decimal(total))
            }
            ScalarExpr::Sub(left, right) => Ok(ScalarValue::Decimal(
                self.eval_decimal(left, entity_id, period)?
                    - self.eval_decimal(right, entity_id, period)?,
            )),
            ScalarExpr::Mul(left, right) => Ok(ScalarValue::Decimal(
                self.eval_decimal(left, entity_id, period)?
                    * self.eval_decimal(right, entity_id, period)?,
            )),
            ScalarExpr::Div(left, right) => {
                let divisor = self.eval_decimal(right, entity_id, period)?;
                if divisor.is_zero() {
                    return Err(EvalError::DivisionByZero);
                }
                Ok(ScalarValue::Decimal(
                    self.eval_decimal(left, entity_id, period)? / divisor,
                ))
            }
            ScalarExpr::Max(items) => {
                let mut iter = items.iter();
                let Some(first) = iter.next() else {
                    return Err(EvalError::TypeMismatch(
                        "max() requires at least one operand".to_string(),
                    ));
                };
                let mut best = self.eval_decimal(first, entity_id, period)?;
                for item in iter {
                    let candidate = self.eval_decimal(item, entity_id, period)?;
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
                let mut best = self.eval_decimal(first, entity_id, period)?;
                for item in iter {
                    let candidate = self.eval_decimal(item, entity_id, period)?;
                    if candidate < best {
                        best = candidate;
                    }
                }
                Ok(ScalarValue::Decimal(best))
            }
            ScalarExpr::Ceil(value) => Ok(ScalarValue::Decimal(
                self.eval_decimal(value, entity_id, period)?.ceil(),
            )),
            ScalarExpr::Floor(value) => Ok(ScalarValue::Decimal(
                self.eval_decimal(value, entity_id, period)?.floor(),
            )),
            ScalarExpr::PeriodStart => Ok(ScalarValue::Date(period.start)),
            ScalarExpr::PeriodEnd => Ok(ScalarValue::Date(period.end)),
            ScalarExpr::DateAddDays { date, days } => {
                let base = self
                    .eval_scalar_expr(date, entity_id, period)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_days expects a date on the left".to_string(),
                        )
                    })?;
                let offset = self
                    .eval_scalar_expr(days, entity_id, period)?
                    .as_index()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "date_add_days expects an integer day count on the right".to_string(),
                        )
                    })?;
                Ok(ScalarValue::Date(base + chrono::Duration::days(offset)))
            }
            ScalarExpr::DaysBetween { from, to } => {
                let a = self
                    .eval_scalar_expr(from, entity_id, period)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "days_between expects a date for `from`".to_string(),
                        )
                    })?;
                let b = self
                    .eval_scalar_expr(to, entity_id, period)?
                    .as_date()
                    .ok_or_else(|| {
                        EvalError::TypeMismatch("days_between expects a date for `to`".to_string())
                    })?;
                Ok(ScalarValue::Integer((b - a).num_days()))
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
                let mut count = 0_i64;
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
                Ok(ScalarValue::Integer(count))
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
                    total += self.eval_related_value(value, &related_id, period)?;
                }
                Ok(ScalarValue::Decimal(total))
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                if self
                    .eval_judgment_expr(condition, entity_id, period)?
                    .is_holds()
                {
                    self.record_skipped_scalar_dependencies(
                        else_expr,
                        entity_id,
                        period,
                        TraceSkipReason::BranchNotSelected,
                    );
                    self.eval_scalar_expr(then_expr, entity_id, period)
                } else {
                    self.record_skipped_scalar_dependencies(
                        then_expr,
                        entity_id,
                        period,
                        TraceSkipReason::BranchNotSelected,
                    );
                    self.eval_scalar_expr(else_expr, entity_id, period)
                }
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
                let left_value = self.eval_scalar_expr(left, entity_id, period)?;
                let right_value = self.eval_scalar_expr(right, entity_id, period)?;
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
                let context = relation_context.ok_or_else(|| {
                    EvalError::TypeMismatch(format!(
                        "relation predicate `{relation}` can only be evaluated inside a derived relation"
                    ))
                })?;
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
    ) -> Result<Decimal, EvalError> {
        self.eval_scalar_expr(expr, entity_id, period)?
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
        let schema = self
            .program
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_string()))?;
        if current_slot >= schema.arity || related_slot >= schema.arity {
            return Err(EvalError::TypeMismatch(format!(
                "relation `{relation}` has arity {}, but slots {current_slot} and {related_slot} were requested",
                schema.arity
            )));
        }

        let mut related_ids = self
            .relation_index
            .get(&(relation.to_string(), current_slot, entity_id.to_string()))
            .into_iter()
            .flat_map(|records| records.iter().copied())
            .filter(|record| record.interval.contains_period(period))
            .filter_map(|record| record.tuple.get(related_slot).cloned())
            .collect::<Vec<String>>();

        if let Some(derivation) = schema.derivation.clone() {
            let mut derived_ids = Vec::new();
            for related_id in self.related_entity_ids(
                &derivation.source_relation,
                derivation.current_slot,
                derivation.related_slot,
                entity_id,
                period,
            )? {
                let context = RelationEvalContext {
                    current_id: entity_id,
                    related_id: &related_id,
                    current_entity: derivation
                        .slot_entities
                        .get(derivation.current_slot)
                        .map(String::as_str),
                    related_entity: derivation
                        .slot_entities
                        .get(derivation.related_slot)
                        .map(String::as_str),
                };
                if self
                    .eval_judgment_expr_inner(
                        &derivation.predicate,
                        &related_id,
                        period,
                        Some(context),
                    )?
                    .is_holds()
                {
                    derived_ids.push(related_id);
                }
            }
            related_ids.extend(derived_ids);
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
