use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use thiserror::Error;

use crate::compile::CompiledProgramArtifact;
use crate::engine::EvalError;
use crate::model::{
    ComparisonOp, DType, DerivedSemantics, IndexedParameter, JudgmentExpr, JudgmentOutcome, Period,
    Program, RelatedValueRef, ScalarExpr, ScalarValue,
};

#[derive(Clone, Debug)]
pub enum DenseColumn {
    Bool(Vec<bool>),
    Integer(Vec<i64>),
    Decimal(Vec<Decimal>),
    Text(Vec<String>),
    Date(Vec<chrono::NaiveDate>),
}

impl DenseColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::Integer(values) => values.len(),
            Self::Decimal(values) => values.len(),
            Self::Text(values) => values.len(),
            Self::Date(values) => values.len(),
        }
    }

    fn as_decimal_vec(&self) -> Result<Vec<Decimal>, EvalError> {
        match self {
            Self::Integer(values) => Ok(values.iter().map(|value| Decimal::from(*value)).collect()),
            Self::Decimal(values) => Ok(values.clone()),
            _ => Err(EvalError::TypeMismatch(
                "expected decimal-compatible dense column".to_string(),
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
                            "parameter key for dense lookup must be integral".to_string(),
                        )
                    })
                })
                .collect(),
            _ => Err(EvalError::TypeMismatch(
                "parameter key for dense lookup must be numeric".to_string(),
            )),
        }
    }

    fn as_date_vec(&self) -> Result<Vec<chrono::NaiveDate>, EvalError> {
        match self {
            Self::Date(values) => Ok(values.clone()),
            _ => Err(EvalError::TypeMismatch(
                "expected date dense column".to_string(),
            )),
        }
    }

    pub fn scalar_value_at(&self, index: usize, dtype: &DType) -> ScalarValue {
        match (self, dtype) {
            (Self::Bool(values), _) => ScalarValue::Bool(values[index]),
            (Self::Integer(values), DType::Integer) => ScalarValue::Integer(values[index]),
            (Self::Integer(values), _) => ScalarValue::Decimal(Decimal::from(values[index])),
            (Self::Decimal(values), _) => ScalarValue::Decimal(values[index]),
            (Self::Text(values), _) => ScalarValue::Text(values[index].clone()),
            (Self::Date(values), _) => ScalarValue::Date(values[index]),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct DenseRelationKey {
    pub name: String,
    pub current_slot: usize,
    pub related_slot: usize,
}

#[derive(Clone, Debug)]
pub struct DenseRelationSchema {
    pub key: DenseRelationKey,
    pub related_inputs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DenseRelationBatchSpec {
    pub offsets: Vec<usize>,
    pub inputs: HashMap<String, DenseColumn>,
}

#[derive(Clone, Debug)]
pub struct DenseBatchSpec {
    pub row_count: usize,
    pub inputs: HashMap<String, DenseColumn>,
    pub relations: HashMap<DenseRelationKey, DenseRelationBatchSpec>,
}

#[derive(Clone, Debug)]
struct DenseRelationBatch {
    offsets: Vec<usize>,
    related_count: usize,
    inputs: Vec<Option<DenseColumn>>,
}

#[derive(Clone, Debug)]
struct DenseBoundBatch {
    row_count: usize,
    /// None entries indicate an optional root input that the caller did not
    /// supply; the executor will fall back to its per-reference default.
    inputs: Vec<Option<DenseColumn>>,
    relations: Vec<DenseRelationBatch>,
}

#[derive(Clone, Debug)]
pub enum DenseOutputValue {
    Scalar(DenseColumn),
    Judgment(Vec<JudgmentOutcome>),
}

#[derive(Clone, Debug)]
pub struct DenseExecutionResult {
    pub row_count: usize,
    pub outputs: HashMap<String, DenseOutputValue>,
}

#[derive(Debug, Error)]
pub enum DenseCompileError {
    #[error(transparent)]
    Eval(#[from] EvalError),
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
    #[error(
        "dense compilation requires an explicit entity because the RuleSpec module defines multiple derived entities"
    )]
    AmbiguousRootEntity,
    #[error("dense compilation could not find derived outputs for entity `{0}`")]
    UnknownEntity(String),
    #[error("dense compilation does not yet support {0}")]
    Unsupported(String),
    #[error(
        "dense compilation only supports dependencies within the same root entity; `{dependency}` from `{derived}` crosses into `{entity}`"
    )]
    CrossEntityDependency {
        derived: String,
        dependency: String,
        entity: String,
    },
}

#[derive(Clone, Debug)]
enum CompiledScalarExpr {
    Literal(ScalarValue),
    Input(usize),
    InputOrElse {
        input: usize,
        default: ScalarValue,
    },
    Derived(usize),
    ParameterLookup {
        parameter: usize,
        index: Box<CompiledScalarExpr>,
    },
    Add(Vec<CompiledScalarExpr>),
    Sub(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Mul(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Div(Box<CompiledScalarExpr>, Box<CompiledScalarExpr>),
    Max(Vec<CompiledScalarExpr>),
    Min(Vec<CompiledScalarExpr>),
    Ceil(Box<CompiledScalarExpr>),
    Floor(Box<CompiledScalarExpr>),
    PeriodStart,
    PeriodEnd,
    DateAddDays {
        date: Box<CompiledScalarExpr>,
        days: Box<CompiledScalarExpr>,
    },
    DaysBetween {
        from: Box<CompiledScalarExpr>,
        to: Box<CompiledScalarExpr>,
    },
    CountRelated {
        relation: usize,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    SumRelatedInput {
        relation: usize,
        input: usize,
        predicate: Option<CompiledRelatedJudgmentExpr>,
    },
    If {
        condition: Box<CompiledJudgmentExpr>,
        then_expr: Box<CompiledScalarExpr>,
        else_expr: Box<CompiledScalarExpr>,
    },
}

#[derive(Clone, Debug)]
enum CompiledRelatedScalarExpr {
    Literal(ScalarValue),
    Input(usize),
    InputOrElse { input: usize, default: ScalarValue },
}

#[derive(Clone, Debug)]
enum CompiledRelatedJudgmentExpr {
    Comparison {
        left: CompiledRelatedScalarExpr,
        op: ComparisonOp,
        right: CompiledRelatedScalarExpr,
    },
    And(Vec<CompiledRelatedJudgmentExpr>),
    Or(Vec<CompiledRelatedJudgmentExpr>),
    Not(Box<CompiledRelatedJudgmentExpr>),
}

#[derive(Clone, Debug)]
enum CompiledJudgmentExpr {
    Comparison {
        left: CompiledScalarExpr,
        op: ComparisonOp,
        right: CompiledScalarExpr,
    },
    Derived(usize),
    And(Vec<CompiledJudgmentExpr>),
    Or(Vec<CompiledJudgmentExpr>),
    Not(Box<CompiledJudgmentExpr>),
}

#[derive(Clone, Debug)]
enum CompiledSemantics {
    Scalar(CompiledScalarExpr),
    Judgment(CompiledJudgmentExpr),
}

#[derive(Clone, Debug)]
struct CompiledDerived {
    name: String,
    semantics: CompiledSemantics,
}

#[derive(Clone, Debug)]
struct CompiledParameter {
    parameter: IndexedParameter,
}

#[derive(Clone, Debug)]
pub struct DenseCompiledProgram {
    root_entity: String,
    root_inputs: Vec<String>,
    /// Set of root input indices that are only ever referenced via
    /// `input_or_else`. These may be omitted at execution time — the
    /// per-reference default is inlined by the executor.
    optional_root_inputs: HashSet<usize>,
    relations: Vec<DenseRelationSchema>,
    /// Per-relation: indices of related inputs that are only ever referenced
    /// via `input_or_else` inside a `where` predicate.
    optional_related_inputs: Vec<HashSet<usize>>,
    parameters: Vec<CompiledParameter>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
}

impl DenseCompiledProgram {
    pub fn from_artifact(
        artifact: &CompiledProgramArtifact,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
        Self::from_program(&artifact.program.to_program()?, entity)
    }

    pub fn from_program(
        program: &Program,
        entity: Option<&str>,
    ) -> Result<Self, DenseCompileError> {
        if let Some(relation) = program
            .relations
            .values()
            .find(|relation| relation.derivation.is_some())
        {
            return Err(DenseCompileError::Unsupported(format!(
                "derived relation `{}`",
                relation.name
            )));
        }
        let root_entity = match entity {
            Some(entity) => entity.to_string(),
            None => {
                let entities = program
                    .derived
                    .values()
                    .map(|derived| derived.entity.clone())
                    .collect::<HashSet<String>>();
                if entities.len() != 1 {
                    return Err(DenseCompileError::AmbiguousRootEntity);
                }
                entities.into_iter().next().expect("one entity")
            }
        };

        let available = program
            .derived
            .values()
            .filter(|derived| derived.entity == root_entity)
            .map(|derived| derived.name.clone())
            .collect::<Vec<String>>();
        if available.is_empty() {
            return Err(DenseCompileError::UnknownEntity(root_entity));
        }

        let mut compiler = DenseCompiler::new(program, root_entity.clone())?;
        for name in available {
            compiler.compile_derived(&name)?;
        }
        Ok(compiler.finish())
    }

    pub fn root_entity(&self) -> &str {
        &self.root_entity
    }

    pub fn root_inputs(&self) -> &[String] {
        &self.root_inputs
    }

    pub fn relations(&self) -> &[DenseRelationSchema] {
        &self.relations
    }

    pub fn output_names(&self) -> Vec<String> {
        self.derived
            .iter()
            .map(|derived| derived.name.clone())
            .collect()
    }

    pub fn execute(
        &self,
        period: &Period,
        batch: DenseBatchSpec,
        outputs: &[String],
    ) -> Result<DenseExecutionResult, EvalError> {
        let batch = self.bind_batch(batch)?;
        let mut executor = DenseExecutor::new(self, period, batch);
        let mut result = HashMap::new();
        for output in outputs {
            let Some(&derived_index) = self.derived_index.get(output) else {
                return Err(EvalError::UnknownDerived(output.clone()));
            };
            let derived = &self.derived[derived_index];
            let value = match &derived.semantics {
                CompiledSemantics::Scalar(_) => {
                    DenseOutputValue::Scalar(executor.evaluate_scalar(derived_index)?.clone())
                }
                CompiledSemantics::Judgment(_) => {
                    DenseOutputValue::Judgment(executor.evaluate_judgment(derived_index)?.clone())
                }
            };
            result.insert(output.clone(), value);
        }
        Ok(DenseExecutionResult {
            row_count: executor.batch.row_count,
            outputs: result,
        })
    }

    fn bind_batch(&self, batch: DenseBatchSpec) -> Result<DenseBoundBatch, EvalError> {
        for (name, column) in &batch.inputs {
            if column.len() != batch.row_count {
                return Err(EvalError::TypeMismatch(format!(
                    "dense root input `{name}` has length {} but row_count is {}",
                    column.len(),
                    batch.row_count
                )));
            }
        }

        let bound_inputs = self
            .root_inputs
            .iter()
            .enumerate()
            .map(|(index, name)| match batch.inputs.get(name).cloned() {
                Some(column) => Ok(Some(column)),
                None if self.optional_root_inputs.contains(&index) => Ok(None),
                None => Err(EvalError::MissingInput {
                    name: name.clone(),
                    entity_id: self.root_entity.clone(),
                    period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                    period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                }),
            })
            .collect::<Result<Vec<Option<DenseColumn>>, EvalError>>()?;

        let mut bound_relations = Vec::with_capacity(self.relations.len());
        for (relation_index, relation) in self.relations.iter().enumerate() {
            let relation_batch = batch.relations.get(&relation.key).ok_or_else(|| {
                EvalError::UnknownRelation(format!(
                    "{}::{}/{}/{}",
                    relation.key.name,
                    relation.key.current_slot,
                    relation.key.related_slot,
                    self.root_entity
                ))
            })?;

            if relation_batch.offsets.len() != batch.row_count + 1 {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must have length {}",
                    relation.key.name,
                    batch.row_count + 1
                )));
            }
            if relation_batch.offsets.first().copied().unwrap_or_default() != 0 {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must start at 0",
                    relation.key.name
                )));
            }
            if !relation_batch
                .offsets
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
            {
                return Err(EvalError::TypeMismatch(format!(
                    "dense relation `{}` offsets must be non-decreasing",
                    relation.key.name
                )));
            }

            let related_count = *relation_batch.offsets.last().unwrap_or(&0);
            let optional_for_relation = &self.optional_related_inputs[relation_index];
            let bound_inputs = relation
                .related_inputs
                .iter()
                .enumerate()
                .map(|(input_index, name)| {
                    let column = match relation_batch.inputs.get(name).cloned() {
                        Some(column) => Some(column),
                        None if optional_for_relation.contains(&input_index) => None,
                        None => {
                            return Err(EvalError::MissingInput {
                                name: name.clone(),
                                entity_id: relation.key.name.clone(),
                                period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1)
                                    .expect("date"),
                                period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1)
                                    .expect("date"),
                            });
                        }
                    };
                    if let Some(column) = &column {
                        if column.len() != related_count {
                            return Err(EvalError::TypeMismatch(format!(
                                "dense relation input `{}` for `{}` has length {} but related row count is {}",
                                name,
                                relation.key.name,
                                column.len(),
                                related_count
                            )));
                        }
                    }
                    Ok(column)
                })
                .collect::<Result<Vec<Option<DenseColumn>>, EvalError>>()?;

            bound_relations.push(DenseRelationBatch {
                offsets: relation_batch.offsets.clone(),
                related_count,
                inputs: bound_inputs,
            });
        }

        Ok(DenseBoundBatch {
            row_count: batch.row_count,
            inputs: bound_inputs,
            relations: bound_relations,
        })
    }
}

struct DenseCompiler<'a> {
    program: &'a Program,
    root_entity: String,
    root_inputs: Vec<String>,
    root_input_index: HashMap<String, usize>,
    /// Root input indices that have only ever been referenced via
    /// `input_or_else`. If a bare `input` reference lands later, the index
    /// is evicted.
    optional_root_inputs: HashSet<usize>,
    relations: Vec<DenseRelationSchema>,
    relation_index: HashMap<DenseRelationKey, usize>,
    relation_input_index: HashMap<(usize, String), usize>,
    /// Per-relation, related-input indices that have only ever been referenced
    /// via `input_or_else` inside a `where` predicate.
    optional_related_inputs: Vec<HashSet<usize>>,
    parameters: Vec<CompiledParameter>,
    parameter_index: HashMap<String, usize>,
    derived: Vec<CompiledDerived>,
    derived_index: HashMap<String, usize>,
    visiting: HashSet<String>,
}

impl<'a> DenseCompiler<'a> {
    fn new(program: &'a Program, root_entity: String) -> Result<Self, DenseCompileError> {
        Ok(Self {
            program,
            root_entity,
            root_inputs: Vec::new(),
            root_input_index: HashMap::new(),
            optional_root_inputs: HashSet::new(),
            relations: Vec::new(),
            relation_index: HashMap::new(),
            relation_input_index: HashMap::new(),
            optional_related_inputs: Vec::new(),
            parameters: Vec::new(),
            parameter_index: HashMap::new(),
            derived: Vec::new(),
            derived_index: HashMap::new(),
            visiting: HashSet::new(),
        })
    }

    fn finish(self) -> DenseCompiledProgram {
        DenseCompiledProgram {
            root_entity: self.root_entity,
            root_inputs: self.root_inputs,
            optional_root_inputs: self.optional_root_inputs,
            relations: self.relations,
            optional_related_inputs: self.optional_related_inputs,
            parameters: self.parameters,
            derived: self.derived,
            derived_index: self.derived_index,
        }
    }

    fn compile_derived(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        if let Some(&index) = self.derived_index.get(name) {
            return Ok(index);
        }
        if self.visiting.contains(name) {
            return Err(DenseCompileError::Unsupported(format!(
                "cyclic dense compilation dependency involving `{name}`"
            )));
        }

        let derived =
            self.program.derived.get(name).ok_or_else(|| {
                DenseCompileError::Unsupported(format!("unknown derived `{name}`"))
            })?;
        if derived.entity != self.root_entity {
            return Err(DenseCompileError::CrossEntityDependency {
                derived: name.to_string(),
                dependency: name.to_string(),
                entity: derived.entity.clone(),
            });
        }

        self.visiting.insert(name.to_string());
        let compiled_semantics = match &derived.semantics {
            DerivedSemantics::Scalar(expr) => {
                CompiledSemantics::Scalar(self.compile_scalar_expr(name, expr)?)
            }
            DerivedSemantics::Judgment(expr) => {
                CompiledSemantics::Judgment(self.compile_judgment_expr(name, expr)?)
            }
        };
        self.visiting.remove(name);

        let index = self.derived.len();
        self.derived.push(CompiledDerived {
            name: derived.name.clone(),
            semantics: compiled_semantics,
        });
        self.derived_index.insert(name.to_string(), index);
        Ok(index)
    }

    fn compile_scalar_expr(
        &mut self,
        derived_name: &str,
        expr: &ScalarExpr,
    ) -> Result<CompiledScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => Ok(CompiledScalarExpr::Input(self.root_input(name, false))),
            ScalarExpr::InputOrElse { name, default } => Ok(CompiledScalarExpr::InputOrElse {
                input: self.root_input(name, true),
                default: default.clone(),
            }),
            ScalarExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown scalar dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != self.root_entity {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    });
                }
                Ok(CompiledScalarExpr::Derived(self.compile_derived(name)?))
            }
            ScalarExpr::ParameterLookup { parameter, index } => {
                Ok(CompiledScalarExpr::ParameterLookup {
                    parameter: self.parameter(parameter)?,
                    index: Box::new(self.compile_scalar_expr(derived_name, index)?),
                })
            }
            ScalarExpr::Add(items) => Ok(CompiledScalarExpr::Add(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
            )),
            ScalarExpr::Sub(left, right) => Ok(CompiledScalarExpr::Sub(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Mul(left, right) => Ok(CompiledScalarExpr::Mul(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Div(left, right) => Ok(CompiledScalarExpr::Div(
                Box::new(self.compile_scalar_expr(derived_name, left)?),
                Box::new(self.compile_scalar_expr(derived_name, right)?),
            )),
            ScalarExpr::Max(items) => Ok(CompiledScalarExpr::Max(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
            )),
            ScalarExpr::Min(items) => Ok(CompiledScalarExpr::Min(
                items
                    .iter()
                    .map(|item| self.compile_scalar_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledScalarExpr>, DenseCompileError>>()?,
            )),
            ScalarExpr::Ceil(value) => Ok(CompiledScalarExpr::Ceil(Box::new(
                self.compile_scalar_expr(derived_name, value)?,
            ))),
            ScalarExpr::Floor(value) => Ok(CompiledScalarExpr::Floor(Box::new(
                self.compile_scalar_expr(derived_name, value)?,
            ))),
            ScalarExpr::PeriodStart => Ok(CompiledScalarExpr::PeriodStart),
            ScalarExpr::PeriodEnd => Ok(CompiledScalarExpr::PeriodEnd),
            ScalarExpr::DateAddDays { date, days } => Ok(CompiledScalarExpr::DateAddDays {
                date: Box::new(self.compile_scalar_expr(derived_name, date)?),
                days: Box::new(self.compile_scalar_expr(derived_name, days)?),
            }),
            ScalarExpr::DaysBetween { from, to } => Ok(CompiledScalarExpr::DaysBetween {
                from: Box::new(self.compile_scalar_expr(derived_name, from)?),
                to: Box::new(self.compile_scalar_expr(derived_name, to)?),
            }),
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                let relation_index = self.relation(relation, *current_slot, *related_slot);
                let predicate = where_clause
                    .as_deref()
                    .map(|inner| self.compile_related_predicate(relation_index, inner))
                    .transpose()?;
                Ok(CompiledScalarExpr::CountRelated {
                    relation: relation_index,
                    predicate,
                })
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => match value {
                RelatedValueRef::Input(name) => {
                    let relation_index = self.relation(relation, *current_slot, *related_slot);
                    let input_index = self.related_input(relation_index, name, false);
                    let predicate = where_clause
                        .as_deref()
                        .map(|inner| self.compile_related_predicate(relation_index, inner))
                        .transpose()?;
                    Ok(CompiledScalarExpr::SumRelatedInput {
                        relation: relation_index,
                        input: input_index,
                        predicate,
                    })
                }
                RelatedValueRef::Derived(name) => Err(DenseCompileError::Unsupported(format!(
                    "sum_related over related derived values (`{name}`)"
                ))),
            },
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(CompiledScalarExpr::If {
                condition: Box::new(self.compile_judgment_expr(derived_name, condition)?),
                then_expr: Box::new(self.compile_scalar_expr(derived_name, then_expr)?),
                else_expr: Box::new(self.compile_scalar_expr(derived_name, else_expr)?),
            }),
        }
    }

    fn compile_judgment_expr(
        &mut self,
        derived_name: &str,
        expr: &JudgmentExpr,
    ) -> Result<CompiledJudgmentExpr, DenseCompileError> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => Ok(CompiledJudgmentExpr::Comparison {
                left: self.compile_scalar_expr(derived_name, left)?,
                op: *op,
                right: self.compile_scalar_expr(derived_name, right)?,
            }),
            JudgmentExpr::Derived(name) => {
                let dependency = self.program.derived.get(name).ok_or_else(|| {
                    DenseCompileError::Unsupported(format!(
                        "unknown judgment dependency `{name}` referenced from `{derived_name}`"
                    ))
                })?;
                if dependency.entity != self.root_entity {
                    return Err(DenseCompileError::CrossEntityDependency {
                        derived: derived_name.to_string(),
                        dependency: name.clone(),
                        entity: dependency.entity.clone(),
                    });
                }
                Ok(CompiledJudgmentExpr::Derived(self.compile_derived(name)?))
            }
            JudgmentExpr::RelationMember { relation, .. } => {
                Err(DenseCompileError::Unsupported(format!(
                    "relation predicate `{relation}`"
                )))
            }
            JudgmentExpr::And(items) => Ok(CompiledJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledJudgmentExpr>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_judgment_expr(derived_name, item))
                    .collect::<Result<Vec<CompiledJudgmentExpr>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledJudgmentExpr::Not(Box::new(
                self.compile_judgment_expr(derived_name, item)?,
            ))),
        }
    }

    fn compile_related_predicate(
        &mut self,
        relation_index: usize,
        expr: &JudgmentExpr,
    ) -> Result<CompiledRelatedJudgmentExpr, DenseCompileError> {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                Ok(CompiledRelatedJudgmentExpr::Comparison {
                    left: self.compile_related_scalar(relation_index, left)?,
                    op: *op,
                    right: self.compile_related_scalar(relation_index, right)?,
                })
            }
            JudgmentExpr::Derived(name) => Err(DenseCompileError::Unsupported(format!(
                "where-clause predicates cannot yet reference derived values (`{name}`)"
            ))),
            JudgmentExpr::RelationMember { relation, .. } => {
                Err(DenseCompileError::Unsupported(format!(
                    "where-clause predicates cannot yet reference relation membership (`{relation}`)"
                )))
            }
            JudgmentExpr::And(items) => Ok(CompiledRelatedJudgmentExpr::And(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Or(items) => Ok(CompiledRelatedJudgmentExpr::Or(
                items
                    .iter()
                    .map(|item| self.compile_related_predicate(relation_index, item))
                    .collect::<Result<Vec<_>, DenseCompileError>>()?,
            )),
            JudgmentExpr::Not(item) => Ok(CompiledRelatedJudgmentExpr::Not(Box::new(
                self.compile_related_predicate(relation_index, item)?,
            ))),
        }
    }

    fn compile_related_scalar(
        &mut self,
        relation_index: usize,
        expr: &ScalarExpr,
    ) -> Result<CompiledRelatedScalarExpr, DenseCompileError> {
        match expr {
            ScalarExpr::Literal(value) => Ok(CompiledRelatedScalarExpr::Literal(value.clone())),
            ScalarExpr::Input(name) => {
                let input_index = self.related_input(relation_index, name, false);
                Ok(CompiledRelatedScalarExpr::Input(input_index))
            }
            ScalarExpr::InputOrElse { name, default } => {
                let input_index = self.related_input(relation_index, name, true);
                Ok(CompiledRelatedScalarExpr::InputOrElse {
                    input: input_index,
                    default: default.clone(),
                })
            }
            other => Err(DenseCompileError::Unsupported(format!(
                "where-clause predicates currently only support comparisons between related inputs and literals; got {other:?}"
            ))),
        }
    }

    fn root_input(&mut self, name: &str, optional: bool) -> usize {
        if let Some(&index) = self.root_input_index.get(name) {
            if !optional {
                self.optional_root_inputs.remove(&index);
            }
            return index;
        }
        let index = self.root_inputs.len();
        self.root_inputs.push(name.to_string());
        self.root_input_index.insert(name.to_string(), index);
        if optional {
            self.optional_root_inputs.insert(index);
        }
        index
    }

    fn relation(&mut self, name: &str, current_slot: usize, related_slot: usize) -> usize {
        let key = DenseRelationKey {
            name: name.to_string(),
            current_slot,
            related_slot,
        };
        if let Some(&index) = self.relation_index.get(&key) {
            index
        } else {
            let index = self.relations.len();
            self.relations.push(DenseRelationSchema {
                key: key.clone(),
                related_inputs: Vec::new(),
            });
            self.optional_related_inputs.push(HashSet::new());
            self.relation_index.insert(key, index);
            index
        }
    }

    fn related_input(&mut self, relation: usize, name: &str, optional: bool) -> usize {
        if let Some(&index) = self.relation_input_index.get(&(relation, name.to_string())) {
            if !optional {
                self.optional_related_inputs[relation].remove(&index);
            }
            return index;
        }
        let index = self.relations[relation].related_inputs.len();
        self.relations[relation]
            .related_inputs
            .push(name.to_string());
        self.relation_input_index
            .insert((relation, name.to_string()), index);
        if optional {
            self.optional_related_inputs[relation].insert(index);
        }
        index
    }

    fn parameter(&mut self, name: &str) -> Result<usize, DenseCompileError> {
        if let Some(&index) = self.parameter_index.get(name) {
            return Ok(index);
        }
        let parameter =
            self.program.parameters.get(name).ok_or_else(|| {
                DenseCompileError::Unsupported(format!("unknown parameter `{name}`"))
            })?;
        let index = self.parameters.len();
        self.parameters.push(CompiledParameter {
            parameter: parameter.clone(),
        });
        self.parameter_index.insert(name.to_string(), index);
        Ok(index)
    }
}

struct DenseExecutor<'a> {
    program: &'a DenseCompiledProgram,
    period: &'a Period,
    batch: DenseBoundBatch,
    scalar_cache: Vec<Option<DenseColumn>>,
    judgment_cache: Vec<Option<Vec<JudgmentOutcome>>>,
}

impl<'a> DenseExecutor<'a> {
    fn new(program: &'a DenseCompiledProgram, period: &'a Period, batch: DenseBoundBatch) -> Self {
        Self {
            program,
            period,
            scalar_cache: vec![None; program.derived.len()],
            judgment_cache: vec![None; program.derived.len()],
            batch,
        }
    }

    fn evaluate_scalar(&mut self, derived_index: usize) -> Result<&DenseColumn, EvalError> {
        if self.scalar_cache[derived_index].is_none() {
            let semantics = self.program.derived[derived_index].semantics.clone();
            let column = match semantics {
                CompiledSemantics::Scalar(expr) => self.eval_scalar_expr(&expr)?,
                CompiledSemantics::Judgment(_) => {
                    return Err(EvalError::ExpectedScalar(
                        self.program.derived[derived_index].name.clone(),
                    ));
                }
            };
            self.scalar_cache[derived_index] = Some(column);
        }
        Ok(self.scalar_cache[derived_index].as_ref().expect("cached"))
    }

    fn evaluate_judgment(
        &mut self,
        derived_index: usize,
    ) -> Result<&Vec<JudgmentOutcome>, EvalError> {
        if self.judgment_cache[derived_index].is_none() {
            let semantics = self.program.derived[derived_index].semantics.clone();
            let values = match semantics {
                CompiledSemantics::Judgment(expr) => self.eval_judgment_expr(&expr)?,
                CompiledSemantics::Scalar(_) => {
                    return Err(EvalError::ExpectedJudgment(
                        self.program.derived[derived_index].name.clone(),
                    ));
                }
            };
            self.judgment_cache[derived_index] = Some(values);
        }
        Ok(self.judgment_cache[derived_index].as_ref().expect("cached"))
    }

    fn eval_scalar_expr(&mut self, expr: &CompiledScalarExpr) -> Result<DenseColumn, EvalError> {
        match expr {
            CompiledScalarExpr::Literal(value) => Ok(match value {
                ScalarValue::Bool(value) => DenseColumn::Bool(vec![*value; self.batch.row_count]),
                ScalarValue::Integer(value) => {
                    DenseColumn::Integer(vec![*value; self.batch.row_count])
                }
                ScalarValue::Decimal(value) => {
                    DenseColumn::Decimal(vec![*value; self.batch.row_count])
                }
                ScalarValue::Text(value) => {
                    DenseColumn::Text(vec![value.clone(); self.batch.row_count])
                }
                ScalarValue::Date(value) => DenseColumn::Date(vec![*value; self.batch.row_count]),
            }),
            CompiledScalarExpr::Input(index) => {
                self.batch.inputs[*index]
                    .clone()
                    .ok_or_else(|| EvalError::MissingInput {
                        name: self.program.root_inputs[*index].clone(),
                        entity_id: self.program.root_entity.clone(),
                        period_start: self.period.start,
                        period_end: self.period.end,
                    })
            }
            CompiledScalarExpr::InputOrElse { input, default } => {
                match &self.batch.inputs[*input] {
                    Some(column) => Ok(column.clone()),
                    None => Ok(broadcast_scalar_literal(default, self.batch.row_count)),
                }
            }
            CompiledScalarExpr::Derived(index) => Ok(self.evaluate_scalar(*index)?.clone()),
            CompiledScalarExpr::ParameterLookup { parameter, index } => {
                let keys = self.eval_scalar_expr(index)?.as_index_vec()?;
                lookup_parameter_dense(
                    &self.program.parameters[*parameter].parameter,
                    &keys,
                    self.period,
                )
            }
            CompiledScalarExpr::Add(items) => {
                let mut total = vec![Decimal::ZERO; self.batch.row_count];
                for item in items {
                    let values = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in values.into_iter().enumerate() {
                        total[index] += value;
                    }
                }
                Ok(DenseColumn::Decimal(total))
            }
            CompiledScalarExpr::Sub(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(DenseColumn::Decimal(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| left - right)
                        .collect(),
                ))
            }
            CompiledScalarExpr::Mul(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(DenseColumn::Decimal(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| left * right)
                        .collect(),
                ))
            }
            CompiledScalarExpr::Div(left, right) => {
                let left = self.eval_scalar_expr(left)?.as_decimal_vec()?;
                let right = self.eval_scalar_expr(right)?.as_decimal_vec()?;
                Ok(DenseColumn::Decimal(
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
            CompiledScalarExpr::Max(items) => {
                let mut values = vec![Decimal::MIN; self.batch.row_count];
                for item in items {
                    let candidate = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in candidate.into_iter().enumerate() {
                        if value > values[index] {
                            values[index] = value;
                        }
                    }
                }
                Ok(DenseColumn::Decimal(values))
            }
            CompiledScalarExpr::Min(items) => {
                let mut values = vec![Decimal::MAX; self.batch.row_count];
                for item in items {
                    let candidate = self.eval_scalar_expr(item)?.as_decimal_vec()?;
                    for (index, value) in candidate.into_iter().enumerate() {
                        if value < values[index] {
                            values[index] = value;
                        }
                    }
                }
                Ok(DenseColumn::Decimal(values))
            }
            CompiledScalarExpr::Ceil(value) => Ok(DenseColumn::Decimal(
                self.eval_scalar_expr(value)?
                    .as_decimal_vec()?
                    .into_iter()
                    .map(|value| value.ceil())
                    .collect(),
            )),
            CompiledScalarExpr::Floor(value) => Ok(DenseColumn::Decimal(
                self.eval_scalar_expr(value)?
                    .as_decimal_vec()?
                    .into_iter()
                    .map(|value| value.floor())
                    .collect(),
            )),
            CompiledScalarExpr::PeriodStart => Ok(DenseColumn::Date(vec![
                self.period.start;
                self.batch.row_count
            ])),
            CompiledScalarExpr::PeriodEnd => Ok(DenseColumn::Date(vec![
                self.period.end;
                self.batch.row_count
            ])),
            CompiledScalarExpr::DateAddDays { date, days } => {
                let base = self.eval_scalar_expr(date)?.as_date_vec()?;
                let offset = self.eval_scalar_expr(days)?.as_index_vec()?;
                Ok(DenseColumn::Date(
                    base.into_iter()
                        .zip(offset)
                        .map(|(base, offset)| base + chrono::Duration::days(offset))
                        .collect(),
                ))
            }
            CompiledScalarExpr::DaysBetween { from, to } => {
                let a = self.eval_scalar_expr(from)?.as_date_vec()?;
                let b = self.eval_scalar_expr(to)?.as_date_vec()?;
                Ok(DenseColumn::Integer(
                    a.into_iter()
                        .zip(b)
                        .map(|(a, b)| (b - a).num_days())
                        .collect(),
                ))
            }
            CompiledScalarExpr::CountRelated {
                relation,
                predicate,
            } => {
                let relation_batch = &self.batch.relations[*relation];
                if let Some(predicate) = predicate {
                    let mask = eval_related_predicate(
                        predicate,
                        &relation_batch.inputs,
                        relation_batch.related_count,
                    )?;
                    let mut counts = Vec::with_capacity(self.batch.row_count);
                    for row in 0..self.batch.row_count {
                        let start = relation_batch.offsets[row];
                        let end = relation_batch.offsets[row + 1];
                        let matched = mask[start..end].iter().filter(|keep| **keep).count() as i64;
                        counts.push(matched);
                    }
                    Ok(DenseColumn::Integer(counts))
                } else {
                    Ok(DenseColumn::Integer(
                        relation_batch
                            .offsets
                            .windows(2)
                            .map(|pair| (pair[1] - pair[0]) as i64)
                            .collect(),
                    ))
                }
            }
            CompiledScalarExpr::SumRelatedInput {
                relation,
                input,
                predicate,
            } => {
                let relation_batch = &self.batch.relations[*relation];
                let values = relation_batch.inputs[*input]
                    .as_ref()
                    .ok_or_else(|| EvalError::MissingInput {
                        name: format!("related_input[{input}]"),
                        entity_id: String::new(),
                        period_start: self.period.start,
                        period_end: self.period.end,
                    })?
                    .as_decimal_vec()?;
                let mask = predicate
                    .as_ref()
                    .map(|predicate| {
                        eval_related_predicate(
                            predicate,
                            &relation_batch.inputs,
                            relation_batch.related_count,
                        )
                    })
                    .transpose()?;
                let mut totals = Vec::with_capacity(self.batch.row_count);
                for row in 0..self.batch.row_count {
                    let start = relation_batch.offsets[row];
                    let end = relation_batch.offsets[row + 1];
                    let mut total = Decimal::ZERO;
                    match &mask {
                        Some(mask) => {
                            for (offset, value) in values[start..end].iter().enumerate() {
                                if mask[start + offset] {
                                    total += *value;
                                }
                            }
                        }
                        None => {
                            for value in &values[start..end] {
                                total += *value;
                            }
                        }
                    }
                    totals.push(total);
                }
                Ok(DenseColumn::Decimal(totals))
            }
            CompiledScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.eval_judgment_expr(condition)?;
                let then_values = self.eval_scalar_expr(then_expr)?;
                let else_values = self.eval_scalar_expr(else_expr)?;
                select_dense_scalar_column(condition, then_values, else_values)
            }
        }
    }

    fn eval_judgment_expr(
        &mut self,
        expr: &CompiledJudgmentExpr,
    ) -> Result<Vec<JudgmentOutcome>, EvalError> {
        match expr {
            CompiledJudgmentExpr::Comparison { left, op, right } => {
                let left = self.eval_scalar_expr(left)?;
                let right = self.eval_scalar_expr(right)?;
                compare_dense_columns(left, *op, right)
            }
            CompiledJudgmentExpr::Derived(index) => Ok(self.evaluate_judgment(*index)?.clone()),
            CompiledJudgmentExpr::And(items) => {
                let mut results = vec![JudgmentOutcome::Holds; self.batch.row_count];
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
            CompiledJudgmentExpr::Or(items) => {
                let mut results = vec![JudgmentOutcome::NotHolds; self.batch.row_count];
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
            CompiledJudgmentExpr::Not(item) => Ok(self
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
}

fn eval_related_predicate(
    expr: &CompiledRelatedJudgmentExpr,
    related_inputs: &[Option<DenseColumn>],
    related_count: usize,
) -> Result<Vec<bool>, EvalError> {
    let length = related_count;
    match expr {
        CompiledRelatedJudgmentExpr::Comparison { left, op, right } => {
            let left = resolve_related_scalar(left, related_inputs, length)?;
            let right = resolve_related_scalar(right, related_inputs, length)?;
            Ok(compare_related_columns(&left, *op, &right)?)
        }
        CompiledRelatedJudgmentExpr::And(items) => {
            let mut result = vec![true; length];
            for item in items {
                let sub = eval_related_predicate(item, related_inputs, related_count)?;
                for (index, keep) in sub.into_iter().enumerate() {
                    result[index] &= keep;
                }
            }
            Ok(result)
        }
        CompiledRelatedJudgmentExpr::Or(items) => {
            let mut result = vec![false; length];
            for item in items {
                let sub = eval_related_predicate(item, related_inputs, related_count)?;
                for (index, keep) in sub.into_iter().enumerate() {
                    result[index] |= keep;
                }
            }
            Ok(result)
        }
        CompiledRelatedJudgmentExpr::Not(item) => {
            Ok(eval_related_predicate(item, related_inputs, related_count)?
                .into_iter()
                .map(|keep| !keep)
                .collect())
        }
    }
}

fn resolve_related_scalar(
    expr: &CompiledRelatedScalarExpr,
    related_inputs: &[Option<DenseColumn>],
    length: usize,
) -> Result<DenseColumn, EvalError> {
    match expr {
        CompiledRelatedScalarExpr::Literal(value) => Ok(broadcast_scalar_literal(value, length)),
        CompiledRelatedScalarExpr::Input(index) => {
            related_inputs[*index]
                .clone()
                .ok_or_else(|| EvalError::MissingInput {
                    name: format!("related_input[{index}]"),
                    entity_id: String::new(),
                    period_start: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                    period_end: chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("date"),
                })
        }
        CompiledRelatedScalarExpr::InputOrElse { input, default } => {
            match &related_inputs[*input] {
                Some(column) => Ok(column.clone()),
                None => Ok(broadcast_scalar_literal(default, length)),
            }
        }
    }
}

fn compare_related_columns(
    left: &DenseColumn,
    op: ComparisonOp,
    right: &DenseColumn,
) -> Result<Vec<bool>, EvalError> {
    match (left, right) {
        (DenseColumn::Bool(left), DenseColumn::Bool(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
                _ => false,
            })
            .collect()),
        (DenseColumn::Text(left), DenseColumn::Text(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
                _ => false,
            })
            .collect()),
        (DenseColumn::Date(left), DenseColumn::Date(right)) => Ok(left
            .iter()
            .zip(right.iter())
            .map(|(left, right)| match op {
                ComparisonOp::Lt => left < right,
                ComparisonOp::Lte => left <= right,
                ComparisonOp::Gt => left > right,
                ComparisonOp::Gte => left >= right,
                ComparisonOp::Eq => left == right,
                ComparisonOp::Ne => left != right,
            })
            .collect()),
        (left, right) => {
            let left = left.as_decimal_vec()?;
            let right = right.as_decimal_vec()?;
            Ok(left
                .into_iter()
                .zip(right)
                .map(|(left, right)| match op {
                    ComparisonOp::Lt => left < right,
                    ComparisonOp::Lte => left <= right,
                    ComparisonOp::Gt => left > right,
                    ComparisonOp::Gte => left >= right,
                    ComparisonOp::Eq => left == right,
                    ComparisonOp::Ne => left != right,
                })
                .collect())
        }
    }
}

fn broadcast_scalar_literal(value: &ScalarValue, length: usize) -> DenseColumn {
    match value {
        ScalarValue::Bool(value) => DenseColumn::Bool(vec![*value; length]),
        ScalarValue::Integer(value) => DenseColumn::Integer(vec![*value; length]),
        ScalarValue::Decimal(value) => DenseColumn::Decimal(vec![*value; length]),
        ScalarValue::Text(value) => DenseColumn::Text(vec![value.clone(); length]),
        ScalarValue::Date(value) => DenseColumn::Date(vec![*value; length]),
    }
}

fn lookup_parameter_dense(
    parameter: &IndexedParameter,
    keys: &[i64],
    period: &Period,
) -> Result<DenseColumn, EvalError> {
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
        Ok(DenseColumn::Integer(
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
    } else if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Bool(_)))
    {
        Ok(DenseColumn::Bool(
            values
                .into_iter()
                .map(|value| match value {
                    ScalarValue::Bool(value) => Ok(value),
                    _ => Err(EvalError::TypeMismatch(
                        "mixed parameter dtypes are not supported".to_string(),
                    )),
                })
                .collect::<Result<Vec<bool>, EvalError>>()?,
        ))
    } else if values
        .iter()
        .all(|value| matches!(value, ScalarValue::Text(_)))
    {
        Ok(DenseColumn::Text(
            values
                .into_iter()
                .map(|value| match value {
                    ScalarValue::Text(value) => Ok(value),
                    _ => Err(EvalError::TypeMismatch(
                        "mixed parameter dtypes are not supported".to_string(),
                    )),
                })
                .collect::<Result<Vec<String>, EvalError>>()?,
        ))
    } else {
        Ok(DenseColumn::Decimal(
            values
                .into_iter()
                .map(|value| {
                    value.as_decimal().ok_or_else(|| {
                        EvalError::TypeMismatch(
                            "parameter values must be numeric in dense mode".to_string(),
                        )
                    })
                })
                .collect::<Result<Vec<Decimal>, EvalError>>()?,
        ))
    }
}

fn select_dense_scalar_column(
    condition: Vec<JudgmentOutcome>,
    then_values: DenseColumn,
    else_values: DenseColumn,
) -> Result<DenseColumn, EvalError> {
    match (then_values, else_values) {
        (DenseColumn::Decimal(then_values), DenseColumn::Decimal(else_values)) => {
            Ok(DenseColumn::Decimal(
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
        (DenseColumn::Integer(then_values), DenseColumn::Integer(else_values)) => {
            Ok(DenseColumn::Integer(
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
        (DenseColumn::Decimal(then_values), DenseColumn::Integer(else_values)) => {
            Ok(DenseColumn::Decimal(
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
        (DenseColumn::Integer(then_values), DenseColumn::Decimal(else_values)) => {
            Ok(DenseColumn::Decimal(
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
        (DenseColumn::Bool(then_values), DenseColumn::Bool(else_values)) => Ok(DenseColumn::Bool(
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
        )),
        (DenseColumn::Text(then_values), DenseColumn::Text(else_values)) => Ok(DenseColumn::Text(
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
        )),
        (DenseColumn::Date(then_values), DenseColumn::Date(else_values)) => Ok(DenseColumn::Date(
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
        )),
        _ => Err(EvalError::TypeMismatch(
            "dense if() branches must have the same dtype".to_string(),
        )),
    }
}

fn compare_dense_columns(
    left: DenseColumn,
    op: ComparisonOp,
    right: DenseColumn,
) -> Result<Vec<JudgmentOutcome>, EvalError> {
    match (left, right) {
        (DenseColumn::Bool(left), DenseColumn::Bool(right)) => Ok(left
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
        (DenseColumn::Text(left), DenseColumn::Text(right)) => Ok(left
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
        (DenseColumn::Date(left), DenseColumn::Date(right)) => Ok(left
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
