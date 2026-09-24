//! Diagnostics and reference resolution must not depend on hash iteration
//! order. Each case below repeats in one process, where every run builds new
//! hash maps and sets with their own hasher keys, and asserts one outcome.

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{
    DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec, PeriodKindSpec, PeriodSpec,
    ProgramSpec, ScalarExprSpec, ScalarValueSpec, SpecError,
};

const CYCLIC_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: gamma
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: alpha + 1
  - name: alpha
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: beta + 1
  - name: beta
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: gamma + 1
"#;

#[test]
fn cyclic_dependency_diagnostic_lists_its_members_in_order() {
    for run in 0..32 {
        let error = CompiledProgramArtifact::from_rulespec_str(CYCLIC_RULESPEC)
            .expect_err("a dependency cycle does not compile");
        assert_eq!(
            error.to_string(),
            "cyclic derived dependency detected involving: alpha, beta, gamma",
            "run {run}"
        );
    }
}

fn literal_rule(name: &str, id: &str, value: i64) -> DerivedSpec {
    DerivedSpec {
        id: Some(id.to_string()),
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype: DTypeSpec::Integer,
        unit: None,
        rounding: None,
        source: None,
        period: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value },
            },
        },
        versions: vec![],
    }
}

/// Two rules sharing one public id used to make a query for that id answer
/// with whichever rule the program's hash map yielded first (1 or 2 across
/// runs). The program is now rejected, naming both rules.
#[test]
fn rules_sharing_a_public_id_are_rejected_not_resolved_by_hash_order() {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        for run in 0..32 {
            let error = execute_request(ExecutionRequest {
                mode: mode.clone(),
                program: ProgramSpec {
                    derived: vec![
                        literal_rule("second", "us:statutes/26/24#same", 2),
                        literal_rule("first", "us:statutes/26/24#same", 1),
                    ],
                    ..ProgramSpec::default()
                },
                dataset: DatasetSpec {
                    inputs: Vec::new(),
                    relations: Vec::new(),
                },
                queries: vec![ExecutionQuery {
                    assessment_date: None,
                    entity_id: "household-1".to_string(),
                    period: PeriodSpec {
                        kind: PeriodKindSpec::Month,
                        start: date,
                        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
                    },
                    outputs: vec!["us:statutes/26/24#same".to_string()],
                }],
            })
            .expect_err("rules sharing a public id are rejected");
            assert!(
                matches!(
                    &error,
                    ApiError::Spec(SpecError::DuplicatePublicId { kind, id, names })
                        if *kind == "derived rule"
                            && id == "us:statutes/26/24#same"
                            && names == "`first`, `second`"
                ),
                "{mode:?} run {run}: {error:?}"
            );
        }
    }
}
