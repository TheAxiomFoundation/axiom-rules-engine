//! Standalone review reproduction for commit 5f9045d (issue #202).
//! Compile against this checkout's built axiom_rules_engine, serde_json, and
//! rust_decimal libraries. main() asserts parity and exits nonzero on the bug.

use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::spec::{PeriodSpec, ProgramSpec};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const INCOMES: [&str; 3] = ["79228162514264337593543950335", "1", "0"];

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn member() -> Value {
    json!({ "kind": "relation_member", "relation": "member", "current_slot": 0, "related_slot": 1 })
}

fn program(expr: Value, judgment: bool) -> Value {
    json!({
        "relations": [{ "name": "member", "arity": 2, "slot_entities": ["Household", "Person"] }],
        "derived": [{
            "name": "n", "entity": "Household", "unit": null,
            "dtype": if judgment { "judgment" } else { "decimal" },
            "semantics": if judgment { "judgment" } else { "scalar" },
            "expr": expr,
        }],
    })
}

fn explain_error(program: &Value, supply_income: bool) -> String {
    let interval = json!({ "start": "2026-01-01", "end": "2026-01-31" });
    let mut inputs = Vec::new();
    let mut relations = Vec::new();
    for (index, income) in INCOMES.iter().enumerate() {
        let person = format!("p{}", index + 1);
        if supply_income {
            inputs.push(json!({
                "name": "income", "entity": "Person", "entity_id": person,
                "interval": interval, "value": { "kind": "decimal", "value": income },
            }));
        }
        relations.push(json!({ "name": "member", "tuple": ["h1", person], "interval": interval }));
    }
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain", "program": program,
        "dataset": { "inputs": inputs, "relations": relations },
        "queries": [{ "entity_id": "h1", "period": period(), "outputs": ["n"] }],
    })).expect("request parses");
    execute_request(request).expect_err("explain must fail").to_string()
}

fn compile_dense(program: &Value) -> Result<DenseCompiledProgram, DenseCompileError> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("artifact compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
}

fn dense_error(program: &Value) -> String {
    let compiled = compile_dense(program).expect("dense compiles the sum");
    let incomes = INCOMES.iter().map(|value| Decimal::from_str(value).unwrap()).collect::<Vec<_>>();
    let relations = compiled.relations().iter().map(|schema| {
        let inputs = schema.related_inputs.iter().map(|name| {
            assert_eq!(name, "income");
            (name.clone(), DenseColumn::Decimal(incomes.clone()))
        }).collect();
        (schema.key.clone(), DenseRelationBatchSpec { offsets: vec![0, 3], inputs })
    }).collect();
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    compiled.execute(
        &period.to_model().expect("period converts"),
        DenseBatchSpec { row_count: 1, inputs: HashMap::new(), relations },
        &["n".to_string()],
    ).expect_err("dense must fail").to_string()
}

fn main() {
    // Documentation says membership anywhere outside a derived filter fails
    // reached rows, but root membership still declines at compilation.
    let root = program(member(), true);
    let root_explain = explain_error(&root, false);
    let root_dense = compile_dense(&root).expect_err("root membership is unsupported");
    println!("ROOT EXPLAIN: {root_explain}\nROOT DENSE: {root_dense:?}");
    assert!(root_explain.contains("can only be evaluated inside a derived relation"));
    assert!(matches!(root_dense, DenseCompileError::Unsupported(_)));

    // Explain visits p1, adds MAX, visits p2, and overflows adding 1. It never
    // reaches p3's relation_member. Dense evaluates every where clause first.
    let income = json!({ "kind": "input", "name": "income" });
    let sum = program(json!({
        "kind": "sum_related", "relation": "member", "current_slot": 0, "related_slot": 1,
        "value": income,
        "where": { "kind": "or", "items": [
            { "kind": "comparison", "left": income, "op": "gt",
              "right": { "kind": "literal", "value": { "kind": "decimal", "value": "0" } } },
            member(),
        ] },
    }), false);
    let expected = explain_error(&sum, true);
    let actual = dense_error(&sum);
    println!("SUM EXPLAIN: {expected}\nSUM DENSE: {actual}");
    assert!(expected.contains("arithmetic overflow"));
    assert_eq!(actual, expected, "sum overflow on p2 must precede p3 membership error");
}
