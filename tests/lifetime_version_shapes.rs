//! Lifetime uses the same dense version-shape restrictions and the same
//! commencement checks for rules inlined on a related-entity axis.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};

fn year(year: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
    }
}

fn compile(module: &str, entity: &str) -> Result<DenseCompiledProgram, DenseCompileError> {
    let artifact = CompiledProgramArtifact::from_rulespec_str(module).unwrap();
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
}

fn versions(formula: &str, shape: &str) -> String {
    let first = format!(
        "    versions:\n      - effective_from: '2026-01-01'\n        formula: {formula}\n"
    );
    match shape {
        "open" => first,
        "bounded" => format!("{first}        effective_to: '2026-12-31'\n"),
        "multiple" => {
            format!("{first}      - effective_from: '2027-01-01'\n        formula: {formula}\n")
        }
        other => panic!("unknown version shape: {other}"),
    }
}

fn root_module(shape: &str) -> String {
    format!(
        "format: rulespec/v1\nrules:\n  - name: dated\n    kind: derived\n    entity: Worker\n    dtype: Integer\n{}",
        versions("sum_over_periods(1)", shape)
    )
}

fn related_module(shape: &str, judgment: bool) -> String {
    let (dtype, formula, aggregate) = if judgment {
        (
            "Judgment",
            "amount > 0",
            "count_where(member_of_household, dated)",
        )
    } else {
        ("Integer", "amount", "sum(member_of_household.dated)")
    };
    format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n  - name: dated\n    kind: derived\n    entity: Person\n    dtype: {dtype}\n{}  - name: annual\n    kind: derived\n    entity: Household\n    dtype: Integer\n    versions:\n      - effective_from: '2000-01-01'\n        formula: {aggregate}\n  - name: lifetime\n    kind: derived\n    entity: Household\n    dtype: Integer\n    versions:\n      - effective_from: '2000-01-01'\n        formula: sum_over_periods(annual)\n",
        versions(formula, shape)
    )
}

fn related_batch(has_member: bool) -> DenseBatchSpec {
    let amount = if has_member { vec![7] } else { vec![] };
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, amount.len()],
                inputs: HashMap::from([("amount".to_string(), DenseColumn::Integer(amount))]),
            },
        )]),
    }
}

fn assert_missing(error: EvalError, period: &Period) {
    assert!(
        matches!(
            error,
            EvalError::MissingDerivedFormulaVersion { ref derived, at }
                if derived == "dated" && at == period.start
        ),
        "unexpected error: {error:?}"
    );
}

/// These refusals predate #260: neither execution entry point can be called
/// until the shared dense compiler accepts the program. In particular,
/// lifetime does not silently select or ignore a bounded or later version.
#[test]
fn lifetime_and_per_period_share_bounded_and_multiple_version_refusals() {
    for shape in ["bounded", "multiple"] {
        let error = compile(&root_module(shape), "Worker").unwrap_err();
        assert_eq!(
            error.to_string(),
            "dense compilation does not yet support versioned derived formulas in `dated`; use generic API execution",
            "root rule with {shape} version shape"
        );
        for judgment in [false, true] {
            let error = compile(&related_module(shape, judgment), "Household").unwrap_err();
            assert_eq!(
                error.to_string(),
                "dense compilation does not yet support versioned inlined derived formulas in `dated`; use generic API execution",
                "inlined rule with {shape} version shape (judgment={judgment})"
            );
        }
    }
}

fn assert_inlined_commencement(judgment: bool) {
    let program = compile(&related_module("open", judgment), "Household").unwrap();
    let early = year(2025);
    let on_time = year(2026);
    let annual = ["annual".to_string()];
    let lifetime = ["lifetime".to_string()];

    // The reduction reaches the same inlined Person rule that annual
    // dense execution reaches, so both numeric entry points must refuse.
    assert_missing(
        program
            .execute(&early, related_batch(true), &annual)
            .unwrap_err(),
        &early,
    );
    assert_missing(
        program
            .execute_lifetime(
                std::slice::from_ref(&early),
                vec![related_batch(true)],
                &lifetime,
            )
            .unwrap_err(),
        &early,
    );
    assert_missing(
        program
            .execute_lifetime_f64(
                std::slice::from_ref(&early),
                vec![related_batch(true)],
                &lifetime,
            )
            .unwrap_err(),
        &early,
    );

    // An empty related-row axis reaches no dated rule, even though the
    // household reaches its aggregate. Dates are checked lazily there too.
    program
        .execute_lifetime(
            std::slice::from_ref(&early),
            vec![related_batch(false)],
            &lifetime,
        )
        .unwrap();
    program
        .execute_lifetime_f64(
            std::slice::from_ref(&early),
            vec![related_batch(false)],
            &lifetime,
        )
        .unwrap();
    program
        .execute_lifetime(
            std::slice::from_ref(&on_time),
            vec![related_batch(true)],
            &lifetime,
        )
        .unwrap();
    program
        .execute_lifetime_f64(
            std::slice::from_ref(&on_time),
            vec![related_batch(true)],
            &lifetime,
        )
        .unwrap();
}

#[test]
fn lifetime_checks_inlined_scalar_only_for_reached_related_rows() {
    assert_inlined_commencement(false);
}

#[test]
fn lifetime_checks_inlined_judgment_only_for_reached_related_rows() {
    assert_inlined_commencement(true);
}
