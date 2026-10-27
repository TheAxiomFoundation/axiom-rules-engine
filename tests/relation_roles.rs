//! Terms, qualified reads and parameters in derived-relation predicates
//! (#214, `docs/relation-roles.md` sections 3.1 to 3.3).
//!
//! Inside a derived relation's predicate, `anchor` is the id the relation is
//! read from and `candidate` the id being tested. `t.name` reads a rule or an
//! input at `t`'s id, and the predicate reads the module's parameters as a
//! rule does.
//!
//! The example tests compile RuleSpec. The properties build programs as JSON
//! and check the invariants of section 7 that this part touches:
//!
//! * 4, qualified reads agree with routing: a predicate written with bare
//!   reads and the same predicate with each read qualified by the term today's
//!   routing reads give byte-identical responses;
//! * 1, order: permuting input records and relation tuples changes no byte;
//! * 7, deferral transparency: every suspend depth gives the same bytes;
//! * 9, modes agree: explain and fast give the same outputs, and dense refuses
//!   by name;
//! * 11, kinds: a program compiles exactly when every rule read through a term
//!   has the term's kind, and then every rule is evaluated on ids of its kind.

use std::collections::BTreeSet;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::DenseCompiledProgram;
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::model::Period;
use axiom_rules_engine::relation_typing::RelationTypingCode;
use axiom_rules_engine::spec::ProgramSpec;
use proptest::prelude::*;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// RuleSpec examples
// ---------------------------------------------------------------------------

/// A module with a `[Person, Person]` relation and one derived relation per
/// construct. `{extra}` is spliced in as further rules.
fn people_module(extra: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: relative_of
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Person]
  - name: is_adult
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: minimum_gap
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: band_of_age
    kind: parameter
    dtype: Integer
    indexed_by: age
    versions:
      - effective_from: 2026-01-01
        values:
          12: 1
          30: 2
          35: 2
          45: 3
  - name: older_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: candidate.age >= anchor.age + minimum_gap
  - name: adult_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: anchor.is_adult and candidate.is_adult
  - name: same_band_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: band_of_age[candidate.age] == band_of_age[anchor.age]
  - name: older_relatives
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(older_relative_of)
  - name: adult_relatives
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(adult_relative_of)
  - name: same_band_relatives
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(same_band_relative_of)
{extra}"#
    )
}

/// A derived relation of `relative_of` with `formula`, and a count over it.
fn relative_relation(name: &str, slots: &str, formula: &str) -> String {
    format!(
        r#"
  - name: {name}
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
{slots}      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
  - name: {name}_count
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len({name})
"#
    )
}

const PERSON_SLOTS: &str = "      slot_entities: [Person, Person]\n";

fn compile(source: &str) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::from_rulespec_str(source)
}

fn month() -> Value {
    json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"})
}

fn year_interval() -> Value {
    json!({"start": "2026-01-01", "end": "2026-12-31"})
}

fn input_record(entity: &str, id: &str, name: &str, value: Value) -> Value {
    json!({
        "name": name, "entity": entity, "entity_id": id,
        "interval": year_interval(), "value": value,
    })
}

fn age(id: &str, years: i64) -> Value {
    input_record(
        "Person",
        id,
        "age",
        json!({"kind": "integer", "value": years}),
    )
}

fn tuple(relation: &str, from: &str, to: &str) -> Value {
    json!({"name": relation, "tuple": [from, to], "interval": year_interval()})
}

/// p1 (30) has relatives p2 (45), p3 (35) and p4 (12); p4 has relative p1.
fn family() -> Value {
    json!({
        "inputs": [age("p1", 30), age("p2", 45), age("p3", 35), age("p4", 12)],
        "relations": [
            tuple("relative_of", "p1", "p2"),
            tuple("relative_of", "p1", "p3"),
            tuple("relative_of", "p1", "p4"),
            tuple("relative_of", "p4", "p1"),
        ],
    })
}

fn request(mode: &str, program: &Value, dataset: &Value, queries: &Value) -> ExecutionRequest {
    serde_json::from_value(json!({
        "mode": mode, "program": program, "dataset": dataset, "queries": queries,
    }))
    .expect("request deserializes")
}

fn run(mode: &str, program: &Value, dataset: &Value, entity_id: &str, outputs: &[&str]) -> Value {
    let queries = json!([{"entity_id": entity_id, "period": month(), "outputs": outputs}]);
    serde_json::to_value(
        execute_request(request(mode, program, dataset, &queries)).expect("request executes"),
    )
    .expect("response serializes")
}

fn output(response: &Value, name: &str) -> Value {
    response["results"][0]["outputs"][name]["value"]["value"].clone()
}

fn program_of(artifact: &CompiledProgramArtifact) -> Value {
    serde_json::to_value(&artifact.program).expect("program serializes")
}

fn predicate_of<'a>(program: &'a Value, relation: &str) -> &'a Value {
    program["relations"]
        .as_array()
        .expect("relations")
        .iter()
        .find(|candidate| candidate["name"] == relation)
        .map(|relation| &relation["derivation"]["predicate"])
        .unwrap_or_else(|| panic!("no relation `{relation}`"))
}

/// The relation typing codes a compile error reports.
fn typing_codes(error: &CompileError) -> Vec<RelationTypingCode> {
    let CompileError::RelationTyping { report, .. } = error else {
        panic!("expected a relation typing error, got {error}");
    };
    report
        .violations
        .iter()
        .map(|violation| violation.code)
        .collect()
}

#[test]
fn a_predicate_reads_inputs_rules_and_parameters_at_both_terms() {
    let artifact = compile(&people_module("")).expect("module compiles");
    let program = program_of(&artifact);
    for mode in ["explain", "fast"] {
        let response = run(
            mode,
            &program,
            &family(),
            "p1",
            &["older_relatives", "adult_relatives", "same_band_relatives"],
        );
        // p2 is at least 10 years older than p1; p3 is not.
        assert_eq!(output(&response, "older_relatives"), json!(1), "{mode}");
        // p1 is an adult, and so are p2 and p3; p4 is not.
        assert_eq!(output(&response, "adult_relatives"), json!(2), "{mode}");
        // p1 and p3 are in band 2.
        assert_eq!(output(&response, "same_band_relatives"), json!(1), "{mode}");

        // From p4 (12), the one relative p1 (30) is at least 10 years older;
        // read the other way round, p4 is not.
        let response = run(
            mode,
            &program,
            &family(),
            "p4",
            &["older_relatives", "adult_relatives", "same_band_relatives"],
        );
        assert_eq!(output(&response, "older_relatives"), json!(1), "{mode}");
        assert_eq!(output(&response, "adult_relatives"), json!(0), "{mode}");
        assert_eq!(output(&response, "same_band_relatives"), json!(0), "{mode}");
    }
}

/// The lowered forms: inputs and rules read through a term, a scalar
/// parameter, and a parameter table indexed by a read through a term.
#[test]
fn qualified_reads_and_parameters_lower_to_their_nodes() {
    let program = program_of(&compile(&people_module("")).expect("module compiles"));
    assert_eq!(
        predicate_of(&program, "older_relative_of"),
        &json!({
            "kind": "comparison",
            "left": {"kind": "role_input", "term": "candidate", "name": "age"},
            "op": "gte",
            "right": {"kind": "add", "items": [
                {"kind": "role_input", "term": "anchor", "name": "age"},
                {"kind": "parameter_lookup", "parameter": "minimum_gap",
                 "index": {"kind": "literal", "value": {"kind": "integer", "value": 0}}},
            ]},
        })
    );
    assert_eq!(
        predicate_of(&program, "adult_relative_of"),
        &json!({"kind": "and", "items": [
            {"kind": "role_derived", "term": "anchor", "name": "is_adult"},
            {"kind": "role_derived", "term": "candidate", "name": "is_adult"},
        ]})
    );
    assert_eq!(
        predicate_of(&program, "same_band_relative_of")["left"],
        json!({"kind": "parameter_lookup", "parameter": "band_of_age",
               "index": {"kind": "role_input", "term": "candidate", "name": "age"}})
    );
}

/// An entity-free rule with a literal value lowers to a parameter for rules,
/// and a predicate reads it the same way.
#[test]
fn an_entity_free_literal_rule_is_a_parameter_in_a_predicate() {
    let extra = r#"
  - name: adult_age
    kind: derived
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "18"
"#
    .to_string()
        + &relative_relation(
            "grown_relative_of",
            PERSON_SLOTS,
            "candidate.age >= adult_age",
        );
    let program = program_of(&compile(&people_module(&extra)).expect("module compiles"));
    assert_eq!(
        predicate_of(&program, "grown_relative_of")["right"],
        json!({"kind": "parameter_lookup", "parameter": "adult_age",
               "index": {"kind": "literal", "value": {"kind": "integer", "value": 0}}})
    );
    let response = run(
        "explain",
        &program,
        &family(),
        "p1",
        &["grown_relative_of_count"],
    );
    assert_eq!(output(&response, "grown_relative_of_count"), json!(2));
}

/// Dense compiles a parameter read in a predicate; only a read through a term
/// needs explain or fast.
#[test]
fn dense_evaluates_a_parameter_in_a_predicate() {
    use axiom_rules_engine::dense::{
        DenseBatchSpec, DenseColumn, DenseOutputValue, DenseRelationBatchSpec,
    };
    let source = r#"
format: rulespec/v1
rules:
  - name: relative_of
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Person]
  - name: adult_age
    kind: derived
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "18"
"#
    .to_string()
        + &relative_relation("grown_relative_of", PERSON_SLOTS, "age >= adult_age");
    let artifact = compile(&source).expect("module compiles");
    let compiled =
        DenseCompiledProgram::from_artifact(&artifact, Some("Person")).expect("dense compiles");
    let key = compiled.relations()[0].key.clone();
    // p1's relatives are 45, 35 and 12.
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: Default::default(),
        relations: [(
            key,
            DenseRelationBatchSpec {
                offsets: vec![0, 3],
                inputs: [("age".into(), DenseColumn::Integer(vec![45, 35, 12]))].into(),
            },
        )]
        .into(),
    };
    let result = compiled
        .execute(
            &Period::month(2026, 1),
            batch,
            &["grown_relative_of_count".into()],
        )
        .expect("dense executes");
    assert!(
        matches!(
            &result.outputs["grown_relative_of_count"],
            DenseOutputValue::Scalar(DenseColumn::Integer(counts)) if counts == &[2]
        ),
        "{:?}",
        result.outputs
    );
}

/// An input read only through a term enters the catalog as a bare read
/// does, and the compiled artifact loads back unchanged.
#[test]
fn qualified_inputs_enter_the_catalog_and_the_artifact_round_trips() {
    let extra = relative_relation("tall_relative_of", PERSON_SLOTS, "candidate.height > 180");
    let artifact = compile(&people_module(&extra)).expect("module compiles");
    let text = serde_json::to_string(&artifact).expect("artifact serializes");
    // No rule reads `height`; only the predicate does, through a term.
    assert!(
        artifact
            .metadata
            .input_catalog
            .iter()
            .any(|entry| entry.slot == "height" && entry.request_names == ["height"]),
        "{:?}",
        artifact.metadata.input_catalog
    );
    let loaded = CompiledProgramArtifact::from_json_str(&text).expect("the artifact loads");
    assert_eq!(
        serde_json::to_string(&loaded).expect("artifact serializes"),
        text
    );
}

/// A trace records a rule read through a term at the term's id, on the
/// evaluated path and on the skipped one.
#[test]
fn traces_record_qualified_rule_reads_at_the_terms_id() {
    let program = program_of(&compile(&people_module("")).expect("module compiles"));
    let response = run("explain", &program, &family(), "p1", &["adult_relatives"]);
    let trace = &response["results"][0]["trace"];
    let evaluated_on = trace
        .as_object()
        .expect("trace")
        .values()
        .filter(|node| node["name"] == "is_adult")
        .map(|node| node["entity_id"].as_str().expect("id").to_string())
        .collect::<BTreeSet<_>>();
    // `anchor.is_adult` at p1, and `candidate.is_adult` at each relative.
    assert_eq!(
        evaluated_on,
        BTreeSet::from(["p1", "p2", "p3", "p4"].map(String::from))
    );

    // p4 is no adult, so `anchor.is_adult and candidate.is_adult` skips the
    // candidate read: it is recorded at the candidate, p1.
    let response = run("explain", &program, &family(), "p4", &["adult_relatives"]);
    assert_eq!(output(&response, "adult_relatives"), json!(0));
    let trace = &response["results"][0]["trace"];
    // A trace key names an id other than the query's in hex: `7031` is p1.
    assert_eq!(
        trace["adult_relatives"]["not_evaluated_dependencies"],
        json!([{"dependency": "is_adult@entity:7031", "reason": "short_circuit"}]),
        "{trace}"
    );
}

/// Unqualified reads keep today's routing: in a `[Person, Person]` relation
/// with declared kinds, a bare rule reads the anchor and a bare input the
/// candidate.
#[test]
fn unqualified_reads_keep_their_routing() {
    let extra = relative_relation("bare_reads", PERSON_SLOTS, "is_adult and age >= 18")
        + &relative_relation(
            "qualified_reads",
            PERSON_SLOTS,
            "anchor.is_adult and candidate.age >= 18",
        );
    let program = program_of(&compile(&people_module(&extra)).expect("module compiles"));
    for anchor in ["p1", "p4"] {
        let response = run(
            "explain",
            &program,
            &family(),
            anchor,
            &["bare_reads_count", "qualified_reads_count"],
        );
        assert_eq!(
            output(&response, "bare_reads_count"),
            output(&response, "qualified_reads_count"),
            "{anchor}"
        );
    }
}

fn refusal(formula: &str) -> String {
    compile(&people_module(&relative_relation(
        "refused",
        PERSON_SLOTS,
        formula,
    )))
    .expect_err("the predicate is refused")
    .to_string()
}

#[test]
fn a_bare_term_is_refused_as_a_value() {
    for formula in [
        "anchor > 3",
        "candidate",
        "not anchor",
        "age >= anchor",
        "band_of_age[candidate] == 1",
        "anchor[0] == 1",
        "len(candidate) > 0",
        "count_where(relative_of, anchor) > 0",
        "candidate.anchor",
        "max(relative_of.anchor) > 0",
        "only(relative_of.candidate) > 0",
        "min_where(relative_of, anchor, is_adult) > 0",
        "max_where(relative_of, age, candidate) > 0",
    ] {
        let error = refusal(formula);
        assert!(
            error.contains("[relation_term_used_as_value]"),
            "{formula}: {error}"
        );
    }
}

#[test]
fn a_qualified_read_inside_an_aggregation_is_refused() {
    for formula in [
        "count_where(relative_of, candidate.is_adult) > 0",
        "sum_where(relative_of, anchor.age, is_adult) > 0",
        "sum_where(relative_of, age, candidate.is_adult) > 0",
        "sum(anchor.age) > 0",
        "only_related(relative_of, candidate.flag) == true",
        "only(candidate.age) > 0",
        "min_where(relative_of, candidate.age, is_adult) > 0",
        "max_where(relative_of, age, anchor.is_adult) > 0",
    ] {
        let error = refusal(formula);
        assert!(
            error.contains("[relation_role_outside_scope]"),
            "{formula}: {error}"
        );
    }
}

/// `anchor` and `candidate` name terms, not relations, so in a predicate
/// `max(candidate.age)` is the scalar maximum of one qualified read rather
/// than an aggregate over a relation named `candidate`.
#[test]
fn min_and_max_of_a_qualified_read_keep_their_scalar_meaning() {
    let extra = relative_relation("max_of_one", PERSON_SLOTS, "max(candidate.age) >= 40")
        + &relative_relation(
            "min_of_two",
            PERSON_SLOTS,
            "min(anchor.age, candidate.age) >= 30",
        );
    let program = program_of(&compile(&people_module(&extra)).expect("module compiles"));
    assert_eq!(
        predicate_of(&program, "max_of_one")["left"],
        json!({"kind": "max", "items": [{"kind": "role_input", "term": "candidate", "name": "age"}]})
    );
    for mode in ["explain", "fast"] {
        let response = run(
            mode,
            &program,
            &family(),
            "p1",
            &["max_of_one_count", "min_of_two_count"],
        );
        // p1 (30): only p2 (45) is 40 or over; p2 and p3 (35) are 30 or
        // over, and so is p1.
        assert_eq!(output(&response, "max_of_one_count"), json!(1), "{mode}");
        assert_eq!(output(&response, "min_of_two_count"), json!(2), "{mode}");
    }
}

#[test]
fn a_qualified_read_of_a_parameter_or_relation_is_refused() {
    for formula in ["candidate.minimum_gap > 0", "anchor.period_start > 0"] {
        let error = refusal(formula);
        assert!(
            error.contains("[relation_role_entity_mismatch]"),
            "{formula}: {error}"
        );
    }
    let error = refusal("candidate.relative_of");
    assert!(error.contains("relation atoms"), "{error}");
}

/// Outside predicates, `anchor` and `candidate` stay ordinary names: a bare
/// one reads an input, and `t.name` is the field access rules never had.
#[test]
fn terms_are_ordinary_names_outside_predicates() {
    let extra = r#"
  - name: reads_an_input_named_anchor
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: anchor + 1
"#;
    let program = program_of(&compile(&people_module(extra)).expect("module compiles"));
    let rule = program["derived"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|rule| rule["name"] == "reads_an_input_named_anchor")
        .expect("rule");
    assert_eq!(
        rule["expr"]["items"][0],
        json!({"kind": "input", "name": "anchor"})
    );

    let extra = r#"
  - name: field_access
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: anchor.age + 1
"#;
    let error = compile(&people_module(extra))
        .expect_err("a rule body has no terms")
        .to_string();
    assert!(error.contains("bare field access"), "{error}");
}

/// A `[Household, Person]` relation: `anchor` is a Household, so reading the
/// Person rule `is_adult` through it is refused.
#[test]
fn a_rule_read_through_a_term_of_another_kind_is_refused() {
    let module = |formula: &str| {
        format!(
            r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Household, Person]
  - name: is_adult
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: adult_member_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      slot_entities: [Household, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
  - name: adults
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(adult_member_of)
"#
        )
    };
    compile(&module("candidate.is_adult")).expect("a Person rule at the Person term compiles");
    let error = compile(&module("anchor.is_adult")).expect_err("a Person rule at a Household");
    assert_eq!(
        typing_codes(&error),
        vec![RelationTypingCode::RelationRoleEntityMismatch]
    );
    assert!(
        error
            .to_string()
            .contains("[relation_role_entity_mismatch]"),
        "{error}"
    );
}

/// Kinds stamped from a source do not cover a same-kind relation, so a
/// derivation that reads through a term must declare its slot kinds.
#[test]
fn a_derivation_without_slot_kinds_cannot_read_through_a_term() {
    let source = people_module(&relative_relation("untyped", "", "candidate.age > 18"));
    let error = compile(&source).expect_err("the derivation declares no slot kinds");
    assert_eq!(
        typing_codes(&error),
        vec![RelationTypingCode::RelationRolesNeedSlotKinds]
    );
    // The same predicate written bare compiles, as it does today.
    compile(&people_module(&relative_relation(
        "untyped", "", "age > 18",
    )))
    .expect("a bare read needs no slot kinds");
}

/// A derivation that declares no kinds takes them from a source whose slots
/// hold different kinds, so its terms are kind-checked without a declaration.
#[test]
fn a_source_with_two_kinds_supplies_the_terms_kinds() {
    let module = |formula: &str| {
        format!(
            r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: is_adult
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: adult_member_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      current_slot: 1
      related_slot: 0
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
  - name: adult_members
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(adult_member_of)
"#
        )
    };
    let artifact = compile(&module("candidate.is_adult")).expect("the source supplies the kinds");
    let dataset = json!({
        "inputs": [age("p1", 30), age("p2", 12)],
        "relations": [
            tuple("member_of_household", "p1", "h1"),
            tuple("member_of_household", "p2", "h1"),
        ],
    });
    let response = run(
        "explain",
        &program_of(&artifact),
        &dataset,
        "h1",
        &["adult_members"],
    );
    assert_eq!(output(&response, "adult_members"), json!(1));
    // `anchor` is the household, so a person's rule read through it is refused.
    let error = compile(&module("anchor.is_adult")).expect_err("`anchor` is a Household id");
    assert_eq!(
        typing_codes(&error),
        vec![RelationTypingCode::RelationRoleEntityMismatch]
    );
}

/// A filtered entity's ids are its source's current ids, so a rule of the
/// filtered entity reads at a term of that kind.
#[test]
fn a_filtered_entitys_rule_reads_at_a_term_of_its_source_kind() {
    let source = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: is_adult
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: adult_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: AdultUnit
      member_relation: adults
      slot_entities: [Person, Household]
      current_slot: 1
      related_slot: 0
    versions:
      - effective_from: 2026-01-01
        formula: candidate.is_adult
  - name: adult_unit_size
    kind: derived
    entity: AdultUnit
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(adults)
  - name: child_in_adult_household_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      slot_entities: [Person, Household]
      current_slot: 1
      related_slot: 0
    versions:
      - effective_from: 2026-01-01
        formula: anchor.adult_unit_size >= 2 and not candidate.is_adult
  - name: children_in_adult_household
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(child_in_adult_household_of)
"#;
    let program = program_of(&compile(source).expect("an AdultUnit id is a Household id"));
    let dataset = |adults: &[&str]| {
        let mut inputs = vec![age("c1", 8)];
        let mut relations = vec![tuple("member_of_household", "c1", "h1")];
        for adult in adults {
            inputs.push(age(adult, 40));
            relations.push(tuple("member_of_household", adult, "h1"));
        }
        json!({"inputs": inputs, "relations": relations})
    };
    for mode in ["explain", "fast"] {
        for (adults, expected) in [(&["a1", "a2"][..], 1), (&["a1"][..], 0)] {
            let response = run(
                mode,
                &program,
                &dataset(adults),
                "h1",
                &["children_in_adult_household"],
            );
            assert_eq!(
                output(&response, "children_in_adult_household"),
                json!(expected),
                "{mode} {adults:?}"
            );
        }
    }
}

#[test]
fn a_cycle_through_a_qualified_rule_read_is_refused() {
    // The scalar form (`t.rule > 0`) and the judgment form (`t.rule`, a bare
    // judgment) lower to different expression variants, each with its own
    // dependency edge.
    let forms = [
        ("loops", "Integer", "len(looping_relative_of)", ".loops > 0"),
        (
            "loops_j",
            "Judgment",
            "len(looping_relative_of) > 0",
            ".loops_j",
        ),
    ];
    for (rule, dtype, body, read) in forms {
        for term in ["anchor", "candidate"] {
            let extra = format!(
                r#"
  - name: {rule}
    kind: derived
    entity: Person
    dtype: {dtype}
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: {body}
  - name: looping_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: {term}{read}
"#
            );
            let error = compile(&people_module(&extra)).expect_err("the cycle is refused");
            assert!(
                matches!(
                    &error,
                    CompileError::CyclicDependency { cycle }
                        if cycle.split(", ").any(|name| name == rule)
                ),
                "{rule} {term}: {error}"
            );
        }
    }
}

#[test]
fn dense_refuses_a_qualified_read_by_name() {
    let artifact = compile(&people_module("")).expect("module compiles");
    let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .err()
        .expect("dense refuses relation roles")
        .to_string();
    assert!(error.contains("qualified read"), "{error}");
    assert!(error.contains("#218"), "{error}");
}

// ---------------------------------------------------------------------------
// Programs written as JSON, as an artifact or a request can carry them
// ---------------------------------------------------------------------------

fn dec(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "decimal", "value": value.to_string()}})
}

fn input(name: &str) -> Value {
    json!({"kind": "input", "name": name})
}

fn derived(name: &str) -> Value {
    json!({"kind": "derived", "name": name})
}

fn role_input(term: &str, name: &str) -> Value {
    json!({"kind": "role_input", "term": term, "name": name})
}

fn role_derived(term: &str, name: &str) -> Value {
    json!({"kind": "role_derived", "term": term, "name": name})
}

fn cmp(left: Value, op: &str, right: Value) -> Value {
    json!({"kind": "comparison", "left": left, "op": op, "right": right})
}

fn scalar_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "decimal", "unit": null,
        "semantics": "scalar", "expr": expr,
    })
}

fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": entity, "dtype": "judgment", "unit": null,
        "semantics": "judgment", "expr": expr,
    })
}

fn count_related(relation: &str, current: usize, related: usize, filter: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "count_related", "relation": relation,
        "current_slot": current, "related_slot": related,
    });
    if let Some(filter) = filter {
        expr["where"] = filter;
    }
    expr
}

fn sum_related(relation: &str, current: usize, related: usize, value: &str) -> Value {
    json!({
        "kind": "sum_related", "relation": relation,
        "current_slot": current, "related_slot": related,
        "value": {"kind": "derived", "name": value},
    })
}

fn household_members() -> Value {
    json!({"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]})
}

fn relatives() -> Value {
    json!({"name": "relative_of", "arity": 2, "slot_entities": ["Person", "Person"]})
}

/// `household_rel`: households (slot 1) to their members (slot 0).
fn household_relation(predicate: Value) -> Value {
    json!({
        "name": "household_rel", "arity": 2, "slot_entities": ["Person", "Household"],
        "derivation": {
            "source_relation": "member_of_household", "current_slot": 1, "related_slot": 0,
            "slot_entities": ["Person", "Household"], "predicate": predicate,
        },
    })
}

/// `relative_rel`: people (slot 0) to their relatives (slot 1).
fn relative_relation_json(predicate: Value) -> Value {
    json!({
        "name": "relative_rel", "arity": 2, "slot_entities": ["Person", "Person"],
        "derivation": {
            "source_relation": "relative_of", "current_slot": 0, "related_slot": 1,
            "slot_entities": ["Person", "Person"], "predicate": predicate,
        },
    })
}

/// The rules the world's relations read, and the aggregations over them.
fn world_rules() -> Vec<Value> {
    vec![
        scalar_rule(
            "p0",
            "Person",
            json!({"kind": "add", "items": [input("age"), dec(1)]}),
        ),
        scalar_rule(
            "p1",
            "Person",
            json!({"kind": "mul", "left": input("score"), "right": dec(2)}),
        ),
        judgment_rule("pj", "Person", cmp(input("age"), "gte", dec(18))),
        // Strict binding refuses a supplied input no node reads, so a rule
        // reads `flag` whether or not a generated predicate does.
        judgment_rule(
            "pf",
            "Person",
            cmp(
                input("flag"),
                "eq",
                json!({"kind": "literal", "value": {"kind": "bool", "value": true}}),
            ),
        ),
        scalar_rule(
            "h0",
            "Household",
            json!({"kind": "add", "items": [input("size"), dec(0)]}),
        ),
        scalar_rule(
            "h1",
            "Household",
            json!({"kind": "sub", "left": input("size"), "right": dec(3)}),
        ),
        judgment_rule("hj", "Household", cmp(input("size"), "gt", dec(2))),
        scalar_rule(
            "n_household",
            "Household",
            count_related("household_rel", 1, 0, None),
        ),
        scalar_rule(
            "s_household",
            "Household",
            sum_related("household_rel", 1, 0, "p0"),
        ),
        scalar_rule(
            "n_relatives",
            "Person",
            count_related("relative_rel", 0, 1, None),
        ),
    ]
}

fn world_program(household_predicate: Value, relative_predicate: Value) -> Value {
    json!({
        "relations": [
            household_members(),
            relatives(),
            household_relation(household_predicate),
            relative_relation_json(relative_predicate),
        ],
        "derived": world_rules(),
    })
}

fn compile_json(program: &Value) -> Result<CompiledProgramArtifact, CompileError> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program deserializes");
    CompiledProgramArtifact::compile(spec)
}

fn always() -> Value {
    cmp(dec(1), "eq", dec(1))
}

/// A read through a term in a rule body, or in a `where` clause inside a
/// predicate, is refused on compile and on a raw request alike.
#[test]
fn json_programs_refuse_a_qualified_read_outside_scope() {
    let mut in_rule = world_program(always(), always());
    in_rule["derived"]
        .as_array_mut()
        .expect("rules")
        .push(scalar_rule("bad", "Person", role_input("candidate", "age")));
    let in_where = world_program(
        cmp(
            count_related(
                "relative_of",
                0,
                1,
                Some(cmp(role_derived("candidate", "p0"), "gt", dec(0))),
            ),
            "gt",
            dec(0),
        ),
        always(),
    );
    let filtered = |kind: &str| {
        let mut aggregate = json!({
            "kind": kind, "relation": "relative_of", "current_slot": 0, "related_slot": 1,
            "value": {"kind": "derived", "name": "p0"},
            "where": cmp(role_derived("candidate", "p0"), "gt", dec(0)),
        });
        if kind == "extreme_related" {
            aggregate["extreme"] = json!("max");
        }
        world_program(cmp(aggregate, "gt", dec(0)), always())
    };
    for program in [
        in_rule,
        in_where,
        filtered("extreme_related"),
        filtered("single_related"),
    ] {
        let error = compile_json(&program).expect_err("compile refuses");
        assert_eq!(
            typing_codes(&error),
            vec![RelationTypingCode::RelationRoleOutsideScope],
            "{error}"
        );
        let queries = json!([{"entity_id": "h1", "period": month(), "outputs": ["n_household"]}]);
        let error = execute_request(request("explain", &program, &world_dataset(), &queries))
            .expect_err("a raw request is refused");
        assert!(
            error.to_string().contains("relation_role_outside_scope"),
            "{error}"
        );
    }
}

/// An artifact whose derivation drops the slot kinds a qualified read needs
/// is refused on load.
#[test]
fn an_artifact_without_slot_kinds_for_a_term_is_refused_on_load() {
    let artifact = compile(&people_module("")).expect("module compiles");
    let mut value = serde_json::to_value(&artifact).expect("artifact serializes");
    for relation in value["program"]["relations"]
        .as_array_mut()
        .expect("relations")
    {
        if relation["name"] == "older_relative_of" {
            relation["derivation"]["slot_entities"] = json!([]);
        }
    }
    let error = CompiledProgramArtifact::from_json_str(&value.to_string())
        .expect_err("the artifact is refused");
    assert!(
        error.to_string().contains("relation_roles_need_slot_kinds"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

const PEOPLE: [&str; 4] = ["p1", "p2", "p3", "p4"];
const HOUSEHOLDS: [&str; 2] = ["h1", "h2"];

/// A read in a generated predicate, named by what today's routing reads.
#[derive(Clone, Copy, Debug)]
enum Read {
    /// An input of the candidate: bare, or `candidate.name`.
    CandidateInput(&'static str),
    /// A rule today's routing evaluates on the anchor.
    AnchorRule(usize),
    /// A rule today's routing evaluates on the candidate.
    CandidateRule(usize),
    Literal(i64),
}

#[derive(Clone, Debug)]
enum Scalar {
    Read(Read),
    Add(Box<Scalar>, Box<Scalar>),
    Sub(Box<Scalar>, Box<Scalar>),
    Mul(Box<Scalar>, Box<Scalar>),
    If(Box<Judgment>, Box<Scalar>, Box<Scalar>),
}

#[derive(Clone, Debug)]
enum Judgment {
    Compare(Scalar, &'static str, Scalar),
    AnchorJudgment,
    CandidateJudgment,
    CandidateFlag,
    And(Vec<Judgment>),
    Or(Vec<Judgment>),
    Not(Box<Judgment>),
}

/// The two relations of the world. Their routing differs: in the
/// `[Person, Household]` one every rule reads the end of its kind; in the
/// `[Person, Person]` one every rule reads the anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Relation {
    Household,
    Relative,
}

impl Relation {
    /// The rules a read evaluates: (anchor rules, candidate rules).
    fn rules(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Household => (&["h0", "h1"], &["p0", "p1"]),
            Self::Relative => (&["p0", "p1"], &[]),
        }
    }

    fn judgments(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Household => ("hj", Some("pj")),
            Self::Relative => ("pj", None),
        }
    }
}

fn read_strategy(relation: Relation) -> BoxedStrategy<Read> {
    let (anchor, candidate) = relation.rules();
    let mut options: Vec<BoxedStrategy<Read>> = vec![
        prop_oneof![Just("age"), Just("score")]
            .prop_map(Read::CandidateInput)
            .boxed(),
        (0..anchor.len()).prop_map(Read::AnchorRule).boxed(),
        (-3_i64..40).prop_map(Read::Literal).boxed(),
    ];
    if !candidate.is_empty() {
        options.push((0..candidate.len()).prop_map(Read::CandidateRule).boxed());
    }
    proptest::strategy::Union::new(options).boxed()
}

fn comparison_strategy(relation: Relation) -> BoxedStrategy<Judgment> {
    let op = prop_oneof![Just("lt"), Just("gt"), Just("eq"), Just("gte")];
    (scalar_strategy(relation), op, scalar_strategy(relation))
        .prop_map(|(left, op, right)| Judgment::Compare(left, op, right))
        .boxed()
}

fn scalar_strategy(relation: Relation) -> BoxedStrategy<Scalar> {
    read_strategy(relation)
        .prop_map(Scalar::Read)
        .prop_recursive(3, 12, 2, move |inner| {
            let condition =
                (read_strategy(relation), read_strategy(relation)).prop_map(|(left, right)| {
                    Judgment::Compare(Scalar::Read(left), "gt", Scalar::Read(right))
                });
            prop_oneof![
                (inner.clone(), inner.clone())
                    .prop_map(|(left, right)| Scalar::Add(Box::new(left), Box::new(right))),
                (inner.clone(), inner.clone())
                    .prop_map(|(left, right)| Scalar::Sub(Box::new(left), Box::new(right))),
                (inner.clone(), inner.clone())
                    .prop_map(|(left, right)| Scalar::Mul(Box::new(left), Box::new(right))),
                (condition, inner.clone(), inner).prop_map(|(condition, then, otherwise)| {
                    Scalar::If(Box::new(condition), Box::new(then), Box::new(otherwise))
                }),
            ]
        })
        .boxed()
}

fn judgment_strategy(relation: Relation) -> BoxedStrategy<Judgment> {
    let mut leaves: Vec<BoxedStrategy<Judgment>> = vec![
        comparison_strategy(relation),
        Just(Judgment::AnchorJudgment).boxed(),
        Just(Judgment::CandidateFlag).boxed(),
    ];
    if relation.judgments().1.is_some() {
        leaves.push(Just(Judgment::CandidateJudgment).boxed());
    }
    proptest::strategy::Union::new(leaves)
        .prop_recursive(3, 12, 3, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 1..4).prop_map(Judgment::And),
                proptest::collection::vec(inner.clone(), 1..4).prop_map(Judgment::Or),
                inner.prop_map(|item| Judgment::Not(Box::new(item))),
            ]
        })
        .boxed()
}

/// How a generated predicate is written.
#[derive(Clone, Copy, PartialEq)]
enum Spelling {
    /// Bare reads, routed as today.
    Routed,
    /// Each read qualified by the term today's routing reads.
    Qualified,
}

fn render_read(read: Read, relation: Relation, spelling: Spelling) -> Value {
    let (anchor, candidate) = relation.rules();
    match (read, spelling) {
        (Read::Literal(value), _) => dec(value),
        (Read::CandidateInput(name), Spelling::Routed) => input(name),
        (Read::CandidateInput(name), Spelling::Qualified) => role_input("candidate", name),
        (Read::AnchorRule(index), Spelling::Routed) => derived(anchor[index]),
        (Read::AnchorRule(index), Spelling::Qualified) => role_derived("anchor", anchor[index]),
        (Read::CandidateRule(index), Spelling::Routed) => derived(candidate[index]),
        (Read::CandidateRule(index), Spelling::Qualified) => {
            role_derived("candidate", candidate[index])
        }
    }
}

fn render_scalar(expr: &Scalar, relation: Relation, spelling: Spelling) -> Value {
    let pair = |left: &Scalar, right: &Scalar| {
        (
            render_scalar(left, relation, spelling),
            render_scalar(right, relation, spelling),
        )
    };
    match expr {
        Scalar::Read(read) => render_read(*read, relation, spelling),
        Scalar::Add(left, right) => {
            let (left, right) = pair(left, right);
            json!({"kind": "add", "items": [left, right]})
        }
        Scalar::Sub(left, right) => {
            let (left, right) = pair(left, right);
            json!({"kind": "sub", "left": left, "right": right})
        }
        Scalar::Mul(left, right) => {
            let (left, right) = pair(left, right);
            json!({"kind": "mul", "left": left, "right": right})
        }
        Scalar::If(condition, then, otherwise) => {
            let (then, otherwise) = pair(then, otherwise);
            json!({
                "kind": "if",
                "condition": render_judgment(condition, relation, spelling),
                "then_expr": then, "else_expr": otherwise,
            })
        }
    }
}

fn render_judgment(expr: &Judgment, relation: Relation, spelling: Spelling) -> Value {
    let (anchor, candidate) = relation.judgments();
    let rule = |term: &str, name: &str| match spelling {
        Spelling::Routed => json!({"kind": "derived", "name": name}),
        Spelling::Qualified => role_derived(term, name),
    };
    match expr {
        Judgment::Compare(left, op, right) => cmp(
            render_scalar(left, relation, spelling),
            op,
            render_scalar(right, relation, spelling),
        ),
        Judgment::AnchorJudgment => rule("anchor", anchor),
        Judgment::CandidateJudgment => rule("candidate", candidate.expect("generated only here")),
        Judgment::CandidateFlag => {
            let flag = match spelling {
                Spelling::Routed => input("flag"),
                Spelling::Qualified => role_input("candidate", "flag"),
            };
            cmp(
                flag,
                "eq",
                json!({"kind": "literal", "value": {"kind": "bool", "value": true}}),
            )
        }
        Judgment::And(items) | Judgment::Or(items) => json!({
            "kind": if matches!(expr, Judgment::And(_)) { "and" } else { "or" },
            "items": items
                .iter()
                .map(|item| render_judgment(item, relation, spelling))
                .collect::<Vec<_>>(),
        }),
        Judgment::Not(item) => {
            json!({"kind": "not", "item": render_judgment(item, relation, spelling)})
        }
    }
}

#[derive(Clone, Debug)]
struct World {
    ages: Vec<i64>,
    scores: Vec<i64>,
    flags: Vec<bool>,
    sizes: Vec<i64>,
    /// Whether each person is a member of each household.
    memberships: Vec<bool>,
    /// Whether each ordered pair of distinct people are relatives.
    relatives: Vec<bool>,
}

fn world_strategy() -> impl Strategy<Value = World> {
    (
        proptest::collection::vec(0_i64..80, PEOPLE.len()),
        proptest::collection::vec(-5_i64..6, PEOPLE.len()),
        proptest::collection::vec(any::<bool>(), PEOPLE.len()),
        proptest::collection::vec(1_i64..7, HOUSEHOLDS.len()),
        proptest::collection::vec(any::<bool>(), PEOPLE.len() * HOUSEHOLDS.len()),
        proptest::collection::vec(any::<bool>(), PEOPLE.len() * (PEOPLE.len() - 1)),
    )
        .prop_map(
            |(ages, scores, flags, sizes, memberships, relatives)| World {
                ages,
                scores,
                flags,
                sizes,
                memberships,
                relatives,
            },
        )
}

fn decimal(value: i64) -> Value {
    json!({"kind": "decimal", "value": value.to_string()})
}

impl World {
    fn inputs(&self) -> Vec<Value> {
        let mut inputs = Vec::new();
        for (index, person) in PEOPLE.iter().enumerate() {
            inputs.push(input_record(
                "Person",
                person,
                "age",
                decimal(self.ages[index]),
            ));
            inputs.push(input_record(
                "Person",
                person,
                "score",
                decimal(self.scores[index]),
            ));
            inputs.push(input_record(
                "Person",
                person,
                "flag",
                json!({"kind": "bool", "value": self.flags[index]}),
            ));
        }
        for (index, household) in HOUSEHOLDS.iter().enumerate() {
            inputs.push(input_record(
                "Household",
                household,
                "size",
                decimal(self.sizes[index]),
            ));
        }
        inputs
    }

    fn tuples(&self) -> Vec<Value> {
        let mut tuples = Vec::new();
        for (index, person) in PEOPLE.iter().enumerate() {
            for (offset, household) in HOUSEHOLDS.iter().enumerate() {
                if self.memberships[index * HOUSEHOLDS.len() + offset] {
                    tuples.push(tuple("member_of_household", person, household));
                }
            }
        }
        let mut pair = 0;
        for from in PEOPLE {
            for to in PEOPLE {
                if from == to {
                    continue;
                }
                if self.relatives[pair] {
                    tuples.push(tuple("relative_of", from, to));
                }
                pair += 1;
            }
        }
        tuples
    }

    fn dataset(&self) -> Value {
        json!({"inputs": self.inputs(), "relations": self.tuples()})
    }
}

fn world_dataset() -> Value {
    World {
        ages: vec![30, 45, 12, 70],
        scores: vec![1, -2, 3, 0],
        flags: vec![true, false, true, true],
        sizes: vec![2, 4],
        memberships: vec![true, false, true, true, false, true, true, false],
        relatives: vec![true; PEOPLE.len() * (PEOPLE.len() - 1)],
    }
    .dataset()
}

fn world_queries() -> Value {
    let mut queries = HOUSEHOLDS
        .iter()
        .map(|household| {
            json!({"entity_id": household, "period": month(),
                   "outputs": ["n_household", "s_household"]})
        })
        .collect::<Vec<_>>();
    queries.extend(
        PEOPLE.iter().map(
            |person| json!({"entity_id": person, "period": month(), "outputs": ["n_relatives"]}),
        ),
    );
    Value::Array(queries)
}

/// The whole response, or the error, as text.
fn respond(mode: &str, program: &Value, dataset: &Value, queries: &Value) -> String {
    match execute_request(request(mode, program, dataset, queries)) {
        Ok(response) => serde_json::to_string(&response).expect("responses serialize"),
        Err(error) => format!("Err({error})"),
    }
}

fn outputs_of(response: &str) -> Value {
    let response: Value = serde_json::from_str(response).expect("a response");
    response["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| result["outputs"].clone())
        .collect()
}

fn predicates() -> impl Strategy<Value = (Judgment, Judgment)> {
    (
        judgment_strategy(Relation::Household),
        judgment_strategy(Relation::Relative),
    )
}

fn program(household: &Judgment, relative: &Judgment, spelling: Spelling) -> Value {
    world_program(
        render_judgment(household, Relation::Household, spelling),
        render_judgment(relative, Relation::Relative, spelling),
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Invariant 4: where today's routing reads an id, the read qualified by
    /// that id's term gives the same response, trace included.
    #[test]
    fn qualified_reads_agree_with_routing(
        (household, relative) in predicates(),
        world in world_strategy(),
    ) {
        let routed = program(&household, &relative, Spelling::Routed);
        let qualified = program(&household, &relative, Spelling::Qualified);
        let dataset = world.dataset();
        let queries = world_queries();
        for mode in ["explain", "fast"] {
            let routed = respond(mode, &routed, &dataset, &queries);
            prop_assert!(!routed.starts_with("Err("), "{}", routed);
            prop_assert_eq!(&respond(mode, &qualified, &dataset, &queries), &routed, "{}", mode);
        }
    }

    /// Invariant 1: permuting input records and relation tuples changes no
    /// byte of the response.
    #[test]
    fn order_of_records_changes_nothing(
        (household, relative) in predicates(),
        world in world_strategy(),
        input_seed in any::<u64>(),
        tuple_seed in any::<u64>(),
    ) {
        let program = program(&household, &relative, Spelling::Qualified);
        let queries = world_queries();
        let mut inputs = world.inputs();
        let mut tuples = world.tuples();
        let base = json!({"inputs": inputs.clone(), "relations": tuples.clone()});
        shuffle(&mut inputs, input_seed);
        shuffle(&mut tuples, tuple_seed);
        let permuted = json!({"inputs": inputs, "relations": tuples});
        for mode in ["explain", "fast"] {
            prop_assert_eq!(
                respond(mode, &program, &permuted, &queries),
                respond(mode, &program, &base, &queries),
                "{}", mode
            );
        }
    }

    /// Invariant 7: every suspend depth gives the bytes deferral off gives.
    #[test]
    fn deferral_is_transparent_to_qualified_reads(
        (household, relative) in predicates(),
        world in world_strategy(),
    ) {
        let program = program(&household, &relative, Spelling::Qualified);
        let dataset = world.dataset();
        let queries = world_queries();
        for mode in ["explain", "fast"] {
            let recursive =
                with_suspend_depth(usize::MAX, || respond(mode, &program, &dataset, &queries));
            for threshold in 1..=6 {
                let deferred =
                    with_suspend_depth(threshold, || respond(mode, &program, &dataset, &queries));
                prop_assert_eq!(&deferred, &recursive, "{} at {}", mode, threshold);
            }
        }
    }

    /// Invariant 9: explain and fast agree on every output, and dense
    /// refuses a program with a qualified read by name.
    #[test]
    fn modes_agree_and_dense_refuses_by_name(
        (household, relative) in predicates(),
        world in world_strategy(),
    ) {
        let program = program(&household, &relative, Spelling::Qualified);
        let dataset = world.dataset();
        let queries = world_queries();
        let explain = respond("explain", &program, &dataset, &queries);
        let fast = respond("fast", &program, &dataset, &queries);
        prop_assert_eq!(outputs_of(&fast), outputs_of(&explain));

        // Each entity's outputs reach one relation: households count
        // `household_rel`, people `relative_rel`.
        let artifact = compile_json(&program).expect("program compiles");
        for (entity, relation) in [("Household", "household_rel"), ("Person", "relative_rel")] {
            let predicate = predicate_of(&program, relation).to_string();
            if !predicate.contains("\"role_") {
                continue;
            }
            let error = DenseCompiledProgram::from_artifact(&artifact, Some(entity))
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            prop_assert!(
                error.contains("qualified read") && error.contains("#218"),
                "{}: {}", entity, error
            );
        }
    }

    /// Invariant 11: a program compiles exactly when every rule read through
    /// a term has the term's kind, the refusal names each read that does
    /// not, and a compiled program evaluates each rule on ids of its kind.
    #[test]
    fn rule_reads_through_terms_are_kind_checked(
        reads in proptest::collection::vec(
            (prop_oneof![Just("anchor"), Just("candidate")],
             prop_oneof![Just("p0"), Just("p1"), Just("h0"), Just("h1")]),
            1..5,
        ),
        world in world_strategy(),
    ) {
        let predicate = json!({"kind": "and", "items": reads
            .iter()
            .map(|(term, rule)| cmp(role_derived(term, rule), "gt", dec(-100)))
            .collect::<Vec<_>>()});
        let program = world_program(predicate, always());
        // In `household_rel` the anchor is a Household and the candidate a
        // Person.
        let mismatched = reads
            .iter()
            .filter(|(term, rule)| (*term == "anchor") != rule.starts_with('h'))
            .map(|(term, rule)| format!("`{term}.{rule}`"))
            .collect::<BTreeSet<_>>();
        match compile_json(&program) {
            Ok(_) => {
                prop_assert!(mismatched.is_empty(), "accepted {:?}", mismatched);
                let response = respond("explain", &program, &world.dataset(), &world_queries());
                let response: Value = serde_json::from_str(&response).expect("a response");
                for result in response["results"].as_array().expect("results") {
                    for node in result["trace"].as_object().expect("trace").values() {
                        let name = node["name"].as_str().expect("name");
                        let id = node["entity_id"].as_str().expect("id");
                        let kind = if name.starts_with('h') || name.ends_with("household") {
                            'h'
                        } else {
                            'p'
                        };
                        prop_assert!(id.starts_with(kind), "{} evaluated on {}", name, id);
                    }
                }
            }
            Err(error) => {
                let CompileError::RelationTyping { report, .. } = &error else {
                    return Err(TestCaseError::fail(format!("unexpected error {error}")));
                };
                prop_assert_eq!(report.violations.len(), mismatched.len(), "{}", report);
                for violation in &report.violations {
                    prop_assert_eq!(
                        violation.code,
                        RelationTypingCode::RelationRoleEntityMismatch
                    );
                    prop_assert!(
                        mismatched.iter().any(|read| violation.message.contains(read.as_str())),
                        "{}", violation.message
                    );
                }
            }
        }
    }
}

/// A seeded Fisher-Yates shuffle, so a failing case replays.
fn shuffle<T>(items: &mut [T], mut seed: u64) {
    for index in (1..items.len()).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let other = (seed >> 33) as usize % (index + 1);
        items.swap(index, other);
    }
}
