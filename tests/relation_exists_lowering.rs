//! Bounded `exists`: formula lowering, scope and named refusals (#214, PR 3).

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use serde_json::{Value, json};

fn rulespec(formula: &str, extra: &str) -> String {
    let formula = serde_json::to_string(formula).unwrap();
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

fn compile(formula: &str, extra: &str) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::from_rulespec_str(&rulespec(formula, extra))
}

fn predicate(artifact: &CompiledProgramArtifact) -> Value {
    serde_json::to_value(&artifact.program).unwrap()["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["name"] == "selected")
        .unwrap()["derivation"]["predicate"]
        .clone()
}

fn refuses(formula: &str, extra: &str, code: &str) {
    let error = compile(formula, extra)
        .expect_err("invalid exists must refuse")
        .to_string();
    assert!(error.contains(code), "{formula}: expected {code}: {error}");
}

#[test]
fn nested_variables_lower_to_innermost_indices_in_reads_atoms_and_comparisons() {
    let formula = "exists(relative_of(anchor, p), exists(relative_of(p, q), relative_of(p, q) and q.age >= p.age and p != candidate))";
    let artifact = compile(formula, "").expect("nested exists compiles");
    let outer = predicate(&artifact);
    assert_eq!(outer["kind"], "exists");
    assert_eq!(outer["terms"], json!(["anchor", {"var": 0}]));
    assert_eq!(outer["variable_slot"], 1);
    let inner = &outer["body"];
    assert_eq!(inner["kind"], "exists");
    // Its enclosing p is Var(0) in the generator, before q is bound.
    assert_eq!(inner["terms"], json!([{"var": 0}, {"var": 0}]));
    let body = &inner["body"];
    // Formula `and` associates left, so the final comparison has one gate
    // around the atom and the value comparison.
    let first = &body["items"][0];
    assert_eq!(first["items"][0]["terms"], json!([{"var": 1}, {"var": 0}]));
    assert_eq!(first["items"][1]["left"]["term"], json!({"var": 0}));
    assert_eq!(first["items"][1]["right"]["term"], json!({"var": 1}));
    assert_eq!(body["items"][1]["left"], json!({"var": 1}));
    assert_eq!(body["items"][1]["right"], "candidate");

    let encoded = serde_json::to_string(&artifact).unwrap();
    assert_eq!(
        serde_json::to_string(&CompiledProgramArtifact::from_json_str(&encoded).unwrap()).unwrap(),
        encoded
    );
}

#[test]
fn variables_are_reserved_before_after_and_beside_the_body() {
    for formula in [
        // First exercise admission itself: disabling the scope guard makes
        // this compile as an ordinary input, rather than merely changing a
        // qualified field's refusal diagnostic.
        "p and exists(relative_of(anchor, p), true)",
        "p.age > 0 and exists(relative_of(anchor, p), true)",
        "exists(relative_of(anchor, p), true) and p.age > 0",
        "exists(relative_of(anchor, p), true) or p",
        "p == anchor and exists(relative_of(anchor, p), true)",
        "exists(relative_of(anchor, p), true) or relative_of(p, candidate)",
        "exists(relative_of(anchor, p), true) or min(p.age) > 0",
        "exists(relative_of(anchor, p), true) or sum(p.age) > 0",
        "exists(relative_of(anchor, p), true) or only(p.age) > 0",
        "(if p.age > 0: 1 else: 0) > 0 and exists(relative_of(anchor, p), true)",
    ] {
        refuses(formula, "", "relation_variable_out_of_scope");
    }
    // A sibling may use the same spelling: neither binding encloses the
    // other, and each use remains inside its own body.
    compile(
        "exists(relative_of(anchor, p), p.age > 0) or exists(relative_of(candidate, p), p.age < 0)",
        "",
    )
    .expect("separate lexical bindings may share a spelling");
}

#[test]
fn generators_require_one_new_lowercase_variable_and_an_atom() {
    for (formula, code) in [
        ("exists()", "relation_exists_arity_mismatch"),
        (
            "exists(relative_of(anchor, p))",
            "relation_exists_arity_mismatch",
        ),
        (
            "exists(relative_of(anchor, p), true, false)",
            "relation_exists_arity_mismatch",
        ),
        (
            "exists(relative_of, true)",
            "relation_exists_generator_not_atom",
        ),
        (
            "exists(missing(anchor, p), true)",
            "relation_exists_generator_not_atom",
        ),
        (
            "exists(relative_of(anchor, candidate), true)",
            "relation_exists_variable_count",
        ),
        (
            "exists(relative_of(anchor, anchor), true)",
            "relation_variable_name_conflict",
        ),
        (
            "exists(relative_of(candidate, candidate), true)",
            "relation_variable_name_conflict",
        ),
        (
            "exists(relative_of(p, q), true)",
            "relation_exists_variable_count",
        ),
        (
            "exists(relative_of(p, p), true)",
            "relation_exists_variable_count",
        ),
        (
            "exists(relative_of(anchor, P), true)",
            "relation_variable_invalid_name",
        ),
        (
            "exists(relative_of(anchor, _p), true)",
            "relation_variable_invalid_name",
        ),
        (
            "exists(relative_of(anchor, pQ), true)",
            "relation_variable_invalid_name",
        ),
        (
            "exists(relative_of(anchor, 3), true)",
            "relation_atom_argument_not_term",
        ),
        (
            "exists(relative_of(p), true)",
            "relation_atom_arity_mismatch",
        ),
        (
            "exists(relative_of(anchor, candidate, p), true)",
            "relation_atom_arity_mismatch",
        ),
        (
            "exists(relative_of(anchor, p), p)",
            "relation_term_used_as_value",
        ),
        (
            "exists(relative_of(anchor, p), p.p)",
            "relation_term_used_as_value",
        ),
        (
            "exists(relative_of(anchor, p), candidate.p)",
            "relation_term_used_as_value",
        ),
        (
            "exists(relative_of(anchor, p), p < candidate)",
            "relation_term_comparison_operator",
        ),
        (
            "exists(relative_of(anchor, p), p == 4)",
            "relation_term_compared_with_value",
        ),
        (
            "exists(relative_of(anchor, p), true) == 1",
            "relation_exists_used_as_value",
        ),
        // A zero-new-variable generator that uses an enclosing variable
        // attempts to rebind it rather than introducing a new variable.
        (
            "exists(relative_of(anchor, p), exists(relative_of(anchor, p), true))",
            "relation_variable_name_conflict",
        ),
    ] {
        refuses(formula, "", code);
    }
    compile(
        "exists(relative_of(anchor, person_2), person_2.age > 0)",
        "",
    )
    .expect("lowercase letters, digits and underscores are allowed");
}

#[test]
fn a_new_variable_cannot_hide_a_visible_rule_parameter_or_relation() {
    for name in ["total", "selected", "relative_of"] {
        refuses(
            &format!("exists(relative_of(anchor, {name}), true)"),
            "",
            "relation_variable_name_conflict",
        );
    }
    let parameter = r#"
  - name: threshold
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "65"
"#;
    refuses(
        "exists(relative_of(anchor, threshold), true)",
        parameter,
        "relation_variable_name_conflict",
    );
}

#[test]
fn exists_is_a_builtin_even_if_a_relation_has_that_name() {
    let extra = r#"
  - name: exists
    kind: data_relation
    data_relation: {arity: 2, arguments: [Person, Person]}
"#;
    compile("exists(relative_of(anchor, p), p.age >= 65)", extra)
        .expect("a relation declaration does not replace the builtin");
    for formula in ["exists(anchor, candidate)", "exists(anchor, candidate) > 0"] {
        refuses(formula, extra, "relation_name_shadows_builtin");
    }
    refuses(
        "exists(exists(anchor, p), true)",
        extra,
        "relation_name_shadows_builtin",
    );
}

#[test]
fn generator_kinds_are_inherited_by_variables_and_checked_in_the_body() {
    let extra = r#"
  - name: residency
    kind: data_relation
    data_relation: {arity: 2, arguments: [Person, Household]}
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: "size"
  - name: homes
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: residency
      slot_entities: [Person, Household]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: "true"
"#;
    compile(
        "exists(residency(anchor, home), home.household_size > 1)",
        extra,
    )
    .expect("data generator variable has its slot's kind");
    compile(
        "exists(homes(anchor, home), home.household_size > 1)",
        extra,
    )
    .expect("derived generator variable has its slot's kind");
    refuses(
        "exists(residency(anchor, home), home == candidate)",
        extra,
        "relation_term_kind_mismatch",
    );
    refuses(
        "exists(relative_of(anchor, p), p.household_size > 1)",
        extra,
        "relation_role_entity_mismatch",
    );
    refuses(
        "exists(residency(anchor, home), relative_of(home, candidate))",
        extra,
        "relation_atom_kind_mismatch",
    );
    refuses(
        "exists(homes(home, candidate), true)",
        extra,
        "inverse_derived_traversal_unsupported",
    );
}

#[test]
fn untyped_generators_and_inverse_derived_traversals_refuse_by_name() {
    let extra = r#"
  - name: untyped
    kind: data_relation
    data_relation: {arity: 2}
  - name: eligible
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
"#;
    refuses(
        "exists(untyped(anchor, p), true)",
        extra,
        "relation_atom_needs_typed_relation",
    );
    refuses(
        "exists(eligible(p, candidate), true)",
        extra,
        "inverse_derived_traversal_unsupported",
    );
    compile("exists(eligible(anchor, p), p.age >= 65)", extra)
        .expect("derived generators traverse forward");
}

#[test]
fn exists_has_no_scope_in_rule_bodies_or_aggregation_clauses_or_values() {
    for (dtype, formula) in [
        ("Judgment", "exists(relative_of(anchor, p), true)"),
        ("Judgment", "not exists(relative_of(anchor, p), true)"),
        ("Integer", "exists(relative_of(anchor, p), true) + 1"),
        (
            "Integer",
            "count_where(relative_of, exists(relative_of(anchor, p), true))",
        ),
        (
            "Integer",
            "sum_where(relative_of, exists(relative_of(anchor, p), true), enabled)",
        ),
    ] {
        let extra = format!(
            "  - name: invalid\n    kind: derived\n    entity: Person\n    dtype: {dtype}\n    versions:\n      - effective_from: 2026-01-01\n        formula: {}\n",
            serde_json::to_string(formula).unwrap()
        );
        refuses("true", &extra, "relation_role_outside_scope");
    }
    for formula in [
        "count_where(relative_of, exists(relative_of(anchor, p), p.age > 0)) > 0",
        "sum_where(relative_of, exists(relative_of(anchor, p), true), enabled) > 0",
        "sum_where(relative_of, age, not exists(relative_of(anchor, p), true)) > 0",
        "exists(relative_of(anchor, p), count_where(relative_of, p.age > 0) > 0)",
    ] {
        refuses(formula, "", "relation_role_outside_scope");
    }
}

#[test]
fn forall_is_not_exists_over_a_negated_body() {
    let artifact = compile("not exists(relative_of(anchor, p), not p.eligible)", "")
        .expect("forall syntax compiles");
    let pred = predicate(&artifact);
    assert_eq!(pred["kind"], "not");
    assert_eq!(pred["item"]["kind"], "exists");
    assert_eq!(pred["item"]["body"]["kind"], "not");
    assert_eq!(
        pred["item"]["body"]["item"]["left"]["term"],
        json!({"var": 0})
    );
}

#[test]
fn nested_exists_traverses_each_expression_level() {
    // Formula/expression size bounds (#240/#245) have not landed on this
    // baseline. Keep a moderate nesting case to cover traversal through
    // every generator and body without inventing a new bound in this PR.
    let mut formula = "p0.age >= p23.age".to_string();
    for depth in (0..24).rev() {
        let anchor = if depth == 0 {
            "anchor".to_string()
        } else {
            format!("p{}", depth - 1)
        };
        formula = format!("exists(relative_of({anchor}, p{depth}), {formula})");
    }
    let artifact = compile(&formula, "").expect("each nested variable stays in scope");
    let mut node = predicate(&artifact);
    for depth in 0..24 {
        assert_eq!(node["kind"], "exists", "depth {depth}");
        node = node["body"].clone();
    }
    assert_eq!(node["left"]["term"], json!({"var": 23}));
    assert_eq!(node["right"]["term"], json!({"var": 0}));
}
