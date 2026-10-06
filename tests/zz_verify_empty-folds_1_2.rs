//! Verification probe (candidate dense-related-bool-text-ordering-value):
//! a count_related / sum_related where-clause or a derived-relation predicate
//! that uses an ordering operator (<, <=, >, >=) on Bool/Bool or Text/Text.
//! Explain rejects these via compare_scalar_values ("boolean comparisons only
//! support == and !=" / "text comparisons ..."). The dense related path
//! (compare_related_columns) is claimed to return `false` for the member,
//! silently excluding it. Prints explain, fast (with fallback metadata) and
//! dense results for each case.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    RelationRecordSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = month_period();
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn program() -> ProgramSpec {
    let bool_gte = serde_json::json!({
        "kind": "comparison",
        "left": {"kind": "input", "name": "flag"},
        "op": "gte",
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    let bool_gt = serde_json::json!({
        "kind": "comparison",
        "left": {"kind": "input", "name": "flag"},
        "op": "gt",
        "right": {"kind": "literal", "value": {"kind": "bool", "value": false}}
    });
    let bool_eq = serde_json::json!({
        "kind": "comparison",
        "left": {"kind": "input", "name": "flag"},
        "op": "eq",
        "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}
    });
    let text_lt = serde_json::json!({
        "kind": "comparison",
        "left": {"kind": "input", "name": "label"},
        "op": "lt",
        "right": {"kind": "literal", "value": {"kind": "text", "value": "c"}}
    });
    serde_json::from_value(serde_json::json!({
        "relations": [
            {
                "name": "member_of_household",
                "arity": 2,
                "slot_entities": ["Person", "Household"]
            },
            {
                "name": "flagged_member_of_household",
                "arity": 2,
                "slot_entities": ["Person", "Household"],
                "derivation": {
                    "source_relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "slot_entities": ["Person", "Household"],
                    "predicate": bool_gte.clone()
                }
            }
        ],
        "derived": [
            {
                "name": "count_bool_eq_control",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": bool_eq
                }
            },
            {
                "name": "count_bool_gte",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": bool_gte
                }
            },
            {
                "name": "count_text_lt",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": text_lt
                }
            },
            {
                "name": "sum_bool_gt",
                "entity": "Household",
                "dtype": "decimal",
                "semantics": "scalar",
                "expr": {
                    "kind": "sum_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "value": {"kind": "input", "name": "amount"},
                    "where": bool_gt
                }
            },
            {
                "name": "count_derived_relation",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "flagged_member_of_household",
                    "current_slot": 1,
                    "related_slot": 0
                }
            }
        ]
    }))
    .expect("ProgramSpec parses")
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn person_input(name: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: "Person".to_string(),
        entity_id: "p1".to_string(),
        interval: interval(),
        value,
    }
}

fn dataset() -> DatasetSpec {
    DatasetSpec {
        inputs: vec![
            person_input("flag", ScalarValueSpec::Bool { value: true }),
            person_input(
                "label",
                ScalarValueSpec::Text {
                    value: "b".to_string(),
                },
            ),
            person_input(
                "amount",
                ScalarValueSpec::Decimal {
                    value: "5".to_string(),
                },
            ),
        ],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["p1".to_string(), "h1".to_string()],
            interval: interval(),
        }],
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
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

fn run_dense(dense: &DenseCompiledProgram, output: &str) -> String {
    let period = month_period().to_model().expect("period converts");
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
                offsets: vec![0, 1],
                inputs: HashMap::from([
                    ("flag".to_string(), DenseColumn::Bool(vec![true])),
                    ("label".to_string(), DenseColumn::Text(vec!["b".to_string()])),
                    (
                        "amount".to_string(),
                        DenseColumn::Decimal(vec![Decimal::from(5)]),
                    ),
                ]),
            },
        )]),
    };
    let outputs = [output.to_string()];
    match dense.execute(&period, batch, &outputs) {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_dense_related_bool_text_ordering_silently_excludes() {
    let artifact = CompiledProgramArtifact::compile(program()).expect("compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"));
    let mut divergences = 0;
    for output in [
        "count_bool_eq_control",
        "count_bool_gte",
        "count_text_lt",
        "sum_bool_gt",
        "count_derived_relation",
    ] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        let dense_result = match &dense {
            Ok(dense) => run_dense(dense, output),
            Err(error) => format!("DENSE-COMPILE-ERR {error}"),
        };
        println!("[{output}] explain: {explain}");
        println!("[{output}] fast:    {fast}");
        println!("[{output}] dense:   {dense_result}");
        if explain.starts_with("ERR") && dense_result.starts_with("OK") {
            divergences += 1;
            println!("[{output}] DIVERGENCE: explain errors, dense returns a value");
        }
        if explain.starts_with("ERR") && fast.starts_with("OK") && fast.contains("actual_mode=Fast")
        {
            println!("[{output}] FAST DIVERGENCE: explain errors, fast returns without fallback");
        }
    }
    println!("dense divergences: {divergences}");
}

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_flag_ordered
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag > other_flag
  - name: ordered_member_count
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, member_flag_ordered)
"#;

#[test]
fn verify_rulespec_count_where_bool_ordering() {
    let artifact = match CompiledProgramArtifact::from_rulespec_str(RULESPEC) {
        Ok(artifact) => artifact,
        Err(error) => {
            println!("[rulespec] COMPILE-ERR {error}");
            return;
        }
    };
    let output = "ordered_member_count";
    let mut data = dataset();
    data.inputs.retain(|input| input.name == "flag");
    data.inputs.push(person_input(
        "other_flag",
        ScalarValueSpec::Bool { value: false },
    ));
    let run = |mode: ExecutionMode| -> String {
        match execute_request(ExecutionRequest {
            mode,
            program: artifact.program.clone(),
            dataset: data.clone(),
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "h1".to_string(),
                period: month_period(),
                outputs: vec![output.to_string()],
            }],
        }) {
            Ok(response) => format!(
                "OK {} | actual_mode={:?} fallback_reason={:?}",
                response.results[0]
                    .outputs
                    .get(output)
                    .map(fmt_output)
                    .unwrap_or_else(|| "<missing>".to_string()),
                response.metadata.actual_mode,
                response.metadata.fallback_reason
            ),
            Err(error) => format!("ERR {error}"),
        }
    };
    println!("[rulespec {output}] explain: {}", run(ExecutionMode::Explain));
    println!("[rulespec {output}] fast:    {}", run(ExecutionMode::Fast));
    let dense = match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
        Ok(dense) => dense,
        Err(error) => {
            println!("[rulespec {output}] dense: DENSE-COMPILE-ERR {error}");
            return;
        }
    };
    let period = month_period().to_model().expect("period converts");
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
                offsets: vec![0, 1],
                inputs: HashMap::from([
                    ("flag".to_string(), DenseColumn::Bool(vec![true])),
                    ("other_flag".to_string(), DenseColumn::Bool(vec![false])),
                ]),
            },
        )]),
    };
    let dense_result = match dense.execute(&period, batch, &[output.to_string()]) {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    };
    println!("[rulespec {output}] dense:   {dense_result}");
}
