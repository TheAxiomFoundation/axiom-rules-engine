//! AXIOM_RULESPEC_REPO_ROOTS may point at a country monorepo root (or its
//! parent); jurisdiction content resolves through the <prefix>/ directory.
//! Kept in its own integration-test binary because it mutates process env.

use axiom_rules_engine::compile::compile_program_file_to_json;

#[test]
fn env_root_at_monorepo_resolves_jurisdiction_dirs() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-monorepo-env-{}",
        std::process::id()
    ));
    let monorepo = temp_root.join("checkouts").join("rulespec-us");
    let us_path = monorepo.join("us/policies/base.yaml");
    // Importer lives OUTSIDE the monorepo, so only the env root can find it.
    let program_path = temp_root.join("elsewhere/program.yaml");
    let artifact_path = temp_root.join("program.compiled.json");

    std::fs::create_dir_all(us_path.parent().expect("us parent")).expect("us dir");
    std::fs::create_dir_all(program_path.parent().expect("program parent")).expect("program dir");
    std::fs::write(
        &us_path,
        r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
"#,
    )
    .expect("federal module is written");
    std::fs::write(
        &program_path,
        r#"
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
"#,
    )
    .expect("program is written");

    // Point at the monorepo root itself; resolution should try <root>/us/.
    unsafe { std::env::set_var("AXIOM_RULESPEC_REPO_ROOTS", &monorepo) };
    let result = compile_program_file_to_json(&program_path, &artifact_path);
    unsafe { std::env::remove_var("AXIOM_RULESPEC_REPO_ROOTS") };

    let artifact = result.expect("env monorepo root resolves us: imports");
    assert!(
        artifact
            .program
            .parameters
            .iter()
            .any(|parameter| parameter.name == "base_amount")
    );

    std::fs::remove_dir_all(&temp_root).ok();
}
