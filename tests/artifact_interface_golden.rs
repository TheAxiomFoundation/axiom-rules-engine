//! Frozen interfaces for the six small programs in `fixtures/artifacts/interface`.
//! Set `AXIOM_UPDATE_ARTIFACT_INTERFACE_FIXTURES=1` to regenerate them after an
//! intentional contract change. The format-3 derivation for already loadable
//! programs is frozen; changing these expectations requires format review.

use std::path::PathBuf;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

const FIXTURES: &[(&str, &str)] = &[
    ("two_entity_membership", "rulespec.yaml"),
    ("self_filter_and_unresolved_alias", "rulespec.yaml"),
    ("tax_unit_payments", "rulespec.yaml"),
    ("cross_entity_and_scalar", "rulespec.yaml"),
    ("dtype_default_domain", "program.json"),
    ("snap_boarder_election", "rulespec.yaml"),
];

/// Invariants 2(a) and 3: canonical artifacts round-trip byte for byte, and
/// the interface derivation stays equal to the independently reviewed golden
/// block for each fixture, including elections, aliases and every dtype row.
#[test]
fn compiled_interfaces_match_golden_blocks() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/artifacts/interface");
    let update =
        std::env::var("AXIOM_UPDATE_ARTIFACT_INTERFACE_FIXTURES").is_ok_and(|value| value == "1");
    assert!(
        !update || std::env::var_os("CI").is_none(),
        "fixture regeneration is refused under CI"
    );
    for &(name, extension) in FIXTURES {
        let source = std::fs::read_to_string(dir.join(format!("{name}.{extension}")))
            .expect("fixture is readable");
        let artifact = if extension == "program.json" {
            let program: ProgramSpec =
                serde_json::from_str(&source).expect("valid program fixture");
            CompiledProgramArtifact::compile(program)
        } else {
            CompiledProgramArtifact::from_rulespec_str(&source)
        }
        .unwrap_or_else(|error| panic!("{name}: {error}"));

        let bytes = serde_json::to_string_pretty(&artifact).unwrap();
        let loaded = CompiledProgramArtifact::from_json_str(&bytes)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            serde_json::to_string_pretty(&loaded).unwrap(),
            bytes,
            "{name}"
        );

        // Match the artifact goldens' build normalization before extracting
        // the block. The interface itself carries no build identity.
        let mut normalized = serde_json::to_value(&artifact).unwrap();
        normalized["engine_build"] = json!({"commit": null, "dirty": null});
        let block = normalized["program"]["interface"].clone();
        assert!(block.is_object(), "{name}: compiled interface is required");
        let pretty = format!("{}\n", serde_json::to_string_pretty(&block).unwrap());
        let path = dir.join(format!("{name}.interface.json"));
        if update {
            std::fs::write(&path, &pretty).expect("write interface golden");
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("{name}: {error}; regenerate with AXIOM_UPDATE_ARTIFACT_INTERFACE_FIXTURES=1")
        });
        let expected_block: Value = serde_json::from_str(&expected).expect("valid golden block");
        assert_eq!(block, expected_block, "{name}: interface contract changed");
        assert_eq!(pretty, expected, "{name}: canonical golden bytes changed");
    }
}
