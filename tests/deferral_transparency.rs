//! Deferral (src/depth.rs) changes no result and adds only a constant factor
//! of work (#206).
//!
//! `tests/execution_mode_parity.rs` checks threshold independence on random
//! programs. These tests pin the cases a random generator reaches rarely or
//! not at all:
//!
//! * handcrafted programs rich in what a retry could duplicate or lose:
//!   trace dependencies, skipped dependencies and parameter reads recorded
//!   before the deferral point, rules with output rounding (and their
//!   pre-rounding trace values) as deferred tasks, `NoMatchingArm` naming the
//!   innermost rule, errors in deferred rules, laziness, relation contexts,
//!   `count`/`sum` whose members each defer, aggregations inside derived
//!   relations' predicates, and one engine reused across calls after an
//!   error. Explain's full response and fast's response are
//!   byte-identical at every threshold and with deferral off;
//! * an aggregation whose every member defers costs one pass: its retries
//!   resume at the member that deferred;
//! * a 20,000-rule chain costs about four times a 5,000-rule chain in every
//!   mode, measured in nodes visited, which does not depend on the machine;
//! * the lifetime gate accepts exactly the outputs that reduce over periods,
//!   directly or through any rule they read, and a rule diamond 40 levels
//!   deep does not make it exponential.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::depth::{count_visits, with_suspend_depth};
use axiom_rules_engine::engine::{Engine, EvalError};
use axiom_rules_engine::model::{DataSet, Period, PeriodKind, Program};
use axiom_rules_engine::spec::ProgramSpec;
use serde_json::{Value, json};

const MIB: usize = 1024 * 1024;

/// Thresholds compared with no deferral. At 1, every reference to a rule not
/// yet evaluated defers once any expression is open.
const THRESHOLDS: [usize; 7] = [1, 2, 3, 4, 5, 6, 7];

/// Run `f` on a fresh thread with a `bytes` stack. Without deferral a debug
/// build recurses through every chain on the stack, so the comparisons run
/// on a large one.
fn on_stack<T: Send>(bytes: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(bytes)
            .spawn_scoped(scope, f)
            .expect("spawn test thread")
            .join()
            .expect("test thread")
    })
}

fn int(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn dec(value: &str) -> Value {
    json!({"kind": "literal", "value": {"kind": "decimal", "value": value}})
}

fn decimal_value(value: &str) -> Value {
    json!({"kind": "decimal", "value": value})
}

fn derived(name: &str) -> Value {
    json!({"kind": "derived", "name": name})
}

fn input(name: &str) -> Value {
    json!({"kind": "input", "name": name})
}

fn input_or(name: &str, default: &str) -> Value {
    json!({"kind": "input_or_else", "name": name, "default": decimal_value(default)})
}

fn param(parameter: &str, index: Value) -> Value {
    json!({"kind": "parameter_lookup", "parameter": parameter, "index": index})
}

fn add(items: Vec<Value>) -> Value {
    json!({"kind": "add", "items": items})
}

fn div(left: Value, right: Value) -> Value {
    json!({"kind": "div", "left": left, "right": right})
}

fn mul(left: Value, right: Value) -> Value {
    json!({"kind": "mul", "left": left, "right": right})
}

fn if_then(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({"kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr})
}

fn cmp(left: Value, op: &str, right: Value) -> Value {
    json!({"kind": "comparison", "left": left, "op": op, "right": right})
}

fn and(items: Vec<Value>) -> Value {
    json!({"kind": "and", "items": items})
}

fn or(items: Vec<Value>) -> Value {
    json!({"kind": "or", "items": items})
}

fn not(item: Value) -> Value {
    json!({"kind": "not", "item": item})
}

fn no_match(subject: Value, patterns: Vec<Value>) -> Value {
    json!({"kind": "no_match", "subject": subject, "patterns": patterns})
}

fn count_related(relation: &str, current: usize, related: usize, filter: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "count_related", "relation": relation,
        "current_slot": current, "related_slot": related,
    });
    if let Some(filter) = filter {
        expr["where"] = filter;
    }
    expr
}

fn sum_related(
    relation: &str,
    current: usize,
    related: usize,
    value: &str,
    filter: Option<Value>,
) -> Value {
    let mut expr = json!({
        "kind": "sum_related", "relation": relation,
        "current_slot": current, "related_slot": related,
        "value": {"kind": "derived", "name": value},
    });
    if let Some(filter) = filter {
        expr["where"] = filter;
    }
    expr
}

fn over_periods(value: Value) -> Value {
    json!({"kind": "over_periods", "over": "sum", "value": value})
}

fn scalar(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "decimal", "unit": null,
        "semantics": "scalar", "expr": expr,
    })
}

fn judgment(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "judgment", "unit": null,
        "semantics": "judgment", "expr": expr,
    })
}

fn with_id(mut rule: Value, id: &str) -> Value {
    rule["id"] = json!(id);
    rule
}

/// A dollar amount rounded half up to whole dollars (the `USD` unit below has
/// no minor units).
fn rounded(mut rule: Value, mode: &str) -> Value {
    rule["unit"] = json!("USD");
    rule["rounding"] = json!(mode);
    rule
}

/// `{prefix}0` .. `{prefix}{n}`: rule `i` adds rule `i + 1` and `tail(i)`; the
/// last rule is `last`.
fn chain(
    prefix: &str,
    entity: &str,
    n: usize,
    tail: impl Fn(usize) -> Value,
    last: Value,
) -> Vec<Value> {
    let mut rules = Vec::with_capacity(n + 1);
    for index in 0..n {
        rules.push(scalar(
            &format!("{prefix}{index}"),
            entity,
            add(vec![
                derived(&format!("{prefix}{}", index + 1)),
                tail(index),
            ]),
        ));
    }
    rules.push(scalar(&format!("{prefix}{n}"), entity, last));
    rules
}

fn month(number: u32) -> Value {
    let end = match number {
        1 => "31",
        2 => "28",
        _ => "30",
    };
    json!({
        "period_kind": "month",
        "start": format!("2026-{number:02}-01"),
        "end": format!("2026-{number:02}-{end}"),
    })
}

fn query(entity_id: &str, number: u32, outputs: &[&str]) -> Value {
    json!({"entity_id": entity_id, "period": month(number), "outputs": outputs})
}

fn input_record(entity: &str, entity_id: &str, name: &str, value: Value) -> Value {
    json!({
        "entity": entity, "entity_id": entity_id, "name": name,
        "interval": {"start": "2026-01-01", "end": "2026-12-31"}, "value": value,
    })
}

fn relation_record(name: &str, tuple: &[&str]) -> Value {
    json!({"name": name, "tuple": tuple, "interval": {"start": "2026-01-01", "end": "2026-12-31"}})
}

fn members_relation() -> Value {
    json!({"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]})
}

fn run(mode: &str, program: &Value, dataset: &Value, queries: &Value) -> String {
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": mode, "program": program, "dataset": dataset, "queries": queries,
    }))
    .expect("request deserializes");
    match execute_request(request) {
        Ok(response) => serde_json::to_string(&response).expect("responses serialize"),
        Err(error) => format!("Err({error:?})"),
    }
}

/// Explain's and fast's responses at every threshold, compared with deferral
/// off. Returns each difference, located.
fn divergences(label: &str, program: &Value, dataset: &Value, queries: &Value) -> Vec<String> {
    let mut divergences = Vec::new();
    for mode in ["explain", "fast"] {
        let recursive = with_suspend_depth(usize::MAX, || run(mode, program, dataset, queries));
        for threshold in THRESHOLDS {
            let deferred = with_suspend_depth(threshold, || run(mode, program, dataset, queries));
            if deferred != recursive {
                let at = deferred
                    .bytes()
                    .zip(recursive.bytes())
                    .position(|(left, right)| left != right)
                    .unwrap_or(deferred.len().min(recursive.len()));
                let from = at.saturating_sub(200);
                divergences.push(format!(
                    "{label}: {mode} at threshold {threshold} differs at byte {at}\n  \
                     recursive: ...{}\n  deferred:  ...{}",
                    &recursive[from..(at + 300).min(recursive.len())],
                    &deferred[from..(at + 300).min(deferred.len())],
                ));
            }
        }
    }
    divergences
}

/// Trace-rich: parameter reads, skipped branches and short circuits before
/// and after deferral points, rounded rules read twice, a rule evaluated and
/// later skipped again, a chain whose tail would fail but is never reached,
/// and several queries over entities and periods.
fn trace_rich() -> (Value, Value, Value) {
    let mut rules = Vec::new();
    rules.extend(chain(
        "c",
        "Household",
        6,
        |_| param("p_rate", int(0)),
        input_or("x", "7"),
    ));
    rules.extend(chain(
        "dd",
        "Household",
        5,
        |_| int(1),
        input("never_supplied"),
    ));
    rules.extend(chain("e", "Household", 5, |_| int(1), div(int(1), int(0))));
    rules.push(judgment(
        "j_false",
        "Household",
        cmp(derived("c3"), "lt", int(0)),
    ));
    rules.push(judgment(
        "j_true",
        "Household",
        cmp(derived("c2"), "gt", int(0)),
    ));
    rules.push(judgment(
        "j_skip_deep",
        "Household",
        cmp(derived("dd0"), "gt", int(0)),
    ));
    rules.push(scalar(
        "s1",
        "Household",
        add(vec![derived("c4"), int(100)]),
    ));
    rules.push(scalar(
        "err_rule",
        "Household",
        add(vec![derived("e0"), int(1)]),
    ));
    rules.push(with_id(
        scalar(
            "top",
            "Household",
            add(vec![
                param("p_idx", int(1)),
                if_then(
                    and(vec![derived("j_false"), derived("j_skip_deep")]),
                    derived("s1"),
                    derived("c1"),
                ),
                derived("s1"),
                if_then(
                    or(vec![derived("j_true"), derived("j_skip_deep")]),
                    param("p_idx", int(2)),
                    add(vec![derived("err_rule"), param("p_skip", int(0))]),
                ),
                derived("c0"),
                param("p_idx", int(0)),
                // Evaluated above, skipped here.
                if_then(not(derived("j_true")), derived("c2"), derived("c5")),
            ]),
        ),
        "us/top",
    ));
    rules.push(rounded(
        scalar("rounded", "Household", div(derived("top"), int(7))),
        "half_up",
    ));
    rules.push(rounded(
        scalar(
            "rounded_deep",
            "Household",
            div(add(vec![derived("c0"), dec("0.5")]), int(3)),
        ),
        "half_even",
    ));
    rules.push(scalar(
        "uses_rounded",
        "Household",
        add(vec![
            derived("rounded"),
            derived("rounded"),
            derived("c5"),
            derived("rounded_deep"),
        ]),
    ));
    for index in 0..5 {
        rules.push(judgment(
            &format!("jc{index}"),
            "Household",
            or(vec![
                and(vec![
                    derived(&format!("jc{}", index + 1)),
                    cmp(param("p_rate", int(0)), "gt", int(0)),
                ]),
                derived("j_skip_deep"),
            ]),
        ));
    }
    rules.push(judgment(
        "jc5",
        "Household",
        cmp(derived("c2"), "gt", int(0)),
    ));
    rules.push(scalar(
        "match_covered",
        "Household",
        if_then(
            cmp(derived("c1"), "eq", int(999)),
            int(1),
            if_then(
                cmp(derived("c1"), "ne", int(999)),
                int(2),
                no_match(derived("c1"), vec![int(999)]),
            ),
        ),
    ));
    let program = json!({
        "units": [{"name": "USD", "kind": "currency", "minor_units": 0}],
        "parameters": [
            {"name": "p_rate", "unit": null, "versions": [
                {"effective_from": "2026-01-01", "effective_to": "2026-01-31",
                 "values": {"0": decimal_value("5")}},
                {"effective_from": "2026-02-01", "values": {"0": decimal_value("6")}}]},
            {"name": "p_idx", "unit": null, "indexed_by": "idx", "versions": [
                {"effective_from": "2026-01-01", "values": {
                    "0": decimal_value("1"), "1": decimal_value("2"), "2": decimal_value("3")}}]},
            {"name": "p_skip", "unit": null, "versions": [
                {"effective_from": "2026-01-01", "values": {"0": decimal_value("9")}}]},
        ],
        "derived": rules,
    });
    let dataset = json!({
        "inputs": [input_record("Household", "h1", "x", decimal_value("3"))],
        "relations": [],
    });
    let queries = json!([
        query("h1", 1, &["us/top", "uses_rounded", "jc0", "match_covered"]),
        query("h2", 1, &["uses_rounded", "us/top"]),
        query("h1", 2, &["jc0", "us/top", "rounded"]),
        query("h1", 1, &["c3", "us/top"]),
    ]);
    (program, dataset, queries)
}

/// Relation contexts: derived relations whose predicates read person and
/// household chains (the entity comes from the relation context), one
/// derived from the other, and `count`/`sum` with `where` clauses over them.
fn relation_contexts() -> (Value, Value, Value) {
    let mut rules = Vec::new();
    rules.extend(chain("pc", "Person", 5, |_| int(1), input("age")));
    rules.extend(chain("hc", "Household", 5, |_| int(0), input("threshold")));
    for index in 0..5 {
        rules.push(judgment(
            &format!("pj{index}"),
            "Person",
            and(vec![
                derived(&format!("pj{}", index + 1)),
                cmp(derived("pc3"), "gt", int(0)),
            ]),
        ));
    }
    rules.push(judgment("pj5", "Person", cmp(input("age"), "gte", int(0))));
    for index in 0..4 {
        rules.push(judgment(
            &format!("hj{index}"),
            "Household",
            or(vec![
                derived(&format!("hj{}", index + 1)),
                cmp(derived("hc2"), "lt", int(0)),
            ]),
        ));
    }
    rules.push(judgment(
        "hj4",
        "Household",
        cmp(derived("hc4"), "gte", int(0)),
    ));
    rules.push(scalar(
        "n_eligible",
        "Household",
        count_related("eligible", 1, 0, None),
    ));
    rules.push(scalar(
        "n_eligible2",
        "Household",
        count_related("eligible2", 1, 0, None),
    ));
    rules.push(scalar(
        "sum_eligible",
        "Household",
        sum_related("eligible", 1, 0, "pc0", Some(derived("pj1"))),
    ));
    rules.push(scalar(
        "n_where",
        "Household",
        count_related(
            "member_of_household",
            1,
            0,
            Some(or(vec![
                derived("pj0"),
                cmp(derived("pc2"), "gt", int(100)),
            ])),
        ),
    ));
    rules.push(scalar(
        "top",
        "Household",
        add(vec![
            derived("n_eligible"),
            derived("sum_eligible"),
            derived("n_where"),
            derived("n_eligible2"),
        ]),
    ));
    let program = json!({
        "relations": [
            members_relation(),
            {"name": "eligible", "arity": 2, "slot_entities": ["Person", "Household"],
             "derivation": {
                "source_relation": "member_of_household", "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": and(vec![
                    cmp(derived("pc0"), "gt", derived("hc0")),
                    or(vec![derived("pj0"), derived("hj0")]),
                    // Short circuits under a relation context record skipped
                    // rules whose entity the context supplies.
                    or(vec![
                        derived("hj0"),
                        derived("pj2"),
                        cmp(derived("hc1"), "gt", derived("pc1")),
                    ]),
                ]),
             }},
            {"name": "eligible2", "arity": 2, "slot_entities": ["Person", "Household"],
             "derivation": {
                "source_relation": "eligible", "current_slot": 1, "related_slot": 0,
                "slot_entities": ["Person", "Household"],
                "predicate": cmp(add(vec![derived("pc1"), derived("hc3")]), "gt", int(20)),
             }},
        ],
        "derived": rules,
    });
    let dataset = json!({
        "inputs": [
            input_record("Person", "p1", "age", decimal_value("30")),
            input_record("Person", "p2", "age", decimal_value("5")),
            input_record("Person", "p3", "age", decimal_value("50")),
            input_record("Person", "p4", "age", decimal_value("41")),
            input_record("Household", "h1", "threshold", decimal_value("10")),
            input_record("Household", "h2", "threshold", decimal_value("40")),
        ],
        "relations": [
            relation_record("member_of_household", &["p1", "h1"]),
            relation_record("member_of_household", &["p2", "h1"]),
            relation_record("member_of_household", &["p3", "h1"]),
            relation_record("member_of_household", &["p3", "h2"]),
            relation_record("member_of_household", &["p4", "h2"]),
        ],
    });
    let queries = json!([
        query("h1", 1, &["top", "n_eligible"]),
        query("h2", 1, &["top"]),
        query("p3", 1, &["pc0", "pj0"]),
    ]);
    (program, dataset, queries)
}

/// Rules shared along several paths: a long chain read by two others, each
/// through a chain of its own, then read directly.
fn diamonds() -> (Value, Value, Value) {
    let mut rules = Vec::new();
    rules.extend(chain(
        "s",
        "Household",
        9,
        |index| param("p", int((index % 3) as i64)),
        input_or("x", "1"),
    ));
    rules.extend(chain("a", "Household", 4, |_| derived("s3"), derived("s0")));
    rules.extend(chain("b", "Household", 4, |_| derived("s7"), derived("s2")));
    rules.push(scalar(
        "top",
        "Household",
        add(vec![
            derived("a0"),
            derived("b0"),
            derived("s5"),
            derived("s0"),
            derived("a2"),
            derived("b1"),
        ]),
    ));
    let program = json!({
        "parameters": [{"name": "p", "unit": null, "indexed_by": "k", "versions": [
            {"effective_from": "2026-01-01", "values": {
                "0": decimal_value("1"), "1": decimal_value("2"), "2": decimal_value("3")}}]}],
        "derived": rules,
    });
    let dataset = json!({
        "inputs": [input_record("Household", "h1", "x", decimal_value("4"))],
        "relations": [],
    });
    let queries = json!([query("h1", 1, &["top", "b0"]), query("h2", 1, &["top"])]);
    (program, dataset, queries)
}

/// A household's members each reach a person chain `depth` rules deep, in a
/// `where` clause and in a summed, rounded value, so every member defers.
/// Member `i` has age `i`; the chain adds 1 per rule, and the `where` clause
/// keeps members whose chain value is odd.
fn deferring_members(members: usize, depth: usize) -> (Value, Value, Value) {
    let mut rules = Vec::new();
    rules.extend(chain("q", "Person", depth, |_| int(1), input("age")));
    rules.push(rounded(
        scalar("share", "Person", div(derived("q0"), int(4))),
        "half_up",
    ));
    for index in 0..depth {
        rules.push(judgment(
            &format!("keep{index}"),
            "Person",
            and(vec![
                derived(&format!("keep{}", index + 1)),
                cmp(input("age"), "gte", int(0)),
            ]),
        ));
    }
    rules.push(judgment(
        &format!("keep{depth}"),
        "Person",
        not(cmp(
            mul(
                json!({"kind": "floor", "value": div(derived("q0"), int(2))}),
                int(2),
            ),
            "eq",
            derived("q0"),
        )),
    ));
    rules.push(scalar(
        "kept",
        "Household",
        count_related("member_of_household", 1, 0, Some(derived("keep0"))),
    ));
    rules.push(scalar(
        "kept_share",
        "Household",
        sum_related("member_of_household", 1, 0, "share", Some(derived("keep0"))),
    ));
    rules.push(scalar(
        "total",
        "Household",
        add(vec![
            derived("kept"),
            derived("kept_share"),
            sum_related("member_of_household", 1, 0, "q0", None),
        ]),
    ));
    let program = json!({
        "units": [{"name": "USD", "kind": "currency", "minor_units": 0}],
        "relations": [members_relation()],
        "derived": rules,
    });
    let mut inputs = Vec::with_capacity(members);
    let mut relations = Vec::with_capacity(members);
    for member in 0..members {
        let id = format!("p{member}");
        inputs.push(input_record(
            "Person",
            &id,
            "age",
            decimal_value(&member.to_string()),
        ));
        relations.push(relation_record("member_of_household", &[&id, "h1"]));
    }
    let dataset = json!({"inputs": inputs, "relations": relations});
    let queries = json!([query("h1", 1, &["total", "kept"])]);
    (program, dataset, queries)
}

/// Two derived relations whose predicates each count, or sum over, a
/// person's kids with a `where` clause reading a chain `depth` rules deep, and
/// a household rule counting both. Each predicate's aggregation defers inside
/// the other's evaluation order, so each resumes while the other is still
/// pending: the resume must name the aggregation it belongs to, not whichever
/// aggregation later occupies the same place in memory. (Resumes were keyed
/// by address, and the predicate was evaluated from a per-call copy, so the
/// second relation's count took over the first's progress and `top` came out
/// 0 instead of 1 in explain at thresholds 1 to 8.)
fn aggregations_in_predicates(depth: usize, sum: bool) -> (Value, Value, Value) {
    let mut rules = Vec::new();
    rules.extend(chain("ka", "Kid", depth, |_| int(1), input("ka_in")));
    rules.extend(chain("kb", "Kid", depth, |_| int(1), input("kb_in")));
    rules.push(judgment(
        "ka_ok",
        "Kid",
        cmp(derived("ka0"), "gt", int(100)),
    ));
    rules.push(judgment(
        "kb_ok",
        "Kid",
        cmp(derived("kb0"), "gt", int(100)),
    ));
    let aggregate = |kids: &str, ok: &str, value: &str| {
        if sum {
            sum_related(kids, 1, 0, value, Some(derived(ok)))
        } else {
            count_related(kids, 1, 0, Some(derived(ok)))
        }
    };
    rules.push(scalar(
        "top",
        "Household",
        add(vec![
            count_related("fa", 1, 0, None),
            count_related("fb", 1, 0, None),
        ]),
    ));
    rules.push(scalar("n_fa", "Household", count_related("fa", 1, 0, None)));
    rules.push(scalar("n_fb", "Household", count_related("fb", 1, 0, None)));
    let filtered = |name: &str, predicate: Value| {
        json!({
            "name": name, "arity": 2, "slot_entities": ["Person", "Household"],
            "derivation": {
                "source_relation": "member_of_household", "current_slot": 1,
                "related_slot": 0, "slot_entities": ["Person", "Household"],
                "predicate": predicate,
            },
        })
    };
    let program = json!({
        "relations": [
            members_relation(),
            {"name": "kid_a", "arity": 2, "slot_entities": ["Kid", "Person"]},
            {"name": "kid_b", "arity": 2, "slot_entities": ["Kid", "Person"]},
            filtered("fa", cmp(aggregate("kid_a", "ka_ok", "ka0"), "gte", int(1))),
            filtered("fb", cmp(aggregate("kid_b", "kb_ok", "kb0"), "gte", int(1))),
        ],
        "derived": rules,
    });
    let mut inputs = Vec::new();
    for (kid, a, b) in [
        ("k1", "200", "200"),
        ("k2", "200", "200"),
        ("k3", "0", "200"),
        ("k4", "0", "0"),
    ] {
        inputs.push(input_record("Kid", kid, "ka_in", decimal_value(a)));
        inputs.push(input_record("Kid", kid, "kb_in", decimal_value(b)));
    }
    let dataset = json!({
        "inputs": inputs,
        "relations": [
            relation_record("member_of_household", &["p1", "h1"]),
            relation_record("member_of_household", &["p2", "h1"]),
            relation_record("kid_a", &["k1", "p1"]),
            relation_record("kid_a", &["k2", "p1"]),
            relation_record("kid_a", &["k3", "p2"]),
            relation_record("kid_b", &["k4", "p1"]),
        ],
    });
    let queries = json!([query("h1", 1, &["top"]), query("h1", 1, &["n_fb", "n_fa"]),]);
    (program, dataset, queries)
}

/// Errors inside deferred rules, and laziness around them.
fn error_programs() -> Vec<(&'static str, Value, Value, Value)> {
    let mut programs = Vec::new();
    let empty = json!({"inputs": [], "relations": []});
    let x3 = json!({
        "inputs": [input_record("Household", "h1", "x", decimal_value("3"))],
        "relations": [],
    });

    // A match fails deep inside a deferred chain of rules with ids: the error
    // names the innermost rule.
    let mut rules: Vec<Value> = chain(
        "c",
        "Household",
        6,
        |_| int(1),
        if_then(
            cmp(input("x"), "eq", int(1)),
            int(10),
            no_match(input("x"), vec![int(1)]),
        ),
    )
    .into_iter()
    .enumerate()
    .map(|(index, rule)| with_id(rule, &format!("us/c{index}")))
    .collect();
    rules.push(with_id(
        scalar("top", "Household", add(vec![derived("c0"), int(1)])),
        "us/top",
    ));
    programs.push((
        "match fails deep",
        json!({"derived": rules}),
        x3.clone(),
        json!([query("h1", 1, &["us/top"])]),
    ));

    // The root rule's match fails on a deep chain's value.
    let mut rules = chain("c", "Household", 6, |_| int(1), input("x"));
    rules.push(with_id(
        scalar("top", "Household", no_match(derived("c0"), vec![int(1)])),
        "us/top",
    ));
    rules.push(scalar(
        "wrap",
        "Household",
        add(vec![derived("top"), int(0)]),
    ));
    programs.push((
        "match fails at the root",
        json!({"derived": rules}),
        x3.clone(),
        json!([query("h1", 1, &["wrap"])]),
    ));

    // A missing input at the bottom of a deferred chain.
    let mut rules = chain("c", "Household", 6, |_| int(1), input("x"));
    rules.push(scalar("top", "Household", add(vec![derived("c0"), int(1)])));
    programs.push((
        "missing input deep",
        json!({"derived": rules}),
        empty.clone(),
        json!([query("h1", 1, &["top"])]),
    ));

    // Which error wins: a deep division by zero read first, or a shallow
    // missing input; and an error after earlier queries cached their rules.
    let mut rules = chain("a", "Household", 6, |_| int(1), div(int(1), int(0)));
    rules.extend(chain("ok", "Household", 6, |_| int(1), int(0)));
    rules.push(scalar("b", "Household", input("x")));
    rules.push(scalar(
        "top",
        "Household",
        add(vec![derived("a0"), derived("b")]),
    ));
    rules.push(scalar(
        "top2",
        "Household",
        add(vec![derived("ok0"), derived("b")]),
    ));
    rules.push(scalar(
        "top3",
        "Household",
        div(derived("a0"), derived("b")),
    ));
    let program = json!({"derived": rules});
    for (label, queries) in [
        ("deep error first", json!([query("h1", 1, &["top"])])),
        ("shallow error second", json!([query("h1", 1, &["top2"])])),
        (
            "division reads its divisor first",
            json!([query("h1", 1, &["top3"])]),
        ),
        (
            "error after a cached query",
            json!([query("h1", 1, &["ok0"]), query("h1", 1, &["top2"])]),
        ),
    ] {
        programs.push((label, program.clone(), empty.clone(), queries));
    }

    // Failing chains in branches and `where` clauses nothing selects are never
    // evaluated.
    let mut rules = chain("e", "Household", 6, |_| int(1), div(int(1), int(0)));
    rules.extend(chain("ep", "Person", 6, |_| int(1), input("nope")));
    rules.push(judgment(
        "ej",
        "Household",
        cmp(derived("e0"), "gt", int(0)),
    ));
    rules.push(judgment("epj", "Person", cmp(derived("ep0"), "gt", int(0))));
    rules.push(scalar(
        "top",
        "Household",
        add(vec![
            if_then(cmp(int(1), "eq", int(0)), derived("e0"), int(1)),
            if_then(
                or(vec![cmp(int(1), "eq", int(1)), derived("ej")]),
                int(2),
                derived("e0"),
            ),
            if_then(
                and(vec![cmp(int(1), "eq", int(0)), derived("ej")]),
                derived("e0"),
                int(3),
            ),
            count_related(
                "member_of_household",
                1,
                0,
                Some(and(vec![cmp(int(1), "eq", int(0)), derived("epj")])),
            ),
            count_related("empty_rel", 1, 0, Some(derived("epj"))),
            sum_related("empty_rel", 1, 0, "ep0", None),
        ]),
    ));
    programs.push((
        "laziness",
        json!({
            "relations": [
                members_relation(),
                {"name": "empty_rel", "arity": 2, "slot_entities": ["Person", "Household"]},
            ],
            "derived": rules,
        }),
        json!({"inputs": [], "relations": [relation_record("member_of_household", &["p1", "h1"])]}),
        json!([query("h1", 1, &["top"])]),
    ));

    // Arithmetic overflow at the bottom of a deferred chain.
    let mut rules = chain(
        "c",
        "Household",
        6,
        |_| int(1),
        mul(dec("79228162514264337593543950335"), int(10)),
    );
    rules.push(scalar("top", "Household", add(vec![derived("c0"), int(1)])));
    programs.push((
        "overflow deep",
        json!({"derived": rules}),
        empty.clone(),
        json!([query("h1", 1, &["top"])]),
    ));

    // A match fails in a derived relation's predicate, inside a count a deep
    // chain reads: the counting rule is the innermost rule.
    let mut rules = chain("pc", "Person", 5, |_| int(1), input("age"));
    rules.push(with_id(
        scalar("cnt", "Household", count_related("odd", 1, 0, None)),
        "us/cnt",
    ));
    rules.extend(chain("w", "Household", 5, |_| int(0), derived("cnt")));
    programs.push((
        "match fails in a relation predicate",
        json!({
            "relations": [
                members_relation(),
                {"name": "odd", "arity": 2, "slot_entities": ["Person", "Household"],
                 "derivation": {
                    "source_relation": "member_of_household", "current_slot": 1,
                    "related_slot": 0, "slot_entities": ["Person", "Household"],
                    "predicate": cmp(no_match(derived("pc0"), vec![int(1)]), "gt", int(0)),
                 }},
            ],
            "derived": rules,
        }),
        json!({
            "inputs": [input_record("Person", "p1", "age", decimal_value("30"))],
            "relations": [relation_record("member_of_household", &["p1", "h1"])],
        }),
        json!([query("h1", 1, &["w0"])]),
    ));

    // A judgment chain whose last rule's comparison reads a failing match.
    let mut rules = Vec::new();
    for index in 0..6 {
        rules.push(with_id(
            judgment(
                &format!("j{index}"),
                "Household",
                and(vec![
                    cmp(int(1), "eq", int(1)),
                    derived(&format!("j{}", index + 1)),
                ]),
            ),
            &format!("us/j{index}"),
        ));
    }
    rules.push(with_id(
        judgment(
            "j6",
            "Household",
            cmp(no_match(input("x"), vec![int(1), int(2)]), "eq", int(1)),
        ),
        "us/j6",
    ));
    programs.push((
        "judgment match fails deep",
        json!({"derived": rules}),
        x3,
        json!([query("h1", 1, &["us/j0"])]),
    ));
    programs
}

/// Handcrafted programs answer byte for byte the same in explain and fast at
/// every threshold as without deferral: values, errors and explain's full
/// trace (dependencies and skipped dependencies in first-occurrence order,
/// parameter reads, pre-rounding values).
#[test]
fn handcrafted_programs_answer_the_same_at_every_threshold() {
    let divergences = on_stack(512 * MIB, || {
        let mut divergences = Vec::new();
        for (label, (program, dataset, queries)) in [
            ("trace-rich", trace_rich()),
            ("relation contexts", relation_contexts()),
            ("diamonds", diamonds()),
            ("deferring members", deferring_members(12, 6)),
            ("counts in predicates", aggregations_in_predicates(3, false)),
            ("sums in predicates", aggregations_in_predicates(3, true)),
            (
                "deep counts in predicates",
                aggregations_in_predicates(12, false),
            ),
        ] {
            divergences.extend(self::divergences(label, &program, &dataset, &queries));
        }
        for (label, program, dataset, queries) in error_programs() {
            divergences.extend(self::divergences(label, &program, &dataset, &queries));
        }
        divergences
    });
    assert!(divergences.is_empty(), "{}", divergences.join("\n"));
}

fn model_period() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn to_model(program: &Value, dataset: &Value) -> (Program, DataSet) {
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain", "program": program, "dataset": dataset, "queries": [],
    }))
    .expect("request deserializes");
    let program = request.program.to_program().expect("program converts");
    let data = request
        .dataset
        .to_dataset_for_program(&program)
        .expect("dataset converts");
    (program, data)
}

/// Calls on one engine, kept after errors the way fast's embedded engine is,
/// then every value, pre-rounding value and judgment the engine cached.
fn engine_session(
    program: &Value,
    dataset: &Value,
    calls: &[(&str, &str, bool)],
    entities: &[&str],
) -> String {
    let (program, data) = to_model(program, dataset);
    let period = model_period();
    let mut engine = Engine::new(&program, &data);
    let mut log = Vec::new();
    for (rule, entity, is_judgment) in calls {
        let result = if *is_judgment {
            format!("{:?}", engine.evaluate_judgment(rule, entity, &period))
        } else {
            format!("{:?}", engine.evaluate_scalar(rule, entity, &period))
        };
        log.push(format!("{rule}({entity}) = {result}"));
    }
    let mut cache = BTreeMap::new();
    for name in program.derived.keys() {
        for entity in entities {
            if let Some(value) = engine.cached_scalar(name, entity, &period) {
                cache.insert(format!("scalar {name}({entity})"), format!("{value:?}"));
            }
            if let Some(value) = engine.cached_pre_rounding(name, entity, &period) {
                cache.insert(
                    format!("pre-rounding {name}({entity})"),
                    format!("{value:?}"),
                );
            }
            if let Some(value) = engine.cached_judgment(name, entity, &period) {
                cache.insert(format!("judgment {name}({entity})"), format!("{value:?}"));
            }
        }
    }
    format!("{log:#?}\n{cache:#?}")
}

/// One engine answers a sequence of calls, some failing, the same at every
/// threshold, and caches the same values: a failed rule is never cached, and
/// a rounded rule's pre-rounding value is kept whichever driver task computed
/// it. A rule that reads itself for other entities (a hand-built program
/// over a tree of people, which `execute_request` refuses) is not mistaken
/// for a cycle: deferral is keyed by rule, entity and period.
#[test]
fn a_reused_engine_caches_the_same_at_every_threshold() {
    let divergences = on_stack(512 * MIB, || {
        let mut divergences = Vec::new();
        let mut rules = chain("ok", "Household", 6, |_| int(1), int(0));
        rules.extend(chain("f", "Household", 6, |_| int(1), input("x")));
        rules.push(rounded(
            scalar(
                "rounded",
                "Household",
                div(add(vec![derived("ok0"), dec("0.5")]), int(4)),
            ),
            "half_up",
        ));
        rules.push(scalar(
            "a",
            "Household",
            add(vec![derived("ok0"), derived("rounded"), derived("f0")]),
        ));
        rules.push(scalar(
            "b",
            "Household",
            add(vec![derived("ok2"), derived("f2"), derived("ok0")]),
        ));
        rules.push(scalar(
            "c",
            "Household",
            add(vec![derived("ok1"), derived("rounded")]),
        ));
        rules.push(judgment("jb", "Household", cmp(derived("b"), "gt", int(0))));
        let program = json!({
            "units": [{"name": "USD", "kind": "currency", "minor_units": 0}],
            "derived": rules,
        });
        let dataset = json!({
            "inputs": [input_record("Household", "h2", "x", decimal_value("1"))],
            "relations": [],
        });
        let calls = [
            ("a", "h1", false),
            ("b", "h1", false),
            ("jb", "h1", true),
            ("c", "h1", false),
            ("a", "h2", false),
            ("b", "h1", false),
        ];
        let session = || engine_session(&program, &dataset, &calls, &["h1", "h2"]);
        let recursive = with_suspend_depth(usize::MAX, session);
        for threshold in THRESHOLDS {
            let deferred = with_suspend_depth(threshold, session);
            if deferred != recursive {
                divergences.push(format!(
                    "reused engine at threshold {threshold}:\nrecursive {recursive}\ndeferred {deferred}"
                ));
            }
        }

        let rules = vec![
            scalar(
                "depth_below",
                "Person",
                add(vec![
                    sum_related("parent_of", 0, 1, "depth_below", None),
                    int(1),
                ]),
            ),
            judgment("deep", "Person", cmp(derived("depth_below"), "gt", int(0))),
        ];
        let program = json!({
            "relations": [{"name": "parent_of", "arity": 2, "slot_entities": ["Person", "Person"]}],
            "derived": rules,
        });
        let people: Vec<String> = (0..12).map(|index| format!("p{index}")).collect();
        let mut relations = Vec::new();
        for index in 0..11 {
            relations.push(relation_record(
                "parent_of",
                &[&people[index], &people[index + 1]],
            ));
        }
        relations.push(relation_record("parent_of", &["p0", "p5"]));
        let dataset = json!({"inputs": [], "relations": relations});
        let entities: Vec<&str> = people.iter().map(String::as_str).collect();
        let calls = [("depth_below", "p0", false), ("deep", "p3", true)];
        let session = || engine_session(&program, &dataset, &calls, &entities);
        let recursive = with_suspend_depth(usize::MAX, session);
        assert!(
            !recursive.contains("DependencyCycle"),
            "the tree is acyclic: {recursive}"
        );
        for threshold in THRESHOLDS {
            let deferred = with_suspend_depth(threshold, session);
            if deferred != recursive {
                divergences.push(format!(
                    "self-reading rule at threshold {threshold}:\nrecursive {recursive}\ndeferred {deferred}"
                ));
            }
        }
        divergences
    });
    assert!(divergences.is_empty(), "{}", divergences.join("\n"));
}

/// A `count` and two `sum`s over 1,000 members, each member reaching person
/// chains deep enough to defer, in the `where` clause and in the summed
/// values. Every member defers, and each deferral retries the household's
/// rule, but the retry resumes the aggregation at the member that deferred.
/// So the evaluation visits a constant multiple of the nodes recursion visits
/// (1.6 times here). Were each retry to evaluate the earlier members again,
/// the work would grow with the square of the member count: 7.1 times
/// recursion's here, measured when this was fixed.
#[test]
fn an_aggregation_whose_members_each_defer_costs_one_pass() {
    const MEMBERS: usize = 1_000;
    let (program, dataset, queries) = deferring_members(MEMBERS, 12);
    on_stack(512 * MIB, move || {
        for mode in ["explain", "fast"] {
            let (recursive, recursive_visits) = with_suspend_depth(usize::MAX, || {
                count_visits(|| run(mode, &program, &dataset, &queries))
            });
            let (deferred, deferred_visits) = with_suspend_depth(8, || {
                count_visits(|| run(mode, &program, &dataset, &queries))
            });
            assert_eq!(deferred, recursive, "{mode}: deferral changed the response");
            assert!(
                !deferred.starts_with("Err") && deferred.contains(&format!("\"{mode}\"")),
                "{mode}: {}",
                &deferred[..deferred.len().min(300)]
            );
            assert!(
                deferred_visits <= 3 * recursive_visits,
                "{mode}: deferral visited {deferred_visits} nodes, recursion {recursive_visits}"
            );
        }
    });
}

/// Dense's answer for `r0` of an `add` chain of `n` rules, and the nodes the
/// compiler and executor visited.
fn dense_add_chain(n: usize) -> (String, usize) {
    let rules = chain("r", "Household", n, |_| int(1), int(0));
    let spec: ProgramSpec =
        serde_json::from_value(json!({"derived": rules})).expect("program deserializes");
    let program = spec.to_program().expect("program converts");
    count_visits(|| {
        let dense =
            DenseCompiledProgram::from_program(&program, Some("Household")).expect("compiles");
        let batch = DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::new(),
        };
        let result = dense
            .execute(&model_period(), batch, &["r0".to_string()])
            .expect("executes");
        format!("{:?}", result.outputs["r0"])
    })
}

/// A 20,000-rule `add` chain answers in explain, fast and dense on a 1 MiB
/// thread (stricter than the 2 MiB of a spawned Rust thread), and costs about
/// four times a 5,000-rule chain in each: nodes visited grow linearly with
/// the chain, retries included. The wall-clock bound is deliberately loose;
/// the visit counts carry the linearity claim.
#[test]
fn a_long_chain_costs_linear_work_in_every_mode_on_a_small_stack() {
    const SHORT: usize = 5_000;
    const LONG: usize = 20_000;
    let started = Instant::now();
    on_stack(MIB, || {
        let sparse = |mode: &str, n: usize| {
            let program = json!({"derived": chain("r", "Household", n, |_| int(1), int(0))});
            count_visits(|| {
                run(
                    mode,
                    &program,
                    &json!({"inputs": [], "relations": []}),
                    &json!([query("h1", 1, &["r0"])]),
                )
            })
        };
        for mode in ["explain", "fast"] {
            let (short, short_visits) = sparse(mode, SHORT);
            let (long, long_visits) = sparse(mode, LONG);
            assert!(
                long.contains(&format!("\"value\":\"{LONG}\""))
                    && long.contains(&format!("\"actual_mode\":\"{mode}\"")),
                "{mode}: {}",
                &long[..long.len().min(300)]
            );
            assert!(short.contains(&format!("\"value\":\"{SHORT}\"")), "{mode}");
            assert!(
                long_visits * 10 <= short_visits * 44,
                "{mode}: {LONG} rules visited {long_visits} nodes, {SHORT} rules {short_visits}"
            );
        }
        let (short, short_visits) = dense_add_chain(SHORT);
        let (long, long_visits) = dense_add_chain(LONG);
        assert_eq!(short, format!("Scalar(Decimal([{SHORT}]))"));
        assert_eq!(long, format!("Scalar(Decimal([{LONG}]))"));
        assert!(
            long_visits * 10 <= short_visits * 44,
            "dense: {LONG} rules visited {long_visits} nodes, {SHORT} rules {short_visits}"
        );
    });
    assert!(
        started.elapsed() < Duration::from_secs(600),
        "took {:?}",
        started.elapsed()
    );
}

fn tax_years() -> Vec<Period> {
    (2020..2023)
        .map(|year| Period {
            kind: PeriodKind::TaxYear,
            start: chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
            end: chrono::NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
        })
        .collect()
}

/// The lifetime gate accepts an output exactly when its formula, or a rule
/// it reads at any depth through scalars or judgments, reduces over periods.
/// A `count` or `sum` over related entities is not a reduction over periods.
/// Requested outputs are checked in order, and the first unknown or
/// non-reducing one is the error. A diamond of rules 40 levels deep (each rule
/// reading the one below twice) is decided in one pass; walking every path
/// would take 2^41 steps.
#[test]
fn the_lifetime_gate_accepts_exactly_the_outputs_that_reduce_over_periods() {
    const DIAMOND: usize = 40;
    let mut rules = vec![
        scalar("red", "Household", over_periods(input("x"))),
        scalar("via", "Household", add(vec![derived("red"), int(1)])),
        scalar("via2", "Household", derived("via")),
        scalar("plain", "Household", add(vec![input("x"), int(1)])),
        scalar(
            "cnt",
            "Household",
            count_related("member_of_household", 1, 0, None),
        ),
        scalar(
            "sm",
            "Household",
            sum_related("member_of_household", 1, 0, "income", None),
        ),
        scalar("cnt_plus", "Household", add(vec![derived("cnt"), int(1)])),
        scalar(
            "mixed",
            "Household",
            add(vec![derived("cnt"), derived("red")]),
        ),
        judgment(
            "jred",
            "Household",
            cmp(over_periods(input("x")), "gt", int(0)),
        ),
        judgment(
            "jvia",
            "Household",
            and(vec![derived("jred"), not(derived("jcnt"))]),
        ),
        judgment("jcnt", "Household", cmp(derived("cnt"), "gt", int(0))),
        scalar(
            "if_jvia",
            "Household",
            if_then(derived("jvia"), int(1), int(0)),
        ),
        scalar(
            "if_jcnt",
            "Household",
            if_then(derived("jcnt"), int(1), int(0)),
        ),
        scalar("income", "Person", input("income")),
        scalar("d0", "Household", input("x")),
    ];
    for level in 1..=DIAMOND {
        let below = derived(&format!("d{}", level - 1));
        rules.push(scalar(
            &format!("d{level}"),
            "Household",
            add(vec![below.clone(), below]),
        ));
    }
    rules.push(scalar(
        "diamond_red",
        "Household",
        add(vec![derived(&format!("d{DIAMOND}")), derived("red")]),
    ));
    rules.push(scalar(
        "diamond",
        "Household",
        derived(&format!("d{DIAMOND}")),
    ));
    let spec: ProgramSpec = serde_json::from_value(json!({
        "relations": [members_relation()],
        "derived": rules,
    }))
    .expect("program deserializes");
    let program = spec.to_program().expect("program converts");
    let batches = || {
        [1, 2, 3]
            .into_iter()
            .map(|x| DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([("x".to_string(), DenseColumn::Integer(vec![x]))]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: vec![0, 2],
                        inputs: HashMap::from([(
                            "income".to_string(),
                            DenseColumn::Integer(vec![10, 20]),
                        )]),
                    },
                )]),
            })
            .collect::<Vec<_>>()
    };
    // The gate's error, or "passes" for any other outcome.
    let gate = |outputs: &[&str]| -> String {
        let dense =
            DenseCompiledProgram::from_program(&program, Some("Household")).expect("compiles");
        let outputs: Vec<String> = outputs.iter().map(|name| (*name).to_string()).collect();
        match dense.execute_lifetime(&tax_years(), batches(), &outputs) {
            Err(
                error @ (EvalError::LifetimeOutputWithoutReduction(_)
                | EvalError::UnknownDerived(_)),
            ) => format!("{error:?}"),
            _ => "passes".to_string(),
        }
    };
    let without_reduction = |output: &str| {
        format!(
            "{:?}",
            EvalError::LifetimeOutputWithoutReduction(output.to_string())
        )
    };
    // Without deferral a debug build compiles the 40-level diamond on the
    // stack, so the comparison runs on a large one.
    on_stack(512 * MIB, || {
        for threshold in [1, 8, usize::MAX] {
            with_suspend_depth(threshold, || {
                for output in [
                    "red",
                    "via",
                    "via2",
                    "mixed",
                    "jred",
                    "jvia",
                    "if_jvia",
                    "diamond_red",
                ] {
                    assert_eq!(gate(&[output]), "passes", "{output} reduces over periods");
                }
                for output in [
                    "plain", "cnt", "sm", "cnt_plus", "jcnt", "if_jcnt", "diamond",
                ] {
                    assert_eq!(
                        gate(&[output]),
                        without_reduction(output),
                        "{output} does not reduce over periods"
                    );
                }
                assert_eq!(gate(&["via", "cnt", "plain"]), without_reduction("cnt"));
                assert_eq!(
                    gate(&["missing", "plain"]),
                    format!("{:?}", EvalError::UnknownDerived("missing".to_string()))
                );
                assert_eq!(gate(&["plain", "missing"]), without_reduction("plain"));
            });
        }
    });
}
