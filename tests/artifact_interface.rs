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
