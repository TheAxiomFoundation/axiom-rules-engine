use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DTypeSpec, DatasetSpec, InputRecordSpec, IntervalSpec, JudgmentOutcomeSpec, PeriodKindSpec,
    PeriodSpec, RelationRecordSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;
use serde::Deserialize;

const FLAT_TAX_PROGRAM_RULESPEC: &str = include_str!("fixtures/rulespec/other/flat_tax/rules.yaml");
const FAMILY_ALLOWANCE_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/other/family_allowance/rules.yaml");
const CHILD_BENEFIT_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/uksi/1987/1967/regulation/15/rules.yaml");
const CHILD_BENEFIT_CASES_YAML: &str =
    include_str!("fixtures/rulespec/uksi/1987/1967/regulation/15/cases.yaml");
const NOTIONAL_CAPITAL_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ssi/2021/249/regulation/71/rules.yaml");
const UC_PROGRAM_RULESPEC: &str = include_str!("fixtures/rulespec/uksi/2013/376/rules.yaml");
const UC_CASES_YAML: &str = include_str!("fixtures/rulespec/uksi/2013/376/cases.yaml");
const STATE_PENSION_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ukpga/2014/19/section/4/rules.yaml");
const STATE_PENSION_CASES_YAML: &str =
    include_str!("fixtures/rulespec/ukpga/2014/19/section/4/cases.yaml");
const CT_MARGINAL_RELIEF_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ukpga/2010/4/section/18B/rules.yaml");
const CT_MARGINAL_RELIEF_CASES_YAML: &str =
    include_str!("fixtures/rulespec/ukpga/2010/4/section/18B/cases.yaml");
const ATED_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ukpga/2013/29/section/99/rules.yaml");
const ATED_CASES_YAML: &str = include_str!("fixtures/rulespec/ukpga/2013/29/section/99/cases.yaml");
const AUTO_ENROLMENT_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ukpga/2008/30/section/3/rules.yaml");
const AUTO_ENROLMENT_CASES_YAML: &str =
    include_str!("fixtures/rulespec/ukpga/2008/30/section/3/cases.yaml");
const CHILD_BENEFIT_RATES_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/uksi/2006/965/regulation/2/rules.yaml");
const CHILD_BENEFIT_RATES_CASES_YAML: &str =
    include_str!("fixtures/rulespec/uksi/2006/965/regulation/2/cases.yaml");
const SCOTTISH_CTR_MAX_PROGRAM_RULESPEC: &str =
    include_str!("fixtures/rulespec/ssi/2021/249/regulation/79/rules.yaml");
const SCOTTISH_CTR_MAX_CASES_YAML: &str =
    include_str!("fixtures/rulespec/ssi/2021/249/regulation/79/cases.yaml");

#[test]
fn dense_flat_tax_matches_explain_mode() {
    let period = month_period();
    let artifact = CompiledProgramArtifact::from_rulespec_str(FLAT_TAX_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("dense compilation succeeds");

    let people = [
        ("person-1", decimal("800")),
        ("person-2", decimal("1500")),
        ("person-3", decimal("4000")),
    ];

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: people
                .iter()
                .map(|(person_id, income)| InputRecordSpec {
                    name: "income".to_string(),
                    entity: "Person".to_string(),
                    entity_id: (*person_id).to_string(),
                    interval: period_interval(&period),
                    value: ScalarValueSpec::Decimal {
                        value: income.normalize().to_string(),
                    },
                })
                .collect(),
            relations: Vec::new(),
        },
        queries: people
            .iter()
            .map(|(person_id, _)| ExecutionQuery {
                entity_id: (*person_id).to_string(),
                period: period.clone(),
                outputs: vec![
                    "gross_income".to_string(),
                    "taxable_income".to_string(),
                    "high_income".to_string(),
                    "income_tax".to_string(),
                    "net_income".to_string(),
                ],
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: people.len(),
                inputs: HashMap::from([(
                    "income".to_string(),
                    DenseColumn::Decimal(people.iter().map(|(_, income)| *income).collect()),
                )]),
                relations: HashMap::new(),
            },
            &[
                "gross_income".to_string(),
                "taxable_income".to_string(),
                "high_income".to_string(),
                "income_tax".to_string(),
                "net_income".to_string(),
            ],
        )
        .expect("dense execution succeeds");

    for row in 0..people.len() {
        compare_scalar(
            explain.results[row]
                .outputs
                .get("gross_income")
                .expect("gross income output"),
            dense_result
                .outputs
                .get("gross_income")
                .expect("dense gross income"),
            row,
        );
        compare_scalar(
            explain.results[row]
                .outputs
                .get("taxable_income")
                .expect("taxable income output"),
            dense_result
                .outputs
                .get("taxable_income")
                .expect("dense taxable income"),
            row,
        );
        compare_judgment(
            explain.results[row]
                .outputs
                .get("high_income")
                .expect("high income output"),
            dense_result
                .outputs
                .get("high_income")
                .expect("dense high income"),
            row,
        );
        compare_scalar(
            explain.results[row]
                .outputs
                .get("income_tax")
                .expect("income tax output"),
            dense_result
                .outputs
                .get("income_tax")
                .expect("dense income tax"),
            row,
        );
        compare_scalar(
            explain.results[row]
                .outputs
                .get("net_income")
                .expect("net income output"),
            dense_result
                .outputs
                .get("net_income")
                .expect("dense net income"),
            row,
        );
    }
}

#[test]
fn dense_family_allowance_matches_explain_mode() {
    let period = month_period();
    let artifact = CompiledProgramArtifact::from_rulespec_str(FAMILY_ALLOWANCE_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .expect("dense compilation succeeds");

    let households = [
        ("household-1", vec![("person-1", decimal("1200"))]),
        (
            "household-2",
            vec![("person-2", decimal("900")), ("person-3", decimal("700"))],
        ),
        (
            "household-3",
            vec![
                ("person-4", decimal("1800")),
                ("person-5", decimal("1600")),
                ("person-6", decimal("1500")),
            ],
        ),
    ];

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: family_allowance_dataset(&period, &households),
        queries: households
            .iter()
            .map(|(household_id, _)| ExecutionQuery {
                entity_id: (*household_id).to_string(),
                period: period.clone(),
                outputs: vec![
                    "household_size".to_string(),
                    "earned_income_total".to_string(),
                    "qualifies".to_string(),
                    "monthly_allowance".to_string(),
                ],
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut offsets = vec![0_usize];
    let mut earned_income = Vec::new();
    for (_, members) in &households {
        for (_, income) in members {
            earned_income.push(*income);
        }
        offsets.push(earned_income.len());
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: households.len(),
                inputs: HashMap::new(),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "member_of_household".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets,
                        inputs: HashMap::from([(
                            "earned_income".to_string(),
                            DenseColumn::Decimal(earned_income),
                        )]),
                    },
                )]),
            },
            &[
                "household_size".to_string(),
                "earned_income_total".to_string(),
                "qualifies".to_string(),
                "monthly_allowance".to_string(),
            ],
        )
        .expect("dense execution succeeds");

    for row in 0..households.len() {
        compare_scalar(
            explain.results[row]
                .outputs
                .get("household_size")
                .expect("household size output"),
            dense_result
                .outputs
                .get("household_size")
                .expect("dense household size"),
            row,
        );
        compare_scalar(
            explain.results[row]
                .outputs
                .get("earned_income_total")
                .expect("earned income total output"),
            dense_result
                .outputs
                .get("earned_income_total")
                .expect("dense earned income total"),
            row,
        );
        compare_judgment(
            explain.results[row]
                .outputs
                .get("qualifies")
                .expect("qualifies output"),
            dense_result
                .outputs
                .get("qualifies")
                .expect("dense qualifies"),
            row,
        );
        compare_scalar(
            explain.results[row]
                .outputs
                .get("monthly_allowance")
                .expect("monthly allowance output"),
            dense_result
                .outputs
                .get("monthly_allowance")
                .expect("dense monthly allowance"),
            row,
        );
    }
}

#[test]
fn dense_child_benefit_responsibility_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(CHILD_BENEFIT_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Child"))
        .expect("dense compilation succeeds");
    let case_file: ChildBenefitCaseFile =
        serde_yaml::from_str(CHILD_BENEFIT_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "cb_recipient_count".to_string(),
        "has_cb_recipient".to_string(),
        "needs_fallback".to_string(),
        "sole_claim_fallback".to_string(),
        "usual_residence_fallback".to_string(),
        "responsible_person".to_string(),
    ];

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: child_benefit_dataset(&case_file.cases),
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.child_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            child_benefit_dense_batch(&case_file.cases),
            &outputs,
        )
        .expect("dense execution succeeds");

    for (row, case) in case_file.cases.iter().enumerate() {
        compare_scalar(
            explain.results[row]
                .outputs
                .get("cb_recipient_count")
                .expect("cb recipient count output"),
            dense_result
                .outputs
                .get("cb_recipient_count")
                .expect("dense cb recipient count"),
            row,
        );
        for judgment in [
            "has_cb_recipient",
            "needs_fallback",
            "sole_claim_fallback",
            "usual_residence_fallback",
        ] {
            compare_judgment(
                explain.results[row]
                    .outputs
                    .get(judgment)
                    .unwrap_or_else(|| panic!("{judgment} output")),
                dense_result
                    .outputs
                    .get(judgment)
                    .unwrap_or_else(|| panic!("dense {judgment}")),
                row,
            );
        }
        compare_scalar(
            explain.results[row]
                .outputs
                .get("responsible_person")
                .expect("responsible person output"),
            dense_result
                .outputs
                .get("responsible_person")
                .expect("dense responsible person"),
            row,
        );
        let _ = case; // silence unused binding when asserts match
    }
}

#[test]
fn dense_scottish_ctr_max_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(SCOTTISH_CTR_MAX_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Dwelling"))
        .expect("dense compilation succeeds");
    let case_file: ScottishCtrCaseFile =
        serde_yaml::from_str(SCOTTISH_CTR_MAX_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "days_in_fy".to_string(),
        "num_non_student_liable".to_string(),
        "a_raw".to_string(),
        "liable_divisor".to_string(),
        "a_effective".to_string(),
        "is_band_e_to_h".to_string(),
        "a_after_taper".to_string(),
        "daily_max_reduction".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "ct_annual".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.ct_annual.clone(),
                },
            },
            InputRecordSpec {
                name: "ct_discounts".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.ct_discounts.clone(),
                },
            },
            InputRecordSpec {
                name: "ct_other_reductions".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.ct_other_reductions.clone(),
                },
            },
            InputRecordSpec {
                name: "band_number".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.band_number,
                },
            },
            InputRecordSpec {
                name: "non_dep_deductions_daily".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.non_dep_deductions_daily.clone(),
                },
            },
            InputRecordSpec {
                name: "partner_only_joint".to_string(),
                entity: "Dwelling".to_string(),
                entity_id: case.dwelling_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.partner_only_joint,
                },
            },
        ]);
        for person in &case.liable_persons {
            dataset.inputs.push(InputRecordSpec {
                name: "is_not_student".to_string(),
                entity: "Person".to_string(),
                entity_id: person.id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: !person.is_student,
                },
            });
            dataset.relations.push(RelationRecordSpec {
                name: "liable_person".to_string(),
                tuple: vec![person.id.clone(), case.dwelling_id.clone()],
                interval: interval.clone(),
            });
        }
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.dwelling_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut ct_annual = Vec::with_capacity(case_file.cases.len());
    let mut ct_discounts = Vec::with_capacity(case_file.cases.len());
    let mut ct_other_reductions = Vec::with_capacity(case_file.cases.len());
    let mut band_number = Vec::with_capacity(case_file.cases.len());
    let mut non_dep_deductions_daily = Vec::with_capacity(case_file.cases.len());
    let mut partner_only_joint = Vec::with_capacity(case_file.cases.len());
    let mut person_offsets = Vec::with_capacity(case_file.cases.len() + 1);
    person_offsets.push(0_usize);
    let mut cursor = 0_usize;
    let mut is_not_student: Vec<bool> = Vec::new();

    for case in &case_file.cases {
        ct_annual.push(decimal(&case.ct_annual));
        ct_discounts.push(decimal(&case.ct_discounts));
        ct_other_reductions.push(decimal(&case.ct_other_reductions));
        band_number.push(case.band_number);
        non_dep_deductions_daily.push(decimal(&case.non_dep_deductions_daily));
        partner_only_joint.push(case.partner_only_joint);
        for person in &case.liable_persons {
            is_not_student.push(!person.is_student);
            cursor += 1;
        }
        person_offsets.push(cursor);
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([
                    ("ct_annual".to_string(), DenseColumn::Decimal(ct_annual)),
                    (
                        "ct_discounts".to_string(),
                        DenseColumn::Decimal(ct_discounts),
                    ),
                    (
                        "ct_other_reductions".to_string(),
                        DenseColumn::Decimal(ct_other_reductions),
                    ),
                    ("band_number".to_string(), DenseColumn::Integer(band_number)),
                    (
                        "non_dep_deductions_daily".to_string(),
                        DenseColumn::Decimal(non_dep_deductions_daily),
                    ),
                    (
                        "partner_only_joint".to_string(),
                        DenseColumn::Bool(partner_only_joint),
                    ),
                ]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "liable_person".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: person_offsets,
                        inputs: HashMap::from([(
                            "is_not_student".to_string(),
                            DenseColumn::Bool(is_not_student),
                        )]),
                    },
                )]),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            match explain_value {
                OutputValue::Scalar { .. } => compare_scalar(explain_value, dense_value, row),
                OutputValue::Judgment { .. } => compare_judgment(explain_value, dense_value, row),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ScottishCtrCaseFile {
    cases: Vec<ScottishCtrCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct ScottishCtrCase {
    dwelling_id: String,
    period: PeriodSpec,
    ct_annual: String,
    ct_discounts: String,
    ct_other_reductions: String,
    band_number: i64,
    non_dep_deductions_daily: String,
    partner_only_joint: bool,
    liable_persons: Vec<ScottishCtrPerson>,
}

#[derive(Clone, Debug, Deserialize)]
struct ScottishCtrPerson {
    id: String,
    is_student: bool,
}

#[test]
fn dense_child_benefit_rates_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(CHILD_BENEFIT_RATES_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Claimant"))
        .expect("dense compilation succeeds");
    let case_file: ChildBenefitRatesCaseFile =
        serde_yaml::from_str(CHILD_BENEFIT_RATES_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "num_children_total".to_string(),
        "num_children_eligible_for_enhanced".to_string(),
        "num_enhanced_rate".to_string(),
        "num_standard_rate".to_string(),
        "weekly_child_benefit".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.push(InputRecordSpec {
            name: "is_voluntary_org".to_string(),
            entity: "Claimant".to_string(),
            entity_id: case.claimant_id.clone(),
            interval: interval.clone(),
            value: ScalarValueSpec::Bool {
                value: case.is_voluntary_org,
            },
        });
        for child in &case.children {
            dataset.inputs.extend([InputRecordSpec {
                name: "is_enhanced_eligible".to_string(),
                entity: "Child".to_string(),
                entity_id: child.id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: child.is_eldest_in_household && !child.resides_with_parent,
                },
            }]);
            dataset.relations.push(RelationRecordSpec {
                name: "child_of_claim".to_string(),
                tuple: vec![child.id.clone(), case.claimant_id.clone()],
                interval: interval.clone(),
            });
        }
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.claimant_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut is_voluntary_org = Vec::with_capacity(case_file.cases.len());
    let mut child_offsets = Vec::with_capacity(case_file.cases.len() + 1);
    child_offsets.push(0_usize);
    let mut cursor = 0_usize;
    let mut is_enhanced: Vec<bool> = Vec::new();
    for case in &case_file.cases {
        is_voluntary_org.push(case.is_voluntary_org);
        for child in &case.children {
            is_enhanced.push(child.is_eldest_in_household && !child.resides_with_parent);
            cursor += 1;
        }
        child_offsets.push(cursor);
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([(
                    "is_voluntary_org".to_string(),
                    DenseColumn::Bool(is_voluntary_org),
                )]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "child_of_claim".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: child_offsets,
                        inputs: HashMap::from([(
                            "is_enhanced_eligible".to_string(),
                            DenseColumn::Bool(is_enhanced),
                        )]),
                    },
                )]),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            compare_scalar(
                explain.results[row]
                    .outputs
                    .get(output)
                    .unwrap_or_else(|| panic!("{output} output for row {row}")),
                dense_result
                    .outputs
                    .get(output)
                    .unwrap_or_else(|| panic!("dense {output}")),
                row,
            );
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ChildBenefitRatesCaseFile {
    cases: Vec<ChildBenefitRatesCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct ChildBenefitRatesCase {
    claimant_id: String,
    period: PeriodSpec,
    is_voluntary_org: bool,
    children: Vec<ChildBenefitRatesChild>,
}

#[derive(Clone, Debug, Deserialize)]
struct ChildBenefitRatesChild {
    id: String,
    is_eldest_in_household: bool,
    resides_with_parent: bool,
}

#[test]
fn dense_auto_enrolment_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(AUTO_ENROLMENT_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Jobholder"))
        .expect("dense compilation succeeds");
    let case_file: AutoEnrolmentCaseFile =
        serde_yaml::from_str(AUTO_ENROLMENT_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "earnings_trigger_for_prp".to_string(),
        "age_at_least_22".to_string(),
        "below_pensionable_age".to_string(),
        "earnings_above_trigger".to_string(),
        "not_already_active_member".to_string(),
        "not_recently_opted_out".to_string(),
        "employer_enrolment_duty".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "current_age_years".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.current_age_years,
                },
            },
            InputRecordSpec {
                name: "pensionable_age_years".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.pensionable_age_years,
                },
            },
            InputRecordSpec {
                name: "earnings_this_prp".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.earnings_this_prp.clone(),
                },
            },
            InputRecordSpec {
                name: "prp_months".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.prp_months.clone(),
                },
            },
            InputRecordSpec {
                name: "active_member_of_qualifying_scheme".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.active_member_of_qualifying_scheme,
                },
            },
            InputRecordSpec {
                name: "recently_opted_out".to_string(),
                entity: "Jobholder".to_string(),
                entity_id: case.jobholder_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.recently_opted_out,
                },
            },
        ]);
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.jobholder_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut current_age = Vec::with_capacity(case_file.cases.len());
    let mut pensionable_age = Vec::with_capacity(case_file.cases.len());
    let mut earnings_this_prp = Vec::with_capacity(case_file.cases.len());
    let mut prp_months = Vec::with_capacity(case_file.cases.len());
    let mut active_member = Vec::with_capacity(case_file.cases.len());
    let mut opted_out = Vec::with_capacity(case_file.cases.len());
    for case in &case_file.cases {
        current_age.push(case.current_age_years);
        pensionable_age.push(case.pensionable_age_years);
        earnings_this_prp.push(decimal(&case.earnings_this_prp));
        prp_months.push(decimal(&case.prp_months));
        active_member.push(case.active_member_of_qualifying_scheme);
        opted_out.push(case.recently_opted_out);
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([
                    (
                        "current_age_years".to_string(),
                        DenseColumn::Integer(current_age),
                    ),
                    (
                        "pensionable_age_years".to_string(),
                        DenseColumn::Integer(pensionable_age),
                    ),
                    (
                        "earnings_this_prp".to_string(),
                        DenseColumn::Decimal(earnings_this_prp),
                    ),
                    ("prp_months".to_string(), DenseColumn::Decimal(prp_months)),
                    (
                        "active_member_of_qualifying_scheme".to_string(),
                        DenseColumn::Bool(active_member),
                    ),
                    (
                        "recently_opted_out".to_string(),
                        DenseColumn::Bool(opted_out),
                    ),
                ]),
                relations: HashMap::new(),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            match explain_value {
                OutputValue::Scalar { .. } => compare_scalar(explain_value, dense_value, row),
                OutputValue::Judgment { .. } => compare_judgment(explain_value, dense_value, row),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct AutoEnrolmentCaseFile {
    cases: Vec<AutoEnrolmentCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct AutoEnrolmentCase {
    jobholder_id: String,
    period: PeriodSpec,
    current_age_years: i64,
    pensionable_age_years: i64,
    earnings_this_prp: String,
    prp_months: String,
    active_member_of_qualifying_scheme: bool,
    recently_opted_out: bool,
}

#[test]
fn dense_ated_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(ATED_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("DwellingInterest"))
        .expect("dense compilation succeeds");
    let case_file: AtedCaseFile = serde_yaml::from_str(ATED_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "band_number".to_string(),
        "annual_chargeable_amount".to_string(),
        "days_in_period".to_string(),
        "days_from_entry".to_string(),
        "tax_chargeable".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "taxable_value".to_string(),
                entity: "DwellingInterest".to_string(),
                entity_id: case.interest_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.taxable_value.clone(),
                },
            },
            InputRecordSpec {
                name: "in_charge_on_first_day".to_string(),
                entity: "DwellingInterest".to_string(),
                entity_id: case.interest_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.in_charge_on_first_day,
                },
            },
            InputRecordSpec {
                name: "entry_day".to_string(),
                entity: "DwellingInterest".to_string(),
                entity_id: case.interest_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Date {
                    value: chrono::NaiveDate::parse_from_str(&case.entry_day, "%Y-%m-%d")
                        .expect("valid date"),
                },
            },
        ]);
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.interest_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut taxable_value = Vec::with_capacity(case_file.cases.len());
    let mut in_charge_on_first_day = Vec::with_capacity(case_file.cases.len());
    let mut entry_day = Vec::with_capacity(case_file.cases.len());
    for case in &case_file.cases {
        taxable_value.push(decimal(&case.taxable_value));
        in_charge_on_first_day.push(case.in_charge_on_first_day);
        entry_day.push(
            chrono::NaiveDate::parse_from_str(&case.entry_day, "%Y-%m-%d").expect("valid date"),
        );
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([
                    (
                        "taxable_value".to_string(),
                        DenseColumn::Decimal(taxable_value),
                    ),
                    (
                        "in_charge_on_first_day".to_string(),
                        DenseColumn::Bool(in_charge_on_first_day),
                    ),
                    ("entry_day".to_string(), DenseColumn::Date(entry_day)),
                ]),
                relations: HashMap::new(),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            compare_scalar(explain_value, dense_value, row);
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct AtedCaseFile {
    cases: Vec<AtedCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct AtedCase {
    interest_id: String,
    period: PeriodSpec,
    taxable_value: String,
    in_charge_on_first_day: bool,
    entry_day: String,
}

#[test]
fn dense_ct_marginal_relief_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(CT_MARGINAL_RELIEF_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Company"))
        .expect("dense compilation succeeds");
    let case_file: CtMarginalReliefCaseFile =
        serde_yaml::from_str(CT_MARGINAL_RELIEF_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "num_associates".to_string(),
        "associates_divisor".to_string(),
        "lower_limit_effective".to_string(),
        "upper_limit_effective".to_string(),
        "within_marginal_band".to_string(),
        "eligible_for_marginal_relief".to_string(),
        "marginal_relief".to_string(),
        "gross_corporation_tax".to_string(),
        "corporation_tax_after_relief".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "uk_resident".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.uk_resident,
                },
            },
            InputRecordSpec {
                name: "close_investment_holding".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.close_investment_holding,
                },
            },
            InputRecordSpec {
                name: "augmented_profits".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.augmented_profits.clone(),
                },
            },
            InputRecordSpec {
                name: "taxable_total_profits".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.taxable_total_profits.clone(),
                },
            },
            InputRecordSpec {
                name: "ring_fence_profits".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.ring_fence_profits.clone(),
                },
            },
            InputRecordSpec {
                name: "ap_year_fraction".to_string(),
                entity: "Company".to_string(),
                entity_id: case.company_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.ap_year_fraction.clone(),
                },
            },
        ]);
        for associate in &case.associates {
            dataset.relations.push(RelationRecordSpec {
                name: "associate_of".to_string(),
                tuple: vec![associate.clone(), case.company_id.clone()],
                interval: interval.clone(),
            });
        }
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.company_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let mut uk_resident = Vec::with_capacity(case_file.cases.len());
    let mut close_investment_holding = Vec::with_capacity(case_file.cases.len());
    let mut augmented_profits = Vec::with_capacity(case_file.cases.len());
    let mut taxable_total_profits = Vec::with_capacity(case_file.cases.len());
    let mut ring_fence_profits = Vec::with_capacity(case_file.cases.len());
    let mut ap_year_fraction = Vec::with_capacity(case_file.cases.len());
    let mut offsets = Vec::with_capacity(case_file.cases.len() + 1);
    offsets.push(0_usize);
    let mut cursor = 0_usize;
    for case in &case_file.cases {
        uk_resident.push(case.uk_resident);
        close_investment_holding.push(case.close_investment_holding);
        augmented_profits.push(decimal(&case.augmented_profits));
        taxable_total_profits.push(decimal(&case.taxable_total_profits));
        ring_fence_profits.push(decimal(&case.ring_fence_profits));
        ap_year_fraction.push(decimal(&case.ap_year_fraction));
        cursor += case.associates.len();
        offsets.push(cursor);
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([
                    ("uk_resident".to_string(), DenseColumn::Bool(uk_resident)),
                    (
                        "close_investment_holding".to_string(),
                        DenseColumn::Bool(close_investment_holding),
                    ),
                    (
                        "augmented_profits".to_string(),
                        DenseColumn::Decimal(augmented_profits),
                    ),
                    (
                        "taxable_total_profits".to_string(),
                        DenseColumn::Decimal(taxable_total_profits),
                    ),
                    (
                        "ring_fence_profits".to_string(),
                        DenseColumn::Decimal(ring_fence_profits),
                    ),
                    (
                        "ap_year_fraction".to_string(),
                        DenseColumn::Decimal(ap_year_fraction),
                    ),
                ]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "associate_of".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets,
                        inputs: HashMap::new(),
                    },
                )]),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            match explain_value {
                OutputValue::Scalar { .. } => compare_scalar(explain_value, dense_value, row),
                OutputValue::Judgment { .. } => compare_judgment(explain_value, dense_value, row),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct CtMarginalReliefCaseFile {
    cases: Vec<CtMarginalReliefCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct CtMarginalReliefCase {
    company_id: String,
    period: PeriodSpec,
    uk_resident: bool,
    close_investment_holding: bool,
    augmented_profits: String,
    taxable_total_profits: String,
    ring_fence_profits: String,
    ap_year_fraction: String,
    associates: Vec<String>,
}

#[test]
fn dense_state_pension_transitional_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(STATE_PENSION_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .expect("dense compilation succeeds");
    let case_file: StatePensionCaseFile =
        serde_yaml::from_str(STATE_PENSION_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "pre_commencement_qy_count".to_string(),
        "post_commencement_qy_count".to_string(),
        "total_qy_count".to_string(),
        "reached_pensionable_age".to_string(),
        "meets_minimum_qy".to_string(),
        "has_any_pre_commencement_year".to_string(),
        "entitled_to_transitional_rate".to_string(),
    ];

    let mut dataset = DatasetSpec::default();
    for case in &case_file.cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "current_age_years".to_string(),
                entity: "Person".to_string(),
                entity_id: case.person_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.current_age_years,
                },
            },
            InputRecordSpec {
                name: "pensionable_age_years".to_string(),
                entity: "Person".to_string(),
                entity_id: case.person_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.pensionable_age_years,
                },
            },
        ]);
        for qy in &case.qualifying_years {
            let year_start =
                chrono::NaiveDate::parse_from_str(&qy.year_start, "%Y-%m-%d").expect("valid date");
            let window_start = chrono::NaiveDate::from_ymd_opt(1978, 4, 6).unwrap();
            let window_end = chrono::NaiveDate::from_ymd_opt(2016, 4, 6).unwrap();
            // Caller pre-applies the s.4(4) classification — the
            // date-window test is a legal fact, not an engine expression.
            let is_pre =
                (qy.is_qualifying && year_start >= window_start && year_start < window_end)
                    || qy.is_reckonable_1979;
            let is_post = qy.is_qualifying && year_start >= window_end;
            dataset.inputs.extend([
                InputRecordSpec {
                    name: "is_pre_commencement_qy".to_string(),
                    entity: "QualifyingYear".to_string(),
                    entity_id: qy.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: is_pre },
                },
                InputRecordSpec {
                    name: "is_post_commencement_qy".to_string(),
                    entity: "QualifyingYear".to_string(),
                    entity_id: qy.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: is_post },
                },
            ]);
            dataset.relations.push(RelationRecordSpec {
                name: "qualifying_year_of".to_string(),
                tuple: vec![qy.id.clone(), case.person_id.clone()],
                interval: interval.clone(),
            });
        }
    }

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.person_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    // Build dense batch.
    let mut current_age = Vec::with_capacity(case_file.cases.len());
    let mut pensionable_age = Vec::with_capacity(case_file.cases.len());
    let mut qy_offsets = Vec::with_capacity(case_file.cases.len() + 1);
    qy_offsets.push(0_usize);
    let mut cursor = 0_usize;
    let mut is_pre_commencement_qy: Vec<bool> = Vec::new();
    let mut is_post_commencement_qy: Vec<bool> = Vec::new();
    let window_start = chrono::NaiveDate::from_ymd_opt(1978, 4, 6).unwrap();
    let window_end = chrono::NaiveDate::from_ymd_opt(2016, 4, 6).unwrap();
    for case in &case_file.cases {
        current_age.push(case.current_age_years);
        pensionable_age.push(case.pensionable_age_years);
        for qy in &case.qualifying_years {
            let year_start =
                chrono::NaiveDate::parse_from_str(&qy.year_start, "%Y-%m-%d").expect("valid date");
            is_pre_commencement_qy.push(
                (qy.is_qualifying && year_start >= window_start && year_start < window_end)
                    || qy.is_reckonable_1979,
            );
            is_post_commencement_qy.push(qy.is_qualifying && year_start >= window_end);
            cursor += 1;
        }
        qy_offsets.push(cursor);
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: case_file.cases.len(),
                inputs: HashMap::from([
                    (
                        "current_age_years".to_string(),
                        DenseColumn::Integer(current_age),
                    ),
                    (
                        "pensionable_age_years".to_string(),
                        DenseColumn::Integer(pensionable_age),
                    ),
                ]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "qualifying_year_of".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: qy_offsets,
                        inputs: HashMap::from([
                            (
                                "is_pre_commencement_qy".to_string(),
                                DenseColumn::Bool(is_pre_commencement_qy),
                            ),
                            (
                                "is_post_commencement_qy".to_string(),
                                DenseColumn::Bool(is_post_commencement_qy),
                            ),
                        ]),
                    },
                )]),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            match explain_value {
                OutputValue::Scalar { .. } => compare_scalar(explain_value, dense_value, row),
                OutputValue::Judgment { .. } => compare_judgment(explain_value, dense_value, row),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct StatePensionCaseFile {
    cases: Vec<StatePensionCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct StatePensionCase {
    person_id: String,
    period: PeriodSpec,
    current_age_years: i64,
    pensionable_age_years: i64,
    qualifying_years: Vec<StatePensionYear>,
}

#[derive(Clone, Debug, Deserialize)]
struct StatePensionYear {
    id: String,
    year_start: String,
    is_qualifying: bool,
    is_reckonable_1979: bool,
}

#[test]
fn dense_universal_credit_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(UC_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("BenefitUnit"))
        .expect("dense compilation succeeds");
    let case_file: UcCaseFile = serde_yaml::from_str(UC_CASES_YAML).expect("fixture parses");
    let period = case_file.cases[0].period.clone();

    let outputs = [
        "standard_allowance".to_string(),
        "child_element_total".to_string(),
        "disabled_child_element_total".to_string(),
        "lcwra_element".to_string(),
        "carer_element".to_string(),
        "housing_element".to_string(),
        "max_uc".to_string(),
        "work_allowance_amount".to_string(),
        "earnings_deduction".to_string(),
        "tariff_income".to_string(),
        "over_capital_limit".to_string(),
        "uc_award".to_string(),
    ];

    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset: uc_dataset(&case_file.cases),
        queries: case_file
            .cases
            .iter()
            .map(|case| ExecutionQuery {
                entity_id: case.benefit_unit_id.clone(),
                period: case.period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            uc_dense_batch(&case_file.cases),
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..case_file.cases.len() {
        for output in &outputs {
            let explain_value = explain.results[row]
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("{output} output for row {row}"));
            let dense_value = dense_result
                .outputs
                .get(output)
                .unwrap_or_else(|| panic!("dense {output}"));
            match explain_value {
                OutputValue::Scalar { .. } => compare_scalar(explain_value, dense_value, row),
                OutputValue::Judgment { .. } => compare_judgment(explain_value, dense_value, row),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct UcCaseFile {
    cases: Vec<UcCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct UcCase {
    benefit_unit_id: String,
    period: PeriodSpec,
    is_couple: bool,
    has_housing_costs: bool,
    eligible_housing_costs: String,
    non_dep_deductions_total: String,
    earned_income_monthly: String,
    unearned_income_monthly: String,
    capital_total: String,
    adults: Vec<UcAdult>,
    children: Vec<UcChild>,
}

#[derive(Clone, Debug, Deserialize)]
struct UcAdult {
    id: String,
    age_25_or_over: bool,
    has_lcwra: bool,
    is_carer: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct UcChild {
    id: String,
    qualifies_for_child_element: bool,
    #[serde(default)]
    is_higher_rate_first_child: bool,
    disability_level: String,
}

fn uc_dataset(cases: &[UcCase]) -> DatasetSpec {
    let mut dataset = DatasetSpec::default();
    for case in cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "is_couple".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.is_couple,
                },
            },
            InputRecordSpec {
                name: "has_housing_costs".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Bool {
                    value: case.has_housing_costs,
                },
            },
            InputRecordSpec {
                name: "eligible_housing_costs".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.eligible_housing_costs.clone(),
                },
            },
            InputRecordSpec {
                name: "non_dep_deductions_total".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.non_dep_deductions_total.clone(),
                },
            },
            InputRecordSpec {
                name: "earned_income_monthly".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.earned_income_monthly.clone(),
                },
            },
            InputRecordSpec {
                name: "unearned_income_monthly".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.unearned_income_monthly.clone(),
                },
            },
            InputRecordSpec {
                name: "capital_total".to_string(),
                entity: "BenefitUnit".to_string(),
                entity_id: case.benefit_unit_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: case.capital_total.clone(),
                },
            },
        ]);
        for adult in &case.adults {
            dataset.inputs.extend([
                InputRecordSpec {
                    name: "age_25_or_over".to_string(),
                    entity: "Adult".to_string(),
                    entity_id: adult.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool {
                        value: adult.age_25_or_over,
                    },
                },
                InputRecordSpec {
                    name: "has_lcwra".to_string(),
                    entity: "Adult".to_string(),
                    entity_id: adult.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool {
                        value: adult.has_lcwra,
                    },
                },
                InputRecordSpec {
                    name: "is_carer".to_string(),
                    entity: "Adult".to_string(),
                    entity_id: adult.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool {
                        value: adult.is_carer,
                    },
                },
            ]);
            dataset.relations.push(RelationRecordSpec {
                name: "adult_of_benefit_unit".to_string(),
                tuple: vec![adult.id.clone(), case.benefit_unit_id.clone()],
                interval: interval.clone(),
            });
        }
        for child in &case.children {
            let q = child.qualifies_for_child_element;
            let higher = q && child.is_higher_rate_first_child;
            let standard = q && !child.is_higher_rate_first_child;
            let dis_lower = child.disability_level == "lower";
            let dis_higher = child.disability_level == "higher";
            dataset.inputs.extend([
                InputRecordSpec {
                    name: "qualifies_for_higher_rate".to_string(),
                    entity: "Child".to_string(),
                    entity_id: child.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: higher },
                },
                InputRecordSpec {
                    name: "qualifies_for_standard_rate".to_string(),
                    entity: "Child".to_string(),
                    entity_id: child.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: standard },
                },
                InputRecordSpec {
                    name: "qualifies_for_child_element".to_string(),
                    entity: "Child".to_string(),
                    entity_id: child.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: q },
                },
                InputRecordSpec {
                    name: "disability_is_lower".to_string(),
                    entity: "Child".to_string(),
                    entity_id: child.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: dis_lower },
                },
                InputRecordSpec {
                    name: "disability_is_higher".to_string(),
                    entity: "Child".to_string(),
                    entity_id: child.id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool { value: dis_higher },
                },
            ]);
            dataset.relations.push(RelationRecordSpec {
                name: "child_of_benefit_unit".to_string(),
                tuple: vec![child.id.clone(), case.benefit_unit_id.clone()],
                interval: interval.clone(),
            });
        }
    }
    dataset
}

fn uc_dense_batch(cases: &[UcCase]) -> DenseBatchSpec {
    let mut is_couple = Vec::with_capacity(cases.len());
    let mut has_housing_costs = Vec::with_capacity(cases.len());
    let mut eligible_housing_costs = Vec::with_capacity(cases.len());
    let mut non_dep_deductions_total = Vec::with_capacity(cases.len());
    let mut earned_income_monthly = Vec::with_capacity(cases.len());
    let mut unearned_income_monthly = Vec::with_capacity(cases.len());
    let mut capital_total = Vec::with_capacity(cases.len());

    let mut adult_offsets = Vec::with_capacity(cases.len() + 1);
    adult_offsets.push(0_usize);
    let mut adult_cursor = 0_usize;
    let mut adult_age_25_or_over: Vec<bool> = Vec::new();
    let mut adult_has_lcwra: Vec<bool> = Vec::new();
    let mut adult_is_carer: Vec<bool> = Vec::new();

    let mut child_offsets = Vec::with_capacity(cases.len() + 1);
    child_offsets.push(0_usize);
    let mut child_cursor = 0_usize;
    let mut child_qualifies: Vec<bool> = Vec::new();
    let mut child_is_higher_rate: Vec<bool> = Vec::new();
    let mut child_is_standard_rate: Vec<bool> = Vec::new();
    let mut child_dis_lower: Vec<bool> = Vec::new();
    let mut child_dis_higher: Vec<bool> = Vec::new();

    for case in cases {
        is_couple.push(case.is_couple);
        has_housing_costs.push(case.has_housing_costs);
        eligible_housing_costs.push(decimal(&case.eligible_housing_costs));
        non_dep_deductions_total.push(decimal(&case.non_dep_deductions_total));
        earned_income_monthly.push(decimal(&case.earned_income_monthly));
        unearned_income_monthly.push(decimal(&case.unearned_income_monthly));
        capital_total.push(decimal(&case.capital_total));

        for adult in &case.adults {
            adult_age_25_or_over.push(adult.age_25_or_over);
            adult_has_lcwra.push(adult.has_lcwra);
            adult_is_carer.push(adult.is_carer);
            adult_cursor += 1;
        }
        adult_offsets.push(adult_cursor);

        for child in &case.children {
            let q = child.qualifies_for_child_element;
            child_qualifies.push(q);
            child_is_higher_rate.push(q && child.is_higher_rate_first_child);
            child_is_standard_rate.push(q && !child.is_higher_rate_first_child);
            child_dis_lower.push(child.disability_level == "lower");
            child_dis_higher.push(child.disability_level == "higher");
            child_cursor += 1;
        }
        child_offsets.push(child_cursor);
    }

    DenseBatchSpec {
        row_count: cases.len(),
        inputs: HashMap::from([
            ("is_couple".to_string(), DenseColumn::Bool(is_couple)),
            (
                "has_housing_costs".to_string(),
                DenseColumn::Bool(has_housing_costs),
            ),
            (
                "eligible_housing_costs".to_string(),
                DenseColumn::Decimal(eligible_housing_costs),
            ),
            (
                "non_dep_deductions_total".to_string(),
                DenseColumn::Decimal(non_dep_deductions_total),
            ),
            (
                "earned_income_monthly".to_string(),
                DenseColumn::Decimal(earned_income_monthly),
            ),
            (
                "unearned_income_monthly".to_string(),
                DenseColumn::Decimal(unearned_income_monthly),
            ),
            (
                "capital_total".to_string(),
                DenseColumn::Decimal(capital_total),
            ),
        ]),
        relations: HashMap::from([
            (
                DenseRelationKey {
                    name: "adult_of_benefit_unit".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: adult_offsets,
                    inputs: HashMap::from([
                        (
                            "age_25_or_over".to_string(),
                            DenseColumn::Bool(adult_age_25_or_over),
                        ),
                        ("has_lcwra".to_string(), DenseColumn::Bool(adult_has_lcwra)),
                        ("is_carer".to_string(), DenseColumn::Bool(adult_is_carer)),
                    ]),
                },
            ),
            (
                DenseRelationKey {
                    name: "child_of_benefit_unit".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: child_offsets,
                    inputs: HashMap::from([
                        (
                            "qualifies_for_child_element".to_string(),
                            DenseColumn::Bool(child_qualifies),
                        ),
                        (
                            "qualifies_for_higher_rate".to_string(),
                            DenseColumn::Bool(child_is_higher_rate),
                        ),
                        (
                            "qualifies_for_standard_rate".to_string(),
                            DenseColumn::Bool(child_is_standard_rate),
                        ),
                        (
                            "disability_is_lower".to_string(),
                            DenseColumn::Bool(child_dis_lower),
                        ),
                        (
                            "disability_is_higher".to_string(),
                            DenseColumn::Bool(child_dis_higher),
                        ),
                    ]),
                },
            ),
        ]),
    }
}

#[test]
fn dense_date_add_days_matches_explain_mode() {
    use axiom_rules_engine::spec::{
        DerivedSemanticsSpec, DerivedSpec, JudgmentExprSpec, ProgramSpec, ScalarExprSpec,
    };

    let mut program = ProgramSpec::default();
    program.derived.push(DerivedSpec {
        id: None,
        name: "relevant_week_start".to_string(),
        entity: "PartWeek".to_string(),
        dtype: DTypeSpec::Date,
        unit: None,
        source: None,
        period: None,
        source_url: None,
        semantics: DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::DateAddDays {
                date: Box::new(ScalarExprSpec::Input {
                    name: "part_week_end".to_string(),
                }),
                days: Box::new(ScalarExprSpec::Literal {
                    value: ScalarValueSpec::Integer { value: -6 },
                }),
            },
        },
    });
    program.derived.push(DerivedSpec {
        id: None,
        name: "relevant_week_ends_on_end".to_string(),
        entity: "PartWeek".to_string(),
        dtype: DTypeSpec::Judgment,
        unit: None,
        source: None,
        period: None,
        source_url: None,
        semantics: DerivedSemanticsSpec::Judgment {
            expr: JudgmentExprSpec::Comparison {
                left: Box::new(ScalarExprSpec::Derived {
                    name: "relevant_week_start".to_string(),
                }),
                op: axiom_rules_engine::spec::ComparisonOpSpec::Lt,
                right: Box::new(ScalarExprSpec::Input {
                    name: "part_week_end".to_string(),
                }),
            },
        },
    });

    let artifact = CompiledProgramArtifact::compile(program).expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("PartWeek"))
        .expect("dense compilation succeeds");

    let period = PeriodSpec {
        kind: PeriodKindSpec::BenefitWeek,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 11).expect("date"),
    };
    let interval = period_interval(&period);
    let part_weeks = [
        (
            "pw-1",
            chrono::NaiveDate::from_ymd_opt(2026, 1, 8).expect("date"),
        ),
        (
            "pw-2",
            chrono::NaiveDate::from_ymd_opt(2026, 1, 11).expect("date"),
        ),
        (
            "pw-3",
            chrono::NaiveDate::from_ymd_opt(2026, 2, 1).expect("date"),
        ),
    ];

    let mut dataset = DatasetSpec::default();
    for (id, end_date) in &part_weeks {
        dataset.inputs.push(InputRecordSpec {
            name: "part_week_end".to_string(),
            entity: "PartWeek".to_string(),
            entity_id: id.to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Date { value: *end_date },
        });
    }

    let outputs = [
        "relevant_week_start".to_string(),
        "relevant_week_ends_on_end".to_string(),
    ];
    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: part_weeks
            .iter()
            .map(|(id, _)| ExecutionQuery {
                entity_id: id.to_string(),
                period: period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: part_weeks.len(),
                inputs: HashMap::from([(
                    "part_week_end".to_string(),
                    DenseColumn::Date(part_weeks.iter().map(|(_, date)| *date).collect()),
                )]),
                relations: HashMap::new(),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..part_weeks.len() {
        compare_scalar(
            explain.results[row]
                .outputs
                .get("relevant_week_start")
                .expect("relevant_week_start output"),
            dense_result
                .outputs
                .get("relevant_week_start")
                .expect("dense relevant_week_start"),
            row,
        );
        compare_judgment(
            explain.results[row]
                .outputs
                .get("relevant_week_ends_on_end")
                .expect("judgment output"),
            dense_result
                .outputs
                .get("relevant_week_ends_on_end")
                .expect("dense judgment"),
            row,
        );
    }
}

#[test]
fn dense_notional_capital_matches_explain_mode() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(NOTIONAL_CAPITAL_PROGRAM_RULESPEC)
        .expect("RuleSpec module compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Applicant"))
        .expect("dense compilation succeeds");

    let period = PeriodSpec {
        kind: PeriodKindSpec::Custom {
            name: "ctr_week".to_string(),
        },
        start: chrono::NaiveDate::from_ymd_opt(2026, 2, 2).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 2, 8).expect("date"),
    };
    let interval = period_interval(&period);

    // Four applicants exercising every branch of the filter.
    struct Disposal {
        amount: &'static str,
        purpose: &'static str,
        reason: &'static str,
    }
    struct Applicant {
        id: &'static str,
        actual_capital: &'static str,
        disposals: Vec<Disposal>,
    }
    let applicants = vec![
        Applicant {
            id: "applicant-a",
            actual_capital: "4000",
            disposals: vec![],
        },
        Applicant {
            id: "applicant-b",
            actual_capital: "3500",
            disposals: vec![
                Disposal {
                    amount: "2500",
                    purpose: "secure_ctr",
                    reason: "none",
                },
                Disposal {
                    amount: "900",
                    purpose: "secure_ctr",
                    reason: "debt",
                },
                Disposal {
                    amount: "700",
                    purpose: "secure_ctr",
                    reason: "reasonable_purchase",
                },
                Disposal {
                    amount: "1100",
                    purpose: "other",
                    reason: "none",
                },
            ],
        },
        Applicant {
            id: "applicant-c",
            actual_capital: "500",
            disposals: vec![Disposal {
                amount: "10000",
                purpose: "secure_ctr",
                reason: "none",
            }],
        },
        Applicant {
            id: "applicant-d",
            actual_capital: "6500",
            disposals: vec![
                Disposal {
                    amount: "4000",
                    purpose: "other",
                    reason: "none",
                },
                Disposal {
                    amount: "3200",
                    purpose: "secure_ctr",
                    reason: "reasonable_purchase",
                },
            ],
        },
    ];

    let mut dataset = DatasetSpec::default();
    for applicant in &applicants {
        dataset.inputs.push(InputRecordSpec {
            name: "actual_capital".to_string(),
            entity: "Applicant".to_string(),
            entity_id: applicant.id.to_string(),
            interval: interval.clone(),
            value: ScalarValueSpec::Decimal {
                value: applicant.actual_capital.to_string(),
            },
        });
        for (disposal_index, disposal) in applicant.disposals.iter().enumerate() {
            let disposal_id = format!("{}-disposal-{}", applicant.id, disposal_index);
            dataset.relations.push(RelationRecordSpec {
                name: "applicant_disposal".to_string(),
                // Match the engine's default slot convention: slot 0 =
                // related (Disposal), slot 1 = current (Applicant).
                tuple: vec![disposal_id.clone(), applicant.id.to_string()],
                interval: interval.clone(),
            });
            dataset.inputs.extend([
                InputRecordSpec {
                    name: "disposal_amount".to_string(),
                    entity: "Disposal".to_string(),
                    entity_id: disposal_id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Decimal {
                        value: disposal.amount.to_string(),
                    },
                },
                // Caller pre-computes the two boolean predicates the
                // filtered-aggregation extensions (sum_where / count_where)
                // consume from each Disposal: qualifies for the notional-
                // capital inclusion, and counts-for-CTR.
                InputRecordSpec {
                    name: "is_qualifying_disposal".to_string(),
                    entity: "Disposal".to_string(),
                    entity_id: disposal_id.clone(),
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool {
                        value: disposal.purpose == "secure_ctr"
                            && disposal.reason != "debt"
                            && disposal.reason != "reasonable_purchase",
                    },
                },
                InputRecordSpec {
                    name: "disposal_counts_for_ctr".to_string(),
                    entity: "Disposal".to_string(),
                    entity_id: disposal_id,
                    interval: interval.clone(),
                    value: ScalarValueSpec::Bool {
                        value: disposal.purpose == "secure_ctr",
                    },
                },
            ]);
        }
    }

    let outputs = [
        "counted_disposals".to_string(),
        "notional_capital".to_string(),
        "capital_for_ctr".to_string(),
    ];
    let explain = execute_request(ExecutionRequest {
        mode: ExecutionMode::Explain,
        program: artifact.program.clone(),
        dataset,
        queries: applicants
            .iter()
            .map(|applicant| ExecutionQuery {
                entity_id: applicant.id.to_string(),
                period: period.clone(),
                outputs: outputs.to_vec(),
            })
            .collect(),
    })
    .expect("explain execution succeeds");

    // Build the dense batch with offsets over the flat disposal list.
    let mut offsets = Vec::with_capacity(applicants.len() + 1);
    offsets.push(0_usize);
    let mut disposal_amount = Vec::new();
    let mut is_qualifying_disposal = Vec::new();
    let mut disposal_counts_for_ctr = Vec::new();
    let mut actual_capital = Vec::with_capacity(applicants.len());
    for applicant in &applicants {
        actual_capital.push(decimal(applicant.actual_capital));
        for disposal in &applicant.disposals {
            disposal_amount.push(decimal(disposal.amount));
            is_qualifying_disposal.push(
                disposal.purpose == "secure_ctr"
                    && disposal.reason != "debt"
                    && disposal.reason != "reasonable_purchase",
            );
            disposal_counts_for_ctr.push(disposal.purpose == "secure_ctr");
        }
        offsets.push(disposal_amount.len());
    }

    let dense_result = dense
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: applicants.len(),
                inputs: HashMap::from([(
                    "actual_capital".to_string(),
                    DenseColumn::Decimal(actual_capital),
                )]),
                relations: HashMap::from([(
                    DenseRelationKey {
                        name: "applicant_disposal".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets,
                        inputs: HashMap::from([
                            (
                                "disposal_amount".to_string(),
                                DenseColumn::Decimal(disposal_amount),
                            ),
                            (
                                "is_qualifying_disposal".to_string(),
                                DenseColumn::Bool(is_qualifying_disposal),
                            ),
                            (
                                "disposal_counts_for_ctr".to_string(),
                                DenseColumn::Bool(disposal_counts_for_ctr),
                            ),
                        ]),
                    },
                )]),
            },
            &outputs,
        )
        .expect("dense execution succeeds");

    for row in 0..applicants.len() {
        for output in &outputs {
            compare_scalar(
                explain.results[row]
                    .outputs
                    .get(output)
                    .unwrap_or_else(|| panic!("{output} output for row {row}")),
                dense_result
                    .outputs
                    .get(output)
                    .unwrap_or_else(|| panic!("dense {output}")),
                row,
            );
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ChildBenefitCaseFile {
    cases: Vec<ChildBenefitCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct ChildBenefitCase {
    child_id: String,
    period: PeriodSpec,
    cb_recipients: Vec<String>,
    cb_claim_count: i64,
    cb_recipient_id: String,
    sole_claimant_id: String,
    usual_resident_id: String,
}

fn child_benefit_dataset(cases: &[ChildBenefitCase]) -> DatasetSpec {
    let mut dataset = DatasetSpec::default();
    for case in cases {
        let interval = period_interval(&case.period);
        dataset.inputs.extend([
            InputRecordSpec {
                name: "cb_claim_count".to_string(),
                entity: "Child".to_string(),
                entity_id: case.child_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Integer {
                    value: case.cb_claim_count,
                },
            },
            InputRecordSpec {
                name: "cb_recipient_id".to_string(),
                entity: "Child".to_string(),
                entity_id: case.child_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Text {
                    value: case.cb_recipient_id.clone(),
                },
            },
            InputRecordSpec {
                name: "sole_claimant_id".to_string(),
                entity: "Child".to_string(),
                entity_id: case.child_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Text {
                    value: case.sole_claimant_id.clone(),
                },
            },
            InputRecordSpec {
                name: "usual_resident_id".to_string(),
                entity: "Child".to_string(),
                entity_id: case.child_id.clone(),
                interval: interval.clone(),
                value: ScalarValueSpec::Text {
                    value: case.usual_resident_id.clone(),
                },
            },
        ]);
        for recipient in &case.cb_recipients {
            dataset.relations.push(RelationRecordSpec {
                name: "cb_receipt".to_string(),
                tuple: vec![recipient.clone(), case.child_id.clone()],
                interval: interval.clone(),
            });
        }
    }
    dataset
}

fn child_benefit_dense_batch(cases: &[ChildBenefitCase]) -> DenseBatchSpec {
    let mut offsets = Vec::with_capacity(cases.len() + 1);
    let mut cb_claim_count = Vec::with_capacity(cases.len());
    let mut cb_recipient_id = Vec::with_capacity(cases.len());
    let mut sole_claimant_id = Vec::with_capacity(cases.len());
    let mut usual_resident_id = Vec::with_capacity(cases.len());
    offsets.push(0_usize);
    let mut cursor = 0_usize;
    for case in cases {
        cursor += case.cb_recipients.len();
        offsets.push(cursor);
        cb_claim_count.push(case.cb_claim_count);
        cb_recipient_id.push(case.cb_recipient_id.clone());
        sole_claimant_id.push(case.sole_claimant_id.clone());
        usual_resident_id.push(case.usual_resident_id.clone());
    }

    DenseBatchSpec {
        row_count: cases.len(),
        inputs: HashMap::from([
            (
                "cb_claim_count".to_string(),
                DenseColumn::Integer(cb_claim_count),
            ),
            (
                "cb_recipient_id".to_string(),
                DenseColumn::Text(cb_recipient_id),
            ),
            (
                "sole_claimant_id".to_string(),
                DenseColumn::Text(sole_claimant_id),
            ),
            (
                "usual_resident_id".to_string(),
                DenseColumn::Text(usual_resident_id),
            ),
        ]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "cb_receipt".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets,
                inputs: HashMap::new(),
            },
        )]),
    }
}

fn compare_scalar(explain: &OutputValue, dense: &DenseOutputValue, row: usize) {
    let OutputValue::Scalar { value, .. } = explain else {
        panic!("expected scalar output");
    };
    let DenseOutputValue::Scalar(dense_column) = dense else {
        panic!("expected dense scalar output");
    };
    let dense_value = dense_column.scalar_value_at(
        row,
        &match value {
            ScalarValueSpec::Bool { .. } => axiom_rules_engine::model::DType::Bool,
            ScalarValueSpec::Integer { .. } => axiom_rules_engine::model::DType::Integer,
            ScalarValueSpec::Decimal { .. } => axiom_rules_engine::model::DType::Decimal,
            ScalarValueSpec::Text { .. } => axiom_rules_engine::model::DType::Text,
            ScalarValueSpec::Date { .. } => axiom_rules_engine::model::DType::Date,
        },
    );
    match (value, dense_value) {
        (ScalarValueSpec::Bool { value }, axiom_rules_engine::model::ScalarValue::Bool(dense)) => {
            assert_eq!(*value, dense)
        }
        (ScalarValueSpec::Integer { value }, axiom_rules_engine::model::ScalarValue::Integer(dense)) => {
            assert_eq!(*value, dense)
        }
        (ScalarValueSpec::Decimal { value }, axiom_rules_engine::model::ScalarValue::Decimal(dense)) => {
            assert_eq!(decimal(value), dense)
        }
        (ScalarValueSpec::Text { value }, axiom_rules_engine::model::ScalarValue::Text(dense)) => {
            assert_eq!(value, &dense)
        }
        (ScalarValueSpec::Date { value }, axiom_rules_engine::model::ScalarValue::Date(dense)) => {
            assert_eq!(*value, dense)
        }
        other => panic!("mismatched scalar values: {other:?}"),
    }
}

fn compare_judgment(explain: &OutputValue, dense: &DenseOutputValue, row: usize) {
    let OutputValue::Judgment { outcome, .. } = explain else {
        panic!("expected judgment output");
    };
    let DenseOutputValue::Judgment(values) = dense else {
        panic!("expected dense judgment output");
    };
    let dense = match values[row] {
        axiom_rules_engine::model::JudgmentOutcome::Holds => JudgmentOutcomeSpec::Holds,
        axiom_rules_engine::model::JudgmentOutcome::NotHolds => JudgmentOutcomeSpec::NotHolds,
        axiom_rules_engine::model::JudgmentOutcome::Undetermined => JudgmentOutcomeSpec::Undetermined,
    };
    assert_eq!(*outcome, dense);
}

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn period_interval(period: &PeriodSpec) -> IntervalSpec {
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn family_allowance_dataset(
    period: &PeriodSpec,
    households: &[(&str, Vec<(&str, Decimal)>)],
) -> DatasetSpec {
    let interval = period_interval(period);
    let mut dataset = DatasetSpec::default();
    for (household_id, members) in households {
        for (person_id, income) in members {
            dataset.inputs.push(InputRecordSpec {
                name: "earned_income".to_string(),
                entity: "Person".to_string(),
                entity_id: (*person_id).to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: income.normalize().to_string(),
                },
            });
            dataset.relations.push(RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec![(*person_id).to_string(), (*household_id).to_string()],
                interval: interval.clone(),
            });
        }
    }
    dataset
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal")
}
