//! Relation atoms and term comparison (#214, PR 2).
//!
//! Properties cover §7 invariants 1, 3, 6, 7, 8, 9 and 11. Monotonicity
//! deliberately excludes derived reads, rules that aggregate, comparisons
//! of counts, and bare membership tests, as the design requires.

use std::collections::BTreeSet;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::engine::Engine;
use axiom_rules_engine::interface::derive_interface;
use axiom_rules_engine::model::{Period, ScalarValue};
use axiom_rules_engine::spec::{DatasetSpec, ProgramSpec};
use proptest::prelude::*;
use serde_json::{Value, json};

fn month() -> Value {
    json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"})
}

fn tuple(name: &str, ids: &[&str]) -> Value {
    json!({"name": name, "tuple": ids,
        "interval": {"start": "2026-01-01", "end": "2026-12-31"}})
}

fn input(id: &str, name: &str, n: i64) -> Value {
    json!({"name": name, "entity": "Person", "entity_id": id,
        "interval": {"start": "2026-01-01", "end": "2026-12-31"},
        "value": {"kind": "integer", "value": n}})
}

fn literal(n: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": n}})
}

fn yes() -> Value {
    json!({"kind": "comparison", "left": literal(1), "op": "eq", "right": literal(1)})
}

fn atom(name: &str, terms: &[&str]) -> Value {
    json!({"kind": "relation_atom", "relation": name, "terms": terms})
}

fn same(left: &str, right: &str, negated: bool) -> Value {
    json!({"kind": "same_entity", "left": left, "right": right, "negated": negated})
}

fn not(item: Value) -> Value {
    json!({"kind": "not", "item": item})
}

fn count(name: &str, relation: &str, entity: &str, current: usize, related: usize) -> Value {
    json!({"name": name, "entity": entity, "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": {"kind": "count_related", "relation": relation,
        "current_slot": current, "related_slot": related}})
}

fn derived_relation(
    name: &str,
    source: &str,
    kinds: &[&str],
    current: usize,
    predicate: Value,
) -> Value {
    json!({"name": name, "arity": kinds.len(), "slot_entities": kinds,
        "derivation": {"source_relation": source, "slot_entities": kinds,
        "current_slot": current, "related_slot": 1 - current, "predicate": predicate}})
}

fn compile_json(program: &Value) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::compile(serde_json::from_value(program.clone()).expect("program spec"))
}

fn execute(mode: &str, program: &Value, dataset: &Value, ids: &[String]) -> Result<Value, String> {
    let queries: Vec<Value> = ids
        .iter()
        .map(|id| json!({"entity_id": id, "period": month(), "outputs": ["total"]}))
        .collect();
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": mode, "program": program, "dataset": dataset, "queries": queries,
    }))
    .expect("request");
    execute_request(request)
        .map(|response| serde_json::to_value(response).expect("response"))
        .map_err(|error| error.to_string())
}

fn run(mode: &str, program: &Value, dataset: &Value, id: &str) -> Value {
    execute(mode, program, dataset, &[id.to_string()])
        .unwrap_or_else(|error| panic!("{mode}: {error}"))
}

fn output(response: &Value, row: usize) -> i64 {
    response["results"][row]["outputs"]["total"]["value"]["value"]
        .as_i64()
        .unwrap_or_else(|| panic!("integer output: {response}"))
}

fn same_kind_program(predicate: Value) -> Value {
    json!({"relations": [
        {"name": "roster", "arity": 2, "slot_entities": ["Person", "Person"]},
        {"name": "relative_of", "arity": 2, "slot_entities": ["Person", "Person"]},
        derived_relation("selected", "roster", &["Person", "Person"], 0, predicate),
    ], "derived": [count("total", "selected", "Person", 0, 1)]})
}

fn rulespec(formula: &str, extra: &str) -> String {
    // JSON strings are YAML strings too, including formulas with colons.
    let formula = serde_json::to_string(formula).expect("quoted formula");
    format!(
        r#"
format: rulespec/v1
rules:
  - name: relative_of
    kind: data_relation
    data_relation: {{arity: 2, arguments: [Person, Person]}}
  - name: selected
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
  - name: total
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(selected)
{extra}"#
    )
}

fn compile_formula(formula: &str, extra: &str) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::from_rulespec_str(&rulespec(formula, extra))
}

#[test]
fn rulespec_atoms_fix_same_kind_direction_and_comparisons_compare_ids() {
    let dataset = json!({"inputs": [], "relations": [
        tuple("relative_of", &["a", "a"]),
        tuple("relative_of", &["a", "b"]),
        tuple("relative_of", &["a", "c"]),
        tuple("relative_of", &["b", "a"]),
    ]});
    for (formula, expected) in [
        ("relative_of(candidate, anchor)", 2),
        ("candidate != anchor", 2),
        ("candidate == anchor", 1),
        ("(if candidate == anchor: 1 else: 0) == 1", 1),
        ("relative_of(candidate, anchor) and candidate != anchor", 1),
    ] {
        let artifact = compile_formula(formula, "").expect("formula compiles");
        let program = serde_json::to_value(&artifact.program).expect("program");
        for mode in ["explain", "fast"] {
            assert_eq!(
                output(&run(mode, &program, &dataset, "a"), 0),
                expected,
                "{formula} {mode}"
            );
        }
        let encoded = serde_json::to_string(&artifact).expect("artifact");
        assert_eq!(
            serde_json::to_string(
                &CompiledProgramArtifact::from_json_str(&encoded).expect("artifact loads")
            )
            .unwrap(),
            encoded
        );
        let predicate = &program["relations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|relation| relation["name"] == "selected")
            .unwrap()["derivation"]["predicate"];
        if formula == "relative_of(candidate, anchor)" {
            assert_eq!(predicate, &atom("relative_of", &["candidate", "anchor"]));
        } else if formula == "candidate != anchor" {
            assert_eq!(predicate, &same("candidate", "anchor", true));
        }
    }
}

#[test]
fn data_atoms_match_all_slots_and_require_the_whole_period() {
    let mut program = same_kind_program(atom("triple", &["anchor", "candidate", "anchor"]));
    program["relations"].as_array_mut().unwrap().push(json!({
        "name": "triple", "arity": 3, "slot_entities": ["Person", "Person", "Person"]
    }));
    let mut partial = tuple("triple", &["a", "d", "a"]);
    partial["interval"]["end"] = json!("2026-01-15");
    let dataset = json!({"inputs": [], "relations": [
        tuple("roster", &["a", "b"]), tuple("roster", &["a", "c"]), tuple("roster", &["a", "d"]),
        tuple("triple", &["a", "b", "a"]), tuple("triple", &["a", "c", "b"]), partial,
    ]});
    compile_json(&program).expect("n-ary data atoms compile");
    for mode in ["explain", "fast"] {
        assert_eq!(output(&run(mode, &program, &dataset, "a"), 0), 1, "{mode}");
    }
}

#[test]
fn derived_atoms_resolve_the_anchor_named_by_their_current_slot() {
    let mut program = same_kind_program(atom("reverse", &["candidate", "anchor"]));
    program["relations"]
        .as_array_mut()
        .unwrap()
        .push(derived_relation(
            "reverse",
            "relative_of",
            &["Person", "Person"],
            1,
            yes(),
        ));
    let dataset = json!({"inputs": [], "relations": [
        tuple("roster", &["a", "b"]), tuple("roster", &["a", "c"]),
        tuple("relative_of", &["b", "a"]), tuple("relative_of", &["a", "c"]),
    ]});
    compile_json(&program).expect("derived atom compiles");
    for mode in ["explain", "fast"] {
        assert_eq!(output(&run(mode, &program, &dataset, "a"), 0), 1, "{mode}");
    }
}

#[test]
fn atom_and_term_comparison_refusals_name_the_construct() {
    for (formula, code) in [
        ("relative_of(anchor)", "relation_atom_arity_mismatch"),
        ("relative_of(anchor, 1)", "relation_atom_argument_not_term"),
        ("candidate == 1", "relation_term_compared_with_value"),
        ("1 != anchor", "relation_term_compared_with_value"),
        ("anchor < candidate", "relation_term_comparison_operator"),
        // An atom is a judgment, not a value.
        (
            "relative_of(candidate, anchor) > 0",
            "relation_atom_used_as_value",
        ),
        (
            "relative_of(candidate, anchor) + 1 > 1",
            "relation_atom_used_as_value",
        ),
        // A bare term is no judgment: `and`, `or` and `not` refuse it as one,
        // and arithmetic refuses it as a value, not as a comparison.
        ("anchor and age > 3", "relation_term_used_as_value"),
        ("age > 3 or candidate", "relation_term_used_as_value"),
        ("anchor and candidate", "relation_term_used_as_value"),
        ("not anchor", "relation_term_used_as_value"),
        ("anchor + candidate == 2", "relation_term_used_as_value"),
    ] {
        let error = compile_formula(formula, "")
            .expect_err("invalid formula")
            .to_string();
        assert!(error.contains(code), "{formula}: {error}");
    }
    for (schema, predicate, code) in [
        (
            json!({"name": "r", "arity": 2}),
            atom("r", &["anchor", "candidate"]),
            "relation_atom_needs_typed_relation",
        ),
        (
            json!({"name": "r", "arity": 2, "slot_entities": ["Person", "Household"]}),
            atom("r", &["anchor", "candidate"]),
            "relation_atom_kind_mismatch",
        ),
        (
            json!({"name": "len", "arity": 2, "slot_entities": ["Person", "Person"]}),
            atom("len", &["anchor", "candidate"]),
            "relation_name_shadows_builtin",
        ),
    ] {
        let mut program = same_kind_program(predicate);
        program["relations"].as_array_mut().unwrap().push(schema);
        let error = compile_json(&program)
            .expect_err("invalid atom")
            .to_string();
        assert!(error.contains(code), "{error}");
    }
    let mut program = mixed_kind_program(same("anchor", "candidate", false), false, 0);
    let error = compile_json(&program)
        .expect_err("terms have different kinds")
        .to_string();
    assert!(error.contains("relation_term_kind_mismatch"), "{error}");
    program["relations"][2]["derivation"]["predicate"] = atom("edge", &["anchor", "candidate"]);
    let error = compile_json(&program)
        .expect_err("terms are in the wrong slots")
        .to_string();
    assert!(error.contains("relation_atom_kind_mismatch"), "{error}");
    // A derived target's declared kinds are checked as a data relation's are.
    compile_json(&mixed_kind_program(
        atom("eligible", &["candidate", "anchor"]),
        true,
        0,
    ))
    .expect("derived atom in slot order");
    let error = compile_json(&mixed_kind_program(
        atom("eligible", &["anchor", "candidate"]),
        true,
        0,
    ))
    .expect_err("derived atom terms are in the wrong slots")
    .to_string();
    assert!(error.contains("relation_atom_kind_mismatch"), "{error}");
}

#[test]
fn rulespec_refuses_atoms_with_untyped_relations_and_builtin_names() {
    for (extra, formula, code) in [
        (
            "  - name: untyped\n    kind: data_relation\n    data_relation: {arity: 2}\n",
            "untyped(anchor, candidate)",
            "relation_atom_needs_typed_relation",
        ),
        (
            "  - name: len\n    kind: data_relation\n    data_relation: {arity: 2, arguments: [Person, Person]}\n",
            "len(anchor, candidate)",
            "relation_name_shadows_builtin",
        ),
        (
            "  - name: len\n    kind: data_relation\n    data_relation: {arity: 2, arguments: [Person, Person]}\n",
            "len(anchor, candidate) > 0",
            "relation_name_shadows_builtin",
        ),
    ] {
        let error = compile_formula(formula, extra)
            .expect_err("invalid atom")
            .to_string();
        assert!(error.contains(code), "{error}");
    }
}

/// Every built-in function name, listed here independently of the engine.
const BUILTIN_FUNCTIONS: [&str; 23] = [
    "max",
    "min",
    "ceil",
    "floor",
    "days_between",
    "date_add_days",
    "date_add_months",
    "date_add_years",
    "sum_over_periods",
    "max_over_periods",
    "count_over_periods",
    "sum_top_n_over_periods",
    "len",
    "sum",
    "only",
    "input_for_year",
    "only_related",
    "count_where",
    "sum_where",
    "min_where",
    "max_where",
    "exactly_one",
    "exists",
];

#[test]
fn every_builtin_function_name_refuses_as_an_atom() {
    for name in BUILTIN_FUNCTIONS {
        let mut program = same_kind_program(atom(name, &["anchor", "candidate"]));
        program["relations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": name, "arity": 2, "slot_entities": ["Person", "Person"]}));
        let error = compile_json(&program)
            .expect_err("built-in name as an atom")
            .to_string();
        assert!(
            error.contains("relation_name_shadows_builtin"),
            "{name}: {error}"
        );
        let extra = format!(
            "  - name: {name}\n    kind: data_relation\n    data_relation: {{arity: 2, arguments: [Person, Person]}}\n"
        );
        for formula in [
            format!("{name}(anchor, candidate)"),
            format!("{name}(candidate, anchor) > 0"),
        ] {
            let error = compile_formula(&formula, &extra)
                .expect_err("built-in name called with terms")
                .to_string();
            assert!(
                error.contains("relation_name_shadows_builtin"),
                "{formula}: {error}"
            );
        }
    }
}

/// A relation named after a built-in leaves calls of the built-in alone: the
/// same predicate gives the same results with and without that relation.
#[test]
fn builtin_calls_keep_their_meaning_beside_a_relation_of_the_same_name() {
    let dataset = json!({"inputs": [input("a", "age", 40), input("b", "age", 30), input("c", "age", 10)],
        "relations": [tuple("relative_of", &["a", "b"]), tuple("relative_of", &["a", "c"]),
            tuple("relative_of", &["c", "a"])]});
    // `kin` fixes the direction an aggregation over a same-kind relation
    // needs; `adult` is a judgment for `count_where` to test.
    let support = r#"
  - name: kin
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: "true"
  - name: adult
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: age > 17
"#;
    for (name, formula) in [
        ("max", "max(age, 0) > 17"),
        ("min", "min(age, 100) > 17"),
        ("len", "len(kin) > 1"),
        ("count_where", "count_where(kin, adult) > 0"),
        ("exactly_one", "exactly_one(age > 17, age > 35)"),
    ] {
        let shadowing = format!(
            "{support}  - name: {name}\n    kind: data_relation\n    data_relation: {{arity: 2, arguments: [Person, Person]}}\n"
        );
        let unrelated = shadowing.replace(&format!("name: {name}\n"), "name: unrelated\n");
        let results = [shadowing, unrelated].map(|extra| {
            let artifact = compile_formula(formula, &extra)
                .unwrap_or_else(|error| panic!("{formula} beside `{name}`: {error}"));
            let program = serde_json::to_value(&artifact.program).unwrap();
            ["explain", "fast"]
                .map(|mode| ["a", "b", "c"].map(|id| output(&run(mode, &program, &dataset, id), 0)))
        });
        assert_eq!(results[0], results[1], "{formula}");
    }
}

/// A filtered entity's relation (one that declares its own `entity`) is an
/// ordinary derived relation to an atom: the atom tests membership for the
/// term in its current slot, the members `len` counts.
#[test]
fn atoms_target_a_filtered_entitys_relation() {
    let source = |formula: &str| {
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
  - name: selected
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      slot_entities: [Person, Household]
      current_slot: 1
      related_slot: 0
    versions:
      - effective_from: 2026-01-01
        formula: "{formula}"
  - name: total
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(selected)
"#
        )
    };
    let dataset = |c_age: i64| {
        json!({"inputs": [input("a", "age", 40), input("b", "age", 8), input("c", "age", c_age)],
            "relations": [tuple("member_of_household", &["a", "h"]),
                tuple("member_of_household", &["b", "h"]), tuple("member_of_household", &["c", "h"])]})
    };
    let count = |formula: &str, c_age: i64| {
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source(formula))
            .unwrap_or_else(|error| panic!("{formula}: {error}"));
        let program = serde_json::to_value(&artifact.program).unwrap();
        ["explain", "fast"].map(|mode| output(&run(mode, &program, &dataset(c_age), "h"), 0))
    };
    for (c_age, adults) in [(30, 2), (17, 1)] {
        assert_eq!(count("adult_unit(candidate, anchor)", c_age), [adults; 2]);
        assert_eq!(count("candidate.is_adult", c_age), [adults; 2]);
        assert_eq!(
            count("not adult_unit(candidate, anchor)", c_age),
            [3 - adults; 2]
        );
    }
    let error =
        CompiledProgramArtifact::from_rulespec_str(&source("adult_unit(anchor, candidate)"))
            .expect_err("terms out of slot order")
            .to_string();
    assert!(error.contains("relation_atom_kind_mismatch"), "{error}");
    // The member relation names the filtered entity's members for `len` and
    // the other aggregations; it is not a relation an atom can test.
    let error = CompiledProgramArtifact::from_rulespec_str(&source("adults(candidate, anchor)"))
        .expect_err("a member relation is no atom target")
        .to_string();
    assert!(error.contains("unknown function `adults`"), "{error}");
}

#[test]
fn rulespec_refuses_atoms_and_term_comparisons_in_aggregation_arguments() {
    for formula in [
        "count_where(relative_of, relative_of(anchor, candidate)) > 0",
        "count_where(relative_of, candidate != anchor) > 0",
        "sum_where(relative_of, age, relative_of(anchor, candidate)) > 0",
        "sum_where(relative_of, age, candidate == anchor) > 0",
    ] {
        let error = compile_formula(formula, "")
            .expect_err("an aggregation's arguments have no predicate terms")
            .to_string();
        assert!(
            error.contains("relation_role_outside_scope"),
            "{formula}: {error}"
        );
    }
}

#[test]
fn a_derived_atom_refuses_arity_other_than_two() {
    let mut program = same_kind_program(atom("triple", &["anchor", "candidate", "anchor"]));
    program["relations"].as_array_mut().unwrap().extend([
        json!({"name": "triple_source", "arity": 3, "slot_entities": ["Person", "Person", "Person"]}),
        derived_relation("triple", "triple_source", &["Person", "Person", "Person"], 0, yes()),
    ]);
    let error = compile_json(&program)
        .expect_err("a derived atom is binary")
        .to_string();
    assert!(
        error.contains("derived_relation_atom_arity_unsupported"),
        "{error}"
    );
}

#[test]
fn an_imported_relation_used_only_by_an_atom_survives_lowering() {
    struct Modules(std::collections::HashMap<String, String>);
    impl axiom_rules_engine::source::ModuleSource for Modules {
        fn load(
            &self,
            target: &str,
        ) -> Result<Option<String>, axiom_rules_engine::source::SourceError> {
            Ok(self.0.get(target).cloned())
        }
    }
    let imported = "format: rulespec/v1\nrules:\n  - name: imported_pair\n    kind: data_relation\n    data_relation: {arity: 2, arguments: [Person, Person]}\n";
    let root = rulespec("imported_pair(candidate, anchor)", "").replacen(
        "rules:\n",
        "imports:\n  - us:policies/atom-pair\nrules:\n",
        1,
    );
    let source = Modules(std::collections::HashMap::from([
        ("us:policies/atom-pair".into(), imported.into()),
        ("us:policies/atom-root".into(), root),
    ]));
    let artifact =
        CompiledProgramArtifact::from_rulespec_with_source("us:policies/atom-root", &source)
            .expect("imported relation used only in an atom compiles");
    let program = serde_json::to_value(&artifact.program).unwrap();
    let pair = "us:policies/atom-pair#relation.imported_pair";
    assert!(
        program["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|relation| relation["name"] == pair)
    );
    let selected = program["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["name"] == "us:policies/atom-root#relation.selected")
        .unwrap();
    assert_eq!(selected["derivation"]["predicate"]["relation"], pair);
}

#[test]
fn atoms_and_identity_are_refused_outside_predicate_scope() {
    for expr in [
        atom("relative_of", &["anchor", "candidate"]),
        same("anchor", "candidate", false),
    ] {
        let mut in_rule = same_kind_program(yes());
        in_rule["derived"].as_array_mut().unwrap().push(json!({
            "name": "bad", "entity": "Person", "dtype": "judgment", "unit": null,
            "semantics": "judgment", "expr": expr,
        }));
        let mut in_where = same_kind_program(yes());
        in_where["relations"][2]["derivation"]["predicate"] = json!({
            "kind": "comparison", "op": "gt", "right": literal(0), "left": {
                "kind": "count_related", "relation": "relative_of", "current_slot": 0,
                "related_slot": 1, "where": expr,
            }
        });
        for program in [in_rule, in_where] {
            let error = compile_json(&program)
                .expect_err("outside predicate scope")
                .to_string();
            assert!(error.contains("relation_role_outside_scope"), "{error}");
            let error = execute(
                "explain",
                &program,
                &json!({"inputs": [], "relations": []}),
                &["a".to_string()],
            )
            .expect_err("raw requests also refuse");
            assert!(error.contains("relation_role_outside_scope"), "{error}");
        }
    }
}

/// `Engine::new` skips compilation and relation typing, so the evaluator's
/// own guard must fail an evaluation that reaches an atom or a comparison
/// outside a predicate, rather than reading the construct as false.
#[test]
fn engine_new_fails_atoms_and_comparisons_reached_outside_a_predicate() {
    let period = Period::month(2026, 1);
    let data = serde_json::from_value::<DatasetSpec>(json!({"inputs": [], "relations": [
        tuple("roster", &["a", "b"]),
        tuple("relative_of", &["a", "b"]),
        tuple("relative_of", &["b", "a"]),
    ]}))
    .unwrap()
    .to_dataset()
    .unwrap();
    let model = |program: Value| {
        serde_json::from_value::<ProgramSpec>(program)
            .unwrap()
            .to_program()
            .unwrap()
    };
    for (expr, construct) in [
        (
            atom("relative_of", &["anchor", "candidate"]),
            "relation atom",
        ),
        (same("anchor", "candidate", false), "term comparison"),
        (same("anchor", "candidate", true), "term comparison"),
    ] {
        let mut in_rule = same_kind_program(yes());
        in_rule["derived"].as_array_mut().unwrap().push(json!({
            "name": "bad", "entity": "Person", "dtype": "judgment", "unit": null,
            "semantics": "judgment", "expr": expr.clone(),
        }));
        let in_rule = model(in_rule);
        let error = Engine::new(&in_rule, &data)
            .evaluate_judgment("bad", "a", &period)
            .expect_err("rule body")
            .to_string();
        assert!(
            error.contains("relation_role_outside_scope") && error.contains(construct),
            "{error}"
        );
        let mut in_where = same_kind_program(yes());
        in_where["relations"][2]["derivation"]["predicate"] = json!({
            "kind": "comparison", "op": "gt", "right": literal(0), "left": {
                "kind": "count_related", "relation": "relative_of", "current_slot": 0,
                "related_slot": 1, "where": expr,
            }
        });
        let in_where = model(in_where);
        let error = Engine::new(&in_where, &data)
            .evaluate_scalar("total", "a", &period)
            .expect_err("aggregation where clause")
            .to_string();
        assert!(
            error.contains("relation_role_outside_scope") && error.contains(construct),
            "{error}"
        );
    }
}

#[test]
fn atom_edges_refuse_direct_indirect_and_rule_routed_cycles() {
    for indirect in [false, true] {
        let mut program = same_kind_program(atom(
            if indirect { "other" } else { "selected" },
            &["anchor", "candidate"],
        ));
        if indirect {
            program["relations"]
                .as_array_mut()
                .unwrap()
                .push(derived_relation(
                    "other",
                    "roster",
                    &["Person", "Person"],
                    0,
                    atom("selected", &["candidate", "anchor"]),
                ));
        }
        let error = compile_json(&program).expect_err("atom cycle");
        let CompileError::CyclicRelationDependency { cycle } = &error else {
            panic!("expected a relation cycle, got {error}");
        };
        let named = cycle.split(", ").collect::<BTreeSet<_>>();
        let expected = if indirect {
            BTreeSet::from(["other", "selected"])
        } else {
            BTreeSet::from(["selected"])
        };
        assert_eq!(named, expected, "{error}");
    }
    let mut program = same_kind_program(atom("other", &["anchor", "candidate"]));
    program["relations"].as_array_mut().unwrap().push(derived_relation("other", "roster",
        &["Person", "Person"], 0, json!({"kind": "comparison", "op": "gt",
            "left": {"kind": "role_derived", "term": "candidate", "name": "total"}, "right": literal(0)})));
    let error = compile_json(&program).expect_err("atom/rule cycle");
    assert!(
        matches!(error, CompileError::CyclicDependency { .. }),
        "{error}"
    );
}

#[test]
fn supplied_derived_tuples_are_refused_program_wide_and_ignored_by_engine_new() {
    for predicate in [
        atom("relative_of", &["anchor", "candidate"]),
        same("anchor", "candidate", true),
        json!({"kind": "comparison", "left": {"kind": "role_input", "term": "candidate", "name": "age"},
            "op": "gte", "right": literal(18)}),
    ] {
        let mut program = same_kind_program(predicate);
        // This relation itself has no new construct. The restriction applies
        // to every derived relation once any predicate uses one.
        program["relations"]
            .as_array_mut()
            .unwrap()
            .push(derived_relation(
                "legacy",
                "roster",
                &["Person", "Person"],
                0,
                yes(),
            ));
        program["derived"][0] = count("total", "legacy", "Person", 0, 1);
        let dataset = json!({"inputs": [], "relations": [tuple("legacy", &["a", "b"])]});
        let error = execute("explain", &program, &dataset, &["a".to_string()])
            .expect_err("supplied derived tuples");
        assert!(
            error.contains("supplied_tuples_for_derived_relation"),
            "{error}"
        );
        let model = serde_json::from_value::<ProgramSpec>(program)
            .unwrap()
            .to_program()
            .unwrap();
        let data = serde_json::from_value::<DatasetSpec>(dataset)
            .unwrap()
            .to_dataset()
            .unwrap();
        assert_eq!(
            Engine::new(&model, &data)
                .evaluate_scalar("total", "a", &Period::month(2026, 1))
                .unwrap(),
            ScalarValue::Integer(0)
        );
    }
    let mut legacy = same_kind_program(yes());
    legacy["relations"][2]["name"] = json!("legacy");
    legacy["derived"][0] = count("total", "legacy", "Person", 0, 1);
    let dataset = json!({"inputs": [], "relations": [tuple("legacy", &["a", "b"])]});
    assert_eq!(output(&run("explain", &legacy, &dataset, "a"), 0), 1);
}

/// Raw ProgramSpec predicates could look up parameters before relation roles,
/// so a parameter lookup alone keeps the union of supplied and derived
/// members in every path, as it does on engines without relation roles.
#[test]
fn parameter_lookups_in_predicates_keep_the_union_of_supplied_tuples() {
    let mut raw = same_kind_program(json!({"kind": "comparison", "op": "gt",
        "left": {"kind": "parameter_lookup", "parameter": "threshold", "index": literal(0)},
        "right": literal(0)}));
    raw["parameters"] = json!([{"name": "threshold", "unit": null, "versions": [
        {"effective_from": "2020-01-01", "values": {"0": {"kind": "integer", "value": 1}}}]}]);
    let raw_dataset = json!({"inputs": [], "relations": [
        tuple("roster", &["a", "b"]), tuple("selected", &["a", "c"])]});
    let parameter = r#"
  - name: enabled
    kind: parameter
    dtype: Bool
    versions:
      - effective_from: 2026-01-01
        formula: "true"
"#;
    let artifact = compile_formula("enabled", parameter).expect("parameter predicate compiles");
    let rulespec_program = serde_json::to_value(&artifact.program).unwrap();
    let rulespec_dataset = json!({"inputs": [], "relations": [
        tuple("relative_of", &["a", "b"]), tuple("selected", &["a", "c"])]});
    for (program, dataset) in [(raw, raw_dataset), (rulespec_program, rulespec_dataset)] {
        compile_json(&program).expect("parameter predicate compiles");
        for mode in ["explain", "fast"] {
            assert_eq!(output(&run(mode, &program, &dataset, "a"), 0), 2, "{mode}");
        }
        let spec = serde_json::from_value::<ProgramSpec>(program).unwrap();
        let interface = derive_interface(&spec).unwrap();
        assert!(
            interface
                .relations
                .iter()
                .all(|relation| relation.accepts_supplied_tuples)
        );
        let model = spec.to_program().unwrap();
        let data = serde_json::from_value::<DatasetSpec>(dataset)
            .unwrap()
            .to_dataset()
            .unwrap();
        assert_eq!(
            Engine::new(&model, &data)
                .evaluate_scalar("total", "a", &Period::month(2026, 1))
                .unwrap(),
            ScalarValue::Integer(2)
        );
    }
}

#[test]
fn atoms_refuse_lenient_binding_and_dense_refuses_both_constructs_by_name() {
    let program = same_kind_program(atom("relative_of", &["anchor", "candidate"]));
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain", "program": program, "dataset": {"inputs": [], "relations": []},
        "queries": [{"entity_id": "a", "period": month(), "outputs": ["total"]}],
        "relation_binding": "lenient",
    }))
    .unwrap();
    let error = execute_request(request)
        .expect_err("lenient atom binding")
        .to_string();
    assert!(
        error.contains("lenient_binding_with_relation_roles"),
        "{error}"
    );
    for (predicate, name) in [
        (
            atom("relative_of", &["anchor", "candidate"]),
            "relation atom",
        ),
        (same("anchor", "candidate", true), "term comparison"),
    ] {
        let artifact = compile_json(&same_kind_program(predicate)).expect("program compiles");
        let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
            .expect_err("dense refuses");
        assert!(
            matches!(error, DenseCompileError::Unsupported(_)),
            "{error}"
        );
        assert!(
            error.to_string().contains(name) && error.to_string().contains("#218"),
            "{error}"
        );
    }
}

/// The bare test has fixed [Person, Household] slots, so its exact atom is
/// edge(candidate, anchor). A derived test adds rule/input traces to compare.
fn mixed_kind_program(predicate: Value, derived: bool, cutoff: i64) -> Value {
    let mut relations = vec![
        json!({"name": "roster", "arity": 2, "slot_entities": ["Person", "Household"]}),
        json!({"name": "edge", "arity": 2, "slot_entities": ["Person", "Household"]}),
        derived_relation("selected", "roster", &["Person", "Household"], 1, predicate),
    ];
    if derived {
        relations.push(derived_relation("eligible", "edge", &["Person", "Household"], 1,
            json!({"kind": "comparison", "op": "gte", "left": {"kind": "derived", "name": "score"}, "right": literal(cutoff)})));
    }
    json!({"relations": relations, "derived": [
        count("total", "selected", "Household", 1, 0),
        {"name": "score", "entity": "Person", "dtype": "integer", "unit": null,
            "semantics": "scalar", "expr": {"kind": "input", "name": "score"}},
    ]})
}

fn world(scores: &[i64], present: &[bool]) -> Value {
    let inputs: Vec<Value> = scores
        .iter()
        .enumerate()
        .map(|(i, score)| input(&format!("p{i}"), "score", *score))
        .collect();
    let mut relations: Vec<Value> = (0..scores.len())
        .map(|i| tuple("roster", &[&format!("p{i}"), "h"]))
        .collect();
    relations.extend(
        present
            .iter()
            .enumerate()
            .filter(|(_, included)| **included)
            .map(|(i, _)| tuple("edge", &[&format!("p{i}"), "h"])),
    );
    json!({"inputs": inputs, "relations": relations})
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn atoms_agree_with_fixed_bare_tests_in_both_modes(
        rows in prop::collection::vec((-20i64..80, any::<bool>()), 1..9),
        derived in any::<bool>(), cutoff in -20i64..80, double_not in any::<bool>(),
    ) {
        let (scores, present): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        let relation = if derived { "eligible" } else { "edge" };
        let bare = json!({"kind": "relation_member", "relation": relation, "current_slot": 1, "related_slot": 0});
        let explicit = atom(relation, &["candidate", "anchor"]);
        let wrap = |expr| if double_not { not(not(expr)) } else { expr };
        let bare = mixed_kind_program(wrap(bare), derived, cutoff);
        let explicit = mixed_kind_program(wrap(explicit), derived, cutoff);
        compile_json(&bare).expect("bare compiles");
        compile_json(&explicit).expect("atom compiles");
        let dataset = world(&scores, &present);
        let expected = scores.iter().zip(&present).filter(|(score, included)| **included && (!derived || **score >= cutoff)).count() as i64;
        let mut modes = Vec::new();
        for mode in ["explain", "fast"] {
            let reference = run(mode, &bare, &dataset, "h");
            let actual = run(mode, &explicit, &dataset, "h");
            prop_assert_eq!(&actual, &reference);
            prop_assert_eq!(output(&actual, 0), expected);
            modes.push(actual["results"][0]["outputs"].clone());
        }
        prop_assert_eq!(&modes[0], &modes[1]);
    }

    #[test]
    fn atom_outputs_and_traces_ignore_order_and_are_deterministic(
        rows in prop::collection::vec((-20i64..80, any::<bool>()), 1..9),
        shift in 0usize..30, derived in any::<bool>(),
    ) {
        let (scores, present): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        let program = mixed_kind_program(atom(if derived { "eligible" } else { "edge" }, &["candidate", "anchor"]), derived, 18);
        let dataset = world(&scores, &present);
        let mut permuted = dataset.clone();
        for key in ["inputs", "relations"] {
            let records = permuted[key].as_array_mut().unwrap();
            records.reverse();
            if !records.is_empty() { let len = records.len(); records.rotate_left(shift % len); }
        }
        for mode in ["explain", "fast"] {
            let first = serde_json::to_string(&run(mode, &program, &dataset, "h")).unwrap();
            let second = serde_json::to_string(&run(mode, &program, &dataset, "h")).unwrap();
            let reordered = serde_json::to_string(&run(mode, &program, &permuted, "h")).unwrap();
            prop_assert_eq!(&first, &second);
            prop_assert_eq!(&first, &reordered);
        }
    }

    /// Each household has one fixed candidate, so the comparison checks
    /// individual membership, not merely the size of the selected set. A
    /// decoy tuple can give the candidate a second tuple under the same
    /// first-slot id, indexed before the real one, so an atom must search
    /// every tuple for that id.
    #[test]
    fn positive_base_atoms_are_monotone_in_supplied_tuples(
        rows in prop::collection::vec((any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()), 1..9),
        use_or in any::<bool>(), double_not in any::<bool>(),
    ) {
        let positive = |relation| {
            let expr = atom(relation, &["candidate", "anchor"]);
            if double_not { not(not(expr)) } else { expr }
        };
        let predicate = json!({"kind": if use_or { "or" } else { "and" },
            "items": [positive("edge"), positive("other")]});
        let predicate = json!({"kind": "and", "items": [same("candidate", "candidate", false), predicate]});
        let mut program = mixed_kind_program(predicate, false, 0);
        program["relations"].as_array_mut().unwrap().push(json!({"name": "other", "arity": 2,
            "slot_entities": ["Person", "Household"]}));
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut ids = Vec::new();
        let mut expected = Vec::new();
        let holds = |edge: bool, other: bool| i64::from(if use_or { edge || other } else { edge && other });
        for (i, (edge, other, add, decoy)) in rows.into_iter().enumerate() {
            let person = format!("p{i}"); let household = format!("h{i}");
            ids.push(household.clone());
            expected.push((holds(edge, other), holds(edge || add, other || add)));
            before.push(tuple("roster", &[&person, &household]));
            after.push(tuple("roster", &[&person, &household]));
            for (name, present) in [("edge", edge), ("other", other)] {
                if decoy {
                    before.push(tuple(name, &[&person, "decoy"]));
                    after.push(tuple(name, &[&person, "decoy"]));
                }
                if present { before.push(tuple(name, &[&person, &household])); }
                if present || add { after.push(tuple(name, &[&person, &household])); }
            }
        }
        for mode in ["explain", "fast"] {
            let old = execute(mode, &program, &json!({"inputs": [], "relations": before}), &ids).unwrap();
            let new = execute(mode, &program, &json!({"inputs": [], "relations": after}), &ids).unwrap();
            for (i, (old_count, new_count)) in expected.iter().enumerate() {
                prop_assert_eq!(output(&old, i), *old_count);
                prop_assert_eq!(output(&new, i), *new_count);
                prop_assert!(output(&old, i) <= output(&new, i));
            }
        }
    }

    #[test]
    fn atom_kinds_match_each_argument_in_generated_programs(
        current in 0usize..2, reverse in any::<bool>(), wrong_slot in 0usize..3,
        kinds in prop::collection::vec(0usize..4, 2),
    ) {
        let names = ["Person", "Household", "Company", "Estate"];
        let slot_kinds = [names[kinds[0]], names[kinds[1]]];
        let terms = if (current == 0) != reverse { ["anchor", "candidate"] } else { ["candidate", "anchor"] };
        let mut target_kinds = if reverse { [slot_kinds[1], slot_kinds[0]] } else { slot_kinds };
        if wrong_slot < 2 {
            let original = names.iter().position(|kind| *kind == target_kinds[wrong_slot]).unwrap();
            target_kinds[wrong_slot] = names[(original + 1) % names.len()];
        }
        let program = json!({"relations": [
            {"name": "roster", "arity": 2, "slot_entities": slot_kinds},
            {"name": "edge", "arity": 2, "slot_entities": target_kinds},
            derived_relation("selected", "roster", &slot_kinds, current, atom("edge", &terms)),
        ], "derived": [count("total", "selected", slot_kinds[current], current, 1-current)]});
        let result = compile_json(&program);
        if wrong_slot < 2 {
            let error = result.expect_err("a wrong kind cannot compile").to_string();
            prop_assert!(error.contains("relation_atom_kind_mismatch"), "{}", error);
        } else { prop_assert!(result.is_ok(), "{:?}", result.err()); }
    }

    #[test]
    fn generated_identity_tests_compare_ids_and_preserve_mode_order_and_trace(
        edges in prop::collection::vec(any::<bool>(), 16), negated in any::<bool>(),
    ) {
        let program = same_kind_program(json!({"kind": "and", "items": [
            atom("relative_of", &["candidate", "anchor"]), same("candidate", "anchor", negated),
        ]}));
        let ids: Vec<String> = (0..4).map(|i| format!("p{i}")).collect();
        let mut records = Vec::new();
        for a in 0..4 { for b in 0..4 {
            records.push(tuple("roster", &[&ids[a], &ids[b]]));
            if edges[a*4+b] { records.push(tuple("relative_of", &[&ids[a], &ids[b]])); }
        }}
        let data = json!({"inputs": [], "relations": records});
        records.reverse();
        let permuted = json!({"inputs": [], "relations": records});
        let mut values = Vec::new();
        for mode in ["explain", "fast"] {
            let first = execute(mode, &program, &data, &ids).unwrap();
            prop_assert_eq!(&first, &execute(mode, &program, &data, &ids).unwrap());
            prop_assert_eq!(&first, &execute(mode, &program, &permuted, &ids).unwrap());
            let counts: Vec<i64> = (0..4).map(|a| output(&first, a)).collect();
            for a in 0..4 {
                let expected = (0..4).filter(|b| edges[b*4+a] && ((a != *b) == negated)).count() as i64;
                prop_assert_eq!(counts[a], expected);
            }
            values.push(counts);
        }
        prop_assert_eq!(&values[0], &values[1]);
    }
}

#[test]
fn derived_atom_results_and_traces_are_identical_at_every_suspend_depth() {
    let mut program = same_kind_program(json!({"kind": "and", "items": [
        atom("eligible", &["candidate", "anchor"]), same("candidate", "anchor", true),
    ]}));
    program["relations"].as_array_mut().unwrap().push(derived_relation(
        "eligible", "relative_of", &["Person", "Person"], 0,
        json!({"kind": "comparison", "op": "gte", "left": {"kind": "role_derived", "term": "anchor", "name": "chain0"}, "right": literal(18)})));
    // Each derived atom computes members for another anchor, and its
    // membership predicate reads a chain long enough to suspend repeatedly.
    let rules = program["derived"].as_array_mut().unwrap();
    rules.push(
        json!({"name": "score", "entity": "Person", "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": {"kind": "input", "name": "score"}}),
    );
    for i in 0..20 {
        rules.push(json!({"name": format!("chain{i}"), "entity": "Person", "dtype": "integer", "unit": null,
            "semantics": "scalar", "expr": {"kind": "derived", "name": if i == 19 { "score".to_string() } else { format!("chain{}", i+1) }}}));
    }
    let dataset = json!({"inputs": [input("b", "score", 17), input("c", "score", 19), input("d", "score", 35)],
        "relations": [tuple("roster", &["a", "b"]), tuple("roster", &["a", "c"]), tuple("roster", &["a", "d"]),
            tuple("relative_of", &["b", "a"]), tuple("relative_of", &["c", "a"]), tuple("relative_of", &["d", "a"])]});
    compile_json(&program).expect("deferring derived atom compiles");
    for mode in ["explain", "fast"] {
        let reference = with_suspend_depth(usize::MAX, || run(mode, &program, &dataset, "a"));
        assert_eq!(output(&reference, 0), 2);
        for depth in 1..=24 {
            let deferred = with_suspend_depth(depth, || run(mode, &program, &dataset, "a"));
            assert_eq!(deferred, reference, "{mode} depth {depth}");
        }
    }
}

#[test]
fn long_atom_dependency_chains_preserve_values_traces_and_suspend_depths() {
    // Every relation uses the SAME base source. The only long dependency
    // path is through predicate atoms, so source-chain accounting cannot
    // conceal a missing relation edge or set the runtime nesting budget.
    // One candidate keeps nested membership computation linear.
    let leaf = json!({"kind": "comparison", "op": "gte",
        "left": {"kind": "role_derived", "term": "candidate", "name": "score"},
        "right": literal(18)});
    let direct = same_kind_program(leaf.clone());
    let mut chained = same_kind_program(atom("atom39", &["anchor", "candidate"]));
    for i in 0..40 {
        let predicate = if i == 0 {
            leaf.clone()
        } else {
            atom(&format!("atom{}", i - 1), &["anchor", "candidate"])
        };
        chained["relations"]
            .as_array_mut()
            .unwrap()
            .push(derived_relation(
                &format!("atom{i}"),
                "roster",
                &["Person", "Person"],
                0,
                predicate,
            ));
    }
    let score = json!({"name": "score", "entity": "Person", "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": {"kind": "input", "name": "score"}});
    let mut direct = direct;
    direct["derived"]
        .as_array_mut()
        .unwrap()
        .push(score.clone());
    chained["derived"].as_array_mut().unwrap().push(score);
    let dataset = json!({"inputs": [input("b", "score", 23)],
        "relations": [tuple("roster", &["a", "b"])]});
    compile_json(&chained).expect("an acyclic atom dependency chain compiles");
    // Unbounded suspend depth disables rule deferral only. Relation
    // resolution still nests at most RELATION_NESTING_LIMIT links natively,
    // 8 in debug or 32 in release, then suspends and retries through its
    // driver. This 40-link chain needed 368 KiB at debug depth 8, versus
    // 992 KiB at depth 32 (rustc 1.94.1, aarch64-apple-darwin, 16 KiB
    // measurement resolution). A 512 KiB thread is 1.4 times that need and
    // fails if each link's stack use doubles; the fixed overhead of the rule
    // evaluation does not double with the links. CI's compiler floats, so
    // if a new rustc trips this guard, re-measure before raising it.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn_scoped(scope, || {
                for mode in ["explain", "fast"] {
                    let expected =
                        with_suspend_depth(usize::MAX, || run(mode, &direct, &dataset, "a"));
                    assert_eq!(output(&expected, 0), 1);
                    let reference =
                        with_suspend_depth(usize::MAX, || run(mode, &chained, &dataset, "a"));
                    assert_eq!(
                        reference, expected,
                        "{mode}: nesting atoms changed the trace"
                    );
                    for depth in [1, 2, 3, 4, 5, 6, 7, 16, 32, 64] {
                        let deferred =
                            with_suspend_depth(depth, || run(mode, &chained, &dataset, "a"));
                        assert_eq!(deferred, reference, "{mode} depth {depth}");
                    }
                }
            })
            .expect("spawn bounded-stack regression thread")
            .join()
            .expect("regression thread");
    });
}
