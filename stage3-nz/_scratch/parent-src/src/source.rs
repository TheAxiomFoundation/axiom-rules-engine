//! Host-supplied module resolution.
//!
//! The core engine is a pure function over (modules, dataset): given RuleSpec
//! module text for every canonical target a program needs, it can lower,
//! compile, and execute without touching a filesystem. `ModuleSource` is the
//! seam where a host supplies that text — from disk, from memory, from a
//! registry, or from a browser bundle.
//!
//! Exact absolute imports are validated and their optional symbol fragments
//! removed in `crate::rulespec` (`resolve_import_target`). Only "find and read
//! the module text" lives behind this trait.

use thiserror::Error;

/// A host failure while loading module text (I/O error, network error, …).
///
/// "Module not found" is not an error: `ModuleSource::load` returns
/// `Ok(None)` for unknown targets so the loader can report an unresolved
/// import with importer context.
#[derive(Debug, Error)]
#[error("module source failed to load `{target}`: {message}")]
pub struct SourceError {
    /// The canonical target whose load failed.
    pub target: String,
    /// Host-specific failure description.
    pub message: String,
}

impl SourceError {
    pub fn new(target: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            message: message.into(),
        }
    }
}

/// Supplies RuleSpec module YAML text for canonical targets.
///
/// `target` is always the canonical form `<jurisdiction>:<relative path
/// without extension>`, for example `us:statutes/7/2015/e`. The loader
/// validates targets and removes an import's optional symbol fragment before
/// calling `load`, so implementations only translate an exact canonical target
/// into module text.
pub trait ModuleSource {
    /// Return the module YAML text for `target`, `Ok(None)` if this source
    /// has no module for it, or `Err` for a host-level failure.
    fn load(&self, target: &str) -> Result<Option<String>, SourceError>;
}

/// Filesystem-backed `ModuleSource` over explicit, validated canonical country
/// monorepos. It never consults environment variables, cwd, ancestors, sibling
/// checkouts, or legacy standalone repositories.
#[cfg(feature = "fs")]
pub struct FsModuleSource {
    roots: crate::rulespec::CanonicalRuleSpecRoots,
}

#[cfg(feature = "fs")]
impl FsModuleSource {
    /// Validate and exclusively use the supplied canonical country roots.
    pub fn new<I, P>(roots: I) -> Result<Self, crate::rulespec::RuleSpecError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<std::path::Path>,
    {
        Ok(Self {
            roots: crate::rulespec::CanonicalRuleSpecRoots::new(roots)?,
        })
    }

    pub(crate) fn from_validated_roots(roots: crate::rulespec::CanonicalRuleSpecRoots) -> Self {
        Self { roots }
    }
}

#[cfg(feature = "fs")]
impl ModuleSource for FsModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        self.roots
            .read_target(target)
            .map_err(|error| SourceError::new(target, error.to_string()))
    }
}
