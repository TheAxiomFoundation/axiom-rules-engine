use std::env;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionRequest, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::{
    ARTIFACT_FORMAT_VERSION, CompiledProgramArtifact, CorpusProvisionIndex, compile_summary_lines,
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
            "capabilities" => {
                if args.next().is_some() {
                    return Err("`capabilities` takes no arguments".into());
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "engine_version": env!("CARGO_PKG_VERSION"),
                        "artifact_format_version": ARTIFACT_FORMAT_VERSION,
                    }))?
                );
                return Ok(());
            }
            "--help" | "-h" | "help" => {
                println!("{TOP_USAGE}");
                return Ok(());
            }
            "compile" => return run_compile(args.collect(), false),
            "compile-composed" => return run_compile(args.collect(), true),
            "run-compiled" => return run_compiled(args.collect()),
            #[cfg(feature = "schema")]
            "emit-schemas" => return run_emit_schemas(args.collect()),
            "migrate" => return run_migrate(args.collect()),
            _ => return Err(format!("unknown command `{command}`\n\n{TOP_USAGE}").into()),
        }
    }

    // Reading a request from stdin is the documented pipeline entry point, but a person who
    // runs the bare binary got `EOF while parsing a value at line 1 column 0`, which reads as
    // a crash rather than as "I expected JSON on stdin". Only consume stdin when it is
    // actually piped (#120).
    if io::stdin().is_terminal() {
        println!("{TOP_USAGE}");
        return Ok(());
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
    use super::{COMPILE_USAGE, TOP_USAGE, version_line};

    #[test]
    fn version_line_uses_package_version() {
        assert_eq!(version_line(), "axiom-rules-engine 0.2.1");
    }

    /// Top-level help must name every command `run` dispatches on, so adding a command
    /// without documenting it fails here rather than silently leaving users without a way
    /// to discover it (#120).
    #[test]
    fn top_usage_lists_every_dispatched_command() {
        for command in [
            "compile",
            "compile-composed",
            "run-compiled",
            "emit-schemas",
            "capabilities",
            "version",
            "help",
        ] {
            assert!(
                TOP_USAGE.contains(command),
                "top-level usage does not mention `{command}`"
            );
        }
    }

    /// The two usage strings serve different levels; the top-level one must not simply be
    /// the compile-specific text, which is what made `--help` useless before (#120).
    #[test]
    fn top_usage_is_distinct_from_compile_usage() {
        assert_ne!(TOP_USAGE, COMPILE_USAGE);
        assert!(TOP_USAGE.starts_with("axiom-rules-engine — RuleSpec compiler and runtime"));
    }
}

const TOP_USAGE: &str = "\
axiom-rules-engine — RuleSpec compiler and runtime

usage: axiom-rules-engine <command> [options]
       axiom-rules-engine < request.json

commands:
  compile           Compile an atomic RuleSpec module inside a canonical country
                    checkout into a program artifact.
  compile-composed  Compile an originless `module.kind: composition` produced by
                    axiom-compose.
  run-compiled      Execute a compiled artifact against a JSON request on stdin.
  emit-schemas      Write JSON Schemas for the wire types (schema feature only).
  migrate           Corpus-migration tooling; `migrate scan <path>...` inventories
                    hand-expanded exactly-one patterns (#152).
  capabilities      Print, as JSON, the engine version and the artifact format
                    version the loader enforces. Semver alone cannot answer
                    whether an artifact will load; this can.
  version           Print the engine version.
  help              Print this message.

Run `axiom-rules-engine <command> --help` for the options of a specific command.

With no command and a piped stdin, a self-contained JSON `ExecutionRequest` is read
from stdin and its response written to stdout:

  axiom-rules-engine run-compiled --artifact compiled.json < request.json

Note: the released v0.1.1 binary has a different compile interface from this one —
its `compile` takes only --program and --output, and it has no `compile-composed`.
See docs/install.md.";

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

/// `migrate scan <file-or-dir>...`: inventory hand-expanded exactly-one
/// patterns across RuleSpec sources. Detection lowers each module through the
/// real loader and walks the serialized spec (see `migrate`); files that do
/// not lower standalone are reported as unscanned, never as pattern-free.
fn run_migrate(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("scan") => {}
        Some("apply") => return run_migrate_apply(args.collect()),
        Some(other) => return Err(format!("unknown migrate subcommand `{other}`").into()),
        None => {
            return Err(
                "usage: migrate scan [--json] <file-or-dir>... | migrate apply [--json] [--write] <file-or-dir>..."
                    .into(),
            );
        }
    }
    let mut json = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    for arg in args {
        if arg == "--json" {
            json = true;
        } else {
            roots.push(PathBuf::from(arg));
        }
    }
    if roots.is_empty() {
        return Err("usage: migrate scan [--json] <file-or-dir>...".into());
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        collect_yaml_files(root, &mut files)?;
    }
    files.sort();

    let mut hits: Vec<(String, axiom_rules_engine::migrate::ExpandedExactlyOne)> = Vec::new();
    let mut unscanned: Vec<(String, String)> = Vec::new();
    let mut scanned = 0usize;
    for file in &files {
        let source = std::fs::read_to_string(file)?;
        if !source.contains("format: rulespec/v1") {
            continue;
        }
        let shown = file.display().to_string();
        match axiom_rules_engine::migrate::scan_source(&source) {
            Ok(found) => {
                scanned += 1;
                for hit in found {
                    hits.push((shown.clone(), hit));
                }
            }
            Err(error) => unscanned.push((shown, error.to_string())),
        }
    }

    if json {
        let payload = serde_json::json!({
            "scanned_modules": scanned,
            "hits": hits
                .iter()
                .map(|(file, hit)| serde_json::json!({
                    "file": file,
                    "rule": hit.rule,
                    "site": hit.site,
                    "arity": hit.arity,
                    "idiom": hit.idiom,
                }))
                .collect::<Vec<_>>(),
            "unscanned": unscanned
                .iter()
                .map(|(file, error)| serde_json::json!({"file": file, "error": error}))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    for (file, hit) in &hits {
        println!(
            "{file}: rule `{}` {} (arity {}, {})",
            hit.rule, hit.site, hit.arity, hit.idiom
        );
    }
    println!(
        "scanned {scanned} modules: {} hand-expanded exactly-one site(s), {} unscanned",
        hits.len(),
        unscanned.len(),
    );
    for (file, error) in &unscanned {
        eprintln!("unscanned {file}: {error}");
    }
    Ok(())
}

/// Recursively collect `.yaml` sources under `root`, skipping companion test
/// files and dot-directories.
fn collect_yaml_files(
    root: &std::path::Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if root.is_file() {
        files.push(root.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_yaml_files(&path, files)?;
        } else if name.ends_with(".yaml") && !name.ends_with(".test.yaml") {
            files.push(path);
        }
    }
    Ok(())
}

/// `migrate apply [--json] [--write] <file-or-dir>...`: rewrite detected
/// hand-expanded exactly-one sites to `exactly_one(...)`, gating every
/// rewrite on engine-executed behavioral equivalence over all 2^n base
/// assignments plus a rescan proving the pattern is gone. Dry-run by
/// default; `--write` saves. Sites with non-fact bases are reported for
/// hands and left untouched.
fn run_migrate_apply(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut json = false;
    let mut write = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--write" => write = true,
            other => roots.push(PathBuf::from(other)),
        }
    }
    if roots.is_empty() {
        return Err("usage: migrate apply [--json] [--write] <file-or-dir>...".into());
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        collect_yaml_files(root, &mut files)?;
    }
    files.sort();

    let mut reports: Vec<serde_json::Value> = Vec::new();
    let mut rewritten_files = 0usize;
    let mut gated_ok = 0usize;
    let mut manual_total = 0usize;
    for file in &files {
        let original = std::fs::read_to_string(file)?;
        if !original.contains("format: rulespec/v1") {
            continue;
        }
        let shown = file.display().to_string();
        let mut source = original.clone();
        loop {
            let (plans, manual) = match axiom_rules_engine::migrate::plan_rewrites(&source) {
                Ok(result) => result,
                Err(error) => {
                    reports.push(serde_json::json!({
                        "file": shown, "unscanned": error.to_string(),
                    }));
                    break;
                }
            };
            for site in &manual {
                manual_total += 1;
                reports.push(serde_json::json!({
                    "file": shown, "rule": site.rule, "site": site.site,
                    "manual": site.reason, "idiom": site.idiom,
                }));
            }
            let Some(plan) = plans.first() else { break };
            let version_index: usize = plan
                .site
                .trim_start_matches("versions[")
                .trim_end_matches("].expr")
                .parse()
                .map_err(|_| format!("unparseable site `{}`", plan.site))?;
            let old_formula = axiom_rules_engine::migrate::extract_version_formula(
                &source,
                &plan.rule,
                version_index,
            )
            .ok_or_else(|| format!("{shown}: cannot locate formula for `{}`", plan.rule))?;
            let gate = axiom_rules_engine::migrate::gate_rewrite(
                &old_formula,
                &plan.replacement,
                &plan.bases,
            )
            .map_err(|error| format!("{shown}: gate failed for `{}`: {error}", plan.rule))?;
            if !(gate.outcomes_match && gate.rescan_clean) {
                return Err(format!(
                    "{shown}: gate REFUSED `{}` ({} assignments, outcomes_match={}, rescan_clean={})",
                    plan.rule, gate.assignments, gate.outcomes_match, gate.rescan_clean
                )
                .into());
            }
            gated_ok += 1;
            reports.push(serde_json::json!({
                "file": shown, "rule": plan.rule, "site": plan.site,
                "idiom": plan.idiom, "arity": plan.arity,
                "replacement": plan.replacement,
                "gate": {"assignments": gate.assignments, "outcomes_match": true, "rescan_clean": true},
            }));
            source = axiom_rules_engine::migrate::replace_version_formula(
                &source,
                &plan.rule,
                version_index,
                &plan.replacement,
            )
            .ok_or_else(|| format!("{shown}: text surgery failed for `{}`", plan.rule))?;
        }
        if source != original {
            rewritten_files += 1;
            if write {
                std::fs::write(file, &source)?;
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": if write { "write" } else { "dry-run" },
                "rewrites_gated": gated_ok,
                "files_rewritten": rewritten_files,
                "manual_sites": manual_total,
                "reports": reports,
            }))?
        );
        return Ok(());
    }
    for report in &reports {
        println!("{report}");
    }
    println!(
        "{}: {gated_ok} rewrite(s) across {rewritten_files} file(s), {manual_total} manual site(s)",
        if write { "written" } else { "dry-run" },
    );
    Ok(())
}
