//! Mandatory relation entity typing.
//!
//! Entity ids are untyped strings and aggregation looks tuples up by
//! `(relation, current_slot, id)`, so a tuple stored in the other orientation
//! used to aggregate nothing: a household with two members had size 0, in
//! explain and fast alike, with exit 0 and no warning. These tests pin the
//! guarantees that replace that silent zero: executed relations must declare
//! slot kinds (compile, artifact load, and raw requests), directions are never
//! guessed, dataset tuples are checked against the declared kinds using input
//! records and queries as kind evidence, and pre-typing artifacts migrate.

use std::collections::BTreeMap;
use std::process::Command;

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse, execute_request,
};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::migrate::{
    ArtifactRelationMigrationError, migrate_artifact_relation_typing,
};
use axiom_rules_engine::relation_typing::RelationTypingCode;
use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules_engine::spec::{
    DatasetBindingOptions, DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec,
    ProgramSpec, RelationRecordSpec, ScalarValueSpec, SpecError,
};

fn interval() -> IntervalSpec {
    IntervalSpec {
        start: "2026-01-01".parse().unwrap(),
        end: "2026-01-31".parse().unwrap(),
    }
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().unwrap(),
        end: "2026-01-31".parse().unwrap(),
    }
}

fn tuple(relation: &str, ids: &[&str]) -> RelationRecordSpec {
    RelationRecordSpec {
        name: relation.to_string(),
        tuple: ids.iter().map(|id| id.to_string()).collect(),
        interval: interval(),
    }
}

fn bool_input(name: &str, entity: &str, id: &str, value: bool) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: id.to_string(),
        interval: interval(),
        value: ScalarValueSpec::Bool { value },
    }
}

fn query(entity_id: &str, outputs: &[&str]) -> ExecutionQuery {
    ExecutionQuery {
        entity_id: entity_id.to_string(),
        period: period(),
        outputs: outputs.iter().map(|output| output.to_string()).collect(),
        assessment_date: None,
    }
}

fn run(
    mode: ExecutionMode,
    program: ProgramSpec,
    dataset: DatasetSpec,
    queries: Vec<ExecutionQuery>,
) -> Result<ExecutionResponse, ApiError> {
    execute_request(ExecutionRequest {
        mode,
        program,
        dataset,
        queries,
    })
}

/// The response's output values as JSON, so assertions do not depend on
/// output enum layout: integers as numbers, judgments as their outcome.
fn outputs(response: &ExecutionResponse) -> Vec<BTreeMap<String, serde_json::Value>> {
    let value = serde_json::to_value(response).unwrap();
    value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| {
            result["outputs"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, output)| {
                    let value = if output["kind"] == "judgment" {
                        output["outcome"].clone()
                    } else {
                        output["value"]["value"].clone()
                    };
                    (name.clone(), value)
                })
                .collect()
        })
        .collect()
}

const BOTH_MODES: [ExecutionMode; 2] = [ExecutionMode::Explain, ExecutionMode::Fast];

/// The shape of rulespec-us `us/statutes/7/2012/j.yaml`: the relation is
/// declared by arity alone and aggregated from a Household rule.
const UNTYPED_SNAP_MEMBERSHIP: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_has_elderly_or_disabled_member
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2008-10-01'
        formula: count_where(member_of_household, member_is_elderly_or_disabled) > 0
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2008-10-01'
        formula: len(member_of_household)
"#;

fn typed_snap_membership(arguments: &str) -> String {
    UNTYPED_SNAP_MEMBERSHIP.replace(
        "      arity: 2\n",
        &format!("      arity: 2\n      arguments: {arguments}\n"),
    )
}

fn snap_program(arguments: &str) -> ProgramSpec {
    CompiledProgramArtifact::from_rulespec_str(&typed_snap_membership(arguments))
        .expect("typed membership compiles")
        .program
}

#[test]
fn raw_program_with_untyped_relation_is_refused_instead_of_counting_zero() {
    // The executed 2026-09-24 probe: two members stored household-first
    // against an untyped relation gave household size 0 in both modes.
    let program: ProgramSpec = serde_json::from_value(serde_json::json!({
        "relations": [{"name": "member_of", "arity": 2}],
        "derived": [{
            "name": "hh_size", "entity": "Household", "dtype": "integer", "unit": null,
            "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "member_of",
                     "current_slot": 1, "related_slot": 0}
        }]
    }))
    .unwrap();
    let dataset = DatasetSpec {
        inputs: vec![],
        relations: vec![
            tuple("member_of", &["h1", "p1"]),
            tuple("member_of", &["h1", "p2"]),
        ],
    };
    for mode in BOTH_MODES {
        let error = run(
            mode.clone(),
            program.clone(),
            dataset.clone(),
            vec![query("h1", &["hh_size"])],
        )
        .expect_err("an untyped relation must not execute");
        let ApiError::RelationTyping(report) = &error else {
            panic!("expected a relation typing error, got {error}");
        };
        assert_eq!(report.violations.len(), 1, "{report}");
        assert_eq!(
            report.violations[0].code,
            RelationTypingCode::UntypedRelation
        );
        assert_eq!(report.violations[0].relation, "member_of");
        assert_eq!(report.violations[0].citing, "hh_size");
        assert!(error.to_string().contains("silent zero"), "{error}");
    }
}

#[test]
fn untyped_rulespec_relation_used_in_aggregation_fails_to_compile() {
    let error = CompiledProgramArtifact::from_rulespec_str(UNTYPED_SNAP_MEMBERSHIP)
        .expect_err("untyped relation typing is mandatory at compile time");
    let CompileError::RelationTyping { report, .. } = &error else {
        panic!("expected a relation typing error, got {error}");
    };
    assert_eq!(
        report.untyped_relations().into_iter().collect::<Vec<_>>(),
        vec!["member_of_household"]
    );
    // Both aggregating rules are cited, in name order.
    let citing = report
        .violations
        .iter()
        .map(|violation| violation.citing.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        citing,
        vec!["household_has_elderly_or_disabled_member", "household_size"]
    );
    assert!(
        error.to_string().contains("data_relation.arguments"),
        "the error says how to fix it: {error}"
    );
}

#[test]
fn both_declared_orders_count_members_when_tuples_follow_the_declaration() {
    for (arguments, member_first) in [("[Person, Household]", true), ("[Household, Person]", false)]
    {
        let program = snap_program(arguments);
        let relations = ["p1", "p2"]
            .iter()
            .map(|person| {
                if member_first {
                    tuple("member_of_household", &[person, "h1"])
                } else {
                    tuple("member_of_household", &["h1", person])
                }
            })
            .collect();
        let dataset = DatasetSpec {
            inputs: vec![
                bool_input("member_is_elderly_or_disabled", "Person", "p1", true),
                bool_input("member_is_elderly_or_disabled", "Person", "p2", false),
            ],
            relations,
        };
        for mode in BOTH_MODES {
            let response = run(
                mode.clone(),
                program.clone(),
                dataset.clone(),
                vec![query(
                    "h1",
                    &["household_size", "household_has_elderly_or_disabled_member"],
                )],
            )
            .unwrap();
            let outputs = &outputs(&response)[0];
            assert_eq!(outputs["household_size"], 2, "{arguments} {mode:?}");
            assert_eq!(
                outputs["household_has_elderly_or_disabled_member"], "holds",
                "{arguments} {mode:?}"
            );
        }
    }
}

#[test]
fn queried_household_id_is_kind_evidence_for_reversed_tuples_without_inputs() {
    // No input record labels any id; only the query says `h1` is a Household.
    let program = snap_program("[Person, Household]").to_program().unwrap();
    let dataset = DatasetSpec {
        inputs: vec![],
        relations: vec![
            tuple("member_of_household", &["h1", "p1"]),
            tuple("member_of_household", &["h1", "p2"]),
        ],
    };
    let query_entities = vec![("h1".to_string(), "Household".to_string())];

    let outcome = dataset
        .to_dataset_for_queries_with_options(
            &program,
            &query_entities,
            DatasetBindingOptions::default(),
        )
        .unwrap();
    assert_eq!(outcome.diagnostics.len(), 2, "one per reversed tuple");
    for diagnostic in &outcome.diagnostics {
        assert_eq!(diagnostic.slot, 0);
        assert_eq!(diagnostic.entity_id, "h1");
        assert_eq!(diagnostic.expected_entity, "Person");
        assert_eq!(diagnostic.actual_entity, "Household");
    }

    let error = dataset
        .to_dataset_for_queries_with_options(
            &program,
            &query_entities,
            DatasetBindingOptions::strict(),
        )
        .expect_err("strict binding refuses the reversed tuples");
    assert!(
        error.to_string().contains("from the dataset input records or the queries"),
        "{error}"
    );

    // The same dataset without the query carries no kind evidence at all.
    let blind = dataset
        .to_dataset_for_queries_with_options(&program, &[], DatasetBindingOptions::strict())
        .unwrap();
    assert!(blind.diagnostics.is_empty());
}

/// For every household of up to three members and every choice of which
/// tuples are stored reversed, strict binding either refuses the dataset or
/// the count is the true member count, and explain agrees with fast. With
/// person inputs present or absent, the queried household id alone is
/// enough evidence to refuse any reversed tuple.
#[test]
fn no_orientation_of_member_tuples_yields_a_silent_wrong_count() {
    let program = snap_program("[Person, Household]");
    let model = program.to_program().unwrap();
    let mut cases = 0;
    for members in 0..=3usize {
        for reversed_mask in 0..(1u32 << members) {
            for label_people in [false, true] {
                let people = (0..members).map(|i| format!("p{i}")).collect::<Vec<_>>();
                let mut relations = people
                    .iter()
                    .enumerate()
                    .map(|(i, person)| {
                        if reversed_mask & (1 << i) != 0 {
                            tuple("member_of_household", &["h1", person])
                        } else {
                            tuple("member_of_household", &[person, "h1"])
                        }
                    })
                    .collect::<Vec<_>>();
                // A second household, always well-formed, shares nothing.
                relations.push(tuple("member_of_household", &["q0", "h2"]));
                let inputs = if label_people {
                    people
                        .iter()
                        .map(|person| {
                            bool_input("member_is_elderly_or_disabled", "Person", person, false)
                        })
                        .collect()
                } else {
                    vec![]
                };
                let dataset = DatasetSpec { inputs, relations };
                let queries = vec![query("h1", &["household_size"])];
                let strict = dataset.to_dataset_for_queries_with_options(
                    &model,
                    &[("h1".to_string(), "Household".to_string())],
                    DatasetBindingOptions::strict(),
                );
                if reversed_mask != 0 {
                    assert!(
                        strict.is_err(),
                        "members={members} mask={reversed_mask:b} labels={label_people}"
                    );
                    continue;
                }
                strict.unwrap();
                let explain = run(
                    ExecutionMode::Explain,
                    program.clone(),
                    dataset.clone(),
                    queries.clone(),
                )
                .unwrap();
                let fast = run(ExecutionMode::Fast, program.clone(), dataset, queries).unwrap();
                assert_eq!(outputs(&explain)[0]["household_size"], members as i64);
                assert_eq!(outputs(&explain), outputs(&fast));
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 8, "every all-forward case executed (4 sizes x 2 label modes)");
}

#[test]
fn same_kind_relation_direction_is_ambiguous_and_refused() {
    let source = r#"
format: rulespec/v1
rules:
  - name: parent_of
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Person]
  - name: child_count
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: len(parent_of)
"#;
    let error = lower_rulespec_str(source).expect_err("the direction is a guess");
    assert!(
        matches!(error, RuleSpecError::AmbiguousRelationDirection { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("derived_relation"), "{error}");
}

#[test]
fn same_kind_relation_aggregates_through_a_derived_relation_with_explicit_slots() {
    let source = r#"
format: rulespec/v1
rules:
  - name: parent_of
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Person]
  - name: dependent_children
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: parent_of
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: '2026-01-01'
        formula: child_is_dependent
  - name: dependent_child_count
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: len(dependent_children)
"#;
    let program = CompiledProgramArtifact::from_rulespec_str(source)
        .expect("explicit derivation slots resolve a same-kind direction")
        .program;
    let dataset = DatasetSpec {
        inputs: vec![
            bool_input("child_is_dependent", "Person", "c1", true),
            bool_input("child_is_dependent", "Person", "c2", false),
        ],
        relations: vec![
            tuple("parent_of", &["parent", "c1"]),
            tuple("parent_of", &["parent", "c2"]),
        ],
    };
    for mode in BOTH_MODES {
        let response = run(
            mode.clone(),
            program.clone(),
            dataset.clone(),
            vec![
                query("parent", &["dependent_child_count"]),
                query("c1", &["dependent_child_count"]),
            ],
        )
        .unwrap();
        let outputs = outputs(&response);
        assert_eq!(outputs[0]["dependent_child_count"], 1, "{mode:?}");
        assert_eq!(outputs[1]["dependent_child_count"], 0, "{mode:?}");
    }
}

#[test]
fn aggregation_from_an_entity_no_slot_holds_is_refused() {
    let source = typed_snap_membership("[Person, TaxUnit]");
    let error = CompiledProgramArtifact::from_rulespec_str(&source)
        .expect_err("a Household rule cannot key a [Person, TaxUnit] relation");
    let CompileError::RelationTyping { report, .. } = &error else {
        panic!("expected a relation typing error, got {error}");
    };
    assert!(
        report
            .violations
            .iter()
            .all(|violation| violation.code == RelationTypingCode::CurrentSlotEntityMismatch),
        "{report}"
    );
}

#[test]
fn related_rule_of_another_entity_is_refused() {
    let source = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: filer_is_head
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: head_flag
  - name: household_has_head_filer
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: count_where(member_of_household, filer_is_head) > 0
"#;
    let error = CompiledProgramArtifact::from_rulespec_str(source)
        .expect_err("a TaxUnit rule cannot run on the Person slot");
    let CompileError::RelationTyping { report, .. } = &error else {
        panic!("expected a relation typing error, got {error}");
    };
    assert_eq!(
        report.violations[0].code,
        RelationTypingCode::RelatedSlotEntityMismatch,
        "{report}"
    );
}

#[test]
fn undeclared_relation_used_in_aggregation_is_untyped() {
    let source = r#"
format: rulespec/v1
rules:
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: len(undeclared_members)
"#;
    let error = CompiledProgramArtifact::from_rulespec_str(source)
        .expect_err("a relation synthesized from a formula has no kinds");
    let CompileError::RelationTyping { report, .. } = &error else {
        panic!("expected a relation typing error, got {error}");
    };
    assert_eq!(
        report.untyped_relations().into_iter().collect::<Vec<_>>(),
        vec!["undeclared_members"]
    );
}

#[test]
fn tuple_length_must_match_relation_arity() {
    let program = snap_program("[Person, Household]");
    let dataset = DatasetSpec {
        inputs: vec![],
        relations: vec![tuple("member_of_household", &["p1"])],
    };
    for mode in BOTH_MODES {
        let error = run(
            mode,
            program.clone(),
            dataset.clone(),
            vec![query("h1", &["household_size"])],
        )
        .expect_err("a one-id tuple for a two-slot relation used to vanish");
        assert!(
            matches!(
                error,
                ApiError::Spec(SpecError::RelationTupleArity {
                    arity: 2,
                    found: 1,
                    ..
                })
            ),
            "{error}"
        );
    }
}

/// D1 from the 2026-09-24 mapping: an aggregate over a derived relation used
/// the legacy slots while the derivation traversed its source with others,
/// and fast read the source tuple at the aggregate's related slot.
#[test]
fn derived_relation_aggregates_use_the_derivation_slots_in_every_mode() {
    let source = r#"
format: rulespec/v1
rules:
  - name: household_members
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Household, Person]
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: household_members
      entity: SnapUnit
      member_relation: members
      slot_entities: [Household, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: '2026-01-01'
        formula: eligible
  - name: unit_income
    kind: derived
    entity: SnapUnit
    dtype: Decimal
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: sum(members.income)
  - name: unit_size
    kind: derived
    entity: SnapUnit
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: len(members)
"#;
    let program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap()
        .program;
    let income = |id: &str, amount: &str| InputRecordSpec {
        name: "income".to_string(),
        entity: "Person".to_string(),
        entity_id: id.to_string(),
        interval: interval(),
        value: ScalarValueSpec::Decimal {
            value: amount.to_string(),
        },
    };
    let dataset = DatasetSpec {
        inputs: vec![
            bool_input("eligible", "Person", "p1", true),
            bool_input("eligible", "Person", "p2", false),
            income("p1", "100"),
            income("p2", "50"),
        ],
        relations: vec![
            tuple("household_members", &["h1", "p1"]),
            tuple("household_members", &["h1", "p2"]),
        ],
    };
    let mut results = Vec::new();
    for mode in BOTH_MODES {
        let response = run(
            mode.clone(),
            program.clone(),
            dataset.clone(),
            vec![query("h1", &["unit_income", "unit_size"])],
        )
        .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        results.push(outputs(&response));
    }
    assert_eq!(results[0][0]["unit_size"], 1);
    assert_eq!(results[0][0]["unit_income"], "100");
    assert_eq!(results[0], results[1], "explain and fast agree");
}

/// A pre-typing artifact: the typed SNAP program with its relation's
/// `slot_entities` removed, as engines before relation typing wrote them.
fn legacy_artifact_json(arguments: &str) -> String {
    let artifact = CompiledProgramArtifact::from_rulespec_str(&typed_snap_membership(arguments))
        .unwrap();
    let mut value = serde_json::to_value(&artifact).unwrap();
    for relation in value["program"]["relations"].as_array_mut().unwrap() {
        relation.as_object_mut().unwrap().remove("slot_entities");
    }
    serde_json::to_string(&value).unwrap()
}

#[test]
fn loading_a_pre_typing_artifact_names_the_migration() {
    let error = CompiledProgramArtifact::from_json_str(&legacy_artifact_json("[Person, Household]"))
        .expect_err("an artifact executing an untyped relation must not load");
    assert!(
        matches!(error, CompileError::LegacyArtifactRelationTyping { .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.contains("migrate artifact"), "{message}");
    assert!(message.contains("member_of_household"), "{message}");
}

#[test]
fn migration_types_an_artifact_as_it_executes() {
    // `household_size` (len) leaves the related slot open; the elderly rule's
    // predicate is an input, so it does too. Usage fixes only the Household
    // slot the aggregates key on.
    let legacy = legacy_artifact_json("[Person, Household]");
    let error = migrate_artifact_relation_typing(&legacy, "legacy.json", &BTreeMap::new())
        .expect_err("the member slot cannot be inferred");
    let ArtifactRelationMigrationError::Uninferable(detail) = &error else {
        panic!("expected an uninferable report, got {error}");
    };
    assert!(detail.contains("member_of_household"), "{detail}");
    assert!(detail.contains("[?, Household]"), "{detail}");

    // An override that contradicts the executed Household slot is refused:
    // migration never reorders an artifact.
    let flipped = BTreeMap::from([(
        "member_of_household".to_string(),
        vec!["Household".to_string(), "Person".to_string()],
    )]);
    let error = migrate_artifact_relation_typing(&legacy, "legacy.json", &flipped)
        .expect_err("the override contradicts execution");
    assert!(
        matches!(
            error,
            ArtifactRelationMigrationError::OverrideContradictsExecution { slot: 1, .. }
        ),
        "{error}"
    );

    let overrides = BTreeMap::from([(
        "member_of_household".to_string(),
        vec!["Person".to_string(), "Household".to_string()],
    )]);
    let migration = migrate_artifact_relation_typing(&legacy, "legacy.json", &overrides).unwrap();
    assert_eq!(migration.changes.len(), 1);
    assert_eq!(migration.changes[0].source, "override");
    let typed = serde_json::to_string(&migration.artifact).unwrap();
    let reloaded = CompiledProgramArtifact::from_json_str(&typed).expect("migrated artifact loads");
    assert_eq!(
        reloaded.program.relations[0].slot_entities,
        vec!["Person", "Household"]
    );
    // Migration types the artifact; it does not change what it computes.
    let original = CompiledProgramArtifact::from_rulespec_str(&typed_snap_membership(
        "[Person, Household]",
    ))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&reloaded).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
}

#[test]
fn migration_infers_both_slots_when_a_related_rule_names_the_member_kind() {
    let source = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: member_is_elderly
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: elderly_flag
  - name: household_has_elderly_member
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: count_where(member_of_household, member_is_elderly) > 0
"#;
    let artifact = CompiledProgramArtifact::from_rulespec_str(source).unwrap();
    let mut value = serde_json::to_value(&artifact).unwrap();
    value["program"]["relations"][0]
        .as_object_mut()
        .unwrap()
        .remove("slot_entities");
    let legacy = serde_json::to_string(&value).unwrap();
    let migration =
        migrate_artifact_relation_typing(&legacy, "legacy.json", &BTreeMap::new()).unwrap();
    assert_eq!(migration.changes.len(), 1);
    assert_eq!(migration.changes[0].source, "inferred");
    assert_eq!(
        migration.changes[0].slot_entities,
        vec!["Person", "Household"]
    );
}

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "axiom-relation-typing-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn cli_refuses_a_legacy_artifact_and_migrates_it() {
    let dir = scratch_dir("cli");
    let legacy_path = dir.join("legacy.json");
    let typed_path = dir.join("typed.json");
    std::fs::write(&legacy_path, legacy_artifact_json("[Person, Household]")).unwrap();
    let request = serde_json::json!({
        "mode": "explain",
        "dataset": {"relations": [
            {"name": "member_of_household", "tuple": ["p1", "h1"],
             "interval": {"start": "2026-01-01", "end": "2026-01-31"}}
        ]},
        "queries": [{"entity_id": "h1",
                     "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
                     "outputs": ["household_size"]}]
    })
    .to_string();
    let run_compiled = |artifact: &std::path::Path| {
        use std::io::Write;
        let mut child = engine()
            .args(["run-compiled", "--artifact"])
            .arg(artifact)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };

    let refused = run_compiled(&legacy_path);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("migrate artifact"), "{stderr}");

    let dry_run = engine()
        .args(["migrate", "artifact", "--artifact"])
        .arg(&legacy_path)
        .args(["--relation-entities", "member_of_household=Person,Household"])
        .output()
        .unwrap();
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    assert!(String::from_utf8_lossy(&dry_run.stdout).contains("dry run"));
    assert!(!typed_path.exists(), "a dry run writes nothing");

    let migrated = engine()
        .args(["migrate", "artifact", "--artifact"])
        .arg(&legacy_path)
        .args(["--relation-entities", "member_of_household=Person,Household"])
        .arg("--output")
        .arg(&typed_path)
        .output()
        .unwrap();
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    let ran = run_compiled(&typed_path);
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&ran.stdout).unwrap();
    assert_eq!(
        response["results"][0]["outputs"]["household_size"]["value"]["value"],
        1
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn capabilities_advertise_mandatory_relation_typing() {
    let output = engine().arg("capabilities").output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("relation_entity_typing")),
        "{value}"
    );
}
