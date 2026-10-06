//! Verification probe: ordered comparisons (<, <=, >, >=) on Bool/Text operands.
//! Explain errors ("boolean comparisons only support == and !="); does bulk
//! (fast) or dense return a value instead?

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec,
    InputRecordSpec, IntervalSpec, JudgmentExprSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    RelationRecordSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec,
};

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
    }
}

fn interval() -> IntervalSpec {
    let p = period();
    IntervalSpec {
        start: p.start,
        end: p.end,
    }
}

fn describe_output(output: &OutputValue) -> String {
    match output {
        OutputValue::Scalar { value, .. } => format!("Scalar({value:?})"),
        OutputValue::Judgment { outcome, .. } => format!("Judgment({outcome:?})"),
    }
}

fn run_api(
    program: &ProgramSpec,
    dataset: &DatasetSpec,
    entity_id: &str,
    output: &str,
    mode: ExecutionMode,
) -> String {
    let result = execute_request(ExecutionRequest {
        mode: mode.clone(),
        program: program.clone(),
        dataset: dataset.clone(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
            period: period(),
            outputs: vec![output.to_string()],
        }],
    });
    match result {
        Ok(response) => format!(
            "OK {} actual_mode={:?} fallback_reason={:?}",
            describe_output(&response.results[0].outputs[output]),
            response.metadata.actual_mode,
            response.metadata.fallback_reason
        ),
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(
    artifact: &CompiledProgramArtifact,
    root: &str,
    batch: impl Fn() -> DenseBatchSpec,
    output: &str,
) -> [String; 2] {
    let dense = match DenseCompiledProgram::from_artifact(artifact, Some(root)) {
        Ok(dense) => dense,
        Err(error) => {
            let msg = format!("DENSE COMPILE ERR {error:?}");
            return [msg.clone(), msg];
        }
    };
    let model_period = period().to_model().unwrap();
    let outputs = vec![output.to_string()];
    let decimal = match dense.execute(&model_period, batch(), &outputs) {
        Ok(result) => format!("OK {:?}", result.outputs[output]),
        Err(error) => format!("ERR {error}"),
    };
    let f64_mode = match dense.execute_f64(&model_period, batch(), &outputs) {
        Ok(result) => format!("OK {:?}", result.outputs[output]),
        Err(error) => format!("ERR {error}"),
    };
    [decimal, f64_mode]
}

/// Case 1 (root, Bool): `flag < flag`, flag = true.
#[test]
fn case1_root_bool_ordered_compare() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: bool_lt
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag < flag
  - name: bool_gte
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag >= flag
"#;
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "flag".to_string(),
            entity: "Household".to_string(),
            entity_id: "household-1".to_string(),
            interval: interval(),
            value: ScalarValueSpec::Bool { value: true },
        }],
        relations: vec![],
    };
    for output in ["bool_lt", "bool_gte"] {
        let explain = run_api(
            &artifact.program,
            &dataset,
            "household-1",
            output,
            ExecutionMode::Explain,
        );
        let fast = run_api(
            &artifact.program,
            &dataset,
            "household-1",
            output,
            ExecutionMode::Fast,
        );
        let [dense_dec, dense_f64] = run_dense(
            &artifact,
            "Household",
            || DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([("flag".to_string(), DenseColumn::Bool(vec![true]))]),
                relations: HashMap::new(),
            },
            output,
        );
        println!("CASE1 {output} explain: {explain}");
        println!("CASE1 {output} fast:    {fast}");
        println!("CASE1 {output} dense execute:     {dense_dec}");
        println!("CASE1 {output} dense execute_f64: {dense_f64}");
    }
}

/// Case 2 (root, Text): `name_a > name_b`, "b" > "a" (lexically true).
#[test]
fn case2_root_text_ordered_compare() {
    let rulespec = r#"
format: rulespec/v1
rules:
  - name: text_gt
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: name_a > name_b
"#;
    let artifact = CompiledProgramArtifact::from_rulespec_str(rulespec).expect("compiles");
    let dataset = DatasetSpec {
        inputs: vec![
            InputRecordSpec {
                name: "name_a".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval(),
                value: ScalarValueSpec::Text {
                    value: "b".to_string(),
                },
            },
            InputRecordSpec {
                name: "name_b".to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval(),
                value: ScalarValueSpec::Text {
                    value: "a".to_string(),
                },
            },
        ],
        relations: vec![],
    };
    let output = "text_gt";
    let explain = run_api(
        &artifact.program,
        &dataset,
        "household-1",
        output,
        ExecutionMode::Explain,
    );
    let fast = run_api(
        &artifact.program,
        &dataset,
        "household-1",
        output,
        ExecutionMode::Fast,
    );
    let [dense_dec, dense_f64] = run_dense(
        &artifact,
        "Household",
        || DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([
                ("name_a".to_string(), DenseColumn::Text(vec!["b".to_string()])),
                ("name_b".to_string(), DenseColumn::Text(vec!["a".to_string()])),
            ]),
            relations: HashMap::new(),
        },
        output,
    );
    println!("CASE2 {output} explain: {explain}");
    println!("CASE2 {output} fast:    {fast}");
    println!("CASE2 {output} dense execute:     {dense_dec}");
    println!("CASE2 {output} dense execute_f64: {dense_f64}");
}

/// Case 3 (related predicate): count members where `is_flagged < true`.
#[test]
fn case3_related_bool_ordered_compare() {
    let program = ProgramSpec {
        relations: vec![RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![DerivedSpec {
            id: None,
            name: "flagged_count".to_string(),
            entity: "Household".to_string(),
            dtype: DTypeSpec::Integer,
            unit: None,
            rounding: None,
            source: None,
            period: None,
            source_url: None,
            corpus_citation_path: None,
            semantics: DerivedSemanticsSpec::Scalar {
                expr: ScalarExprSpec::CountRelated {
                    relation: "member_of_household".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    where_clause: Some(Box::new(JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Input {
                            name: "is_flagged".to_string(),
                        }),
                        op: ComparisonOpSpec::Lt,
                        right: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Bool { value: true },
                        }),
                    })),
                },
            },
            versions: vec![],
        }],
        ..ProgramSpec::default()
    };
    let artifact = CompiledProgramArtifact::compile(program).expect("compiles");
    let dataset = DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "is_flagged".to_string(),
            entity: "Person".to_string(),
            entity_id: "person-1".to_string(),
            interval: interval(),
            value: ScalarValueSpec::Bool { value: false },
        }],
        relations: vec![RelationRecordSpec {
            name: "member_of_household".to_string(),
            tuple: vec!["person-1".to_string(), "household-1".to_string()],
            interval: interval(),
        }],
    };
    let output = "flagged_count";
    let explain = run_api(
        &artifact.program,
        &dataset,
        "household-1",
        output,
        ExecutionMode::Explain,
    );
    let fast = run_api(
        &artifact.program,
        &dataset,
        "household-1",
        output,
        ExecutionMode::Fast,
    );
    let [dense_dec, dense_f64] = run_dense(
        &artifact,
        "Household",
        || DenseBatchSpec {
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
                    inputs: HashMap::from([(
                        "is_flagged".to_string(),
                        DenseColumn::Bool(vec![false]),
                    )]),
                },
            )]),
        },
        output,
    );
    println!("CASE3 {output} explain: {explain}");
    println!("CASE3 {output} fast:    {fast}");
    println!("CASE3 {output} dense execute:     {dense_dec}");
    println!("CASE3 {output} dense execute_f64: {dense_f64}");
}
