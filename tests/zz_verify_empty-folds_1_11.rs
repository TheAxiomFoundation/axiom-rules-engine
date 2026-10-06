//! Verification probe (offlens-bulk-related-derived-drops-rounding): a derived
//! relation whose predicate references a derived rule that declares currency
//! `rounding:`. Explain evaluates the derived through `evaluate_scalar`, which
//! applies the declared output rounding. The candidate claims bulk
//! (`eval_related_scalar_expr` Derived) and dense (`compile_related_scalar`
//! Derived / `compile_current_scalar_expr`) inline the derived's body and never
//! round, so the predicate sees the unrounded value.
//!
//! Variant A: the rounded derived lives on the CURRENT entity (Household).
//! Variant B: the rounded derived lives on the RELATED entity (Person).

use std::collections::HashMap;
use std::str::FromStr;

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
use rust_decimal::Decimal;

// Variant A: Household-level rounded derived in the relation predicate.
const RULESPEC_HH: &str = r#"
format: rulespec/v1
units:
  - name: GBP0
    kind: currency
    minor_units: 0
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: hh_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: GBP0
    rounding: half_up
    versions:
      - effective_from: 2026-01-01
        formula: hh_raw
  - name: eligible_members
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and hh_amount > 10
  - name: n_eligible
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(eligible_members)
"#;

// Variant A2: as A, but the derived relation declares slot entities so explain
// can resolve the Household derived to the household id in the relation context.
const RULESPEC_HH_SLOTS: &str = r#"
format: rulespec/v1
units:
  - name: GBP0
    kind: currency
    minor_units: 0
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: hh_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: GBP0
    rounding: half_up
    versions:
      - effective_from: 2026-01-01
        formula: hh_raw
  - name: eligible_members
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and hh_amount > 10
  - name: n_eligible
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(eligible_members)
"#;

// Variant B: Person-level (related entity) rounded derived in the predicate.
const RULESPEC_PERSON: &str = r#"
format: rulespec/v1
units:
  - name: GBP0
    kind: currency
    minor_units: 0
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: p_amount
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: GBP0
    rounding: half_up
    versions:
      - effective_from: 2026-01-01
        formula: p_raw
  - name: eligible_members
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and p_amount > 10
  - name: n_eligible
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
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

#[derive(Clone, Copy)]
enum Variant {
    Household,
    Person,
}

fn dataset(variant: Variant, raw: &str) -> DatasetSpec {
    let period = month_period();
    let input = match variant {
        Variant::Household => InputRecordSpec {
            name: "hh_raw".to_string(),
            entity: "Household".to_string(),
            entity_id: "h1".to_string(),
            interval: period_interval(&period),
            value: ScalarValueSpec::Decimal {
                value: raw.to_string(),
            },
        },
        Variant::Person => InputRecordSpec {
            name: "p_raw".to_string(),
            entity: "Person".to_string(),
            entity_id: "p1".to_string(),
            interval: period_interval(&period),
            value: ScalarValueSpec::Decimal {
                value: raw.to_string(),
            },
        },
    };
    DatasetSpec {
        inputs: vec![input],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["p1".to_string(), "h1".to_string()],
            interval: period_interval(&period),
        }],
    }
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    variant: Variant,
    raw: &str,
    entity_id: &str,
    output: &str,
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset(variant, raw),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: month_period(),
            outputs: vec![output.to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get(output)
                .map(fmt_output)
                .unwrap_or_else(|| "<missing>".to_string());
            format!(
                "OK {value} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(
    dense: &DenseCompiledProgram,
    variant: Variant,
    raw: &str,
    output: &str,
    f64_mode: bool,
) -> String {
    let period = month_period().to_model().expect("period converts");
    let value = Decimal::from_str(raw).expect("decimal");
    let column = if f64_mode {
        DenseColumn::Float(vec![raw.parse::<f64>().expect("f64")])
    } else {
        DenseColumn::Decimal(vec![value])
    };
    let (root_inputs, related_inputs) = match variant {
        Variant::Household => (
            HashMap::from([("hh_raw".to_string(), column)]),
            HashMap::new(),
        ),
        Variant::Person => (
            HashMap::new(),
            HashMap::from([("p_raw".to_string(), column)]),
        ),
    };
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: root_inputs,
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 1],
                inputs: related_inputs,
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
        Err(error) => format!("ERR {error}"),
    }
}

fn probe(label: &str, rulespec: &str, variant: Variant, rounded_rule: &str, raw: &str) {
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    println!("##### {label}: raw = {raw} #####");
    println!(
        "  explain n_eligible : {}",
        run_api(ExecutionMode::Explain, &artifact, variant, raw, "h1", "n_eligible")
    );
    println!(
        "  fast    n_eligible : {}",
        run_api(ExecutionMode::Fast, &artifact, variant, raw, "h1", "n_eligible")
    );
    let rounded_entity_id = match variant {
        Variant::Household => "h1",
        Variant::Person => "p1",
    };
    println!(
        "  explain {rounded_rule} (control) : {}",
        run_api(
            ExecutionMode::Explain,
            &artifact,
            variant,
            raw,
            rounded_entity_id,
            rounded_rule
        )
    );
    println!(
        "  fast    {rounded_rule} (control) : {}",
        run_api(
            ExecutionMode::Fast,
            &artifact,
            variant,
            raw,
            rounded_entity_id,
            rounded_rule
        )
    );
    match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
        Ok(dense) => {
            println!(
                "  dense decimal n_eligible : {}",
                run_dense(&dense, variant, raw, "n_eligible", false)
            );
            println!(
                "  dense f64     n_eligible : {}",
                run_dense(&dense, variant, raw, "n_eligible", true)
            );
            if let Variant::Household = variant {
                println!(
                    "  dense decimal {rounded_rule} (control) : {}",
                    run_dense(&dense, variant, raw, rounded_rule, false)
                );
            }
        }
        Err(error) => println!("  dense compile ERR: {error}"),
    }
}

#[test]
fn verify_related_derived_drops_rounding() {
    // Candidate case: 10.4 half_up -> 10; 10 > 10 is false in explain.
    probe("A household-derived", RULESPEC_HH, Variant::Household, "hh_amount", "10.4");
    // Control: 10.6 half_up -> 11; 11 > 10 holds everywhere.
    probe("A household-derived", RULESPEC_HH, Variant::Household, "hh_amount", "10.6");
    // Control: 9.6 half_up -> 10; unrounded 9.6 > 10 also false: all agree on 0.
    probe("A household-derived", RULESPEC_HH, Variant::Household, "hh_amount", "9.6");

    probe("A2 household-derived+slots", RULESPEC_HH_SLOTS, Variant::Household, "hh_amount", "10.4");
    probe("A2 household-derived+slots", RULESPEC_HH_SLOTS, Variant::Household, "hh_amount", "10.6");
    probe("A2 household-derived+slots", RULESPEC_HH_SLOTS, Variant::Household, "hh_amount", "9.6");

    probe("B person-derived", RULESPEC_PERSON, Variant::Person, "p_amount", "10.4");
    probe("B person-derived", RULESPEC_PERSON, Variant::Person, "p_amount", "10.6");
}
