//! The migrate-scan detector must find real hand-written expansions (which
//! lower as nested binary and/or chains), ignore already-migrated sugar, and
//! refuse near-miss shapes.

use axiom_rules_engine::migrate::scan_source;

const HAND_EXPANDED: &str = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (
            status_single
            and not status_married_separate
            and not status_joint
            and not status_head_of_household
          )
          or (
            not status_single
            and status_married_separate
            and not status_joint
            and not status_head_of_household
          )
          or (
            not status_single
            and not status_married_separate
            and status_joint
            and not status_head_of_household
          )
          or (
            not status_single
            and not status_married_separate
            and not status_joint
            and status_head_of_household
          )
"#;

#[test]
fn finds_the_hand_expanded_shape_through_binary_nesting() {
    let hits = scan_source(HAND_EXPANDED).expect("module lowers");
    assert_eq!(hits.len(), 1, "exactly one site: {hits:?}");
    assert_eq!(hits[0].rule, "filing_status_is_valid");
    assert_eq!(hits[0].site, "versions[0].expr");
    assert_eq!(hits[0].arity, 4);
}

#[test]
fn ignores_already_migrated_sugar() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: exactly_one(status_single, status_married_separate, status_joint, status_head_of_household)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert!(hits.is_empty(), "sugar is not a candidate: {hits:?}");
}

#[test]
fn finds_the_minimum_arity_pair() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: one_election
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (takes_standard_deduction and not itemizes)
          or (not takes_standard_deduction and itemizes)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].arity, 2);
}

#[test]
fn refuses_or_of_ands_that_is_not_exactly_one() {
    // Overlapping branches: no negations of the complement set.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: any_route_applies
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (is_elderly and has_income)
          or (is_disabled and has_income)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert!(
        hits.is_empty(),
        "overlapping branches are not a gate: {hits:?}"
    );
}

#[test]
fn refuses_mismatched_branch_and_base_counts() {
    // Three branches over two bases (an at-least pattern, not exactly-one).
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: lopsided
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a and not b)
          or (not a and b)
          or (a and b)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert!(hits.is_empty(), "three branches over two bases: {hits:?}");
}

#[test]
fn finds_the_flat_pairwise_exclusion_idiom() {
    // The Hawaii/Wisconsin shape: disjunction of the statuses, then a NOT
    // for every unordered pair.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a or b or c or d)
          and not (a and b)
          and not (a and c)
          and not (a and d)
          and not (b and c)
          and not (b and d)
          and not (c and d)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].idiom, "pairwise_exclusions");
    assert_eq!(hits[0].arity, 4);
}

#[test]
fn finds_the_factored_triangular_exclusion_idiom() {
    // The North Dakota shape: each status excludes the ones after it.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a or b or c or d)
          and not (a and (b or c or d))
          and not (b and (c or d))
          and not (c and d)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].idiom, "pairwise_exclusions");
    assert_eq!(hits[0].arity, 4);
}

#[test]
fn refuses_incomplete_pair_coverage() {
    // Missing not(c and d): at-most-one does not hold, so this is not a gate.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: leaky
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a or b or c or d)
          and not (a and (b or c or d))
          and not (b and (c or d))
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert!(
        hits.is_empty(),
        "incomplete coverage is not a gate: {hits:?}"
    );
}

#[test]
fn refuses_pairwise_shape_with_extra_conjuncts() {
    // A residency conjunct rides along: the chain is not a pure gate.
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: entangled
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          is_resident
          and (a or b)
          and not (a and b)
"#;
    let hits = scan_source(rulespec).expect("module lowers");
    assert!(hits.is_empty(), "extra conjuncts are not a gate: {hits:?}");
}

mod apply {
    use axiom_rules_engine::migrate::{
        extract_version_formula, gate_rewrite, plan_rewrites, replace_version_formula, scan_source,
    };

    #[test]
    fn plans_gates_and_rewrites_the_hand_expanded_shape() {
        let (plans, manual) = plan_rewrites(super::HAND_EXPANDED).expect("lowers");
        assert!(manual.is_empty());
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!(
            plan.replacement,
            "exactly_one(status_single, status_married_separate, status_joint, status_head_of_household)"
        );
        let old = extract_version_formula(super::HAND_EXPANDED, &plan.rule, 0).expect("extracts");
        let gate = gate_rewrite(&old, &plan.replacement, &plan.bases).expect("gates");
        assert_eq!(gate.assignments, 16);
        assert!(gate.outcomes_match && gate.rescan_clean);
        let rewritten =
            replace_version_formula(super::HAND_EXPANDED, &plan.rule, 0, &plan.replacement)
                .expect("rewrites");
        assert!(rewritten.contains("formula: exactly_one(status_single,"));
        assert!(
            scan_source(&rewritten)
                .expect("rewritten lowers")
                .is_empty()
        );
        let (again, _) = plan_rewrites(&rewritten).expect("rewritten plans");
        assert!(again.is_empty(), "apply is idempotent");
    }

    #[test]
    fn gates_and_rewrites_the_factored_pairwise_shape() {
        let source = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a or b or c or d)
          and not (a and (b or c or d))
          and not (b and (c or d))
          and not (c and d)
"#;
        let (plans, manual) = plan_rewrites(source).expect("lowers");
        assert!(manual.is_empty());
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].replacement, "exactly_one(a, b, c, d)");
        let old = extract_version_formula(source, &plans[0].rule, 0).expect("extracts");
        let gate = gate_rewrite(&old, &plans[0].replacement, &plans[0].bases).expect("gates");
        assert!(gate.outcomes_match && gate.rescan_clean);
        assert_eq!(gate.assignments, 16);
    }

    #[test]
    fn a_wrong_replacement_fails_the_gate() {
        // Deliberately drop a base: the behavioral gate must catch it.
        let (plans, _) = plan_rewrites(super::HAND_EXPANDED).expect("lowers");
        let plan = &plans[0];
        let old = extract_version_formula(super::HAND_EXPANDED, &plan.rule, 0).expect("extracts");
        let sabotaged = "exactly_one(status_single, status_married_separate, status_joint)";
        let gate = gate_rewrite(&old, sabotaged, &plan.bases).expect("gate runs");
        assert!(!gate.outcomes_match, "dropping a base must not pass");
    }

    #[test]
    fn non_fact_bases_are_reported_for_hands() {
        let source = r#"
format: rulespec/v1
rules:
  - name: mixed_gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (monthly_income > 1000 and not is_exempt)
          or (not (monthly_income > 1000) and is_exempt)
"#;
        let (plans, manual) = plan_rewrites(source).expect("lowers");
        assert!(plans.is_empty());
        assert_eq!(manual.len(), 1);
        assert!(manual[0].reason.contains("not a bare fact reference"));
    }
}

#[test]
fn text_surgery_never_anchors_on_a_longer_name_sharing_the_prefix() {
    use axiom_rules_engine::migrate::{extract_version_formula, replace_version_formula};
    // `gate_extra` precedes `gate`; a prefix-matched locator would anchor on
    // the wrong rule. The gate would catch the mismatch loudly, but the
    // locator must be exact in the first place.
    let source = r#"
format: rulespec/v1
rules:
  - name: gate_extra
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: other_fact
  - name: gate
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (a and not b) or (not a and b)
"#;
    let extracted = extract_version_formula(source, "gate", 0).expect("locates the exact rule");
    assert!(extracted.contains("(a and not b)"), "got: {extracted}");
    let rewritten =
        replace_version_formula(source, "gate", 0, "exactly_one(a, b)").expect("rewrites");
    assert!(rewritten.contains("formula: exactly_one(a, b)"));
    assert!(
        rewritten.contains("formula: other_fact"),
        "gate_extra untouched"
    );
}
