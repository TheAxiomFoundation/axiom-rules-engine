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
        formula: max(0, payment_standard - countable_income)
  - name: payment_standard
    kind: derived
    entity: Scalar
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "900"
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
    outputs: &[&str],
) -> Result<DenseCompiledProgram, DenseCompileError> {
    DenseCompiledProgram::from_artifact_for_outputs(artifact, Some(entity), &names(outputs))
}

fn names(outputs: &[&str]) -> Vec<String> {
    outputs.iter().map(|output| output.to_string()).collect()
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
    dense_batch_values(
        dense,
        DenseBatchSpec {
            row_count: rows[0].1.len(),
            inputs: rows
                .iter()
                .map(|(slot, values)| (slot.to_string(), decimal_column(values)))
                .collect(),
            relations: Default::default(),
        },
        output,
    )
}

fn dense_batch_values(
    dense: &DenseCompiledProgram,
    batch: DenseBatchSpec,
    output: &str,
) -> Vec<Decimal> {
    let row_count = batch.row_count;
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

fn interval() -> IntervalSpec {
    IntervalSpec {
        start: tax_year().start,
        end: tax_year().end,
    }
}

/// Explain's input records for `rows`, each row the entity `{entity}:{row}`.
fn input_records(
    artifact: &CompiledProgramArtifact,
    entity: &str,
    rows: Rows<'_>,
) -> Vec<InputRecordSpec> {
    let catalog = artifact
        .metadata
        .input_catalog
        .iter()
        .map(|entry| (entry.slot.clone(), entry.canonical_request_name.clone()))
        .collect::<BTreeMap<String, String>>();
    let mut inputs = Vec::new();
    for (slot, values) in rows {
        for (row, value) in values.iter().enumerate() {
            inputs.push(InputRecordSpec {
                name: catalog[*slot].clone(),
                entity: entity.to_string(),
                entity_id: format!("{entity}:{row}"),
                interval: interval(),
                value: ScalarValueSpec::Decimal {
                    value: value.to_string(),
                },
            });
        }
    }
    inputs
}

/// Explain's value of `output` for each row, each row its own entity.
fn explain_values(
    artifact: &CompiledProgramArtifact,
    entity: &str,
    rows: Rows<'_>,
    output: &str,
) -> Vec<Decimal> {
    let dataset = DatasetSpec {
        inputs: input_records(artifact, entity, rows),
        relations: Vec::new(),
    };
    explain_dataset(artifact, dataset, entity, rows[0].1.len(), output)
}

/// Explain's value of `output` for `{entity}:0` to `{entity}:{count - 1}`.
fn explain_dataset(
    artifact: &CompiledProgramArtifact,
    dataset: DatasetSpec,
    entity: &str,
    count: usize,
    output: &str,
) -> Vec<Decimal> {
    let output_id = artifact
        .program
        .derived
        .iter()
        .find(|rule| rule.name == output)
        .and_then(|rule| rule.id.clone())
        .expect("the output has a legal id");
    let request = CompiledExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        dataset,
        queries: (0..count)
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
        ["countable_income", "individual_payment", "payment_standard"]
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

/// The deduction module with the relation a refusal points to: `TaxUnit`
/// members, beside a `Household` relation the refusal must not name, and a
/// rule that counts the members. The deduction still reads the `Person` rule
/// directly.
const DEDUCTION_WITH_RELATIONS: &str = r#"
format: rulespec/v1
imports:
  - us:policies/fixture/exclusion
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, TaxUnit]
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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
  - name: tax_unit_size
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_tax_unit)
"#;

/// The deduction as the refusal advises: it reads the `Person` rule through
/// the `TaxUnit` members.
const DEDUCTION_THROUGH_RELATION: &str = r#"
format: rulespec/v1
imports:
  - us:policies/fixture/exclusion
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, TaxUnit]
  - name: senior_deduction_modified_adjusted_gross_income
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: adjusted_gross_income + sum(member_of_tax_unit.section_911_amount_excluded_from_gross_income)
"#;

fn credit_artifact_with(deduction: &'static str) -> CompiledProgramArtifact {
    artifact(
        CREDIT_TARGET,
        &[
            (CREDIT_TARGET, CREDIT),
            (DEDUCTION_TARGET, deduction),
            (EXCLUSION_TARGET, EXCLUSION),
        ],
    )
}

/// The whole-root compile still refuses the credit's program, and its error
/// says why dense refuses the read and what a caller can do instead.
#[test]
fn the_whole_root_refusal_says_the_restriction_is_deliberate_and_how_to_avoid_it() {
    let error = DenseCompiledProgram::from_artifact(&credit_artifact(), Some("TaxUnit"))
        .expect_err("the whole root holds the deduction that reads a Person rule");
    assert_eq!(
        error.to_string(),
        "dense compilation only supports dependencies within the same root entity; \
         `section_911_amount_excluded_from_gross_income` from \
         `senior_deduction_modified_adjusted_gross_income` crosses into `Person`. Dense \
         evaluates the rows of one entity and reads another entity's rule only through a \
         relation, by design: read `section_911_amount_excluded_from_gross_income` with a \
         related aggregate (`only`, `sum`, `min`, `max`, `count_where`) over a relation \
         between `TaxUnit` and `Person` (this program declares none; a `data_relation` whose \
         `arguments` name both entities is one), or compile only outputs that do not reach \
         `senior_deduction_modified_adjusted_gross_income`"
    );
    match error {
        DenseCompileError::CrossEntityDependency {
            reader_entity,
            relations,
            ..
        } => {
            assert_eq!(reader_entity, "TaxUnit");
            assert!(relations.is_empty());
        }
        other => panic!("expected a cross-entity refusal, got {other}"),
    }
}

/// The legal id of the `TaxUnit` members relation, which the lowered program
/// names it by.
const MEMBER_OF_TAX_UNIT: &str = "us:policies/fixture/deduction#relation.member_of_tax_unit";

/// When the program declares a relation between the two entities, the
/// refusal names it by its legal id, which says which module declares it,
/// and names no relation between other entities.
#[test]
fn the_refusal_names_the_relation_the_program_declares_between_the_two_entities() {
    let artifact = credit_artifact_with(DEDUCTION_WITH_RELATIONS);
    for error in [
        DenseCompiledProgram::from_artifact(&artifact, Some("TaxUnit"))
            .expect_err("the whole root holds the direct read"),
        compile_requested(
            &artifact,
            "TaxUnit",
            &["senior_deduction_modified_adjusted_gross_income"],
        )
        .expect_err("the requested deduction holds the direct read"),
    ] {
        let message = error.to_string();
        assert!(
            message.contains(&format!(
                "over a relation between `TaxUnit` and `Person`, such as `{MEMBER_OF_TAX_UNIT}`, \
                 which this program declares, or compile only outputs that do not reach \
                 `senior_deduction_modified_adjusted_gross_income`"
            )),
            "{message}"
        );
        assert!(!message.contains("member_of_household"), "{message}");
        assert!(
            matches!(
                &error,
                DenseCompileError::CrossEntityDependency { relations, .. }
                    if relations == &[MEMBER_OF_TAX_UNIT]
            ),
            "{error:?}"
        );
    }
}

/// A batch for the requested closure needs only the columns and relations
/// the closure reads.
#[test]
fn the_requested_closure_reads_only_the_relations_its_rules_read() {
    let artifact = credit_artifact_with(DEDUCTION_WITH_RELATIONS);
    let credit = compile_requested(&artifact, "TaxUnit", &["earned_income_credit"])
        .expect("the credit does not reach the direct read");
    assert!(credit.relations().is_empty());
    assert_eq!(
        sorted(credit.root_inputs().iter().cloned()),
        ["self_employment_income", "wages"]
    );

    let with_size = compile_requested(
        &artifact,
        "TaxUnit",
        &["earned_income_credit", "tax_unit_size"],
    )
    .expect("the member count does not reach the direct read either");
    assert_eq!(
        sorted(with_size.output_names()),
        [
            "earned_income_credit",
            "earned_income_for_credit",
            "tax_unit_size"
        ]
    );
    assert_eq!(
        with_size
            .relations()
            .iter()
            .map(|relation| relation.key.name.as_str())
            .collect::<Vec<_>>(),
        [MEMBER_OF_TAX_UNIT]
    );
}

/// The read the refusal advises compiles and gives explain's values.
#[test]
fn reading_the_rule_through_the_relation_the_refusal_names_compiles() {
    let artifact = credit_artifact_with(DEDUCTION_THROUGH_RELATION);
    let output = "senior_deduction_modified_adjusted_gross_income";
    let dense = compile_requested(&artifact, "TaxUnit", &[output])
        .expect("a read through the relation compiles");
    let [relation] = dense.relations() else {
        panic!("the deduction reads one relation");
    };
    assert_eq!(relation.related_inputs, ["foreign_earned_income"]);

    // Tax unit 0 has two members, tax unit 1 none.
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: [(
            "adjusted_gross_income".to_string(),
            decimal_column(&["100", "50"]),
        )]
        .into(),
        relations: [(
            relation.key.clone(),
            axiom_rules_engine::dense::DenseRelationBatchSpec {
                offsets: vec![0, 2, 2],
                inputs: [(
                    "foreign_earned_income".to_string(),
                    decimal_column(&["10", "5.25"]),
                )]
                .into(),
                ..Default::default()
            },
        )]
        .into(),
    };
    let dense_values = dense_batch_values(&dense, batch, output);
    assert_eq!(
        dense_values,
        ["115.25", "50"]
            .map(|value| Decimal::from_str(value).unwrap())
            .to_vec()
    );

    let mut inputs = input_records(
        &artifact,
        "TaxUnit",
        &[("adjusted_gross_income", &["100", "50"])],
    );
    inputs.extend(input_records(
        &artifact,
        "Person",
        &[("foreign_earned_income", &["10", "5.25"])],
    ));
    let relations = ["Person:0", "Person:1"]
        .map(|person| axiom_rules_engine::spec::RelationRecordSpec {
            name: format!("{DEDUCTION_TARGET}#relation.member_of_tax_unit"),
            tuple: vec![person.to_string(), "TaxUnit:0".to_string()],
            interval: interval(),
        })
        .to_vec();
    assert_eq!(
        dense_values,
        explain_dataset(
            &artifact,
            DatasetSpec { inputs, relations },
            "TaxUnit",
            2,
            output
        )
    );
}

/// What the requested outputs may be, and how the root entity follows from
/// them when none is named.
#[test]
fn requested_outputs_are_validated_before_anything_compiles() {
    let artifact = credit_artifact();

    let error = DenseCompiledProgram::from_artifact_for_outputs(&artifact, Some("TaxUnit"), &[])
        .expect_err("an empty request compiles nothing");
    assert!(
        matches!(error, DenseCompileError::NoRequestedOutputs),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "dense compilation needs at least one requested output"
    );

    let error = compile_requested(&artifact, "TaxUnit", &["no_such_rule"])
        .expect_err("an unknown output is refused");
    assert_eq!(
        error.to_string(),
        "dense compilation could not find requested output `no_such_rule`"
    );

    // A legal id names the rule, but dense outputs are rule names.
    let error = compile_requested(
        &artifact,
        "TaxUnit",
        &["us:policies/fixture/credit#earned_income_credit"],
    )
    .expect_err("dense outputs are rule names");
    assert_eq!(
        error.to_string(),
        "dense compilation could not find requested output \
         `us:policies/fixture/credit#earned_income_credit`; dense names outputs by rule name, \
         so request `earned_income_credit`"
    );

    let error = compile_requested(
        &artifact,
        "TaxUnit",
        &[
            "earned_income_credit",
            "section_911_amount_excluded_from_gross_income",
        ],
    )
    .expect_err("a Person rule has no TaxUnit rows");
    assert_eq!(
        error.to_string(),
        "requested output `section_911_amount_excluded_from_gross_income` is a `Person` rule, \
         but this dense program evaluates `TaxUnit` rows; compile a dense program for `Person` \
         to compute it"
    );

    let error = DenseCompiledProgram::from_artifact_for_outputs(
        &artifact,
        None,
        &names(&[
            "section_911_amount_excluded_from_gross_income",
            "earned_income_credit",
        ]),
    )
    .expect_err("no one entity holds both outputs");
    assert_eq!(
        error.to_string(),
        "the requested outputs belong to entities `Person` and `TaxUnit`, and a dense program \
         evaluates the rows of one; request outputs of one entity, or name the entity"
    );

    // With no entity named, the requested outputs name it, although the
    // program holds rules of two entities.
    let credit = DenseCompiledProgram::from_artifact_for_outputs(
        &artifact,
        None,
        &names(&["earned_income_credit", "earned_income_credit"]),
    )
    .expect("the credit's entity is TaxUnit");
    assert_eq!(credit.root_entity(), "TaxUnit");
    assert_eq!(
        sorted(credit.output_names()),
        ["earned_income_credit", "earned_income_for_credit"]
    );
    let exclusion = DenseCompiledProgram::from_artifact_for_outputs(
        &artifact,
        None,
        &names(&["section_911_amount_excluded_from_gross_income"]),
    )
    .expect("the exclusion compiles for Person");
    assert_eq!(exclusion.root_entity(), "Person");
    assert_eq!(exclusion.root_inputs(), ["foreign_earned_income"]);
}

/// An entity-free `Scalar` output compiles beside the root entity's, and
/// alone, with no entity named, as a `Scalar` program.
#[test]
fn a_scalar_output_compiles_with_the_root_entity_or_alone() {
    let artifact = payment_artifact();
    let with_person = compile_requested(&artifact, "Person", &["payment_standard"])
        .expect("a Scalar output compiles for Person rows");
    assert_eq!(with_person.root_entity(), "Person");
    assert_eq!(with_person.output_names(), ["payment_standard"]);
    assert!(with_person.root_inputs().is_empty());

    let alone = DenseCompiledProgram::from_artifact_for_outputs(
        &artifact,
        None,
        &names(&["payment_standard"]),
    )
    .expect("a Scalar output alone compiles as a Scalar program");
    assert_eq!(alone.root_entity(), "Scalar");
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: Default::default(),
        relations: Default::default(),
    };
    assert_eq!(
        dense_batch_values(&alone, batch, "payment_standard"),
        [Decimal::from(900), Decimal::from(900)]
    );

    // A Scalar program cannot read a Person rule.
    let error = DenseCompiledProgram::from_artifact_for_outputs(
        &artifact,
        Some("Scalar"),
        &names(&["individual_payment"]),
    )
    .expect_err("a Person output has no Scalar rows");
    assert!(
        matches!(error, DenseCompileError::OutputOfOtherEntity { .. }),
        "{error}"
    );
}

/// The requested closure and the whole-root compile agree wherever the
/// whole root compiles: here the `Couple` program, whose one rule both
/// compile.
#[test]
fn the_requested_closure_matches_the_whole_root_where_the_whole_root_compiles() {
    let artifact = payment_artifact();
    let whole = DenseCompiledProgram::from_artifact(&artifact, Some("Couple"))
        .expect("the Couple rule reads nothing of another entity");
    let requested = compile_requested(&artifact, "Couple", &["amount_after_subsection_a_increase"])
        .expect("the requested closure compiles");
    assert_eq!(whole.output_names(), requested.output_names());
    assert_eq!(whole.root_inputs(), requested.root_inputs());
    let rows: Rows<'_> = &[("couple_income", &["1200", "0"])];
    let output = "amount_after_subsection_a_increase";
    assert_eq!(
        dense_values(&whole, rows, output),
        dense_values(&requested, rows, output)
    );
    assert_eq!(
        dense_values(&requested, rows, output),
        explain_values(&artifact, "Couple", rows, output)
    );
}
