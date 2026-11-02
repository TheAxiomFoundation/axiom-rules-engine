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

fn compiled_fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-first-contact-{tag}-{}",
            std::process::id()
        ));
    let rulespec_root = root.join("rulespec-us");
    let program = rulespec_root.join("us/policies/tests/first_contact.yaml");
    let artifact = root.join("first_contact.compiled.json");
    std::fs::create_dir_all(program.parent().expect("parent")).expect("temp dir");
    std::fs::write(&program, MODULE).expect("module written");
    let out = Command::new(ENGINE)
        .args(["compile", "--program"])
        .arg(&program)
        .arg("--rulespec-root")
        .arg(&rulespec_root)
        .arg("--output")
        .arg(&artifact)
        .output()
        .expect("compile runs");
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Fixture { root, artifact }
}

fn run_compiled(artifact: &Path, mode: &str, input_name: Option<&str>) -> Output {
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
            "outputs": [QUALIFIED_OUTPUT],
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
        let missing = run_compiled(&fixture.artifact, mode, None);
        assert!(!missing.status.success(), "{mode}: the input is missing");
        let stderr = String::from_utf8_lossy(&missing.stderr);
        let name = missing_input_name(&stderr);
        assert_eq!(name, QUALIFIED_INPUT, "{mode}: {stderr}");

        // Paste the printed name back in as the dataset input.
        let supplied = run_compiled(&fixture.artifact, mode, Some(name));
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
    let out = run_compiled(&fixture.artifact, "explain", Some("amount"));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dataset input `amount` must use an absolute legal RuleSpec reference"),
        "{stderr}"
    );
}
