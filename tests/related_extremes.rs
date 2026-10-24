//! `min`, `max` and `only` over related entities (issue #232, step 1).
//!
//! `min(r.f)` / `max(r.f)` (and `min_where` / `max_where`) take the least or
//! greatest value of `f` over the related entities; `only(r.f)` reads `f` for
//! the single related entity. An empty `min`/`max` and an `only` over zero or
//! several entities fail by name rather than returning a default
//! (`DECISIONS.md`, 2026-10-13).
//!
//! The plain examples come first. The properties then check, on generated
//! families, the invariants stated on the PR:
//!
//! 1. bounds: `min <= v` and `max >= v` for every selected member value `v`,
//!    and each equals some member's value;
//! 2. order: permuting relation tuples and input records changes no result
//!    and no trace;
//! 3. reference: every aggregate equals a fold over the member values
//!    computed here, independently of the engine;
//! 4. with exactly one member, `only(r.f)` equals `sum(r.f)`;
//! 5. `min_where(r, f, p)` equals `min` over the members for which `p`
//!    holds, both against the reference and on a dataset holding only those
//!    members;
//! 6. an empty set fails `min`, `max` and `only` by name, several members
//!    fail `only`, and the guarded forms never fail;
//! 7. explain, fast and dense agree (dense in both numeric modes).
//!
//! Invariant 8 (a program that uses none of them compiles to the same bytes)
//! is checked against `origin/main`'s binary on the PR, not here.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::OnceLock;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, DerivedTraceNode, ExecutionMode, ExecutionQuery,
    ExecutionResponse, NotEvaluatedReason, OutputValue, execute_compiled_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules_engine::spec::{
    DatasetSpec, DerivedSemanticsSpec, ExtremeSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec,
    PeriodSpec, RelatedValueRefSpec, RelationRecordSpec, ScalarExprSpec, ScalarValueSpec,
};
use chrono::NaiveDate;
use proptest::prelude::*;
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// The module under test
// ---------------------------------------------------------------------------

const RELATION: &str = "member_of_family";

fn rule(entity: &str, name: &str, dtype: &str, formula: &str) -> String {
    format!(
        "  - name: {name}\n    kind: derived\n    entity: {entity}\n    dtype: {dtype}\n    period: Month\n    versions:\n      - effective_from: '2026-01-01'\n        formula: |-\n          {}\n",
        formula.replace('\n', "\n          ")
    )
}

/// Family rules: every aggregate the properties check, over inputs and over
/// person rules (which the debug build's deferral threshold resumes).
const FAMILY_RULES: &[(&str, &str, &str)] = &[
    ("youngest_age", "Decimal", "min(member_of_family.age)"),
    ("oldest_age", "Decimal", "max(member_of_family.age)"),
    (
        "youngest_eligible_age",
        "Decimal",
        "min_where(member_of_family, age, eligible)",
    ),
    (
        "oldest_eligible_age",
        "Decimal",
        "max_where(member_of_family, age, eligible)",
    ),
    (
        "youngest_age_plus_one",
        "Decimal",
        "min(member_of_family.age_plus_one)",
    ),
    (
        "oldest_qualifying_age_plus_one",
        "Decimal",
        "max_where(member_of_family, age_plus_one, qualifies)",
    ),
    ("earliest_birth", "Date", "min(member_of_family.birth_date)"),
    ("latest_birth", "Date", "max(member_of_family.birth_date)"),
    ("only_age", "Decimal", "only(member_of_family.age)"),
    ("only_birth", "Date", "only(member_of_family.birth_date)"),
    ("total_age", "Decimal", "sum(member_of_family.age)"),
    ("member_count", "Integer", "len(member_of_family)"),
    (
        "guarded_youngest",
        "Decimal",
        "if len(member_of_family) > 0: min(member_of_family.age)\nelse: -100",
    ),
    (
        "guarded_oldest",
        "Decimal",
        "if len(member_of_family) > 0: max(member_of_family.age)\nelse: -100",
    ),
    (
        "guarded_youngest_eligible",
        "Decimal",
        "if count_where(member_of_family, eligible) > 0: min_where(member_of_family, age, eligible)\nelse: -100",
    ),
    (
        "guarded_only",
        "Decimal",
        "if len(member_of_family) == 1: only(member_of_family.age)\nelse: -100",
    ),
    ("family_income_doubled", "Money", "family_income * 2"),
    (
        "total_age_plus_one",
        "Decimal",
        "sum(member_of_family.age_plus_one)",
    ),
    (
        "guarded_youngest_plus_one",
        "Decimal",
        "if len(member_of_family) > 0: min(member_of_family.age_plus_one)\nelse: -100",
    ),
    (
        "guarded_total_plus_one",
        "Decimal",
        "if len(member_of_family) > 0: sum(member_of_family.age_plus_one)\nelse: -100",
    ),
];

const PERSON_RULES: &[(&str, &str, &str)] = &[
    ("age_plus_one", "Decimal", "age + 1"),
    ("qualifies", "Judgment", "eligible and age >= 0"),
    (
        "family_income_of",
        "Money",
        "only(member_of_family.family_income)",
    ),
    (
        "family_income_doubled_of",
        "Money",
        "only(member_of_family.family_income_doubled)",
    ),
    (
        "guarded_family_income_of",
        "Money",
        "if len(member_of_family) == 1: only(member_of_family.family_income)\nelse: 0",
    ),
];

fn module_source() -> String {
    let mut source = String::from(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n",
    );
    for (name, dtype, formula) in PERSON_RULES {
        source.push_str(&rule("Person", name, dtype, formula));
    }
    for (name, dtype, formula) in FAMILY_RULES {
        source.push_str(&rule("Family", name, dtype, formula));
    }
    source
}

fn artifact() -> &'static CompiledProgramArtifact {
    static ARTIFACT: OnceLock<CompiledProgramArtifact> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        CompiledProgramArtifact::from_rulespec_str(&module_source()).expect("module compiles")
    })
}

fn dense_program(root: &str) -> DenseCompiledProgram {
    DenseCompiledProgram::from_artifact(artifact(), Some(root)).expect("dense compiles")
}

fn dense_family() -> &'static DenseCompiledProgram {
    static PROGRAM: OnceLock<DenseCompiledProgram> = OnceLock::new();
    PROGRAM.get_or_init(|| dense_program("Family"))
}

fn dense_person() -> &'static DenseCompiledProgram {
    static PROGRAM: OnceLock<DenseCompiledProgram> = OnceLock::new();
    PROGRAM.get_or_init(|| dense_program("Person"))
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// A person's inputs. Ages of one roster share a kind (dense columns are
/// uniformly typed).
#[derive(Clone, Debug)]
struct Person {
    age: ScalarValueSpec,
    eligible: bool,
    birth_date: NaiveDate,
    /// Indexes of the families the person belongs to.
    families: Vec<usize>,
}

#[derive(Clone, Debug)]
struct Roster {
    people: Vec<Person>,
    /// Each family's `family_income`.
    family_incomes: Vec<Decimal>,
}

impl Roster {
    fn family_id(index: usize) -> String {
        format!("f{index:02}")
    }

    /// Zero-padded so that the ids' string order (explain's member order) is
    /// the batch order dense is given.
    fn person_id(index: usize) -> String {
        format!("p{index:02}")
    }

    /// Indexes of the people in `family`, in id order.
    fn members(&self, family: usize) -> Vec<usize> {
        (0..self.people.len())
            .filter(|person| self.people[*person].families.contains(&family))
            .collect()
    }
}

fn month() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: date("2026-01-01"),
        end: date("2026-01-31"),
    }
}

fn interval() -> IntervalSpec {
    IntervalSpec {
        start: month().start,
        end: month().end,
    }
}

fn date(value: &str) -> NaiveDate {
    NaiveDate::from_str(value).expect("valid date")
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal")
}

fn input(entity: &str, entity_id: String, name: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id,
        interval: interval(),
        value,
    }
}

/// The roster as a dataset: inputs person by person then family by family,
/// tuples person by person.
fn dataset(roster: &Roster) -> DatasetSpec {
    let mut inputs = Vec::new();
    let mut relations = Vec::new();
    for (index, person) in roster.people.iter().enumerate() {
        let id = Roster::person_id(index);
        inputs.push(input("Person", id.clone(), "age", person.age.clone()));
        inputs.push(input(
            "Person",
            id.clone(),
            "eligible",
            ScalarValueSpec::Bool {
                value: person.eligible,
            },
        ));
        inputs.push(input(
            "Person",
            id.clone(),
            "birth_date",
            ScalarValueSpec::Date {
                value: person.birth_date,
            },
        ));
        for family in &person.families {
            relations.push(RelationRecordSpec {
                name: RELATION.to_string(),
                tuple: vec![id.clone(), Roster::family_id(*family)],
                interval: interval(),
            });
        }
    }
    for (index, income) in roster.family_incomes.iter().enumerate() {
        inputs.push(input(
            "Family",
            Roster::family_id(index),
            "family_income",
            ScalarValueSpec::Decimal {
                value: income.to_string(),
            },
        ));
    }
    DatasetSpec { inputs, relations }
}

fn query(entity_id: String, outputs: &[&str]) -> ExecutionQuery {
    ExecutionQuery {
        entity_id,
        period: month(),
        outputs: outputs.iter().map(|output| output.to_string()).collect(),
        assessment_date: None,
    }
}

fn run(
    mode: ExecutionMode,
    dataset: DatasetSpec,
    queries: Vec<ExecutionQuery>,
) -> Result<ExecutionResponse, ApiError> {
    execute_compiled_request(
        artifact().clone(),
        CompiledExecutionRequest {
            relation_binding: Default::default(),
            mode,
            dataset,
            queries,
            pins: Vec::new(),
        },
    )
}

/// One entity's one output under explain.
fn explain_one(
    dataset: &DatasetSpec,
    entity_id: String,
    output: &str,
) -> Result<ScalarValueSpec, EvalError> {
    match run(
        ExecutionMode::Explain,
        dataset.clone(),
        vec![query(entity_id, &[output])],
    ) {
        Ok(response) => match &response.results[0].outputs[output] {
            OutputValue::Scalar { value, .. } => Ok(value.clone()),
            other => panic!("`{output}` is scalar, got {other:?}"),
        },
        Err(ApiError::Eval(error)) => Err(error),
        Err(other) => panic!("`{output}` failed outside evaluation: {other}"),
    }
}

fn numeric(value: &ScalarValueSpec) -> Decimal {
    match value {
        ScalarValueSpec::Integer { value } => Decimal::from(*value),
        ScalarValueSpec::Decimal { value } => decimal(value),
        other => panic!("expected a number, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Dense
// ---------------------------------------------------------------------------

/// A related input column over `rows` (each a person index), in that order.
fn person_columns(
    roster: &Roster,
    rows: &[usize],
    names: &[String],
) -> HashMap<String, DenseColumn> {
    let mut columns = HashMap::new();
    for name in names {
        let column = match name.as_str() {
            "age" => match roster.people.first().map(|person| &person.age) {
                Some(ScalarValueSpec::Integer { .. }) => DenseColumn::Integer(
                    rows.iter()
                        .map(|row| match roster.people[*row].age {
                            ScalarValueSpec::Integer { value } => value,
                            ref other => panic!("mixed age kinds: {other:?}"),
                        })
                        .collect(),
                ),
                _ => DenseColumn::Decimal(
                    rows.iter()
                        .map(|row| numeric(&roster.people[*row].age))
                        .collect(),
                ),
            },
            "eligible" => DenseColumn::Bool(
                rows.iter()
                    .map(|row| roster.people[*row].eligible)
                    .collect(),
            ),
            "birth_date" => DenseColumn::Date(
                rows.iter()
                    .map(|row| roster.people[*row].birth_date)
                    .collect(),
            ),
            other => panic!("no person column `{other}`"),
        };
        columns.insert(name.clone(), column);
    }
    columns
}

/// Dense results for `families` (rows, in order) of the Family program, in
/// mode Decimal or f64.
fn dense_families(
    roster: &Roster,
    families: &[usize],
    output: &str,
    f64_mode: bool,
) -> Result<DenseColumn, EvalError> {
    let program = dense_family();
    let mut relations = HashMap::new();
    for schema in program.relations() {
        let mut offsets = vec![0];
        let mut rows = Vec::new();
        for family in families {
            rows.extend(roster.members(*family));
            offsets.push(rows.len());
        }
        relations.insert(
            schema.key.clone(),
            DenseRelationBatchSpec {
                offsets,
                inputs: person_columns(roster, &rows, &schema.related_inputs),
            },
        );
    }
    let inputs = HashMap::from([(
        "family_income".to_string(),
        DenseColumn::Decimal(
            families
                .iter()
                .map(|family| roster.family_incomes[*family])
                .collect(),
        ),
    )]);
    let batch = DenseBatchSpec {
        row_count: families.len(),
        inputs,
        relations,
    };
    let period = month().to_model().expect("period");
    let outputs = vec![output.to_string()];
    let result = if f64_mode {
        program.execute_f64(&period, batch, &outputs)?
    } else {
        program.execute(&period, batch, &outputs)?
    };
    match result.outputs.get(output) {
        Some(DenseOutputValue::Scalar(column)) => Ok(column.clone()),
        other => panic!("`{output}` is a scalar output, got {other:?}"),
    }
}

/// Dense results for person `rows` of the Person program (Decimal mode).
fn dense_people(roster: &Roster, people: &[usize], output: &str) -> Result<DenseColumn, EvalError> {
    let program = dense_person();
    let mut relations = HashMap::new();
    for schema in program.relations() {
        let mut offsets = vec![0];
        let mut families = Vec::new();
        for person in people {
            let mut own = roster.people[*person].families.clone();
            own.sort_unstable();
            families.extend(own);
            offsets.push(families.len());
        }
        let mut inputs = HashMap::new();
        for name in &schema.related_inputs {
            assert_eq!(
                name, "family_income",
                "the Person program reads family income"
            );
            inputs.insert(
                name.clone(),
                DenseColumn::Decimal(
                    families
                        .iter()
                        .map(|family| roster.family_incomes[*family])
                        .collect(),
                ),
            );
        }
        relations.insert(
            schema.key.clone(),
            DenseRelationBatchSpec { offsets, inputs },
        );
    }
    let columns = person_columns(roster, people, &["age".to_string(), "eligible".to_string()]);
    let batch = DenseBatchSpec {
        row_count: people.len(),
        inputs: columns,
        relations,
    };
    let result = dense_person().execute(
        &month().to_model().expect("period"),
        batch,
        &[output.to_string()],
    )?;
    match result.outputs.get(output) {
        Some(DenseOutputValue::Scalar(column)) => Ok(column.clone()),
        other => panic!("`{output}` is a scalar output, got {other:?}"),
    }
}

/// Row `row` of a dense column as explain writes an output value: decimals
/// normalized (`ScalarValueSpec::from_model`).
fn dense_value(column: &DenseColumn, row: usize) -> ScalarValueSpec {
    match dense_raw_value(column, row) {
        ScalarValueSpec::Decimal { value } => ScalarValueSpec::Decimal {
            value: decimal(&value).normalize().to_string(),
        },
        other => other,
    }
}

/// Row `row` of a dense column exactly, decimal scale included.
fn dense_raw_value(column: &DenseColumn, row: usize) -> ScalarValueSpec {
    match column {
        DenseColumn::Integer(values) => ScalarValueSpec::Integer { value: values[row] },
        DenseColumn::Decimal(values) => ScalarValueSpec::Decimal {
            value: values[row].to_string(),
        },
        DenseColumn::Date(values) => ScalarValueSpec::Date { value: values[row] },
        DenseColumn::Bool(values) => ScalarValueSpec::Bool { value: values[row] },
        DenseColumn::Text(values) => ScalarValueSpec::Text {
            value: values[row].clone(),
        },
        DenseColumn::Float(values) => ScalarValueSpec::Decimal {
            value: Decimal::from_str(&values[row].to_string())
                .expect("finite f64")
                .to_string(),
        },
    }
}

/// `value` with its decimal normalized, for comparing an f64 result.
fn normalized(value: &ScalarValueSpec) -> ScalarValueSpec {
    match value {
        ScalarValueSpec::Decimal { value } => ScalarValueSpec::Decimal {
            value: decimal(value).normalize().to_string(),
        },
        ScalarValueSpec::Integer { value } => ScalarValueSpec::Decimal {
            value: Decimal::from(*value).normalize().to_string(),
        },
        other => other.clone(),
    }
}

/// The variant and fields of an error that every evaluator reports alike.
/// Dense has no entity ids, so it names the root entity kind where explain
/// names the id (as it does for a missing input); the id is left out here.
fn error_shape(error: &EvalError) -> String {
    match error {
        EvalError::EmptyRelatedAggregate {
            function,
            relation,
            period_start,
            period_end,
            ..
        } => format!("EmptyRelatedAggregate({function}, {relation}, {period_start}..{period_end})"),
        EvalError::NoRelatedEntity {
            relation,
            period_start,
            period_end,
            ..
        } => format!("NoRelatedEntity({relation}, {period_start}..{period_end})"),
        EvalError::AmbiguousRelatedEntity {
            relation,
            period_start,
            period_end,
            count,
            ..
        } => format!("AmbiguousRelatedEntity({relation}, {period_start}..{period_end}, {count})"),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Plain examples
// ---------------------------------------------------------------------------

fn person(age: i64, families: &[usize]) -> Person {
    Person {
        age: ScalarValueSpec::Integer { value: age },
        eligible: age < 6,
        birth_date: NaiveDate::from_ymd_opt(2026 - age as i32, 3, 1).expect("date"),
        families: families.to_vec(),
    }
}

/// Family f00: children aged 9, 2 and 5 and an adult of 35 (p04), who is
/// also in f02. Family f01: nobody. p03 is in no family.
fn example_roster() -> Roster {
    Roster {
        people: vec![
            person(9, &[0]),
            person(2, &[0]),
            person(5, &[0]),
            person(40, &[]),
            person(35, &[0, 2]),
        ],
        family_incomes: vec![decimal("52000"), decimal("61000.50"), decimal("70000")],
    }
}

/// The three children only: f00 with ages 9, 2 and 5.
fn three_children() -> Roster {
    Roster {
        people: vec![person(9, &[0]), person(2, &[0]), person(5, &[0])],
        family_incomes: vec![decimal("52000")],
    }
}

#[test]
fn a_family_of_three_children_aged_2_5_and_9_has_min_2_and_max_9() {
    let roster = three_children();
    let data = dataset(&roster);
    for (output, expected) in [("youngest_age", "2"), ("oldest_age", "9")] {
        let value = explain_one(&data, Roster::family_id(0), output).expect("explain answers");
        assert_eq!(numeric(&value), decimal(expected), "{output}");
        // Fast answers on its own path, with explain's value.
        let fast = run(
            ExecutionMode::Fast,
            data.clone(),
            vec![query(Roster::family_id(0), &[output])],
        )
        .expect("fast answers");
        assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
        match &fast.results[0].outputs[output] {
            OutputValue::Scalar { value: fast, .. } => assert_eq!(fast, &value),
            other => panic!("{other:?}"),
        }
        for f64_mode in [false, true] {
            let column = dense_families(&roster, &[0], output, f64_mode).expect("dense answers");
            assert_eq!(
                numeric(&dense_value(&column, 0)),
                decimal(expected),
                "{output}"
            );
        }
    }
    // The selected members: under five-year-olds are eligible (ages 2 and 5).
    let youngest = explain_one(&data, Roster::family_id(0), "youngest_eligible_age").unwrap();
    let oldest = explain_one(&data, Roster::family_id(0), "oldest_eligible_age").unwrap();
    assert_eq!(
        (numeric(&youngest), numeric(&oldest)),
        (decimal("2"), decimal("5"))
    );
    // Dates compare chronologically: the youngest child was born last.
    let earliest = explain_one(&data, Roster::family_id(0), "earliest_birth").unwrap();
    let latest = explain_one(&data, Roster::family_id(0), "latest_birth").unwrap();
    assert_eq!(
        (earliest, latest),
        (
            ScalarValueSpec::Date {
                value: date("2017-03-01")
            },
            ScalarValueSpec::Date {
                value: date("2024-03-01")
            }
        )
    );
}

#[test]
fn a_child_in_one_family_reads_that_familys_income() {
    let roster = three_children();
    let data = dataset(&roster);
    for child in 0..3 {
        let income = explain_one(&data, Roster::person_id(child), "family_income_of")
            .expect("a child in one family reads its income");
        assert_eq!(numeric(&income), decimal("52000"));
        let doubled = explain_one(&data, Roster::person_id(child), "family_income_doubled_of")
            .expect("a related rule is read for the family");
        assert_eq!(numeric(&doubled), decimal("104000"));
    }
    let column = dense_people(&roster, &[0, 1, 2], "family_income_of").expect("dense answers");
    for row in 0..3 {
        assert_eq!(numeric(&dense_value(&column, row)), decimal("52000"));
    }
}

#[test]
fn duplicate_relation_tuples_change_no_extreme_value_only_cardinality_or_trace() {
    for roster in [
        Roster {
            people: vec![person(5, &[0])],
            family_incomes: vec![decimal("52000")],
        },
        three_children(),
    ] {
        let data = dataset(&roster);
        let mut duplicated = data.clone();
        duplicated
            .relations
            .extend(data.relations.iter().rev().cloned());
        duplicated.relations.extend(data.relations.iter().cloned());
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            for query in [
                query(Roster::family_id(0), &["youngest_age", "oldest_age"]),
                query(Roster::family_id(0), &["only_age"]),
                query(Roster::person_id(0), &["family_income_doubled_of"]),
            ] {
                let original = run(mode.clone(), data.clone(), vec![query.clone()]);
                let repeated = run(mode.clone(), duplicated.clone(), vec![query]);
                match (original, repeated) {
                    (Ok(original), Ok(repeated)) => assert_eq!(
                        serde_json::to_value(original).unwrap(),
                        serde_json::to_value(repeated).unwrap()
                    ),
                    (Err(original), Err(repeated)) => {
                        assert!(matches!(
                            &original,
                            ApiError::Eval(EvalError::AmbiguousRelatedEntity { count: 3, .. })
                        ));
                        assert_eq!(original.to_string(), repeated.to_string());
                    }
                    (original, repeated) => {
                        panic!("duplicate tuples changed the outcome: {original:?}, {repeated:?}")
                    }
                }
            }
        }
    }
}

#[test]
fn integer_and_decimal_member_values_compare_numerically() {
    let mut roster = three_children();
    roster.people[0].age = ScalarValueSpec::Decimal {
        value: "-1.25".to_string(),
    };
    roster.people[1].age = ScalarValueSpec::Integer { value: 5 };
    roster.people[2].age = ScalarValueSpec::Decimal {
        value: "9.50".to_string(),
    };
    for person in &mut roster.people {
        person.eligible = true;
    }
    let data = dataset(&roster);
    for (output, expected) in [
        ("youngest_age", "-1.25"),
        ("oldest_age", "9.5"),
        ("youngest_eligible_age", "-1.25"),
        ("oldest_eligible_age", "9.5"),
    ] {
        let value = explain_one(&data, Roster::family_id(0), output).unwrap();
        assert_eq!(
            value,
            ScalarValueSpec::Decimal {
                value: expected.to_string()
            },
            "{output}"
        );
        let fast = run(
            ExecutionMode::Fast,
            data.clone(),
            vec![query(Roster::family_id(0), &[output])],
        )
        .unwrap();
        assert_eq!(fast.metadata.actual_mode, ExecutionMode::Fast);
        assert!(matches!(
            &fast.results[0].outputs[output],
            OutputValue::Scalar { value: actual, .. } if actual == &value
        ));
        // A dense column represents this numeric mix as decimals.
        let dense = dense_families(&roster, &[0], output, false).unwrap();
        assert_eq!(dense_value(&dense, 0), value);
    }
}

#[test]
fn a_child_in_no_family_fails_rather_than_reading_zero() {
    let roster = example_roster();
    let data = dataset(&roster);
    let error = explain_one(&data, Roster::person_id(3), "family_income_of")
        .expect_err("no family is an error, not 0");
    assert!(
        matches!(
            &error,
            EvalError::NoRelatedEntity { relation, entity_id, period_start, period_end }
                if relation == RELATION
                    && entity_id == "p03"
                    && *period_start == month().start
                    && *period_end == month().end
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "only over relation `member_of_family` found no related entity for entity `p03` over 2026-01-01..2026-01-31; it needs exactly one"
    );
    let fast = run(
        ExecutionMode::Fast,
        data.clone(),
        vec![query(Roster::person_id(3), &["family_income_of"])],
    )
    .expect_err("fast fails as explain does");
    assert_eq!(fast.to_string(), error.to_string());
    let dense = dense_people(&roster, &[3], "family_income_of").expect_err("dense fails too");
    assert_eq!(error_shape(&dense), error_shape(&error));
    // Guarded by a count, the same read never fails.
    let guarded = explain_one(&data, Roster::person_id(3), "guarded_family_income_of")
        .expect("the guarded form answers");
    assert_eq!(numeric(&guarded), Decimal::ZERO);
}

#[test]
fn a_person_in_two_families_fails_naming_the_count() {
    let roster = example_roster();
    let data = dataset(&roster);
    let error = explain_one(&data, Roster::person_id(4), "family_income_of")
        .expect_err("two families are ambiguous");
    assert!(
        matches!(
            &error,
            EvalError::AmbiguousRelatedEntity { relation, entity_id, count: 2, .. }
                if relation == RELATION && entity_id == "p04"
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "only over relation `member_of_family` found 2 related entities for entity `p04` over 2026-01-01..2026-01-31; it needs exactly one"
    );
    let dense = dense_people(&roster, &[4], "family_income_of").expect_err("dense fails too");
    assert_eq!(error_shape(&dense), error_shape(&error));
}

#[test]
fn an_empty_family_fails_min_and_max_by_name_and_the_guard_answers() {
    let roster = example_roster();
    let data = dataset(&roster);
    let family = Roster::family_id(1);
    for (output, function) in [
        ("youngest_age", "min"),
        ("oldest_age", "max"),
        ("earliest_birth", "min"),
        ("youngest_eligible_age", "min_where"),
        ("oldest_eligible_age", "max_where"),
    ] {
        let error = explain_one(&data, family.clone(), output).expect_err("empty fails");
        assert!(
            matches!(
                &error,
                EvalError::EmptyRelatedAggregate { function: named, relation, entity_id, .. }
                    if *named == function && relation == RELATION && entity_id == "f01"
            ),
            "{output}: {error:?}"
        );
    }
    let error = explain_one(&data, family.clone(), "youngest_age").unwrap_err();
    assert_eq!(
        error.to_string(),
        "min over relation `member_of_family` has no related entity for entity `f01` over 2026-01-01..2026-01-31, so it has no value; guard it with a count of the same members (for example `if len(member_of_family) > 0 then ... else ...`)"
    );
    let only = explain_one(&data, family.clone(), "only_age").expect_err("empty only fails");
    assert!(
        matches!(only, EvalError::NoRelatedEntity { .. }),
        "{only:?}"
    );
    for output in [
        "guarded_youngest",
        "guarded_oldest",
        "guarded_youngest_eligible",
        "guarded_only",
    ] {
        let value = explain_one(&data, family.clone(), output).expect("guarded forms answer");
        assert_eq!(numeric(&value), decimal("-100"), "{output}");
    }
}

#[test]
fn min_where_and_max_where_with_no_selected_member_fail_even_when_the_family_is_not_empty() {
    let mut roster = three_children();
    for person in &mut roster.people {
        person.eligible = false;
    }
    let data = dataset(&roster);
    let error = explain_one(&data, Roster::family_id(0), "youngest_eligible_age")
        .expect_err("no eligible member");
    assert!(
        matches!(
            &error,
            EvalError::EmptyRelatedAggregate {
                function: "min_where",
                ..
            }
        ),
        "{error:?}"
    );
    let value = explain_one(&data, Roster::family_id(0), "guarded_youngest_eligible")
        .expect("guarded by the same count");
    assert_eq!(numeric(&value), decimal("-100"));
    let dense = dense_families(&roster, &[0], "youngest_eligible_age", false)
        .expect_err("dense fails as explain does");
    assert_eq!(error_shape(&dense), error_shape(&error));
}

#[test]
fn a_member_value_failure_precedes_a_later_member_predicate_failure() {
    let mut roster = three_children();
    roster.people[0].eligible = true;
    let mut data = dataset(&roster);
    data.inputs.retain(|record| {
        !(record.entity_id == "p00" && record.name == "age")
            && !(record.entity_id == "p01" && record.name == "eligible")
    });
    // Deliberately reverse the tuples: members still run in sorted id order,
    // and each member's predicate runs before its value.
    data.relations.reverse();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        for output in ["youngest_eligible_age", "oldest_eligible_age"] {
            let error = run(
                mode.clone(),
                data.clone(),
                vec![query(Roster::family_id(0), &[output])],
            )
            .expect_err("the first member has no value");
            assert!(
                matches!(
                    &error,
                    ApiError::Eval(EvalError::MissingInput { name, entity_id, .. })
                        if name == "age" && entity_id == "p00"
                ),
                "{output}: {error}"
            );
        }
    }
    // An excluded member's missing value is never read. Selection can then
    // reach the later predicate, which becomes the first failure.
    data.inputs
        .iter_mut()
        .find(|record| record.entity_id == "p00" && record.name == "eligible")
        .unwrap()
        .value = ScalarValueSpec::Bool { value: false };
    for output in ["youngest_eligible_age", "oldest_eligible_age"] {
        let error = explain_one(&data, Roster::family_id(0), output).unwrap_err();
        assert!(
            matches!(
                &error,
                EvalError::MissingInput { name, entity_id, .. }
                    if name == "eligible" && entity_id == "p01"
            ),
            "{output}: {error}"
        );
    }
}

#[test]
fn only_over_a_family_with_several_members_fails_and_reads_no_value() {
    // f00 has three members; one lacks `age`. Explain reads no member's
    // value before it knows there is exactly one, so the error is the
    // ambiguity, not the missing input.
    let roster = three_children();
    let mut data = dataset(&roster);
    data.inputs
        .retain(|record| !(record.name == "age" && record.entity_id == "p01"));
    let error = explain_one(&data, Roster::family_id(0), "only_age").expect_err("ambiguous");
    assert!(
        matches!(error, EvalError::AmbiguousRelatedEntity { count: 3, .. }),
        "{error:?}"
    );
}

#[test]
fn only_returns_the_member_value_with_its_own_kind() {
    let roster = Roster {
        people: vec![person(7, &[0])],
        family_incomes: vec![decimal("1")],
    };
    let data = dataset(&roster);
    assert_eq!(
        explain_one(&data, Roster::family_id(0), "only_age").unwrap(),
        ScalarValueSpec::Integer { value: 7 }
    );
    assert_eq!(
        explain_one(&data, Roster::family_id(0), "only_birth").unwrap(),
        ScalarValueSpec::Date {
            value: date("2019-03-01")
        }
    );
    let column = dense_families(&roster, &[0], "only_birth", false).unwrap();
    assert_eq!(
        dense_value(&column, 0),
        ScalarValueSpec::Date {
            value: date("2019-03-01")
        }
    );
}

#[test]
fn only_returns_text_and_boolean_values_in_every_mode() {
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}{}",
        rule("Family", "only_name", "Text", "only(member_of_family.name)"),
        rule(
            "Family",
            "only_flag",
            "Bool",
            "only(member_of_family.eligible)"
        ),
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
    let mut data = dataset(&Roster {
        people: vec![person(5, &[0])],
        family_incomes: vec![decimal("1")],
    });
    data.inputs.retain(|record| record.name == "eligible");
    data.inputs.push(input(
        "Person",
        Roster::person_id(0),
        "name",
        ScalarValueSpec::Text {
            value: "Ana".to_string(),
        },
    ));
    let expected = [
        (
            "only_name",
            ScalarValueSpec::Text {
                value: "Ana".into(),
            },
        ),
        ("only_flag", ScalarValueSpec::Bool { value: true }),
    ];
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_compiled_request(
            artifact.clone(),
            CompiledExecutionRequest {
                relation_binding: Default::default(),
                mode: mode.clone(),
                dataset: data.clone(),
                queries: vec![query(Roster::family_id(0), &["only_name", "only_flag"])],
                pins: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(response.metadata.actual_mode, mode);
        for (output, value) in &expected {
            assert!(matches!(
                &response.results[0].outputs[*output],
                OutputValue::Scalar { value: actual, .. } if actual == value
            ));
        }
    }
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
    let relations = dense
        .relations()
        .iter()
        .map(|schema| {
            let inputs = schema
                .related_inputs
                .iter()
                .map(|name| {
                    let column = match name.as_str() {
                        "name" => DenseColumn::Text(vec!["Ana".into()]),
                        "eligible" => DenseColumn::Bool(vec![true]),
                        other => panic!("unexpected input: {other}"),
                    };
                    (name.clone(), column)
                })
                .collect();
            (
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: vec![0, 1],
                    inputs,
                },
            )
        })
        .collect();
    let response = dense
        .execute(
            &month().to_model().unwrap(),
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations,
            },
            &["only_name".into(), "only_flag".into()],
        )
        .unwrap();
    for (output, value) in &expected {
        match &response.outputs[*output] {
            DenseOutputValue::Scalar(values) => assert_eq!(dense_value(values, 0), *value),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn equal_values_of_different_scale_give_one_result_in_every_member_order() {
    // 2.5 and 2.50 are equal. Explain writes decimals normalized; dense
    // returns its column as computed, and there the larger scale wins
    // whichever member comes first, so the batch order never shows.
    for ages in [["2.50", "2.5"], ["2.5", "2.50"]] {
        let roster = Roster {
            people: ages
                .iter()
                .map(|age| Person {
                    age: ScalarValueSpec::Decimal {
                        value: age.to_string(),
                    },
                    eligible: true,
                    birth_date: date("2020-01-01"),
                    families: vec![0],
                })
                .collect(),
            family_incomes: vec![decimal("1")],
        };
        let data = dataset(&roster);
        for output in ["youngest_age", "oldest_age"] {
            let value = explain_one(&data, Roster::family_id(0), output).unwrap();
            assert_eq!(
                value,
                ScalarValueSpec::Decimal {
                    value: "2.5".to_string()
                }
            );
            let column = dense_families(&roster, &[0], output, false).unwrap();
            assert_eq!(
                dense_raw_value(&column, 0),
                ScalarValueSpec::Decimal {
                    value: "2.50".to_string()
                }
            );
            assert_eq!(dense_value(&column, 0), value);
        }
    }
    // Zero and negative zero: the non-negative one wins.
    for ages in [["-0", "0"], ["0", "-0"]] {
        let roster = Roster {
            people: ages
                .iter()
                .map(|age| Person {
                    age: ScalarValueSpec::Decimal {
                        value: age.to_string(),
                    },
                    eligible: true,
                    birth_date: date("2020-01-01"),
                    families: vec![0],
                })
                .collect(),
            family_incomes: vec![decimal("1")],
        };
        let column = dense_families(&roster, &[0], "youngest_age", false).unwrap();
        match column {
            DenseColumn::Decimal(values) => assert!(!values[0].is_sign_negative()),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_boolean_or_text_value_is_a_type_error_that_names_the_aggregate() {
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}{}",
        rule(
            "Family",
            "youngest_flag",
            "Decimal",
            "min(member_of_family.eligible)"
        ),
        rule(
            "Family",
            "latest_name",
            "Decimal",
            "max(member_of_family.name)"
        ),
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).expect("compiles");
    let mut data = dataset(&three_children());
    data.inputs.retain(|record| record.name == "eligible");
    for (index, name) in ["Ana", "Ben", "Cy"].iter().enumerate() {
        data.inputs.push(input(
            "Person",
            Roster::person_id(index),
            "name",
            ScalarValueSpec::Text {
                value: name.to_string(),
            },
        ));
    }
    for (output, expected) in [
        (
            "youngest_flag",
            "type mismatch: min over relation `member_of_family` compares numbers or dates, but a related value is a boolean",
        ),
        (
            "latest_name",
            "type mismatch: max over relation `member_of_family` compares numbers or dates, but a related value is text",
        ),
    ] {
        let error = execute_compiled_request(
            artifact.clone(),
            CompiledExecutionRequest {
                relation_binding: Default::default(),
                mode: ExecutionMode::Explain,
                dataset: data.clone(),
                queries: vec![query(Roster::family_id(0), &[output])],
                pins: Vec::new(),
            },
        )
        .expect_err("not comparable");
        assert_eq!(error.to_string(), expected);
        // Dense reports the same text.
        let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
        let mut relations = HashMap::new();
        for schema in program.relations() {
            let mut inputs = HashMap::new();
            for name in &schema.related_inputs {
                let column = match name.as_str() {
                    "eligible" => DenseColumn::Bool(vec![true, false, true]),
                    "name" => DenseColumn::Text(vec!["Ana".into(), "Ben".into(), "Cy".into()]),
                    other => panic!("{other}"),
                };
                inputs.insert(name.clone(), column);
            }
            relations.insert(
                schema.key.clone(),
                DenseRelationBatchSpec {
                    offsets: vec![0, 3],
                    inputs,
                },
            );
        }
        let dense = program
            .execute(
                &month().to_model().unwrap(),
                DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::new(),
                    relations,
                },
                &[output.to_string()],
            )
            .expect_err("dense refuses the comparison");
        assert_eq!(dense.to_string(), expected);
    }
}

#[test]
fn only_other_forms_of_min_and_max_keep_their_scalar_meaning() {
    // A relation field access is an aggregate only as the single argument.
    let lowered = |formula: &str| {
        let source = format!(
            "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}",
            rule("Family", "value", "Decimal", formula)
        );
        CompiledProgramArtifact::from_rulespec_str(&source).map(|artifact| {
            let derived = artifact
                .program
                .derived
                .iter()
                .find(|derived| derived.name == "value")
                .expect("rule")
                .clone();
            let semantics = derived
                .versions
                .first()
                .map_or(derived.semantics, |version| version.semantics.clone());
            match semantics {
                DerivedSemanticsSpec::Scalar { expr } => expr,
                other => panic!("{other:?}"),
            }
        })
    };
    assert!(matches!(
        lowered("min(member_of_family.age)").unwrap(),
        ScalarExprSpec::ExtremeRelated {
            extreme: ExtremeSpec::Min,
            ref relation,
            current_slot: 1,
            related_slot: 0,
            value: RelatedValueRefSpec::Input { .. },
            where_clause: None,
        } if relation == RELATION
    ));
    assert!(matches!(
        lowered("max(family_income, 3)").unwrap(),
        ScalarExprSpec::Max { ref items } if items.len() == 2
    ));
    assert!(matches!(
        lowered("min(family_income)").unwrap(),
        ScalarExprSpec::Min { ref items } if items.len() == 1
    ));
    // As today, a relation field access is not a scalar operand.
    let error = lowered("min(member_of_family.age, 3)").expect_err("not an aggregate");
    assert!(
        error
            .to_string()
            .contains("bare field access in scalar position not supported"),
        "{error}"
    );
    for (formula, message) in [
        (
            "only(member_of_family)",
            "only(...) requires a relation field access argument",
        ),
        ("only(member_of_family.age, 1)", "only takes 1 arg"),
        ("min_where(member_of_family, age)", "min_where takes 3 args"),
        (
            "max_where(member_of_family.age, age, eligible)",
            "max_where arg 1 must be a variable name",
        ),
    ] {
        let error = lowered(formula).expect_err(formula);
        assert!(error.to_string().contains(message), "{formula}: {error}");
    }
}

#[test]
fn same_kind_relation_direction_is_refused_for_every_new_call() {
    for formula in [
        "min(parent_of.age)",
        "max(parent_of.age)",
        "min_where(parent_of, age, eligible)",
        "max_where(parent_of, age, eligible)",
        "only(parent_of.age)",
    ] {
        let source = format!(
            "format: rulespec/v1\nrules:\n  - name: parent_of\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Person]\n{}",
            rule("Person", "value", "Decimal", formula),
        );
        let error = lower_rulespec_str(&source).expect_err("both relation directions match");
        assert!(
            matches!(
                &error,
                RuleSpecError::AmbiguousRelationDirection { relation, citing, entity, .. }
                    if relation == "parent_of" && citing == "value" && entity == "Person"
            ),
            "{formula}: {error}"
        );
    }
}

#[test]
fn the_new_aggregates_serialize_as_new_kinds_and_round_trip() {
    let artifact = artifact();
    let json = serde_json::to_string(artifact).expect("serializes");
    assert!(
        json.contains(r#""kind":"extreme_related","extreme":"min""#),
        "{json}"
    );
    assert!(json.contains(r#""kind":"extreme_related","extreme":"max""#));
    assert!(json.contains(r#""kind":"single_related""#));
    let reloaded = CompiledProgramArtifact::from_json_str(&json).expect("reloads");
    assert_eq!(
        serde_json::to_string(&reloaded).expect("serializes"),
        json,
        "the artifact round-trips byte for byte"
    );
}

/// The root node of `output`'s trace: its executed expression, the keys of
/// the rules it evaluated and of those it skipped.
fn trace_root(
    response: &ExecutionResponse,
    output: &str,
) -> (String, Vec<String>, Vec<(String, NotEvaluatedReason)>) {
    match &response.results[0].trace[output] {
        DerivedTraceNode::Scalar {
            executed_expression,
            dependencies,
            not_evaluated_dependencies,
            ..
        } => (
            executed_expression.clone(),
            dependencies.clone(),
            not_evaluated_dependencies
                .iter()
                .map(|skipped| (skipped.dependency.clone(), skipped.reason))
                .collect(),
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_trace_records_the_member_rules_an_aggregate_evaluates_and_skips_as_sum_does() {
    let roster = three_children();
    let response = run(
        ExecutionMode::Explain,
        dataset(&roster),
        vec![query(
            Roster::family_id(0),
            &["youngest_age_plus_one", "total_age_plus_one"],
        )],
    )
    .expect("explain answers");
    let (expression, minimum, _) = trace_root(&response, "youngest_age_plus_one");
    assert_eq!(
        expression,
        r#"min_related("member_of_family", 1, 0, age_plus_one)"#
    );
    let (_, total, _) = trace_root(&response, "total_age_plus_one");
    assert_eq!(minimum, total, "min records the dependencies sum records");
    let mut evaluated_on = minimum
        .iter()
        .map(|key| match &response.results[0].trace[key] {
            DerivedTraceNode::Scalar {
                name, entity_id, ..
            } => {
                assert_eq!(name, "age_plus_one");
                entity_id.clone()
            }
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    evaluated_on.sort();
    assert_eq!(evaluated_on, ["p00", "p01", "p02"]);

    // On the branch not taken, the aggregate's rule is listed as skipped,
    // exactly as for the same guard around a sum.
    let empty = Roster {
        people: Vec::new(),
        family_incomes: vec![decimal("1")],
    };
    let response = run(
        ExecutionMode::Explain,
        dataset(&empty),
        vec![query(
            Roster::family_id(0),
            &["guarded_youngest_plus_one", "guarded_total_plus_one"],
        )],
    )
    .expect("the guarded forms answer");
    let (_, _, skipped_min) = trace_root(&response, "guarded_youngest_plus_one");
    let (_, _, skipped_sum) = trace_root(&response, "guarded_total_plus_one");
    assert_eq!(skipped_min, skipped_sum);
    assert!(
        skipped_min
            .iter()
            .any(|(dependency, reason)| dependency.contains("age_plus_one")
                && *reason == NotEvaluatedReason::BranchNotSelected),
        "{skipped_min:?}"
    );

    // `only` reads its member's rule once, for the one member.
    let one = Roster {
        people: vec![person(4, &[0])],
        family_incomes: vec![decimal("10")],
    };
    let response = run(
        ExecutionMode::Explain,
        dataset(&one),
        vec![query(Roster::person_id(0), &["family_income_doubled_of"])],
    )
    .expect("explain answers");
    let (expression, dependencies, _) = trace_root(&response, "family_income_doubled_of");
    assert_eq!(
        expression,
        r#"single_related("member_of_family", 0, 1, family_income_doubled)"#
    );
    assert_eq!(dependencies.len(), 1);
}

#[test]
fn dense_refuses_a_nested_aggregate_and_lifetime_refuses_a_bare_one_by_name() {
    // An aggregate inside a related rule that dense would inline.
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}{}",
        rule(
            "Person",
            "own_family_income",
            "Money",
            "only(member_of_family.family_income)"
        ),
        rule(
            "Family",
            "total_family_income",
            "Money",
            "sum(member_of_family.own_family_income)"
        ),
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).expect("compiles");
    let error = DenseCompiledProgram::from_artifact(&artifact, Some("Family"))
        .expect_err("a nested aggregate is refused");
    assert!(
        matches!(&error, DenseCompileError::Unsupported(message)
            if message == "only over relation `member_of_family` nested inside a related expression"),
        "{error}"
    );
    // Lifetime execution reads a relation aggregate in no period, so outside
    // an over-periods reduction it refuses one by name, as it refuses `sum`.
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: member_of_family\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Family]\n{}{}{}",
        rule(
            "Family",
            "with_youngest",
            "Money",
            "sum_over_periods(family_income) + min(member_of_family.age)"
        ),
        rule(
            "Family",
            "with_oldest",
            "Money",
            "sum_over_periods(family_income) + max(member_of_family.age)"
        ),
        rule(
            "Family",
            "with_only",
            "Money",
            "sum_over_periods(family_income) + only(member_of_family.age)"
        ),
    );
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source).expect("compiles");
    let program = DenseCompiledProgram::from_artifact(&artifact, Some("Family")).unwrap();
    let period = month().to_model().unwrap();
    for (output, leaf) in [
        ("with_youngest", "min over a relation"),
        ("with_oldest", "max over a relation"),
        ("with_only", "only over a relation"),
    ] {
        let relations = program
            .relations()
            .iter()
            .map(|schema| {
                (
                    schema.key.clone(),
                    DenseRelationBatchSpec {
                        offsets: vec![0, 1],
                        inputs: HashMap::from([("age".to_string(), DenseColumn::Integer(vec![3]))]),
                    },
                )
            })
            .collect();
        let error = program
            .execute_lifetime(
                std::slice::from_ref(&period),
                vec![DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::from([(
                        "family_income".to_string(),
                        DenseColumn::Decimal(vec![decimal("10")]),
                    )]),
                    relations,
                }],
                &[output.to_string()],
            )
            .expect_err("refused");
        assert!(
            matches!(&error, EvalError::LifetimeAmbiguousLeaf(named) if named == leaf),
            "{output}: {error}"
        );
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// Integer ages, or decimal ages whose pool repeats values at other scales
/// and in both signs of zero, so ties are common.
const INT_AGES: [i64; 7] = [0, 2, 2, 5, 9, -1, 17];
const DEC_AGES: [&str; 10] = [
    "2", "2.0", "2.50", "2.5", "0", "-0", "0.00", "9", "-1.5", "17.25",
];

fn roster_strategy() -> impl Strategy<Value = Roster> {
    (1_usize..=4, any::<bool>()).prop_flat_map(|(families, integer_ages)| {
        let person = (
            0..DEC_AGES.len(),
            any::<bool>(),
            0_i64..4000,
            proptest::collection::vec(0..families, 0..=2),
        )
            .prop_map(move |(age, eligible, day, mut memberships)| {
                memberships.sort_unstable();
                memberships.dedup();
                Person {
                    age: if integer_ages {
                        ScalarValueSpec::Integer {
                            value: INT_AGES[age % INT_AGES.len()],
                        }
                    } else {
                        ScalarValueSpec::Decimal {
                            value: DEC_AGES[age].to_string(),
                        }
                    },
                    eligible,
                    birth_date: date("2010-01-01") + chrono::Days::new(day as u64),
                    families: memberships,
                }
            });
        (
            proptest::collection::vec(person, 0..=7),
            proptest::collection::vec(
                prop_oneof![Just("0"), Just("52000"), Just("61000.50"), Just("-3")],
                families,
            ),
        )
            .prop_map(|(people, incomes)| Roster {
                people,
                family_incomes: incomes.into_iter().map(decimal).collect(),
            })
    })
}

/// What the reference fold expects for one entity's output: a value, or
/// the name of the error.
#[derive(Clone, Debug, PartialEq)]
enum Expected {
    Value(ScalarValueSpec),
    Empty(&'static str),
    NoMember,
    Ambiguous(usize),
}

/// The least (or greatest) of `values`, ties broken toward the larger scale
/// and then the non-negative sign: an independent restatement of the rule
/// documented in `docs/execution-semantics.md`.
fn reference_extreme(values: &[Decimal], greatest: bool) -> Option<Decimal> {
    let target = if greatest {
        values.iter().max()
    } else {
        values.iter().min()
    }?;
    values
        .iter()
        .filter(|value| *value == target)
        .max_by_key(|value| (value.scale(), !value.is_sign_negative()))
        .copied()
}

/// Explain writes a decimal output normalized.
fn written(value: Decimal) -> ScalarValueSpec {
    ScalarValueSpec::Decimal {
        value: value.normalize().to_string(),
    }
}

fn expected_number(value: Option<Decimal>, function: &'static str) -> Expected {
    match value {
        Some(value) => Expected::Value(written(value)),
        None => Expected::Empty(function),
    }
}

fn expected_date(values: &[NaiveDate], greatest: bool, function: &'static str) -> Expected {
    let value = if greatest {
        values.iter().max()
    } else {
        values.iter().min()
    };
    match value {
        Some(value) => Expected::Value(ScalarValueSpec::Date { value: *value }),
        None => Expected::Empty(function),
    }
}

fn expected_only(values: Vec<ScalarValueSpec>) -> Expected {
    match values.len() {
        0 => Expected::NoMember,
        1 => Expected::Value(match values.into_iter().next().expect("one") {
            ScalarValueSpec::Decimal { value } => written(decimal(&value)),
            other => other,
        }),
        count => Expected::Ambiguous(count),
    }
}

/// The reference fold for every Family output the properties check.
fn family_reference(roster: &Roster, family: usize) -> BTreeMap<&'static str, Expected> {
    let members = roster.members(family);
    let ages = members
        .iter()
        .map(|person| numeric(&roster.people[*person].age))
        .collect::<Vec<_>>();
    let eligible_ages = members
        .iter()
        .filter(|person| roster.people[**person].eligible)
        .map(|person| numeric(&roster.people[*person].age))
        .collect::<Vec<_>>();
    let plus_one = ages
        .iter()
        .map(|age| age + Decimal::ONE)
        .collect::<Vec<_>>();
    let qualifying_plus_one = members
        .iter()
        .filter(|person| {
            let person = &roster.people[**person];
            person.eligible && numeric(&person.age) >= Decimal::ZERO
        })
        .map(|person| numeric(&roster.people[*person].age) + Decimal::ONE)
        .collect::<Vec<_>>();
    let births = members
        .iter()
        .map(|person| roster.people[*person].birth_date)
        .collect::<Vec<_>>();
    let guard = |expected: Expected| match expected {
        Expected::Value(value) => Expected::Value(value),
        _ => Expected::Value(written(decimal("-100"))),
    };
    let youngest = expected_number(reference_extreme(&ages, false), "min");
    let only_age = expected_only(
        members
            .iter()
            .map(|person| roster.people[*person].age.clone())
            .collect(),
    );
    BTreeMap::from([
        ("youngest_age", youngest.clone()),
        (
            "oldest_age",
            expected_number(reference_extreme(&ages, true), "max"),
        ),
        (
            "youngest_eligible_age",
            expected_number(reference_extreme(&eligible_ages, false), "min_where"),
        ),
        (
            "oldest_eligible_age",
            expected_number(reference_extreme(&eligible_ages, true), "max_where"),
        ),
        (
            "youngest_age_plus_one",
            expected_number(reference_extreme(&plus_one, false), "min"),
        ),
        (
            "oldest_qualifying_age_plus_one",
            expected_number(reference_extreme(&qualifying_plus_one, true), "max_where"),
        ),
        ("earliest_birth", expected_date(&births, false, "min")),
        ("latest_birth", expected_date(&births, true, "max")),
        ("only_age", only_age.clone()),
        (
            "only_birth",
            expected_only(
                births
                    .iter()
                    .map(|value| ScalarValueSpec::Date { value: *value })
                    .collect(),
            ),
        ),
        ("guarded_youngest", guard(youngest)),
        (
            "guarded_oldest",
            guard(expected_number(reference_extreme(&ages, true), "max")),
        ),
        (
            "guarded_youngest_eligible",
            guard(expected_number(
                reference_extreme(&eligible_ages, false),
                "min_where",
            )),
        ),
        ("guarded_only", guard(only_age)),
        (
            "guarded_youngest_plus_one",
            guard(expected_number(reference_extreme(&plus_one, false), "min")),
        ),
    ])
}

fn person_reference(roster: &Roster, person: usize) -> BTreeMap<&'static str, Expected> {
    let incomes = roster.people[person]
        .families
        .iter()
        .map(|family| roster.family_incomes[*family])
        .collect::<Vec<_>>();
    let only = |values: Vec<Decimal>| {
        expected_only(
            values
                .into_iter()
                .map(|value| ScalarValueSpec::Decimal {
                    value: value.to_string(),
                })
                .collect(),
        )
    };
    BTreeMap::from([
        ("family_income_of", only(incomes.clone())),
        (
            "family_income_doubled_of",
            only(incomes.iter().map(|income| income * Decimal::TWO).collect()),
        ),
        (
            "guarded_family_income_of",
            match only(incomes) {
                Expected::Value(value) => Expected::Value(value),
                _ => Expected::Value(ScalarValueSpec::Integer { value: 0 }),
            },
        ),
    ])
}

fn observed(result: Result<ScalarValueSpec, EvalError>, entity_id: &str) -> Expected {
    match result {
        Ok(value) => Expected::Value(value),
        Err(EvalError::EmptyRelatedAggregate {
            function,
            relation,
            entity_id: named,
            ..
        }) if relation == RELATION && named == entity_id => Expected::Empty(function),
        Err(EvalError::NoRelatedEntity {
            relation,
            entity_id: named,
            ..
        }) if relation == RELATION && named == entity_id => Expected::NoMember,
        Err(EvalError::AmbiguousRelatedEntity {
            relation,
            entity_id: named,
            count,
            ..
        }) if relation == RELATION && named == entity_id => Expected::Ambiguous(count),
        Err(other) => panic!("unexpected error for {entity_id}: {other}"),
    }
}

/// Numeric values compare exactly by value; everything else by equality.
fn same_value(left: &ScalarValueSpec, right: &ScalarValueSpec) -> bool {
    match (left, right) {
        (
            ScalarValueSpec::Integer { .. } | ScalarValueSpec::Decimal { .. },
            ScalarValueSpec::Integer { .. } | ScalarValueSpec::Decimal { .. },
        ) => numeric(left) == numeric(right),
        _ => left == right,
    }
}

fn config() -> ProptestConfig {
    let cases = std::env::var("AXIOM_RELATED_EXTREMES_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(64);
    ProptestConfig {
        cases,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config())]

    /// Invariants 1, 3, 5 and 6 under explain, entity by entity.
    #[test]
    fn explain_matches_the_reference_fold_and_its_bounds(roster in roster_strategy()) {
        let data = dataset(&roster);
        for family in 0..roster.family_incomes.len() {
            let id = Roster::family_id(family);
            let members = roster.members(family);
            for (output, expected) in family_reference(&roster, family) {
                let actual = observed(explain_one(&data, id.clone(), output), &id);
                // Exact for decimals: the representation (scale) is part of
                // the contract, not only the value.
                prop_assert_eq!(&actual, &expected, "{} of {}", output, id);
            }
            // Bounds (invariant 1), stated on the engine's own answers.
            if !members.is_empty() {
                let ages = members
                    .iter()
                    .map(|person| numeric(&roster.people[*person].age))
                    .collect::<Vec<_>>();
                let low = numeric(&explain_one(&data, id.clone(), "youngest_age").unwrap());
                let high = numeric(&explain_one(&data, id.clone(), "oldest_age").unwrap());
                prop_assert!(ages.iter().all(|age| low <= *age && *age <= high));
                prop_assert!(ages.contains(&low) && ages.contains(&high));
            }
            // Invariant 4: with one member, only == sum.
            if members.len() == 1 {
                let only = explain_one(&data, id.clone(), "only_age").unwrap();
                let sum = explain_one(&data, id.clone(), "total_age").unwrap();
                prop_assert_eq!(numeric(&only), numeric(&sum));
            }
        }
        for person in 0..roster.people.len() {
            let id = Roster::person_id(person);
            for (output, expected) in person_reference(&roster, person) {
                let actual = observed(explain_one(&data, id.clone(), output), &id);
                let agrees = match (&actual, &expected) {
                    (Expected::Value(actual), Expected::Value(expected)) => {
                        same_value(actual, expected)
                    }
                    _ => actual == expected,
                };
                prop_assert!(agrees, "{} of {}: {:?} != {:?}", output, id, actual, expected);
            }
        }
    }

    /// Invariant 5, metamorphically: `min_where(r, f, p)` over a family
    /// equals `min(r.f)` over the same family with only the members for which
    /// `p` holds.
    #[test]
    fn a_filtered_extreme_equals_the_unfiltered_one_over_the_selected_members(
        roster in roster_strategy()
    ) {
        let data = dataset(&roster);
        let mut selected = roster.clone();
        for person in &mut selected.people {
            if !person.eligible {
                person.families.clear();
            }
        }
        let selected_data = dataset(&selected);
        for family in 0..roster.family_incomes.len() {
            let id = Roster::family_id(family);
            for (filtered, plain) in [
                ("youngest_eligible_age", "youngest_age"),
                ("oldest_eligible_age", "oldest_age"),
            ] {
                let filtered = explain_one(&data, id.clone(), filtered);
                let plain = explain_one(&selected_data, id.clone(), plain);
                match (filtered, plain) {
                    (Ok(filtered), Ok(plain)) => prop_assert_eq!(filtered, plain),
                    (
                        Err(EvalError::EmptyRelatedAggregate { .. }),
                        Err(EvalError::EmptyRelatedAggregate { .. }),
                    ) => {}
                    (filtered, plain) => prop_assert!(false, "{:?} vs {:?}", filtered, plain),
                }
            }
        }
    }

    /// Invariant 2: permuting the relation tuples and the input records
    /// changes no result and no trace, in explain or fast.
    #[test]
    fn permuting_tuples_and_records_changes_no_result_and_no_trace(
        (roster, tuple_order, input_order) in roster_strategy().prop_flat_map(|roster| {
            let data = dataset(&roster);
            let tuples = (0..data.relations.len()).collect::<Vec<_>>();
            let inputs = (0..data.inputs.len()).collect::<Vec<_>>();
            (Just(roster), Just(tuples).prop_shuffle(), Just(inputs).prop_shuffle())
        })
    ) {
        let data = dataset(&roster);
        let permuted = DatasetSpec {
            inputs: input_order.iter().map(|index| data.inputs[*index].clone()).collect(),
            relations: tuple_order.iter().map(|index| data.relations[*index].clone()).collect(),
        };
        let family_outputs = FAMILY_RULES.iter().map(|(name, _, _)| *name).collect::<Vec<_>>();
        let person_outputs = PERSON_RULES
            .iter()
            .filter(|(name, _, _)| name.ends_with("_of"))
            .map(|(name, _, _)| *name)
            .collect::<Vec<_>>();
        let mut queries = Vec::new();
        for family in 0..roster.family_incomes.len() {
            for output in &family_outputs {
                queries.push(query(Roster::family_id(family), &[output]));
            }
        }
        for person in 0..roster.people.len() {
            for output in &person_outputs {
                queries.push(query(Roster::person_id(person), &[output]));
            }
        }
        for query in queries {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let original = run(mode.clone(), data.clone(), vec![query.clone()]);
                let shuffled = run(mode, permuted.clone(), vec![query.clone()]);
                match (original, shuffled) {
                    (Ok(original), Ok(shuffled)) => prop_assert_eq!(
                        serde_json::to_value(&original).unwrap(),
                        serde_json::to_value(&shuffled).unwrap()
                    ),
                    (Err(original), Err(shuffled)) => {
                        prop_assert_eq!(original.to_string(), shuffled.to_string())
                    }
                    (original, shuffled) => {
                        prop_assert!(false, "{:?} vs {:?}", original.is_ok(), shuffled.is_ok())
                    }
                }
            }
        }
    }

    /// Invariant 7: fast returns explain's answer for every family and person
    /// output (on its own path unless a date sends it to explain), and dense
    /// holds explain's value or fails with explain's error, in Decimal mode
    /// exactly and in f64 mode by value.
    #[test]
    fn explain_fast_and_dense_agree(roster in roster_strategy()) {
        let data = dataset(&roster);
        for family in 0..roster.family_incomes.len() {
            let id = Roster::family_id(family);
            for (output, _, _) in FAMILY_RULES {
                let explain = explain_one(&data, id.clone(), output);
                let fast = run(ExecutionMode::Fast, data.clone(), vec![query(id.clone(), &[output])]);
                match (&explain, &fast) {
                    (Ok(value), Ok(response)) => {
                        match &response.results[0].outputs[*output] {
                            OutputValue::Scalar { value: fast, .. } => prop_assert_eq!(fast, value),
                            other => prop_assert!(false, "{:?}", other),
                        }
                        let dates = matches!(value, ScalarValueSpec::Date { .. });
                        prop_assert_eq!(
                            response.metadata.actual_mode.clone(),
                            if dates { ExecutionMode::Explain } else { ExecutionMode::Fast },
                            "{} of {}: {:?}", output, id, response.metadata.fallback_reason
                        );
                    }
                    (Err(explain), Err(ApiError::Eval(fast))) => {
                        prop_assert_eq!(fast.to_string(), explain.to_string())
                    }
                    (explain, fast) => prop_assert!(false, "{:?} vs {:?}", explain, fast.is_ok()),
                }
                for f64_mode in [false, true] {
                    let dense = dense_families(&roster, &[family], output, f64_mode);
                    match (&explain, dense) {
                        (Ok(value), Ok(column)) => {
                            let dense = dense_value(&column, 0);
                            if f64_mode {
                                prop_assert_eq!(normalized(&dense), normalized(value), "{} of {}", output, id);
                            } else if matches!(column, DenseColumn::Integer(_))
                                || matches!(value, ScalarValueSpec::Integer { .. })
                            {
                                // Dense keeps an integer column where explain
                                // promotes (or the reverse) only through a
                                // guard's literal branch; the value is equal.
                                prop_assert!(same_value(&dense, value), "{} of {}", output, id);
                            } else {
                                prop_assert_eq!(&dense, value, "{} of {}", output, id);
                            }
                        }
                        (Err(explain), Err(dense)) => {
                            prop_assert_eq!(error_shape(&dense), error_shape(explain), "{} of {}", output, id)
                        }
                        (explain, dense) => prop_assert!(false, "{} of {}: {:?} vs {:?}", output, id, explain, dense),
                    }
                }
            }
        }
        // The whole batch at once, when every family answers.
        let families = (0..roster.family_incomes.len()).collect::<Vec<_>>();
        for (output, _, _) in FAMILY_RULES {
            let answers = families
                .iter()
                .map(|family| explain_one(&data, Roster::family_id(*family), output))
                .collect::<Vec<_>>();
            if answers.iter().all(Result::is_ok) {
                let column = dense_families(&roster, &families, output, false)
                    .expect("dense answers the batch");
                for (row, answer) in answers.iter().enumerate() {
                    prop_assert!(same_value(&dense_value(&column, row), answer.as_ref().unwrap()));
                }
            } else {
                prop_assert!(dense_families(&roster, &families, output, false).is_err());
            }
        }
        for person in 0..roster.people.len() {
            let id = Roster::person_id(person);
            for output in ["family_income_of", "guarded_family_income_of"] {
                let explain = explain_one(&data, id.clone(), output);
                match (&explain, dense_people(&roster, &[person], output)) {
                    (Ok(value), Ok(column)) => prop_assert!(same_value(&dense_value(&column, 0), value)),
                    (Err(explain), Err(dense)) => prop_assert_eq!(error_shape(&dense), error_shape(explain)),
                    (explain, dense) => prop_assert!(false, "{} of {}: {:?} vs {:?}", output, id, explain, dense),
                }
            }
        }
    }

    /// Invariant 2 for dense: the batch order of a family's members changes
    /// no min/max/only answer (ties included).
    #[test]
    fn dense_answers_do_not_depend_on_member_order(
        (roster, reversed) in roster_strategy().prop_flat_map(|roster| (Just(roster), any::<bool>()))
    ) {
        let mut reordered = roster.clone();
        // Renumbering the people moves each family's members to other
        // positions in the batch; every family keeps the same members.
        if reversed {
            reordered.people.reverse();
        } else {
            reordered.people.rotate_left(roster.people.len().min(1));
        }
        for family in 0..roster.family_incomes.len() {
            for output in ["youngest_age", "oldest_age", "earliest_birth", "latest_birth", "youngest_eligible_age", "only_age"] {
                let original = dense_families(&roster, &[family], output, false);
                let moved = dense_families(&reordered, &[family], output, false);
                match (original, moved) {
                    (Ok(original), Ok(moved)) => prop_assert_eq!(dense_raw_value(&original, 0), dense_raw_value(&moved, 0)),
                    (Err(original), Err(moved)) => prop_assert_eq!(error_shape(&original), error_shape(&moved)),
                    (original, moved) => prop_assert!(false, "{:?} vs {:?}", original, moved),
                }
            }
        }
    }
}

/// Each deferral case runs every output at four thresholds in two modes, so
/// it takes a quarter of the cases.
fn deferral_config() -> ProptestConfig {
    let base = config();
    ProptestConfig {
        cases: (base.cases / 4).max(1),
        ..base
    }
}

proptest! {
    #![proptest_config(deferral_config())]

    /// Deferring rules to the driver (`src/depth.rs`) resumes an interrupted
    /// `min`/`max`/`only` fold at the member that deferred; no threshold
    /// changes an answer, an error or a trace.
    #[test]
    fn deferral_changes_no_answer_error_or_trace(roster in roster_strategy()) {
        let data = dataset(&roster);
        let mut queries = Vec::new();
        for family in 0..roster.family_incomes.len() {
            for (output, _, _) in FAMILY_RULES {
                queries.push(query(Roster::family_id(family), &[output]));
            }
        }
        for person in 0..roster.people.len() {
            for output in ["family_income_of", "family_income_doubled_of", "guarded_family_income_of"] {
                queries.push(query(Roster::person_id(person), &[output]));
            }
        }
        let outcome = |mode: ExecutionMode, query: &ExecutionQuery| {
            match run(mode, data.clone(), vec![query.clone()]) {
                Ok(response) => Ok(serde_json::to_value(&response).expect("serializes")),
                Err(error) => Err(error.to_string()),
            }
        };
        for query in &queries {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let recursive = with_suspend_depth(usize::MAX, || outcome(mode.clone(), query));
                for threshold in [1, 2, 3] {
                    let deferred = with_suspend_depth(threshold, || outcome(mode.clone(), query));
                    prop_assert_eq!(&deferred, &recursive, "{:?} at threshold {}", query.outputs, threshold);
                }
            }
        }
    }
}

#[test]
fn the_rulespec_reference_example_answers_as_documented() {
    // The worked example in docs/rulespec.md, run as written.
    let reference = include_str!("../docs/rulespec.md");
    let section = &reference[reference
        .find("### Minimum, maximum and exactly-one over related entities")
        .expect("the section exists")..];
    let start = section
        .find("```yaml\n")
        .expect("the section has an example")
        + "```yaml\n".len();
    let end = start + section[start..].find("\n```").expect("the example ends");
    let source = format!("format: rulespec/v1\n{}\n", &section[start..end]);
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(&source).expect("the example compiles");
    let tax_year = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: date("2026-04-01"),
        end: date("2027-03-31"),
    };
    let span = IntervalSpec {
        start: tax_year.start,
        end: tax_year.end,
    };
    let record = |entity: &str, id: &str, name: &str, value: ScalarValueSpec| InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: id.to_string(),
        interval: span.clone(),
        value,
    };
    let tuple = |person: &str, family: &str| RelationRecordSpec {
        name: RELATION.to_string(),
        tuple: vec![person.to_string(), family.to_string()],
        interval: span.clone(),
    };
    // f0 has children aged 7 and 4; f1 has none; p2 is in no family and p3
    // in both.
    let dataset = DatasetSpec {
        inputs: vec![
            record("Person", "p0", "age", ScalarValueSpec::Integer { value: 7 }),
            record("Person", "p1", "age", ScalarValueSpec::Integer { value: 4 }),
            record(
                "Family",
                "f0",
                "family_income",
                ScalarValueSpec::Decimal {
                    value: "48000".to_string(),
                },
            ),
            record(
                "Family",
                "f1",
                "family_income",
                ScalarValueSpec::Decimal {
                    value: "30000".to_string(),
                },
            ),
        ],
        relations: vec![
            tuple("p0", "f0"),
            tuple("p1", "f0"),
            tuple("p3", "f0"),
            tuple("p3", "f1"),
        ],
    };
    let ask = |data: &DatasetSpec, entity_id: &str, output: &str| {
        execute_compiled_request(
            artifact.clone(),
            CompiledExecutionRequest {
                relation_binding: Default::default(),
                mode: ExecutionMode::Explain,
                dataset: data.clone(),
                queries: vec![ExecutionQuery {
                    entity_id: entity_id.to_string(),
                    period: tax_year.clone(),
                    outputs: vec![output.to_string()],
                    assessment_date: None,
                }],
                pins: Vec::new(),
            },
        )
        .map(|response| match &response.results[0].outputs[output] {
            OutputValue::Scalar { value, .. } => numeric(value),
            other => panic!("{other:?}"),
        })
    };
    // p3 has no age recorded, and `min` reads every member's, so f0's
    // youngest child is asked of a dataset holding only its children.
    let mut children_only = dataset.clone();
    children_only
        .relations
        .retain(|tuple| tuple.tuple[0] != "p3");
    assert_eq!(
        ask(&children_only, "f0", "youngest_child_age").unwrap(),
        decimal("4")
    );
    assert_eq!(
        ask(&children_only, "f1", "youngest_child_age").unwrap(),
        Decimal::ZERO
    );
    assert_eq!(
        ask(&dataset, "p0", "family_income_of_child").unwrap(),
        decimal("48000")
    );
    let none = ask(&dataset, "p2", "family_income_of_child").expect_err("no family");
    assert!(
        matches!(none, ApiError::Eval(EvalError::NoRelatedEntity { .. })),
        "{none}"
    );
    let two = ask(&dataset, "p3", "family_income_of_child").expect_err("two families");
    assert!(
        matches!(
            two,
            ApiError::Eval(EvalError::AmbiguousRelatedEntity { count: 2, .. })
        ),
        "{two}"
    );
}
