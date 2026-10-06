//! Verification probe (dense-zero-member-root-projection): a derived relation
//! whose filter references a current-entity (Household) scalar or judgment.
//! Household-2 has zero members and `hh_b = 0`, so `hh_a / hh_b` divides by
//! zero there. Explain only evaluates the filter per member, so household-2
//! never evaluates it. Dense evaluates the root scalar/judgment over every
//! root row before projecting onto related rows.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};

// Variant A: filter compares a Household scalar (RootScalar path).
const RULESPEC_SCALAR: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: hh_ratio
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: hh_a / hh_b
  - name: eligible_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: EligibleUnit
      member_relation: eligible_members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and hh_ratio > 1
  - name: n_eligible
    kind: derived
    entity: EligibleUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(eligible_members)
"#;

// Variant B: filter references a Household judgment (RootJudgment path).
const RULESPEC_JUDGMENT: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: hh_ok
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: hh_a / hh_b > 1
  - name: eligible_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: EligibleUnit
      member_relation: eligible_members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and hh_ok
  - name: n_eligible
    kind: derived
    entity: EligibleUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(eligible_members)
"#;

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

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn hh_input(name: &str, household: &str, value: i64, period: &PeriodSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: "Household".to_string(),
        entity_id: household.to_string(),
        interval: period_interval(period),
        value: ScalarValueSpec::Integer { value },
    }
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    output: &str,
    query_ids: &[&str],
    h2_b: i64,
) -> String {
    let period = month_period();
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![
                hh_input("hh_a", "household-1", 10, &period),
                hh_input("hh_b", "household-1", 2, &period),
                hh_input("hh_a", "household-2", 10, &period),
                hh_input("hh_b", "household-2", h2_b, &period),
            ],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["person-1".to_string(), "household-1".to_string()],
                interval: period_interval(&period),
            }],
        },
        queries: query_ids
            .iter()
            .map(|entity_id| ExecutionQuery {
                assessment_date: None,
                entity_id: entity_id.to_string(),
                period: period.clone(),
                outputs: vec![output.to_string()],
            })
            .collect(),
    });
    match result {
        Ok(response) => {
            let values = response
                .results
                .iter()
                .map(|result| {
                    format!(
                        "{}={}",
                        result.entity_id,
                        result
                            .outputs
                            .get(output)
                            .map(fmt_output)
                            .unwrap_or_else(|| "<missing>".to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "OK [{values}] | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, h2_b: i64, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::from([
            ("hh_a".to_string(), DenseColumn::Integer(vec![10, 10])),
            ("hh_b".to_string(), DenseColumn::Integer(vec![2, h2_b])),
        ]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 1, 1],
                inputs: HashMap::new(),
            },
        )]),
    };
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, batch, &outputs)
    } else {
        dense.execute(&period, batch, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error} (debug: {error:?})"),
    }
}

fn probe(label: &str, rulespec: &str, household_derived: &str) {
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("EligibleUnit"))
        .expect("dense compiles");
    println!("##### {label} #####");
    for h2_b in [0_i64, 1] {
        println!("--- h2 hh_b = {h2_b} (h2 has zero members) ---");
        println!(
            "  explain n_eligible [h1,h2] : {}",
            run_api(
                ExecutionMode::Explain,
                &artifact,
                "n_eligible",
                &["household-1", "household-2"],
                h2_b
            )
        );
        println!(
            "  explain n_eligible [h2]    : {}",
            run_api(
                ExecutionMode::Explain,
                &artifact,
                "n_eligible",
                &["household-2"],
                h2_b
            )
        );
        println!(
            "  fast    n_eligible [h1,h2] : {}",
            run_api(
                ExecutionMode::Fast,
                &artifact,
                "n_eligible",
                &["household-1", "household-2"],
                h2_b
            )
        );
        println!(
            "  explain {household_derived} [h2] (control): {}",
            run_api(
                ExecutionMode::Explain,
                &artifact,
                household_derived,
                &["household-2"],
                h2_b
            )
        );
        println!(
            "  dense decimal n_eligible   : {}",
            run_dense(&dense, "n_eligible", h2_b, false)
        );
        println!(
            "  dense f64     n_eligible   : {}",
            run_dense(&dense, "n_eligible", h2_b, true)
        );
    }
}

#[test]
fn verify_dense_zero_member_root_projection() {
    probe(
        "A: RootScalar filter (hh_ratio > 1)",
        RULESPEC_SCALAR,
        "hh_ratio",
    );
    probe(
        "B: RootJudgment filter (hh_ok)",
        RULESPEC_JUDGMENT,
        "hh_ok",
    );
}
