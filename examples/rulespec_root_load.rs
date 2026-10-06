//! Written by rulespec-us tools/engine_root_load.py; see that file for the
//! contract. Do not edit here: the copy in rulespec-us is the source.
//!
//! usage: rulespec_root_load <absolute rulespec-<cc> root> <module list> <composed scratch dir>
//!
//! The module list holds one root-relative module path per line. Every module
//! yields one JSON line on stdout for the atomic surface; a module the engine
//! refuses on that surface because it itself declares `module.kind` yields a
//! second line for the composed surface, compiled from a copy under the
//! scratch directory (composed programs must sit outside every root).
use std::io::Write;
use std::path::{Path, PathBuf};

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::rulespec::{CanonicalRuleSpecRoots, RuleSpecError};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: rulespec_root_load <root> <module list> <composed scratch dir>");
        std::process::exit(2);
    }
    let root = PathBuf::from(&args[1]);
    let composed_dir = PathBuf::from(&args[3]);
    let roots = match CanonicalRuleSpecRoots::new([&root]) {
        Ok(roots) => roots,
        Err(error) => {
            println!("{}", serde_json::json!({ "root_error": error.to_string() }));
            std::process::exit(3);
        }
    };
    let list = std::fs::read_to_string(&args[2]).expect("read module list");
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for module in list.lines().filter(|line| !line.is_empty()) {
        let path = root.join(module);
        let atomic = CompiledProgramArtifact::from_rulespec_file(&path, &roots);
        let declares_kind = declares_own_kind(&atomic, &path, module);
        emit(&mut out, module, "atomic", &atomic);
        if declares_kind {
            let copy = composed_dir.join(module);
            std::fs::create_dir_all(copy.parent().expect("module path has a parent"))
                .expect("create composed scratch directory");
            std::fs::copy(&path, &copy).expect("copy composition outside the root");
            let composed = CompiledProgramArtifact::from_composed_rulespec_file(&copy, &roots);
            emit(&mut out, module, "composed", &composed);
        }
    }
    out.flush().expect("flush results");
}

/// True only when the module itself declares `module.kind`, not when an
/// import it pulls in does: the engine names the offending module in the
/// error, by file path for the loaded file or by canonical target otherwise.
fn declares_own_kind(
    result: &Result<CompiledProgramArtifact, CompileError>,
    path: &Path,
    module: &str,
) -> bool {
    let Err(CompileError::RuleSpec {
        error: RuleSpecError::ModuleKindOnAtomicSurface { path: offender },
        ..
    }) = result
    else {
        return false;
    };
    let target = module
        .strip_suffix(".yaml")
        .and_then(|stem| stem.split_once('/'))
        .map(|(jurisdiction, rest)| format!("{jurisdiction}:{rest}"));
    *offender == path.display().to_string() || Some(offender) == target.as_ref()
}

fn emit(
    out: &mut impl Write,
    module: &str,
    surface: &str,
    result: &Result<CompiledProgramArtifact, CompileError>,
) {
    let line = match result {
        Ok(_) => serde_json::json!({ "module": module, "surface": surface, "ok": true }),
        Err(error) => serde_json::json!({
            "module": module,
            "surface": surface,
            "ok": false,
            "error": error.to_string(),
        }),
    };
    writeln!(out, "{line}").expect("write result line");
}
