use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::{Engine, EvalError};
use crate::model::{DerivedSemantics, JudgmentOutcome};
use crate::spec::{
    DTypeSpec, DatasetSpec, JudgmentOutcomeSpec, PeriodSpec, ProgramSpec, RoundingModeSpec,
    ScalarValueSpec,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub mode: ExecutionMode,
    pub program: ProgramSpec,
    pub dataset: DatasetSpec,
    pub queries: Vec<ExecutionQuery>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledExecutionRequest {
    pub mode: ExecutionMode,
    pub dataset: DatasetSpec,
    pub queries: Vec<ExecutionQuery>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Explain,
    Fast,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionQuery {
    pub entity_id: String,
    pub period: PeriodSpec,
    pub outputs: Vec<String>,
    /// Decision/assessment time: the date the determination is made, as
    /// opposed to `period`, which is valid time — the benefit period the law
    /// governs. Reserved for the bitemporal version-selection semantics in
    /// `docs/bitemporal.md`.
    ///
    /// Today this field is parsed and validated only: when present it must be
    /// on or after `period.start`, and it has NO effect on evaluation yet.
    /// Version selection still considers every version, exactly as if the
    /// assessment had full knowledge of all enactments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment_date: Option<NaiveDate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResponse {
    pub metadata: ExecutionMetadata,
    pub results: Vec<QueryResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    pub requested_mode: ExecutionMode,
    pub actual_mode: ExecutionMode,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub entity_id: String,
    pub period: PeriodSpec,
    /// Echo of the query's `assessment_date`, so callers can tie a result to
    /// the assessment it was requested under. See `docs/bitemporal.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment_date: Option<NaiveDate>,
    pub outputs: BTreeMap<String, OutputValue>,
    #[serde(default)]
    pub trace: BTreeMap<String, DerivedTraceNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputValue {
    Scalar {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        dtype: DTypeSpec,
        unit: Option<String>,
        value: ScalarValueSpec,
    },
    Judgment {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        unit: Option<String>,
        outcome: JudgmentOutcomeSpec,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DerivedTraceNode {
    Scalar {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Entity instance on which this rule was evaluated. Query-entity
        /// instances retain the historical map key; related instances receive
        /// an opaque suffixed key so dependency edges remain closed.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        entity_id: String,
        dtype: DTypeSpec,
        unit: Option<String>,
        /// The rule's output value. When the rule applied currency rounding,
        /// this is the rounded (post-rounding) value.
        value: ScalarValueSpec,
        /// The output-rounding mode the rule applied, if any. Present whenever
        /// the rule declares `rounding:`, so an auditor sees the rounding step
        /// was part of the determination even when the value was already whole.
        #[serde(skip_serializing_if = "Option::is_none")]
        rounding: Option<crate::spec::RoundingModeSpec>,
        /// The value before rounding, present only when rounding actually
        /// changed it. Together with `value` and `rounding` this shows the
        /// rounding step (pre-value → mode → rounded value) for auditable law.
        #[serde(skip_serializing_if = "Option::is_none")]
        pre_rounding_value: Option<ScalarValueSpec>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_url: Option<String>,
        /// Canonical projection of the exact expression selected and executed
        /// for this period. Unlike `source`, this is generated from executable
        /// IR and cannot drift into a contradictory arithmetic narrative.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        executed_expression: String,
        /// Exact temporal parameter cells read while executing this node.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parameter_reads: Vec<TraceParameterRead>,
        /// Derived edges actually traversed. Every key resolves in this trace.
        dependencies: Vec<String>,
        /// Parent-specific edges that were present in the selected expression
        /// but not traversed because execution short-circuited or chose another
        /// branch.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_evaluated_dependencies: Vec<NotEvaluatedDependency>,
    },
    Judgment {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        entity_id: String,
        unit: Option<String>,
        outcome: JudgmentOutcomeSpec,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_url: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        executed_expression: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parameter_reads: Vec<TraceParameterRead>,
        dependencies: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_evaluated_dependencies: Vec<NotEvaluatedDependency>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotEvaluatedDependency {
    pub dependency: String,
    pub reason: NotEvaluatedReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotEvaluatedReason {
    ShortCircuit,
    BranchNotSelected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceParameterRead {
    /// Public parameter identity (durable `id` when present, otherwise name).
    pub parameter: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub index: i64,
    pub value: ScalarValueSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub effective_from: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Eval(#[from] EvalError),
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
    #[error(
        "assessment_date {assessment_date} is before the query period start {period_start}; a determination cannot be assessed before the period it covers begins (see docs/bitemporal.md)"
    )]
    AssessmentDateBeforePeriodStart {
        assessment_date: NaiveDate,
        period_start: NaiveDate,
    },
}

/// Reject queries whose `assessment_date` predates the period they assess.
/// Assessing a period before it starts would be a projection, not a
/// determination; projection semantics are explicitly out of scope for now
/// (see docs/bitemporal.md).
fn validate_assessment_dates(queries: &[ExecutionQuery]) -> Result<(), ApiError> {
    for query in queries {
        if let Some(assessment_date) = query.assessment_date {
            if assessment_date < query.period.start {
                return Err(ApiError::AssessmentDateBeforePeriodStart {
                    assessment_date,
                    period_start: query.period.start,
                });
            }
        }
    }
    Ok(())
}

pub fn execute_request(request: ExecutionRequest) -> Result<ExecutionResponse, ApiError> {
    validate_assessment_dates(&request.queries)?;
    let requested_mode = request.mode.clone();
    let program = request.program.to_program()?;
    let dataset = request.dataset.to_dataset_for_program(&program)?;
    crate::engine::validate_input_spells(&dataset)?;

    match requested_mode {
        ExecutionMode::Explain => execute_explain(
            &program,
            &dataset,
            request.queries,
            ExecutionMetadata {
                requested_mode: ExecutionMode::Explain,
                actual_mode: ExecutionMode::Explain,
                fallback_reason: None,
            },
        ),
        ExecutionMode::Fast => {
            // Fast accepts one shared query period. Resolve every covering
            // input once under the same latest-start rule used by Explain
            // before handing the dataset to the order-overwriting bulk
            // evaluator. If periods differ, leave the full dataset intact:
            // bulk will report Unsupported and the ordinary Explain path will
            // select independently for each query period.
            let resolved_dataset;
            let fast_dataset = if let Some(first_query) = request.queries.first()
                && request
                    .queries
                    .iter()
                    .all(|query| query.period == first_query.period)
            {
                let period = first_query.period.to_model()?;
                resolved_dataset = crate::engine::resolve_inputs_for_period(&dataset, &period);
                &resolved_dataset
            } else {
                &dataset
            };

            match crate::bulk::try_execute(&program, fast_dataset, &request.queries)? {
                crate::bulk::FastPathResult::Executed(response) => {
                    Ok(response.with_metadata(ExecutionMetadata {
                        requested_mode: ExecutionMode::Fast,
                        actual_mode: ExecutionMode::Fast,
                        fallback_reason: None,
                    }))
                }
                crate::bulk::FastPathResult::Unsupported { reason } => execute_explain(
                    &program,
                    fast_dataset,
                    request.queries,
                    ExecutionMetadata {
                        requested_mode: ExecutionMode::Fast,
                        actual_mode: ExecutionMode::Explain,
                        fallback_reason: Some(reason),
                    },
                ),
            }
        }
    }
}

pub fn execute_compiled_request(
    artifact: CompiledProgramArtifact,
    request: CompiledExecutionRequest,
) -> Result<ExecutionResponse, ApiError> {
    execute_request(ExecutionRequest {
        mode: request.mode,
        program: artifact.program,
        dataset: request.dataset,
        queries: request.queries,
    })
}

fn execute_explain(
    program: &crate::model::Program,
    dataset: &crate::model::DataSet,
    queries: Vec<ExecutionQuery>,
    metadata: ExecutionMetadata,
) -> Result<ExecutionResponse, ApiError> {
    let mut engine = Engine::new(&program, &dataset);
    let mut results = Vec::with_capacity(queries.len());

    for query in queries {
        let period = query.period.to_model()?;
        let mut outputs = BTreeMap::new();

        for output_reference in &query.outputs {
            let output_name = program
                .resolve_derived_name(output_reference)
                .ok_or_else(|| EvalError::UnknownDerived(output_reference.clone()))?;
            let derived = program
                .derived
                .get(&output_name)
                .ok_or_else(|| EvalError::UnknownDerived(output_reference.clone()))?;
            let output_key = derived
                .id
                .clone()
                .unwrap_or_else(|| output_name.to_string());

            let semantics = derived.semantics_at(&period).ok_or_else(|| {
                EvalError::MissingDerivedFormulaVersion {
                    derived: output_name.clone(),
                    at: period.start,
                }
            })?;
            match semantics {
                DerivedSemantics::Scalar(_) => {
                    let value = engine.evaluate_scalar(&output_name, &query.entity_id, &period)?;
                    outputs.insert(
                        output_key,
                        OutputValue::Scalar {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            dtype: DTypeSpec::from_model(&derived.dtype),
                            unit: derived.unit.clone(),
                            value: ScalarValueSpec::from_model(value),
                        },
                    );
                }
                DerivedSemantics::Judgment(_) => {
                    let outcome =
                        engine.evaluate_judgment(&output_name, &query.entity_id, &period)?;
                    outputs.insert(
                        output_key,
                        OutputValue::Judgment {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            unit: derived.unit.clone(),
                            outcome: match outcome {
                                JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
                                JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
                                JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
                            },
                        },
                    );
                }
            }
        }

        let trace = collect_trace(program, &engine, &query.entity_id, &period);

        results.push(QueryResult {
            entity_id: query.entity_id,
            period: query.period,
            assessment_date: query.assessment_date,
            outputs,
            trace,
        });
    }

    Ok(ExecutionResponse { metadata, results })
}

fn collect_trace(
    program: &crate::model::Program,
    engine: &Engine,
    entity_id: &str,
    period: &crate::model::Period,
) -> BTreeMap<String, DerivedTraceNode> {
    let mut trace = BTreeMap::new();
    let mut instances = engine.evaluated_trace_instances(period, entity_id);
    instances.sort_by(|left, right| {
        (&left.key.derived, &left.key.entity_id).cmp(&(&right.key.derived, &right.key.entity_id))
    });

    for instance in instances {
        let Some(derived) = program.derived.get(&instance.key.derived) else {
            continue;
        };
        let Some(semantics) = derived.semantics_at(period) else {
            continue;
        };
        let trace_key = trace_instance_key(program, &instance.key, entity_id);
        let dependencies = instance
            .execution
            .dependencies
            .iter()
            .map(|dependency| trace_instance_key(program, dependency, entity_id))
            .collect();
        let not_evaluated_dependencies = instance
            .execution
            .not_evaluated_dependencies
            .iter()
            .map(|dependency| match dependency {
                crate::engine::SkippedTraceDependency::Derived { key, reason } => {
                    NotEvaluatedDependency {
                        dependency: trace_instance_key(program, key, entity_id),
                        reason: trace_skip_reason(*reason),
                    }
                }
                crate::engine::SkippedTraceDependency::Parameter { parameter, reason } => {
                    NotEvaluatedDependency {
                        dependency: public_parameter_key(program, parameter),
                        reason: trace_skip_reason(*reason),
                    }
                }
            })
            .collect();
        let parameter_reads = instance
            .execution
            .parameter_reads
            .iter()
            .map(|read| {
                let parameter = program
                    .parameters
                    .get(&read.parameter)
                    .expect("an evaluated parameter remains in the program");
                TraceParameterRead {
                    parameter: public_parameter_key(program, &read.parameter),
                    name: parameter.name.clone(),
                    id: parameter.id.clone(),
                    index: read.index,
                    value: ScalarValueSpec::from_model(read.value.clone()),
                    unit: parameter.unit.clone(),
                    effective_from: read.effective_from,
                    effective_to: read.effective_to,
                    source: parameter.source.clone(),
                    source_url: parameter.source_url.clone(),
                }
            })
            .collect();

        let node = match (&instance.value, semantics) {
            (
                crate::engine::EvaluatedTraceValue::Scalar {
                    value,
                    pre_rounding_value,
                },
                DerivedSemantics::Scalar(expr),
            ) => DerivedTraceNode::Scalar {
                name: derived.name.clone(),
                id: derived.id.clone(),
                entity_id: instance.key.entity_id.clone(),
                dtype: DTypeSpec::from_model(&derived.dtype),
                unit: derived.unit.clone(),
                value: ScalarValueSpec::from_model(value.clone()),
                rounding: derived
                    .rounding
                    .map(|rounding| RoundingModeSpec::from_model(rounding.mode)),
                pre_rounding_value: pre_rounding_value.clone().map(ScalarValueSpec::from_model),
                source: derived.source.clone(),
                source_url: derived.source_url.clone(),
                executed_expression: format_scalar_expression(program, expr),
                parameter_reads,
                dependencies,
                not_evaluated_dependencies,
            },
            (
                crate::engine::EvaluatedTraceValue::Judgment(outcome),
                DerivedSemantics::Judgment(expr),
            ) => DerivedTraceNode::Judgment {
                name: derived.name.clone(),
                id: derived.id.clone(),
                entity_id: instance.key.entity_id.clone(),
                unit: derived.unit.clone(),
                outcome: match outcome {
                    JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
                    JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
                    JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
                },
                source: derived.source.clone(),
                source_url: derived.source_url.clone(),
                executed_expression: format_judgment_expression(program, expr),
                parameter_reads,
                dependencies,
                not_evaluated_dependencies,
            },
            // A cache entry and its selected semantics are created together;
            // a type mismatch here would indicate internal corruption.
            _ => continue,
        };
        trace.insert(trace_key, node);
    }
    trace
}

fn trace_skip_reason(reason: crate::engine::TraceSkipReason) -> NotEvaluatedReason {
    match reason {
        crate::engine::TraceSkipReason::ShortCircuit => NotEvaluatedReason::ShortCircuit,
        crate::engine::TraceSkipReason::BranchNotSelected => NotEvaluatedReason::BranchNotSelected,
    }
}

fn trace_instance_key(
    program: &crate::model::Program,
    key: &crate::engine::CacheKey,
    query_entity_id: &str,
) -> String {
    let public = program.public_derived_key(&key.derived);
    if key.entity_id == query_entity_id {
        return public;
    }
    let mut encoded_entity = String::with_capacity(key.entity_id.len() * 2);
    for byte in key.entity_id.as_bytes() {
        use std::fmt::Write as _;
        write!(&mut encoded_entity, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("{public}@entity:{encoded_entity}")
}

fn public_parameter_key(program: &crate::model::Program, name: &str) -> String {
    program
        .parameters
        .get(name)
        .and_then(|parameter| parameter.id.clone())
        .unwrap_or_else(|| name.to_string())
}

fn format_scalar_expression(
    program: &crate::model::Program,
    expr: &crate::model::ScalarExpr,
) -> String {
    use crate::model::{RelatedValueRef, ScalarExpr};
    match expr {
        ScalarExpr::Literal(value) => format_scalar_value(value),
        ScalarExpr::Input(name) => format!("input({})", json_string(name)),
        ScalarExpr::InputOrElse { name, default } => {
            format!(
                "input_or_else({}, {})",
                json_string(name),
                format_scalar_value(default)
            )
        }
        ScalarExpr::Derived(name) => program.public_derived_key(name),
        ScalarExpr::ParameterLookup { parameter, index } => format!(
            "{}[{}]",
            public_parameter_key(program, parameter),
            format_scalar_expression(program, index)
        ),
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            let (name, separator) = match expr {
                ScalarExpr::Add(_) => ("", " + "),
                ScalarExpr::Max(_) => ("max", ", "),
                ScalarExpr::Min(_) => ("min", ", "),
                _ => unreachable!(),
            };
            let body = items
                .iter()
                .map(|item| format_scalar_expression(program, item))
                .collect::<Vec<_>>()
                .join(separator);
            if name.is_empty() {
                format!("({body})")
            } else {
                format!("{name}({body})")
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right) => {
            let operator = match expr {
                ScalarExpr::Sub(_, _) => "-",
                ScalarExpr::Mul(_, _) => "*",
                ScalarExpr::Div(_, _) => "/",
                _ => unreachable!(),
            };
            format!(
                "({} {operator} {})",
                format_scalar_expression(program, left),
                format_scalar_expression(program, right)
            )
        }
        ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => {
            let function = if matches!(expr, ScalarExpr::Ceil(_)) {
                "ceil"
            } else {
                "floor"
            };
            format!("{function}({})", format_scalar_expression(program, value))
        }
        ScalarExpr::PeriodStart => "period_start".to_string(),
        ScalarExpr::PeriodEnd => "period_end".to_string(),
        ScalarExpr::DateAddDays { date, days } => {
            format!(
                "date_add_days({}, {})",
                format_scalar_expression(program, date),
                format_scalar_expression(program, days)
            )
        }
        ScalarExpr::DaysBetween { from, to } => {
            format!(
                "days_between({}, {})",
                format_scalar_expression(program, from),
                format_scalar_expression(program, to)
            )
        }
        ScalarExpr::CountRelated {
            relation,
            current_slot,
            related_slot,
            where_clause,
        } => format!(
            "count_related({}, {current_slot}, {related_slot}{})",
            json_string(relation),
            where_clause
                .as_ref()
                .map(|predicate| format!(
                    ", where: {}",
                    format_judgment_expression(program, predicate)
                ))
                .unwrap_or_default()
        ),
        ScalarExpr::SumRelated {
            relation,
            current_slot,
            related_slot,
            value,
            where_clause,
        } => format!(
            "sum_related({}, {current_slot}, {related_slot}, {}{})",
            json_string(relation),
            match value {
                RelatedValueRef::Input(name) => format!("input({})", json_string(name)),
                RelatedValueRef::Derived(name) => program.public_derived_key(name),
            },
            where_clause
                .as_ref()
                .map(|predicate| format!(
                    ", where: {}",
                    format_judgment_expression(program, predicate)
                ))
                .unwrap_or_default()
        ),
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "(if {} then {} else {})",
            format_judgment_expression(program, condition),
            format_scalar_expression(program, then_expr),
            format_scalar_expression(program, else_expr)
        ),
        ScalarExpr::OverPeriods { kind, value, n } => format!(
            "{}({}{})",
            kind.as_call_name(),
            format_scalar_expression(program, value),
            n.as_ref()
                .map(|n| format!(", {}", format_scalar_expression(program, n)))
                .unwrap_or_default()
        ),
    }
}

fn format_judgment_expression(
    program: &crate::model::Program,
    expr: &crate::model::JudgmentExpr,
) -> String {
    use crate::model::{ComparisonOp, JudgmentExpr};
    match expr {
        JudgmentExpr::Comparison { left, op, right } => format!(
            "({} {} {})",
            format_scalar_expression(program, left),
            match op {
                ComparisonOp::Lt => "<",
                ComparisonOp::Lte => "<=",
                ComparisonOp::Gt => ">",
                ComparisonOp::Gte => ">=",
                ComparisonOp::Eq => "==",
                ComparisonOp::Ne => "!=",
            },
            format_scalar_expression(program, right)
        ),
        JudgmentExpr::Derived(name) => program.public_derived_key(name),
        JudgmentExpr::RelationMember {
            relation,
            current_slot,
            related_slot,
        } => format!(
            "relation_member({}, {current_slot}, {related_slot})",
            json_string(relation)
        ),
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            let separator = if matches!(expr, JudgmentExpr::And(_)) {
                " and "
            } else {
                " or "
            };
            format!(
                "({})",
                items
                    .iter()
                    .map(|item| format_judgment_expression(program, item))
                    .collect::<Vec<_>>()
                    .join(separator)
            )
        }
        JudgmentExpr::Not(item) => {
            format!("not ({})", format_judgment_expression(program, item))
        }
    }
}

fn format_scalar_value(value: &crate::model::ScalarValue) -> String {
    use crate::model::ScalarValue;
    match value {
        ScalarValue::Bool(value) => value.to_string(),
        ScalarValue::Integer(value) => value.to_string(),
        ScalarValue::Decimal(value) => value.to_string(),
        ScalarValue::Text(value) => json_string(value),
        ScalarValue::Date(value) => format!("date({})", json_string(&value.to_string())),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a Rust string always serialises as JSON")
}

impl ExecutionResponse {
    pub fn with_metadata(mut self, metadata: ExecutionMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}
