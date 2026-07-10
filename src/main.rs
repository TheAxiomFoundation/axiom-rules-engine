use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionRequest, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::{
    CompiledProgramArtifact, CorpusProvisionIndex, compile_summary_lines,
};
use axiom_rules_engine::rulespec::AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    if let Some(command) = args.next() {
        match command.as_str() {
            "compile" => return run_compile(args.collect()),
            "run-compiled" => return run_compiled(args.collect()),
            #[cfg(feature = "schema")]
            "emit-schemas" => return run_emit_schemas(args.collect()),
            _ => return Err(format!("unknown command `{command}`").into()),
        }
    }

    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let request: ExecutionRequest = serde_json::from_str(&input)?;
    let response = execute_request(request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

const COMPILE_USAGE: &str = "\
usage: axiom-rules-engine compile --program <rules.yaml> --output <compiled.json> [--exclusive-rulespec-roots] [--corpus-provisions <path>]...

  --program <path>            RuleSpec module or program YAML to compile.
  --output <path>             Where to write the compiled artifact JSON.
  --exclusive-rulespec-roots  Resolve canonical imports only through non-empty
                              AXIOM_RULESPEC_REPO_ROOTS entries. Never fall back
                              to importer ancestors, cwd, or sibling checkouts.
  --corpus-provisions <path>  Optional, repeatable. A corpus provisions JSONL
                              file, or a directory scanned recursively for
                              *.jsonl files in sorted path order. Each record's
                              citation_path -> source_url mapping resolves the
                              source_url of every rule/parameter whose origin
                              module declares that corpus_citation_path and
                              that has no inline source_url. When the same
                              citation path appears more than once, the record
                              loaded later wins. Purely a compile-time lookup:
                              same inputs always produce a byte-identical
                              artifact.";

fn run_compile(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut program_path: Option<PathBuf> = None;
    let mut output_path: Option<PathBuf> = None;
    let mut provisions_paths: Vec<PathBuf> = Vec::new();
    let mut exclusive_rulespec_roots = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--program" => {
                program_path = iter.next().map(PathBuf::from);
            }
            "--output" => {
                output_path = iter.next().map(PathBuf::from);
            }
            "--corpus-provisions" => {
                provisions_paths.push(
                    iter.next()
                        .map(PathBuf::from)
                        .ok_or("`--corpus-provisions` requires a path argument")?,
                );
            }
            "--exclusive-rulespec-roots" => {
                exclusive_rulespec_roots = true;
            }
            "--help" | "-h" => {
                println!("{COMPILE_USAGE}");
                return Ok(());
            }
            _ => {
                return Err(format!("unknown compile argument `{arg}`\n{COMPILE_USAGE}").into());
            }
        }
    }

    let program_path =
        program_path.ok_or("missing required `--program /path/to/rules` argument")?;
    let output_path =
        output_path.ok_or("missing required `--output /path/to/compiled.json` argument")?;

    if exclusive_rulespec_roots {
        // SAFETY: the single-threaded CLI has not spawned any worker threads;
        // compilation reads this process-local setting synchronously below.
        unsafe { env::set_var(AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE_ENV, "1") };
    }

    let mut artifact = CompiledProgramArtifact::from_rulespec_file(&program_path)?;
    if !provisions_paths.is_empty() {
        let provisions = CorpusProvisionIndex::from_paths(&provisions_paths)?;
        let resolved = artifact.resolve_source_urls(&provisions);
        println!("corpus_provisions_indexed: {}", provisions.len());
        println!("corpus_source_urls_resolved: {resolved}");
    }
    artifact.write_json_file(&output_path)?;
    println!("compiled_program: {}", output_path.display());
    for (key, value) in compile_summary_lines(&artifact) {
        println!("{key}: {value}");
    }
    Ok(())
}

fn run_compiled(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut artifact_path: Option<PathBuf> = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--artifact" => {
                artifact_path = iter.next().map(PathBuf::from);
            }
            _ => {
                return Err(format!("unknown run-compiled argument `{arg}`").into());
            }
        }
    }

    let artifact_path =
        artifact_path.ok_or("missing required `--artifact /path/to/compiled.json` argument")?;
    let artifact = CompiledProgramArtifact::from_json_file(&artifact_path)?;

    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let request: CompiledExecutionRequest = serde_json::from_str(&input)?;
    let response = execute_compiled_request(artifact, request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// `emit-schemas --out <dir>`: write the published JSON Schemas into `<dir>`.
/// The checked-in `schemas/` directory is the golden copy; the
/// `schemas_are_current` test regenerates in memory and fails on any drift, so
/// this subcommand is a convenience for refreshing that directory, not the
/// source of truth.
#[cfg(feature = "schema")]
fn run_emit_schemas(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut out_dir: Option<PathBuf> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--out" => {
                out_dir = iter.next().map(PathBuf::from);
            }
            _ => {
                return Err(format!("unknown emit-schemas argument `{arg}`").into());
            }
        }
    }
    let out_dir = out_dir.ok_or("missing required `--out /path/to/schemas` argument")?;
    let written = axiom_rules_engine::schema::write_all_to_dir(&out_dir)?;
    for path in written {
        println!("wrote {}", path.display());
    }
    Ok(())
}
