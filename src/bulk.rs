//! The fast (bulk) evaluator: one columnar pass over every query row of a
//! request, computing exactly what the explain interpreter computes.
//!
//! Every expression node is evaluated under a [`RowMask`] of the rows whose
//! reference evaluation reaches it, and errors are recorded per row
//! ([`RowErrors`]) rather than aborting the batch. `if` evaluates each branch
//! only for the rows that select it, `and`/`or` only for undecided rows, and a
//! derived rule only for the rows that reach a reference to it. The request
//! then fails with the first failing (query, output) in explain's order, or
//! not at all. See `docs/execution-semantics.md`.
//!
//! rust_decimal and chrono operators panic on overflow. Evaluator arithmetic
//! uses the checked helpers in engine.rs instead (see clippy.toml); an
//! overflow fails its row like any other evaluation error.
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{BTreeMap, HashMap};

use rust_decimal::Decimal;

use crate::api::{
    ExecutionMetadata, ExecutionMode, ExecutionQuery, ExecutionResponse, OutputValue, QueryResult,
};
use crate::engine::{
    ArithmeticError, Engine, EvalError, checked_add, checked_div, checked_mul, checked_sub,
    compare_scalar_values, relation_member_outside_derived_relation,
};
use crate::lazy::{RowErrors, RowMask};
use crate::model::{
    ComparisonOp, DataSet, DerivedSemantics, JudgmentExpr, JudgmentOutcome, Period, Program,
    ScalarExpr, ScalarValue,
};
use crate::spec::{DTypeSpec, JudgmentOutcomeSpec, PeriodSpec, ScalarValueSpec};

/// Values of one scalar node for every row. Rows outside the node's mask, and
/// rows that failed, hold an unspecified placeholder.
#[derive(Clone, Debug)]
enum ScalarColumn {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<Decimal>),
    Text(Vec<String>),
    /// Rows whose values differ in kind, such as an integer branch selected on
    /// some rows and a decimal one on others. Each row keeps the exact value
    /// the explain path produces for it.
    Mixed(Vec<ScalarValue>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Bool,
    Integer,
    Decimal,
    Text,
}

fn value_kind(value: &ScalarValue) -> Option<ValueKind> {
    match value {
        ScalarValue::Bool(_) => Some(ValueKind::Bool),
        ScalarValue::Integer(_) => Some(ValueKind::Integer),
        ScalarValue::Decimal(_) => Some(ValueKind::Decimal),
        ScalarValue::Text(_) => Some(ValueKind::Text),
        ScalarValue::Date(_) => None,
    }
}

impl ScalarColumn {
    fn placeholder(len: usize) -> Self {
        Self::Integer(vec![0; len])
    }

    fn broadcast(value: &ScalarValue, len: usize) -> Self {
        match value {
            ScalarValue::Bool(value) => Self::Bool(vec![*value; len]),
            ScalarValue::Integer(value) => Self::Integer(vec![*value; len]),
            ScalarValue::Decimal(value) => Self::Decimal(vec![*value; len]),
            ScalarValue::Text(value) => Self::Text(vec![value.clone(); len]),
            ScalarValue::Date(_) => Self::Mixed(vec![value.clone(); len]),
        }
    }

    /// Build a column from the values of the rows that produced one. Callers
    /// reject date values first: bulk columns never hold dates.
    fn from_entries(len: usize, entries: Vec<(usize, ScalarValue)>) -> Self {
        let mut kind = None;
        let mut mixed = false;
        for (_, value) in &entries {
            let value_kind = value_kind(value);
            match kind {
                None => kind = value_kind,
                Some(existing) if Some(existing) != value_kind => mixed = true,
                Some(_) => {}
            }
        }
        if mixed {
            let mut values = vec![ScalarValue::Integer(0); len];
            for (row, value) in entries {
                values[row] = value;
            }
            return Self::Mixed(values);
        }
        match kind {
            None => Self::placeholder(len),
            Some(ValueKind::Bool) => {
                let mut values = vec![false; len];
                for (row, value) in entries {
                    if let ScalarValue::Bool(value) = value {
                        values[row] = value;
                    }
                }
                Self::Bool(values)
            }
            Some(ValueKind::Integer) => {
                let mut values = vec![0; len];
                for (row, value) in entries {
                    if let ScalarValue::Integer(value) = value {
                        values[row] = value;
                    }
                }
                Self::Integer(values)
            }
            Some(ValueKind::Decimal) => {
                let mut values = vec![Decimal::ZERO; len];
                for (row, value) in entries {
                    if let ScalarValue::Decimal(value) = value {
                        values[row] = value;
                    }
                }
                Self::Decimal(values)
            }
            Some(ValueKind::Text) => {
                let mut values = vec![String::new(); len];
                for (row, value) in entries {
                    if let ScalarValue::Text(value) = value {
                        values[row] = value;
                    }
                }
                Self::Text(values)
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::Integer(values) => values.len(),
            Self::Decimal(values) => values.len(),
            Self::Text(values) => values.len(),
            Self::Mixed(values) => values.len(),
        }
    }

    /// The row's value, of exactly the kind the explain path returns.
    fn value_at(&self, row: usize) -> ScalarValue {
        match self {
            Self::Bool(values) => ScalarValue::Bool(values[row]),
            Self::Integer(values) => ScalarValue::Integer(values[row]),
            Self::Decimal(values) => ScalarValue::Decimal(values[row]),
            Self::Text(values) => ScalarValue::Text(values[row].clone()),
            Self::Mixed(values) => values[row].clone(),
        }
    }

    /// Read the live rows as numbers, recording explain's type error for a row
    /// whose value is not numeric.
    fn decimals(&self, live: &RowMask, errors: &mut RowErrors) -> Vec<Decimal> {
        match self {
            Self::Integer(values) => values.iter().map(|value| Decimal::from(*value)).collect(),
            Self::Decimal(values) => values.clone(),
            Self::Mixed(values) => {
                let mut numbers = vec![Decimal::ZERO; values.len()];
                for row in live.rows() {
                    match values[row].as_decimal() {
                        Some(value) => numbers[row] = value,
                        None => errors.record(row, not_numeric()),
                    }
                }
                numbers
            }
            Self::Bool(_) | Self::Text(_) => {
                errors.record_all(live, &not_numeric());
                vec![Decimal::ZERO; self.len()]
            }
        }
    }

    /// Overwrite `rows` with `source`'s values, widening to [`Self::Mixed`]
    /// when the two columns hold different kinds.
    fn assign_rows(&mut self, source: &ScalarColumn, rows: &RowMask) {
        match (&mut *self, source) {
            (Self::Bool(target), Self::Bool(source)) => {
                for row in rows.rows() {
                    target[row] = source[row];
                }
            }
            (Self::Integer(target), Self::Integer(source)) => {
                for row in rows.rows() {
                    target[row] = source[row];
                }
            }
            (Self::Decimal(target), Self::Decimal(source)) => {
                for row in rows.rows() {
                    target[row] = source[row];
                }
            }
            (Self::Text(target), Self::Text(source)) => {
                for row in rows.rows() {
                    target[row] = source[row].clone();
                }
            }
            (Self::Mixed(target), source) => {
                for row in rows.rows() {
                    target[row] = source.value_at(row);
                }
            }
            (target, source) => {
                let mut values = (0..target.len())
                    .map(|row| target.value_at(row))
                    .collect::<Vec<_>>();
                for row in rows.rows() {
                    values[row] = source.value_at(row);
                }
                *target = Self::Mixed(values);
            }
        }
    }

    /// The per-row choice of an `if`: `then_values` on `then_rows`,
    /// `else_values` on `else_rows`. A branch no row selects contributes
    /// nothing, not even its kind.
    fn select(
        then_rows: &RowMask,
        then_values: ScalarColumn,
        else_rows: &RowMask,
        else_values: ScalarColumn,
    ) -> Self {
        if then_rows.is_empty() {
            return else_values;
        }
        if else_rows.is_empty() {
            return then_values;
        }
        let mut selected = then_values;
        selected.assign_rows(&else_values, else_rows);
        selected
    }

    /// Round decimal values under a rule's currency rounding. Integers pass
    /// through unchanged, as they do in the explain path.
    fn rounded(self, rounding: crate::model::Rounding) -> Self {
        match self {
            Self::Decimal(values) => Self::Decimal(
                values
                    .into_iter()
                    .map(|value| rounding.apply(value))
                    .collect(),
            ),
            Self::Mixed(values) => Self::Mixed(
                values
                    .into_iter()
                    .map(|value| match value {
                        ScalarValue::Decimal(amount) => {
                            ScalarValue::Decimal(rounding.apply(amount))
                        }
                        other => other,
                    })
                    .collect(),
            ),
            other => other,
        }
    }
}

fn not_numeric() -> EvalError {
    EvalError::TypeMismatch("expected numeric scalar".to_string())
}

fn unsupported(construct: &str) -> EvalError {
    EvalError::TypeMismatch(format!("bulk fast mode does not yet support {construct}"))
}

pub fn try_execute(
    program: &Program,
    data: &DataSet,
    queries: &[ExecutionQuery],
) -> Result<FastPathResult, EvalError> {
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
    let row_count = queries.len();

    // Resolve every requested output before evaluating any, in request
    // order. A parameter output or an unknown one sends the whole request to
    // explain before bulk evaluates anything. Each derived output is then
    // evaluated only for the rows that request it: a row of another entity
    // kind never reads this output's inputs.
    let mut requested: Vec<(String, Vec<bool>)> = Vec::new();
    let mut requested_index: HashMap<String, usize> = HashMap::new();
    for (row, query) in queries.iter().enumerate() {
        for output_reference in &query.outputs {
            let Some(output_name) = program.resolve_derived_name(output_reference) else {
                let reason = if program.resolve_parameter_name(output_reference).is_some() {
                    format!("parameter output `{output_reference}` uses the explain path")
                } else {
                    format!("unknown output `{output_reference}`; explain reports it")
                };
                return Ok(FastPathResult::Unsupported { reason });
            };
            let index = *requested_index
                .entry(output_name.clone())
                .or_insert_with(|| {
                    requested.push((output_name.clone(), vec![false; row_count]));
                    requested.len() - 1
                });
            requested[index].1[row] = true;
        }
    }

    // Evaluate each distinct output once, in request order. Masking makes a
    // recorded error one explain also reaches, but explain's first error
    // (by query, then output) is the request's answer, so the first output
    // bulk cannot answer, because a live row reaches a construct bulk does
    // not support or fails, sends the whole request to explain, which then
    // reports its own first error. Bulk never returns an evaluation error.
    let mut evaluator = BulkEvaluator::new(program, data, period, entity_ids);
    let mut evaluated = HashMap::with_capacity(requested.len());
    for (output_name, rows) in requested {
        let mask = RowMask::from_bits(rows);
        let output = match evaluator.evaluate_output(&output_name, &mask) {
            Ok(output) => output,
            Err(error) => {
                let reason = unsupported_reason(&error).unwrap_or_else(|| {
                    format!("bulk evaluation failed ({error}); explain decides the outcome")
                });
                return Ok(FastPathResult::Unsupported { reason });
            }
        };
        if let Some((_, error)) = output.errors.first() {
            return Ok(FastPathResult::Unsupported {
                reason: format!("bulk evaluation failed ({error}); explain decides the outcome"),
            });
        }
        evaluated.insert(output_name, output);
    }

    // Every requested row succeeded; assemble in request order.
    let mut results = Vec::with_capacity(queries.len());
    for (row_index, query) in queries.iter().enumerate() {
        let mut outputs = BTreeMap::new();
        for output_reference in &query.outputs {
            let output_name = program
                .resolve_derived_name(output_reference)
                .ok_or_else(|| EvalError::UnknownDerived(output_reference.clone()))?;
            let derived = evaluator.get_derived(&output_name)?;
            let output = evaluated
                .get(&output_name)
                .expect("every resolved output was evaluated");
            let output_key = derived
                .id
                .clone()
                .unwrap_or_else(|| output_name.to_string());
            let value = match &output.values {
                OutputColumn::Scalar(column) => OutputValue::Scalar {
                    name: derived.name.clone(),
                    id: derived.id.clone(),
                    dtype: DTypeSpec::from_model(&derived.dtype),
                    unit: derived.unit.clone(),
                    value: ScalarValueSpec::from_model(column.value_at(row_index)),
                },
                OutputColumn::Judgment(outcomes) => OutputValue::Judgment {
                    name: derived.name.clone(),
                    id: derived.id.clone(),
                    unit: derived.unit.clone(),
                    outcome: JudgmentOutcomeSpec::from(outcomes[row_index]),
                },
            };
            outputs.insert(output_key, value);
        }
        results.push(QueryResult {
            entity_id: query.entity_id.clone(),
            period: PeriodSpec {
                kind: query.period.kind.clone(),
                start: query.period.start,
                end: query.period.end,
            },
            assessment_date: query.assessment_date,
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

enum OutputColumn {
    Scalar(ScalarColumn),
    Judgment(Vec<JudgmentOutcome>),
}

struct EvaluatedOutput {
    values: OutputColumn,
    errors: RowErrors,
}

/// A derived rule's column, computed so far for `computed` rows.
struct DerivedColumn<T> {
    values: T,
    errors: RowErrors,
    computed: RowMask,
}

/// Evaluation result of one node: per-row values and per-row errors. A
/// column-level `Err` from an `eval_*` method means the batch reached a
/// construct fast mode declines, and the request falls back to explain.
type Scalars = (ScalarColumn, RowErrors);
type Judgments = (Vec<JudgmentOutcome>, RowErrors);

struct BulkEvaluator<'a> {
    program: &'a Program,
    data: &'a DataSet,
    period: Period,
    entity_ids: Vec<String>,
    query_input_cells: HashMap<String, Vec<Option<ScalarValue>>>,
    scalar_cache: HashMap<String, DerivedColumn<ScalarColumn>>,
    judgment_cache: HashMap<String, DerivedColumn<Vec<JudgmentOutcome>>>,
    /// The reference interpreter, built on first use, for per-entity relation
    /// aggregation.
    engine: Option<Engine<'a>>,
}

impl<'a> BulkEvaluator<'a> {
    fn new(
        program: &'a Program,
        data: &'a DataSet,
        period: Period,
        entity_ids: Vec<String>,
    ) -> Self {
        let mut query_rows: HashMap<&str, Vec<usize>> = HashMap::new();
        for (row, entity_id) in entity_ids.iter().enumerate() {
            query_rows.entry(entity_id.as_str()).or_default().push(row);
        }

        // `api::execute_request` resolves the dataset to one covering record
        // per fact for this period before calling bulk execution.
        let mut query_input_cells: HashMap<String, Vec<Option<ScalarValue>>> = HashMap::new();
        for record in &data.inputs {
            if !record.interval.contains_period(&period) {
                continue;
            }
            if let Some(rows) = query_rows.get(record.entity_id.as_str()) {
                let cells = query_input_cells
                    .entry(record.name.clone())
                    .or_insert_with(|| vec![None; entity_ids.len()]);
                for row in rows {
                    cells[*row] = Some(record.value.clone());
                }
            }
        }

        Self {
            program,
            data,
            period,
            entity_ids,
            query_input_cells,
            scalar_cache: HashMap::new(),
            judgment_cache: HashMap::new(),
            engine: None,
        }
    }

    fn len(&self) -> usize {
        self.entity_ids.len()
    }

    fn get_derived(&self, name: &str) -> Result<&'a crate::model::Derived, EvalError> {
        self.program
            .derived
            .get(name)
            .ok_or_else(|| EvalError::UnknownDerived(name.to_string()))
    }

    fn engine(&mut self) -> &mut Engine<'a> {
        let (program, data) = (self.program, self.data);
        self.engine
            .get_or_insert_with(|| Engine::new_untraced(program, data))
    }

    /// Evaluate a requested output for the rows in `mask`, in the order the
    /// explain path checks it: formula version, then the rule itself.
    fn evaluate_output(
        &mut self,
        name: &str,
        mask: &RowMask,
    ) -> Result<EvaluatedOutput, EvalError> {
        let derived = self.get_derived(name)?;
        match derived.semantics_at(&self.period) {
            Some(DerivedSemantics::Judgment(_)) => {
                let (values, errors) = self.evaluate_judgment(name, mask)?;
                Ok(EvaluatedOutput {
                    values: OutputColumn::Judgment(values),
                    errors,
                })
            }
            Some(DerivedSemantics::Scalar(_)) => {
                let (values, errors) = self.evaluate_scalar(name, mask)?;
                Ok(EvaluatedOutput {
                    values: OutputColumn::Scalar(values),
                    errors,
                })
            }
            None => {
                let mut errors = RowErrors::new();
                errors.record_all(
                    mask,
                    &EvalError::MissingDerivedFormulaVersion {
                        derived: name.to_string(),
                        at: self.period.start,
                    },
                );
                Ok(EvaluatedOutput {
                    values: OutputColumn::Scalar(ScalarColumn::placeholder(self.len())),
                    errors,
                })
            }
        }
    }

    /// A derived scalar's column for the rows in `mask`, computing it only for
    /// rows no earlier reference asked for.
    fn evaluate_scalar(&mut self, name: &str, mask: &RowMask) -> Result<Scalars, EvalError> {
        let pending = match self.scalar_cache.get(name) {
            Some(cached) => mask.difference(&cached.computed),
            None => mask.clone(),
        };
        if !pending.is_empty() {
            let (values, errors) = self.compute_scalar(name, &pending)?;
            match self.scalar_cache.get_mut(name) {
                Some(cached) => {
                    cached
                        .values
                        .assign_rows(&values, &pending.without(&errors));
                    cached.errors.absorb(errors);
                    cached.computed = cached.computed.union(&pending);
                }
                None => {
                    self.scalar_cache.insert(
                        name.to_string(),
                        DerivedColumn {
                            values,
                            errors,
                            computed: pending,
                        },
                    );
                }
            }
        }
        Ok(match self.scalar_cache.get(name) {
            Some(cached) => (cached.values.clone(), cached.errors.restricted_to(mask)),
            None => (ScalarColumn::placeholder(self.len()), RowErrors::new()),
        })
    }

    fn compute_scalar(&mut self, name: &str, mask: &RowMask) -> Result<Scalars, EvalError> {
        let expr = match self.derived_formula(name) {
            Ok(DerivedSemantics::Scalar(expr)) => expr,
            Ok(DerivedSemantics::Judgment(_)) => {
                return Ok(self.fail_all(mask, EvalError::ExpectedScalar(name.to_string())));
            }
            Err(error) => return Ok(self.fail_all(mask, error)),
        };
        let (mut values, errors) = self.eval_scalar_expr(expr, mask)?;
        // Opt-in output rounding, applied before caching so dependents and
        // direct outputs both see rounded values, as on the explain path.
        if let Some(rounding) = self.get_derived(name)?.rounding {
            values = values.rounded(rounding);
        }
        Ok((values, errors))
    }

    fn evaluate_judgment(&mut self, name: &str, mask: &RowMask) -> Result<Judgments, EvalError> {
        let pending = match self.judgment_cache.get(name) {
            Some(cached) => mask.difference(&cached.computed),
            None => mask.clone(),
        };
        if !pending.is_empty() {
            let (values, errors) = self.compute_judgment(name, &pending)?;
            match self.judgment_cache.get_mut(name) {
                Some(cached) => {
                    for row in pending.without(&errors).rows() {
                        cached.values[row] = values[row];
                    }
                    cached.errors.absorb(errors);
                    cached.computed = cached.computed.union(&pending);
                }
                None => {
                    self.judgment_cache.insert(
                        name.to_string(),
                        DerivedColumn {
                            values,
                            errors,
                            computed: pending,
                        },
                    );
                }
            }
        }
        Ok(match self.judgment_cache.get(name) {
            Some(cached) => (cached.values.clone(), cached.errors.restricted_to(mask)),
            None => (
                vec![JudgmentOutcome::NotHolds; self.len()],
                RowErrors::new(),
            ),
        })
    }

    fn compute_judgment(&mut self, name: &str, mask: &RowMask) -> Result<Judgments, EvalError> {
        let expr = match self.derived_formula(name) {
            Ok(DerivedSemantics::Judgment(expr)) => expr,
            Ok(DerivedSemantics::Scalar(_)) => {
                let (_, errors) =
                    self.fail_all(mask, EvalError::ExpectedJudgment(name.to_string()));
                return Ok((vec![JudgmentOutcome::NotHolds; self.len()], errors));
            }
            Err(error) => {
                let (_, errors) = self.fail_all(mask, error);
                return Ok((vec![JudgmentOutcome::NotHolds; self.len()], errors));
            }
        };
        self.eval_judgment_expr(expr, mask)
    }

    /// The formula a derived rule evaluates at this period, after the checks
    /// the explain path makes first (declared unit, then formula version).
    fn derived_formula(&self, name: &str) -> Result<&'a DerivedSemantics, EvalError> {
        let derived = self.get_derived(name)?;
        if let Some(unit) = &derived.unit
            && !self.program.units.contains_key(unit)
        {
            return Err(EvalError::UnknownUnit(unit.clone()));
        }
        derived
            .semantics_at(&self.period)
            .ok_or_else(|| EvalError::MissingDerivedFormulaVersion {
                derived: name.to_string(),
                at: self.period.start,
            })
    }

    fn fail_all(&self, mask: &RowMask, error: EvalError) -> Scalars {
        let mut errors = RowErrors::new();
        errors.record_all(mask, &error);
        (ScalarColumn::placeholder(self.len()), errors)
    }

    fn eval_scalar_expr(
        &mut self,
        expr: &ScalarExpr,
        mask: &RowMask,
    ) -> Result<Scalars, EvalError> {
        let len = self.len();
        if mask.is_empty() {
            return Ok((ScalarColumn::placeholder(len), RowErrors::new()));
        }
        match expr {
            ScalarExpr::Literal(value) => {
                if matches!(value, ScalarValue::Date(_)) {
                    return Err(unsupported("date literals"));
                }
                Ok((ScalarColumn::broadcast(value, len), RowErrors::new()))
            }
            ScalarExpr::Input(name) => self.eval_input(name, None, mask),
            ScalarExpr::InputOrElse { name, default } => self.eval_input(name, Some(default), mask),
            ScalarExpr::Derived(name) => self.evaluate_scalar(name, mask),
            ScalarExpr::ParameterLookup { parameter, index } => {
                self.eval_parameter_lookup(parameter, index, mask)
            }
            ScalarExpr::Add(items) => {
                let mut errors = RowErrors::new();
                let mut live = mask.clone();
                let mut total = vec![Decimal::ZERO; len];
                for item in items {
                    let (values, next) = self.eval_decimal_operand(item, &live, &mut errors)?;
                    let mut overflow = RowErrors::new();
                    for row in next.rows() {
                        match checked_add(total[row], values[row]) {
                            Ok(sum) => total[row] = sum,
                            Err(error) => overflow.record(row, error.into()),
                        }
                    }
                    live = next.without(&overflow);
                    errors.absorb(overflow);
                }
                Ok((ScalarColumn::Decimal(total), errors))
            }
            ScalarExpr::Sub(left, right) => self.eval_binary(left, right, mask, checked_sub),
            ScalarExpr::Mul(left, right) => self.eval_binary(left, right, mask, checked_mul),
            ScalarExpr::Div(left, right) => {
                // The divisor is evaluated, and checked for zero, before the
                // dividend: a row with a zero divisor never reads the dividend.
                let mut errors = RowErrors::new();
                let (divisors, live) = self.eval_decimal_operand(right, mask, &mut errors)?;
                let mut zero = RowErrors::new();
                for row in live.rows() {
                    if divisors[row].is_zero() {
                        zero.record(row, EvalError::DivisionByZero);
                    }
                }
                let live = live.without(&zero);
                errors.absorb(zero);
                let (dividends, live) = self.eval_decimal_operand(left, &live, &mut errors)?;
                let mut quotients = vec![Decimal::ZERO; len];
                for row in live.rows() {
                    match checked_div(dividends[row], divisors[row]) {
                        Ok(quotient) => quotients[row] = quotient,
                        Err(error) => errors.record(row, error.into()),
                    }
                }
                Ok((ScalarColumn::Decimal(quotients), errors))
            }
            ScalarExpr::Max(items) => {
                self.eval_extremum(items, mask, "max", |candidate, best| candidate > best)
            }
            ScalarExpr::Min(items) => {
                self.eval_extremum(items, mask, "min", |candidate, best| candidate < best)
            }
            ScalarExpr::Ceil(value) => self.eval_unary(value, mask, |value| value.ceil()),
            ScalarExpr::Floor(value) => self.eval_unary(value, mask, |value| value.floor()),
            ScalarExpr::PeriodStart | ScalarExpr::PeriodEnd => {
                Err(unsupported("period_start / period_end"))
            }
            ScalarExpr::DateAddDays { .. } => Err(unsupported("date_add_days")),
            ScalarExpr::DateAddMonths { .. } => Err(unsupported("date_add_months")),
            ScalarExpr::DateAddYears { .. } => Err(unsupported("date_add_years")),
            ScalarExpr::DaysBetween { .. } => Err(unsupported("days_between")),
            ScalarExpr::OverPeriods { kind, .. } => Ok(self.fail_all(
                mask,
                EvalError::OverPeriodsOutsideLifetime(kind.as_call_name()),
            )),
            ScalarExpr::CountRelated { .. } | ScalarExpr::SumRelated { .. } => {
                self.eval_per_entity(expr, mask)
            }
            // The fallback of a `match` without `_`: every row that reaches it
            // fails with explain's error, and any failure sends the request to
            // explain, which names the rule.
            ScalarExpr::NoMatch { subject, patterns } => {
                let (values, mut errors) = self.eval_scalar_expr(subject, mask)?;
                for row in mask.without(&errors).rows() {
                    errors.record(
                        row,
                        crate::engine::no_matching_arm(subject, &values.value_at(row), patterns),
                    );
                }
                Ok((ScalarColumn::placeholder(len), errors))
            }
            ScalarExpr::If {
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
                errors.absorb(then_errors);
                errors.absorb(else_errors);
                Ok((
                    ScalarColumn::select(&then_rows, then_values, &else_rows, else_values),
                    errors,
                ))
            }
        }
    }

    /// Evaluate `expr` for the rows in `mask` and read it as numbers. Returns
    /// the numbers and the rows still live afterwards; failures land in
    /// `errors`.
    fn eval_decimal_operand(
        &mut self,
        expr: &ScalarExpr,
        mask: &RowMask,
        errors: &mut RowErrors,
    ) -> Result<(Vec<Decimal>, RowMask), EvalError> {
        let (column, operand_errors) = self.eval_scalar_expr(expr, mask)?;
        let mut live = mask.without(&operand_errors);
        errors.absorb(operand_errors);
        let mut conversion_errors = RowErrors::new();
        let values = column.decimals(&live, &mut conversion_errors);
        if !conversion_errors.is_empty() {
            live = live.without(&conversion_errors);
            errors.absorb(conversion_errors);
        }
        Ok((values, live))
    }

    fn eval_binary(
        &mut self,
        left: &ScalarExpr,
        right: &ScalarExpr,
        mask: &RowMask,
        operation: impl Fn(Decimal, Decimal) -> Result<Decimal, ArithmeticError>,
    ) -> Result<Scalars, EvalError> {
        let mut errors = RowErrors::new();
        let (left, live) = self.eval_decimal_operand(left, mask, &mut errors)?;
        let (right, live) = self.eval_decimal_operand(right, &live, &mut errors)?;
        let mut values = vec![Decimal::ZERO; self.len()];
        for row in live.rows() {
            match operation(left[row], right[row]) {
                Ok(value) => values[row] = value,
                Err(error) => errors.record(row, error.into()),
            }
        }
        Ok((ScalarColumn::Decimal(values), errors))
    }

    fn eval_unary(
        &mut self,
        value: &ScalarExpr,
        mask: &RowMask,
        operation: impl Fn(Decimal) -> Decimal,
    ) -> Result<Scalars, EvalError> {
        let mut errors = RowErrors::new();
        let (values, live) = self.eval_decimal_operand(value, mask, &mut errors)?;
        let mut result = vec![Decimal::ZERO; self.len()];
        for row in live.rows() {
            result[row] = operation(values[row]);
        }
        Ok((ScalarColumn::Decimal(result), errors))
    }

    fn eval_extremum(
        &mut self,
        items: &[ScalarExpr],
        mask: &RowMask,
        function: &str,
        replaces: impl Fn(Decimal, Decimal) -> bool,
    ) -> Result<Scalars, EvalError> {
        let Some((first, rest)) = items.split_first() else {
            return Ok(self.fail_all(
                mask,
                EvalError::TypeMismatch(format!("{function}() requires at least one operand")),
            ));
        };
        let mut errors = RowErrors::new();
        let (mut best, mut live) = self.eval_decimal_operand(first, mask, &mut errors)?;
        for item in rest {
            let (candidates, next) = self.eval_decimal_operand(item, &live, &mut errors)?;
            live = next;
            for row in live.rows() {
                if replaces(candidates[row], best[row]) {
                    best[row] = candidates[row];
                }
            }
        }
        Ok((ScalarColumn::Decimal(best), errors))
    }

    fn eval_input(
        &mut self,
        name: &str,
        default: Option<&ScalarValue>,
        mask: &RowMask,
    ) -> Result<Scalars, EvalError> {
        let cells = self.query_input_cells.get(name);
        let mut errors = RowErrors::new();
        let mut entries = Vec::with_capacity(mask.count());
        for row in mask.rows() {
            let value = match (cells.and_then(|cells| cells[row].as_ref()), default) {
                (Some(value), _) => value.clone(),
                (None, Some(default)) => default.clone(),
                (None, None) => {
                    errors.record(
                        row,
                        EvalError::MissingInput {
                            name: name.to_string(),
                            entity_id: self.entity_ids[row].clone(),
                            period_start: self.period.start,
                            period_end: self.period.end,
                        },
                    );
                    continue;
                }
            };
            if matches!(value, ScalarValue::Date(_)) {
                return Err(unsupported("date inputs"));
            }
            entries.push((row, value));
        }
        Ok((ScalarColumn::from_entries(self.len(), entries), errors))
    }

    fn eval_parameter_lookup(
        &mut self,
        parameter: &str,
        index: &ScalarExpr,
        mask: &RowMask,
    ) -> Result<Scalars, EvalError> {
        let (keys, mut errors) = self.eval_scalar_expr(index, mask)?;
        let live = mask.without(&errors);
        let definition = self.program.parameters.get(parameter);
        let version = definition.and_then(|definition| {
            definition
                .versions
                .iter()
                .filter(|version| version.applies_at(self.period.start))
                .max_by_key(|version| version.effective_from)
        });
        let mut entries = Vec::with_capacity(live.count());
        for row in live.rows() {
            let Some(key) = keys.value_at(row).as_index() else {
                errors.record(
                    row,
                    EvalError::TypeMismatch(format!(
                        "parameter key for `{parameter}` must be an integer"
                    )),
                );
                continue;
            };
            if definition.is_none() {
                errors.record(row, EvalError::UnknownParameter(parameter.to_string()));
                continue;
            }
            let Some(value) = version.and_then(|version| version.values.get(&key)) else {
                errors.record(
                    row,
                    EvalError::MissingParameterValue {
                        parameter: parameter.to_string(),
                        key,
                        at: self.period.start,
                    },
                );
                continue;
            };
            if matches!(value, ScalarValue::Date(_)) {
                return Err(unsupported("date parameter values"));
            }
            entries.push((row, value.clone()));
        }
        Ok((ScalarColumn::from_entries(self.len(), entries), errors))
    }

    /// Relation aggregations visit each row's related entities, so they run
    /// row by row on the reference interpreter: identical id resolution,
    /// derived-relation filtering, `where` laziness and related values.
    fn eval_per_entity(&mut self, expr: &ScalarExpr, mask: &RowMask) -> Result<Scalars, EvalError> {
        let period = self.period.clone();
        let mut errors = RowErrors::new();
        let mut entries = Vec::with_capacity(mask.count());
        for row in mask.rows() {
            let entity_id = self.entity_ids[row].clone();
            match self.engine().eval_scalar_expr(expr, &entity_id, &period) {
                Ok(value) => {
                    if matches!(value, ScalarValue::Date(_)) {
                        return Err(unsupported("date values"));
                    }
                    entries.push((row, value));
                }
                Err(error) => errors.record(row, error),
            }
        }
        Ok((ScalarColumn::from_entries(self.len(), entries), errors))
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &JudgmentExpr,
        mask: &RowMask,
    ) -> Result<Judgments, EvalError> {
        let len = self.len();
        if mask.is_empty() {
            return Ok((vec![JudgmentOutcome::NotHolds; len], RowErrors::new()));
        }
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                let (left, mut errors) = self.eval_scalar_expr(left, mask)?;
                let live = mask.without(&errors);
                let (right, right_errors) = self.eval_scalar_expr(right, &live)?;
                let live = live.without(&right_errors);
                errors.absorb(right_errors);
                let outcomes = compare_columns(&left, *op, &right, &live, &mut errors);
                Ok((outcomes, errors))
            }
            JudgmentExpr::Derived(name) => self.evaluate_judgment(name, mask),
            JudgmentExpr::RelationMember { relation, .. } => {
                let (_, errors) =
                    self.fail_all(mask, relation_member_outside_derived_relation(relation));
                Ok((vec![JudgmentOutcome::NotHolds; len], errors))
            }
            JudgmentExpr::And(items) => {
                self.eval_short_circuit(items, mask, JudgmentOutcome::NotHolds)
            }
            JudgmentExpr::Or(items) => self.eval_short_circuit(items, mask, JudgmentOutcome::Holds),
            JudgmentExpr::Not(item) => {
                let (values, errors) = self.eval_judgment_expr(item, mask)?;
                Ok((
                    values
                        .into_iter()
                        .map(|value| match value {
                            JudgmentOutcome::Holds => JudgmentOutcome::NotHolds,
                            JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
                            JudgmentOutcome::Undetermined => JudgmentOutcome::Undetermined,
                        })
                        .collect(),
                    errors,
                ))
            }
        }
    }

    /// `and` (`decisive` = not_holds) or `or` (`decisive` = holds): each item
    /// is evaluated only for the rows no earlier item decided. A row with an
    /// undetermined item and no decisive one is undetermined.
    fn eval_short_circuit(
        &mut self,
        items: &[JudgmentExpr],
        mask: &RowMask,
        decisive: JudgmentOutcome,
    ) -> Result<Judgments, EvalError> {
        let exhausted = match decisive {
            JudgmentOutcome::NotHolds => JudgmentOutcome::Holds,
            _ => JudgmentOutcome::NotHolds,
        };
        let len = self.len();
        let mut outcomes = vec![exhausted; len];
        let mut undetermined = vec![false; len];
        let mut errors = RowErrors::new();
        let mut pending = mask.clone();
        for item in items {
            if pending.is_empty() {
                break;
            }
            let (values, item_errors) = self.eval_judgment_expr(item, &pending)?;
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
}

/// Compare two columns on the live rows with the reference comparison
/// semantics. Same-kind numeric, boolean and text columns take a vectorised
/// path; anything else compares row by row with the explain function.
fn compare_columns(
    left: &ScalarColumn,
    op: ComparisonOp,
    right: &ScalarColumn,
    live: &RowMask,
    errors: &mut RowErrors,
) -> Vec<JudgmentOutcome> {
    let mut outcomes = vec![JudgmentOutcome::NotHolds; left.len()];
    let outcome = |holds: bool| {
        if holds {
            JudgmentOutcome::Holds
        } else {
            JudgmentOutcome::NotHolds
        }
    };
    let numeric = |left: Decimal, right: Decimal| match op {
        ComparisonOp::Lt => left < right,
        ComparisonOp::Lte => left <= right,
        ComparisonOp::Gt => left > right,
        ComparisonOp::Gte => left >= right,
        ComparisonOp::Eq => left == right,
        ComparisonOp::Ne => left != right,
    };
    match (left, right) {
        (ScalarColumn::Integer(left), ScalarColumn::Integer(right)) => {
            for row in live.rows() {
                outcomes[row] =
                    outcome(numeric(Decimal::from(left[row]), Decimal::from(right[row])));
            }
        }
        (
            ScalarColumn::Integer(_) | ScalarColumn::Decimal(_),
            ScalarColumn::Integer(_) | ScalarColumn::Decimal(_),
        ) => {
            let mut ignored = RowErrors::new();
            let left = left.decimals(live, &mut ignored);
            let right = right.decimals(live, &mut ignored);
            for row in live.rows() {
                outcomes[row] = outcome(numeric(left[row], right[row]));
            }
        }
        _ => {
            for row in live.rows() {
                match compare_scalar_values(&left.value_at(row), op, &right.value_at(row)) {
                    Ok(holds) => outcomes[row] = outcome(holds),
                    Err(error) => errors.record(row, error),
                }
            }
        }
    }
    outcomes
}
