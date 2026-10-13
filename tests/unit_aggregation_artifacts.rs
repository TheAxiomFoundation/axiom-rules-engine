//! Stage-3 aggregation artifacts across a rebuild and the format bump
//! (`docs/artifact-format.md`, "Stage-3 unit aggregation"). The phase-two
//! check compares executable content, so an embedded artifact's own
//! `engine_version` and `engine_build` never make it unloadable, while its
//! program and metadata must still be what this engine regenerates.

#![cfg(feature = "unit-derivation")]

use axiom_rules_engine::compile::{CompiledProgramArtifact, EngineBuild};
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::unit_derivation::{
    CompiledAggregationArtifact, EmbeddedArtifactLoading, UnitDerivationDocumentRegistry,
};
use serde_json::{Value, json};

const PLAN: &str = include_str!("fixtures/unit_derivation/nz_income_explorer_family.yaml");
const SOURCE_TARGET: &str = "nz:statutes/income_tax/family_scheme/tax_credits";
/// The unmodified stage-3 output of the engine at 6031295 (crate 0.2.2,
/// artifact format 2); see `tests/fast_path_metadata.rs`.
const PREVIOUS_ENGINE: &str =
    include_str!("fixtures/artifacts/nz_best_start_carer_sum.pre-lazy-fast-path.stage3.json");
const ANOTHER_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct NzGrossModuleSource;

impl ModuleSource for NzGrossModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok((target == SOURCE_TARGET).then(|| {
            include_str!("fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml").to_string()
        }))
    }
}

fn source_artifact() -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_with_source(SOURCE_TARGET, &NzGrossModuleSource)
        .expect("the NZ source compiles")
}

fn compile_stage3(source: &CompiledProgramArtifact) -> Value {
    let mut registry = UnitDerivationDocumentRegistry::default();
    let artifact = registry
        .register_aggregation_source(PLAN, source)
        .expect("the plan compiles against the source artifact");
    serde_json::from_str(&artifact.to_json_pretty().unwrap()).unwrap()
}

fn load(value: &Value) -> Result<CompiledAggregationArtifact, String> {
    CompiledAggregationArtifact::from_json_str(&value.to_string())
        .map_err(|error| error.to_string())
}

/// Invariant 9, first part. A stage-3 artifact whose embedded artifacts carry
/// another `engine_build` and `engine_version` loads: the source artifact's
/// identity is part of the serialization its digest covers, and the
/// phase-two artifact's is not compared with the running engine's.
#[test]
fn a_stage3_artifact_loads_whatever_build_wrote_its_embedded_artifacts() {
    let mut source = source_artifact();
    source.engine_version = Some("9.9.9".to_string());
    source.engine_build = Some(EngineBuild {
        commit: Some(ANOTHER_COMMIT.to_string()),
        dirty: Some(true),
    });
    let mut stage3 = compile_stage3(&source);
    assert_eq!(stage3["source_artifact"]["engine_version"], json!("9.9.9"));
    assert_eq!(
        stage3["source_artifact"]["engine_build"]["commit"],
        json!(ANOTHER_COMMIT)
    );
    stage3["phase_two_artifact"]["engine_version"] = json!("0.0.1");
    stage3["phase_two_artifact"]["engine_build"] = json!({"commit": null, "dirty": null});

    let loaded = load(&stage3).expect("another build's embedded artifacts load");
    assert_eq!(
        loaded.phase_two_artifact.engine_version.as_deref(),
        Some("0.0.1")
    );
    assert_eq!(
        loaded.phase_two_artifact.engine_build,
        Some(EngineBuild {
            commit: None,
            dirty: None,
        })
    );
    assert_eq!(
        serde_json::to_value(&loaded.source_artifact).unwrap(),
        stage3["source_artifact"]
    );
}

/// Invariant 9, second part. One whose phase-two `program` or `metadata`
/// differs from what this engine regenerates is refused, and the stored
/// phase-two artifact must pass its own loader.
#[test]
fn a_phase_two_artifact_with_other_executable_content_is_refused() {
    let stage3 = compile_stage3(&source_artifact());
    load(&stage3).expect("the untouched artifact loads");

    let mut program = stage3.clone();
    program["phase_two_artifact"]["program"]["relations"][0]["slot_entities"] =
        json!(["Household", "Person"]);
    let mut metadata = stage3.clone();
    metadata["phase_two_artifact"]["metadata"]["fast_path"]["strategy"] = json!("tampered");
    for (case, value) in [("program", program), ("metadata", metadata)] {
        let error = load(&value).expect_err(case);
        assert!(
            error.contains("embedded phase-two artifact does not match the registered plan"),
            "{case}: {error}"
        );
    }

    // Same program and metadata, but an interface its own loader refuses.
    let mut interface = stage3.clone();
    interface["phase_two_artifact"]["program"]["interface"]["relations"][0]["lookup_slots"] =
        json!([0]);
    let error = load(&interface).expect_err("the stored artifact must pass its loader");
    assert!(
        error.contains("production phase-two artifact validation failed"),
        "{error}"
    );
}

/// Invariant 9, third part. The artifact the previous engine wrote (both
/// embedded artifacts format 2) loads through the legacy path when the
/// caller opts in, with its source digest unchanged and without restamping
/// any `engine_version`; without the opt-in it is refused, naming the
/// embedded artifact as the older side.
#[test]
fn the_previous_engines_stage3_artifact_loads_through_the_legacy_path() {
    let stored: Value = serde_json::from_str(PREVIOUS_ENGINE).unwrap();
    assert_eq!(
        stored["source_artifact"]["artifact_format_version"],
        json!(2)
    );
    assert_eq!(
        stored["phase_two_artifact"]["artifact_format_version"],
        json!(2)
    );

    let refused = CompiledAggregationArtifact::from_json_str(PREVIOUS_ENGINE)
        .expect_err("the default loader reads format 3 only")
        .to_string();
    assert!(
        refused.contains("is older than this engine: it is artifact format 2"),
        "{refused}"
    );

    let legacy = EmbeddedArtifactLoading {
        accept_legacy_format_2: true,
    };
    let loaded = CompiledAggregationArtifact::from_json_str_with_loading(PREVIOUS_ENGINE, legacy)
        .expect("the previous engine's artifact loads through the legacy path");
    assert_eq!(
        json!(loaded.source_artifact_digest),
        stored["source_artifact_digest"]
    );
    assert_eq!(
        serde_json::to_value(&loaded.source_artifact).unwrap(),
        stored["source_artifact"]
    );
    assert_eq!(
        json!(loaded.phase_two_artifact.engine_version),
        stored["phase_two_artifact"]["engine_version"]
    );
    assert_eq!(loaded.source_artifact.interface, None);

    // An embedded phase-two artifact from any earlier version still loads.
    let mut older = stored.clone();
    older["phase_two_artifact"]["engine_version"] = json!("0.1.0");
    CompiledAggregationArtifact::from_json_str_with_loading(&older.to_string(), legacy)
        .expect("the phase-two engine_version is the stored artifact's own");

    // The registry keeps the opt-in for execution.
    let mut registry = UnitDerivationDocumentRegistry::default();
    let plan_id = registry
        .register_aggregation_json_with_loading(PREVIOUS_ENGINE, legacy)
        .expect("the registry loads it with the opt-in")
        .plan_id()
        .to_string();
    assert!(registry.aggregation(&plan_id).is_some());
    assert!(
        UnitDerivationDocumentRegistry::default()
            .register_aggregation_json(PREVIOUS_ENGINE)
            .is_err()
    );
}
