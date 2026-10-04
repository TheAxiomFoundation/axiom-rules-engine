//! `metadata.fast_path` across engine versions.
//!
//! Fast mode answers `sum_related` over a related derived value since it began
//! running relation aggregations row by row on the explain interpreter (#201),
//! so a fresh compile no longer lists that blocker. Artifacts compiled before
//! then still carry it, and artifact loading requires stored metadata to match
//! the embedded program, so the loader accepts `fast_path` computed under the
//! retired rule set as well, keeps it byte for byte, and reports a
//! `stale_fast_path_metadata` diagnostic.
//!
//! `fixtures/artifacts/related_derived_sums.pre-lazy-fast-path.compiled.json`
//! is the unmodified output of the engine at commit 6031295 (origin/main just
//! before this change, crate 0.2.2) compiling
//! `fixtures/artifacts/related_derived_sums.rulespec.yaml` with
//! `CompiledProgramArtifact::from_rulespec_str` and `write_json_file`. Never
//! regenerate it with a newer engine: it stands for artifacts already
//! published.

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionMode, ExecutionQuery, OutputValue, execute_compiled_request,
};
use axiom_rules_engine::compile::{CompiledProgramArtifact, FastPathMetadata};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};

const PRE_CHANGE_ARTIFACT: &str =
    include_str!("fixtures/artifacts/related_derived_sums.pre-lazy-fast-path.compiled.json");
const FIXTURE_SOURCE: &str = include_str!("fixtures/artifacts/related_derived_sums.rulespec.yaml");

const RETIRED_SUM_BLOCKER: &str =
    "fast mode does not yet support sum_related over related derived values";
const STALE: &str = "stale_fast_path_metadata";
const MISMATCH: &str = "metadata does not match the compiled program";

/// Only a relation sum over a related derived value: the shape the retired
/// blocker named, and nothing else bulk fast mode declines.
const RELATED_DERIVED_SUM: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: member_income
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income + unearned_income
  - name: household_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.member_income)
"#;

fn fixture_value() -> serde_json::Value {
    serde_json::from_str(PRE_CHANGE_ARTIFACT).expect("fixture is JSON")
}

fn stored_fast_path(value: &serde_json::Value) -> FastPathMetadata {
    serde_json::from_value(value["metadata"]["fast_path"].clone()).expect("fast_path parses")
}

fn load(value: &serde_json::Value) -> Result<CompiledProgramArtifact, String> {
    CompiledProgramArtifact::from_json_str(&value.to_string()).map_err(|error| error.to_string())
}

fn stale_diagnostics(artifact: &CompiledProgramArtifact) -> Vec<String> {
    artifact
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == STALE)
        .map(|diagnostic| diagnostic.message.clone())
        .collect()
}

fn assert_rejected(value: &serde_json::Value, case: &str) {
    let error = load(value).expect_err(case);
    assert!(
        error.contains(MISMATCH),
        "{case}: unexpected error: {error}"
    );
}

#[test]
fn an_artifact_compiled_before_the_change_still_loads_unchanged() {
    let loaded = CompiledProgramArtifact::from_json_str(PRE_CHANGE_ARTIFACT)
        .expect("an artifact the previous engine compiled must still load");

    // The stored metadata is kept exactly: re-serializing reproduces the
    // published bytes, so digests over the artifact do not move.
    let reserialized = serde_json::to_string_pretty(&loaded).expect("artifact serialises");
    assert_eq!(reserialized, PRE_CHANGE_ARTIFACT);
    let stored = stored_fast_path(&fixture_value());
    assert_eq!(loaded.metadata.fast_path, stored);
    assert!(!stored.compatible);
    assert_eq!(
        stored
            .blockers
            .iter()
            .filter(|blocker| blocker.ends_with(RETIRED_SUM_BLOCKER))
            .count(),
        5
    );

    let stale = stale_diagnostics(&loaded);
    assert_eq!(
        stale.len(),
        1,
        "one stale_fast_path_metadata diagnostic: {stale:?}"
    );
    let message = &stale[0];
    for expected in [
        "computed by engine 0.2.2",
        "(17 blockers, compatible: false)",
        "fast mode answers `sum_related` over related derived values",
        "this engine lists 12 blockers (compatible: false)",
        "no longer blocks `household_income`, `counted_income`",
        "kept unchanged",
        "Recompile with this engine",
    ] {
        assert!(
            message.contains(expected),
            "missing `{expected}` in: {message}"
        );
    }
    let diagnostic = loaded
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == STALE)
        .expect("stale diagnostic");
    assert_eq!(diagnostic.path, "<memory>");
}

/// The fixture is a genuine compile of its committed source: today's engine
/// reproduces every byte of it except the retired blockers, which it no longer
/// lists, and keeps every other blocker in place and in order.
#[test]
fn a_fresh_compile_drops_only_the_retired_blocker() {
    let fresh = CompiledProgramArtifact::from_rulespec_str(FIXTURE_SOURCE)
        .expect("fixture source compiles");
    assert!(
        fresh
            .metadata
            .fast_path
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("sum_related")),
        "fresh blockers: {:#?}",
        fresh.metadata.fast_path.blockers
    );
    assert!(stale_diagnostics(&fresh).is_empty());

    let mut expected = fixture_value();
    let stored = stored_fast_path(&expected);
    let kept = stored
        .blockers
        .into_iter()
        .filter(|blocker| !blocker.ends_with(RETIRED_SUM_BLOCKER))
        .collect::<Vec<_>>();
    assert_eq!(kept.len(), 12);
    expected["metadata"]["fast_path"]["blockers"] = serde_json::json!(kept);
    expected["metadata"]["fast_path"]["compatible"] = serde_json::json!(kept.is_empty());
    let actual = serde_json::to_value(&fresh).expect("fresh artifact serialises");
    assert_eq!(actual["engine_version"], expected["engine_version"]);
    assert_eq!(actual, expected);

    // A fresh artifact loads with no stale diagnostic and round-trips exactly.
    let source = serde_json::to_string_pretty(&fresh).expect("fresh artifact serialises");
    let loaded = CompiledProgramArtifact::from_json_str(&source).expect("fresh artifact loads");
    assert!(stale_diagnostics(&loaded).is_empty());
    assert_eq!(
        serde_json::to_string_pretty(&loaded).expect("artifact serialises"),
        source
    );
}

/// The loader accepts only `fast_path` some engine actually computed for the
/// embedded program; the retired rule set is not a wildcard.
#[test]
fn fast_path_metadata_no_engine_computed_is_still_rejected() {
    let base = fixture_value();
    let stored = stored_fast_path(&base);

    let mut flipped = base.clone();
    flipped["metadata"]["fast_path"]["compatible"] = serde_json::json!(true);
    assert_rejected(&flipped, "compatible flipped on retired blockers");

    let mut strategy = base.clone();
    strategy["metadata"]["fast_path"]["strategy"] = serde_json::json!("tampered");
    assert_rejected(&strategy, "tampered strategy");

    let mut partial = base.clone();
    let first_retired = stored
        .blockers
        .iter()
        .position(|blocker| blocker.ends_with(RETIRED_SUM_BLOCKER))
        .expect("fixture lists a retired blocker");
    let mut blockers = stored.blockers.clone();
    blockers.remove(first_retired);
    partial["metadata"]["fast_path"]["blockers"] = serde_json::json!(blockers);
    assert_rejected(&partial, "one retired blocker missing");

    let mut extra = base.clone();
    let mut blockers = stored.blockers.clone();
    blockers.push(format!("member_income: {RETIRED_SUM_BLOCKER}"));
    extra["metadata"]["fast_path"]["blockers"] = serde_json::json!(blockers);
    assert_rejected(&extra, "a retired blocker on a rule with no relation sum");

    let mut reordered = base.clone();
    let mut blockers = stored.blockers.clone();
    let last_date = blockers
        .iter()
        .rposition(|blocker| !blocker.ends_with(RETIRED_SUM_BLOCKER))
        .expect("fixture lists a date blocker");
    blockers.swap(first_retired, last_date);
    reordered["metadata"]["fast_path"]["blockers"] = serde_json::json!(blockers);
    assert_rejected(&reordered, "retired blockers out of traversal order");

    // A retired fast_path does not relax the rest of the metadata check.
    let mut order = base.clone();
    order["metadata"]["evaluation_order"] = serde_json::json!([]);
    assert_rejected(&order, "evaluation order tampered beside retired fast_path");
    let mut catalog = base.clone();
    catalog["metadata"]["input_catalog"] = serde_json::json!([]);
    assert_rejected(&catalog, "input catalog tampered beside retired fast_path");

    // Retired blockers on a program with no relation sum over a derived value:
    // no engine ever computed that.
    let plain = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: doubled
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount * 2
"#,
    )
    .expect("plain program compiles");
    let mut plain = serde_json::to_value(&plain).expect("artifact serialises");
    plain["metadata"]["fast_path"] = serde_json::json!({
        "strategy": "generic_bulk",
        "compatible": false,
        "blockers": [format!("doubled: {RETIRED_SUM_BLOCKER}")],
    });
    assert_rejected(&plain, "retired blocker on a program without the shape");
}

/// What a fresh compile no longer blocks, fast mode answers on its own path.
#[test]
fn fast_mode_answers_the_shape_the_retired_blocker_named() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RELATED_DERIVED_SUM)
        .expect("RuleSpec module compiles");
    assert_eq!(
        artifact.metadata.fast_path,
        FastPathMetadata {
            strategy: "generic_bulk".to_string(),
            compatible: true,
            blockers: Vec::new(),
        }
    );

    // The same program as the previous engine stamped it still loads.
    let mut legacy = serde_json::to_value(&artifact).expect("artifact serialises");
    legacy["metadata"]["fast_path"] = serde_json::json!({
        "strategy": "generic_bulk",
        "compatible": false,
        "blockers": [
            format!("household_income: {RETIRED_SUM_BLOCKER}"),
            format!("household_income: {RETIRED_SUM_BLOCKER}"),
        ],
    });
    let loaded = load(&legacy).expect("the previous engine's stamp loads");
    assert_eq!(stale_diagnostics(&loaded).len(), 1);
    assert!(
        stale_diagnostics(&loaded)[0].contains("this engine lists 0 blockers (compatible: true)")
    );
    assert!(stale_diagnostics(&loaded)[0].contains("computed by engine "));

    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let money = |entity_id: &str, name: &str, value: &str| InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: entity_id.to_string(),
        interval: interval.clone(),
        value: ScalarValueSpec::Decimal {
            value: value.to_string(),
        },
    };
    let member = |person: &str, household: &str| RelationRecordSpec {
        name: "member_of_household".to_string(),
        tuple: vec![person.to_string(), household.to_string()],
        interval: interval.clone(),
    };
    let dataset = DatasetSpec {
        inputs: vec![
            money("p1", "earned_income", "100"),
            money("p1", "unearned_income", "5"),
            money("p2", "earned_income", "40"),
            money("p2", "unearned_income", "0"),
            money("p3", "earned_income", "7"),
            money("p3", "unearned_income", "3"),
        ],
        relations: vec![member("p1", "h1"), member("p2", "h1"), member("p3", "h2")],
    };
    let queries = ["h1", "h2"]
        .into_iter()
        .map(|household| ExecutionQuery {
            assessment_date: None,
            entity_id: household.to_string(),
            period: period.clone(),
            outputs: vec!["household_income".to_string()],
        })
        .collect::<Vec<_>>();

    for artifact in [artifact, loaded] {
        let response = execute_compiled_request(
            artifact,
            CompiledExecutionRequest {
                mode: ExecutionMode::Fast,
                dataset: dataset.clone(),
                queries: queries.clone(),
                pins: Vec::new(),
                relation_binding: Default::default(),
            },
        )
        .expect("fast request succeeds");
        assert_eq!(response.metadata.actual_mode, ExecutionMode::Fast);
        assert_eq!(response.metadata.fallback_reason, None);
        let totals = response
            .results
            .iter()
            .map(|result| match &result.outputs["household_income"] {
                OutputValue::Scalar { value, .. } => serde_json::to_value(value).expect("value"),
                other => panic!("expected a scalar, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            totals,
            [
                serde_json::json!({"kind": "decimal", "value": "145"}),
                serde_json::json!({"kind": "decimal", "value": "10"}),
            ]
        );
    }
}

/// Unit aggregation digests the serialization of the source artifact it loads,
/// so a loader that rewrote stale `fast_path` would break every aggregation
/// artifact embedding one. The stored copy must survive the whole chain.
#[cfg(feature = "unit-derivation")]
#[test]
fn an_aggregation_artifact_over_a_pre_change_source_still_loads() {
    use axiom_rules_engine::unit_derivation::{
        CompiledAggregationArtifact, UnitDerivationDocumentRegistry,
    };

    const TARGET: &str = "nz:statutes/income_tax/family_scheme/tax_credits";
    struct Source;
    impl axiom_rules_engine::source::ModuleSource for Source {
        fn load(
            &self,
            target: &str,
        ) -> Result<Option<String>, axiom_rules_engine::source::SourceError> {
            Ok((target == TARGET).then(|| {
                let mut source =
                    include_str!("fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml")
                        .to_string();
                source.push_str(
                    r#"
  - name: carer_of_child
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Child, Carer]
  - name: carer_best_start_total
    kind: derived
    entity: Carer
    dtype: Money
    period: Year
    unit: NZD
    versions:
      - effective_from: '2026-04-01'
        formula: sum(carer_of_child.best_start_tax_credit_before_abatement)
"#,
                );
                source
            }))
        }
    }

    let fresh = CompiledProgramArtifact::from_rulespec_with_source(TARGET, &Source)
        .expect("extended NZ source compiles");
    assert!(
        fresh
            .metadata
            .fast_path
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("carer_best_start_total")),
        "fresh blockers: {:#?}",
        fresh.metadata.fast_path.blockers
    );
    let mut stamped = serde_json::to_value(&fresh).expect("artifact serialises");
    let mut legacy = stored_fast_path(&stamped);
    // The previous engine listed the sum once for the rule's formula and once
    // for its single version.
    legacy.blockers.extend([
        format!("carer_best_start_total: {RETIRED_SUM_BLOCKER}"),
        format!("carer_best_start_total: {RETIRED_SUM_BLOCKER}"),
    ]);
    legacy.compatible = false;
    stamped["metadata"]["fast_path"] = serde_json::to_value(&legacy).expect("serialises");
    let source_artifact = load(&stamped).expect("the previous engine's stamp loads");
    assert_eq!(source_artifact.metadata.fast_path, legacy);

    let plan = include_str!("fixtures/unit_derivation/nz_income_explorer_family.yaml");
    let mut registry = UnitDerivationDocumentRegistry::default();
    let compiled = registry
        .register_aggregation_source(plan, &source_artifact)
        .expect("aggregation compiles over the pre-change source")
        .to_json_pretty()
        .expect("aggregation artifact serialises");
    let reloaded = CompiledAggregationArtifact::from_json_str(&compiled)
        .expect("its source digest still matches after loading");
    assert_eq!(reloaded.source_artifact.metadata.fast_path, legacy);
    assert_eq!(reloaded.to_json_pretty().expect("serialises"), compiled);
}
