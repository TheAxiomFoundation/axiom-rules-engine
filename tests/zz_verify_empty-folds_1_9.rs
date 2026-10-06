//! Verification probe (candidate dense-empty-bool-related-column): an empty
//! related column typed Bool (python-ext turns a plain `[]` into Bool) makes
//! dense `sum(rel.x)` and a numeric `count_where` predicate fail over zero
//! members, where explain returns 0 without inspecting any value type.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
};

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: total
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: GBP
    versions:
      - effective_from: '2025-01-01'
        formula: sum(member_of_household.inc)
  - name: inc_positive
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: inc > 0
  - name: n_pos
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: count_where(member_of_household, inc_positive)
  - name: n_all
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2025-01-01'
        formula: len(member_of_household)
"#;

const OUTPUTS: [&str; 3] = ["total", "n_pos", "n_all"];

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    let period = month_period();
    // Two households, zero members each. A relation record for an unrelated
    // pair (p-other in h-other) keeps the relation name known to the dataset
    // without giving h1/h2 any members.
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![RelationRecordSpec {
                name: "member_of_household".to_string(),
                tuple: vec!["p-other".to_string(), "h-other".to_string()],
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
            }],
        },
        queries: ["h1", "h2"]
            .iter()
            .map(|id| ExecutionQuery {
                assessment_date: None,
                entity_id: (*id).to_string(),
                period: period.clone(),
                outputs: vec![output.to_string()],
            })
            .collect(),
    });
    match result {
        Ok(response) => {
            let values: Vec<String> = response
                .results
                .iter()
                .map(|r| {
                    r.outputs
                        .get(output)
                        .map(fmt_output)
                        .unwrap_or_else(|| "<missing>".to_string())
                })
                .collect();
            format!(
                "OK {values:?} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(
    dense: &DenseCompiledProgram,
    output: &str,
    inc: DenseColumn,
    f64_mode: bool,
) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_household".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 0, 0],
                inputs: HashMap::from([("inc".to_string(), inc)]),
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

#[test]
fn verify_dense_empty_bool_related_column() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(RULESPEC).expect("compiles");
    let dense =
        DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles");
    println!("dense relations: {:?}", dense.relations());

    let mut divergences = 0;
    for output in OUTPUTS {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        println!("[{output}] explain            : {explain}");
        println!("[{output}] fast               : {fast}");
        let cases: [(&str, DenseColumn, bool); 6] = [
            ("dense Decimal Bool([])", DenseColumn::Bool(vec![]), false),
            ("dense f64     Bool([])", DenseColumn::Bool(vec![]), true),
            ("dense Decimal Decimal([]) (control)", DenseColumn::Decimal(vec![]), false),
            ("dense Decimal Integer([]) (control)", DenseColumn::Integer(vec![]), false),
            ("dense f64     Float([]) (control)", DenseColumn::Float(vec![]), true),
            ("dense Decimal Text([])", DenseColumn::Text(vec![]), false),
        ];
        for (label, column, f64_mode) in cases {
            let result = run_dense(&dense, output, column, f64_mode);
            println!("[{output}] {label:<38}: {result}");
            if label.contains("Bool([])") && result.starts_with("ERR") && explain.starts_with("OK")
            {
                divergences += 1;
            }
        }
    }
    println!("DIVERGENCES (explain OK, dense Bool([]) ERR): {divergences}");
}
