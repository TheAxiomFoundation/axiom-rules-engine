use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::{
    CompileError, CompiledProgramArtifact, compile_program_file_to_json,
};
use axiom_rules_engine::rulespec::{RuleSpecError, ValidationStatus, lower_rulespec_str};
use axiom_rules_engine::spec::{
    DatasetSpec, DerivedSemanticsSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec,
    ScalarExprSpec, ScalarValueSpec, SpecError,
};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn rulespec_lowers_snap_like_formulas() {
    let rulespec = r#"
format: rulespec/v1
module:
  id: us-tx:policies/hhsc/snap/overlay-subset
  title: Texas SNAP overlay subset
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
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
            .any(|r| r.name
                == "us-tx:policies/hhsc/snap/overlay-subset#relation.member_of_household")
    );
    assert!(
        artifact
            .metadata
            .evaluation_order
            .contains(&"snap_allotment".to_string())
    );
}

#[test]
fn rulespec_source_metadata_allows_quoted_phrases() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: snap_household_food_contribution_rate
    kind: parameter
    dtype: Rate
    source: 7 USC 2017(a), "30 per centum"
    versions:
      - effective_from: 2008-10-01
        formula: "0.30"
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec compiles");

    assert_eq!(artifact.program.parameters.len(), 1);
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
        effective_to: 2026-09-30
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
    assert_eq!(
        table.versions[0].effective_to,
        Some("2026-09-30".parse().expect("valid effective_to"))
    );
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
            assessment_date: None,
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
fn repo_backed_rulespec_outputs_reject_bare_friendly_names() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/7/2017/a.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "1"
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let program = artifact.program.to_program().expect("program lowers");

    assert_eq!(
        program.resolve_derived_name("us:statutes/7/2017/a#snap_regular_month_allotment"),
        Some("snap_regular_month_allotment".to_string())
    );
    assert_eq!(
        program.resolve_derived_name("snap_regular_month_allotment"),
        None
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_backed_rulespec_execution_rejects_bare_friendly_input_names() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/7/2017/a.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };

    let error = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "snap_maximum_allotment".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Decimal {
                    value: "298".to_string(),
                },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec!["us:statutes/7/2017/a#snap_regular_month_allotment".to_string()],
        }],
    })
    .expect_err("repo-backed execution must reject bare input names");

    assert!(error.to_string().contains(
        "dataset input `snap_maximum_allotment` must use an absolute legal RuleSpec reference"
    ));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_backed_rulespec_execution_resolves_absolute_input_names() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/7/2017/a.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let output_id = "us:statutes/7/2017/a#snap_regular_month_allotment".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:statutes/7/2017/a#input.snap_maximum_allotment".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Decimal {
                    value: "298".to_string(),
                },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("absolute input reference executes");

    let OutputValue::Scalar { value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_regular_month_allotment output")
    else {
        panic!("expected scalar output");
    };
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "298");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_backed_rulespec_execution_resolves_indexed_parameter_input_names() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/policies/irs/brackets.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: income_tax_bracket_rates
    kind: parameter
    dtype: Rate
    indexed_by: bracket
    versions:
      - effective_from: 2026-01-01
        values:
          1: 0.10
  - name: first_bracket_rate
    kind: derived
    entity: TaxUnit
    dtype: Rate
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: income_tax_bracket_rates[1]
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-12-31".parse().expect("valid date"),
    };
    let output_id = "us:policies/irs/brackets#first_bracket_rate".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:policies/irs/brackets#input.bracket".to_string(),
                entity: "TaxUnit".to_string(),
                entity_id: "tax-unit-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Integer { value: 1 },
            }],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("absolute indexed parameter input reference executes");

    let OutputValue::Scalar { value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("first_bracket_rate output")
    else {
        panic!("expected scalar output");
    };
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "0.1");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_backed_rulespec_execution_resolves_absolute_upstream_output_inputs() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/7/2014/e/6/A.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: snap_net_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: max(0, snap_monthly_household_income - snap_standard_deduction)
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let output_id = "us:statutes/7/2014/e/6/A#snap_net_income".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name: "us:statutes/7/2014/e/6/A#input.snap_monthly_household_income"
                        .to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Decimal {
                        value: "1000".to_string(),
                    },
                },
                InputRecordSpec {
                    name: "us:policies/usda/snap/fy-2026-cola/deductions#snap_standard_deduction"
                        .to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Decimal {
                        value: "209".to_string(),
                    },
                },
            ],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("absolute upstream output input reference executes");

    let OutputValue::Scalar { value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_net_income output")
    else {
        panic!("expected scalar output");
    };
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "791");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_backed_rulespec_execution_resolves_absolute_relation_names() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/7/2012/j.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_household_has_elderly_or_disabled_member
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: count_where(member_of_household, snap_member_is_elderly_or_disabled) > 0
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let output_id =
        "us:statutes/7/2012/j#snap_household_has_elderly_or_disabled_member".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:statutes/7/2012/j#input.snap_member_is_elderly_or_disabled".to_string(),
                entity: "Member".to_string(),
                entity_id: "member-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Bool { value: true },
            }],
            relations: vec![axiom_rules_engine::spec::RelationRecordSpec {
                name: "us:statutes/7/2012/j#relation.member_of_household".to_string(),
                tuple: vec!["member-1".to_string(), "household-1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("absolute relation reference executes");

    let OutputValue::Judgment { outcome, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_household_has_elderly_or_disabled_member output")
    else {
        panic!("expected judgment output");
    };
    assert_eq!(
        *outcome,
        axiom_rules_engine::spec::JudgmentOutcomeSpec::Holds
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn count_where_can_use_related_derived_judgment_predicates() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/regulations/7-cfr/273/5.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_student_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: not snap_member_student_ineligible
  - name: snap_student_eligible
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: count_where(member_of_household, snap_member_student_eligible) > 0
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let output_id = "us:regulations/7-cfr/273/5#snap_student_eligible".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:regulations/7-cfr/273/5#input.snap_member_student_ineligible".to_string(),
                entity: "Member".to_string(),
                entity_id: "member-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Bool { value: false },
            }],
            relations: vec![axiom_rules_engine::spec::RelationRecordSpec {
                name: "us:regulations/7-cfr/273/5#relation.member_of_household".to_string(),
                tuple: vec!["member-1".to_string(), "household-1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("related derived predicate executes");

    let OutputValue::Judgment { outcome, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_student_eligible output")
    else {
        panic!("expected judgment output");
    };
    assert_eq!(
        *outcome,
        axiom_rules_engine::spec::JudgmentOutcomeSpec::Holds
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn composition_count_where_uses_imported_relation_for_imported_predicate() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let relation_file = root.join("rulespec-us/statutes/7/2012/j.yaml");
    let member_rules_file = root.join("rulespec-us/regulations/7-cfr/273/5.yaml");
    let program_file = root.join("rulespec-us-co/policies/snap.yaml");
    fs::create_dir_all(relation_file.parent().expect("relation file has parent"))
        .expect("create relation rules dir");
    fs::create_dir_all(
        member_rules_file
            .parent()
            .expect("member rules file has parent"),
    )
    .expect("create member rules dir");
    fs::create_dir_all(program_file.parent().expect("program file has parent"))
        .expect("create program rules dir");
    fs::write(
        &relation_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
"#,
    )
    .expect("write relation RuleSpec");
    fs::write(
        &member_rules_file,
        r#"
format: rulespec/v1
rules:
  - name: snap_member_student_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: not snap_member_student_ineligible
"#,
    )
    .expect("write member RuleSpec");
    fs::write(
        &program_file,
        r#"
format: rulespec/v1
imports:
  - us:statutes/7/2012/j
  - us:regulations/7-cfr/273/5
rules:
  - name: snap_student_eligible
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2025-10-01
        formula: count_where(member_of_household, snap_member_student_eligible) > 0
"#,
    )
    .expect("write composition RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&program_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let output_id = "us-co:policies/snap#snap_student_eligible".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:regulations/7-cfr/273/5#input.snap_member_student_ineligible".to_string(),
                entity: "Person".to_string(),
                entity_id: "person-1".to_string(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Bool { value: false },
            }],
            relations: vec![axiom_rules_engine::spec::RelationRecordSpec {
                name: "us:statutes/7/2012/j#relation.member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("composition imported relation reference executes");

    let OutputValue::Judgment { outcome, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_student_eligible output")
    else {
        panic!("expected judgment output");
    };
    assert_eq!(
        *outcome,
        axiom_rules_engine::spec::JudgmentOutcomeSpec::Holds
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn sum_where_can_sum_related_derived_scalar_values() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/26/25A.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create temp rules repo");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: education_credit_member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
  - name: aotc_first_expense_threshold
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "2000"
  - name: aotc_student_potential
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: min(qualified_tuition_and_related_expenses, aotc_first_expense_threshold)
  - name: aotc_eligible_student_claim
    kind: derived
    entity: Person
    dtype: Judgment
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: aotc_election_in_effect and not has_felony_drug_conviction
  - name: american_opportunity_credit_before_phaseout
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: |-
          sum_where(
              education_credit_member_of_tax_unit,
              aotc_student_potential,
              aotc_eligible_student_claim
          )
"#,
    )
    .expect("write temp RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&rules_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-12-31".parse().expect("valid date"),
    };
    let output_id = "us:statutes/26/25A#american_opportunity_credit_before_phaseout".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name: "us:statutes/26/25A#input.qualified_tuition_and_related_expenses"
                        .to_string(),
                    entity: "Person".to_string(),
                    entity_id: "student-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Decimal {
                        value: "3000".to_string(),
                    },
                },
                InputRecordSpec {
                    name: "us:statutes/26/25A#input.aotc_election_in_effect".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "student-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Bool { value: true },
                },
                InputRecordSpec {
                    name: "us:statutes/26/25A#input.has_felony_drug_conviction".to_string(),
                    entity: "Person".to_string(),
                    entity_id: "student-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Bool { value: false },
                },
            ],
            relations: vec![axiom_rules_engine::spec::RelationRecordSpec {
                name: "us:statutes/26/25A#relation.education_credit_member_of_tax_unit".to_string(),
                tuple: vec!["student-1".to_string(), "tax-unit-1".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("related derived scalar sum executes");

    let OutputValue::Scalar { value, .. } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("american_opportunity_credit_before_phaseout output")
    else {
        panic!("expected scalar output");
    };
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "2000");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn rulespec_namespaces_same_named_relations_by_origin_target() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let first_file = root.join("rulespec-us/statutes/26/24/h.yaml");
    let second_file = root.join("rulespec-us/statutes/26/63/c.yaml");
    let program_file = root.join("program.yaml");
    fs::create_dir_all(first_file.parent().expect("rules file has parent"))
        .expect("create first rules dir");
    fs::create_dir_all(second_file.parent().expect("rules file has parent"))
        .expect("create second rules dir");
    fs::write(
        &first_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
  - name: ctc_member_count
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_tax_unit)
"#,
    )
    .expect("write first RuleSpec");
    fs::write(
        &second_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
  - name: standard_deduction_member_count
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_tax_unit)
"#,
    )
    .expect("write second RuleSpec");
    fs::write(
        &program_file,
        r#"
format: rulespec/v1
imports:
  - us:statutes/26/24/h
  - us:statutes/26/63/c
rules: []
"#,
    )
    .expect("write program RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&program_file).expect("RuleSpec compiles");
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-12-31".parse().expect("valid date"),
    };
    let ctc_output = "us:statutes/26/24/h#ctc_member_count".to_string();
    let standard_deduction_output =
        "us:statutes/26/63/c#standard_deduction_member_count".to_string();

    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![
                axiom_rules_engine::spec::RelationRecordSpec {
                    name: "us:statutes/26/24/h#relation.member_of_tax_unit".to_string(),
                    tuple: vec!["child-1".to_string(), "tax-unit-1".to_string()],
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                },
                axiom_rules_engine::spec::RelationRecordSpec {
                    name: "us:statutes/26/63/c#relation.member_of_tax_unit".to_string(),
                    tuple: vec!["head-1".to_string(), "tax-unit-1".to_string()],
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                },
            ],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "tax-unit-1".to_string(),
            period,
            outputs: vec![ctc_output.clone(), standard_deduction_output.clone()],
        }],
    })
    .expect("same-named relation references execute");

    let OutputValue::Scalar {
        value: ScalarValueSpec::Integer { value: ctc_count },
        ..
    } = response.results[0]
        .outputs
        .get(&ctc_output)
        .expect("ctc member count output")
    else {
        panic!("expected integer scalar");
    };
    let OutputValue::Scalar {
        value: ScalarValueSpec::Integer {
            value: standard_deduction_count,
        },
        ..
    } = response.results[0]
        .outputs
        .get(&standard_deduction_output)
        .expect("standard deduction member count output")
    else {
        panic!("expected integer scalar");
    };
    assert_eq!(*ctc_count, 1);
    assert_eq!(*standard_deduction_count, 1);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn rulespec_rejects_namespaced_relation_arity_mismatch() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let rules_file = root.join("rulespec-us/statutes/26/63/c.yaml");
    fs::create_dir_all(rules_file.parent().expect("rules file has parent"))
        .expect("create rules dir");
    fs::write(
        &rules_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 1
  - name: standard_deduction_member_count
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_tax_unit)
"#,
    )
    .expect("write RuleSpec");

    let error = CompiledProgramArtifact::from_rulespec_file(&rules_file)
        .expect_err("relation arity mismatch should be rejected");
    assert!(
        error
            .to_string()
            .contains("us:statutes/26/63/c#relation.member_of_tax_unit"),
        "{error}"
    );
    assert!(
        error.to_string().contains("conflicting arities 2 and 1"),
        "{error}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn rulespec_keeps_unscoped_inferred_relation_when_import_uses_same_short_name() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("axiom-rules-engine-test-{nonce}"));
    let imported_file = root.join("rulespec-us/statutes/26/63/c.yaml");
    let program_file = root.join("program.yaml");
    fs::create_dir_all(imported_file.parent().expect("rules file has parent"))
        .expect("create rules dir");
    fs::write(
        &imported_file,
        r#"
format: rulespec/v1
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
"#,
    )
    .expect("write imported RuleSpec");
    fs::write(
        &program_file,
        r#"
format: rulespec/v1
imports:
  - us:statutes/26/63/c
rules:
  - name: local_member_count
    kind: derived
    entity: TaxUnit
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_tax_unit)
"#,
    )
    .expect("write program RuleSpec");

    let artifact =
        CompiledProgramArtifact::from_rulespec_file(&program_file).expect("RuleSpec compiles");
    assert!(
        artifact
            .program
            .relations
            .iter()
            .any(|relation| relation.name == "us:statutes/26/63/c#relation.member_of_tax_unit")
    );
    assert!(
        artifact
            .program
            .relations
            .iter()
            .any(|relation| relation.name == "member_of_tax_unit")
    );

    let _ = fs::remove_dir_all(root);
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
fn rulespec_accepts_source_relations_without_emitting_program_items() {
    let rulespec = r#"
format: rulespec/v1
module:
  summary: Colorado restates a federal SNAP maximum allotment table.
rules:
  - name: co_snap_maximum_allotment_restates_usda_fy_2026
    kind: source_relation
    source: 10 CCR 2506-1 section 4.207.3(D)
    source_relation:
      type: restates
      target: us:policies/usda/snap/fy-2026-cola#snap_maximum_allotment
      authority: federal
    verification:
      values:
        snap_maximum_allotment_table:
          1: 298
          2: 546
"#;

    let program = lower_rulespec_str(rulespec).expect("source relation compiles");
    assert!(program.parameters.is_empty());
    assert!(program.derived.is_empty());
    assert!(program.relations.is_empty());
}

#[test]
fn rulespec_rejects_source_relation_without_target() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: co_snap_maximum_allotment_restates_usda_fy_2026
    kind: source_relation
    source: 10 CCR 2506-1 section 4.207.3(D)
    source_relation:
      type: restates
      authority: federal
"#;

    let err = lower_rulespec_str(rulespec).expect_err("missing target should fail");
    assert!(matches!(
        err,
        RuleSpecError::MissingSourceRelationTarget { name }
            if name == "co_snap_maximum_allotment_restates_usda_fy_2026"
    ));
}

#[test]
fn rulespec_rejects_source_relation_without_type() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: co_snap_maximum_allotment_restates_usda_fy_2026
    kind: source_relation
    source_relation:
      target: us:policies/usda/snap/fy-2026-cola#snap_maximum_allotment
"#,
    )
    .expect_err("missing source relation type should fail");

    assert!(matches!(
        err,
        RuleSpecError::MissingSourceRelationType { name }
            if name == "co_snap_maximum_allotment_restates_usda_fy_2026"
    ));
}

#[test]
fn rulespec_rejects_source_relation_bare_target() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: co_snap_maximum_allotment_restates_usda_fy_2026
    kind: source_relation
    source_relation:
      type: restates
      target: snap_maximum_allotment
"#,
    )
    .expect_err("bare source relation target should fail");

    assert!(matches!(
        err,
        RuleSpecError::InvalidSourceRelationReference { name, field, value }
            if name == "co_snap_maximum_allotment_restates_usda_fy_2026"
                && field == "target"
                && value == "snap_maximum_allotment"
    ));
}

#[test]
fn rulespec_rejects_executable_body_on_source_relation() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: co_snap_standard_deduction_restates_usda_fy_2026
    kind: source_relation
    entity: Household
    dtype: Money
    period: Month
    source_relation:
      type: restates
      target: us:policies/usda/snap/fy-2026-cola/deductions#snap_standard_deduction
    versions:
      - effective_from: 2025-10-01
        formula: "209"
"#,
    )
    .expect_err("source relation executable body should fail");

    assert!(matches!(
        err,
        RuleSpecError::SourceRelationHasExecutableBody { name }
            if name == "co_snap_standard_deduction_restates_usda_fy_2026"
    ));
}

#[test]
fn rulespec_rejects_sets_relation_without_delegation_basis() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: co_snap_heating_cooling_sua_sets_federal_slot
    kind: source_relation
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#state_utility_allowance_amount
      value: us-co:policies/cdhs/snap/fy-2026#co_snap_heating_cooling_sua
"#,
    )
    .expect_err("sets source relation without delegation should fail");

    assert!(matches!(
        err,
        RuleSpecError::MissingSourceRelationDelegation {
            name,
            relation_type
        } if name == "co_snap_heating_cooling_sua_sets_federal_slot"
            && relation_type == "sets"
    ));
}

#[test]
fn rulespec_rejects_sets_relation_value_without_fragment() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: co_snap_heating_cooling_sua_sets_federal_slot
    kind: source_relation
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#state_utility_allowance_amount
      value: us-co:policies/cdhs/snap/fy-2026
      basis:
        delegation: us:regulations/7-cfr/273/9#state_utility_allowance_delegation
"#,
    )
    .expect_err("source relation value without fragment should fail");

    assert!(matches!(
        err,
        RuleSpecError::InvalidSourceRelationReference { name, field, value }
            if name == "co_snap_heating_cooling_sua_sets_federal_slot"
                && field == "value"
                && value == "us-co:policies/cdhs/snap/fy-2026"
    ));
}

#[test]
fn rulespec_accepts_sets_relation_with_absolute_value_and_delegation() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: co_snap_heating_cooling_sua_sets_federal_slot
    kind: source_relation
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#state_utility_allowance_amount
      value: us-co:policies/cdhs/snap/fy-2026#co_snap_heating_cooling_sua
      basis:
        delegation: us:regulations/7-cfr/273/9#state_utility_allowance_delegation
"#;

    let program = lower_rulespec_str(rulespec).expect("valid sets source relation compiles");
    assert!(program.parameters.is_empty());
    assert!(program.derived.is_empty());
    assert!(program.relations.is_empty());
}

#[test]
fn rulespec_rejects_amendment_without_operation() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: emergency_act_amends_snap_resource_limit_2026
    kind: source_relation
    source_relation:
      type: amends
      target: us:regulations/7-cfr/273/8#snap_resource_limit
      amendment:
        effective:
          start: 2026-04-01
"#,
    )
    .expect_err("amendment without operation should fail");

    assert!(matches!(
        err,
        RuleSpecError::MissingAmendmentOperation { name }
            if name == "emergency_act_amends_snap_resource_limit_2026"
    ));
}

#[test]
fn rulespec_rejects_amendment_without_effective_interval() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: emergency_act_amends_snap_resource_limit_2026
    kind: source_relation
    source_relation:
      type: amends
      target: us:regulations/7-cfr/273/8#snap_resource_limit
      amendment:
        operation: replace
"#,
    )
    .expect_err("amendment without effective interval should fail");

    assert!(matches!(
        err,
        RuleSpecError::MissingAmendmentEffective { name }
            if name == "emergency_act_amends_snap_resource_limit_2026"
    ));
}

#[test]
fn rulespec_rejects_legacy_reiteration_kind() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: legacy_restates
    kind: reiteration
    reiterates:
      target: us:policies/usda/snap/fy-2026-cola#snap_maximum_allotment
"#,
    )
    .expect_err("legacy reiteration kind should fail");

    assert!(matches!(
        err,
        RuleSpecError::UnsupportedRuleKind { name, kind }
            if name == "legacy_restates" && kind == "reiteration"
    ));
}

#[test]
fn rulespec_rejects_top_level_relations_in_rulespec() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
relations:
  - name: member_of_household
    arity: 2
"#,
    )
    .expect_err("RuleSpec relation declarations must be rule records");

    assert!(matches!(err, RuleSpecError::TopLevelRelationsUnsupported));
}

#[test]
fn rulespec_rejects_bare_relation_rule_without_kind() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    arity: 2
"#,
    )
    .expect_err("bare relation-shaped rule should fail");

    assert!(matches!(
        err,
        RuleSpecError::TopLevelArityUnsupported { name }
            if name == "member_of_household"
    ));
}

#[test]
fn rulespec_rejects_data_relation_with_top_level_arity() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    arity: 2
"#,
    )
    .expect_err("data relation arity must be nested under data_relation");

    assert!(matches!(
        err,
        RuleSpecError::TopLevelArityUnsupported { name }
            if name == "member_of_household"
    ));
}

#[test]
fn rulespec_rejects_data_relation_without_nested_arity() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation: {}
"#,
    )
    .expect_err("data relation must declare data_relation.arity");

    assert!(matches!(
        err,
        RuleSpecError::MissingDataRelationArity { name }
            if name == "member_of_household"
    ));
}

#[test]
fn rulespec_rejects_missing_rule_kind() {
    let err = lower_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: inferred_parameter
    versions:
      - effective_from: 2026-01-01
        formula: "1"
"#,
    )
    .expect_err("RuleSpec rules must declare kind explicitly");

    assert!(matches!(
        err,
        RuleSpecError::MissingRuleKind { name }
            if name == "inferred_parameter"
    ));
}

#[test]
fn rulespec_lowers_date_and_relation_judgment_formulas() {
    let rulespec = r#"
format: rulespec/v1
module:
  id: uk:statutes/ukpga/1988/50/section/21
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
fn rulespec_lowers_derived_relations_as_filtered_runtime_relations() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2025-01-01
        formula: has_ssn and not student_ineligible
  - name: eligible_member_of_household
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
    versions:
      - effective_from: 2025-01-01
        formula: member_of_household and snap_member_eligible
  - name: snap_unit_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: len(eligible_member_of_household)
"#,
    )
    .expect("derived relation RuleSpec compiles");

    let relation = artifact
        .program
        .relations
        .iter()
        .find(|relation| relation.name == "eligible_member_of_household")
        .expect("derived relation emitted");
    assert!(relation.derivation.is_some());
    let member_order = artifact
        .metadata
        .evaluation_order
        .iter()
        .position(|name| name == "snap_member_eligible")
        .expect("member predicate is ordered");
    let unit_size_order = artifact
        .metadata
        .evaluation_order
        .iter()
        .position(|name| name == "snap_unit_size")
        .expect("filtered relation consumer is ordered");
    assert!(member_order < unit_size_order);
}

#[test]
fn rulespec_rewrites_filtered_entity_member_alias_to_derived_relation() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2025-01-01
        formula: has_ssn
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2025-01-01
        formula: member_of_household and snap_member_eligible
  - name: snap_unit_size
    kind: derived
    entity: SnapUnit
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: len(members)
"#,
    )
    .expect("filtered entity RuleSpec compiles");

    let relation = artifact
        .program
        .relations
        .iter()
        .find(|relation| relation.name == "snap_unit")
        .expect("snap_unit relation emitted");
    let derivation = relation.derivation.as_ref().expect("relation is derived");
    assert_eq!(derivation.entity.as_deref(), Some("SnapUnit"));
    assert_eq!(derivation.member_relation.as_deref(), Some("members"));
    assert_eq!(derivation.slot_entities, vec!["Person", "Household"]);
    assert!(
        artifact
            .program
            .relations
            .iter()
            .all(|relation| relation.name != "members")
    );

    let snap_unit_size = artifact
        .program
        .derived
        .iter()
        .find(|derived| derived.name == "snap_unit_size")
        .expect("snap unit size emitted");
    let DerivedSemanticsSpec::Scalar { expr } = &snap_unit_size.semantics else {
        panic!("snap_unit_size should be scalar");
    };
    let ScalarExprSpec::CountRelated { relation, .. } = expr else {
        panic!("snap_unit_size should count a relation");
    };
    assert_eq!(relation, "snap_unit");
}

#[test]
fn compile_rejects_derived_relation_cycles() {
    let err = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: relation_a
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relation_b
    versions:
      - effective_from: 2025-01-01
        formula: relation_b
  - name: relation_b
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relation_a
    versions:
      - effective_from: 2025-01-01
        formula: relation_a
  - name: count_a
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2025-01-01
        formula: len(relation_a)
"#,
    )
    .expect_err("relation derivation cycle should be rejected");

    assert!(matches!(err, CompileError::CyclicRelationDependency { .. }));
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
fn duplicate_derived_rule_names_return_compile_error() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: duplicate_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "1"
  - name: duplicate_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "2"
"#;

    let err = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect_err("duplicate derived names should fail compilation");
    assert!(matches!(
        err,
        CompileError::DuplicateDerivedRule { name } if name == "duplicate_amount"
    ));
}

#[test]
fn rulespec_lowers_multi_version_derived_formula() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: savers_credit_gross_contributions
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        effective_to: 2026-12-31
        formula: qualified_retirement_contributions
      - effective_from: 2027-01-01
        formula: able_account_contributions
"#;

    let program = lower_rulespec_str(rulespec).expect("multi-version derived formulas lower");
    let derived = program
        .derived
        .iter()
        .find(|derived| derived.name == "savers_credit_gross_contributions")
        .expect("derived output present");
    assert_eq!(derived.versions.len(), 2);
    assert_eq!(
        derived.versions[0].effective_to,
        Some("2026-12-31".parse().expect("valid effective_to"))
    );
    assert!(matches!(
        &derived.versions[0].semantics,
        DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::Input { name },
        } if name == "qualified_retirement_contributions"
    ));
    assert!(matches!(
        &derived.versions[1].semantics,
        DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::Input { name },
        } if name == "able_account_contributions"
    ));
}

#[test]
fn rulespec_retains_single_bounded_derived_as_a_runtime_version() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: temporary_credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        effective_to: 2026-12-31
        formula: "100"
"#;

    let program = lower_rulespec_str(rulespec).expect("bounded derived formula lowers");
    let derived = program
        .derived
        .iter()
        .find(|derived| derived.name == "temporary_credit")
        .expect("derived output present");
    assert_eq!(derived.versions.len(), 1);
    assert_eq!(
        derived.versions[0].effective_to,
        Some("2026-12-31".parse().expect("valid effective_to"))
    );
}

#[test]
fn compile_rejects_an_effective_to_before_effective_from() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: impossible_window
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        effective_to: 2025-12-31
        formula: "1"
"#;

    let error = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect_err("an inverted effective range must fail compilation");
    assert!(matches!(
        error,
        CompileError::Spec(SpecError::InvalidEffectiveRange { rule, .. })
            if rule == "impossible_window"
    ));
}

#[test]
fn compile_program_file_to_json_accepts_rulespec_yaml() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-rulespec-yaml-test-{}",
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

#[test]
fn compile_program_file_to_json_merges_rulespec_imports() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-rulespec-import-test-{}",
        std::process::id()
    ));
    let us_path = temp_root
        .join("rulespec-us")
        .join("policies/usda/snap/fy-2026-cola/maximum-allotments.yaml");
    let co_path = temp_root
        .join("rulespec-us-co")
        .join("policies/cdhs/snap/fy-2026-benefit.yaml");
    let artifact_path = temp_root.join("benefit.compiled.json");

    std::fs::create_dir_all(us_path.parent().expect("us parent")).expect("us dir");
    std::fs::create_dir_all(co_path.parent().expect("co parent")).expect("co dir");
    std::fs::write(
        &us_path,
        r#"
format: rulespec/v1
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: snap_maximum_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment_table[household_size]
"#,
    )
    .expect("import fixture is written");
    std::fs::write(
        &co_path,
        r#"
format: rulespec/v1
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
rules:
  - name: snap_household_food_contribution_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2025-10-01
        formula: "0.30"
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: floor(snap_maximum_allotment - (net_income * snap_household_food_contribution_rate))
"#,
    )
    .expect("program fixture is written");

    let artifact = compile_program_file_to_json(&co_path, &artifact_path)
        .expect("RuleSpec file with canonical import compiles");
    assert!(
        artifact
            .program
            .parameters
            .iter()
            .any(|parameter| parameter.name == "snap_maximum_allotment_table")
    );
    assert!(
        artifact
            .program
            .parameters
            .iter()
            .any(|parameter| parameter.id.as_deref()
                == Some(
                    "us:policies/usda/snap/fy-2026-cola/maximum-allotments#snap_maximum_allotment_table"
                ))
    );
    assert!(
        artifact
            .program
            .derived
            .iter()
            .any(|derived| derived.name == "snap_regular_month_allotment")
    );
    let output_id =
        "us-co:policies/cdhs/snap/fy-2026-benefit#snap_regular_month_allotment".to_string();
    assert!(
        artifact
            .program
            .derived
            .iter()
            .any(|derived| derived.id.as_deref() == Some(output_id.as_str()))
    );

    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    };
    let response = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program,
        dataset: DatasetSpec {
            inputs: vec![
                InputRecordSpec {
                    name:
                        "us:policies/usda/snap/fy-2026-cola/maximum-allotments#input.household_size"
                            .to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Integer { value: 1 },
                },
                InputRecordSpec {
                    name: "us-co:policies/cdhs/snap/fy-2026-benefit#input.net_income".to_string(),
                    entity: "Household".to_string(),
                    entity_id: "household-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Decimal {
                        value: "100".to_string(),
                    },
                },
            ],
            relations: Vec::new(),
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: vec![output_id.clone()],
        }],
    })
    .expect("imported formula executes");

    let OutputValue::Scalar {
        name, id, value, ..
    } = response.results[0]
        .outputs
        .get(&output_id)
        .expect("snap_regular_month_allotment output")
    else {
        panic!("expected scalar output");
    };
    assert_eq!(name, "snap_regular_month_allotment");
    assert_eq!(id.as_deref(), Some(output_id.as_str()));
    let ScalarValueSpec::Decimal { value } = value else {
        panic!("expected decimal scalar");
    };
    assert_eq!(value, "268");

    std::fs::remove_dir_all(temp_root).expect("temp dir is removed");
}

#[test]
fn rulespec_module_provenance_round_trips_into_artifact() {
    let rulespec = r#"
format: rulespec/v1
module:
  id: us:statutes/7/2017/a
  title: SNAP allotment
  source_verification:
    corpus_citation_path: us/statute/7/2017/a
    corpus_citation_paths:
      - us/statute/7/2017/a
    source_sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
  encoding_provenance:
    encoder: axiom-encode/0.2.645
    model: claude-fable-5
    run_id: run-2026-06-10-001
    reviewed_by: human-reviewer
  validation:
    - oracle: policyengine-us
      status: matches
      last_run: 2026-06-09
    - oracle: snapscreener
      status: pending
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec)
        .expect("RuleSpec with provenance metadata compiles");
    assert_eq!(artifact.program.parameters.len(), 1);

    let json = serde_json::to_string(&artifact).expect("artifact serializes");
    let artifact = CompiledProgramArtifact::from_json_str(&json).expect("artifact round-trips");

    let module = artifact
        .program
        .module
        .as_ref()
        .expect("module metadata survives lowering and the artifact round trip");
    assert_eq!(module.id.as_deref(), Some("us:statutes/7/2017/a"));

    let verification = module
        .source_verification
        .as_ref()
        .expect("source verification block survives");
    assert_eq!(
        verification.corpus_citation_path.as_deref(),
        Some("us/statute/7/2017/a")
    );
    assert_eq!(
        verification.source_sha256.as_deref(),
        Some("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
    );
    assert!(
        verification.extra.contains_key("corpus_citation_paths"),
        "unmodeled source_verification subfields are preserved verbatim"
    );

    let provenance = module
        .encoding_provenance
        .as_ref()
        .expect("encoding provenance survives");
    assert_eq!(provenance.encoder.as_deref(), Some("axiom-encode/0.2.645"));
    assert_eq!(provenance.model.as_deref(), Some("claude-fable-5"));
    assert_eq!(provenance.run_id.as_deref(), Some("run-2026-06-10-001"));
    assert_eq!(provenance.reviewed_by.as_deref(), Some("human-reviewer"));

    assert_eq!(module.validation.len(), 2);
    assert_eq!(module.validation[0].oracle, "policyengine-us");
    assert_eq!(module.validation[0].status, ValidationStatus::Matches);
    assert_eq!(
        module.validation[0]
            .last_run
            .expect("last_run parses as a date")
            .to_string(),
        "2026-06-09"
    );
    assert_eq!(module.validation[1].oracle, "snapscreener");
    assert_eq!(module.validation[1].status, ValidationStatus::Pending);
    assert_eq!(module.validation[1].last_run, None);
}

#[test]
fn rulespec_artifact_omits_module_key_when_metadata_absent() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#;

    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    let json = serde_json::to_string(&artifact).expect("artifact serializes");
    assert!(
        !json.contains("\"module\""),
        "artifacts without module metadata stay byte-identical to today's shape"
    );
}

#[test]
fn rulespec_rejects_malformed_source_sha256() {
    for bad_sha in [
        "abc123",
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a0", // 63 chars
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a0g", // non-hex char
    ] {
        let rulespec = format!(
            r#"
format: rulespec/v1
module:
  id: us:statutes/7/2017/a
  source_verification:
    corpus_citation_path: us/statute/7/2017/a
    source_sha256: "{bad_sha}"
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#
        );

        let err = lower_rulespec_str(&rulespec).expect_err("malformed sha should fail");
        assert!(matches!(
            &err,
            RuleSpecError::InvalidSourceSha256 { path, value }
                if path == "us:statutes/7/2017/a" && value == bad_sha
        ));
    }
}

#[test]
fn rulespec_malformed_source_sha256_error_names_the_file() {
    let temp_root = std::env::temp_dir().join(format!(
        "axiom-rules-engine-source-sha-test-{}",
        std::process::id()
    ));
    let program_path = temp_root.join("rules.yaml");
    std::fs::create_dir_all(&temp_root).expect("temp dir is created");
    std::fs::write(
        &program_path,
        r#"
format: rulespec/v1
module:
  source_verification:
    source_sha256: not-a-digest
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

    let err = CompiledProgramArtifact::from_rulespec_file(&program_path)
        .expect_err("malformed sha should fail");
    let message = err.to_string();
    assert!(
        message.contains("rules.yaml"),
        "error should name the file, got: {message}"
    );
    assert!(
        message.contains("not-a-digest"),
        "error should echo the malformed value, got: {message}"
    );
    std::fs::remove_dir_all(temp_root).expect("temp dir is removed");
}

#[test]
fn rulespec_rejects_unknown_validation_status() {
    let rulespec = r#"
format: rulespec/v1
module:
  validation:
    - oracle: policyengine-us
      status: disputed
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#;

    let err = lower_rulespec_str(rulespec).expect_err("unknown validation status should fail");
    assert!(matches!(err, RuleSpecError::Yaml(_)));
    let message = err.to_string();
    assert!(
        message.contains("disputed"),
        "error should name the rejected status, got: {message}"
    );
    assert!(
        message.contains("pending"),
        "error should list the accepted statuses, got: {message}"
    );
}

#[test]
fn rulespec_rejects_unknown_encoding_provenance_field() {
    let rulespec = r#"
format: rulespec/v1
module:
  encoding_provenance:
    encoder: axiom-encode/0.2.645
    vibes: high
rules:
  - name: flat_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: "10"
"#;

    let err = lower_rulespec_str(rulespec).expect_err("unknown provenance field should fail");
    assert!(matches!(err, RuleSpecError::Yaml(_)));
    let message = err.to_string();
    assert!(
        message.contains("vibes"),
        "error should name the rejected field, got: {message}"
    );
}
