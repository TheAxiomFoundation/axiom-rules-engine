//! Verification probe (formula-accepts-empty-extremum): RuleSpec formula
//! lowering (src/formula.rs lower_to_scalar "max"/"min") accepts `max()` /
//! `min()` with zero arguments, while sibling functions (ceil/floor/
//! days_between) reject a wrong arity at compile time. The resulting
//! artifact compiles, round-trips through JSON, and loads into dense; at
//! runtime explain errors, while fast (bulk) and dense return sentinels or a
//! plausible value.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

/// Exact repro from the candidate.
const REPRO: &str = r#"
format: rulespec/v1
module: {title: probe}
rules:
  - name: m
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: max()
"#;

/// Runtime probe: empty and "plausible" nested variants.
const RUNTIME: &str = r#"
format: rulespec/v1
module: {title: probe}
rules:
  - name: out_max
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: max()
  - name: out_min
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: min()
  - name: plaus_max
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: max(income, max())
  - name: plaus_min
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: min(income, min())
"#;

fn sibling_rulespec(formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
module: {{title: probe}}
rules:
  - name: s
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-01-01
        formula: {formula}
"#
    )
}

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

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

fn dataset(with_income: bool) -> DatasetSpec {
    if !with_income {
        return DatasetSpec::default();
    }
    DatasetSpec {
        inputs: vec![InputRecordSpec {
            name: "income".to_string(),
            entity: "Person".to_string(),
            entity_id: "p1".to_string(),
            interval: IntervalSpec {
                start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
                end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("date"),
            },
            value: ScalarValueSpec::Decimal {
                value: "100".to_string(),
            },
        }],
        relations: vec![],
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    run_api_with(mode, artifact, output, true)
}

fn run_api_with(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    output: &str,
    with_income: bool,
) -> String {
    let result = catch_unwind(AssertUnwindSafe(|| {
        execute_request(ExecutionRequest {
            mode,
            program: artifact.program.clone(),
            dataset: dataset(with_income),
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "p1".to_string(),
                period: month_period(),
                outputs: vec![output.to_string()],
            }],
        })
    }));
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(response)) => {
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
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([(
            "income".to_string(),
            DenseColumn::Decimal(vec![Decimal::from(100)]),
        )]),
        relations: HashMap::new(),
    };
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&period, batch, &outputs)
        } else {
            dense.execute(&period, batch, &outputs)
        }
    }));
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(result)) => format!("OK {:?}", result.outputs.get(output)),
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

#[test]
fn verify_formula_accepts_empty_extremum() {
    // 1. Compile-time surface: exact repro.
    println!("=== compile: exact repro (max()) ===");
    match CompiledProgramArtifact::from_rulespec_str(REPRO) {
        Ok(artifact) => {
            let derived = &artifact.program.derived[0];
            println!(
                "  from_rulespec_str: OK; derived[0] = {}",
                serde_json::to_string(derived).expect("serialize derived")
            );
            let json = serde_json::to_string(&artifact).expect("serialize artifact");
            match CompiledProgramArtifact::from_json_str(&json) {
                Ok(_) => println!("  from_json_str (artifact reload): OK"),
                Err(error) => println!("  from_json_str (artifact reload): ERR {error}"),
            }
            match DenseCompiledProgram::from_artifact(&artifact, Some("Person")) {
                Ok(_) => println!("  DenseCompiledProgram::from_artifact: OK"),
                Err(error) => println!("  DenseCompiledProgram::from_artifact: ERR {error}"),
            }
            println!(
                "  explain      : {}",
                run_api_with(ExecutionMode::Explain, &artifact, "m", false)
            );
            println!(
                "  fast         : {}",
                run_api_with(ExecutionMode::Fast, &artifact, "m", false)
            );
            if let Ok(dense) = DenseCompiledProgram::from_artifact(&artifact, Some("Person")) {
                println!("  dense decimal: {}", run_dense(&dense, "m", false));
                println!("  dense f64    : {}", run_dense(&dense, "m", true));
            }
        }
        Err(error) => println!("  from_rulespec_str: ERR {error}"),
    }

    // 2. Sibling arity checks for comparison.
    println!("=== compile: sibling arity checks ===");
    for formula in [
        "min()",
        "max()",
        "ceil()",
        "floor()",
        "days_between()",
        "ceil(1, 2)",
    ] {
        let source = sibling_rulespec(formula);
        match CompiledProgramArtifact::from_rulespec_str(&source) {
            Ok(artifact) => println!(
                "  {formula:<16}: compile OK; expr = {}",
                serde_json::to_string(&artifact.program.derived[0]).expect("serialize")
            ),
            Err(error) => println!("  {formula:<16}: compile ERR {error}"),
        }
    }

    // 3. Runtime divergence across evaluators.
    let artifact = CompiledProgramArtifact::from_rulespec_str(RUNTIME).expect("runtime compiles");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Person"));
    for output in ["out_max", "out_min", "plaus_max", "plaus_min"] {
        println!("=== runtime: {output} (income = 100) ===");
        println!("  explain      : {}", run_api(ExecutionMode::Explain, &artifact, output));
        println!("  fast         : {}", run_api(ExecutionMode::Fast, &artifact, output));
        match &dense {
            Ok(dense) => {
                println!("  dense decimal: {}", run_dense(dense, output, false));
                println!("  dense f64    : {}", run_dense(dense, output, true));
            }
            Err(error) => println!("  dense        : compile ERR {error}"),
        }
    }
}
