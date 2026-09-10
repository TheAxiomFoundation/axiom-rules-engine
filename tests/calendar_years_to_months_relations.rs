//! The new unary node must preserve current/related scope and member aliases.
use std::collections::HashMap;

use axiom_rules_engine::{
    compile::CompiledProgramArtifact,
    dense::{
        DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
        DenseRelationBatchSpec, DenseRelationKey,
    },
    spec::{PeriodKindSpec, PeriodSpec},
};
use chrono::NaiveDate;
use rust_decimal::Decimal;

#[test]
fn conversion_preserves_current_related_scopes_and_filtered_member_aliases() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        r#"format: rulespec/v1
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
  - name: family_months
    kind: derived
    entity: Family
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: calendar_years_to_months(family_years)
  - name: person_months
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: calendar_years_to_months(person_years)
  - name: fits
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: '2000-01-01'
        formula: person_months <= family_months
  - name: selected_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_family
      entity: SelectedUnit
      member_relation: members
      slot_entities: [Person, Family]
    versions:
      - effective_from: '2000-01-01'
        formula: member_of_family and fits
  - name: months
    kind: derived
    entity: SelectedUnit
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: calendar_years_to_months(len(members))
"#,
    )
    .unwrap();
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("SelectedUnit")).unwrap();
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: NaiveDate::from_ymd_opt(2001, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2001, 12, 31).unwrap(),
    }
    .to_model()
    .unwrap();
    let batch = |family_years, person_years| DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::from([("family_years".into(), family_years)]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_family".into(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2, 4],
                inputs: HashMap::from([("person_years".into(), person_years)]),
            },
        )]),
    };
    let result = dense
        .execute(
            &period,
            batch(
                DenseColumn::Decimal(vec![Decimal::from(2), Decimal::from(3)]),
                DenseColumn::Integer(vec![1, 2, 2, 4]),
            ),
            &["months".into()],
        )
        .unwrap();
    match &result.outputs["months"] {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
            assert_eq!(values, &[24, 12]);
        }
        other => panic!("expected exact Integer result, got {other:?}"),
    }
    for (family_years, person_years) in [
        (
            DenseColumn::Decimal(vec![Decimal::new(25, 1), Decimal::from(3)]),
            DenseColumn::Integer(vec![1, 2, 2, 4]),
        ),
        (
            DenseColumn::Integer(vec![2, 3]),
            DenseColumn::Float(vec![1.0, 2.0, 2.0, 4.0]),
        ),
    ] {
        let error = dense
            .execute(
                &period,
                batch(family_years, person_years),
                &["months".into()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("calendar_years_to_months"));
    }
}
