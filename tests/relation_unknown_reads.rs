//! A determined phase-two answer must survive every unknown membership: it
//! has to equal the answer each completion of the unknowns gives. The walk
//! places some reads of `participating` at the requested person and counts
//! every unknown for the rest. The world is small enough to check all of it.
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

type Shape = (&'static str, Option<Value>, Value);

fn literal(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn positive(left: Value) -> Value {
    json!({"kind": "comparison", "op": "gt", "left": left, "right": literal(0)})
}

fn tautology() -> Value {
    json!({"kind": "comparison", "op": "eq", "left": literal(1), "right": literal(1)})
}

/// A person's `selected` units: those of `source`, kept by `predicate`.
fn selected_from(source: &str, predicate: Value) -> Value {
    json!({"name": "selected", "arity": 2, "slot_entities": ["Unit", "Person"], "derivation": {
        "source_relation": source, "current_slot": 1, "related_slot": 0,
        "slot_entities": ["Unit", "Person"], "predicate": predicate}})
}

fn selected(predicate: Value) -> Value {
    selected_from("constituent", predicate)
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

/// Each shape reads `participating` through a fold or membership test at the
/// requested person, so the walk places the read there.
fn placed_shapes() -> Vec<Shape> {
    let one_if = |condition: Value, otherwise: Value| {
        json!({"kind": "if", "condition": condition, "then_expr": literal(1),
            "else_expr": otherwise})
    };
    let only_eligible = json!({"kind": "only_related", "relation": "participating",
        "current_slot": 1, "related_slot": 0, "value": {"kind": "input", "name": "eligible"}});
    let member = json!({"kind": "relation_member", "relation": "participating",
        "current_slot": 1, "related_slot": 0});
    vec![
        (
            "direct count",
            None,
            json!({"kind": "count_related", "relation": "participating", "current_slot": 1,
                "related_slot": 0}),
        ),
        (
            "count of a derived relation",
            Some(selected_from("participating", tautology())),
            count_selected(),
        ),
        (
            "only related",
            None,
            one_if(
                json!({"kind": "comparison", "op": "eq", "left": only_eligible,
                    "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}}),
                literal(0),
            ),
        ),
        (
            "membership test in a dead branch",
            None,
            one_if(tautology(), one_if(member, literal(0))),
        ),
    ]
}

/// Each shape reads `participating` away from the requested person's own
/// fold, so the walk cannot place the read at that person.
fn unplaced_shapes() -> Vec<Shape> {
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

fn reduce(
    program: &Program,
    world: &World,
    person: &str,
) -> Result<CompleteReduction<ScalarValue>, String> {
    evaluate(program, world, "total", person)
}

fn evaluate(
    program: &Program,
    world: &World,
    rule: &str,
    entity: &str,
) -> Result<CompleteReduction<ScalarValue>, String> {
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
    let mut base = DataSet::default();
    for unit in UNITS {
        base.add_input(
            "eligible",
            "Unit",
            unit,
            Interval::covering(&period),
            ScalarValue::Bool(true),
        );
    }
    let phase =
        materialize_phase_two_dataset(&base, program, &input, &run, Interval::covering(&period))
            .unwrap();
    phase
        .engine(program)
        .evaluate_scalar(rule, entity, &period)
        .map_err(|error| format!("{error:?}"))
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

fn worlds() -> Vec<World> {
    let people = (0..UNITS.len())
        .flat_map(|unit| [Status::In, Status::Out, Status::Unknown].map(|status| (unit, status)))
        .collect::<Vec<_>>();
    let mut worlds = Vec::new();
    for &first in &people {
        for &second in &people {
            for &third in &people {
                worlds.push([first, second, third]);
            }
        }
    }
    worlds
}

/// One unknown membership in `u0`, and `p0` in `u0`. Each shape answers
/// differently for the two completions, so no shape may report a value.
#[test]
fn every_read_of_an_unknown_membership_stays_indeterminate() {
    let world = [(0, Status::Unknown), (0, Status::Out), (1, Status::Out)];
    let mut escaped = Vec::new();
    for (name, relation, total) in unplaced_shapes() {
        let program = program(relation, total);
        let answers = completions(&world)
            .iter()
            .map(|resolved| format!("{:?}", reduce(&program, resolved, "p0")))
            .collect::<BTreeSet<_>>();
        assert_eq!(answers.len(), 2, "{name}: the unknown changes nothing");
        let reduction = reduce(&program, &world, "p0");
        if !matches!(reduction, Ok(CompleteReduction::Indeterminate { .. })) {
            escaped.push(format!("{name}: {reduction:?}"));
        }
    }
    assert!(escaped.is_empty(), "past an unknown: {escaped:#?}");
}

/// A placed read sees only the requested person's memberships, so that
/// person's status alone decides whether the answer stays Indeterminate. An
/// unknown elsewhere does not make it so. A known `only_related` read with no
/// related entity still fails on its own.
#[test]
fn a_placed_read_holds_on_the_requested_persons_unknown_alone() {
    for (name, relation, total) in placed_shapes() {
        let program = program(relation, total);
        for world in worlds() {
            for (index, person) in PERSONS.iter().enumerate() {
                let reduction = reduce(&program, &world, person);
                assert_eq!(
                    matches!(reduction, Ok(CompleteReduction::Indeterminate { .. })),
                    world[index].1 == Status::Unknown,
                    "{name} in {world:?} at {person}: {reduction:?}"
                );
            }
        }
    }
}

/// `p0` participates in `u0` and another person's status is unknown, so each
/// placed shape reports the value `p0`'s own membership gives.
#[test]
fn a_placed_read_reports_its_value_past_another_persons_unknown() {
    let world = [(0, Status::In), (1, Status::Unknown), (1, Status::Out)];
    for (name, relation, total) in placed_shapes() {
        assert_eq!(
            reduce(&program(relation, total), &world, "p0"),
            Ok(CompleteReduction::Determined(ScalarValue::Integer(1))),
            "{name}"
        );
    }
}

/// `size` reads slot 0 at a unit, so only that unit's unknowns hold it.
#[test]
fn a_placed_read_at_a_unit_holds_on_that_units_unknowns_alone() {
    let program = program(None, literal(0));
    for world in worlds() {
        for (index, unit) in UNITS.iter().enumerate() {
            let reduction = evaluate(&program, &world, "size", unit);
            assert_eq!(
                matches!(reduction, Ok(CompleteReduction::Indeterminate { .. })),
                world.contains(&(index, Status::Unknown)),
                "{world:?} at {unit}: {reduction:?}"
            );
        }
    }
}

#[test]
fn a_determined_answer_holds_in_every_completion() {
    assert_eq!(worlds().len(), 216);
    let mut past_an_unknown = 0;
    for (name, relation, total) in unplaced_shapes().into_iter().chain(placed_shapes()) {
        let program = program(relation, total);
        for world in worlds() {
            for person in PERSONS {
                let answer = reduce(&program, &world, person);
                if !matches!(answer, Ok(CompleteReduction::Determined(_))) {
                    continue;
                }
                let resolved_worlds = completions(&world);
                let unknowns = world
                    .iter()
                    .filter(|(_, status)| *status == Status::Unknown)
                    .count();
                assert_eq!(resolved_worlds.len(), 1 << unknowns);
                for resolved in &resolved_worlds {
                    assert_eq!(
                        reduce(&program, resolved, person),
                        answer,
                        "{name} in {world:?} completed as {resolved:?}"
                    );
                }
                past_an_unknown += usize::from(resolved_worlds.len() > 1);
            }
        }
    }
    assert!(past_an_unknown > 0, "no answer got past an unknown");
}
