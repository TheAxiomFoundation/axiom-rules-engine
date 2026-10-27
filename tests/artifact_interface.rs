//! `derive_interface` on small programs, with every expected value written
//! out by hand from the rules in `docs/artifact-format.md`: the dtype demand
//! table, the evaluation-flow `entities`, defaults and closed domains, the
//! entity roster and the relations block.

use axiom_rules_engine::interface::{
    DTypeBasis, EntityInstances, InputDType, InterfaceEntity, InterfaceInput, InterfaceRelation,
    ProgramInterface, RelationProvenance, RelationReads, ValuesBasis, derive_interface,
};
use axiom_rules_engine::spec::{ProgramSpec, ScalarValueSpec};
use serde_json::{Value, json};

fn program(value: Value) -> ProgramSpec {
    serde_json::from_value(value).expect("test program is a valid ProgramSpec")
}

fn derive(value: Value) -> ProgramInterface {
    derive_interface(&program(value)).expect("test program derives an interface")
}

fn input(name: &str) -> Value {
    json!({"kind": "input", "name": name})
}

fn input_or_else(name: &str, default: Value) -> Value {
    json!({"kind": "input_or_else", "name": name, "default": default})
}

fn derived_ref(name: &str) -> Value {
    json!({"kind": "derived", "name": name})
}

fn role_input(term: &str, name: &str) -> Value {
    json!({"kind": "role_input", "term": term, "name": name})
}

fn role_derived(term: &str, name: &str) -> Value {
    json!({"kind": "role_derived", "term": term, "name": name})
}

fn int(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn dec(value: &str) -> Value {
    json!({"kind": "literal", "value": {"kind": "decimal", "value": value}})
}

fn boolean(value: bool) -> Value {
    json!({"kind": "literal", "value": {"kind": "bool", "value": value}})
}

fn text(value: &str) -> Value {
    json!({"kind": "literal", "value": {"kind": "text", "value": value}})
}

fn date(value: &str) -> Value {
    json!({"kind": "literal", "value": {"kind": "date", "value": value}})
}

fn cmp(left: Value, op: &str, right: Value) -> Value {
    json!({"kind": "comparison", "left": left, "op": op, "right": right})
}

fn add(items: Vec<Value>) -> Value {
    json!({"kind": "add", "items": items})
}

fn scalar_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "decimal", "unit": null,
        "semantics": "scalar", "expr": expr
    })
}

fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "judgment", "unit": null,
        "semantics": "judgment", "expr": expr
    })
}

fn find<'a>(interface: &'a ProgramInterface, slot: &str) -> &'a InterfaceInput {
    interface
        .inputs
        .iter()
        .find(|input| input.name == slot)
        .unwrap_or_else(|| panic!("no input `{slot}` in {interface:#?}"))
}

fn kinds(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// Every row of the demand table, and the combination rules: a slot's dtype
/// is set only when every read site demands a type, `decimal` when the
/// demands mix `integer` and `number`, and null for any site that demands
/// nothing.
#[test]
fn dtype_follows_the_demand_table() {
    let interface = derive(json!({
        "parameters": [{
            "name": "table", "unit": null, "indexed_by": "household_size",
            "versions": [
                {"effective_from": "2020-01-01", "values": {
                    "1": {"kind": "integer", "value": 10},
                    "3": {"kind": "integer", "value": 30}
                }},
                {"effective_from": "2024-01-01", "values": {
                    "2": {"kind": "integer", "value": 20}
                }}
            ]
        }],
        "derived": [
            judgment_rule("bool_eq", "Household", cmp(input("flag"), "eq", boolean(true))),
            judgment_rule("text_ne", "Household", cmp(text("a"), "ne", input("label"))),
            scalar_rule("date_shift", "Household", json!({
                "kind": "date_add_days", "date": input("start_date"), "days": input("offset")
            })),
            scalar_rule("date_span", "Household", json!({
                "kind": "days_between", "from": input("from_date"), "to": {"kind": "period_end"}
            })),
            judgment_rule("date_cmp", "Household", cmp(input("due"), "lt", {
                json!({"kind": "period_start"})
            })),
            judgment_rule("date_literal_cmp", "Household",
                cmp(input("deadline"), "gte", date("2026-01-01"))),
            scalar_rule("lookup", "Household", json!({
                "kind": "parameter_lookup", "parameter": "table", "index": input("size")
            })),
            scalar_rule("sum", "Household", add(vec![input("amount"), int(1)])),
            judgment_rule("over", "Household", cmp(input("threshold"), "gt", dec("2.5"))),
            judgment_rule("against_arithmetic", "Household",
                cmp(input("limit"), "lte", add(vec![int(1), int(2)]))),
            // `mixed` is an index in one place and an addend in another.
            scalar_rule("mixed_index", "Household", json!({
                "kind": "parameter_lookup", "parameter": "table", "index": input("mixed")
            })),
            scalar_rule("mixed_number", "Household",
                json!({"kind": "floor", "value": input("mixed")})),
            // No evidence: the rule's value, an `if` branch, a comparison
            // with another input, an ordered comparison with a boolean.
            scalar_rule("bare", "Household", input("bare")),
            scalar_rule("branch", "Household", json!({
                "kind": "if", "condition": cmp(int(1), "eq", int(1)),
                "then_expr": input("branch_value"), "else_expr": int(0)
            })),
            judgment_rule("pair", "Household", cmp(input("left"), "eq", input("right"))),
            judgment_rule("ordered_bool", "Household", cmp(input("ordered"), "lt", boolean(true))),
            // One site with evidence and one without: no dtype.
            scalar_rule("partial_number", "Household", add(vec![input("partial"), int(1)])),
            scalar_rule("partial_bare", "Household", input("partial")),
            // bool evidence and text evidence disagree.
            judgment_rule("as_bool", "Household", cmp(input("either"), "eq", boolean(false))),
            judgment_rule("as_text", "Household", cmp(input("either"), "eq", text("x")))
        ]
    }));

    let expect = |slot: &str, dtype: Option<InputDType>| {
        let input = find(&interface, slot);
        assert_eq!(input.dtype, dtype, "dtype of `{slot}`");
        assert_eq!(
            input.dtype_basis,
            dtype.map(|_| DTypeBasis::Usage),
            "dtype_basis of `{slot}`"
        );
    };
    expect("flag", Some(InputDType::Bool));
    expect("label", Some(InputDType::Text));
    expect("start_date", Some(InputDType::Date));
    expect("offset", Some(InputDType::Integer));
    expect("from_date", Some(InputDType::Date));
    expect("due", Some(InputDType::Date));
    expect("deadline", Some(InputDType::Date));
    expect("size", Some(InputDType::Integer));
    expect("amount", Some(InputDType::Decimal));
    expect("threshold", Some(InputDType::Decimal));
    expect("limit", Some(InputDType::Decimal));
    expect("mixed", Some(InputDType::Decimal));
    for slot in [
        "bare",
        "branch_value",
        "left",
        "right",
        "ordered",
        "partial",
        "either",
    ] {
        expect(slot, None);
    }

    // Only `size` is read at an index and nowhere else; `mixed` is also an
    // addend, so its domain is open. The domain is the union of the keys of
    // every version, sorted.
    let size = find(&interface, "size");
    assert_eq!(size.values, Some(vec![1, 2, 3]));
    assert_eq!(size.values_basis, Some(ValuesBasis::ParameterIndex));
    let mixed = find(&interface, "mixed");
    assert_eq!(mixed.values, None);
    assert_eq!(mixed.values_basis, None);
    // `household_size` is an `indexed_by` label no expression reads: it is
    // in the input catalog but not an interface input.
    assert!(
        interface
            .inputs
            .iter()
            .all(|input| input.name != "household_size")
    );
}

/// A default is reported only when every read site is an `input_or_else`
/// with the same literal.
#[test]
fn default_requires_every_site_to_agree() {
    let zero = json!({"kind": "integer", "value": 0});
    let interface = derive(json!({
        "derived": [
            scalar_rule("a", "Household", add(vec![input_or_else("shared", zero.clone()), int(1)])),
            scalar_rule("b", "Household", json!({"kind": "ceil", "value": input_or_else("shared", zero.clone())})),
            scalar_rule("c", "Household", add(vec![input_or_else("split", zero.clone()), int(1)])),
            scalar_rule("d", "Household", add(vec![input_or_else("split", json!({"kind": "integer", "value": 1})), int(1)])),
            scalar_rule("e", "Household", add(vec![input_or_else("half", zero.clone()), int(1)])),
            scalar_rule("f", "Household", add(vec![input("half"), int(1)]))
        ]
    }));
    let shared = find(&interface, "shared");
    assert_eq!(shared.default, Some(ScalarValueSpec::Integer { value: 0 }));
    assert_eq!(shared.dtype, Some(InputDType::Decimal));
    assert_eq!(find(&interface, "split").default, None);
    assert_eq!(find(&interface, "half").default, None);
}

/// `entities` follows evaluation flow. A rule referenced from a rule of
/// another kind is evaluated on that kind's ids too, so its inputs list both;
/// an entity-free rule always contributes `Scalar`; an aggregation's summed
/// value and `where` clause run on the related slot's kind.
#[test]
fn entities_follow_evaluation_flow() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]}
        ],
        "derived": [
            scalar_rule("person_amount", "Person", add(vec![input("person_input"), int(1)])),
            scalar_rule("tax_unit_total", "TaxUnit", add(vec![derived_ref("person_amount"), int(1)])),
            scalar_rule("rate", "Scalar", add(vec![input("rate_input"), int(1)])),
            scalar_rule("household_scaled", "Household", add(vec![derived_ref("rate"), int(1)])),
            judgment_rule("member_counts", "Person", cmp(input("member_flag"), "eq", boolean(true))),
            scalar_rule("member_income", "Person", add(vec![input("wage"), int(0)])),
            scalar_rule("household_income", "Household", json!({
                "kind": "sum_related", "relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "value": {"kind": "input", "name": "earnings"},
                "where": {"kind": "derived", "name": "member_counts"}
            })),
            scalar_rule("household_wages", "Household", json!({
                "kind": "sum_related", "relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "value": {"kind": "derived", "name": "member_income"}
            })),
            scalar_rule("household_only", "Household", add(vec![input("household_input"), int(1)]))
        ]
    }));
    assert_eq!(
        find(&interface, "person_input").entities,
        kinds(&["Person", "TaxUnit"])
    );
    assert_eq!(
        find(&interface, "rate_input").entities,
        kinds(&["Household", "Scalar"])
    );
    assert_eq!(find(&interface, "earnings").entities, kinds(&["Person"]));
    assert_eq!(find(&interface, "member_flag").entities, kinds(&["Person"]));
    assert_eq!(find(&interface, "wage").entities, kinds(&["Person"]));
    assert_eq!(
        find(&interface, "household_input").entities,
        kinds(&["Household"])
    );
    // `Scalar` is in the roster because a rule declares it and an input
    // lists it.
    assert!(
        interface
            .entities
            .iter()
            .any(|entity| entity.kind == "Scalar"
                && entity.id_template.is_none()
                && entity.instances == EntityInstances::Unscoped)
    );
}

/// The roster, an alias and its derived relation, `lookup_slots` counting a
/// derived relation's traversal against its source, and the predicate's
/// routing: a rule whose entity is the declared current kind runs on the
/// current id, every other rule and every read site on the related id.
#[test]
fn roster_and_relations_describe_a_filtered_entity() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "snap_unit", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "entity": "SnapUnit",
                "slot_entities": ["Person", "Household"],
                "predicate": {"kind": "and", "items": [
                    {"kind": "derived", "name": "member_eligible"},
                    {"kind": "derived", "name": "household_opted_in"},
                    cmp(input("member_age"), "gte", int(18)),
                    {"kind": "relation_member", "relation": "member_of_household",
                     "current_slot": 1, "related_slot": 0}
                ]}
            }}
        ],
        "derived": [
            judgment_rule("member_eligible", "Person", cmp(input("eligible_flag"), "eq", boolean(true))),
            judgment_rule("household_opted_in", "Household", cmp(input("opt_in"), "eq", boolean(true))),
            scalar_rule("unit_size", "SnapUnit", json!({
                "kind": "count_related", "relation": "snap_unit",
                "current_slot": 1, "related_slot": 0
            })),
            scalar_rule("unit_allotment", "SnapUnit", add(vec![input("unit_input"), int(0)]))
        ]
    }));

    assert_eq!(
        interface.entities,
        vec![
            InterfaceEntity {
                kind: "Household".to_string(),
                id_template: Some("Household:{index}".to_string()),
                instances: EntityInstances::Supplied,
            },
            InterfaceEntity {
                kind: "Person".to_string(),
                id_template: Some("Person:{index}".to_string()),
                instances: EntityInstances::Supplied,
            },
            InterfaceEntity {
                kind: "SnapUnit".to_string(),
                id_template: Some("Household:{index}".to_string()),
                instances: EntityInstances::Alias {
                    of: Some("Household".to_string()),
                    via: vec!["snap_unit".to_string()],
                },
            },
        ]
    );
    // A SnapUnit rule runs on household ids.
    assert_eq!(
        find(&interface, "unit_input").entities,
        kinds(&["Household"])
    );
    // The predicate's read site runs on the related (person) id; its
    // Household rule runs on the current (household) id.
    assert_eq!(find(&interface, "member_age").entities, kinds(&["Person"]));
    assert_eq!(
        find(&interface, "eligible_flag").entities,
        kinds(&["Person"])
    );
    assert_eq!(find(&interface, "opt_in").entities, kinds(&["Household"]));

    assert_eq!(
        interface.relations,
        vec![
            InterfaceRelation {
                name: "member_of_household".to_string(),
                arity: 2,
                slot_entities: Some(kinds(&["Person", "Household"])),
                lookup_slots: vec![1],
                accepts_supplied_tuples: true,
                provenance: RelationProvenance::Supplied,
            },
            InterfaceRelation {
                name: "snap_unit".to_string(),
                arity: 2,
                slot_entities: Some(kinds(&["Person", "Household"])),
                lookup_slots: vec![1],
                accepts_supplied_tuples: true,
                provenance: RelationProvenance::Derived {
                    rule: "snap_unit".to_string(),
                    source_relation: "member_of_household".to_string(),
                    reads: RelationReads {
                        inputs: kinds(&["member_age"]),
                        rules: kinds(&["household_opted_in", "member_eligible"]),
                        relations: kinds(&["member_of_household"]),
                    },
                },
            },
        ]
    );
}

/// Every interface object refuses an unknown key and a missing one, a
/// nullable key included, when deserialized: the stage-3 artifact reads its
/// embedded artifacts through serde before the loader sees them. The program
/// declares one election so the `election` object is covered too.
#[test]
fn interface_objects_have_exactly_their_keys() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "unit", "arity": 2, "derivation": {
                "source_relation": "member_of_household", "current_slot": 1, "related_slot": 0,
                "entity": "Unit", "predicate": cmp(input("age"), "gt", int(1))
            }}
        ],
        "derived": [scalar_rule("total", "Household", add(vec![input("x"), int(1)]))],
        "input_declarations": [{"input": "x", "election": {
            "actor": "Household", "provision": "us/regulation/7/273/1/b/3"
        }}]
    }));
    assert!(
        interface
            .inputs
            .iter()
            .any(|input| input.election.is_some())
    );
    let value = serde_json::to_value(&interface).unwrap();
    let round_trip: ProgramInterface = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(round_trip, interface);

    let mut objects = Vec::new();
    collect_object_pointers(&value, String::new(), &mut objects);
    assert!(objects.len() > 8, "{objects:?}");
    for pointer in objects {
        let keys = value
            .pointer(&pointer)
            .and_then(Value::as_object)
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut extra = value.clone();
        extra
            .pointer_mut(&pointer)
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("unexpected".to_string(), json!(null));
        assert!(
            serde_json::from_value::<ProgramInterface>(extra).is_err(),
            "an unknown key at `{pointer}` must be refused"
        );
        for key in keys {
            let mut missing = value.clone();
            missing
                .pointer_mut(&pointer)
                .and_then(Value::as_object_mut)
                .unwrap()
                .remove(&key);
            assert!(
                serde_json::from_value::<ProgramInterface>(missing).is_err(),
                "removing `{key}` at `{pointer}` must be refused"
            );
        }
    }
}

fn collect_object_pointers(value: &Value, pointer: String, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            out.push(pointer.clone());
            for (key, nested) in map {
                collect_object_pointers(nested, format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                collect_object_pointers(nested, format!("{pointer}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// Invariant 3: every input inside an over-periods reduction has no dtype
/// demand, including an input nested in arithmetic. Explain refuses the
/// reduction before reading its operand. A uniform default is still reported.
#[test]
fn over_periods_operands_do_not_infer_input_dtypes() {
    let program = program(json!({
        "derived": [scalar_rule("lifetime_total", "Household", json!({
            "kind": "over_periods", "over": "sum",
            "value": add(vec![input_or_else("amount", json!({"kind":"integer","value":0}))])
        }))]
    }));
    let artifact = axiom_rules_engine::compile::CompiledProgramArtifact::compile(program).unwrap();
    let model = artifact.program.to_program().unwrap();
    let data = axiom_rules_engine::model::DataSet {
        inputs: vec![],
        relations: vec![],
    };
    let period: axiom_rules_engine::spec::PeriodSpec = serde_json::from_value(json!({
        "period_kind":"month", "start":"2026-01-01", "end":"2026-01-31"
    }))
    .unwrap();
    assert!(matches!(
        axiom_rules_engine::engine::Engine::new(&model, &data).evaluate_scalar(
            "lifetime_total",
            "Household:1",
            &period.to_model().unwrap()
        ),
        Err(axiom_rules_engine::engine::EvalError::OverPeriodsOutsideLifetime(_))
    ));
    let input = find(artifact.interface.as_ref().unwrap(), "amount");
    assert_eq!(input.dtype, None);
    assert_eq!(input.dtype_basis, None);
    assert_eq!(input.default, Some(ScalarValueSpec::Integer { value: 0 }));
}

/// Invariant 3: suppressing a reduction's nested dtype demands preserves
/// structural parameter-index domains, and ends at the reduction boundary.
#[test]
fn over_periods_preserves_parameter_domains_and_outer_demands() {
    let interface = derive(json!({
        "parameters": [{
            "name":"table", "indexed_by":"catalog_only", "unit":null,
            "versions":[{"effective_from":"2020-01-01","values":{
                "1":{"kind":"integer","value":10},
                "3":{"kind":"integer","value":30}
            }}]
        }],
        "derived":[scalar_rule("nested", "Household", add(vec![
            json!({"kind":"over_periods","over":"sum_top_n",
                "value":{"kind":"parameter_lookup","parameter":"table","index":input("index")},
                "n":add(vec![input("top_n")])}),
            input("outside")
        ]))]
    }));
    let index = find(&interface, "index");
    assert_eq!(index.dtype, None);
    assert_eq!(index.values, Some(vec![1, 3]));
    assert_eq!(index.values_basis, Some(ValuesBasis::ParameterIndex));
    assert_eq!(find(&interface, "top_n").dtype, None);
    assert_eq!(find(&interface, "outside").dtype, Some(InputDType::Decimal));
}

/// Invariant 3: a canonical selected-year input keeps its bare runtime slot
/// in the interface and its owning module's catalog request name as legal_id.
/// The contract id inside `input_for_year` does not create a second slot.
#[test]
fn canonical_selected_year_input_uses_its_runtime_slot() {
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    use axiom_rules_engine::source::{ModuleSource, SourceError};

    const TARGET: &str = "ca:policies/tests/annual";
    const SOURCE: &str = include_str!("fixtures/selected_year_input.yaml");
    struct AnnualSource;
    impl ModuleSource for AnnualSource {
        fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
            Ok((target == TARGET).then(|| SOURCE.to_string()))
        }
    }

    let artifact = CompiledProgramArtifact::from_rulespec_with_source(TARGET, &AnnualSource)
        .expect("canonical selected-year fixture compiles");
    let legal_id = format!("{TARGET}#input.income");
    assert_eq!(artifact.program.selected_year_inputs[0].id, legal_id);
    let catalog = &artifact.metadata.input_catalog;
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].slot, "income");
    assert_eq!(catalog[0].canonical_request_name, legal_id);

    let interface = artifact.interface.as_ref().unwrap();
    assert_eq!(
        interface
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect::<Vec<_>>(),
        ["income"]
    );
    let input = find(interface, "income");
    assert_eq!(input.legal_id, legal_id);
    assert_eq!(input.entities, kinds(&["Person"]));
}

/// Invariant 6: `min`/`max` over related entities compare every selected
/// member's value but return only the winner, so a fractional losing value
/// is read successfully where the aggregate is a table index or a day
/// offset. The members' slot is therefore `decimal`, not `integer`. `only`
/// returns its one member's value, which keeps the index's `integer`.
#[test]
fn an_extremum_types_its_losing_members_as_numbers() {
    use axiom_rules_engine::api::{ApiError, ExecutionRequest, execute_request};
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    use axiom_rules_engine::engine::EvalError;

    let related = |kind: &str, relation: &str, current: usize, slot: &str| {
        let mut node = json!({
            "kind": kind, "relation": relation, "current_slot": current,
            "related_slot": 1 - current, "value": input(slot)
        });
        if kind == "extreme_related" {
            node["extreme"] = json!(if slot == "rank" { "min" } else { "max" });
        }
        node
    };
    let lookup =
        |index: Value| json!({"kind":"parameter_lookup","parameter":"table","index":index});
    let program = json!({
        "relations": [
            {"name":"members","arity":2,"slot_entities":["Person","Household"]},
            {"name":"links","arity":2,"slot_entities":["Household","Household"]}
        ],
        "parameters": [{"name":"table","unit":null,"versions":[{"effective_from":"2020-01-01",
            "values":{"1":{"kind":"integer","value":10}}}]}],
        "derived": [
            scalar_rule("least", "Household", lookup(related("extreme_related", "members", 1, "rank"))),
            scalar_rule("latest", "Household", json!({"kind":"date_add_days",
                "date":{"kind":"period_start"},
                "days":related("extreme_related", "members", 1, "days")})),
            scalar_rule("linked", "Household", lookup(related("single_related", "links", 0, "link_rank")))
        ]
    });
    let artifact = CompiledProgramArtifact::compile(self::program(program.clone())).unwrap();
    let interface = artifact.interface.as_ref().unwrap();
    for (slot, dtype) in [
        ("rank", InputDType::Decimal),
        ("days", InputDType::Decimal),
        ("link_rank", InputDType::Integer),
    ] {
        let input = find(interface, slot);
        assert_eq!(input.dtype, Some(dtype), "{slot}");
        assert_eq!(input.values, None, "{slot}: no domain through an aggregate");
    }

    // Person:2's value loses each extremum and is not a whole number.
    let cell = |slot: &str, id: &str, value: Value| {
        let kind = id.split(':').next().unwrap();
        json!({"name":slot,"entity":kind,"entity_id":id,
            "interval":{"start":"2020-01-01","end":"2030-12-31"},"value":value})
    };
    let decimal = |value: &str| json!({"kind":"decimal","value":value});
    let cells = vec![
        cell("rank", "Person:1", decimal("1.0")),
        cell("rank", "Person:2", decimal("1.5")),
        cell("days", "Person:1", decimal("1")),
        cell("days", "Person:2", decimal("0.5")),
        cell(
            "link_rank",
            "Household:2",
            json!({"kind":"integer","value":1}),
        ),
    ];
    let tuple = |relation: &str, from: &str, to: &str| json!({"name":relation,"tuple":[from,to],"interval":{"start":"2020-01-01","end":"2030-12-31"}});
    let run = |mode: &str, inputs: &[Value]| {
        let request: ExecutionRequest = serde_json::from_value(json!({
            "mode": mode, "program": program,
            "dataset": {"inputs": inputs, "relations": [
                tuple("members", "Person:1", "Household:1"),
                tuple("members", "Person:2", "Household:1"),
                tuple("links", "Household:1", "Household:2")
            ]},
            "queries": [{"entity_id":"Household:1",
                "period":{"period_kind":"month","start":"2026-01-01","end":"2026-01-31"},
                "outputs":["least","latest","linked"]}]
        }))
        .unwrap();
        execute_request(request)
    };
    for mode in ["explain", "fast"] {
        let response = serde_json::to_value(run(mode, &cells).unwrap()).unwrap();
        let outputs = &response["results"][0]["outputs"];
        assert_eq!(
            outputs["least"]["value"],
            json!({"kind":"integer","value":10}),
            "{mode}"
        );
        assert_eq!(
            outputs["latest"]["value"],
            json!({"kind":"date","value":"2026-01-02"}),
            "{mode}"
        );
        assert_eq!(
            outputs["linked"]["value"],
            json!({"kind":"integer","value":10}),
            "{mode}"
        );
    }
    // Each fractional loser is read: without it the evaluation misses it.
    for slot in ["rank", "days"] {
        let without = cells
            .iter()
            .filter(|cell| !(cell["name"] == slot && cell["entity_id"] == "Person:2"))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            matches!(
                run("explain", &without),
                Err(ApiError::Eval(EvalError::MissingInput { name, entity_id, .. }))
                    if name == slot && entity_id == "Person:2"
            ),
            "{slot}"
        );
    }
}

/// The `where` clause of `min`, `max` and `only` is read at each member, so
/// its inputs list the related kind with a judgment's dtype.
#[test]
fn an_aggregates_where_clause_is_read_at_each_member() {
    let related = |kind: &str, relation: &str, current: usize, value: &str, flag: &str| {
        let mut node = json!({
            "kind": kind, "relation": relation, "current_slot": current,
            "related_slot": 1 - current, "value": input(value),
            "where": cmp(input(flag), "eq", boolean(true))
        });
        if kind == "extreme_related" {
            node["extreme"] = json!(if flag == "lowest_counted" {
                "min"
            } else {
                "max"
            });
        }
        node
    };
    let lookup =
        |index: Value| json!({"kind":"parameter_lookup","parameter":"table","index":index});
    let interface = derive(json!({
        "relations": [
            {"name":"members","arity":2,"slot_entities":["Person","Household"]},
            {"name":"links","arity":2,"slot_entities":["Household","Household"]}
        ],
        "parameters": [{"name":"table","unit":null,"versions":[{"effective_from":"2020-01-01",
            "values":{"1":{"kind":"integer","value":10}}}]}],
        "derived": [
            scalar_rule("lowest", "Household",
                lookup(related("extreme_related", "members", 1, "rank", "lowest_counted"))),
            scalar_rule("highest", "Household",
                lookup(related("extreme_related", "members", 1, "rank", "highest_counted"))),
            scalar_rule("linked", "Household",
                lookup(related("single_related", "links", 0, "link_rank", "link_counted")))
        ]
    }));
    for (slot, entity) in [
        ("lowest_counted", "Person"),
        ("highest_counted", "Person"),
        ("link_counted", "Household"),
    ] {
        let input = find(&interface, slot);
        assert_eq!(input.entities, kinds(&[entity]), "{slot}");
        assert_eq!(input.dtype, Some(InputDType::Bool), "{slot}");
    }
}

/// Reads through a term run on the term's id, whatever the rule's entity:
/// `anchor.x` on the derivation's current kind and `candidate.x` on its
/// related kind. The relation's `reads` list them as it lists unqualified
/// reads. An unqualified read beside them keeps the predicate's routing.
#[test]
fn reads_through_a_term_run_on_the_terms_kind() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "older_members", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": {"kind": "and", "items": [
                    cmp(role_input("candidate", "age"), "gte", int(18)),
                    cmp(role_input("anchor", "head_age"), "gte", int(30)),
                    cmp(role_derived("candidate", "member_score"), "gt", int(0)),
                    role_derived("anchor", "household_eligible"),
                    cmp(input("member_flag"), "eq", boolean(true))
                ]}
            }}
        ],
        "derived": [
            judgment_rule("household_eligible", "Household", cmp(input("opt_in"), "eq", boolean(true))),
            scalar_rule("member_score", "Person", add(vec![input("score_input"), int(0)])),
            scalar_rule("older_count", "Household", json!({
                "kind": "count_related", "relation": "older_members",
                "current_slot": 1, "related_slot": 0
            }))
        ]
    }));
    for (slot, entities, dtype) in [
        ("age", &["Person"], Some(InputDType::Decimal)),
        ("head_age", &["Household"], Some(InputDType::Decimal)),
        ("score_input", &["Person"], Some(InputDType::Decimal)),
        ("opt_in", &["Household"], Some(InputDType::Bool)),
        ("member_flag", &["Person"], Some(InputDType::Bool)),
    ] {
        let input = find(&interface, slot);
        assert_eq!(input.entities, kinds(entities), "{slot}");
        assert_eq!(input.dtype, dtype, "{slot}");
    }
    let older = interface
        .relations
        .iter()
        .find(|relation| relation.name == "older_members")
        .unwrap();
    assert_eq!(older.lookup_slots, vec![1]);
    assert_eq!(
        older.provenance,
        RelationProvenance::Derived {
            rule: "older_members".to_string(),
            source_relation: "member_of_household".to_string(),
            reads: RelationReads {
                inputs: kinds(&["age", "head_age", "member_flag"]),
                rules: kinds(&["household_eligible", "member_score"]),
                relations: vec![],
            },
        }
    );
}

/// A read through a term at a parameter's index takes the parameter's keys as
/// its closed domain, as an unqualified read at an index does.
#[test]
fn a_read_through_a_term_at_a_parameter_index_lists_its_keys() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "banded_members", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": cmp(json!({
                    "kind": "parameter_lookup", "parameter": "band_of_age",
                    "index": role_input("candidate", "band_age")
                }), "eq", int(2))
            }}
        ],
        "parameters": [{
            "name": "band_of_age", "unit": null,
            "versions": [{"effective_from": "2020-01-01", "values": {
                "12": {"kind": "integer", "value": 1},
                "30": {"kind": "integer", "value": 2},
                "45": {"kind": "integer", "value": 3}
            }}]
        }],
        "derived": [scalar_rule("banded_count", "Household", json!({
            "kind": "count_related", "relation": "banded_members",
            "current_slot": 1, "related_slot": 0
        }))]
    }));
    let band_age = find(&interface, "band_age");
    assert_eq!(band_age.entities, kinds(&["Person"]));
    assert_eq!(band_age.dtype, Some(InputDType::Integer));
    assert_eq!(band_age.values, Some(vec![12, 30, 45]));
    assert_eq!(band_age.values_basis, Some(ValuesBasis::ParameterIndex));
}

/// A rule read through `anchor` runs on the anchor's id even when its entity
/// is a filtered entity of the current kind, which an unqualified read of the
/// same rule would run on the related id. Both positions are covered: a
/// judgment and a scalar.
#[test]
fn a_filtered_entity_rule_read_through_anchor_runs_on_the_anchors_id() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "adults_of", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": {"kind": "and", "items": [
                    cmp(role_input("candidate", "age"), "gte", int(18)),
                    role_derived("anchor", "unit_opted_in"),
                    cmp(role_derived("anchor", "unit_score"), "gt", int(0))
                ]}
            }},
            {"name": "unit_members", "arity": 2, "derivation": {
                "source_relation": "adults_of",
                "current_slot": 1, "related_slot": 0,
                "entity": "Unit",
                "slot_entities": ["Person", "Household"],
                "predicate": cmp(role_input("anchor", "head_age"), "gte", int(30))
            }}
        ],
        "derived": [
            judgment_rule("unit_opted_in", "Unit", cmp(input("opt_in"), "eq", boolean(true))),
            scalar_rule("unit_score", "Unit", add(vec![input("unit_points"), int(0)])),
            scalar_rule("adult_count", "Household", json!({
                "kind": "count_related", "relation": "adults_of",
                "current_slot": 1, "related_slot": 0
            }))
        ]
    }));
    // Each Unit rule runs on household ids, as its own entity and as the
    // anchor's; neither runs on a person id.
    for slot in ["opt_in", "unit_points"] {
        assert_eq!(
            find(&interface, slot).entities,
            kinds(&["Household"]),
            "{slot}"
        );
    }
    assert_eq!(find(&interface, "age").entities, kinds(&["Person"]));
    assert_eq!(find(&interface, "head_age").entities, kinds(&["Household"]));
    // The Unit rules' own entity also yields household ids, so only the
    // relation's reads show that the predicate references them.
    let adults_of = interface
        .relations
        .iter()
        .find(|relation| relation.name == "adults_of")
        .unwrap();
    assert_eq!(
        adults_of.provenance,
        RelationProvenance::Derived {
            rule: "adults_of".to_string(),
            source_relation: "member_of_household".to_string(),
            reads: RelationReads {
                inputs: kinds(&["age"]),
                rules: kinds(&["unit_opted_in", "unit_score"]),
                relations: vec![],
            },
        }
    );
}

/// An unqualified read of a filtered-entity rule in a predicate runs on the
/// related id unless the rule's entity is the derivation's declared current
/// kind. Here `CareUnit` filters `Person`, the related kind, so its rule runs
/// on person ids only, never on the current household's.
#[test]
fn an_unqualified_filtered_entity_rule_in_a_predicate_runs_on_the_related_id() {
    let interface = derive(json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "guardian_of", "arity": 2, "slot_entities": ["Person", "Person"]},
            {"name": "care_unit", "arity": 2, "derivation": {
                "source_relation": "guardian_of",
                "current_slot": 0, "related_slot": 1,
                "entity": "CareUnit",
                "slot_entities": ["Person", "Person"],
                "predicate": cmp(input("cares"), "eq", boolean(true))
            }},
            {"name": "covered_members", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": derived_ref("care_unit_covered")
            }}
        ],
        "derived": [
            judgment_rule("care_unit_covered", "CareUnit",
                cmp(input("covered"), "eq", boolean(true))),
            scalar_rule("covered_count", "Household", json!({
                "kind": "count_related", "relation": "covered_members",
                "current_slot": 1, "related_slot": 0
            }))
        ]
    }));
    assert_eq!(find(&interface, "covered").entities, kinds(&["Person"]));
    assert_eq!(find(&interface, "cares").entities, kinds(&["Person"]));
}
