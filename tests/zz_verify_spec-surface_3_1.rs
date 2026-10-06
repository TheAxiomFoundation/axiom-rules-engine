//! Verification probe (candidate spec-to-model-accepts-empty-extremum):
//! ScalarExprSpec::to_model accepts `max`/`min` with an empty `items` list, so
//! every serialized entry point (ProgramSpec JSON -> CompiledProgramArtifact::compile,
//! compiled-artifact JSON -> from_json_str, execute_request, DenseCompiledProgram)
//! loads the program. Explain then errors lazily, while fast (bulk) and dense
//! return the Decimal::MIN / Decimal::MAX seed.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseBatchSpec, DenseCompiledProgram};
use axiom_rules_engine::spec::{DatasetSpec, PeriodKindSpec, PeriodSpec, ProgramSpec};

const PROGRAM_JSON: &str = r#"{
  "units": [{"name":"USD","kind":"currency","minor_units":2}],
  "derived": [
    {"name":"m","entity":"Person","dtype":"decimal","unit":"USD","semantics":"scalar",
     "expr":{"kind":"max","items":[]},"versions":[]},
    {"name":"n","entity":"Person","dtype":"decimal","unit":"USD","semantics":"scalar",
     "expr":{"kind":"min","items":[]},"versions":[]},
    {"name":"m_plus","entity":"Person","dtype":"decimal","unit":"USD","semantics":"scalar",
     "expr":{"kind":"add","items":[{"kind":"max","items":[]},{"kind":"literal","value":{"kind":"decimal","value":"1"}}]},"versions":[]}
  ]
}"#;

const RULESPEC_EMPTY_MAX: &str = r#"
format: rulespec/v1
rules:
  - name: m
    kind: derived
    entity: Person
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: max()
"#;

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

fn run_api(mode: ExecutionMode, program: &ProgramSpec, output: &str) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: DatasetSpec {
            inputs: vec![],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "p1".to_string(),
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

fn run_dense(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::new(),
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
fn verify_spec_accepts_empty_extremum() {
    // 1. Serde accepts the node.
    let program: ProgramSpec = serde_json::from_str(PROGRAM_JSON).expect("serde accepts");
    println!("serde ProgramSpec parse: OK");

    // 2. ProgramSpec::to_program accepts it.
    match program.to_program() {
        Ok(_) => println!("ProgramSpec::to_program: OK (no emptiness check)"),
        Err(error) => println!("ProgramSpec::to_program: ERR {error}"),
    }

    // 3. CompiledProgramArtifact::compile accepts it.
    let artifact = match CompiledProgramArtifact::compile(program.clone()) {
        Ok(artifact) => {
            println!(
                "CompiledProgramArtifact::compile: OK fast_path.compatible={} blockers={:?}",
                artifact.metadata.fast_path.compatible, artifact.metadata.fast_path.blockers
            );
            Some(artifact)
        }
        Err(error) => {
            println!("CompiledProgramArtifact::compile: ERR {error}");
            None
        }
    };

    // 4. Compiled-artifact JSON round trip via from_json_str.
    if let Some(artifact) = &artifact {
        let json = serde_json::to_string(artifact).expect("artifact serializes");
        match CompiledProgramArtifact::from_json_str(&json) {
            Ok(_) => println!("CompiledProgramArtifact::from_json_str: OK"),
            Err(error) => println!("CompiledProgramArtifact::from_json_str: ERR {error}"),
        }
    }

    // 5. RuleSpec surface for comparison.
    let rulespec_artifact = match CompiledProgramArtifact::from_rulespec_str(RULESPEC_EMPTY_MAX) {
        Ok(artifact) => {
            println!("RuleSpec `formula: max()`: compiles OK");
            Some(artifact)
        }
        Err(error) => {
            println!("RuleSpec `formula: max()`: ERR {error}");
            None
        }
    };
    if let Some(rs) = &rulespec_artifact {
        println!("=== rulespec m ===");
        println!("  explain      : {}", run_api(ExecutionMode::Explain, &rs.program, "m"));
        println!("  fast         : {}", run_api(ExecutionMode::Fast, &rs.program, "m"));
        match DenseCompiledProgram::from_artifact(rs, Some("Person")) {
            Ok(dense) => {
                println!("  dense decimal: {}", run_dense(&dense, "m", false));
                println!("  dense f64    : {}", run_dense(&dense, "m", true));
            }
            Err(error) => println!("  dense        : compile ERR {error}"),
        }
    }

    // 6. Evaluate across explain / fast / dense.
    let dense = artifact
        .as_ref()
        .map(|artifact| DenseCompiledProgram::from_artifact(artifact, Some("Person")));
    for output in ["m", "n", "m_plus"] {
        println!("=== {output} ===");
        println!(
            "  explain      : {}",
            run_api(ExecutionMode::Explain, &program, output)
        );
        println!(
            "  fast         : {}",
            run_api(ExecutionMode::Fast, &program, output)
        );
        match &dense {
            Some(Ok(dense)) => {
                println!("  dense decimal: {}", run_dense(dense, output, false));
                println!("  dense f64    : {}", run_dense(dense, output, true));
            }
            Some(Err(error)) => println!("  dense        : compile ERR {error}"),
            None => println!("  dense        : no artifact"),
        }
    }
}
