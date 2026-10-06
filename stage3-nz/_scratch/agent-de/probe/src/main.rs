use axiom_rules_engine::model::{
    DType, DataSet, Derived, DerivedSemantics, Interval, JudgmentExpr, Period, Program,
    RelatedValueRef, RelationDerivation, RelationSchema, ScalarExpr, ScalarValue,
};
use axiom_rules_engine::unit_derivation::{
    BoolExpr, Citation, CompleteReduction, ConstitutionInput, ConstitutionPlan, EdgeKind,
    EdgeRule, EmissionRelations, Evidence, Projection, RosterInput, StatusRule,
    UnitDerivationConfig, compile, derive_units, materialize_phase_two_dataset, PrototypeRun,
};

fn citation(provision: &str) -> Citation {
    Citation::new(provision, "agent-de adversarial fixture")
}

fn evidence(id: &str) -> Evidence {
    Evidence {
        id: id.to_string(),
        citation: citation("fixture:evidence"),
    }
}

fn derived(name: &str, dtype: DType, semantics: DerivedSemantics) -> Derived {
    Derived {
        id: None,
        name: name.to_string(),
        entity: "Family".to_string(),
        dtype,
        unit: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: Vec::new(),
    }
}

fn main() {
    let participating = "nz:test#participating_member";
    let constituent = "nz:test#unit_constituent";
    let filtered = "nz:test#known_participating_member";
    let plan = ConstitutionPlan {
        id: "nz:test:unknown-participation".to_string(),
        entity_type: "Family".to_string(),
        roster_relation: "nz:test#roster".to_string(),
        relations: EmissionRelations {
            unit_constituent: constituent.to_string(),
            participating_member: participating.to_string(),
        },
        derived_bools: Vec::new(),
        edges: vec![EdgeRule {
            id: "known-family-edge".to_string(),
            kind: EdgeKind::Base,
            left: "a".to_string(),
            right: "b".to_string(),
            when: BoolExpr::Literal(true),
            citation: citation("known family composition"),
            defeaters: Vec::new(),
        }],
        cuts: Vec::new(),
        attachments: Vec::new(),
        bars: Vec::new(),
        statuses: vec![StatusRule {
            id: "unknown-status-b".to_string(),
            person: "b".to_string(),
            when: BoolExpr::fact(axiom_rules_engine::unit_derivation::FactRef::Bool(
                "status_b".to_string(),
            )),
            citation: citation("unknown participating status"),
        }],
        base_chain_policy: None,
    };
    let input = ConstitutionInput {
        roster: RosterInput {
            relation: "nz:test#roster".to_string(),
            scope: "family:one".to_string(),
            persons: vec!["a".to_string(), "b".to_string()],
            completeness: Some(evidence("complete-roster")),
        },
        segment: "2026-07-01/2026-07-31".to_string(),
        segment_complete: true,
        relation_families: Vec::new(),
        bool_facts: Vec::new(),
        supplied_entities: Vec::new(),
        integrity_constraints: Vec::new(),
    };
    let config = UnitDerivationConfig {
        enabled: true,
        ..Default::default()
    };
    let derivation = derive_units(&compile(plan).unwrap(), &input, &config).unwrap();
    println!("indeterminate={:?}", derivation.indeterminate);
    let unit = derivation.units[0].id.clone();
    let run = PrototypeRun {
        derivation,
        comparisons: Vec::new(),
    };

    let mut phase_two = Program::default();
    for relation in [constituent, participating] {
        phase_two
            .add_relation_schema(RelationSchema {
                name: relation.to_string(),
                arity: 2,
                slot_entities: vec!["Family".to_string(), "person".to_string()],
                derivation: None,
            })
            .unwrap();
    }
    phase_two
        .add_relation_schema(RelationSchema {
            name: filtered.to_string(),
            arity: 2,
            slot_entities: vec!["Family".to_string(), "person".to_string()],
            derivation: Some(RelationDerivation {
                source_relation: constituent.to_string(),
                current_slot: 0,
                related_slot: 1,
                entity: Some("person".to_string()),
                member_relation: Some(participating.to_string()),
                slot_entities: vec!["Family".to_string(), "person".to_string()],
                predicate: JudgmentExpr::RelationMember {
                    relation: participating.to_string(),
                    current_slot: 0,
                    related_slot: 1,
                },
            }),
        })
        .unwrap();
    let direct_count = ScalarExpr::CountRelated {
        relation: participating.to_string(),
        current_slot: 0,
        related_slot: 1,
        where_clause: None,
    };
    let direct_sum = ScalarExpr::SumRelated {
        relation: participating.to_string(),
        current_slot: 0,
        related_slot: 1,
        value: RelatedValueRef::Input("amount".to_string()),
        where_clause: None,
    };
    let gated_count = ScalarExpr::CountRelated {
        relation: filtered.to_string(),
        current_slot: 0,
        related_slot: 1,
        where_clause: None,
    };
    let gated_sum = ScalarExpr::SumRelated {
        relation: filtered.to_string(),
        current_slot: 0,
        related_slot: 1,
        value: RelatedValueRef::Input("amount".to_string()),
        where_clause: None,
    };
    for item in [
        derived(
            "direct_count",
            DType::Integer,
            DerivedSemantics::Scalar(direct_count),
        ),
        derived(
            "direct_sum",
            DType::Decimal,
            DerivedSemantics::Scalar(direct_sum),
        ),
        derived(
            "gated_count",
            DType::Integer,
            DerivedSemantics::Scalar(gated_count),
        ),
        derived(
            "gated_sum",
            DType::Decimal,
            DerivedSemantics::Scalar(gated_sum),
        ),
    ] {
        phase_two.add_derived(item).unwrap();
    }

    let period = Period::month(2026, 7);
    let interval = Interval::covering(&period);
    let mut base = DataSet::default();
    base.add_input(
        "amount",
        "person",
        "a",
        interval.clone(),
        ScalarValue::Integer(10),
    );
    base.add_input(
        "amount",
        "person",
        "b",
        interval.clone(),
        ScalarValue::Integer(20),
    );
    let materialized = materialize_phase_two_dataset(
        &base,
        &phase_two,
        &input,
        &run,
        interval,
    )
    .unwrap();
    println!("knowledge={:?}", materialized.relation_knowledge());
    let mut engine = materialized.engine(&phase_two);
    for name in ["direct_count", "direct_sum", "gated_count", "gated_sum"] {
        let result: CompleteReduction<ScalarValue> =
            engine.evaluate_scalar(name, &unit, &period).unwrap();
        println!("{name}={result:?}");
    }

    assert!(matches!(
        engine.evaluate_scalar("direct_count", &unit, &period).unwrap(),
        CompleteReduction::Indeterminate { .. }
    ));
    assert!(matches!(
        engine.evaluate_scalar("direct_sum", &unit, &period).unwrap(),
        CompleteReduction::Indeterminate { .. }
    ));
    assert_eq!(
        engine.evaluate_scalar("gated_count", &unit, &period).unwrap(),
        CompleteReduction::Determined(ScalarValue::Integer(1))
    );
    assert!(matches!(
        engine.evaluate_scalar("gated_sum", &unit, &period).unwrap(),
        CompleteReduction::Determined(ScalarValue::Decimal(value)) if value.to_string() == "10"
    ));

    let _ = Projection::ParticipatingMember;
}
