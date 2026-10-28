//! Atom programs order rules through the complete relation dependency graph:
//! each rule is the smallest name whose waits are listed, as the frozen order
//! specifies for legacy programs (`compile/evaluation_order_tests.rs`), with
//! waits followed through atoms, sources and predicates. Earlier artifacts
//! keep their frozen order, including qualified-read and term-comparison
//! programs which do not use an atom.

use std::collections::{BTreeMap, BTreeSet};

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
use serde_json::{Value, json};

fn integer(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn always() -> Value {
    json!({"kind": "comparison", "left": integer(1), "op": "eq", "right": integer(1)})
}

fn atom(relation: &str) -> Value {
    json!({"kind": "relation_atom", "relation": relation, "terms": ["anchor", "candidate"]})
}

fn relation(name: &str, source: &str, predicate: Value) -> Value {
    json!({"name": name, "arity": 2, "slot_entities": ["Person", "Person"],
        "derivation": {"source_relation": source, "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Person", "Person"], "predicate": predicate}})
}

fn score_test() -> Value {
    json!({"kind": "comparison", "op": "gt", "right": integer(0),
        "left": {"kind": "role_derived", "term": "candidate", "name": "z_score"}})
}

fn program(shape: &str) -> Value {
    let mut relations = vec![
        json!({"name": "base", "arity": 2, "slot_entities": ["Person", "Person"]}),
        relation("selected", "base", atom("inner")),
    ];
    match shape {
        "target" => relations.push(relation("inner", "base", score_test())),
        "source" => {
            relations.push(relation("inner", "inner_source", always()));
            relations.push(relation("inner_source", "base", score_test()));
        }
        "nested" => {
            relations.push(relation("inner", "base", atom("nested")));
            relations.push(relation("nested", "base", score_test()));
        }
        other => panic!("unknown test shape {other}"),
    }
    json!({"relations": relations, "derived": [
        {"name": "a_total", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "selected", "current_slot": 0, "related_slot": 1}},
        {"name": "b_rule", "entity": "Person", "dtype": "integer", "semantics": "scalar", "expr": integer(0)},
        {"name": "z_score", "entity": "Person", "dtype": "integer", "semantics": "scalar",
            "expr": {"kind": "input", "name": "score"}},
    ]})
}

fn compile(value: Value) -> Result<CompiledProgramArtifact, CompileError> {
    let spec: ProgramSpec = serde_json::from_value(value).unwrap();
    CompiledProgramArtifact::compile(spec)
}

#[test]
fn atom_order_follows_target_source_and_nested_atom_predicate_rules() {
    for shape in ["target", "source", "nested"] {
        let original = program(shape);
        for permutation in 0..6 {
            let mut value = original.clone();
            let rules = value["derived"].as_array_mut().unwrap();
            rules.rotate_left(permutation % 3);
            if permutation >= 3 {
                rules.reverse();
            }
            let relations = value["relations"].as_array_mut().unwrap();
            let len = relations.len();
            relations.rotate_left(permutation % len);
            if permutation % 2 == 0 {
                relations.reverse();
            }
            let artifact = compile(value).unwrap();
            assert_eq!(
                artifact.metadata.evaluation_order,
                ["b_rule", "z_score", "a_total"],
                "{shape}, permutation {permutation}"
            );
            let encoded = serde_json::to_string(&artifact).unwrap();
            let loaded = CompiledProgramArtifact::from_json_str(&encoded).unwrap();
            assert_eq!(serde_json::to_string(&loaded).unwrap(), encoded);
        }
    }
}

#[test]
fn programs_without_atoms_keep_their_frozen_order() {
    let membership = json!({"kind": "relation_member", "relation": "inner", "current_slot": 0, "related_slot": 1});
    let qualified = json!({"kind": "comparison", "op": "gt", "right": integer(0),
        "left": {"kind": "role_input", "term": "candidate", "name": "age"}});
    let comparison =
        json!({"kind": "same_entity", "left": "candidate", "right": "anchor", "negated": true});
    for predicate in [
        membership.clone(),
        json!({"kind": "and", "items": [membership.clone(), qualified]}),
        json!({"kind": "and", "items": [membership, comparison]}),
    ] {
        let mut value = program("target");
        value["relations"][1]["derivation"]["predicate"] = predicate;
        let artifact = compile(value).unwrap();
        assert_eq!(
            artifact.metadata.evaluation_order,
            ["a_total", "b_rule", "z_score"]
        );
    }
}

#[test]
fn atom_cycles_include_relations_read_by_only_related_rules() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "role_derived", "term": "candidate", "name": "z_score"
    });
    value["derived"][2] = json!({
        "name": "z_score", "entity": "Person", "dtype": "judgment", "semantics": "judgment",
        "expr": {"kind": "comparison", "op": "eq",
            "left": {"kind": "only_related", "relation": "selected", "current_slot": 0, "related_slot": 1,
                "value": {"kind": "input", "name": "flag"}},
            "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}}
    });
    let error = compile(value).unwrap_err();
    assert!(
        matches!(error, CompileError::CyclicDependency { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("z_score"), "{error}");
}

#[test]
fn atom_order_includes_only_related_reads_inside_predicates() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "comparison", "op": "eq",
        "left": {"kind": "only_related", "relation": "inner_source", "current_slot": 0, "related_slot": 1,
            "value": {"kind": "input", "name": "flag"}},
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    value["relations"]
        .as_array_mut()
        .unwrap()
        .push(relation("inner_source", "base", score_test()));
    let artifact = compile(value).unwrap();
    assert_eq!(
        artifact.metadata.evaluation_order,
        ["b_rule", "z_score", "a_total"]
    );
}

#[test]
fn unused_atom_cycles_through_only_related_are_refused() {
    let mut value = program("target");
    value["relations"][2]["derivation"]["predicate"] = json!({
        "kind": "comparison", "op": "eq",
        "left": {"kind": "only_related", "relation": "selected", "current_slot": 0, "related_slot": 1,
            "value": {"kind": "input", "name": "flag"}},
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    // Neither remaining rule reads either relation: this must be refused
    // even when there is no unfinished rule node for the cycle to block.
    value["derived"].as_array_mut().unwrap().remove(0);
    let error = compile(value).unwrap_err();
    assert!(
        matches!(error, CompileError::CyclicRelationDependency { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("inner"), "{error}");
    assert!(error.to_string().contains("selected"), "{error}");
}

const RULE_NAMES: [&str; 6] = ["q_rule", "c_rule", "x_rule", "a_rule", "m_rule", "k_rule"];

/// What each generated node reads directly: rules, and derived relations.
type Reads = BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)>;

/// An acyclic atom program: every rule and derived relation reads only nodes
/// earlier in a generated sequence, and `rel0`'s predicate always holds an
/// atom, so the program takes the combined-graph order. Records each node's
/// direct reads at construction, independently of the compiler.
fn generated_atom_program(bytes: &[u8]) -> (Value, Reads, Reads) {
    let mut next = bytes.iter().copied().cycle();
    let mut pick = |n: usize| usize::from(next.next().unwrap()) % n;
    let rule_count = 1 + pick(RULE_NAMES.len());
    let relation_count = 1 + pick(4);
    let mut nodes = (0..rule_count)
        .map(|index| (true, index))
        .chain((0..relation_count).map(|index| (false, index)))
        .collect::<Vec<_>>();
    for i in (1..nodes.len()).rev() {
        let j = pick(i + 1);
        nodes.swap(i, j);
    }
    let (mut rules, mut relations) = (
        Vec::new(),
        vec![json!({
            "name": "base", "arity": 2, "slot_entities": ["Person", "Person"]
        })],
    );
    let (mut rule_reads, mut relation_reads) = (Reads::new(), Reads::new());
    let (mut earlier_rules, mut earlier_relations) = (Vec::<String>::new(), Vec::<String>::new());
    for (is_rule, index) in nodes {
        let mut reads = (BTreeSet::new(), BTreeSet::new());
        if is_rule {
            let mut items = vec![integer(0)];
            for _ in 0..pick(4) {
                if pick(2) == 0 && !earlier_rules.is_empty() {
                    let rule = earlier_rules[pick(earlier_rules.len())].clone();
                    items.push(json!({"kind": "derived", "name": rule}));
                    reads.0.insert(rule);
                } else if !earlier_relations.is_empty() {
                    let relation = earlier_relations[pick(earlier_relations.len())].clone();
                    items.push(json!({"kind": "count_related", "relation": relation,
                        "current_slot": 0, "related_slot": 1}));
                    reads.1.insert(relation);
                }
            }
            let name = RULE_NAMES[index].to_string();
            rules.push(json!({"name": name, "entity": "Person", "dtype": "integer",
                "semantics": "scalar", "expr": {"kind": "add", "items": items}}));
            rule_reads.insert(name.clone(), reads);
            earlier_rules.push(name);
        } else {
            let name = format!("rel{index}");
            let source = if earlier_relations.is_empty() || pick(2) == 0 {
                "base".to_string()
            } else {
                earlier_relations[pick(earlier_relations.len())].clone()
            };
            if source != "base" {
                reads.1.insert(source.clone());
            }
            let mut items = vec![always()];
            if index == 0 {
                items.push(atom("base"));
            }
            for _ in 0..pick(4) {
                if pick(2) == 0 && !earlier_rules.is_empty() {
                    let rule = earlier_rules[pick(earlier_rules.len())].clone();
                    items.push(
                        json!({"kind": "comparison", "op": "gte", "right": integer(0),
                        "left": {"kind": "role_derived", "term": "candidate", "name": rule}}),
                    );
                    reads.0.insert(rule);
                } else if !earlier_relations.is_empty() && pick(2) == 0 {
                    let target = earlier_relations[pick(earlier_relations.len())].clone();
                    items.push(atom(&target));
                    reads.1.insert(target);
                } else {
                    items.push(atom("base"));
                }
            }
            relations.push(relation(
                &name,
                &source,
                json!({"kind": "and", "items": items}),
            ));
            relation_reads.insert(name.clone(), reads);
            earlier_relations.push(name);
        }
    }
    (
        json!({"relations": relations, "derived": rules}),
        rule_reads,
        relation_reads,
    )
}

/// The rules a derived relation's members wait for: those its predicate reads
/// and, transitively, those of every derived relation it reads.
fn relation_waits(relation: &str, relation_reads: &Reads) -> BTreeSet<String> {
    let (rules, relations) = &relation_reads[relation];
    let mut waits = rules.clone();
    for read in relations {
        waits.extend(relation_waits(read, relation_reads));
    }
    waits
}

/// The specification: the i-th rule is the smallest name not yet listed
/// whose waits are all listed, where a rule waits for each rule it reads and
/// each rule a derived relation it reads waits for.
fn smallest_ready_order(rule_reads: &Reads, relation_reads: &Reads) -> Vec<String> {
    let waits = rule_reads
        .iter()
        .map(|(rule, (rules, relations))| {
            let mut all = rules.clone();
            for relation in relations {
                all.extend(relation_waits(relation, relation_reads));
            }
            (rule.clone(), all)
        })
        .collect::<BTreeMap<_, _>>();
    let mut order = Vec::<String>::new();
    while order.len() < waits.len() {
        let next = waits
            .iter()
            .find(|(rule, waits)| {
                !order.contains(rule) && waits.iter().all(|wait| order.contains(wait))
            })
            .map(|(rule, _)| rule.clone())
            .expect("an acyclic program always has a ready rule");
        order.push(next);
    }
    order
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn atom_program_order_is_the_smallest_ready_rule_order(
        bytes in prop::collection::vec(any::<u8>(), 64),
        rotation in 0_usize..8,
    ) {
        let (value, rule_reads, relation_reads) = generated_atom_program(&bytes);
        let expected = smallest_ready_order(&rule_reads, &relation_reads);
        let artifact = compile(value.clone()).unwrap();
        prop_assert_eq!(&artifact.metadata.evaluation_order, &expected);
        // Declaration order changes nothing.
        let mut shuffled = value;
        let rules = shuffled["derived"].as_array_mut().unwrap();
        let len = rules.len();
        rules.rotate_left(rotation % len);
        rules.reverse();
        let relations = shuffled["relations"].as_array_mut().unwrap();
        let len = relations.len();
        relations.rotate_left(rotation % len);
        prop_assert_eq!(compile(shuffled).unwrap().metadata.evaluation_order, expected);
    }
}
