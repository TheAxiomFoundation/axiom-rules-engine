//! The canonical module-target grammar and the ledger path grammar.
//!
//! A canonical module target is `<jurisdiction>:<atomic-root>/<path>`. Its
//! path may hold `:` and the en dash (U+2013), as real modules do
//! (`us-la:statutes/47:297/4`, `us-nj:statutes/54a:4-7`,
//! `us:statutes/42/1437c–1`), and as the axiom-api ledger grammar certifies
//! (`PATH = [\w./:–-]+`). These tests pin, for every input:
//!
//! - acceptance: every target the widened grammar describes validates to
//!   itself, and imports of it resolve to it;
//! - rejection: whitespace, quotes, backslashes, `#`, empty and dot segments,
//!   a colon or a capital in the jurisdiction, non-atomic roots, and module
//!   extensions stay rejected;
//! - the filesystem mapping: path -> target -> path is the identity on every
//!   admitted file, so no two files share a target;
//! - agreement with the published JSON Schema patterns, with the axiom-api
//!   ledger grammar, and with the shared corpus Python also checks.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::rulespec::{
    CanonicalRuleSpecRoots, RuleSpecError, load_rulespec_file, resolve_import_target,
    validate_module_target,
};
use axiom_rules_engine::source::{FsModuleSource, ModuleSource, SourceError};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};

const IMPORTER: &str = "us:statutes/7/2014/a";
const ATOMIC_ROOTS: [&str; 4] = ["legislation", "policies", "regulations", "statutes"];
const EN_DASH: char = '\u{2013}';

static NONCE: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Real ids
// ---------------------------------------------------------------------------

/// Every rulespec-us module whose path holds a colon or an en dash
/// (rulespec-us origin/main f468c8dae). On the engine before this change all
/// seven were unaddressable: `module filename is not canonical` or `path does
/// not map injectively to an exact canonical module target`.
const REAL_COLON_AND_EN_DASH_TARGETS: [&str; 7] = [
    "us-la:statutes/47:294",
    "us-la:statutes/47:295",
    "us-la:statutes/47:297/4",
    "us-la:statutes/47:297/8",
    "us-la:statutes/47:32",
    "us-nj:statutes/54a:4-7",
    "us:statutes/42/1437c\u{2013}1",
];

#[test]
fn real_colon_and_en_dash_targets_are_canonical() {
    for target in REAL_COLON_AND_EN_DASH_TARGETS {
        assert_eq!(
            validate_module_target(target).expect("real module target is canonical"),
            target
        );
    }
}

#[test]
fn real_imports_of_colon_targets_resolve_to_their_module() {
    for (import, module) in [
        (
            "us-la:statutes/47:32#individual_income_tax_rate",
            "us-la:statutes/47:32",
        ),
        (
            "us-la:statutes/47:297/4#low_income_federal_adjusted_gross_income_limit",
            "us-la:statutes/47:297/4",
        ),
        (
            "us-nj:statutes/54a:4-7#nj_earned_income_tax_credit_percentage",
            "us-nj:statutes/54a:4-7",
        ),
        (
            "us:statutes/42/1437c\u{2013}1/b#annual_public_housing_agency_plan_requirement",
            "us:statutes/42/1437c\u{2013}1/b",
        ),
    ] {
        assert_eq!(
            resolve_import_target("us-la:policies/income_tax/pilot_liability_pipeline", import)
                .expect("real import resolves"),
            module
        );
        // A colon target can itself import: the importer passes validation.
        assert_eq!(
            resolve_import_target(module, IMPORTER).expect("colon importer validates"),
            IMPORTER
        );
    }
}

#[test]
fn council_jurisdictions_stay_canonical() {
    for target in [
        "uk-isle-of-wight:policies/isle-of-wight/council-tax-reduction",
        "uk-bath-and-north-east-somerset:policies/bath-and-north-east-somerset/council-tax-reduction",
        "uk-st-helens:policies/st-helens/council-tax-reduction",
        "be-vlg:regulations/employment/jobbonus",
    ] {
        assert_eq!(
            validate_module_target(target).expect("council target"),
            target
        );
    }
}

#[test]
fn space_paths_stay_rejected_as_encoding_debt() {
    // rulespec-us carries four us-nh modules below `He-W 7xx/` directories.
    // The ledger rejects the space too; renaming them is encoding work.
    for target in [
        "us-nh:regulations/he-w-700/He-W 704/04",
        "us-nh:regulations/he-w-700/He-W 704/05",
        "us-nh:regulations/he-w-700/He-W 709/0",
        "us-nh:regulations/he-w-700/He-W 722/01",
    ] {
        assert!(matches!(
            validate_module_target(target),
            Err(RuleSpecError::InvalidModuleTarget { .. })
        ));
    }
}

// ---------------------------------------------------------------------------
// Shared corpus: Rust, the published schema, and Python agree on every label
// ---------------------------------------------------------------------------

fn shared_corpus() -> (String, Vec<(String, bool, String)>) {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/module-target-grammar.json"
    ))
    .expect("shared corpus");
    let corpus: Value = serde_json::from_str(&text).expect("shared corpus JSON");
    let importer = corpus["importer"].as_str().expect("importer").to_string();
    let cases = corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            (
                case["import"].as_str().expect("import").to_string(),
                case["valid"].as_bool().expect("valid"),
                case["why"].as_str().expect("why").to_string(),
            )
        })
        .collect();
    (importer, cases)
}

#[test]
fn rust_import_resolution_matches_the_shared_corpus_labels() {
    let (importer, cases) = shared_corpus();
    assert!(cases.len() > 100, "corpus covers the named and probe cases");
    let mismatches = cases
        .iter()
        .filter(|(import, valid, _)| resolve_import_target(&importer, import).is_ok() != *valid)
        .map(|(import, valid, why)| format!("{import:?}: expected valid={valid} ({why})"))
        .collect::<Vec<_>>();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// A jurisdiction: two letters, then up to three hyphenated segments of at
/// least `min_segment` lowercase letters or digits.
fn jurisdiction(min_segment: usize) -> impl Strategy<Value = String> {
    let segment = proptest::string::string_regex(&format!("[a-z0-9]{{{min_segment},6}}"))
        .expect("segment regex");
    ("[a-z]{2}", vec(segment, 0..4)).prop_map(|(country, segments)| {
        std::iter::once(country)
            .chain(segments)
            .collect::<Vec<_>>()
            .join("-")
    })
}

/// One path character, weighted so `:` and the en dash appear often.
fn path_char(allow_tilde: bool) -> impl Strategy<Value = char> {
    let punctuation = if allow_tilde { "[_.~-]" } else { "[_.-]" };
    prop_oneof![
        6 => "[A-Za-z0-9]".prop_map(|s| s.chars().next().expect("one char")),
        2 => Just(':'),
        2 => Just(EN_DASH),
        2 => proptest::string::string_regex(punctuation)
            .expect("punctuation regex")
            .prop_map(|s| s.chars().next().expect("one char")),
    ]
}

fn has_module_extension(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".yaml") || lower.ends_with(".yml") || lower.ends_with(".test")
}

/// A path segment in the widened grammar, never `.` or `..`.
fn path_segment(allow_tilde: bool) -> impl Strategy<Value = String> {
    vec(path_char(allow_tilde), 1..9)
        .prop_map(|chars| chars.into_iter().collect::<String>())
        .prop_filter("dot aliases are not segments", |segment| {
            !matches!(segment.as_str(), "." | "..")
        })
}

/// A canonical target: `(jurisdiction, path segments after the root, target)`.
fn canonical_target(
    min_jurisdiction_segment: usize,
    allow_tilde: bool,
) -> impl Strategy<Value = String> {
    (
        jurisdiction(min_jurisdiction_segment),
        proptest::sample::select(ATOMIC_ROOTS.to_vec()),
        vec(path_segment(allow_tilde), 1..5),
    )
        .prop_filter("module filenames carry no extension", |(_, _, segments)| {
            !has_module_extension(segments.last().expect("one segment"))
        })
        .prop_map(|(jurisdiction, root, segments)| {
            format!("{jurisdiction}:{root}/{}", segments.join("/"))
        })
}

fn fragment() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_.-]{1,12}"
}

fn runner(seed: u64, default_cases: u32) -> TestRunner {
    let cases = std::env::var("AXIOM_MODULE_TARGET_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &bytes),
    )
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// For every target in the widened grammar (including `~` and one-character
/// subdivision segments, which the engine accepted before and keeps):
/// `validate_module_target(t) == Ok(t)`, and an import `t#f` with a valid
/// fragment resolves to `t`.
#[test]
fn every_generated_canonical_target_validates_to_itself() {
    let strategy = (canonical_target(1, true), fragment());
    if let Err(error) = runner(0x7a11, 512).run(&strategy, |(target, fragment)| {
        prop_assert_eq!(validate_module_target(&target).ok(), Some(target.clone()));
        prop_assert_eq!(
            resolve_import_target(IMPORTER, &target).ok(),
            Some(target.clone())
        );
        prop_assert_eq!(
            resolve_import_target(IMPORTER, &format!("{target}#{fragment}")).ok(),
            Some(target.clone())
        );
        prop_assert_eq!(
            resolve_import_target(&target, IMPORTER).ok(),
            Some(IMPORTER.to_string())
        );
        Ok(())
    }) {
        panic!("{error}");
    }
}

/// A way to break a canonical target. Each mutation yields a string the
/// engine must reject whatever target it is applied to.
#[derive(Clone, Debug)]
enum Breakage {
    /// Insert a character outside the grammar anywhere in the target.
    InsertForbidden(usize, char),
    /// Replace one path segment (the root included) with "", "." or "..".
    DotOrEmptySegment(usize, &'static str),
    /// Insert a colon anywhere in the jurisdiction, ends included.
    ColonInJurisdiction(usize),
    /// Uppercase one letter of the jurisdiction.
    UppercaseJurisdiction(usize),
    /// Append a module extension, in some ASCII case, to the filename.
    Extension(&'static str),
    /// Replace the atomic root with something that is not one.
    NonAtomicRoot(&'static str),
    /// Drop everything after the root, or leave a trailing slash.
    Truncate(bool),
}

const FORBIDDEN: [char; 32] = [
    ' ', '\t', '\n', '\r', '\u{a0}', '\u{2003}', '\u{3000}', '#', '"', '\'', '\\', '@', '+', '%',
    '!', '$', '&', '(', ')', '*', ',', ';', '=', '[', ']', '|', '?', '\u{f3}', '\u{2014}',
    '\u{2010}', '\u{2212}', '\u{ff1a}',
];

fn breakage() -> impl Strategy<Value = Breakage> {
    prop_oneof![
        (any::<usize>(), proptest::sample::select(FORBIDDEN.to_vec()))
            .prop_map(|(index, ch)| Breakage::InsertForbidden(index, ch)),
        (any::<usize>(), proptest::sample::select(vec!["", ".", ".."]))
            .prop_map(|(index, segment)| Breakage::DotOrEmptySegment(index, segment)),
        any::<usize>().prop_map(Breakage::ColonInJurisdiction),
        any::<usize>().prop_map(Breakage::UppercaseJurisdiction),
        proptest::sample::select(vec![".yaml", ".yml", ".test", ".YAML", ".Yml", ".TeSt"])
            .prop_map(Breakage::Extension),
        proptest::sample::select(vec!["programs", "Statutes", "statute", "manual", ""])
            .prop_map(Breakage::NonAtomicRoot),
        any::<bool>().prop_map(Breakage::Truncate),
    ]
}

fn apply_breakage(target: &str, breakage: &Breakage) -> String {
    let (jurisdiction, relative) = target.split_once(':').expect("canonical target");
    let mut segments = relative.split('/').map(str::to_string).collect::<Vec<_>>();
    match breakage {
        Breakage::InsertForbidden(index, ch) => {
            let boundaries = target
                .char_indices()
                .map(|(offset, _)| offset)
                .chain([target.len()])
                .collect::<Vec<_>>();
            let at = boundaries[index % boundaries.len()];
            format!("{}{ch}{}", &target[..at], &target[at..])
        }
        Breakage::DotOrEmptySegment(index, replacement) => {
            let at = index % segments.len();
            segments[at] = replacement.to_string();
            format!("{jurisdiction}:{}", segments.join("/"))
        }
        Breakage::ColonInJurisdiction(index) => {
            let at = index % (jurisdiction.len() + 1);
            format!(
                "{}:{}:{relative}",
                &jurisdiction[..at],
                &jurisdiction[at..]
            )
        }
        Breakage::UppercaseJurisdiction(index) => {
            let letters = jurisdiction
                .char_indices()
                .filter(|(_, ch)| ch.is_ascii_lowercase())
                .map(|(offset, _)| offset)
                .collect::<Vec<_>>();
            let at = letters[index % letters.len()];
            let mut upper = jurisdiction.to_string();
            upper.replace_range(at..at + 1, &jurisdiction[at..at + 1].to_ascii_uppercase());
            format!("{upper}:{relative}")
        }
        Breakage::Extension(extension) => format!("{target}{extension}"),
        Breakage::NonAtomicRoot(root) => {
            segments[0] = root.to_string();
            format!("{jurisdiction}:{}", segments.join("/"))
        }
        Breakage::Truncate(trailing_slash) => {
            if *trailing_slash {
                format!("{target}/")
            } else {
                format!("{jurisdiction}:{}", segments[0])
            }
        }
    }
}

/// For every canonical target and every breakage: the broken string is
/// rejected by `validate_module_target` and, as an import, by
/// `resolve_import_target`.
#[test]
fn every_broken_target_is_rejected() {
    let strategy = (canonical_target(1, true), breakage());
    if let Err(error) = runner(0xb4d, 1024).run(&strategy, |(target, breakage)| {
        let broken = apply_breakage(&target, &breakage);
        prop_assert!(
            validate_module_target(&broken).is_err(),
            "{:?} applied to {:?} gave accepted {:?}",
            breakage,
            target,
            broken
        );
        // `#` starts a fragment in an import, so it alone may form a valid
        // import; every other breakage is invalid as an import too.
        if !matches!(breakage, Breakage::InsertForbidden(_, '#')) {
            prop_assert!(
                resolve_import_target(IMPORTER, &broken).is_err(),
                "{:?} applied to {:?} gave accepted import {:?}",
                breakage,
                target,
                broken
            );
        }
        Ok(())
    }) {
        panic!("{error}");
    }
}

// ---------------------------------------------------------------------------
// Ledger differential (axiom-api src/legal-id-grammar.ts)
// ---------------------------------------------------------------------------

/// The axiom-api ledger grammar, copied verbatim from `src/legal-id-grammar.ts`
/// (TheAxiomFoundation/axiom-api#263, c868ea34). JSON Schema patterns use
/// ECMA-262 regex semantics, where `\w` is `[A-Za-z0-9_]`, as in the
/// TypeScript source (no `u` flag).
const LEDGER_JURISDICTION_SOURCE: &str = "[a-z]{2}(?:-[a-z0-9]{2,})*";
const LEDGER_PATH_SOURCE: &str = "[\\w./:\\u2013-]+";
const LEDGER_FRAGMENT_SOURCE: &str = "[\\w.-]+";

fn ledger_node_legal_id() -> jsonschema::Validator {
    jsonschema::draft7::new(&json!({
        "type": "string",
        "pattern": format!(
            "^{LEDGER_JURISDICTION_SOURCE}:{LEDGER_PATH_SOURCE}#{LEDGER_FRAGMENT_SOURCE}$"
        ),
    }))
    .expect("ledger grammar compiles")
}

/// Engine-only spellings: `~` in a path, or a one-character subdivision
/// segment. The engine accepted both before the ledger existed and keeps
/// them; no rulespec repository uses either.
fn uses_engine_only_spelling(target: &str) -> bool {
    let (jurisdiction, relative) = target.split_once(':').expect("canonical target");
    relative.contains('~') || jurisdiction.split('-').skip(1).any(|part| part.len() < 2)
}

#[test]
fn the_ledger_grammar_admits_the_real_ids() {
    let ledger = ledger_node_legal_id();
    for id in [
        "us-la:statutes/47:297/4#low_income_federal_adjusted_gross_income_limit",
        "us-nj:statutes/54a:4-7#nj_earned_income_tax_credit_percentage",
        "us:statutes/42/1437c\u{2013}1/b#annual_public_housing_agency_plan_requirement",
        "uk-isle-of-wight:policies/isle-of-wight/council-tax-reduction#rule",
        "us:statutes/7/2017/a#input.household_size",
    ] {
        assert!(ledger.is_valid(&json!(id)), "ledger rejects {id}");
        assert!(resolve_import_target(IMPORTER, id).is_ok(), "engine rejects {id}");
    }
    for id in [
        "us-nh:regulations/he-w-700/He-W 704/04#rule",
        "us:statutes/42/1437c\u{2014}1#rule",
        "us-la:statutes/47:294#a:b",
    ] {
        assert!(!ledger.is_valid(&json!(id)), "ledger admits {id}");
        assert!(resolve_import_target(IMPORTER, id).is_err(), "engine admits {id}");
    }
}

/// For every module-addressable node id: the ledger certifies it exactly when
/// the engine resolves it, except that the engine also accepts `~` and
/// one-character subdivision segments (deliberately wider). "Module
/// addressable" means the path starts at an atomic root and has no empty or
/// dot segment and no extension, which the ledger does not check and the
/// engine requires.
#[test]
fn engine_and_ledger_agree_on_module_addressable_ids() {
    let ledger = ledger_node_legal_id();
    let strategy = (
        prop_oneof![canonical_target(2, false), canonical_target(1, true)],
        fragment(),
    );
    if let Err(error) = runner(0x1ed9e7, 1024).run(&strategy, |(target, fragment)| {
        let id = format!("{target}#{fragment}");
        prop_assert!(resolve_import_target(IMPORTER, &id).is_ok(), "{}", id);
        prop_assert_eq!(
            ledger.is_valid(&json!(id)),
            !uses_engine_only_spelling(&target),
            "{}",
            id
        );
        Ok(())
    }) {
        panic!("{error}");
    }
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

fn temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-{label}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ))
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("module directory");
    std::fs::write(&path, text).expect("module file");
    path
}

fn parameter_module(name: &str, value: &str) -> String {
    format!(
        r#"format: rulespec/v1
rules:
  - name: {name}
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "{value}"
"#
    )
}

/// An importer shaped like `us-la:policies/income_tax/pilot_liability_pipeline`:
/// it imports symbols from colon and en-dash modules across jurisdictions.
const PIPELINE: &str = r#"format: rulespec/v1
imports:
  - us-la:statutes/47:32#individual_income_tax_rate
  - us-la:statutes/47:297/4#credit_limit
  - us-nj:statutes/54a:4-7
  - us:statutes/42/1437c–1#housing_amount
rules:
  - name: liability
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: individual_income_tax_rate + credit_limit + nj_amount + housing_amount + amount
"#;

const PIPELINE_TARGET: &str = "us-la:policies/income_tax/pilot_liability_pipeline";

fn colon_fixture(label: &str) -> (PathBuf, PathBuf, BTreeMap<&'static str, String>) {
    let temp = temp_dir(label);
    let root = temp.join("rulespec-us");
    let modules = BTreeMap::from([
        (
            "us-la:statutes/47:32",
            parameter_module("individual_income_tax_rate", "1"),
        ),
        (
            "us-la:statutes/47:297/4",
            parameter_module("credit_limit", "10"),
        ),
        ("us-nj:statutes/54a:4-7", parameter_module("nj_amount", "100")),
        (
            "us:statutes/42/1437c\u{2013}1",
            parameter_module("housing_amount", "1000"),
        ),
        (PIPELINE_TARGET, PIPELINE.to_string()),
    ]);
    for (target, text) in &modules {
        let (jurisdiction, relative) = target.split_once(':').expect("target");
        write(&root, &format!("{jurisdiction}/{relative}.yaml"), text);
    }
    (temp, root, modules)
}

struct InMemoryModuleSource(HashMap<String, String>);

impl ModuleSource for InMemoryModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).cloned())
    }
}

#[test]
fn colon_and_en_dash_modules_load_from_disk_and_map_back_to_their_targets() {
    let (temp, root, modules) = colon_fixture("colon-modules");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let source = FsModuleSource::new([&root]).expect("filesystem source");
    for (target, text) in &modules {
        let (jurisdiction, relative) = target.split_once(':').expect("target");
        let path = root.join(jurisdiction).join(format!("{relative}.yaml"));
        assert_eq!(
            roots.target_for_path(&path).expect("path maps to a target"),
            *target
        );
        assert_eq!(
            source.load(target).expect("target loads").as_deref(),
            Some(text.as_str())
        );
        load_rulespec_file(&path, &roots).expect("module lowers");
    }
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn an_importer_of_colon_modules_compiles_identically_from_disk_and_memory_and_executes() {
    let (temp, root, modules) = colon_fixture("colon-importer");
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    let (jurisdiction, relative) = PIPELINE_TARGET.split_once(':').expect("target");
    let pipeline = root.join(jurisdiction).join(format!("{relative}.yaml"));

    let from_disk =
        CompiledProgramArtifact::from_rulespec_file(&pipeline, &roots).expect("disk compile");
    let memory = InMemoryModuleSource(
        modules
            .iter()
            .map(|(target, text)| (target.to_string(), text.clone()))
            .collect(),
    );
    let from_memory = CompiledProgramArtifact::from_rulespec_with_source(PIPELINE_TARGET, &memory)
        .expect("in-memory compile (the wasm path)");
    assert_eq!(
        serde_json::to_value(&from_disk).expect("disk artifact JSON"),
        serde_json::to_value(&from_memory).expect("memory artifact JSON"),
    );

    let parameter_ids = from_disk
        .program
        .parameters
        .iter()
        .filter_map(|parameter| parameter.id.clone())
        .collect::<Vec<_>>();
    for id in [
        "us-la:statutes/47:32#individual_income_tax_rate",
        "us-la:statutes/47:297/4#credit_limit",
        "us-nj:statutes/54a:4-7#nj_amount",
        "us:statutes/42/1437c\u{2013}1#housing_amount",
    ] {
        assert!(
            parameter_ids.iter().any(|candidate| candidate == id),
            "{id} missing from {parameter_ids:?}"
        );
    }

    // The artifact survives its own load-time validation, ids included.
    let json = serde_json::to_string(&from_disk).expect("artifact JSON");
    let reloaded = CompiledProgramArtifact::from_json_str(&json).expect("artifact reloads");

    let output_id = format!("{PIPELINE_TARGET}#liability");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("date"),
        end: "2026-01-31".parse().expect("date"),
    };
    let response = execute_request(ExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        program: reloaded.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: format!("{PIPELINE_TARGET}#input.amount"),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Decimal {
                    value: "10000".to_string(),
                },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("program executes");
    let OutputValue::Scalar { id, value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("liability output")
    else {
        panic!("expected a scalar output");
    };
    assert_eq!(id.as_deref(), Some(output_id.as_str()));
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected a decimal");
    };
    assert_eq!(value, "11111");
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn the_cli_compiles_a_colon_module_and_its_importer() {
    let (temp, root, _) = colon_fixture("colon-cli");
    for (program, label) in [
        (root.join("us-la/statutes/47:297/4.yaml"), "colon-directory"),
        (root.join("us-nj/statutes/54a:4-7.yaml"), "colon-filename"),
        (
            root.join("us/statutes/42/1437c\u{2013}1.yaml"),
            "en-dash-filename",
        ),
        (
            root.join("us-la/policies/income_tax/pilot_liability_pipeline.yaml"),
            "importer",
        ),
    ] {
        let output = temp.join(format!("{label}.json"));
        let result = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
            .args([
                "compile",
                "--program",
                program.to_str().expect("utf8 program"),
                "--rulespec-root",
                root.to_str().expect("utf8 root"),
                "--output",
                output.to_str().expect("utf8 output"),
            ])
            .output()
            .expect("compile runs");
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(output.exists(), "{label}: artifact written");
    }
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn space_modules_stay_unaddressable_beside_loadable_colon_modules() {
    let (temp, root, _) = colon_fixture("space-debt");
    let space = write(
        &root,
        "us-nh/regulations/he-w-700/He-W 704/04.yaml",
        &parameter_module("allowance", "1"),
    );
    let roots = CanonicalRuleSpecRoots::new([&root])
        .expect("a legacy space path does not refuse the whole root");
    assert!(matches!(
        roots.target_for_path(&space),
        Err(RuleSpecError::InvalidFilesystemPath { .. })
    ));
    load_rulespec_file(root.join("us-la/statutes/47:297/4.yaml"), &roots)
        .expect("colon module still loads beside it");
    std::fs::remove_dir_all(temp).ok();
}

/// Two files whose paths differ only by a backslash inside a directory name
/// (`a\b/c.yaml` against `a/b/c.yaml`) must never share a target. Before the
/// round-trip check both mapped to `us:policies/a/b/c`, and loading the first
/// read the second.
#[cfg(unix)]
#[test]
fn a_backslash_directory_does_not_alias_nested_directories() {
    let temp = temp_dir("backslash-alias");
    let root = temp.join("rulespec-us");
    let aliased = write(&root, "us/policies/a\\b/c.yaml", &parameter_module("x", "1"));
    let nested = write(&root, "us/policies/a/b/c.yaml", &parameter_module("y", "2"));
    let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
    assert!(
        roots.target_for_path(&aliased).is_err(),
        "backslash directory must not map to {:?}",
        roots.target_for_path(&aliased)
    );
    assert_eq!(
        roots.target_for_path(&nested).expect("nested path maps"),
        "us:policies/a/b/c"
    );
    std::fs::remove_dir_all(temp).ok();
}

/// One generated path component for the filesystem property: lowercase only
/// (macOS volumes are usually case-insensitive), with `:`, the en dash, and
/// characters the grammar rejects.
fn file_component() -> impl Strategy<Value = String> {
    let ch = prop_oneof![
        8 => "[a-z0-9]".prop_map(|s| s.chars().next().expect("one char")),
        2 => Just(':'),
        2 => Just(EN_DASH),
        2 => proptest::sample::select(vec!['_', '-', '.', '~']),
        1 => proptest::sample::select(vec!['\\', ' ', '#', '"', '\'', '@', '\u{f3}', '\u{2014}']),
    ];
    vec(ch, 1..7)
        .prop_map(|chars| chars.into_iter().collect::<String>())
        .prop_filter("the filesystem has no . or .. entries", |component| {
            !matches!(component.as_str(), "." | "..")
        })
}

/// Whether a relative module path below `us-la/statutes/` (without `.yaml`)
/// is canonical: every component in the grammar's character set and the
/// filename free of module extensions.
fn expected_canonical(components: &[String]) -> bool {
    components.iter().all(|component| {
        component.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '~' | ':' | EN_DASH)
        })
    }) && !has_module_extension(components.last().expect("filename"))
}

/// For every generated set of module files: a file maps to a target exactly
/// when its path is canonical, the target is `us-la:statutes/<path>`, loading
/// that target reads that file (path -> target -> path is the identity), and
/// no two files share a target.
#[test]
fn filesystem_targets_round_trip_and_are_injective() {
    let strategy = vec(vec(file_component(), 1..4), 1..7);
    let cases = std::env::var("AXIOM_MODULE_TARGET_FS_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(48);
    let mut runner = runner(0xf5, cases);
    let result = runner.run(&strategy, |files| {
        let temp = temp_dir("fs-round-trip");
        let root = temp.join("rulespec-us");
        std::fs::create_dir_all(root.join("us-la/statutes")).expect("content root");
        let mut created = Vec::new();
        for (index, components) in files.iter().enumerate() {
            let relative = format!("us-la/statutes/{}.yaml", components.join("/"));
            let path = root.join(&relative);
            let made = path
                .parent()
                .map(|parent| std::fs::create_dir_all(parent).is_ok())
                .unwrap_or(false)
                && !path.exists()
                && std::fs::write(&path, format!("# file {index}\n")).is_ok();
            if made {
                created.push((path, components.clone(), format!("# file {index}\n")));
            }
        }
        let roots = CanonicalRuleSpecRoots::new([&root]).expect("canonical root");
        let source = FsModuleSource::new([&root]).expect("filesystem source");
        let mut seen: HashMap<String, PathBuf> = HashMap::new();
        let mut outcome = Ok(());
        for (path, components, text) in &created {
            let mapped = roots.target_for_path(path);
            let check = (|| {
                if expected_canonical(components) {
                    let expected = format!("us-la:statutes/{}", components.join("/"));
                    prop_assert_eq!(mapped.as_ref().ok(), Some(&expected), "{:?}", path);
                } else {
                    prop_assert!(mapped.is_err(), "{:?} mapped to {:?}", path, mapped);
                }
                if let Ok(target) = &mapped {
                    let loaded = source.load(target).expect("load");
                    prop_assert_eq!(
                        loaded.as_deref(),
                        Some(text.as_str()),
                        "{} must read back {:?}",
                        target,
                        path
                    );
                    if let Some(previous) = seen.insert(target.clone(), path.clone()) {
                        prop_assert!(
                            false,
                            "{:?} and {:?} share target {}",
                            previous,
                            path,
                            target
                        );
                    }
                }
                Ok(())
            })();
            if check.is_err() {
                outcome = check;
                break;
            }
        }
        std::fs::remove_dir_all(&temp).ok();
        outcome
    });
    if let Err(error) = result {
        panic!("{error}");
    }
}

// ---------------------------------------------------------------------------
// Published JSON Schema patterns agree with the validator
// ---------------------------------------------------------------------------

#[cfg(feature = "schema")]
mod schema_agreement {
    use super::*;
    use axiom_rules_engine::schema::all_schemas;

    fn schema(file_name: &str) -> Value {
        all_schemas()
            .into_iter()
            .find(|named| named.file_name == file_name)
            .unwrap_or_else(|| panic!("{file_name} is published"))
            .schema
    }

    fn module_validator() -> jsonschema::Validator {
        jsonschema::draft7::new(&schema("rulespec-module.v1.schema.json"))
            .expect("module schema compiles")
    }

    fn import_validates(validator: &jsonschema::Validator, import: &str) -> bool {
        validator.is_valid(&json!({ "format": "rulespec/v1", "imports": [import], "rules": [] }))
    }

    /// Collect every property subschema named `name` anywhere in `value`.
    fn property_schemas(value: &Value, name: &str, found: &mut Vec<Value>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(properties)) = map.get("properties")
                    && let Some(property) = properties.get(name)
                {
                    found.push(property.clone());
                }
                for nested in map.values() {
                    property_schemas(nested, name, found);
                }
            }
            Value::Array(items) => {
                for nested in items {
                    property_schemas(nested, name, found);
                }
            }
            _ => {}
        }
    }

    fn artifact_property_validators(name: &str) -> Vec<jsonschema::Validator> {
        let mut found = Vec::new();
        property_schemas(&schema("compiled-artifact.v2.schema.json"), name, &mut found);
        assert!(!found.is_empty(), "artifact schema has `{name}` properties");
        found
            .iter()
            .map(|property| jsonschema::draft7::new(property).expect("property schema compiles"))
            .collect()
    }

    fn is_identifier(value: &str) -> bool {
        let mut chars = value.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    /// What the artifact `id` patterns must admit: a resolvable import that
    /// carries a fragment.
    fn expected_rule_id(value: &str) -> bool {
        value.contains('#') && resolve_import_target(IMPORTER, value).is_ok()
    }

    /// What the input catalog patterns must admit: a bare identifier, or a
    /// canonical target followed by `#input.<identifier>`.
    fn expected_request_name(value: &str) -> bool {
        is_identifier(value)
            || value.split_once('#').is_some_and(|(target, fragment)| {
                fragment
                    .strip_prefix("input.")
                    .is_some_and(is_identifier)
                    && validate_module_target(target).is_ok()
            })
    }

    #[test]
    fn module_imports_schema_matches_the_shared_corpus_labels() {
        let validator = module_validator();
        let (_, cases) = shared_corpus();
        let mismatches = cases
            .iter()
            .filter(|(import, valid, _)| import_validates(&validator, import) != *valid)
            .map(|(import, valid, why)| format!("{import:?}: expected valid={valid} ({why})"))
            .collect::<Vec<_>>();
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn artifact_id_schemas_keep_null_and_real_ids() {
        for validator in artifact_property_validators("id") {
            assert!(validator.is_valid(&Value::Null), "rule ids stay nullable");
            for id in [
                "us-la:statutes/47:297/4#low_income_federal_adjusted_gross_income_limit",
                "us-nj:statutes/54a:4-7#nj_earned_income_tax_credit_percentage",
                "us:statutes/42/1437c\u{2013}1#housing_amount",
                "uk-isle-of-wight:policies/isle-of-wight/council-tax-reduction#rule",
            ] {
                assert!(validator.is_valid(&json!(id)), "{id}");
            }
        }
        for validator in artifact_property_validators("canonical_request_name") {
            assert!(validator.is_valid(&json!(
                "us-la:statutes/47:297/4#input.federal_adjusted_gross_income"
            )));
        }
    }

    /// Generated strings near the grammar's edges: canonical targets, broken
    /// ones, and either with a valid or invalid fragment.
    fn near_miss() -> impl Strategy<Value = String> {
        let target = prop_oneof![
            canonical_target(1, true),
            (canonical_target(1, true), breakage())
                .prop_map(|(target, breakage)| apply_breakage(&target, &breakage)),
        ];
        let suffix = prop_oneof![
            Just(String::new()),
            fragment().prop_map(|fragment| format!("#{fragment}")),
            "[A-Za-z_][A-Za-z0-9_]{0,8}".prop_map(|slot| format!("#input.{slot}")),
            "[A-Za-z0-9_.:#~ \u{2013}-]{0,6}".prop_map(|fragment| format!("#{fragment}")),
        ];
        (target, suffix).prop_map(|(target, suffix)| format!("{target}{suffix}"))
    }

    /// For every generated string: the published `imports` schema admits it
    /// exactly when `resolve_import_target` does; every artifact `id` pattern
    /// admits it exactly when it is a resolvable import with a fragment; and
    /// every input-catalog pattern admits it exactly when it is a bare
    /// identifier or `<canonical target>#input.<identifier>`.
    #[test]
    fn published_patterns_agree_with_the_validator() {
        let module = module_validator();
        let ids = artifact_property_validators("id");
        let canonical_request_names = artifact_property_validators("canonical_request_name");
        // `request_names` is an array; its item schema is checked by
        // validating a one-element array.
        let request_name_arrays = artifact_property_validators("request_names");
        if let Err(error) = runner(0x5c4e, 1024).run(&near_miss(), |value| {
            prop_assert_eq!(
                import_validates(&module, &value),
                resolve_import_target(IMPORTER, &value).is_ok(),
                "imports: {:?}",
                value
            );
            for validator in &ids {
                prop_assert_eq!(
                    validator.is_valid(&json!(value)),
                    expected_rule_id(&value),
                    "id: {:?}",
                    value
                );
            }
            for validator in &canonical_request_names {
                prop_assert_eq!(
                    validator.is_valid(&json!(value)),
                    expected_request_name(&value),
                    "canonical_request_name: {:?}",
                    value
                );
            }
            for validator in &request_name_arrays {
                prop_assert_eq!(
                    validator.is_valid(&json!([value])),
                    expected_request_name(&value),
                    "request_names: {:?}",
                    value
                );
            }
            Ok(())
        }) {
            panic!("{error}");
        }
    }
}
