//! Finite existential domains, independent value oracles, and public execution contracts.
use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

fn integer(n: i64) -> Value {
    json!({"kind":"literal", "value":{"kind":"integer", "value":n}})
}
fn truth(holds: bool) -> Value {
    json!({"kind":"comparison", "left":integer(0), "op":if holds {"eq"} else {"ne"}, "right":integer(0)})
}
fn variable(index: usize) -> Value {
    json!({"var": index})
}
fn read(term: Value, name: &str) -> Value {
    json!({"kind":"role_derived", "term":term, "name":name})
}
fn positive(term: Value) -> Value {
    json!({"kind":"comparison", "left":read(term, "score"), "op":"gt", "right":integer(0)})
}
fn exists(relation: &str, terms: Vec<Value>, slot: usize, body: Value) -> Value {
    json!({"kind":"exists", "relation":relation, "terms":terms, "variable_slot":slot, "body":body})
}
fn not(item: Value) -> Value {
    json!({"kind":"not", "item":item})
}
fn derived(name: &str, source: &str, predicate: Value) -> Value {
    json!({"name":name, "arity":2, "slot_entities":["Person","Person"], "derivation":{
        "source_relation":source,"current_slot":0,"related_slot":1,
        "slot_entities":["Person","Person"],"predicate":predicate}})
}
fn program(predicate: Value) -> Value {
    json!({"relations":[
        {"name":"roster", "arity":2,"slot_entities":["Person","Person"]},
        {"name":"edge", "arity":2,"slot_entities":["Person","Person"]},
        derived("selected", "roster", predicate),
        derived("eligible", "edge", positive(json!("candidate"))),
    ], "derived":[
        {"name":"total", "entity":"Person", "dtype":"integer", "semantics":"scalar",
            "expr":{"kind":"count_related", "relation":"selected", "current_slot":0,"related_slot":1}},
        {"name":"score", "entity":"Person", "dtype":"integer", "semantics":"scalar",
            "expr":{"kind":"input", "name":"points"}},
    ]})
}
fn tuple(relation: &str, ids: &[&str]) -> Value {
    json!({"name":relation,"tuple":ids,"interval":{"start":"2026-01-01","end":"2026-12-31"}})
}
fn input(id: &str, n: i64) -> Value {
    json!({"name":"points", "entity":"Person", "entity_id":id,
        "interval":{"start":"2026-01-01","end":"2026-12-31"},"value":{"kind":"integer","value":n}})
}
fn run(mode: &str, program: &Value, data: &Value, ids: &[String]) -> Result<Value, String> {
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode":mode,"program":program,"dataset":data,
        "queries":ids.iter().map(|id| json!({"entity_id":id,
            "period":{"period_kind":"month","start":"2026-01-01","end":"2026-01-31"},
            "outputs":["total"]})).collect::<Vec<_>>()
    }))
    .unwrap();
    execute_request(request)
        .map(|response| serde_json::to_value(response).unwrap())
        .map_err(|error| error.to_string())
}
fn count(response: &Value, row: usize) -> i64 {
    response["results"][row]["outputs"]["total"]["value"]["value"]
        .as_i64()
        .unwrap()
}
fn query_ids() -> Vec<String> {
    vec!["a".into()]
}

#[test]
fn exists_and_for_all_cover_empty_domains_and_both_generator_directions() {
    let data = json!({"inputs":[input("p", 65),input("q", 17)], "relations":[
        tuple("roster", &["a","b"]),tuple("roster", &["a","c"]),
        tuple("edge", &["a","p"]),tuple("edge", &["a","q"]),tuple("edge", &["p","b"])
    ]});
    for (terms, slot, body, expected) in [
        (
            vec![json!("anchor"), variable(0)],
            1,
            positive(variable(0)),
            2,
        ),
        (
            vec![variable(0), json!("candidate")],
            0,
            positive(variable(0)),
            1,
        ),
        (
            vec![json!("candidate"), variable(0)],
            1,
            positive(variable(0)),
            0,
        ),
    ] {
        for mode in ["explain", "fast"] {
            let existential = exists("edge", terms.clone(), slot, body.clone());
            assert_eq!(
                count(
                    &run(mode, &program(existential), &data, &query_ids()).unwrap(),
                    0
                ),
                expected
            );
            let universal = not(exists("edge", terms.clone(), slot, not(body.clone())));
            assert_eq!(
                count(
                    &run(mode, &program(universal), &data, &query_ids()).unwrap(),
                    0
                ),
                2
            );
        }
    }
    let all_adults = json!({"kind":"comparison", "left":read(variable(0),"score"), "op":"gte", "right":integer(65)});
    for mode in ["explain", "fast"] {
        let universal = not(exists(
            "edge",
            vec![json!("anchor"), variable(0)],
            1,
            not(all_adults.clone()),
        ));
        assert_eq!(
            count(
                &run(mode, &program(universal), &data, &query_ids()).unwrap(),
                0
            ),
            0
        );
    }
}

#[test]
fn data_domains_check_every_bound_slot_period_and_deduplicate() {
    let body = positive(variable(0));
    for variable_slot in 0..3 {
        let mut terms = vec![json!("anchor"), json!("candidate"), json!("anchor")];
        terms[variable_slot] = variable(0);
        let mut p = program(exists("triple", terms, variable_slot, body.clone()));
        p["relations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"triple","arity":3,"slot_entities":["Person","Person","Person"]}));
        let mut matching = ["a", "b", "a"];
        matching[variable_slot] = "p";
        let mut decoy = matching;
        decoy[(variable_slot + 1) % 3] = "wrong";
        let mut partial = tuple("triple", &matching);
        partial["tuple"][variable_slot] = json!("missing");
        partial["interval"]["end"] = json!("2026-01-15");
        let d = json!({"inputs":[input("p",1)],"relations":[
            tuple("roster",&["a","b"]),tuple("triple",&decoy),partial,
            tuple("triple",&matching),tuple("triple",&matching),
        ]});
        for mode in ["explain", "fast"] {
            assert_eq!(count(&run(mode, &p, &d, &query_ids()).unwrap(), 0), 1);
        }
    }
}

#[test]
fn sorted_domains_propagate_first_failure_and_stop_before_a_later_failure() {
    let p = program(exists(
        "edge",
        vec![json!("anchor"), variable(0)],
        1,
        positive(variable(0)),
    ));
    let data = |first: &str, second: &str| {
        json!({"inputs":[input("b",1)],"relations":[
            tuple("roster",&["a","c"]),tuple("edge",&["a",second]),tuple("edge",&["a",first])
        ]})
    };
    for mode in ["explain", "fast"] {
        assert_eq!(
            count(&run(mode, &p, &data("b", "z"), &query_ids()).unwrap(), 0),
            1
        );
        let error = run(mode, &p, &data("aa", "b"), &query_ids()).unwrap_err();
        assert!(error.contains("points") && error.contains("aa"), "{error}");
    }
}

#[test]
fn unary_data_generator_has_a_finite_unbound_domain() {
    let mut p = program(exists("unary", vec![variable(0)], 0, positive(variable(0))));
    p["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"unary","arity":1,"slot_entities":["Person"]}));
    let d = json!({"inputs":[input("p",1)],"relations":[tuple("roster",&["a","b"]),tuple("unary",&["p"])]});
    for mode in ["explain", "fast"] {
        assert_eq!(count(&run(mode, &p, &d, &query_ids()).unwrap(), 0), 1);
    }
}

#[test]
fn exists_opts_into_strict_binding_and_dense_names_its_refusal() {
    let mut p = program(exists(
        "edge",
        vec![json!("anchor"), variable(0)],
        1,
        truth(true),
    ));
    // `exists` alone must opt in; an unrelated qualified predicate would
    // otherwise hide a missing uses_relation_roles arm.
    p["relations"]
        .as_array_mut()
        .unwrap()
        .retain(|relation| relation["name"] != "eligible");
    let spec: ProgramSpec = serde_json::from_value(p.clone()).unwrap();
    let artifact = CompiledProgramArtifact::compile(spec).unwrap();
    let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .err()
        .expect("dense refuses exists");
    assert!(
        matches!(error, DenseCompileError::Unsupported(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("exists") && error.to_string().contains("#218"),
        "{error}"
    );
    let empty = json!({"inputs":[],"relations":[]});
    let mut request: Value = json!({"mode":"explain","program":p,"dataset":empty,"queries":[]});
    request["relation_binding"] = json!("lenient");
    let error = execute_request(serde_json::from_value(request).unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("lenient_binding_with_relation_roles"),
        "{error}"
    );
    let supplied = json!({"inputs":[],"relations":[tuple("selected",&["a","b"])]});
    let error = run("explain", &p, &supplied, &query_ids()).unwrap_err();
    assert!(
        error.contains("supplied_tuples_for_derived_relation"),
        "{error}"
    );
}
