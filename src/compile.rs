use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
#[cfg(feature = "fs")]
use std::fs;
#[cfg(feature = "fs")]
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::spec::{
    DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, RelatedValueRefSpec, ScalarExprSpec,
};

#[derive(Debug, Error)]
pub enum CompileError {
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
    #[cfg(feature = "fs")]
    #[error("failed to read compiled artefact `{path}`: {error}")]
    ReadArtifactFile { path: String, error: std::io::Error },
    #[error("unknown derived dependency `{dependency}` referenced from `{derived}`")]
    UnknownDerivedDependency { derived: String, dependency: String },
    #[error("duplicate derived rule `{name}`")]
    DuplicateDerivedRule { name: String },
    #[error("cyclic derived dependency detected involving: {cycle}")]
    CyclicDependency { cycle: String },
    #[error("unknown relation dependency `{dependency}` referenced from relation `{relation}`")]
    UnknownRelationDependency {
        relation: String,
        dependency: String,
    },
    #[error("cyclic relation dependency detected involving: {cycle}")]
    CyclicRelationDependency { cycle: String },
    #[cfg(feature = "fs")]
    #[error("failed to read program file `{path}`: {error}")]
    ReadProgramFile { path: String, error: std::io::Error },
    #[cfg(feature = "fs")]
    #[error("failed to write compiled artefact `{path}`: {error}")]
    WriteArtifactFile { path: String, error: std::io::Error },
    #[error("failed to serialise compiled artefact: {0}")]
    SerializeArtifact(serde_json::Error),
    #[error("failed to parse compiled artefact `{path}`: {error}")]
    DeserializeArtifact {
        path: String,
        error: serde_json::Error,
    },
    #[error("failed to load RuleSpec module `{path}`: {error}")]
    RuleSpec {
        path: String,
        error: crate::rulespec::RuleSpecError,
    },
    #[error(
        "ambiguous RuleSpec module YAML `{path}` has a top-level `rules:` key but no RuleSpec discriminator (`format: rulespec/v1` or `schema: axiom.rules.*`)"
    )]
    AmbiguousRuleSpecYaml { path: String },
    #[error(
        "compiled artefact `{path}` has artifact_format_version {found}, but this engine supports up to {supported}; recompile the program with this engine or upgrade the engine"
    )]
    UnsupportedArtifactFormatVersion {
        path: String,
        found: u32,
        supported: u32,
    },
    #[cfg(feature = "fs")]
    #[error("failed to read corpus provisions `{path}`: {error}")]
    ReadProvisionsFile { path: String, error: std::io::Error },
    #[error("failed to parse corpus provision record at `{path}` line {line}: {error}")]
    ParseProvisionRecord {
        path: String,
        line: usize,
        error: serde_json::Error,
    },
}

/// Format version stamped into every artifact this engine compiles.
/// Artifacts with a missing field (version 0) predate stamping and are
/// accepted; artifacts newer than this are rejected at load.
pub const ARTIFACT_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompiledProgramArtifact {
    #[serde(default)]
    pub artifact_format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    pub program: ProgramSpec,
    pub metadata: CompiledProgramMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompiledProgramMetadata {
    pub evaluation_order: Vec<String>,
    pub fast_path: FastPathMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FastPathMetadata {
    pub strategy: String,
    pub compatible: bool,
    pub blockers: Vec<String>,
}

impl CompiledProgramArtifact {
    pub fn compile(program: ProgramSpec) -> Result<Self, CompileError> {
        // Reject a rounding declaration on a non-currency (or undeclared) unit
        // at compile time, so a malformed artifact never ships. Execution paths
        // re-check the same invariant via `to_program`.
        program.validate_rounding()?;
        let evaluation_order = evaluation_order(&program)?;
        let fast_path = fast_path_metadata(&program);
        Ok(Self {
            artifact_format_version: ARTIFACT_FORMAT_VERSION,
            engine_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            program,
            metadata: CompiledProgramMetadata {
                evaluation_order,
                fast_path,
            },
        })
    }

    fn check_format_version(self, path: &str) -> Result<Self, CompileError> {
        if self.artifact_format_version > ARTIFACT_FORMAT_VERSION {
            return Err(CompileError::UnsupportedArtifactFormatVersion {
                path: path.to_string(),
                found: self.artifact_format_version,
                supported: ARTIFACT_FORMAT_VERSION,
            });
        }
        Ok(self)
    }

    pub fn from_rulespec_str(source: &str) -> Result<Self, CompileError> {
        if crate::rulespec::looks_like_rulespec_yaml(source) {
            let program = crate::rulespec::lower_rulespec_str(source).map_err(|error| {
                CompileError::RuleSpec {
                    path: "<memory>".to_string(),
                    error,
                }
            })?;
            return Self::compile(program);
        }
        if crate::rulespec::has_top_level_rules_key(source) {
            return Err(CompileError::AmbiguousRuleSpecYaml {
                path: "<memory>".to_string(),
            });
        }
        Err(CompileError::RuleSpec {
            path: "<memory>".to_string(),
            error: crate::rulespec::RuleSpecError::MissingDiscriminator,
        })
    }

    /// Compile the module at `root_target` (canonical form, for example
    /// `us:statutes/7/2015/e`), resolving every module through a
    /// host-supplied [`crate::source::ModuleSource`]. The pure counterpart of
    /// [`Self::from_rulespec_file`]: no filesystem or environment access.
    pub fn from_rulespec_with_source(
        root_target: &str,
        source: &dyn crate::source::ModuleSource,
    ) -> Result<Self, CompileError> {
        let program =
            crate::rulespec::load_rulespec_with_source(root_target, source).map_err(|error| {
                CompileError::RuleSpec {
                    path: root_target.to_string(),
                    error,
                }
            })?;
        Self::compile(program)
    }

    #[cfg(feature = "fs")]
    pub fn from_rulespec_file(path: impl AsRef<Path>) -> Result<Self, CompileError> {
        let p = path.as_ref();
        let source = fs::read_to_string(p).map_err(|error| CompileError::ReadProgramFile {
            path: p.display().to_string(),
            error,
        })?;
        if crate::rulespec::looks_like_rulespec_yaml(&source) {
            let program =
                crate::rulespec::load_rulespec_file(p).map_err(|error| CompileError::RuleSpec {
                    path: p.display().to_string(),
                    error,
                })?;
            return Self::compile(program);
        }
        if crate::rulespec::has_top_level_rules_key(&source) {
            return Err(CompileError::AmbiguousRuleSpecYaml {
                path: p.display().to_string(),
            });
        }
        Err(CompileError::RuleSpec {
            path: p.display().to_string(),
            error: crate::rulespec::RuleSpecError::MissingDiscriminator,
        })
    }

    pub fn from_json_str(source: &str) -> Result<Self, CompileError> {
        let artifact: Self =
            serde_json::from_str(source).map_err(|error| CompileError::DeserializeArtifact {
                path: "<memory>".to_string(),
                error,
            })?;
        artifact.check_format_version("<memory>")
    }

    #[cfg(feature = "fs")]
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self, CompileError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|error| CompileError::ReadArtifactFile {
            path: path.display().to_string(),
            error,
        })?;
        let artifact: Self =
            serde_json::from_str(&source).map_err(|error| CompileError::DeserializeArtifact {
                path: path.display().to_string(),
                error,
            })?;
        artifact.check_format_version(&path.display().to_string())
    }

    #[cfg(feature = "fs")]
    pub fn write_json_file(&self, path: impl AsRef<Path>) -> Result<(), CompileError> {
        let path = path.as_ref();
        let json = serde_json::to_string_pretty(self).map_err(CompileError::SerializeArtifact)?;
        fs::write(path, json).map_err(|error| CompileError::WriteArtifactFile {
            path: path.display().to_string(),
            error,
        })
    }

    /// Resolve each rule's and parameter's `corpus_citation_path` to a
    /// `source_url` through a corpus provision index, filling only entries
    /// whose `source_url` is not already set (an inline URL always wins).
    /// Returns how many URLs were filled. Purely a lookup over the given
    /// index — no network, no clock — so the same artifact plus the same
    /// provisions always produce byte-identical output.
    pub fn resolve_source_urls(&mut self, provisions: &CorpusProvisionIndex) -> usize {
        let mut resolved = 0;
        let mut fill = |citation_path: &Option<String>, source_url: &mut Option<String>| {
            if source_url.is_some() {
                return;
            }
            let Some(url) = citation_path
                .as_deref()
                .and_then(|citation_path| provisions.source_url(citation_path))
            else {
                return;
            };
            *source_url = Some(url.to_string());
            resolved += 1;
        };
        for parameter in &mut self.program.parameters {
            fill(&parameter.corpus_citation_path, &mut parameter.source_url);
        }
        for derived in &mut self.program.derived {
            fill(&derived.corpus_citation_path, &mut derived.source_url);
        }
        resolved
    }
}

/// An index of corpus provision records, mapping a `citation_path` (the join
/// key modules declare as `source_verification.corpus_citation_path`) to the
/// provision's `source_url`. Built from the JSONL provision files published
/// in axiom-corpus (`data/corpus/provisions/**/*.jsonl`); records without a
/// `citation_path` or `source_url` are skipped. When the same citation path
/// appears more than once, the record loaded later wins, so loading dated
/// snapshot files in sorted order keeps the newest snapshot's URL —
/// deterministically, since the input order alone decides.
#[derive(Clone, Debug, Default)]
pub struct CorpusProvisionIndex {
    urls: BTreeMap<String, String>,
}

/// The subset of a corpus provision record the join reads. Every other field
/// in the JSONL record is ignored.
#[derive(Deserialize)]
struct ProvisionRecord {
    #[serde(default)]
    citation_path: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
}

impl CorpusProvisionIndex {
    /// The `source_url` recorded for `citation_path`, if any.
    pub fn source_url(&self, citation_path: &str) -> Option<&str> {
        self.urls.get(citation_path).map(String::as_str)
    }

    /// Number of citation paths with a resolvable URL.
    pub fn len(&self) -> usize {
        self.urls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    /// Add every record in a JSONL provisions document. `path` names the
    /// document for error reporting only. Blank lines are skipped; a line
    /// that is not a JSON object is an error.
    pub fn add_jsonl_str(&mut self, text: &str, path: &str) -> Result<(), CompileError> {
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ProvisionRecord = serde_json::from_str(line).map_err(|error| {
                CompileError::ParseProvisionRecord {
                    path: path.to_string(),
                    line: index + 1,
                    error,
                }
            })?;
            let (Some(citation_path), Some(source_url)) = (record.citation_path, record.source_url)
            else {
                continue;
            };
            self.urls.insert(citation_path, source_url);
        }
        Ok(())
    }

    /// Add provision records from `path`: a JSONL file, or a directory
    /// scanned recursively for `*.jsonl` files in sorted path order (so a
    /// directory of dated snapshots loads deterministically, newest last).
    #[cfg(feature = "fs")]
    pub fn add_path(&mut self, path: impl AsRef<Path>) -> Result<(), CompileError> {
        let path = path.as_ref();
        let read_error = |error: std::io::Error| CompileError::ReadProvisionsFile {
            path: path.display().to_string(),
            error,
        };
        if path.is_dir() {
            let mut files = Vec::new();
            collect_jsonl_files(path, &mut files).map_err(read_error)?;
            files.sort();
            for file in files {
                self.add_path(&file)?;
            }
            return Ok(());
        }
        let text = fs::read_to_string(path).map_err(read_error)?;
        self.add_jsonl_str(&text, &path.display().to_string())
    }

    /// Build an index from `paths`, each a JSONL file or a directory of
    /// them, loaded in the order given.
    #[cfg(feature = "fs")]
    pub fn from_paths(
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<Self, CompileError> {
        let mut index = Self::default();
        for path in paths {
            index.add_path(path)?;
        }
        Ok(index)
    }
}

#[cfg(feature = "fs")]
fn collect_jsonl_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

fn evaluation_order(program: &ProgramSpec) -> Result<Vec<String>, CompileError> {
    let mut derived_names = HashSet::new();
    for derived in &program.derived {
        if !derived_names.insert(derived.name.clone()) {
            return Err(CompileError::DuplicateDerivedRule {
                name: derived.name.clone(),
            });
        }
    }
    validate_relation_derivation_graph(program)?;
    let relation_dependencies = relation_derivation_dependencies(program, &derived_names)?;

    let mut incoming_counts = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for derived in &program.derived {
        let dependencies = derived_dependencies(derived, &relation_dependencies);
        incoming_counts.insert(derived.name.clone(), dependencies.len());

        for dependency in dependencies {
            if !derived_names.contains(&dependency) {
                return Err(CompileError::UnknownDerivedDependency {
                    derived: derived.name.clone(),
                    dependency,
                });
            }
            dependents
                .entry(dependency)
                .or_default()
                .push(derived.name.clone());
        }
    }

    for next in dependents.values_mut() {
        next.sort();
    }

    let mut ready = incoming_counts
        .iter()
        .filter_map(|(name, count)| (*count == 0).then_some(name.clone()))
        .collect::<BTreeSet<String>>();
    let mut order = Vec::with_capacity(program.derived.len());

    while let Some(name) = ready.pop_first() {
        order.push(name.clone());
        if let Some(next) = dependents.get(&name) {
            for dependent in next {
                if let Some(count) = incoming_counts.get_mut(dependent) {
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(dependent.clone());
                    }
                }
            }
        }
    }

    if order.len() != program.derived.len() {
        let cycle = incoming_counts
            .into_iter()
            .filter_map(|(name, count)| (count > 0).then_some(name))
            .collect::<Vec<String>>()
            .join(", ");
        return Err(CompileError::CyclicDependency { cycle });
    }

    Ok(order)
}

fn fast_path_metadata(program: &ProgramSpec) -> FastPathMetadata {
    let mut blockers = Vec::new();
    for derived in &program.derived {
        collect_fast_blockers_from_semantics(&derived.name, &derived.semantics, &mut blockers);
        for version in &derived.versions {
            collect_fast_blockers_from_semantics(&derived.name, &version.semantics, &mut blockers);
        }
    }

    FastPathMetadata {
        strategy: "generic_bulk".to_string(),
        compatible: blockers.is_empty(),
        blockers,
    }
}

fn collect_fast_blockers_from_semantics(
    derived_name: &str,
    semantics: &DerivedSemanticsSpec,
    blockers: &mut Vec<String>,
) {
    match semantics {
        DerivedSemanticsSpec::Scalar { expr } => {
            collect_fast_blockers_from_scalar_expr(derived_name, expr, blockers);
        }
        DerivedSemanticsSpec::Judgment { expr } => {
            collect_fast_blockers_from_judgment_expr(derived_name, expr, blockers);
        }
    }
}

fn collect_fast_blockers_from_scalar_expr(
    derived_name: &str,
    expr: &ScalarExprSpec,
    blockers: &mut Vec<String>,
) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. }
        | ScalarExprSpec::Derived { .. } => {}
        ScalarExprSpec::CountRelated { .. } => {}
        ScalarExprSpec::ParameterLookup { index, .. } => {
            collect_fast_blockers_from_scalar_expr(derived_name, index, blockers);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                collect_fast_blockers_from_scalar_expr(derived_name, item, blockers);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            collect_fast_blockers_from_scalar_expr(derived_name, left, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, right, blockers);
        }
        ScalarExprSpec::Ceil { value } | ScalarExprSpec::Floor { value } => {
            collect_fast_blockers_from_scalar_expr(derived_name, value, blockers);
        }
        ScalarExprSpec::PeriodStart | ScalarExprSpec::PeriodEnd => {
            blockers.push(format!(
                "{derived_name}: bulk fast mode does not yet support period_start / period_end; explain mode and the generic dense path do"
            ));
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            blockers.push(format!(
                "{derived_name}: bulk fast mode does not yet support date_add_days; explain mode and the generic dense path do"
            ));
            collect_fast_blockers_from_scalar_expr(derived_name, date, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, days, blockers);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            blockers.push(format!(
                "{derived_name}: bulk fast mode does not yet support days_between; explain mode and the generic dense path do"
            ));
            collect_fast_blockers_from_scalar_expr(derived_name, from, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, to, blockers);
        }
        ScalarExprSpec::SumRelated { value, .. } => {
            if matches!(value, RelatedValueRefSpec::Derived { .. }) {
                blockers.push(format!(
                    "{derived_name}: fast mode does not yet support sum_related over related derived values"
                ));
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_fast_blockers_from_judgment_expr(derived_name, condition, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, then_expr, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, else_expr, blockers);
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            blockers.push(format!(
                "{derived_name}: bulk fast mode does not support over-periods reductions; use the dense lifetime execution surface"
            ));
            collect_fast_blockers_from_scalar_expr(derived_name, value, blockers);
            if let Some(n) = n {
                collect_fast_blockers_from_scalar_expr(derived_name, n, blockers);
            }
        }
    }
}

fn collect_fast_blockers_from_judgment_expr(
    derived_name: &str,
    expr: &JudgmentExprSpec,
    blockers: &mut Vec<String>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            collect_fast_blockers_from_scalar_expr(derived_name, left, blockers);
            collect_fast_blockers_from_scalar_expr(derived_name, right, blockers);
        }
        JudgmentExprSpec::Derived { .. } | JudgmentExprSpec::RelationMember { .. } => {}
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                collect_fast_blockers_from_judgment_expr(derived_name, item, blockers);
            }
        }
        JudgmentExprSpec::Not { item } => {
            collect_fast_blockers_from_judgment_expr(derived_name, item, blockers);
        }
    }
}

fn validate_relation_derivation_graph(program: &ProgramSpec) -> Result<(), CompileError> {
    let relation_names = program
        .relations
        .iter()
        .map(|relation| relation.name.clone())
        .collect::<HashSet<String>>();
    let mut graph: HashMap<String, HashSet<String>> = HashMap::new();

    for relation in &program.relations {
        let Some(derivation) = &relation.derivation else {
            continue;
        };
        let mut dependencies = HashSet::new();
        dependencies.insert(derivation.source_relation.clone());
        collect_relation_members_from_judgment(&derivation.predicate, &mut dependencies);

        for dependency in &dependencies {
            if !relation_names.contains(dependency) {
                return Err(CompileError::UnknownRelationDependency {
                    relation: relation.name.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
        graph.insert(relation.name.clone(), dependencies);
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for relation in graph.keys() {
        detect_relation_cycle(relation, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn detect_relation_cycle(
    relation: &str,
    graph: &HashMap<String, HashSet<String>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> Result<(), CompileError> {
    if visited.contains(relation) {
        return Ok(());
    }
    if !visiting.insert(relation.to_string()) {
        let mut cycle = visiting.iter().cloned().collect::<Vec<String>>();
        cycle.sort();
        return Err(CompileError::CyclicRelationDependency {
            cycle: cycle.join(", "),
        });
    }
    if let Some(dependencies) = graph.get(relation) {
        for dependency in dependencies {
            if graph.contains_key(dependency) {
                detect_relation_cycle(dependency, graph, visiting, visited)?;
            }
        }
    }
    visiting.remove(relation);
    visited.insert(relation.to_string());
    Ok(())
}

fn relation_derivation_dependencies(
    program: &ProgramSpec,
    derived_names: &HashSet<String>,
) -> Result<HashMap<String, HashSet<String>>, CompileError> {
    let mut dependencies_by_relation = HashMap::new();
    for relation in &program.relations {
        let Some(derivation) = &relation.derivation else {
            continue;
        };
        let mut dependencies = HashSet::new();
        collect_judgment_dependencies(&derivation.predicate, &mut dependencies, &HashMap::new());
        for dependency in &dependencies {
            if !derived_names.contains(dependency) {
                return Err(CompileError::UnknownDerivedDependency {
                    derived: relation.name.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
        dependencies_by_relation.insert(relation.name.clone(), dependencies);
    }
    Ok(dependencies_by_relation)
}

fn derived_dependencies(
    derived: &crate::spec::DerivedSpec,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) -> HashSet<String> {
    let mut dependencies = HashSet::new();
    match &derived.semantics {
        DerivedSemanticsSpec::Scalar { expr } => {
            collect_scalar_dependencies(expr, &mut dependencies, relation_dependencies);
        }
        DerivedSemanticsSpec::Judgment { expr } => {
            collect_judgment_dependencies(expr, &mut dependencies, relation_dependencies);
        }
    }
    for version in &derived.versions {
        match &version.semantics {
            DerivedSemanticsSpec::Scalar { expr } => {
                collect_scalar_dependencies(expr, &mut dependencies, relation_dependencies);
            }
            DerivedSemanticsSpec::Judgment { expr } => {
                collect_judgment_dependencies(expr, &mut dependencies, relation_dependencies);
            }
        }
    }
    dependencies
}

fn collect_scalar_dependencies(
    expr: &ScalarExprSpec,
    dependencies: &mut HashSet<String>,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. } => {}
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            if let Some(relation_dependencies) = relation_dependencies.get(relation) {
                dependencies.extend(relation_dependencies.iter().cloned());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_dependencies(predicate, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::Derived { name } => {
            dependencies.insert(name.clone());
        }
        ScalarExprSpec::ParameterLookup { index, .. } => {
            collect_scalar_dependencies(index, dependencies, relation_dependencies);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                collect_scalar_dependencies(item, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            collect_scalar_dependencies(left, dependencies, relation_dependencies);
            collect_scalar_dependencies(right, dependencies, relation_dependencies);
        }
        ScalarExprSpec::Ceil { value } | ScalarExprSpec::Floor { value } => {
            collect_scalar_dependencies(value, dependencies, relation_dependencies);
        }
        ScalarExprSpec::PeriodStart | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::DateAddDays { date, days } => {
            collect_scalar_dependencies(date, dependencies, relation_dependencies);
            collect_scalar_dependencies(days, dependencies, relation_dependencies);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            collect_scalar_dependencies(from, dependencies, relation_dependencies);
            collect_scalar_dependencies(to, dependencies, relation_dependencies);
        }
        ScalarExprSpec::SumRelated {
            value,
            relation,
            where_clause,
            ..
        } => {
            if let Some(relation_dependencies) = relation_dependencies.get(relation) {
                dependencies.extend(relation_dependencies.iter().cloned());
            }
            if let RelatedValueRefSpec::Derived { name } = value {
                dependencies.insert(name.clone());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_dependencies(predicate, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_dependencies(condition, dependencies, relation_dependencies);
            collect_scalar_dependencies(then_expr, dependencies, relation_dependencies);
            collect_scalar_dependencies(else_expr, dependencies, relation_dependencies);
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            collect_scalar_dependencies(value, dependencies, relation_dependencies);
            if let Some(n) = n {
                collect_scalar_dependencies(n, dependencies, relation_dependencies);
            }
        }
    }
}

fn collect_judgment_dependencies(
    expr: &JudgmentExprSpec,
    dependencies: &mut HashSet<String>,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            collect_scalar_dependencies(left, dependencies, relation_dependencies);
            collect_scalar_dependencies(right, dependencies, relation_dependencies);
        }
        JudgmentExprSpec::Derived { name } => {
            dependencies.insert(name.clone());
        }
        JudgmentExprSpec::RelationMember { .. } => {}
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                collect_judgment_dependencies(item, dependencies, relation_dependencies);
            }
        }
        JudgmentExprSpec::Not { item } => {
            collect_judgment_dependencies(item, dependencies, relation_dependencies);
        }
    }
}

fn collect_relation_members_from_scalar(expr: &ScalarExprSpec, relations: &mut HashSet<String>) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. }
        | ScalarExprSpec::Derived { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::ParameterLookup { index, .. }
        | ScalarExprSpec::Ceil { value: index }
        | ScalarExprSpec::Floor { value: index } => {
            collect_relation_members_from_scalar(index, relations);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                collect_relation_members_from_scalar(item, relations);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            collect_relation_members_from_scalar(left, relations);
            collect_relation_members_from_scalar(right, relations);
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            collect_relation_members_from_scalar(date, relations);
            collect_relation_members_from_scalar(days, relations);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            collect_relation_members_from_scalar(from, relations);
            collect_relation_members_from_scalar(to, relations);
        }
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            relations.insert(relation.clone());
            if let Some(where_clause) = where_clause {
                collect_relation_members_from_judgment(where_clause, relations);
            }
        }
        ScalarExprSpec::SumRelated {
            relation,
            where_clause,
            ..
        } => {
            relations.insert(relation.clone());
            if let Some(where_clause) = where_clause {
                collect_relation_members_from_judgment(where_clause, relations);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_relation_members_from_judgment(condition, relations);
            collect_relation_members_from_scalar(then_expr, relations);
            collect_relation_members_from_scalar(else_expr, relations);
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            collect_relation_members_from_scalar(value, relations);
            if let Some(n) = n {
                collect_relation_members_from_scalar(n, relations);
            }
        }
    }
}

fn collect_relation_members_from_judgment(
    expr: &JudgmentExprSpec,
    relations: &mut HashSet<String>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            collect_relation_members_from_scalar(left, relations);
            collect_relation_members_from_scalar(right, relations);
        }
        JudgmentExprSpec::Derived { .. } => {}
        JudgmentExprSpec::RelationMember { relation, .. } => {
            relations.insert(relation.clone());
        }
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                collect_relation_members_from_judgment(item, relations);
            }
        }
        JudgmentExprSpec::Not { item } => {
            collect_relation_members_from_judgment(item, relations);
        }
    }
}

#[cfg(feature = "fs")]
pub fn compile_program_file_to_json(
    program_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
) -> Result<CompiledProgramArtifact, CompileError> {
    let p = program_path.as_ref();
    let artifact = CompiledProgramArtifact::from_rulespec_file(p)?;
    artifact.write_json_file(output_path)?;
    Ok(artifact)
}

pub fn compile_summary_lines(artifact: &CompiledProgramArtifact) -> BTreeMap<String, String> {
    let mut lines = BTreeMap::new();
    lines.insert(
        "artifact_format_version".to_string(),
        artifact.artifact_format_version.to_string(),
    );
    if let Some(engine_version) = &artifact.engine_version {
        lines.insert("engine_version".to_string(), engine_version.clone());
    }
    lines.insert(
        "derived_outputs".to_string(),
        artifact.program.derived.len().to_string(),
    );
    lines.insert(
        "evaluation_order".to_string(),
        artifact.metadata.evaluation_order.join(", "),
    );
    lines.insert(
        "fast_path_strategy".to_string(),
        artifact.metadata.fast_path.strategy.clone(),
    );
    lines.insert(
        "fast_path_compatible".to_string(),
        artifact.metadata.fast_path.compatible.to_string(),
    );
    if !artifact.metadata.fast_path.blockers.is_empty() {
        lines.insert(
            "fast_path_blockers".to_string(),
            artifact.metadata.fast_path.blockers.join(" | "),
        );
    }
    lines
}
