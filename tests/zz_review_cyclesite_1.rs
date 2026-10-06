// Scratch review repro (delete before finishing): cycle guard keyed by rule
// name vs. site changes from_program's error message for a cycle that crosses
// from a related site to a root site.

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::DenseCompiledProgram;
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

fn member() -> Value {
    json!({ "name": "member", "arity": 2, "slot_entities": ["Household", "Person"] })
}
fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}
fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}
fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}
fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}
fn and(items: Vec<Value>) -> Value {
    json!({ "kind": "and", "items": items })
}
fn if_then_else(c: Value, t: Value, e: Value) -> Value {
    json!({ "kind": "if", "condition": c, "then_expr": t, "else_expr": e })
}
fn count_where(relation: &str, w: Value) -> Value {
    json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1, "where": w })
}
fn relation_member(relation: &str) -> Value {
    json!({ "kind": "relation_member", "relation": relation, "current_slot": 0, "related_slot": 1 })
}
fn scalar_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "decimal", "unit": null, "semantics": "scalar", "expr": expr })
}
fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

fn programs() -> Vec<(&'static str, Value)> {
    let s_rule = scalar_rule("s", "Scalar", if_then_else(derived("j"), integer(1), integer(0)));
    let total = scalar_rule("total", "Household", count_where("member", derived("j")));
    vec![
        (
            "cross-entity variant",
            json!({
                "relations": [member()],
                "derived": [
                    judgment_rule("p", "Person", compare(input("income"), "gt", integer(0))),
                    judgment_rule("j", "Scalar", and(vec![derived("p"), compare(derived("s"), "gt", integer(0))])),
                    s_rule.clone(),
                    total.clone(),
                ],
            }),
        ),
        (
            "relation_member variant",
            json!({
                "relations": [member()],
                "derived": [
                    judgment_rule("j", "Scalar", and(vec![relation_member("member"), compare(derived("s"), "gt", integer(0))])),
                    s_rule,
                    total,
                ],
            }),
        ),
    ]
}

#[test]
fn zz_review_cyclesite() {
    for (label, program) in programs() {
        for attempt in 0..3 {
            let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("parses");
            let lowered = spec.to_program().expect("lowers");
            let result = DenseCompiledProgram::from_program(&lowered, Some("Household"));
            match result {
                Ok(_) => println!("{label} [{attempt}]: OK"),
                Err(error) => println!("{label} [{attempt}]: ERR {error:?}"),
            }
        }
        let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("parses");
        match CompiledProgramArtifact::compile(spec) {
            Ok(_) => println!("{label} artifact: OK"),
            Err(error) => println!("{label} artifact: ERR {error}"),
        }
    }
}
