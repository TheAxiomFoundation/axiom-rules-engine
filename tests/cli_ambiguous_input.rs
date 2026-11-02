//! The ambiguous-input error names the inputs as the dataset spelled them
//! (#289). A composition imports `us:policies/a` and `us:policies/b`, and each
//! reads `wages`. When the composition's own rule does not read `wages`, the
//! dataset binder accepts only `us:policies/a#input.wages` and
//! `us:policies/b#input.wages` for that slot, so an error that names the bare
//! slot names an input the dataset cannot supply.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ENGINE: &str = env!("CARGO_BIN_EXE_axiom-rules-engine");
const A_WAGES: &str = "us:policies/a#input.wages";
const B_WAGES: &str = "us:policies/b#input.wages";

/// A module with one monthly money rule on `Household`. `header` holds any
/// lines between `format` and `rules`.
fn one_rule_module(header: &str, rule: &str, formula: &str) -> String {
    format!(
        "format: rulespec/v1\n{header}rules:\n  - name: {rule}\n    kind: derived\n    entity: Household\n    dtype: Money\n    period: Month\n    unit: USD\n    versions:\n      - effective_from: 2026-01-01\n        formula: {formula}\n"
    )
}

const COMPOSITION_HEADER: &str = "\
module:
  kind: composition
  summary: Two imports that read the same slot.
imports:
  - us:policies/a
  - us:policies/b
";

const OUTPUTS: &[&str] = &[
    "total_amount",
    "us:policies/a#a_amount",
    "us:policies/b#b_amount",
];

struct Fixture {
    root: PathBuf,
    artifact: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("temp dir");
    std::fs::write(path, contents).expect("file written");
}

/// Compile the shared-slot composition. Its own rule sums the imports' rules
/// and, when `composition_reads_wages`, adds `wages` too.
fn shared_slot_fixture(tag: &str, composition_reads_wages: bool) -> Fixture {
    let root = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-ambiguous-input-{tag}-{}",
            std::process::id()
        ));
    let fixture = Fixture {
        artifact: root.join("compiled.json"),
        root,
    };
    let policies = fixture.root.join("rulespec-us/us/policies");
    write_file(
        &policies.join("a.yaml"),
        &one_rule_module("", "a_amount", "wages + 1"),
    );
    write_file(
        &policies.join("b.yaml"),
        &one_rule_module("", "b_amount", "wages * 2"),
    );
    let total = if composition_reads_wages {
        "a_amount + b_amount + wages"
    } else {
        "a_amount + b_amount"
    };
    let composition = fixture.root.join("generated/composition.yaml");
    write_file(
        &composition,
        &one_rule_module(COMPOSITION_HEADER, "total_amount", total),
    );
    let out = Command::new(ENGINE)
        .args(["compile-composed", "--program"])
        .arg(&composition)
        .arg("--rulespec-root")
        .arg(fixture.root.join("rulespec-us"))
        .arg("--output")
        .arg(&fixture.artifact)
        .output()
        .expect("compile runs");
    assert!(
        out.status.success(),
        "compile-composed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fixture
}

/// The request body for `inputs`, each a (name, value) on `household-1`
/// over January 2026.
fn request_body(mode: &str, inputs: &[(&str, &str)]) -> serde_json::Value {
    let interval = serde_json::json!({"start": "2026-01-01", "end": "2026-01-31"});
    let inputs = inputs
        .iter()
        .map(|(name, value)| {
            serde_json::json!({
                "name": name,
                "entity": "Household",
                "entity_id": "household-1",
                "interval": interval,
                "value": {"kind": "decimal", "value": value},
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "mode": mode,
        "dataset": {"inputs": inputs, "relations": []},
        "queries": [{
            "entity_id": "household-1",
            "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
            "outputs": OUTPUTS,
        }],
    })
}

fn engine_with_stdin(args: &[&std::ffi::OsStr], stdin: &[u8]) -> Output {
    let mut child = Command::new(ENGINE)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("request written");
    child.wait_with_output().expect("engine completes")
}

/// Run the request through `run-compiled` and through `run`, which reads the
/// same program inline. Both paths must answer alike.
fn run_both_paths(fixture: &Fixture, mode: &str, inputs: &[(&str, &str)]) -> [Output; 2] {
    let body = request_body(mode, inputs);
    let compiled = engine_with_stdin(
        &[
            "run-compiled".as_ref(),
            "--artifact".as_ref(),
            fixture.artifact.as_os_str(),
        ],
        body.to_string().as_bytes(),
    );
    let artifact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.artifact).expect("artifact read"))
            .expect("artifact JSON");
    let mut inline = body;
    inline["program"] = artifact["program"].clone();
    let raw = engine_with_stdin(&["run".as_ref()], inline.to_string().as_bytes());
    [compiled, raw]
}

fn ambiguity(name: &str, also: Option<&str>) -> String {
    let also = also
        .map(|other| format!(" (`{name}` and `{other}` name the same input)"))
        .unwrap_or_default();
    format!(
        "ambiguous input `{name}` for entity `household-1`: records effective from 2026-01-01 have conflicting values{also}; merge or split the spells so one value applies at each start date\n"
    )
}

#[test]
fn conflicting_values_under_two_accepted_names_name_both() {
    // The issue's counterexample.
    let fixture = shared_slot_fixture("two-names", false);
    for mode in ["explain", "fast"] {
        for (inputs, expected) in [
            (
                [(A_WAGES, "15"), (B_WAGES, "100")],
                ambiguity(A_WAGES, Some(B_WAGES)),
            ),
            (
                [(B_WAGES, "100"), (A_WAGES, "15")],
                ambiguity(B_WAGES, Some(A_WAGES)),
            ),
            // One name twice: that name, as the dataset spelled it.
            (
                [(A_WAGES, "15"), (A_WAGES, "100")],
                ambiguity(A_WAGES, None),
            ),
        ] {
            for out in run_both_paths(&fixture, mode, &inputs) {
                assert_eq!(out.status.code(), Some(1), "{mode} {inputs:?}");
                assert_eq!(
                    String::from_utf8_lossy(&out.stderr),
                    expected,
                    "{mode} {inputs:?}"
                );
            }
        }
    }
}

#[test]
fn the_names_in_the_error_bind_when_supplied_alone() {
    // Each name the error prints is one the binder accepts: supplied alone,
    // it runs.
    let fixture = shared_slot_fixture("names-bind", false);
    for mode in ["explain", "fast"] {
        for name in [A_WAGES, B_WAGES] {
            for out in run_both_paths(&fixture, mode, &[(name, "15")]) {
                assert!(
                    out.status.success(),
                    "{mode} {name}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let response: serde_json::Value =
                    serde_json::from_slice(&out.stdout).expect("response JSON");
                let outputs = &response["results"][0]["outputs"];
                assert_eq!(outputs["us:policies/a#a_amount"]["value"]["value"], "16");
                assert_eq!(outputs["us:policies/b#b_amount"]["value"]["value"], "30");
                assert_eq!(response["metadata"]["actual_mode"], mode, "{response}");
            }
        }
    }
}

#[test]
fn equal_values_under_two_names_are_not_ambiguous() {
    let fixture = shared_slot_fixture("equal-values", false);
    for mode in ["explain", "fast"] {
        for out in run_both_paths(&fixture, mode, &[(A_WAGES, "15"), (B_WAGES, "15")]) {
            assert!(
                out.status.success(),
                "{mode}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn a_bare_slot_the_composition_reads_is_named_as_spelled() {
    // When the composition reads `wages` itself, the bare slot binds too,
    // and the error names whichever spellings conflict.
    let fixture = shared_slot_fixture("bare-slot", true);
    for mode in ["explain", "fast"] {
        for out in run_both_paths(&fixture, mode, &[("wages", "15"), (B_WAGES, "100")]) {
            assert_eq!(out.status.code(), Some(1), "{mode}");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                ambiguity("wages", Some(B_WAGES)),
                "{mode}"
            );
        }
    }
}

mod property {
    //! For any dataset of `wages` records under the two accepted names, the
    //! engine reports an ambiguity exactly when two records share a start
    //! date and differ in value, and it names the first such pair in dataset
    //! order, as the dataset spelled them. Explain and fast agree.
    use super::*;
    use axiom_rules_engine::api::{ApiError, CompiledExecutionRequest, execute_compiled_request};
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    use axiom_rules_engine::engine::EvalError;
    use proptest::prelude::*;

    /// (name, value, start day in January 2026)
    type Record = (&'static str, &'static str, u32);

    /// What the engine should report: the first and second names of the
    /// first conflicting pair, or nothing.
    fn expected(records: &[Record]) -> Option<(String, Option<String>, u32)> {
        let mut first_at = std::collections::HashMap::<u32, usize>::new();
        for (index, (name, value, day)) in records.iter().enumerate() {
            match first_at.get(day) {
                Some(&first) if records[first].1 != *value => {
                    let first_name = records[first].0;
                    return Some((
                        first_name.to_string(),
                        (*name != first_name).then(|| name.to_string()),
                        *day,
                    ));
                }
                Some(_) => {}
                None => {
                    first_at.insert(*day, index);
                }
            }
        }
        None
    }

    fn request(mode: &str, records: &[Record]) -> CompiledExecutionRequest {
        let inputs = records
            .iter()
            .map(|(name, value, day)| {
                serde_json::json!({
                    "name": name,
                    "entity": "Household",
                    "entity_id": "household-1",
                    "interval": {"start": format!("2026-01-{day:02}"), "end": "2026-01-31"},
                    "value": {"kind": "decimal", "value": value},
                })
            })
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({
            "mode": mode,
            "dataset": {"inputs": inputs, "relations": []},
            "queries": [{
                "entity_id": "household-1",
                "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
                "outputs": OUTPUTS,
            }],
        }))
        .expect("a compiled request")
    }

    fn record() -> impl Strategy<Value = Record> {
        (
            prop::sample::select(vec![A_WAGES, B_WAGES]),
            prop::sample::select(vec!["15", "100"]),
            prop::sample::select(vec![1u32, 15]),
        )
    }

    #[test]
    fn the_engine_names_the_first_conflicting_pair_as_spelled() {
        let fixture = shared_slot_fixture("property", false);
        let artifact =
            CompiledProgramArtifact::from_json_file(&fixture.artifact).expect("artifact loads");
        let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(256));
        runner
            .run(&prop::collection::vec(record(), 1..6), |records| {
                let expected = expected(&records);
                for mode in ["explain", "fast"] {
                    let result =
                        execute_compiled_request(artifact.clone(), request(mode, &records));
                    match (&result, &expected) {
                        (
                            Err(ApiError::Eval(EvalError::AmbiguousInput {
                                name,
                                other_name,
                                entity_id,
                                effective_from,
                            })),
                            Some((expected_name, expected_other, day)),
                        ) => {
                            prop_assert_eq!(name, expected_name, "{} {:?}", mode, records);
                            prop_assert_eq!(other_name, expected_other, "{} {:?}", mode, records);
                            prop_assert_eq!(entity_id, "household-1");
                            prop_assert_eq!(
                                effective_from.to_string(),
                                format!("2026-01-{day:02}")
                            );
                        }
                        (Err(ApiError::Eval(EvalError::AmbiguousInput { .. })), None) => {
                            prop_assert!(
                                false,
                                "{} {:?}: no conflict, got {:?}",
                                mode,
                                records,
                                result
                            );
                        }
                        (_, Some(_)) => {
                            prop_assert!(
                                false,
                                "{} {:?}: expected an ambiguity, got {:?}",
                                mode,
                                records,
                                result
                            );
                        }
                        (_, None) => {}
                    }
                }
                Ok(())
            })
            .expect("the property holds");
    }
}
