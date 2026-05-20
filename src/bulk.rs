use std::collections::{BTreeMap, HashMap, HashSet};

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::api::{
    ExecutionMetadata, ExecutionMode, ExecutionQuery, ExecutionResponse, OutputValue, QueryResult,
};
use crate::engine::EvalError;
use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, IndexedParameter, JudgmentExpr,
    JudgmentOutcome, Period, Program, RelatedValueRef, ScalarExpr, ScalarValue,
};
use crate::spec::{DTypeSpec, JudgmentOutcomeSpec, PeriodSpec, ScalarValueSpec};

#[derive(Clone, Debug)]
enum ScalarColumn {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<Decimal>),
    Text(Vec<String>),
}

impl ScalarColumn {
    fn as_decimal_vec(&self) -> Result<Vec<Decimal>, EvalError> {
        match self {
            Self::Integer(values) => Ok(values.iter().map(|value| Decimal::from(*value)).collect()),
            Self::Decimal(values) => Ok(values.clone()),
            _ => Err(EvalError::TypeMismatch(
                "expected decimal-compatible bulk column".to_string(),
            )),
        }
    }

    fn as_index_vec(&self) -> Result<Vec<i64>, EvalError> {
        match self {
            Self::Integer(values) => Ok(values.clone()),
            Self::Decimal(values) => values
                .iter()
                .map(|value| {
                    value.to_i64().ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "parameter key for bulk lookup must be integral".to_string(),
                        )
                    })
                })
                .collect(),
            _ => Err(EvalError::TypeMismatch(
                "parameter key for bulk lookup must be numeric".to_string(),
            )),
        }
    }

    fn scalar_value_at(&self, index: usize, dtype: &DType) -> ScalarValue {
        match (self, dtype) {
            (Self::Bool(values), _) => ScalarValue::Bool(values[index]),
            (Self::Integer(values), DType::Integer) => ScalarValue::Integer(values[index]),
            (Self::Integer(values), _) => ScalarValue::Decimal(Decimal::from(values[index])),
            (Self::Decimal(values), _) => ScalarValue::Decimal(values[index]),
            (Self::Text(values), _) => ScalarValue::Text(values[index].clone()),
        }
    }
}

pub fn try_execute(
    program: &Program,
    data: &DataSet,
    queries: &[ExecutionQuery],
) -> Result<FastPathResult, EvalError> {
    if program
        .relations
        .values()
        .any(|relation| relation.derivation.is_some())
    {
        return Ok(FastPathResult::Unsupported {
            reason: "fast mode currently does not support derived relations".to_string(),
        });
    }
    if queries.is_empty() {
        return Ok(FastPathResult::Executed(empty_response()));
    }

    let Some(first_period) = queries.first().map(|query| &query.period) else {
        return Ok(FastPathResult::Executed(empty_response()));
    };
    if queries.iter().any(|query| query.period != *first_period) {
        return Ok(FastPathResult::Unsupported {
            reason: "fast mode currently requires all queries in a batch to share one period"
                .to_string(),
        });
    }

    let period = first_period
        .to_model()
        .map_err(|error| EvalError::TypeMismatch(error.to_string()))?;
    let entity_ids = queries
        .iter()
        .map(|query| query.entity_id.clone())
        .collect::<Vec<String>>();

    let mut evaluator = BulkEvaluator::new(program, data, period, entity_ids);
    let mut results = Vec::with_capacity(queries.len());
    let mut requested = HashSet::new();
    for query in queries {
        for output_reference in &query.outputs {
            let output_name = program
                .resolve_derived_name(output_reference)
                .ok_or_else(|| EvalError::UnknownDerived(output_reference.clone()))?;
            requested.insert(output_name.to_string());
        }
    }

    for output in &requested {
        let derived = evaluator.get_derived(output)?;
        let warmup = match &derived.semantics {
            DerivedSemantics::Scalar(_) => evaluator.evaluate_scalar(output).map(|_| ()),
            DerivedSemantics::Judgment(_) => evaluator.evaluate_judgment(output).map(|_| ()),
        };
        if let Err(error) = warmup {
            if let Some(reason) = unsupported_reason(&error) {
                return Ok(FastPathResult::Unsupported { reason });
            }
            return Err(error);
        }
    }

    for (row_index, query) in queries.iter().enumerate() {
        let mut outputs = BTreeMap::new();
        for output_reference in &query.outputs {
            let output_name = program
                .resolve_derived_name(output_reference)
                .ok_or_else(|| EvalError::UnknownDerived(output_reference.clone()))?;
            let derived = evaluator.get_derived(&output_name)?.clone();
            let output_key = derived
                .id
                .clone()
                .unwrap_or_else(|| output_name.to_string());
            match &derived.semantics {
                DerivedSemantics::Scalar(_) => {
                    let column = evaluator.evaluate_scalar(&output_name)?;
                    outputs.insert(
                        output_key,
                        OutputValue::Scalar {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            dtype: DTypeSpec::from_model(&derived.dtype),
                            unit: derived.unit.clone(),
                            value: ScalarValueSpec::from_model(
                                column.scalar_value_at(row_index, &derived.dtype),
                            ),
                        },
                    );
                }
                DerivedSemantics::Judgment(_) => {
                    let column = evaluator.evaluate_judgment(&output_name)?;
                    outputs.insert(
                        output_key,
                        OutputValue::Judgment {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            unit: derived.unit.clone(),
                            outcome: JudgmentOutcomeSpec::from(column[row_index]),
                        },
                    );
                }
            }
        }
        results.push(QueryResult {
            entity_id: query.entity_id.clone(),
            period: PeriodSpec {
                kind: query.period.kind.clone(),
                start: query.period.start,
                end: query.period.end,
            },
            outputs,
            trace: BTreeMap::new(),
        });
    }

    Ok(FastPathResult::Executed(ExecutionResponse {
        metadata: fast_mode_metadata(),
        results,
    }))
}

pub enum FastPathResult {
    Executed(ExecutionResponse),
    Unsupported { reason: String },
}

fn empty_response() -> ExecutionResponse {
    ExecutionResponse {
        metadata: fast_mode_metadata(),
        results: Vec::new(),
    }
}

fn fast_mode_metadata() -> ExecutionMetadata {
    ExecutionMetadata {
        requested_mode: ExecutionMode::Fast,
        actual_mode: ExecutionMode::Fast,
        fallback_reason: None,
    }
}

fn unsupported_reason(error: &EvalError) -> Option<String> {
    match error {
        EvalError::TypeMismatch(message)
            if message.starts_with("bulk execution does not yet support")
                || message.starts_with("bulk fast mode does not yet support")
                || message.starts_with("fast mode does not yet support") =>
        {
            Some(message.clone())
        }
        _ => None,
    }
}

struct BulkEvaluator<'a> {
    program: &'a Program,
    period: Period,
    entity_ids: Vec<String>,
    query_input_cells: HashMap<String, Vec<Option<ScalarValue>>>,
    related_input_index: HashMap<(String, String), ScalarValue>,
    relation_adjacency: HashMap<(String, usize), Vec<Vec<Vec<String>>>>,
    scalar_cache: HashMap<String, ScalarColumn>,
    judgment_cache: HashMap<String, Vec<JudgmentOutcome>>,
}

impl<'a> BulkEvaluator<'a> {
    fn new(
        program: &'a Program,
        data: &'a DataSet,
        period: Period,
        entity_ids: Vec<String>,
    ) -> Self {
        let query_index = entity_ids
            .iter()
            .enumerate()
            .map(|(index, entity_id)| (entity_id.clone(), index))
            .collect::<HashMap<String, usize>>();

        let mut query_input_cells: HashMap<String, Vec<Option<ScalarValue>>> = HashMap::new();
        let mut related_input_index = HashMap::new();
        for record in &data.inputs {
            if record.interval.contains_period(&period) {
                if let Some(&query_row) = query_index.get(&record.entity_id) {
                    query_input_cells
                        .entry(record.name.clone())
                        .or_insert_with(|| vec![None; entity_ids.len()])[query_row] =
                        Some(record.value.clone());
                } else {
                    related_input_index.insert(
                        (record.name.clone(), record.entity_id.clone()),
                        record.value.clone(),
                    );
                }
            }
        }

        let mut relation_adjacency = HashMap::new();
        for record in &data.relations {
            if record.interval.contains_period(&period) {
                for (slot, value) in record.tuple.iter().enumerate() {
                    if let Some(&query_row) = query_index.get(value) {
                        relation_adjacency
                            .entry((record.name.clone(), slot))
                            .or_insert_with(|| vec![Vec::new(); entity_ids.len()])[query_row]
                            .push(record.tuple.clone());
                    }
                }
            }
        }

        Self {
            program,
            period,
            entity_ids,
            query_input_cells,
            related_input_index,
            relation_adjacency,
            scalar_cache: HashMap::new(),
            judgment_cache: HashMap::new(),
        }
    }

    fn get_derived(&self, name: &str) -> Result<&Derived, EvalError> {
        self.program
            .derived
            .get(name)
            .ok_or_else(|| EvalError::UnknownDerived(name.to_string()))
    }

    fn evaluate_scalar(&mut self, name: &str) -> Result<&ScalarColumn, EvalError> {
        if !self.scalar_cache.contains_key(name) {
            let derived = self.get_derived(name)?.clone();
            let column = match &derived.semantics {
                DerivedSemantics::Scalar(expr) => self.eval_scalar_expr(expr)?,
                DerivedSemantics::Judgment(_) => {
                    return Err(EvalError::ExpectedScalar(name.to_string()));
                }
            };
            self.scalar_cache.insert(name.to_string(), column);
        }
        Ok(self.scalar_cache.get(name).expect("column cached"))
    }

    fn evaluate_judgment(&mut self, name: &str) -> Result<&Vec<JudgmentOutcome>, EvalError> {
        if !self.judgment_cache.contains_key(name) {
            let derived = self.get_derived(name)?.clone();
            let column = match &derived.semantics {
                DerivedSemantics::Judgment(expr) => self.eval_judgment_expr(expr)?,
                DerivedSemantics::Scalar(_) => {
                    return Err(EvalError::ExpectedJudgment(name.to_string()));
                }
            };
            self.judgment_cache.insert(name.to_string(), column);
        }
        Ok(self.judgment_cache.get(name).expect("column cached"))
    }

    fn eval_scalar_expr(&mut self, expr: &ScalarExpr) -> Result<ScalarColumn, EvalError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(match value {
                ScalarValue::Bool(value) => ScalarColumn::Bool(vec![*value; self.entity_ids.len()]),
                ScalarValue::Integer(value) => {
                    ScalarColumn::Integer(vec![*value; self.entity_ids.len()])
                }
                ScalarValue::Decimal(value) => {
                    ScalarColumn::Decimal(vec![*value; self.entity_ids.len()])
                }
                ScalarValue::Text(value) => {
                    ScalarColumn::Text(vec![value.clone(); self.entity_ids.len()])
                }
                ScalarValue::Date(_) => {
                    return Err(EvalError::TypeMismatch(
                        "bulk fast mode does not yet support date literals".to_string(),
                    ));
                }
            }),
            ScalarExpr::Input(name) | ScalarExpr::InputOrElse { name, .. } => {
                let default = match expr {
                    ScalarExpr::InputOrElse { default, .. } => Some(default.clone()),
                    _ => None,
                };
                let empty_cells: Vec<Option<ScalarValue>> = if default.is_some() {
                    vec![None; self.entity_ids.len()]
                } else {
                    Vec::new()
                };
                let cells = match self.query_input_cells.get(name) {
                    Some(cells) => cells,
                    None if default.is_some() => &empty_cells,
                    None => {
                        return Err(EvalError::MissingInput {
                            name: name.clone(),
                            entity_id: self.entity_ids.first().cloned().unwrap_or_default(),
                            period_start: self.period.start,
                            period_end: self.period.end,
                        });
                    }
                };

                let mut values = Vec::with_capacity(cells.len());
                let mut saw_bool = false;
                let mut saw_text = false;
                let mut saw_decimal = false;
                for (row_index, cell) in cells.iter().enumerate() {
                    let value = match cell.clone() {
                        Some(value) => value,
                        None => match &default {
                            Some(default) => default.clone(),
                            None => {
                                return Err(EvalError::MissingInput {
                                    name: name.clone(),
                                    entity_id: self.entity_ids[row_index].clone(),
                                    period_start: self.period.start,
                                    period_end: self.period.end,
                                });
                            }
                        },
                    };
                    match &value {
                        ScalarValue::Bool(_) => saw_bool = true,
                        ScalarValue::Text(_) => saw_text = true,
                        ScalarValue::Integer(_) | ScalarValue::Decimal(_) => saw_decimal = true,
                        ScalarValue::Date(_) => {
                            return Err(EvalError::TypeMismatch(
                                "bulk fast mode does not yet support date inputs".to_string(),
                            ));
                        }
                    }
                    values.push(value);
                }
                if saw_bool {
                    Ok(ScalarColumn::Bool(
                        values
                            .into_iter()
                            .map(|value| match value {
                                ScalarValue::Bool(value) => Ok(value),
                                _ => Err(EvalError::TypeMismatch(
                                    "mixed bulk input dtypes are not supported".to_string(),
                                )),
                            })
                            .collect::<Result<Vec<bool>, EvalError>>()?,
                    ))
                } else if saw_text {
                    Ok(ScalarColumn::Text(
                        values
                            .into_iter()
                            .map(|value| match value {
                                ScalarValue::Text(value) => Ok(value),
                                _ => Err(EvalError::TypeMismatch(
                                    "mixed bulk input dtypes are not supported".to_string(),
                                )),
                            })
                            .collect::<Result<Vec<String>, EvalError>>()?,
                    ))
                } else if saw_decimal {
                    let integers_only = values
                        .iter()
                        .all(|value| matches!(value, ScalarValue::Integer(_)));
                    if integers_only {
                        Ok(ScalarColumn::Integer(
                            values
                                .into_iter()
                                .map(|value| match value {
                                    ScalarValue::Integer(value) => Ok(value),
                                    _ => Err(EvalError::TypeMismatch(
                                        "mixed bulk input dtypes are not supported".to_string(),
                                    )),
                                })
                                .collect::<Result<Vec<i64>, EvalError>>()?,
                        ))
                    } else {
                        Ok(ScalarColumn::Decimal(
                            values
                                .into_iter()
                                .map(|value| {
                                    value.as_decimal().ok_or_else(|| {
                                        EvalError::TypeMismatch(
                                            "mixed bulk input dtypes are not supported".to_string(),
                                        )
                                    })
                                })
                                .collect::<Result<Vec<Decimal>, EvalError>>()?,
                        ))
                    }
                } else {
                    Err(EvalError::TypeMismatch(
                        "empty bulk input column".to_string(),
                    ))
                }
            }
            ScalarExpr::Derived(name) => Ok(self.evaluate_scalar(name)?.clone()),
            ScalarExpr::ParameterLookup { parameter, index } => {
                let keys = self.eval_scalar_expr(index)?.as_index_vec()?;
                let parameter = self
                    .program
                    .parameters
                    .get(parameter)
                    .ok_or_else(|| EvalError::UnknownParameter(parameter.clone()))?;
                Ok(lookup_parameter_bulk(parameter, &keys, &self.period)?)
            }
            ScalarExpr::Add(items) => {
                let mut total = vec![Decimal::ZERO; self.entity_ids.len()];
                for item in items {
                    let values = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in values.into_iter().enumerate() {
                        total[index] += value;
                    }
                }
                Ok(ScalarColumn::Decimal(total))
            }
            ScalarExpr::Sub(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(ScalarColumn::Decimal(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| left - right)
                        .collect(),
                ))
            }
            ScalarExpr::Mul(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(ScalarColumn::Decimal(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| left * right)
                        .collect(),
                ))
            }
            ScalarExpr::Div(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(ScalarColumn::Decimal(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| {
                            if right.is_zero() {
                                Err(EvalError::DivisionByZero)
                            } else {
                                Ok(left / right)
                            }
                        })
                        .collect::<Result<Vec<Decimal>, EvalError>>()?,
                ))
            }
            ScalarExpr::Max(items) => {
                let mut values = vec![Decimal::MIN; self.entity_ids.len()];
                for item in items {
                    let candidate = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in candidate.into_iter().enumerate() {
                        if value > values[index] {
                            values[index] = value;
                        }
                    }
                }
                Ok(ScalarColumn::Decimal(values))
            }
            ScalarExpr::Min(items) => {
                let mut values = vec![Decimal::MAX; self.entity_ids.len()];
                for item in items {
                    let candidate = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in candidate.into_iter().enumerate() {
                        if value < values[index] {
                            values[index] = value;
                        }
                    }
                }
                Ok(ScalarColumn::Decimal(values))
            }
            ScalarExpr::Ceil(value) => Ok(ScalarColumn::Decimal(
                self.eval_scalar_expr(value)?
                    .as_decimal_vec()?
                    .into_iter()
                    .map(|value| value.ceil())
                    .collect(),
            )),
            ScalarExpr::Floor(value) => Ok(ScalarColumn::Decimal(
                self.eval_scalar_expr(value)?
                    .as_decimal_vec()?
                    .into_iter()
                    .map(|value| value.floor())
                    .collect(),
            )),
            ScalarExpr::PeriodStart | ScalarExpr::PeriodEnd => Err(EvalError::TypeMismatch(
                "bulk fast mode does not yet support period_start / period_end".to_string(),
            )),
            ScalarExpr::DateAddDays { .. } => Err(EvalError::TypeMismatch(
                "bulk fast mode does not yet support date_add_days".to_string(),
            )),
            ScalarExpr::DaysBetween { .. } => Err(EvalError::TypeMismatch(
                "bulk fast mode does not yet support days_between".to_string(),
            )),
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                if where_clause.is_some() {
                    return Err(EvalError::TypeMismatch(
                        "bulk fast mode does not yet support count_related where-clauses"
                            .to_string(),
                    ));
                }
                let mut values = Vec::with_capacity(self.entity_ids.len());
                for row_index in 0..self.entity_ids.len() {
                    let related = self.related_rows(relation, *current_slot, row_index)?;
                    let count = related
                        .iter()
                        .filter(|tuple| tuple.get(*related_slot).is_some())
                        .count() as i64;
                    values.push(count);
                }
                Ok(ScalarColumn::Integer(values))
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                if where_clause.is_some() {
                    return Err(EvalError::TypeMismatch(
                        "bulk fast mode does not yet support sum_related where-clauses".to_string(),
                    ));
                }
                let mut totals = Vec::with_capacity(self.entity_ids.len());
                for row_index in 0..self.entity_ids.len() {
                    let related_ids = self
                        .related_rows(relation, *current_slot, row_index)?
                        .iter()
                        .filter_map(|tuple| tuple.get(*related_slot).cloned())
                        .collect::<Vec<String>>();
                    let mut total = Decimal::ZERO;
                    match value {
                        RelatedValueRef::Input(name) => {
                            for related_id in related_ids {
                                let scalar = self
                                    .related_input_index
                                    .get(&(name.clone(), related_id.clone()))
                                    .cloned()
                                    .ok_or_else(|| EvalError::MissingInput {
                                        name: name.clone(),
                                        entity_id: related_id,
                                        period_start: self.period.start,
                                        period_end: self.period.end,
                                    })?;
                                total += scalar.as_decimal().ok_or_else(|| {
                                    EvalError::TypeMismatch(
                                        "related aggregation requires numeric values".to_string(),
                                    )
                                })?;
                            }
                        }
                        RelatedValueRef::Derived(_) => return Err(EvalError::TypeMismatch(
                            "bulk execution does not yet support aggregating related derived values"
                                .to_string(),
                        )),
                    }
                    totals.push(total);
                }
                Ok(ScalarColumn::Decimal(totals))
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.eval_judgment_expr(condition)?;
                let then_values = self.eval_scalar_expr(then_expr)?;
                let else_values = self.eval_scalar_expr(else_expr)?;
                select_scalar_column(condition, then_values, else_values)
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &JudgmentExpr,
    ) -> Result<Vec<JudgmentOutcome>, EvalError> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                let left = self.eval_scalar_expr(left)?;
                let right = self.eval_scalar_expr(right)?;
                compare_columns(left, *op, right)
            }
            JudgmentExpr::Derived(name) => Ok(self.evaluate_judgment(name)?.clone()),
            JudgmentExpr::RelationMember { relation, .. } => Err(EvalError::TypeMismatch(
                format!("fast mode cannot evaluate relation predicate `{relation}`"),
            )),
            JudgmentExpr::And(items) => {
                let mut results = vec![JudgmentOutcome::Holds; self.entity_ids.len()];
                for item in items {
                    let values = self.eval_judgment_expr(item)?;
                    for (index, value) in values.into_iter().enumerate() {
                        results[index] = match (results[index], value) {
                            (JudgmentOutcome::NotHolds, _) | (_, JudgmentOutcome::NotHolds) => {
                                JudgmentOutcome::NotHolds
                            }
                            (JudgmentOutcome::Undetermined, _)
                            | (_, JudgmentOutcome::Undetermined) => JudgmentOutcome::Undetermined,
                            _ => JudgmentOutcome::Holds,
                        };
                    }
                }
                Ok(results)
            }
            JudgmentExpr::Or(items) => {
                let mut results = vec![JudgmentOutcome::NotHolds; self.entity_ids.len()];
                for item in items {
                    let values = self.eval_judgment_expr(item)?;
                    for (index, value) in values.into_iter().enumerate() {
                        results[index] = match (results[index], value) {
                            (JudgmentOutcome::Holds, _) | (_, JudgmentOutcome::Holds) => {
                                JudgmentOutcome::Holds
                            }
                            (JudgmentOutcome::Undetermined, _)
                            | (_, JudgmentOutcome::Undetermined) => JudgmentOutcome::Undetermined,
                            _ => JudgmentOutcome::NotHolds,
                        };
                    }
                }
                Ok(results)
            }
            JudgmentExpr::Not(item) => Ok(self
                .eval_judgment_expr(item)?
                .into_iter()
                .map(|value| match value {
                    JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
                    JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
                    JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
                })
                .collect()),
        }
    }

    fn related_rows(
        &self,
        relation: &str,
        slot: usize,
        row_index: usize,
    ) -> Result<&Vec<Vec<String>>, EvalError> {
        self.relation_adjacency
            .get(&(relation.to_string(), slot))
            .and_then(|rows| rows.get(row_index))
            .ok_or_else(|| {
                EvalError::UnknownRelation(format!(
                    "{relation}::{}",
                    self.entity_ids.get(row_index).cloned().unwrap_or_default()
                ))
            })
    }
}

fn lookup_parameter_bulk(
    parameter: &IndexedParameter,
    keys: &[i64],
    period: &Period,
) -> Result<ScalarColumn, EvalError> {
    let version = parameter
        .versions
        .iter()
        .filter(|version| version.effective_from <= period.start)
        .max_by_key(|version| version.effective_from)
        .ok_or_else(|| EvalError::MissingParameterValue {
            parameter: parameter.name.clone(),
            key: keys.first().copied().unwrap_or_default(),
            at: period.start,
        })?;

    let values = keys
        .iter()
        .map(|key| {
            version
                .values
                .get(key)
                .cloned()
                .ok_or_else(|| EvalError::MissingParameterValue {
                    parameter: parameter.name.clone(),
                    key: *key,
                    at: period.start,
                })
        })
        .collect::<Result<Vec<ScalarValue>, EvalError>>()?;

    if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Integer(_)))
    {
        Ok(ScalarColumn::Integer(
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
    } else {
        Ok(ScalarColumn::Decimal(
            values
                .into_iter()
                .map(|value| {
                    value.as_decimal().ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "parameter values must be numeric in bulk mode".to_string(),
                        )
                    })
                })
                .collect::<Result<Vec<Decimal>, EvalError>>()?,
        ))
    }
}

fn select_scalar_column(
    condition: Vec<JudgmentOutcome>,
    then_values: ScalarColumn,
    else_values: ScalarColumn,
) -> Result<ScalarColumn, EvalError> {
    match (then_values, else_values) {
        (ScalarColumn::Decimal(then_values), ScalarColumn::Decimal(else_values)) => {
            Ok(ScalarColumn::Decimal(
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
        (ScalarColumn::Integer(then_values), ScalarColumn::Integer(else_values)) => {
            Ok(ScalarColumn::Integer(
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
        (ScalarColumn::Decimal(then_values), ScalarColumn::Integer(else_values)) => {
            Ok(ScalarColumn::Decimal(
                condition
                    .into_iter()
                    .zip(then_values)
                    .zip(else_values)
                    .map(|((condition, then_value), else_value)| {
                        if condition.is_holds() {
                            then_value
                        } else {
                            Decimal::from(else_value)
                        }
                    })
                    .collect(),
            ))
        }
        (ScalarColumn::Integer(then_values), ScalarColumn::Decimal(else_values)) => {
            Ok(ScalarColumn::Decimal(
                condition
                    .into_iter()
                    .zip(then_values)
                    .zip(else_values)
                    .map(|((condition, then_value), else_value)| {
                        if condition.is_holds() {
                            Decimal::from(then_value)
                        } else {
                            else_value
                        }
                    })
                    .collect(),
            ))
        }
        (ScalarColumn::Bool(then_values), ScalarColumn::Bool(else_values)) => {
            Ok(ScalarColumn::Bool(
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
        (ScalarColumn::Text(then_values), ScalarColumn::Text(else_values)) => {
            Ok(ScalarColumn::Text(
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
        _ => Err(EvalError::TypeMismatch(
            "bulk if() branches must have the same dtype".to_string(),
        )),
    }
}

fn compare_columns(
    left: ScalarColumn,
    op: ComparisonOp,
    right: ScalarColumn,
) -> Result<Vec<JudgmentOutcome>, EvalError> {
    match (left, right) {
        (ScalarColumn::Bool(left), ScalarColumn::Bool(right)) => Ok(left
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
        (ScalarColumn::Text(left), ScalarColumn::Text(right)) => Ok(left
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
        (left, right) => {
            let left = left.as_decimal_vec()?;
            let right = right.as_decimal_vec()?;
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
