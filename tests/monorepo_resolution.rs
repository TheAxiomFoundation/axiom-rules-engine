//! Country-monorepo layout: one repo named rulespec-<country> holding a
//! directory per jurisdiction (us/, us-co/, …). Durable IDs must be
//! identical to the legacy sibling-checkout layout.

use axiom_rules_engine::compile::compile_program_file_to_json;

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
