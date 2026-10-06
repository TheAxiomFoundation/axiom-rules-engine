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
