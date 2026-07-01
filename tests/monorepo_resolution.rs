//! Country-monorepo layout: one repo named rulespec-<country> holding a
//! directory per jurisdiction (us/, us-co/, …). Durable IDs must be
//! identical to the legacy sibling-checkout layout.

use axiom_rules_engine::compile::compile_program_file_to_json;
use axiom_rules_engine::spec::ScalarValueSpec;

const FEDERAL_MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: snap_maximum_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment_table[household_size]
"#;

const STATE_MODULE: &str = r#"
format: rulespec/v1
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
rules:
  - name: snap_household_food_contribution_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2025-10-01
        formula: "0.30"
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: floor(snap_maximum_allotment - (net_income * snap_household_food_contribution_rate))
"#;

const FEDERAL_UTILITY_HOOK_MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: snap_utility_allowance_delegation
    kind: source_relation
    source_relation:
      type: delegates
      target: us:regulations/7-cfr/273/9#snap_total_allowable_shelter_expenses
      authority: federal
  - name: snap_standard_utility_allowance_state_option
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "0"
  - name: snap_total_allowable_shelter_expenses
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: household_shelter_costs + snap_standard_utility_allowance_state_option
"#;

const STATE_UTILITY_SET_MODULE: &str = r#"
format: rulespec/v1
imports:
  - us:regulations/7-cfr/273/9
rules:
  - name: sets_snap_standard_utility_allowance
    kind: source_relation
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#snap_standard_utility_allowance_state_option
      authority: state
      value: us-co:regulations/10-ccr-2506-1/4.407.31#snap_standard_utility_allowance
      basis:
        delegation: us:regulations/7-cfr/273/9#snap_utility_allowance_delegation
  - name: snap_standard_utility_allowance
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "594"
"#;

#[test]
fn monorepo_jurisdiction_dirs_resolve_with_unchanged_ids() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-monorepo-resolution-{}",
        std::process::id()
    ));
    let us_path = temp_root
        .join("rulespec-us")
        .join("us/policies/usda/snap/fy-2026-cola/maximum-allotments.yaml");
    let co_path = temp_root
        .join("rulespec-us")
        .join("us-co/policies/cdhs/snap/fy-2026-benefit.yaml");
    let artifact_path = temp_root.join("benefit.compiled.json");

    std::fs::create_dir_all(us_path.parent().expect("us parent")).expect("us dir");
    std::fs::create_dir_all(co_path.parent().expect("co parent")).expect("co dir");
    std::fs::write(&us_path, FEDERAL_MODULE).expect("federal module is written");
    std::fs::write(&co_path, STATE_MODULE).expect("state module is written");

    let artifact = compile_program_file_to_json(&co_path, &artifact_path)
        .expect("state module inside a country monorepo resolves federal imports");

    assert!(artifact.program.parameters.iter().any(|parameter| {
        parameter.id.as_deref()
            == Some(
                "us:policies/usda/snap/fy-2026-cola/maximum-allotments#snap_maximum_allotment_table",
            )
    }));
    assert!(artifact.program.derived.iter().any(|derived| {
        derived.id.as_deref()
            == Some("us-co:policies/cdhs/snap/fy-2026-benefit#snap_regular_month_allotment")
    }));

    std::fs::remove_dir_all(&temp_root).ok();
}

#[cfg(unix)]
#[test]
fn symlinked_configured_roots_preserve_logical_rulespec_ids() {
    use std::os::unix::fs::symlink;

    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-symlink-root-{}",
        std::process::id()
    ));
    let physical_federal = temp_root.join("axiom-rulespec-us-main");
    let configured_parent = temp_root.join("configured-roots");
    let state_repo = temp_root.join("rulespec-us-co");
    let federal_module = physical_federal.join("us/regulations/7-cfr/273/9.yaml");
    let state_module = state_repo.join("regulations/10-ccr-2506-1/4.407.31.yaml");
    let artifact_path = temp_root.join("utility.compiled.json");

    std::fs::create_dir_all(federal_module.parent().expect("federal parent")).expect("federal dir");
    std::fs::create_dir_all(state_module.parent().expect("state parent")).expect("state dir");
    std::fs::create_dir_all(&configured_parent).expect("configured parent dir");
    std::fs::write(&federal_module, FEDERAL_UTILITY_HOOK_MODULE)
        .expect("federal utility hook module is written");
    std::fs::write(&state_module, STATE_UTILITY_SET_MODULE)
        .expect("state utility set module is written");
    symlink(&physical_federal, configured_parent.join("rulespec-us"))
        .expect("configured rulespec-us symlink is created");

    let previous_roots = std::env::var_os("AXIOM_RULESPEC_REPO_ROOTS");
    unsafe { std::env::set_var("AXIOM_RULESPEC_REPO_ROOTS", &configured_parent) };
    let artifact = compile_program_file_to_json(&state_module, &artifact_path)
        .expect("state sets relation resolves federal hook through symlinked configured root");
    if let Some(previous_roots) = previous_roots {
        unsafe { std::env::set_var("AXIOM_RULESPEC_REPO_ROOTS", previous_roots) };
    } else {
        unsafe { std::env::remove_var("AXIOM_RULESPEC_REPO_ROOTS") };
    }

    assert!(artifact.program.derived.iter().any(|derived| {
        derived.id.as_deref()
            == Some("us:regulations/7-cfr/273/9#snap_standard_utility_allowance_state_option")
    }));

    std::fs::remove_dir_all(&temp_root).ok();
}

#[test]
fn federal_content_under_country_dir_keeps_us_prefix() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-monorepo-federal-{}",
        std::process::id()
    ));
    let us_module = temp_root
        .join("rulespec-us")
        .join("us/policies/usda/snap/fy-2026-cola/maximum-allotments.yaml");
    let artifact_path = temp_root.join("federal.compiled.json");

    std::fs::create_dir_all(us_module.parent().expect("us parent")).expect("us dir");
    std::fs::write(&us_module, FEDERAL_MODULE).expect("federal module is written");

    let artifact = compile_program_file_to_json(&us_module, &artifact_path)
        .expect("federal module compiles under monorepo layout");
    assert!(artifact.program.parameters.iter().any(|parameter| {
        parameter.id.as_deref()
            == Some(
                "us:policies/usda/snap/fy-2026-cola/maximum-allotments#snap_maximum_allotment_table",
            )
    }));

    std::fs::remove_dir_all(&temp_root).ok();
}

#[test]
fn suffixed_country_monorepo_worktree_uses_local_imports_and_country_ids() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-monorepo-worktree-{}",
        std::process::id()
    ));
    let worktree = temp_root.join("rulespec-us-medicaid-primary-categories-20260630");
    let root_module = worktree.join("us/statutes/42/1396a/a/10.yaml");
    let local_import = worktree.join("us/statutes/42/1396a/m.yaml");
    let stale_sibling_import = temp_root.join("rulespec-us/us/statutes/42/1396a/m.yaml");
    let artifact_path = temp_root.join("medicaid.compiled.json");

    std::fs::create_dir_all(root_module.parent().expect("root parent")).expect("root dir");
    std::fs::create_dir_all(local_import.parent().expect("local import parent"))
        .expect("local import dir");
    std::fs::create_dir_all(stale_sibling_import.parent().expect("stale import parent"))
        .expect("stale import dir");
    std::fs::write(
        &local_import,
        r#"
format: rulespec/v1
rules:
  - name: optional_category_amount
    kind: parameter
    dtype: Count
    versions:
      - effective_from: 2026-01-01
        formula: "2"
"#,
    )
    .expect("local imported module is written");
    std::fs::write(
        &stale_sibling_import,
        r#"
format: rulespec/v1
rules:
  - name: optional_category_amount
    kind: parameter
    dtype: Count
    versions:
      - effective_from: 2026-01-01
        formula: "9"
"#,
    )
    .expect("stale sibling imported module is written");
    std::fs::write(
        &root_module,
        r#"
format: rulespec/v1
imports:
  - us:statutes/42/1396a/m#optional_category_amount
rules:
  - name: medicaid_category_marker
    kind: derived
    entity: Person
    dtype: Count
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: optional_category_amount
"#,
    )
    .expect("root module is written");

    let artifact = compile_program_file_to_json(&root_module, &artifact_path)
        .expect("suffixed country worktree compiles with local imports");

    let amount = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| {
            parameter.id.as_deref() == Some("us:statutes/42/1396a/m#optional_category_amount")
        })
        .expect("local imported parameter keeps us id");
    assert!(matches!(
        amount.versions[0].values.get(&0),
        Some(ScalarValueSpec::Integer { value: 2 })
    ));
    assert!(artifact.program.derived.iter().any(|derived| {
        derived.id.as_deref() == Some("us:statutes/42/1396a/a/10#medicaid_category_marker")
    }));

    std::fs::remove_dir_all(&temp_root).ok();
}
