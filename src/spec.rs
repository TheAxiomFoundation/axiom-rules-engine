use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, IndexedParameter, InputRecord,
    Interval, JudgmentExpr, JudgmentOutcome, ParameterVersion, Period, PeriodKind, Program,
    RelatedValueRef, RelationRecord, ScalarExpr, ScalarValue, UnitDef, UnitKind,
};

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("invalid decimal literal `{literal}`")]
    InvalidDecimal { literal: String },
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("failed to read programme `{path}`: {error}")]
    ReadFile { path: String, error: std::io::Error },
    #[error("duplicate {kind} `{name}` when merging extended programme")]
    DuplicateOnMerge { kind: String, name: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgramSpec {
    #[serde(default)]
    pub extends: Option<String>,
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

    /// Load a programme from `path`, resolving any `extends: <other.yaml>`
    /// relative to the current file's directory. Conflicting parameter names
    /// have their versions concatenated, preserving effective_from order; the
    /// engine picks whichever version is live for the query period. Units,
    /// relations, and derived outputs are additive with duplicate-name errors.
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
            program.add_relation(&relation.name, relation.arity);
        }

        for parameter in &self.parameters {
            program.add_parameter(parameter.to_model()?);
        }

        for derived in &self.derived {
            program.add_derived(derived.to_model()?);
        }

        Ok(program)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
pub struct RelationSpec {
    pub name: String,
    pub arity: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedParameterSpec {
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
pub struct DerivedSpec {
    pub name: String,
    pub entity: String,
    pub dtype: DTypeSpec,
    pub unit: Option<String>,
    // Time granularity of the calculation (Year / Month / Day / Instant).
    // Parsed for RuleSpec authoring and round-trip serialisation; the
    // engine treats the query period as authoritative at runtime.
    #[serde(default)]
    pub period: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(flatten)]
    pub semantics: DerivedSemanticsSpec,
}

impl DerivedSpec {
    fn to_model(&self) -> Result<Derived, SpecError> {
        Ok(Derived {
            name: self.name.clone(),
            entity: self.entity.clone(),
            dtype: self.dtype.to_model(),
            unit: self.unit.clone(),
            source: self.source.clone(),
            source_url: self.source_url.clone(),
            semantics: self.semantics.to_model()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScalarValueSpec {
    Bool {
        value: bool,
    },
    Integer {
        value: i64,
    },
    Decimal {
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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

/// Merge an extending programme into its base. Parameter versions are
/// concatenated by parameter name (the engine's effective_from ordering picks
/// the right version at evaluation). Units, relations, and derived outputs
/// are additive — duplicate names across base and extension raise
/// `SpecError::DuplicateOnMerge`.
pub fn merge_programs(
    mut base: ProgramSpec,
    extension: ProgramSpec,
) -> Result<ProgramSpec, SpecError> {
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
