use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
    DenseRelationSchema,
};
use axiom_rules_engine::model::{JudgmentOutcome, Period, PeriodKind};
use chrono::NaiveDate;
use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use rust_decimal::prelude::ToPrimitive;

#[pyclass(module = "axiom_rules_engine_dense")]
#[derive(Clone)]
struct RelationSchemaHandle {
    #[pyo3(get)]
    key: String,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    current_slot: usize,
    #[pyo3(get)]
    related_slot: usize,
    #[pyo3(get)]
    related_inputs: Vec<String>,
}

impl From<&DenseRelationSchema> for RelationSchemaHandle {
    fn from(schema: &DenseRelationSchema) -> Self {
        Self {
            key: relation_key(&schema.key),
            name: schema.key.name.clone(),
            current_slot: schema.key.current_slot,
            related_slot: schema.key.related_slot,
            related_inputs: schema.related_inputs.clone(),
        }
    }
}

#[pyclass(module = "axiom_rules_engine_dense", name = "CompiledDenseProgram")]
struct CompiledDenseProgramHandle {
    compiled: DenseCompiledProgram,
}

#[pymethods]
impl CompiledDenseProgramHandle {
    #[staticmethod]
    #[pyo3(signature = (path, entity=None))]
    fn from_file(path: &str, entity: Option<&str>) -> PyResult<Self> {
        let artifact = CompiledProgramArtifact::from_rulespec_file(path)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let compiled = DenseCompiledProgram::from_artifact(&artifact, entity)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        Ok(Self { compiled })
    }

    #[getter]
    fn root_entity(&self) -> String {
        self.compiled.root_entity().to_string()
    }

    fn root_inputs(&self) -> Vec<String> {
        self.compiled.root_inputs().to_vec()
    }

    fn output_names(&self) -> Vec<String> {
        self.compiled.output_names()
    }

    fn relations(&self) -> Vec<RelationSchemaHandle> {
        self.compiled
            .relations()
            .iter()
            .map(RelationSchemaHandle::from)
            .collect()
    }

    #[pyo3(signature = (period_kind, start, end, inputs, relations=None, outputs=None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        period_kind: &str,
        start: &str,
        end: &str,
        inputs: Bound<'py, PyDict>,
        relations: Option<Bound<'py, PyDict>>,
        outputs: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let period = Period {
            kind: parse_period_kind(period_kind),
            start: parse_date(start)?,
            end: parse_date(end)?,
        };
        let batch = build_batch(&self.compiled, inputs, relations)?;
        let output_names = outputs.unwrap_or_else(|| self.compiled.output_names());
        let execution = self
            .compiled
            .execute(&period, batch, &output_names)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;

        let output_dict = PyDict::new(py);
        for (name, value) in execution.outputs {
            match value {
                axiom_rules_engine::dense::DenseOutputValue::Scalar(column) => match column {
                    DenseColumn::Bool(values) => {
                        output_dict.set_item(name, PyArray1::from_vec(py, values))?;
                    }
                    DenseColumn::Integer(values) => {
                        output_dict.set_item(name, PyArray1::from_vec(py, values))?;
                    }
                    DenseColumn::Decimal(values) => {
                        let materialised = values
                            .into_iter()
                            .map(|value| {
                                value.to_f64().ok_or_else(|| {
                                    PyRuntimeError::new_err(
                                        "failed to materialise decimal output as float64",
                                    )
                                })
                            })
                            .collect::<PyResult<Vec<f64>>>()?;
                        output_dict.set_item(name, PyArray1::from_vec(py, materialised))?;
                    }
                    DenseColumn::Text(values) => {
                        output_dict.set_item(name, PyList::new(py, values)?)?;
                    }
                    DenseColumn::Date(values) => {
                        let materialised = values
                            .into_iter()
                            .map(|value| value.to_string())
                            .collect::<Vec<String>>();
                        output_dict.set_item(name, PyList::new(py, materialised)?)?;
                    }
                },
                axiom_rules_engine::dense::DenseOutputValue::Judgment(values) => {
                    let materialised = values
                        .into_iter()
                        .map(|value| judgment_code(&value))
                        .collect::<Vec<i8>>();
                    output_dict.set_item(name, PyArray1::from_vec(py, materialised))?;
                }
            }
        }

        let result = PyDict::new(py);
        result.set_item("row_count", execution.row_count)?;
        result.set_item("outputs", output_dict)?;
        Ok(result)
    }
}

#[pymodule]
fn axiom_rules_engine_dense(_py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CompiledDenseProgramHandle>()?;
    module.add_class::<RelationSchemaHandle>()?;
    Ok(())
}

fn build_batch(
    compiled: &DenseCompiledProgram,
    inputs: Bound<'_, PyDict>,
    relations: Option<Bound<'_, PyDict>>,
) -> PyResult<DenseBatchSpec> {
    // Row count comes from the first supplied input column. Optional inputs
    // may legitimately be absent, so we scan until we find one that is.
    let row_count = match compiled
        .root_inputs()
        .iter()
        .find_map(|name| inputs.get_item(name).ok().flatten().map(|c| (name, c)))
    {
        Some((_, column)) => dense_column_from_python(&column)?.len(),
        None => match compiled.relations().first() {
            Some(schema) => {
                let relation_batches = relations.as_ref().ok_or_else(|| {
                    PyValueError::new_err("dense execution requires relation data")
                })?;
                let relation = relation_batches
                    .get_item(relation_key(&schema.key))?
                    .ok_or_else(|| {
                        PyValueError::new_err(format!(
                            "missing dense relation batch `{}`",
                            relation_key(&schema.key)
                        ))
                    })?;
                let relation_dict = relation.cast::<PyDict>()?;
                let offsets = extract_index_vec(
                    &relation_dict
                        .get_item("offsets")?
                        .ok_or_else(|| PyValueError::new_err("missing dense relation offsets"))?,
                )?;
                offsets.len().saturating_sub(1)
            }
            None => {
                return Err(PyValueError::new_err(
                    "dense compilation produced neither root inputs nor relations",
                ));
            }
        },
    };

    // Only collect inputs the caller actually supplied. `bind_batch` will
    // default any declared-but-missing optional inputs through their
    // `input_or_else` defaults, and error on hard-required missing inputs.
    let mut root_inputs = HashMap::new();
    for name in compiled.root_inputs() {
        if let Some(value) = inputs.get_item(name)? {
            root_inputs.insert(name.clone(), dense_column_from_python(&value)?);
        }
    }

    let relation_batches = relations.unwrap_or_else(|| PyDict::new(inputs.py()));
    let mut bound_relations = HashMap::new();
    for schema in compiled.relations() {
        let key = relation_key(&schema.key);
        let value = relation_batches.get_item(&key)?.ok_or_else(|| {
            PyValueError::new_err(format!("missing dense relation batch `{key}`"))
        })?;
        let relation_dict = value.cast::<PyDict>()?;
        let offsets = extract_index_vec(
            &relation_dict
                .get_item("offsets")?
                .ok_or_else(|| PyValueError::new_err("missing dense relation offsets"))?,
        )?;
        let raw_inputs = relation_dict
            .get_item("inputs")?
            .ok_or_else(|| PyValueError::new_err("missing dense relation inputs"))?;
        let input_dict = raw_inputs.cast::<PyDict>()?;
        let mut related_inputs = HashMap::new();
        for input_name in &schema.related_inputs {
            let column = input_dict.get_item(input_name)?.ok_or_else(|| {
                PyValueError::new_err(format!(
                    "missing dense relation input `{input_name}` for `{key}`"
                ))
            })?;
            related_inputs.insert(input_name.clone(), dense_column_from_python(&column)?);
        }
        bound_relations.insert(
            schema.key.clone(),
            DenseRelationBatchSpec {
                offsets,
                inputs: related_inputs,
            },
        );
    }

    Ok(DenseBatchSpec {
        row_count,
        inputs: root_inputs,
        relations: bound_relations,
    })
}

fn dense_column_from_python(value: &Bound<'_, PyAny>) -> PyResult<DenseColumn> {
    if let Ok(array) = value.extract::<PyReadonlyArray1<'_, bool>>() {
        return Ok(DenseColumn::Bool(array.as_slice()?.to_vec()));
    }
    if let Ok(array) = value.extract::<PyReadonlyArray1<'_, i64>>() {
        return Ok(DenseColumn::Integer(array.as_slice()?.to_vec()));
    }
    if let Ok(values) = value.extract::<Vec<bool>>() {
        return Ok(DenseColumn::Bool(values));
    }
    if let Ok(values) = value.extract::<Vec<i64>>() {
        return Ok(DenseColumn::Integer(values));
    }
    if let Ok(values) = value.extract::<Vec<String>>() {
        return Ok(DenseColumn::Text(values));
    }
    Err(PyValueError::new_err(
        "dense columns must be bool/int64 numpy arrays or simple Python lists",
    ))
}

fn extract_index_vec(value: &Bound<'_, PyAny>) -> PyResult<Vec<usize>> {
    if let Ok(array) = value.extract::<PyReadonlyArray1<'_, i64>>() {
        return array
            .as_slice()?
            .iter()
            .map(|item| {
                usize::try_from(*item).map_err(|_| {
                    PyValueError::new_err("dense relation offsets must be non-negative")
                })
            })
            .collect();
    }
    if let Ok(values) = value.extract::<Vec<i64>>() {
        return values
            .into_iter()
            .map(|item| {
                usize::try_from(item).map_err(|_| {
                    PyValueError::new_err("dense relation offsets must be non-negative")
                })
            })
            .collect();
    }
    Err(PyValueError::new_err(
        "dense relation offsets must be an int64 numpy array or Python integer list",
    ))
}

fn parse_date(value: &str) -> PyResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|error| PyValueError::new_err(format!("invalid date `{value}`: {error}")))
}

fn parse_period_kind(value: &str) -> PeriodKind {
    match value {
        "month" => PeriodKind::Month,
        "benefit_week" => PeriodKind::BenefitWeek,
        "tax_year" => PeriodKind::TaxYear,
        other => PeriodKind::Custom(other.to_string()),
    }
}

fn relation_key(key: &DenseRelationKey) -> String {
    format!("{}:{}:{}", key.name, key.current_slot, key.related_slot)
}

fn judgment_code(outcome: &JudgmentOutcome) -> i8 {
    match outcome {
        JudgmentOutcome::Holds => 1,
        JudgmentOutcome::NotHolds => -1,
        JudgmentOutcome::Undetermined => 0,
    }
}
