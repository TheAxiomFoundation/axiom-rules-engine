//! A request can carry a raw `ProgramSpec`, which skips the checks a compiled
//! artifact passes when it is loaded. The evaluators recurse through the
//! program's dependency graph, so a cycle there used to recurse until the
//! stack overflowed and aborted the process (exit 134) in every mode. The
//! request now fails with the cycle instead.

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionQuery, ExecutionRequest, execute_request,
};
use axiom_rules_engine::compile::CompileError;
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec, JudgmentExprSpec,
    PeriodKindSpec, PeriodSpec, ProgramSpec, RelationDerivationSpec, RelationSpec, ScalarExprSpec,
    ScalarValueSpec,
};

fn household_rule(name: &str, expr: ScalarExprSpec) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype: DTypeSpec::Integer,
        unit: None,
        rounding: None,
        source: None,
        period: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemanticsSpec::Scalar { expr },
        versions: vec![],
    }
}

fn request(mode: ExecutionMode, program: ProgramSpec, output: &str) -> ExecutionRequest {
    let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    ExecutionRequest {
        mode,
        program,
        dataset: DatasetSpec {
            inputs: Vec::new(),
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: PeriodSpec {
                kind: PeriodKindSpec::Month,
                start,
                end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
            },
            outputs: vec![output.to_string()],
        }],
    }
}

#[test]
fn a_derived_rule_cycle_is_an_error_not_a_stack_overflow() {
    let derived = |name: &str| ScalarExprSpec::Derived {
        name: name.to_string(),
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let program = ProgramSpec {
            derived: vec![
                household_rule("alpha", derived("beta")),
                household_rule("beta", derived("alpha")),
            ],
            ..ProgramSpec::default()
        };
        let error = execute_request(request(mode.clone(), program, "alpha"))
            .expect_err("a cyclic program is refused");
        assert!(
            matches!(
                &error,
                ApiError::InvalidProgram(CompileError::CyclicDependency { cycle })
                    if cycle == "alpha, beta"
            ),
            "{mode:?}: {error:?}"
        );
    }
}

#[test]
fn a_derived_relation_derived_from_itself_is_an_error_not_a_stack_overflow() {
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let program = ProgramSpec {
            relations: vec![RelationSpec {
                name: "loop".to_string(),
                arity: 2,
                slot_entities: Vec::new(),
                derivation: Some(RelationDerivationSpec {
                    source_relation: "loop".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    entity: None,
                    member_relation: None,
                    slot_entities: Vec::new(),
                    predicate: JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Integer { value: 1 },
                        }),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Integer { value: 1 },
                        }),
                    },
                }),
            }],
            derived: vec![household_rule(
                "loop_size",
                ScalarExprSpec::CountRelated {
                    relation: "loop".to_string(),
                    current_slot: 0,
                    related_slot: 1,
                    where_clause: None,
                },
            )],
            ..ProgramSpec::default()
        };
        let error = execute_request(request(mode.clone(), program, "loop_size"))
            .expect_err("a relation derived from itself is refused");
        assert!(
            matches!(
                &error,
                ApiError::InvalidProgram(CompileError::CyclicRelationDependency { cycle })
                    if cycle == "loop"
            ),
            "{mode:?}: {error:?}"
        );
    }
}
