//! The first commands a new user runs, through the built binary (#120): help
//! lists the real commands, and a missing-input error names the input the way
//! the dataset must spell it, so pasting the name back in works.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ENGINE: &str = env!("CARGO_BIN_EXE_axiom-rules-engine");

const MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount + base_amount
"#;

const QUALIFIED_INPUT: &str = "us:policies/tests/first_contact#input.amount";
const QUALIFIED_OUTPUT: &str = "us:policies/tests/first_contact#adjusted_amount";

/// A module with one monthly money rule on `Household`. `header` holds any
/// lines between `format` and `rules`.
fn one_rule_module(header: &str, rule: &str, formula: &str) -> String {
    format!(
        r#"format: rulespec/v1
{header}rules:
  - name: {rule}
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
"#
    )
}

const SHARED_SLOT_COMPOSITION_HEADER: &str = "\
module:
  kind: composition
  summary: Two imports that read the same slot.
imports:
  - us:policies/a
  - us:policies/b
";

const SHARED_SLOT_OUTPUTS: &[&str] = &[
    "total_amount",
    "us:policies/a#a_amount",
    "us:policies/b#b_amount",
];

/// The commands this build dispatches. `emit-schemas` is listed in every
/// build; without the `schema` feature it explains itself as unavailable.
const COMMANDS: &[&str] = &[
    "compile",
    "compile-composed",
    "run",
    "run-compiled",
    "emit-schemas",
    "migrate",
    "capabilities",
    "version",
    "help",
];

#[test]
fn help_flags_and_command_list_the_real_commands() {
    for arg in ["--help", "-h", "help"] {
        let out = Command::new(ENGINE).arg(arg).output().expect("run engine");
        let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
        assert!(
            out.status.success(),
            "`{arg}` exits 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for command in COMMANDS {
            assert!(
                stdout
                    .lines()
                    .any(|line| line.trim_start().starts_with(&format!("{command} "))),
                "`{arg}` lists `{command}`:\n{stdout}"
            );
        }
    }
}

#[test]
fn every_listed_command_answers_its_own_help() {
    // The top-level help says `<command> --help` gives a command's options,
    // so every listed command must answer it. Without the `schema` feature,
    // `emit-schemas` cannot run; it says so instead of `unknown command`.
    for command in COMMANDS {
        for flag in ["--help", "-h"] {
            let out = Command::new(ENGINE)
                .args([command, flag])
                .stdin(Stdio::null())
                .output()
                .expect("run engine");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if *command == "emit-schemas" && !cfg!(feature = "schema") {
                assert!(!out.status.success(), "{stdout}");
                assert!(
                    stderr.contains("needs a build with the `schema` feature"),
                    "{stderr}"
                );
                continue;
            }
            assert!(out.status.success(), "`{command} {flag}` exits 0: {stderr}");
            assert!(stdout.contains("usage"), "`{command} {flag}`: {stdout}");
        }
    }
}

#[test]
fn no_argument_commands_refuse_arguments_around_their_help_flag() {
    // `version` and `capabilities` take no arguments. `--help` or `-h` alone
    // prints usage; any other argument, before the flag or after it, gets the
    // error an argument gets without the flag, and no usage.
    for command in ["version", "--version", "capabilities"] {
        let refused = Command::new(ENGINE)
            .args([command, "extra"])
            .output()
            .expect("run engine");
        assert!(!refused.status.success(), "`{command} extra` exits 1");
        let refusal = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(refusal.contains("takes no arguments"), "{refusal}");
        for flag in ["--help", "-h"] {
            for args in [
                [command, flag, "extra"],
                [command, "extra", flag],
                [command, flag, "-h"],
            ] {
                let out = Command::new(ENGINE)
                    .args(args)
                    .output()
                    .expect("run engine");
                let shown = args.join(" ");
                assert!(!out.status.success(), "`{shown}` exits 1");
                assert_eq!(
                    String::from_utf8_lossy(&out.stderr),
                    refusal,
                    "`{shown}` is refused like `{command} extra`"
                );
                assert!(
                    out.stdout.is_empty(),
                    "`{shown}` prints no usage: {}",
                    String::from_utf8_lossy(&out.stdout)
                );
            }
        }
    }
}

#[test]
fn unknown_command_names_the_help_command() {
    let out = Command::new(ENGINE)
        .arg("definitely-not-a-command")
        .output()
        .expect("run engine");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown command"), "{stderr}");
    assert!(
        stderr.contains("run-compiled"),
        "lists the commands: {stderr}"
    );
}

/// With no command and a terminal on stdin, the binary prints usage instead
/// of failing to parse an empty request. `script` gives the child a pty.
#[cfg(target_os = "linux")]
#[test]
fn bare_binary_on_a_terminal_prints_usage() {
    let Ok(out) = Command::new("script")
        .args(["-qec", ENGINE, "/dev/null"])
        .stdin(Stdio::null())
        .output()
    else {
        eprintln!("skipping: `script` is not installed");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("usage: axiom-rules-engine"), "{stdout}");
    assert!(!stdout.contains("EOF while parsing"), "{stdout}");
}

struct Fixture {
    root: PathBuf,
    artifact: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture_root(tag: &str) -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-first-contact-{tag}-{}",
            std::process::id()
        ))
}

/// Compile `program` with `command` against the fixture's `rulespec-us` root.
fn compile(command: &str, root: PathBuf, program: &Path) -> Fixture {
    let artifact = root.join("compiled.json");
    let out = Command::new(ENGINE)
        .args([command, "--program"])
        .arg(program)
        .arg("--rulespec-root")
        .arg(root.join("rulespec-us"))
        .arg("--output")
        .arg(&artifact)
        .output()
        .expect("compile runs");
    assert!(
        out.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Fixture { root, artifact }
}

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("temp dir");
    std::fs::write(path, contents).expect("file written");
}

fn compiled_fixture(tag: &str) -> Fixture {
    let root = fixture_root(tag);
    let program = root.join("rulespec-us/us/policies/tests/first_contact.yaml");
    write_file(&program, MODULE);
    compile("compile", root, &program)
}

/// An originless composition of `us:policies/a` and `us:policies/b`, whose
/// rules both read `slot`. The composition's own rule sums theirs and, when
/// `composition_reads_slot`, adds `slot` too.
fn shared_slot_fixture(tag: &str, slot: &str, composition_reads_slot: bool) -> Fixture {
    let root = fixture_root(tag);
    let policies = root.join("rulespec-us/us/policies");
    write_file(
        &policies.join("a.yaml"),
        &one_rule_module("", "a_amount", &format!("{slot} + 1")),
    );
    write_file(
        &policies.join("b.yaml"),
        &one_rule_module("", "b_amount", &format!("{slot} * 2")),
    );
    let total_formula = if composition_reads_slot {
        format!("a_amount + b_amount + {slot}")
    } else {
        "a_amount + b_amount".to_string()
    };
    let composition = root.join("generated/composition.yaml");
    write_file(
        &composition,
        &one_rule_module(
            SHARED_SLOT_COMPOSITION_HEADER,
            "total_amount",
            &total_formula,
        ),
    );
    compile("compile-composed", root, &composition)
}

fn run_compiled(artifact: &Path, mode: &str, input_name: Option<&str>, outputs: &[&str]) -> Output {
    let interval = serde_json::json!({"start": "2026-01-01", "end": "2026-01-31"});
    let inputs = input_name
        .map(|name| {
            vec![serde_json::json!({
                "name": name,
                "entity": "Household",
                "entity_id": "household-1",
                "interval": interval,
                "value": {"kind": "decimal", "value": "15"},
            })]
        })
        .unwrap_or_default();
    let request = serde_json::json!({
        "mode": mode,
        "dataset": {"inputs": inputs, "relations": []},
        "queries": [{
            "entity_id": "household-1",
            "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
            "outputs": outputs,
        }],
    });
    let mut child = Command::new(ENGINE)
        .args(["run-compiled", "--artifact"])
        .arg(artifact)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(request.to_string().as_bytes())
        .expect("request written");
    child.wait_with_output().expect("engine completes")
}

/// The name between the first pair of backticks after `missing input `.
fn missing_input_name(stderr: &str) -> &str {
    let rest = stderr
        .split_once("missing input `")
        .unwrap_or_else(|| panic!("a missing-input error: {stderr}"))
        .1;
    &rest[..rest.find('`').expect("closing backtick")]
}

#[test]
fn missing_input_error_names_an_input_the_dataset_accepts() {
    let fixture = compiled_fixture("missing-input");
    for mode in ["explain", "fast"] {
        let missing = run_compiled(&fixture.artifact, mode, None, &[QUALIFIED_OUTPUT]);
        assert!(!missing.status.success(), "{mode}: the input is missing");
        let stderr = String::from_utf8_lossy(&missing.stderr);
        let name = missing_input_name(&stderr);
        assert_eq!(name, QUALIFIED_INPUT, "{mode}: {stderr}");

        // Paste the printed name back in as the dataset input.
        let supplied = run_compiled(&fixture.artifact, mode, Some(name), &[QUALIFIED_OUTPUT]);
        assert!(
            supplied.status.success(),
            "{mode}: the printed name is accepted: {}",
            String::from_utf8_lossy(&supplied.stderr)
        );
        let response: serde_json::Value =
            serde_json::from_slice(&supplied.stdout).expect("response JSON");
        let output = &response["results"][0]["outputs"][QUALIFIED_OUTPUT];
        assert_eq!(output["value"]["value"], "25", "{mode}: {response}");
    }
}

#[test]
fn bare_slot_name_is_still_refused_for_a_qualified_program() {
    // The engine prints the qualified name because the bare one does not
    // bind; this pins that the refusal itself is unchanged.
    let fixture = compiled_fixture("bare-slot");
    let out = run_compiled(
        &fixture.artifact,
        "explain",
        Some("amount"),
        &[QUALIFIED_OUTPUT],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dataset input `amount` must use an absolute legal RuleSpec reference"),
        "{stderr}"
    );
}

/// Runs the shared-slot rule for one composition in both modes. The artifact
/// lists `request_names` for `slot`; a missing `slot` prints `printed`; that
/// name pasted back and every other listed name bind the one shared slot (so
/// `a_amount` is 16 and `b_amount` 30 whichever name supplies 15); each
/// `refused` name is refused.
fn assert_shared_slot_rule(
    fixture: &Fixture,
    slot: &str,
    request_names: &[&str],
    printed: &str,
    total: &str,
    refused: &[&str],
) {
    let artifact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.artifact).expect("artifact read"))
            .expect("artifact JSON");
    assert_eq!(
        artifact["metadata"]["input_catalog"],
        serde_json::json!([{
            "slot": slot,
            "canonical_request_name": printed,
            "request_names": request_names,
        }])
    );
    for mode in ["explain", "fast"] {
        let missing = run_compiled(&fixture.artifact, mode, None, SHARED_SLOT_OUTPUTS);
        assert!(!missing.status.success(), "{mode}: the input is missing");
        let stderr = String::from_utf8_lossy(&missing.stderr);
        let name = missing_input_name(&stderr);
        assert_eq!(name, printed, "{mode}: {stderr}");

        // The printed name pasted back first, then every other listed name.
        let others = request_names.iter().filter(|listed| **listed != name);
        for supplied_name in std::iter::once(name).chain(others.copied()) {
            let supplied = run_compiled(
                &fixture.artifact,
                mode,
                Some(supplied_name),
                SHARED_SLOT_OUTPUTS,
            );
            assert!(
                supplied.status.success(),
                "{mode}: `{supplied_name}` is accepted: {}",
                String::from_utf8_lossy(&supplied.stderr)
            );
            let response: serde_json::Value =
                serde_json::from_slice(&supplied.stdout).expect("response JSON");
            assert_eq!(
                response["metadata"]["actual_mode"], mode,
                "{mode} runs as {mode}: {response}"
            );
            let outputs = &response["results"][0]["outputs"];
            for (output, value) in [
                ("total_amount", total),
                ("us:policies/a#a_amount", "16"),
                ("us:policies/b#b_amount", "30"),
            ] {
                assert_eq!(
                    outputs[output]["value"]["value"], value,
                    "{mode}, `{supplied_name}`: {output}: {response}"
                );
            }
        }

        for refused_name in refused {
            let out = run_compiled(
                &fixture.artifact,
                mode,
                Some(refused_name),
                SHARED_SLOT_OUTPUTS,
            );
            assert!(!out.status.success(), "{mode}: `{refused_name}` is refused");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(&format!(
                    "dataset input `{refused_name}` must use an absolute legal RuleSpec reference"
                )),
                "{mode}: {stderr}"
            );
        }
    }
}

// Each shared-slot test runs `amount`, which sorts before the module names,
// and `wages`, which sorts after them, so the printed name cannot come from
// the catalog's order alone.

#[test]
fn shared_slot_missing_input_prints_the_first_module_name() {
    // Only the imports read the slot, so the catalog lists each module's name
    // and not the bare slot, and the error prints the first module's name.
    for (slot, request_names, unlisted) in [
        (
            "amount",
            ["us:policies/a#input.amount", "us:policies/b#input.amount"],
            "us:policies/c#input.amount",
        ),
        (
            "wages",
            ["us:policies/a#input.wages", "us:policies/b#input.wages"],
            "us:policies/c#input.wages",
        ),
    ] {
        let fixture = shared_slot_fixture(&format!("shared-slot-qualified-{slot}"), slot, false);
        assert_shared_slot_rule(
            &fixture,
            slot,
            &request_names,
            request_names[0],
            "46",
            &[slot, unlisted],
        );
    }
}

#[test]
fn shared_slot_missing_input_prints_the_bare_slot_the_composition_reads() {
    // The originless composition's own rule reads the slot, so the catalog
    // lists the bare slot beside both modules' names, and the error prints the
    // bare slot wherever it sorts.
    for (slot, request_names, unlisted) in [
        (
            "amount",
            [
                "amount",
                "us:policies/a#input.amount",
                "us:policies/b#input.amount",
            ],
            "us:policies/c#input.amount",
        ),
        (
            "wages",
            [
                "us:policies/a#input.wages",
                "us:policies/b#input.wages",
                "wages",
            ],
            "us:policies/c#input.wages",
        ),
    ] {
        let fixture = shared_slot_fixture(&format!("shared-slot-bare-{slot}"), slot, true);
        assert_shared_slot_rule(&fixture, slot, &request_names, slot, "61", &[unlisted]);
    }
}
