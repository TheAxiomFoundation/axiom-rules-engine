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

/// The compile error carried by a request refused for its dependency graph.
fn graph_error(error: &ApiError) -> Option<&CompileError> {
    match error {
        ApiError::InvalidProgram(error) => Some(error),
        _ => None,
    }
}

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
                graph_error(&error),
                Some(CompileError::CyclicDependency { cycle })
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
                graph_error(&error),
                Some(CompileError::CyclicRelationDependency { cycle })
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
/// The household is related to itself, so a relation predicate that reads
/// `e` reads it for the same household, and a cycle through `e` recurses
/// instead of stopping at an entity with no relations of its own.
fn run_e(program: &ProgramSpec) -> Vec<Result<(), ApiError>> {
    let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    let end = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date");
    [ExecutionMode::Explain, ExecutionMode::Fast]
        .into_iter()
        .map(|mode| {
            let mut request = request(mode, program.clone(), "e");
            request.dataset.relations = vec![RelationRecordSpec {
                name: "member".to_string(),
                tuple: vec!["household-1".to_string(), "household-1".to_string()],
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
                outcome.as_ref().err().and_then(graph_error),
                Some(CompileError::CyclicDependency { cycle })
                    if cycle == "e"
            ),
            "{label}: {outcome:?}"
        );
    }
}

/// Cycles routed through relations: `e` counts over `R`, and `R`'s membership
/// depends on `e` through its source relation, a `relation_member` predicate,
/// or a count inside its predicate. Before this check, each shape overflowed
/// the stack in both modes, whether sent inline or compiled: the check run
/// when an artifact is loaded follows only a derived relation's own
/// predicate, so it accepted all three.
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
/// or wasm either, and a request pin cannot rescue it: pins are applied after
/// the artifact loads.
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
        matches!(graph_error(&error), Some(CompileError::CyclicDependency { cycle }) if cycle == "j1, j2"),
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
        matches!(graph_error(&error), Some(CompileError::CyclicDependency { cycle }) if cycle == "v"),
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
            graph_error(&error),
            Some(CompileError::UnknownDerivedDependency { derived, dependency })
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
/// successfully. Before this check, all 20 cyclic programs overflowed the
/// stack in both modes when sent inline; compiling them refused the 8 whose
/// `R` reads `e` directly and accepted the other 12. A cycle the check missed
/// would abort this test process.
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
                                assert!(outcome.is_ok(), "{label}: {outcome:?}");
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

/// The routed-cycle check refuses programs but never reorders them: an
/// artifact's stored evaluation order must equal the order recomputed when it
/// loads, so a changed order would make previously valid artifacts
/// unloadable. This artifact was compiled before the check existed (main at
/// 5a29e03). `e` counts `R`, which is filtered from `S`, whose predicate reads
/// `z`; the program is acyclic.
#[test]
fn an_artifact_compiled_before_this_check_still_loads_and_runs() {
    const ARTIFACT: &str = r#"{"artifact_format_version":2,"engine_version":"0.2.2","metadata":{"evaluation_order":["e","z"],"fast_path":{"blockers":[],"compatible":true,"strategy":"generic_bulk"},"input_catalog":[]},"program":{"derived":[{"dtype":"integer","entity":"Household","expr":{"current_slot":0,"kind":"count_related","related_slot":1,"relation":"R","where":null},"name":"e","period":null,"semantics":"scalar","source":null,"source_url":null,"unit":null},{"dtype":"integer","entity":"Household","expr":{"kind":"literal","value":{"kind":"integer","value":1}},"name":"z","period":null,"semantics":"scalar","source":null,"source_url":null,"unit":null}],"parameters":[],"relations":[{"arity":2,"name":"member"},{"arity":2,"derivation":{"current_slot":0,"predicate":{"kind":"comparison","left":{"kind":"derived","name":"z"},"op":"gte","right":{"kind":"literal","value":{"kind":"integer","value":0}}},"related_slot":1,"source_relation":"member"},"name":"S"},{"arity":2,"derivation":{"current_slot":0,"predicate":{"kind":"comparison","left":{"kind":"literal","value":{"kind":"integer","value":1}},"op":"eq","right":{"kind":"literal","value":{"kind":"integer","value":1}}},"related_slot":1,"source_relation":"S"},"name":"R"}],"units":[]}}"#;
    let artifact = axiom_rules_engine::compile::CompiledProgramArtifact::from_json_str(ARTIFACT)
        .expect("a previously compiled acyclic artifact still loads");
    assert_eq!(artifact.metadata.evaluation_order, ["e", "z"]);
    let recompiled =
        axiom_rules_engine::compile::CompiledProgramArtifact::compile(artifact.program.clone())
            .expect("the program still compiles");
    assert_eq!(recompiled.metadata, artifact.metadata);
    for outcome in run_e(&artifact.program) {
        assert!(outcome.is_ok(), "{outcome:?}");
    }
}

/// The check covers the whole program, as the compile-time check does: every
/// dated version of a rule contributes its dependencies, whichever dates a
/// request asks about. Here `a` counts `R` in 2025 and `b` reads `a` in 2026,
/// while `R`'s membership reads `b`. No single year's rules form a cycle and
/// main evaluated 2026 to `a = b = 1`, but the program is refused, just as
/// compilation already refuses a direct cycle that exists only across
/// versions.
#[test]
fn a_cycle_formed_across_dated_versions_is_refused() {
    let dated = |name: &str, in_2025: ScalarExprSpec, from_2026: ScalarExprSpec| DerivedSpec {
        versions: vec![
            DerivedVersionSpec {
                effective_from: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
                effective_to: Some(
                    chrono::NaiveDate::from_ymd_opt(2025, 12, 31).expect("valid date"),
                ),
                semantics: DerivedSemanticsSpec::Scalar { expr: in_2025 },
            },
            DerivedVersionSpec {
                effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
                effective_to: None,
                semantics: DerivedSemanticsSpec::Scalar { expr: from_2026 },
            },
        ],
        ..household_rule(name, literal(0))
    };
    let reads = |name: &str| ScalarExprSpec::Derived {
        name: name.to_string(),
    };
    let reads_b = JudgmentExprSpec::Comparison {
        left: Box::new(reads("b")),
        op: ComparisonOpSpec::Gte,
        right: Box::new(literal(0)),
    };
    let program = ProgramSpec {
        relations: vec![
            base_relation(),
            derived_relation("S", "member", reads_b),
            derived_relation("R", "S", always()),
        ],
        derived: vec![
            dated("a", count("R"), literal(1)),
            dated("b", literal(1), reads("a")),
        ],
        ..ProgramSpec::default()
    };
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let error = execute_request(request(mode.clone(), program.clone(), "a"))
            .expect_err("a cycle across versions is refused");
        assert!(
            matches!(
                graph_error(&error),
                Some(CompileError::CyclicDependency { cycle })
                    if cycle == "a, b"
            ),
            "{mode:?}: {error:?}"
        );
    }
}
