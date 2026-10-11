//! A rule's runtime expression has at most `spec::MAX_EXPRESSION_SIZE` nodes
//! (issue 241).
//!
//! A depth bound does not bound size. Lowering a `match` copies its subject
//! into every arm, and converting `exactly_one` of `n` operands copies each
//! operand `n` times, so nesting either grows the program exponentially within
//! every depth bound. A chain of `exactly_one` 131 levels deep once grew a test
//! process to 79 GB in two minutes. The formula lowering and the conversion
//! now count nodes before building them and refuse past the bound. The
//! lowering's count includes the subject of a `match` with only `_`, which it
//! builds and drops, and dense compiles a `match` fallback without a copy of
//! the last arm, so neither builds more than the expression.
//!
//! Every case here runs in a child process. The test reads the child's
//! resident memory, sleeping 10 ms between reads, and kills the child, and
//! fails, once it passes `CAP_MB`, if it runs past `TIMEOUT`, or if its memory
//! cannot be read within `READ_TIMEOUT`. So a regression fails these tests
//! rather than growing without limit. Between two readings the child can grow
//! for 10 ms plus however long the read takes, at most `READ_TIMEOUT`.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::depth::count_visits;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{MAX_EXPRESSION_SIZE, ProgramSpec};
use serde_json::{Value, json};

/// The most a case may grow to. At the size bound a rule takes a few
/// megabytes; a debug build of the test binary takes some tens.
const CAP_MB: u64 = 1024;
const TIMEOUT: Duration = Duration::from_secs(180);
const CASE: &str = "AXIOM_EXPRESSION_SIZE_CASE";

/// How long one read of a child's memory may take, from starting `ps` to
/// having all its output.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `case` in a child copy of this test binary, kill it if it grows past
/// `CAP_MB` or `TIMEOUT`, and fail unless it passes.
fn in_capped_child(case: &str) {
    run_capped("child_case", case);
}

/// Read `pipe` to its end on a thread of its own, so a caller can wait for it
/// with a deadline.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            let _ = sender.send(text);
        });
    }
    receiver
}

/// The resident memory of process `pid`, in KB, as `ps` reports it.
fn resident_kb(pid: u32) -> Result<u64, String> {
    read_size("ps", &["-o", "rss=", "-p", &pid.to_string()])
}

/// The number `program` prints. An error if it cannot be run, fails, prints
/// anything but a number, or has not both exited and closed its output within
/// `READ_TIMEOUT` of starting: a process it leaves holding its output open
/// cannot hold the read past the deadline either.
fn read_size(program: &str, args: &[&str]) -> Result<u64, String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut reader = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    let stdout = drain(reader.stdout.take());
    let stderr = drain(reader.stderr.take());
    let late = || format!("{program} did not answer within {READ_TIMEOUT:?}");
    let status = loop {
        if let Some(status) = reader
            .try_wait()
            .map_err(|error| format!("{program}: {error}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            stop(&mut reader);
            return Err(late());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let stdout = stdout
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| late())?;
    if !status.success() {
        let stderr = stderr
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        return Err(format!("{program} exited with {status}: {}", stderr.trim()));
    }
    stdout
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("{program} printed {:?}, not a size", stdout.trim()))
}

/// Run test `test` of this binary in a child with `case` in its environment,
/// polling its resident memory; kill it and panic if it grows past `CAP_MB`
/// or runs past `TIMEOUT`, and panic if it fails.
fn run_capped(test: &str, case: &str) {
    run_capped_reading(test, case, resident_kb);
}

/// Kill `child` and wait for it to exit.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// [`run_capped`], reading the child's memory with `read`. A read that fails
/// while the child is still running stops the child and fails the case: an
/// unmeasured child is never left to grow. The child's output goes through
/// pipes this process drains, not a file, so nothing is written to disk.
fn run_capped_reading(test: &str, case: &str, read: impl Fn(u32) -> Result<u64, String>) {
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            test,
            "--exact",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CASE, case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let failure = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break (!status.success()).then(|| format!("exited with {status}"));
        }
        let rss_kb = match read(child.id()) {
            Ok(rss_kb) => rss_kb,
            // `ps` finds no process once the child has exited.
            Err(_) if child.try_wait().is_ok_and(|status| status.is_some()) => continue,
            Err(error) => {
                stop(&mut child);
                break Some(format!("could not have its memory read: {error}"));
            }
        };
        if rss_kb > CAP_MB * 1024 {
            stop(&mut child);
            break Some(format!("grew past {CAP_MB} MB ({rss_kb} KB)"));
        }
        if started.elapsed() > TIMEOUT {
            stop(&mut child);
            break Some(format!("ran past {TIMEOUT:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if let Some(failure) = failure {
        // The child has exited, so its pipes close.
        let output = |pipe: Receiver<String>| pipe.recv_timeout(READ_TIMEOUT).unwrap_or_default();
        panic!(
            "case `{case}` {failure}:\n{}{}",
            output(stdout),
            output(stderr)
        );
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
        "formula_match_with_only_a_wildcard" => formula_match_with_only_a_wildcard(),
        "dense_match_fallbacks_in_last_arms" => dense_match_fallbacks_in_last_arms(),
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

/// `match S: _ => 0`, where `S` is forty `match`es each the subject of the
/// next, with two arms and `_`: `S` would lower to about 10 × 2^40 nodes, which
/// the wildcard then drops. Lowering builds the subject to check it, so it
/// counts it, and refuses the formula before building anything.
fn formula_match_with_only_a_wildcard() {
    let mut subject = "x".to_string();
    for _ in 0..40 {
        subject = format!("(match {subject}: 0 => 0 1 => 0 _ => 0)");
    }
    assert_refused_by_lowering(
        CompiledProgramArtifact::from_rulespec_str(&rulespec(
            "Integer",
            &format!("match {subject}: _ => 0"),
        ))
        .expect_err("refused"),
    );
}

/// `match x: 0 => (match x: 0 => (... 1))`, 40 deep and without `_`, for a
/// household and for each of its members, and over a household's lifetime
/// with a sum over the years innermost.
const FALLBACKS_IN_LAST_ARMS: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: deep
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "NESTED"
  - name: member_deep
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "NESTED"
  - name: members_deep
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.member_deep)
  - name: lifetime_deep
    kind: derived
    entity: Household
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2020-01-01
        formula: "LIFETIME"
"#;

/// Each `match` without `_` ends in a fallback that fails the rows reaching
/// it, and whose column is its last arm's value. Forty of them, each the last
/// arm of the one before, are 281 runtime nodes; dense once compiled a copy of
/// the last arm for each fallback, about 2^40 nodes, and evaluated each copy
/// again. It now compiles each node once, here and in a household's sum over
/// its members (where it inlines the members' rule), evaluates each node at
/// most once per fallback above it plus once, and answers as explain does. The same
/// holds in each of dense's three evaluators: a rule's rows, related rows,
/// and a lifetime.
fn dense_match_fallbacks_in_last_arms() {
    let nest = |innermost: &str| {
        let mut nested = innermost.to_string();
        for _ in 0..40 {
            nested = format!("match x: 0 => ({nested})");
        }
        nested
    };
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        &FALLBACKS_IN_LAST_ARMS
            .replace("NESTED", &nest("1"))
            .replace("LIFETIME", &nest("sum_over_periods(e)")),
    )
    .expect("the module compiles");
    let (dense, visits) = count_visits(|| {
        DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles")
    });
    // `deep`, the member rule inlined into `members_deep`, and
    // `lifetime_deep`: each at most its 281 runtime nodes.
    assert!(visits <= 3 * 281, "dense compiled {visits} nodes");

    let period = Period {
        kind: PeriodKind::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    };
    // Two households, the first with two members.
    let batch = |household_x: [i64; 2], member_x: [i64; 3]| DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::from([
            ("x".to_string(), DenseColumn::Integer(household_x.to_vec())),
            ("e".to_string(), DenseColumn::Integer(vec![5, 7])),
        ]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2, 3],
                inputs: HashMap::from([("x".to_string(), DenseColumn::Integer(member_x.to_vec()))]),
            },
        )]),
    };
    let outputs = ["deep".to_string(), "members_deep".to_string()];
    let (result, visits) =
        count_visits(|| dense.execute(&period, batch([0, 0], [0, 0, 0]), &outputs));
    let result = result.expect("every row reaches the innermost arm");
    // Each fallback evaluates its last arm once more, for no rows, and any
    // fallback inside it reuses that: so each of the 281 nodes is evaluated
    // at most once, and once more for each of the 40 fallbacks above it.
    assert!(visits <= 281 * 41, "dense evaluated {visits} nodes");
    let integers = |result: &DenseExecutionResult, name: &str| match &result.outputs[name] {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values.clone(),
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values
            .iter()
            .map(|value| i64::try_from(*value).expect("integral"))
            .collect(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(integers(&result, "deep"), vec![1, 1]);
    assert_eq!(integers(&result, "members_deep"), vec![2, 1]);

    // Over two years, `e` summed innermost: 5 and 7 each year.
    let year = |year: i32| Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
    };
    let years = [year(2024), year(2025)];
    let lifetime = ["lifetime_deep".to_string()];
    let (result, visits) = count_visits(|| {
        dense.execute_lifetime(
            &years,
            vec![batch([0, 0], [0, 0, 0]), batch([0, 0], [0, 0, 0])],
            &lifetime,
        )
    });
    let result = result.expect("every year reaches the innermost arm");
    assert!(
        visits <= 281 * 41,
        "dense evaluated {visits} nodes over a lifetime"
    );
    assert_eq!(integers(&result, "lifetime_deep"), vec![10, 14]);
    let lifetime_error = dense
        .execute_lifetime(
            &years,
            vec![batch([0, 1], [0, 0, 0]), batch([0, 1], [0, 0, 0])],
            &lifetime,
        )
        .expect_err("dense fails the uncovered household's lifetime")
        .to_string();
    assert!(
        lifetime_error.contains("`lifetime_deep` covers `x` = 1"),
        "{lifetime_error}"
    );

    // A row no arm covers fails with explain's error, at a household and at
    // a member.
    let program = serde_json::to_value(&artifact.program).expect("program serializes");
    let explained = answer(
        "explain",
        &program,
        ("x", json!({"kind": "integer", "value": 1})),
    )
    .expect_err("explain fails the uncovered household");
    let dense_error = dense
        .execute(&period, batch([1, 0], [0, 0, 0]), &["deep".to_string()])
        .expect_err("dense fails the uncovered household")
        .to_string();
    assert!(
        explained.contains(&dense_error),
        "dense: {dense_error}\nexplain: {explained}"
    );
    assert!(
        dense_error.contains("`deep` covers `x` = 1"),
        "{dense_error}"
    );
    let member_error = dense
        .execute(
            &period,
            batch([0, 0], [0, 1, 0]),
            &["members_deep".to_string()],
        )
        .expect_err("dense fails the household of the uncovered member")
        .to_string();
    assert!(
        member_error.contains("`member_deep` covers `x` = 1"),
        "{member_error}"
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

#[test]
fn a_match_with_only_a_wildcard_is_refused_for_the_subject_it_would_drop() {
    in_capped_child("formula_match_with_only_a_wildcard");
}

#[test]
fn dense_compiles_match_fallbacks_in_last_arms_once() {
    in_capped_child("dense_match_fallbacks_in_last_arms");
}

/// Whether process `pid` still exists, as `ps` sees it.
fn is_running(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A child whose memory cannot be read is stopped, at once, and reported:
/// not left to run unmeasured.
#[test]
fn a_child_whose_memory_cannot_be_read_is_stopped() {
    if std::env::var(CASE).as_deref() == Ok("unread") {
        std::thread::sleep(TIMEOUT);
        return;
    }
    let child = AtomicU32::new(0);
    let started = Instant::now();
    let outcome = std::panic::catch_unwind(|| {
        run_capped_reading(
            "a_child_whose_memory_cannot_be_read_is_stopped",
            "unread",
            |pid| {
                child.store(pid, Ordering::SeqCst);
                Err("no reading".to_string())
            },
        );
    });
    let pid = child.load(Ordering::SeqCst);
    assert_ne!(pid, 0, "the child was never read");
    if is_running(pid) {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        panic!("the unmeasured child {pid} was left running");
    }
    let message = outcome
        .expect_err("the unmeasured child is stopped")
        .downcast::<String>()
        .map(|message| *message)
        .unwrap_or_default();
    assert!(
        message.contains("could not have its memory read: no reading"),
        "{message}"
    );
    assert!(
        started.elapsed() < TIMEOUT / 2,
        "stopped only after {:?}",
        started.elapsed()
    );
}

/// A read whose process exits but leaves its output open, here to a
/// background `sleep`, or never exits, fails at `READ_TIMEOUT` instead of
/// waiting for it.
#[test]
fn a_memory_read_that_does_not_finish_times_out() {
    for script in ["sleep 30 & echo 1", "sleep 30"] {
        let started = Instant::now();
        let error = read_size("sh", &["-c", script]).expect_err("the read times out");
        let took = started.elapsed();
        assert!(
            error.contains("did not answer within"),
            "`{script}`: {error}"
        );
        assert!(
            took >= READ_TIMEOUT && took < READ_TIMEOUT * 3,
            "`{script}` took {took:?}"
        );
    }
    assert_eq!(read_size("sh", &["-c", "echo 42"]), Ok(42));
    assert!(read_size("sh", &["-c", "echo many"]).is_err());
    assert!(read_size("sh", &["-c", "exit 3"]).is_err());
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
