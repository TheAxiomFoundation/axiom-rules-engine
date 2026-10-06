//! Verification probe (candidate dense-sum-where-evaluates-excluded-members):
//! dense SumRelated computes the member value for every related row before
//! applying the where-clause mask, so a Div-by-zero on a member the
//! where-clause EXCLUDES fails the whole aggregation, whereas explain skips
//! the excluded member (`continue` before `eval_related_value`).

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
use rust_decimal::Decimal;

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: member_ratio
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: 2025-01-01
        formula: inc / denom
  - name: total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    versions:
      - effective_from: 2025-01-01
        formula: sum_where(member_of_household, member_ratio, eligible)
"#;

fn june_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2025, 6, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2025, 6, 30).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = june_period();
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

/// members: (person id, inc, denom, eligible)
fn dataset(members: &[(&str, i64, i64, bool)]) -> DatasetSpec {
    let mut dataset = DatasetSpec::default();
    for (id, inc, denom, eligible) in members {
        let push = |dataset: &mut DatasetSpec, name: &str, value: ScalarValueSpec| {
            dataset.inputs.push(InputRecordSpec {
                name: name.to_string(),
                entity: "Person".to_string(),
                entity_id: (*id).to_string(),
                interval: interval(),
                value,
            });
        };
        push(
            &mut dataset,
            "inc",
            ScalarValueSpec::Decimal {
                value: inc.to_string(),
            },
        );
        push(
            &mut dataset,
            "denom",
            ScalarValueSpec::Decimal {
                value: denom.to_string(),
            },
        );
        push(
            &mut dataset,
            "eligible",
            ScalarValueSpec::Bool { value: *eligible },
        );
        dataset.relations.push(RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec![(*id).to_string(), "h1".to_string()],
            interval: interval(),
        });
    }
    dataset
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    members: &[(&str, i64, i64, bool)],
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset(members),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
            period: june_period(),
            outputs: vec!["total".to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get("total")
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
    members: &[(&str, i64, i64, bool)],
    f64_mode: bool,
) -> String {
    let period = june_period().to_model().expect("period converts");
    let inc: Vec<i64> = members.iter().map(|m| m.1).collect();
    let denom: Vec<i64> = members.iter().map(|m| m.2).collect();
    let eligible: Vec<bool> = members.iter().map(|m| m.3).collect();
    let (inc_col, denom_col) = if f64_mode {
        (
            DenseColumn::Float(inc.iter().map(|v| *v as f64).collect()),
            DenseColumn::Float(denom.iter().map(|v| *v as f64).collect()),
        )
    } else {
        (
            DenseColumn::Decimal(inc.iter().map(|v| Decimal::from(*v)).collect()),
            DenseColumn::Decimal(denom.iter().map(|v| Decimal::from(*v)).collect()),
        )
    };
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, members.len()],
                inputs: HashMap::from([
                    ("inc".to_string(), inc_col),
                    ("denom".to_string(), denom_col),
                    ("eligible".to_string(), DenseColumn::Bool(eligible)),
                ]),
            },
        )]),
    };
    let outputs = ["total".to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, batch, &outputs)
    } else {
        dense.execute(&period, batch, &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get("total")),
        Err(error) => format!("ERR {error}"),
    }
}

fn probe(label: &str, members: &[(&str, i64, i64, bool)]) -> (String, String, String, String) {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles");
    let explain = run_api(ExecutionMode::Explain, &artifact, members);
    let fast = run_api(ExecutionMode::Fast, &artifact, members);
    let dense_dec = run_dense(&dense, members, false);
    let dense_f64 = run_dense(&dense, members, true);
    println!("[{label}] explain   : {explain}");
    println!("[{label}] fast      : {fast}");
    println!("[{label}] dense dec : {dense_dec}");
    println!("[{label}] dense f64 : {dense_f64}");
    (explain, fast, dense_dec, dense_f64)
}

#[test]
fn verify_dense_sum_where_evaluates_excluded_members() {
    // Candidate case: p2 has denom=0 but is EXCLUDED by the where-clause.
    let (explain, fast, dense_dec, dense_f64) = probe(
        "excluded div0",
        &[("p1", 10, 2, true), ("p2", 10, 0, false)],
    );
    // Control 1: p2 excluded with a non-zero denom (all evaluators should agree on 5).
    probe(
        "control excluded ok",
        &[("p1", 10, 2, true), ("p2", 10, 5, false)],
    );
    // Control 2: p2 INCLUDED with denom=0 (all evaluators should error).
    probe(
        "control included div0",
        &[("p1", 10, 2, true), ("p2", 10, 0, true)],
    );

    let diverges = explain.starts_with("OK") && dense_dec.starts_with("ERR");
    println!(
        "DIVERGENCE (explain OK, dense dec ERR on excluded member): {diverges}; dense f64 ERR: {}; fast: {fast}",
        dense_f64.starts_with("ERR")
    );
}
