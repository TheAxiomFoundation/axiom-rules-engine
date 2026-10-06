//! The axiom-compose bridge is explicit and cannot weaken atomic loading.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::rulespec::{CanonicalRuleSpecRoots, RuleSpecError};

const ATOMIC_MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: atomic_adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount
"#;

const COMPOSITION: &str = r#"
format: rulespec/v1
module:
  kind: composition
  summary: Deterministic test composition.
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

fn fixture(label: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let temp = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-composed-{label}-{}",
            std::process::id()
        ));
    std::fs::remove_dir_all(&temp).ok();
    let root = temp.join("rulespec-us");
    let atomic = root.join("us/policies/base.yaml");
    let composed = temp.join("generated/composition.yaml");
    std::fs::create_dir_all(atomic.parent().expect("atomic parent")).expect("atomic dir");
    std::fs::create_dir_all(composed.parent().expect("composed parent")).expect("composed dir");
    std::fs::write(&atomic, ATOMIC_MODULE).expect("atomic module");
    std::fs::write(&composed, COMPOSITION).expect("composition");
    (temp, root, atomic, composed)
}

fn compile_command(command: &str, program: &Path, root: &Path, output: &Path) -> Command {
    let mut process = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"));
    process.args([
        command,
        "--program",
        program.to_str().expect("utf8 program"),
        "--rulespec-root",
        root.to_str().expect("utf8 root"),
        "--output",
        output.to_str().expect("utf8 output"),
    ]);
    process
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn composed_compile_resolves_atomic_imports_and_keeps_root_rules_originless() {
    let (temp, root, _, composed) = fixture("success");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let artifact = CompiledProgramArtifact::from_composed_rulespec_file(&composed, &roots)
        .expect("composition compiles");

    assert_eq!(artifact.metadata.input_catalog.len(), 1);
    assert_eq!(artifact.metadata.input_catalog[0].slot, "amount");
    assert_eq!(
        artifact.metadata.input_catalog[0].canonical_request_name,
        "amount"
    );
    assert_eq!(
        artifact.metadata.input_catalog[0].request_names,
        ["amount", "us:policies/base#input.amount"]
    );
    let runtime = artifact.program.to_program().expect("runtime program");
    assert_eq!(
        runtime.resolve_input_name("amount").as_deref(),
        Some("amount")
    );
    assert_eq!(
        runtime.resolve_input_name("us:policies/fake#input.amount"),
        None,
        "originless inputs must not admit invented module prefixes"
    );

    assert!(
        artifact
            .program
            .parameters
            .iter()
            .any(|parameter| { parameter.id.as_deref() == Some("us:policies/base#base_amount") })
    );
    let adjusted = artifact
        .program
        .derived
        .iter()
        .find(|derived| derived.name == "adjusted_amount")
        .expect("synthesized output");
    assert_eq!(adjusted.id, None, "composed root rules must be originless");

    let output = temp.join("composition.compiled.json");
    let cli = compile_command("compile-composed", &composed, &root, &output)
        .output()
        .expect("compile-composed command");
    assert!(cli.status.success(), "{}", stderr(&cli));
    assert!(output.is_file());
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn atomic_and_composed_entry_points_are_not_interchangeable() {
    let (temp, root, atomic, composed) = fixture("separation");
    let in_root_composition = root.join("us/policies/composition.yaml");
    std::fs::write(&in_root_composition, COMPOSITION).expect("in-root composition");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");

    assert!(CompiledProgramArtifact::from_rulespec_file(&composed, &roots).is_err());
    assert!(
        CompiledProgramArtifact::from_rulespec_file(&in_root_composition, &roots).is_err(),
        "moving a composition under an atomic root must not make it atomic"
    );
    assert!(CompiledProgramArtifact::from_rulespec_str(COMPOSITION).is_err());
    assert!(CompiledProgramArtifact::from_composed_rulespec_file(&atomic, &roots).is_err());

    let atomic_cli = compile_command("compile", &composed, &root, &temp.join("atomic.json"))
        .output()
        .expect("atomic compile command");
    assert!(!atomic_cli.status.success());
    assert!(stderr(&atomic_cli).contains("outside every explicitly configured"));

    let in_root_cli = compile_command(
        "compile",
        &in_root_composition,
        &root,
        &temp.join("in-root-atomic.json"),
    )
    .output()
    .expect("in-root atomic compile command");
    assert!(!in_root_cli.status.success());
    assert!(stderr(&in_root_cli).contains("module.kind"));

    let composed_cli = compile_command(
        "compile-composed",
        &atomic,
        &root,
        &temp.join("composed.json"),
    )
    .output()
    .expect("composed compile command");
    assert!(!composed_cli.status.success());
    assert!(stderr(&composed_cli).contains("composed output must be outside"));
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn composed_compile_rejects_program_specs_atomic_sources_and_relative_dependencies() {
    let (temp, root, _, _) = fixture("negative-inputs");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let generated = temp.join("generated");

    let cases = [
        (
            "program-spec.yaml",
            "program: us/snap\nperiod: fy-2026\nscope: {}\n",
            "format must be exactly `rulespec/v1`",
        ),
        (
            "atomic-source.yaml",
            ATOMIC_MODULE,
            "module must be a mapping",
        ),
        (
            "wrong-kind.yaml",
            "format: rulespec/v1\nmodule:\n  kind: atomic\nrules: []\n",
            "module.kind must be exactly `composition`",
        ),
        (
            "identified.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\n  id: us:policies/fake\nrules: []\n",
            "module.id is forbidden",
        ),
        (
            "relative-import.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nimports: [../base]\nrules: []\n",
            "must be a canonical atomic module target",
        ),
        (
            "removed-extends.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nextends: us:policies/base\nrules: []\n",
            "top-level `extends` was removed",
        ),
        (
            "program-import.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nimports: [us:programs/snap]\nrules: []\n",
            "programs/ targets are forbidden",
        ),
        (
            "extension-alias.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nimports: [us:policies/base.yaml]\nrules: []\n",
            "must be a canonical atomic module target",
        ),
        (
            "fragment-alias.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nimports: [us:policies/base#base_amount]\nrules: []\n",
            "must be a canonical atomic module target",
        ),
        (
            "slash-alias.yaml",
            "format: rulespec/v1\nmodule:\n  kind: composition\nimports: [us:/policies/base]\nrules: []\n",
            "must be a canonical atomic module target",
        ),
        (
            "schema-alias.yaml",
            "schema: axiom.rules.module.v1\nmodule:\n  kind: composition\nrules: []\n",
            "format must be exactly `rulespec/v1`",
        ),
        (
            "dual-schema-alias.yaml",
            "format: rulespec/v1\nschema: axiom.rules.module.v1\nmodule:\n  kind: composition\nrules: []\n",
            "top-level `schema` was removed",
        ),
    ];

    for (name, source, expected) in cases {
        let path = generated.join(name);
        std::fs::write(&path, source).expect("negative fixture");
        let error = CompiledProgramArtifact::from_composed_rulespec_file(&path, &roots)
            .expect_err("invalid composition must fail");
        assert!(
            matches!(
                &error,
                axiom_rules_engine::compile::CompileError::RuleSpec {
                    error: RuleSpecError::InvalidComposedProgram { .. },
                    ..
                }
            ),
            "{name}: {error}"
        );
        assert!(error.to_string().contains(expected), "{name}: {error}");
    }
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn atomic_module_id_is_removed_in_favor_of_path_identity() {
    for id in [
        "us:policies/snap",
        "us:programs/snap",
        "us:policies/snap.yaml",
        "us:/policies/snap",
    ] {
        let source = format!("format: rulespec/v1\nmodule:\n  id: {id}\nrules: []\n");
        assert!(
            CompiledProgramArtifact::from_rulespec_str(&source).is_err(),
            "every module.id spelling must fail: {id}"
        );
    }

    let (temp, root, atomic, _) = fixture("module-id-removed");
    std::fs::write(
        &atomic,
        "format: rulespec/v1\nmodule:\n  id: us:policies/base\nrules: []\n",
    )
    .expect("identified module");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let error = CompiledProgramArtifact::from_rulespec_file(&atomic, &roots)
        .expect_err("module.id must fail even when it matches the path");
    assert!(error.to_string().contains("declares removed module.id"));
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn composed_compile_rejects_yaml_like_double_extension_without_redirecting() {
    let (temp, root, _, composed) = fixture("double-extension");
    let ambiguous = composed.with_file_name("composition.yaml.yaml");
    std::fs::write(&ambiguous, COMPOSITION).expect("ambiguous composition");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let error = CompiledProgramArtifact::from_composed_rulespec_file(&ambiguous, &roots)
        .expect_err("double-extension composition must fail");
    assert!(error.to_string().contains("double extension"));
    std::fs::remove_dir_all(temp).ok();
}
