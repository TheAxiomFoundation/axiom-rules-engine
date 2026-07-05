use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, DerivedVersion, IndexedParameter,
    InputRecord, Interval, JudgmentExpr, JudgmentOutcome, OverPeriodsKind, ParameterVersion,
    Period, PeriodKind, Program, RelatedValueRef, RelationDerivation, RelationRecord,
    RelationSchema, Rounding, RoundingMode, ScalarExpr, ScalarValue, UnitDef, UnitKind,
};

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("invalid decimal literal `{literal}`")]
    InvalidDecimal { literal: String },
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[cfg(feature = "fs")]
    #[error("failed to read program file `{path}`: {error}")]
    ReadFile { path: String, error: std::io::Error },
    #[error("duplicate {kind} `{name}` when merging extended program")]
    DuplicateOnMerge { kind: String, name: String },
    #[error(
        "dataset input `{reference}` must use an absolute legal RuleSpec reference that resolves to an input slot, derived rule, or parameter in the compiled program"
    )]
    InvalidDatasetInputReference { reference: String },
    #[error(
        "dataset relation `{reference}` must use an absolute legal RuleSpec reference that resolves to a declared relation in the compiled program"
    )]
    InvalidDatasetRelationReference { reference: String },
    #[error(
        "derived rule `{derived}` declares `rounding: {mode}` but its unit {unit} is not a declared currency unit; output rounding only applies to Currency units (with minor_units)"
    )]
    RoundingOnNonCurrencyUnit {
        derived: String,
        mode: String,
        unit: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProgramSpec {
    #[serde(default)]
    pub extends: Option<String>,
    /// Module-level metadata (source pinning, encoding provenance,
    /// validation status) carried through RuleSpec lowering for tooling and
    /// artifact pass-through. Compilation and execution never read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<crate::rulespec::ModuleMetadata>,
    #[serde(default)]
    pub units: Vec<UnitSpec>,
    #[serde(default)]
    pub relations: Vec<RelationSpec>,
    #[serde(default)]
    pub parameters: Vec<IndexedParameterSpec>,
    #[serde(default)]
    pub derived: Vec<DerivedSpec>,
}

impl ProgramSpec {
    pub fn from_yaml_str(source: &str) -> Result<Self, SpecError> {
        Ok(serde_yaml::from_str(source)?)
    }

    /// Validate every declared `rounding:` against its rule's unit, without
    /// building the full runtime [`Program`]. A rule that declares rounding on a
    /// non-currency (or undeclared) unit is rejected. Called at compile time
    /// (`CompiledProgramArtifact::compile`) so a malformed artifact never ships;
    /// `to_program` performs the same check while resolving `minor_units`, so
    /// the execution path is guarded too.
    pub fn validate_rounding(&self) -> Result<(), SpecError> {
        // A lightweight units view; the full `to_program` also does this, but
        // this stays cheap and independent of relation/derived-graph checks.
        let program = Program {
            units: self
                .units
                .iter()
                .map(|unit| {
                    let unit = unit.to_model();
                    (unit.name.clone(), unit)
                })
                .collect(),
            ..Program::default()
        };
        for derived in &self.derived {
            derived.resolve_rounding(&program)?;
        }
        Ok(())
    }

    /// Load a program from `path`, resolving any `extends: <other.yaml>`
    /// relative to the current file's directory. Conflicting parameter names
    /// have their versions concatenated, preserving effective_from order; the
    /// engine picks whichever version is live for the query period. Units,
    /// relations, and derived outputs are additive with duplicate-name errors.
    #[cfg(feature = "fs")]
    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Self, SpecError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|error| SpecError::ReadFile {
            path: path.display().to_string(),
            error,
        })?;
        let mut spec: Self = serde_yaml::from_str(&source)?;
        if let Some(extends) = spec.extends.take() {
            let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(""));
            let base_path = base_dir.join(&extends);
            let base = Self::from_yaml_file(&base_path)?;
            spec = merge_programs(base, spec)?;
        }
        Ok(spec)
    }

    pub fn to_program(&self) -> Result<Program, SpecError> {
        let mut program = Program::default();

        for unit in &self.units {
            program.add_unit(unit.to_model());
        }

        for relation in &self.relations {
            program.add_relation_schema(relation.to_model()?);
        }

        for parameter in &self.parameters {
            program.add_parameter(parameter.to_model()?);
        }

        // Units are already in `program`, so a derived rule's `rounding:` mode
        // can be resolved against its currency unit here. A rule that declares
        // rounding on a non-currency (or undeclared) unit is rejected — this is
        // the compile-time validation the contract requires, enforced on every
        // `to_program` so a malformed artifact cannot execute either.
        for derived in &self.derived {
            let mut model = derived.to_model()?;
            model.rounding = derived.resolve_rounding(&program)?;
            program.add_derived(model);
        }

        Ok(program)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DatasetSpec {
    #[serde(default)]
    pub inputs: Vec<InputRecordSpec>,
    #[serde(default)]
    pub relations: Vec<RelationRecordSpec>,
}

impl DatasetSpec {
    pub fn to_dataset(&self) -> Result<DataSet, SpecError> {
        Ok(DataSet {
            inputs: self
                .inputs
                .iter()
                .map(InputRecordSpec::to_model)
                .collect::<Result<Vec<InputRecord>, SpecError>>()?,
            relations: self
                .relations
                .iter()
                .map(RelationRecordSpec::to_model)
                .collect::<Result<Vec<RelationRecord>, SpecError>>()?,
        })
    }

    pub fn to_dataset_for_program(&self, program: &Program) -> Result<DataSet, SpecError> {
        let input_slots = program.input_slots();
        let inputs = self
            .inputs
            .iter()
            .map(|input| input.to_model_for_program(program, &input_slots))
            .collect::<Result<Vec<InputRecord>, SpecError>>()?;
        let relations = self
            .relations
            .iter()
            .map(|relation| relation.to_model_for_program(program))
            .collect::<Result<Vec<RelationRecord>, SpecError>>()?;
        Ok(DataSet { inputs, relations })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UnitSpec {
    pub name: String,
    #[serde(flatten)]
    pub kind: UnitKindSpec,
}

impl UnitSpec {
    fn to_model(&self) -> UnitDef {
        UnitDef {
            name: self.name.clone(),
            kind: self.kind.to_model(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnitKindSpec {
    Currency { minor_units: u8 },
    Count,
    Ratio,
    Duration,
    Custom { label: String },
}

impl UnitKindSpec {
    fn to_model(&self) -> UnitKind {
        match self {
            Self::Currency { minor_units } => UnitKind::Currency {
                minor_units: *minor_units,
            },
            Self::Count => UnitKind::Count,
            Self::Ratio => UnitKind::Ratio,
            Self::Duration => UnitKind::Duration,
            Self::Custom { label } => UnitKind::Custom(label.clone()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RelationSpec {
    pub name: String,
    pub arity: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<RelationDerivationSpec>,
}

impl RelationSpec {
    fn to_model(&self) -> Result<RelationSchema, SpecError> {
        Ok(RelationSchema {
            name: self.name.clone(),
            arity: self.arity,
            derivation: self
                .derivation
                .as_ref()
                .map(RelationDerivationSpec::to_model)
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RelationDerivationSpec {
    pub source_relation: String,
    pub current_slot: usize,
    pub related_slot: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_relation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slot_entities: Vec<String>,
    pub predicate: JudgmentExprSpec,
}

impl RelationDerivationSpec {
    fn to_model(&self) -> Result<RelationDerivation, SpecError> {
        Ok(RelationDerivation {
            source_relation: self.source_relation.clone(),
            current_slot: self.current_slot,
            related_slot: self.related_slot,
            entity: self.entity.clone(),
            member_relation: self.member_relation.clone(),
            slot_entities: self.slot_entities.clone(),
            predicate: self.predicate.to_model()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IndexedParameterSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub unit: Option<String>,
    #[serde(default)]
    pub indexed_by: Option<String>,
    #[serde(default)]
    pub versions: Vec<ParameterVersionSpec>,
}

impl IndexedParameterSpec {
    fn to_model(&self) -> Result<IndexedParameter, SpecError> {
        Ok(IndexedParameter {
            id: self.id.clone(),
            name: self.name.clone(),
            unit: self.unit.clone(),
            indexed_by: self.indexed_by.clone(),
            versions: self
                .versions
                .iter()
                .map(ParameterVersionSpec::to_model)
                .collect::<Result<Vec<ParameterVersion>, SpecError>>()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParameterVersionSpec {
    pub effective_from: NaiveDate,
    pub values: BTreeMap<i64, ScalarValueSpec>,
}

impl ParameterVersionSpec {
    fn to_model(&self) -> Result<ParameterVersion, SpecError> {
        let mut values = BTreeMap::new();
        for (key, value) in &self.values {
            values.insert(*key, value.to_model()?);
        }
        Ok(ParameterVersion {
            effective_from: self.effective_from,
            values,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DerivedSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub entity: String,
    pub dtype: DTypeSpec,
    pub unit: Option<String>,
    // Time granularity of the calculation (Year / Month / Day / Instant).
    // Parsed for RuleSpec authoring and round-trip serialisation; the
    // engine treats the query period as authoritative at runtime.
    #[serde(default)]
    pub period: Option<String>,
    /// Opt-in output-rounding mode. When present AND `unit` is a declared
    /// currency, the rule's output is rounded to the unit's `minor_units` under
    /// this mode in every execution path. Absent means today's behavior (no
    /// rounding). Declaring it on a non-currency unit is a compile error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounding: Option<RoundingModeSpec>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(flatten)]
    pub semantics: DerivedSemanticsSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<DerivedVersionSpec>,
}

impl DerivedSpec {
    fn to_model(&self) -> Result<Derived, SpecError> {
        Ok(Derived {
            id: self.id.clone(),
            name: self.name.clone(),
            entity: self.entity.clone(),
            dtype: self.dtype.to_model(),
            unit: self.unit.clone(),
            // Resolved separately in `to_program`, where the units map is
            // available to read `minor_units` and to validate.
            rounding: None,
            source: self.source.clone(),
            source_url: self.source_url.clone(),
            semantics: self.semantics.to_model()?,
            versions: self
                .versions
                .iter()
                .map(DerivedVersionSpec::to_model)
                .collect::<Result<Vec<DerivedVersion>, SpecError>>()?,
        })
    }

    /// Resolve this rule's declared `rounding:` mode into a concrete
    /// [`Rounding`] (mode + the unit's `minor_units`), or `None` when no
    /// rounding is declared. Errors if rounding is declared but the rule's
    /// `unit` is not a declared currency — the contract's compile-time check.
    fn resolve_rounding(&self, program: &Program) -> Result<Option<Rounding>, SpecError> {
        let Some(mode) = self.rounding else {
            return Ok(None);
        };
        let mode = mode.to_model();
        let minor_units = self
            .unit
            .as_deref()
            .and_then(|unit| program.currency_minor_units(unit));
        match minor_units {
            Some(minor_units) => Ok(Some(Rounding { mode, minor_units })),
            None => Err(SpecError::RoundingOnNonCurrencyUnit {
                derived: self.name.clone(),
                mode: mode.as_str().to_string(),
                unit: match self.unit.as_deref() {
                    Some(unit) => format!("`{unit}`"),
                    None => "(none declared)".to_string(),
                },
            }),
        }
    }
}

/// The RuleSpec `rounding:` vocabulary, mirroring [`crate::model::RoundingMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoundingModeSpec {
    HalfUp,
    HalfEven,
    Floor,
    Ceil,
}

impl RoundingModeSpec {
    fn to_model(self) -> RoundingMode {
        match self {
            Self::HalfUp => RoundingMode::HalfUp,
            Self::HalfEven => RoundingMode::HalfEven,
            Self::Floor => RoundingMode::Floor,
            Self::Ceil => RoundingMode::Ceil,
        }
    }

    pub fn from_model(mode: RoundingMode) -> Self {
        match mode {
            RoundingMode::HalfUp => Self::HalfUp,
            RoundingMode::HalfEven => Self::HalfEven,
            RoundingMode::Floor => Self::Floor,
            RoundingMode::Ceil => Self::Ceil,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DerivedVersionSpec {
    pub effective_from: NaiveDate,
    #[serde(flatten)]
    pub semantics: DerivedSemanticsSpec,
}

impl DerivedVersionSpec {
    fn to_model(&self) -> Result<DerivedVersion, SpecError> {
        Ok(DerivedVersion {
            effective_from: self.effective_from,
            semantics: self.semantics.to_model()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// `schemars` derives only the canonical snake_case names and drops the serde
// aliases below, which would make the schema reject a `dtype` serde accepts.
// Override with the full accepted set (canonical + aliases).
#[cfg_attr(
    feature = "schema",
    schemars(schema_with = "crate::schema::dtype_schema")
)]
#[serde(rename_all = "snake_case")]
pub enum DTypeSpec {
    // Accept RuleSpec's PascalCase vocabulary alongside our snake_case. `Money`
    // and `Rate` both map to Decimal — the engine doesn't distinguish them
    // at runtime, but they preserve authoring intent from source documents.
    #[serde(alias = "Judgment")]
    Judgment,
    #[serde(alias = "Bool", alias = "Boolean", alias = "boolean")]
    Bool,
    #[serde(alias = "Integer")]
    Integer,
    #[serde(
        alias = "Decimal",
        alias = "Money",
        alias = "money",
        alias = "Rate",
        alias = "rate"
    )]
    Decimal,
    #[serde(alias = "Text")]
    Text,
    #[serde(alias = "Date")]
    Date,
}

impl DTypeSpec {
    pub fn from_model(dtype: &DType) -> Self {
        match dtype {
            DType::Judgment => Self::Judgment,
            DType::Bool => Self::Bool,
            DType::Integer => Self::Integer,
            DType::Decimal => Self::Decimal,
            DType::Text => Self::Text,
            DType::Date => Self::Date,
        }
    }

    fn to_model(&self) -> DType {
        match self {
            Self::Judgment => DType::Judgment,
            Self::Bool => DType::Bool,
            Self::Integer => DType::Integer,
            Self::Decimal => DType::Decimal,
            Self::Text => DType::Text,
            Self::Date => DType::Date,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "semantics", rename_all = "snake_case")]
pub enum DerivedSemanticsSpec {
    Scalar { expr: ScalarExprSpec },
    Judgment { expr: JudgmentExprSpec },
}

impl DerivedSemanticsSpec {
    fn to_model(&self) -> Result<DerivedSemantics, SpecError> {
        match self {
            Self::Scalar { expr } => Ok(DerivedSemantics::Scalar(expr.to_model()?)),
            Self::Judgment { expr } => Ok(DerivedSemantics::Judgment(expr.to_model()?)),
        }
    }
}

fn deserialise_decimal_as_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Accept either a quoted string (preserves arbitrary precision) or a
    // YAML integer literal (no precision loss; converted to its base-10
    // representation). YAML float literals are intentionally rejected
    // because f64 can't exactly represent most decimal fractions (£0.1,
    // for example, round-trips through f64 as 0.1000000000000000055…),
    // which would silently corrupt currency parameters.
    #[derive(Deserialize)]
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    #[serde(untagged)]
    enum DecimalInput {
        Str(String),
        Int(i64),
    }
    match DecimalInput::deserialize(deserializer)? {
        DecimalInput::Str(s) => Ok(s),
        DecimalInput::Int(n) => Ok(n.to_string()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScalarValueSpec {
    Bool {
        value: bool,
    },
    Integer {
        value: i64,
    },
    Decimal {
        // `deserialise_decimal_as_string` accepts a quoted string (arbitrary
        // precision) or a JSON/YAML integer, and rejects floats. The Rust
        // field is `String`, but the accepted JSON is string-or-integer, so
        // the schema says so rather than the misleading `string`.
        #[cfg_attr(
            feature = "schema",
            schemars(schema_with = "crate::schema::string_or_integer_schema")
        )]
        #[serde(deserialize_with = "deserialise_decimal_as_string")]
        value: String,
    },
    Text {
        value: String,
    },
    Date {
        value: NaiveDate,
    },
}

impl ScalarValueSpec {
    pub fn from_model(value: ScalarValue) -> Self {
        match value {
            ScalarValue::Bool(value) => Self::Bool { value },
            ScalarValue::Integer(value) => Self::Integer { value },
            ScalarValue::Decimal(value) => Self::Decimal {
                value: value.normalize().to_string(),
            },
            ScalarValue::Text(value) => Self::Text { value },
            ScalarValue::Date(value) => Self::Date { value },
        }
    }

    fn to_model(&self) -> Result<ScalarValue, SpecError> {
        match self {
            Self::Bool { value } => Ok(ScalarValue::Bool(*value)),
            Self::Integer { value } => Ok(ScalarValue::Integer(*value)),
            Self::Decimal { value } => Ok(ScalarValue::Decimal(Decimal::from_str(value).map_err(
                |_| SpecError::InvalidDecimal {
                    literal: value.clone(),
                },
            )?)),
            Self::Text { value } => Ok(ScalarValue::Text(value.clone())),
            Self::Date { value } => Ok(ScalarValue::Date(*value)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScalarExprSpec {
    Literal {
        value: ScalarValueSpec,
    },
    Input {
        name: String,
    },
    InputOrElse {
        name: String,
        default: ScalarValueSpec,
    },
    Derived {
        name: String,
    },
    ParameterLookup {
        parameter: String,
        index: Box<ScalarExprSpec>,
    },
    Add {
        items: Vec<ScalarExprSpec>,
    },
    Sub {
        left: Box<ScalarExprSpec>,
        right: Box<ScalarExprSpec>,
    },
    Mul {
        left: Box<ScalarExprSpec>,
        right: Box<ScalarExprSpec>,
    },
    Div {
        left: Box<ScalarExprSpec>,
        right: Box<ScalarExprSpec>,
    },
    Max {
        items: Vec<ScalarExprSpec>,
    },
    Min {
        items: Vec<ScalarExprSpec>,
    },
    Ceil {
        value: Box<ScalarExprSpec>,
    },
    Floor {
        value: Box<ScalarExprSpec>,
    },
    PeriodStart,
    PeriodEnd,
    DateAddDays {
        date: Box<ScalarExprSpec>,
        days: Box<ScalarExprSpec>,
    },
    DaysBetween {
        from: Box<ScalarExprSpec>,
        to: Box<ScalarExprSpec>,
    },
    CountRelated {
        relation: String,
        current_slot: usize,
        related_slot: usize,
        #[serde(default, rename = "where")]
        where_clause: Option<Box<JudgmentExprSpec>>,
    },
    SumRelated {
        relation: String,
        current_slot: usize,
        related_slot: usize,
        value: RelatedValueRefSpec,
        #[serde(default, rename = "where")]
        where_clause: Option<Box<JudgmentExprSpec>>,
    },
    If {
        condition: Box<JudgmentExprSpec>,
        then_expr: Box<ScalarExprSpec>,
        else_expr: Box<ScalarExprSpec>,
    },
    /// Reduction over an entity's own period axis (lifetime execution only).
    /// `n` is present only for the `sum_top_n` reduction. Expression-level,
    /// additive to the serialized surface — no artifact-format-version bump.
    OverPeriods {
        over: OverPeriodsKindSpec,
        value: Box<ScalarExprSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<Box<ScalarExprSpec>>,
    },
}

/// The over-periods reduction vocabulary, mirroring
/// [`crate::model::OverPeriodsKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OverPeriodsKindSpec {
    Sum,
    Max,
    Count,
    SumTopN,
}

impl OverPeriodsKindSpec {
    fn to_model(self) -> OverPeriodsKind {
        match self {
            Self::Sum => OverPeriodsKind::Sum,
            Self::Max => OverPeriodsKind::Max,
            Self::Count => OverPeriodsKind::Count,
            Self::SumTopN => OverPeriodsKind::SumTopN,
        }
    }

    pub fn from_model(kind: OverPeriodsKind) -> Self {
        match kind {
            OverPeriodsKind::Sum => Self::Sum,
            OverPeriodsKind::Max => Self::Max,
            OverPeriodsKind::Count => Self::Count,
            OverPeriodsKind::SumTopN => Self::SumTopN,
        }
    }
}

impl ScalarExprSpec {
    fn to_model(&self) -> Result<ScalarExpr, SpecError> {
        match self {
            Self::Literal { value } => Ok(ScalarExpr::Literal(value.to_model()?)),
            Self::Input { name } => Ok(ScalarExpr::Input(name.clone())),
            Self::InputOrElse { name, default } => Ok(ScalarExpr::InputOrElse {
                name: name.clone(),
                default: default.to_model()?,
            }),
            Self::Derived { name } => Ok(ScalarExpr::Derived(name.clone())),
            Self::ParameterLookup { parameter, index } => Ok(ScalarExpr::ParameterLookup {
                parameter: parameter.clone(),
                index: Box::new(index.to_model()?),
            }),
            Self::Add { items } => Ok(ScalarExpr::Add(
                items
                    .iter()
                    .map(ScalarExprSpec::to_model)
                    .collect::<Result<Vec<ScalarExpr>, SpecError>>()?,
            )),
            Self::Sub { left, right } => Ok(ScalarExpr::Sub(
                Box::new(left.to_model()?),
                Box::new(right.to_model()?),
            )),
            Self::Mul { left, right } => Ok(ScalarExpr::Mul(
                Box::new(left.to_model()?),
                Box::new(right.to_model()?),
            )),
            Self::Div { left, right } => Ok(ScalarExpr::Div(
                Box::new(left.to_model()?),
                Box::new(right.to_model()?),
            )),
            Self::Max { items } => Ok(ScalarExpr::Max(
                items
                    .iter()
                    .map(ScalarExprSpec::to_model)
                    .collect::<Result<Vec<ScalarExpr>, SpecError>>()?,
            )),
            Self::Min { items } => Ok(ScalarExpr::Min(
                items
                    .iter()
                    .map(ScalarExprSpec::to_model)
                    .collect::<Result<Vec<ScalarExpr>, SpecError>>()?,
            )),
            Self::Ceil { value } => Ok(ScalarExpr::Ceil(Box::new(value.to_model()?))),
            Self::Floor { value } => Ok(ScalarExpr::Floor(Box::new(value.to_model()?))),
            Self::PeriodStart => Ok(ScalarExpr::PeriodStart),
            Self::PeriodEnd => Ok(ScalarExpr::PeriodEnd),
            Self::DateAddDays { date, days } => Ok(ScalarExpr::DateAddDays {
                date: Box::new(date.to_model()?),
                days: Box::new(days.to_model()?),
            }),
            Self::DaysBetween { from, to } => Ok(ScalarExpr::DaysBetween {
                from: Box::new(from.to_model()?),
                to: Box::new(to.to_model()?),
            }),
            Self::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => Ok(ScalarExpr::CountRelated {
                relation: relation.clone(),
                current_slot: *current_slot,
                related_slot: *related_slot,
                where_clause: where_clause
                    .as_ref()
                    .map(|inner| inner.to_model().map(Box::new))
                    .transpose()?,
            }),
            Self::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => Ok(ScalarExpr::SumRelated {
                relation: relation.clone(),
                current_slot: *current_slot,
                related_slot: *related_slot,
                value: value.to_model(),
                where_clause: where_clause
                    .as_ref()
                    .map(|inner| inner.to_model().map(Box::new))
                    .transpose()?,
            }),
            Self::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(ScalarExpr::If {
                condition: Box::new(condition.to_model()?),
                then_expr: Box::new(then_expr.to_model()?),
                else_expr: Box::new(else_expr.to_model()?),
            }),
            Self::OverPeriods { over, value, n } => Ok(ScalarExpr::OverPeriods {
                kind: over.to_model(),
                value: Box::new(value.to_model()?),
                n: n.as_ref()
                    .map(|inner| inner.to_model().map(Box::new))
                    .transpose()?,
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RelatedValueRefSpec {
    Input { name: String },
    Derived { name: String },
}

impl RelatedValueRefSpec {
    fn to_model(&self) -> RelatedValueRef {
        match self {
            Self::Input { name } => RelatedValueRef::Input(name.clone()),
            Self::Derived { name } => RelatedValueRef::Derived(name.clone()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JudgmentExprSpec {
    Comparison {
        left: Box<ScalarExprSpec>,
        op: ComparisonOpSpec,
        right: Box<ScalarExprSpec>,
    },
    Derived {
        name: String,
    },
    RelationMember {
        relation: String,
        current_slot: usize,
        related_slot: usize,
    },
    And {
        items: Vec<JudgmentExprSpec>,
    },
    Or {
        items: Vec<JudgmentExprSpec>,
    },
    Not {
        item: Box<JudgmentExprSpec>,
    },
}

impl JudgmentExprSpec {
    fn to_model(&self) -> Result<JudgmentExpr, SpecError> {
        match self {
            Self::Comparison { left, op, right } => Ok(JudgmentExpr::Comparison {
                left: left.to_model()?,
                op: op.to_model(),
                right: right.to_model()?,
            }),
            Self::Derived { name } => Ok(JudgmentExpr::Derived(name.clone())),
            Self::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => Ok(JudgmentExpr::RelationMember {
                relation: relation.clone(),
                current_slot: *current_slot,
                related_slot: *related_slot,
            }),
            Self::And { items } => Ok(JudgmentExpr::And(
                items
                    .iter()
                    .map(JudgmentExprSpec::to_model)
                    .collect::<Result<Vec<JudgmentExpr>, SpecError>>()?,
            )),
            Self::Or { items } => Ok(JudgmentExpr::Or(
                items
                    .iter()
                    .map(JudgmentExprSpec::to_model)
                    .collect::<Result<Vec<JudgmentExpr>, SpecError>>()?,
            )),
            Self::Not { item } => Ok(JudgmentExpr::Not(Box::new(item.to_model()?))),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOpSpec {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Ne,
}

impl ComparisonOpSpec {
    fn to_model(self) -> ComparisonOp {
        match self {
            Self::Lt => ComparisonOp::Lt,
            Self::Lte => ComparisonOp::Lte,
            Self::Gt => ComparisonOp::Gt,
            Self::Gte => ComparisonOp::Gte,
            Self::Eq => ComparisonOp::Eq,
            Self::Ne => ComparisonOp::Ne,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JudgmentOutcomeSpec {
    Holds,
    NotHolds,
    Undetermined,
}

impl From<JudgmentOutcome> for JudgmentOutcomeSpec {
    fn from(value: JudgmentOutcome) -> Self {
        match value {
            JudgmentOutcome::Holds => Self::Holds,
            JudgmentOutcome::NotHolds => Self::NotHolds,
            JudgmentOutcome::Undetermined => Self::Undetermined,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PeriodSpec {
    #[serde(flatten)]
    pub kind: PeriodKindSpec,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl PeriodSpec {
    pub fn to_model(&self) -> Result<Period, SpecError> {
        Ok(Period {
            kind: self.kind.to_model(),
            start: self.start,
            end: self.end,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "period_kind", rename_all = "snake_case")]
pub enum PeriodKindSpec {
    Month,
    BenefitWeek,
    TaxYear,
    Custom { name: String },
}

impl PeriodKindSpec {
    fn to_model(&self) -> PeriodKind {
        match self {
            Self::Month => PeriodKind::Month,
            Self::BenefitWeek => PeriodKind::BenefitWeek,
            Self::TaxYear => PeriodKind::TaxYear,
            Self::Custom { name } => PeriodKind::Custom(name.clone()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IntervalSpec {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl IntervalSpec {
    fn to_model(&self) -> Interval {
        Interval {
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InputRecordSpec {
    pub name: String,
    pub entity: String,
    pub entity_id: String,
    pub interval: IntervalSpec,
    pub value: ScalarValueSpec,
}

impl InputRecordSpec {
    fn to_model(&self) -> Result<InputRecord, SpecError> {
        Ok(InputRecord {
            name: self.name.clone(),
            entity: self.entity.clone(),
            entity_id: self.entity_id.clone(),
            interval: self.interval.to_model(),
            value: self.value.to_model()?,
        })
    }

    fn to_model_for_program(
        &self,
        program: &Program,
        input_slots: &std::collections::HashSet<&str>,
    ) -> Result<InputRecord, SpecError> {
        let name = program
            .resolve_input_name_with_slots(&self.name, input_slots)
            .ok_or_else(|| SpecError::InvalidDatasetInputReference {
                reference: self.name.clone(),
            })?;
        Ok(InputRecord {
            name,
            entity: self.entity.clone(),
            entity_id: self.entity_id.clone(),
            interval: self.interval.to_model(),
            value: self.value.to_model()?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RelationRecordSpec {
    pub name: String,
    pub tuple: Vec<String>,
    pub interval: IntervalSpec,
}

impl RelationRecordSpec {
    fn to_model(&self) -> Result<RelationRecord, SpecError> {
        Ok(RelationRecord {
            name: self.name.clone(),
            tuple: self.tuple.clone(),
            interval: self.interval.to_model(),
        })
    }

    fn to_model_for_program(&self, program: &Program) -> Result<RelationRecord, SpecError> {
        let name = program.resolve_relation_name(&self.name).ok_or_else(|| {
            SpecError::InvalidDatasetRelationReference {
                reference: self.name.clone(),
            }
        })?;
        Ok(RelationRecord {
            name,
            tuple: self.tuple.clone(),
            interval: self.interval.to_model(),
        })
    }
}

/// Merge an extending program into its base. Parameter versions are
/// concatenated by parameter name (the engine's effective_from ordering picks
/// the right version at evaluation). Units, relations, and derived outputs
/// are additive — duplicate names across base and extension raise
/// `SpecError::DuplicateOnMerge`.
pub fn merge_programs(
    mut base: ProgramSpec,
    extension: ProgramSpec,
) -> Result<ProgramSpec, SpecError> {
    if extension.module.is_some() {
        base.module = extension.module;
    }
    for unit in extension.units {
        if base.units.iter().any(|u| u.name == unit.name) {
            continue;
        }
        base.units.push(unit);
    }
    for relation in extension.relations {
        if base.relations.iter().any(|r| r.name == relation.name) {
            return Err(SpecError::DuplicateOnMerge {
                kind: "relation".to_string(),
                name: relation.name,
            });
        }
        base.relations.push(relation);
    }
    for parameter in extension.parameters {
        if let Some(existing) = base
            .parameters
            .iter_mut()
            .find(|p| p.name == parameter.name)
        {
            existing.versions.extend(parameter.versions);
        } else {
            base.parameters.push(parameter);
        }
    }
    for derived in extension.derived {
        if base.derived.iter().any(|d| d.name == derived.name) {
            return Err(SpecError::DuplicateOnMerge {
                kind: "derived".to_string(),
                name: derived.name,
            });
        }
        base.derived.push(derived);
    }
    Ok(base)
}
