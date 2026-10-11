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
//! Compile and load agree on that bound (#236). A formula within it compiles
//! to an artifact nested deeper than the 128 levels of JSON serde_json reads
//! by default, so the loader used to refuse artifacts `compile` had just
//! written. It now reads JSON nested up to `compile::MAX_JSON_DEPTH` levels.
//!
//! These tests pin that:
//!
//! * every way of nesting a formula is a formula error far past the bound,
//!   in a rule's formula and in a derived relation's predicate;
//! * one level past the bound is refused, in every way of nesting;
//! * at the bound, every way of nesting compiles, serializes to an artifact
//!   that loads back unchanged, and answers alike in explain, fast and dense,
//!   before and after the reload;
//! * the formulas reported in #236 reload and answer, in-process and through
//!   the CLI;
//! * JSON nested past the loader's bound is an error;
//! * a rule pinned many times is wrapped once, not once per pin.
//!
//! A release build does all of it on a 1 MiB thread. CI runs these tests in
//! a debug build, whose frames are several times larger, on the stacks named
//! below; `cargo test --release --test formula_depth` checks the 1 MiB claim.
//!
//! The parser's own unit tests (`src/formula.rs`) check the depth it
//! computes against an independent count on generated formulas, and that
//! generated formulas reload and answer the same.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionRequest, ExecutionResponse, execute_compiled_request,
    execute_request,
};
use axiom_rules_engine::compile::{CompiledProgramArtifact, MAX_JSON_DEPTH};
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
    // `exactly_one(not not flag, not flag)`: a call around a deep argument.
    // The runtime program expands it to an or of ands of nots, which adds
    // levels and copies the argument once per argument.
    Shape {
        name: "exactly one of a deep argument",
        dtype: "Judgment",
        formula: |depth| format!("exactly_one({}flag, not flag)", "not ".repeat(depth - 2)),
        inputs: flag_input,
        answer: |depth| json!(if depth % 2 == 0 { "holds" } else { "not_holds" }),
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

fn dataset(inputs: &[(&str, Value)]) -> Value {
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
    json!({"inputs": inputs, "relations": []})
}

fn queries() -> Value {
    json!([{"entity_id": "h1", "period": period(), "outputs": ["deep"]}])
}

/// A request with the program inline, as JSON text.
fn request_json(
    mode: &str,
    artifact: &CompiledProgramArtifact,
    inputs: &[(&str, Value)],
) -> String {
    json!({
        "mode": mode,
        "program": artifact.program,
        "dataset": dataset(inputs),
        "queries": queries(),
    })
    .to_string()
}

/// The request read from JSON text, as a host sends it.
fn request(
    mode: &str,
    artifact: &CompiledProgramArtifact,
    inputs: &[(&str, Value)],
) -> ExecutionRequest {
    ExecutionRequest::from_json_str(&request_json(mode, artifact, inputs))
        .expect("request deserializes")
}

/// How deep a JSON document nests, in arrays and objects.
fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(fields) => 1 + fields.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// `artifact` after a trip through its JSON, which must not change it.
fn reloaded(artifact: &CompiledProgramArtifact, context: &str) -> CompiledProgramArtifact {
    let json = serde_json::to_string(artifact).expect("artifact serializes");
    let loaded = CompiledProgramArtifact::from_json_str(&json)
        .unwrap_or_else(|error| panic!("{context}: the artifact does not reload: {error}"));
    assert_eq!(
        serde_json::to_string(&loaded).expect("artifact serializes"),
        json,
        "{context}: reloading changed the artifact"
    );
    loaded
}

/// `deep`'s answer in explain, fast and dense, which must agree with
/// `expected`.
fn assert_answers(
    artifact: &CompiledProgramArtifact,
    inputs: &[(&str, Value)],
    expected: &Value,
    context: &str,
) {
    let explain = execute_request(request("explain", artifact, inputs))
        .unwrap_or_else(|error| panic!("{context}: explain: {error}"));
    assert_eq!(&answer(&explain).0, expected, "{context}: explain");

    let fast = execute_request(request("fast", artifact, inputs))
        .unwrap_or_else(|error| panic!("{context}: fast: {error}"));
    let (value, stayed_fast) = answer(&fast);
    assert_eq!(&value, expected, "{context}: fast");
    assert!(stayed_fast, "{context}: fast fell back to explain");

    assert_eq!(
        &dense_answer(artifact, inputs),
        expected,
        "{context}: dense"
    );
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

/// At the bound, every way of nesting compiles, serializes to an artifact
/// that loads back unchanged, and answers alike in explain, fast and dense,
/// before and after the reload. The artifact nests deeper than the 128 levels
/// serde_json reads by default, and within half the loader's bound.
#[test]
fn every_way_of_nesting_at_the_bound_reloads_and_answers_in_every_mode() {
    let mut deepest = 0;
    for shape in SHAPES {
        deepest = deepest.max(on_stack(AT_BOUND_STACK, || {
            let depth = MAX_FORMULA_DEPTH;
            let source = rulespec(shape.dtype, &(shape.formula)(depth));
            let artifact = CompiledProgramArtifact::from_rulespec_str(&source)
                .unwrap_or_else(|error| panic!("{}: compile: {error}", shape.name));
            let inputs = (shape.inputs)(depth);
            let expected = canonical(&(shape.answer)(depth));

            assert_answers(&artifact, &inputs, &expected, shape.name);
            let loaded = reloaded(&artifact, shape.name);
            assert_answers(
                &loaded,
                &inputs,
                &expected,
                &format!("{}, reloaded", shape.name),
            );
            json_depth(&serde_json::to_value(&artifact).expect("artifact serializes"))
        }));
    }
    assert!(
        deepest > 2 * MAX_FORMULA_DEPTH && deepest <= MAX_JSON_DEPTH / 2 + 16,
        "the deepest artifact at the bound nests {deepest} levels of JSON; the loader reads {MAX_JSON_DEPTH}"
    );
}

/// The formulas reported in #236, at the sizes that first failed to load: a
/// sum of 62 terms, an `and` chain of 61, a `match` of 119 arms, an `elif`
/// chain of 119 branches and a minus nested 120 deep. Each compiled, and the
/// loader then refused the artifact with `recursion limit exceeded`.
#[test]
fn the_formulas_reported_in_issue_236_reload_and_answer() {
    on_stack(AT_BOUND_STACK, || {
        let x = |value: i64| vec![("x", integer(value))];
        let mut matched = String::from("match x:");
        let mut chained = String::from("if x == 1: 1");
        for arm in 1..=119 {
            matched.push_str(&format!(" {arm} => {arm}"));
            if arm > 1 {
                chained.push_str(&format!(" elif x == {arm}: {arm}"));
            }
        }
        matched.push_str(" _ => 0");
        chained.push_str(" else: 0");
        for (name, dtype, formula, inputs, expected) in [
            (
                "sum of 62 terms",
                "Integer",
                format!("x{}", " + x".repeat(61)),
                x(2),
                integer(124),
            ),
            (
                "and chain of 61",
                "Judgment",
                format!("flag{}", " and flag".repeat(60)),
                flag_input(0),
                json!("holds"),
            ),
            (
                "match of 119 arms",
                "Integer",
                matched,
                x(119),
                integer(119),
            ),
            (
                "elif chain of 119",
                "Integer",
                chained,
                x(119),
                integer(119),
            ),
            (
                "minus nested 120 deep",
                "Integer",
                format!("{}x", "-".repeat(120)),
                x(7),
                integer(7),
            ),
        ] {
            let artifact = CompiledProgramArtifact::from_rulespec_str(&rulespec(dtype, &formula))
                .unwrap_or_else(|error| panic!("{name}: compile: {error}"));
            let loaded = reloaded(&artifact, name);
            assert_answers(&loaded, &inputs, &canonical(&expected), name);
        }
    });
}

/// The same through the CLI, which is where #236 was reported: `run-compiled`
/// loads an artifact file, and the bare command reads a request whose program
/// is inline, as the Python client sends it.
#[test]
fn the_cli_loads_an_artifact_and_a_request_nested_past_128_levels() {
    let artifact = on_stack(AT_BOUND_STACK, || {
        CompiledProgramArtifact::from_rulespec_str(&rulespec(
            "Integer",
            &format!("x{}", " + x".repeat(61)),
        ))
        .expect("a sum of 62 terms compiles")
    });
    let artifact_json = serde_json::to_string(&artifact).expect("artifact serializes");
    assert!(
        json_depth(&serde_json::to_value(&artifact).expect("artifact serializes")) > 128,
        "the artifact should nest past serde_json's default limit"
    );
    let inputs = [("x", integer(2))];
    let directory = std::env::temp_dir().join(format!(
        "axiom-rules-engine-formula-depth-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create temp directory");
    let artifact_path = directory.join("sum62.compiled.json");
    std::fs::write(&artifact_path, &artifact_json).expect("write artifact");

    let run = |args: &[&str], stdin: String| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn axiom-rules-engine binary");
        child
            .stdin
            .take()
            .expect("stdin available")
            .write_all(stdin.as_bytes())
            .expect("request written");
        let output = child.wait_with_output().expect("binary completes");
        assert!(
            output.status.success(),
            "{args:?}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: ExecutionResponse =
            serde_json::from_slice(&output.stdout).expect("response parses");
        answer(&response).0
    };
    let compiled_request = json!({
        "mode": "explain", "dataset": dataset(&inputs), "queries": queries(),
    })
    .to_string();
    assert_eq!(
        run(
            &[
                "run-compiled",
                "--artifact",
                artifact_path.to_str().expect("utf8 path")
            ],
            compiled_request
        ),
        canonical(&integer(124)),
        "run-compiled"
    );
    assert_eq!(
        run(&[], request_json("explain", &artifact, &inputs)),
        canonical(&integer(124)),
        "inline request"
    );
    std::fs::remove_dir_all(directory).ok();
}

/// JSON nested past the loader's bound is an error that says so, however
/// deep it goes: the bound is what keeps reading it on the stack.
#[test]
fn json_nested_past_the_loaders_bound_is_an_error() {
    on_stack(REFUSAL_STACK, || {
        for levels in [MAX_JSON_DEPTH + 1, 1_000_000] {
            let nested = format!("{}{}", "[".repeat(levels), "]".repeat(levels));
            let message = format!("JSON nests more than {MAX_JSON_DEPTH} levels deep");
            let error = CompiledProgramArtifact::from_json_str(&nested)
                .expect_err("an artifact nested past the bound is refused")
                .to_string();
            assert!(error.contains(&message), "artifact: {error}");
            let error = ExecutionRequest::from_json_str(&nested)
                .expect_err("a request nested past the bound is refused")
                .to_string();
            assert!(error.contains(&message), "request: {error}");
        }
    });
}

/// A request's pins are the one other thing that deepens a tree: a pin wraps
/// its rule's expression in an `if`. A rule pinned many times used to be
/// wrapped once per pin, so a flat request with 100,000 pins on one rule
/// built a 100,000-level tree and overflowed the stack. The rule takes its
/// last pin, as the nested wraps always evaluated to, and is wrapped once.
#[test]
fn a_rule_pinned_many_times_takes_its_last_pin_and_is_wrapped_once() {
    let artifact = || {
        CompiledProgramArtifact::from_rulespec_str(&rulespec("Integer", "x + 1"))
            .expect("the rule compiles")
    };
    let respond = |mode: &str, pins: &[i64]| {
        let pins: Vec<Value> = pins
            .iter()
            .map(|value| json!({"rule": "deep", "value": integer(*value)}))
            .collect();
        let request: CompiledExecutionRequest = serde_json::from_value(json!({
            "mode": mode,
            "dataset": dataset(&[("x", integer(1))]),
            "queries": queries(),
            "pins": pins,
        }))
        .expect("request deserializes");
        execute_compiled_request(artifact(), request)
            .unwrap_or_else(|error| panic!("{mode} with {} pins: {error}", pins.len()))
    };
    on_stack(REFUSAL_STACK, || {
        for mode in ["explain", "fast"] {
            // However many pins precede it, the last one is the answer, and
            // the whole response is the one the last pin alone gives.
            let alone = serde_json::to_value(respond(mode, &[7])).expect("response serializes");
            for earlier in [1, 2, 5, 100_000] {
                let mut pins: Vec<i64> = (0..earlier).map(|pin| 1_000 + pin).collect();
                pins.push(7);
                let response = respond(mode, &pins);
                assert_eq!(
                    answer(&response).0,
                    canonical(&integer(7)),
                    "{mode}: {earlier} pins before the last"
                );
                assert_eq!(
                    serde_json::to_value(response).expect("response serializes"),
                    alone,
                    "{mode}: {earlier} pins before the last change the response"
                );
            }
        }
    });

    // Every pin is still checked, in order, a pin that a later one supersedes
    // included: the first pin on an unknown rule is the error.
    let request: CompiledExecutionRequest = serde_json::from_value(json!({
        "mode": "explain",
        "dataset": dataset(&[("x", integer(1))]),
        "queries": queries(),
        "pins": [
            {"rule": "deep", "value": integer(1)},
            {"rule": "deep", "value": integer(2)},
            {"rule": "missing", "value": integer(3)},
            {"rule": "also_missing", "value": integer(4)},
            {"rule": "missing", "value": integer(5)},
        ],
    }))
    .expect("request deserializes");
    let error = execute_compiled_request(artifact(), request)
        .expect_err("an unknown pinned rule is an error")
        .to_string();
    assert!(
        error.contains("`missing`") && !error.contains("also_missing"),
        "{error}"
    );
}
