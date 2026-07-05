use std::collections::HashMap;

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
    LifetimeAmbiguousLeaf(&'static str),
    #[error("sum_top_n_over_periods requires n >= 1, got {0}")]
    OverPeriodsTopNInvalid(i64),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    derived: String,
    entity_id: String,
    period: Period,
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
        let value = match semantics {
            DerivedSemantics::Scalar(expr) => self.eval_scalar_expr(expr, entity_id, period)?,
            DerivedSemantics::Judgment(_) => {
                return Err(EvalError::ExpectedScalar(derived_name.to_string()));
            }
        };
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
        let value = match semantics {
            DerivedSemantics::Judgment(expr) => self.eval_judgment_expr(expr, entity_id, period)?,
            DerivedSemantics::Scalar(_) => {
                return Err(EvalError::ExpectedJudgment(derived_name.to_string()));
            }
        };
        self.judgment_cache.insert(key, value);
        Ok(value)
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
            ScalarExpr::Derived(name) => self.evaluate_scalar(name, entity_id, period),
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
                    self.eval_scalar_expr(then_expr, entity_id, period)
                } else {
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
                for item in items {
                    match self.eval_judgment_expr_inner(
                        item,
                        entity_id,
                        period,
                        relation_context,
                    )? {
                        JudgmentOutcome::Holds => {}
                        JudgmentOutcome::NotHolds => return Ok(JudgmentOutcome::NotHolds),
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
                for item in items {
                    match self.eval_judgment_expr_inner(
                        item,
                        entity_id,
                        period,
                        relation_context,
                    )? {
                        JudgmentOutcome::Holds => return Ok(JudgmentOutcome::Holds),
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
            RelatedValueRef::Derived(name) => self.evaluate_scalar(name, entity_id, period)?,
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
        self.input_index
            .get(&(name.to_string(), entity_id.to_string()))
            .into_iter()
            .flat_map(|records| records.iter().copied())
            .find(|record| record.interval.contains_period(period))
            .map(|record| record.value.clone())
            .ok_or_else(|| EvalError::MissingInput {
                name: name.to_string(),
                entity_id: entity_id.to_string(),
                period_start: period.start,
                period_end: period.end,
            })
    }

    fn lookup_parameter(
        &self,
        name: &str,
        key: i64,
        period: &Period,
    ) -> Result<ScalarValue, EvalError> {
        let parameter = self
            .program
            .parameters
            .get(name)
            .ok_or_else(|| EvalError::UnknownParameter(name.to_string()))?;
        let version = parameter
            .versions
            .iter()
            .filter(|version| version.effective_from <= period.start)
            .max_by_key(|version| version.effective_from)
            .ok_or_else(|| EvalError::MissingParameterValue {
                parameter: name.to_string(),
                key,
                at: period.start,
            })?;
        version
            .values
            .get(&key)
            .cloned()
            .ok_or_else(|| EvalError::MissingParameterValue {
                parameter: name.to_string(),
                key,
                at: period.start,
            })
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
