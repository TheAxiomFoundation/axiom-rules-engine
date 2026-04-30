use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use serde::Deserialize;
use thiserror::Error;

use crate::spec::{
    IndexedParameterSpec, ParameterVersionSpec, ProgramSpec, RelationSpec, ScalarValueSpec,
    UnitSpec, merge_programs,
};

#[derive(Debug, Error)]
pub enum RuleSpecError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("RuleSpec requires `format: rulespec/v1` or `schema: axiom.rules.*`")]
    MissingDiscriminator,
    #[error("failed to read RuleSpec file `{path}`: {error}")]
    ReadFile { path: String, error: std::io::Error },
    #[error("failed to parse RuleSpec formula: {0}")]
    Formula(#[from] crate::formula::FormulaError),
    #[error("RuleSpec rule `{name}` uses unsupported kind `{kind}`")]
    UnsupportedRuleKind { name: String, kind: String },
    #[error("RuleSpec rule `{name}` has no formula version")]
    MissingFormula { name: String },
    #[error("RuleSpec rule `{name}` has a formula version without effective_from")]
    MissingEffectiveFrom { name: String },
    #[error("RuleSpec parameter table `{name}` has values but no indexed_by")]
    MissingIndexedBy { name: String },
    #[error("RuleSpec relation `{name}` is declared with conflicting arities {existing} and {new}")]
    RelationArityConflict {
        name: String,
        existing: usize,
        new: usize,
    },
    #[error("failed to load extended RuleSpec programme: {0}")]
    Extended(#[from] crate::spec::SpecError),
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RulesDocument {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub module: Option<ModuleMetadata>,
    #[serde(default)]
    pub units: Vec<UnitSpec>,
    #[serde(default)]
    pub relations: Vec<RelationSpec>,
    #[serde(default)]
    pub rules: Vec<RuleDefinition>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ModuleMetadata {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    #[serde(alias = "Parameter")]
    Parameter,
    #[serde(alias = "Derived")]
    Derived,
    #[serde(alias = "Relation")]
    Relation,
    #[serde(alias = "DerivedRelation", alias = "derivedRelation")]
    DerivedRelation,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceRef {
    #[serde(
        default,
        alias = "source",
        deserialize_with = "deserialize_optional_string_like"
    )]
    pub citation: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RuleDefinition {
    pub name: String,
    #[serde(default)]
    pub kind: Option<RuleKind>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub entity: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub dtype: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub period: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub unit: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub label: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub default: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub indexed_by: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub source_url: Option<String>,
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    #[serde(default)]
    pub arity: Option<usize>,
    #[serde(default, alias = "from")]
    pub effective_from: Option<NaiveDate>,
    #[serde(default, alias = "to")]
    pub effective_to: Option<NaiveDate>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub formula: Option<String>,
    #[serde(default)]
    pub versions: Vec<RuleVersion>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RuleVersion {
    #[serde(default, alias = "from")]
    pub effective_from: Option<NaiveDate>,
    #[serde(default, alias = "to")]
    pub effective_to: Option<NaiveDate>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub formula: Option<String>,
    #[serde(default, deserialize_with = "deserialize_parameter_value_map")]
    pub values: BTreeMap<i64, ScalarValueSpec>,
}

pub fn looks_like_rulespec_yaml(source: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(source) else {
        return false;
    };
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let format_key = serde_yaml::Value::String("format".to_string());
    if mapping
        .get(&format_key)
        .and_then(serde_yaml::Value::as_str)
        .is_some_and(|format| format == "rulespec/v1")
    {
        return true;
    }
    let schema_key = serde_yaml::Value::String("schema".to_string());
    if mapping
        .get(&schema_key)
        .and_then(serde_yaml::Value::as_str)
        .is_some_and(|schema| schema.starts_with("axiom.rules"))
    {
        return true;
    }
    false
}

pub fn has_top_level_rules_key(source: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(source) else {
        return false;
    };
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    mapping.contains_key(&serde_yaml::Value::String("rules".to_string()))
}

pub fn lower_rulespec_str(source: &str) -> Result<ProgramSpec, RuleSpecError> {
    if !looks_like_rulespec_yaml(source) {
        return Err(RuleSpecError::MissingDiscriminator);
    }
    let document: RulesDocument = serde_yaml::from_str(source)?;
    document.to_program_spec()
}

pub fn load_rulespec_file(path: impl AsRef<Path>) -> Result<ProgramSpec, RuleSpecError> {
    let path = path.as_ref();
    let source = fs::read_to_string(path).map_err(|error| RuleSpecError::ReadFile {
        path: path.display().to_string(),
        error,
    })?;
    if !looks_like_rulespec_yaml(&source) {
        return Err(RuleSpecError::MissingDiscriminator);
    }
    let document: RulesDocument = serde_yaml::from_str(&source)?;
    let program = document.to_program_spec()?;
    if let Some(extends) = document.extends {
        let base_path = path.parent().unwrap_or_else(|| Path::new("")).join(extends);
        let base = load_extended_program(&base_path)?;
        return Ok(merge_programs(base, program)?);
    }
    Ok(program)
}

fn load_extended_program(path: &Path) -> Result<ProgramSpec, RuleSpecError> {
    load_rulespec_file(path)
}

impl RulesDocument {
    pub fn to_program_spec(&self) -> Result<ProgramSpec, RuleSpecError> {
        let mut formula_source = String::new();
        self.write_header(&mut formula_source);

        let mut explicit_relations = self.relations.clone();
        let mut table_parameters = Vec::new();
        let mut table_parameter_names = HashSet::new();
        for rule in &self.rules {
            match rule.effective_kind() {
                RuleKind::Parameter | RuleKind::Derived => {
                    if rule.is_parameter_table() {
                        rule.write_formula_stub_definition(&mut formula_source)?;
                        table_parameter_names.insert(rule.name.clone());
                        table_parameters.push(rule.to_indexed_parameter_spec()?);
                    } else {
                        rule.write_formula_definition(&mut formula_source)?;
                    }
                }
                RuleKind::Relation => {
                    explicit_relations.push(RelationSpec {
                        name: rule.name.clone(),
                        arity: rule.arity.unwrap_or(2),
                    });
                }
                RuleKind::DerivedRelation => {
                    return Err(RuleSpecError::UnsupportedRuleKind {
                        name: rule.name.clone(),
                        kind: "derived_relation".to_string(),
                    });
                }
            }
        }

        let mut program = if formula_source.trim().is_empty() {
            ProgramSpec::default()
        } else {
            crate::formula::lower_source(&formula_source)?
        };
        if !table_parameters.is_empty() {
            program
                .parameters
                .retain(|parameter| !table_parameter_names.contains(&parameter.name));
            program.parameters.extend(table_parameters);
        }
        append_missing_units(&mut program, &self.units);
        append_missing_relations(&mut program, &explicit_relations)?;
        Ok(program)
    }

    fn write_header(&self, out: &mut String) {
        let Some(module) = &self.module else {
            return;
        };
        if let Some(id) = &module.id {
            out.push_str("# module: ");
            out.push_str(id);
            out.push('\n');
        }
        if let Some(title) = &module.title {
            out.push_str("# title: ");
            out.push_str(title);
            out.push('\n');
        }
        if let Some(status) = &module.status {
            out.push_str("# status: ");
            out.push_str(status);
            out.push('\n');
        }
        if let Some(summary) = &module.summary {
            for line in summary.lines() {
                out.push_str("# ");
                out.push_str(line);
                out.push('\n');
            }
        }
        if !out.is_empty() {
            out.push('\n');
        }
    }
}

impl RuleDefinition {
    fn effective_kind(&self) -> RuleKind {
        self.kind.clone().unwrap_or_else(|| {
            if self.arity.is_some() && self.formula.is_none() && self.versions.is_empty() {
                RuleKind::Relation
            } else if self.entity.is_some() {
                RuleKind::Derived
            } else {
                RuleKind::Parameter
            }
        })
    }

    fn effective_versions(&self) -> Vec<RuleVersion> {
        if !self.versions.is_empty() {
            return self.versions.clone();
        }
        if self.formula.is_some() || self.effective_from.is_some() {
            return vec![RuleVersion {
                effective_from: self.effective_from,
                effective_to: self.effective_to,
                formula: self.formula.clone(),
                values: BTreeMap::new(),
            }];
        }
        Vec::new()
    }

    fn is_parameter_table(&self) -> bool {
        self.effective_kind() == RuleKind::Parameter
            && self
                .versions
                .iter()
                .any(|version| !version.values.is_empty())
    }

    fn to_indexed_parameter_spec(&self) -> Result<IndexedParameterSpec, RuleSpecError> {
        let indexed_by =
            self.indexed_by
                .clone()
                .ok_or_else(|| RuleSpecError::MissingIndexedBy {
                    name: self.name.clone(),
                })?;
        let mut versions = Vec::new();
        for version in &self.versions {
            if version.values.is_empty() {
                continue;
            }
            let effective_from =
                version
                    .effective_from
                    .ok_or_else(|| RuleSpecError::MissingEffectiveFrom {
                        name: self.name.clone(),
                    })?;
            versions.push(ParameterVersionSpec {
                effective_from,
                values: version.values.clone(),
            });
        }
        if versions.is_empty() {
            return Err(RuleSpecError::MissingFormula {
                name: self.name.clone(),
            });
        }
        Ok(IndexedParameterSpec {
            name: self.name.clone(),
            unit: self.unit.clone(),
            indexed_by: Some(indexed_by),
            versions,
        })
    }

    fn write_formula_definition(&self, out: &mut String) -> Result<(), RuleSpecError> {
        let versions = self.effective_versions();
        if versions.is_empty() {
            return Err(RuleSpecError::MissingFormula {
                name: self.name.clone(),
            });
        }

        out.push_str(&self.name);
        out.push_str(":\n");
        write_metadata_raw(out, "entity", self.entity.as_deref());
        write_metadata(out, "dtype", self.dtype.as_deref());
        write_metadata(out, "period", self.period.as_deref());
        write_metadata(out, "unit", self.unit.as_deref());
        write_metadata(out, "label", self.label.as_deref());
        write_metadata(out, "description", self.description.as_deref());
        write_metadata(out, "default", self.default.as_deref());
        write_metadata(out, "indexed_by", self.indexed_by.as_deref());
        write_metadata(out, "status", self.status.as_deref());
        let (source, source_url) = self.effective_source();
        write_metadata(out, "source", source.as_deref());
        write_metadata(out, "source_url", source_url.as_deref());

        for version in versions {
            let start =
                version
                    .effective_from
                    .ok_or_else(|| RuleSpecError::MissingEffectiveFrom {
                        name: self.name.clone(),
                    })?;
            let formula = version
                .formula
                .as_deref()
                .map(str::trim)
                .filter(|formula| !formula.is_empty())
                .ok_or_else(|| RuleSpecError::MissingFormula {
                    name: self.name.clone(),
                })?;
            out.push_str("    from ");
            out.push_str(&start.to_string());
            if let Some(end) = version.effective_to {
                out.push_str(" to ");
                out.push_str(&end.to_string());
            }
            out.push_str(":\n");
            for line in formula.lines() {
                out.push_str("        ");
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out.push('\n');
        Ok(())
    }

    fn write_formula_stub_definition(&self, out: &mut String) -> Result<(), RuleSpecError> {
        let effective_from = self
            .versions
            .iter()
            .find(|version| !version.values.is_empty())
            .and_then(|version| version.effective_from)
            .ok_or_else(|| RuleSpecError::MissingEffectiveFrom {
                name: self.name.clone(),
            })?;

        out.push_str(&self.name);
        out.push_str(":\n");
        write_metadata(out, "dtype", self.dtype.as_deref());
        write_metadata(out, "unit", self.unit.as_deref());
        write_metadata(out, "indexed_by", self.indexed_by.as_deref());
        let (source, source_url) = self.effective_source();
        write_metadata(out, "source", source.as_deref());
        write_metadata(out, "source_url", source_url.as_deref());
        out.push_str("    from ");
        out.push_str(&effective_from.to_string());
        out.push_str(":\n        0\n\n");
        Ok(())
    }

    fn effective_source(&self) -> (Option<String>, Option<String>) {
        let citation = self.source.clone().or_else(|| {
            self.sources
                .iter()
                .find_map(|source| source.citation.clone())
        });
        let url = self
            .source_url
            .clone()
            .or_else(|| self.sources.iter().find_map(|source| source.url.clone()));
        (citation, url)
    }
}

fn append_missing_units(program: &mut ProgramSpec, units: &[UnitSpec]) {
    let mut existing = program
        .units
        .iter()
        .map(|unit| unit.name.clone())
        .collect::<HashSet<_>>();
    for unit in units {
        if existing.insert(unit.name.clone()) {
            program.units.push(unit.clone());
        }
    }
}

fn append_missing_relations(
    program: &mut ProgramSpec,
    relations: &[RelationSpec],
) -> Result<(), RuleSpecError> {
    for relation in relations {
        if let Some(existing) = program
            .relations
            .iter()
            .find(|existing| existing.name == relation.name)
        {
            if existing.arity != relation.arity {
                return Err(RuleSpecError::RelationArityConflict {
                    name: relation.name.clone(),
                    existing: existing.arity,
                    new: relation.arity,
                });
            }
            continue;
        }
        program.relations.push(relation.clone());
    }
    Ok(())
}

fn write_metadata(out: &mut String, key: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if value.is_empty() {
        return;
    }
    out.push_str("    ");
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&quote_formula_string(value));
    out.push('\n');
}

fn write_metadata_raw(out: &mut String, key: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if value.is_empty() {
        return;
    }
    out.push_str("    ");
    out.push_str(key);
    out.push_str(": ");
    out.push_str(value);
    out.push('\n');
}

fn quote_formula_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn deserialize_optional_string_like<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_yaml::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        serde_yaml::Value::Null => Ok(None),
        serde_yaml::Value::String(value) => Ok(Some(value)),
        serde_yaml::Value::Bool(value) => Ok(Some(value.to_string())),
        serde_yaml::Value::Number(value) => Ok(Some(value.to_string())),
        other => Err(serde::de::Error::custom(format!(
            "expected scalar string-like value, got {other:?}"
        ))),
    }
}

fn deserialize_parameter_value_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<i64, ScalarValueSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = BTreeMap::<i64, serde_yaml::Value>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|(key, value)| Ok((key, scalar_value_from_yaml(value)?)))
        .collect()
}

fn scalar_value_from_yaml<E: serde::de::Error>(
    value: serde_yaml::Value,
) -> Result<ScalarValueSpec, E> {
    match value {
        serde_yaml::Value::Bool(value) => Ok(ScalarValueSpec::Bool { value }),
        serde_yaml::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(ScalarValueSpec::Integer { value })
            } else {
                Ok(ScalarValueSpec::Decimal {
                    value: value.to_string(),
                })
            }
        }
        serde_yaml::Value::String(value) => {
            if let Ok(parsed) = value.parse::<i64>() {
                Ok(ScalarValueSpec::Integer { value: parsed })
            } else if value.parse::<rust_decimal::Decimal>().is_ok() {
                Ok(ScalarValueSpec::Decimal { value })
            } else if let Ok(parsed) = NaiveDate::parse_from_str(&value, "%Y-%m-%d") {
                Ok(ScalarValueSpec::Date { value: parsed })
            } else {
                Ok(ScalarValueSpec::Text { value })
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected scalar parameter value, got {other:?}"
        ))),
    }
}
