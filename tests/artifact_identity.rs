//! The two identities format 3 adds (#125): `engine_build`, stamped by
//! `build.rs`, and `source_closure`, the module texts a compile lowered.
//!
//! The expected module digests and closure digests below were computed with
//! Python's `hashlib` from the same texts, independently of the engine's own
//! SHA-256 routine.

use std::collections::BTreeMap;
use std::process::Command;

use axiom_rules_engine::compile::{
    CompiledProgramArtifact, CorpusProvisionIndex, EngineBuild, SourceClosure, SourceClosureModule,
    compile_summary_lines,
};
use axiom_rules_engine::rulespec::CanonicalRuleSpecRoots;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::ProgramSpec;

const ROOT: &str = "format: rulespec/v1\nimports:\n  - us:policies/identity/middle\n  - us:policies/identity/leaf\nrules:\n  - name: total\n    kind: derived\n    entity: Household\n    dtype: Money\n    period: Month\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: base_amount + floor_amount + amount\n";
const ROOT_SHA256: &str = "aebde665013bced10d820e40800e90b3db2f9d1712e3e37ad3518635778e95b1";
const MIDDLE: &str = "format: rulespec/v1\nimports:\n  - us:policies/identity/leaf\nrules:\n  - name: base_amount\n    kind: parameter\n    dtype: Money\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"10\"\n";
const MIDDLE_SHA256: &str = "d892e8625838f2b99791b2e68790e361f00122b7ae47a142bc844a58c6b360d9";
const LEAF: &str = "format: rulespec/v1\nrules:\n  - name: floor_amount\n    kind: parameter\n    dtype: Money\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"1\"\n";
const LEAF_SHA256: &str = "67e0b4da564720b05fb996d9f8d6e7ac112946bd5e668e9cf6f829a537d5b44e";
const UNUSED: &str = "format: rulespec/v1\nrules:\n  - name: never_imported\n    kind: parameter\n    dtype: Money\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"2\"\n";
const UNUSED_SHA256: &str = "83423e32cc16ee07d24fd8bb3349d8dd17a73b91ad37c2854320f18616cf1aed";
/// `sha256:` of the domain line and the three modules the root reaches.
const ATOMIC_DIGEST: &str =
    "sha256:1fcf96ea50751aa7c504fe1dfa61c33a810640546b6dc6fd16f6add05fb45f9f";
/// The same for `LEAF` lowered as an originless in-memory string.
const IN_MEMORY_DIGEST: &str =
    "sha256:8638a79703f9409e7345fd15ef65861cc2d692e1ccbfb01c845e0a6d89c1ffed";
const COMPOSITION: &str = "format: rulespec/v1\nmodule:\n  kind: composition\n  summary: Identity test composition.\nimports:\n  - us:policies/identity/leaf\nrules:\n  - name: composed_total\n    kind: derived\n    entity: Household\n    dtype: Money\n    period: Month\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: floor_amount + amount\n";
const COMPOSITION_SHA256: &str = "dcd3ad86cd4c337840e00fdfec09d3c39e17a792eeb916df3c7541952a7105cd";
/// The composition (originless, first) and the module it imports.
const COMPOSED_DIGEST: &str =
    "sha256:43e4b024cfc389012d84dddc09809b07ba0fe825c153cdff708ed114b3a1e15c";

const ROOT_TARGET: &str = "us:policies/identity/root";
const MIDDLE_TARGET: &str = "us:policies/identity/middle";
const LEAF_TARGET: &str = "us:policies/identity/leaf";

struct Modules(BTreeMap<&'static str, &'static str>);

impl ModuleSource for Modules {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).map(|text| text.to_string()))
    }
}

fn modules() -> Modules {
    Modules(BTreeMap::from([
        (ROOT_TARGET, ROOT),
        (MIDDLE_TARGET, MIDDLE),
        (LEAF_TARGET, LEAF),
        ("us:policies/identity/unused", UNUSED),
    ]))
}

fn module(target: Option<&str>, sha256: &str) -> SourceClosureModule {
    SourceClosureModule {
        target: target.map(str::to_string),
        sha256: sha256.to_string(),
    }
}

/// Every module text the loader read, the root included, once each, sorted
/// by target; a module nothing imports is not in it.
#[test]
fn the_source_closure_lists_every_module_text_the_loader_read() {
    let artifact = CompiledProgramArtifact::from_rulespec_with_source(ROOT_TARGET, &modules())
        .expect("the module graph compiles");
    assert_eq!(
        artifact.source_closure,
        Some(SourceClosure {
            root: Some(ROOT_TARGET.to_string()),
            modules: vec![
                module(Some(LEAF_TARGET), LEAF_SHA256),
                module(Some(MIDDLE_TARGET), MIDDLE_SHA256),
                module(Some(ROOT_TARGET), ROOT_SHA256),
            ],
            digest: ATOMIC_DIGEST.to_string(),
        })
    );
    assert!(
        !serde_json::to_string(&artifact)
            .unwrap()
            .contains(UNUSED_SHA256)
    );
}

/// An in-memory string has no target: the root is null and so is its
/// module's target.
#[test]
fn an_in_memory_root_is_originless() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(LEAF).expect("the module compiles");
    assert_eq!(
        artifact.source_closure,
        Some(SourceClosure {
            root: None,
            modules: vec![module(None, LEAF_SHA256)],
            digest: IN_MEMORY_DIGEST.to_string(),
        })
    );
}

/// A composition file is an originless root, listed first, followed by the
/// canonical modules it imports.
#[test]
fn a_composition_root_is_originless_and_lists_its_imports() {
    let temp = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-source-closure-{}",
            std::process::id()
        ));
    std::fs::remove_dir_all(&temp).ok();
    let root = temp.join("rulespec-us");
    let leaf = root.join("us/policies/identity/leaf.yaml");
    let composition = temp.join("generated/composition.yaml");
    std::fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    std::fs::create_dir_all(composition.parent().unwrap()).unwrap();
    std::fs::write(&leaf, LEAF).unwrap();
    std::fs::write(&composition, COMPOSITION).unwrap();
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");

    let composed = CompiledProgramArtifact::from_composed_rulespec_file(&composition, &roots)
        .expect("the composition compiles");
    assert_eq!(
        composed.source_closure,
        Some(SourceClosure {
            root: None,
            modules: vec![
                module(None, COMPOSITION_SHA256),
                module(Some(LEAF_TARGET), LEAF_SHA256),
            ],
            digest: COMPOSED_DIGEST.to_string(),
        })
    );

    // The filesystem loader records the same texts as a ModuleSource does.
    let atomic = CompiledProgramArtifact::from_rulespec_file(&leaf, &roots)
        .expect("the atomic module compiles from its file");
    assert_eq!(
        atomic
            .source_closure
            .as_ref()
            .map(|closure| &closure.modules),
        Some(&vec![module(Some(LEAF_TARGET), LEAF_SHA256)])
    );
    std::fs::remove_dir_all(&temp).ok();
}

/// A program compiled from a `ProgramSpec` has no RuleSpec source, so the
/// artifact omits `source_closure`.
#[test]
fn a_program_spec_has_no_source_closure() {
    let lowered = CompiledProgramArtifact::from_rulespec_str(LEAF).expect("the module compiles");
    let artifact =
        CompiledProgramArtifact::compile(lowered.program.clone()).expect("the program compiles");
    assert_eq!(artifact.source_closure, None);
    let value = serde_json::to_value(&artifact).unwrap();
    assert!(value.get("source_closure").is_none(), "{value}");
    assert_eq!(
        value["engine_build"],
        serde_json::to_value(EngineBuild::current()).unwrap()
    );

    let empty = CompiledProgramArtifact::compile(ProgramSpec::default()).unwrap();
    assert_eq!(empty.source_closure, None);
}

/// The same source, the same options and the same build give the same
/// bytes; a corpus provision index fills `source_url` after compilation and
/// leaves the closure alone.
#[test]
fn compilation_is_byte_deterministic_and_provisions_are_outside_the_closure() {
    let compile = || {
        CompiledProgramArtifact::from_rulespec_with_source(ROOT_TARGET, &modules())
            .expect("the module graph compiles")
    };
    let first = serde_json::to_string_pretty(&compile()).unwrap();
    let second = serde_json::to_string_pretty(&compile()).unwrap();
    assert_eq!(first, second);

    let mut cited = CompiledProgramArtifact::from_rulespec_str(
        &LEAF.replace(
            "format: rulespec/v1\n",
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/statute/7/2017/a\n",
        ),
    )
    .expect("the cited module compiles");
    let closure = cited.source_closure.clone();
    let mut provisions = CorpusProvisionIndex::default();
    provisions
        .add_jsonl_str(
            r#"{"citation_path": "us/statute/7/2017/a", "source_url": "https://example.test/7/2017/a"}"#,
            "<test>",
        )
        .unwrap();
    assert_eq!(cited.resolve_source_urls(&provisions), 1);
    assert_eq!(cited.source_closure, closure);
}

/// `engine_build` is the build's identity: a 40-hex-digit commit or null,
/// with `dirty` null whenever the commit is. Built from a git work tree whose
/// top is this crate, without the override, it names that tree's HEAD.
#[test]
fn engine_build_is_stamped_from_the_build() {
    let build = EngineBuild::current();
    let artifact = CompiledProgramArtifact::from_rulespec_str(LEAF).expect("the module compiles");
    assert_eq!(artifact.engine_build, Some(build.clone()));
    match &build.commit {
        Some(commit) => assert!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{commit}"
        ),
        None => assert_eq!(build.dirty, None),
    }

    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(crate_dir)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let top_level = git(&["rev-parse", "--show-toplevel"]);
    let is_top = top_level.is_some_and(|top| {
        std::fs::canonicalize(top).ok() == std::fs::canonicalize(crate_dir).ok()
    });
    if std::env::var_os("AXIOM_RULES_ENGINE_BUILD_COMMIT").is_none() && is_top {
        assert_eq!(build.commit, git(&["rev-parse", "HEAD"]));
        assert!(
            build.dirty.is_some(),
            "git can tell whether the tree is dirty"
        );
    }
}

/// The compile summary reports both identities.
#[test]
fn the_compile_summary_reports_the_identities() {
    let artifact = CompiledProgramArtifact::from_rulespec_with_source(ROOT_TARGET, &modules())
        .expect("the module graph compiles");
    let lines = compile_summary_lines(&artifact);
    assert_eq!(
        lines.get("engine_build"),
        Some(&EngineBuild::current().to_string())
    );
    assert_eq!(
        lines.get("source_closure_digest").map(String::as_str),
        Some(ATOMIC_DIGEST)
    );
    assert_eq!(
        lines.get("source_closure_modules").map(String::as_str),
        Some("3")
    );
}
