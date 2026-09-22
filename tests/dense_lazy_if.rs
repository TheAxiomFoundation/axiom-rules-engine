//! Invented conditional histories; no statutory fixtures or native data.
use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::lifetime_api::{
    execute_lifetime_wire_request, parse_lifetime_wire_request,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{DerivedSemanticsSpec, ScalarExprSpec, ScalarValueSpec};
use rust_decimal::Decimal;
use serde_json::json;

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: format!("{y}-01-01").parse().unwrap(),
        end: format!("{y}-12-31").parse().unwrap(),
    }
}

fn artifact(formula: &str) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(&format!(
        "format: rulespec/v1\nrules:\n  - name: result\n    kind: derived\n    entity: Worker\n    dtype: Money\n    versions:\n      - effective_from: '1960-01-01'\n        formula: '{formula}'\n"
    ))
    .unwrap()
}

fn compile(formula: &str) -> DenseCompiledProgram {
    DenseCompiledProgram::from_artifact(&artifact(formula), Some("Worker")).unwrap()
}

fn decimals(values: &[&str]) -> DenseColumn {
    DenseColumn::Decimal(values.iter().map(|v| v.parse().unwrap()).collect())
}

fn batch(row_count: usize, inputs: &[(&str, DenseColumn)]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count,
        inputs: inputs
            .iter()
            .map(|(name, column)| (name.to_string(), column.clone()))
            .collect(),
        relations: HashMap::new(),
    }
}

fn scalar(result: &DenseExecutionResult) -> &DenseColumn {
    match &result.outputs["result"] {
        DenseOutputValue::Scalar(column) => column,
        other => panic!("expected scalar, got {other:?}"),
    }
}

fn assert_decimals(result: &DenseExecutionResult, expected: &[&str]) {
    match scalar(result) {
        DenseColumn::Decimal(values) => {
            assert_eq!(
                *values,
                expected
                    .iter()
                    .map(|v| v.parse::<Decimal>().unwrap())
                    .collect::<Vec<_>>()
            );
        }
        other => panic!("expected exact Decimal, got {other:?}"),
    }
}

fn history() -> Vec<DenseBatchSpec> {
    vec![
        batch(
            2,
            &[
                ("history_year", DenseColumn::Integer(vec![2023, 2023])),
                ("index_year", DenseColumn::Integer(vec![2024, 2024])),
                ("amount", decimals(&["10.2", "2.2"])),
                ("index", decimals(&["6", "6"])),
                ("denominator", decimals(&["4", "4"])),
            ],
        ),
        batch(
            2,
            &[
                ("history_year", DenseColumn::Integer(vec![2025, 2025])),
                ("index_year", DenseColumn::Integer(vec![2024, 2024])),
                ("amount", decimals(&["20.1", "3.1"])),
                ("index", decimals(&["6", "6"])),
                // No denominator is supplied for this unindexed observation.
            ],
        ),
    ]
}

const HISTORY_FORMULA: &str =
    "sum_over_periods(if history_year < index_year: amount * index / denominator else: amount)";

#[test]
fn decimal_history_skips_unindexed_observations_missing_denominator() {
    let plan =
        CalculationLifetimePlan::from_artifact(&artifact(HISTORY_FORMULA), "Worker", year(2026))
            .unwrap();
    let result = plan
        .execute(&[year(2023), year(2025)], history(), &["result".into()])
        .unwrap();
    assert_decimals(&result, &["35.4", "6.4"]);
    let legacy = compile(HISTORY_FORMULA)
        .execute_lifetime(&[year(2023), year(2025)], history(), &["result".into()])
        .unwrap();
    assert_decimals(&legacy, &["35.4", "6.4"]);
}

#[test]
fn missing_denominator_in_needed_observation_names_that_period() {
    let plan =
        CalculationLifetimePlan::from_artifact(&artifact(HISTORY_FORMULA), "Worker", year(2026))
            .unwrap();
    let mut missing = history();
    missing[0].inputs.remove("denominator");
    let error = plan
        .execute(&[year(2023), year(2025)], missing, &["result".into()])
        .unwrap_err();
    assert!(
        matches!(error, DenseCompileError::Eval(EvalError::MissingInput { name, period_start, period_end, .. })
        if name == "denominator" && period_start == year(2023).start && period_end == year(2023).end)
    );
}

#[test]
fn dense_uniform_masks_skip_either_missing_branch_but_mixed_masks_require_both() {
    let program = compile("if flag: amount else: missing");
    for (flags, name) in [
        (vec![true, true], "amount"),
        (vec![false, false], "missing"),
    ] {
        let inputs = batch(
            2,
            &[
                ("flag", DenseColumn::Bool(flags)),
                (name, decimals(&["1.1", "2.2"])),
            ],
        );
        let result = program
            .execute(&year(2025), inputs, &["result".into()])
            .unwrap();
        assert_decimals(&result, &["1.1", "2.2"]);
    }
    for flags in [vec![true, false], vec![false, true]] {
        for (present, absent) in [("amount", "missing"), ("missing", "amount")] {
            let error = program
                .execute(
                    &year(2025),
                    batch(
                        2,
                        &[
                            ("flag", DenseColumn::Bool(flags.clone())),
                            (present, decimals(&["1", "2"])),
                        ],
                    ),
                    &["result".into()],
                )
                .unwrap_err();
            assert!(matches!(error, EvalError::MissingInput { name, .. } if name == absent));
        }
    }
}

#[test]
fn lifetime_outer_uniform_masks_are_lazy_and_mixed_masks_still_require_inputs() {
    let program = compile(
        "if count_over_periods(flag) > 0: sum_over_periods(amount) else: sum_over_periods(missing)",
    );
    for (flags, name) in [
        (vec![true, true], "amount"),
        (vec![false, false], "missing"),
    ] {
        let inputs = batch(
            2,
            &[
                ("flag", DenseColumn::Bool(flags)),
                (name, decimals(&["1.1", "2.2"])),
            ],
        );
        let result = program
            .execute_lifetime(
                &[year(2024), year(2025)],
                vec![inputs.clone(), inputs],
                &["result".into()],
            )
            .unwrap();
        assert_decimals(&result, &["2.2", "4.4"]);
    }
    let inputs = batch(
        2,
        &[
            ("flag", DenseColumn::Bool(vec![true, false])),
            ("amount", decimals(&["1", "2"])),
        ],
    );
    assert!(
        matches!(program.execute_lifetime(&[year(2025)], vec![inputs], &["result".into()]), Err(EvalError::MissingInput { name, .. }) if name == "missing")
    );
}

#[test]
fn uniform_numeric_branch_is_lazy_but_mixed_rows_keep_eager_arithmetic() {
    let program = compile("if flag: amount / denominator else: amount");
    let inputs = batch(
        2,
        &[
            ("flag", DenseColumn::Bool(vec![false, false])),
            ("amount", decimals(&["4", "8"])),
            ("denominator", decimals(&["0", "0"])),
        ],
    );
    assert_decimals(
        &program
            .execute(&year(2025), inputs, &["result".into()])
            .unwrap(),
        &["4", "8"],
    );
    // Row 1 selects the safe else branch, but mixed-column evaluation still
    // evaluates its zero denominator. Per-row numeric masking is not supported.
    let inputs = batch(
        2,
        &[
            ("flag", DenseColumn::Bool(vec![true, false])),
            ("amount", decimals(&["4", "8"])),
            ("denominator", decimals(&["2", "0"])),
        ],
    );
    assert!(matches!(
        program.execute(&year(2025), inputs, &["result".into()]),
        Err(EvalError::DivisionByZero)
    ));
}

#[test]
fn empty_batches_keep_both_branch_validation_and_type_resolution() {
    let program = compile("if flag: amount else: missing");
    let inputs = batch(
        0,
        &[
            ("flag", DenseColumn::Bool(vec![])),
            ("amount", DenseColumn::Integer(vec![])),
            ("missing", decimals(&[])),
        ],
    );
    assert_decimals(
        &program
            .execute(&year(2025), inputs.clone(), &["result".into()])
            .unwrap(),
        &[],
    );
    let mut missing = inputs;
    missing.inputs.remove("missing");
    assert!(
        matches!(program.execute(&year(2025), missing, &["result".into()]), Err(EvalError::MissingInput { name, .. }) if name == "missing")
    );
    let outer = compile(
        "if count_over_periods(flag) > 0: sum_over_periods(amount) else: sum_over_periods(missing)",
    );
    assert!(
        matches!(outer.execute_lifetime(&[year(2025)], vec![batch(0, &[("flag", DenseColumn::Bool(vec![])), ("amount", decimals(&[]))])], &["result".into()]), Err(EvalError::MissingInput { name, .. }) if name == "missing")
    );
}

#[test]
fn missing_condition_and_malformed_present_column_are_still_errors() {
    let program = compile("if flag: amount else: missing");
    assert!(
        matches!(program.execute(&year(2025), batch(1, &[("amount", decimals(&["1"]))]), &["result".into()]), Err(EvalError::MissingInput { name, .. }) if name == "flag")
    );
    let malformed = batch(
        1,
        &[
            ("flag", DenseColumn::Bool(vec![true])),
            ("amount", decimals(&["1"])),
            ("missing", decimals(&[])),
        ],
    );
    assert!(matches!(
        program.execute(&year(2025), malformed, &["result".into()]),
        Err(EvalError::TypeMismatch(_))
    ));
}

#[test]
fn input_or_else_is_per_reference_even_when_strict_reference_is_unselected() {
    let mut program = artifact("if flag: amount else: amount").program;
    let mut semantics = program.derived[0].semantics.clone();
    let DerivedSemanticsSpec::Scalar {
        expr: ScalarExprSpec::If { then_expr, .. },
    } = &mut semantics
    else {
        panic!("conditional fixture")
    };
    *then_expr = Box::new(ScalarExprSpec::InputOrElse {
        name: "amount".into(),
        default: ScalarValueSpec::Decimal {
            value: "2.5".into(),
        },
    });
    program.derived[0].semantics = semantics.clone();
    program.derived[0].versions[0].semantics = semantics;
    let program = DenseCompiledProgram::from_artifact(
        &CompiledProgramArtifact::compile(program).unwrap(),
        Some("Worker"),
    )
    .unwrap();
    let result = program
        .execute(
            &year(2025),
            batch(1, &[("flag", DenseColumn::Bool(vec![true]))]),
            &["result".into()],
        )
        .unwrap();
    assert_decimals(&result, &["2.5"]);
    assert!(
        matches!(program.execute(&year(2025), batch(1, &[("flag", DenseColumn::Bool(vec![false]))]), &["result".into()]), Err(EvalError::MissingInput { name, .. }) if name == "amount")
    );
}

#[test]
fn lifetime_wire_widens_selected_integer_to_declared_decimal_exactly() {
    let mut source =
        artifact("if count_over_periods(flag) > 0: 7 else: sum_over_periods(missing)").program;
    let output = "us:statutes/99/9#result";
    source.derived[0].id = Some(output.into());
    let artifact = CompiledProgramArtifact::compile(source).unwrap();
    let request = json!({
        "schema":"axiom-rules-engine/lifetime-request/v2", "entity":"Worker",
        "periods":[{"period_kind":"tax_year","start":"2025-01-01","end":"2025-12-31"}],
        "calculation_period":{"period_kind":"tax_year","start":"2026-01-01","end":"2026-12-31"},
        "output_period":{"period_kind":"tax_year","start":"2026-01-01","end":"2026-12-31"},
        "outputs":[output],
        "batches":[{"row_count":1,"entity_ids":["toy-worker"],"inputs":{
            "us:statutes/99/9#input.flag":{"kind":"bool","values":[true]}
        }}]
    });
    let result = execute_lifetime_wire_request(
        artifact,
        parse_lifetime_wire_request(&request.to_string()).unwrap(),
    )
    .unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(
        result["outputs"][output]["column"],
        json!({"kind":"decimal","values":["7"]})
    );
}
