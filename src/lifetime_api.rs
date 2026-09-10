//! Bounded JSON transport for the real Decimal lifetime executor.
//!
//! Row identity is caller-declared positional alignment. This interface does
//! not supply missing periods, infer relations, or add a determination period.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::compile::{CompileError, CompiledProgramArtifact};
use crate::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
};
use crate::engine::EvalError;
use crate::model::{Period, PeriodKind};
use crate::spec::{DTypeSpec, JudgmentOutcomeSpec, SpecError};

pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PERIODS: usize = 512;
pub const MAX_ROWS: usize = 100_000;
pub const MAX_COLUMNS: usize = 256;
pub const MAX_CELLS: usize = 2_000_000;
pub const MAX_STRING_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum LifetimeRequestSchema {
    #[serde(rename = "axiom-rules-engine/lifetime-request/v1")]
    V1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum LifetimeResponseSchema {
    #[serde(rename = "axiom-rules-engine/lifetime-response/v1")]
    V1,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LifetimeArithmetic {
    #[default]
    Decimal,
}

/// Same period wire vocabulary as PeriodSpec, with unknown fields refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "period_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifetimePeriod {
    Month {
        start: NaiveDate,
        end: NaiveDate,
    },
    BenefitWeek {
        start: NaiveDate,
        end: NaiveDate,
    },
    TaxYear {
        start: NaiveDate,
        end: NaiveDate,
    },
    Custom {
        name: String,
        start: NaiveDate,
        end: NaiveDate,
    },
}

impl LifetimePeriod {
    fn to_model(&self) -> Period {
        let (kind, start, end) = match self {
            Self::Month { start, end } => (PeriodKind::Month, start, end),
            Self::BenefitWeek { start, end } => (PeriodKind::BenefitWeek, start, end),
            Self::TaxYear { start, end } => (PeriodKind::TaxYear, start, end),
            Self::Custom { name, start, end } => (PeriodKind::Custom(name.clone()), start, end),
        };
        Period {
            kind,
            start: *start,
            end: *end,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifetimeColumn {
    Bool { values: Vec<bool> },
    Integer { values: Vec<i64> },
    Decimal { values: Vec<String> },
    Text { values: Vec<String> },
    Date { values: Vec<NaiveDate> },
}

impl LifetimeColumn {
    fn len(&self) -> usize {
        match self {
            Self::Bool { values } => values.len(),
            Self::Integer { values } => values.len(),
            Self::Decimal { values } | Self::Text { values } => values.len(),
            Self::Date { values } => values.len(),
        }
    }

    fn into_dense(self, field: &str) -> Result<DenseColumn, LifetimeApiError> {
        Ok(match self {
            Self::Bool { values } => DenseColumn::Bool(values),
            Self::Integer { values } => DenseColumn::Integer(values),
            Self::Decimal { values } => DenseColumn::Decimal(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(row, value)| {
                        check_string(&value, field)?;
                        Decimal::from_str_exact(&value).map_err(|_| {
                            LifetimeApiError::invalid(
                                field,
                                format!("row {row} is not an exactly representable decimal"),
                            )
                        })
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Self::Text { values } => {
                for value in &values {
                    check_string(value, field)?;
                }
                DenseColumn::Text(values)
            }
            Self::Date { values } => DenseColumn::Date(values),
        })
    }

    fn from_dense(column: DenseColumn, dtype: &DTypeSpec) -> Result<Self, LifetimeApiError> {
        // Widen integer values exactly when the rule declares Decimal. No
        // other transport coercion (especially Decimal truncation) is allowed.
        let column = match (dtype, column) {
            (DTypeSpec::Decimal, DenseColumn::Integer(values)) => {
                DenseColumn::Decimal(values.into_iter().map(Decimal::from).collect())
            }
            (_, column) => column,
        };
        let actual = match &column {
            DenseColumn::Bool(_) => DTypeSpec::Bool,
            DenseColumn::Integer(_) => DTypeSpec::Integer,
            DenseColumn::Decimal(_) => DTypeSpec::Decimal,
            DenseColumn::Text(_) => DTypeSpec::Text,
            DenseColumn::Date(_) => DTypeSpec::Date,
            DenseColumn::Float(_) => {
                return Err(LifetimeApiError::Output(
                    "Decimal execution returned a float column".into(),
                ));
            }
        };
        if actual != *dtype {
            return Err(LifetimeApiError::Output(format!(
                "declared {dtype:?} differs from evaluated {actual:?}"
            )));
        }
        Ok(match column {
            DenseColumn::Bool(values) => Self::Bool { values },
            DenseColumn::Integer(values) => Self::Integer { values },
            DenseColumn::Decimal(values) => Self::Decimal {
                values: values
                    .into_iter()
                    .map(|value| value.normalize().to_string())
                    .collect(),
            },
            DenseColumn::Text(values) => Self::Text { values },
            DenseColumn::Date(values) => Self::Date { values },
            DenseColumn::Float(_) => unreachable!("float column was refused above"),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LifetimeBatch {
    pub row_count: usize,
    pub entity_ids: Vec<String>,
    pub inputs: BTreeMap<String, LifetimeColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LifetimeExecutionRequest {
    pub schema: LifetimeRequestSchema,
    pub entity: String,
    #[serde(default)]
    pub arithmetic: LifetimeArithmetic,
    pub periods: Vec<LifetimePeriod>,
    pub batches: Vec<LifetimeBatch>,
    pub outputs: Vec<String>,
    pub output_period: LifetimePeriod,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum JudgmentColumnKind {
    #[serde(rename = "judgment")]
    Judgment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged, deny_unknown_fields)]
pub enum LifetimeOutputColumn {
    Scalar(LifetimeColumn),
    Judgment {
        kind: JudgmentColumnKind,
        values: Vec<JudgmentOutcomeSpec>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LifetimeOutput {
    pub id: String,
    pub name: String,
    pub dtype: DTypeSpec,
    pub unit: Option<String>,
    pub column: LifetimeOutputColumn,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LifetimeExecutionResponse {
    pub schema: LifetimeResponseSchema,
    pub engine_version: String,
    pub artifact_format_version: u32,
    pub arithmetic: LifetimeArithmetic,
    pub entity: String,
    pub row_count: usize,
    pub entity_ids: Vec<String>,
    pub periods: Vec<LifetimePeriod>,
    pub reference_period: LifetimePeriod,
    pub output_period: LifetimePeriod,
    pub outputs: BTreeMap<String, LifetimeOutput>,
}

#[derive(Debug, Error)]
pub enum LifetimeApiError {
    #[error("invalid lifetime request at {field}: {message}")]
    Request { field: String, message: String },
    #[error("unsupported lifetime request: {0}")]
    Unsupported(String),
    #[error("invalid lifetime output: {0}")]
    Output(String),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Artifact(#[from] CompileError),
    #[error("{0}")]
    Compile(#[from] DenseCompileError),
    #[error("{0}")]
    Spec(#[from] SpecError),
    #[error("{0}")]
    Evaluation(#[from] EvalError),
}

impl LifetimeApiError {
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Request {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Bounded diagnostic; the typed Rust error and its source are retained.
    pub fn diagnostic(&self) -> Value {
        let category = match self {
            Self::Request { .. } | Self::Json(_) => "invalid_request",
            Self::Unsupported(_) | Self::Compile(_) => "unsupported",
            Self::Artifact(_) => "artifact",
            Self::Spec(_) | Self::Evaluation(_) | Self::Output(_) => "evaluation",
        };
        let field = match self {
            Self::Request { field, .. } => Some(field),
            _ => None,
        };
        json!({
            "schema": "axiom-rules-engine/lifetime-error/v1",
            "category": category,
            "field": field.map(|value| value.chars().take(MAX_STRING_BYTES).collect::<String>()),
            "message": self.to_string().chars().take(MAX_STRING_BYTES).collect::<String>(),
        })
    }
}

fn check_string(value: &str, field: &str) -> Result<(), LifetimeApiError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(LifetimeApiError::invalid(
            field,
            "string exceeds byte limit",
        ));
    }
    Ok(())
}

/// Parse without silently overwriting duplicate object keys at any depth.
pub fn parse_lifetime_request(source: &str) -> Result<LifetimeExecutionRequest, LifetimeApiError> {
    if source.len() > MAX_REQUEST_BYTES {
        return Err(LifetimeApiError::invalid(
            "request",
            "request exceeds byte limit",
        ));
    }
    Ok(serde_json::from_value(parse_unique_json(source)?)?)
}

/// Use the standard artifact admission after checking byte and duplicate-key limits.
pub fn parse_lifetime_artifact(source: &str) -> Result<CompiledProgramArtifact, LifetimeApiError> {
    if source.len() > MAX_ARTIFACT_BYTES {
        return Err(LifetimeApiError::invalid(
            "artifact",
            "artifact exceeds byte limit",
        ));
    }
    parse_unique_json(source)?;
    Ok(CompiledProgramArtifact::from_json_str(source)?)
}

pub fn execute_lifetime_request(
    artifact: CompiledProgramArtifact,
    request: LifetimeExecutionRequest,
) -> Result<LifetimeExecutionResponse, LifetimeApiError> {
    // Public Rust callers can construct artifact structs directly. Re-admit the
    // serialized contract rather than trusting supplied metadata in that case.
    let artifact = parse_lifetime_artifact(&serde_json::to_string(&artifact)?)?;
    check_string(&request.entity, "entity")?;
    if request.periods.is_empty() || request.periods.len() > MAX_PERIODS {
        return Err(LifetimeApiError::invalid(
            "periods",
            "period count must be in 1..=512",
        ));
    }
    if request.periods.len() != request.batches.len() {
        return Err(LifetimeApiError::invalid(
            "batches",
            "one batch is required per period",
        ));
    }
    if request.outputs.is_empty() || request.outputs.len() > MAX_COLUMNS {
        return Err(LifetimeApiError::invalid(
            "outputs",
            "output count must be in 1..=256",
        ));
    }
    if request.periods.last() != Some(&request.output_period) {
        return Err(LifetimeApiError::Unsupported("output_period must equal the final supplied period; a separate determination period is not supported".into()));
    }
    let periods: Vec<Period> = request
        .periods
        .iter()
        .map(LifetimePeriod::to_model)
        .collect();
    for (index, period) in periods.iter().enumerate() {
        if let PeriodKind::Custom(name) = &period.kind {
            check_string(name, "periods.name")?;
            if name.is_empty() {
                return Err(LifetimeApiError::invalid(
                    "periods.name",
                    "custom period name must not be empty",
                ));
            }
        }
        if period.start > period.end {
            return Err(LifetimeApiError::invalid(
                format!("periods[{index}]"),
                "start must not follow end",
            ));
        }
        if index > 0 && (period.kind != periods[0].kind || period.start <= periods[index - 1].end) {
            return Err(LifetimeApiError::invalid(
                format!("periods[{index}]"),
                "periods must have the same kind and be ascending without overlap",
            ));
        }
    }
    let program = artifact.program.to_program()?;
    let dense = DenseCompiledProgram::from_program(&program, Some(&request.entity))?;
    if !dense.relations().is_empty() {
        return Err(LifetimeApiError::Unsupported(
            "relation context is not supported by lifetime JSON v1".into(),
        ));
    }
    let catalog = program.input_catalog();
    let root_inputs: HashSet<&str> = dense.root_inputs().iter().map(String::as_str).collect();
    let available_outputs: HashSet<String> = dense.output_names().into_iter().collect();
    let mut outputs = Vec::new();
    let mut seen_outputs = HashSet::new();
    for reference in &request.outputs {
        check_string(reference, "outputs")?;
        if !reference.contains('#') {
            return Err(LifetimeApiError::invalid(
                "outputs",
                "full durable public output IDs are required",
            ));
        }
        let name = program
            .resolve_derived_name(reference)
            .filter(|name| {
                available_outputs.contains(name)
                    && program
                        .derived
                        .get(name)
                        .and_then(|derived| derived.id.as_deref())
                        == Some(reference.as_str())
            })
            .ok_or_else(|| {
                LifetimeApiError::invalid(
                    "outputs",
                    format!("unknown public output {reference:?} for this entity"),
                )
            })?;
        if !seen_outputs.insert(name.clone()) {
            return Err(LifetimeApiError::invalid(
                "outputs",
                "duplicate resolved output",
            ));
        }
        outputs.push(name);
    }
    let entity_ids = request.batches[0].entity_ids.clone();
    if entity_ids
        .len()
        .checked_mul(outputs.len())
        .is_none_or(|cells| cells > MAX_CELLS)
    {
        return Err(LifetimeApiError::invalid(
            "outputs",
            "output cell count exceeds limit",
        ));
    }
    let mut batches = Vec::with_capacity(request.batches.len());
    let mut cells = 0usize;
    for (index, batch) in request.batches.into_iter().enumerate() {
        let field = format!("batches[{index}]");
        if batch.row_count > MAX_ROWS || batch.entity_ids.len() != batch.row_count {
            return Err(LifetimeApiError::invalid(
                &field,
                "row count exceeds limit or differs from entity_ids length",
            ));
        }
        if batch.entity_ids != entity_ids {
            return Err(LifetimeApiError::invalid(
                &field,
                "entity_ids must match the first batch exactly in order",
            ));
        }
        let mut seen_ids = HashSet::new();
        for id in &batch.entity_ids {
            check_string(id, &field)?;
            if id.is_empty() || !seen_ids.insert(id) {
                return Err(LifetimeApiError::invalid(
                    &field,
                    "entity_ids must be nonempty unique strings",
                ));
            }
        }
        if batch.inputs.len() > MAX_COLUMNS {
            return Err(LifetimeApiError::invalid(
                &field,
                "input column count exceeds limit",
            ));
        }
        cells = cells
            .checked_add(batch.row_count)
            .ok_or_else(|| LifetimeApiError::invalid(&field, "cell count overflow"))?;
        let mut inputs = HashMap::new();
        for (reference, column) in batch.inputs {
            check_string(&reference, &field)?;
            if !reference.contains('#') {
                return Err(LifetimeApiError::invalid(
                    &field,
                    "unknown public input: full durable input IDs are required",
                ));
            }
            let name = program
                .resolve_input_name_with_catalog(&reference, &catalog)
                .filter(|name| {
                    root_inputs.contains(name.as_str())
                        && catalog
                            .get(name)
                            .is_some_and(|references| references.contains(&reference))
                })
                .ok_or_else(|| {
                    LifetimeApiError::invalid(
                        &field,
                        format!("unknown public input {reference:?} for this entity"),
                    )
                })?;
            if inputs.contains_key(&name) {
                return Err(LifetimeApiError::invalid(
                    &field,
                    "multiple public input names resolve to the same slot",
                ));
            }
            if column.len() != batch.row_count {
                return Err(LifetimeApiError::invalid(
                    &field,
                    format!("input {reference:?} length differs from row_count"),
                ));
            }
            cells = cells
                .checked_add(column.len())
                .ok_or_else(|| LifetimeApiError::invalid(&field, "cell count overflow"))?;
            if cells > MAX_CELLS {
                return Err(LifetimeApiError::invalid(
                    &field,
                    "total supplied cell count exceeds limit",
                ));
            }
            inputs.insert(
                name,
                column.into_dense(&format!("{field}.inputs[{reference:?}]"))?,
            );
        }
        if cells > MAX_CELLS {
            return Err(LifetimeApiError::invalid(
                &field,
                "total supplied cell count exceeds limit",
            ));
        }
        batches.push(DenseBatchSpec {
            row_count: batch.row_count,
            inputs,
            relations: HashMap::new(),
        });
    }
    let execution = dense.execute_lifetime(&periods, batches, &outputs)?;
    let mut result = BTreeMap::new();
    for (name, value) in execution.outputs {
        let derived = artifact
            .program
            .derived
            .iter()
            .find(|derived| derived.name == name)
            .expect("dense output is declared by the admitted program");
        let column = match value {
            DenseOutputValue::Scalar(column) => {
                LifetimeOutputColumn::Scalar(LifetimeColumn::from_dense(column, &derived.dtype)?)
            }
            DenseOutputValue::Judgment(values) => {
                if derived.dtype != DTypeSpec::Judgment {
                    return Err(LifetimeApiError::Output(format!(
                        "declared {:?} differs from evaluated Judgment",
                        derived.dtype
                    )));
                }
                LifetimeOutputColumn::Judgment {
                    kind: JudgmentColumnKind::Judgment,
                    values: values.into_iter().map(JudgmentOutcomeSpec::from).collect(),
                }
            }
        };
        result.insert(
            program.public_derived_key(&name),
            LifetimeOutput {
                id: derived.id.clone().ok_or_else(|| {
                    LifetimeApiError::Output("output has no retained public ID".into())
                })?,
                name,
                dtype: derived.dtype.clone(),
                unit: derived.unit.clone(),
                column,
            },
        );
    }
    Ok(LifetimeExecutionResponse {
        schema: LifetimeResponseSchema::V1,
        engine_version: crate::ENGINE_VERSION.into(),
        artifact_format_version: artifact.artifact_format_version,
        arithmetic: LifetimeArithmetic::Decimal,
        entity: request.entity,
        row_count: execution.row_count,
        entity_ids,
        periods: request.periods,
        reference_period: request.output_period.clone(),
        output_period: request.output_period,
        outputs: result,
    })
}

fn parse_unique_json(source: &str) -> Result<Value, serde_json::Error> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct UniqueVisitor;
            impl<'de> Visitor<'de> for UniqueVisitor {
                type Value = Unique;
                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("JSON without duplicate object keys")
                }
                fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Unique, E> {
                    Ok(Unique(value.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Unique, E> {
                    Ok(Unique(value.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Unique, E> {
                    Ok(Unique(value.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Unique, E> {
                    serde_json::Number::from_f64(value)
                        .map(|value| Unique(Value::Number(value)))
                        .ok_or_else(|| E::custom("nonfinite number"))
                }
                fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Unique, E> {
                    Ok(Unique(value.into()))
                }
                fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Unique, E> {
                    Ok(Unique(value.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Unique, A::Error> {
                    let mut values = Vec::new();
                    while let Some(Unique(value)) = sequence.next_element()? {
                        values.push(value);
                    }
                    Ok(Unique(Value::Array(values)))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                        if values.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(Unique(Value::Object(values)))
                }
            }
            deserializer.deserialize_any(UniqueVisitor)
        }
    }
    serde_json::from_str::<Unique>(source).map(|value| value.0)
}
