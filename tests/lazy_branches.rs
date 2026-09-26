//! Named acceptance tests for lazy per-row evaluation in the columnar
//! evaluators (`docs/execution-semantics.md`).
//!
//! Each shape below failed a whole fast or dense batch before active-row
//! masking, although explain answered every row: an error on a branch, an
//! `and`/`or` operand or a related member that the row's evaluation never
//! reaches. Most are real `rulespec-us` idioms, named after the module that
//! reproduced the failure on 2026-09-24. Every test runs explain, fast and
//! dense (Decimal) and requires them to agree; `tests/execution_mode_parity.rs`
//! checks the same contract on generated programs.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionRequest,
    ExecutionResponse, OutputValue, RulePin, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{JudgmentOutcome, Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec, PeriodSpec,
    RelationRecordSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug)]
enum V {
    I(i64),
    D(&'static str),
    B(bool),
}

impl V {
    fn spec(self) -> ScalarValueSpec {
        match self {
            V::I(value) => ScalarValueSpec::Integer { value },
            V::D(value) => ScalarValueSpec::Decimal {
                value: value.to_string(),
            },
            V::B(value) => ScalarValueSpec::Bool { value },
        }
    }
}

/// One household: its inputs (an absent name is a missing input) and its
/// members' inputs.
#[derive(Clone, Default)]
struct Household {
    inputs: Vec<(&'static str, V)>,
    members: Vec<Vec<(&'static str, V)>>,
}

fn household(inputs: &[(&'static str, V)]) -> Household {
    Household {
        inputs: inputs.to_vec(),
        members: Vec::new(),
    }
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn compile(rulespec: &str) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles")
}

fn dataset(households: &[Household]) -> DatasetSpec {
    let period = period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let mut inputs = Vec::new();
    let mut relations = Vec::new();
    for (row, household) in households.iter().enumerate() {
        let id = format!("h{row}");
        for (name, value) in &household.inputs {
            inputs.push(InputRecordSpec {
                name: name.to_string(),
                entity: "Household".to_string(),
                entity_id: id.clone(),
                interval: interval.clone(),
                value: value.spec(),
            });
        }
        for (member, member_inputs) in household.members.iter().enumerate() {
            let person = format!("p{row}_{member}");
            for (name, value) in member_inputs {
                inputs.push(InputRecordSpec {
                    name: name.to_string(),
                    entity: "Person".to_string(),
                    entity_id: person.clone(),
                    interval: interval.clone(),
                    value: value.spec(),
                });
            }
            relations.push(RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![person, id.clone()],
                interval: interval.clone(),
            });
        }
    }
    DatasetSpec { inputs, relations }
}

fn queries(households: &[Household], outputs: &[&str]) -> Vec<ExecutionQuery> {
    (0..households.len())
        .map(|row| ExecutionQuery {
            assessment_date: None,
            entity_id: format!("h{row}"),
            period: period(),
            outputs: outputs.iter().map(|output| output.to_string()).collect(),
        })
        .collect()
}

fn run(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    households: &[Household],
    outputs: &[&str],
    pins: &[(&str, V)],
) -> Result<ExecutionResponse, ApiError> {
    execute_compiled_request(
        artifact.clone(),
        CompiledExecutionRequest {
            mode,
            dataset: dataset(households),
            queries: queries(households, outputs),
            pins: pins
                .iter()
                .map(|(rule, value)| RulePin {
                    rule: rule.to_string(),
                    value: value.spec(),
                })
                .collect(),
        },
    )
}

fn without_trace(response: &ExecutionResponse) -> serde_json::Value {
    let mut results = serde_json::to_value(&response.results).expect("results serialise");
    for result in results.as_array_mut().expect("array") {
        result.as_object_mut().expect("object").remove("trace");
    }
    results
}

fn column(values: Vec<V>) -> DenseColumn {
    match values.first() {
        Some(V::I(_)) => DenseColumn::Integer(
            values
                .iter()
                .map(|value| match value {
                    V::I(value) => *value,
                    other => panic!("mixed column: {other:?}"),
                })
                .collect(),
        ),
        Some(V::D(_)) => DenseColumn::Decimal(
            values
                .iter()
                .map(|value| match value {
                    V::D(value) => value.parse().expect("decimal"),
                    other => panic!("mixed column: {other:?}"),
                })
                .collect(),
        ),
        _ => DenseColumn::Bool(
            values
                .iter()
                .map(|value| match value {
                    V::B(value) => *value,
                    other => panic!("mixed column: {other:?}"),
                })
                .collect(),
        ),
    }
}

/// Dense columns for every input all rows supply. An input no row supplies is
/// an absent column; one only some rows supply cannot be a dense column.
fn columns(rows: &[Vec<(&'static str, V)>]) -> HashMap<String, DenseColumn> {
    let mut names = Vec::new();
    for row in rows {
        for (name, _) in row {
            if !names.contains(name) {
                names.push(*name);
            }
        }
    }
    names
        .into_iter()
        .map(|name| {
            let values = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .find(|(input, _)| *input == name)
                        .map(|(_, value)| *value)
                        .unwrap_or_else(|| panic!("`{name}` must be supplied by every dense row"))
                })
                .collect();
            (name.to_string(), column(values))
        })
        .collect()
}

fn dense(
    artifact: &CompiledProgramArtifact,
    households: &[Household],
    outputs: &[&str],
) -> Result<DenseExecutionResult, EvalError> {
    let program = DenseCompiledProgram::from_artifact(artifact, Some("Household"))
        .expect("dense compilation succeeds");
    let mut relations = HashMap::new();
    if !program.relations().is_empty() {
        let mut offsets = vec![0];
        let mut members = Vec::new();
        for household in households {
            members.extend(household.members.iter().cloned());
            offsets.push(members.len());
        }
        relations.insert(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets,
                inputs: columns(&members),
            },
        );
    }
    let rows = households
        .iter()
        .map(|household| household.inputs.clone())
        .collect::<Vec<_>>();
    program.execute(
        &period().to_model().expect("period converts"),
        DenseBatchSpec {
            row_count: households.len(),
            inputs: columns(&rows),
            relations,
        },
        &outputs
            .iter()
            .map(|output| output.to_string())
            .collect::<Vec<_>>(),
    )
}

fn decimal_of(value: &ScalarValueSpec) -> Option<Decimal> {
    match value {
        ScalarValueSpec::Integer { value } => Some(Decimal::from(*value)),
        ScalarValueSpec::Decimal { value } => value.parse().ok(),
        _ => None,
    }
}

/// Explain answers every row; fast answers the same, on its own path, with
/// the same value kinds; dense holds explain's values.
fn assert_all_modes_agree(rulespec: &str, households: &[Household], outputs: &[&str]) {
    let artifact = compile(rulespec);
    let explain = run(ExecutionMode::Explain, &artifact, households, outputs, &[])
        .expect("explain answers every row");
    let fast = run(ExecutionMode::Fast, &artifact, households, outputs, &[])
        .expect("fast answers every row");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(fast.metadata.fallback_reason, None);
    assert_eq!(without_trace(&fast), without_trace(&explain));

    let dense = dense(&artifact, households, outputs).expect("dense answers every row");
    for (row, result) in explain.results.iter().enumerate() {
        for output in outputs {
            let explain_value = &result.outputs[*output];
            match (explain_value, &dense.outputs[*output]) {
                (OutputValue::Scalar { value, .. }, DenseOutputValue::Scalar(column)) => {
                    let dense_value = match column {
                        DenseColumn::Integer(values) => Some(Decimal::from(values[row])),
                        DenseColumn::Decimal(values) => Some(values[row]),
                        DenseColumn::Bool(values) => {
                            assert_eq!(value, &ScalarValueSpec::Bool { value: values[row] });
                            continue;
                        }
                        other => panic!("unexpected dense column {other:?}"),
                    };
                    assert_eq!(
                        decimal_of(value),
                        dense_value,
                        "{output} on row {row}: explain {value:?}"
                    );
                }
                (OutputValue::Judgment { outcome, .. }, DenseOutputValue::Judgment(values)) => {
                    let dense_outcome = match values[row] {
                        JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
                        JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
                        JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
                    };
                    assert_eq!(outcome, &dense_outcome, "{output} on row {row}");
                }
                (explain, dense) => panic!("{output}: explain {explain:?}, dense {dense:?}"),
            }
        }
    }
}

fn module(rules: &str) -> String {
    format!("format: rulespec/v1\nrules:\n{rules}")
}

fn rule(name: &str, dtype: &str, formula: &str) -> String {
    format!(
        "  - name: {name}\n    kind: derived\n    entity: Household\n    dtype: {dtype}\n    period: Month\n    versions:\n      - effective_from: '2026-01-01'\n        formula: |-\n          {}\n",
        formula.replace('\n', "\n          ")
    )
}

fn person_rule(name: &str, dtype: &str, formula: &str) -> String {
    rule(name, dtype, formula).replace("entity: Household", "entity: Person")
}

const MEMBERS: &str = "  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n";

fn size_and_income(size: i64, income: &'static str) -> Household {
    household(&[("household_size", V::I(size)), ("income", V::D(income))])
}

#[test]
fn a_zero_guarded_division_answers_every_row() {
    // 7 CFR 273.11(c) and 10 CCR 2506-1 4.411: prorate over a member count
    // that is zero for most households, guarded by the count itself.
    let rulespec = module(&rule(
        "per_capita_income",
        "Money",
        "if household_size == 0: 0\nelse: income / household_size",
    ));
    let rows = [size_and_income(0, "600"), size_and_income(2, "600")];
    assert_all_modes_agree(&rulespec, &rows, &["per_capita_income"]);
}

#[test]
fn a_zero_guard_written_as_match_answers_every_row() {
    let rulespec = module(&rule(
        "per_capita_income",
        "Money",
        "match household_size:\n    0 => 0\n    _ => income / household_size",
    ));
    let rows = [size_and_income(0, "600"), size_and_income(3, "600")];
    assert_all_modes_agree(&rulespec, &rows, &["per_capita_income"]);
}

#[test]
fn and_and_or_guards_skip_their_later_operands() {
    let rulespec = module(&format!(
        "{}{}",
        rule(
            "below_limit",
            "Judgment",
            "household_size > 0 and income / household_size < 400",
        ),
        rule(
            "exempt_or_above",
            "Judgment",
            "household_size == 0 or income / household_size > 250",
        ),
    ));
    let rows = [
        size_and_income(0, "600"),
        size_and_income(2, "600"),
        size_and_income(3, "600"),
    ];
    assert_all_modes_agree(&rulespec, &rows, &["below_limit", "exempt_or_above"]);
}

#[test]
fn an_and_guarded_proration_answers_every_row() {
    // SC SNAP manual page 60: prorate a nonrecurring medical expense over the
    // remaining certification months, when verification arrived and months
    // remain.
    let rulespec = module(&rule(
        "prorated_nonrecurring_medical_expense_amount",
        "Money",
        "if verification_provided and months_remaining > 0: expense / months_remaining\nelse: 0",
    ));
    let row = |provided, months| {
        household(&[
            ("verification_provided", V::B(provided)),
            ("months_remaining", V::I(months)),
            ("expense", V::D("120")),
        ])
    };
    let rows = [row(true, 0), row(false, 0), row(true, 4)];
    assert_all_modes_agree(
        &rulespec,
        &rows,
        &["prorated_nonrecurring_medical_expense_amount"],
    );
}

#[test]
fn a_rule_reached_only_through_a_dead_branch_is_never_evaluated() {
    // The ratio divides by zero on the first row, which never selects it.
    let rulespec = module(&format!(
        "{}{}",
        rule("ratio", "Money", "income / household_size"),
        rule("benefit", "Money", "if household_size > 0: ratio\nelse: 0"),
    ));
    let rows = [size_and_income(0, "600"), size_and_income(4, "600")];
    assert_all_modes_agree(&rulespec, &rows, &["benefit"]);
}

#[test]
fn an_input_read_only_on_the_untaken_branch_may_be_missing() {
    // Alabama TANF payment computation: the application day is read only
    // through the proration, which only mid-month additions select.
    let rulespec = module(&format!(
        "{}{}",
        rule("proration_days", "Integer", "30 - application_date_day"),
        rule(
            "prorated_amount",
            "Money",
            "if applied_after_first_day: floor((grant_difference * proration_days) / 30)\nelse: grant_difference",
        ),
    ));
    let artifact = compile(&rulespec);
    let mid_month = household(&[
        ("applied_after_first_day", V::B(true)),
        ("application_date_day", V::I(10)),
        ("grant_difference", V::D("100")),
    ]);
    let whole_month = household(&[
        ("applied_after_first_day", V::B(false)),
        ("grant_difference", V::D("100")),
    ]);
    let rows = [mid_month.clone(), whole_month.clone()];
    let explain = run(
        ExecutionMode::Explain,
        &artifact,
        &rows,
        &["prorated_amount"],
        &[],
    )
    .expect("explain answers both rows");
    let fast = run(
        ExecutionMode::Fast,
        &artifact,
        &rows,
        &["prorated_amount"],
        &[],
    )
    .expect("fast answers both rows");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(without_trace(&fast), without_trace(&explain));

    // Dense: a column no live row reads may be omitted.
    let result = dense(
        &artifact,
        &[whole_month.clone(), whole_month],
        &["prorated_amount"],
    )
    .expect("no row reads the application day");
    assert!(matches!(
        &result.outputs["prorated_amount"],
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) if values == &[Decimal::from(100); 2]
    ));
    // A live row that reads an omitted column fails as explain does.
    let mut without_day = mid_month;
    without_day
        .inputs
        .retain(|(name, _)| *name != "application_date_day");
    let error = dense(&artifact, &[without_day], &["prorated_amount"])
        .expect_err("the mid-month row reads the missing day");
    assert!(
        matches!(&error, EvalError::MissingInput { name, .. } if name == "application_date_day"),
        "{error}"
    );
}

#[test]
fn a_pinned_rule_never_reads_its_original_inputs() {
    let rulespec = module(&format!(
        "{}{}",
        rule("net_income", "Money", "gross_earned_income - deductions"),
        rule("benefit", "Money", "max(0, 500 - net_income / 2)"),
    ));
    let artifact = compile(&rulespec);
    let rows = [household(&[]), household(&[])];
    let pins = [("net_income", V::D("400"))];
    let explain = run(
        ExecutionMode::Explain,
        &artifact,
        &rows,
        &["benefit"],
        &pins,
    )
    .expect("explain honours the pin");
    let fast = run(ExecutionMode::Fast, &artifact, &rows, &["benefit"], &pins)
        .expect("fast honours the pin without the original inputs");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(without_trace(&fast), without_trace(&explain));
}

#[test]
fn each_row_keeps_the_value_kind_of_the_branch_it_selects() {
    let rulespec = module(&rule("choice", "Integer", "if flag: 1\nelse: 2.5"));
    let rows = [
        household(&[("flag", V::B(true))]),
        household(&[("flag", V::B(false))]),
    ];
    assert_all_modes_agree(&rulespec, &rows, &["choice"]);
    let fast = run(
        ExecutionMode::Fast,
        &compile(&rulespec),
        &rows,
        &["choice"],
        &[],
    )
    .expect("fast answers");
    let kinds = fast
        .results
        .iter()
        .map(|result| match &result.outputs["choice"] {
            OutputValue::Scalar { value, .. } => value.clone(),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            ScalarValueSpec::Integer { value: 1 },
            ScalarValueSpec::Decimal {
                value: "2.5".to_string()
            }
        ]
    );
}

#[test]
fn sum_where_never_reads_a_value_its_predicate_excludes() {
    // The hourly income divides by hours, which are zero exactly for the
    // members the predicate excludes.
    let rulespec = module(&format!(
        "{MEMBERS}{}{}{}",
        person_rule("works", "Judgment", "hours > 0"),
        person_rule("hourly_income", "Money", "income / hours"),
        rule(
            "worker_hourly_income",
            "Money",
            "sum_where(member_of_household, hourly_income, works)"
        ),
    ));
    let member =
        |hours: i64, income: &'static str| vec![("hours", V::I(hours)), ("income", V::D(income))];
    let rows = [
        Household {
            inputs: Vec::new(),
            members: vec![member(0, "300"), member(10, "200")],
        },
        Household {
            inputs: Vec::new(),
            members: vec![member(0, "50")],
        },
    ];
    assert_all_modes_agree(&rulespec, &rows, &["worker_hourly_income"]);
}

#[test]
fn a_mixed_entity_fast_batch_evaluates_each_output_for_the_rows_that_request_it() {
    let rulespec = module(&format!(
        "{MEMBERS}{}{}",
        rule(
            "household_income",
            "Money",
            "sum(member_of_household.income)"
        ),
        person_rule("own_income", "Money", "income"),
    ));
    let artifact = compile(&rulespec);
    let interval = IntervalSpec {
        start: period().start,
        end: period().end,
    };
    let person_income = |person: &str, value: &str| InputRecordSpec {
        name: "income".to_string(),
        entity: "Person".to_string(),
        entity_id: person.to_string(),
        interval: interval.clone(),
        value: V::D(Box::leak(value.to_string().into_boxed_str())).spec(),
    };
    let request = |mode| ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![person_income("p1", "100"), person_income("p2", "50")],
            relations: ["p1", "p2"]
                .into_iter()
                .map(|person| RelationRecordSpec {
                    name: "member_of_household".to_string(),
                    tuple: vec![person.to_string(), "h1".to_string()],
                    interval: interval.clone(),
                })
                .collect(),
        },
        queries: vec![
            ExecutionQuery {
                assessment_date: None,
                entity_id: "h1".to_string(),
                period: period(),
                outputs: vec!["household_income".to_string()],
            },
            ExecutionQuery {
                assessment_date: None,
                entity_id: "p1".to_string(),
                period: period(),
                outputs: vec!["own_income".to_string()],
            },
        ],
    };
    let explain = execute_request(request(ExecutionMode::Explain)).expect("explain answers");
    let fast = execute_request(request(ExecutionMode::Fast)).expect("fast answers");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(without_trace(&fast), without_trace(&explain));
}

#[test]
fn dense_rejects_only_live_rows_that_select_incompatible_dtypes() {
    // A branch no row selects never constrains the column's dtype; rows that
    // select both a number and a boolean cannot share one dense column.
    let rulespec = module(&rule("choice", "Integer", "if flag: 1\nelse: other_flag"));
    let artifact = compile(&rulespec);
    let row = |flag| household(&[("flag", V::B(flag)), ("other_flag", V::B(true))]);
    let all_numeric =
        dense(&artifact, &[row(true), row(true)], &["choice"]).expect("the boolean branch is dead");
    assert!(matches!(
        &all_numeric.outputs["choice"],
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) if values == &[1, 1]
    ));
    let error = dense(&artifact, &[row(true), row(false)], &["choice"])
        .expect_err("live rows select both dtypes");
    assert_eq!(
        error.to_string(),
        "type mismatch: dense if() branches must have the same dtype"
    );
    // Fast and explain represent the per-row kinds.
    let explain = run(
        ExecutionMode::Explain,
        &artifact,
        &[row(true), row(false)],
        &["choice"],
        &[],
    )
    .expect("explain answers");
    let fast = run(
        ExecutionMode::Fast,
        &artifact,
        &[row(true), row(false)],
        &["choice"],
        &[],
    )
    .expect("fast answers");
    assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
    assert_eq!(without_trace(&fast), without_trace(&explain));
}

#[test]
fn dense_f64_mode_is_lazy_too() {
    let rulespec = module(&rule(
        "per_capita_income",
        "Money",
        "if household_size == 0: 0\nelse: income / household_size",
    ));
    let program = DenseCompiledProgram::from_artifact(&compile(&rulespec), Some("Household"))
        .expect("dense compilation succeeds");
    let result = program
        .execute_f64(
            &period().to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: 2,
                inputs: HashMap::from([
                    (
                        "household_size".to_string(),
                        DenseColumn::Integer(vec![0, 2]),
                    ),
                    ("income".to_string(), DenseColumn::Float(vec![600.0, 600.0])),
                ]),
                relations: HashMap::new(),
            },
            &["per_capita_income".to_string()],
        )
        .expect("the zero row never divides");
    assert!(matches!(
        &result.outputs["per_capita_income"],
        DenseOutputValue::Scalar(DenseColumn::Float(values)) if values == &[0.0, 300.0]
    ));
}

#[test]
fn lifetime_reductions_skip_branches_no_row_selects() {
    let rulespec = "format: rulespec/v1\nrules:\n  - name: guarded_ratio_total\n    kind: derived\n    entity: Worker\n    dtype: Money\n    period: Year\n    versions:\n      - effective_from: '1960-01-01'\n        formula: |-\n          sum_over_periods(if earnings > 0: 1000 / earnings else: 0)\n";
    let program = DenseCompiledProgram::from_artifact(&compile(rulespec), Some("Worker"))
        .expect("dense compilation succeeds");
    let year = |y| Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    };
    let batch = |earnings: Vec<Decimal>| DenseBatchSpec {
        row_count: earnings.len(),
        inputs: HashMap::from([("earnings".to_string(), DenseColumn::Decimal(earnings))]),
        relations: HashMap::new(),
    };
    let result = program
        .execute_lifetime(
            &[year(2001), year(2002), year(2003)],
            vec![
                batch(vec![Decimal::ZERO, Decimal::from(500)]),
                batch(vec![Decimal::from(500), Decimal::ZERO]),
                batch(vec![Decimal::from(250), Decimal::from(1000)]),
            ],
            &["guarded_ratio_total".to_string()],
        )
        .expect("a zero-earnings year never divides");
    assert!(matches!(
        &result.outputs["guarded_ratio_total"],
        DenseOutputValue::Scalar(DenseColumn::Decimal(values))
            if values == &[Decimal::from(6), Decimal::from(3)]
    ));
}

#[test]
fn a_cached_rule_keeps_its_dtype_whichever_output_reaches_it_first() {
    // `a` reaches `passthrough` only through a branch no row takes, so the
    // first touch computes `passthrough` for no rows; requesting it afterwards
    // extends that cache. Neither order may change the column's dtype, which
    // is the input column's dtype, in either numeric mode.
    let rulespec = module(&format!(
        "{}{}",
        rule("passthrough", "Money", "x"),
        rule("a", "Money", "if 0 == 1: passthrough\nelse: 0"),
    ));
    let program = DenseCompiledProgram::from_artifact(&compile(&rulespec), Some("Household"))
        .expect("dense compilation succeeds");
    let period = period().to_model().expect("period converts");
    let batch = |column: DenseColumn| DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([("x".to_string(), column)]),
        relations: HashMap::new(),
    };
    for outputs in [["a", "passthrough"], ["passthrough", "a"]] {
        let outputs = outputs.map(str::to_string);
        let decimal_in_f64 = program
            .execute_f64(
                &period,
                batch(DenseColumn::Decimal(vec![Decimal::ONE])),
                &outputs,
            )
            .expect("f64 execution succeeds");
        assert!(
            matches!(
                &decimal_in_f64.outputs["passthrough"],
                DenseOutputValue::Scalar(DenseColumn::Decimal(values)) if values == &[Decimal::ONE]
            ),
            "{outputs:?}: {:?}",
            decimal_in_f64.outputs["passthrough"]
        );
        let float_in_decimal = program
            .execute(&period, batch(DenseColumn::Float(vec![1.0])), &outputs)
            .expect("decimal execution succeeds");
        assert!(
            matches!(
                &float_in_decimal.outputs["passthrough"],
                DenseOutputValue::Scalar(DenseColumn::Float(values)) if values == &[1.0]
            ),
            "{outputs:?}: {:?}",
            float_in_decimal.outputs["passthrough"]
        );
    }
}
