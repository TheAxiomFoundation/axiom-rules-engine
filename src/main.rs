use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionRequest, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::{
    CompiledProgramArtifact, CorpusProvisionIndex, compile_summary_lines,
};
use axiom_rules_engine::rulespec::CanonicalRuleSpecRoots;

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
            "--version" | "version" => {
                if args.next().is_some() {
                    return Err("`version` takes no arguments".into());
                }
                println!("{}", version_line());
                return Ok(());
            }
            "compile" => return run_compile(args.collect(), false),
            "compile-composed" => return run_compile(args.collect(), true),
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

fn version_line() -> String {
    format!("axiom-rules-engine {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::version_line;

    #[test]
    fn version_line_uses_package_version() {
        assert_eq!(version_line(), "axiom-rules-engine 0.1.0");
    }
}

const COMPILE_USAGE: &str = "\
usage: axiom-rules-engine compile --program <absolute rules.yaml> --rulespec-root <absolute rulespec-cc> [--rulespec-root <absolute rulespec-cc>]... --output <compiled.json> [--corpus-provisions <path>]...
       axiom-rules-engine compile-composed --program <absolute composition.yaml> --rulespec-root <absolute rulespec-cc> [--rulespec-root <absolute rulespec-cc>]... --output <compiled.json> [--corpus-provisions <path>]...

  --program <path>            `compile`: atomic module inside a configured root.
                              `compile-composed`: originless, external
                              `module.kind: composition` output from
                              axiom-compose with canonical imports only.
  --rulespec-root <path>      Required, repeatable exact canonical country
                              checkout named rulespec-<country>. This is the
                              sole filesystem import authority.
  --output <path>             Where to write the compiled artifact JSON.
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

fn run_compile(args: Vec<String>, composed: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut program_path: Option<PathBuf> = None;
    let mut output_path: Option<PathBuf> = None;
    let mut rulespec_roots: Vec<PathBuf> = Vec::new();
    let mut provisions_paths: Vec<PathBuf> = Vec::new();

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--program" => {
                program_path = iter.next().map(PathBuf::from);
            }
            "--output" => {
                output_path = iter.next().map(PathBuf::from);
            }
            "--rulespec-root" => {
                rulespec_roots.push(
                    iter.next()
                        .map(PathBuf::from)
                        .ok_or("`--rulespec-root` requires a path argument")?,
                );
            }
            "--corpus-provisions" => {
                provisions_paths.push(
                    iter.next()
                        .map(PathBuf::from)
                        .ok_or("`--corpus-provisions` requires a path argument")?,
                );
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

    let rulespec_roots = CanonicalRuleSpecRoots::new(&rulespec_roots)?;
    let mut artifact = if composed {
        CompiledProgramArtifact::from_composed_rulespec_file(&program_path, &rulespec_roots)?
    } else {
        CompiledProgramArtifact::from_rulespec_file(&program_path, &rulespec_roots)?
    };
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
