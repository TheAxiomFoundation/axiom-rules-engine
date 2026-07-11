use std::collections::{BTreeMap, HashMap, HashSet};
#[cfg(feature = "fs")]
use std::fs;
#[cfg(feature = "fs")]
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::source::ModuleSource;

use crate::spec::{
    DerivedSemanticsSpec, IndexedParameterSpec, JudgmentExprSpec, ParameterVersionSpec,
    ProgramSpec, RelatedValueRefSpec, RelationDerivationSpec, RelationSpec, ScalarExprSpec,
    ScalarValueSpec, UnitSpec,
};

/// The only content roots admitted directly below a jurisdiction directory.
pub const RULESPEC_FILESYSTEM_ROOTS: [&str; 5] = [
    "legislation",
    "policies",
    "programs",
    "regulations",
    "statutes",
];

/// Filesystem roots that contain atomic `rulespec/v1` modules.
///
/// `programs/` is deliberately absent: it contains declarative ProgramSpecs
/// consumed by `axiom-compose`, not atomic RuleSpec modules consumed here.
pub const RULESPEC_ATOMIC_ROOTS: [&str; 4] = ["legislation", "policies", "regulations", "statutes"];

#[derive(Debug, Error)]
pub enum RuleSpecError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("RuleSpec requires exact `format: rulespec/v1`")]
    MissingDiscriminator,
    #[error(
        "RuleSpec module `{path}` declares removed top-level `extends`; use `imports` for atomic dependencies and axiom-compose for program composition"
    )]
    ExtendsUnsupported { path: String },
    #[error(
        "RuleSpec module `{path}` declares removed top-level `schema`; use only exact `format: rulespec/v1`"
    )]
    SchemaDiscriminatorUnsupported { path: String },
    #[cfg(feature = "fs")]
    #[error("failed to read RuleSpec file `{path}`: {error}")]
    ReadFile { path: String, error: std::io::Error },
    #[cfg(feature = "fs")]
    #[error("invalid RuleSpec repository root configuration: {message}")]
    RepositoryRootConfiguration { message: String },
    #[cfg(feature = "fs")]
    #[error("invalid filesystem RuleSpec path `{path}`: {message}")]
    InvalidFilesystemPath { path: String, message: String },
    #[cfg(feature = "fs")]
    #[error("invalid composed RuleSpec program `{path}`: {message}")]
    InvalidComposedProgram { path: String, message: String },
    #[error(
        "atomic RuleSpec module `{path}` must not declare module.kind; `composition` is accepted only by the composed-program surface"
    )]
    ModuleKindOnAtomicSurface { path: String },
    #[error(
        "atomic RuleSpec module `{path}` declares removed module.id; canonical path/ModuleSource target is the sole module identity"
    )]
    ModuleIdUnsupported { path: String },
    #[error("RuleSpec import `{target}` in `{path}` could not be resolved")]
    UnresolvedImport { path: String, target: String },
    #[error("RuleSpec import cycle detected at `{path}`")]
    ImportCycle { path: String },
    #[error(
        "`{target}` is not a canonical RuleSpec module target of the form `<jurisdiction>:<relative path without extension>`"
    )]
    InvalidModuleTarget { target: String },
    #[error("module source has no module for `{target}`")]
    ModuleNotFound { target: String },
    #[error(transparent)]
    Source(#[from] crate::source::SourceError),
    #[error("failed to parse RuleSpec formula: {0}")]
    Formula(#[from] crate::formula::FormulaError),
    #[error("RuleSpec rule `{name}` uses unsupported kind `{kind}`")]
    UnsupportedRuleKind { name: String, kind: String },
    #[error("RuleSpec rule `{name}` must declare `kind`")]
    MissingRuleKind { name: String },
    #[error(
        "RuleSpec rule `{name}` declares `rounding` but has kind `{kind}`; output rounding applies only to `derived` currency rules"
    )]
    RoundingOnNonDerivedRule { name: String, kind: String },
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
    #[error(
        "RuleSpec derived relation `{name}` must declare exactly one membership formula version"
    )]
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
    #[error(
        "RuleSpec source relation `{name}` sets target `{target}`, but the target does not resolve to a parameter in the merged program"
    )]
    SourceRelationSetTargetNotParameter { name: String, target: String },
    #[error(
        "RuleSpec source relation `{name}` sets value `{value}`, but the value does not resolve to a parameter in the merged program"
    )]
    SourceRelationSetValueNotParameter { name: String, value: String },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` to `{target}` because their indexed_by values differ"
    )]
    SourceRelationSetIndexedByMismatch {
        name: String,
        target: String,
        value: String,
    },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` to `{target}` because their units differ"
    )]
    SourceRelationSetUnitMismatch {
        name: String,
        target: String,
        value: String,
    },
    #[error(
        "RuleSpec source relation `{name}` sets target `{target}`, but the target does not resolve to an executable parameter or derived rule in the merged program"
    )]
    SourceRelationSetTargetNotExecutable { name: String, target: String },
    #[error(
        "RuleSpec source relation `{name}` sets value `{value}`, but the value does not resolve to an executable parameter or derived rule in the merged program"
    )]
    SourceRelationSetValueNotExecutable { name: String, value: String },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` ({value_kind}) to `{target}` ({target_kind})"
    )]
    SourceRelationSetKindMismatch {
        name: String,
        target: String,
        value: String,
        target_kind: String,
        value_kind: String,
    },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` to `{target}` because their derived entities differ"
    )]
    SourceRelationSetEntityMismatch {
        name: String,
        target: String,
        value: String,
    },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` to `{target}` because their derived dtypes differ"
    )]
    SourceRelationSetDTypeMismatch {
        name: String,
        target: String,
        value: String,
    },
    #[error(
        "RuleSpec source relation `{name}` cannot bind `{value}` to `{target}` because their derived periods differ"
    )]
    SourceRelationSetPeriodMismatch {
        name: String,
        target: String,
        value: String,
    },
    #[error("RuleSpec relation `{name}` is declared with conflicting arities {existing} and {new}")]
    RelationArityConflict {
        name: String,
        existing: usize,
        new: usize,
    },
    #[error(
        "RuleSpec module `{path}` declares source_verification.source_sha256 `{value}`, which is not a 64-character hexadecimal SHA-256 digest"
    )]
    InvalidSourceSha256 { path: String, value: String },
    #[error("RuleSpec module `{path}` declares non-canonical corpus_citation_path `{value}`")]
    InvalidCorpusCitationPath { path: String, value: String },
    #[error(
        "RuleSpec module `{path}` declares removed plural `corpus_citation_paths`; every source/proof node must declare exactly one singular `corpus_citation_path`"
    )]
    PluralCorpusCitationPaths { path: String },
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RulesDocument {
    #[serde(default)]
    pub format: Option<String>,
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

/// Module-level metadata block of a RuleSpec document. Every field is
/// optional; the whole block is descriptive and inert — it never affects
/// lowering output names, compilation, or execution. The lowered
/// [`ProgramSpec`] keeps the (merged) root module's metadata so tooling
/// reading a loaded module or a compiled artifact can see it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModuleMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_verification: Option<SourceVerification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_provenance: Option<EncodingProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation: Vec<ValidationRecord>,
}

impl ModuleMetadata {
    /// Validate the shape of declared metadata. `path` names the module for
    /// error reporting: the file path for filesystem loads, the canonical
    /// target for source-backed loads, or the module id / `<memory>` for
    /// in-memory strings.
    fn validate(&self, path: &str) -> Result<(), RuleSpecError> {
        if let Some(verification) = self.source_verification.as_ref()
            && !is_canonical_corpus_citation_path(&verification.corpus_citation_path)
        {
            return Err(RuleSpecError::InvalidCorpusCitationPath {
                path: path.to_string(),
                value: verification.corpus_citation_path.clone(),
            });
        }
        if let Some(sha) = self
            .source_verification
            .as_ref()
            .and_then(|verification| verification.source_sha256.as_deref())
            && (sha.len() != 64 || !sha.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            return Err(RuleSpecError::InvalidSourceSha256 {
                path: path.to_string(),
                value: sha.to_string(),
            });
        }
        Ok(())
    }
}

/// Grounding of a module in legal source text. `corpus_citation_path`
/// addresses the provision in the Axiom corpus; `source_sha256` pins the
/// SHA-256 hex digest of the exact provision text the module was encoded
/// from, so tooling can detect mechanically when the published source has
/// changed out from under the encoding. The mapping is exact: the singular
/// citation path is required and unknown/plural fields are rejected.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceVerification {
    pub corpus_citation_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
}

/// Who and what produced the encoding: tool (`encoder`, for example
/// `axiom-encode/0.2.645`), `model`, `run_id`, and human `reviewed_by`.
/// All fields optional; unknown subfields are rejected so typos do not
/// silently pass for provenance.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EncodingProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
}

/// One oracle-validation result for the module: which oracle, whether the
/// encoding currently matches it, and when it last ran.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ValidationRecord {
    pub oracle: String,
    pub status: ValidationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<NaiveDate>,
}

/// Outcome of an oracle validation run. Any other status string is
/// rejected at parse time.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Matches,
    Mismatches,
    Pending,
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

impl RuleKind {
    /// The declared kind string, for error messages.
    fn as_str(&self) -> &str {
        match self {
            Self::Parameter => "parameter",
            Self::Derived => "derived",
            Self::DataRelation => "data_relation",
            Self::DerivedRelation => "derived_relation",
            Self::SourceRelation => "source_relation",
            Self::Unsupported(kind) => kind,
        }
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
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub entity: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_like")]
    pub member_relation: Option<String>,
    #[serde(default)]
    pub slot_entities: Vec<String>,
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
    /// `source_verification.corpus_citation_path` of the module the rule was
    /// declared in, stamped at load time alongside `origin_target` so the
    /// module-level join key survives import merging.
    #[serde(skip)]
    pub origin_citation_path: Option<String>,
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
    /// Opt-in output-rounding mode for a `derived` currency rule. Lowered onto
    /// the resulting [`crate::spec::DerivedSpec`]; `to_program` resolves it
    /// against the rule's currency unit and rejects it on a non-currency unit.
    #[serde(default)]
    pub rounding: Option<crate::spec::RoundingModeSpec>,
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
    mapping
        .get(serde_yaml::Value::String("format".to_string()))
        .and_then(serde_yaml::Value::as_str)
        == Some("rulespec/v1")
}

pub fn has_top_level_rules_key(source: &str) -> bool {
    has_top_level_key(source, "rules")
}

fn has_top_level_key(source: &str, key: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(source) else {
        return false;
    };
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    mapping.contains_key(serde_yaml::Value::String(key.to_string()))
}

fn reject_removed_extends(source: &str, path: &str) -> Result<(), RuleSpecError> {
    if has_top_level_key(source, "extends") {
        return Err(RuleSpecError::ExtendsUnsupported {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn reject_removed_schema_discriminator(source: &str, path: &str) -> Result<(), RuleSpecError> {
    if has_top_level_key(source, "schema") {
        return Err(RuleSpecError::SchemaDiscriminatorUnsupported {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn reject_atomic_metadata_declarations(source: &str, path: &str) -> Result<(), RuleSpecError> {
    let value: serde_yaml::Value = serde_yaml::from_str(source)?;
    let Some(module) = value
        .as_mapping()
        .and_then(|mapping| mapping.get(serde_yaml::Value::String("module".to_string())))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Ok(());
    };
    if module.contains_key(serde_yaml::Value::String("kind".to_string())) {
        return Err(RuleSpecError::ModuleKindOnAtomicSurface {
            path: path.to_string(),
        });
    }
    if module.contains_key(serde_yaml::Value::String("id".to_string())) {
        return Err(RuleSpecError::ModuleIdUnsupported {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn validate_recursive_corpus_contract(source: &str, path: &str) -> Result<(), RuleSpecError> {
    let value: serde_yaml::Value = serde_yaml::from_str(source)?;
    validate_recursive_corpus_value(&value, path)
}

fn validate_recursive_corpus_value(
    value: &serde_yaml::Value,
    path: &str,
) -> Result<(), RuleSpecError> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            for (key, nested) in mapping {
                if key.as_str() == Some("corpus_citation_paths") {
                    return Err(RuleSpecError::PluralCorpusCitationPaths {
                        path: path.to_string(),
                    });
                }
                if key.as_str() == Some("corpus_citation_path") {
                    let Some(citation_path) = nested.as_str() else {
                        return Err(RuleSpecError::InvalidCorpusCitationPath {
                            path: path.to_string(),
                            value: format!("{nested:?}"),
                        });
                    };
                    if !is_canonical_corpus_citation_path(citation_path) {
                        return Err(RuleSpecError::InvalidCorpusCitationPath {
                            path: path.to_string(),
                            value: citation_path.to_string(),
                        });
                    }
                }
                if key.as_str() == Some("source_sha256") {
                    let Some(digest) = nested.as_str() else {
                        return Err(RuleSpecError::InvalidSourceSha256 {
                            path: path.to_string(),
                            value: format!("{nested:?}"),
                        });
                    };
                    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(RuleSpecError::InvalidSourceSha256 {
                            path: path.to_string(),
                            value: digest.to_string(),
                        });
                    }
                }
                validate_recursive_corpus_value(nested, path)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for nested in sequence {
                validate_recursive_corpus_value(nested, path)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn is_canonical_corpus_citation_path(value: &str) -> bool {
    if value.is_empty() || value != value.trim() || value.contains(['\\', '#', '"', '\'']) {
        return false;
    }
    let segments = value.split('/').collect::<Vec<_>>();
    if segments.len() < 3
        || !is_canonical_corpus_jurisdiction(segments[0])
        || !is_canonical_corpus_document_class(segments[1])
    {
        return false;
    }
    segments[2..].iter().all(|segment| {
        *segment == segment.trim()
            && segment
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
            && segment.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '-' | ':' | '\u{2013}')
            })
    })
}

fn is_canonical_corpus_document_class(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_canonical_corpus_jurisdiction(value: &str) -> bool {
    let mut parts = value.split('-');
    let country = parts.next().unwrap_or_default();
    if !(2..=3).contains(&country.len()) || !country.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return false;
    }
    parts.all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

pub fn lower_rulespec_str(source: &str) -> Result<ProgramSpec, RuleSpecError> {
    if !looks_like_rulespec_yaml(source) {
        return Err(RuleSpecError::MissingDiscriminator);
    }
    reject_removed_extends(source, "<memory>")?;
    reject_removed_schema_discriminator(source, "<memory>")?;
    reject_atomic_metadata_declarations(source, "<memory>")?;
    validate_recursive_corpus_contract(source, "<memory>")?;
    let mut document: RulesDocument = serde_yaml::from_str(source)?;
    document.validate_atomic_module_metadata("<memory>")?;
    document.assign_origin_target(None);
    document.to_program_spec()
}

#[cfg(feature = "fs")]
pub fn load_rulespec_file(
    path: impl AsRef<Path>,
    roots: &CanonicalRuleSpecRoots,
) -> Result<ProgramSpec, RuleSpecError> {
    let target = roots.target_for_path(path.as_ref())?;
    let source = crate::source::FsModuleSource::from_validated_roots(roots.clone());
    load_rulespec_with_source(&target, &source)
}

/// Load an ephemeral RuleSpec program emitted by `axiom-compose`.
///
/// Unlike an atomic module, the root document is deliberately originless and
/// may live outside the RuleSpec checkout. It must be an exact composition
/// document and every dependency directive must already be a canonical atomic
/// target resolved exclusively through `roots`.
#[cfg(feature = "fs")]
pub fn load_composed_rulespec_file(
    path: impl AsRef<Path>,
    roots: &CanonicalRuleSpecRoots,
) -> Result<ProgramSpec, RuleSpecError> {
    let path = path.as_ref();
    validate_exact_regular_yaml(path)?;
    if roots.contains_path(path) {
        return Err(invalid_composed_program(
            path,
            "composed output must be outside every canonical RuleSpec checkout",
        ));
    }
    let source = fs::read_to_string(path).map_err(|error| RuleSpecError::ReadFile {
        path: path.display().to_string(),
        error,
    })?;
    validate_composed_program_discriminator(path, &source)?;
    validate_recursive_corpus_contract(&source, &path.display().to_string())?;

    let mut document: RulesDocument = serde_yaml::from_str(&source)?;
    document.validate_module_metadata(&path.display().to_string())?;
    document.assign_origin_target(None);

    let module_source = crate::source::FsModuleSource::from_validated_roots(roots.clone());
    let mut context = SourceLoadContext::default();
    let mut combined = RulesDocument::default();

    for import in &document.imports {
        let target = canonical_composed_dependency(path, import, "import")?;
        let imported = load_rulespec_document_from_source(&target, &module_source, &mut context)?;
        combined = merge_rules_documents(combined, imported);
    }
    combined = merge_rules_documents(combined, document.without_imports());
    combined.to_program_spec()
}

/// Load and lower the module at `root_target` (canonical form, for example
/// `us:statutes/7/2015/e`), resolving every import through a host-supplied
/// [`ModuleSource`]. No filesystem or environment access happens here: the
/// core treats the program as a pure function over (modules, dataset), and
/// where module text comes from is the host's concern.
///
/// Exact absolute imports are validated with [`resolve_import_target`], then
/// fetched from `source`. Module identity, deduplication, cycle detection, and
/// durable rule ids all use the canonical target.
pub fn load_rulespec_with_source(
    root_target: &str,
    source: &dyn ModuleSource,
) -> Result<ProgramSpec, RuleSpecError> {
    let root_target = validate_module_target(root_target)?;
    let mut context = SourceLoadContext::default();
    let document = load_rulespec_document_from_source(&root_target, source, &mut context)?;
    document.to_program_spec()
}

#[derive(Default)]
struct SourceLoadContext {
    stack: Vec<String>,
    loaded: HashSet<String>,
}

fn load_rulespec_document_from_source(
    target: &str,
    source: &dyn ModuleSource,
    context: &mut SourceLoadContext,
) -> Result<RulesDocument, RuleSpecError> {
    if context.stack.iter().any(|loading| loading == target) {
        return Err(RuleSpecError::ImportCycle {
            path: target.to_string(),
        });
    }
    if context.loaded.contains(target) {
        return Ok(RulesDocument::default());
    }
    let text = source
        .load(target)?
        .ok_or_else(|| RuleSpecError::ModuleNotFound {
            target: target.to_string(),
        })?;
    if !looks_like_rulespec_yaml(&text) {
        return Err(RuleSpecError::MissingDiscriminator);
    }
    reject_removed_extends(&text, target)?;
    reject_removed_schema_discriminator(&text, target)?;
    reject_atomic_metadata_declarations(&text, target)?;
    validate_recursive_corpus_contract(&text, target)?;
    context.stack.push(target.to_string());
    let mut document: RulesDocument = serde_yaml::from_str(&text)?;
    document.validate_atomic_module_metadata(target)?;
    document.assign_origin_target(Some(target.to_string()));
    let mut combined = RulesDocument::default();

    for import in &document.imports {
        let import_target = resolve_import_target(target, import)?;
        let imported = load_rulespec_document_from_source(&import_target, source, context)?;
        combined = merge_rules_documents(combined, imported);
    }

    combined = merge_rules_documents(combined, document.without_imports());
    context.loaded.insert(target.to_string());
    context.stack.pop();
    Ok(combined)
}

/// Validate an exact canonical atomic module target.
///
/// Canonical targets have the form `<jurisdiction>:<atomic-root>/<path>` with
/// no extension, fragment, whitespace, quotes, backslashes, empty/dot
/// segments, or redundant separators. Its successful return is byte-for-byte
/// equal to `target`.
pub fn validate_module_target(target: &str) -> Result<String, RuleSpecError> {
    let invalid = || RuleSpecError::InvalidModuleTarget {
        target: target.to_string(),
    };
    if target.is_empty()
        || target.chars().any(char::is_whitespace)
        || target.contains(['"', '\'', '\\', '#'])
    {
        return Err(invalid());
    }
    let Some((prefix, relative)) = target.split_once(':') else {
        return Err(invalid());
    };
    if !is_canonical_repo_prefix(prefix) || relative.contains(':') {
        return Err(invalid());
    }
    let segments = relative.split('/').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            segment.is_empty()
                || matches!(*segment, "." | "..")
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~')
                })
        })
        || !RULESPEC_ATOMIC_ROOTS.contains(&segments[0])
    {
        return Err(invalid());
    }
    let filename = segments.last().expect("validated target has path segments");
    let filename_lower = filename.to_ascii_lowercase();
    if filename_lower.ends_with(".yaml")
        || filename_lower.ends_with(".yml")
        || filename_lower.ends_with(".test")
    {
        return Err(invalid());
    }
    Ok(target.to_string())
}

/// Validate an exact absolute atomic import and return its module target.
///
/// Imports may append one exact `#fragment` for symbol-level dependency
/// resolution. The fragment is validated and removed only for module lookup.
/// Relative paths and every compatibility spelling are rejected.
pub fn resolve_import_target(importer_target: &str, import: &str) -> Result<String, RuleSpecError> {
    let unresolved = || RuleSpecError::UnresolvedImport {
        path: importer_target.to_string(),
        target: import.to_string(),
    };
    validate_module_target(importer_target)?;
    let mut parts = import.split('#');
    let module_target = parts.next().expect("split always yields one part");
    if let Some(fragment) = parts.next()
        && (fragment.is_empty()
            || !fragment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    {
        return Err(unresolved());
    }
    if parts.next().is_some() {
        return Err(unresolved());
    }
    validate_module_target(module_target).map_err(|_| unresolved())
}

fn merge_rules_documents(mut base: RulesDocument, extension: RulesDocument) -> RulesDocument {
    if extension.format.is_some() {
        base.format = extension.format;
    }
    if extension.module.is_some() {
        base.module = extension.module;
    }
    base.units.extend(extension.units);
    base.relations.extend(extension.relations);
    base.rules.extend(extension.rules);
    base
}

pub(crate) fn is_canonical_repo_prefix(prefix: &str) -> bool {
    let mut parts = prefix.split('-');
    let Some(country) = parts.next() else {
        return false;
    };
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return false;
    }
    parts.all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
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

#[cfg(feature = "fs")]
#[derive(Clone, Debug)]
struct CanonicalCountryRoot {
    path: PathBuf,
    country: String,
}

/// Validated, explicit country-monorepo roots for filesystem module loading.
///
/// Every root is an absolute, real, unaliased directory named exactly
/// `rulespec-<two-letter-country>`. There is no environment, cwd, ancestor,
/// sibling-checkout, or legacy standalone-repository fallback.
///
/// Roots are a deterministic authority boundary, not a sandbox against a
/// concurrently hostile filesystem. Callers must keep them trusted and
/// quiescent for construction and compilation; validation followed by reads
/// cannot exclude TOCTOU replacement or hard-link mutation.
#[cfg(feature = "fs")]
#[derive(Clone, Debug)]
pub struct CanonicalRuleSpecRoots {
    roots: Vec<CanonicalCountryRoot>,
}

#[cfg(feature = "fs")]
impl CanonicalRuleSpecRoots {
    pub fn new<I, P>(roots: I) -> Result<Self, RuleSpecError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let raw_roots = roots
            .into_iter()
            .map(|root| root.as_ref().to_path_buf())
            .collect::<Vec<_>>();
        if raw_roots.is_empty() {
            return Err(repository_root_error(
                "at least one explicit rulespec-<country> root is required",
            ));
        }

        let mut validated = Vec::with_capacity(raw_roots.len());
        let mut seen_paths = HashSet::new();
        let mut seen_countries = HashSet::new();
        for root in raw_roots {
            let country = validate_country_root(&root)?;
            if !seen_paths.insert(root.clone()) {
                return Err(repository_root_error(format!(
                    "duplicate root `{}`",
                    root.display()
                )));
            }
            if let Some(existing) = validated.iter().find(|existing: &&CanonicalCountryRoot| {
                root.starts_with(&existing.path) || existing.path.starts_with(&root)
            }) {
                return Err(repository_root_error(format!(
                    "overlapping roots `{}` and `{}` are forbidden",
                    existing.path.display(),
                    root.display()
                )));
            }
            if !seen_countries.insert(country.clone()) {
                return Err(repository_root_error(format!(
                    "duplicate country `{country}`; configure exactly one rulespec-{country} root"
                )));
            }
            validated.push(CanonicalCountryRoot {
                path: root,
                country,
            });
        }
        Ok(Self { roots: validated })
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.roots.iter().map(|root| root.path.as_path())
    }

    fn contains_path(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(&root.path))
    }

    pub fn target_for_path(&self, path: &Path) -> Result<String, RuleSpecError> {
        validate_exact_regular_yaml(path)?;
        for root in &self.roots {
            let Ok(relative) = path.strip_prefix(&root.path) else {
                continue;
            };
            let components = relative
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid_path(path, "path components must be UTF-8"))?;
            if components.len() < 3 {
                return Err(invalid_path(
                    path,
                    "module must be below <jurisdiction>/<atomic-root>/",
                ));
            }
            let jurisdiction = components[0];
            if !jurisdiction_matches_country(jurisdiction, &root.country) {
                return Err(invalid_path(
                    path,
                    format!(
                        "jurisdiction `{jurisdiction}` does not match country `{}`",
                        root.country
                    ),
                ));
            }
            if !RULESPEC_ATOMIC_ROOTS.contains(&components[1]) {
                return Err(invalid_path(
                    path,
                    format!(
                        "`{}` is not an atomic RuleSpec root; expected one of {}",
                        components[1],
                        RULESPEC_ATOMIC_ROOTS.join(", ")
                    ),
                ));
            }
            let relative = relative.with_extension("");
            let relative = relative
                .to_str()
                .ok_or_else(|| invalid_path(path, "path components must be UTF-8"))?
                .replace('\\', "/");
            let target = format!(
                "{jurisdiction}:{}",
                relative_without_jurisdiction(&relative)
            );
            let normalized = validate_module_target(&target).map_err(|_| {
                invalid_path(
                    path,
                    "path does not map injectively to an exact canonical module target",
                )
            })?;
            if normalized != target {
                return Err(invalid_path(
                    path,
                    format!(
                        "path maps to non-canonical target `{target}` instead of exact `{normalized}`"
                    ),
                ));
            }
            return Ok(target);
        }
        Err(invalid_path(
            path,
            "path is outside every explicitly configured RuleSpec root",
        ))
    }

    pub(crate) fn read_target(&self, target: &str) -> Result<Option<String>, RuleSpecError> {
        let target = validate_module_target(target)?;
        let (jurisdiction, relative) = target
            .split_once(':')
            .expect("normalized targets contain a jurisdiction");
        let country = jurisdiction
            .split_once('-')
            .map(|(country, _)| country)
            .unwrap_or(jurisdiction);
        let Some(root) = self.roots.iter().find(|root| root.country == country) else {
            return Ok(None);
        };
        let path = root
            .path
            .join(jurisdiction)
            .join(format!("{relative}.yaml"));
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(RuleSpecError::ReadFile {
                    path: path.display().to_string(),
                    error,
                });
            }
        }
        validate_exact_regular_yaml(&path)?;
        fs::read_to_string(&path)
            .map(Some)
            .map_err(|error| RuleSpecError::ReadFile {
                path: path.display().to_string(),
                error,
            })
    }
}

#[cfg(feature = "fs")]
fn validate_country_root(root: &Path) -> Result<String, RuleSpecError> {
    if !root.is_absolute() {
        return Err(repository_root_error(format!(
            "root `{}` must be absolute",
            root.display()
        )));
    }
    validate_exact_component_spelling(root).map_err(repository_root_error)?;
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        repository_root_error(format!("cannot inspect root `{}`: {error}", root.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(repository_root_error(format!(
            "root `{}` must be a real directory, not a symlink or special path",
            root.display()
        )));
    }
    let resolved = root.canonicalize().map_err(|error| {
        repository_root_error(format!("cannot resolve root `{}`: {error}", root.display()))
    })?;
    if resolved.as_os_str() != root.as_os_str() {
        return Err(repository_root_error(format!(
            "root `{}` is an alias; pass its exact canonical path `{}`",
            root.display(),
            resolved.display()
        )));
    }
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| repository_root_error("root name must be UTF-8"))?;
    let country = name.strip_prefix("rulespec-").ok_or_else(|| {
        repository_root_error(format!(
            "root `{}` must be named exactly rulespec-<country>",
            root.display()
        ))
    })?;
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return Err(repository_root_error(format!(
            "root `{}` must use a two-letter lowercase country code",
            root.display()
        )));
    }
    for content_root in RULESPEC_FILESYSTEM_ROOTS {
        if root.join(content_root).exists() {
            return Err(repository_root_error(format!(
                "root-level `{content_root}/` is forbidden in `{}`; content belongs below a direct jurisdiction directory",
                root.display()
            )));
        }
    }

    let mut jurisdiction_count = 0usize;
    let mut content_root_count = 0usize;
    for entry in fs::read_dir(root).map_err(|error| {
        repository_root_error(format!("cannot read root `{}`: {error}", root.display()))
    })? {
        let entry = entry.map_err(|error| {
            repository_root_error(format!("cannot read root `{}`: {error}", root.display()))
        })?;
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                repository_root_error(format!("non-UTF-8 path `{}` is forbidden", path.display()))
            })?;
        let lowercase_name = name.to_ascii_lowercase();
        if name != lowercase_name
            && (RULESPEC_FILESYSTEM_ROOTS.contains(&lowercase_name.as_str())
                || is_canonical_repo_prefix(&lowercase_name))
        {
            return Err(repository_root_error(format!(
                "case-aliased reserved path `{name}` is forbidden in `{}`",
                root.display()
            )));
        }
        let file_type = entry.file_type().map_err(|error| {
            repository_root_error(format!("cannot inspect `{}`: {error}", path.display()))
        })?;
        if file_type.is_symlink() {
            return Err(repository_root_error(format!(
                "symlink `{}` is forbidden",
                path.display()
            )));
        }
        if !file_type.is_dir() {
            continue;
        }
        if !is_canonical_repo_prefix(&name) {
            continue;
        }
        if !jurisdiction_matches_country(&name, country) {
            return Err(repository_root_error(format!(
                "jurisdiction `{name}` does not match country `{country}` in `{}`",
                root.display()
            )));
        }
        jurisdiction_count += 1;
        for content_entry in fs::read_dir(&path).map_err(|error| {
            repository_root_error(format!("cannot read `{}`: {error}", path.display()))
        })? {
            let content_entry = content_entry.map_err(|error| {
                repository_root_error(format!("cannot read `{}`: {error}", path.display()))
            })?;
            let content_name = content_entry.file_name();
            let content_name = content_name.to_str().ok_or_else(|| {
                repository_root_error(format!(
                    "non-UTF-8 path `{}` is forbidden",
                    content_entry.path().display()
                ))
            })?;
            let lowercase_content_name = content_name.to_ascii_lowercase();
            if content_name != lowercase_content_name
                && RULESPEC_FILESYSTEM_ROOTS.contains(&lowercase_content_name.as_str())
            {
                return Err(repository_root_error(format!(
                    "case-aliased content root `{}` is forbidden",
                    content_entry.path().display()
                )));
            }
        }
        for content_root in RULESPEC_FILESYSTEM_ROOTS {
            let content_path = path.join(content_root);
            let content_metadata = match fs::symlink_metadata(&content_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(repository_root_error(format!(
                        "cannot inspect `{}`: {error}",
                        content_path.display()
                    )));
                }
            };
            if content_metadata.file_type().is_symlink() || !content_metadata.is_dir() {
                return Err(repository_root_error(format!(
                    "content root `{}` must be a real directory",
                    content_path.display()
                )));
            }
            validate_exact_component_spelling(&content_path).map_err(repository_root_error)?;
            content_root_count += 1;
            validate_content_tree(&content_path)?;
        }
    }
    if jurisdiction_count == 0 || content_root_count == 0 {
        return Err(repository_root_error(format!(
            "root `{}` is empty: it must contain a direct matching jurisdiction with at least one canonical content root",
            root.display()
        )));
    }
    Ok(country.to_string())
}

#[cfg(feature = "fs")]
fn validate_content_tree(root: &Path) -> Result<(), RuleSpecError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            repository_root_error(format!("cannot read `{}`: {error}", directory.display()))
        })? {
            let entry = entry.map_err(|error| {
                repository_root_error(format!("cannot read `{}`: {error}", directory.display()))
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                repository_root_error(format!(
                    "non-UTF-8 path component `{}` is forbidden",
                    path.display()
                ))
            })?;
            if name.chars().any(char::is_whitespace)
                || name
                    .chars()
                    .any(|ch| matches!(ch, '#' | ':' | '"' | '\'' | '\\'))
                || !name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~')
                })
            {
                return Err(repository_root_error(format!(
                    "non-canonical path component `{name}` in `{}`",
                    path.display()
                )));
            }
            let file_type = entry.file_type().map_err(|error| {
                repository_root_error(format!("cannot inspect `{}`: {error}", path.display()))
            })?;
            if file_type.is_symlink() {
                return Err(repository_root_error(format!(
                    "symlink `{}` is forbidden",
                    path.display()
                )));
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if !file_type.is_file() {
                return Err(repository_root_error(format!(
                    "special path `{}` is forbidden",
                    path.display()
                )));
            } else if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
                let extension_lower = extension.to_ascii_lowercase();
                if matches!(extension_lower.as_str(), "yaml" | "yml") && extension != "yaml" {
                    return Err(repository_root_error(format!(
                        "YAML file `{}` must use the exact `.yaml` extension",
                        path.display()
                    )));
                }
                if extension == "yaml" && has_yaml_like_stem(&path) {
                    return Err(repository_root_error(format!(
                        "YAML file `{}` has an ambiguous YAML-like double extension",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn validate_exact_regular_yaml(path: &Path) -> Result<(), RuleSpecError> {
    if !path.is_absolute() {
        return Err(invalid_path(path, "path must be absolute"));
    }
    validate_exact_component_spelling(path).map_err(|message| invalid_path(path, message))?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("yaml") {
        return Err(invalid_path(
            path,
            "module files must use the `.yaml` extension",
        ));
    }
    if has_yaml_like_stem(path) {
        return Err(invalid_path(
            path,
            "module files must not use a YAML-like double extension",
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_path(path, "module filename must be valid UTF-8"))?;
    if file_name.chars().any(char::is_whitespace)
        || file_name
            .chars()
            .any(|ch| matches!(ch, '#' | ':' | '"' | '\'' | '\\'))
    {
        return Err(invalid_path(path, "module filename is not canonical"));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| invalid_path(path, format!("cannot inspect path: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_path(
            path,
            "module must be a real regular file, not a symlink or special path",
        ));
    }
    let resolved = path
        .canonicalize()
        .map_err(|error| invalid_path(path, format!("cannot resolve path: {error}")))?;
    if resolved.as_os_str() != path.as_os_str() {
        return Err(invalid_path(
            path,
            format!(
                "path is an alias; pass its exact canonical path `{}`",
                resolved.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn has_yaml_like_stem(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| {
            let stem = stem.to_ascii_lowercase();
            stem.ends_with(".yaml") || stem.ends_with(".yml")
        })
}

#[cfg(feature = "fs")]
fn validate_exact_component_spelling(path: &Path) -> Result<(), String> {
    use std::path::Component;

    let mut cursor = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => cursor.push(component.as_os_str()),
            Component::Normal(expected) => {
                let entries = fs::read_dir(&cursor).map_err(|error| {
                    format!(
                        "cannot inspect parent `{}` for exact path spelling: {error}",
                        cursor.display()
                    )
                })?;
                let mut exact = false;
                for entry in entries {
                    let entry = entry.map_err(|error| {
                        format!(
                            "cannot inspect parent `{}` for exact path spelling: {error}",
                            cursor.display()
                        )
                    })?;
                    if entry.file_name() == expected {
                        exact = true;
                        break;
                    }
                }
                if !exact {
                    return Err(format!(
                        "path component `{}` is a spelling alias under `{}`",
                        expected.to_string_lossy(),
                        cursor.display()
                    ));
                }
                cursor.push(expected);
            }
            Component::CurDir | Component::ParentDir => {
                return Err(format!(
                    "path `{}` contains a dot alias component",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn jurisdiction_matches_country(jurisdiction: &str, country: &str) -> bool {
    jurisdiction == country
        || jurisdiction
            .strip_prefix(country)
            .is_some_and(|suffix| suffix.starts_with('-') && is_canonical_repo_prefix(jurisdiction))
}

#[cfg(feature = "fs")]
fn relative_without_jurisdiction(relative: &str) -> &str {
    relative
        .split_once('/')
        .map(|(_, rest)| rest)
        .expect("validated paths include a jurisdiction and content root")
}

#[cfg(feature = "fs")]
fn repository_root_error(message: impl Into<String>) -> RuleSpecError {
    RuleSpecError::RepositoryRootConfiguration {
        message: message.into(),
    }
}

#[cfg(feature = "fs")]
fn invalid_path(path: &Path, message: impl Into<String>) -> RuleSpecError {
    RuleSpecError::InvalidFilesystemPath {
        path: path.display().to_string(),
        message: message.into(),
    }
}

#[cfg(feature = "fs")]
fn invalid_composed_program(path: &Path, message: impl Into<String>) -> RuleSpecError {
    RuleSpecError::InvalidComposedProgram {
        path: path.display().to_string(),
        message: message.into(),
    }
}

#[cfg(feature = "fs")]
fn validate_composed_program_discriminator(path: &Path, source: &str) -> Result<(), RuleSpecError> {
    let value: serde_yaml::Value = serde_yaml::from_str(source)?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| invalid_composed_program(path, "root must be a mapping"))?;
    let field = |name: &str| mapping.get(serde_yaml::Value::String(name.to_string()));
    if field("format").and_then(serde_yaml::Value::as_str) != Some("rulespec/v1") {
        return Err(invalid_composed_program(
            path,
            "format must be exactly `rulespec/v1`",
        ));
    }
    if field("extends").is_some() {
        return Err(invalid_composed_program(
            path,
            "top-level `extends` was removed; use `imports` for atomic dependencies",
        ));
    }
    if field("schema").is_some() {
        return Err(invalid_composed_program(
            path,
            "top-level `schema` was removed; use only exact `format: rulespec/v1`",
        ));
    }
    let module = field("module")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| invalid_composed_program(path, "module must be a mapping"))?;
    let module_field = |name: &str| module.get(serde_yaml::Value::String(name.to_string()));
    if module_field("kind").and_then(serde_yaml::Value::as_str) != Some("composition") {
        return Err(invalid_composed_program(
            path,
            "module.kind must be exactly `composition`",
        ));
    }
    if module_field("id").is_some() {
        return Err(invalid_composed_program(
            path,
            "module.id is forbidden because composed root rules are originless",
        ));
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn canonical_composed_dependency(
    path: &Path,
    dependency: &str,
    field: &str,
) -> Result<String, RuleSpecError> {
    let normalized = validate_module_target(dependency).map_err(|_| {
        invalid_composed_program(
            path,
            format!(
                "{field} `{dependency}` must be a canonical atomic module target; relative and programs/ targets are forbidden"
            ),
        )
    })?;
    if dependency != normalized {
        return Err(invalid_composed_program(
            path,
            format!("{field} `{dependency}` is not exact canonical form `{normalized}`"),
        ));
    }
    Ok(normalized)
}

impl RulesDocument {
    fn validate_module_metadata(&self, path: &str) -> Result<(), RuleSpecError> {
        match &self.module {
            Some(module) => module.validate(path),
            None => Ok(()),
        }
    }

    fn validate_atomic_module_metadata(&self, path: &str) -> Result<(), RuleSpecError> {
        self.validate_module_metadata(path)?;
        let Some(module) = self.module.as_ref() else {
            return Ok(());
        };
        if module.kind.is_some() {
            return Err(RuleSpecError::ModuleKindOnAtomicSurface {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    fn assign_origin_target(&mut self, origin_target: Option<String>) {
        // The module-level corpus citation path rides onto every rule here,
        // because `merge_rules_documents` keeps only the root module's
        // metadata: without stamping, an imported module's join key would be
        // lost with its `module:` block.
        let citation_path = self
            .module
            .as_ref()
            .and_then(|module| module.source_verification.as_ref())
            .map(|verification| verification.corpus_citation_path.clone());
        for rule in &mut self.rules {
            rule.origin_target = origin_target.clone();
            rule.origin_citation_path = citation_path.clone();
        }
    }

    fn without_imports(mut self) -> Self {
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
                Some(RuleKind::DataRelation | RuleKind::DerivedRelation) => Some(rule.name.clone()),
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
            // Rounding is an output-rule concern; reject it on any non-derived
            // kind up front (a parameter/relation with `rounding:` would
            // otherwise be silently dropped by the name-match that attaches it).
            if rule.rounding.is_some() && !matches!(kind, RuleKind::Derived) {
                return Err(RuleSpecError::RoundingOnNonDerivedRule {
                    name: rule.name.clone(),
                    kind: kind.as_str().to_string(),
                });
            }
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
        apply_source_relation_sets(&mut program, &self.rules)?;
        rewrite_filtered_entity_member_aliases(&mut program);
        // Carried for tooling and artifact pass-through only; nothing in
        // compilation or execution reads it.
        program.module = self.module.clone();
        Ok(program)
    }

    fn apply_rule_ids(&self, program: &mut ProgramSpec) {
        for rule in &self.rules {
            // A rule's declared `rounding:` mode rides onto the lowered derived
            // spec here (after the formula round-trip), mirroring how ids are
            // reattached by name. The mode is validated against the unit later
            // in `to_program`. Only `derived` rules carry rounding.
            if rule.rounding.is_some() {
                for derived in &mut program.derived {
                    if derived.name == rule.name {
                        derived.rounding = rule.rounding;
                    }
                }
            }
            // The origin module's corpus citation path is reattached by name
            // the same way, so every parameter and derived rule carries the
            // join key to its legal source in the corpus.
            if let Some(citation_path) = rule.origin_citation_path.as_ref() {
                for parameter in &mut program.parameters {
                    if parameter.name == rule.name {
                        parameter.corpus_citation_path = Some(citation_path.clone());
                    }
                }
                for derived in &mut program.derived {
                    if derived.name == rule.name {
                        derived.corpus_citation_path = Some(citation_path.clone());
                    }
                }
            }
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
        let (source, source_url) = self.effective_source();
        Ok(IndexedParameterSpec {
            id: self.canonical_rule_id(),
            name: self.name.clone(),
            unit: self.unit.clone(),
            indexed_by: Some(indexed_by),
            source,
            source_url,
            // Reattached by name in `apply_rule_ids`, like the scalar
            // parameters lowered through the formula layer.
            corpus_citation_path: None,
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
        let derived_relation = self.derived_relation.as_ref().ok_or_else(|| {
            RuleSpecError::MissingDerivedRelation {
                name: self.name.clone(),
            }
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
                current_slot: derived_relation
                    .current_slot
                    .unwrap_or(inferred_current_slot),
                related_slot: derived_relation
                    .related_slot
                    .unwrap_or(inferred_related_slot),
                entity: derived_relation.entity.clone(),
                member_relation: derived_relation.member_relation.clone(),
                slot_entities: derived_relation.slot_entities.clone(),
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

/// Merge the RuleSpec module's explicitly-declared `units:` into the program.
/// A declaration for a name the formula layer already seeded (its currency
/// defaults — GBP/USD/EUR at `minor_units: 2`, etc.) **overrides** that default
/// rather than being dropped, so an encoder can declare e.g. whole-dollar
/// `USD { minor_units: 0 }` for a program that rounds to dollars. New names are
/// appended. This is what "RuleSpec modules may override their own units" in the
/// formula-default seeding means; the rounding contract depends on the declared
/// `minor_units` actually taking effect.
fn append_missing_units(program: &mut ProgramSpec, units: &[UnitSpec]) {
    for unit in units {
        if let Some(existing) = program.units.iter_mut().find(|u| u.name == unit.name) {
            existing.kind = unit.kind.clone();
        } else {
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

fn apply_source_relation_sets(
    program: &mut ProgramSpec,
    rules: &[RuleDefinition],
) -> Result<(), RuleSpecError> {
    for rule in rules {
        let Some(source_relation) = rule.source_relation.as_ref() else {
            continue;
        };
        if source_relation.relation_type.as_ref() != Some(&SourceRelationType::Sets) {
            continue;
        }
        let Some(target_ref) = source_relation
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| target.contains('#'))
        else {
            continue;
        };
        let Some(value_ref) = source_relation
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| value.contains('#'))
        else {
            continue;
        };
        if target_ref == value_ref {
            continue;
        }

        let value_binding = find_set_binding(program, value_ref);
        let target_binding = find_set_binding(program, target_ref);

        let (Some(value_binding), Some(target_binding)) = (value_binding, target_binding) else {
            if value_binding.is_none() && target_binding.is_none() {
                continue;
            }
            if value_binding.is_none() {
                return match target_binding
                    .expect("target binding is present when value is absent")
                    .kind
                {
                    SetBindingKind::Parameter => {
                        Err(RuleSpecError::SourceRelationSetValueNotParameter {
                            name: rule.name.clone(),
                            value: value_ref.to_string(),
                        })
                    }
                    SetBindingKind::Derived => {
                        Err(RuleSpecError::SourceRelationSetValueNotExecutable {
                            name: rule.name.clone(),
                            value: value_ref.to_string(),
                        })
                    }
                };
            }
            return match value_binding
                .expect("value binding is present when target is absent")
                .kind
            {
                SetBindingKind::Parameter => {
                    Err(RuleSpecError::SourceRelationSetTargetNotParameter {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                    })
                }
                SetBindingKind::Derived => {
                    Err(RuleSpecError::SourceRelationSetTargetNotExecutable {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                    })
                }
            };
        };

        if value_binding.kind != target_binding.kind {
            return Err(RuleSpecError::SourceRelationSetKindMismatch {
                name: rule.name.clone(),
                target: target_ref.to_string(),
                value: value_ref.to_string(),
                target_kind: target_binding.kind.as_str().to_string(),
                value_kind: value_binding.kind.as_str().to_string(),
            });
        }

        match value_binding.kind {
            SetBindingKind::Parameter => {
                let value_parameter = program.parameters[value_binding.index].clone();
                let target_parameter = &mut program.parameters[target_binding.index];

                if target_parameter.indexed_by != value_parameter.indexed_by {
                    return Err(RuleSpecError::SourceRelationSetIndexedByMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }
                if target_parameter.unit.is_some()
                    && value_parameter.unit.is_some()
                    && target_parameter.unit != value_parameter.unit
                {
                    return Err(RuleSpecError::SourceRelationSetUnitMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }

                target_parameter.versions = value_parameter.versions;
            }
            SetBindingKind::Derived => {
                let value_derived = program.derived[value_binding.index].clone();
                let target_derived = &mut program.derived[target_binding.index];

                if target_derived.entity != value_derived.entity {
                    return Err(RuleSpecError::SourceRelationSetEntityMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }
                if target_derived.dtype != value_derived.dtype {
                    return Err(RuleSpecError::SourceRelationSetDTypeMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }
                if target_derived.unit.is_some()
                    && value_derived.unit.is_some()
                    && target_derived.unit != value_derived.unit
                {
                    return Err(RuleSpecError::SourceRelationSetUnitMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }
                if target_derived.period.is_some()
                    && value_derived.period.is_some()
                    && target_derived.period != value_derived.period
                {
                    return Err(RuleSpecError::SourceRelationSetPeriodMismatch {
                        name: rule.name.clone(),
                        target: target_ref.to_string(),
                        value: value_ref.to_string(),
                    });
                }

                target_derived.semantics = value_derived.semantics;
                target_derived.versions = value_derived.versions;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetBindingKind {
    Parameter,
    Derived,
}

impl SetBindingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Parameter => "parameter",
            Self::Derived => "derived",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SetBinding {
    kind: SetBindingKind,
    index: usize,
}

fn find_set_binding(program: &ProgramSpec, reference: &str) -> Option<SetBinding> {
    if let Some(index) = program
        .parameters
        .iter()
        .position(|parameter| parameter.id.as_deref() == Some(reference))
    {
        return Some(SetBinding {
            kind: SetBindingKind::Parameter,
            index,
        });
    }
    program
        .derived
        .iter()
        .position(|derived| derived.id.as_deref() == Some(reference))
        .map(|index| SetBinding {
            kind: SetBindingKind::Derived,
            index,
        })
}

fn rewrite_filtered_entity_member_aliases(program: &mut ProgramSpec) {
    let aliases = program
        .relations
        .iter()
        .filter_map(|relation| {
            let derivation = relation.derivation.as_ref()?;
            Some((
                derivation.entity.as_ref()?.clone(),
                derivation.member_relation.as_ref()?.clone(),
                relation.name.clone(),
            ))
        })
        .collect::<Vec<(String, String, String)>>();
    if aliases.is_empty() {
        return;
    }

    for derived in &mut program.derived {
        for (entity, alias, relation_name) in &aliases {
            if &derived.entity != entity {
                continue;
            }
            match &mut derived.semantics {
                DerivedSemanticsSpec::Scalar { expr } => {
                    rewrite_relation_alias_in_scalar(expr, alias, relation_name);
                }
                DerivedSemanticsSpec::Judgment { expr } => {
                    rewrite_relation_alias_in_judgment(expr, alias, relation_name);
                }
            }
            for version in &mut derived.versions {
                match &mut version.semantics {
                    DerivedSemanticsSpec::Scalar { expr } => {
                        rewrite_relation_alias_in_scalar(expr, alias, relation_name);
                    }
                    DerivedSemanticsSpec::Judgment { expr } => {
                        rewrite_relation_alias_in_judgment(expr, alias, relation_name);
                    }
                }
            }
        }
    }

    let alias_names = aliases
        .into_iter()
        .map(|(_, alias, _)| alias)
        .collect::<HashSet<String>>();
    let used_relations = used_relation_names(program);
    program.relations.retain(|relation| {
        !alias_names.contains(&relation.name) || used_relations.contains(&relation.name)
    });
}

fn rewrite_relation_alias_in_scalar(expr: &mut ScalarExprSpec, alias: &str, relation_name: &str) {
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
            rewrite_relation_alias_in_scalar(index, alias, relation_name);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                rewrite_relation_alias_in_scalar(item, alias, relation_name);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            rewrite_relation_alias_in_scalar(left, alias, relation_name);
            rewrite_relation_alias_in_scalar(right, alias, relation_name);
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            rewrite_relation_alias_in_scalar(date, alias, relation_name);
            rewrite_relation_alias_in_scalar(days, alias, relation_name);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            rewrite_relation_alias_in_scalar(from, alias, relation_name);
            rewrite_relation_alias_in_scalar(to, alias, relation_name);
        }
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            if relation == alias {
                *relation = relation_name.to_string();
            }
            if let Some(where_clause) = where_clause {
                rewrite_relation_alias_in_judgment(where_clause, alias, relation_name);
            }
        }
        ScalarExprSpec::SumRelated {
            relation,
            where_clause,
            ..
        } => {
            if relation == alias {
                *relation = relation_name.to_string();
            }
            if let Some(where_clause) = where_clause {
                rewrite_relation_alias_in_judgment(where_clause, alias, relation_name);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_relation_alias_in_judgment(condition, alias, relation_name);
            rewrite_relation_alias_in_scalar(then_expr, alias, relation_name);
            rewrite_relation_alias_in_scalar(else_expr, alias, relation_name);
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            rewrite_relation_alias_in_scalar(value, alias, relation_name);
            if let Some(n) = n {
                rewrite_relation_alias_in_scalar(n, alias, relation_name);
            }
        }
    }
}

fn rewrite_relation_alias_in_judgment(
    expr: &mut JudgmentExprSpec,
    alias: &str,
    relation_name: &str,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            rewrite_relation_alias_in_scalar(left, alias, relation_name);
            rewrite_relation_alias_in_scalar(right, alias, relation_name);
        }
        JudgmentExprSpec::Derived { .. } => {}
        JudgmentExprSpec::RelationMember { relation, .. } => {
            if relation == alias {
                *relation = relation_name.to_string();
            }
        }
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                rewrite_relation_alias_in_judgment(item, alias, relation_name);
            }
        }
        JudgmentExprSpec::Not { item } => {
            rewrite_relation_alias_in_judgment(item, alias, relation_name);
        }
    }
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
    let unambiguous_short_rewrites = unambiguous_relation_rewrites(rewrites);
    let derived_origin_targets = derived_origin_targets(program);
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
                rewrite_scalar_relation_references(
                    expr,
                    origin_target,
                    rewrites,
                    &unambiguous_short_rewrites,
                    &derived_origin_targets,
                );
            }
            DerivedSemanticsSpec::Judgment { expr } => {
                rewrite_judgment_relation_references(
                    expr,
                    origin_target,
                    rewrites,
                    &unambiguous_short_rewrites,
                    &derived_origin_targets,
                );
            }
        }
        for version in &mut derived.versions {
            match &mut version.semantics {
                DerivedSemanticsSpec::Scalar { expr } => {
                    rewrite_scalar_relation_references(
                        expr,
                        origin_target,
                        rewrites,
                        &unambiguous_short_rewrites,
                        &derived_origin_targets,
                    );
                }
                DerivedSemanticsSpec::Judgment { expr } => {
                    rewrite_judgment_relation_references(
                        expr,
                        origin_target,
                        rewrites,
                        &unambiguous_short_rewrites,
                        &derived_origin_targets,
                    );
                }
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
            rewrite_judgment_relation_references(
                &mut derivation.predicate,
                origin_target,
                rewrites,
                &unambiguous_short_rewrites,
                &derived_origin_targets,
            );
        }
    }
    let used_relations = used_relation_names(program);
    program.relations.retain(|relation| {
        !namespaced_short_names.contains(&relation.name) || used_relations.contains(&relation.name)
    });
    Ok(())
}

fn unambiguous_relation_rewrites(
    rewrites: &HashMap<(String, String), String>,
) -> HashMap<String, String> {
    let mut candidates: HashMap<String, Option<String>> = HashMap::new();
    for ((_, short_name), canonical_name) in rewrites {
        candidates
            .entry(short_name.clone())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(canonical_name.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(canonical_name.clone()));
    }
    candidates
        .into_iter()
        .filter_map(|(short_name, canonical_name)| {
            canonical_name.map(|canonical_name| (short_name, canonical_name))
        })
        .collect()
}

fn derived_origin_targets(program: &ProgramSpec) -> HashMap<String, String> {
    let mut candidates: HashMap<String, Option<String>> = HashMap::new();
    for derived in &program.derived {
        let Some(origin_target) = derived
            .id
            .as_deref()
            .and_then(|id| id.split_once('#').map(|(target, _)| target.to_string()))
        else {
            continue;
        };
        candidates
            .entry(derived.name.clone())
            .and_modify(|candidate| {
                if candidate.as_ref() != Some(&origin_target) {
                    *candidate = None;
                }
            })
            .or_insert(Some(origin_target));
    }
    candidates
        .into_iter()
        .filter_map(|(name, origin_target)| {
            origin_target.map(|origin_target| (name, origin_target))
        })
        .collect()
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
        for version in &derived.versions {
            match &version.semantics {
                DerivedSemanticsSpec::Scalar { expr } => {
                    collect_scalar_relation_names(expr, &mut names);
                }
                DerivedSemanticsSpec::Judgment { expr } => {
                    collect_judgment_relation_names(expr, &mut names);
                }
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
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            collect_scalar_relation_names(value, names);
            if let Some(n) = n {
                collect_scalar_relation_names(n, names);
            }
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
    unambiguous_short_rewrites: &HashMap<String, String>,
    derived_origin_targets: &HashMap<String, String>,
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
            rewrite_scalar_relation_references(
                index,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                rewrite_scalar_relation_references(
                    item,
                    origin_target,
                    rewrites,
                    unambiguous_short_rewrites,
                    derived_origin_targets,
                );
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            rewrite_scalar_relation_references(
                left,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                right,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            rewrite_scalar_relation_references(
                date,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                days,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            rewrite_scalar_relation_references(
                from,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                to,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            if !rewrite_relation_name(relation, origin_target, rewrites)
                && related_aggregation_uses_imported_derived(
                    None,
                    where_clause.as_deref(),
                    origin_target,
                    derived_origin_targets,
                )
            {
                rewrite_unambiguous_relation_name(relation, unambiguous_short_rewrites);
            }
            if let Some(where_clause) = where_clause {
                rewrite_judgment_relation_references(
                    where_clause,
                    origin_target,
                    rewrites,
                    unambiguous_short_rewrites,
                    derived_origin_targets,
                );
            }
        }
        ScalarExprSpec::SumRelated {
            relation,
            value,
            where_clause,
            ..
        } => {
            if !rewrite_relation_name(relation, origin_target, rewrites)
                && related_aggregation_uses_imported_derived(
                    Some(value),
                    where_clause.as_deref(),
                    origin_target,
                    derived_origin_targets,
                )
            {
                rewrite_unambiguous_relation_name(relation, unambiguous_short_rewrites);
            }
            if let Some(where_clause) = where_clause {
                rewrite_judgment_relation_references(
                    where_clause,
                    origin_target,
                    rewrites,
                    unambiguous_short_rewrites,
                    derived_origin_targets,
                );
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_judgment_relation_references(
                condition,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                then_expr,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                else_expr,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            rewrite_scalar_relation_references(
                value,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            if let Some(n) = n {
                rewrite_scalar_relation_references(
                    n,
                    origin_target,
                    rewrites,
                    unambiguous_short_rewrites,
                    derived_origin_targets,
                );
            }
        }
    }
}

fn rewrite_judgment_relation_references(
    expr: &mut JudgmentExprSpec,
    origin_target: &str,
    rewrites: &HashMap<(String, String), String>,
    unambiguous_short_rewrites: &HashMap<String, String>,
    derived_origin_targets: &HashMap<String, String>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            rewrite_scalar_relation_references(
                left,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
            rewrite_scalar_relation_references(
                right,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
        JudgmentExprSpec::Derived { .. } => {}
        JudgmentExprSpec::RelationMember { relation, .. } => {
            rewrite_relation_name(relation, origin_target, rewrites);
        }
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            for item in items {
                rewrite_judgment_relation_references(
                    item,
                    origin_target,
                    rewrites,
                    unambiguous_short_rewrites,
                    derived_origin_targets,
                );
            }
        }
        JudgmentExprSpec::Not { item } => {
            rewrite_judgment_relation_references(
                item,
                origin_target,
                rewrites,
                unambiguous_short_rewrites,
                derived_origin_targets,
            );
        }
    }
}

fn rewrite_relation_name(
    relation: &mut String,
    origin_target: &str,
    rewrites: &HashMap<(String, String), String>,
) -> bool {
    if let Some(rewrite) = rewrites.get(&(origin_target.to_string(), relation.clone())) {
        *relation = rewrite.clone();
        true
    } else {
        false
    }
}

fn rewrite_unambiguous_relation_name(
    relation: &mut String,
    unambiguous_short_rewrites: &HashMap<String, String>,
) -> bool {
    if relation.contains("#relation.") {
        return false;
    }
    if let Some(rewrite) = unambiguous_short_rewrites.get(relation) {
        *relation = rewrite.clone();
        true
    } else {
        false
    }
}

fn related_aggregation_uses_imported_derived(
    value: Option<&RelatedValueRefSpec>,
    where_clause: Option<&JudgmentExprSpec>,
    origin_target: &str,
    derived_origin_targets: &HashMap<String, String>,
) -> bool {
    if let Some(RelatedValueRefSpec::Derived { name }) = value {
        if derived_reference_is_imported(name, origin_target, derived_origin_targets) {
            return true;
        }
    }
    where_clause
        .map(|expr| judgment_uses_imported_derived(expr, origin_target, derived_origin_targets))
        .unwrap_or(false)
}

fn judgment_uses_imported_derived(
    expr: &JudgmentExprSpec,
    origin_target: &str,
    derived_origin_targets: &HashMap<String, String>,
) -> bool {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            scalar_uses_imported_derived(left, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(right, origin_target, derived_origin_targets)
        }
        JudgmentExprSpec::Derived { name } => {
            derived_reference_is_imported(name, origin_target, derived_origin_targets)
        }
        JudgmentExprSpec::RelationMember { .. } => false,
        JudgmentExprSpec::And { items } | JudgmentExprSpec::Or { items } => {
            items.iter().any(|item| {
                judgment_uses_imported_derived(item, origin_target, derived_origin_targets)
            })
        }
        JudgmentExprSpec::Not { item } => {
            judgment_uses_imported_derived(item, origin_target, derived_origin_targets)
        }
    }
}

fn scalar_uses_imported_derived(
    expr: &ScalarExprSpec,
    origin_target: &str,
    derived_origin_targets: &HashMap<String, String>,
) -> bool {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => false,
        ScalarExprSpec::Derived { name } => {
            derived_reference_is_imported(name, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::ParameterLookup { index, .. }
        | ScalarExprSpec::Ceil { value: index }
        | ScalarExprSpec::Floor { value: index } => {
            scalar_uses_imported_derived(index, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => items
            .iter()
            .any(|item| scalar_uses_imported_derived(item, origin_target, derived_origin_targets)),
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            scalar_uses_imported_derived(left, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(right, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::DateAddDays { date, days } => {
            scalar_uses_imported_derived(date, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(days, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            scalar_uses_imported_derived(from, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(to, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::CountRelated { where_clause, .. } => where_clause
            .as_deref()
            .map(|expr| judgment_uses_imported_derived(expr, origin_target, derived_origin_targets))
            .unwrap_or(false),
        ScalarExprSpec::SumRelated {
            value,
            where_clause,
            ..
        } => related_aggregation_uses_imported_derived(
            Some(value),
            where_clause.as_deref(),
            origin_target,
            derived_origin_targets,
        ),
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            judgment_uses_imported_derived(condition, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(then_expr, origin_target, derived_origin_targets)
                || scalar_uses_imported_derived(else_expr, origin_target, derived_origin_targets)
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            scalar_uses_imported_derived(value, origin_target, derived_origin_targets)
                || n.as_deref().is_some_and(|n| {
                    scalar_uses_imported_derived(n, origin_target, derived_origin_targets)
                })
        }
    }
}

fn derived_reference_is_imported(
    name: &str,
    origin_target: &str,
    derived_origin_targets: &HashMap<String, String>,
) -> bool {
    derived_origin_targets
        .get(name)
        .map(|derived_origin| derived_origin != origin_target)
        .unwrap_or(false)
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
