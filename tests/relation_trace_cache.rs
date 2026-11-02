use axiom_rules_engine::api::{
    DerivedTraceNode, ExecutionMode, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::spec::ScalarValueSpec;
use serde_json::{Value, json};

fn literal(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn count(relation: &str) -> Value {
    json!({"kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1})
}

fn rule(name: &str, expr: Value) -> Value {
    json!({"name": name, "entity": "Node", "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": expr})
}

fn dependencies(node: &DerivedTraceNode) -> &[String] {
    match node {
        DerivedTraceNode::Scalar { dependencies, .. } => dependencies,
        _ => panic!("expected scalar trace"),
    }
}

#[test]
fn shared_candidate_fold_preserves_cached_relation_trace_for_a_later_query() {
    let mut head = count("owners");
    head["where"] = json!({"kind": "comparison", "left": count("selected"),
        "op": "gt", "right": literal(0)});
    let mut selected_count = count("items");
    selected_count["where"] = json!({"kind": "comparison",
        "left": {"kind": "derived", "name": "p"}, "op": "eq", "right": literal(1)});
    let mut relations = ["owners", "base", "items"]
        .into_iter()
        .map(|name| json!({"name": name, "arity": 2, "slot_entities": ["Node", "Node"]}))
        .collect::<Vec<_>>();
    relations.push(json!({"name": "selected", "arity": 2,
        "slot_entities": ["Node", "Node"],
        "derivation": {"source_relation": "base", "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Node", "Node"],
            "predicate": {"kind": "comparison", "left": selected_count,
                "op": "eq", "right": literal(1)}}}));
    let facts = [
        ("owners", "root", "A"),
        ("owners", "root", "B"),
        ("base", "A", "X"),
        ("base", "B", "X"),
        ("items", "X", "Y"),
    ]
    .into_iter()
    .map(|(name, left, right)| {
        json!({"name": name, "tuple": [left, right],
            "interval": {"start": "2026-01-01", "end": "2026-12-31"}})
    })
    .collect::<Vec<_>>();
    let period = json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"});
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain", "program": {"relations": relations, "derived": [
            rule("p", literal(1)), rule("head", head), rule("other", count("selected")),
        ]},
        "dataset": {"inputs": [], "relations": facts},
        "queries": [
            {"entity_id": "root", "period": period, "outputs": ["head"]},
            {"entity_id": "B", "period": period, "outputs": ["other"]},
        ],
    }))
    .unwrap();

    // The first query resolves selected(A) and selected(B) through the same X.
    // The later query must replay selected(B)'s dependency on p(Y), including
    // when a completed predicate fold was reused while filling that cache.
    for suspend_depth in [usize::MAX, 1] {
        let mut second_query_alone = request.clone();
        second_query_alone.queries.remove(0);
        let mut fast_request = request.clone();
        fast_request.mode = ExecutionMode::Fast;
        let (response, alone, fast) =
            axiom_rules_engine::depth::with_suspend_depth(suspend_depth, || {
                (
                    execute_request(request.clone()).expect("strictly typed request evaluates"),
                    execute_request(second_query_alone).expect("same second query evaluates alone"),
                    execute_request(fast_request).expect("same request evaluates in fast mode"),
                )
            });
        assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
        assert_eq!(fast.metadata.fallback_reason, None);
        for (index, name, expected) in [(0, "head", 2), (1, "other", 1)] {
            assert!(matches!(
                &response.results[index].outputs[name],
                OutputValue::Scalar {
                    value: ScalarValueSpec::Integer { value }, ..
                } if *value == expected
            ));
            assert_eq!(
                serde_json::to_value(&fast.results[index].outputs).unwrap(),
                serde_json::to_value(&response.results[index].outputs).unwrap(),
                "Explain and Fast values must agree at suspend depth {suspend_depth}",
            );
            assert!(fast.results[index].trace.is_empty());
        }
        let expected_dependencies = dependencies(&alone.results[0].trace["other"]);
        assert_eq!(
            expected_dependencies,
            ["p@entity:59"],
            "query alone must record p(Y)",
        );
        assert!(alone.results[0].trace.contains_key("p@entity:59"));
        assert_eq!(
            dependencies(&response.results[0].trace["head"]),
            expected_dependencies,
            "the first query did evaluate p(Y)",
        );
        assert_eq!(
            dependencies(&response.results[1].trace["other"]),
            expected_dependencies,
            "a prior query must not remove p(Y) from other(B)'s trace at suspend depth {suspend_depth}",
        );
        assert!(
            response.results[1].trace.contains_key("p@entity:59"),
            "the cached relation must retain the related rule trace node",
        );
    }
}
