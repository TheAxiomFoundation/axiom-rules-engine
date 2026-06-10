//! Host-supplied module resolution: full compile + execute over modules held
//! in an in-memory map, with zero filesystem access. Durable ids must be
//! identical to the filesystem jurisdiction-repo layouts.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::rulespec::{
    RuleSpecError, load_rulespec_with_source, resolve_import_target,
};
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

/// A `ModuleSource` over an in-memory map: the shape a wasm host, a server
/// holding modules in memory, or a registry client would implement.
struct InMemoryModuleSource {
    modules: HashMap<String, String>,
}

impl InMemoryModuleSource {
    fn new(modules: &[(&str, &str)]) -> Self {
        Self {
            modules: modules
                .iter()
                .map(|(target, text)| (target.to_string(), text.to_string()))
                .collect(),
        }
    }
}

impl ModuleSource for InMemoryModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.modules.get(target).cloned())
    }
}

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
fn in_memory_module_source_compiles_and_executes_without_filesystem() {
    let source = InMemoryModuleSource::new(&[
        (
            "us:policies/usda/snap/fy-2026-cola/maximum-allotments",
            FEDERAL_MODULE,
        ),
        ("us-co:policies/cdhs/snap/fy-2026-benefit", STATE_MODULE),
    ]);

    let program = load_rulespec_with_source("us-co:policies/cdhs/snap/fy-2026-benefit", &source)
        .expect("in-memory modules load and lower");
    assert!(program.parameters.iter().any(|parameter| {
        parameter.id.as_deref()
            == Some(
                "us:policies/usda/snap/fy-2026-cola/maximum-allotments#snap_maximum_allotment_table",
            )
    }));

    let artifact =
        CompiledProgramArtifact::from_rulespec_with_source(
            "us-co:policies/cdhs/snap/fy-2026-benefit",
            &source,
        )
        .expect("in-memory modules compile");
    let output_id =
        "us-co:policies/cdhs/snap/fy-2026-benefit#snap_regular_month_allotment".to_string();
    assert!(
        artifact
            .program
            .derived
            .iter()
            .any(|derived| derived.id.as_deref() == Some(output_id.as_str()))
    );

    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name:
                        "us:policies/usda/snap/fy-2026-cola/maximum-allotments#input.household_size"
                            .to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Integer { value: 1 },
                },
                InputRecordSpec {
                    name: "us-co:policies/cdhs/snap/fy-2026-benefit#input.net_income".to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Decimal {
                        value: "100".to_string(),
                    },
                },
            ],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("in-memory program executes");

    let OutputValue::Scalar { name, id, value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_regular_month_allotment output")
    else {
        panic!("expected scalar output");
    };
    assert_eq!(name, "snap_regular_month_allotment");
    assert_eq!(id.as_deref(), Some(output_id.as_str()));
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "268");
}

#[test]
fn relative_imports_resolve_against_the_importers_canonical_target() {
    let source = InMemoryModuleSource::new(&[
        (
            "us:statutes/7/2015/e",
            r#"
format: rulespec/v1
imports:
  - ../2014/base
  - ./peer.yaml
rules:
  - name: total_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: base_amount * peer_rate
"#,
        ),
        (
            "us:statutes/7/2014/base",
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
        ),
        (
            "us:statutes/7/2015/peer",
            r#"
format: rulespec/v1
rules:
  - name: peer_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2026-01-01
        formula: "2"
"#,
        ),
    ]);

    let program = load_rulespec_with_source("us:statutes/7/2015/e", &source)
        .expect("relative imports resolve through the importer's canonical target");
    assert!(program.parameters.iter().any(|parameter| {
        parameter.id.as_deref() == Some("us:statutes/7/2014/base#base_amount")
    }));
    assert!(program.parameters.iter().any(|parameter| {
        parameter.id.as_deref() == Some("us:statutes/7/2015/peer#peer_rate")
    }));
    assert!(program.derived.iter().any(|derived| {
        derived.id.as_deref() == Some("us:statutes/7/2015/e#total_amount")
    }));
}

#[test]
fn diamond_imports_load_each_module_once() {
    let shared = r#"
format: rulespec/v1
rules:
  - name: shared_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "5"
"#;
    let source = InMemoryModuleSource::new(&[
        (
            "us:policies/root",
            r#"
format: rulespec/v1
imports:
  - us:policies/left
  - us:policies/right
rules:
  - name: combined
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: left_amount + right_amount
"#,
        ),
        (
            "us:policies/left",
            r#"
format: rulespec/v1
imports:
  - us:policies/shared
rules:
  - name: left_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: shared_amount + 1
"#,
        ),
        (
            "us:policies/right",
            r#"
format: rulespec/v1
imports:
  - us:policies/shared
rules:
  - name: right_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: shared_amount + 2
"#,
        ),
        ("us:policies/shared", shared),
    ]);

    // A duplicated `shared` module would produce duplicate parameter
    // definitions; compilation succeeding proves the diamond deduplicated.
    let artifact = CompiledProgramArtifact::from_rulespec_with_source("us:policies/root", &source)
        .expect("diamond imports compile once each");
    assert_eq!(
        artifact
            .program
            .parameters
            .iter()
            .filter(|parameter| parameter.name == "shared_amount")
            .count(),
        1
    );
}

#[test]
fn import_cycles_are_detected_without_filesystem() {
    let source = InMemoryModuleSource::new(&[
        (
            "us:policies/a",
            r#"
format: rulespec/v1
imports:
  - us:policies/b
rules:
  - name: a_amount
    kind: parameter
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: "1"
"#,
        ),
        (
            "us:policies/b",
            r#"
format: rulespec/v1
imports:
  - us:policies/a
rules:
  - name: b_amount
    kind: parameter
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: "2"
"#,
        ),
    ]);

    let error = load_rulespec_with_source("us:policies/a", &source)
        .expect_err("cyclic imports are rejected");
    assert!(matches!(error, RuleSpecError::ImportCycle { .. }), "{error}");
}

#[test]
fn missing_modules_are_reported_with_importer_context() {
    let source = InMemoryModuleSource::new(&[(
        "us:policies/root",
        r#"
format: rulespec/v1
imports:
  - us:policies/absent
rules:
  - name: amount
    kind: parameter
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: "1"
"#,
    )]);

    let error = load_rulespec_with_source("us:policies/root", &source)
        .expect_err("missing imported module errors");
    let RuleSpecError::ModuleNotFound { target } = &error else {
        panic!("expected ModuleNotFound, got {error}");
    };
    assert_eq!(target, "us:policies/absent");

    let error = load_rulespec_with_source("us:policies/never-written", &source)
        .expect_err("missing root module errors");
    assert!(matches!(error, RuleSpecError::ModuleNotFound { .. }), "{error}");
}

/// `FsModuleSource` + the pure loader must agree with the path-based loader
/// on the same country-monorepo checkout.
#[cfg(feature = "fs")]
#[test]
fn fs_module_source_matches_file_loading_on_a_monorepo_checkout() {
    use axiom_rules_engine::source::FsModuleSource;

    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-fs-module-source-{}",
        std::process::id()
    ));
    let us_path = temp_root
        .join("rulespec-us")
        .join("us/policies/usda/snap/fy-2026-cola/maximum-allotments.yaml");
    let co_path = temp_root
        .join("rulespec-us")
        .join("us-co/policies/cdhs/snap/fy-2026-benefit.yaml");
    std::fs::create_dir_all(us_path.parent().expect("us parent")).expect("us dir");
    std::fs::create_dir_all(co_path.parent().expect("co parent")).expect("co dir");
    std::fs::write(&us_path, FEDERAL_MODULE).expect("federal module is written");
    std::fs::write(&co_path, STATE_MODULE).expect("state module is written");

    let source = FsModuleSource::new(&co_path);
    let from_source =
        load_rulespec_with_source("us-co:policies/cdhs/snap/fy-2026-benefit", &source)
            .expect("FsModuleSource resolves the monorepo checkout");
    let from_file = axiom_rules_engine::rulespec::load_rulespec_file(&co_path)
        .expect("path-based loading resolves the monorepo checkout");

    assert_eq!(
        serde_json::to_value(&from_source).expect("source program serialises"),
        serde_json::to_value(&from_file).expect("file program serialises"),
    );

    std::fs::remove_dir_all(&temp_root).ok();
}

#[test]
fn import_targets_resolve_as_pure_string_logic() {
    // Canonical imports pass through normalized: fragments and extensions drop.
    assert_eq!(
        resolve_import_target("us:statutes/7/2015/e", "us-co:policy/x.yaml#fragment")
            .expect("canonical import resolves"),
        "us-co:policy/x"
    );
    // Relative imports resolve against the importer's directory.
    assert_eq!(
        resolve_import_target("us:statutes/7/2015/e", "../2014/base")
            .expect("parent-relative import resolves"),
        "us:statutes/7/2014/base"
    );
    assert_eq!(
        resolve_import_target("us:statutes/7/2015/e", "./6/A.yaml")
            .expect("dot-relative import resolves"),
        "us:statutes/7/2015/6/A"
    );
    // Escaping above the jurisdiction root is rejected.
    assert!(resolve_import_target("us:statutes/e", "../../escape").is_err());
    // Absolute filesystem paths have no meaning without a filesystem.
    assert!(resolve_import_target("us:statutes/e", "/etc/passwd").is_err());
}
