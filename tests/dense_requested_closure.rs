//! Dense compilation judges the requested program, not every rule its import
//! closure brings in (#266).
//!
//! The fixtures are synthetic. They reproduce the two shapes the issue
//! reports from `rulespec-us`: a module the requested outputs import holds an
//! unrelated rule that reads a rule of another entity directly, with no
//! relation. The rule names echo the ones the issue names; the formulas are
//! placeholders, not encodings of the law.
//!
//! * **Credit (26 USC 32 shape).** `earned_income_credit` on `TaxUnit` reads
//!   `earned_income_for_credit` from the `deduction` module it imports. That
//!   module also holds `senior_deduction_modified_adjusted_gross_income`, a
//!   `TaxUnit` rule that reads the `Person` rule
//!   `section_911_amount_excluded_from_gross_income` from a third module.
//!   Nothing the credit reaches reads it.
//! * **Payment (42 USC 1382 shape).** `individual_payment` on `Person` reads
//!   `countable_income` from the `income` module it imports. That module also
//!   holds `applicable_individual_annual_income`, a `Person` rule that reads
//!   `amount_after_subsection_a_increase`, a rule of another entity (`Couple`
//!   here). Nothing the payment reaches reads it.

use std::collections::BTreeMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    CompiledExecutionRequest, ExecutionMode, ExecutionQuery, OutputValue, execute_compiled_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::model::DType;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

const CREDIT_TARGET: &str = "us:policies/fixture/credit";
const DEDUCTION_TARGET: &str = "us:policies/fixture/deduction";
const EXCLUSION_TARGET: &str = "us:policies/fixture/exclusion";

const CREDIT: &str = r#"
format: rulespec/v1
imports:
  - us:policies/fixture/deduction
rules:
  - name: earned_income_credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income_for_credit * 0.1
"#;

const DEDUCTION: &str = r#"
format: rulespec/v1
imports:
  - us:policies/fixture/exclusion
rules:
  - name: earned_income_for_credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: wages + self_employment_income
  - name: senior_deduction_modified_adjusted_gross_income
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: adjusted_gross_income + section_911_amount_excluded_from_gross_income
"#;

const EXCLUSION: &str = r#"
format: rulespec/v1
rules:
  - name: section_911_amount_excluded_from_gross_income
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: foreign_earned_income
"#;

const PAYMENT_TARGET: &str = "us:policies/fixture/payment";
const INCOME_TARGET: &str = "us:policies/fixture/income";

const PAYMENT: &str = r#"
format: rulespec/v1
imports:
  - us:policies/fixture/income
rules:
  - name: individual_payment
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: max(0, 900 - countable_income)
"#;

const INCOME: &str = r#"
format: rulespec/v1
rules:
  - name: countable_income
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income + unearned_income
  - name: applicable_individual_annual_income
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: countable_income + amount_after_subsection_a_increase
  - name: amount_after_subsection_a_increase
    kind: derived
    entity: Couple
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: couple_income
"#;

struct Modules(BTreeMap<&'static str, &'static str>);

impl ModuleSource for Modules {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).map(|text| text.to_string()))
    }
}

fn artifact(root: &str, modules: &[(&'static str, &'static str)]) -> CompiledProgramArtifact {
    let modules = Modules(modules.iter().copied().collect());
    CompiledProgramArtifact::from_rulespec_with_source(root, &modules)
        .expect("the fixture compiles to an artifact")
}

fn credit_artifact() -> CompiledProgramArtifact {
    artifact(
        CREDIT_TARGET,
        &[
            (CREDIT_TARGET, CREDIT),
            (DEDUCTION_TARGET, DEDUCTION),
            (EXCLUSION_TARGET, EXCLUSION),
        ],
    )
}

fn payment_artifact() -> CompiledProgramArtifact {
    artifact(
        PAYMENT_TARGET,
        &[(PAYMENT_TARGET, PAYMENT), (INCOME_TARGET, INCOME)],
    )
}

/// Compile `outputs` of `entity` for dense execution.
fn compile_requested(
    artifact: &CompiledProgramArtifact,
    entity: &str,
    _outputs: &[&str],
) -> Result<DenseCompiledProgram, DenseCompileError> {
    DenseCompiledProgram::from_artifact(artifact, Some(entity))
}

fn sorted(names: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    let mut names = names.into_iter().map(Into::into).collect::<Vec<String>>();
    names.sort();
    names
}

fn tax_year() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: "2026-01-01".parse().unwrap(),
        end: "2027-01-01".parse().unwrap(),
    }
}

fn decimal_column(values: &[&str]) -> DenseColumn {
    DenseColumn::Decimal(
        values
            .iter()
            .map(|value| Decimal::from_str(value).unwrap())
            .collect(),
    )
}

/// One entity's input values, by input slot, one per row.
type Rows<'a> = &'a [(&'a str, &'a [&'a str])];

fn dense_values(dense: &DenseCompiledProgram, rows: Rows<'_>, output: &str) -> Vec<Decimal> {
    let row_count = rows[0].1.len();
    let batch = DenseBatchSpec {
        row_count,
        inputs: rows
            .iter()
            .map(|(slot, values)| (slot.to_string(), decimal_column(values)))
            .collect(),
        relations: Default::default(),
    };
    let result = dense
        .execute(
            &tax_year().to_model().unwrap(),
            batch,
            &[output.to_string()],
        )
        .expect("dense executes the requested output");
    match &result.outputs[output] {
        DenseOutputValue::Scalar(column) => (0..row_count)
            .map(|row| match column.scalar_value_at(row, &DType::Decimal) {
                axiom_rules_engine::model::ScalarValue::Decimal(value) => value,
                other => panic!("expected a decimal, got {other:?}"),
            })
            .collect(),
        other => panic!("expected a scalar column, got {other:?}"),
    }
}

/// Explain's value of `output` for each row, each row its own entity.
fn explain_values(
    artifact: &CompiledProgramArtifact,
    entity: &str,
    rows: Rows<'_>,
    output: &str,
) -> Vec<Decimal> {
    let catalog = artifact
        .metadata
        .input_catalog
        .iter()
        .map(|entry| (entry.slot.clone(), entry.canonical_request_name.clone()))
        .collect::<BTreeMap<String, String>>();
    let output_id = artifact
        .program
        .derived
        .iter()
        .find(|rule| rule.name == output)
        .and_then(|rule| rule.id.clone())
        .expect("the output has a legal id");
    let row_count = rows[0].1.len();
    let interval = IntervalSpec {
        start: tax_year().start,
        end: tax_year().end,
    };
    let mut inputs = Vec::new();
    for (slot, values) in rows {
        for (row, value) in values.iter().enumerate() {
            inputs.push(InputRecordSpec {
                name: catalog[*slot].clone(),
                entity: entity.to_string(),
                entity_id: format!("{entity}:{row}"),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: value.to_string(),
                },
            });
        }
    }
    let request = CompiledExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        dataset: DatasetSpec {
            inputs,
            relations: Vec::new(),
        },
        queries: (0..row_count)
            .map(|row| ExecutionQuery {
                entity_id: format!("{entity}:{row}"),
                period: tax_year(),
                outputs: vec![output_id.clone()],
                assessment_date: None,
            })
            .collect(),
        pins: Vec::new(),
    };
    let response = execute_compiled_request(artifact.clone(), request)
        .expect("explain evaluates the requested output");
    response
        .results
        .iter()
        .map(|result| match &result.outputs[&output_id] {
            OutputValue::Scalar {
                value: ScalarValueSpec::Decimal { value },
                ..
            } => Decimal::from_str(value).unwrap(),
            other => panic!("expected a decimal, got {other:?}"),
        })
        .collect()
}

/// The credit compiles for `TaxUnit`: the unrelated deduction that reads a
/// `Person` rule is not part of it, so it cannot refuse it. The compiled
/// program holds the credit and what it reads, and nothing else.
#[test]
fn the_credit_compiles_without_the_unrelated_rule_that_crosses_into_person() {
    let artifact = credit_artifact();
    let dense = compile_requested(&artifact, "TaxUnit", &["earned_income_credit"])
        .unwrap_or_else(|error| panic!("the credit's program compiles for TaxUnit: {error}"));
    assert_eq!(dense.root_entity(), "TaxUnit");
    assert_eq!(
        sorted(dense.output_names()),
        ["earned_income_credit", "earned_income_for_credit"]
    );
    assert_eq!(
        sorted(dense.root_inputs().iter().cloned()),
        ["self_employment_income", "wages"]
    );
    assert!(dense.relations().is_empty());

    let rows: Rows<'_> = &[
        ("wages", &["1000", "0", "250.50"]),
        ("self_employment_income", &["200", "0", "49.50"]),
    ];
    let dense_credit = dense_values(&dense, rows, "earned_income_credit");
    assert_eq!(
        dense_credit,
        explain_values(&artifact, "TaxUnit", rows, "earned_income_credit")
    );
    assert_eq!(
        dense_credit,
        ["120.0", "0.0", "30.000"]
            .map(|value| Decimal::from_str(value).unwrap())
            .to_vec()
    );
}

/// The payment compiles for `Person`: the unrelated income rule that reads a
/// `Couple` rule is not part of it.
#[test]
fn the_payment_compiles_without_the_unrelated_rule_that_crosses_into_another_entity() {
    let artifact = payment_artifact();
    let dense = compile_requested(&artifact, "Person", &["individual_payment"])
        .unwrap_or_else(|error| panic!("the payment's program compiles for Person: {error}"));
    assert_eq!(dense.root_entity(), "Person");
    assert_eq!(
        sorted(dense.output_names()),
        ["countable_income", "individual_payment"]
    );
    assert_eq!(
        sorted(dense.root_inputs().iter().cloned()),
        ["earned_income", "unearned_income"]
    );

    let rows: Rows<'_> = &[
        ("earned_income", &["100", "1000", "0"]),
        ("unearned_income", &["50", "0", "899.99"]),
    ];
    let dense_payment = dense_values(&dense, rows, "individual_payment");
    assert_eq!(
        dense_payment,
        explain_values(&artifact, "Person", rows, "individual_payment")
    );
    assert_eq!(
        dense_payment,
        ["750", "0", "0.01"]
            .map(|value| Decimal::from_str(value).unwrap())
            .to_vec()
    );
}

/// A requested output whose own closure reads another entity's rule
/// directly still refuses, naming the rule that reads it and the rule read.
#[test]
fn a_cross_entity_read_inside_the_requested_closure_still_refuses() {
    let error = compile_requested(
        &credit_artifact(),
        "TaxUnit",
        &["senior_deduction_modified_adjusted_gross_income"],
    )
    .expect_err("the deduction reads a Person rule directly");
    match &error {
        DenseCompileError::CrossEntityDependency {
            derived,
            dependency,
            entity,
            ..
        } => {
            assert_eq!(derived, "senior_deduction_modified_adjusted_gross_income");
            assert_eq!(dependency, "section_911_amount_excluded_from_gross_income");
            assert_eq!(entity, "Person");
        }
        other => panic!("expected a cross-entity refusal, got {other}"),
    }

    let error = compile_requested(
        &payment_artifact(),
        "Person",
        &["applicable_individual_annual_income"],
    )
    .expect_err("the income rule reads a Couple rule directly");
    assert!(
        matches!(
            &error,
            DenseCompileError::CrossEntityDependency { dependency, entity, .. }
                if dependency == "amount_after_subsection_a_increase" && entity == "Couple"
        ),
        "{error}"
    );
}
