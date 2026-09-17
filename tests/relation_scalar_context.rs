use axiom_rules_engine::api::{ExecutionMode, ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::spec::{JudgmentOutcomeSpec, ScalarValueSpec};

#[test]
fn related_scalar_comparison_preserves_both_entity_scopes_in_each_mode() {
    assert_correlated_predicate(false);
}

#[test]
fn related_scalar_context_survives_arithmetic_and_conditional_operands() {
    assert_correlated_predicate(true);
}

fn assert_correlated_predicate(nested_operand: bool) {
    for mode in [ExecutionMode::Fast, ExecutionMode::Explain] {
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/execution/relation-scalar-context.json"
        ))
        .expect("technical ProgramSpec fixture parses");
        if nested_operand {
            fixture["program"]["relations"][1]["derivation"]["predicate"]["right"] = serde_json::json!({
                "kind": "if",
                "condition": {
                    "kind": "comparison",
                    "left": {"kind": "derived", "name": "group_key"},
                    "op": "gt",
                    "right": {"kind": "literal", "value": {"kind": "integer", "value": 0}}
                },
                "then_expr": {
                    "kind": "add",
                    "items": [
                        {"kind": "derived", "name": "group_key"},
                        {"kind": "literal", "value": {"kind": "integer", "value": 0}}
                    ]
                },
                "else_expr": {"kind": "derived", "name": "group_key"}
            });
        }
        let mut request: ExecutionRequest =
            serde_json::from_value(fixture).expect("request parses");
        request.mode = mode.clone();
        let response = execute_request(request).expect("correlated scalar predicate executes");
        if nested_operand && mode == ExecutionMode::Fast {
            // Conditional scalar membership operands currently use the documented
            // generic fallback. Its values must still match an Explain request.
            assert_eq!(response.metadata.actual_mode, ExecutionMode::Explain);
            assert!(response.metadata.fallback_reason.is_some());
        } else {
            assert_eq!(response.metadata.actual_mode, mode);
            assert!(response.metadata.fallback_reason.is_none());
        }
        assert_eq!(response.results.len(), 4);
        for (row, (expected_matches, expected_records, expected_outcome)) in
            response.results.iter().zip([
                (2, 2, JudgmentOutcomeSpec::Holds),
                (1, 2, JudgmentOutcomeSpec::NotHolds),
                (0, 0, JudgmentOutcomeSpec::NotHolds),
                (0, 1, JudgmentOutcomeSpec::NotHolds),
            ])
        {
            for (name, expected) in [
                ("match_count", expected_matches),
                ("record_count", expected_records),
            ] {
                let OutputValue::Scalar {
                    value: ScalarValueSpec::Integer { value },
                    ..
                } = &row.outputs[name]
                else {
                    panic!("expected integer count");
                };
                assert_eq!(*value, expected, "{mode:?}: {} {name}", row.entity_id);
            }
            let OutputValue::Judgment { outcome, .. } = &row.outputs["all_records_match"] else {
                panic!("expected judgment");
            };
            assert_eq!(*outcome, expected_outcome, "{mode:?}: {}", row.entity_id);
            if response.metadata.actual_mode == ExecutionMode::Explain {
                let trace = serde_json::to_value(&row.trace).expect("trace serializes");
                let group_nodes: Vec<_> = trace
                    .as_object()
                    .expect("trace is a map")
                    .values()
                    .filter(|node| node["name"] == "group_key")
                    .collect();
                assert_eq!(group_nodes.len(), usize::from(expected_records > 0));
                for node in group_nodes {
                    assert_eq!(node["entity_id"], row.entity_id);
                }
            }
        }
    }
}
