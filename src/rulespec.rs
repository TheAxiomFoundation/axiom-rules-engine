use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::Deserialize;
use thiserror::Error;

use crate::spec::{
    DerivedSemanticsSpec, IndexedParameterSpec, JudgmentExprSpec, ParameterVersionSpec,
    ProgramSpec, RelationDerivationSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec, UnitSpec,
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
    #[error("RuleSpec rule `{name}` must declare `kind`")]
    MissingRuleKind { name: String },
    #[error("RuleSpec rule `{name}` has no formula version")]
    MissingFormula { name: String },
    #[error("RuleSpec rule `{name}` has a formula version without effective_from")]
    MissingEffectiveFrom { name: String },
    #[error("RuleSpec parameter table `{name}` has values but no indexed_by")]
    MissingIndexedBy { name: String },
    #[error(
        "RuleSpec files must declare runtime predicates as `rules[].kind: data_relation`, not top-level `relations`"
    )]
    TopLevelRelationsUnsupported,
    #[error(
        "RuleSpec rule `{name}` must declare runtime predicate arity under `data_relation.arity`, not top-level `arity`"
    )]
    TopLevelArityUnsupported { name: String },
    #[error("RuleSpec data relation `{name}` must declare data_relation.arity")]
    MissingDataRelationArity { name: String },
    #[error("RuleSpec derived relation `{name}` must declare derived_relation")]
    MissingDerivedRelation { name: String },
    #[error("RuleSpec derived relation `{name}` must declare derived_relation.arity")]
    MissingDerivedRelationArity { name: String },
    #[error("RuleSpec derived relation `{name}` must declare derived_relation.source_relation")]
    MissingDerivedRelationSource { name: String },
    #[error("RuleSpec derived relation `{name}` must declare exactly one membership formula version")]
    InvalidDerivedRelationFormulaVersions { name: String },
    #[error("RuleSpec source relation `{name}` must declare source_relation")]
    MissingSourceRelation { name: String },
    #[error("RuleSpec source relation `{name}` must declare source_relation.type")]
    MissingSourceRelationType { name: String },
    #[error("RuleSpec source relation `{name}` must declare source_relation.target")]
    MissingSourceRelationTarget { name: String },
    #[error("RuleSpec source relation `{name}` has non-absolute `{field}` reference `{value}`")]
    InvalidSourceRelationReference {
        name: String,
        field: String,
        value: String,
    },
    #[error(
        "RuleSpec source relation `{name}` with type `{relation_type}` must declare source_relation.basis.delegation"
    )]
    MissingSourceRelationDelegation { name: String, relation_type: String },
    #[error(
        "RuleSpec amendment source relation `{name}` must declare source_relation.amendment.operation"
    )]
    MissingAmendmentOperation { name: String },
    #[error(
        "RuleSpec amendment source relation `{name}` must declare source_relation.amendment.effective"
    )]
    MissingAmendmentEffective { name: String },
    #[error(
        "RuleSpec source relation `{name}` cannot include executable formula, version, table, or runtime relation fields"
    )]
    SourceRelationHasExecutableBody { name: String },
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Parameter,
    Derived,
    DataRelation,
    DerivedRelation,
    SourceRelation,
    Unsupported(String),
}

impl<'de> Deserialize<'de> for RuleKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "parameter" => Self::Parameter,
            "derived" => Self::Derived,
            "data_relation" => Self::DataRelation,
            "derived_relation" => Self::DerivedRelation,
            "source_relation" => Self::SourceRelation,
            _ => Self::Unsupported(value),
        })
    }
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
pub struct DataRelationRef {
    #[serde(default)]
    pub arity: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DerivedRelationRef {
    #[serde(default)]
    pub arity: Option<usize>,
    #[serde(default, alias = "source")]
    pub source_relation: Option<String>,
    #[serde(default)]
    pub current_slot: Option<usize>,
    #[serde(default)]
    pub related_slot: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceRelationType {
    Defines,
    Delegates,
    Implements,
    Sets,
    Amends,
    Restates,
    Cites,
}

impl SourceRelationType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Defines => "defines",
            Self::Delegates => "delegates",
            Self::Implements => "implements",
            Self::Sets => "sets",
            Self::Amends => "amends",
            Self::Restates => "restates",
            Self::Cites => "cites",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceRelationBasis {
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub delegation: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceRelationAmendment {
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub operation: Option<String>,
    #[serde(default)]
    pub effective: Option<serde_yaml::Value>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub superseding_rule: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceRelationRef {
    #[serde(default, rename = "type")]
    pub relation_type: Option<SourceRelationType>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub target: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub authority: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub value: Option<String>,
    #[serde(default)]
    pub basis: Option<SourceRelationBasis>,
    #[serde(default)]
    pub amendment: Option<SourceRelationAmendment>,
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
    pub data_relation: Option<DataRelationRef>,
    #[serde(default)]
    pub derived_relation: Option<DerivedRelationRef>,
    #[serde(default)]
    pub source_relation: Option<SourceRelationRef>,
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

    let repo_name = format!("rulespec-{prefix}");
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
        .rposition(|component| component.starts_with("rulespec-"))?;
    let prefix = components[repo_index].strip_prefix("rulespec-")?;
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

fn is_absolute_rulespec_ref(value: &str) -> bool {
    let value = value.trim();
    let Some((prefix, relative)) = value.split_once(':') else {
        return false;
    };
    is_canonical_repo_prefix(prefix)
        && !relative.trim_matches('/').is_empty()
        && !value.chars().any(char::is_whitespace)
}

fn candidate_rule_repo_roots(importer_path: &Path, repo_name: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |candidate: PathBuf| {
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    };

    if let Some(env_roots) = env::var_os("AXIOM_RULESPEC_REPO_ROOTS") {
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
        if !self.relations.is_empty() {
            return Err(RuleSpecError::TopLevelRelationsUnsupported);
        }
        let mut formula_source = String::new();
        self.write_header(&mut formula_source);

        let mut explicit_relations = Vec::new();
        let mut relation_rewrites = HashMap::new();
        let mut table_parameters = Vec::new();
        let mut table_parameter_names = HashSet::new();
        let derived_rule_names = self
            .rules
            .iter()
            .filter_map(|rule| match rule.kind.as_ref() {
                Some(RuleKind::Derived) => Some(rule.name.clone()),
                _ => None,
            })
            .collect::<HashSet<String>>();
        let relation_predicate_names = self
            .rules
            .iter()
            .filter_map(|rule| match rule.kind.as_ref() {
                Some(RuleKind::DataRelation | RuleKind::DerivedRelation) => {
                    Some(rule.name.clone())
                }
                _ => None,
            })
            .collect::<HashSet<String>>();
        for rule in &self.rules {
            let kind = match rule.declared_kind() {
                Ok(kind) => kind,
                Err(RuleSpecError::MissingRuleKind { .. }) if rule.arity.is_some() => {
                    return Err(RuleSpecError::TopLevelArityUnsupported {
                        name: rule.name.clone(),
                    });
                }
                Err(error) => return Err(error),
            };
            match kind {
                RuleKind::Unsupported(kind) => {
                    return Err(RuleSpecError::UnsupportedRuleKind {
                        name: rule.name.clone(),
                        kind,
                    });
                }
                RuleKind::Parameter | RuleKind::Derived => {
                    rule.reject_top_level_arity()?;
                    if rule.is_parameter_table() {
                        rule.write_formula_stub_definition(&mut formula_source)?;
                        table_parameter_names.insert(rule.name.clone());
                        table_parameters.push(rule.to_indexed_parameter_spec()?);
                    } else {
                        rule.write_formula_definition(&mut formula_source)?;
                    }
                }
                RuleKind::DataRelation => {
                    rule.reject_top_level_arity()?;
                    let relation = rule.to_data_relation_spec()?;
                    if relation.name != rule.name {
                        if let Some(origin_target) = rule.origin_target.as_deref() {
                            relation_rewrites.insert(
                                (origin_target.to_string(), rule.name.clone()),
                                relation.name.clone(),
                            );
                        }
                    }
                    explicit_relations.push(relation);
                }
                RuleKind::DerivedRelation => {
                    rule.reject_top_level_arity()?;
                    let relation = rule
                        .to_derived_relation_spec(&derived_rule_names, &relation_predicate_names)?;
                    if relation.name != rule.name {
                        if let Some(origin_target) = rule.origin_target.as_deref() {
                            relation_rewrites.insert(
                                (origin_target.to_string(), rule.name.clone()),
                                relation.name.clone(),
                            );
                        }
                    }
                    explicit_relations.push(relation);
                }
                RuleKind::SourceRelation => {
                    rule.reject_top_level_arity()?;
                    rule.validate_source_relation()?;
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
        rewrite_relation_references(&mut program, &relation_rewrites, &explicit_relations)?;
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

    fn canonical_relation_id(&self) -> String {
        self.origin_target
            .as_ref()
            .map(|target| format!("{target}#relation.{}", self.name))
            .unwrap_or_else(|| self.name.clone())
    }

    fn declared_kind(&self) -> Result<RuleKind, RuleSpecError> {
        self.kind
            .clone()
            .ok_or_else(|| RuleSpecError::MissingRuleKind {
                name: self.name.clone(),
            })
    }

    fn reject_top_level_arity(&self) -> Result<(), RuleSpecError> {
        if self.arity.is_some() {
            return Err(RuleSpecError::TopLevelArityUnsupported {
                name: self.name.clone(),
            });
        }
        Ok(())
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
        self.versions
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

    fn to_data_relation_spec(&self) -> Result<RelationSpec, RuleSpecError> {
        let arity = self
            .data_relation
            .as_ref()
            .and_then(|data_relation| data_relation.arity)
            .ok_or_else(|| RuleSpecError::MissingDataRelationArity {
                name: self.name.clone(),
            })?;
        Ok(RelationSpec {
            name: self.canonical_relation_id(),
            arity,
            derivation: None,
        })
    }

    fn to_derived_relation_spec(
        &self,
        derived_names: &HashSet<String>,
        relation_predicate_names: &HashSet<String>,
    ) -> Result<RelationSpec, RuleSpecError> {
        let derived_relation =
            self.derived_relation
                .as_ref()
                .ok_or_else(|| RuleSpecError::MissingDerivedRelation {
                    name: self.name.clone(),
                })?;
        let arity =
            derived_relation
                .arity
                .ok_or_else(|| RuleSpecError::MissingDerivedRelationArity {
                    name: self.name.clone(),
                })?;
        let source_relation = derived_relation
            .source_relation
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .ok_or_else(|| RuleSpecError::MissingDerivedRelationSource {
                name: self.name.clone(),
            })?
            .to_string();
        let versions = self.effective_versions();
        if versions.len() != 1 {
            return Err(RuleSpecError::InvalidDerivedRelationFormulaVersions {
                name: self.name.clone(),
            });
        }
        let formula = versions[0]
            .formula
            .as_deref()
            .map(str::trim)
            .filter(|formula| !formula.is_empty())
            .ok_or_else(|| RuleSpecError::MissingFormula {
                name: self.name.clone(),
            })?;
        let predicate = crate::formula::lower_judgment_formula(
            formula,
            derived_names.clone(),
            relation_predicate_names.clone(),
        )?;
        let (inferred_current_slot, inferred_related_slot) =
            crate::formula::infer_relation_slots_for_rulespec(source_relation.as_str());
        Ok(RelationSpec {
            name: self.canonical_relation_id(),
            arity,
            derivation: Some(RelationDerivationSpec {
                source_relation,
                current_slot: derived_relation.current_slot.unwrap_or(inferred_current_slot),
                related_slot: derived_relation.related_slot.unwrap_or(inferred_related_slot),
                predicate,
            }),
        })
    }

    fn validate_source_relation(&self) -> Result<(), RuleSpecError> {
        if self.has_executable_body() {
            return Err(RuleSpecError::SourceRelationHasExecutableBody {
                name: self.name.clone(),
            });
        }

        let source_relation =
            self.source_relation
                .as_ref()
                .ok_or_else(|| RuleSpecError::MissingSourceRelation {
                    name: self.name.clone(),
                })?;
        let relation_type = source_relation.relation_type.as_ref().ok_or_else(|| {
            RuleSpecError::MissingSourceRelationType {
                name: self.name.clone(),
            }
        })?;
        let target = source_relation
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| RuleSpecError::MissingSourceRelationTarget {
                name: self.name.clone(),
            })?;
        self.validate_absolute_source_relation_ref("target", target)?;

        if let Some(value) = source_relation.value.as_deref() {
            self.validate_absolute_source_relation_ref("value", value)?;
            self.validate_source_relation_fragment_ref("value", value)?;
        }
        if let Some(delegation) = source_relation
            .basis
            .as_ref()
            .and_then(|basis| basis.delegation.as_deref())
        {
            self.validate_absolute_source_relation_ref("basis.delegation", delegation)?;
            self.validate_source_relation_fragment_ref("basis.delegation", delegation)?;
        }
        if let Some(superseding_rule) = source_relation
            .amendment
            .as_ref()
            .and_then(|amendment| amendment.superseding_rule.as_deref())
        {
            self.validate_absolute_source_relation_ref(
                "amendment.superseding_rule",
                superseding_rule,
            )?;
            self.validate_source_relation_fragment_ref(
                "amendment.superseding_rule",
                superseding_rule,
            )?;
        }

        if matches!(
            relation_type,
            SourceRelationType::Implements | SourceRelationType::Sets
        ) && source_relation
            .basis
            .as_ref()
            .and_then(|basis| basis.delegation.as_deref())
            .map(str::trim)
            .filter(|delegation| !delegation.is_empty())
            .is_none()
        {
            return Err(RuleSpecError::MissingSourceRelationDelegation {
                name: self.name.clone(),
                relation_type: relation_type.as_str().to_string(),
            });
        }

        if *relation_type == SourceRelationType::Amends {
            let amendment = source_relation.amendment.as_ref();
            if amendment
                .and_then(|amendment| amendment.operation.as_deref())
                .map(str::trim)
                .filter(|operation| !operation.is_empty())
                .is_none()
            {
                return Err(RuleSpecError::MissingAmendmentOperation {
                    name: self.name.clone(),
                });
            }
            if amendment
                .and_then(|amendment| amendment.effective.as_ref())
                .is_none()
            {
                return Err(RuleSpecError::MissingAmendmentEffective {
                    name: self.name.clone(),
                });
            }
        }
        Ok(())
    }

    fn has_executable_body(&self) -> bool {
        self.formula
            .as_deref()
            .map(str::trim)
            .is_some_and(|formula| !formula.is_empty())
            || !self.versions.is_empty()
            || self.arity.is_some()
            || self.indexed_by.is_some()
            || self.entity.is_some()
            || self.dtype.is_some()
            || self.period.is_some()
            || self.unit.is_some()
            || self.default.is_some()
    }

    fn validate_absolute_source_relation_ref(
        &self,
        field: &str,
        value: &str,
    ) -> Result<(), RuleSpecError> {
        if is_absolute_rulespec_ref(value) {
            return Ok(());
        }
        Err(RuleSpecError::InvalidSourceRelationReference {
            name: self.name.clone(),
            field: field.to_string(),
            value: value.to_string(),
        })
    }

    fn validate_source_relation_fragment_ref(
        &self,
        field: &str,
        value: &str,
    ) -> Result<(), RuleSpecError> {
        if value.trim().split_once('#').is_some() {
            return Ok(());
        }
        Err(RuleSpecError::InvalidSourceRelationReference {
            name: self.name.clone(),
            field: field.to_string(),
            value: value.to_string(),
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
            .iter_mut()
            .find(|existing| existing.name == relation.name)
        {
            if existing.arity != relation.arity {
                return Err(RuleSpecError::RelationArityConflict {
                    name: relation.name.clone(),
                    existing: existing.arity,
                    new: relation.arity,
                });
            }
            if existing.derivation.is_none() && relation.derivation.is_some() {
                existing.derivation = relation.derivation.clone();
            }
            continue;
        }
        program.relations.push(relation.clone());
    }
    Ok(())
}

fn rewrite_relation_references(
    program: &mut ProgramSpec,
    rewrites: &HashMap<(String, String), String>,
    explicit_relations: &[RelationSpec],
) -> Result<(), RuleSpecError> {
    if rewrites.is_empty() {
        return Ok(());
    }
    let inferred_arities: HashMap<String, usize> = program
        .relations
        .iter()
        .map(|relation| (relation.name.clone(), relation.arity))
        .collect();
    let explicit_arities: HashMap<String, usize> = explicit_relations
        .iter()
        .map(|relation| (relation.name.clone(), relation.arity))
        .collect();
    for ((_, short_name), canonical_name) in rewrites {
        let Some(inferred_arity) = inferred_arities.get(short_name) else {
            continue;
        };
        let Some(explicit_arity) = explicit_arities.get(canonical_name) else {
            continue;
        };
        if inferred_arity != explicit_arity {
            return Err(RuleSpecError::RelationArityConflict {
                name: canonical_name.clone(),
                existing: *inferred_arity,
                new: *explicit_arity,
            });
        }
    }
    let namespaced_short_names: HashSet<String> = rewrites
        .keys()
        .map(|(_, relation)| relation.clone())
        .collect();
    for derived in &mut program.derived {
        let Some(origin_target) = derived
            .id
            .as_deref()
            .and_then(|id| id.split_once('#').map(|(target, _)| target))
        else {
            continue;
        };
        match &mut derived.semantics {
            DerivedSemanticsSpec::Scalar { expr } => {
                rewrite_scalar_relation_references(expr, origin_target, rewrites);
            }
            DerivedSemanticsSpec::Judgment { expr } => {
                rewrite_judgment_relation_references(expr, origin_target, rewrites);
            }
        }
    }
    for relation in &mut program.relations {
        let Some(origin_target) = relation
            .name
            .split_once("#relation.")
            .map(|(target, _)| target)
        else {
            continue;
        };
        if let Some(derivation) = &mut relation.derivation {
            rewrite_relation_name(&mut derivation.source_relation, origin_target, rewrites);
            rewrite_judgment_relation_references(&mut derivation.predicate, origin_target, rewrites);
        }
    }
    let used_relations = used_relation_names(program);
    program.relations.retain(|relation| {
        !namespaced_short_names.contains(&relation.name) || used_relations.contains(&relation.name)
    });
    Ok(())
}

fn used_relation_names(program: &ProgramSpec) -> HashSet<String> {
    let mut names = HashSet::new();
    for derived in &program.derived {
        match &derived.semantics {
            DerivedSemanticsSpec::Scalar { expr } => {
                collect_scalar_relation_names(expr, &mut names);
            }
            DerivedSemanticsSpec::Judgment { expr } => {
                collect_judgment_relation_names(expr, &mut names);
            }
        }
    }
    for relation in &program.relations {
        if let Some(derivation) = &relation.derivation {
            names.insert(derivation.source_relation.clone());
            collect_judgment_relation_names(&derivation.predicate, &mut names);
        }
    }
    names
}

fn collect_scalar_relation_names(expr: &ScalarExprSpec, names: &mut HashSet<String>) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::Derived { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::InputOrElse { default: _, .. } => {}
        ScalarExprSpec::ParameterLookup { index, .. }
        | ScalarExprSpec::Ceil { value: index }
        | ScalarExprSpec::Floor { value: index } => {
            collect_scalar_relation_names(index, names);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                collect_scalar_relation_names(item, names);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            collect_scalar_relation_names(left, names);
            collect_scalar_relation_names(right, names);
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            collect_scalar_relation_names(date, names);
            collect_scalar_relation_names(days, names);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            collect_scalar_relation_names(from, names);
            collect_scalar_relation_names(to, names);
        }
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            names.insert(relation.clone());
            if let Some(where_clause) = where_clause {
                collect_judgment_relation_names(where_clause, names);
            }
        }
        ScalarExprSpec::SumRelated {
            relation,
            where_clause,
            ..
        } => {
            names.insert(relation.clone());
            if let Some(where_clause) = where_clause {
                collect_judgment_relation_names(where_clause, names);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_relation_names(condition, names);
            collect_scalar_relation_names(then_expr, names);
            collect_scalar_relation_names(else_expr, names);
        }
    }
}

fn collect_judgment_relation_names(expr: &JudgmentExprSpec, names: &mut HashSet<String>) {
    match expr {
        JudgmentExprSpec::Derived { .. } => {}
        JudgmentExprSpec::RelationMember { relation, .. } => {
            names.insert(relation.clone());
        }
        JudgmentExprSpec::Comparison { left, right, .. } => {
            collect_scalar_relation_names(left, names);
            collect_scalar_relation_names(right, names);
        }
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                collect_judgment_relation_names(item, names);
            }
        }
        JudgmentExprSpec::Not { item } => {
            collect_judgment_relation_names(item, names);
        }
    }
}

fn rewrite_scalar_relation_references(
    expr: &mut ScalarExprSpec,
    origin_target: &str,
    rewrites: &HashMap<(String, String), String>,
) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::Derived { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::InputOrElse { default: _, .. } => {}
        ScalarExprSpec::ParameterLookup { index, .. }
        | ScalarExprSpec::Ceil { value: index }
        | ScalarExprSpec::Floor { value: index } => {
            rewrite_scalar_relation_references(index, origin_target, rewrites);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                rewrite_scalar_relation_references(item, origin_target, rewrites);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            rewrite_scalar_relation_references(left, origin_target, rewrites);
            rewrite_scalar_relation_references(right, origin_target, rewrites);
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            rewrite_scalar_relation_references(date, origin_target, rewrites);
            rewrite_scalar_relation_references(days, origin_target, rewrites);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            rewrite_scalar_relation_references(from, origin_target, rewrites);
            rewrite_scalar_relation_references(to, origin_target, rewrites);
        }
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            rewrite_relation_name(relation, origin_target, rewrites);
            if let Some(where_clause) = where_clause {
                rewrite_judgment_relation_references(where_clause, origin_target, rewrites);
            }
        }
        ScalarExprSpec::SumRelated {
            relation,
            where_clause,
            ..
        } => {
            rewrite_relation_name(relation, origin_target, rewrites);
            if let Some(where_clause) = where_clause {
                rewrite_judgment_relation_references(where_clause, origin_target, rewrites);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_judgment_relation_references(condition, origin_target, rewrites);
            rewrite_scalar_relation_references(then_expr, origin_target, rewrites);
            rewrite_scalar_relation_references(else_expr, origin_target, rewrites);
        }
    }
}

fn rewrite_judgment_relation_references(
    expr: &mut JudgmentExprSpec,
    origin_target: &str,
    rewrites: &HashMap<(String, String), String>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            rewrite_scalar_relation_references(left, origin_target, rewrites);
            rewrite_scalar_relation_references(right, origin_target, rewrites);
        }
        JudgmentExprSpec::Derived { .. } => {}
        JudgmentExprSpec::RelationMember { relation, .. } => {
            rewrite_relation_name(relation, origin_target, rewrites);
        }
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                rewrite_judgment_relation_references(item, origin_target, rewrites);
            }
        }
        JudgmentExprSpec::Not { item } => {
            rewrite_judgment_relation_references(item, origin_target, rewrites);
        }
    }
}

fn rewrite_relation_name(
    relation: &mut String,
    origin_target: &str,
    rewrites: &HashMap<(String, String), String>,
) {
    if let Some(rewrite) = rewrites.get(&(origin_target.to_string(), relation.clone())) {
        *relation = rewrite.clone();
    }
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
