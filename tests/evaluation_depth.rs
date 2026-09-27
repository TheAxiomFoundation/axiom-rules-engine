//! Deep programs answer in every mode, on a small stack (#206).
//!
//! Explain, fast and dense used to recurse through every rule a formula
//! references, so a long enough chain of valid, acyclic rules aborted the
//! process with a stack overflow in one mode while another mode answered.
//! Each evaluator (and dense's compiler) now defers a rule it has not
//! evaluated to its driver once it is `depth::with_suspend_depth` levels deep
//! (see `src/depth.rs`), so stack use is bounded whatever the chain's length.
//!
//! These tests pin that:
//!
//! * around explicit thresholds, every chain shape answers the same in
//!   explain, fast and dense, and the same as with deferral off;
//! * a 20,000-rule chain answers in every mode on a 1 MiB thread;
//! * the three requests that reproduced #206 answer in both modes;
//! * a hand-built cyclic program is an error, not an overflow or a hang;
//! * dense declines, as a compile error, a relation aggregation that inlines
//!   rules deeper than `dense::MAX_INLINE_DEPTH`, and compiles one at it.
//!
//! `tests/execution_mode_parity.rs` checks the general property: on random
//! programs, deferral at every rule reference changes no response, trace or
//! error in any mode.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionRequest, ExecutionResponse, execute_request};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    DenseRelationKey, MAX_INLINE_DEPTH,
};
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::engine::{Engine, EvalError};
use axiom_rules_engine::model::{DataSet, Period, PeriodKind};
use axiom_rules_engine::spec::ProgramSpec;
use rust_decimal::Decimal;
use serde_json::{Value, json};

const MIB: usize = 1024 * 1024;

/// Run `f` on a fresh thread with a `bytes` stack. The deferral threshold is
/// per thread, so `f` sets its own.
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

fn literal(value: Value) -> Value {
    json!({"kind": "literal", "value": value})
}

fn int(value: i64) -> Value {
    literal(json!({"kind": "integer", "value": value}))
}

fn derived(name: &str) -> Value {
    json!({"kind": "derived", "name": name})
}

fn always() -> Value {
    json!({"kind": "comparison", "left": int(0), "op": "eq", "right": int(0)})
}

fn rule(name: &str, entity: &str, dtype: &str, semantics: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": dtype, "unit": null,
        "semantics": semantics, "expr": expr,
    })
}

fn household(name: &str, dtype: &str, expr: Value) -> Value {
    rule(name, "Household", dtype, "scalar", expr)
}

/// A chain shape: `n` rules, `r0` referring to `r1` and so on, the last one a
/// literal. Each shape's answer does not depend on `n`.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `r_i = r_{i+1}`.
    Plain,
    /// `r_i = r_{i+1} + 1`: the answer counts the hops.
    Add,
    /// `r_i = if 0 == 0 then r_{i+1} else 0`.
    If,
    /// `r_i = and(r_{i+1}, 0 == 0)`, judgments.
    And,
    /// `r_i = ceil(ceil(... r_{i+1} ...))`, six levels of nesting per rule.
    Nested,
}

const SHAPES: [Shape; 5] = [
    Shape::Plain,
    Shape::Add,
    Shape::If,
    Shape::And,
    Shape::Nested,
];

impl Shape {
    fn rules(self, n: usize) -> Vec<Value> {
        let name = |index: usize| format!("r{index}");
        let mut rules = Vec::with_capacity(n + 1);
        for index in 0..n {
            let next = derived(&name(index + 1));
            rules.push(match self {
                Shape::Plain => household(&name(index), "integer", next),
                Shape::Add => household(
                    &name(index),
                    "decimal",
                    json!({"kind": "add", "items": [next, int(1)]}),
                ),
                Shape::If => household(
                    &name(index),
                    "integer",
                    json!({"kind": "if", "condition": always(), "then_expr": next, "else_expr": int(0)}),
                ),
                Shape::And => rule(
                    &name(index),
                    "Household",
                    "judgment",
                    "judgment",
                    json!({"kind": "and", "items": [next, always()]}),
                ),
                Shape::Nested => {
                    let mut expr = next;
                    for _ in 0..6 {
                        expr = json!({"kind": "ceil", "value": expr});
                    }
                    household(&name(index), "decimal", expr)
                }
            });
        }
        rules.push(match self {
            Shape::Plain | Shape::If => household(&name(n), "integer", int(2)),
            Shape::Add => household(
                &name(n),
                "decimal",
                literal(json!({"kind": "decimal", "value": "0"})),
            ),
            Shape::And => rule(&name(n), "Household", "judgment", "judgment", always()),
            Shape::Nested => household(
                &name(n),
                "decimal",
                literal(json!({"kind": "decimal", "value": "1.5"})),
            ),
        });
        rules
    }

    /// `r0`'s answer, as the JSON output value explain and fast return.
    fn expected(self, n: usize) -> Value {
        match self {
            Shape::Plain | Shape::If => json!({"kind": "integer", "value": 2}),
            Shape::Add => json!({"kind": "decimal", "value": n.to_string()}),
            Shape::And => json!("holds"),
            Shape::Nested if n == 0 => json!({"kind": "decimal", "value": "1.5"}),
            Shape::Nested => json!({"kind": "decimal", "value": "2"}),
        }
    }
}

fn period() -> Value {
    json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"})
}

fn model_period() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn request(mode: &str, program: Value, dataset: Value, queries: Value) -> ExecutionRequest {
    serde_json::from_value(json!({
        "mode": mode, "program": program, "dataset": dataset, "queries": queries,
    }))
    .expect("request deserializes")
}

fn query(entity_id: &str, outputs: &[&str]) -> Value {
    json!({"entity_id": entity_id, "period": period(), "outputs": outputs})
}

fn empty_dataset() -> Value {
    json!({"inputs": [], "relations": []})
}

/// Each query's first output value, and whether a fast request stayed fast.
fn answers(response: &ExecutionResponse) -> (Vec<Value>, bool) {
    let values = response
        .results
        .iter()
        .map(|result| {
            let output = serde_json::to_value(result.outputs.values().next().expect("one output"))
                .expect("output serializes");
            output
                .get("value")
                .or_else(|| output.get("outcome"))
                .cloned()
                .expect("a value or an outcome")
        })
        .collect();
    let stayed_fast = serde_json::to_value(&response.metadata).expect("metadata serializes")["actual_mode"]
        == json!("fast");
    (values, stayed_fast)
}

fn run_chain(shape: Shape, n: usize, mode: &str) -> (Vec<Value>, bool) {
    let program = json!({"derived": shape.rules(n)});
    let response = execute_request(request(
        mode,
        program,
        empty_dataset(),
        json!([query("h1", &["r0"])]),
    ))
    .unwrap_or_else(|error| panic!("{shape:?} chain of {n} in {mode}: {error}"));
    answers(&response)
}

/// Dense's value for `r0` over a one-row batch, as the JSON explain returns
/// (dense returns a numeric answer as a decimal; see its dtype contract).
fn dense_chain(shape: Shape, n: usize) -> Value {
    let spec: ProgramSpec =
        serde_json::from_value(json!({"derived": shape.rules(n)})).expect("program deserializes");
    let program = spec.to_program().expect("program converts");
    let dense = DenseCompiledProgram::from_program(&program, Some("Household"))
        .unwrap_or_else(|error| panic!("{shape:?} chain of {n}: dense compile: {error}"));
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::new(),
    };
    let result = dense
        .execute(&model_period(), batch, &["r0".to_string()])
        .unwrap_or_else(|error| panic!("{shape:?} chain of {n}: dense execute: {error}"));
    match &result.outputs["r0"] {
        DenseOutputValue::Judgment(values) => {
            json!(
                format!("{:?}", values[0])
                    .to_lowercase()
                    .replace("nothold", "not_hold")
            )
        }
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
            let value = values[0].normalize();
            if value.fract().is_zero() && matches!(shape, Shape::Plain | Shape::If) {
                json!({"kind": "integer", "value": value.to_string().parse::<i64>().expect("integer")})
            } else {
                json!({"kind": "decimal", "value": value.to_string()})
            }
        }
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
            json!({"kind": "integer", "value": values[0]})
        }
        other => panic!("{shape:?} chain of {n}: unexpected dense column {other:?}"),
    }
}

/// Just below, at and above an explicit threshold, and twice past it, every
/// chain shape answers the same in explain, fast and dense, and the same as
/// with deferral off. A plain chain's `r_i` sits about `2 i` levels deep, so
/// `0..=2 T + 4` rules cross the threshold for every shape. The run with
/// deferral off recurses through the whole chain, which needs a large stack
/// in a debug build; that is the bug deferral fixes.
#[test]
fn every_mode_answers_chains_around_the_deferral_threshold() {
    on_stack(64 * MIB, chains_around_the_deferral_threshold);
}

fn chains_around_the_deferral_threshold() {
    for threshold in [4, 16] {
        for shape in SHAPES {
            for n in 0..=(2 * threshold + 4) {
                let expected = shape.expected(n);
                let recursive = with_suspend_depth(usize::MAX, || {
                    (
                        run_chain(shape, n, "explain"),
                        run_chain(shape, n, "fast"),
                        dense_chain(shape, n),
                    )
                });
                let deferred = with_suspend_depth(threshold, || {
                    (
                        run_chain(shape, n, "explain"),
                        run_chain(shape, n, "fast"),
                        dense_chain(shape, n),
                    )
                });
                let context = format!("{shape:?} chain of {n} at threshold {threshold}");
                assert_eq!(deferred, recursive, "{context}: deferral changed an answer");
                let ((explain, _), (fast, stayed_fast), dense) = deferred;
                assert_eq!(explain, vec![expected.clone()], "{context}: explain");
                assert_eq!(fast, vec![expected.clone()], "{context}: fast");
                assert!(stayed_fast, "{context}: fast fell back to explain");
                assert_eq!(dense, expected, "{context}: dense");
            }
        }
    }
}

/// A chain far longer than any threshold answers in every mode at the default
/// threshold on a 1 MiB thread: deferral bounds stack use by the threshold
/// and one rule's nesting, not by the chain's length. Before #206 was fixed,
/// release explain overflowed an 8 MiB stack at about 6,900 rules.
#[test]
fn every_mode_answers_a_20000_rule_chain_on_a_1_mib_stack() {
    const RULES: usize = 20_000;
    for shape in [Shape::Plain, Shape::Add, Shape::If, Shape::And] {
        on_stack(MIB, move || {
            let expected = shape.expected(RULES);
            assert_eq!(
                run_chain(shape, RULES, "explain").0,
                vec![expected.clone()],
                "{shape:?} explain"
            );
            let (fast, stayed_fast) = run_chain(shape, RULES, "fast");
            assert_eq!(fast, vec![expected.clone()], "{shape:?} fast");
            assert!(stayed_fast, "{shape:?}: fast fell back to explain");
            assert_eq!(dense_chain(shape, RULES), expected, "{shape:?} dense");
        });
    }
}

/// Rules that each nest their reference deep inside the formula: deferral
/// happens only at rule references, so a segment holds the threshold plus one
/// rule's nesting. Debug frames are about thirty times release frames, so
/// this runs on 8 MiB, the size of a main thread.
#[test]
fn every_mode_answers_a_long_chain_of_nested_rules() {
    const RULES: usize = 3_000;
    on_stack(8 * MIB, || {
        let expected = Shape::Nested.expected(RULES);
        assert_eq!(
            run_chain(Shape::Nested, RULES, "explain").0,
            vec![expected.clone()]
        );
        let (fast, stayed_fast) = run_chain(Shape::Nested, RULES, "fast");
        assert_eq!(fast, vec![expected.clone()]);
        assert!(stayed_fast, "fast fell back to explain");
        assert_eq!(dense_chain(Shape::Nested, RULES), expected);
    });
}

/// The three requests that reproduced #206 on main at de4a5fa (release, 8 MiB
/// main thread): explain aborted on the first; fast on the other two. They run
/// on a 64 MiB thread only because the third nests 120 `if`s inside every rule,
/// which a debug build cannot fit in a smaller stack even without a chain.
#[test]
fn the_issue_206_reproductions_answer_in_both_modes() {
    on_stack(64 * MIB, || {
        // 4,363 chained `add` rules; explain aborted, fast answered 4362.
        let add_chain = || json!({"derived": Shape::Add.rules(4_362)});
        // 10,201 plain rules, the same household queried for r5100, then r10200.
        // Explain caches r5100's chain for h1, so its second query stops there;
        // fast caches per row, so its second row walked all 10,201 rules.
        let batch = || {
            let mut rules = vec![household("r0", "integer", int(2))];
            for index in 1..=10_200 {
                rules.push(household(
                    &format!("r{index}"),
                    "integer",
                    derived(&format!("r{}", index - 1)),
                ));
            }
            json!({"derived": rules})
        };
        let batch_queries = json!([query("h1", &["r5100"]), query("h1", &["r10200"])]);
        // 105 rules, each 120 nested `if`s around the next rule; fast aborted.
        let nested_ifs = || {
            let mut rules = Vec::new();
            for index in 0..105 {
                let mut expr = if index == 104 {
                    int(1)
                } else {
                    derived(&format!("r{}", index + 1))
                };
                for _ in 0..120 {
                    expr = json!({"kind": "if", "condition": always(), "then_expr": expr, "else_expr": int(0)});
                }
                rules.push(household(&format!("r{index}"), "integer", expr));
            }
            json!({"derived": rules})
        };
        for mode in ["explain", "fast"] {
            let run = |program: Value, queries: Value| {
                let response = execute_request(request(mode, program, empty_dataset(), queries))
                    .unwrap_or_else(|error| panic!("{mode}: {error}"));
                let (values, stayed_fast) = answers(&response);
                assert_eq!(
                    stayed_fast,
                    mode == "fast",
                    "{mode}: fast fell back to explain"
                );
                values
            };
            assert_eq!(
                run(add_chain(), json!([query("h1", &["r0"])])),
                vec![json!({"kind": "decimal", "value": "4362"})],
                "{mode}: add chain"
            );
            assert_eq!(
                run(batch(), batch_queries.clone()),
                vec![json!({"kind": "integer", "value": 2}); 2],
                "{mode}: batch structure"
            );
            assert_eq!(
                run(nested_ifs(), json!([query("h0", &["r0"])])),
                vec![json!({"kind": "integer", "value": 1})],
                "{mode}: nested ifs"
            );
        }
    });
}

/// A hand-built `Program` skips the dependency check that compiled artifacts
/// and `execute_request` run. A cycle there used to recurse until the stack
/// overflowed; the driver now finds the rule deferred while an evaluation it
/// started still waits for it, and reports the cycle. A cycle longer than the
/// threshold is found the same way.
#[test]
fn a_hand_built_cyclic_program_is_an_error_not_an_overflow() {
    for length in [2, 60] {
        let mut rules = Vec::new();
        for index in 0..length {
            let next = format!("c{}", (index + 1) % length);
            rules.push(household(
                &format!("c{index}"),
                "decimal",
                json!({"kind": "add", "items": [derived(&next), int(1)]}),
            ));
        }
        let spec: ProgramSpec =
            serde_json::from_value(json!({"derived": rules})).expect("program deserializes");
        let program = spec.to_program().expect("conversion does not check cycles");
        let data = DataSet {
            inputs: Vec::new(),
            relations: Vec::new(),
        };
        for threshold in [1, 3] {
            let result = on_stack(8 * MIB, || {
                with_suspend_depth(threshold, || {
                    Engine::new(&program, &data).evaluate_scalar("c0", "h1", &model_period())
                })
            });
            match result {
                Err(EvalError::DependencyCycle(rule)) => {
                    assert!(rule.starts_with('c'), "cycle of {length}: named {rule}")
                }
                other => panic!("cycle of {length} at threshold {threshold}: {other:?}"),
            }
        }
        let default = on_stack(8 * MIB, || {
            Engine::new(&program, &data).evaluate_scalar("c0", "h1", &model_period())
        });
        assert!(
            matches!(default, Err(EvalError::DependencyCycle(_))),
            "cycle of {length} at the default threshold: {default:?}"
        );
        let dense = on_stack(8 * MIB, || {
            DenseCompiledProgram::from_program(&program, Some("Household")).map(|_| ())
        });
        let message = dense.expect_err("dense refuses a cycle").to_string();
        assert!(
            message.contains("cyclic dense compilation dependency"),
            "cycle of {length}: {message}"
        );
    }
}

/// A household counting its members where a person judgment holds; that
/// judgment is a chain of `n` person rules, `p0 = p1`, ..., `p_n = 0 == 0`.
/// Dense inlines the whole chain into the count's `where` clause.
fn inlined_chain(n: usize) -> Value {
    let mut rules = vec![household(
        "members_ok",
        "integer",
        json!({
            "kind": "count_related", "relation": "member_of_household",
            "current_slot": 1, "related_slot": 0, "where": derived("p0"),
        }),
    )];
    for index in 0..n {
        rules.push(rule(
            &format!("p{index}"),
            "Person",
            "judgment",
            "judgment",
            derived(&format!("p{}", index + 1)),
        ));
    }
    rules.push(rule(
        &format!("p{n}"),
        "Person",
        "judgment",
        "judgment",
        always(),
    ));
    json!({
        "relations": [{
            "name": "member_of_household", "arity": 2,
            "slot_entities": ["Person", "Household"],
        }],
        "derived": rules,
    })
}

fn dense_inlined_chain(n: usize) -> Result<String, String> {
    let spec: ProgramSpec = serde_json::from_value(inlined_chain(n)).expect("program deserializes");
    let program = spec.to_program().expect("program converts");
    let dense = DenseCompiledProgram::from_program(&program, Some("Household"))
        .map_err(|error| error.to_string())?;
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2],
                inputs: HashMap::new(),
            },
        )]),
    };
    let result = dense
        .execute(&model_period(), batch, &["members_ok".to_string()])
        .map_err(|error| error.to_string())?;
    Ok(format!("{:?}", result.outputs["members_ok"]))
}

/// Dense inlines the rules a relation aggregation reads, and inlined rules have
/// nothing to defer to, so dense bounds how deep they nest. The `where`
/// clause's reference to `p0`, each rule's reference to the next, the final
/// comparison and its literal operands each add one level, so a chain of
/// `MAX_INLINE_DEPTH - 3` rules compiles and one more is declined, as a compile
/// error naming the bound; explain and fast answer both. The bound depends on
/// the program alone, so every threshold agrees.
#[test]
fn dense_declines_a_relation_aggregation_that_inlines_past_its_bound() {
    let deepest = MAX_INLINE_DEPTH - 3;
    let expected = json!({"kind": "integer", "value": 2});
    on_stack(256 * MIB, move || {
        for threshold in [1, 128] {
            with_suspend_depth(threshold, || {
                assert_eq!(
                    dense_inlined_chain(deepest),
                    Ok("Scalar(Integer([2]))".to_string()),
                    "threshold {threshold}: a chain at the bound compiles"
                );
                let declined = dense_inlined_chain(deepest + 1)
                    .expect_err("a chain past the bound is declined");
                assert!(
                    declined.contains(&format!("nest more than {MAX_INLINE_DEPTH} levels deep")),
                    "threshold {threshold}: {declined}"
                );
                for n in [deepest, deepest + 1] {
                    for mode in ["explain", "fast"] {
                        let dataset = json!({
                            "inputs": [],
                            "relations": [
                                {"name": "member_of_household", "tuple": ["p1", "h1"],
                                 "interval": {"start": "2026-01-01", "end": "2026-01-31"}},
                                {"name": "member_of_household", "tuple": ["p2", "h1"],
                                 "interval": {"start": "2026-01-01", "end": "2026-01-31"}},
                            ],
                        });
                        let response = execute_request(request(
                            mode,
                            inlined_chain(n),
                            dataset,
                            json!([query("h1", &["members_ok"])]),
                        ))
                        .unwrap_or_else(|error| panic!("{mode}, chain of {n}: {error}"));
                        assert_eq!(
                            answers(&response).0,
                            vec![expected.clone()],
                            "{mode}, chain of {n}"
                        );
                    }
                }
            });
        }
    });
}

/// Lifetime execution recurses in two places: through the rules a reduction's
/// operand reads, in each period's executor, and through the rules read
/// outside any reduction. Both defer, so a deep chain on either side answers
/// on a small stack: `total = t0`, `t_i = t_{i+1}`, `t_n = sum_over_periods(r0)`,
/// `r_i = r_{i+1} + 1`, `r_m = x`.
#[test]
fn lifetime_execution_answers_deep_chains_on_both_sides_of_a_reduction() {
    const OUTSIDE: usize = 5_000;
    const INSIDE: usize = 5_000;
    let mut rules = vec![household("total", "decimal", derived("t0"))];
    for index in 0..OUTSIDE {
        rules.push(household(
            &format!("t{index}"),
            "decimal",
            derived(&format!("t{}", index + 1)),
        ));
    }
    rules.push(household(
        &format!("t{OUTSIDE}"),
        "decimal",
        json!({"kind": "over_periods", "over": "sum", "value": derived("r0")}),
    ));
    for index in 0..INSIDE {
        rules.push(household(
            &format!("r{index}"),
            "decimal",
            json!({"kind": "add", "items": [derived(&format!("r{}", index + 1)), int(1)]}),
        ));
    }
    rules.push(household(
        &format!("r{INSIDE}"),
        "decimal",
        json!({"kind": "input", "name": "x"}),
    ));
    let spec: ProgramSpec =
        serde_json::from_value(json!({"derived": rules})).expect("program deserializes");
    let program = spec.to_program().expect("program converts");
    let years = (2020..2023)
        .map(|year| Period {
            kind: PeriodKind::TaxYear,
            start: chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
            end: chrono::NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
        })
        .collect::<Vec<_>>();
    let batches = [1, 2, 3]
        .into_iter()
        .map(|x| DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([("x".to_string(), DenseColumn::Integer(vec![x]))]),
            relations: HashMap::new(),
        })
        .collect::<Vec<_>>();
    let total = on_stack(2 * MIB, move || {
        let dense =
            DenseCompiledProgram::from_program(&program, Some("Household")).expect("compiles");
        dense
            .execute_lifetime(&years, batches, &["total".to_string()])
            .expect("executes")
    });
    // Each year contributes x + INSIDE.
    let expected = Decimal::from(1 + 2 + 3 + 3 * INSIDE as i64);
    match &total.outputs["total"] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => assert_eq!(values[0], expected),
        other => panic!("unexpected lifetime column {other:?}"),
    }
}
