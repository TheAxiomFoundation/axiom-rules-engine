//! Invented conditional histories; no statutory fixtures or native data.
use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseExecutionResult, DenseOutputValue, DenseRelationBatchSpec,
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
fn uniform_and_mixed_numeric_branches_skip_unselected_arithmetic() {
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
    // Row 1 selects the safe else branch and must not divide by zero.
    let mut inputs = batch(
        2,
        &[
            ("flag", DenseColumn::Bool(vec![true, false])),
            ("amount", decimals(&["4", "8"])),
            ("denominator", decimals(&["2", "0"])),
        ],
    );
    assert_decimals(
        &program
            .execute(&year(2025), inputs.clone(), &["result".into()])
            .unwrap(),
        &["2", "8"],
    );
    inputs
        .inputs
        .insert("flag".into(), DenseColumn::Bool(vec![true, true]));
    assert!(matches!(
        program.execute(&year(2025), inputs, &["result".into()]),
        Err(EvalError::DivisionByZero)
    ));
}

#[test]
fn nested_mixed_branches_match_singletons_and_permuted_rows() {
    let program =
        compile("if first: (if second: amount / denominator else: amount) else: amount / other");
    let rows = [
        (true, true, "4", "2", "0"),
        (true, false, "8", "0", "0"),
        (false, true, "9", "0", "3"),
    ];
    for order in [[0, 1, 2], [2, 0, 1]] {
        let make = |indices: &[usize]| {
            batch(
                indices.len(),
                &[
                    (
                        "first",
                        DenseColumn::Bool(indices.iter().map(|&i| rows[i].0).collect()),
                    ),
                    (
                        "second",
                        DenseColumn::Bool(indices.iter().map(|&i| rows[i].1).collect()),
                    ),
                    (
                        "amount",
                        decimals(&indices.iter().map(|&i| rows[i].2).collect::<Vec<_>>()),
                    ),
                    (
                        "denominator",
                        decimals(&indices.iter().map(|&i| rows[i].3).collect::<Vec<_>>()),
                    ),
                    (
                        "other",
                        decimals(&indices.iter().map(|&i| rows[i].4).collect::<Vec<_>>()),
                    ),
                ],
            )
        };
        let result = program
            .execute(&year(2025), make(&order), &["result".into()])
            .unwrap();
        let DenseColumn::Decimal(values) = scalar(&result) else {
            panic!("Decimal expected")
        };
        for (row, &index) in order.iter().enumerate() {
            let single = program
                .execute(&year(2025), make(&[index]), &["result".into()])
                .unwrap();
            let DenseColumn::Decimal(single) = scalar(&single) else {
                panic!("Decimal expected")
            };
            assert_eq!(values[row], single[0]);
        }
        let fast = program
            .execute_f64(&year(2025), make(&order), &["result".into()])
            .unwrap();
        let DenseColumn::Float(fast) = scalar(&fast) else {
            panic!("Float expected")
        };
        assert_eq!(*fast, order.map(|i| [2.0, 8.0, 3.0][i]).to_vec());
    }
}

fn cached_branch_artifact() -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: quotient
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: amount / denominator
  - name: guarded
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: 'if flag: quotient else: amount'
  - name: result
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: sum_over_periods(guarded)
  - name: outer
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: 'if flag: sum_over_periods(quotient) else: sum_over_periods(amount)'
  - name: unguarded
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: sum_over_periods(quotient)
"#,
    )
    .unwrap()
}

#[test]
fn lifetime_mixed_branches_preserve_whole_histories_and_isolate_caches() {
    let source = cached_branch_artifact();
    let program = DenseCompiledProgram::from_artifact(&source, Some("Worker")).unwrap();
    let make = |flags: Vec<bool>, amounts: &[&str]| {
        batch(
            2,
            &[
                ("flag", DenseColumn::Bool(flags)),
                ("amount", decimals(amounts)),
                ("denominator", decimals(&["2", "0"])),
            ],
        )
    };
    let first = make(vec![true, false], &["4", "8"]);
    let second = make(vec![true, false], &["6", "10"]);
    let periods = [year(2024), year(2025)];
    for plan_result in [
        program
            .execute_lifetime(
                &periods,
                vec![first.clone(), second.clone()],
                &["result".into(), "outer".into()],
            )
            .unwrap(),
        CalculationLifetimePlan::from_artifact(&source, "Worker", year(2026))
            .unwrap()
            .execute(
                &periods,
                vec![first.clone(), second.clone()],
                &["result".into(), "outer".into()],
            )
            .unwrap(),
    ] {
        assert_decimals(&plan_result, &["5", "18"]);
        let DenseOutputValue::Scalar(DenseColumn::Decimal(values)) = &plan_result.outputs["outer"]
        else {
            panic!("Decimal expected")
        };
        assert_eq!(*values, vec![Decimal::from(5), Decimal::from(18)]);
    }
    // A cached partial quotient must never satisfy a later full-column read.
    assert!(matches!(
        program.execute(
            &year(2025),
            first.clone(),
            &["guarded".into(), "quotient".into()]
        ),
        Err(EvalError::DivisionByZero)
    ));
    for first_output in ["result", "outer"] {
        assert!(matches!(
            program.execute_lifetime(
                &periods,
                vec![first.clone(), second.clone()],
                &[first_output.into(), "unguarded".into()]
            ),
            Err(EvalError::DivisionByZero)
        ));
    }
    // Per-observation masks may change; each period still contributes one value per person.
    let third = batch(
        2,
        &[
            ("flag", DenseColumn::Bool(vec![false, true])),
            ("amount", decimals(&["6", "10"])),
            ("denominator", decimals(&["0", "2"])),
        ],
    );
    assert_decimals(
        &program
            .execute_lifetime(&periods, vec![first, third], &["result".into()])
            .unwrap(),
        &["8", "13"],
    );
}

#[test]
fn mixed_branch_dtype_merge_preserves_exact_integer_and_numeric_promotion() {
    let program = compile("if flag: left else: right");
    let result = program
        .execute(
            &year(2025),
            batch(
                2,
                &[
                    ("flag", DenseColumn::Bool(vec![true, false])),
                    ("left", DenseColumn::Integer(vec![9_007_199_254_740_993, 0])),
                    ("right", decimals(&["0", "0.2"])),
                ],
            ),
            &["result".into()],
        )
        .unwrap();
    assert_decimals(&result, &["9007199254740993", "0.2"]);
    for (left, right, expected) in [
        (
            DenseColumn::Integer(vec![1, 2]),
            DenseColumn::Integer(vec![3, 4]),
            "Integer([1, 4])",
        ),
        (
            DenseColumn::Bool(vec![true, false]),
            DenseColumn::Bool(vec![true, false]),
            "Bool([true, false])",
        ),
        (
            DenseColumn::Text(vec!["a".into(), "b".into()]),
            DenseColumn::Text(vec!["c".into(), "d".into()]),
            "Text([\"a\", \"d\"])",
        ),
        (
            DenseColumn::Date(vec![year(2024).start, year(2024).end]),
            DenseColumn::Date(vec![year(2025).start, year(2025).end]),
            "Date([2024-01-01, 2025-12-31])",
        ),
    ] {
        let result = program
            .execute(
                &year(2025),
                batch(
                    2,
                    &[
                        ("flag", DenseColumn::Bool(vec![true, false])),
                        ("left", left),
                        ("right", right),
                    ],
                ),
                &["result".into()],
            )
            .unwrap();
        assert_eq!(format!("{:?}", scalar(&result)), expected);
    }
}

#[test]
fn mixed_lifetime_count_branches_keep_invariant_context() {
    let source = r#"
format: rulespec/v1
rules:
  - name: adjustment
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2024-01-01'
        formula: '1'
      - effective_from: '2025-01-01'
        formula: '2'
      - effective_from: '2026-01-01'
        formula: '1'
  - name: n
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2024-01-01'
        formula: 'if group: count_over_periods(flag) + adjustment else: count_over_periods(flag)'
  - name: result
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2024-01-01'
        formula: sum_top_n_over_periods(amount, n)
"#;
    let source = CompiledProgramArtifact::from_rulespec_str(source).unwrap();
    let program = DenseCompiledProgram::from_artifact(&source, Some("Worker")).unwrap();
    let inputs = batch(
        2,
        &[
            ("group", DenseColumn::Bool(vec![true, false])),
            ("flag", DenseColumn::Bool(vec![false, true])),
            ("amount", decimals(&["4", "8"])),
        ],
    );
    let periods = [year(2024), year(2025)];
    for outputs in [vec!["result".into()], vec!["n".into(), "result".into()]] {
        assert!(matches!(
            program.execute_lifetime(&periods, vec![inputs.clone(), inputs.clone()], &outputs),
            Err(EvalError::OverPeriodsTopNPeriodVarying { .. })
        ));
    }
    // The v2 fixed calculation law makes adjustment invariant, retaining each person's N.
    assert_decimals(
        &CalculationLifetimePlan::from_artifact(&source, "Worker", year(2026))
            .unwrap()
            .execute(&periods, vec![inputs.clone(), inputs], &["result".into()])
            .unwrap(),
        &["4", "16"],
    );
}

#[test]
fn lifetime_outer_branch_skips_unselected_invalid_reduced_counts() {
    let program = compile(
        "if group: sum_top_n_over_periods(amount, count_over_periods(flag)) else: sum_over_periods(amount)",
    );
    let make = |flags| {
        batch(
            2,
            &[
                ("group", DenseColumn::Bool(vec![true, false])),
                ("flag", DenseColumn::Bool(flags)),
                ("amount", decimals(&["4", "8"])),
            ],
        )
    };
    let first = make(vec![true, false]);
    let second = make(vec![false, false]);
    assert_decimals(
        &program
            .execute_lifetime(
                &[year(2024), year(2025)],
                vec![first, second],
                &["result".into()],
            )
            .unwrap(),
        &["4", "16"],
    );
}

#[test]
fn lifetime_wire_promotes_mixed_integer_and_decimal_branches_exactly() {
    let mut source = artifact(
        "if count_over_periods(flag) > 0: 9007199254740993 else: sum_over_periods(amount)",
    )
    .program;
    let output = "us:statutes/99/9#result";
    source.derived[0].id = Some(output.into());
    let artifact = CompiledProgramArtifact::compile(source).unwrap();
    let request = json!({
        "schema":"axiom-rules-engine/lifetime-request/v2", "entity":"Worker",
        "periods":[{"period_kind":"tax_year","start":"2025-01-01","end":"2025-12-31"}],
        "calculation_period":{"period_kind":"tax_year","start":"2026-01-01","end":"2026-12-31"},
        "output_period":{"period_kind":"tax_year","start":"2026-01-01","end":"2026-12-31"},
        "outputs":[output],
        "batches":[{"row_count":2,"entity_ids":["toy-a", "toy-b"],"inputs":{
            "us:statutes/99/9#input.flag":{"kind":"bool","values":[true,false]},
            "us:statutes/99/9#input.amount":{"kind":"decimal","values":["0", "0.2"]}
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
        json!({"kind":"decimal","values":["9007199254740993","0.2"]})
    );
}

#[test]
fn mixed_branches_select_related_parent_ranges_and_root_projections() {
    let source = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: members
    kind: data_relation
    data_relation:
      arity: 2
      slot_entities: [Child, Family]
  - name: parent_bonus
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: bonus
  - name: child_value
    kind: derived
    entity: Child
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: child_amount / denominator + parent_bonus
  - name: result
    kind: derived
    entity: Family
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: 'if flag: sum(members.child_value) else: fallback'
"#,
    )
    .unwrap();
    let program = DenseCompiledProgram::from_artifact(&source, Some("Family")).unwrap();
    let mut inputs = batch(
        4,
        &[
            ("flag", DenseColumn::Bool(vec![true, false, true, true])),
            ("bonus", decimals(&["10", "20", "30", "100"])),
            ("fallback", decimals(&["0", "5", "0", "0"])),
        ],
    );
    for relation in program.relations() {
        inputs.relations.insert(
            relation.key.clone(),
            DenseRelationBatchSpec {
                offsets: vec![0, 2, 3, 3, 4],
                inputs: HashMap::from([
                    ("child_amount".into(), decimals(&["6", "8", "9", "12"])),
                    ("denominator".into(), decimals(&["2", "4", "0", "3"])),
                ]),
            },
        );
    }
    assert_decimals(
        &program
            .execute(&year(2025), inputs.clone(), &["result".into()])
            .unwrap(),
        &["25", "5", "0", "104"],
    );
    inputs.inputs.insert(
        "flag".into(),
        DenseColumn::Bool(vec![true, true, false, true]),
    );
    assert!(matches!(
        program.execute(&year(2025), inputs, &["result".into()]),
        Err(EvalError::DivisionByZero)
    ));
}

#[test]
fn mixed_branches_keep_filtered_relation_chains_aligned() {
    let source = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: membership
    kind: data_relation
    data_relation: {arity: 2}
  - name: eligible
    kind: derived
    entity: Child
    dtype: Judgment
    versions:
      - effective_from: '2000-01-01'
        formula: allowed
  - name: adult
    kind: derived
    entity: Child
    dtype: Judgment
    versions:
      - effective_from: '2000-01-01'
        formula: age >= 18
  - name: eligible_group
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: membership
      entity: EligibleGroup
      member_relation: eligible_members
      slot_entities: [Child, Family]
    versions:
      - effective_from: '2000-01-01'
        formula: eligible
  - name: adult_group
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: eligible_group
      entity: AdultGroup
      member_relation: adult_members
      slot_entities: [Child, Family]
    versions:
      - effective_from: '2000-01-01'
        formula: adult
  - name: result
    kind: derived
    entity: AdultGroup
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: 'if flag: len(adult_members) else: fallback'
"#,
    )
    .unwrap();
    let program = DenseCompiledProgram::from_artifact(&source, Some("AdultGroup")).unwrap();
    let mut inputs = batch(
        4,
        &[
            ("flag", DenseColumn::Bool(vec![true, false, true, true])),
            ("fallback", decimals(&["0", "7", "0", "0"])),
        ],
    );
    for relation in program.relations() {
        inputs.relations.insert(
            relation.key.clone(),
            DenseRelationBatchSpec {
                offsets: vec![0, 3, 4, 4, 6],
                inputs: HashMap::from([
                    (
                        "allowed".into(),
                        DenseColumn::Bool(vec![true, false, true, true, true, false]),
                    ),
                    (
                        "age".into(),
                        DenseColumn::Integer(vec![30, 40, 12, 50, 21, 25]),
                    ),
                ]),
            },
        );
    }
    assert_decimals(
        &program
            .execute(&year(2025), inputs, &["result".into()])
            .unwrap(),
        &["1", "7", "0", "1"],
    );
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
