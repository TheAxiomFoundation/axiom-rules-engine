//! Canonical country-monorepo loading keeps durable jurisdiction IDs while
//! resolving every module through one explicit validated root set.

use axiom_rules_engine::compile::compile_program_file_to_json;
use axiom_rules_engine::rulespec::CanonicalRuleSpecRoots;

const FEDERAL_MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: snap_maximum_allotment
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "298"
"#;

const STATE_MODULE: &str = r#"
format: rulespec/v1
imports:
  - us:policies/usda/snap/maximum-allotment
rules:
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment
"#;

#[test]
fn explicit_country_root_resolves_direct_matching_jurisdictions() {
    let temp = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-canonical-monorepo-{}",
            std::process::id()
        ));
    std::fs::remove_dir_all(&temp).ok();
    let root = temp.join("rulespec-us");
    let federal = root.join("us/policies/usda/snap/maximum-allotment.yaml");
    let state = root.join("us-co/policies/cdhs/snap/benefit.yaml");
    let output = temp.join("benefit.compiled.json");
    std::fs::create_dir_all(federal.parent().expect("federal parent")).expect("federal dir");
    std::fs::create_dir_all(state.parent().expect("state parent")).expect("state dir");
    std::fs::write(&federal, FEDERAL_MODULE).expect("federal module");
    std::fs::write(&state, STATE_MODULE).expect("state module");

    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical country root");
    let artifact = compile_program_file_to_json(&state, &output, &roots)
        .expect("state module resolves federal import");

    assert!(artifact.program.parameters.iter().any(|parameter| {
        parameter.id.as_deref()
            == Some("us:policies/usda/snap/maximum-allotment#snap_maximum_allotment")
    }));
    assert!(artifact.program.derived.iter().any(|derived| {
        derived.id.as_deref()
            == Some("us-co:policies/cdhs/snap/benefit#snap_regular_month_allotment")
    }));
    std::fs::remove_dir_all(temp).ok();
}
