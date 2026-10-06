//! Verification probe (dense-compile-accepts-empty-extremum): a hand-built
//! `model::Program` containing `ScalarExpr::Max(vec![])` / `Min(vec![])`
//! reaches `DenseCompiledProgram::from_program` with no spec validation. This
//! probe checks (a) whether the dense COMPILER rejects the empty extremum at
//! compile time (root, related-row, current-in-related and lifetime compile
//! paths), (b) what dense execution returns, and (c) what the explain engine
//! (`Engine::new` + `evaluate_scalar`) does over the very same `Program`.
//! For context it also runs the RuleSpec `max()` path through
//! `execute_request` in Explain and Fast mode.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::{Engine, EvalError};
use axiom_rules_engine::model::{
    DType, DataSet, Derived, DerivedSemantics, Interval, OverPeriodsKind, Period, PeriodKind,
    Program, RelatedValueRef, ScalarExpr, ScalarValue,
};
use axiom_rules_engine::spec::{DatasetSpec, PeriodKindSpec, PeriodSpec};
use rust_decimal::Decimal;

fn derived(name: &str, entity: &str, expr: ScalarExpr) -> Derived {
    Derived {
        id: None,
        name: name.to_string(),
        entity: entity.to_string(),
        dtype: DType::Decimal,
        unit: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemantics::Scalar(expr),
        versions: vec![],
    }
}

fn lit(v: i64) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Decimal(Decimal::from(v)))
}

fn month() -> Period {
    Period::month(2026, 1)
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: chrono::NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
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

fn fmt_dense(
    result: std::thread::Result<Result<DenseExecutionResult, EvalError>>,
    output: &str,
) -> String {
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(result)) => format!("OK {:?}", result.outputs.get(output)),
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

fn fmt_explain(result: std::thread::Result<Result<ScalarValue, EvalError>>) -> String {
    match result {
        Err(panic) => format!("PANIC {}", panic_text(&panic)),
        Ok(Ok(value)) => format!("OK {value:?}"),
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

/// Hand-built program: no RuleSpec, no ProgramSpec, no validation.
fn hand_built_program() -> Program {
    let mut p = Program::default();
    p.add_relation("member_of_family", 2).expect("relation");
    // Root (Person) rules.
    p.add_derived(derived("m", "Person", ScalarExpr::Max(vec![])))
        .expect("m");
    p.add_derived(derived("n", "Person", ScalarExpr::Min(vec![])))
        .expect("n");
    // Plausible-looking: max(5, min()) -> dense Decimal::MAX, explain error.
    p.add_derived(derived(
        "plaus",
        "Person",
        ScalarExpr::Max(vec![lit(5), ScalarExpr::Min(vec![])]),
    ))
    .expect("plaus");
    // Plausible-looking: 7 + max() -> dense 7 + Decimal::MIN.
    p.add_derived(derived(
        "plus_max",
        "Person",
        ScalarExpr::Add(vec![lit(7), ScalarExpr::Max(vec![])]),
    ))
    .expect("plus_max");
    // Family root: related-row compile path (compile_related_scalar) via a
    // Person-entity dependency summed over the relation.
    p.add_derived(derived(
        "fam_sum_m",
        "Family",
        ScalarExpr::SumRelated {
            relation: "member_of_family".to_string(),
            current_slot: 1,
            related_slot: 0,
            value: RelatedValueRef::Derived("m".to_string()),
            where_clause: None,
        },
    ))
    .expect("fam_sum_m");
    // Family-entity dependency referenced as a related value: this goes
    // through compile_current_scalar_expr (RootScalar) in the dense compiler.
    p.add_derived(derived("fam_n", "Family", ScalarExpr::Min(vec![])))
        .expect("fam_n");
    p.add_derived(derived(
        "fam_sum_cur",
        "Family",
        ScalarExpr::SumRelated {
            relation: "member_of_family".to_string(),
            current_slot: 1,
            related_slot: 0,
            value: RelatedValueRef::Derived("fam_n".to_string()),
            where_clause: None,
        },
    ))
    .expect("fam_sum_cur");
    // Worker lifetime: sum_over_periods(earnings) + min().
    p.add_derived(derived(
        "life",
        "Worker",
        ScalarExpr::Add(vec![
            ScalarExpr::OverPeriods {
                kind: OverPeriodsKind::Sum,
                value: Box::new(ScalarExpr::Input("earnings".to_string())),
                n: None,
            },
            ScalarExpr::Min(vec![]),
        ]),
    ))
    .expect("life");
    p
}

fn dataset() -> DataSet {
    let mut data = DataSet::default();
    let interval = Interval {
        start: month().start,
        end: month().end,
    };
    for child in ["child-1", "child-2"] {
        data.add_relation(
            "member_of_family",
            vec![child.to_string(), "family-1".to_string()],
            interval.clone(),
        );
    }
    data
}

fn run_explain(program: &Program, data: &DataSet, name: &str, entity_id: &str) -> String {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut engine = Engine::new(program, data);
        engine.evaluate_scalar(name, entity_id, &month())
    }));
    fmt_explain(result)
}

fn run_dense_root(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::new(),
    };
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&month(), batch, &outputs)
        } else {
            dense.execute(&month(), batch, &outputs)
        }
    }));
    fmt_dense(result, output)
}

fn run_dense_family(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member_of_family".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2],
                inputs: HashMap::new(),
            },
        )]),
    };
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_f64(&month(), batch, &outputs)
        } else {
            dense.execute(&month(), batch, &outputs)
        }
    }));
    fmt_dense(result, output)
}

fn run_dense_lifetime(dense: &DenseCompiledProgram, output: &str, f64_mode: bool) -> String {
    let periods = vec![year(2001), year(2002), year(2003)];
    let batches = [100.0, 250.0, 50.0]
        .into_iter()
        .map(|v| DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([("earnings".to_string(), DenseColumn::Float(vec![v]))]),
            relations: HashMap::new(),
        })
        .collect::<Vec<_>>();
    let outputs = [output.to_string()];
    let result = catch_unwind(AssertUnwindSafe(|| {
        if f64_mode {
            dense.execute_lifetime_f64(&periods, batches, &outputs)
        } else {
            dense.execute_lifetime(&periods, batches, &outputs)
        }
    }));
    fmt_dense(result, output)
}

fn compile(program: &Program, entity: &str) -> Option<DenseCompiledProgram> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        DenseCompiledProgram::from_program(program, Some(entity))
    }));
    match result {
        Err(panic) => {
            println!(
                "  from_program({entity}) : PANIC {}",
                panic_text(&panic)
            );
            None
        }
        Ok(Err(error)) => {
            println!("  from_program({entity}) : COMPILE ERR {error}");
            None
        }
        Ok(Ok(dense)) => {
            println!(
                "  from_program({entity}) : COMPILED OK (outputs {:?})",
                {
                    let mut names = dense.output_names();
                    names.sort();
                    names
                }
            );
            Some(dense)
        }
    }
}

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: out_max
    kind: derived
    entity: Person
    dtype: Money
    versions:
      - effective_from: 2026-01-01
        formula: max()
"#;

fn month_spec() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    // No inputs: `max()` references nothing, so an empty dataset suffices.
    let dataset = DatasetSpec::default();
    let result = catch_unwind(AssertUnwindSafe(|| {
        execute_request(ExecutionRequest {
            mode,
            program: artifact.program.clone(),
            dataset,
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "child-1".to_string(),
                period: month_spec(),
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
                .map(|v| match v {
                    OutputValue::Scalar { value, .. } => format!("{value:?}"),
                    OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
                })
                .unwrap_or_else(|| "<missing>".to_string());
            format!(
                "OK {value} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Ok(Err(error)) => format!("ERR {error}"),
    }
}

#[test]
fn verify_dense_compile_accepts_empty_extremum() {
    let program = hand_built_program();
    let data = dataset();

    println!("=== DENSE COMPILE of hand-built Program (no spec validation) ===");
    let dense_person = compile(&program, "Person");
    let dense_family = compile(&program, "Family");
    let dense_worker = compile(&program, "Worker");

    println!("=== ROOT (Person, compile_scalar_expr) ===");
    for output in ["m", "n", "plaus", "plus_max"] {
        println!("--- {output} ---");
        println!(
            "  explain (Engine::new+evaluate_scalar, same Program): {}",
            run_explain(&program, &data, output, "child-1")
        );
        if let Some(dense) = &dense_person {
            println!("  dense decimal: {}", run_dense_root(dense, output, false));
            println!("  dense f64    : {}", run_dense_root(dense, output, true));
        }
    }

    println!("=== RELATED (Family, compile_related_scalar / compile_current_scalar_expr) ===");
    for output in ["fam_sum_m", "fam_sum_cur"] {
        println!("--- {output} (family-1 with 2 children) ---");
        println!(
            "  explain (Engine::new+evaluate_scalar, same Program): {}",
            run_explain(&program, &data, output, "family-1")
        );
        if let Some(dense) = &dense_family {
            println!("  dense decimal: {}", run_dense_family(dense, output, false));
            println!("  dense f64    : {}", run_dense_family(dense, output, true));
        }
    }

    println!("=== LIFETIME (Worker, compile_scalar_expr reused by LifetimeExecutor) ===");
    println!(
        "  explain (per-period Engine, same Program, 2026-01): {}",
        run_explain(&program, &data, "life", "worker-1")
    );
    if let Some(dense) = &dense_worker {
        println!("  dense lifetime decimal: {}", run_dense_lifetime(dense, "life", false));
        println!("  dense lifetime f64    : {}", run_dense_lifetime(dense, "life", true));
    }

    println!("=== CONTEXT: RuleSpec `max()` through execute_request ===");
    match CompiledProgramArtifact::from_rulespec_str(RULESPEC) {
        Err(error) => println!("  rulespec compile: ERR {error}"),
        Ok(artifact) => {
            println!("  rulespec compile: OK (spec layer accepts `max()`)");
            println!("  explain: {}", run_api(ExecutionMode::Explain, &artifact, "out_max"));
            println!("  fast   : {}", run_api(ExecutionMode::Fast, &artifact, "out_max"));
        }
    }
}
