use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::Deserialize;
use thiserror::Error;

use crate::spec::{
    IndexedParameterSpec, ParameterVersionSpec, ProgramSpec, RelationSpec, ScalarValueSpec,
    UnitSpec,
};

#[derive(Debug, Error)]
pub enum RuleSpecError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("RuleSpec requires `format: rulespec/v1` or `schema: axiom.rules.*`")]
    MissingDiscriminator,
    #[error("failed to read RuleSpec file `{path}`: {error}")]
    ReadFile { path: String, error: std::io::Error },
    #[error("RuleSpec import `{target}` in `{path}` could not be resolved")]
    UnresolvedImport { path: String, target: String },
    #[error("RuleSpec import cycle detected at `{path}`")]
    ImportCycle { path: String },
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
    #[error("RuleSpec reiteration `{name}` must declare reiterates.target")]
    MissingReiterationTarget { name: String },
    #[error("RuleSpec relation `{name}` is declared with conflicting arities {existing} and {new}")]
    RelationArityConflict {
        name: String,
        existing: usize,
        new: usize,
    },
    #[error("failed to load extended RuleSpec module: {0}")]
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
    pub imports: Vec<String>,
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
    #[serde(alias = "Reiteration")]
    Reiteration,
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
pub struct ReiterationRef {
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub target: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub authority: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub relationship: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RuleDefinition {
    pub name: String,
    #[serde(skip)]
    pub origin_target: Option<String>,
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
    pub reiterates: Option<ReiterationRef>,
    #[serde(default)]
    pub verification: Option<serde_yaml::Value>,
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
    let mut document: RulesDocument = serde_yaml::from_str(source)?;
    let origin_target = document
        .module
        .as_ref()
        .and_then(|module| module.id.clone());
    document.assign_origin_target(origin_target);
    document.to_program_spec()
}

pub fn load_rulespec_file(path: impl AsRef<Path>) -> Result<ProgramSpec, RuleSpecError> {
    let path = path.as_ref();
    let mut context = RuleSpecLoadContext::default();
    let document = load_rulespec_document_inner(path, &mut context)?;
    document.to_program_spec()
}

#[derive(Default)]
struct RuleSpecLoadContext {
    stack: Vec<PathBuf>,
    loaded: HashSet<PathBuf>,
}

fn load_rulespec_document_inner(
    path: &Path,
    context: &mut RuleSpecLoadContext,
) -> Result<RulesDocument, RuleSpecError> {
    let source = fs::read_to_string(path).map_err(|error| RuleSpecError::ReadFile {
        path: path.display().to_string(),
        error,
    })?;
    let resolved_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if context.stack.contains(&resolved_path) {
        return Err(RuleSpecError::ImportCycle {
            path: resolved_path.display().to_string(),
        });
    }
    if context.loaded.contains(&resolved_path) {
        return Ok(RulesDocument::default());
    }

    if !looks_like_rulespec_yaml(&source) {
        return Err(RuleSpecError::MissingDiscriminator);
    }
    context.stack.push(resolved_path.clone());
    let mut document: RulesDocument = serde_yaml::from_str(&source)?;
    document.assign_origin_target(canonical_rulespec_target(path));
    let mut combined = RulesDocument::default();

    if let Some(extends) = document.extends.as_deref() {
        let base_path = path.parent().unwrap_or_else(|| Path::new("")).join(extends);
        let base = load_extended_document(&base_path, context)?;
        combined = merge_rules_documents(combined, base);
    }

    for import in &document.imports {
        let import_path = resolve_rulespec_import(path, import)?;
        let imported = load_rulespec_document_inner(&import_path, context)?;
        combined = merge_rules_documents(combined, imported);
    }

    combined = merge_rules_documents(combined, document.without_dependency_directives());
    context.loaded.insert(resolved_path);
    context.stack.pop();
    Ok(combined)
}

fn load_extended_document(
    path: &Path,
    context: &mut RuleSpecLoadContext,
) -> Result<RulesDocument, RuleSpecError> {
    load_rulespec_document_inner(path, context)
}

fn merge_rules_documents(mut base: RulesDocument, extension: RulesDocument) -> RulesDocument {
    if extension.format.is_some() {
        base.format = extension.format;
    }
    if extension.schema.is_some() {
        base.schema = extension.schema;
    }
    if extension.module.is_some() {
        base.module = extension.module;
    }
    base.units.extend(extension.units);
    base.relations.extend(extension.relations);
    base.rules.extend(extension.rules);
    base
}

fn resolve_rulespec_import(importer_path: &Path, import: &str) -> Result<PathBuf, RuleSpecError> {
    let target = import.trim().trim_matches(['"', '\'']);
    let target_without_fragment = target
        .split_once('#')
        .map(|(base, _)| base)
        .unwrap_or(target)
        .trim();
    if let Some((prefix, relative)) = target_without_fragment.split_once(':') {
        if is_canonical_repo_prefix(prefix) {
            return resolve_canonical_rulespec_import(importer_path, prefix, relative, import);
        }
    }

    let relative = import_target_to_rulespec_path(target_without_fragment);
    let target_path = if relative.is_absolute() {
        relative
    } else {
        importer_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(relative)
    };
    if target_path.exists() {
        return Ok(target_path);
    }
    Err(RuleSpecError::UnresolvedImport {
        path: importer_path.display().to_string(),
        target: import.to_string(),
    })
}

fn resolve_canonical_rulespec_import(
    importer_path: &Path,
    prefix: &str,
    relative: &str,
    import: &str,
) -> Result<PathBuf, RuleSpecError> {
    let relative_path = import_target_to_rulespec_path(relative.trim().trim_matches('/'));
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RuleSpecError::UnresolvedImport {
            path: importer_path.display().to_string(),
            target: import.to_string(),
        });
    }

    let repo_name = format!("rules-{prefix}");
    for root in candidate_rule_repo_roots(importer_path, &repo_name) {
        let target_path = root.join(&relative_path);
        if target_path.exists() {
            return Ok(target_path);
        }
    }
    Err(RuleSpecError::UnresolvedImport {
        path: importer_path.display().to_string(),
        target: import.to_string(),
    })
}

fn canonical_rulespec_target(path: &Path) -> Option<String> {
    let resolved_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let components = resolved_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<String>>();
    let repo_index = components
        .iter()
        .rposition(|component| component.starts_with("rules-"))?;
    let prefix = components[repo_index].strip_prefix("rules-")?;
    if prefix.is_empty() || repo_index + 1 >= components.len() {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &components[repo_index + 1..] {
        relative.push(component);
    }
    let mut relative = relative.to_string_lossy().replace('\\', "/");
    if relative.ends_with(".yaml") || relative.ends_with(".yml") {
        if let Some(stem) = Path::new(&relative).with_extension("").to_str() {
            relative = stem.replace('\\', "/");
        }
    }
    if relative.is_empty() {
        None
    } else {
        Some(format!("{prefix}:{relative}"))
    }
}

fn import_target_to_rulespec_path(target: &str) -> PathBuf {
    let normalized = target.trim().trim_matches(['"', '\'']);
    let path = PathBuf::from(normalized);
    if normalized.ends_with(".yaml") || normalized.ends_with(".yml") {
        path
    } else {
        PathBuf::from(format!("{normalized}.yaml"))
    }
}

fn is_canonical_repo_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

fn candidate_rule_repo_roots(importer_path: &Path, repo_name: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |candidate: PathBuf| {
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    };

    if let Some(env_roots) = env::var_os("AXIOM_RULE_REPO_ROOTS") {
        for raw_root in env::split_paths(&env_roots) {
            if raw_root.file_name().is_some_and(|name| name == repo_name) {
                add(raw_root);
            } else {
                add(raw_root.join(repo_name));
            }
        }
    }

    for ancestor in importer_path.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == repo_name) {
            add(ancestor.to_path_buf());
        }
        if let Some(parent) = ancestor.parent() {
            add(parent.join(repo_name));
        }
        add(ancestor.join("_axiom").join(repo_name));
    }

    if let Ok(cwd) = env::current_dir() {
        add(cwd.join(repo_name));
        add(cwd.join("_axiom").join(repo_name));
    }
    candidates
}

impl RulesDocument {
    fn assign_origin_target(&mut self, origin_target: Option<String>) {
        for rule in &mut self.rules {
            rule.origin_target = origin_target.clone();
        }
    }

    fn without_dependency_directives(mut self) -> Self {
        self.extends = None;
        self.imports.clear();
        self
    }

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
                RuleKind::Reiteration => {
                    rule.validate_reiteration()?;
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
        self.apply_rule_ids(&mut program);
        append_missing_units(&mut program, &self.units);
        append_missing_relations(&mut program, &explicit_relations)?;
        Ok(program)
    }

    fn apply_rule_ids(&self, program: &mut ProgramSpec) {
        for rule in &self.rules {
            let Some(rule_id) = rule.canonical_rule_id() else {
                continue;
            };
            for parameter in &mut program.parameters {
                if parameter.name == rule.name {
                    parameter.id = Some(rule_id.clone());
                }
            }
            for derived in &mut program.derived {
                if derived.name == rule.name {
                    derived.id = Some(rule_id.clone());
                }
            }
        }
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
    fn canonical_rule_id(&self) -> Option<String> {
        self.origin_target
            .as_ref()
            .map(|target| format!("{target}#{}", self.name))
    }

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
            id: self.canonical_rule_id(),
            name: self.name.clone(),
            unit: self.unit.clone(),
            indexed_by: Some(indexed_by),
            versions,
        })
    }

    fn validate_reiteration(&self) -> Result<(), RuleSpecError> {
        let target = self
            .reiterates
            .as_ref()
            .and_then(|reiterates| reiterates.target.as_deref())
            .map(str::trim)
            .unwrap_or_default();
        if target.is_empty() {
            return Err(RuleSpecError::MissingReiterationTarget {
                name: self.name.clone(),
            });
        }
        Ok(())
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
