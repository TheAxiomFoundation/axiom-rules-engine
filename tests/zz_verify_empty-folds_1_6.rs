//! Verification probe (candidate bulk-integer-decimal-output-kind): fast mode
//! reports `decimal` kind where explain reports `integer` for an if() with
//! Integer/Decimal branches, and for Integer values in non-Integer rules.

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec, InputRecordSpec,
    IntervalSpec, JudgmentExprSpec, PeriodKindSpec, PeriodSpec, ProgramSpec, ScalarExprSpec,
    ScalarValueSpec,
};

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn input(entity_id: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: "x".to_string(),
        entity: "Person".to_string(),
        entity_id: entity_id.to_string(),
        interval: IntervalSpec {
            start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
            end: chrono::NaiveDate::from_ymd_opt(2026, 12, 31).expect("date"),
        },
        value,
    }
}

fn derived(name: &str, dtype: DTypeSpec, expr: ScalarExprSpec) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Person".to_string(),
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

fn if_expr(then_value: ScalarValueSpec, else_value: ScalarValueSpec) -> ScalarExprSpec {
    ScalarExprSpec::If {
        condition: Box::new(JudgmentExprSpec::Comparison {
            left: Box::new(ScalarExprSpec::Input {
                name: "x".to_string(),
            }),
            op: ComparisonOpSpec::Gt,
            right: Box::new(ScalarExprSpec::Literal {
                value: ScalarValueSpec::Integer { value: 0 },
            }),
        }),
        then_expr: Box::new(ScalarExprSpec::Literal { value: then_value }),
        else_expr: Box::new(ScalarExprSpec::Literal { value: else_value }),
    }
}

fn int(value: i64) -> ScalarValueSpec {
    ScalarValueSpec::Integer { value }
}

fn dec(value: &str) -> ScalarValueSpec {
    ScalarValueSpec::Decimal {
        value: value.to_string(),
    }
}

fn program() -> ProgramSpec {
    let input_x = || ScalarExprSpec::Input {
        name: "x".to_string(),
    };
    ProgramSpec {
        derived: vec![
            derived("v_int", DTypeSpec::Integer, if_expr(int(1), dec("2.5"))),
            derived("v_dec", DTypeSpec::Decimal, if_expr(int(1), dec("2.5"))),
            derived("v_dec_intint", DTypeSpec::Decimal, if_expr(int(1), int(2))),
            derived("passthru", DTypeSpec::Decimal, input_x()),
            derived("passthru_int", DTypeSpec::Integer, input_x()),
        ],
        ..ProgramSpec::default()
    }
}

fn fmt_output(value: Option<&OutputValue>) -> String {
    match value {
        Some(OutputValue::Scalar { value, dtype, .. }) => format!(
            "{} (rule dtype {:?})",
            serde_json::to_string(value).expect("value serialises"),
            dtype
        ),
        Some(OutputValue::Judgment { outcome, .. }) => format!("{outcome:?}"),
        None => "<missing>".to_string(),
    }
}

fn run(
    mode: ExecutionMode,
    program: ProgramSpec,
    dataset: DatasetSpec,
    outputs: &[&str],
) -> Vec<String> {
    let queries = ["p1", "p2"]
        .iter()
        .map(|id| ExecutionQuery {
            assessment_date: None,
            entity_id: id.to_string(),
            period: month_period(),
            outputs: outputs.iter().map(|o| o.to_string()).collect(),
        })
        .collect();
    match execute_request(ExecutionRequest {
        mode,
        program,
        dataset,
        queries,
    }) {
        Ok(response) => {
            let mut lines = vec![format!(
                "actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )];
            for result in &response.results {
                for output in outputs {
                    lines.push(format!(
                        "{} {output}: {}",
                        result.entity_id,
                        fmt_output(result.outputs.get(*output))
                    ));
                }
            }
            lines
        }
        Err(error) => vec![format!("ERR {error}")],
    }
}

fn compare(label: &str, program: ProgramSpec, dataset: DatasetSpec, outputs: &[&str]) {
    let explain = run(
        ExecutionMode::Explain,
        program.clone(),
        dataset.clone(),
        outputs,
    );
    let fast = run(ExecutionMode::Fast, program, dataset, outputs);
    println!("=== {label} ===");
    for line in &explain {
        println!("  explain: {line}");
    }
    for line in &fast {
        println!("  fast   : {line}");
    }
    // Skip the metadata line (index 0) when reporting diverging rows.
    for (e, f) in explain.iter().zip(fast.iter()).skip(1) {
        if e != f {
            println!("  DIVERGE: explain `{e}` vs fast `{f}`");
        }
    }
}

const RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: rs_money
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if x > 0: 1
          else: 2.5
  - name: rs_int
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if x > 0: 1
          else: 2.5
  - name: rs_passthru_money
    kind: derived
    entity: Person
    dtype: Money
    unit: GBP
    versions:
      - effective_from: 2026-01-01
        formula: x
"#;

#[test]
fn verify_bulk_integer_decimal_output_kind() {
    let outputs = ["v_int", "v_dec", "v_dec_intint", "passthru", "passthru_int"];

    let integer_data = DatasetSpec {
        inputs: vec![input("p1", int(1)), input("p2", int(-1))],
        relations: vec![],
    };
    compare(
        "ProgramSpec; data p1 x=Integer 1, p2 x=Integer -1",
        program(),
        integer_data.clone(),
        &outputs,
    );

    let mixed_data = DatasetSpec {
        inputs: vec![input("p1", int(1)), input("p2", dec("2.5"))],
        relations: vec![],
    };
    compare(
        "ProgramSpec; data p1 x=Integer 1, p2 x=Decimal 2.5",
        program(),
        mixed_data.clone(),
        &outputs,
    );

    match CompiledProgramArtifact::from_rulespec_str(RULESPEC) {
        Ok(artifact) => {
            let rs_outputs = ["rs_money", "rs_int", "rs_passthru_money"];
            compare(
                "RuleSpec; data p1 x=Integer 1, p2 x=Integer -1",
                artifact.program.clone(),
                integer_data,
                &rs_outputs,
            );
            compare(
                "RuleSpec; data p1 x=Integer 1, p2 x=Decimal 2.5",
                artifact.program.clone(),
                mixed_data,
                &rs_outputs,
            );
        }
        Err(error) => println!("RuleSpec compile ERR {error}"),
    }
}
