//! Source links in compiled artifacts: parameters keep their `source` /
//! `source_url` citations, every rule and parameter carries its origin
//! module's `source_verification.corpus_citation_path`, and the optional
//! compile-time corpus-provision join resolves citation paths to source
//! URLs deterministically.

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact, CorpusProvisionIndex};
use axiom_rules_engine::rulespec::load_rulespec_with_source;
use axiom_rules_engine::source::{ModuleSource, SourceError};

struct InMemoryModuleSource {
    modules: HashMap<String, String>,
}

impl InMemoryModuleSource {
    fn new(modules: &[(&str, &str)]) -> Self {
        Self {
            modules: modules
                .iter()
                .map(|(target, text)| (target.to_string(), text.to_string()))
                .collect(),
        }
    }
}

impl ModuleSource for InMemoryModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.modules.get(target).cloned())
    }
}

const CITED_MODULE: &str = r#"
format: rulespec/v1
module:
  id: us-co:regulations/10-ccr-2506-1/4.402.2
  source_verification:
    corpus_citation_path: us-co/regulation/10-ccr-2506-1/4.402.2
rules:
  - name: snap_income_annualization_months
    kind: parameter
    dtype: Count
    source: 10 CCR 2506-1, 4.402.2(A)(1)-(2)
    versions:
      - effective_from: 2025-10-01
        formula: "12"
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    source: USDA SNAP FY 2026 COLA maximum monthly allotment table
    source_url: https://www.fns.usda.gov/snap/fy-2026-cola
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: snap_average_monthly_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    source: 10 CCR 2506-1, 4.402.2 introductory paragraph
    versions:
      - effective_from: 2025-10-01
        formula: income_intended_to_cover_specific_period / snap_income_annualization_months
"#;

fn cited_artifact() -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(CITED_MODULE).expect("RuleSpec compiles")
}

#[test]
fn parameters_keep_source_citations() {
    let artifact = cited_artifact();
    let scalar = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_income_annualization_months")
        .expect("scalar parameter is present");
    assert_eq!(
        scalar.source.as_deref(),
        Some("10 CCR 2506-1, 4.402.2(A)(1)-(2)")
    );
    assert_eq!(scalar.source_url, None);

    let table = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_maximum_allotment_table")
        .expect("indexed parameter is present");
    assert_eq!(
        table.source.as_deref(),
        Some("USDA SNAP FY 2026 COLA maximum monthly allotment table")
    );
    assert_eq!(
        table.source_url.as_deref(),
        Some("https://www.fns.usda.gov/snap/fy-2026-cola")
    );
}

#[test]
fn rules_and_parameters_carry_their_module_corpus_citation_path() {
    let artifact = cited_artifact();
    for parameter in &artifact.program.parameters {
        assert_eq!(
            parameter.corpus_citation_path.as_deref(),
            Some("us-co/regulation/10-ccr-2506-1/4.402.2"),
            "parameter `{}` should carry its module's citation path",
            parameter.name
        );
    }
    for derived in &artifact.program.derived {
        assert_eq!(
            derived.corpus_citation_path.as_deref(),
            Some("us-co/regulation/10-ccr-2506-1/4.402.2"),
            "derived rule `{}` should carry its module's citation path",
            derived.name
        );
    }
}

#[test]
fn imported_rules_keep_their_own_module_citation_path() {
    // Two modules with different citation paths: every rule must carry the
    // path of the module it was declared in, not the root module's.
    let federal = r#"
format: rulespec/v1
module:
  id: us:policies/usda/snap/fy-2026-cola/maximum-allotments
  source_verification:
    corpus_citation_path: us/guidance/usda/fns/snap-fy2026-cola/page-1
rules:
  - name: snap_maximum_allotment_base
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "298"
"#;
    let state = r#"
format: rulespec/v1
module:
  id: us-co:regulations/10-ccr-2506-1/4.602
  source_verification:
    corpus_citation_path: us-co/regulation/10-ccr-2506-1/4.602
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
rules:
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: max(0, snap_maximum_allotment_base - net_income)
"#;
    let source = InMemoryModuleSource::new(&[
        (
            "us:policies/usda/snap/fy-2026-cola/maximum-allotments",
            federal,
        ),
        ("us-co:regulations/10-ccr-2506-1/4.602", state),
    ]);
    let program = load_rulespec_with_source("us-co:regulations/10-ccr-2506-1/4.602", &source)
        .expect("modules load");
    let artifact = CompiledProgramArtifact::compile(program).expect("program compiles");

    let parameter = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_maximum_allotment_base")
        .expect("imported parameter is present");
    assert_eq!(
        parameter.corpus_citation_path.as_deref(),
        Some("us/guidance/usda/fns/snap-fy2026-cola/page-1")
    );

    let derived = artifact
        .program
        .derived
        .iter()
        .find(|derived| derived.name == "snap_regular_month_allotment")
        .expect("root derived rule is present");
    assert_eq!(
        derived.corpus_citation_path.as_deref(),
        Some("us-co/regulation/10-ccr-2506-1/4.602")
    );
}

#[test]
fn modules_without_source_verification_emit_no_citation_path() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: snap_household_food_contribution_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2008-10-01
        formula: "0.30"
"#;
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");
    assert_eq!(artifact.program.parameters[0].corpus_citation_path, None);
}

const PROVISIONS_JSONL: &str = concat!(
    r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.402.2", "source_url": "https://www.sos.state.co.us/CCR/GenerateRulePdf.do?ruleVersionId=12299", "kind": "section"}"#,
    "\n",
    r#"{"citation_path": "us-co/regulation", "source_url": null, "kind": "collection"}"#,
    "\n\n",
    r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.602", "source_url": "https://www.sos.state.co.us/CCR/GenerateRulePdf.do?ruleVersionId=12299#4.602"}"#,
    "\n",
);

#[test]
fn corpus_provisions_join_resolves_source_urls_without_overriding_inline_urls() {
    let mut artifact = cited_artifact();
    let mut provisions = CorpusProvisionIndex::default();
    provisions
        .add_jsonl_str(PROVISIONS_JSONL, "<memory>")
        .expect("provisions parse");
    // The null-source_url collection record is skipped.
    assert_eq!(provisions.len(), 2);

    let resolved = artifact.resolve_source_urls(&provisions);
    // The scalar parameter and the derived rule resolve through the join;
    // the table parameter keeps its inline URL.
    assert_eq!(resolved, 2);
    let scalar = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_income_annualization_months")
        .expect("scalar parameter is present");
    assert_eq!(
        scalar.source_url.as_deref(),
        Some("https://www.sos.state.co.us/CCR/GenerateRulePdf.do?ruleVersionId=12299")
    );
    assert_eq!(
        scalar.source.as_deref(),
        Some("10 CCR 2506-1, 4.402.2(A)(1)-(2)"),
        "resolving the URL must not disturb the citation"
    );
    let table = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_maximum_allotment_table")
        .expect("indexed parameter is present");
    assert_eq!(
        table.source_url.as_deref(),
        Some("https://www.fns.usda.gov/snap/fy-2026-cola"),
        "an inline source_url always wins over the join"
    );
    let derived = artifact
        .program
        .derived
        .iter()
        .find(|derived| derived.name == "snap_average_monthly_income")
        .expect("derived rule is present");
    assert_eq!(
        derived.source_url.as_deref(),
        Some("https://www.sos.state.co.us/CCR/GenerateRulePdf.do?ruleVersionId=12299")
    );
}

#[test]
fn corpus_provisions_join_is_deterministic() {
    let compile_and_resolve = || {
        let mut artifact = cited_artifact();
        let mut provisions = CorpusProvisionIndex::default();
        provisions
            .add_jsonl_str(PROVISIONS_JSONL, "<memory>")
            .expect("provisions parse");
        artifact.resolve_source_urls(&provisions);
        // Strip the engine version stamp so this asserts on the join and
        // program serialization alone.
        let mut artifact = artifact;
        artifact.engine_version = None;
        serde_json::to_string_pretty(&artifact).expect("artifact serializes")
    };
    assert_eq!(
        compile_and_resolve(),
        compile_and_resolve(),
        "same inputs must produce a byte-identical artifact"
    );
}

#[test]
fn corpus_provisions_later_records_win_for_the_same_citation_path() {
    let mut provisions = CorpusProvisionIndex::default();
    provisions
        .add_jsonl_str(
            concat!(
                r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.402.2", "source_url": "https://example.org/older-snapshot"}"#,
                "\n",
                r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.402.2", "source_url": "https://example.org/newer-snapshot"}"#,
            ),
            "<memory>",
        )
        .expect("provisions parse");
    assert_eq!(
        provisions.source_url("us-co/regulation/10-ccr-2506-1/4.402.2"),
        Some("https://example.org/newer-snapshot")
    );
}

#[test]
fn corpus_provisions_reject_malformed_records_with_line_numbers() {
    let mut provisions = CorpusProvisionIndex::default();
    let error = provisions
        .add_jsonl_str(
            concat!(
                r#"{"citation_path": "us-co/regulation", "source_url": "https://example.org"}"#,
                "\n",
                "not json",
            ),
            "provisions.jsonl",
        )
        .expect_err("malformed line is rejected");
    let CompileError::ParseProvisionRecord { path, line, .. } = error else {
        panic!("expected ParseProvisionRecord, got {error:?}");
    };
    assert_eq!(path, "provisions.jsonl");
    assert_eq!(line, 2);
}

#[test]
fn corpus_provisions_load_from_files_and_directories() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-provisions-{nonce}"));
    let nested = root.join("us-co").join("regulation");
    fs::create_dir_all(&nested).expect("create provisions directory");
    // Dated snapshots: sorted path order loads the newer file last, so its
    // record wins for the shared citation path.
    fs::write(
        nested.join("2026-03-01-10-ccr-2506-1.jsonl"),
        concat!(
            r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.402.2", "source_url": "https://example.org/older-snapshot"}"#,
            "\n",
        ),
    )
    .expect("write older snapshot");
    fs::write(
        nested.join("2026-04-29-10-ccr-2506-1.jsonl"),
        concat!(
            r#"{"citation_path": "us-co/regulation/10-ccr-2506-1/4.402.2", "source_url": "https://example.org/newer-snapshot"}"#,
            "\n",
        ),
    )
    .expect("write newer snapshot");
    fs::write(nested.join("notes.txt"), "not provisions").expect("write non-jsonl file");

    let provisions = CorpusProvisionIndex::from_paths([&root]).expect("directory loads");
    assert_eq!(provisions.len(), 1);
    assert_eq!(
        provisions.source_url("us-co/regulation/10-ccr-2506-1/4.402.2"),
        Some("https://example.org/newer-snapshot")
    );

    let single = CorpusProvisionIndex::from_paths([nested.join("2026-03-01-10-ccr-2506-1.jsonl")])
        .expect("single file loads");
    assert_eq!(
        single.source_url("us-co/regulation/10-ccr-2506-1/4.402.2"),
        Some("https://example.org/older-snapshot")
    );

    fs::remove_dir_all(&root).expect("clean up provisions directory");
}
