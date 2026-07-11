//! Adversarial coverage for the unconditional explicit-root filesystem contract.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::rulespec::{
    CanonicalRuleSpecRoots, RuleSpecError, load_rulespec_file, load_rulespec_with_source,
};
use axiom_rules_engine::source::FsModuleSource;

static NONCE: AtomicU64 = AtomicU64::new(0);

const BASE_MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: base_adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount
"#;

const PROGRAM_MODULE: &str = r#"
format: rulespec/v1
imports:
  - us:policies/base
rules:
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: base_amount + amount
"#;

fn temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-{label}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ))
}

fn canonical_fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let temp = temp_dir(label);
    let root = temp.join("rulespec-us");
    let base = root.join("us/policies/base.yaml");
    let program = root.join("us-co/policies/program.yaml");
    std::fs::create_dir_all(base.parent().expect("base parent")).expect("base dir");
    std::fs::create_dir_all(program.parent().expect("program parent")).expect("program dir");
    std::fs::write(base, BASE_MODULE).expect("base module");
    std::fs::write(&program, PROGRAM_MODULE).expect("program module");
    (temp, root, program)
}

fn compile_command(program: &Path, artifact: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"));
    command.args([
        "compile",
        "--program",
        program.to_str().expect("utf8 program path"),
        "--output",
        artifact.to_str().expect("utf8 artifact path"),
    ]);
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn cli_requires_explicit_repeatable_roots_and_ignores_legacy_environment() {
    let (temp, root, program) = canonical_fixture("cli-explicit-roots");
    let output = temp.join("program.compiled.json");

    let missing = compile_command(&program, &output)
        .env("AXIOM_RULESPEC_REPO_ROOTS", &root)
        .env("AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE", "1")
        .output()
        .expect("missing-root command");
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("at least one explicit rulespec-<country> root is required"),
        "{}",
        stderr(&missing)
    );
    assert!(!output.exists());

    let success = compile_command(&program, &output)
        .args(["--rulespec-root", root.to_str().expect("utf8 root")])
        .output()
        .expect("explicit-root command");
    assert!(success.status.success(), "{}", stderr(&success));
    assert!(output.exists());

    let duplicate = compile_command(&program, &temp.join("duplicate.json"))
        .args(["--rulespec-root", root.to_str().expect("utf8 root")])
        .args(["--rulespec-root", root.to_str().expect("utf8 root")])
        .output()
        .expect("duplicate-root command");
    assert!(!duplicate.status.success());
    assert!(stderr(&duplicate).contains("duplicate root"));

    let removed_flag = compile_command(&program, &temp.join("removed.json"))
        .arg("--exclusive-rulespec-roots")
        .output()
        .expect("removed-flag command");
    assert!(!removed_flag.status.success());
    assert!(stderr(&removed_flag).contains("unknown compile argument"));

    let help = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(["compile", "--help"])
        .output()
        .expect("help command");
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--rulespec-root"));
    assert!(!help.contains("--exclusive-rulespec-roots"));
    assert!(!help.contains("AXIOM_RULESPEC_REPO_ROOTS"));
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn filesystem_source_and_file_loader_share_the_exact_root_set() {
    let (temp, root, program) = canonical_fixture("source-root-set");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical roots");
    let source = FsModuleSource::new([&root]).expect("filesystem source");

    let from_source = load_rulespec_with_source("us-co:policies/program", &source)
        .expect("source loader compiles");
    let from_file = load_rulespec_file(&program, &roots).expect("file loader compiles");
    assert_eq!(
        serde_json::to_value(from_source).expect("source program JSON"),
        serde_json::to_value(from_file).expect("file program JSON")
    );

    let artifact = CompiledProgramArtifact::from_rulespec_file(&program, &roots)
        .expect("atomic artifact compiles");
    let entry = artifact
        .metadata
        .input_catalog
        .iter()
        .find(|entry| entry.slot == "amount")
        .expect("amount input catalog entry");
    assert_eq!(
        entry.canonical_request_name,
        "us-co:policies/program#input.amount"
    );
    assert_eq!(
        entry.request_names,
        [
            "us-co:policies/program#input.amount",
            "us:policies/base#input.amount"
        ]
    );
    let runtime = artifact.program.to_program().expect("runtime program");
    assert_eq!(runtime.resolve_input_name("amount"), None);
    assert_eq!(
        runtime
            .resolve_input_name("us-co:policies/program#input.amount")
            .as_deref(),
        Some("amount")
    );
    assert_eq!(
        runtime
            .resolve_input_name("us:policies/base#input.amount")
            .as_deref(),
        Some("amount"),
        "every actual owner remains an accepted request name"
    );
    assert_eq!(
        runtime.resolve_input_name("us:policies/fake#input.amount"),
        None,
        "an exact but non-owning module prefix must not alias the slot"
    );

    let outside = temp.join("outside.yaml");
    std::fs::write(&outside, PROGRAM_MODULE).expect("outside module");
    assert!(CompiledProgramArtifact::from_rulespec_file(&outside, &roots).is_err());
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn roots_reject_empty_duplicate_relative_aliased_and_noncanonical_checkouts() {
    assert!(CanonicalRuleSpecRoots::new(std::iter::empty::<&Path>()).is_err());

    let (temp, root, _) = canonical_fixture("invalid-root-shapes");
    assert!(CanonicalRuleSpecRoots::new([&root, &root]).is_err());
    let second_us = temp.join("second/rulespec-us");
    std::fs::create_dir_all(second_us.join("us/policies")).expect("second US checkout");
    assert!(CanonicalRuleSpecRoots::new([&root, &second_us]).is_err());

    let nested_uk = root.join("rulespec-uk");
    std::fs::create_dir_all(nested_uk.join("uk/policies")).expect("nested UK checkout");
    let overlap = CanonicalRuleSpecRoots::new([&root, &nested_uk])
        .expect_err("nested configured roots must fail");
    assert!(overlap.to_string().contains("overlapping roots"));
    assert!(CanonicalRuleSpecRoots::new([Path::new("rulespec-us")]).is_err());
    assert!(CanonicalRuleSpecRoots::new([root.join(".")]).is_err());

    let suffixed = temp.join("rulespec-us-feature-branch");
    std::fs::create_dir_all(suffixed.join("us/policies")).expect("suffixed checkout");
    assert!(CanonicalRuleSpecRoots::new([&suffixed]).is_err());

    let standalone = temp.join("rulespec-us-co");
    std::fs::create_dir_all(standalone.join("policies")).expect("standalone checkout");
    assert!(CanonicalRuleSpecRoots::new([&standalone]).is_err());

    let empty = temp.join("rulespec-uk");
    std::fs::create_dir_all(&empty).expect("empty checkout");
    assert!(CanonicalRuleSpecRoots::new([&empty]).is_err());
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn roots_reject_root_level_and_wrong_country_content() {
    let temp = temp_dir("invalid-content-shapes");
    let root_level = temp.join("root-level/rulespec-us");
    std::fs::create_dir_all(root_level.join("policies")).expect("root-level policies");
    assert!(CanonicalRuleSpecRoots::new([&root_level]).is_err());

    let wrong_country = temp.join("wrong-country/rulespec-us");
    std::fs::create_dir_all(wrong_country.join("uk/policies")).expect("wrong jurisdiction");
    assert!(CanonicalRuleSpecRoots::new([&wrong_country]).is_err());
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn programs_and_yml_are_never_atomic_modules() {
    let temp = temp_dir("programs-non-atomic");
    let root = temp.join("rulespec-us");
    let atomic = root.join("us/policies/base.yaml");
    let program = root.join("us/programs/snap.yaml");
    std::fs::create_dir_all(atomic.parent().expect("atomic parent")).expect("atomic dir");
    std::fs::create_dir_all(program.parent().expect("program parent")).expect("program dir");
    std::fs::write(&atomic, BASE_MODULE).expect("atomic module");
    std::fs::write(
        &program,
        "format: axiom-compose/program/v1\nprogram: snap\nmodules: []\n",
    )
    .expect("ProgramSpec");

    let roots =
        CanonicalRuleSpecRoots::new([&root]).expect("programs are valid filesystem content");
    assert!(matches!(
        load_rulespec_file(&program, &roots),
        Err(RuleSpecError::InvalidFilesystemPath { .. })
    ));
    let source = FsModuleSource::new([&root]).expect("filesystem source");
    assert!(matches!(
        load_rulespec_with_source("us:programs/snap", &source),
        Err(RuleSpecError::InvalidModuleTarget { .. })
    ));

    let companion = root.join("us/policies/base.test.yaml");
    std::fs::write(&companion, BASE_MODULE).expect("companion test file");
    assert!(matches!(
        load_rulespec_file(&companion, &roots),
        Err(RuleSpecError::InvalidFilesystemPath { .. })
    ));
    assert!(matches!(
        load_rulespec_with_source("us:policies/base.test", &source),
        Err(RuleSpecError::InvalidModuleTarget { .. })
    ));

    let yml = root.join("us/policies/legacy.yml");
    std::fs::write(&yml, BASE_MODULE).expect("legacy yml");
    assert!(CanonicalRuleSpecRoots::new([&root]).is_err());
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn roots_reject_ambiguous_extensions_and_reserved_path_aliases() {
    for (label, relative) in [
        ("double-yaml", "us/policies/foo.yaml.yaml"),
        ("double-yml", "us/policies/foo.yml.yaml"),
        ("uppercase-extension", "us/policies/foo.YAML"),
        ("mixed-extension", "us/policies/foo.Yml"),
        ("fragment-name", "us/policies/foo#bar.yaml"),
        ("space-name", "us/policies/foo bar.yaml"),
        ("quote-name", "us/policies/foo'.yaml"),
        ("colon-name", "us/policies/foo:bar.yaml"),
        ("at-name", "us/policies/foo@bar.yaml"),
        ("unicode-name", "us/policies/fóo.yaml"),
    ] {
        let temp = temp_dir(label);
        let root = temp.join("rulespec-us");
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture dir");
        std::fs::write(&path, BASE_MODULE).expect("invalid fixture");
        assert!(
            CanonicalRuleSpecRoots::new([&root]).is_err(),
            "non-canonical path must fail: {relative}"
        );
        std::fs::remove_dir_all(temp).ok();
    }
}

#[test]
fn roots_reject_case_variant_reserved_directories_on_case_sensitive_filesystems() {
    for (label, relative) in [
        ("case-jurisdiction", "US/policies"),
        ("case-content-root", "us/Policies"),
    ] {
        let temp = temp_dir(label);
        let root = temp.join("rulespec-us");
        std::fs::create_dir_all(root.join(relative)).expect("case-variant fixture");
        assert!(
            CanonicalRuleSpecRoots::new([&root]).is_err(),
            "case-variant reserved path must fail: {relative}"
        );
        std::fs::remove_dir_all(temp).ok();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn case_aliased_roots_and_module_paths_are_rejected_on_case_insensitive_filesystems() {
    let temp = temp_dir("case-alias-rejection");
    let actual_root = temp.join("RULESPEC-US");
    let actual_module = actual_root.join("us/policies/Base.yaml");
    std::fs::create_dir_all(actual_module.parent().expect("module parent")).expect("module dir");
    std::fs::write(&actual_module, BASE_MODULE).expect("module");

    let root_alias = temp.join("rulespec-us");
    if !root_alias.exists() {
        std::fs::remove_dir_all(temp).ok();
        return;
    }
    assert!(CanonicalRuleSpecRoots::new([&root_alias]).is_err());

    let intermediate = temp.join("root-spelling-transition");
    std::fs::rename(&actual_root, &intermediate).expect("move aliased root aside");
    std::fs::rename(&intermediate, &root_alias).expect("install canonical root spelling");
    let roots = CanonicalRuleSpecRoots::new([&root_alias]).expect("canonical root spelling");
    let module_alias = root_alias.join("us/policies/base.yaml");
    assert!(load_rulespec_file(&module_alias, &roots).is_err());
    let source = FsModuleSource::new([&root_alias]).expect("filesystem source");
    assert!(load_rulespec_with_source("us:policies/base", &source).is_err());

    let mis_cased_root = temp.join("mis-cased/rulespec-us");
    let mis_cased_content = mis_cased_root.join("us/Policies/base.yaml");
    std::fs::create_dir_all(mis_cased_content.parent().expect("content parent"))
        .expect("mis-cased content dir");
    std::fs::write(&mis_cased_content, BASE_MODULE).expect("mis-cased module");
    if mis_cased_root.join("us/policies").exists() {
        assert!(CanonicalRuleSpecRoots::new([&mis_cased_root]).is_err());
    }
    std::fs::remove_dir_all(temp).ok();
}

#[cfg(unix)]
#[test]
fn symlinked_roots_and_content_are_rejected() {
    use std::os::unix::fs::symlink;

    let (temp, root, _) = canonical_fixture("symlink-rejection");
    let alias_parent = temp.join("aliases");
    std::fs::create_dir_all(&alias_parent).expect("alias parent");
    let alias = alias_parent.join("rulespec-us");
    symlink(&root, &alias).expect("root symlink");
    assert!(CanonicalRuleSpecRoots::new([&alias]).is_err());

    let external = temp.join("external.yaml");
    std::fs::write(&external, BASE_MODULE).expect("external module");
    symlink(&external, root.join("us/policies/symlink.yaml")).expect("content symlink");
    assert!(CanonicalRuleSpecRoots::new([&root]).is_err());
    std::fs::remove_dir_all(temp).ok();
}
