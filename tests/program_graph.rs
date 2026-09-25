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
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec,
    DerivedVersionSpec, IntervalSpec, JudgmentExprSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    RelationDerivationSpec, RelationRecordSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec,
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

fn literal(value: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Integer { value },
    }
}

fn always() -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(literal(1)),
        op: ComparisonOpSpec::Eq,
        right: Box::new(literal(1)),
    }
}

/// A relation predicate that reads the rule `e`.
fn reads_e() -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(ScalarExprSpec::Derived {
            name: "e".to_string(),
        }),
        op: ComparisonOpSpec::Gt,
        right: Box::new(literal(0)),
    }
}

fn count(relation: &str) -> ScalarExprSpec {
    ScalarExprSpec::CountRelated {
        relation: relation.to_string(),
        current_slot: 0,
        related_slot: 1,
        where_clause: None,
    }
}

fn member_of(relation: &str) -> JudgmentExprSpec {
    JudgmentExprSpec::RelationMember {
        relation: relation.to_string(),
        current_slot: 0,
        related_slot: 1,
    }
}

fn counts_any(relation: &str) -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(count(relation)),
        op: ComparisonOpSpec::Gt,
        right: Box::new(literal(0)),
    }
}

fn base_relation() -> RelationSpec {
    RelationSpec {
        name: "member".to_string(),
        arity: 2,
        slot_entities: Vec::new(),
        derivation: None,
    }
}

fn derived_relation(name: &str, source: &str, predicate: JudgmentExprSpec) -> RelationSpec {
    RelationSpec {
        name: name.to_string(),
        arity: 2,
        slot_entities: Vec::new(),
        derivation: Some(RelationDerivationSpec {
            source_relation: source.to_string(),
            current_slot: 0,
            related_slot: 1,
            entity: None,
            member_relation: None,
            slot_entities: Vec::new(),
            predicate,
        }),
    }
}

/// Run `e` over one household in both modes and return each mode's outcome.
fn run_e(program: &ProgramSpec) -> Vec<Result<(), ApiError>> {
    let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    let end = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date");
    [ExecutionMode::Explain, ExecutionMode::Fast]
        .into_iter()
        .map(|mode| {
            let mut request = request(mode, program.clone(), "e");
            request.dataset.relations = vec![RelationRecordSpec {
                name: "member".to_string(),
                tuple: vec!["household-1".to_string(), "person-1".to_string()],
                interval: IntervalSpec { start, end },
            }];
            execute_request(request).map(|_| ())
        })
        .collect()
}

fn assert_cycle_through_e(program: &ProgramSpec, label: &str) {
    for outcome in run_e(program) {
        assert!(
            matches!(
                &outcome,
                Err(ApiError::InvalidProgram(CompileError::CyclicDependency { cycle }))
                    if cycle == "e"
            ),
            "{label}: {outcome:?}"
        );
    }
}

/// Cycles routed through relations: `e` counts over `R`, and `R`'s membership
/// depends on `e` through its source relation, a `relation_member` predicate,
/// or a count inside its predicate. All three used to overflow the stack
/// even after direct cycles were refused.
#[test]
fn a_cycle_routed_through_other_relations_is_refused() {
    let shapes = [
        (
            "through R's source relation",
            vec![
                derived_relation("S", "member", reads_e()),
                derived_relation("R", "S", always()),
            ],
        ),
        (
            "through relation_member in R's predicate",
            vec![
                derived_relation("Q", "member", reads_e()),
                derived_relation("R", "member", member_of("Q")),
            ],
        ),
        (
            "through a count in R's predicate",
            vec![
                derived_relation("Q", "member", reads_e()),
                derived_relation("R", "member", counts_any("Q")),
            ],
        ),
    ];
    for (label, derived_relations) in shapes {
        let mut relations = vec![base_relation()];
        relations.extend(derived_relations);
        let program = ProgramSpec {
            relations,
            derived: vec![household_rule("e", count("R"))],
            ..ProgramSpec::default()
        };
        assert_cycle_through_e(&program, label);
    }
}

/// A compiled artifact runs the same check when it is loaded, so a
/// relation-routed cycle cannot reach the evaluators through `run-compiled`
/// or wasm either.
#[test]
fn a_compiled_artifact_with_a_relation_routed_cycle_is_refused_at_load() {
    let program = ProgramSpec {
        relations: vec![
            base_relation(),
            derived_relation("S", "member", reads_e()),
            derived_relation("R", "S", always()),
        ],
        derived: vec![household_rule("e", count("R"))],
        ..ProgramSpec::default()
    };
    let artifact = serde_json::json!({
        "artifact_format_version": 2,
        "engine_version": env!("CARGO_PKG_VERSION"),
        "program": program,
        "metadata": {
            "evaluation_order": [],
            "fast_path": {"strategy": "generic_bulk", "compatible": true, "blockers": []},
            "input_catalog": []
        }
    });
    let error =
        axiom_rules_engine::compile::CompiledProgramArtifact::from_json_str(&artifact.to_string())
            .expect_err("the artifact is refused");
    assert!(
        matches!(&error, CompileError::CyclicDependency { cycle } if cycle == "e"),
        "{error:?}"
    );
}

#[test]
fn judgment_and_version_only_cycles_are_refused() {
    let judgment = |name: &str, other: &str| DerivedSpec {
        dtype: DTypeSpec::Judgment,
        semantics: DerivedSemanticsSpec::Judgment {
            expr: JudgmentExprSpec::Derived {
                name: other.to_string(),
            },
        },
        ..household_rule(name, literal(0))
    };
    let judgments = ProgramSpec {
        derived: vec![judgment("j1", "j2"), judgment("j2", "j1")],
        ..ProgramSpec::default()
    };
    let error = execute_request(request(ExecutionMode::Explain, judgments, "j1"))
        .expect_err("a judgment cycle is refused");
    assert!(
        matches!(&error, ApiError::InvalidProgram(CompileError::CyclicDependency { cycle }) if cycle == "j1, j2"),
        "{error:?}"
    );

    // The cycle exists only in a dated version's formula.
    let versioned = ProgramSpec {
        derived: vec![DerivedSpec {
            versions: vec![DerivedVersionSpec {
                effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
                effective_to: None,
                semantics: DerivedSemanticsSpec::Scalar {
                    expr: ScalarExprSpec::Derived {
                        name: "v".to_string(),
                    },
                },
            }],
            ..household_rule("v", literal(0))
        }],
        ..ProgramSpec::default()
    };
    let error = execute_request(request(ExecutionMode::Fast, versioned, "v"))
        .expect_err("a version-only cycle is refused");
    assert!(
        matches!(&error, ApiError::InvalidProgram(CompileError::CyclicDependency { cycle }) if cycle == "v"),
        "{error:?}"
    );
}

/// As compilation does, an inline program that names an undefined rule is
/// refused up front, even where evaluation would never reach the reference.
#[test]
fn an_undefined_rule_reference_is_refused_even_in_a_dead_branch() {
    let program = ProgramSpec {
        derived: vec![household_rule(
            "d",
            ScalarExprSpec::If {
                condition: Box::new(always()),
                then_expr: Box::new(literal(7)),
                else_expr: Box::new(ScalarExprSpec::Derived {
                    name: "nope".to_string(),
                }),
            },
        )],
        ..ProgramSpec::default()
    };
    let error = execute_request(request(ExecutionMode::Explain, program, "d"))
        .expect_err("an undefined reference is refused");
    assert!(
        matches!(
            &error,
            ApiError::InvalidProgram(CompileError::UnknownDerivedDependency { derived, dependency })
                if derived == "d" && dependency == "nope"
        ),
        "{error:?}"
    );
}

/// Every combination of the edges a rule and three derived relations can
/// form: `e` counts over `R` or is a literal; `R` is derived from the base
/// relation or from `S`; `R`'s predicate is trivial, reads `e`, tests
/// membership in `Q`, or counts `Q`; `S` and `Q` read `e` or not. The test
/// models the dependency graph itself and checks the engine agrees: a program
/// is refused as cyclic exactly when `e` depends on itself, and otherwise runs
/// without being refused. A cycle the check missed would abort this test
/// process with a stack overflow.
#[test]
fn cycle_detection_agrees_with_a_model_of_every_small_program() {
    #[derive(Clone, Copy, Debug)]
    enum RPredicate {
        Always,
        ReadsE,
        MemberOfQ,
        CountsQ,
    }
    let mut checked = 0;
    for e_counts_r in [false, true] {
        for r_from_s in [false, true] {
            for r_predicate in [
                RPredicate::Always,
                RPredicate::ReadsE,
                RPredicate::MemberOfQ,
                RPredicate::CountsQ,
            ] {
                for s_reads_e in [false, true] {
                    for q_reads_e in [false, true] {
                        let pick = |reads| if reads { reads_e() } else { always() };
                        let program = ProgramSpec {
                            relations: vec![
                                base_relation(),
                                derived_relation("S", "member", pick(s_reads_e)),
                                derived_relation("Q", "member", pick(q_reads_e)),
                                derived_relation(
                                    "R",
                                    if r_from_s { "S" } else { "member" },
                                    match r_predicate {
                                        RPredicate::Always => always(),
                                        RPredicate::ReadsE => reads_e(),
                                        RPredicate::MemberOfQ => member_of("Q"),
                                        RPredicate::CountsQ => counts_any("Q"),
                                    },
                                ),
                            ],
                            derived: vec![household_rule(
                                "e",
                                if e_counts_r { count("R") } else { literal(1) },
                            )],
                            ..ProgramSpec::default()
                        };
                        let r_depends_on_e = matches!(r_predicate, RPredicate::ReadsE)
                            || (r_from_s && s_reads_e)
                            || (matches!(r_predicate, RPredicate::MemberOfQ | RPredicate::CountsQ)
                                && q_reads_e);
                        let label = format!(
                            "e_counts_r={e_counts_r} r_from_s={r_from_s} r_predicate={r_predicate:?} s_reads_e={s_reads_e} q_reads_e={q_reads_e}"
                        );
                        if e_counts_r && r_depends_on_e {
                            assert_cycle_through_e(&program, &label);
                        } else {
                            for outcome in run_e(&program) {
                                assert!(
                                    !matches!(outcome, Err(ApiError::InvalidProgram(_))),
                                    "{label}: {outcome:?}"
                                );
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 64);
}
