//! `exactly_one(...)` is authoring sugar with a precise contract: it lowers
//! to an Or-of-And-of-Nots expansion behaving exactly like the form encoders
//! previously wrote by hand — same outcomes on every Boolean assignment,
//! same short-circuit reads, and the same missing-input faults when a read
//! reaches an absent input. The lowered tree is flat n-ary where
//! hand-chained operators nest as binary pairs, so compiled structure and
//! trace text render the flatter shape while behavior stays identical.

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec, PeriodSpec,
    ScalarValueSpec,
};

const SUGARED_RULESPEC: &str = r#"
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

const EXPANDED_RULESPEC: &str = r#"
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

fn simple_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    }
}

fn judgment_output(output: &OutputValue) -> JudgmentOutcomeSpec {
    match output {
        OutputValue::Judgment { outcome, .. } => *outcome,
        other => panic!("expected judgment output, got {other:?}"),
    }
}

fn status_dataset(period: &PeriodSpec, statuses: [bool; 4]) -> DatasetSpec {
    let names = [
        "status_single",
        "status_married_separate",
        "status_joint",
        "status_head_of_household",
    ];
    DatasetSpec {
        inputs: names
            .into_iter()
            .zip(statuses)
            .map(|(name, value)| InputRecordSpec {
                name: name.to_string(),
                entity: "TaxUnit".to_string(),
                entity_id: "tax-unit-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Bool { value },
            })
            .collect(),
        relations: vec![],
    }
}

fn run(rulespec: &str, statuses: [bool; 4]) -> JudgmentOutcomeSpec {
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: status_dataset(&period, statuses),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period,
            outputs: vec!["filing_status_is_valid".to_string()],
        }],
    })
    .expect("request executes");
    judgment_output(
        response.results[0]
            .outputs
            .get("filing_status_is_valid")
            .expect("judgment output"),
    )
}

#[test]
fn exactly_one_lowers_to_a_first_class_gate() {
    // The serialized surface — what artifacts and graph consumers read —
    // carries one n-input exactly_one node, not the ~n² expansion.
    let program = axiom_rules_engine::rulespec::lower_rulespec_str(SUGARED_RULESPEC)
        .expect("sugared RuleSpec lowers");
    let program = serde_json::to_value(&program).expect("program serialises");
    let expr = program
        .pointer("/derived/0/expr")
        .expect("judgment expression is serialised");

    let holds = |name: &str| {
        serde_json::json!({
            "kind": "comparison",
            "left": {"kind": "input", "name": name},
            "op": "eq",
            "right": {"kind": "literal", "value": {"kind": "bool", "value": true}},
        })
    };
    let names = [
        "status_single",
        "status_married_separate",
        "status_joint",
        "status_head_of_household",
    ];
    let expected = serde_json::json!({
        "kind": "exactly_one",
        "items": names.iter().map(|name| holds(name)).collect::<Vec<_>>(),
    });
    assert_eq!(
        expr, &expected,
        "exactly_one must serialise as one first-class gate with n inputs",
    );
}

#[test]
fn structured_exactly_one_yaml_round_trips_and_matches_the_sugar() {
    // The structured spec surface accepts kind: exactly_one directly, and a
    // structured judgment behaves identically to the formula sugar.
    use axiom_rules_engine::spec::JudgmentExprSpec;
    let yaml = r#"
kind: exactly_one
items:
  - kind: derived
    name: a
  - kind: derived
    name: b
"#;
    let parsed: JudgmentExprSpec =
        serde_yaml::from_str(yaml).expect("structured exactly_one parses");
    let back = serde_json::to_value(&parsed).expect("re-serialises");
    assert_eq!(back["kind"], "exactly_one");
    assert_eq!(back["items"].as_array().map(Vec::len), Some(2));
}

#[test]
fn exactly_one_matches_the_expansion_on_every_input_combination() {
    for bits in 0..16u8 {
        let statuses = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0];
        let sugared = run(SUGARED_RULESPEC, statuses);
        let expanded = run(EXPANDED_RULESPEC, statuses);
        assert_eq!(sugared, expanded, "divergence at inputs {statuses:?}");
        let expected = if statuses.iter().filter(|held| **held).count() == 1 {
            JudgmentOutcomeSpec::Holds
        } else {
            JudgmentOutcomeSpec::NotHolds
        };
        assert_eq!(sugared, expected, "wrong outcome at inputs {statuses:?}");
    }
}

fn partial_status_dataset(period: &PeriodSpec, supplied: &[(&str, bool)]) -> DatasetSpec {
    DatasetSpec {
        inputs: supplied
            .iter()
            .map(|(name, value)| InputRecordSpec {
                name: name.to_string(),
                entity: "TaxUnit".to_string(),
                entity_id: "tax-unit-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Bool { value: *value },
            })
            .collect(),
        relations: vec![],
    }
}

fn run_partial(rulespec: &str, supplied: &[(&str, bool)]) -> Result<JudgmentOutcomeSpec, String> {
    let period = simple_period();
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(rulespec).expect("RuleSpec lowers");
    execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program,
        dataset: partial_status_dataset(&period, supplied),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period,
            outputs: vec!["filing_status_is_valid".to_string()],
        }],
    })
    .map(|response| {
        judgment_output(
            response.results[0]
                .outputs
                .get("filing_status_is_valid")
                .expect("judgment output"),
        )
    })
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn exactly_one_matches_the_expansion_under_missing_inputs() {
    // Missing inputs are not a third truth value at this surface: an input
    // fault fires when a read reaches it, and short-circuiting can resolve a
    // branch before any missing read happens. The sugar's contract is that
    // both behaviors track the manual expansion exactly, because both forms
    // read the same inputs in the same order.

    // The first two statuses both hold: every branch short-circuits on one
    // of them, so the last two statuses are never read and the outcome is
    // determined with half the inputs absent.
    let determined = [("status_single", true), ("status_married_separate", true)];
    let sugared = run_partial(SUGARED_RULESPEC, &determined);
    let expanded = run_partial(EXPANDED_RULESPEC, &determined);
    assert_eq!(sugared, expanded, "divergence with inputs {determined:?}");
    assert_eq!(sugared, Ok(JudgmentOutcomeSpec::NotHolds));

    // Statuses one and three hold: the first branch reads the (absent)
    // second status before anything can short-circuit, so both forms fault
    // on the same missing input.
    let faulting = [("status_single", true), ("status_joint", true)];
    let sugared = run_partial(SUGARED_RULESPEC, &faulting);
    let expanded = run_partial(EXPANDED_RULESPEC, &faulting);
    assert_eq!(sugared, expanded, "divergence with inputs {faulting:?}");
    let error = sugared.expect_err("unreachable inputs must fault");
    assert!(
        error.contains("MissingInput") && error.contains("status_married_separate"),
        "fault must name the first missing read, got: {error}",
    );

    // Nothing supplied: both forms fault on the first read.
    let empty: [(&str, bool); 0] = [];
    let sugared = run_partial(SUGARED_RULESPEC, &empty);
    let expanded = run_partial(EXPANDED_RULESPEC, &empty);
    assert_eq!(sugared, expanded, "divergence with no inputs");
    let error = sugared.expect_err("empty dataset must fault");
    assert!(
        error.contains("MissingInput") && error.contains("status_single"),
        "fault must name the first missing read, got: {error}",
    );
}

const SUGARED_PAIR_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: exactly_one(status_single, status_joint)
"#;

const EXPANDED_PAIR_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: filing_status_is_valid
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: |-
          (status_single and not status_joint)
          or (not status_single and status_joint)
"#;

#[test]
fn exactly_one_pair_is_exclusive_or() {
    // The minimum arity behaves as exclusive-or and matches its expansion.
    for (single, joint) in [(false, false), (false, true), (true, false), (true, true)] {
        let supplied = [("status_single", single), ("status_joint", joint)];
        let sugared = run_partial(SUGARED_PAIR_RULESPEC, &supplied);
        let expanded = run_partial(EXPANDED_PAIR_RULESPEC, &supplied);
        assert_eq!(sugared, expanded, "pair divergence at {supplied:?}");
        let expected = if single ^ joint {
            JudgmentOutcomeSpec::Holds
        } else {
            JudgmentOutcomeSpec::NotHolds
        };
        assert_eq!(sugared, Ok(expected), "pair wrong outcome at {supplied:?}");
    }
}

#[test]
fn exactly_one_accepts_arbitrary_judgment_arguments() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: single_path_applies
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: exactly_one(status_single, monthly_income > 1000, not status_joint)
"#;
    axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect("judgment-shaped arguments lower");
}

#[test]
fn exactly_one_rejects_fewer_than_two_arguments() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: degenerate
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: exactly_one(status_single)
"#;
    let error = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect_err("single-argument exactly_one must be rejected")
        .to_string();
    assert!(
        error.contains("exactly_one takes at least 2 arguments"),
        "error must name the arity contract, got: {error}",
    );
}

#[test]
fn unknown_judgment_functions_are_named_in_the_error() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: bad_call
    kind: derived
    entity: TaxUnit
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: only_one(status_single, status_joint)
"#;
    let error = axiom_rules_engine::rulespec::lower_rulespec_str(rulespec)
        .expect_err("unknown judgment function must be rejected")
        .to_string();
    assert!(
        error.contains("unknown function `only_one` in judgment position"),
        "error must name the unknown function, got: {error}",
    );
}
