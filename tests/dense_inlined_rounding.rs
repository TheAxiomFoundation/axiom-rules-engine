//! Inlined derived rules must expose their rounded values to every dependent.
//! Explain supplies each period's oracle; lifetime sums reduce those same values.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

const MODES: [&str; 4] = ["half_up", "half_even", "floor", "ceil"];
const HEADER: &str = "format: rulespec/v1\nunits:\n  - name: USD\n    kind: currency\n    minor_units: 0\nrules:\n  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n";

#[derive(Clone, Debug)]
struct Household {
    status: i64,
    members: Vec<i64>,
}

fn rule(
    name: &str,
    entity: Option<&str>,
    dtype: &str,
    formula: &str,
    mode: Option<&str>,
) -> String {
    let entity = entity.map_or_else(String::new, |entity| format!("    entity: {entity}\n"));
    let rounding = mode.map_or_else(String::new, |mode| format!("    rounding: {mode}\n"));
    let unit = if dtype == "Money" {
        "    unit: USD\n"
    } else {
        ""
    };
    format!(
        "  - name: {name}\n    kind: derived\n{entity}    dtype: {dtype}\n{unit}{rounding}    versions:\n      - effective_from: 2026-01-01\n        formula: |-\n          {formula}\n"
    )
}

fn period(month: u32) -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, month, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2026, month, if month == 1 { 31 } else { 28 })
            .unwrap(),
    }
}

fn dataset(period: &PeriodSpec, households: &[Household]) -> DatasetSpec {
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let mut dataset = DatasetSpec::default();
    for (row, household) in households.iter().enumerate() {
        let household_id = format!("household-{row}");
        dataset.inputs.push(InputRecordSpec {
            name: "household_status".to_string(),
            entity: "Household".to_string(),
            entity_id: household_id.clone(),
            interval: interval.clone(),
            value: ScalarValueSpec::Integer {
                value: household.status,
            },
        });
        for (member, status) in household.members.iter().enumerate() {
            let person_id = format!("person-{row}-{member}");
            dataset.inputs.push(InputRecordSpec {
                name: "status".to_string(),
                entity: "Person".to_string(),
                entity_id: person_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer { value: *status },
            });
            dataset.relations.push(RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person_id, household_id.clone()],
                interval: interval.clone(),
            });
        }
    }
    dataset
}

fn batch(households: &[Household]) -> DenseBatchSpec {
    let mut offsets = vec![0];
    let mut statuses = Vec::new();
    for household in households {
        statuses.extend_from_slice(&household.members);
        offsets.push(statuses.len());
    }
    DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([(
            "household_status".to_string(),
            DenseColumn::Integer(
                households
                    .iter()
                    .map(|household| household.status)
                    .collect(),
            ),
        )]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets,
                inputs: HashMap::from([("status".to_string(), DenseColumn::Integer(statuses))]),
            },
        )]),
    }
}

fn numeric(value: &OutputValue) -> Decimal {
    match value {
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => Decimal::from_str(value).unwrap(),
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => Decimal::from(*value),
        other => panic!("expected numeric output, got {other:?}"),
    }
}

fn decimal_at(result: &DenseExecutionResult, output: &str, row: usize) -> Decimal {
    match &result.outputs[output] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values[row],
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => Decimal::from(values[row]),
        other => panic!("expected Decimal mode numeric output, got {other:?}"),
    }
}

fn float_at(result: &DenseExecutionResult, output: &str, row: usize) -> f64 {
    match &result.outputs[output] {
        DenseOutputValue::Scalar(DenseColumn::Float(values)) => values[row],
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values[row] as f64,
        other => panic!("expected f64 mode numeric output, got {other:?}"),
    }
}

fn compare_results(
    context: &str,
    float_tolerance: f64,
    expected: Option<&BTreeMap<String, Vec<Decimal>>>,
    decimal: Result<DenseExecutionResult, EvalError>,
    float: Result<DenseExecutionResult, EvalError>,
) {
    assert_eq!(
        decimal.is_ok(),
        expected.is_some(),
        "{context}: Decimal success parity: {decimal:?}"
    );
    assert_eq!(
        float.is_ok(),
        expected.is_some(),
        "{context}: f64 success parity: {float:?}"
    );
    let Some(expected) = expected else { return };
    let decimal = decimal.unwrap();
    let float = float.unwrap();
    let mut mismatches = Vec::new();
    for (output, values) in expected {
        assert_eq!(decimal.row_count, values.len());
        assert_eq!(float.row_count, values.len());
        for (row, value) in values.iter().enumerate() {
            let d = decimal_at(&decimal, output, row);
            let f = float_at(&float, output, row);
            // Whole-unit cases compare exactly; accumulated currency cents
            // need not have an exact f64 representation.
            let expected_float = value.to_f64().unwrap();
            let float_matches = f == expected_float || (f - expected_float).abs() < float_tolerance;
            if (d != *value || !float_matches) && mismatches.len() < 12 {
                mismatches.push(format!(
                    "{output}[{row}]: explain={value}, Decimal={d}, f64={f:?}"
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{context}:\n{}",
        mismatches.join("\n")
    );
}

/// Compare both numeric modes, including every output/row, and reduce the
/// independent Explain results to check both lifetime entry points as well.
fn assert_parity(
    program: &str,
    entity: &str,
    outputs: &[&str],
    households: &[Household],
    expected_success: bool,
    context: &str,
) {
    let float_tolerance = if program.contains("minor_units: 2") {
        1e-12
    } else {
        0.0
    };
    let mut program = program.to_string();
    for output in outputs {
        program.push_str(&rule(
            &format!("lifetime_{output}"),
            Some(entity),
            "Money",
            &format!("sum_over_periods({output})"),
            None,
        ));
    }
    let artifact = CompiledProgramArtifact::from_rulespec_str(&program).expect("RuleSpec compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some(entity)).expect("dense compiles");
    let outputs: Vec<_> = outputs.iter().map(|name| (*name).to_string()).collect();
    let periods = [period(1), period(2)];
    let mut lifetime = BTreeMap::<String, Vec<Decimal>>::new();
    for period in &periods {
        let explain = execute_request(ExecutionRequest {
            mode: ExecutionMode::Explain,
            program: artifact.program.clone(),
            dataset: dataset(period, households),
            queries: (0..households.len())
                .map(|row| ExecutionQuery {
                    assessment_date: None,
                    entity_id: format!("household-{row}"),
                    period: period.clone(),
                    outputs: outputs.clone(),
                })
                .collect(),
        });
        assert_eq!(
            explain.is_ok(),
            expected_success,
            "{context}: Explain: {explain:?}"
        );
        let expected = explain.ok().map(|explain| {
            outputs
                .iter()
                .map(|output| {
                    let values: Vec<_> = explain
                        .results
                        .iter()
                        .map(|row| numeric(&row.outputs[output]))
                        .collect();
                    let total = lifetime
                        .entry(format!("lifetime_{output}"))
                        .or_insert_with(|| vec![Decimal::ZERO; households.len()]);
                    for (total, value) in total.iter_mut().zip(&values) {
                        *total += value;
                    }
                    (output.clone(), values)
                })
                .collect::<BTreeMap<_, _>>()
        });
        let period = period.to_model().unwrap();
        compare_results(
            context,
            float_tolerance,
            expected.as_ref(),
            dense.execute(&period, batch(households), &outputs),
            dense.execute_f64(&period, batch(households), &outputs),
        );
    }
    let periods: Vec<_> = periods
        .iter()
        .map(|period| period.to_model().unwrap())
        .collect();
    let outputs: Vec<_> = outputs
        .iter()
        .map(|output| format!("lifetime_{output}"))
        .collect();
    compare_results(
        &format!("{context}, lifetime"),
        float_tolerance,
        expected_success.then_some(&lifetime),
        dense.execute_lifetime(
            &periods,
            periods.iter().map(|_| batch(households)).collect(),
            &outputs,
        ),
        dense.execute_lifetime_f64(
            &periods,
            periods.iter().map(|_| batch(households)).collect(),
            &outputs,
        ),
    );
}

fn variants(mode: Option<&str>) -> String {
    let mut program = HEADER.to_string();
    for (name, entity, dtype, formula, rounding) in [
        ("half", Some("Person"), "Money", "status / 2", mode),
        ("doubled_half", Some("Person"), "Money", "half * 2", None),
        (
            "half_reaches_one",
            Some("Person"),
            "Judgment",
            "half >= 1",
            None,
        ),
        ("rate", None, "Money", "5 / 2", mode),
        (
            "rated_status",
            Some("Person"),
            "Money",
            "status * rate",
            None,
        ),
        (
            "household_half",
            Some("Household"),
            "Money",
            "household_status / 2",
            mode,
        ),
        (
            "household_doubled",
            Some("Household"),
            "Money",
            "household_half * 2",
            None,
        ),
        (
            "total_half",
            Some("Household"),
            "Money",
            "sum(member_of_household.half)",
            None,
        ),
        (
            "total_doubled",
            Some("Household"),
            "Money",
            "sum(member_of_household.doubled_half)",
            None,
        ),
        (
            "count_half",
            Some("Household"),
            "Integer",
            "count_where(member_of_household, half_reaches_one)",
            None,
        ),
        (
            "total_rated",
            Some("Household"),
            "Money",
            "sum(member_of_household.rated_status)",
            None,
        ),
    ] {
        program.push_str(&rule(name, entity, dtype, formula, rounding));
    }
    program
}

const VARIANT_OUTPUTS: [&str; 6] = [
    "total_half",
    "total_doubled",
    "count_half",
    "total_rated",
    "household_half",
    "household_doubled",
];

#[test]
fn dense_inlined_rounding_repro_matches_explain() {
    assert_parity(
        &variants(Some("half_up")),
        "Household",
        &["total_half"],
        &[Household {
            status: 1,
            members: vec![1, 1],
        }],
        true,
        "two members with status 1",
    );
}

#[test]
fn dense_inlined_rounding_variants_exhaustive_domain() {
    let mut households = vec![Household {
        status: 1,
        members: Vec::new(),
    }];
    // Every member list of length zero, one or two over [-3, 3] covers signed
    // midpoints, exact integers, cancellation and mixed rounded predicates.
    // Separate root-dependency and current-filter tests sweep root inputs.
    for first in -3..=3 {
        households.push(Household {
            status: 1,
            members: vec![first],
        });
        for second in -3..=3 {
            households.push(Household {
                status: 1,
                members: vec![first, second],
            });
        }
    }
    for mode in MODES {
        assert_parity(
            &variants(Some(mode)),
            "Household",
            &VARIANT_OUTPUTS,
            &households,
            true,
            mode,
        );
        // Eighths are exact in both numeric representations, including the
        // half-cent ties at .125 and .375 when rounded to two minor units.
        let cents = variants(Some(mode))
            .replace("minor_units: 0", "minor_units: 2")
            .replace("status / 2", "status / 8");
        assert_parity(
            &cents,
            "Household",
            &VARIANT_OUTPUTS,
            &households,
            true,
            &format!("{mode}, two minor units"),
        );
    }
}

#[test]
fn dense_inlined_rounding_without_declarations_preserves_values() {
    let households = [-3, -1, 0, 1, 3]
        .into_iter()
        .map(|status| Household {
            status,
            members: vec![status, 1],
        })
        .collect::<Vec<_>>();
    assert_parity(
        &variants(None),
        "Household",
        &VARIANT_OUTPUTS,
        &households,
        true,
        "no rounding",
    );
}

#[test]
fn dense_inlined_rounding_root_dependencies_preserve_values() {
    let households = (-3..=3)
        .map(|status| Household {
            status,
            members: Vec::new(),
        })
        .collect::<Vec<_>>();
    for mode in MODES {
        assert_parity(
            &variants(Some(mode)),
            "Household",
            &["household_half", "household_doubled"],
            &households,
            true,
            &format!("root dependency, {mode}"),
        );
    }
}

fn filtered_program(mode: &str, predicate: &str) -> String {
    let mut program = variants(Some(mode));
    program.push_str(&rule(
        "household_reaches_one",
        Some("Household"),
        "Judgment",
        "household_half >= 1",
        None,
    ));
    program.push_str(&rule(
        "household_doubled_reaches_two",
        Some("Household"),
        "Judgment",
        "household_doubled >= 2",
        None,
    ));
    program.push_str(&format!("  - name: accepted_household\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: member_of_household\n      entity: AcceptedHousehold\n      member_relation: accepted_members\n      slot_entities: [Person, Household]\n    versions:\n      - effective_from: 2026-01-01\n        formula: {predicate}\n"));
    program.push_str(&rule(
        "accepted_count",
        Some("AcceptedHousehold"),
        "Integer",
        "len(accepted_members)",
        None,
    ));
    program
}

#[test]
fn dense_inlined_rounding_current_entity_filters_match_explain() {
    let households = (-3..=3)
        .map(|status| Household {
            status,
            members: vec![1, 3],
        })
        .collect::<Vec<_>>();
    for mode in MODES {
        for predicate in [
            "household_half >= 1",
            "household_reaches_one",
            "household_doubled >= 2",
            "household_doubled_reaches_two",
        ] {
            assert_parity(
                &filtered_program(mode, predicate),
                "AcceptedHousehold",
                &["accepted_count"],
                &households,
                true,
                &format!("{mode}: {predicate}"),
            );
        }
    }
}

#[test]
fn dense_inlined_rounding_preserves_live_row_errors() {
    let households = [
        Household {
            status: 0,
            members: vec![],
        },
        Household {
            status: 2,
            members: vec![2, -2],
        },
        Household {
            status: -2,
            members: vec![0, 2],
        },
    ];
    let mut program = HEADER.to_string();
    // The shared dataset includes this current-row input even when the
    // requested outputs only read related rows.
    program.push_str(&rule(
        "household_status_value",
        Some("Household"),
        "Money",
        "household_status",
        None,
    ));
    for (name, entity, dtype, formula, rounding) in [
        (
            "rounded_ratio",
            "Person",
            "Money",
            "1 / status",
            Some("half_up"),
        ),
        (
            "guarded_ratio",
            "Person",
            "Money",
            "if status == 0: 0 else: rounded_ratio",
            None,
        ),
        (
            "guarded_predicate",
            "Person",
            "Judgment",
            "status == 0 or rounded_ratio >= 1",
            None,
        ),
        (
            "live_predicate",
            "Person",
            "Judgment",
            "rounded_ratio >= 1",
            None,
        ),
        (
            "safe_sum",
            "Household",
            "Money",
            "sum(member_of_household.guarded_ratio)",
            None,
        ),
        (
            "safe_count",
            "Household",
            "Integer",
            "count_where(member_of_household, guarded_predicate)",
            None,
        ),
        (
            "live_sum",
            "Household",
            "Money",
            "sum(member_of_household.rounded_ratio)",
            None,
        ),
        (
            "live_count",
            "Household",
            "Integer",
            "count_where(member_of_household, live_predicate)",
            None,
        ),
    ] {
        program.push_str(&rule(name, Some(entity), dtype, formula, rounding));
    }
    assert_parity(
        &program,
        "Household",
        &["safe_sum", "safe_count"],
        &households,
        true,
        "dead related errors",
    );
    for output in ["live_sum", "live_count"] {
        assert_parity(
            &program,
            "Household",
            &[output],
            &households,
            false,
            "live related errors",
        );
    }
    let mut program = filtered_program("half_up", "household_safe")
        .replace("household_status / 2", "1 / household_status");
    program.push_str(&rule(
        "household_safe",
        Some("Household"),
        "Judgment",
        "household_status == 0 or household_reaches_one",
        None,
    ));
    let current_households = [
        Household {
            status: 0,
            members: vec![1],
        },
        households[1].clone(),
        households[2].clone(),
    ];
    assert_parity(
        &program,
        "AcceptedHousehold",
        &["accepted_count"],
        &current_households,
        true,
        "dead current errors",
    );
    let program = program.replace(
        "household_status == 0 or household_reaches_one",
        "household_reaches_one",
    );
    let live_households = [
        Household {
            status: 0,
            members: vec![1],
        },
        households[1].clone(),
    ];
    assert_parity(
        &program,
        "AcceptedHousehold",
        &["accepted_count"],
        &live_households,
        false,
        "live current errors",
    );
}
