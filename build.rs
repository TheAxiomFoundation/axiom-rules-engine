//! Stamps the build identity a compiled artifact records as `engine_build`
//! (`docs/artifact-format.md`): the commit this engine was built from and
//! whether its sources differed from that commit.
//!
//! It uses only `std` and never fails the build: any git or filesystem error
//! leaves the commit (and so the dirty flag) unknown. The library reads the
//! result from `AXIOM_RULES_ENGINE_STAMPED_COMMIT` (40 lowercase hex digits,
//! or empty) and `AXIOM_RULES_ENGINE_STAMPED_DIRTY` (`true`, `false`, or
//! empty).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Pins the commit, for builds that know it better than git can see it (a
/// release pipeline building from an exported tree, for example). Honoured
/// only when it is 40 hex digits.
const COMMIT_OVERRIDE: &str = "AXIOM_RULES_ENGINE_BUILD_COMMIT";
/// `true` or `false` beside the commit override; anything else is unknown.
const DIRTY_OVERRIDE: &str = "AXIOM_RULES_ENGINE_BUILD_DIRTY";
/// The sources whose difference from the commit makes a build dirty.
const SOURCES: [&str; 4] = ["src", "build.rs", "Cargo.toml", "Cargo.lock"];

fn main() {
    println!("cargo:rerun-if-env-changed={COMMIT_OVERRIDE}");
    println!("cargo:rerun-if-env-changed={DIRTY_OVERRIDE}");
    let crate_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    // A path that does not exist would make cargo rerun this script on
    // every build, so only existing paths are watched.
    for path in SOURCES.iter().chain([".cargo_vcs_info.json"].iter()) {
        watch(&crate_dir.join(path));
    }
    let (commit, dirty) = build_identity(&crate_dir);
    println!(
        "cargo:rustc-env=AXIOM_RULES_ENGINE_STAMPED_COMMIT={}",
        commit.unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=AXIOM_RULES_ENGINE_STAMPED_DIRTY={}",
        match dirty {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
    );
}

fn watch(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The commit and dirty flag, from the environment override, else git when
/// the crate directory is the top of a git work tree, else the
/// `.cargo_vcs_info.json` cargo writes into a packaged crate. The dirty flag
/// is unknown whenever the commit is.
fn build_identity(crate_dir: &Path) -> (Option<String>, Option<bool>) {
    if let Some(commit) = std::env::var(COMMIT_OVERRIDE)
        .ok()
        .filter(|commit| is_commit(commit))
    {
        let dirty = match std::env::var(DIRTY_OVERRIDE).ok().as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        return (Some(commit.to_ascii_lowercase()), dirty);
    }
    if let Some(identity) = from_git(crate_dir) {
        return identity;
    }
    from_vcs_info(crate_dir).unwrap_or((None, None))
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn git(crate_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(crate_dir)
        .args(args)
        // Never take the index lock another git process may hold.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|stdout| stdout.trim_end_matches(['\n', '\r']).to_string())
}

/// `None` when the crate directory is not the top of a git work tree: a copy
/// vendored inside another repository must not report that repository's
/// commit.
fn from_git(crate_dir: &Path) -> Option<(Option<String>, Option<bool>)> {
    let top_level = git(crate_dir, &["rev-parse", "--show-toplevel"])?;
    if std::fs::canonicalize(&top_level).ok()? != std::fs::canonicalize(crate_dir).ok()? {
        return None;
    }
    watch_git_refs(crate_dir);
    let commit = git(crate_dir, &["rev-parse", "HEAD"]).filter(|commit| is_commit(commit))?;
    let mut status = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    status.extend(SOURCES);
    let dirty = git(crate_dir, &status).map(|changes| !changes.is_empty());
    Some((Some(commit.to_ascii_lowercase()), dirty))
}

/// Rerun when HEAD moves: watch HEAD, the ref it names and `packed-refs`,
/// resolved through `git rev-parse --git-path` so a linked worktree (whose
/// HEAD lives under the main repository's `worktrees/` directory) resolves
/// correctly.
fn watch_git_refs(crate_dir: &Path) {
    let git_path = |name: &str| {
        git(crate_dir, &["rev-parse", "--git-path", name]).map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                crate_dir.join(path)
            }
        })
    };
    let Some(head) = git_path("HEAD") else {
        return;
    };
    watch(&head);
    if let Some(reference) = std::fs::read_to_string(&head).ok().and_then(|text| {
        text.strip_prefix("ref: ")
            .map(|name| name.trim().to_string())
    }) && let Some(path) = git_path(&reference)
    {
        watch(&path);
    }
    if let Some(packed) = git_path("packed-refs") {
        watch(&packed);
    }
}

/// `.cargo_vcs_info.json`: `{"git": {"sha1": "<40 hex>", "dirty": true}, ...}`.
/// The dirty flag is unknown when cargo did not record one.
fn from_vcs_info(crate_dir: &Path) -> Option<(Option<String>, Option<bool>)> {
    let text = std::fs::read_to_string(crate_dir.join(".cargo_vcs_info.json")).ok()?;
    let commit = json_value(&text, "sha1")?.trim_matches('"').to_string();
    if !is_commit(&commit) {
        return None;
    }
    let dirty = match json_value(&text, "dirty") {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    Some((Some(commit.to_ascii_lowercase()), dirty))
}

/// The raw text of the value after `"key":` in a flat JSON object, up to the
/// next comma or closing brace. Enough for the two scalar keys read above.
fn json_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = text[start..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}
