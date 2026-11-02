//! Every command and subcommand answers a lone `--help` or `-h` with its usage
//! and exit 0, and writes nothing (#290). A help flag never stands in for an
//! option's value, and a help flag beside other arguments is refused rather
//! than printing usage and ignoring them.
//!
//! The walk reads the command list from `help` and the `migrate`
//! subcommands from `migrate --help`, so a command added to either is walked
//! too.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ENGINE: &str = env!("CARGO_BIN_EXE_axiom-rules-engine");
const HELP_FLAGS: [&str; 2] = ["--help", "-h"];

/// An empty directory to run the engine in, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("system temp directory has an exact path")
            .join(format!(
                "axiom-rules-engine-cli-help-{tag}-{}",
                std::process::id()
            ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Scratch(path)
    }

    fn entries(&self) -> Vec<String> {
        let mut entries = std::fs::read_dir(&self.0)
            .expect("scratch directory readable")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn engine_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(ENGINE)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("run engine")
}

/// The commands `help` lists: the first word of each line under `commands:`
/// indented by exactly two spaces.
fn listed_commands() -> Vec<String> {
    let out = Command::new(ENGINE)
        .arg("help")
        .output()
        .expect("run engine");
    let usage = String::from_utf8(out.stdout).expect("utf8 help");
    let commands = usage
        .split_once("commands:\n")
        .expect("help has a commands section")
        .1;
    commands
        .lines()
        .take_while(|line| !line.is_empty())
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .map(|line| {
            line.split_whitespace()
                .next()
                .expect("a command name")
                .to_string()
        })
        .collect()
}

/// The `migrate` subcommands `migrate --help` names, in order.
fn migrate_subcommands() -> Vec<String> {
    let out = Command::new(ENGINE)
        .args(["migrate", "--help"])
        .output()
        .expect("run engine");
    let usage = String::from_utf8(out.stdout).expect("utf8 help");
    let mut subcommands = Vec::new();
    for word in usage.split("migrate ").skip(1) {
        let name = word.split_whitespace().next().unwrap_or_default();
        if name.chars().all(|c| c.is_ascii_lowercase()) && !subcommands.contains(&name.to_string())
        {
            subcommands.push(name.to_string());
        }
    }
    subcommands
}

/// Every command path the binary dispatches: each listed command, and each
/// `migrate` subcommand.
fn command_paths() -> Vec<Vec<String>> {
    let mut paths = Vec::new();
    for command in listed_commands() {
        if command == "migrate" {
            for subcommand in migrate_subcommands() {
                paths.push(vec![command.clone(), subcommand]);
            }
        }
        paths.push(vec![command]);
    }
    paths
}

/// A build without the `schema` feature lists `emit-schemas` but cannot run it.
fn unavailable(path: &[String]) -> bool {
    path[0] == "emit-schemas" && !cfg!(feature = "schema")
}

#[test]
fn the_walk_covers_every_known_command_path() {
    let paths = command_paths();
    for expected in [
        &["compile"][..],
        &["compile-composed"],
        &["run"],
        &["run-compiled"],
        &["emit-schemas"],
        &["migrate"],
        &["migrate", "scan"],
        &["migrate", "apply"],
        &["migrate", "artifact"],
        &["capabilities"],
        &["version"],
        &["help"],
    ] {
        assert!(
            paths.iter().any(|path| path == expected),
            "the walk misses {expected:?}: {paths:?}"
        );
    }
}

#[test]
fn every_command_path_prints_its_help_and_writes_nothing() {
    for path in command_paths() {
        if unavailable(&path) {
            continue;
        }
        for flag in HELP_FLAGS {
            let scratch = Scratch::new("lone");
            let mut args = path.iter().map(String::as_str).collect::<Vec<_>>();
            args.push(flag);
            let shown = args.join(" ");
            let out = engine_in(&scratch.0, &args);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(0), "`{shown}`: {stderr}");
            assert!(
                stderr.is_empty(),
                "`{shown}` writes nothing on stderr: {stderr}"
            );
            // A subcommand prints its own usage, not its parent's.
            let usage_of = path.join(" ");
            assert!(
                stdout.starts_with("usage: ") || path == ["help"],
                "`{shown}` prints usage:\n{stdout}"
            );
            assert!(
                stdout.contains(&format!("axiom-rules-engine {usage_of}"))
                    || (path == ["help"] && stdout.contains("usage: axiom-rules-engine <command>")),
                "`{shown}` prints the usage of `{usage_of}`:\n{stdout}"
            );
            assert_eq!(
                scratch.entries(),
                Vec::<String>::new(),
                "`{shown}` writes nothing"
            );
        }
    }
}

#[test]
fn a_help_flag_beside_other_arguments_is_refused() {
    // One rule for every option-parsing command: a help flag prints usage
    // only on its own. `version` and `capabilities` take no arguments and
    // refuse extras in their own way; `help` prints the top-level usage
    // whatever follows it.
    for path in command_paths() {
        if unavailable(&path) || matches!(path[0].as_str(), "version" | "capabilities" | "help") {
            continue;
        }
        for flag in HELP_FLAGS {
            let command = path.join(" ");
            for extra in [
                vec![flag, "extra"],
                vec!["extra", flag],
                vec![flag, flag],
                vec!["--json", flag],
            ] {
                let scratch = Scratch::new("extra");
                let mut args = path.iter().map(String::as_str).collect::<Vec<_>>();
                args.extend(&extra);
                let shown = args.join(" ");
                let out = engine_in(&scratch.0, &args);
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert_eq!(out.status.code(), Some(1), "`{shown}` exits 1: {stderr}");
                assert!(
                    out.stdout.is_empty(),
                    "`{shown}` prints no usage: {}",
                    String::from_utf8_lossy(&out.stdout)
                );
                // An argument the command does not know may be refused
                // first; otherwise the help flag is.
                assert!(
                    stderr == format!("`{command} {flag}` takes no other arguments\n")
                        || stderr.contains("unknown"),
                    "`{shown}`: {stderr}"
                );
                assert_eq!(
                    scratch.entries(),
                    Vec::<String>::new(),
                    "`{shown}` writes nothing"
                );
            }
            // The help flag first and only known arguments after it: always
            // the help-flag refusal.
            let scratch = Scratch::new("after");
            let mut args = path.iter().map(String::as_str).collect::<Vec<_>>();
            args.extend([flag, flag]);
            let out = engine_in(&scratch.0, &args);
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                format!("`{command} {flag}` takes no other arguments\n"),
                "`{}`",
                args.join(" ")
            );
        }
    }
}

/// Each option that takes a value, by command path.
fn value_options() -> Vec<(Vec<&'static str>, &'static str)> {
    let mut options = Vec::new();
    for command in ["compile", "compile-composed"] {
        for option in [
            "--program",
            "--output",
            "--rulespec-root",
            "--corpus-provisions",
        ] {
            options.push((vec![command], option));
        }
    }
    options.push((vec!["run"], "--relation-binding"));
    for option in [
        "--artifact",
        "--relation-binding",
        "--accept-artifact-format",
    ] {
        options.push((vec!["run-compiled"], option));
    }
    if cfg!(feature = "schema") {
        options.push((vec!["emit-schemas"], "--out"));
    }
    for option in ["--artifact", "--output", "--relation-entities"] {
        options.push((vec!["migrate", "artifact"], option));
    }
    if cfg!(feature = "unit-derivation") {
        for option in [
            "--plan",
            "--source-artifact",
            "--output",
            "--accept-artifact-format",
        ] {
            options.push((vec!["compile-unit-aggregation"], option));
        }
        for option in ["--artifact", "--accept-artifact-format"] {
            options.push((vec!["run-unit-aggregation"], option));
        }
    }
    options
}

#[test]
fn a_help_flag_is_never_an_option_value() {
    for (path, option) in value_options() {
        for flag in HELP_FLAGS {
            let scratch = Scratch::new("value");
            let mut args = path.clone();
            args.extend([option, flag]);
            let shown = args.join(" ");
            let out = engine_in(&scratch.0, &args);
            assert_eq!(out.status.code(), Some(1), "`{shown}` exits 1");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                format!("`{option}` takes a value, not `{flag}`\n"),
                "`{shown}`"
            );
            assert!(out.stdout.is_empty(), "`{shown}` prints nothing");
            assert_eq!(
                scratch.entries(),
                Vec::<String>::new(),
                "`{shown}` writes nothing"
            );
        }
    }
}

/// The issue's example: `emit-schemas --out --help` wrote the schemas into
/// `./--help/` and exited 0.
#[cfg(feature = "schema")]
#[test]
fn emit_schemas_out_help_writes_no_directory() {
    let scratch = Scratch::new("emit");
    let out = engine_in(&scratch.0, &["emit-schemas", "--out", "--help"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!scratch.0.join("--help").exists());
    assert_eq!(scratch.entries(), Vec::<String>::new());
}
