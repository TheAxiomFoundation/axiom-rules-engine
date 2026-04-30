use axiom_rules::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules::compile::{CompileError, CompiledProgramArtifact, compile_program_file_to_json};
use axiom_rules::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

#[test]
fn rulespec_lowers_snap_like_formulas() {
    let rulespec = r#"
format: rulespec/v1
module:
  id: us.snap.tx.demo
  title: Texas SNAP demo subset
relations:
  - name: member_of_household
    arity: 2
rules:
  - name: snap_state_sme_flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    source: "TWH / TW Bulletin 25-15 §2"
    versions:
      - effective_from: 2025-10-01
        formula: "170"
  - name: snap_medical_deduction_threshold
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2008-10-01
        formula: "35"
  - name: standard_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    sources:
      - citation: "7 CFR 273.9(c)(1)(i)"
        url: "https://www.ecfr.gov/current/title-7/section-273.9"
    versions:
      - effective_from: 2025-10-01
        formula: |
          match household_size:
              1 => 209
              2 => 209
              3 => 209
              4 => 223
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: len(member_of_household)
  - name: earned_income_total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: sum(member_of_household.earned_income)
  - name: unearned_income_total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: sum(member_of_household.unearned_income)
  - name: gross_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: earned_income_total + unearned_income_total
  - name: medical_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    source: "7 CFR 273.9(d)(3)(x) - Texas SME election"
    versions:
      - effective_from: 2025-10-01
        formula: |
          if has_elderly_or_disabled_member:
              if total_medical_expenses > snap_medical_deduction_threshold:
                  snap_state_sme_flat_amount
              else: 0
          else: 0
  - name: snap_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: max(0, gross_income - medical_deduction)
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");
    let program = &artifact.program;
    assert_eq!(program.parameters.len(), 2);
    assert_eq!(program.derived.len(), 7);
    assert!(
        program
            .relations
            .iter()
            .any(|r| r.name == "member_of_household")
    );
    assert!(
        artifact
            .metadata
            .evaluation_order
            .contains(&"snap_allotment".to_string())
    );
}

#[test]
fn rulespec_lowers_indexed_parameter_tables_and_lookup_syntax() {
    let rulespec = r#"
format: rulespec/v1
module:
  summary: |-
    The maximum monthly allotments are 298 and 546 for household sizes 1 and 2,
    plus 218 for each additional person.
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    source: USDA SNAP FY 2026 COLA maximum monthly allotment table
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: snap_maximum_allotment_additional_member
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "218"
  - name: max_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: |-
          if household_size > 2:
              snap_maximum_allotment_table[2] + ((household_size - 2) * snap_maximum_allotment_additional_member)
          else: snap_maximum_allotment_table[household_size]
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");
    let table = artifact
        .program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "snap_maximum_allotment_table")
        .expect("indexed parameter is present");
    assert_eq!(table.versions.len(), 1);
    assert_eq!(table.versions[0].values.len(), 2);
    assert_eq!(table.indexed_by.as_deref(), Some("household_size"));

    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "household_size".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Integer { value: 3 },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["max_allotment".to_string()],
        }],
    })
    .expect("indexed parameter lookup executes");

    let OutputValue::Scalar { value, .. } = response.results[0]
        .outputs
        .get("max_allotment")
        .expect("max_allotment output")
    else {
        panic!("expected scalar output");
    };
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "764");
}

#[test]
fn rulespec_rejects_parameter_values_without_indexed_by() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
"#;

    let err = lower_rulespec_str(rulespec).expect_err("missing indexed_by should fail");
    assert!(matches!(
        err,
        RuleSpecError::MissingIndexedBy { name } if name == "snap_maximum_allotment_table"
    ));
}

#[test]
fn rulespec_lowers_date_and_relation_judgment_formulas() {
    let rulespec = r#"
format: rulespec/v1
module:
  id: uk.housing.section21.demo
rules:
  - name: minimum_notice_days
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: 2015-10-01
        formula: "56"
  - name: notice_days
    kind: derived
    entity: Tenancy
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2015-10-01
        formula: days_between(notice_served_date, possession_date)
  - name: recent_council_notice_count
    kind: derived
    entity: Tenancy
    dtype: Integer
    period: Day
    versions:
      - effective_from: 2015-10-01
        formula: count_where(council_notice_of_tenancy, notice_within_relevant_period)
  - name: retaliatory_eviction_bar_applies
    kind: derived
    entity: Tenancy
    dtype: Judgment
    period: Day
    versions:
      - effective_from: 2015-10-01
        formula: recent_council_notice_count > 0
  - name: section_21_notice_valid
    kind: derived
    entity: Tenancy
    dtype: Judgment
    period: Day
    source: "Housing Act 1988 s.21"
    versions:
      - effective_from: 2015-10-01
        formula: |
          notice_days >= minimum_notice_days
          and not retaliatory_eviction_bar_applies
          and not tenancy_deposit_unprotected
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");
    let program = &artifact.program;
    assert_eq!(program.parameters.len(), 1);
    assert_eq!(program.derived.len(), 4);
    let notice = program
        .derived
        .iter()
        .find(|derived| derived.name == "section_21_notice_valid")
        .expect("notice validity output exists");
    assert_eq!(notice.entity, "Tenancy");
    assert_eq!(notice.source.as_deref(), Some("Housing Act 1988 s.21"));
}

#[test]
fn rulespec_rejects_derived_relations_until_relation_outputs_are_modelled() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: eligible_member_of_household
    kind: derived_relation
    arity: 2
    versions:
      - effective_from: 2025-01-01
        formula: member_of_household
"#,
    )
    .expect_err("derived_relation is intentionally not supported yet");

    assert!(matches!(err, RuleSpecError::UnsupportedRuleKind { .. }));
}

#[test]
fn compile_rejects_rules_yaml_without_rulespec_discriminator() {
    let err = CompiledProgramArtifact::from_rulespec_str(
        r#"
rules:
  - name: ambiguous
    formula: "1"
"#,
    )
    .expect_err("ambiguous RuleSpec-shaped YAML must be rejected");

    assert!(matches!(err, CompileError::AmbiguousRuleSpecYaml { .. }));
}

#[test]
fn compile_program_file_to_json_accepts_rulespec_yaml() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-rulespec-compile-test-{}",
        std::process::id()
    ));
    let program_path = temp_root.join("rules.yaml");
    let artifact_path = temp_root.join("rules.compiled.json");
    std::fs::create_dir_all(&temp_root).expect("temp dir is created");
    std::fs::write(
        &program_path,
        r#"
format: rulespec/v1
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#,
    )
    .expect("RuleSpec fixture is written");

    let artifact = compile_program_file_to_json(&program_path, &artifact_path)
        .expect("RuleSpec file compiles");

    assert!(
        artifact_path.exists(),
        "compiled artifact should be written"
    );
    assert_eq!(artifact.program.parameters.len(), 1);
    std::fs::remove_dir_all(temp_root).expect("temp dir is removed");
}
