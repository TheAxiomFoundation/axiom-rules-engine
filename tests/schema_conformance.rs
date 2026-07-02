//! Conformance of the published schemas against the real `rulespec-us`
//! corpus.
//!
//! For every RuleSpec module (`*.yaml` with a `format: rulespec/v1` /
//! `schema: axiom.rules*` discriminator) and every companion test
//! (`*.test.yaml`) under a `rulespec-us` checkout, this validates the file
//! against the matching published schema and tallies pass/fail counts,
//! grouping failures by cause.
//!
//! Program-spec files under `programs/**` (keyed by `program:` / `scope:` /
//! `transformations:`) are a DIFFERENT format — a compose spec, not a rule
//! module — and are excluded from module-schema validation and counted
//! separately.
//!
//! The test is skipped when no checkout is found, so CI in this repo (which
//! has no `rulespec-us`) stays green while local runs produce the real
//! numbers. When the checkout is present, the tally is compared against a
//! checked-in ratchet [`MAX_KNOWN_FAILURES`]: the count may fall (tighten the
//! constant) but not rise. Set `AXIOM_SCHEMA_CONFORMANCE_REPORT=1` to print
//! the full per-cause breakdown and example files.

#![cfg(feature = "schema")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axiom_rules_engine::schema::all_schemas;
use jsonschema::Validator;
use serde_json::Value;

/// Ratchet: the maximum number of module/test files under `rulespec-us`
/// allowed to fail their schema. It is `0` because the schemas were authored
/// to mirror the engine's serde *deserialization* acceptance exactly, and at
/// the time of writing all 3,017 discriminated modules and all 3,010 companion
/// tests validate (verified bidirectionally by `schema_fidelity.rs`). This is
/// a real measured floor, not an aspiration: raising it should never be a way
/// to make a red run pass. If a future corpus file legitimately deserializes
/// but does not validate, the schema is the bug — fix the schema, do not raise
/// this. Only raise it (with a linked issue) if the corpus starts carrying
/// files that are genuinely malformed at the deserialization layer.
const MAX_KNOWN_FAILURES: usize = 0;

/// Candidate locations for a `rulespec-us` checkout, relative to this repo.
fn rulespec_us_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("AXIOM_RULESPEC_US_ROOT") {
        let path = PathBuf::from(explicit);
        return path.is_dir().then_some(path);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        // Sibling checkout: .../TheAxiomFoundation/{axiom-rules-engine, rulespec-us}
        manifest.parent().map(|p| p.join("rulespec-us")),
        // Worktree layout: .../TheAxiomFoundation/_worktrees/<wt>/  →  ../../rulespec-us
        manifest
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("rulespec-us")),
    ];
    candidates.into_iter().flatten().find(|path| path.is_dir())
}

fn load_schema(file_name: &str) -> Validator {
    let schema = all_schemas()
        .into_iter()
        .find(|named| named.file_name == file_name)
        .unwrap_or_else(|| panic!("{file_name} is published"))
        .schema;
    jsonschema::draft7::new(&schema).expect("published schema compiles")
}

/// A YAML document may hold multiple `---`-separated documents; RuleSpec files
/// are single-document, but be defensive and take the first.
fn parse_yaml(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    serde_yaml::from_str::<Value>(&text).map_err(|e| format!("yaml parse error: {e}"))
}

fn looks_like_rulespec(doc: &Value) -> bool {
    let format_ok = doc
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|f| f == "rulespec/v1");
    let schema_ok = doc
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with("axiom.rules"));
    format_ok || schema_ok
}

/// A compose/program spec (a different format), recognized by its keys.
fn is_program_spec(path: &Path, doc: &Value) -> bool {
    let under_programs = path.components().any(|c| c.as_os_str() == "programs");
    let has_program_keys = doc.get("program").is_some()
        || doc.get("scope").is_some()
        || doc.get("transformations").is_some();
    under_programs && has_program_keys
}

fn walk_yaml(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip VCS and tooling directories.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == ".git" || name == "node_modules" || name == "target" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A short, stable cause key for grouping failures: the first validation
/// error's message, truncated and stripped of instance-specific tails.
fn cause_key(validator: &Validator, instance: &Value) -> String {
    let Some(first) = validator.iter_errors(instance).next() else {
        return "unknown".to_string();
    };
    let path = first.instance_path().to_string();
    let msg = first.to_string();
    // Collapse the value-bearing prefix of common messages so cases group.
    let head = msg.split(" is not ").next().unwrap_or(&msg);
    let head = if head.len() > 60 { &head[..60] } else { head };
    let path_tail = path.rsplit('/').next().unwrap_or("");
    format!("at .../{path_tail}: {head}…")
}

struct Tally {
    module_pass: usize,
    module_fail: usize,
    test_pass: usize,
    test_fail: usize,
    program_specs: usize,
    non_rulespec: usize,
    yaml_errors: usize,
    causes: BTreeMap<String, usize>,
    examples: BTreeMap<String, PathBuf>,
}

#[test]
// `MAX_KNOWN_FAILURES` is a ratchet meant to be tuned (and may become
// non-zero if the corpus ever carries genuinely malformed files). The
// `total_fail <= MAX_KNOWN_FAILURES` / `>` comparisons read as "absurd" to
// clippy only because the current faithful value is 0; they are the correct
// ratchet expressions, so silence the lint here rather than hard-coding `== 0`.
#[allow(clippy::absurd_extreme_comparisons)]
fn rulespec_us_conforms_to_published_schemas() {
    let Some(root) = rulespec_us_root() else {
        eprintln!(
            "skipping: no rulespec-us checkout found \
             (set AXIOM_RULESPEC_US_ROOT to run this conformance check)"
        );
        return;
    };

    let module_schema = load_schema("rulespec-module.v1.schema.json");
    let test_schema = load_schema("rulespec-test.v1.schema.json");

    let mut tally = Tally {
        module_pass: 0,
        module_fail: 0,
        test_pass: 0,
        test_fail: 0,
        program_specs: 0,
        non_rulespec: 0,
        yaml_errors: 0,
        causes: BTreeMap::new(),
        examples: BTreeMap::new(),
    };

    for path in walk_yaml(&root) {
        let is_test = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".test.yaml"));

        let doc = match parse_yaml(&path) {
            Ok(doc) => doc,
            Err(_) => {
                tally.yaml_errors += 1;
                continue;
            }
        };

        if is_test {
            if test_schema.is_valid(&doc) {
                tally.test_pass += 1;
            } else {
                tally.test_fail += 1;
                let key = format!("[test] {}", cause_key(&test_schema, &doc));
                *tally.causes.entry(key.clone()).or_insert(0) += 1;
                tally.examples.entry(key).or_insert_with(|| path.clone());
            }
            continue;
        }

        if is_program_spec(&path, &doc) {
            tally.program_specs += 1;
            continue;
        }
        if !looks_like_rulespec(&doc) {
            // sources/ registry files, known-*.yaml ledgers, and other
            // non-module YAML: out of scope for the module schema.
            tally.non_rulespec += 1;
            continue;
        }

        if module_schema.is_valid(&doc) {
            tally.module_pass += 1;
        } else {
            tally.module_fail += 1;
            let key = format!("[module] {}", cause_key(&module_schema, &doc));
            *tally.causes.entry(key.clone()).or_insert(0) += 1;
            tally.examples.entry(key).or_insert_with(|| path.clone());
        }
    }

    let total_validated = tally.module_pass + tally.module_fail + tally.test_pass + tally.test_fail;
    let total_fail = tally.module_fail + tally.test_fail;

    let report = std::env::var_os("AXIOM_SCHEMA_CONFORMANCE_REPORT").is_some();
    if report || total_fail > MAX_KNOWN_FAILURES {
        eprintln!(
            "\n=== rulespec-us schema conformance (root: {}) ===",
            root.display()
        );
        eprintln!(
            "modules: {} pass, {} fail",
            tally.module_pass, tally.module_fail
        );
        eprintln!(
            "tests:   {} pass, {} fail",
            tally.test_pass, tally.test_fail
        );
        eprintln!(
            "excluded: {} program specs, {} non-rulespec yaml, {} yaml parse errors",
            tally.program_specs, tally.non_rulespec, tally.yaml_errors
        );
        eprintln!(
            "validated {total_validated} files; {total_fail} failures across {} causes:",
            tally.causes.len()
        );
        for (cause, count) in &tally.causes {
            eprintln!("  {count:>4}  {cause}");
            if let Some(example) = tally.examples.get(cause) {
                eprintln!("        e.g. {}", example.display());
            }
        }
        eprintln!();
    }

    // Guard against a vacuous green: if the walker found almost nothing, the
    // checkout is malformed or the discovery logic broke — a "0 failures" over
    // 0 files must not pass. These floors are far below the ~3k modules / ~3k
    // tests the corpus actually has.
    assert!(
        tally.module_pass + tally.module_fail >= 1000,
        "expected to validate >=1000 RuleSpec modules under {}, saw {} — \
         discovery is likely broken",
        root.display(),
        tally.module_pass + tally.module_fail
    );
    assert!(
        tally.test_pass + tally.test_fail >= 1000,
        "expected to validate >=1000 companion tests under {}, saw {} — \
         discovery is likely broken",
        root.display(),
        tally.test_pass + tally.test_fail
    );

    assert!(
        total_fail <= MAX_KNOWN_FAILURES,
        "schema conformance regressed: {total_fail} module/test files fail \
         (ratchet allows {MAX_KNOWN_FAILURES}). Re-run with \
         AXIOM_SCHEMA_CONFORMANCE_REPORT=1 for the per-cause breakdown, then \
         either fix the schema or, if the corpus is genuinely at fault, raise \
         the ratchet with justification."
    );
}
