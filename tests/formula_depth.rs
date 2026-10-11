//! A RuleSpec formula nests at most `rulespec::MAX_FORMULA_DEPTH` levels.
//!
//! The formula parser recursed once per nesting level and built an expression
//! tree as deep as the formula nested; lowering, compilation, every evaluator
//! and dropping the tree then recursed through that tree again. A formula
//! nested a few thousand levels deep, such as `((((...1...))))` or
//! `----...1`, overflowed the stack: the process aborted with no error to
//! catch (PyO3 cannot convert it into a Python exception, and wasm traps). The
//! parser now refuses a formula deeper than the bound before it builds a tree
//! that deep, so no later stage recurses further than one bounded formula.
//!
//! These tests pin that:
//!
//! * every way of nesting a formula is a formula error far past the bound,
//!   in a rule's formula and in a derived relation's predicate;
//! * one level past the bound is refused, in every way of nesting;
//! * at the bound, every way of nesting compiles and answers alike in
//!   explain, fast and dense.
//!
//! A release build does all of it on a 1 MiB thread. CI runs these tests in
//! a debug build, whose frames are several times larger, on the stacks named
//! below; `cargo test --release --test formula_depth` checks the 1 MiB claim.
//!
//! The parser's own unit tests (`src/formula.rs`) check the depth it
//! computes against an independent count on generated formulas.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionRequest, ExecutionResponse, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::rulespec::{MAX_FORMULA_DEPTH, lower_rulespec_str};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const MIB: usize = 1024 * 1024;

/// The stack a formula at the bound is compiled and evaluated on: 1 MiB in a
/// release build, where the deepest stage measured (dense, on a flat `and`
/// chain) needed about 0.6 MiB. A debug build needed up to 5.8 MiB for the
/// same formulas, so it gets 16 MiB.
const AT_BOUND_STACK: usize = if cfg!(debug_assertions) {
    16 * MIB
} else {
    MIB
};

/// The stack a formula past the bound is refused on. The parser descends to
/// the bound before it refuses, which took about 0.3 MiB in a release build
/// and 2.8 MiB in a debug build; a debug build gets a main thread's 8 MiB.
const REFUSAL_STACK: usize = if cfg!(debug_assertions) { 8 * MIB } else { MIB };

/// Far past the bound: the issue's reproductions aborted at 5,000 levels.
const FAR_PAST: usize = 100_000;

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

/// A way of nesting a formula. `formula(depth)` nests exactly `depth` levels
/// in the parser's count (see `MAX_FORMULA_DEPTH`), and evaluating it with
/// `inputs(depth)` answers `answer(depth)`: a number, or a judgment's
/// outcome.
struct Shape {
    name: &'static str,
    dtype: &'static str,
    formula: fn(usize) -> String,
    inputs: fn(usize) -> Vec<(&'static str, Value)>,
    answer: fn(usize) -> Value,
}

fn integer(value: i64) -> Value {
    json!({"kind": "integer", "value": value})
}

/// An answer in one spelling. The modes agree on a number's value but not on
/// whether to call a whole number an integer or a decimal, which is not what
/// these tests are about.
fn canonical(answer: &Value) -> Value {
    match answer["kind"].as_str() {
        Some("integer") => json!(answer["value"].as_i64().expect("integer").to_string()),
        Some("decimal") => json!(
            answer["value"]
                .as_str()
                .expect("decimal string")
                .parse::<Decimal>()
                .expect("decimal")
                .normalize()
                .to_string()
        ),
        _ => answer.clone(),
    }
}

fn signed(depth: usize) -> i64 {
    i64::try_from(depth).expect("depth fits i64")
}

fn no_inputs(_: usize) -> Vec<(&'static str, Value)> {
    Vec::new()
}

fn flag_input(_: usize) -> Vec<(&'static str, Value)> {
    vec![("flag", json!({"kind": "bool", "value": true}))]
}

/// `x` names the last branch of a `depth`-level chain, so evaluation walks
/// every branch before it.
fn last_branch_input(depth: usize) -> Vec<(&'static str, Value)> {
    vec![("x", integer(signed(depth - 2)))]
}

const SHAPES: &[Shape] = &[
    // `----1`: the issue's second reproduction.
    Shape {
        name: "negation",
        dtype: "Integer",
        formula: |depth| format!("{}1", "-".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |depth| integer(if depth % 2 == 0 { -1 } else { 1 }),
    },
    // `((((1))))`: the issue's first reproduction. Parentheses build no tree
    // level, but the parser descends once per group.
    Shape {
        name: "parentheses",
        dtype: "Integer",
        formula: |depth| format!("{}1{}", "(".repeat(depth - 1), ")".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |_| integer(1),
    },
    // `1 + 1 + 1`: a flat chain builds `(1 + 1) + 1` without the parser
    // recursing at all.
    Shape {
        name: "flat sum",
        dtype: "Integer",
        formula: |depth| format!("1{}", " + 1".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |depth| integer(signed(depth)),
    },
    Shape {
        name: "flat difference",
        dtype: "Integer",
        formula: |depth| format!("0{}", " - 1".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |depth| integer(1 - signed(depth)),
    },
    Shape {
        name: "flat product",
        dtype: "Integer",
        formula: |depth| format!("1{}", " * 1".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |_| integer(1),
    },
    // `1 + (1 + (1))`: right-nested through parentheses.
    Shape {
        name: "nested sum",
        dtype: "Integer",
        formula: |depth| format!("{}1{}", "1 + (".repeat(depth - 1), ")".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |depth| integer(signed(depth)),
    },
    Shape {
        name: "nested calls",
        dtype: "Integer",
        formula: |depth| format!("{}1{}", "max(".repeat(depth - 1), ")".repeat(depth - 1)),
        inputs: no_inputs,
        answer: |_| integer(1),
    },
    // `if true: if true: 1 else: 0 else: 0`
    Shape {
        name: "nested if",
        dtype: "Integer",
        formula: |depth| {
            format!(
                "{}1{}",
                "if true: ".repeat(depth - 1),
                " else: 0".repeat(depth - 1)
            )
        },
        inputs: no_inputs,
        answer: |_| integer(1),
    },
    // `if x == 1: 1 elif x == 2: 2 ... else: 0`: lowering nests one `if` per
    // branch, so a chain of `depth - 2` branches is `depth` levels deep.
    Shape {
        name: "elif chain",
        dtype: "Integer",
        formula: |depth| {
            let branches = depth - 2;
            let mut formula = String::from("if x == 1: 1");
            for branch in 2..=branches {
                formula.push_str(&format!(" elif x == {branch}: {branch}"));
            }
            formula.push_str(" else: 0");
            formula
        },
        inputs: last_branch_input,
        answer: |depth| integer(signed(depth - 2)),
    },
    // `match x: 1 => 1 2 => 2 ... _ => 0`: lowering nests one `if` per arm,
    // like an `elif` chain.
    Shape {
        name: "match arms",
        dtype: "Integer",
        formula: |depth| {
            let arms = depth - 2;
            let mut formula = String::from("match x:");
            for arm in 1..=arms {
                formula.push_str(&format!(" {arm} => {arm}"));
            }
            formula.push_str(" _ => 0");
            formula
        },
        inputs: last_branch_input,
        answer: |depth| integer(signed(depth - 2)),
    },
    Shape {
        name: "negated judgment",
        dtype: "Judgment",
        formula: |depth| format!("{}flag", "not ".repeat(depth - 1)),
        inputs: flag_input,
        answer: |depth| json!(if depth % 2 == 0 { "not_holds" } else { "holds" }),
    },
    Shape {
        name: "flat conjunction",
        dtype: "Judgment",
        formula: |depth| format!("flag{}", " and flag".repeat(depth - 1)),
        inputs: flag_input,
        answer: |_| json!("holds"),
    },
    Shape {
        name: "flat disjunction",
        dtype: "Judgment",
        formula: |depth| format!("not flag{}", " or not flag".repeat(depth - 2)),
        inputs: flag_input,
        answer: |_| json!("not_holds"),
    },
];

fn rulespec(dtype: &str, formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: deep
    kind: derived
    entity: Household
    dtype: {dtype}
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: "{formula}"
"#
    )
}

/// A derived relation whose predicate is `formula`.
fn derived_relation_rulespec(formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: eligible_member_of_household
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
    versions:
      - effective_from: 2026-01-01
        formula: "member_of_household and {formula}"
  - name: eligible_count
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(eligible_member_of_household)
"#
    )
}

fn assert_too_deep(error: impl std::fmt::Display, context: &str) {
    let message = error.to_string();
    assert!(
        message.contains(&format!("nests more than {MAX_FORMULA_DEPTH} levels deep")),
        "{context}: expected the formula depth error, got: {message}"
    );
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

fn request(
    mode: &str,
    artifact: &CompiledProgramArtifact,
    inputs: &[(&str, Value)],
) -> ExecutionRequest {
    let inputs: Vec<Value> = inputs
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name, "entity": "Household", "entity_id": "h1",
                "interval": {"start": "2026-01-01", "end": "2026-01-31"},
                "value": value,
            })
        })
        .collect();
    serde_json::from_value(json!({
        "mode": mode,
        "program": artifact.program,
        "dataset": {"inputs": inputs, "relations": []},
        "queries": [{"entity_id": "h1", "period": period(), "outputs": ["deep"]}],
    }))
    .expect("request deserializes")
}

/// The answer to `deep`, and whether a fast request stayed fast.
fn answer(response: &ExecutionResponse) -> (Value, bool) {
    let output =
        serde_json::to_value(&response.results[0].outputs["deep"]).expect("output serializes");
    let value = canonical(
        output
            .get("value")
            .or_else(|| output.get("outcome"))
            .expect("a value or an outcome"),
    );
    let stayed_fast = serde_json::to_value(&response.metadata).expect("metadata serializes")["actual_mode"]
        == json!("fast");
    (value, stayed_fast)
}

/// Dense's answer to `deep` over a one-row batch.
fn dense_answer(artifact: &CompiledProgramArtifact, inputs: &[(&str, Value)]) -> Value {
    let dense = DenseCompiledProgram::from_artifact(artifact, Some("Household"))
        .unwrap_or_else(|error| panic!("dense compile: {error}"));
    let columns = inputs
        .iter()
        .map(|(name, value)| {
            let column = match value["kind"].as_str() {
                Some("integer") => {
                    DenseColumn::Integer(vec![value["value"].as_i64().expect("integer input")])
                }
                Some("bool") => {
                    DenseColumn::Bool(vec![value["value"].as_bool().expect("bool input")])
                }
                other => panic!("no dense column for input kind {other:?}"),
            };
            (name.to_string(), column)
        })
        .collect::<HashMap<_, _>>();
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: columns,
        relations: HashMap::new(),
    };
    let result = dense
        .execute(&model_period(), batch, &["deep".to_string()])
        .unwrap_or_else(|error| panic!("dense execute: {error}"));
    match &result.outputs["deep"] {
        DenseOutputValue::Judgment(values) => json!(
            format!("{:?}", values[0])
                .to_lowercase()
                .replace("nothold", "not_hold")
        ),
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => canonical(&integer(values[0])),
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
            json!(values[0].normalize().to_string())
        }
        other => panic!("unexpected dense column {other:?}"),
    }
}

/// The two reproductions from the report: 2,000 levels compiled on main,
/// 5,000 aborted the process. Both are now formula errors.
#[test]
fn the_reported_reproductions_are_formula_errors() {
    on_stack(REFUSAL_STACK, || {
        for levels in [2_000, 5_000] {
            for formula in [
                format!("{}1{}", "(".repeat(levels), ")".repeat(levels)),
                format!("{}1", "-".repeat(levels)),
            ] {
                let context = format!("{} at {levels} levels", &formula[..1]);
                let error =
                    CompiledProgramArtifact::from_rulespec_str(&rulespec("Integer", &formula))
                        .expect_err("a formula past the bound is refused");
                assert_too_deep(error, &context);
            }
        }
    });
}

/// Every way of nesting, far past the bound, is refused as a formula error
/// before any stage recurses through it: in a rule's formula, through both
/// lowering entry points, and in a derived relation's predicate.
#[test]
fn every_way_of_nesting_far_past_the_bound_is_a_formula_error() {
    on_stack(REFUSAL_STACK, || {
        for shape in SHAPES {
            let source = rulespec(shape.dtype, &(shape.formula)(FAR_PAST));
            assert_too_deep(
                lower_rulespec_str(&source).expect_err("lowering refuses it"),
                shape.name,
            );
            assert_too_deep(
                CompiledProgramArtifact::from_rulespec_str(&source)
                    .expect_err("compilation refuses it"),
                shape.name,
            );
        }
        for predicate in [
            format!("{}flag", "not ".repeat(FAR_PAST)),
            format!("{}flag{}", "(".repeat(FAR_PAST), ")".repeat(FAR_PAST)),
            format!("flag{}", " and flag".repeat(FAR_PAST)),
        ] {
            assert_too_deep(
                CompiledProgramArtifact::from_rulespec_str(&derived_relation_rulespec(&predicate))
                    .expect_err("a derived relation's predicate is bounded too"),
                "derived relation predicate",
            );
        }
    });
}

/// One level past the bound is refused, in every way of nesting.
#[test]
fn every_way_of_nesting_one_level_past_the_bound_is_refused() {
    on_stack(REFUSAL_STACK, || {
        for shape in SHAPES {
            let source = rulespec(shape.dtype, &(shape.formula)(MAX_FORMULA_DEPTH + 1));
            assert_too_deep(
                CompiledProgramArtifact::from_rulespec_str(&source)
                    .expect_err("one level past the bound is refused"),
                shape.name,
            );
        }
        assert_too_deep(
            CompiledProgramArtifact::from_rulespec_str(&derived_relation_rulespec(&format!(
                "{}flag",
                "not ".repeat(MAX_FORMULA_DEPTH)
            )))
            .expect_err("a predicate one level past the bound is refused"),
            "derived relation predicate",
        );
    });
}

/// At the bound, every way of nesting compiles and answers alike in explain,
/// fast and dense.
#[test]
fn every_way_of_nesting_at_the_bound_answers_in_every_mode() {
    for shape in SHAPES {
        on_stack(AT_BOUND_STACK, || {
            let depth = MAX_FORMULA_DEPTH;
            let source = rulespec(shape.dtype, &(shape.formula)(depth));
            let artifact = CompiledProgramArtifact::from_rulespec_str(&source)
                .unwrap_or_else(|error| panic!("{}: compile: {error}", shape.name));
            let inputs = (shape.inputs)(depth);
            let expected = canonical(&(shape.answer)(depth));

            let explain = execute_request(request("explain", &artifact, &inputs))
                .unwrap_or_else(|error| panic!("{}: explain: {error}", shape.name));
            assert_eq!(answer(&explain).0, expected, "{}: explain", shape.name);

            let fast = execute_request(request("fast", &artifact, &inputs))
                .unwrap_or_else(|error| panic!("{}: fast: {error}", shape.name));
            let (value, stayed_fast) = answer(&fast);
            assert_eq!(value, expected, "{}: fast", shape.name);
            assert!(stayed_fast, "{}: fast fell back to explain", shape.name);

            assert_eq!(
                dense_answer(&artifact, &inputs),
                expected,
                "{}: dense",
                shape.name
            );
        });
    }
}
