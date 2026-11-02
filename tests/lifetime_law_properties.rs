//! Properties of calculation-period law selection and nested top-N counts.
//!
//! * **Law in force at the calculation period.** For generated version
//!   histories of a formula and a parameter, a calculation plan evaluates
//!   every observation under the versions in force at the calculation
//!   period's start: the latest version that has started and not ended. When
//!   none is in force it refuses, naming the rule or parameter. The result
//!   does not depend on the observation dates, and the plan reports the
//!   versions it selected.
//! * **A nested count is a flat count.** `sum_top_n_over_periods(x,
//!   count_over_periods(q))`, the same count through a derived rule, and the
//!   count supplied as a period-invariant input all give the same result per
//!   row, through lifetime execution and a calculation plan. A count outside
//!   `1..=periods` is refused the same way by all three.
//!
//! The fixtures are invented. Seeds are fixed, so CI is deterministic.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseExecutionResult, DenseOutputValue, SelectedVersion,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use proptest::prelude::*;
use rust_decimal::Decimal;

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: format!("{y}-01-01").parse().unwrap(),
        end: format!("{y}-12-31").parse().unwrap(),
    }
}

fn decimal_output(result: &DenseExecutionResult, name: &str, row: usize) -> Decimal {
    match &result.outputs[name] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values[row],
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => Decimal::from(values[row]),
        other => panic!("{name} is numeric: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Law in force at the calculation period
// ---------------------------------------------------------------------------

/// One version: its start year, whether it ends the year before the next
/// version starts (a gap with no law in force), and its value.
#[derive(Clone, Debug)]
struct Version {
    from: i32,
    gap_after: bool,
    value: i64,
}

fn history(values: std::ops::RangeInclusive<i64>) -> impl Strategy<Value = Vec<Version>> {
    proptest::collection::btree_set(1995..=2030_i32, 1..=4).prop_flat_map(move |starts| {
        let starts = starts.into_iter().collect::<Vec<_>>();
        let count = starts.len();
        (
            Just(starts),
            proptest::collection::vec(any::<bool>(), count),
            proptest::collection::vec(values.clone(), count),
        )
            .prop_map(|(starts, gaps, values)| {
                starts
                    .into_iter()
                    .zip(gaps)
                    .zip(values)
                    .map(|((from, gap_after), value)| Version {
                        from,
                        gap_after,
                        value,
                    })
                    .collect()
            })
    })
}

/// `effective_to` for version `index`: the day before the next start, or two
/// years earlier when that version leaves a gap. The last version is open.
fn ends(versions: &[Version], index: usize) -> Option<i32> {
    let next = versions.get(index + 1)?;
    let end = if versions[index].gap_after {
        next.from - 2
    } else {
        next.from - 1
    };
    Some(end.max(versions[index].from))
}

/// Each version's body: a formula line for a rule, a one-cell table for a
/// parameter.
fn versions_yaml(versions: &[Version], body: impl Fn(&Version) -> String) -> String {
    let mut yaml = String::new();
    for (index, version) in versions.iter().enumerate() {
        yaml.push_str(&format!(
            "      - effective_from: '{}-01-01'\n",
            version.from
        ));
        if let Some(end) = ends(versions, index) {
            yaml.push_str(&format!("        effective_to: '{end}-12-31'\n"));
        }
        yaml.push_str(&body(version));
    }
    yaml
}

/// The version in force on 1 January of `y`: started, not ended, latest start.
fn in_force(versions: &[Version], y: i32) -> Option<(usize, &Version)> {
    versions
        .iter()
        .enumerate()
        .filter(|(index, version)| {
            version.from <= y && ends(versions, *index).is_none_or(|end| end >= y)
        })
        .max_by_key(|(_, version)| version.from)
}

fn law_artifact(formulas: &[Version], factors: &[Version]) -> CompiledProgramArtifact {
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: factor\n    kind: parameter\n    dtype: Integer\n    indexed_by: k\n    versions:\n{}  - name: total\n    kind: derived\n    entity: Worker\n    dtype: Money\n    versions:\n{}",
        versions_yaml(factors, |version| format!(
            "        values:\n          1: {}\n",
            version.value
        )),
        versions_yaml(formulas, |version| format!(
            "        formula: '{} * sum_over_periods(amount * factor[k])'\n",
            version.value
        )),
    );
    CompiledProgramArtifact::from_rulespec_str(&source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"))
}

fn amounts_batches(observations: &[i32], amounts: &[Vec<i64>]) -> Vec<DenseBatchSpec> {
    observations
        .iter()
        .enumerate()
        .map(|(period, _)| DenseBatchSpec {
            row_count: amounts.len(),
            inputs: HashMap::from([
                (
                    "amount".into(),
                    DenseColumn::Decimal(
                        amounts
                            .iter()
                            .map(|row| Decimal::from(row[period]))
                            .collect(),
                    ),
                ),
                ("k".into(), DenseColumn::Integer(vec![1; amounts.len()])),
            ]),
            relations: HashMap::new(),
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn calculation_plans_select_the_law_in_force_at_the_calculation_period(
        formulas in history(1..=5),
        factors in history(-3..=7),
        calculation in 1993..=2034_i32,
        amounts in proptest::collection::vec(proptest::collection::vec(-50..=50_i64, 3), 1..=3),
        shift in 0..=5_i32,
    ) {
        let artifact = law_artifact(&formulas, &factors);
        let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(calculation));
        let formula = in_force(&formulas, calculation);
        let factor = in_force(&factors, calculation);
        let (Some((formula_index, formula)), Some((factor_index, factor))) = (formula, factor) else {
            let error = plan.expect_err("no law in force is refused");
            match (formula.is_none(), &error) {
                (true, DenseCompileError::Eval(EvalError::MissingDerivedFormulaVersion { derived, at })) => {
                    prop_assert_eq!(derived, "total");
                    prop_assert_eq!(*at, year(calculation).start);
                }
                (false, DenseCompileError::MissingParameterVersion { parameter, at }) => {
                    prop_assert_eq!(parameter, "factor");
                    prop_assert_eq!(*at, year(calculation).start);
                }
                (_, error) => prop_assert!(false, "unexpected refusal {error}"),
            }
            return Ok(());
        };
        let plan = plan.expect("law in force compiles");
        let selected_formula = plan.selected_versions().iter().any(|selected| matches!(
            selected,
            SelectedVersion::Derived { name, version_index, .. }
                if name == "total" && *version_index == formula_index
        ));
        let selected_factor = plan.selected_versions().iter().any(|selected| matches!(
            selected,
            SelectedVersion::Parameter { name, version_index, .. }
                if name == "factor" && *version_index == factor_index
        ));
        prop_assert!(selected_formula, "formula version {} reported", formula_index);
        prop_assert!(selected_factor, "factor version {} reported", factor_index);

        // Completed history only: observations end before the calculation
        // starts. Two different sets of dates must give the same result.
        let early = [1980, 1981, 1982];
        let late = [calculation - 3 - shift, calculation - 2 - shift, calculation - 1 - shift];
        let mut results = Vec::new();
        for observations in [early, late] {
            let periods = observations.map(year);
            let result = plan
                .execute(&periods, amounts_batches(&observations, &amounts), &["total".into()])
                .expect("the plan executes");
            results.push(result);
        }
        for (row, amounts) in amounts.iter().enumerate() {
            let expected = Decimal::from(formula.value)
                * amounts.iter().map(|amount| Decimal::from(amount * factor.value)).sum::<Decimal>();
            for result in &results {
                prop_assert_eq!(decimal_output(result, "total", row), expected);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A nested count is a flat count
// ---------------------------------------------------------------------------

const COUNT_SOURCE: &str = r#"
format: rulespec/v1
rules:
  - name: qualifying
    kind: derived
    entity: Worker
    dtype: Integer
    versions:
      - effective_from: '1990-01-01'
        formula: count_over_periods(q)
  - name: nested
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '1990-01-01'
        formula: sum_top_n_over_periods(x, count_over_periods(q))
  - name: through_rule
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '1990-01-01'
        formula: sum_top_n_over_periods(x, qualifying)
  - name: flat
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '1990-01-01'
        formula: sum_top_n_over_periods(x, n)
"#;

#[derive(Debug, PartialEq)]
enum TopN {
    Value(Decimal),
    OutOfRange,
}

fn top_n(result: Result<DenseExecutionResult, String>, name: &str) -> TopN {
    match result {
        Ok(result) => TopN::Value(decimal_output(&result, name, 0)),
        Err(error) if error.contains("sum_top_n_over_periods") => TopN::OutOfRange,
        Err(error) => panic!("{name}: unexpected refusal {error}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn nesting_a_count_changes_nothing_a_flat_count_gives(
        rows in proptest::collection::vec(
            proptest::collection::vec((-40..=40_i64, -1..=2_i64), 1..=5),
            1..=3,
        ).prop_filter("one period count for every row", |rows| {
            rows.iter().all(|row| row.len() == rows[0].len())
        }),
    ) {
        let artifact = CompiledProgramArtifact::from_rulespec_str(COUNT_SOURCE).unwrap();
        let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).unwrap();
        let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2020)).unwrap();
        let period_count = rows[0].len();
        let periods = (0..period_count).map(|p| year(2000 + p as i32)).collect::<Vec<_>>();
        for row in &rows {
            let count = row.iter().filter(|(_, q)| *q != 0).count() as i64;
            let batches = row
                .iter()
                .map(|(x, q)| DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::from([
                        ("x".into(), DenseColumn::Decimal(vec![Decimal::from(*x)])),
                        ("q".into(), DenseColumn::Integer(vec![*q])),
                        ("n".into(), DenseColumn::Integer(vec![count])),
                    ]),
                    relations: HashMap::new(),
                })
                .collect::<Vec<_>>();
            let mut sorted = row.iter().map(|(x, _)| Decimal::from(*x)).collect::<Vec<_>>();
            sorted.sort_by(|a, b| b.cmp(a));
            let expected = if (1..=period_count as i64).contains(&count) {
                TopN::Value(sorted[..count as usize].iter().sum())
            } else {
                TopN::OutOfRange
            };
            let counted = dense
                .execute_lifetime(&periods, batches.clone(), &["qualifying".into()])
                .unwrap();
            prop_assert_eq!(decimal_output(&counted, "qualifying", 0), Decimal::from(count));
            for name in ["nested", "through_rule", "flat"] {
                let lifetime = dense
                    .execute_lifetime(&periods, batches.clone(), &[name.into()])
                    .map_err(|error| error.to_string());
                prop_assert_eq!(top_n(lifetime, name), expected_clone(&expected), "lifetime {}", name);
                let calculation = plan
                    .execute(&periods, batches.clone(), &[name.into()])
                    .map_err(|error| error.to_string());
                prop_assert_eq!(top_n(calculation, name), expected_clone(&expected), "calculation {}", name);
            }
        }
    }
}

fn expected_clone(expected: &TopN) -> TopN {
    match expected {
        TopN::Value(value) => TopN::Value(*value),
        TopN::OutOfRange => TopN::OutOfRange,
    }
}
