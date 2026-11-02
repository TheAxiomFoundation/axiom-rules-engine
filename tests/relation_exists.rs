//! Finite existential domains, independent value oracles, and public execution contracts.
use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
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
fn variable_input_reads_use_the_bound_id_in_nested_frames() {
    let at = |index| json!({"kind":"role_input","term":variable(index),"name":"points"});
    let age = json!({"kind":"comparison","left":at(0),"op":"gte","right":integer(65)});
    let nested = exists(
        "edge",
        vec![variable(0), variable(0)],
        1,
        json!({"kind":"comparison","left":at(1),"op":"gt","right":at(0)}),
    );
    let data = json!({"inputs":[input("a",10),input("b",90),input("c",0),input("p",65),input("q",17)],
        "relations":[tuple("roster", &["a","b"]),tuple("roster", &["a","c"]),
            tuple("edge", &["a","p"]),tuple("edge", &["p","q"])]});
    for body in [age, nested] {
        let p = program(exists("edge", vec![json!("anchor"), variable(0)], 1, body));
        for mode in ["explain", "fast"] {
            assert_eq!(count(&run(mode, &p, &data, &query_ids()).unwrap(), 0), 2);
        }
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

proptest! {
    #![proptest_config(ProptestConfig {cases:48, rng_seed:proptest::test_runner::RngSeed::Fixed(21403), ..ProptestConfig::default()})]
    #[test]
    fn exists_values_order_determinism_and_modes_have_an_independent_oracle(
        rows in prop::collection::vec((-2i64..3, any::<bool>()),0..8),
        derived_domain in any::<bool>(), nested in any::<bool>(), shift in 0usize..32,
    ) {
        let body=if nested {
            exists("edge",vec![variable(0),variable(0)],1,json!({"kind":"and","items":[positive(variable(1)),positive(variable(0))]}))
        } else { positive(variable(0)) };
        let p=program(exists(if derived_domain {"eligible"} else {"edge"},vec![json!("anchor"),variable(0)],1,body));
        let mut records=vec![tuple("roster",&["a","b"]),tuple("roster",&["a","c"])];
        let mut inputs=vec![];
        for (index,(score,present)) in rows.iter().enumerate() {
            let id=format!("p{index}"); inputs.push(input(&id,*score));
            if *present { records.push(tuple("edge",&["a",&id])); records.push(tuple("edge",&["a",&id])); }
            // Inner frame can only reach its own id, independently of outer candidate.
            records.push(tuple("edge",&[&id,&id]));
        }
        let expected=2*i64::from(rows.iter().any(|(score,present)|*present&&*score>0));
        let data=json!({"inputs":inputs,"relations":records});
        let mut permuted=data.clone();
        for key in ["inputs","relations"] { let items=permuted[key].as_array_mut().unwrap(); items.reverse(); if !items.is_empty() { let n=items.len(); items.rotate_left(shift%n); } }
        let mut values=vec![];
        for mode in ["explain","fast"] {
            let first=run(mode,&p,&data,&query_ids()).unwrap();
            prop_assert_eq!(count(&first,0),expected);
            prop_assert_eq!(&first,&run(mode,&p,&data,&query_ids()).unwrap());
            prop_assert_eq!(&first,&run(mode,&p,&permuted,&query_ids()).unwrap());
            for depth in 0..=4 { prop_assert_eq!(&first,&with_suspend_depth(depth,||run(mode,&p,&data,&query_ids()).unwrap())); }
            values.push(first["results"][0]["outputs"].clone());
        }
        prop_assert_eq!(&values[0],&values[1]);
    }

    /// §7.6 excludes derived reads, aggregating rules, count comparisons and bare tests.
    /// These domains and atoms are base relations, under zero or two `not`s.
    #[test]
    fn positive_base_exists_are_monotone_with_exact_memberships(
        rows in prop::collection::vec((any::<bool>(),any::<bool>(),any::<bool>()),1..10), double_not in any::<bool>(),
    ) {
        let body=json!({"kind":"same_entity","left":variable(0),"right":"candidate","negated":false});
        let predicate=exists("edge",vec![json!("anchor"),variable(0)],1,body);
        let p=program(if double_not {not(not(predicate))}else{predicate});
        let (mut old,mut new,mut ids,mut expected)=(vec![],vec![],vec![],vec![]);
        for (index,(present,add,decoy)) in rows.iter().enumerate() {
            let a=format!("a{index}");let c=format!("c{index}");ids.push(a.clone());
            old.push(tuple("roster",&[&a,&c]));new.push(tuple("roster",&[&a,&c]));
            if *decoy {old.push(tuple("edge",&[&a,"decoy"]));new.push(tuple("edge",&[&a,"decoy"]));}
            if *present {old.push(tuple("edge",&[&a,&c]));}
            if *present||*add {new.push(tuple("edge",&[&a,&c]));}
            expected.push((i64::from(*present),i64::from(*present||*add)));
        }
        for mode in ["explain","fast"] {
            let a=run(mode,&p,&json!({"inputs":[],"relations":old}),&ids).unwrap();
            let b=run(mode,&p,&json!({"inputs":[],"relations":new}),&ids).unwrap();
            for (row,(before,after)) in expected.iter().enumerate(){prop_assert_eq!(count(&a,row),*before);prop_assert_eq!(count(&b,row),*after);prop_assert!(before<=after);}
        }
    }
}
