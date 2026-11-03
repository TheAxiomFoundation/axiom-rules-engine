//! Differential test for lifetime reductions across the execution paths.
//!
//! Lifetime execution has no explain counterpart, but each reduction is a
//! fold over per-period values that every evaluator computes. So for each
//! generated program and dataset:
//!
//! 1. explain, fast and per-period dense evaluate the reduced expression for
//!    every (row, period); the three must agree on value or refusal;
//! 2. this file folds those per-period outcomes into the expected lifetime
//!    outcome (`sum`, `max`, `count`, `sum_top_n` with a literal or a nested
//!    `count_over_periods` count);
//! 3. dense lifetime (`execute_lifetime`, Decimal) must return that outcome
//!    row by row, and a calculation plan (`CalculationLifetimePlan`) whose law
//!    has one version must return the same values.
//!
//! Generated programs use decimal, integer and boolean inputs, divisors that
//! are often zero, an indexed parameter with a missing key,
//! `calendar_years_to_months` with an overflowing input, `if` helpers and
//! nested top-N counts. The refusals expected on both sides are the
//! evaluation errors those produce (division by zero, missing parameter key,
//! calendar overflow) and `OverPeriodsTopNOutOfRange` for a count outside
//! `1..=periods`. The calculation plan must refuse with the same variant and
//! message as dense lifetime; a missing parameter key names the calculation
//! date instead of the observation's, and that variant's other fields must
//! match.
//!
//! `AXIOM_LIFETIME_DIFF_CASES=<n>` sets the case count (default 48). The seed
//! is fixed, so CI is deterministic.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use rust_decimal::Decimal;

const PERIODS: [i32; 3] = [2001, 2002, 2003];
const ROWS: usize = 3;
const OUTPUTS: [&str; 4] = ["out_sum", "out_max", "out_count", "out_top"];

// ---------------------------------------------------------------------------
// Generation: a byte string decoded into a program. Shrinking the bytes
// toward zero shrinks the program toward `amount`.
// ---------------------------------------------------------------------------

struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let byte = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }
}

const LITERALS: [&str; 5] = ["0", "1", "-2", "1.5", "3"];

fn num(bytes: &mut Bytes<'_>, depth: u8, helpers: &[&str]) -> String {
    let choice = bytes.next();
    if depth == 0 || choice % 3 == 0 {
        let leaf = bytes.next() as usize % (5 + helpers.len());
        return match leaf {
            0 => "amount".into(),
            1 => "bonus".into(),
            2 => LITERALS[bytes.next() as usize % LITERALS.len()].into(),
            3 => "rate[key]".into(),
            4 => "calendar_years_to_months(years)".into(),
            helper => helpers[helper - 5].into(),
        };
    }
    let left = num(bytes, depth - 1, helpers);
    match (choice / 3) % 6 {
        0 => format!("({left} + {})", num(bytes, depth - 1, helpers)),
        1 => format!("({left} - {})", num(bytes, depth - 1, helpers)),
        2 => format!(
            "({left} * {})",
            LITERALS[bytes.next() as usize % LITERALS.len()]
        ),
        3 => format!("({left} / divisor)"),
        4 => format!("max({left}, {})", num(bytes, depth - 1, helpers)),
        _ => format!("floor({left})"),
    }
}

fn condition(bytes: &mut Bytes<'_>) -> &'static str {
    ["flag", "amount > 0", "h1 > 0"][bytes.next() as usize % 3]
}

/// The `n` of the top-N output, and the rule whose per-period values a
/// nested count reads, if any.
fn top_n(bytes: &mut Bytes<'_>) -> (String, Option<&'static str>) {
    match bytes.next() % 5 {
        0 => ("1".into(), None),
        1 => ("2".into(), None),
        // Past the period count: refused for every row.
        2 => ("4".into(), None),
        3 => ("count_over_periods(flag_num)".into(), Some("flag_num")),
        _ => ("count_over_periods(h1)".into(), Some("h1")),
    }
}

struct Generated {
    source: String,
    n_literal: Option<i64>,
    n_counts: Option<&'static str>,
    text: String,
}

fn rule(name: &str, dtype: &str, formula: &str) -> String {
    let formula = formula.replace('\n', "\n          ");
    format!(
        "  - name: {name}\n    kind: derived\n    entity: Worker\n    dtype: {dtype}\n    versions:\n      - effective_from: '1990-01-01'\n        formula: |-\n          {formula}\n"
    )
}

fn generate(data: &[u8]) -> Generated {
    let mut bytes = Bytes { data, at: 0 };
    let h1 = num(&mut bytes, 2, &[]);
    let h2 = format!(
        "if {}: {}\nelse: {}",
        condition(&mut bytes),
        num(&mut bytes, 1, &["h1"]),
        num(&mut bytes, 1, &["h1"])
    );
    let inner = num(&mut bytes, 2, &["h1", "h2"]);
    let (n, n_counts) = top_n(&mut bytes);
    let mut source = String::from(
        "format: rulespec/v1\nrules:\n  - name: rate\n    kind: parameter\n    dtype: Integer\n    versions:\n      - effective_from: '1990-01-01'\n        formula: '0'\n",
    );
    source.push_str(&rule("h1", "Money", &h1));
    source.push_str(&rule("h2", "Money", &h2));
    source.push_str(&rule("inner", "Money", &inner));
    source.push_str(&rule("flag_num", "Integer", "if flag: 1\nelse: 0"));
    // Reads every input, so a calculation plan, which compiles every rule for
    // the entity, accepts each supplied column. Never requested.
    source.push_str(&rule(
        "touch",
        "Money",
        "if flag: amount + bonus + divisor + rate[key] + calendar_years_to_months(years)\nelse: 0",
    ));
    source.push_str(&rule("out_sum", "Money", "sum_over_periods(inner)"));
    source.push_str(&rule("out_max", "Money", "max_over_periods(inner)"));
    source.push_str(&rule("out_count", "Integer", "count_over_periods(inner)"));
    source.push_str(&rule(
        "out_top",
        "Money",
        &format!("sum_top_n_over_periods(inner, {n})"),
    ));
    Generated {
        n_literal: n.parse().ok(),
        n_counts,
        text: format!("h1 = {h1}\nh2 = {h2}\ninner = {inner}\nn = {n}"),
        source,
    }
}

fn artifact(source: &str) -> CompiledProgramArtifact {
    let mut program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap_or_else(|error| panic!("generated program compiles: {error}\n{source}"))
        .program;
    let rate = program
        .parameters
        .iter_mut()
        .find(|parameter| parameter.name == "rate")
        .expect("rate parameter");
    rate.indexed_by = Some("rate_key".into());
    // Key 3 is deliberately absent.
    rate.versions[0].values = BTreeMap::from([
        (1, ScalarValueSpec::Integer { value: 2 }),
        (2, ScalarValueSpec::Integer { value: 5 }),
    ])
    .into_iter()
    .collect();
    CompiledProgramArtifact::compile(program).expect("generated artifact compiles")
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Cell {
    amount: Decimal,
    bonus: i64,
    flag: bool,
    divisor: Decimal,
    key: i64,
    years: i64,
}

fn cell(choice: [u8; 6]) -> Cell {
    let decimal = |text: &str| Decimal::from_str(text).unwrap();
    Cell {
        amount: decimal(["0", "2", "-1.5", "10.25"][choice[0] as usize % 4]),
        bonus: [0, 4, -3][choice[1] as usize % 3],
        flag: choice[2] % 2 == 1,
        divisor: decimal(["2", "0", "0.5"][choice[3] as usize % 3]),
        key: [1, 2, 3][choice[4] as usize % 3],
        // The last overflows i64 months: a refusal on every path.
        years: [0, 2, -1, 0, 2, 800_000_000_000_000_000][choice[5] as usize % 6],
    }
}

/// `cells[period][row]`.
fn cells(data: &[u8]) -> Vec<Vec<Cell>> {
    (0..PERIODS.len())
        .map(|period| {
            (0..ROWS)
                .map(|row| {
                    let at = (period * ROWS + row) * 6;
                    let mut choice = [0; 6];
                    for (offset, slot) in choice.iter_mut().enumerate() {
                        *slot = data.get(at + offset).copied().unwrap_or(0);
                    }
                    cell(choice)
                })
                .collect()
        })
        .collect()
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: format!("{y}-01-01").parse().unwrap(),
        end: format!("{y}-12-31").parse().unwrap(),
    }
}

fn batch(cells: &[&Cell]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: cells.len(),
        inputs: HashMap::from([
            (
                "amount".into(),
                DenseColumn::Decimal(cells.iter().map(|c| c.amount).collect()),
            ),
            (
                "bonus".into(),
                DenseColumn::Integer(cells.iter().map(|c| c.bonus).collect()),
            ),
            (
                "flag".into(),
                DenseColumn::Bool(cells.iter().map(|c| c.flag).collect()),
            ),
            (
                "divisor".into(),
                DenseColumn::Decimal(cells.iter().map(|c| c.divisor).collect()),
            ),
            (
                "key".into(),
                DenseColumn::Integer(cells.iter().map(|c| c.key).collect()),
            ),
            (
                "years".into(),
                DenseColumn::Integer(cells.iter().map(|c| c.years).collect()),
            ),
        ]),
        relations: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Per-period oracles
// ---------------------------------------------------------------------------

type Outcome = Result<Decimal, String>;

fn explain_like(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    cell: &Cell,
    y: i32,
    rule: &str,
) -> Outcome {
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: year(y).start,
        end: year(y).end,
    };
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let record = |name: &str, value: ScalarValueSpec| InputRecordSpec {
        name: name.into(),
        entity: "Worker".into(),
        entity_id: "w".into(),
        interval: interval.clone(),
        value,
    };
    let request = ExecutionRequest {
        relation_binding: Default::default(),
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![
                record(
                    "amount",
                    ScalarValueSpec::Decimal {
                        value: cell.amount.to_string(),
                    },
                ),
                record("bonus", ScalarValueSpec::Integer { value: cell.bonus }),
                record("flag", ScalarValueSpec::Bool { value: cell.flag }),
                record(
                    "divisor",
                    ScalarValueSpec::Decimal {
                        value: cell.divisor.to_string(),
                    },
                ),
                record("key", ScalarValueSpec::Integer { value: cell.key }),
                record("years", ScalarValueSpec::Integer { value: cell.years }),
            ],
            ..Default::default()
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "w".into(),
            period,
            outputs: vec![rule.into()],
        }],
    };
    let response = execute_request(request).map_err(|error| error.to_string())?;
    match &response.results[0].outputs[rule] {
        OutputValue::Scalar { value, .. } => Ok(spec_decimal(value)),
        other => panic!("{rule} is scalar: {other:?}"),
    }
}

fn spec_decimal(value: &ScalarValueSpec) -> Decimal {
    match value {
        ScalarValueSpec::Integer { value } => Decimal::from(*value),
        ScalarValueSpec::Decimal { value } => Decimal::from_str(value).unwrap(),
        other => panic!("numeric value expected: {other:?}"),
    }
}

fn column_decimal(column: &DenseColumn, row: usize) -> Decimal {
    match column {
        DenseColumn::Decimal(values) => values[row],
        DenseColumn::Integer(values) => Decimal::from(values[row]),
        other => panic!("numeric column expected: {other:?}"),
    }
}

fn output(result: &DenseExecutionResult, name: &str, row: usize) -> Decimal {
    match &result.outputs[name] {
        DenseOutputValue::Scalar(column) => column_decimal(column, row),
        other => panic!("{name} is scalar: {other:?}"),
    }
}

/// One rule's value for one row in one period, after checking that explain,
/// fast and per-period dense agree on it.
fn per_period(
    artifact: &CompiledProgramArtifact,
    dense: &DenseCompiledProgram,
    cell: &Cell,
    period_index: usize,
    rule: &str,
) -> Result<Outcome, String> {
    let y = PERIODS[period_index];
    let explain = explain_like(artifact, ExecutionMode::Explain, cell, y, rule);
    let fast = explain_like(artifact, ExecutionMode::Fast, cell, y, rule);
    let dense = dense
        .execute(&year(y), batch(&[cell]), &[rule.into()])
        .map(|result| output(&result, rule, 0))
        .map_err(|error| error.to_string());
    if explain.is_ok() != fast.is_ok()
        || explain.is_ok() != dense.is_ok()
        || explain.as_ref().ok() != fast.as_ref().ok()
        || explain.as_ref().ok() != dense.as_ref().ok()
    {
        return Err(format!(
            "{rule} in {y} for {cell:?}: explain {explain:?}, fast {fast:?}, dense {dense:?}"
        ));
    }
    Ok(explain)
}

// ---------------------------------------------------------------------------
// The folded expectation
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Expected {
    Value(Decimal),
    /// An evaluation error in some period the reduction reaches.
    Refused,
    /// `OverPeriodsTopNOutOfRange`.
    TopNOutOfRange,
}

fn all_ok(values: &[Outcome]) -> Option<Vec<Decimal>> {
    values
        .iter()
        .map(|value| value.as_ref().ok().copied())
        .collect()
}

fn expected(
    generated: &Generated,
    output: &str,
    inner: &[Outcome],
    counted: Option<&[Outcome]>,
) -> Expected {
    let Some(values) = all_ok(inner) else {
        return Expected::Refused;
    };
    match output {
        "out_sum" => Expected::Value(values.iter().sum()),
        "out_max" => Expected::Value(values.iter().copied().max().unwrap()),
        "out_count" => Expected::Value(Decimal::from(
            values.iter().filter(|value| !value.is_zero()).count(),
        )),
        "out_top" => {
            let n = match (generated.n_literal, counted) {
                (Some(n), _) => n,
                (None, Some(counted)) => match all_ok(counted) {
                    Some(counted) => counted.iter().filter(|value| !value.is_zero()).count() as i64,
                    None => return Expected::Refused,
                },
                (None, None) => unreachable!("a nested count names its rule"),
            };
            if n < 1 || n > PERIODS.len() as i64 {
                return Expected::TopNOutOfRange;
            }
            let mut sorted = values.clone();
            sorted.sort_by(|a, b| b.cmp(a));
            Expected::Value(sorted[..n as usize].iter().sum())
        }
        other => unreachable!("{other}"),
    }
}

fn observed(result: Result<DenseExecutionResult, EvalError>, name: &str) -> Expected {
    match result {
        Ok(result) => Expected::Value(output(&result, name, 0)),
        Err(EvalError::OverPeriodsTopNOutOfRange { .. }) => Expected::TopNOutOfRange,
        Err(_) => Expected::Refused,
    }
}

fn check(data: &[u8], cells_data: &[u8]) -> Result<(), TestCaseError> {
    let generated = generate(data);
    let artifact = artifact(&generated.source);
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Worker"))
        .map_err(|error| TestCaseError::fail(format!("dense compiles: {error}")))?;
    let plan = CalculationLifetimePlan::from_artifact(&artifact, "Worker", year(2010))
        .map_err(|error| TestCaseError::fail(format!("calculation plan compiles: {error}")))?;
    let cells = cells(cells_data);
    let periods = PERIODS.map(year);
    let outputs = OUTPUTS.map(String::from);
    let context =
        |message: String| TestCaseError::fail(format!("{message}\nprogram:\n{}", generated.text));

    let mut per_row = Vec::new();
    for row in 0..ROWS {
        let mut inner = Vec::new();
        let mut counted = Vec::new();
        for (period, row_cells) in cells.iter().enumerate() {
            inner.push(
                per_period(&artifact, &dense, &row_cells[row], period, "inner").map_err(context)?,
            );
            if let Some(rule) = generated.n_counts {
                counted.push(
                    per_period(&artifact, &dense, &row_cells[row], period, rule)
                        .map_err(context)?,
                );
            }
        }
        let row_batches = || {
            cells
                .iter()
                .map(|row_cells| batch(&[&row_cells[row]]))
                .collect::<Vec<_>>()
        };
        let mut row_outcomes = Vec::new();
        for name in OUTPUTS {
            let want = expected(
                &generated,
                name,
                &inner,
                generated.n_counts.map(|_| counted.as_slice()),
            );
            let lifetime = dense.execute_lifetime(&periods, row_batches(), &[name.into()]);
            let got = observed(lifetime, name);
            if got != want {
                return Err(context(format!(
                    "row {row}, {name}: lifetime {got:?}, folded per-period {want:?}\ninner {inner:?}\ncounted {counted:?}\ncells {:?}",
                    cells.iter().map(|c| &c[row]).collect::<Vec<_>>()
                )));
            }
            row_outcomes.push(want);
        }
        per_row.push(row_outcomes);
    }

    // The whole batch at once: it succeeds exactly when every row does, with
    // the same values, and fails with the first failing row's error.
    let all_batches = || {
        cells
            .iter()
            .map(|row_cells| batch(&row_cells.iter().collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    };
    let every_row_ok = per_row.iter().all(|row| {
        row.iter()
            .all(|outcome| matches!(outcome, Expected::Value(_)))
    });
    let lifetime = dense.execute_lifetime(&periods, all_batches(), &outputs);
    let calculation = plan.execute(&periods, all_batches(), &outputs);
    match (&lifetime, every_row_ok) {
        (Ok(result), true) => {
            for (row, outcomes) in per_row.iter().enumerate() {
                for (name, outcome) in OUTPUTS.iter().zip(outcomes) {
                    let Expected::Value(value) = outcome else {
                        unreachable!()
                    };
                    if output(result, name, row) != *value {
                        return Err(context(format!("batch row {row}, {name} differs")));
                    }
                }
            }
        }
        (Err(error), false) => {
            let first_row = per_row
                .iter()
                .position(|row| {
                    row.iter()
                        .any(|outcome| !matches!(outcome, Expected::Value(_)))
                })
                .unwrap();
            let alone = dense
                .execute_lifetime(
                    &periods,
                    cells
                        .iter()
                        .map(|row_cells| batch(&[&row_cells[first_row]]))
                        .collect(),
                    &outputs,
                )
                .expect_err("the first failing row fails alone");
            if alone.to_string() != error.to_string() {
                return Err(context(format!(
                    "batch error `{error}` is not first failing row {first_row}'s `{alone}`"
                )));
            }
        }
        (lifetime, _) => {
            return Err(context(format!(
                "batch {lifetime:?} against per-row outcomes {per_row:?}"
            )));
        }
    }

    // One version of every rule and table: fixing law at 2010 changes nothing.
    match (&lifetime, &calculation) {
        (Ok(lifetime), Ok(calculation)) => {
            for name in OUTPUTS {
                for row in 0..ROWS {
                    if output(lifetime, name, row) != output(calculation, name, row) {
                        return Err(context(format!(
                            "calculation plan differs at row {row}, {name}"
                        )));
                    }
                }
            }
        }
        (Err(lifetime), Err(calculation)) => {
            same_refusal(lifetime, calculation).map_err(context)?;
        }
        (lifetime, calculation) => {
            return Err(context(format!(
                "lifetime {lifetime:?} against calculation plan {calculation:?}"
            )));
        }
    }
    Ok(())
}

/// A one-version plan refuses with the same evaluation error as dense
/// lifetime: the same variant and message. The one difference is the date a
/// missing parameter key names: the plan looks it up at the calculation
/// period, so its message names that date.
fn same_refusal(lifetime: &EvalError, calculation: &DenseCompileError) -> Result<(), String> {
    let DenseCompileError::Eval(calculation) = calculation else {
        return Err(format!(
            "plan refusal `{calculation}` is not an evaluation error; lifetime gave `{lifetime}`"
        ));
    };
    let same = match (lifetime, calculation) {
        (
            EvalError::MissingParameterValue { parameter, key, .. },
            EvalError::MissingParameterValue {
                parameter: plan_parameter,
                key: plan_key,
                at,
            },
        ) => parameter == plan_parameter && key == plan_key && *at == year(2010).start,
        _ => lifetime.to_string() == calculation.to_string(),
    };
    if same {
        Ok(())
    } else {
        Err(format!(
            "plan refusal `{calculation}` differs from lifetime's `{lifetime}`"
        ))
    }
}

#[test]
fn lifetime_reductions_fold_the_per_period_evaluators() {
    let cases = std::env::var("AXIOM_LIFETIME_DIFF_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(48);
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]),
    );
    let strategy = (
        proptest::collection::vec(any::<u8>(), 48),
        proptest::collection::vec(any::<u8>(), PERIODS.len() * ROWS * 6),
    );
    if let Err(error) = runner.run(&strategy, |(program, data)| check(&program, &data)) {
        panic!("{error}");
    }
}

/// The simplest program the generator emits (`inner = amount`, `n = 1`)
/// over the simplest data, as a readable smoke case.
#[test]
fn simplest_generated_case_agrees() {
    check(&[], &[]).unwrap();
}
