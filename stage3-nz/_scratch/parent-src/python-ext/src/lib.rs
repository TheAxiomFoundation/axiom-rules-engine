use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
    DenseRelationSchema,
};
use axiom_rules_engine::model::{JudgmentOutcome, Period, PeriodKind};
use axiom_rules_engine::spec::DTypeSpec;
use chrono::NaiveDate;
use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use rust_decimal::Decimal;
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

/// Authoring-level metadata for one derived rule: what the RuleSpec module
/// declared, before lowering to the runtime model (which drops `period`).
/// Adapters (e.g. populace's RulesEngine adapter) resolve variables through
/// this instead of re-parsing YAML.
#[pyclass(module = "axiom_rules_engine_dense")]
#[derive(Clone)]
struct DerivedMetadataHandle {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    id: Option<String>,
    #[pyo3(get)]
    entity: String,
    #[pyo3(get)]
    dtype: String,
    #[pyo3(get)]
    unit: Option<String>,
    #[pyo3(get)]
    period: Option<String>,
    #[pyo3(get)]
    source: Option<String>,
}

fn dtype_name(dtype: &DTypeSpec) -> &'static str {
    match dtype {
        DTypeSpec::Judgment => "judgment",
        DTypeSpec::Bool => "bool",
        DTypeSpec::Integer => "integer",
        DTypeSpec::Decimal => "decimal",
        DTypeSpec::Text => "text",
        DTypeSpec::Date => "date",
    }
}

#[pyclass(module = "axiom_rules_engine_dense", name = "CompiledDenseProgram")]
struct CompiledDenseProgramHandle {
    compiled: DenseCompiledProgram,
    derived_metadata: Vec<DerivedMetadataHandle>,
    input_catalog: HashMap<String, String>,
    input_request_names: HashMap<String, Vec<String>>,
}

#[pymethods]
impl CompiledDenseProgramHandle {
    #[staticmethod]
    #[pyo3(signature = (path, rulespec_roots, entity=None))]
    fn from_file(path: &str, rulespec_roots: Vec<String>, entity: Option<&str>) -> PyResult<Self> {
        let roots = axiom_rules_engine::rulespec::CanonicalRuleSpecRoots::new(&rulespec_roots)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let artifact = CompiledProgramArtifact::from_rulespec_file(path, &roots)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        compiled_dense_from_artifact(artifact, entity)
    }

    #[staticmethod]
    #[pyo3(signature = (path, rulespec_roots, entity=None))]
    fn from_composed_file(
        path: &str,
        rulespec_roots: Vec<String>,
        entity: Option<&str>,
    ) -> PyResult<Self> {
        let roots = axiom_rules_engine::rulespec::CanonicalRuleSpecRoots::new(&rulespec_roots)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let artifact = CompiledProgramArtifact::from_composed_rulespec_file(path, &roots)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        compiled_dense_from_artifact(artifact, entity)
    }

    /// Metadata for every derived rule in the compiled module, across all
    /// entities (not just the dense root). Order follows the module.
    fn derived_metadata(&self) -> Vec<DerivedMetadataHandle> {
        self.derived_metadata.clone()
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

    fn input_catalog(&self) -> HashMap<String, String> {
        self.input_catalog.clone()
    }

    fn input_request_names(&self) -> HashMap<String, Vec<String>> {
        self.input_request_names.clone()
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
        self.run(
            py,
            period_kind,
            start,
            end,
            inputs,
            relations,
            outputs,
            false,
        )
    }

    /// Execute in `f64` arithmetic: numeric outputs skip the Decimal
    /// round-trip entirely. Intended for microsimulation-style batch
    /// workloads; exact legal determinations should use `execute`.
    #[pyo3(signature = (period_kind, start, end, inputs, relations=None, outputs=None))]
    fn execute_f64<'py>(
        &self,
        py: Python<'py>,
        period_kind: &str,
        start: &str,
        end: &str,
        inputs: Bound<'py, PyDict>,
        relations: Option<Bound<'py, PyDict>>,
        outputs: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.run(
            py,
            period_kind,
            start,
            end,
            inputs,
            relations,
            outputs,
            true,
        )
    }
    /// Execute over an entity's lifetime in `f64` arithmetic: one positionally
    /// aligned input batch per period, so formulas can reduce over the period
    /// axis with `sum_over_periods` / `max_over_periods` / `count_over_periods`
    /// / `sum_top_n_over_periods`.
    ///
    /// `periods` is a list of `(period_kind, start, end)` triples and `batches`
    /// a same-length list of `(inputs[, relations])` — each `inputs` a dict of
    /// column arrays and each optional `relations` the dict shape `execute`
    /// accepts. Row `i` must be the same entity in every batch (identical row
    /// order and count). Every requested output's formula must contain an
    /// over-periods reduction; period-specific outputs should use `execute_f64`.
    #[pyo3(signature = (periods, batches, outputs=None))]
    fn execute_lifetime_f64<'py>(
        &self,
        py: Python<'py>,
        periods: Vec<(String, String, String)>,
        batches: Vec<Bound<'py, PyAny>>,
        outputs: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        if periods.len() != batches.len() {
            return Err(PyValueError::new_err(format!(
                "lifetime execution needs one batch per period: got {} periods and {} batches",
                periods.len(),
                batches.len()
            )));
        }

        let parsed_periods = periods
            .iter()
            .map(|(period_kind, start, end)| {
                Ok(Period {
                    kind: parse_period_kind(period_kind),
                    start: parse_date(start)?,
                    end: parse_date(end)?,
                })
            })
            .collect::<PyResult<Vec<Period>>>()?;

        let built_batches = batches
            .into_iter()
            .map(|batch| {
                let (inputs, relations) = split_lifetime_batch(&batch)?;
                build_batch(&self.compiled, inputs, relations)
            })
            .collect::<PyResult<Vec<DenseBatchSpec>>>()?;

        let output_names = outputs.unwrap_or_else(|| self.compiled.output_names());
        let execution = self
            .compiled
            .execute_lifetime_f64(&parsed_periods, built_batches, &output_names)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        execution_to_pydict(py, execution)
    }
}

fn compiled_dense_from_artifact(
    artifact: CompiledProgramArtifact,
    entity: Option<&str>,
) -> PyResult<CompiledDenseProgramHandle> {
    // Capture authoring metadata (including `period`, which the runtime model
    // drops) for every derived rule across all entities — the dense program
    // itself compiles only the root entity's rules.
    let derived_metadata = artifact
        .program
        .derived
        .iter()
        .map(|spec| DerivedMetadataHandle {
            name: spec.name.clone(),
            id: spec.id.clone(),
            entity: spec.entity.clone(),
            dtype: dtype_name(&spec.dtype).to_string(),
            unit: spec.unit.clone(),
            period: spec.period.clone(),
            source: spec.source.clone(),
        })
        .collect();
    let input_catalog = artifact
        .metadata
        .input_catalog
        .iter()
        .map(|entry| (entry.slot.clone(), entry.canonical_request_name.clone()))
        .collect();
    let input_request_names = artifact
        .metadata
        .input_catalog
        .iter()
        .map(|entry| (entry.slot.clone(), entry.request_names.clone()))
        .collect();
    let compiled = DenseCompiledProgram::from_artifact(&artifact, entity)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(CompiledDenseProgramHandle {
        compiled,
        derived_metadata,
        input_catalog,
        input_request_names,
    })
}

impl CompiledDenseProgramHandle {
    #[allow(clippy::too_many_arguments)]
    fn run<'py>(
        &self,
        py: Python<'py>,
        period_kind: &str,
        start: &str,
        end: &str,
        inputs: Bound<'py, PyDict>,
        relations: Option<Bound<'py, PyDict>>,
        outputs: Option<Vec<String>>,
        use_f64: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let period = Period {
            kind: parse_period_kind(period_kind),
            start: parse_date(start)?,
            end: parse_date(end)?,
        };
        let batch = build_batch(&self.compiled, inputs, relations)?;
        let output_names = outputs.unwrap_or_else(|| self.compiled.output_names());
        let execution = if use_f64 {
            self.compiled.execute_f64(&period, batch, &output_names)
        } else {
            self.compiled.execute(&period, batch, &output_names)
        }
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;

        execution_to_pydict(py, execution)
    }
}

/// Materialise a dense execution result into the `{row_count, outputs}` dict the
/// Python surface returns. Shared by the per-period and lifetime entry points.
fn execution_to_pydict(
    py: Python<'_>,
    execution: axiom_rules_engine::dense::DenseExecutionResult,
) -> PyResult<Bound<'_, PyDict>> {
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
                DenseColumn::Float(values) => {
                    output_dict.set_item(name, PyArray1::from_vec(py, values))?;
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

/// Split one lifetime batch entry into its `(inputs, relations)` parts. A batch
/// is either a bare inputs dict, or a `(inputs[, relations])` tuple/list — the
/// same relation dict shape `build_batch` already consumes.
fn split_lifetime_batch<'py>(
    batch: &Bound<'py, PyAny>,
) -> PyResult<(Bound<'py, PyDict>, Option<Bound<'py, PyDict>>)> {
    if let Ok(inputs) = batch.cast::<PyDict>() {
        return Ok((inputs.clone(), None));
    }
    // Otherwise expect a sequence: (inputs,) or (inputs, relations).
    let items = batch.try_iter()?.collect::<PyResult<Vec<_>>>()?;
    if items.is_empty() || items.len() > 2 {
        return Err(PyValueError::new_err(
            "each lifetime batch must be an inputs dict or an (inputs[, relations]) tuple",
        ));
    }
    let inputs = items[0].cast::<PyDict>()?.clone();
    let relations = match items.get(1) {
        Some(value) if !value.is_none() => Some(value.cast::<PyDict>()?.clone()),
        _ => None,
    };
    Ok((inputs, relations))
}

#[pymodule]
fn axiom_rules_engine_dense(_py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CompiledDenseProgramHandle>()?;
    module.add_class::<RelationSchemaHandle>()?;
    module.add_class::<DerivedMetadataHandle>()?;
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
    if let Ok(array) = value.extract::<PyReadonlyArray1<'_, f64>>() {
        return decimal_column_from_f64(array.as_slice()?);
    }
    if let Ok(values) = value.extract::<Vec<bool>>() {
        return Ok(DenseColumn::Bool(values));
    }
    if let Ok(values) = value.extract::<Vec<i64>>() {
        return Ok(DenseColumn::Integer(values));
    }
    if let Ok(values) = value.extract::<Vec<f64>>() {
        return decimal_column_from_f64(&values);
    }
    if let Ok(values) = value.extract::<Vec<String>>() {
        return Ok(DenseColumn::Text(values));
    }
    Err(PyValueError::new_err(
        "dense columns must be bool/int64/float64 numpy arrays or simple Python lists",
    ))
}

fn decimal_column_from_f64(values: &[f64]) -> PyResult<DenseColumn> {
    values
        .iter()
        .map(|value| {
            Decimal::from_f64_retain(*value)
                .map(|decimal| decimal.round_dp(9).normalize())
                .ok_or_else(|| {
                    PyValueError::new_err(format!("cannot represent {value} as a decimal"))
                })
        })
        .collect::<PyResult<Vec<Decimal>>>()
        .map(DenseColumn::Decimal)
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
