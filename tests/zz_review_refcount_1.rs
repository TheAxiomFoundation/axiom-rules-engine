// Scratch reproduction for PR #223 review (refcount/claims lens). DELETE before finishing.
use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
};
use axiom_rules_engine::spec::{PeriodSpec, ProgramSpec, ScalarValueSpec};
use rust_decimal::Decimal;
use serde_json::{Value, json};

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}
fn interval() -> Value {
    json!({ "start": "2026-01-01", "end": "2026-01-31" })
}
fn member() -> Value {
    json!({ "name": "member", "arity": 2, "slot_entities": ["Household", "Person"] })
}
fn derived_relation(name: &str, source: &str, predicate: Value) -> Value {
    json!({
        "name": name, "arity": 2, "slot_entities": ["Household", "Person"],
        "derivation": { "source_relation": source, "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Household", "Person"], "predicate": predicate },
    })
}
fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}
fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}
fn decimal(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "decimal", "value": value } })
}
fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}
fn text(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "text", "value": value } })
}
fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}
fn add(items: Vec<Value>) -> Value {
    json!({ "kind": "add", "items": items })
}
fn and(items: Vec<Value>) -> Value {
    json!({ "kind": "and", "items": items })
}
fn or(items: Vec<Value>) -> Value {
    json!({ "kind": "or", "items": items })
}
fn if_then_else(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr })
}
fn count(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1 });
    if let Some(w) = where_clause {
        expr["where"] = w;
    }
    expr
}
fn sum(relation: &str, rule: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "sum_related", "relation": relation, "current_slot": 0, "related_slot": 1,
        "value": { "kind": "derived", "name": rule } });
    if let Some(w) = where_clause {
        expr["where"] = w;
    }
    expr
}
fn scalar_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "decimal", "unit": null, "semantics": "scalar", "expr": expr })
}
fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}
fn program(relations: Vec<Value>, derived: Vec<Value>) -> Value {
    json!({ "relations": relations, "derived": derived })
}
fn compile_dense(program: &Value) -> Result<DenseCompiledProgram, String> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("parses");
    DenseCompiledProgram::from_program(&spec.to_program().expect("lowers"), Some("Household"))
        .map_err(|e| e.to_string())
}

type Answer = Result<Vec<Vec<Decimal>>, String>;

/// households: (size, incomes)
fn explain(households: &[(i64, Vec<&str>)], program: &Value, outputs: &[&str]) -> Answer {
    let s = program.to_string();
    let reads_income = s.contains(r#""kind":"input","name":"income""#);
    let reads_size = s.contains(r#""kind":"input","name":"size""#);
    let mut inputs = Vec::new();
    let mut tuples = Vec::new();
    for (index, (size, incomes)) in households.iter().enumerate() {
        let id = format!("h{index}");
        if reads_size {
            inputs.push(json!({ "name": "size", "entity": "Household", "entity_id": id, "interval": interval(),
                "value": { "kind": "integer", "value": size } }));
        }
        for (position, income) in incomes.iter().enumerate() {
            let person = format!("{id}p{position}");
            if reads_income {
                inputs.push(json!({ "name": "income", "entity": "Person", "entity_id": person, "interval": interval(),
                    "value": { "kind": "decimal", "value": income } }));
            }
            tuples.push(json!({ "name": "member", "tuple": [id, person], "interval": interval() }));
        }
    }
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain", "program": program,
        "dataset": { "inputs": inputs, "relations": tuples },
        "queries": (0..households.len()).map(|i| json!({ "entity_id": format!("h{i}"), "period": period(), "outputs": outputs })).collect::<Vec<_>>(),
    }))
    .expect("request parses");
    let response = execute_request(request).map_err(|e| e.to_string())?;
    Ok(response
        .results
        .iter()
        .map(|result| {
            outputs
                .iter()
                .map(|o| match &result.outputs[*o] {
                    OutputValue::Scalar { value, .. } => match value {
                        ScalarValueSpec::Integer { value } => Decimal::from(*value),
                        ScalarValueSpec::Decimal { value } => Decimal::from_str(value).unwrap(),
                        other => panic!("unexpected {other:?}"),
                    },
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        })
        .collect())
}

fn dense(compiled: &DenseCompiledProgram, households: &[(i64, Vec<&str>)], outputs: &[&str]) -> Answer {
    let mut offsets = vec![0];
    let mut incomes = Vec::new();
    for (_, hh) in households {
        offsets.push(offsets.last().copied().unwrap() + hh.len());
        incomes.extend(hh.iter().map(|i| Decimal::from_str(i).unwrap()));
    }
    let relations = compiled
        .relations()
        .iter()
        .map(|schema| {
            (
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: offsets.clone(),
                    inputs: HashMap::from([("income".to_string(), DenseColumn::Decimal(incomes.clone()))]),
                },
            )
        })
        .collect();
    let period: PeriodSpec = serde_json::from_value(period()).unwrap();
    let period = period.to_model().unwrap();
    let batch = DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([(
            "size".to_string(),
            DenseColumn::Integer(households.iter().map(|h| h.0).collect()),
        )]),
        relations,
    };
    let outputs: Vec<String> = outputs.iter().map(|o| o.to_string()).collect();
    let result = compiled.execute(&period, batch, &outputs).map_err(|e| e.to_string())?;
    Ok((0..result.row_count)
        .map(|row| {
            outputs
                .iter()
                .map(|o| match &result.outputs[o] {
                    DenseOutputValue::Scalar(DenseColumn::Integer(v)) => Decimal::from(v[row]),
                    DenseOutputValue::Scalar(DenseColumn::Decimal(v)) => v[row],
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        })
        .collect())
}

/// `b` is a number on members with income and text on the others. `pos`
/// reads it only for members with income, `neg` only for the others; each
/// read alone sees one dtype. `dup` is the same program with `neg` reading an
/// identical copy `b2`, so neither body is shared (the in-place path, as on main).
fn mixed_dtype_program(share: bool) -> Value {
    let body = || {
        if_then_else(
            compare(input("income"), "gt", decimal("0")),
            input("income"),
            text("none"),
        )
    };
    let mut rules = vec![
        scalar_rule("b", "Person", body()),
        judgment_rule(
            "pos",
            "Person",
            and(vec![
                compare(input("income"), "gt", decimal("0")),
                compare(derived("b"), "gt", decimal("0")),
            ]),
        ),
        judgment_rule(
            "neg",
            "Person",
            and(vec![
                compare(input("income"), "lte", decimal("0")),
                compare(derived(if share { "b" } else { "b2" }), "eq", text("none")),
            ]),
        ),
        scalar_rule("c_pos", "Household", count("member", Some(derived("pos")))),
        scalar_rule("c_neg", "Household", count("member", Some(derived("neg")))),
    ];
    if !share {
        rules.push(scalar_rule("b2", "Person", body()));
    }
    program(vec![member()], rules)
}

#[test]
fn zz_mixed_dtype_shared_body() {
    let households = vec![(1, vec!["5", "0"])];
    for share in [false, true] {
        let program = mixed_dtype_program(share);
        let compiled = compile_dense(&program).expect("compiles");
        for outputs in [&["c_pos"][..], &["c_neg"], &["c_pos", "c_neg"]] {
            println!(
                "share={share} outputs={outputs:?}\n  explain = {:?}\n  dense   = {:?}",
                explain(&households, &program, outputs),
                dense(&compiled, &households, outputs)
            );
        }
    }
}

/// The random test's `shares_a_body` is satisfied by its fixed template alone:
/// `eligible`, `link1` and `link0` all read the household judgment.
#[test]
fn zz_template_alone_shares_a_body() {
    let rules = vec![
        scalar_rule("p0", "Person", input("income")),
        judgment_rule("pj", "Person", compare(input("income"), "gt", decimal("0"))),
        scalar_rule("h0", "Household", input("size")),
        judgment_rule("hj", "Household", compare(input("size"), "gt", decimal("0"))),
        scalar_rule("total", "Household", sum("member", "p0", Some(derived("pj")))),
        scalar_rule("counted", "Household", count("eligible", None)),
        scalar_rule("chained", "Household", count("link0", Some(derived("pj")))),
        scalar_rule(
            "direct",
            "Household",
            add(vec![derived("h0"), sum("member", "p0", None)]),
        ),
    ];
    let eligible = and(vec![
        derived("hj"),
        compare(derived("h0"), "gte", decimal("0")),
        derived("pj"),
    ]);
    let program = program(
        vec![
            member(),
            derived_relation("eligible", "member", eligible),
            derived_relation("link1", "member", or(vec![derived("hj"), derived("pj")])),
            derived_relation("link0", "link1", and(vec![derived("hj"), derived("pj")])),
        ],
        rules,
    );
    let compiled = compile_dense(&program).expect("compiles");
    let plan = format!("{compiled:?}");
    let counts: Vec<&str> = plan
        .match_indices("references: ")
        .map(|(at, p)| {
            let rest = &plan[at + p.len()..];
            &rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap()]
        })
        .collect();
    println!("references per body: {counts:?}");
}

/// Stack depth of a linear chain on a 2 MB thread. ZZ_ARM = sum | predicate,
/// ZZ_DEPTH = levels, ZZ_EXEC = 1 to also execute.
#[test]
fn zz_stack_depth() {
    let Ok(arm) = std::env::var("ZZ_ARM") else { return };
    let depth: usize = std::env::var("ZZ_DEPTH").unwrap().parse().unwrap();
    let exec = std::env::var("ZZ_EXEC").is_ok();
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let program = match arm.as_str() {
                "sum" => {
                    let mut rules = vec![scalar_rule("p0", "Person", input("income"))];
                    for i in 1..=depth {
                        rules.push(scalar_rule(
                            &format!("p{i}"),
                            "Person",
                            add(vec![derived(&format!("p{}", i - 1)), integer(1)]),
                        ));
                    }
                    rules.push(scalar_rule("total", "Household", sum("member", &format!("p{depth}"), None)));
                    program(vec![member()], rules)
                }
                _ => {
                    let mut rules = vec![scalar_rule("h0", "Household", input("size"))];
                    for i in 1..=depth {
                        rules.push(scalar_rule(
                            &format!("h{i}"),
                            "Household",
                            add(vec![derived(&format!("h{}", i - 1)), integer(1)]),
                        ));
                    }
                    rules.push(scalar_rule("total", "Household", count("eligible", None)));
                    program(
                        vec![
                            member(),
                            derived_relation(
                                "eligible",
                                "member",
                                compare(derived(&format!("h{depth}")), "gt", integer(0)),
                            ),
                        ],
                        rules,
                    )
                }
            };
            let compiled = compile_dense(&program).expect("compiles");
            if exec {
                let households = vec![(1, vec!["5", "0"])];
                let _ = dense(&compiled, &households, &["total"]);
            }
            println!("ok depth {depth}");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn zz_mixed_dtype_via_artifact() {
    let program = mixed_dtype_program(true);
    let spec: ProgramSpec = serde_json::from_value(program).unwrap();
    let artifact = axiom_rules_engine::compile::CompiledProgramArtifact::compile(spec);
    println!("artifact compile: {}", if artifact.is_ok() { "ok".to_string() } else { format!("{:?}", artifact.as_ref().err()) });
    if let Ok(artifact) = artifact {
        let compiled = DenseCompiledProgram::from_artifact(&artifact, Some("Household")).unwrap();
        println!("dense via artifact: {:?}", dense(&compiled, &[(1, vec!["5", "0"])], &["c_pos", "c_neg"]));
    }
}

fn ref_totals(program: &Value) -> (usize, usize) {
    let plan = format!("{:?}", compile_dense(program).expect("compiles"));
    let nodes = plan.matches("Inline(").count();
    let refs: usize = plan
        .match_indices("references: ")
        .map(|(at, p)| {
            let rest = &plan[at + p.len()..];
            rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap()].parse::<usize>().unwrap()
        })
        .sum();
    (nodes, refs)
}

#[test]
fn zz_references_equal_nodes() {
    let labelled = json!({
        "kind": "if",
        "condition": compare(derived("s"), "eq", decimal("100")),
        "then_expr": add(vec![derived("s"), derived("hh")]),
        "else_expr": { "kind": "no_match", "subject": derived("s"), "patterns": [decimal("100")] },
    });
    let hh_match = json!({
        "kind": "if",
        "condition": compare(derived("hh"), "eq", integer(1)),
        "then_expr": derived("hh"),
        "else_expr": { "kind": "no_match", "subject": input("size"), "patterns": [integer(1)] },
    });
    let programs = vec![
        mixed_dtype_program(true),
        mixed_dtype_program(false),
        program(
            vec![member(), derived_relation("eligible", "member", compare(derived("hm"), "gt", integer(0)))],
            vec![
                scalar_rule("s", "Person", add(vec![input("income"), derived("hh")])),
                scalar_rule("hh", "Household", add(vec![input("size"), integer(1)])),
                scalar_rule("hm", "Household", hh_match),
                scalar_rule("l", "Person", labelled),
                scalar_rule("t1", "Household", sum("member", "l", Some(compare(derived("s"), "gt", decimal("0"))))),
                scalar_rule("t2", "Household", count("eligible", Some(compare(derived("l"), "gt", derived("hh"))))),
                scalar_rule("t3", "Household", sum("member", "hh", None)),
            ],
        ),
    ];
    for p in &programs {
        let (nodes, refs) = ref_totals(p);
        println!("Inline nodes = {nodes}, sum of references = {refs}");
        assert_eq!(nodes, refs);
    }
    // explain vs dense on the third program
    let hh = vec![(1, vec!["100", "0"]), (2, vec!["100"]), (1, vec![])];
    let compiled = compile_dense(&programs[2]).unwrap();
    for outputs in [&["t1"][..], &["t2"], &["t3"], &["t1", "t2", "t3"], &["t3", "t2", "t1"]] {
        println!("{outputs:?}: explain={:?} dense={:?}", explain(&hh, &programs[2], outputs), dense(&compiled, &hh, outputs));
    }
}

/// k household rules in a chain, one derived relation whose predicate reads
/// the top, and k outputs that each count that relation.
fn fan_program(k: usize) -> Value {
    let mut rules = vec![scalar_rule("h0", "Household", input("size"))];
    for i in 1..=k {
        rules.push(scalar_rule(&format!("h{i}"), "Household", add(vec![derived(&format!("h{}", i - 1)), integer(1)])));
    }
    for j in 0..k {
        rules.push(scalar_rule(&format!("c{j}"), "Household", count("eligible", None)));
    }
    program(
        vec![member(), derived_relation("eligible", "member", compare(derived(&format!("h{k}")), "gt", integer(0)))],
        rules,
    )
}

#[test]
fn zz_fan_timing() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
            let households: Vec<(i64, Vec<&str>)> = (0..200).map(|i| (i % 3, vec!["1", "2"])).collect();
            for k in [50usize, 100, 200, 400] {
                let program = fan_program(k);
                let compiled = compile_dense(&program).unwrap();
                let plan = format!("{compiled:?}");
                let outputs: Vec<String> = (0..k).map(|j| format!("c{j}")).collect();
                let refs: Vec<&str> = outputs.iter().map(|s| s.as_str()).collect();
                let start = std::time::Instant::now();
                let answer = dense(&compiled, &households, &refs);
                let elapsed = start.elapsed();
                println!("k={k}: plan {} bytes, execute {:?}, ok={}", plan.len(), elapsed, answer.is_ok());
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn zz_bytes_per_level() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(|| {
        let two = |depth: usize, arm: &str| -> usize {
            let p = match arm {
                "sum" => {
                    let mut rules = vec![scalar_rule("p0", "Person", input("income"))];
                    for i in 1..=depth {
                        let b = || derived(&format!("p{}", i - 1));
                        rules.push(scalar_rule(&format!("p{i}"), "Person", add(vec![b(), b()])));
                    }
                    rules.push(scalar_rule("total", "Household", sum("member", &format!("p{depth}"), None)));
                    program(vec![member()], rules)
                }
                _ => {
                    let mut rules = vec![scalar_rule("h0", "Household", input("size"))];
                    for i in 1..=depth {
                        let b = || derived(&format!("h{}", i - 1));
                        rules.push(scalar_rule(&format!("h{i}"), "Household", add(vec![b(), b()])));
                    }
                    rules.push(scalar_rule("total", "Household", count("eligible", None)));
                    program(vec![member(), derived_relation("eligible", "member", compare(derived(&format!("h{depth}")), "gt", integer(0)))], rules)
                }
            };
            format!("{:?}", compile_dense(&p).unwrap()).len()
        };
        for arm in ["sum", "pred"] {
            let sizes: Vec<usize> = [16, 17, 18].iter().map(|d| two(*d, arm)).collect();
            println!("{arm}: sizes {sizes:?}, per level {} / {}", sizes[1] - sizes[0], sizes[2] - sizes[1]);
        }
    }).unwrap().join().unwrap();
}
