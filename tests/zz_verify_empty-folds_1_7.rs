//! Verification probe (candidate dense-undeclared-relation-or-slot-out-of-arity):
//! dense compiles and runs count/sum over an undeclared relation, or over
//! slots >= the relation's arity, where explain (and bulk -> fallback) error.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec, InputRecordSpec, IntervalSpec,
    PeriodKindSpec, PeriodSpec, ProgramSpec, RelatedValueRefSpec, RelationRecordSpec,
    RelationSpec, ScalarExprSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn derived(name: &str, dtype: DTypeSpec, expr: ScalarExprSpec) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype,
        unit: None,
        rounding: None,
        source: None,
        period: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemanticsSpec::Scalar { expr },
        versions: vec![],
    }
}

fn count(relation: &str, current_slot: usize, related_slot: usize) -> ScalarExprSpec {
    ScalarExprSpec::CountRelated {
        relation: relation.to_string(),
        current_slot,
        related_slot,
        where_clause: None,
    }
}

fn program() -> ProgramSpec {
    ProgramSpec {
        relations: vec![RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: None,
        }],
        derived: vec![
            // Control: a valid, declared relation with in-range slots.
            derived(
                "n_ok",
                DTypeSpec::Integer,
                count("member_of_household", 1, 0),
            ),
            // Undeclared relation.
            derived("n_undeclared", DTypeSpec::Integer, count("not_declared", 1, 0)),
            // related_slot >= arity.
            derived(
                "n_bad_slot",
                DTypeSpec::Integer,
                count("member_of_household", 1, 2),
            ),
            // current_slot >= arity.
            derived(
                "n_bad_current_slot",
                DTypeSpec::Integer,
                count("member_of_household", 5, 0),
            ),
            // sum over an undeclared relation.
            derived(
                "sum_undeclared",
                DTypeSpec::Decimal,
                ScalarExprSpec::SumRelated {
                    relation: "not_declared".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    value: RelatedValueRefSpec::Input {
                        name: "amount".to_string(),
                    },
                    where_clause: None,
                },
            ),
        ],
        ..ProgramSpec::default()
    }
}

const OUTPUTS: [&str; 5] = [
    "n_ok",
    "n_undeclared",
    "n_bad_slot",
    "n_bad_current_slot",
    "sum_undeclared",
];

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    let period = month_period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "amount".to_string(),
                entity: "Person".to_string(),
                entity_id: "p1".to_string(),
                interval: interval.clone(),
                value: ScalarValueSpec::Decimal {
                    value: "10".to_string(),
                },
            }],
            relations: vec![
                RelationRecordSpec {
                    name: "member_of_household".to_string(),
                    tuple: vec!["p1".to_string(), "h1".to_string()],
                    interval: interval.clone(),
                },
                RelationRecordSpec {
                    name: "member_of_household".to_string(),
                    tuple: vec!["p2".to_string(), "h1".to_string()],
                    interval,
                },
            ],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
            period,
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

fn key(name: &str, current_slot: usize, related_slot: usize) -> DenseRelationKey {
    DenseRelationKey {
        name: name.to_string(),
        current_slot,
        related_slot,
    }
}

fn dense_batch() -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([
            (
                key("member_of_household", 1, 0),
                DenseRelationBatchSpec {
                    offsets: vec![0, 2],
                    inputs: HashMap::new(),
                },
            ),
            (
                key("not_declared", 1, 0),
                DenseRelationBatchSpec {
                    offsets: vec![0, 3],
                    inputs: HashMap::from([(
                        "amount".to_string(),
                        DenseColumn::Decimal(vec![
                            Decimal::from(10),
                            Decimal::from(20),
                            Decimal::from(30),
                        ]),
                    )]),
                },
            ),
            (
                key("member_of_household", 1, 2),
                DenseRelationBatchSpec {
                    offsets: vec![0, 2],
                    inputs: HashMap::new(),
                },
            ),
            (
                key("member_of_household", 5, 0),
                DenseRelationBatchSpec {
                    offsets: vec![0, 4],
                    inputs: HashMap::new(),
                },
            ),
        ]),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, dense_batch(), &outputs)
    } else {
        dense.execute(&period, dense_batch(), &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn probe_dense_undeclared_relation_or_slot_out_of_arity() {
    let artifact = match CompiledProgramArtifact::compile(program()) {
        Ok(artifact) => {
            println!("COMPILE ProgramSpec artifact: OK");
            artifact
        }
        Err(error) => {
            println!("COMPILE ProgramSpec artifact: ERR {error}");
            panic!("artifact compile failed: {error}");
        }
    };

    // Round-trip through the artifact JSON boundary, as a shipped artifact would.
    let json = serde_json::to_string(&artifact).expect("artifact serialises");
    let reloaded: CompiledProgramArtifact =
        serde_json::from_str(&json).expect("artifact deserialises");
    println!("ARTIFACT JSON round-trip: OK ({} bytes)", json.len());

    let dense = match DenseCompiledProgram::from_artifact(&reloaded, Some("Household")) {
        Ok(dense) => {
            println!("DENSE compile: OK");
            dense
        }
        Err(error) => {
            println!("DENSE compile: ERR {error}");
            panic!("dense compile failed: {error}");
        }
    };
    for relation in dense.relations() {
        println!(
            "DENSE relation schema key: {}::{}/{}",
            relation.key.name, relation.key.current_slot, relation.key.related_slot
        );
    }

    let mut divergences = 0;
    for output in OUTPUTS {
        let explain = run_api(ExecutionMode::Explain, &reloaded, output);
        let fast = run_api(ExecutionMode::Fast, &reloaded, output);
        let dense_decimal = run_dense(&dense, output, false);
        let dense_f64 = run_dense(&dense, output, true);
        println!("[{output}] explain: {explain}");
        println!("[{output}] fast:    {fast}");
        println!("[{output}] dense:   {dense_decimal}");
        println!("[{output}] dense64: {dense_f64}");
        if explain.starts_with("ERR") && dense_decimal.starts_with("OK") {
            divergences += 1;
            println!("[{output}] DIVERGENCE: explain errors, dense returns a value");
        }
    }
    println!("TOTAL divergences (explain ERR, dense OK): {divergences}");
}

/// Reachability side-probe: does RuleSpec lowering let an undeclared relation
/// through to the ProgramSpec (as opposed to a hand-built artifact)?
#[test]
fn probe_rulespec_undeclared_relation_reachability() {
    let source = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: n_undeclared
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(not_declared)
"#;
    match CompiledProgramArtifact::from_rulespec_str(source) {
        Ok(artifact) => {
            println!(
                "RULESPEC len(not_declared): compiled OK; derived expr = {:?}",
                artifact
                    .program
                    .derived
                    .iter()
                    .find(|d| d.name == "n_undeclared")
                    .map(|d| &d.semantics)
            );
            match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
                Ok(dense) => {
                    for relation in dense.relations() {
                        println!(
                            "RULESPEC dense relation key: {}::{}/{}",
                            relation.key.name,
                            relation.key.current_slot,
                            relation.key.related_slot
                        );
                    }
                }
                Err(error) => println!("RULESPEC dense compile: ERR {error}"),
            }
        }
        Err(error) => println!("RULESPEC len(not_declared): compile ERR {error}"),
    }
}
