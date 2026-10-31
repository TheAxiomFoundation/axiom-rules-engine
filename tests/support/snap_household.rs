#![allow(dead_code)]
//! Execute the unchanged section 8 fixture, with test-only set projections.
use std::collections::BTreeSet;
use std::sync::OnceLock;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileOptions, CompiledProgramArtifact};
use serde_json::{Value, json};

pub const SOURCE: &str = include_str!("../fixtures/snap_household.rulespec.yaml");

pub fn artifact() -> CompiledProgramArtifact {
    let artifact = CompiledProgramArtifact::from_rulespec_str_with_options(
        SOURCE,
        CompileOptions {
            strict_relation_entities: true,
            ..CompileOptions::default()
        },
    )
    .expect("section 8 compiles with strict relation entities");
    assert!(
        artifact.diagnostics.is_empty(),
        "{:?}",
        artifact.diagnostics
    );
    artifact
}

pub fn program() -> Value {
    static PROGRAM: OnceLock<Value> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let mut program = serde_json::to_value(artifact().program).unwrap();
        // Unique powers of two expose membership through public scalar outputs.
        // These projections are test instrumentation, outside the fixture.
        let rules = program["derived"].as_array_mut().unwrap();
        rules.push(json!({"name":"member_token","entity":"Person","dtype":"integer",
            "semantics":"scalar","expr":{"kind":"input","name":"member_token"}}));
        for (prefix, relation) in [
            ("household", "snap_household_member"),
            ("participating", "snap_participating_member"),
            ("tie", "snap_tie"),
            ("two_ties", "snap_within_two_ties"),
        ] {
            rules.push(json!({"name":format!("{prefix}_mask"),"entity":"Person","dtype":"integer",
                "semantics":"scalar","expr":{"kind":"sum_related","relation":relation,
                    "current_slot":0,"related_slot":1,"value":{"kind":"derived","name":"member_token"}}}));
            rules.push(json!({"name":format!("{prefix}_count"),"entity":"Person","dtype":"integer",
                "semantics":"scalar","expr":{"kind":"count_related","relation":relation,"current_slot":0,"related_slot":1}}));
        }
        program
    }).clone()
}

pub fn input(id: &str, name: &str, value: Value) -> Value {
    json!({"entity":"Person","entity_id":id,"name":name,
        "interval":{"start":"2026-01-01","end":"2026-12-31"},"value":value})
}
pub fn integer(value: i64) -> Value {
    json!({"kind":"integer","value":value})
}
pub fn judgment(value: bool) -> Value {
    json!({"kind":"bool","value":value})
}
pub fn tuple(relation: &str, left: &str, right: &str) -> Value {
    json!({"name":relation,"tuple":[left,right],"interval":{"start":"2026-01-01","end":"2026-12-31"}})
}

/// Compare complete response/trace bytes and failures at both rule depths.
pub fn run(
    mode: &str,
    program: &Value,
    data: &Value,
    applicants: &[&str],
    outputs: &[&str],
) -> Result<Value, String> {
    let queries = applicants
        .iter()
        .map(|id| {
            json!({"entity_id":id,"outputs":outputs,
        "period":{"period_kind":"month","start":"2026-01-01","end":"2026-01-31"}})
        })
        .collect::<Vec<_>>();
    let request: ExecutionRequest = serde_json::from_value(json!({"mode":mode,"program":program,
        "dataset":data,"queries":queries}))
    .unwrap();
    let at = |depth| {
        axiom_rules_engine::depth::with_suspend_depth(depth, || execute_request(request.clone()))
            .map(|response| serde_json::to_value(response).unwrap())
            .map_err(|error| error.to_string())
    };
    let deferred = at(1);
    assert_eq!(
        deferred,
        at(usize::MAX),
        "{mode}: suspend depth changed response"
    );
    deferred
}
pub fn number(response: &Value, row: usize, output: &str) -> i64 {
    let value = &response["results"][row]["outputs"][output]["value"]["value"];
    value
        .as_i64()
        .unwrap_or_else(|| value.as_str().unwrap().parse().unwrap())
}
pub fn members(response: &Value, row: usize, output: &str, ids: &[&str]) -> BTreeSet<String> {
    let mask = number(response, row, output);
    assert_eq!(mask >> ids.len(), 0, "mask names an unknown person");
    ids.iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, id)| (*id).to_owned())
        .collect()
}

pub fn permuted(data: &Value) -> Value {
    let mut data = data.clone();
    for key in ["inputs", "relations"] {
        data[key].as_array_mut().unwrap().reverse();
    }
    data
}
