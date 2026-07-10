//! Exclusive RuleSpec root resolution must never fall through to ambient
//! ancestor/cwd/sibling repositories. Kept in one integration-test binary
//! because the direct `FsModuleSource` checks mutate process environment.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};

use axiom_rules_engine::rulespec::{
    AXIOM_RULESPEC_REPO_ROOTS_ENV, AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, RuleSpecError,
    load_rulespec_with_source,
};
use axiom_rules_engine::source::FsModuleSource;

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

fn restore_env(name: &str, previous: Option<OsString>) {
    // SAFETY: this integration-test binary contains one test and does not
    // spawn threads while mutating its process environment.
    unsafe {
        if let Some(value) = previous {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }
}

#[test]
fn exclusive_roots_block_ambient_fallback_and_form_a_cli_capability_handshake() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-exclusive-roots-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&temp_root).ok();

    // The importer lives under `temp_root/work`, so ordinary ancestor
    // discovery sees `temp_root/rulespec-us`. The configured monorepo starts
    // empty and therefore proves whether resolution falls through to ambient.
    let program_path = temp_root.join("work/program.yaml");
    let ambient_module = temp_root.join("rulespec-us/us/policies/base.yaml");
    let configured_monorepo = temp_root.join("work/configured/rulespec-us");
    std::fs::create_dir_all(program_path.parent().expect("program parent"))
        .expect("program directory");
    std::fs::create_dir_all(ambient_module.parent().expect("ambient parent"))
        .expect("ambient module directory");
    std::fs::create_dir_all(&configured_monorepo).expect("configured monorepo directory");
    std::fs::write(&program_path, PROGRAM_MODULE).expect("program module is written");
    std::fs::write(&ambient_module, BASE_MODULE).expect("ambient module is written");

    // FsModuleSource uses the same candidate-root policy as path-based
    // compilation: additive mode finds the ambient ancestor, exclusive mode
    // refuses it when the configured root lacks the module.
    let previous_roots = std::env::var_os(AXIOM_RULESPEC_REPO_ROOTS_ENV);
    let previous_exclusive = std::env::var_os(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV);
    // SAFETY: this integration-test binary contains one test and does not
    // spawn threads while mutating its process environment.
    unsafe {
        std::env::set_var(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_monorepo);
        std::env::remove_var(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV);
    }
    let source = FsModuleSource::new(&program_path);
    let additive_source_result = load_rulespec_with_source("us:policies/base", &source);
    // SAFETY: same single-threaded environment mutation described above.
    unsafe { std::env::set_var(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1") };
    let exclusive_source_result = load_rulespec_with_source("us:policies/base", &source);
    restore_env(AXIOM_RULESPEC_REPO_ROOTS_ENV, previous_roots);
    restore_env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, previous_exclusive);

    assert!(
        additive_source_result.is_ok(),
        "ordinary source resolution should retain ambient fallback: {additive_source_result:?}"
    );
    assert!(
        matches!(
            exclusive_source_result,
            Err(RuleSpecError::ModuleNotFound { ref target }) if target == "us:policies/base"
        ),
        "exclusive FsModuleSource must not load the ambient module: {exclusive_source_result:?}"
    );

    let additive_artifact = temp_root.join("additive.compiled.json");
    let additive_output = compile_command(&program_path, &additive_artifact)
        .env(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_monorepo)
        .env_remove(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV)
        .output()
        .expect("additive compile command runs");
    assert!(
        additive_output.status.success(),
        "ordinary resolution should preserve ambient fallback: {}",
        stderr(&additive_output)
    );

    let exclusive_artifact = temp_root.join("exclusive.compiled.json");
    let exclusive_output = compile_command(&program_path, &exclusive_artifact)
        .env(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_monorepo)
        .env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1")
        .output()
        .expect("exclusive env compile command runs");
    assert!(!exclusive_output.status.success());
    assert!(
        stderr(&exclusive_output).contains("could not be resolved"),
        "exclusive env mode should report the blocked import: {}",
        stderr(&exclusive_output)
    );
    assert!(!exclusive_artifact.exists());

    let invalid_mode_artifact = temp_root.join("invalid-mode.compiled.json");
    let invalid_mode_output = compile_command(&program_path, &invalid_mode_artifact)
        .env(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_monorepo)
        .env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "true")
        .output()
        .expect("invalid-exclusive-mode compile command runs");
    assert!(!invalid_mode_output.status.success());
    assert!(
        stderr(&invalid_mode_output)
            .contains("AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE must be exactly `1` when set"),
        "invalid exclusive mode must fail closed: {}",
        stderr(&invalid_mode_output)
    );
    assert!(!invalid_mode_artifact.exists());

    let no_roots_artifact = temp_root.join("no-roots.compiled.json");
    let no_roots_output = compile_command(&program_path, &no_roots_artifact)
        .env_remove(AXIOM_RULESPEC_REPO_ROOTS_ENV)
        .env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1")
        .output()
        .expect("missing-roots compile command runs");
    assert!(!no_roots_output.status.success());
    assert!(
        stderr(&no_roots_output).contains(
            "AXIOM_RULESPEC_REPO_ROOTS to contain at least one path and no empty entries"
        ),
        "exclusive mode must fail closed without configured roots: {}",
        stderr(&no_roots_output)
    );
    assert!(!no_roots_artifact.exists());

    let roots_with_empty_entry =
        std::env::join_paths([configured_monorepo.as_path(), Path::new("")])
            .expect("root list with empty entry");
    let empty_entry_artifact = temp_root.join("empty-entry.compiled.json");
    let empty_entry_output = compile_command(&program_path, &empty_entry_artifact)
        .env(AXIOM_RULESPEC_REPO_ROOTS_ENV, roots_with_empty_entry)
        .env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1")
        .output()
        .expect("empty-entry compile command runs");
    assert!(!empty_entry_output.status.success());
    assert!(
        stderr(&empty_entry_output).contains("at least one path and no empty entries"),
        "exclusive mode must reject empty configured-root entries: {}",
        stderr(&empty_entry_output)
    );
    assert!(!empty_entry_artifact.exists());

    // Once the module exists under the configured root, the CLI flag both
    // enables exclusive mode and compiles successfully. Passing the flag is a
    // capability handshake: older engines reject it as an unknown argument.
    let configured_module = configured_monorepo.join("us/policies/base.yaml");
    let decoy_monorepo = temp_root.join("work/decoy/rulespec-us");
    std::fs::create_dir_all(configured_module.parent().expect("configured parent"))
        .expect("configured module directory");
    std::fs::create_dir_all(&decoy_monorepo).expect("decoy monorepo directory");
    std::fs::write(&configured_module, BASE_MODULE).expect("configured module is written");
    let configured_roots =
        std::env::join_paths([decoy_monorepo.as_path(), configured_monorepo.as_path()])
            .expect("two configured RuleSpec roots");

    let previous_roots = std::env::var_os(AXIOM_RULESPEC_REPO_ROOTS_ENV);
    let previous_exclusive = std::env::var_os(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV);
    // SAFETY: same single-threaded environment mutation described above.
    unsafe {
        std::env::set_var(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_roots);
        std::env::set_var(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1");
    }
    let configured_source_result = load_rulespec_with_source("us:policies/base", &source);
    restore_env(AXIOM_RULESPEC_REPO_ROOTS_ENV, previous_roots);
    restore_env(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, previous_exclusive);
    assert!(
        configured_source_result.is_ok(),
        "exclusive FsModuleSource should load configured modules: {configured_source_result:?}"
    );

    let flag_artifact = temp_root.join("flag.compiled.json");
    let flag_output = compile_command(&program_path, &flag_artifact)
        .arg("--exclusive-rulespec-roots")
        .env(AXIOM_RULESPEC_REPO_ROOTS_ENV, &configured_roots)
        .env_remove(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV)
        .output()
        .expect("exclusive flag compile command runs");
    assert!(
        flag_output.status.success(),
        "configured-root import should compile in exclusive flag mode: {}",
        stderr(&flag_output)
    );
    assert!(
        std::fs::read_to_string(&flag_artifact)
            .expect("flag artifact is readable")
            .contains("us:policies/base#base_amount")
    );

    let help_output = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(["compile", "--help"])
        .output()
        .expect("compile help command runs");
    assert!(help_output.status.success());
    assert!(String::from_utf8_lossy(&help_output.stdout).contains("--exclusive-rulespec-roots"));

    std::fs::remove_dir_all(&temp_root).ok();
}
