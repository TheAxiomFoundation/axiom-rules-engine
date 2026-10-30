//! A determined phase-two answer must survive every unknown membership: it
//! has to equal the answer each completion of the unknowns gives. Every read
//! here reaches `participating` at an entity other than the requested one.
#![cfg(feature = "unit-derivation")]

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::model::{DataSet, Interval, Period, Program, ScalarValue};
use axiom_rules_engine::spec::ProgramSpec;
use axiom_rules_engine::unit_derivation::{
    Citation, CompleteReduction, ConstitutionInput, DerivationTrace, DerivedUnit,
    EmissionRelations, Evidence, IndeterminateKind, IndeterminatePerson, MembershipTuple,
    Projection, ProjectionRole, PrototypeRun, Provenance, RosterInput, SuppliedEntity,
    UnitDerivationResult, materialize_phase_two_dataset,
};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const PERSONS: [&str; 3] = ["p0", "p1", "p2"];
const UNITS: [&str; 2] = ["u0", "u1"];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    In,
    Out,
    Unknown,
}

/// Each person's unit and participation.
type World = [(usize, Status); 3];

fn literal(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn positive(left: Value) -> Value {
    json!({"kind": "comparison", "op": "gt", "left": left, "right": literal(0)})
}

fn tautology() -> Value {
    json!({"kind": "comparison", "op": "eq", "left": literal(1), "right": literal(1)})
}

/// A person's `selected` units: those of `constituent`, kept by `predicate`.
fn selected(predicate: Value) -> Value {
    json!({"name": "selected", "arity": 2, "slot_entities": ["Unit", "Person"], "derivation": {
        "source_relation": "constituent", "current_slot": 1, "related_slot": 0,
        "slot_entities": ["Unit", "Person"], "predicate": predicate}})
}

fn count_selected() -> Value {
    json!({"kind": "count_related", "relation": "selected", "current_slot": 1, "related_slot": 0})
}

fn over_units(kind: &str, value: Value) -> Value {
    let mut expr = json!({"kind": kind, "relation": "constituent", "current_slot": 1,
        "related_slot": 0, "value": value});
    if kind == "extreme_related" {
        expr["extreme"] = json!("max");
    }
    expr
}

fn size_read(term: &str) -> Value {
    json!({"kind": "role_derived", "term": term, "name": "size"})
}

fn bound_size_read() -> Value {
    json!({"kind": "role_derived", "term": {"var": 0}, "name": "size"})
}

fn exists(relation: &str, terms: Value, slot: usize, body: Value) -> Value {
    json!({"kind": "exists", "relation": relation, "terms": terms,
        "variable_slot": slot, "body": body})
}

/// Each shape reads `participating` away from the requested person's own
/// fold, so the walk cannot place the read at that person.
fn shapes() -> Vec<(&'static str, Option<Value>, Value)> {
    let derived_size = json!({"kind": "derived", "name": "size"});
    let bare_size = positive(json!({"kind": "derived", "name": "size"}));
    let some_member = exists(
        "members",
        json!(["candidate", {"var": 0}]),
        1,
        json!({"kind": "same_entity", "left": {"var": 0}, "right": {"var": 0}, "negated": false}),
    );
    let atom = json!({"kind": "relation_atom", "relation": "members",
        "terms": ["candidate", "anchor"]});
    vec![
        (
            "exists over a derived generator",
            Some(selected(some_member.clone())),
            count_selected(),
        ),
        (
            "for all over a derived generator",
            Some(selected(json!({"kind": "not",
            "item": some_member}))),
            count_selected(),
        ),
        (
            "atom over a derived relation",
            Some(selected(atom)),
            count_selected(),
        ),
        (
            "rule read at a bound variable",
            Some(selected(exists(
                "constituent",
                json!([{"var": 0}, "anchor"]),
                0,
                positive(bound_size_read()),
            ))),
            count_selected(),
        ),
        (
            "rule read at the candidate",
            Some(selected(positive(size_read("candidate")))),
            count_selected(),
        ),
        (
            "bare rule read in a predicate",
            Some(selected(bare_size.clone())),
            count_selected(),
        ),
        (
            "judgment read at the candidate",
            Some(selected(
                json!({"kind": "role_derived", "term": "candidate", "name": "big"}),
            )),
            count_selected(),
        ),
        (
            "bare judgment read in a predicate",
            Some(selected(json!({"kind": "derived", "name": "big"}))),
            count_selected(),
        ),
        (
            "rule read in a where clause",
            None,
            json!({"kind": "count_related",
            "relation": "constituent", "current_slot": 1, "related_slot": 0, "where": bare_size}),
        ),
        (
            "rule value of a sum",
            None,
            over_units("sum_related", derived_size.clone()),
        ),
        (
            "rule value of an extreme",
            None,
            over_units("extreme_related", derived_size.clone()),
        ),
        (
            "rule value of a single",
            None,
            over_units("single_related", derived_size),
        ),
    ]
}

fn program(relation: Option<Value>, total: Value) -> Program {
    let mut relations = vec![
        json!({"name": "constituent", "arity": 2, "slot_entities": ["Unit", "Person"]}),
        json!({"name": "participating", "arity": 2, "slot_entities": ["Unit", "Person"]}),
        json!({"name": "members", "arity": 2, "slot_entities": ["Unit", "Person"], "derivation": {
            "source_relation": "participating", "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Unit", "Person"], "predicate": tautology()}}),
    ];
    relations.extend(relation);
    let spec: ProgramSpec = serde_json::from_value(json!({
        "relations": relations,
        "derived": [
            {"name": "size", "entity": "Unit", "dtype": "integer", "unit": null,
                "semantics": "scalar", "expr": {"kind": "count_related",
                    "relation": "participating", "current_slot": 0, "related_slot": 1}},
            {"name": "big", "entity": "Unit", "dtype": "judgment", "unit": null,
                "semantics": "judgment",
                "expr": positive(json!({"kind": "derived", "name": "size"}))},
            {"name": "total", "entity": "Person", "dtype": "integer", "unit": null,
                "semantics": "scalar", "expr": total},
        ],
    }))
    .unwrap();
    CompiledProgramArtifact::compile(spec)
        .unwrap()
        .program
        .to_program()
        .unwrap()
}

fn period() -> Period {
    Period::month(2026, 1)
}

fn reduce(program: &Program, world: &World, person: &str) -> CompleteReduction<ScalarValue> {
    let provenance = Provenance::Derived {
        constitution: "test".into(),
        trace_root: "test".into(),
    };
    let membership = |relation: &str, unit: usize, person: usize, projection| MembershipTuple {
        relation: relation.into(),
        unit: UNITS[unit].into(),
        person: PERSONS[person].into(),
        projection,
        role: ProjectionRole::Member,
        citations: BTreeSet::new(),
        provenance: provenance.clone(),
    };
    let mut memberships = Vec::new();
    let mut indeterminate = Vec::new();
    for (person, (unit, status)) in world.iter().enumerate() {
        memberships.push(membership(
            "constituent",
            *unit,
            person,
            Projection::UnitConstituent,
        ));
        match status {
            Status::In => memberships.push(membership(
                "participating",
                *unit,
                person,
                Projection::ParticipatingMember,
            )),
            Status::Out => {}
            Status::Unknown => indeterminate.push(IndeterminatePerson {
                person: PERSONS[person].into(),
                kind: IndeterminateKind::Role(Projection::ParticipatingMember),
                unresolved_facts: BTreeSet::from([format!("unknown:{person}")]),
            }),
        }
    }
    let units = UNITS
        .iter()
        .enumerate()
        .map(|(index, id)| DerivedUnit {
            entity_type: "Unit".into(),
            id: (*id).into(),
            members: world
                .iter()
                .enumerate()
                .filter(|(_, (unit, _))| *unit == index)
                .map(|(person, _)| PERSONS[person].to_string())
                .collect(),
            provenance: provenance.clone(),
        })
        .collect();
    let run = PrototypeRun {
        comparisons: Vec::new(),
        derivation: UnitDerivationResult {
            relations: EmissionRelations {
                unit_constituent: "constituent".into(),
                participating_member: "participating".into(),
            },
            units,
            memberships,
            indeterminate,
            trace: DerivationTrace {
                root: "test".into(),
                worlds_evaluated: 1,
                events: BTreeSet::new(),
                completeness_evidence: BTreeSet::new(),
                input_conflicts: BTreeSet::new(),
                input_conflict_impacts: BTreeSet::new(),
            },
        },
    };
    let input = ConstitutionInput {
        roster: RosterInput {
            relation: "people".into(),
            scope: "test".into(),
            persons: PERSONS.iter().map(|id| (*id).to_string()).collect(),
            completeness: None,
        },
        segment: "test".into(),
        segment_complete: true,
        relation_families: Vec::new(),
        bool_facts: Vec::new(),
        integrity_constraints: Vec::new(),
        supplied_entities: PERSONS
            .iter()
            .map(|id| SuppliedEntity {
                entity_type: "Person".into(),
                id: (*id).into(),
                evidence: Evidence {
                    id: "test".into(),
                    citation: Citation::new("test", "test"),
                },
            })
            .collect(),
    };
    let period = period();
    let phase = materialize_phase_two_dataset(
        &DataSet::default(),
        program,
        &input,
        &run,
        Interval::covering(&period),
    )
    .unwrap();
    phase
        .engine(program)
        .evaluate_scalar("total", person, &period)
        .unwrap_or_else(|error| CompleteReduction::Indeterminate {
            reasons: BTreeSet::from([format!("evaluation failed: {error:?}")]),
        })
}

/// Every way the world's unknown memberships could resolve.
fn completions(world: &World) -> Vec<World> {
    let mut worlds = vec![*world];
    for index in 0..world.len() {
        if world[index].1 == Status::Unknown {
            worlds = worlds
                .into_iter()
                .flat_map(|known| {
                    [Status::In, Status::Out].map(|status| {
                        let mut resolved = known;
                        resolved[index].1 = status;
                        resolved
                    })
                })
                .collect();
        }
    }
    worlds
}

fn world() -> impl Strategy<Value = World> {
    let person = (
        0..UNITS.len(),
        prop_oneof![Just(Status::In), Just(Status::Out), Just(Status::Unknown)],
    );
    [person.clone(), person.clone(), person]
}

/// One unknown membership in `u0`, and `p0` in `u0`. Each shape answers
/// differently for the two completions, so no shape may report a value.
#[test]
fn every_read_of_an_unknown_membership_stays_indeterminate() {
    let world = [(0, Status::Unknown), (0, Status::Out), (1, Status::Out)];
    let mut escaped = Vec::new();
    for (name, relation, total) in shapes() {
        let program = program(relation, total);
        let answers = completions(&world)
            .iter()
            .map(|resolved| format!("{:?}", reduce(&program, resolved, "p0")))
            .collect::<BTreeSet<_>>();
        assert_eq!(answers.len(), 2, "{name}: the unknown changes nothing");
        let reduction = reduce(&program, &world, "p0");
        if !matches!(reduction, CompleteReduction::Indeterminate { .. }) {
            escaped.push(format!("{name}: {reduction:?}"));
        }
    }
    assert!(escaped.is_empty(), "past an unknown: {escaped:#?}");
}

/// The walk keeps a fold's own read at the requested entity, so an unknown
/// membership of another unit leaves a direct count determined.
#[test]
fn a_direct_count_ignores_an_unknown_in_another_unit() {
    let program = program(
        None,
        json!({"kind": "count_related", "relation": "participating", "current_slot": 1,
            "related_slot": 0}),
    );
    let world = [(0, Status::In), (1, Status::Unknown), (1, Status::Out)];
    assert_eq!(
        reduce(&program, &world, "p0"),
        CompleteReduction::Determined(ScalarValue::Integer(1))
    );
}

proptest! {
    #[test]
    fn a_determined_answer_holds_in_every_completion(
        shape in 0..shapes().len(),
        world in world(),
        person in 0..PERSONS.len(),
    ) {
        let (name, relation, total) = shapes().swap_remove(shape);
        let program = program(relation, total);
        if let answer @ CompleteReduction::Determined(_) = reduce(&program, &world, PERSONS[person]) {
            for resolved in completions(&world) {
                prop_assert_eq!(
                    &reduce(&program, &resolved, PERSONS[person]),
                    &answer,
                    "{} in {:?} completed as {:?}", name, world, resolved
                );
            }
        }
    }
}
