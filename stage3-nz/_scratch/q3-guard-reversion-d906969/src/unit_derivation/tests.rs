use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::*;

fn citation(provision: &str) -> Citation {
    Citation::new(provision, "synthetic fixture authority")
}

fn evidence(id: &str) -> Evidence {
    Evidence {
        id: id.to_string(),
        citation: citation("fixture:evidence"),
    }
}

fn observed(value: bool, id: &str) -> ObservedBool {
    ObservedBool {
        value,
        evidence: evidence(id),
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn base_plan(persons: &[&str]) -> ConstitutionPlan {
    let _ = persons;
    ConstitutionPlan {
        id: "us:7-cfr-273-1:snap-household".to_string(),
        entity_type: "snap_household".to_string(),
        roster_relation: "us:test#dwelling_resident".to_string(),
        relations: EmissionRelations {
            unit_constituent: "us:test#member_of_household".to_string(),
            participating_member: "us:test#snap_unit_member".to_string(),
        },
        derived_bools: Vec::new(),
        edges: Vec::new(),
        cuts: Vec::new(),
        attachments: Vec::new(),
        bars: Vec::new(),
        statuses: Vec::new(),
        base_chain_policy: None,
    }
}

fn complete_input(persons: &[&str]) -> ConstitutionInput {
    ConstitutionInput {
        roster: RosterInput {
            relation: "us:test#dwelling_resident".to_string(),
            scope: "dwelling:test".to_string(),
            persons: persons.iter().map(|person| (*person).to_string()).collect(),
            completeness: Some(evidence("complete-roster")),
        },
        segment: "2026-07-01/2026-07-31".to_string(),
        segment_complete: true,
        relation_families: Vec::new(),
        bool_facts: Vec::new(),
        supplied_entities: Vec::new(),
        integrity_constraints: Vec::new(),
    }
}

fn enabled() -> UnitDerivationConfig {
    UnitDerivationConfig {
        enabled: true,
        ..UnitDerivationConfig::default()
    }
}

fn edge(id: &str, kind: EdgeKind, left: &str, right: &str, when: BoolExpr) -> EdgeRule {
    EdgeRule {
        id: id.to_string(),
        kind,
        left: left.to_string(),
        right: right.to_string(),
        when,
        citation: citation(id),
        defeaters: Vec::new(),
    }
}

fn relation_expr(family: &str, left: &str, right: &str) -> BoolExpr {
    BoolExpr::fact(FactRef::Relation {
        family: family.to_string(),
        tuple: vec![left.to_string(), right.to_string()],
    })
}

fn partition(result: &UnitDerivationResult, projection: Projection) -> Vec<Vec<String>> {
    let mut by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    for tuple in &result.memberships {
        if tuple.projection == projection && tuple.role == ProjectionRole::Member {
            by_unit
                .entry(tuple.unit.clone())
                .or_default()
                .insert(tuple.person.clone());
        }
    }
    let mut blocks = by_unit
        .into_values()
        .map(|block| block.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    blocks.sort();
    blocks
}

fn participant_count_program() -> crate::model::Program {
    let mut phase_two = crate::model::Program::default();
    phase_two
        .add_relation_schema(crate::model::RelationSchema {
            name: "us:test#snap_unit_member".to_string(),
            arity: 2,
            slot_entities: vec!["snap_household".to_string(), "person".to_string()],
            derivation: None,
        })
        .unwrap();
    phase_two
        .add_derived(crate::model::Derived {
            id: None,
            name: "participant_count".to_string(),
            entity: "snap_household".to_string(),
            dtype: crate::model::DType::Integer,
            unit: None,
            rounding: None,
            source: None,
            source_url: None,
            corpus_citation_path: None,
            semantics: crate::model::DerivedSemantics::Scalar(
                crate::model::ScalarExpr::CountRelated {
                    relation: "us:test#snap_unit_member".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    where_clause: None,
                },
            ),
            versions: Vec::new(),
        })
        .unwrap();
    phase_two
}

fn derived_relation_gate_program() -> crate::model::Program {
    use crate::model::{
        DType, Derived, DerivedSemantics, JudgmentExpr, RelatedValueRef, RelationDerivation,
        RelationSchema, ScalarExpr,
    };

    let constituent = "us:test#member_of_household";
    let participating = "us:test#snap_unit_member";
    let filtered = "us:test#known_snap_unit_member";
    let mut phase_two = crate::model::Program::default();
    for relation in [constituent, participating] {
        phase_two
            .add_relation_schema(RelationSchema {
                name: relation.to_string(),
                arity: 2,
                slot_entities: vec!["snap_household".to_string(), "person".to_string()],
                derivation: None,
            })
            .unwrap();
    }
    phase_two
        .add_relation_schema(RelationSchema {
            name: filtered.to_string(),
            arity: 2,
            slot_entities: vec!["snap_household".to_string(), "person".to_string()],
            derivation: Some(RelationDerivation {
                source_relation: constituent.to_string(),
                current_slot: 0,
                related_slot: 1,
                entity: Some("person".to_string()),
                member_relation: Some(participating.to_string()),
                slot_entities: vec!["snap_household".to_string(), "person".to_string()],
                // Keep the membership check nested so the regression proves
                // the whole predicate tree is traversed, not just a top-level
                // RelationMember special case.
                predicate: JudgmentExpr::And(vec![JudgmentExpr::Not(Box::new(JudgmentExpr::Not(
                    Box::new(JudgmentExpr::RelationMember {
                        relation: participating.to_string(),
                        current_slot: 0,
                        related_slot: 1,
                    }),
                )))]),
            }),
        })
        .unwrap();

    let derived = |name: &str, dtype, semantics| Derived {
        id: None,
        name: name.to_string(),
        entity: "snap_household".to_string(),
        dtype,
        unit: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: Vec::new(),
    };
    for item in [
        derived(
            "direct_count",
            DType::Integer,
            DerivedSemantics::Scalar(ScalarExpr::CountRelated {
                relation: participating.to_string(),
                current_slot: 0,
                related_slot: 1,
                where_clause: None,
            }),
        ),
        derived(
            "direct_sum",
            DType::Decimal,
            DerivedSemantics::Scalar(ScalarExpr::SumRelated {
                relation: participating.to_string(),
                current_slot: 0,
                related_slot: 1,
                value: RelatedValueRef::Input("amount".to_string()),
                where_clause: None,
            }),
        ),
        derived(
            "gated_count",
            DType::Integer,
            DerivedSemantics::Scalar(ScalarExpr::CountRelated {
                relation: filtered.to_string(),
                current_slot: 0,
                related_slot: 1,
                where_clause: None,
            }),
        ),
        derived(
            "gated_sum",
            DType::Decimal,
            DerivedSemantics::Scalar(ScalarExpr::SumRelated {
                relation: filtered.to_string(),
                current_slot: 0,
                related_slot: 1,
                value: RelatedValueRef::Input("amount".to_string()),
                where_clause: None,
            }),
        ),
    ] {
        phase_two.add_derived(item).unwrap();
    }
    phase_two
}

#[test]
fn runtime_flag_is_independently_off_by_default() {
    let compiled = compile(base_plan(&["a"])).unwrap();
    let error = derive_units(&compiled, &complete_input(&["a"]), &Default::default())
        .expect_err("runtime default must stay disabled");
    assert_eq!(error, UnitDerivationError::Disabled);
}

#[test]
fn scoped_family_completeness_and_locality_prevent_unknown_as_absence() {
    let mut plan = base_plan(&["a", "b", "c"]);
    plan.edges.push(edge(
        "pap-a-b",
        EdgeKind::Base,
        "a",
        "b",
        relation_expr("purchase_and_prepare", "a", "b"),
    ));
    let compiled = compile(plan).unwrap();
    let mut input = complete_input(&["a", "b", "c"]);
    input.relation_families.push(RelationFamilyInput {
        name: "purchase_and_prepare".to_string(),
        scope: input.roster.scope.clone(),
        completeness: None,
        facts: Vec::new(),
    });

    let unresolved = derive_units(&compiled, &input, &enabled()).unwrap();
    assert!(
        unresolved
            .indeterminate
            .iter()
            .any(|item| item.person == "a" && item.kind == IndeterminateKind::Membership)
    );
    assert!(
        unresolved
            .indeterminate
            .iter()
            .any(|item| item.person == "b" && item.kind == IndeterminateKind::Membership)
    );
    assert!(
        !unresolved
            .indeterminate
            .iter()
            .any(|item| item.person == "c")
    );
    assert!(partition(&unresolved, Projection::UnitConstituent).contains(&vec!["c".to_string()]));

    input.relation_families[0].completeness = Some(evidence("complete-pap"));
    let complete = derive_units(&compiled, &input, &enabled()).unwrap();
    assert!(complete.indeterminate.is_empty());
    assert_eq!(
        partition(&complete, Projection::UnitConstituent),
        vec![
            vec!["a".to_string()],
            vec!["b".to_string()],
            vec!["c".to_string()]
        ]
    );
}

#[test]
fn incomplete_roster_mints_nothing() {
    let compiled = compile(base_plan(&["a", "b"])).unwrap();
    let mut input = complete_input(&["a", "b"]);
    input.roster.completeness = None;
    let result = derive_units(&compiled, &input, &enabled()).unwrap();
    assert!(result.units.is_empty());
    assert_eq!(result.indeterminate.len(), 2);
    assert!(
        result
            .indeterminate
            .iter()
            .all(|item| { item.kind == IndeterminateKind::RosterNotAssertedComplete })
    );
}

#[test]
fn composition_can_be_determined_while_participation_is_unknown() {
    let mut plan = base_plan(&["claimant", "excluded"]);
    plan.edges.push(edge(
        "pap",
        EdgeKind::Base,
        "claimant",
        "excluded",
        BoolExpr::Literal(true),
    ));
    plan.statuses.push(StatusRule {
        id: "ssn-noncooperation".to_string(),
        person: "excluded".to_string(),
        when: BoolExpr::fact(FactRef::Bool("ssn_noncooperation".to_string())),
        citation: citation("7-cfr-273.1(b)(7)(iv)"),
    });
    let compiled = compile(plan).unwrap();
    let result = derive_units(
        &compiled,
        &complete_input(&["claimant", "excluded"]),
        &enabled(),
    )
    .unwrap();
    assert_eq!(
        partition(&result, Projection::UnitConstituent),
        vec![vec!["claimant".to_string(), "excluded".to_string()]]
    );
    assert!(result.indeterminate.iter().any(|item| {
        item.person == "excluded"
            && item.kind == IndeterminateKind::Role(Projection::ParticipatingMember)
    }));
    assert!(
        !result
            .indeterminate
            .iter()
            .any(|item| item.person == "claimant")
    );

    let mut known_input = complete_input(&["claimant", "excluded"]);
    known_input.bool_facts.push(BoolFactInput {
        name: "ssn_noncooperation".to_string(),
        observations: vec![observed(true, "known-ssn-noncooperation")],
        explicit_unknown: None,
    });
    let known = derive_units(&compiled, &known_input, &enabled()).unwrap();
    let run = PrototypeRun {
        derivation: known,
        comparisons: Vec::new(),
    };
    let period = crate::model::Period::month(2026, 7);
    let phase_two = participant_count_program();
    let dataset = materialize_phase_two_dataset(
        &crate::model::DataSet::default(),
        &phase_two,
        &known_input,
        &run,
        crate::model::Interval::covering(&period),
    )
    .unwrap();
    let mut engine = dataset.engine(&phase_two);
    assert_eq!(
        engine
            .evaluate_scalar("participant_count", &run.derivation.units[0].id, &period)
            .unwrap(),
        CompleteReduction::Determined(crate::model::ScalarValue::Integer(1))
    );
}

#[test]
fn unknown_status_survives_materialization_and_indeterminates_phase_two_count() {
    let mut plan = base_plan(&["a", "b"]);
    plan.edges.push(edge(
        "pap",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Literal(true),
    ));
    plan.statuses.push(StatusRule {
        id: "unknown-status-for-b".to_string(),
        person: "b".to_string(),
        when: BoolExpr::fact(FactRef::Bool("status_b".to_string())),
        citation: citation("review:S1-adversarial-status"),
    });
    let input = complete_input(&["a", "b"]);
    let derivation = derive_units(&compile(plan).unwrap(), &input, &enabled()).unwrap();
    assert!(derivation.indeterminate.iter().any(|item| {
        item.person == "b" && item.kind == IndeterminateKind::Role(Projection::ParticipatingMember)
    }));

    let run = PrototypeRun {
        derivation,
        comparisons: Vec::new(),
    };
    let period = crate::model::Period::month(2026, 7);
    let phase_two = participant_count_program();
    let materialized = materialize_phase_two_dataset(
        &crate::model::DataSet::default(),
        &phase_two,
        &input,
        &run,
        crate::model::Interval::covering(&period),
    )
    .unwrap();
    assert_eq!(materialized.relation_knowledge().len(), 1);
    let mut engine = materialized.engine(&phase_two);
    let reduction = engine
        .evaluate_scalar("participant_count", &run.derivation.units[0].id, &period)
        .unwrap();
    assert!(matches!(
        reduction,
        CompleteReduction::Indeterminate { ref reasons }
            if reasons == &set(&["bool:status_b"])
    ));
}

#[test]
fn unknown_and_conflict_survive_derived_relation_gates_for_counts_and_sums() {
    for conflict in [false, true] {
        let label = if conflict { "Conflict" } else { "Unknown" };
        let mut plan = base_plan(&["a", "b"]);
        plan.edges.push(edge(
            "known-family-edge",
            EdgeKind::Base,
            "a",
            "b",
            BoolExpr::Literal(true),
        ));
        plan.statuses.push(StatusRule {
            id: "status-b".to_string(),
            person: "b".to_string(),
            when: BoolExpr::fact(FactRef::Bool("status_b".to_string())),
            citation: citation("review:S-E-derived-relation-gate"),
        });
        let mut input = complete_input(&["a", "b"]);
        if conflict {
            input.bool_facts.push(BoolFactInput {
                name: "status_b".to_string(),
                observations: vec![
                    observed(true, "status-source-true"),
                    observed(false, "status-source-false"),
                ],
                explicit_unknown: None,
            });
        }

        let derivation = derive_units(&compile(plan).unwrap(), &input, &enabled()).unwrap();
        assert!(derivation.indeterminate.iter().any(|item| {
            item.person == "b"
                && item.kind == IndeterminateKind::Role(Projection::ParticipatingMember)
                && item.unresolved_facts == set(&["bool:status_b"])
        }));
        let unit = derivation.units[0].id.clone();
        let run = PrototypeRun {
            derivation,
            comparisons: Vec::new(),
        };
        let period = crate::model::Period::month(2026, 7);
        let interval = crate::model::Interval::covering(&period);
        let phase_two = derived_relation_gate_program();
        let mut base = crate::model::DataSet::default();
        base.add_input(
            "amount",
            "person",
            "a",
            interval.clone(),
            crate::model::ScalarValue::Integer(10),
        );
        base.add_input(
            "amount",
            "person",
            "b",
            interval.clone(),
            crate::model::ScalarValue::Integer(20),
        );
        let materialized =
            materialize_phase_two_dataset(&base, &phase_two, &input, &run, interval).unwrap();
        assert_eq!(materialized.relation_knowledge().len(), 1);
        let unresolved = &materialized.relation_knowledge()[0];
        assert_eq!(unresolved.name, "us:test#snap_unit_member");
        assert_eq!(unresolved.tuple, vec![unit.clone(), "b".to_string()]);
        match (&unresolved.truth, conflict) {
            (LiftedTruth::Unknown { reasons }, false)
            | (LiftedTruth::Conflict { reasons }, true) => {
                assert_eq!(reasons, &set(&["bool:status_b"]));
            }
            (truth, _) => panic!("{label} materialized as {truth:?}"),
        }

        let mut engine = materialized.engine(&phase_two);
        for name in ["direct_count", "direct_sum", "gated_count", "gated_sum"] {
            assert_eq!(
                engine.evaluate_scalar(name, &unit, &period).unwrap(),
                CompleteReduction::Indeterminate {
                    reasons: set(&["bool:status_b"]),
                },
                "{label} was silently dropped by `{name}`"
            );
        }
    }
}

fn simultaneous_cut_plan(decision: CutEdgeDecision) -> ConstitutionPlan {
    let mut plan = base_plan(&["a", "b", "c", "d", "e"]);
    for (id, left, right) in [
        ("base-ab", "a", "b"),
        ("base-ac", "a", "c"),
        ("base-bc", "b", "c"),
        ("base-de", "d", "e"),
    ] {
        plan.edges.push(edge(
            id,
            EdgeKind::Base,
            left,
            right,
            BoolExpr::Literal(true),
        ));
    }
    for (id, left, right) in [("under22-a-c", "a", "c"), ("under22-b-c", "b", "c")] {
        plan.edges.push(edge(
            id,
            EdgeKind::Combination,
            left,
            right,
            BoolExpr::Literal(true),
        ));
    }
    plan.cuts.push(CutRule {
        id: "elderly-disabled".to_string(),
        members: set(&["a", "b"]),
        when: BoolExpr::Literal(true),
        citation: citation("7-cfr-273.1(b)(2)"),
        election: None,
        edge_precedence: vec![
            CutEdgePrecedence {
                edge_rule: "under22-a-c".to_string(),
                decision: decision.clone(),
            },
            CutEdgePrecedence {
                edge_rule: "under22-b-c".to_string(),
                decision,
            },
        ],
        precedes: Vec::new(),
    });
    plan
}

#[test]
fn snap_elderly_disabled_precedence_is_shown_both_ways_and_can_remain_unresolved() {
    let input = complete_input(&["a", "b", "c", "d", "e"]);
    let blocked = derive_units(
        &compile(simultaneous_cut_plan(CutEdgeDecision::Blocked {
            citation: citation("reviewed-block-reading"),
        }))
        .unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(
        partition(&blocked, Projection::UnitConstituent),
        vec![
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["d".to_string(), "e".to_string()],
        ]
    );

    let overrides = derive_units(
        &compile(simultaneous_cut_plan(CutEdgeDecision::Overrides {
            citation: citation("reviewed-override-reading"),
        }))
        .unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(
        partition(&overrides, Projection::UnitConstituent),
        vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["c".to_string()],
            vec!["d".to_string(), "e".to_string()],
        ]
    );

    let unresolved = derive_units(
        &compile(simultaneous_cut_plan(CutEdgeDecision::Unresolved {
            issue: "tier-b:b1-b2-precedence".to_string(),
        }))
        .unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert!(["a", "b", "c"].iter().all(|person| {
        unresolved
            .indeterminate
            .iter()
            .any(|item| &item.person == person)
    }));
    assert!(["d", "e"].iter().all(|person| {
        !unresolved
            .indeterminate
            .iter()
            .any(|item| &item.person == person)
    }));
    assert_eq!(
        partition(&unresolved, Projection::UnitConstituent),
        vec![vec!["d".to_string(), "e".to_string()]]
    );
}

#[test]
fn disjoint_cuts_are_all_blocked_against_one_frozen_edge_set() {
    let mut plan = base_plan(&["a", "b", "c", "d"]);
    for (id, kind, left, right) in [
        ("base-ab", EdgeKind::Base, "a", "b"),
        ("base-cd", EdgeKind::Base, "c", "d"),
        ("combo-ac", EdgeKind::Combination, "a", "c"),
        ("combo-bd", EdgeKind::Combination, "b", "d"),
    ] {
        plan.edges
            .push(edge(id, kind, left, right, BoolExpr::Literal(true)));
    }
    for (id, members) in [
        ("left-cut", set(&["a", "b"])),
        ("right-cut", set(&["c", "d"])),
    ] {
        plan.cuts.push(CutRule {
            id: id.to_string(),
            members,
            when: BoolExpr::Literal(true),
            citation: citation(id),
            election: None,
            edge_precedence: ["combo-ac", "combo-bd"]
                .into_iter()
                .map(|edge_rule| CutEdgePrecedence {
                    edge_rule: edge_rule.to_string(),
                    decision: CutEdgeDecision::Blocked {
                        citation: citation("combination-blocks"),
                    },
                })
                .collect(),
            precedes: Vec::new(),
        });
    }
    let result = derive_units(
        &compile(plan).unwrap(),
        &complete_input(&["a", "b", "c", "d"]),
        &enabled(),
    )
    .unwrap();
    let blocked = result
        .trace
        .events
        .iter()
        .filter(|event| matches!(event, TraceEvent::CutBlocked { .. }))
        .count();
    assert_eq!(blocked, 4);
    assert_eq!(
        partition(&result, Projection::UnitConstituent),
        vec![vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ]]
    );
}

#[test]
fn undeclared_overlap_conflicts_while_identical_cuts_coalesce() {
    let mut overlap = base_plan(&["a", "b", "c", "d"]);
    for (id, members) in [("ab", set(&["a", "b"])), ("bc", set(&["b", "c"]))] {
        overlap.cuts.push(CutRule {
            id: id.to_string(),
            members,
            when: BoolExpr::Literal(true),
            citation: citation(id),
            election: None,
            edge_precedence: Vec::new(),
            precedes: Vec::new(),
        });
    }
    let result = derive_units(
        &compile(overlap).unwrap(),
        &complete_input(&["a", "b", "c", "d"]),
        &enabled(),
    )
    .unwrap();
    assert!(["a", "b", "c"].iter().all(|person| {
        result.indeterminate.iter().any(|item| {
            item.person == *person && item.kind == IndeterminateKind::ConstitutionOverlapConflict
        })
    }));
    assert!(!result.indeterminate.iter().any(|item| item.person == "d"));

    let mut identical = base_plan(&["a", "b", "c"]);
    for id in ["same-one", "same-two"] {
        identical.cuts.push(CutRule {
            id: id.to_string(),
            members: set(&["a", "b"]),
            when: BoolExpr::Literal(true),
            citation: citation(id),
            election: None,
            edge_precedence: Vec::new(),
            precedes: Vec::new(),
        });
    }
    let coalesced = derive_units(
        &compile(identical).unwrap(),
        &complete_input(&["a", "b", "c"]),
        &enabled(),
    )
    .unwrap();
    assert!(coalesced.indeterminate.is_empty());
    assert_eq!(
        partition(&coalesced, Projection::UnitConstituent),
        vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["c".to_string()]
        ]
    );
}

#[test]
fn identical_cuts_with_contradictory_legal_decisions_fail_compilation() {
    let mut plan = base_plan(&["a", "b", "c"]);
    plan.edges.push(edge(
        "combination-b-c",
        EdgeKind::Combination,
        "b",
        "c",
        BoolExpr::Literal(true),
    ));
    for (id, decision) in [
        (
            "same-blocked",
            CutEdgeDecision::Blocked {
                citation: citation("blocked-reading"),
            },
        ),
        (
            "same-overrides",
            CutEdgeDecision::Overrides {
                citation: citation("override-reading"),
            },
        ),
    ] {
        plan.cuts.push(CutRule {
            id: id.to_string(),
            members: set(&["a", "b"]),
            when: BoolExpr::Literal(true),
            citation: citation(id),
            election: None,
            edge_precedence: vec![CutEdgePrecedence {
                edge_rule: "combination-b-c".to_string(),
                decision,
            }],
            precedes: Vec::new(),
        });
    }

    let error = compile(plan).expect_err("contradictory coalesced cuts need explicit precedence");
    assert!(matches!(
        error,
        UnitDerivationError::InvalidPlan(message)
            if message.contains("coalescible cuts") && message.contains("without explicit cut precedence")
    ));
}

fn boarder_plan(request: BoolExpr) -> ConstitutionPlan {
    let mut plan = base_plan(&["provider", "provider_spouse", "boarder", "boarder_spouse"]);
    let election = match &request {
        BoolExpr::Fact(FactRef::Bool(fact)) if fact == "provider_request" => {
            Some(ElectionRequirement {
                fact: fact.clone(),
                actor: "provider candidate".to_string(),
                missing: MissingElectionPolicy::Unknown,
            })
        }
        _ => None,
    };
    plan.edges.push(edge(
        "provider-group",
        EdgeKind::Base,
        "provider",
        "provider_spouse",
        BoolExpr::Literal(true),
    ));
    plan.edges.push(edge(
        "boarder-spouse",
        EdgeKind::Combination,
        "boarder",
        "boarder_spouse",
        BoolExpr::Literal(true),
    ));
    plan.attachments.push(AttachmentRule {
        id: "provider-request-attachment".to_string(),
        members: set(&["boarder", "boarder_spouse"]),
        target_anchor: "provider".to_string(),
        when: request,
        actor: "provider candidate".to_string(),
        election,
        citation: citation("7-cfr-273.1(b)(3)"),
    });
    plan.bars.push(BarRule {
        id: "independent-participation-bar".to_string(),
        members: set(&["boarder", "boarder_spouse"]),
        when: BoolExpr::Literal(true),
        citation: citation("7-cfr-273.1(b)(3)"),
    });
    plan
}

#[test]
fn attachment_and_independent_bar_are_distinct_and_requests_have_no_default() {
    let input = complete_input(&["provider", "provider_spouse", "boarder", "boarder_spouse"]);
    let unattached = derive_units(
        &compile(boarder_plan(BoolExpr::Literal(false))).unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(unattached.units.len(), 2);
    assert!(unattached.memberships.iter().any(|tuple| {
        tuple.person == "boarder" && tuple.role == ProjectionRole::BarredIndependent
    }));

    let attached = derive_units(
        &compile(boarder_plan(BoolExpr::Literal(true))).unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(attached.units.len(), 1);
    assert!(attached.memberships.iter().all(|tuple| {
        tuple.projection != Projection::ParticipatingMember || tuple.role == ProjectionRole::Member
    }));

    let unresolved = derive_units(
        &compile(boarder_plan(BoolExpr::fact(FactRef::Bool(
            "provider_request".to_string(),
        ))))
        .unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(unresolved.indeterminate.len(), 4);
    assert!(unresolved.units.is_empty());

    let mut defaulted_plan = boarder_plan(BoolExpr::fact(FactRef::Bool(
        "provider_request".to_string(),
    )));
    defaulted_plan.attachments[0].election = Some(ElectionRequirement {
        fact: "provider_request".to_string(),
        actor: "provider candidate".to_string(),
        missing: MissingElectionPolicy::AuthorityDefault {
            value: false,
            citation: citation("cited-administrative-default"),
        },
    });
    let defaulted = derive_units(
        &compile(defaulted_plan.clone()).unwrap(),
        &input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(defaulted.units.len(), 2);
    assert!(defaulted.trace.events.iter().any(|event| {
        matches!(
            event,
            TraceEvent::ElectionDefault {
                fact,
                actor,
                value: false,
                ..
            } if fact == "provider_request" && actor == "provider candidate"
        )
    }));

    let mut explicitly_unknown_input = input;
    explicitly_unknown_input.bool_facts.push(BoolFactInput {
        name: "provider_request".to_string(),
        observations: Vec::new(),
        explicit_unknown: Some(evidence("explicit-unknown-request")),
    });
    let explicitly_unknown = derive_units(
        &compile(defaulted_plan).unwrap(),
        &explicitly_unknown_input,
        &enabled(),
    )
    .unwrap();
    assert_eq!(explicitly_unknown.indeterminate.len(), 4);
    assert!(
        !explicitly_unknown
            .trace
            .events
            .iter()
            .any(|event| { matches!(event, TraceEvent::ElectionDefault { .. }) })
    );
}

#[test]
fn explicit_unknown_marker_cannot_be_overridden_by_observations() {
    let mut plan = base_plan(&["provider", "boarder"]);
    plan.attachments.push(AttachmentRule {
        id: "request-attachment".to_string(),
        members: set(&["boarder"]),
        target_anchor: "provider".to_string(),
        when: BoolExpr::fact(FactRef::Bool("provider_request".to_string())),
        actor: "provider candidate".to_string(),
        election: Some(ElectionRequirement {
            fact: "provider_request".to_string(),
            actor: "provider candidate".to_string(),
            missing: MissingElectionPolicy::Unknown,
        }),
        citation: citation("request-authority"),
    });
    let mut input = complete_input(&["provider", "boarder"]);
    input.bool_facts.push(BoolFactInput {
        name: "provider_request".to_string(),
        observations: vec![observed(true, "observed-true")],
        explicit_unknown: Some(evidence("explicitly-unknown")),
    });

    let error = derive_units(&compile(plan).unwrap(), &input, &enabled())
        .expect_err("explicit Unknown plus observations is structurally inconsistent");
    assert!(matches!(
        error,
        UnitDerivationError::InvalidPlan(message)
            if message.contains("both explicit Unknown and observations")
    ));
}

#[test]
fn actors_are_authoritative_and_request_evidence_is_traced_for_cuts_and_attachments() {
    let mut plan = base_plan(&["provider", "boarder"]);
    plan.cuts.push(CutRule {
        id: "requested-cut".to_string(),
        members: set(&["boarder"]),
        when: BoolExpr::fact(FactRef::Bool("cut_request".to_string())),
        citation: citation("cut-authority"),
        election: Some(ElectionRequirement {
            fact: "cut_request".to_string(),
            actor: "boarder".to_string(),
            missing: MissingElectionPolicy::Unknown,
        }),
        edge_precedence: Vec::new(),
        precedes: Vec::new(),
    });
    plan.attachments.push(AttachmentRule {
        id: "provider-attachment".to_string(),
        members: set(&["boarder"]),
        target_anchor: "provider".to_string(),
        when: BoolExpr::fact(FactRef::Bool("provider_request".to_string())),
        actor: "provider candidate".to_string(),
        election: Some(ElectionRequirement {
            fact: "provider_request".to_string(),
            actor: "provider candidate".to_string(),
            missing: MissingElectionPolicy::Unknown,
        }),
        citation: citation("attachment-authority"),
    });

    let mut mismatch = plan.clone();
    mismatch.attachments[0].actor = "different actor".to_string();
    assert!(matches!(
        compile(mismatch).unwrap_err(),
        UnitDerivationError::InvalidPlan(message) if message.contains("disagrees with election actor")
    ));

    let mut input = complete_input(&["provider", "boarder"]);
    input.bool_facts.extend([
        BoolFactInput {
            name: "cut_request".to_string(),
            observations: vec![observed(true, "cut-request-evidence")],
            explicit_unknown: None,
        },
        BoolFactInput {
            name: "provider_request".to_string(),
            observations: vec![observed(true, "attachment-request-evidence")],
            explicit_unknown: None,
        },
    ]);
    let result = derive_units(&compile(plan).unwrap(), &input, &enabled()).unwrap();
    assert!(result.trace.events.iter().any(|event| matches!(
        event,
        TraceEvent::CutApplied { rule, actor: Some(actor), request_evidence }
            if rule == "requested-cut"
                && actor == "boarder"
                && request_evidence == &set(&["cut-request-evidence"])
    )));
    assert!(result.trace.events.iter().any(|event| matches!(
        event,
        TraceEvent::Attachment { rule, actor, request_evidence, .. }
            if rule == "provider-attachment"
                && actor == "provider candidate"
                && request_evidence == &set(&["attachment-request-evidence"])
    )));
}

#[test]
fn dependency_check_uses_the_full_transitive_closure() {
    let mut plan = base_plan(&["a", "b"]);
    plan.derived_bools.push(DerivedBool {
        id: "unit-scoped".to_string(),
        stratum: Stratum::Unit,
        expr: BoolExpr::Literal(true),
    });
    plan.derived_bools.push(DerivedBool {
        id: "mislabelled-person".to_string(),
        stratum: Stratum::Person,
        expr: BoolExpr::Derived("unit-scoped".to_string()),
    });
    plan.edges.push(edge(
        "bad-edge",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Derived("mislabelled-person".to_string()),
    ));
    let error = compile(plan).expect_err("transitive unit dependency must fail");
    assert!(matches!(
        error,
        UnitDerivationError::UnitStratumDependency(_)
    ));
}

#[test]
fn tier_c_base_chain_has_no_engine_default() {
    let mut plan = base_plan(&["a", "b", "c"]);
    plan.edges.push(edge(
        "ab",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Literal(true),
    ));
    plan.edges.push(edge(
        "bc",
        EdgeKind::Base,
        "b",
        "c",
        BoolExpr::Literal(true),
    ));
    let input = complete_input(&["a", "b", "c"]);
    let no_policy = derive_units(&compile(plan.clone()).unwrap(), &input, &enabled()).unwrap();
    assert!(no_policy.units.is_empty());
    assert!(
        no_policy
            .indeterminate
            .iter()
            .all(|item| { item.kind == IndeterminateKind::DiscretionNotEncoded })
    );

    plan.base_chain_policy = Some(citation("named-state-273.1(c)-policy"));
    let explicit = derive_units(&compile(plan).unwrap(), &input, &enabled()).unwrap();
    assert_eq!(explicit.units.len(), 1);
}

#[test]
fn lifted_derived_relation_never_drops_unknown_or_conflict() {
    let relation = lift_derived_relation(vec![
        LiftedCandidate {
            id: "kept".to_string(),
            predicate: LiftedTruth::Holds,
        },
        LiftedCandidate {
            id: "dropped".to_string(),
            predicate: LiftedTruth::DoesNotHold,
        },
        LiftedCandidate {
            id: "unknown".to_string(),
            predicate: LiftedTruth::Unknown {
                reasons: set(&["missing-ssn-fact"]),
            },
        },
        LiftedCandidate {
            id: "conflict".to_string(),
            predicate: LiftedTruth::Conflict {
                reasons: set(&["conflicting-status-records"]),
            },
        },
    ]);
    assert_eq!(relation.members, vec!["kept"]);
    assert_eq!(relation.unresolved.len(), 2);
    assert!(matches!(
        relation.count_complete(),
        CompleteReduction::Indeterminate { .. }
    ));
}

#[test]
fn runtime_entity_registry_rejects_supplied_and_derived_collisions() {
    let mut registry = EntityRegistry::default();
    registry.bind_supplied("person", "same-id", "e1").unwrap();
    let collision = registry
        .bind_supplied("household", "same-id", "e2")
        .expect_err("all runtime instance collisions fail");
    assert!(matches!(
        collision,
        UnitDerivationError::EntityCollision { .. }
    ));

    let compiled = compile(base_plan(&["a"])).unwrap();
    let input = complete_input(&["a"]);
    let first = derive_units(&compiled, &input, &enabled()).unwrap();
    let mut colliding = input;
    colliding.supplied_entities.push(SuppliedEntity {
        entity_type: "supplied_household".to_string(),
        id: first.units[0].id.clone(),
        evidence: evidence("supplied-unit"),
    });
    let error = derive_units(&compiled, &colliding, &enabled())
        .expect_err("emission must insert through the registry");
    assert!(matches!(error, UnitDerivationError::EntityCollision { .. }));
}

#[test]
fn materialization_rebuilds_complete_registry_and_rejects_base_dataset_collision() {
    let compiled = compile(base_plan(&["a"])).unwrap();
    let input = complete_input(&["a"]);
    let derivation = derive_units(&compiled, &input, &enabled()).unwrap();
    let derived_unit_id = derivation.units[0].id.clone();
    let run = PrototypeRun {
        derivation,
        comparisons: Vec::new(),
    };
    let period = crate::model::Period::month(2026, 7);
    let interval = crate::model::Interval::covering(&period);
    let phase_two = participant_count_program();

    let mut noncolliding = crate::model::DataSet::default();
    noncolliding.add_input(
        "age",
        "person",
        "base-only-person",
        interval.clone(),
        crate::model::ScalarValue::Integer(40),
    );
    let materialized =
        materialize_phase_two_dataset(&noncolliding, &phase_two, &input, &run, interval.clone())
            .unwrap();
    assert_eq!(
        materialized
            .registry()
            .get("base-only-person")
            .map(|entry| entry.entity_type.as_str()),
        Some("person")
    );
    assert!(materialized.registry().get("a").is_some());
    assert!(materialized.registry().get(&derived_unit_id).is_some());

    let mut colliding = crate::model::DataSet::default();
    colliding.add_input(
        "adversarial",
        "some_other_entity_type",
        derived_unit_id,
        interval.clone(),
        crate::model::ScalarValue::Bool(true),
    );
    let error = materialize_phase_two_dataset(&colliding, &phase_two, &input, &run, interval)
        .expect_err("complete base dataset must bind before derived units");
    assert!(matches!(error, UnitDerivationError::EntityCollision { .. }));
}

#[test]
fn shadow_channel_is_checked_non_evaluable_and_partition_normalized() {
    let mut plan = base_plan(&["a", "b"]);
    plan.edges.push(edge(
        "group",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Literal(true),
    ));
    let compiled = compile(plan).unwrap();
    let entries = [Projection::UnitConstituent, Projection::ParticipatingMember]
        .into_iter()
        .map(|projection| LedgerEntry {
            fixture: "shadow-case".to_string(),
            projection,
            expectation: LedgerExpectation::Equal,
            statutory_basis: citation("frozen-before-run"),
        })
        .collect();
    let prototype = Prototype::new(compiled, enabled(), FrozenLedger::new(entries).unwrap());
    let supplied = [Projection::UnitConstituent, Projection::ParticipatingMember]
        .into_iter()
        .flat_map(|projection| {
            ["a", "b"]
                .into_iter()
                .map(move |person| SuppliedMembershipTuple {
                    relation: match projection {
                        Projection::UnitConstituent => "us:test#member_of_household",
                        Projection::ParticipatingMember => "us:test#snap_unit_member",
                    }
                    .to_string(),
                    unit: "caller-unit-id-is-ignored".to_string(),
                    person: person.to_string(),
                    projection,
                })
        })
        .collect::<Vec<_>>();

    let direct_error = prototype
        .run(&complete_input(&["a", "b"]), &supplied, None)
        .expect_err("direct supplied and derived membership must conflict");
    assert!(matches!(
        direct_error,
        UnitDerivationError::SuppliedAndDerivedMembership(_)
    ));

    let shadow = prototype.bind_shadow("shadow-case", supplied).unwrap();
    let run = prototype
        .run(&complete_input(&["a", "b"]), &[], Some(&shadow))
        .unwrap();
    assert_eq!(run.comparisons.len(), 2);
    assert!(
        run.comparisons
            .iter()
            .all(|item| { item.observed == ObservedComparison::Equal && item.conforms_to_ledger })
    );
    assert!(
        run.derivation
            .memberships
            .iter()
            .all(|tuple| { tuple.unit != "caller-unit-id-is-ignored" })
    );
    let first_bytes = serialize_experimental_run(&run).unwrap();
    let second_bytes = serialize_experimental_run(&run).unwrap();
    assert_eq!(first_bytes, second_bytes);
    let envelope: serde_json::Value = serde_json::from_slice(&first_bytes).unwrap();
    assert_eq!(
        envelope["experimental_semantics_version"],
        EXPERIMENTAL_SEMANTICS_VERSION
    );
    assert!(envelope.get("artifact_format_version").is_none());
}

#[test]
fn invariant_normalized_input_conflict_is_classified_per_affected_projection() {
    let mut plan = base_plan(&["a", "b"]);
    plan.edges.push(edge(
        "group",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Literal(true),
    ));
    plan.statuses.push(StatusRule {
        id: "status-b".to_string(),
        person: "b".to_string(),
        when: BoolExpr::fact(FactRef::Bool("conflicted_status".to_string())),
        citation: citation("status-authority"),
    });
    let compiled = compile(plan).unwrap();
    let ledger = FrozenLedger::new(vec![
        LedgerEntry {
            fixture: "conflict-case".to_string(),
            projection: Projection::UnitConstituent,
            expectation: LedgerExpectation::Equal,
            statutory_basis: citation("frozen-constituent"),
        },
        LedgerEntry {
            fixture: "conflict-case".to_string(),
            projection: Projection::ParticipatingMember,
            expectation: LedgerExpectation::Conflict,
            statutory_basis: citation("frozen-participating"),
        },
    ])
    .unwrap();
    let prototype = Prototype::new(compiled, enabled(), ledger);
    let supplied = vec![
        SuppliedMembershipTuple {
            relation: "us:test#member_of_household".to_string(),
            unit: "shadow".to_string(),
            person: "a".to_string(),
            projection: Projection::UnitConstituent,
        },
        SuppliedMembershipTuple {
            relation: "us:test#member_of_household".to_string(),
            unit: "shadow".to_string(),
            person: "b".to_string(),
            projection: Projection::UnitConstituent,
        },
        SuppliedMembershipTuple {
            relation: "us:test#snap_unit_member".to_string(),
            unit: "shadow".to_string(),
            person: "a".to_string(),
            projection: Projection::ParticipatingMember,
        },
    ];
    let shadow = prototype.bind_shadow("conflict-case", supplied).unwrap();
    let mut input = complete_input(&["a", "b"]);
    input.bool_facts.push(BoolFactInput {
        name: "conflicted_status".to_string(),
        observations: vec![observed(true, "source-one"), observed(true, "source-two")],
        explicit_unknown: None,
    });
    let run = prototype.run(&input, &[], Some(&shadow)).unwrap();
    assert!(run.derivation.indeterminate.is_empty());
    assert_eq!(run.derivation.trace.input_conflict_impacts.len(), 1);
    assert_eq!(run.comparisons[0].observed, ObservedComparison::Equal);
    assert_eq!(run.comparisons[1].observed, ObservedComparison::Conflict);
    assert!(
        run.comparisons
            .iter()
            .all(|comparison| comparison.conforms_to_ledger)
    );
}

fn nz_evidence(id: &str) -> AggregationEvidence {
    AggregationEvidence { id: id.to_string() }
}

fn nz_known<T>(value: T, id: &str) -> AggregationKnowledge<T> {
    AggregationKnowledge::Known {
        value,
        evidence: nz_evidence(id),
    }
}

const NZ_GROSS_RULE_ID: &str =
    "nz:statutes/income_tax/family_scheme/tax_credits#best_start_tax_credit_before_abatement";

struct NzGrossModuleSource(String);

impl crate::source::ModuleSource for NzGrossModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, crate::source::SourceError> {
        Ok((target == "nz:statutes/income_tax/family_scheme/tax_credits").then(|| self.0.clone()))
    }
}

fn nz_source_artifact() -> crate::compile::CompiledProgramArtifact {
    compile_nz_source_artifact(include_str!(
        "../../tests/fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml"
    ))
}

fn compile_nz_source_artifact(source: &str) -> crate::compile::CompiledProgramArtifact {
    crate::compile::CompiledProgramArtifact::from_rulespec_with_source(
        "nz:statutes/income_tax/family_scheme/tax_credits",
        &NzGrossModuleSource(source.to_string()),
    )
    .unwrap()
}

fn nz_gross_computation(person_id: &str, gross: &str) -> EngineComputationRequest {
    let entitlement_days = match gross {
        "4041" => 365,
        "0" => 0,
        other => panic!("unsupported fixture gross `{other}`"),
    };
    EngineComputationRequest {
        dataset: serde_json::from_value(serde_json::json!({
            "inputs": [
                {
                    "name": "nz:statutes/income_tax/family_scheme/tax_credits#input.best_start_entitlement_days",
                    "entity": "Child",
                    "entity_id": person_id,
                    "interval": {"start": "2026-04-01", "end": "2027-03-31"},
                    "value": {"kind": "integer", "value": entitlement_days}
                },
                {
                    "name": "nz:statutes/income_tax/family_scheme/tax_credits#input.best_start_child_care_fraction",
                    "entity": "Child",
                    "entity_id": person_id,
                    "interval": {"start": "2026-04-01", "end": "2027-03-31"},
                    "value": {"kind": "decimal", "value": "1"}
                }
            ]
        }))
        .unwrap(),
    }
}

fn nz_person(id: &str, age_years: i64, values: &[(&str, &str)], child: bool) -> AggregationPerson {
    let gross = values.iter().find_map(|(name, value)| {
        (*name == "best_start_before_care_and_abatement").then_some(*value)
    });
    let mut scalars = values
        .iter()
        .filter(|(name, _)| *name != "best_start_before_care_and_abatement")
        .map(|(name, value)| {
            (
                (*name).to_string(),
                nz_known((*value).to_string(), &format!("scalar:{id}:{name}")),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if child {
        scalars
            .entry("in_work_tax_credit_child_exclusive_care_fraction".to_string())
            .or_insert_with(|| nz_known("1".to_string(), &format!("iwtc-care:{id}")));
    }
    AggregationPerson {
        id: id.to_string(),
        role: if child {
            AggregationPersonRole::Child
        } else {
            AggregationPersonRole::Adult
        },
        evidence: nz_evidence(&format!("person:{id}")),
        age_years: Some(nz_known(age_years, &format!("age:{id}"))),
        scalars,
        facts: if child {
            BTreeMap::from([
                (
                    "principal_care_for_family_scheme".to_string(),
                    nz_known(true, &format!("principal-care:{id}")),
                ),
                (
                    "in_work_tax_credit_principal_caregiver".to_string(),
                    nz_known(true, &format!("iwtc-principal-care:{id}")),
                ),
                (
                    "not_financially_independent_at_age_18".to_string(),
                    nz_known(true, &format!("financial-dependence:{id}")),
                ),
                (
                    "attending_school_or_tertiary_at_age_18".to_string(),
                    nz_known(true, &format!("education:{id}")),
                ),
                (
                    "commissioner_period_contains_segment_at_age_18".to_string(),
                    nz_known(true, &format!("mc9-period:{id}")),
                ),
            ])
        } else {
            BTreeMap::new()
        },
        engine_computations: gross
            .map(|gross| {
                BTreeMap::from([(
                    "best_start_before_care_and_abatement".to_string(),
                    nz_gross_computation(id, gross),
                )])
            })
            .unwrap_or_default(),
    }
}

fn nz_adult_values<'a>(
    weekly_wage: &'a str,
    annual_wage: &'a str,
    base_income: &'a str,
    net_benefit: &'a str,
    gross_benefit: &'a str,
    tax: &'a str,
    net_wage: &'a str,
    hours: &'a str,
    ietc: &'a str,
    ietc_continuous: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        ("weekly_wage", weekly_wage),
        ("annual_wage", annual_wage),
        ("annual_family_scheme_base_income", base_income),
        ("weekly_net_benefit", net_benefit),
        ("weekly_gross_benefit", gross_benefit),
        ("weekly_wage_tax", tax),
        ("weekly_net_wage", net_wage),
        ("weekly_hours", hours),
        ("ietc_weekly", ietc),
        ("ietc_continuous_weekly", ietc_continuous),
    ]
}

fn nz_relation(name: &str, tuples: &[(&str, &str)], scope: &str) -> AggregationRelationFamily {
    AggregationRelationFamily {
        name: name.to_string(),
        scope: scope.to_string(),
        completeness: Some(nz_evidence(&format!("complete:{scope}:{name}"))),
        facts: tuples
            .iter()
            .map(|(left, right)| AggregationRelationFact {
                tuple: [(*left).to_string(), (*right).to_string()],
                knowledge: nz_known(true, &format!("relation:{name}:{left}:{right}")),
            })
            .collect(),
    }
}

fn nz_request() -> AggregationRequest {
    let scope = "scenario:binding-best-start";
    AggregationRequest {
        scope: scope.to_string(),
        segment: "2026-04-01/2027-03-31".to_string(),
        roster_completeness: Some(nz_evidence("complete:roster")),
        segment_completeness: Some(nz_evidence("complete:segment")),
        persons: vec![
            nz_person(
                "primary",
                25,
                &nz_adult_values(
                    "1000.12345678901234567890123456789012345",
                    "52000",
                    "52000",
                    "11",
                    "13",
                    "101",
                    "899",
                    "40",
                    "3",
                    "4",
                ),
                false,
            ),
            nz_person(
                "partner",
                25,
                &nz_adult_values(
                    "740.87654321098765432109876543210987655",
                    "38480",
                    "38480",
                    "7",
                    "5",
                    "71",
                    "669",
                    "20",
                    "2",
                    "6",
                ),
                false,
            ),
            nz_person(
                "child-0",
                1,
                &[
                    ("best_start_before_care_and_abatement", "4041"),
                    ("best_start_claimant_care_fraction", "1"),
                ],
                true,
            ),
            nz_person(
                "child-1",
                2,
                &[
                    ("best_start_before_care_and_abatement", "4041"),
                    ("best_start_claimant_care_fraction", "1"),
                ],
                true,
            ),
        ],
        relations: vec![
            nz_relation("partner_of", &[("primary", "partner")], scope),
            nz_relation(
                "dependent_child_of",
                &[("primary", "child-0"), ("primary", "child-1")],
                scope,
            ),
        ],
        family_inputs: vec![AggregationFamilyInput {
            anchor_person: "primary".to_string(),
            evidence: nz_evidence("family-input:primary"),
            named_people: BTreeMap::from([(
                "family_unit_reference_person".to_string(),
                nz_known("primary".to_string(), "family-reference:primary"),
            )]),
            scalars: BTreeMap::from([
                (
                    "family_scheme_income".to_string(),
                    nz_known("90480".to_string(), "family-income"),
                ),
                (
                    "best_start_family_abatement".to_string(),
                    nz_known("1000".to_string(), "best-start-adjustment"),
                ),
                (
                    "best_start_abatement_threshold".to_string(),
                    nz_known("80000".to_string(), "best-start-threshold"),
                ),
                (
                    "best_start_abatement_rate".to_string(),
                    nz_known("0.1".to_string(), "best-start-rate"),
                ),
            ]),
        }],
    }
}

fn determined<T>(value: &AggregationValue<T>) -> &T {
    match value {
        AggregationValue::Determined { value } => value,
        AggregationValue::Indeterminate { reasons } => {
            panic!("expected Determined, found {reasons:?}")
        }
    }
}

fn indeterminate_reasons<T>(value: &AggregationValue<T>) -> &BTreeSet<String> {
    match value {
        AggregationValue::Indeterminate { reasons } => reasons,
        AggregationValue::Determined { .. } => panic!("expected Indeterminate value"),
    }
}

fn determined_families(value: &AggregationFamilyKnowledge) -> &[AggregatedFamily] {
    match value {
        AggregationFamilyKnowledge::Determined { value } => value,
        AggregationFamilyKnowledge::Indeterminate { reasons, .. } => {
            panic!("expected Determined families, found {reasons:?}")
        }
    }
}

fn indeterminate_families(value: &AggregationFamilyKnowledge) -> &[AggregatedFamily] {
    match value {
        AggregationFamilyKnowledge::Indeterminate {
            value: Some(value), ..
        } => value,
        AggregationFamilyKnowledge::Indeterminate {
            reasons,
            value: None,
        } => panic!("expected retained family topology, found only {reasons:?}"),
        AggregationFamilyKnowledge::Determined { .. } => {
            panic!("expected Indeterminate families")
        }
    }
}

#[test]
fn nz_family_fixture_uses_registered_artifact_and_asserts_every_output() {
    let mut registry = UnitDerivationDocumentRegistry::default();
    let source_artifact = nz_source_artifact();
    let artifact = registry
        .register_aggregation_source(
            include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml"),
            &source_artifact,
        )
        .unwrap();
    let artifact_json = artifact.to_json_pretty().unwrap();
    let mut reloaded = UnitDerivationDocumentRegistry::default();
    let plan_id = reloaded
        .register_aggregation_json(&artifact_json)
        .unwrap()
        .plan_id()
        .to_string();
    let request = nz_request();
    let result = reloaded
        .execute_aggregation(&plan_id, &request, &enabled())
        .unwrap();
    assert_eq!(result.schema, EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA);
    assert_eq!(result.plan, "nz:incomeexplorer:family-aggregation-2026-27");
    assert!(result.plan_digest.starts_with("sha256:"));
    assert!(result.trace_root.starts_with("sha256:"));
    let families = determined_families(&result.families);
    assert_eq!(families.len(), 1);
    let family = &families[0];
    assert_eq!(
        family.members,
        vec!["child-0", "child-1", "partner", "primary"]
    );
    assert_eq!(*determined(&family.partner_present), true);
    assert_eq!(
        family
            .counts
            .iter()
            .map(|(name, value)| (name.as_str(), *determined(value)))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("best_start_eligible_child_count", 2),
            ("child_count_age_0_13", 2),
            ("child_count_age_14_18", 0),
            ("dependent_child_count", 2),
            ("youngest_child_age", 1),
        ])
    );
    assert_eq!(
        family
            .predicates
            .iter()
            .map(|(name, value)| (name.as_str(), *determined(value)))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("has_dependent_children", true),
            ("has_two_or_more_dependent_children", true),
            ("jss_sole_parent_youngest_age_at_least_14", false),
            ("sole_parent_with_dependent_children", false),
            ("youngest_child_age_at_least_14", false),
        ])
    );
    assert_eq!(
        family
            .scalars
            .iter()
            .map(|(name, value)| (name.as_str(), determined(value).as_str()))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("annual_family_scheme_base_income", "90480"),
            ("annual_wages", "90480"),
            ("best_start_entitlement_days", "365"),
            ("best_start_total", "7082"),
            ("best_start_total_before_abatement", "8082"),
            ("best_start_total_continuous_abatement", "7034"),
            ("family_tax_credit_eldest_dependent_child_care_units", "1"),
            ("family_tax_credit_entitlement_days", "365"),
            (
                "family_tax_credit_subsequent_dependent_child_care_units",
                "1"
            ),
            ("hours_total", "60"),
            ("ietc_continuous_weekly", "10"),
            ("ietc_weekly", "5"),
            ("in_work_tax_credit_child_exclusive_care_fraction", "1"),
            ("in_work_tax_credit_weekly_periods", "52"),
            ("weekly_gross_benefit", "18"),
            ("weekly_net_benefit", "18"),
            ("weekly_net_wage", "1568"),
            ("weekly_wage_tax", "172"),
            ("weekly_wages", "1741"),
            ("wff_family_credit_abatement_days", "365"),
        ])
    );
    assert_eq!(family.children.len(), 2);
    for (index, child) in family.children.iter().enumerate() {
        assert_eq!(child.person, format!("child-{index}"));
        assert_eq!(child.family, family.id);
        assert_eq!(*determined(&child.age_years), (index + 1) as i64);
        assert_eq!(
            child
                .age_bands
                .iter()
                .map(|(name, value)| (name.as_str(), *determined(value)))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("best_start_eligible_child_count", true),
                ("child_count_age_0_13", true),
                ("child_count_age_14_18", false),
                ("dependent_child_count", true),
            ])
        );
        assert_eq!(
            child
                .scalars
                .iter()
                .map(|(name, value)| (name.as_str(), determined(value).as_str()))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("best_start_claimant_care_fraction", "1"),
                ("family_scheme_income", "90480"),
            ])
        );
    }

    let disabled = reloaded
        .execute_aggregation(&plan_id, &request, &Default::default())
        .expect_err("aggregation retains the independent runtime gate");
    assert_eq!(disabled, UnitDerivationError::Disabled);
}

#[test]
fn nz_tier_b_legal_choices_are_explicit_cited_inputs_and_limitations() {
    let artifact = nz_artifact();
    let plan = &artifact.plan;
    assert_eq!(
        plan.partner_presence.reference_person_input,
        "family_unit_reference_person"
    );
    assert!(
        plan.partner_presence
            .citation
            .provision
            .contains("MC 7(1) chapeau")
    );
    assert_eq!(
        plan.partner_presence.citation.authority,
        "nz/statute/act/public/2007/0097/section/MC-7"
    );
    assert!(!plan.partner_presence.citation.provision.contains("MB 2"));
    assert!(
        plan.membership_relations
            .iter()
            .find(|relation| relation.relation == "partner_of")
            .unwrap()
            .citation
            .provision
            .contains("Caller-evidenced")
    );
    assert_eq!(plan.age_18_conditions.age, 18);
    assert_eq!(
        plan.age_18_conditions.citation.provision,
        "Income Tax Act 2007 s MC 9(1)(a)-(b) and (2)-(3)"
    );
    for (input, provision) in [
        (
            &plan.age_18_conditions.not_financially_independent_input,
            "Income Tax Act 2007 s MC 9(1)(a)",
        ),
        (
            &plan.age_18_conditions.attending_school_or_tertiary_input,
            "Income Tax Act 2007 s MC 9(1)(b)",
        ),
        (
            &plan.age_18_conditions.commissioner_period_input,
            "Income Tax Act 2007 s MC 9(2)-(3)",
        ),
    ] {
        let declaration = plan
            .inputs
            .iter()
            .find(|declaration| declaration.name == *input)
            .unwrap();
        assert_eq!(
            declaration.citation.authority,
            "nz/statute/act/public/2007/0097/section/MC-9"
        );
        assert_eq!(declaration.citation.provision, provision);
    }
    let age_14_18 = plan
        .child_counts
        .iter()
        .find(|count| count.output == "child_count_age_14_18")
        .unwrap();
    assert!(
        age_14_18
            .citation
            .provision
            .contains("para (a) for ages 14-15")
    );
    assert!(
        age_14_18
            .citation
            .provision
            .contains("para (b), including not financially independent, for ages 16-17")
    );
    assert!(
        age_14_18
            .citation
            .provision
            .contains("para (c) with s MC 9(1)(a)-(b), (2)-(3) applies at age 18")
    );
    assert_eq!(
        age_14_18.citation.authority,
        "nz/statute/act/public/2007/0097/section/YA-1"
    );
    let best_start_care = plan
        .inputs
        .iter()
        .find(|input| input.name == "best_start_claimant_care_fraction")
        .unwrap();
    assert_eq!(
        best_start_care.citation.provision,
        "Income Tax Act 2007 s MG 2(5)"
    );
    let iwtc_care = plan
        .inputs
        .iter()
        .find(|input| input.name == "in_work_tax_credit_child_exclusive_care_fraction")
        .unwrap();
    assert_eq!(
        iwtc_care.citation.provision,
        "Income Tax Act 2007 ss MC 10(4) and MD 10(2), (3)(d)"
    );
    let iwtc_principal = plan
        .inputs
        .iter()
        .find(|input| input.name == "in_work_tax_credit_principal_caregiver")
        .unwrap();
    assert_eq!(
        iwtc_principal.citation.provision,
        "Income Tax Act 2007 s MC 10(3)"
    );
    assert_eq!(
        plan.child_agreements
            .iter()
            .find(|agreement| {
                agreement.output == "in_work_tax_credit_child_exclusive_care_fraction"
            })
            .unwrap()
            .citation
            .provision,
        "Income Tax Act 2007 ss MC 10(4) and MD 10(2), (3)(d)"
    );
    let limitation_ids = plan
        .limitations
        .iter()
        .map(|limitation| limitation.id.as_str())
        .collect::<BTreeSet<_>>();
    for required in [
        "mc7-applicability-and-selection-out-of-scope",
        "mc9-commissioner-period",
        "mg2-care-fact",
        "relationship-and-role-classification",
        "mc10-iwtc-care",
        "pinned-full-period-scenario",
        "jss-scenario-gate",
    ] {
        assert!(limitation_ids.contains(required));
    }
    let mc7_limitation = plan
        .limitations
        .iter()
        .find(|limitation| limitation.id == "mc7-applicability-and-selection-out-of-scope")
        .unwrap();
    assert!(mc7_limitation.statement.contains("MC 7(1) applicability"));
    assert!(
        mc7_limitation
            .statement
            .contains("MC 7(2) allocation or Commissioner selection")
    );
    assert_eq!(
        mc7_limitation.citation.provision,
        "Income Tax Act 2007 s MC 7(1)-(2)"
    );
    assert_eq!(
        plan.limitations
            .iter()
            .find(|limitation| limitation.id == "mc9-commissioner-period")
            .unwrap()
            .citation
            .provision,
        "Income Tax Act 2007 s MC 9(2)-(3)"
    );
    assert_eq!(
        plan.limitations
            .iter()
            .find(|limitation| limitation.id == "mc10-iwtc-care")
            .unwrap()
            .citation
            .provision,
        "Income Tax Act 2007 ss MC 10(3)-(4) and MD 10(2), (3)(d)"
    );
}

#[test]
fn reviewer_best_start_plan_is_rejected_by_named_family_scope_guard() {
    let source = include_str!("../../tests/fixtures/unit_derivation/per_child_expressible.yaml");
    assert!(
        include_str!("../../tests/fixtures/unit_derivation/per_child_request.json")
            .contains("\"two_child_abatement\": \"2000\"")
    );
    let error = compile_aggregation_plan(source, &nz_source_artifact())
        .expect_err("the exact accepted reviewer plan must now fail semantic validation");
    assert_eq!(
        error,
        UnitDerivationError::DuplicateFamilyScopeReduction {
            key: "untyped-family-scope".to_string(),
            first: "safe_best_start_total".to_string(),
            second: "best_start_total".to_string(),
        }
    );
}

#[test]
fn relabelled_request_scalar_is_rejected_by_named_engine_provenance_guard() {
    let artifact = compile_aggregation_plan(
        include_str!("../../tests/fixtures/unit_derivation/renamed_child_gross_plan.yaml"),
        &nz_source_artifact(),
    )
    .unwrap();
    let request: AggregationRequest = serde_json::from_str(include_str!(
        "../../tests/fixtures/unit_derivation/renamed_child_gross_request.json"
    ))
    .unwrap();

    let error = execute_aggregation_plan(&artifact, &request, &enabled())
        .expect_err("a request scalar cannot acquire engine-computed provenance by relabelling");
    match error {
        UnitDerivationError::InvalidChildGrossProvenance {
            operation,
            input,
            child,
            expected,
            found,
        } => {
            assert_eq!(operation, "best_start_total");
            assert_eq!(input, "best_start_after_per_child_abatement");
            assert_eq!(child, "child-0");
            assert_eq!(found, "request_supplied");
            assert!(expected.contains(&format!("rule_id={NZ_GROSS_RULE_ID}")));
            assert!(expected.contains("stage=gross"));
            assert!(expected.contains(&artifact.source_artifact_digest));
        }
        other => panic!("expected InvalidChildGrossProvenance, found {other:?}"),
    }
}

#[test]
fn request_scalar_cannot_launder_through_a_same_name_engine_recipe() {
    let artifact = nz_artifact();
    let mut request = nz_request();
    let child = request
        .persons
        .iter_mut()
        .find(|person| person.id == "child-0")
        .unwrap();
    child.scalars.insert(
        "best_start_before_care_and_abatement".to_string(),
        nz_known("3041".to_string(), "laundered-request-scalar"),
    );

    let error = execute_aggregation_plan(&artifact, &request, &enabled())
        .expect_err("an asserted scalar must retain request-supplied provenance");
    assert!(matches!(
        error,
        UnitDerivationError::InvalidChildGrossProvenance {
            operation,
            input,
            child,
            found,
            ..
        } if operation == "best_start_total"
            && input == "best_start_before_care_and_abatement"
            && child == "child-0"
            && found == "request_supplied"
    ));
}

#[test]
fn unknown_scalar_and_missing_relation_completeness_never_become_zero() {
    let artifact = compile_aggregation_plan(
        include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml"),
        &nz_source_artifact(),
    )
    .unwrap();
    let mut scalar_unknown = nz_request();
    scalar_unknown.persons[0].scalars.insert(
        "weekly_wage".to_string(),
        AggregationKnowledge::Unknown {
            evidence: nz_evidence("weekly-wage-unknown"),
        },
    );
    let result = execute_aggregation_plan(&artifact, &scalar_unknown, &enabled()).unwrap();
    let family = &indeterminate_families(&result.families)[0];
    assert!(matches!(
        family.scalars["weekly_wages"],
        AggregationValue::Indeterminate { .. }
    ));

    let mut scalar_conflict = nz_request();
    scalar_conflict.persons[0].scalars.insert(
        "weekly_wage".to_string(),
        AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: "1000".to_string(),
                    evidence: nz_evidence("weekly-wage-one"),
                },
                AggregationObservation {
                    value: "2000".to_string(),
                    evidence: nz_evidence("weekly-wage-two"),
                },
            ],
        },
    );
    let result = execute_aggregation_plan(&artifact, &scalar_conflict, &enabled()).unwrap();
    assert!(matches!(
        indeterminate_families(&result.families)[0].scalars["weekly_wages"],
        AggregationValue::Indeterminate { .. }
    ));

    let mut missing_family_scalar = nz_request();
    missing_family_scalar.family_inputs[0]
        .scalars
        .remove("family_scheme_income");
    let result = execute_aggregation_plan(&artifact, &missing_family_scalar, &enabled()).unwrap();
    let family = &indeterminate_families(&result.families)[0];
    assert!(matches!(
        family.scalars["best_start_total_continuous_abatement"],
        AggregationValue::Indeterminate { .. }
    ));
    assert!(family.children.iter().all(|child| matches!(
        child.scalars["family_scheme_income"],
        AggregationValue::Indeterminate { .. }
    )));

    let mut missing_completeness = nz_request();
    let missing_child_relation = missing_completeness
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap();
    missing_child_relation.completeness = None;
    missing_child_relation.facts.clear();
    let result = execute_aggregation_plan(&artifact, &missing_completeness, &enabled()).unwrap();
    assert!(matches!(
        result.families,
        AggregationFamilyKnowledge::Indeterminate { value: None, .. }
    ));
}

#[test]
fn mixed_conflict_care_and_unknown_age_indetermines_family_without_losing_topology() {
    let artifact = nz_artifact();
    let mut request = nz_request();
    request
        .persons
        .iter_mut()
        .find(|person| person.id == "child-0")
        .unwrap()
        .scalars
        .insert(
            "best_start_claimant_care_fraction".to_string(),
            AggregationKnowledge::Conflict {
                observations: vec![
                    AggregationObservation {
                        value: "1".to_string(),
                        evidence: nz_evidence("mixed:care:full"),
                    },
                    AggregationObservation {
                        value: "0.5".to_string(),
                        evidence: nz_evidence("mixed:care:half"),
                    },
                ],
            },
        );
    let child_1 = request
        .persons
        .iter_mut()
        .find(|person| person.id == "child-1")
        .unwrap();
    child_1.age_years = Some(AggregationKnowledge::Unknown {
        evidence: nz_evidence("mixed:age:unknown"),
    });

    let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
    let care_reasons = BTreeSet::from([
        "input:child-0:best_start_claimant_care_fraction:conflict:mixed:care:full".to_string(),
        "input:child-0:best_start_claimant_care_fraction:conflict:mixed:care:half".to_string(),
    ]);
    let age_reasons =
        BTreeSet::from(["input:child-1:age_years:unknown:mixed:age:unknown".to_string()]);
    let mut all_reasons = care_reasons.clone();
    all_reasons.extend(age_reasons.iter().cloned());
    let families = match &result.families {
        AggregationFamilyKnowledge::Indeterminate {
            reasons,
            value: Some(value),
        } => {
            assert_eq!(reasons, &all_reasons);
            value
        }
        other => panic!("expected Indeterminate families with retained topology, found {other:?}"),
    };

    assert_eq!(families.len(), 1);
    let family = &families[0];
    assert_eq!(
        family.members,
        vec!["child-0", "child-1", "partner", "primary"]
    );
    assert_eq!(
        family
            .children
            .iter()
            .map(|child| child.person.as_str())
            .collect::<Vec<_>>(),
        vec!["child-0", "child-1"]
    );
    let child_0 = family
        .children
        .iter()
        .find(|child| child.person == "child-0")
        .unwrap();
    let child_1 = family
        .children
        .iter()
        .find(|child| child.person == "child-1")
        .unwrap();
    assert_eq!(
        indeterminate_reasons(&child_0.scalars["best_start_claimant_care_fraction"]),
        &care_reasons
    );
    assert_eq!(
        indeterminate_reasons(&family.scalars["best_start_total"]),
        &all_reasons
    );
    assert_eq!(
        indeterminate_reasons(&family.counts["dependent_child_count"]),
        &age_reasons
    );
    assert_eq!(
        indeterminate_reasons(&family.counts["youngest_child_age"]),
        &age_reasons
    );
    assert_eq!(indeterminate_reasons(&child_1.age_years), &age_reasons);
}

#[test]
fn indeterminate_relation_knowledge_cannot_produce_a_smaller_determined_family() {
    let artifact = nz_artifact();
    let cases = vec![
        AggregationKnowledge::Unknown {
            evidence: nz_evidence("child-relation-unknown"),
        },
        AggregationKnowledge::Conflict {
            observations: vec![AggregationObservation {
                value: true,
                evidence: nz_evidence("child-relation-conflict-one"),
            }],
        },
        AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: true,
                    evidence: nz_evidence("child-relation-conflict-true"),
                },
                AggregationObservation {
                    value: false,
                    evidence: nz_evidence("child-relation-conflict-false"),
                },
            ],
        },
        AggregationKnowledge::Observations {
            observations: Vec::new(),
        },
    ];
    for knowledge in cases {
        let mut request = nz_request();
        request
            .relations
            .iter_mut()
            .find(|family| family.name == "dependent_child_of")
            .unwrap()
            .facts[0]
            .knowledge = knowledge;
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        assert!(
            matches!(
                result.families,
                AggregationFamilyKnowledge::Indeterminate { value: None, .. }
            ),
            "an unresolved child relation must not become a determined smaller count"
        );
    }
}

#[test]
fn omitted_relation_is_unknown_but_evidenced_complete_empty_is_known_empty() {
    let artifact = nz_artifact();
    let mut omitted = nz_request();
    omitted
        .relations
        .retain(|family| family.name != "dependent_child_of");
    let result = execute_aggregation_plan(&artifact, &omitted, &enabled()).unwrap();
    assert!(matches!(
        result.families,
        AggregationFamilyKnowledge::Indeterminate { value: None, .. }
    ));

    let mut complete_empty = nz_request();
    complete_empty
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts
        .clear();
    let result = execute_aggregation_plan(&artifact, &complete_empty, &enabled()).unwrap();
    let primary = indeterminate_families(&result.families)
        .iter()
        .find(|family| family.members.contains(&"primary".to_string()))
        .unwrap();
    assert_eq!(*determined(&primary.counts["dependent_child_count"]), 0);
    assert_eq!(
        determined(&primary.scalars["best_start_total_before_abatement"]),
        "0"
    );
}

fn nz_artifact() -> CompiledAggregationArtifact {
    compile_aggregation_plan(
        include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml"),
        &nz_source_artifact(),
    )
    .unwrap()
}

fn replace_children(request: &mut AggregationRequest, ages: &[i64]) {
    request
        .persons
        .retain(|person| !person.id.starts_with("child-"));
    let child_relation = request
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap();
    child_relation.facts.clear();
    for (index, age) in ages.iter().enumerate() {
        let id = format!("child-{index}");
        request.persons.push(nz_person(
            &id,
            *age,
            &[
                ("best_start_before_care_and_abatement", "4041"),
                ("best_start_claimant_care_fraction", "1"),
            ],
            true,
        ));
        child_relation.facts.push(AggregationRelationFact {
            tuple: ["primary".to_string(), id.clone()],
            knowledge: nz_known(true, &format!("relation:dependent_child_of:primary:{id}")),
        });
    }
}

#[test]
fn child_age_boundaries_and_every_mc9_condition_are_operational() {
    let artifact = nz_artifact();
    let mut request = nz_request();
    replace_children(&mut request, &[0, 2, 3, 13, 14, 18, 19]);
    let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
    let family = &determined_families(&result.families)[0];
    assert_eq!(*determined(&family.counts["dependent_child_count"]), 6);
    assert_eq!(
        *determined(&family.counts["best_start_eligible_child_count"]),
        2
    );
    assert_eq!(*determined(&family.counts["child_count_age_0_13"]), 4);
    assert_eq!(*determined(&family.counts["child_count_age_14_18"]), 2);
    assert_eq!(*determined(&family.counts["youngest_child_age"]), 0);
    let expected = [
        (true, true, false, true),
        (true, true, false, true),
        (false, true, false, true),
        (false, true, false, true),
        (false, false, true, true),
        (false, false, true, true),
        (false, false, false, false),
    ];
    for (child, expected) in family.children.iter().zip(expected) {
        assert_eq!(
            (
                *determined(&child.age_bands["best_start_eligible_child_count"]),
                *determined(&child.age_bands["child_count_age_0_13"]),
                *determined(&child.age_bands["child_count_age_14_18"]),
                *determined(&child.age_bands["dependent_child_count"]),
            ),
            expected
        );
    }

    for fact in [
        "not_financially_independent_at_age_18",
        "attending_school_or_tertiary_at_age_18",
        "commissioner_period_contains_segment_at_age_18",
    ] {
        let mut false_request = request.clone();
        false_request
            .persons
            .iter_mut()
            .find(|person| person.id == "child-5")
            .unwrap()
            .facts
            .insert(fact.to_string(), nz_known(false, &format!("false:{fact}")));
        let result = execute_aggregation_plan(&artifact, &false_request, &enabled()).unwrap();
        let family = &determined_families(&result.families)[0];
        assert_eq!(*determined(&family.counts["dependent_child_count"]), 5);
        assert_eq!(*determined(&family.counts["child_count_age_14_18"]), 1);
    }

    let mut unknown = request;
    unknown
        .persons
        .iter_mut()
        .find(|person| person.id == "child-5")
        .unwrap()
        .facts
        .insert(
            "attending_school_or_tertiary_at_age_18".to_string(),
            AggregationKnowledge::Unknown {
                evidence: nz_evidence("education-unknown"),
            },
        );
    let result = execute_aggregation_plan(&artifact, &unknown, &enabled()).unwrap();
    let family = &indeterminate_families(&result.families)[0];
    assert!(matches!(
        family.counts["dependent_child_count"],
        AggregationValue::Indeterminate { .. }
    ));
    assert!(matches!(
        family.counts["child_count_age_14_18"],
        AggregationValue::Indeterminate { .. }
    ));
}

#[test]
fn missing_unknown_and_conflicting_child_inputs_never_reduce_to_zero_or_false() {
    let artifact = nz_artifact();
    for age in [
        None,
        Some(AggregationKnowledge::Unknown {
            evidence: nz_evidence("age-unknown"),
        }),
        Some(AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: 1,
                    evidence: nz_evidence("age-one"),
                },
                AggregationObservation {
                    value: 2,
                    evidence: nz_evidence("age-two"),
                },
            ],
        }),
    ] {
        let mut request = nz_request();
        request
            .persons
            .iter_mut()
            .find(|person| person.id == "child-0")
            .unwrap()
            .age_years = age;
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        let family = &indeterminate_families(&result.families)[0];
        assert!(matches!(
            family.counts["dependent_child_count"],
            AggregationValue::Indeterminate { .. }
        ));
        assert!(matches!(
            family.children[0].age_years,
            AggregationValue::Indeterminate { .. }
        ));
    }

    for knowledge in [
        AggregationKnowledge::Unknown {
            evidence: nz_evidence("principal-care-unknown"),
        },
        AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: true,
                    evidence: nz_evidence("principal-care-yes"),
                },
                AggregationObservation {
                    value: false,
                    evidence: nz_evidence("principal-care-no"),
                },
            ],
        },
    ] {
        let mut request = nz_request();
        request
            .persons
            .iter_mut()
            .find(|person| person.id == "child-0")
            .unwrap()
            .facts
            .insert("principal_care_for_family_scheme".to_string(), knowledge);
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        let family = &indeterminate_families(&result.families)[0];
        assert!(matches!(
            family.counts["dependent_child_count"],
            AggregationValue::Indeterminate { .. }
        ));
        assert!(matches!(
            family.scalars["best_start_total"],
            AggregationValue::Indeterminate { .. }
        ));
    }
}

#[test]
fn mg2_care_fraction_and_principal_care_are_not_defaulted() {
    let artifact = nz_artifact();
    for (fraction, expected_gross) in [("0", "0"), ("0.5", "2020.5"), ("1", "4041")] {
        let mut request = nz_request();
        replace_children(&mut request, &[1]);
        request
            .persons
            .iter_mut()
            .find(|person| person.id == "child-0")
            .unwrap()
            .scalars
            .insert(
                "best_start_claimant_care_fraction".to_string(),
                nz_known(fraction.to_string(), &format!("care:{fraction}")),
            );
        request.family_inputs[0].scalars.insert(
            "best_start_family_abatement".to_string(),
            nz_known("0".to_string(), "no-adjustment"),
        );
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        let family = &determined_families(&result.families)[0];
        assert_eq!(
            determined(&family.scalars["best_start_total_before_abatement"]),
            expected_gross
        );
    }

    let mut no_principal_care = nz_request();
    replace_children(&mut no_principal_care, &[1]);
    no_principal_care
        .persons
        .iter_mut()
        .find(|person| person.id == "child-0")
        .unwrap()
        .facts
        .insert(
            "principal_care_for_family_scheme".to_string(),
            nz_known(false, "no-principal-care"),
        );
    let result = execute_aggregation_plan(&artifact, &no_principal_care, &enabled()).unwrap();
    let family = &determined_families(&result.families)[0];
    assert_eq!(*determined(&family.counts["dependent_child_count"]), 0);
    assert_eq!(
        determined(&family.scalars["best_start_total_before_abatement"]),
        "0"
    );

    for knowledge in [
        AggregationKnowledge::Unknown {
            evidence: nz_evidence("best-start-care-unknown"),
        },
        AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: "0.5".to_string(),
                    evidence: nz_evidence("best-start-care-half"),
                },
                AggregationObservation {
                    value: "1".to_string(),
                    evidence: nz_evidence("best-start-care-full"),
                },
            ],
        },
    ] {
        let mut request = nz_request();
        request.persons[2]
            .scalars
            .insert("best_start_claimant_care_fraction".to_string(), knowledge);
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        let family = &indeterminate_families(&result.families)[0];
        assert!(matches!(
            family.scalars["best_start_total"],
            AggregationValue::Indeterminate { .. }
        ));
    }

    for fraction in ["-0.1", "1.1"] {
        let mut request = nz_request();
        request.persons[2].scalars.insert(
            "best_start_claimant_care_fraction".to_string(),
            nz_known(fraction.to_string(), &format!("out-of-range:{fraction}")),
        );
        assert!(matches!(
            execute_aggregation_plan(&artifact, &request, &enabled()),
            Err(UnitDerivationError::InvalidPlan(message))
                if message.contains("must be between 0 and 1")
        ));
    }

    let mut inconsistent_iwtc = nz_request();
    inconsistent_iwtc.persons[2].scalars.insert(
        "in_work_tax_credit_child_exclusive_care_fraction".to_string(),
        nz_known("0.5".to_string(), "iwtc-care-half"),
    );
    let result = execute_aggregation_plan(&artifact, &inconsistent_iwtc, &enabled()).unwrap();
    let family = &indeterminate_families(&result.families)[0];
    assert!(matches!(
        family.scalars["in_work_tax_credit_child_exclusive_care_fraction"],
        AggregationValue::Indeterminate { .. }
    ));
}

#[test]
fn direction_partner_anchor_unpartnered_and_actual_shape_predicates_are_checked() {
    let artifact = nz_artifact();
    let mut reversed_child = nz_request();
    reversed_child
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts[0]
        .tuple = ["child-0".to_string(), "primary".to_string()];
    assert_eq!(
        execute_aggregation_plan(&artifact, &reversed_child, &enabled()).unwrap_err(),
        UnitDerivationError::InvalidRelationshipRole {
            relation: "dependent_child_of".to_string(),
            person: "child-0".to_string(),
            role: "caregiver_must_be_adult".to_string(),
        }
    );

    let mut reversed_non_holder = nz_request();
    reversed_non_holder.persons.push(nz_person(
        "caregiver-2",
        30,
        &nz_adult_values("0", "0", "0", "0", "0", "0", "0", "0", "0", "0"),
        false,
    ));
    reversed_non_holder.persons.push(nz_person(
        "child-2",
        6,
        &[
            ("best_start_before_care_and_abatement", "0"),
            ("best_start_claimant_care_fraction", "1"),
        ],
        true,
    ));
    reversed_non_holder
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts
        .push(AggregationRelationFact {
            tuple: ["child-2".to_string(), "caregiver-2".to_string()],
            knowledge: nz_known(true, "reversed-non-holder"),
        });
    assert!(matches!(
        execute_aggregation_plan(&artifact, &reversed_non_holder, &enabled()),
        Err(UnitDerivationError::InvalidRelationshipRole { person, role, .. })
            if person == "child-2" && role == "caregiver_must_be_adult"
    ));

    let mut reversed_partner = nz_request();
    reversed_partner
        .relations
        .iter_mut()
        .find(|family| family.name == "partner_of")
        .unwrap()
        .facts[0]
        .tuple = ["partner".to_string(), "primary".to_string()];
    let result = execute_aggregation_plan(&artifact, &reversed_partner, &enabled()).unwrap();
    assert!(*determined(
        &determined_families(&result.families)[0].partner_present
    ));

    let mut unpartnered = nz_request();
    unpartnered.persons.retain(|person| person.id != "partner");
    unpartnered
        .relations
        .iter_mut()
        .find(|family| family.name == "partner_of")
        .unwrap()
        .facts
        .clear();
    replace_children(&mut unpartnered, &[14]);
    let result = execute_aggregation_plan(&artifact, &unpartnered, &enabled()).unwrap();
    let family = &determined_families(&result.families)[0];
    assert!(!*determined(&family.partner_present));
    assert!(*determined(
        &family.predicates["sole_parent_with_dependent_children"]
    ));
    assert!(*determined(
        &family.predicates["jss_sole_parent_youngest_age_at_least_14"]
    ));
    assert!(*determined(&family.predicates["has_dependent_children"]));
    assert!(!*determined(
        &family.predicates["has_two_or_more_dependent_children"]
    ));

    let mut unknown_holder = nz_request();
    unknown_holder.family_inputs[0].named_people.insert(
        "family_unit_reference_person".to_string(),
        AggregationKnowledge::Unknown {
            evidence: nz_evidence("family-reference-unknown"),
        },
    );
    let result = execute_aggregation_plan(&artifact, &unknown_holder, &enabled()).unwrap();
    assert!(matches!(
        indeterminate_families(&result.families)[0].partner_present,
        AggregationValue::Indeterminate { .. }
    ));

    let mut child_holder = nz_request();
    child_holder.family_inputs[0].named_people.insert(
        "family_unit_reference_person".to_string(),
        nz_known("child-0".to_string(), "invalid-child-holder"),
    );
    assert!(matches!(
        execute_aggregation_plan(&artifact, &child_holder, &enabled()),
        Err(UnitDerivationError::InvalidRelationshipRole { person, .. })
            if person == "child-0"
    ));
}

#[test]
fn unrelated_family_preserves_family_id_but_changes_bound_trace() {
    let artifact = nz_artifact();
    let base_request = nz_request();
    let base = execute_aggregation_plan(&artifact, &base_request, &enabled()).unwrap();
    let base_family = &determined_families(&base.families)[0];
    let base_id = base_family.id.clone();

    let mut expanded = base_request.clone();
    expanded.persons.push(nz_person(
        "other-adult",
        30,
        &nz_adult_values("9", "468", "468", "0", "0", "0", "9", "1", "0", "0"),
        false,
    ));
    expanded.persons.push(nz_person(
        "other-child",
        6,
        &[
            ("best_start_before_care_and_abatement", "0"),
            ("best_start_claimant_care_fraction", "1"),
        ],
        true,
    ));
    expanded
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts
        .push(AggregationRelationFact {
            tuple: ["other-adult".to_string(), "other-child".to_string()],
            knowledge: nz_known(true, "other-family-child"),
        });
    expanded.family_inputs.push(AggregationFamilyInput {
        anchor_person: "other-adult".to_string(),
        evidence: nz_evidence("family-input:other-adult"),
        named_people: BTreeMap::from([(
            "family_unit_reference_person".to_string(),
            nz_known("other-adult".to_string(), "family-reference:other-adult"),
        )]),
        scalars: BTreeMap::from([
            (
                "family_scheme_income".to_string(),
                nz_known("468".to_string(), "other-family-income"),
            ),
            (
                "best_start_family_abatement".to_string(),
                nz_known("0".to_string(), "other-adjustment"),
            ),
            (
                "best_start_abatement_threshold".to_string(),
                nz_known("80000".to_string(), "other-threshold"),
            ),
            (
                "best_start_abatement_rate".to_string(),
                nz_known("0.1".to_string(), "other-rate"),
            ),
        ]),
    });
    let expanded_result = execute_aggregation_plan(&artifact, &expanded, &enabled()).unwrap();
    let families = determined_families(&expanded_result.families);
    assert_eq!(families.len(), 2);
    let unchanged = families
        .iter()
        .find(|family| family.members.contains(&"primary".to_string()))
        .unwrap();
    assert_eq!(unchanged.id, base_id);
    assert_eq!(unchanged, base_family);
    let other = families
        .iter()
        .find(|family| family.members.contains(&"other-adult".to_string()))
        .unwrap();
    assert_eq!(
        other.members,
        vec!["other-adult".to_string(), "other-child".to_string()]
    );
    assert!(!*determined(&other.partner_present));
    assert_eq!(*determined(&other.counts["dependent_child_count"]), 1);
    assert_eq!(
        *determined(&other.counts["best_start_eligible_child_count"]),
        0
    );
    assert_eq!(determined(&other.scalars["weekly_wages"]), "9");
    assert_eq!(determined(&other.scalars["annual_wages"]), "468");
    assert_eq!(
        determined(&other.scalars["best_start_total_before_abatement"]),
        "0"
    );
    assert_eq!(determined(&other.scalars["best_start_total"]), "0");
    assert_eq!(other.children.len(), 1);
    assert_eq!(
        determined(&other.children[0].scalars["family_scheme_income"]),
        "468"
    );
    assert_ne!(expanded_result.trace_root, base.trace_root);

    let mut reordered = base_request;
    reordered.persons.reverse();
    reordered.relations.reverse();
    for family in &mut reordered.relations {
        family.facts.reverse();
    }
    let reordered_result = execute_aggregation_plan(&artifact, &reordered, &enabled()).unwrap();
    assert_eq!(reordered_result.trace_root, base.trace_root);
    assert_eq!(reordered_result.families, base.families);
}

#[test]
fn aggregation_trace_binds_scalar_evidence_and_normalizes_same_tuple_facts() {
    let artifact = nz_artifact();
    let base_request = nz_request();
    let base = execute_aggregation_plan(&artifact, &base_request, &enabled()).unwrap();

    let mut evidence_edit = base_request.clone();
    if let AggregationKnowledge::Known { evidence, .. } = evidence_edit.persons[0]
        .scalars
        .get_mut("weekly_wage")
        .unwrap()
    {
        evidence.id = "weekly-wage-independent-source".to_string();
    }
    let edited = execute_aggregation_plan(&artifact, &evidence_edit, &enabled()).unwrap();
    assert_eq!(edited.families, base.families);
    assert_ne!(edited.trace_root, base.trace_root);

    let mut corroborated = base_request.clone();
    corroborated
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts
        .push(AggregationRelationFact {
            tuple: ["primary".to_string(), "child-0".to_string()],
            knowledge: nz_known(true, "corroborating-child-source"),
        });
    let first = execute_aggregation_plan(&artifact, &corroborated, &enabled()).unwrap();
    corroborated
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap()
        .facts
        .reverse();
    let reversed = execute_aggregation_plan(&artifact, &corroborated, &enabled()).unwrap();
    assert_eq!(reversed.trace_root, first.trace_root);
    assert_eq!(reversed.families, first.families);

    let mut exact_duplicate = base_request;
    let relation = exact_duplicate
        .relations
        .iter_mut()
        .find(|family| family.name == "dependent_child_of")
        .unwrap();
    relation.facts.push(relation.facts[0].clone());
    let deduplicated = execute_aggregation_plan(&artifact, &exact_duplicate, &enabled()).unwrap();
    assert_eq!(deduplicated.trace_root, base.trace_root);
    assert_eq!(deduplicated.families, base.families);
}

#[test]
fn conflicting_family_adjustment_is_indeterminate_and_order_invariant() {
    let artifact = nz_artifact();
    let mut agreeing = nz_request();
    agreeing.family_inputs[0].scalars.insert(
        "best_start_family_abatement".to_string(),
        AggregationKnowledge::Observations {
            observations: vec![
                AggregationObservation {
                    value: "1000".to_string(),
                    evidence: nz_evidence("agreeing-adjustment-one"),
                },
                AggregationObservation {
                    value: "1000".to_string(),
                    evidence: nz_evidence("agreeing-adjustment-two"),
                },
            ],
        },
    );
    let agreeing_result = execute_aggregation_plan(&artifact, &agreeing, &enabled()).unwrap();
    let family = &determined_families(&agreeing_result.families)[0];
    assert_eq!(determined(&family.scalars["best_start_total"]), "7082");

    let mut disagreeing = agreeing;
    if let AggregationKnowledge::Observations { observations } = disagreeing.family_inputs[0]
        .scalars
        .get_mut("best_start_family_abatement")
        .unwrap()
    {
        observations[1].value = "2000".to_string();
    }
    let disagreeing_result = execute_aggregation_plan(&artifact, &disagreeing, &enabled()).unwrap();
    assert!(matches!(
        indeterminate_families(&disagreeing_result.families)[0].scalars["best_start_total"],
        AggregationValue::Indeterminate { .. }
    ));

    let mut request = nz_request();
    request.family_inputs[0].scalars.insert(
        "best_start_family_abatement".to_string(),
        AggregationKnowledge::Conflict {
            observations: vec![
                AggregationObservation {
                    value: "1000".to_string(),
                    evidence: nz_evidence("adjustment-one"),
                },
                AggregationObservation {
                    value: "2000".to_string(),
                    evidence: nz_evidence("adjustment-two"),
                },
            ],
        },
    );
    let first = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
    let family = &indeterminate_families(&first.families)[0];
    assert!(matches!(
        family.scalars["best_start_total"],
        AggregationValue::Indeterminate { .. }
    ));
    if let AggregationKnowledge::Conflict { observations } = request.family_inputs[0]
        .scalars
        .get_mut("best_start_family_abatement")
        .unwrap()
    {
        observations.reverse();
    }
    let reversed = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
    assert_eq!(reversed.trace_root, first.trace_root);
    assert_eq!(reversed.families, first.families);
}

#[test]
fn decimal_context_uses_half_even_at_both_midpoint_parities() {
    let source =
        include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml")
            .replace("decimal_precision: 40", "decimal_precision: 2");
    let artifact = compile_aggregation_plan(&source, &nz_source_artifact()).unwrap();
    for (input, expected) in [("1.25", "1.2"), ("1.35", "1.4")] {
        let mut request = nz_request();
        request.persons.retain(|person| person.id == "primary");
        request
            .relations
            .iter_mut()
            .for_each(|family| family.facts.clear());
        request.persons[0].scalars.insert(
            "weekly_wage".to_string(),
            nz_known(input.to_string(), &format!("midpoint:{input}")),
        );
        request.family_inputs[0].scalars.insert(
            "best_start_family_abatement".to_string(),
            nz_known("0".to_string(), "zero-adjustment"),
        );
        let result = execute_aggregation_plan(&artifact, &request, &enabled()).unwrap();
        let family = &determined_families(&result.families)[0];
        assert_eq!(determined(&family.scalars["weekly_wages"]), expected);
    }
}

#[test]
fn compiled_artifact_digest_tampering_and_plan_semantic_mutations_are_detected() {
    let artifact = nz_artifact();
    let pristine: serde_json::Value =
        serde_json::from_str(&artifact.to_json_pretty().unwrap()).unwrap();
    let mut value = pristine.clone();
    value["plan_digest"] = serde_json::json!(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    let error = CompiledAggregationArtifact::from_json_str(&serde_json::to_string(&value).unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        UnitDerivationError::InvalidAggregationArtifact(message)
            if message.contains("advertised plan digest")
    ));

    let source =
        include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml");
    let citation_edit = compile_aggregation_plan(
        &source.replace(
            "Income Tax Act 2007 s LC 13",
            "Income Tax Act 2007 s LC 13 (citation mutation)",
        ),
        &nz_source_artifact(),
    )
    .unwrap();
    assert_ne!(artifact.plan_digest, citation_edit.plan_digest);

    let source_edit = compile_aggregation_plan(
        source,
        &compile_nz_source_artifact(
            include_str!("../../tests/fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml")
                .replace("formula: '4041'", "formula: '9999'")
                .as_str(),
        ),
    )
    .unwrap();
    assert_ne!(
        artifact.source_artifact_digest,
        source_edit.source_artifact_digest
    );
    assert_ne!(artifact.plan_digest, source_edit.plan_digest);

    for (pointer, replacement) in [
        ("/format", serde_json::json!("wrong-format")),
        ("/semantics_version", serde_json::json!("wrong-semantics")),
        (
            "/constitution_digest",
            serde_json::json!(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            ),
        ),
        (
            "/phase_two_artifact/engine_version",
            serde_json::json!("tampered-engine"),
        ),
        (
            "/source_artifact_digest",
            serde_json::json!(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            ),
        ),
        (
            "/source_artifact/engine_version",
            serde_json::json!("tampered-engine"),
        ),
    ] {
        let mut mutant = pristine.clone();
        *mutant.pointer_mut(pointer).unwrap() = replacement;
        let mut registry = UnitDerivationDocumentRegistry::default();
        assert!(
            registry
                .register_aggregation_json(&serde_json::to_string(&mutant).unwrap())
                .is_err(),
            "registry accepted artifact mutation at `{pointer}`"
        );
    }

    let mut registry = UnitDerivationDocumentRegistry::default();
    registry
        .register_aggregation_artifact(artifact.clone())
        .unwrap();
    assert!(matches!(
        registry.register_aggregation_artifact(artifact),
        Err(UnitDerivationError::DuplicateNamespace { namespace, .. })
            if namespace == "compiled unit-derivation document"
    ));
}

#[test]
fn every_stage3_structural_guard_kills_its_isolated_plan_mutant() {
    let source =
        include_str!("../../tests/fixtures/unit_derivation/nz_income_explorer_family.yaml");
    let mutants = [
        (
            "all-members scalar reduction",
            source.replacen("selector: adults", "selector: all_members", 1),
        ),
        (
            "legacy child scalar alias",
            source.replacen(
                "child_gross_input: best_start_before_care_and_abatement",
                "child_value: best_start_before_care_and_abatement",
                1,
            ),
        ),
        (
            "family adjustment moved to child scope",
            source.replacen(
                "- name: best_start_family_abatement\n    scope: family",
                "- name: best_start_family_abatement\n    scope: child",
                1,
            ),
        ),
        (
            "family adjustment key mismatch",
            source.replacen(
                "- name: best_start_family_abatement\n    scope: family\n    kind: family_adjustment\n    reduction_key: best_start",
                "- name: best_start_family_abatement\n    scope: family\n    kind: family_adjustment\n    reduction_key: another_family_reduction",
                1,
            ),
        ),
        (
            "child gross loses typed provenance",
            source.replacen("kind: child_gross_amount", "kind: additive_amount", 1),
        ),
        (
            "wrong care operand",
            source.replacen(
                "care_fraction_input: best_start_claimant_care_fraction",
                "care_fraction_input: in_work_tax_credit_child_exclusive_care_fraction",
                1,
            ),
        ),
        (
            "child relation made symmetric",
            source.replacen(
                "- relation: dependent_child_of\n    direction: directed",
                "- relation: dependent_child_of\n    direction: symmetric",
                1,
            ),
        ),
        (
            "child tuple role swapped",
            source.replacen("right_role: child", "right_role: caregiver", 1),
        ),
        (
            "child projection reads family input",
            source.replacen(
                "- output: best_start_claimant_care_fraction\n    input: best_start_claimant_care_fraction",
                "- output: best_start_claimant_care_fraction\n    input: family_scheme_income",
                1,
            ),
        ),
        (
            "shape predicate names missing count",
            source.replacen(
                "count: dependent_child_count, minimum: 1",
                "count: missing_child_count, minimum: 1",
                1,
            ),
        ),
        (
            "required legal citation removed",
            source.replacen(
                "authority: nz/statute/act/public/2007/0097/section/MC-7",
                "authority: ''",
                1,
            ),
        ),
    ];
    for (name, mutant) in mutants {
        assert_ne!(mutant, source, "mutant `{name}` must change the fixture");
        assert!(
            compile_aggregation_plan(&mutant, &nz_source_artifact()).is_err(),
            "plan validator accepted `{name}` mutant"
        );
    }
}

#[test]
fn identity_preimage_is_order_invariant_and_matches_the_frozen_vector() {
    let first = unit_id(
        [0x5a; 32],
        "us:test#member_of_household",
        "dwelling:test",
        "2026-07-01/2026-07-31",
        &["person:b".to_string(), "person:a".to_string()],
    );
    let second = unit_id(
        [0x5a; 32],
        "us:test#member_of_household",
        "dwelling:test",
        "2026-07-01/2026-07-31",
        &["person:a".to_string(), "person:b".to_string()],
    );
    assert_eq!(first, second);
    assert_eq!(
        first,
        "sha256:bfd22d31b021e9ba47db44115094f66f5f70884490349b30fe396e747649beb4"
    );
}

#[test]
fn compiled_semantics_digest_is_independently_recomputed_and_churns_on_semantic_edits() {
    let minimal = base_plan(&["a"]);
    let compiled = compile(minimal.clone()).unwrap();
    let mut encoding = b"axiom.unit-derivation.semantics.stage2\0".to_vec();
    for value in [
        minimal.id.as_str(),
        minimal.entity_type.as_str(),
        minimal.roster_relation.as_str(),
        minimal.relations.unit_constituent.as_str(),
        minimal.relations.participating_member.as_str(),
    ] {
        independent_lp(&mut encoding, value.as_bytes());
    }
    // Empty derived-bool, edge, cut, attachment, bar, and status collections.
    for _ in 0..6 {
        encoding.extend_from_slice(&0_u32.to_be_bytes());
    }
    // No base-chain policy.
    encoding.push(0);
    assert_eq!(compiled.semantics_digest(), independent_sha256(&encoding));

    let mut formula_edit = minimal.clone();
    formula_edit.edges.push(edge(
        "semantic-edge",
        EdgeKind::Base,
        "a",
        "b",
        BoolExpr::Literal(true),
    ));
    let mut citation_edit = formula_edit.clone();
    citation_edit.edges[0].citation = citation("different-authority");
    assert_ne!(
        compile(formula_edit).unwrap().semantics_digest(),
        compile(citation_edit).unwrap().semantics_digest(),
        "citation edits must churn the internally computed semantics digest"
    );
}

fn trace_binding_plan() -> ConstitutionPlan {
    let mut plan = base_plan(&["a", "b"]);
    plan.edges.push(edge(
        "kin-edge",
        EdgeKind::Base,
        "a",
        "b",
        relation_expr("kin", "a", "b"),
    ));
    plan.statuses.push(StatusRule {
        id: "status-b".to_string(),
        person: "b".to_string(),
        when: BoolExpr::fact(FactRef::Bool("status_b".to_string())),
        citation: citation("trace:status-b"),
    });
    plan
}

fn trace_binding_input() -> ConstitutionInput {
    let mut input = complete_input(&["a", "b"]);
    let scope = input.roster.scope.clone();
    input.relation_families = vec![
        RelationFamilyInput {
            name: "kin".to_string(),
            scope: scope.clone(),
            completeness: Some(evidence("complete-kin")),
            facts: vec![
                RelationFact {
                    tuple: vec!["a".to_string(), "b".to_string()],
                    observation: observed(true, "kin-a-b"),
                },
                RelationFact {
                    tuple: vec!["b".to_string(), "a".to_string()],
                    observation: observed(false, "kin-b-a"),
                },
            ],
        },
        RelationFamilyInput {
            name: "unused_relation".to_string(),
            scope,
            completeness: Some(evidence("complete-unused-relation")),
            facts: vec![RelationFact {
                tuple: vec!["a".to_string(), "b".to_string()],
                observation: observed(true, "unused-relation-a-b"),
            }],
        },
    ];
    input.bool_facts = vec![
        BoolFactInput {
            name: "status_b".to_string(),
            observations: vec![observed(false, "status-b-false")],
            explicit_unknown: None,
        },
        BoolFactInput {
            name: "unused_bool".to_string(),
            observations: vec![
                observed(true, "unused-bool-true"),
                observed(false, "unused-bool-false"),
            ],
            explicit_unknown: None,
        },
    ];
    input.supplied_entities = vec![
        SuppliedEntity {
            entity_type: "external_case".to_string(),
            id: "external-1".to_string(),
            evidence: evidence("external-1-evidence"),
        },
        SuppliedEntity {
            entity_type: "external_case".to_string(),
            id: "external-2".to_string(),
            evidence: evidence("external-2-evidence"),
        },
    ];
    input.integrity_constraints = vec![
        IntegrityConstraint {
            id: "constraint-a".to_string(),
            expr: BoolExpr::Literal(true),
            citation: citation("trace:constraint-a"),
        },
        IntegrityConstraint {
            id: "constraint-b".to_string(),
            expr: BoolExpr::Literal(true),
            citation: citation("trace:constraint-b"),
        },
    ];
    input
}

fn trace_binding_root(plan: &ConstitutionPlan, input: &ConstitutionInput) -> String {
    derive_units(&compile(plan.clone()).unwrap(), input, &enabled())
        .unwrap()
        .trace
        .root
}

#[test]
fn trace_root_structurally_normalizes_input_order_and_exact_duplicates() {
    let plan = trace_binding_plan();
    let input = trace_binding_input();
    let expected = trace_binding_root(&plan, &input);

    let mut reordered = input.clone();
    reordered.roster.persons.reverse();
    reordered.relation_families.reverse();
    for family in &mut reordered.relation_families {
        family.facts.reverse();
    }
    reordered.bool_facts.reverse();
    for fact in &mut reordered.bool_facts {
        fact.observations.reverse();
    }
    reordered.supplied_entities.reverse();
    reordered.integrity_constraints.reverse();
    assert_eq!(
        trace_binding_root(&plan, &reordered),
        expected,
        "permuting set-like request records must not churn the trace root"
    );

    let mut duplicated = input.clone();
    let relation_fact = duplicated.relation_families[0].facts[0].clone();
    duplicated.relation_families[0].facts.push(relation_fact);
    let observation = duplicated.bool_facts[0].observations[0].clone();
    duplicated.bool_facts[0].observations.push(observation);
    let supplied = duplicated.supplied_entities[0].clone();
    duplicated.supplied_entities.push(supplied);
    let constraint = duplicated.integrity_constraints[0].clone();
    duplicated.integrity_constraints.push(constraint);
    assert_eq!(
        trace_binding_root(&plan, &duplicated),
        expected,
        "exact duplicate records are one normalized observation"
    );

    assert_eq!(
        expected,
        "sha256:76b0ad50498beddd41b510b4315ab17e2dcc1e24e4623ecf445f8c8e27e9eab1"
    );
}

#[test]
fn trace_root_binds_compiled_semantics_and_every_constitution_input_surface() {
    let plan = trace_binding_plan();
    let input = trace_binding_input();
    let baseline = trace_binding_root(&plan, &input);

    let mut semantic_edit = plan.clone();
    semantic_edit.derived_bools.push(DerivedBool {
        id: "unused-but-semantic".to_string(),
        stratum: Stratum::Person,
        expr: BoolExpr::Literal(true),
    });
    assert_ne!(
        trace_binding_root(&semantic_edit, &input),
        baseline,
        "compiled semantics digest is part of the trace commitment"
    );

    let mut mutations = Vec::<(&str, ConstitutionInput)>::new();

    let mut changed = input.clone();
    changed.roster.persons.push("c".to_string());
    mutations.push(("roster members", changed));

    let mut changed = input.clone();
    changed.roster.completeness.as_mut().unwrap().id = "other-roster-evidence".to_string();
    mutations.push(("roster completeness evidence", changed));

    let mut changed = input.clone();
    changed.segment = "2026-08-01/2026-08-31".to_string();
    mutations.push(("segment", changed));

    let mut changed = input.clone();
    changed.relation_families[0].facts[0].observation.value = false;
    mutations.push(("relation observation value", changed));

    let mut changed = input.clone();
    changed.relation_families[0].facts[0]
        .observation
        .evidence
        .id = "other-relation-evidence".to_string();
    mutations.push(("relation observation evidence", changed));

    let mut changed = input.clone();
    changed.relation_families[0]
        .completeness
        .as_mut()
        .unwrap()
        .citation
        .authority = "other-relation-completeness-authority".to_string();
    mutations.push(("relation completeness evidence", changed));

    let mut changed = input.clone();
    changed.relation_families[0].scope = "other-scope".to_string();
    mutations.push(("relation scope", changed));

    let mut changed = input.clone();
    changed.bool_facts[0].observations[0].value = true;
    mutations.push(("boolean observation value", changed));

    let mut changed = input.clone();
    changed.bool_facts[0].observations[0]
        .evidence
        .citation
        .provision = "other-boolean-evidence".to_string();
    mutations.push(("boolean observation evidence", changed));

    let mut changed = input.clone();
    changed.bool_facts[0].observations.clear();
    changed.bool_facts[0].explicit_unknown = Some(evidence("explicit-status-unknown"));
    mutations.push(("explicit Unknown evidence", changed));

    let mut changed = input.clone();
    changed.supplied_entities[0].evidence.id = "other-supplied-entity-evidence".to_string();
    mutations.push(("supplied entity evidence", changed));

    let mut changed = input.clone();
    changed.integrity_constraints[0].citation.authority = "other-integrity-authority".to_string();
    mutations.push(("integrity constraint", changed));

    for (surface, changed) in mutations {
        assert_ne!(
            trace_binding_root(&plan, &changed),
            baseline,
            "changing {surface} must churn the trace root"
        );
    }
}

fn independent_lp(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}

// Test-only SHA-256 recomputation kept independent of the prototype's hash
// routine so the digest assertion does not merely call production twice.
fn independent_sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut padded = input.to_vec();
    let bit_len = (padded.len() as u64) * 8;
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = [
        0x6a09e667_u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes(chunk[offset..offset + 4].try_into().unwrap());
        }
        for index in 16..64 {
            let a = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let b = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(a)
                .wrapping_add(words[index - 7])
                .wrapping_add(b);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let choose = (e & f) ^ ((!e) & g);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let first = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let second = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h].into_iter()) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut output = [0_u8; 32];
    for (index, word) in state.into_iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}

#[test]
fn strict_segmentation_precedes_selection() {
    let compiled = compile(base_plan(&["a"])).unwrap();
    let mut input = complete_input(&["a"]);
    input.segment_complete = false;
    assert_eq!(
        derive_units(&compiled, &input, &enabled()).unwrap_err(),
        UnitDerivationError::RequiresSegmentation
    );
}

#[derive(Debug, Deserialize)]
struct SnapFixtureFile {
    cases: Vec<SnapFixture>,
}

#[derive(Debug, Deserialize)]
struct SnapFixture {
    id: String,
    roster: Vec<String>,
    base_groups: Vec<Vec<String>>,
    combination_edges: Vec<[String; 2]>,
    status_excluded: Vec<String>,
    expected_constituent: Vec<Vec<String>>,
    expected_participating: Vec<Vec<String>>,
}

#[test]
fn synthetic_snap_pilot_shape_fixtures_match_7_cfr_projection_shapes() {
    let fixture: SnapFixtureFile = serde_yaml::from_str(include_str!(
        "../../tests/fixtures/unit_derivation/snap_household_shapes.yaml"
    ))
    .unwrap();
    for case in fixture.cases {
        let roster_refs = case.roster.iter().map(String::as_str).collect::<Vec<_>>();
        let mut plan = base_plan(&roster_refs);
        let mut pap_facts = Vec::new();
        for (group_index, group) in case.base_groups.iter().enumerate() {
            for left_index in 0..group.len() {
                for right_index in (left_index + 1)..group.len() {
                    let left = &group[left_index];
                    let right = &group[right_index];
                    plan.edges.push(edge(
                        &format!("pap-{group_index}-{left}-{right}"),
                        EdgeKind::Base,
                        left,
                        right,
                        relation_expr("purchase_and_prepare", left, right),
                    ));
                    pap_facts.push(RelationFact {
                        tuple: vec![left.clone(), right.clone()],
                        observation: observed(true, &format!("pap:{left}:{right}")),
                    });
                }
            }
        }
        let mut combination_facts = Vec::new();
        for [left, right] in &case.combination_edges {
            plan.edges.push(edge(
                &format!("mandatory-{left}-{right}"),
                EdgeKind::Combination,
                left,
                right,
                relation_expr("mandatory_combination", left, right),
            ));
            combination_facts.push(RelationFact {
                tuple: vec![left.clone(), right.clone()],
                observation: observed(true, &format!("combination:{left}:{right}")),
            });
        }
        for person in &case.status_excluded {
            plan.statuses.push(StatusRule {
                id: format!("excluded-{person}"),
                person: person.clone(),
                when: BoolExpr::Literal(true),
                citation: citation("7-cfr-273.1(b)(7)/273.11(c)"),
            });
        }
        let mut input = complete_input(&roster_refs);
        input.relation_families = vec![
            RelationFamilyInput {
                name: "purchase_and_prepare".to_string(),
                scope: input.roster.scope.clone(),
                completeness: Some(evidence(&format!("{}:complete-pap", case.id))),
                facts: pap_facts,
            },
            RelationFamilyInput {
                name: "mandatory_combination".to_string(),
                scope: input.roster.scope.clone(),
                completeness: Some(evidence(&format!("{}:complete-combination", case.id))),
                facts: combination_facts,
            },
        ];
        let result = derive_units(&compile(plan).unwrap(), &input, &enabled()).unwrap();
        let mut expected_constituent = case.expected_constituent;
        let mut expected_participating = case.expected_participating;
        expected_constituent
            .iter_mut()
            .for_each(|block| block.sort());
        expected_participating
            .iter_mut()
            .for_each(|block| block.sort());
        expected_constituent.sort();
        expected_participating.sort();
        assert_eq!(
            partition(&result, Projection::UnitConstituent),
            expected_constituent,
            "fixture {} constituent projection",
            case.id
        );
        assert_eq!(
            partition(&result, Projection::ParticipatingMember),
            expected_participating,
            "fixture {} participation projection",
            case.id
        );
    }
}
