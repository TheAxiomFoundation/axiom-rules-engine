//! Related aggregate dependencies obey their own formula versions.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, OutputValue,
    execute_compiled_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;

fn period(year: i32) -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: format!("{year}-01-01").parse().unwrap(),
        end: format!("{year}-01-31").parse().unwrap(),
    }
}

fn numeric(value: &ScalarValueSpec) -> Decimal {
    match value {
        ScalarValueSpec::Integer { value } => Decimal::from(*value),
        ScalarValueSpec::Decimal { value } => value.parse().unwrap(),
        other => panic!("{other:?}"),
    }
}

fn source(formula: &str, scalar_versions: &str, predicate_versions: &str) -> String {
    format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n  - name: child_value\n    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Month\n    versions:\n{scalar_versions}\n  - name: child_eligible\n    kind: derived\n    entity: Person\n    dtype: Judgment\n    period: Month\n    versions:\n{predicate_versions}\n  - name: answer\n    kind: derived\n    entity: Family\n    dtype: Decimal\n    period: Month\n    versions:\n      - effective_from: '2026-01-01'\n        formula: |-\n          {}\n",
        formula.replace('\n', "\n          ")
    )
}

const SCALAR_2026: &str = "      - effective_from: '2026-01-01'\n        formula: age + 1";
const SCALAR_2027: &str = "      - effective_from: '2027-01-01'\n        formula: age + 1";
const PREDICATE_2026: &str = "      - effective_from: '2026-01-01'\n        formula: age >= 0";
const PREDICATE_2027: &str = "      - effective_from: '2027-01-01'\n        formula: age >= 0";

fn explain(
    artifact: &CompiledProgramArtifact,
    period: &PeriodSpec,
    has_member: bool,
) -> Result<ScalarValueSpec, EvalError> {
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "age".into(),
            entity: "Person".into(),
            entity_id: "p".into(),
            interval: interval.clone(),
            value: ScalarValueSpec::Integer { value: 2 },
        }],
        relations: if has_member {
            vec![RelationRecordSpec {
                name: "member_of_family".into(),
                tuple: vec!["p".into(), "f".into()],
                interval,
            }]
        } else {
            Vec::new()
        },
    };
    match execute_compiled_request(
        artifact.clone(),
        CompiledExecutionRequest {
            relation_binding: Default::default(),
            mode: ExecutionMode::Explain,
            dataset,
            queries: vec![ExecutionQuery {
                entity_id: "f".into(),
                period: period.clone(),
                outputs: vec!["answer".into()],
                assessment_date: None,
            }],
            pins: Vec::new(),
        },
    ) {
        Ok(response) => match &response.results[0].outputs["answer"] {
            OutputValue::Scalar { value, .. } => Ok(value.clone()),
            other => panic!("{other:?}"),
        },
        Err(ApiError::Eval(error)) => Err(error),
        Err(other) => panic!("{other}"),
    }
}

fn dense(
    program: &DenseCompiledProgram,
    period: &PeriodSpec,
    has_member: bool,
) -> Result<DenseColumn, EvalError> {
    let relations = program
        .relations()
        .iter()
        .map(|schema| {
            let inputs = schema
                .related_inputs
                .iter()
                .map(|name| {
                    assert_eq!(name, "age");
                    (
                        name.clone(),
                        DenseColumn::Integer(if has_member { vec![2] } else { Vec::new() }),
                    )
                })
                .collect();
            (
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: vec![0, usize::from(has_member)],
                    inputs,
                },
            )
        })
        .collect();
    let result = program.execute(
        &period.to_model().unwrap(),
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations,
        },
        &["answer".into()],
    )?;
    match &result.outputs["answer"] {
        DenseOutputValue::Scalar(column) => Ok(column.clone()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn dense_related_values_and_predicates_enforce_commencement() {
    for (formula, scalar, predicate, dependency) in [
        (
            "min(member_of_family.child_value)",
            SCALAR_2027,
            PREDICATE_2026,
            "child_value",
        ),
        (
            "max(member_of_family.child_value)",
            SCALAR_2027,
            PREDICATE_2026,
            "child_value",
        ),
        (
            "only(member_of_family.child_value)",
            SCALAR_2027,
            PREDICATE_2026,
            "child_value",
        ),
        (
            "min_where(member_of_family, age, child_eligible)",
            SCALAR_2026,
            PREDICATE_2027,
            "child_eligible",
        ),
        (
            "max_where(member_of_family, age, child_eligible)",
            SCALAR_2026,
            PREDICATE_2027,
            "child_eligible",
        ),
        (
            "min_where(member_of_family, child_value, child_eligible)",
            SCALAR_2027,
            PREDICATE_2026,
            "child_value",
        ),
        (
            "max_where(member_of_family, child_value, child_eligible)",
            SCALAR_2027,
            PREDICATE_2026,
            "child_value",
        ),
    ] {
        let artifact =
            CompiledProgramArtifact::from_rulespec_str(&source(formula, scalar, predicate))
                .unwrap();
        let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
        let before = period(2026);
        let sparse_error = explain(&artifact, &before, true).unwrap_err();
        assert!(
            matches!(&sparse_error, EvalError::MissingDerivedFormulaVersion { derived, at }
            if derived == dependency && *at == before.start),
            "{formula}: {sparse_error}"
        );
        let dense_error = dense(&program, &before, true).expect_err(formula);
        assert_eq!(
            dense_error.to_string(),
            sparse_error.to_string(),
            "{formula}"
        );
        let after = period(2027);
        let expected = explain(&artifact, &after, true).unwrap();
        let column = dense(&program, &after, true).unwrap();
        assert_eq!(
            column
                .scalar_value_at(0, &axiom_rules_engine::model::DType::Decimal)
                .as_decimal(),
            Some(numeric(&expected)),
            "{formula}"
        );
    }
}

#[test]
fn dense_commencement_checks_are_lazy_for_empty_and_skipped_paths() {
    for function in ["min", "max", "only"] {
        let formula = format!(
            "if len(member_of_family) > 0: {function}(member_of_family.child_value)\nelse: 7"
        );
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
            &formula,
            SCALAR_2027,
            PREDICATE_2027,
        ))
        .unwrap();
        let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
        assert_eq!(
            Some(numeric(&explain(&artifact, &period(2026), false).unwrap())),
            dense(&program, &period(2026), false)
                .unwrap()
                .scalar_value_at(0, &axiom_rules_engine::model::DType::Decimal)
                .as_decimal()
        );
    }
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
        "min_where(member_of_family, child_value, child_eligible)",
        SCALAR_2027,
        "      - effective_from: '2026-01-01'\n        formula: age < 0",
    ))
    .unwrap();
    let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
    assert!(matches!(
        explain(&artifact, &period(2026), true),
        Err(EvalError::EmptyRelatedAggregate { .. })
    ));
    assert!(matches!(
        dense(&program, &period(2026), true),
        Err(EvalError::EmptyRelatedAggregate { .. })
    ));
}

#[test]
fn dense_refuses_complex_related_versions_naming_the_construct() {
    let scalar_versions = [
        "      - effective_from: '2026-01-01'\n        effective_to: '2026-12-31'\n        formula: age + 1",
        "      - effective_from: '2026-01-01'\n        formula: age + 1\n      - effective_from: '2027-01-01'\n        formula: age + 2",
    ];
    let predicate_versions = [
        "      - effective_from: '2026-01-01'\n        effective_to: '2026-12-31'\n        formula: age >= 0",
        "      - effective_from: '2026-01-01'\n        formula: age >= 0\n      - effective_from: '2027-01-01'\n        formula: age >= 5",
    ];
    for function in ["min", "max", "only"] {
        for scalar in scalar_versions {
            let formula = format!("{function}(member_of_family.child_value)");
            let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
                &formula,
                scalar,
                PREDICATE_2026,
            ))
            .unwrap();
            assert!(
                matches!(DenseCompiledProgram::from_artifact(&artifact, Some("Family")), Err(DenseCompileError::Unsupported(message)) if message.contains(function) && message.contains("child_value")),
                "{formula}"
            );
        }
    }
    for function in ["min_where", "max_where"] {
        for scalar in scalar_versions {
            let formula = format!("{function}(member_of_family, child_value, child_eligible)");
            let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
                &formula,
                scalar,
                PREDICATE_2026,
            ))
            .unwrap();
            assert!(
                matches!(DenseCompiledProgram::from_artifact(&artifact, Some("Family")), Err(DenseCompileError::Unsupported(message)) if message.contains(function) && message.contains("child_value")),
                "{formula}"
            );
        }
        for predicate in predicate_versions {
            let formula = format!("{function}(member_of_family, age, child_eligible)");
            let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
                &formula,
                SCALAR_2026,
                predicate,
            ))
            .unwrap();
            assert!(
                matches!(DenseCompiledProgram::from_artifact(&artifact, Some("Family")), Err(DenseCompileError::Unsupported(message)) if message.contains(function) && message.contains("child_eligible")),
                "{formula}"
            );
        }
    }
}

#[test]
fn dense_checks_versions_of_dependencies_nested_in_related_rule_bodies() {
    for formula in [
        "min_where(member_of_family, age, child_eligible)",
        "max_where(member_of_family, age, child_eligible)",
    ] {
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source(
            formula,
            SCALAR_2027,
            "      - effective_from: '2026-01-01'\n        formula: child_value >= 0",
        ))
        .unwrap();
        let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
        let sparse = explain(&artifact, &period(2026), true).unwrap_err();
        let column = dense(&program, &period(2026), true).unwrap_err();
        assert_eq!(column.to_string(), sparse.to_string(), "{formula}");
        assert!(
            matches!(sparse, EvalError::MissingDerivedFormulaVersion { derived, .. } if derived == "child_value")
        );
    }
}

#[test]
fn dense_checks_current_record_dependencies_in_derived_relation_filters() {
    for (dtype, predicate, later_formula) in [
        ("Decimal", "family_gate >= 0", "1"),
        ("Judgment", "family_gate", "1 == 1"),
    ] {
        for function in ["min", "max", "only"] {
            let module = format!(
                "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n  - name: family_gate\n    kind: derived\n    entity: Family\n    dtype: {dtype}\n    versions:\n      - effective_from: '2026-01-01'\n        formula: family_later\n  - name: family_later\n    kind: derived\n    entity: Family\n    dtype: {dtype}\n    versions:\n      - effective_from: '2027-01-01'\n        formula: {later_formula}\n  - name: eligible_family\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: member_of_family\n      entity: EligibleFamily\n      member_relation: eligible_members\n      slot_entities: [Person, Family]\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {predicate}\n  - name: answer\n    kind: derived\n    entity: EligibleFamily\n    dtype: Decimal\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {function}(eligible_members.age)\n"
            );
            let artifact = CompiledProgramArtifact::from_rulespec_str(&module).unwrap();
            let program =
                DenseCompiledProgram::from_artifact(&artifact, Some("EligibleFamily")).unwrap();
            let before = period(2026);
            let sparse = explain(&artifact, &before, true).unwrap_err();
            assert!(
                matches!(&sparse, EvalError::MissingDerivedFormulaVersion { derived, .. } if derived == "family_later"),
                "{dtype}: {sparse}"
            );
            let column = dense(&program, &before, true).unwrap_err();
            assert_eq!(
                column.to_string(),
                sparse.to_string(),
                "{dtype}: {function}"
            );
            let after = period(2027);
            assert_eq!(
                Some(numeric(&explain(&artifact, &after, true).unwrap())),
                dense(&program, &after, true)
                    .unwrap()
                    .scalar_value_at(0, &axiom_rules_engine::model::DType::Decimal)
                    .as_decimal(),
                "{dtype}: {function}"
            );
        }
    }
}

#[test]
fn dense_shared_relation_refusals_do_not_depend_on_the_reader_or_deferral_threshold() {
    // All these aggregates reach the same unsupported membership test in a
    // shared derived relation. The refusal must name that test consistently,
    // whichever aggregate reaches the schema first as compilation proceeds.
    let mut reference = None;
    for formula in [
        "len(eligible_members)",
        "sum(eligible_members.age)",
        "min(eligible_members.age)",
        "max(eligible_members.age)",
        "only(eligible_members.age)",
        "min_where(eligible_members, age, eligible)",
        "max_where(eligible_members, age, eligible)",
    ] {
        let module = format!(
            "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n  - name: other_relation\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n  - name: eligible_family\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: member_of_family\n      entity: EligibleFamily\n      member_relation: eligible_members\n      slot_entities: [Person, Family]\n    versions:\n      - effective_from: '2026-01-01'\n        formula: other_relation\n  - name: answer\n    kind: derived\n    entity: EligibleFamily\n    dtype: Decimal\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {formula}\n"
        );
        let artifact = CompiledProgramArtifact::from_rulespec_str(&module).unwrap();
        let program = artifact.program.to_program().unwrap();
        for threshold in [usize::MAX, 1, 2, 3] {
            let error = with_suspend_depth(threshold, || {
                DenseCompiledProgram::from_program(&program, Some("EligibleFamily"))
                    .expect_err("membership in another relation is unsupported")
            });
            let DenseCompileError::Unsupported(message) = error else {
                panic!("{formula}: {error}");
            };
            assert!(
                message.contains("`relation_member` of `other_relation`"),
                "{message}"
            );
            match &reference {
                None => reference = Some(message),
                Some(reference) => assert_eq!(
                    &message, reference,
                    "{formula}, deferral threshold {threshold}"
                ),
            }
        }
    }
}
