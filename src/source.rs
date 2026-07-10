//! Host-supplied module resolution.
//!
//! The core engine is a pure function over (modules, dataset): given RuleSpec
//! module text for every canonical target a program needs, it can lower,
//! compile, and execute without touching a filesystem. `ModuleSource` is the
//! seam where a host supplies that text — from disk, from memory, from a
//! registry, or from a browser bundle.
//!
//! Resolution of relative imports to canonical targets is pure string logic on
//! the importer's canonical target and stays in `crate::rulespec`
//! (`resolve_import_target`). Only "find and read the module text" lives
//! behind this trait.

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
/// normalizes targets (quotes, fragments, `.yaml`/`.yml` extensions, relative
/// imports) before calling `load`, so implementations only translate a
/// canonical target into module text.
pub trait ModuleSource {
    /// Return the module YAML text for `target`, `Ok(None)` if this source
    /// has no module for it, or `Err` for a host-level failure.
    fn load(&self, target: &str) -> Result<Option<String>, SourceError>;
}

/// Filesystem-backed `ModuleSource`: the jurisdiction-repo resolution the
/// engine has always used (legacy sibling checkouts, country monorepos,
/// `AXIOM_RULESPEC_REPO_ROOTS`), anchored at a path.
///
/// Candidate repo roots are discovered from the anchor's ancestors plus the
/// environment, exactly like `load_rulespec_file` discovers them from an
/// importing file's path. When `AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE=1`, only
/// the non-empty roots configured by `AXIOM_RULESPEC_REPO_ROOTS` are searched.
#[cfg(feature = "fs")]
pub struct FsModuleSource {
    anchor: std::path::PathBuf,
}

#[cfg(feature = "fs")]
impl FsModuleSource {
    /// Create a source anchored at `anchor` — typically the root module file
    /// or the directory the host is working in. Additive repo-root discovery
    /// walks the anchor's ancestors; exclusive mode deliberately ignores it.
    pub fn new(anchor: impl Into<std::path::PathBuf>) -> Self {
        Self {
            anchor: anchor.into(),
        }
    }
}

#[cfg(feature = "fs")]
impl ModuleSource for FsModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        let Some((prefix, relative)) = target.split_once(':') else {
            return Ok(None);
        };
        if !crate::rulespec::is_canonical_repo_prefix(prefix) {
            return Ok(None);
        }
        let relative_path =
            std::path::PathBuf::from(format!("{}.yaml", relative.trim().trim_matches('/')));
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Ok(None);
        }
        let roots = crate::rulespec::candidate_rule_repo_roots(&self.anchor, prefix)
            .map_err(|message| SourceError::new(target, message))?;
        for root in roots {
            let candidate = root.join(&relative_path);
            if candidate.exists() {
                return std::fs::read_to_string(&candidate)
                    .map(Some)
                    .map_err(|error| SourceError::new(target, error.to_string()));
            }
        }
        Ok(None)
    }
}
