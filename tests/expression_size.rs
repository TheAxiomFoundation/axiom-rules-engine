//! A rule's runtime expression has at most `spec::MAX_EXPRESSION_SIZE` nodes
//! (issue 241).
//!
//! A depth bound does not bound size. Lowering a `match` copies its subject
//! into every arm, and converting `exactly_one` of `n` operands copies each
//! operand `n` times, so nesting either grows the program exponentially within
//! every depth bound. A chain of `exactly_one` 131 levels deep once grew a test
//! process to 79 GB in two minutes. The formula lowering and the conversion
//! now count nodes before building them and refuse past the bound.
//!
//! Every case here runs in a child process that the test kills, and fails, if
//! it grows past `CAP_MB` or runs past `TIMEOUT`. A regression fails these
//! tests; it cannot exhaust the machine running them.

use std::fs::File;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{MAX_EXPRESSION_SIZE, ProgramSpec};
use serde_json::{Value, json};

/// The most a case may grow to. At the size bound a rule takes a few
/// megabytes; a debug build of the test binary takes some tens.
const CAP_MB: u64 = 1024;
const TIMEOUT: Duration = Duration::from_secs(180);
const CASE: &str = "AXIOM_EXPRESSION_SIZE_CASE";

/// Run `case` in a child copy of this test binary, kill it if it grows past
/// `CAP_MB` or `TIMEOUT`, and fail unless it passes.
fn in_capped_child(case: &str) {
    run_capped("child_case", case);
}

/// Run test `test` of this binary in a child with `case` in its environment,
/// polling its resident memory; kill it and panic if it grows past `CAP_MB`
/// or runs past `TIMEOUT`, and panic if it fails.
fn run_capped(test: &str, case: &str) {
    let log = std::env::temp_dir().join(format!(
        "axiom-expression-size-{case}-{}.log",
        std::process::id()
    ));
    let output = File::create(&log).expect("create child log");
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            test,
            "--exact",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CASE, case)
        .stdout(output.try_clone().expect("clone log"))
        .stderr(output)
        .spawn()
        .expect("spawn child");
    let started = Instant::now();
    let failure = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break (!status.success()).then(|| format!("exited with {status}"));
        }
        let rss_kb = Command::new("ps")
            .args(["-o", "rss=", "-p", &child.id().to_string()])
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if rss_kb > CAP_MB * 1024 {
            let _ = child.kill();
            let _ = child.wait();
            break Some(format!("grew past {CAP_MB} MB ({rss_kb} KB)"));
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break Some(format!("ran past {TIMEOUT:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    if let Some(failure) = failure {
        panic!("case `{case}` {failure}:\n{text}");
    }
}

/// The cases, run only in a child (see [`in_capped_child`]).
#[test]
#[ignore = "run by the other tests in a memory-capped child process"]
fn child_case() {
    let case = std::env::var(CASE).expect("run by in_capped_child");
    match case.as_str() {
        "formula_exactly_one_40_deep" => formula_exactly_one_40_deep(),
        "json_exactly_one_131_deep" => json_exactly_one_131_deep(),
        "formula_match_subjects_nested" => formula_match_subjects_nested(),
        "formula_at_the_size_bound" => formula_at_the_size_bound(),
        "exactly_one_at_the_size_bound" => exactly_one_at_the_size_bound(),
        other => panic!("unknown case {other}"),
    }
}

fn rulespec(dtype: &str, formula: &str) -> String {
    format!(
        "format: rulespec/v1\nrules:\n  - name: deep\n    kind: derived\n    entity: Household\n    dtype: {dtype}\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"{formula}\"\n"
    )
}

fn assert_too_large(error: impl std::fmt::Display, context: &str) {
    let message = error.to_string();
    assert!(
        message.contains(&format!(
            "expands to more than {MAX_EXPRESSION_SIZE} expression nodes"
        )),
        "{context}: expected the size error, got: {message}"
    );
}

/// The size error from the formula lowering, which counts before it builds;
/// not the one conversion reports after lowering built the whole tree.
fn assert_refused_by_lowering(error: impl std::fmt::Display) {
    let message = error.to_string();
    assert_too_large(&message, "formula");
    assert!(
        message.contains("the formula of `deep` expands"),
        "expected the lowering to refuse it, got: {message}"
    );
}

fn period() -> Value {
    json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"})
}

/// `deep`'s answer for household `h1` with `input` set, read from JSON text.
fn answer(mode: &str, program: &Value, input: (&str, Value)) -> Result<Value, String> {
    let request = json!({
        "mode": mode,
        "program": program,
        "dataset": {"inputs": [{
            "name": input.0, "entity": "Household", "entity_id": "h1",
            "interval": {"start": "2026-01-01", "end": "2026-01-31"}, "value": input.1,
        }], "relations": []},
        "queries": [{"entity_id": "h1", "period": period(), "outputs": ["deep"]}],
    })
    .to_string();
    let request = ExecutionRequest::from_json_str(&request).map_err(|error| error.to_string())?;
    let response = execute_request(request).map_err(|error| error.to_string())?;
    let output = serde_json::to_value(&response.results[0].outputs["deep"]).expect("serializes");
    Ok(output
        .get("value")
        .or_else(|| output.get("outcome"))
        .cloned()
        .expect("a value or an outcome"))
}

/// `exactly_one(exactly_one(... (flag, flag) ..., flag), flag)`, 40 deep: well
/// within the formula's depth bound, about 2^40 nodes once converted. Lowering
/// refuses it before building anything.
fn formula_exactly_one_40_deep() {
    let mut formula = "flag".to_string();
    for _ in 0..40 {
        formula = format!("exactly_one({formula}, flag)");
    }
    assert_refused_by_lowering(
        CompiledProgramArtifact::from_rulespec_str(&rulespec("Judgment", &formula))
            .expect_err("refused"),
    );
}

/// The program that exhausted the machine: `exactly_one` chained 131 deep in
/// JSON, at the expression depth bound. Refused wherever it enters.
fn json_exactly_one_131_deep() {
    let leaf = json!({"kind": "derived", "name": "x"});
    let mut expr = leaf.clone();
    for _ in 1..131 {
        expr = json!({"kind": "exactly_one", "items": [expr, leaf]});
    }
    let program = json!({"derived": [{
        "name": "deep", "entity": "Household", "dtype": "judgment", "unit": null,
        "semantics": "judgment", "expr": expr,
    }, {
        "name": "x", "entity": "Household", "dtype": "judgment", "unit": null,
        "semantics": "judgment",
        "expr": {"kind": "comparison", "left": {"kind": "input", "name": "flag"}, "op": "eq",
                 "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}},
    }]});
    for mode in ["explain", "fast"] {
        assert_too_large(
            answer(
                mode,
                &program,
                ("flag", json!({"kind": "bool", "value": true})),
            )
            .expect_err("refused"),
            mode,
        );
    }
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program deserializes");
    assert_too_large(spec.to_program().expect_err("refused"), "to_program");
    assert_too_large(
        CompiledProgramArtifact::compile(spec).expect_err("refused"),
        "compile",
    );
    let base = CompiledProgramArtifact::from_rulespec_str(&rulespec("Judgment", "flag"))
        .expect("the base artifact compiles");
    let mut artifact = serde_json::to_value(&base).expect("artifact serializes");
    artifact["program"]["derived"] = program["derived"].clone();
    assert_too_large(
        CompiledProgramArtifact::from_json_str(&artifact.to_string()).expect_err("refused"),
        "artifact",
    );
}

/// A `match` whose subject is a `match`, eight levels, ten arms each:
/// lowering would copy each subject into every arm, about 10^8 nodes. The
/// lowering's own count refuses it, before anything is built.
fn formula_match_subjects_nested() {
    let arms = (1..=10)
        .map(|arm| format!(" {arm} => {arm}"))
        .collect::<String>();
    let mut formula = "x".to_string();
    for _ in 0..8 {
        formula = format!("(match {formula}:{arms} _ => 0)");
    }
    assert_refused_by_lowering(
        CompiledProgramArtifact::from_rulespec_str(&rulespec("Integer", &formula))
            .expect_err("refused"),
    );
}

/// `max(x, x, ...)` of `n` operands is `n + 1` nodes: at the bound it
/// compiles and answers, one node past it is refused.
fn formula_at_the_size_bound() {
    let max_of = |operands: u64| {
        let operands = usize::try_from(operands).expect("fits");
        format!("max({})", vec!["x"; operands].join(", "))
    };
    let at_bound = CompiledProgramArtifact::from_rulespec_str(&rulespec(
        "Integer",
        &max_of(MAX_EXPRESSION_SIZE - 1),
    ))
    .expect("a formula at the bound compiles");
    let program = serde_json::to_value(&at_bound.program).expect("program serializes");
    for mode in ["explain", "fast"] {
        assert_eq!(
            answer(
                mode,
                &program,
                ("x", json!({"kind": "integer", "value": 3}))
            ),
            Ok(json!({"kind": "decimal", "value": "3"})),
            "{mode}"
        );
    }
    assert_refused_by_lowering(
        CompiledProgramArtifact::from_rulespec_str(&rulespec(
            "Integer",
            &max_of(MAX_EXPRESSION_SIZE),
        ))
        .expect_err("one node past the bound is refused"),
    );
}

/// `exactly_one` of `n` inputs is `1 + n² + 3n²` nodes once converted: 158
/// operands are within the bound and answer, 159 are refused.
fn exactly_one_at_the_size_bound() {
    let exactly_one =
        |operands: usize| format!("exactly_one({})", vec!["flag"; operands].join(", "));
    let within =
        CompiledProgramArtifact::from_rulespec_str(&rulespec("Judgment", &exactly_one(158)))
            .expect("158 operands compile");
    let program = serde_json::to_value(&within.program).expect("program serializes");
    for mode in ["explain", "fast"] {
        assert_eq!(
            answer(
                mode,
                &program,
                ("flag", json!({"kind": "bool", "value": true}))
            ),
            Ok(json!("not_holds")),
            "{mode}"
        );
    }
    assert_refused_by_lowering(
        CompiledProgramArtifact::from_rulespec_str(&rulespec("Judgment", &exactly_one(159)))
            .expect_err("159 operands are refused"),
    );
}

#[test]
fn a_formula_of_nested_exactly_one_is_refused() {
    in_capped_child("formula_exactly_one_40_deep");
}

#[test]
fn a_program_of_nested_exactly_one_is_refused_wherever_it_enters() {
    in_capped_child("json_exactly_one_131_deep");
}

#[test]
fn a_formula_of_nested_match_subjects_is_refused() {
    in_capped_child("formula_match_subjects_nested");
}

#[test]
fn a_formula_at_the_size_bound_answers_and_one_node_past_it_is_refused() {
    in_capped_child("formula_at_the_size_bound");
}

#[test]
fn exactly_one_at_the_size_bound_answers_and_one_operand_more_is_refused() {
    in_capped_child("exactly_one_at_the_size_bound");
}

/// The cap works: a child that allocates past it is stopped and reported.
#[test]
fn a_child_that_grows_past_the_cap_is_stopped() {
    if std::env::var(CASE).as_deref() == Ok("grow") {
        let mut hoard: Vec<Vec<u8>> = Vec::new();
        loop {
            hoard.push(vec![1u8; 64 * 1024 * 1024]);
        }
    }
    let outcome = std::panic::catch_unwind(|| {
        run_capped("a_child_that_grows_past_the_cap_is_stopped", "grow");
    });
    let message = outcome
        .expect_err("the growing child is stopped")
        .downcast::<String>()
        .map(|message| *message)
        .unwrap_or_default();
    assert!(
        message.contains(&format!("grew past {CAP_MB} MB")),
        "{message}"
    );
}
