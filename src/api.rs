use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::{Engine, EvalError};
use crate::model::{DerivedSemantics, JudgmentOutcome};
use crate::spec::{
    DTypeSpec, DatasetSpec, JudgmentOutcomeSpec, PeriodSpec, ProgramSpec, ScalarValueSpec,
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
        dtype: DTypeSpec,
        unit: Option<String>,
        value: ScalarValueSpec,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_url: Option<String>,
        dependencies: Vec<String>,
    },
    Judgment {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        unit: Option<String>,
        outcome: JudgmentOutcomeSpec,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_url: Option<String>,
        dependencies: Vec<String>,
    },
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Eval(#[from] EvalError),
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
}

pub fn execute_request(request: ExecutionRequest) -> Result<ExecutionResponse, ApiError> {
    let requested_mode = request.mode.clone();
    let program = request.program.to_program()?;
    let dataset = request.dataset.to_dataset_for_program(&program)?;

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
            match crate::bulk::try_execute(&program, &dataset, &request.queries)? {
                crate::bulk::FastPathResult::Executed(response) => {
                    Ok(response.with_metadata(ExecutionMetadata {
                        requested_mode: ExecutionMode::Fast,
                        actual_mode: ExecutionMode::Fast,
                        fallback_reason: None,
                    }))
                }
                crate::bulk::FastPathResult::Unsupported { reason } => execute_explain(
                    &program,
                    &dataset,
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

            match &derived.semantics {
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
    for derived in program.derived.values() {
        match &derived.semantics {
            DerivedSemantics::Scalar(expr) => {
                if let Some(value) = engine.cached_scalar(&derived.name, entity_id, period) {
                    trace.insert(
                        program.public_derived_key(&derived.name),
                        DerivedTraceNode::Scalar {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            dtype: DTypeSpec::from_model(&derived.dtype),
                            unit: derived.unit.clone(),
                            value: ScalarValueSpec::from_model(value),
                            source: derived.source.clone(),
                            source_url: derived.source_url.clone(),
                            dependencies: public_dependencies(program, scalar_dependencies(expr)),
                        },
                    );
                }
            }
            DerivedSemantics::Judgment(expr) => {
                if let Some(outcome) = engine.cached_judgment(&derived.name, entity_id, period) {
                    trace.insert(
                        program.public_derived_key(&derived.name),
                        DerivedTraceNode::Judgment {
                            name: derived.name.clone(),
                            id: derived.id.clone(),
                            unit: derived.unit.clone(),
                            outcome: match outcome {
                                JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
                                JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
                                JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
                            },
                            source: derived.source.clone(),
                            source_url: derived.source_url.clone(),
                            dependencies: public_dependencies(program, judgment_dependencies(expr)),
                        },
                    );
                }
            }
        }
    }
    trace
}

fn public_dependencies(program: &crate::model::Program, dependencies: Vec<String>) -> Vec<String> {
    dependencies
        .into_iter()
        .map(|dependency| program.public_derived_key(&dependency))
        .collect()
}

fn scalar_dependencies(expr: &crate::model::ScalarExpr) -> Vec<String> {
    let mut deps: std::collections::BTreeSet<String> = Default::default();
    collect_scalar_deps(expr, &mut deps);
    deps.into_iter().collect()
}

fn judgment_dependencies(expr: &crate::model::JudgmentExpr) -> Vec<String> {
    let mut deps: std::collections::BTreeSet<String> = Default::default();
    collect_judgment_deps(expr, &mut deps);
    deps.into_iter().collect()
}

fn collect_scalar_deps(
    expr: &crate::model::ScalarExpr,
    deps: &mut std::collections::BTreeSet<String>,
) {
    use crate::model::{RelatedValueRef, ScalarExpr};
    match expr {
        ScalarExpr::Literal(_) | ScalarExpr::Input(_) | ScalarExpr::InputOrElse { .. } => {}
        ScalarExpr::Derived(name) => {
            deps.insert(name.clone());
        }
        ScalarExpr::ParameterLookup { index, .. } => collect_scalar_deps(index, deps),
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_deps(item, deps);
            }
        }
        ScalarExpr::Sub(a, b) | ScalarExpr::Mul(a, b) | ScalarExpr::Div(a, b) => {
            collect_scalar_deps(a, deps);
            collect_scalar_deps(b, deps);
        }
        ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => collect_scalar_deps(value, deps),
        ScalarExpr::PeriodStart | ScalarExpr::PeriodEnd => {}
        ScalarExpr::DateAddDays { date, days } => {
            collect_scalar_deps(date, deps);
            collect_scalar_deps(days, deps);
        }
        ScalarExpr::DaysBetween { from, to } => {
            collect_scalar_deps(from, deps);
            collect_scalar_deps(to, deps);
        }
        ScalarExpr::CountRelated { where_clause, .. } => {
            if let Some(predicate) = where_clause {
                collect_judgment_deps(predicate, deps);
            }
        }
        ScalarExpr::SumRelated {
            value,
            where_clause,
            ..
        } => {
            if let RelatedValueRef::Derived(name) = value {
                deps.insert(name.clone());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_deps(predicate, deps);
            }
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_deps(condition, deps);
            collect_scalar_deps(then_expr, deps);
            collect_scalar_deps(else_expr, deps);
        }
    }
}

fn collect_judgment_deps(
    expr: &crate::model::JudgmentExpr,
    deps: &mut std::collections::BTreeSet<String>,
) {
    use crate::model::JudgmentExpr;
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_deps(left, deps);
            collect_scalar_deps(right, deps);
        }
        JudgmentExpr::Derived(name) => {
            deps.insert(name.clone());
        }
        JudgmentExpr::RelationMember { .. } => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_deps(item, deps);
            }
        }
        JudgmentExpr::Not(item) => collect_judgment_deps(item, deps),
    }
}

impl ExecutionResponse {
    pub fn with_metadata(mut self, metadata: ExecutionMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}
