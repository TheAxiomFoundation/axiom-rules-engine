//! Typing and binding a long chain of derived relations is linear in the
//! chain and never recurses once per link.
//!
//! A derived relation that declares no slot kinds has its source's. Typing
//! and binding used to ask for each relation's kinds by recursing down its
//! chain, once per relation and once per dataset tuple, so a chain of 4,000
//! links took 3 to 9 seconds to type and 50,000 links overflowed the stack.
//! Filters of filters and the namesakes of untyped relations were resolved
//! per entity and per relation in the same way. Every test here runs a
//! 50,000-link chain on the 2 MiB test thread within a time bound that the
//! per-link walks miss by orders of magnitude: each test overflows the stack
//! or runs past its bound on the code before this change.

use std::time::{Duration, Instant};

use axiom_rules_engine::model::Program;
use axiom_rules_engine::relation_typing::{
    RelationTypingCode, check_program, executed_relations, filtered_entity_kinds,
};
use axiom_rules_engine::spec::{DatasetBindingOptions, DatasetSpec, ProgramSpec};
use serde_json::{Value, json};

const LINKS: usize = 50_000;

/// Generous for a debug build: the linear walks take well under a second.
const BOUND: Duration = Duration::from_secs(10);

fn relation_name(link: usize) -> String {
    format!("rel{link:07}")
}

fn always() -> Value {
    json!({
        "kind": "comparison",
        "left": {"kind": "literal", "value": {"kind": "integer", "value": 1}},
        "op": "eq",
        "right": {"kind": "literal", "value": {"kind": "integer", "value": 1}}
    })
}

/// `rel0 <- rel1 <- ... <- rel{LINKS-1} <- base`, where `base` declares
/// `base_kinds` and `derivation(link)` adds fields to each link's derivation.
fn chain(base_kinds: Value, derivation: impl Fn(usize) -> Value) -> Vec<Value> {
    let mut relations = vec![json!({"name": "base", "arity": 2, "slot_entities": base_kinds})];
    for link in 0..LINKS {
        let source = if link + 1 < LINKS {
            relation_name(link + 1)
        } else {
            "base".to_string()
        };
        let mut fields = json!({
            "source_relation": source,
            "current_slot": 0,
            "related_slot": 1,
            "predicate": always()
        });
        if let (Value::Object(fields), Value::Object(extra)) = (&mut fields, derivation(link)) {
            fields.extend(extra);
        }
        relations.push(json!({"name": relation_name(link), "arity": 2, "derivation": fields}));
    }
    relations
}

/// A Household rule counting over the head of the chain.
fn head_count() -> Value {
    json!({
        "name": "head",
        "entity": "Household",
        "dtype": "integer",
        "semantics": "scalar",
        "expr": {"kind": "count_related", "relation": relation_name(0),
                 "current_slot": 0, "related_slot": 1}
    })
}

fn program(relations: Vec<Value>, derived: Vec<Value>) -> Program {
    let spec: ProgramSpec =
        serde_json::from_value(json!({"relations": relations, "derived": derived}))
            .expect("program spec parses");
    spec.to_program().expect("program builds")
}

fn timed<T>(what: &str, run: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = run();
    let elapsed = started.elapsed();
    assert!(elapsed < BOUND, "{what} took {elapsed:?}");
    result
}

#[test]
fn a_chain_inheriting_its_kinds_types_and_binds_in_linear_time() {
    let program = program(
        chain(json!(["Household", "Person"]), |_| json!({})),
        vec![head_count()],
    );
    timed("typing", || check_program(&program)).expect("the chain is well typed");
    let executed = timed("executed relations", || executed_relations(&program));
    assert_eq!(executed.len(), LINKS + 1);

    // A tuple may be supplied under any link's name; every link has the
    // base's kinds. One tuple per link, plus one stored the wrong way round.
    let interval = json!({"start": "2026-01-01", "end": "2026-12-31"});
    let mut tuples = (0..LINKS)
        .map(|link| json!({"name": relation_name(link), "tuple": ["h1", format!("p{link}")], "interval": interval}))
        .collect::<Vec<_>>();
    tuples.push(
        json!({"name": relation_name(LINKS / 2), "tuple": ["p0", "h1"], "interval": interval}),
    );
    let dataset: DatasetSpec = serde_json::from_value(json!({"inputs": [], "relations": tuples}))
        .expect("dataset spec parses");
    // Queries are kind evidence: `h1` is evaluated as a Household and `p0`
    // as a Person.
    let queried = [("h1", "Household"), ("p0", "Person")]
        .map(|(id, entity)| (id.to_string(), entity.to_string()));
    let outcome = timed("binding", || {
        dataset.to_dataset_for_queries_with_options(
            &program,
            &queried,
            DatasetBindingOptions {
                strict_relation_entities: false,
            },
        )
    })
    .expect("lenient binding keeps the dataset");
    // The reversed tuple is reported in both slots; nothing else is.
    assert_eq!(outcome.diagnostics.len(), 2, "{:?}", outcome.diagnostics);
    assert!(
        outcome
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.relation == relation_name(LINKS / 2))
    );
}

#[test]
fn a_chain_filtering_to_one_entity_types_in_linear_time() {
    let program = program(
        chain(
            json!(["Household", "Person"]),
            |_| json!({"entity": "Unit"}),
        ),
        vec![head_count()],
    );
    timed("typing", || check_program(&program)).expect("the chain is well typed");
    let kinds = timed("filtered kinds", || filtered_entity_kinds(&program));
    assert_eq!(kinds.get("Unit").map(String::as_str), Some("Household"));
}

#[test]
fn a_chain_filtering_to_a_fresh_entity_per_link_types_in_linear_time() {
    let program = program(
        chain(
            json!(["Household", "Person"]),
            |link| json!({"entity": format!("U{link}")}),
        ),
        vec![head_count()],
    );
    timed("typing", || check_program(&program)).expect("the chain is well typed");
    let kinds = timed("filtered kinds", || filtered_entity_kinds(&program));
    assert_eq!(kinds.len(), LINKS);
    assert!(kinds.values().all(|kind| kind == "Household"));
}

#[test]
fn a_chain_of_filters_of_filters_resolves_in_linear_time() {
    // `U{i}` is queried with `U{i+1}` ids, down to `Household`: each link
    // filters `base` and declares the next filter as its current kind.
    let mut relations =
        vec![json!({"name": "base", "arity": 2, "slot_entities": ["Household", "Person"]})];
    for link in 0..LINKS {
        let current = if link + 1 < LINKS {
            format!("U{}", link + 1)
        } else {
            "Household".to_string()
        };
        relations.push(json!({
            "name": relation_name(link),
            "arity": 2,
            "derivation": {
                "source_relation": "base",
                "current_slot": 0,
                "related_slot": 1,
                "entity": format!("U{link}"),
                "slot_entities": [current, "Person"],
                "predicate": always()
            }
        }));
    }
    let program = program(relations, Vec::new());
    let kinds = timed("filtered kinds", || filtered_entity_kinds(&program));
    assert_eq!(kinds.len(), LINKS);
    assert!(kinds.values().all(|kind| kind == "Household"));
    // Nothing executes the links, so nothing about them is ill-typed.
    timed("typing", || check_program(&program)).expect("no executed relation is ill-typed");
}

#[test]
fn an_untyped_chain_is_refused_in_linear_time() {
    // No relation declares kinds: every executed link is reported, and each
    // report looks for module-qualified namesakes of the link.
    let program = program(chain(json!([]), |_| json!({})), vec![head_count()]);
    let report = timed("typing", || check_program(&program)).expect_err("untyped relations");
    let untyped = report
        .violations
        .iter()
        .filter(|violation| violation.code == RelationTypingCode::UntypedRelation)
        .count();
    assert_eq!(untyped, LINKS + 1);
}

#[test]
fn a_chain_declaring_fresh_kinds_per_link_is_refused_in_linear_time() {
    // The request PR #211 measured at 9.7 s for 4,000 links of binding:
    // every link declares its own kinds, and the head is counted.
    let program = program(
        chain(
            json!(["Household", "Person"]),
            |link| json!({"slot_entities": [format!("H{link}"), format!("P{link}")]}),
        ),
        vec![head_count()],
    );
    let report = timed("typing", || check_program(&program)).expect_err("kinds contradict");
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.code == RelationTypingCode::CurrentSlotEntityMismatch),
        "{report}"
    );
}
