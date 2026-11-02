//! A root-entity rule that reads another entity without a relation has no
//! dense plan. It used to refuse the whole module, which let one rule decide
//! what every other rule could do.
//!
//! A module names every rule its import closure reaches, not only the ones a
//! caller wants, so the refusal reached rules that had nothing to do with the
//! offending one. Compiling 26 USC 32 (EITC) failed on a senior-deduction rule
//! from `26/151` reading a `Person` rule from `26/911/a`; neither is part of
//! EITC. See TheAxiomFoundation/axiom-rules-engine#266.
//!
//! Now the rule is left out, every other rule compiles, and asking for the
//! left-out rule reports why.

use std::collections::HashMap;

use axiom_rules_engine::dense::{DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::Period;
use axiom_rules_engine::spec::{PeriodKindSpec, PeriodSpec, ProgramSpec};
use serde_json::{Value, json};

fn compile(derived: Vec<Value>) -> Result<DenseCompiledProgram, DenseCompileError> {
    let spec: ProgramSpec = serde_json::from_value(json!({ "relations": [], "derived": derived }))
        .expect("a valid ProgramSpec");
    let program = spec.to_program().expect("a program");
    DenseCompiledProgram::from_program(&program, Some("Household"))
}

/// A `Household` rule reading a `Person` rule directly: no relation, so the
/// dense plan cannot place the other entity's rows.
fn crossing_rules() -> Vec<Value> {
    vec![
        rule(
            "person_only_amount",
            "Person",
            json!({"kind": "input", "name": "person_amount"}),
        ),
        rule(
            "crosses_into_person",
            "Household",
            json!({"kind": "derived", "name": "person_only_amount"}),
        ),
    ]
}

fn month() -> Period {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
    .to_model()
    .expect("the period converts")
}

/// One `household_amount` column, the only input the compiled rule reads.
fn batch(amounts: Vec<f64>) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: amounts.len(),
        inputs: HashMap::from([(
            "household_amount".to_string(),
            DenseColumn::Float(amounts),
        )]),
        relations: HashMap::new(),
    }
}

fn rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({
        "name": name,
        "entity": entity,
        "dtype": "decimal",
        "unit": null,
        "semantics": "scalar",
        "expr": expr,
    })
}

fn independent_rule() -> Value {
    rule(
        "household_only_amount",
        "Household",
        json!({"kind": "input", "name": "household_amount"}),
    )
}

#[test]
fn an_unrelated_rule_still_compiles() {
    let mut derived = crossing_rules();
    derived.push(independent_rule());
    let program = compile(derived).expect("the module compiles despite the crossing rule");

    assert_eq!(program.output_names(), vec!["household_only_amount"]);
    assert!(
        program
            .unavailable_outputs()
            .contains_key("crosses_into_person"),
        "the crossing rule is recorded, not silently dropped: {:?}",
        program.unavailable_outputs()
    );
    let reason = &program.unavailable_outputs()["crosses_into_person"];
    assert!(
        reason.contains("crosses into `Person`"),
        "the reason is the compile error: {reason}"
    );
}

#[test]
fn the_unavailable_rule_reports_its_reason_when_requested() {
    let mut derived = crossing_rules();
    derived.push(independent_rule());
    let program = compile(derived).expect("the module compiles");

    let error = program
        .execute_f64(
            &month(),
            batch(vec![1.0]),
            &["crosses_into_person".to_string()],
        )
        .expect_err("a rule with no dense plan cannot be executed");

    match error {
        EvalError::UnavailableDenseOutput { output, reason } => {
            assert_eq!(output, "crosses_into_person");
            assert!(reason.contains("crosses into `Person`"), "{reason}");
        }
        other => panic!("expected UnavailableDenseOutput, got {other:?}"),
    }
}

#[test]
fn a_genuinely_missing_name_is_still_unknown() {
    let program = compile(vec![independent_rule()]).expect("the module compiles");

    let error = program
        .execute_f64(
            &month(),
            batch(vec![1.0]),
            &["no_such_rule".to_string()],
        )
        .expect_err("an absent name is not an output");

    assert!(
        matches!(error, EvalError::UnknownDerived(name) if name == "no_such_rule"),
        "a name the module does not define stays UnknownDerived"
    );
}

#[test]
fn a_module_of_only_crossing_rules_is_still_refused() {
    // Leaving every rule out would hand back a program that can answer nothing,
    // so the first recorded reason is reported instead of an empty success.
    let error = compile(crossing_rules()).expect_err("nothing compiles, so there is no program");
    let message = error.to_string();
    assert!(
        message.contains("no rule of entity `Household` compiles"),
        "{message}"
    );
    assert!(message.contains("crosses into `Person`"), "{message}");
}

#[test]
fn the_independent_rule_still_computes_its_value() {
    let mut derived = crossing_rules();
    derived.push(independent_rule());
    let program = compile(derived).expect("the module compiles");

    let result = program
        .execute_f64(
            &month(),
            batch(vec![3.0, 4.0]),
            &["household_only_amount".to_string()],
        )
        .expect("the rule that does not cross entities runs");

    assert_eq!(result.row_count, 2);
    assert!(result.outputs.contains_key("household_only_amount"));
}
