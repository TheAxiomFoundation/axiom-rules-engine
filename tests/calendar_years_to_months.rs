//! Synthetic calendar-unit tests through compilation and the real executors.
use std::collections::HashMap;
use std::str::FromStr;

use axiom_rules_engine::{
    api::{ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request},
    compile::CompiledProgramArtifact,
    dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue},
    lifetime_api::{execute_lifetime_request, parse_lifetime_request},
    spec::{
        DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
    },
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{Value, json};

const MONTHS: &str = "us:statutes/99/1#months";
const MEAN: &str = "us:statutes/99/1#monthly_mean";

fn source(formula: &str) -> String {
    format!(
        "format: rulespec/v1\nmodule:\n  summary: Synthetic calendar-unit conversion, without policy assumptions.\nrules:\n  - name: months\n    kind: derived\n    entity: Worker\n    dtype: Integer\n    versions:\n      - effective_from: '2000-01-01'\n        formula: '{formula}'\n"
    )
}

fn retain_ids(artifact: CompiledProgramArtifact) -> CompiledProgramArtifact {
    let mut program = artifact.program;
    for derived in &mut program.derived {
        derived.id = Some(format!("us:statutes/99/1#{}", derived.name));
    }
    CompiledProgramArtifact::compile(program).unwrap()
}

fn artifact(formula: &str) -> CompiledProgramArtifact {
    retain_ids(CompiledProgramArtifact::from_rulespec_str(&source(formula)).unwrap())
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: NaiveDate::from_ymd_opt(2001, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2001, 12, 31).unwrap(),
    }
}

fn request(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    years: ScalarValueSpec,
) -> ExecutionRequest {
    let period = period();
    ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: "us:statutes/99/1#input.years".into(),
                entity: "Worker".into(),
                entity_id: "worker".into(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: years,
            }],
            ..Default::default()
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "worker".into(),
            period,
            outputs: vec![MONTHS.into()],
        }],
    }
}

fn batch(column: DenseColumn) -> DenseBatchSpec {
    let row_count = match &column {
        DenseColumn::Integer(v) => v.len(),
        DenseColumn::Decimal(v) => v.len(),
        DenseColumn::Float(v) => v.len(),
        DenseColumn::Bool(v) => v.len(),
        DenseColumn::Text(v) => v.len(),
        DenseColumn::Date(v) => v.len(),
    };
    DenseBatchSpec {
        row_count,
        inputs: HashMap::from([("years".into(), column)]),
        relations: HashMap::new(),
    }
}

fn assert_integer(column: &DenseOutputValue, expected: &[i64]) {
    match column {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => assert_eq!(values, expected),
        other => panic!("expected exact Integer column, got {other:?}"),
    }
}

#[test]
fn calendar_unit_formula_compiles() {
    artifact("calendar_years_to_months(years)");
}

#[test]
fn calendar_unit_formula_rejects_wrong_arity() {
    for formula in [
        "calendar_years_to_months()",
        "calendar_years_to_months(years, 1)",
    ] {
        let error = CompiledProgramArtifact::from_rulespec_str(&source(formula)).unwrap_err();
        assert!(error.to_string().contains("takes 1 arg"), "{error}");
    }
}

#[test]
fn scalar_modes_preserve_exact_integer_and_integral_decimal_values() {
    let artifact = artifact("calendar_years_to_months(years)");
    for (years, expected) in [
        (0, 0),
        (-2, -24),
        (7, 84),
        (9_007_199_254_740_993, 108_086_391_056_891_916),
        (768_614_336_404_564_650, 9_223_372_036_854_775_800),
        (-768_614_336_404_564_650, -9_223_372_036_854_775_800),
    ] {
        for input in [
            ScalarValueSpec::Integer { value: years },
            ScalarValueSpec::Decimal {
                value: format!("{years}.000"),
            },
        ] {
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let result = execute_request(request(&artifact, mode, input.clone())).unwrap();
                match &result.results[0].outputs[MONTHS] {
                    OutputValue::Scalar {
                        value: ScalarValueSpec::Integer { value },
                        ..
                    } => assert_eq!(*value, expected),
                    other => panic!("expected Integer, got {other:?}"),
                }
            }
        }
    }
}

#[test]
fn scalar_modes_reject_fractional_non_numeric_and_out_of_range_values() {
    let artifact = artifact("calendar_years_to_months(years)");
    let mut invalid = vec![
        ScalarValueSpec::Bool { value: true },
        ScalarValueSpec::Text { value: "2".into() },
        ScalarValueSpec::Date {
            value: period().start,
        },
    ];
    for value in [
        "0.5",
        "-0.5",
        "1.0000000000000000000000000001",
        "9223372036854775808",
        "-9223372036854775809",
        "79228162514264337593543950335",
    ] {
        invalid.push(ScalarValueSpec::Decimal {
            value: value.into(),
        });
    }
    for value in [
        i64::MAX,
        i64::MIN,
        768_614_336_404_564_651,
        -768_614_336_404_564_651,
    ] {
        invalid.push(ScalarValueSpec::Integer { value });
        invalid.push(ScalarValueSpec::Decimal {
            value: value.to_string(),
        });
    }
    for input in invalid {
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            assert!(
                execute_request(request(&artifact, mode, input.clone())).is_err(),
                "accepted {input:?}"
            );
        }
    }
}

#[test]
fn dense_modes_keep_exact_integer_output_for_supported_numeric_columns() {
    let artifact = artifact("calendar_years_to_months(years)");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).unwrap();
    let years = [0, -2, 9_007_199_254_740_993, 768_614_336_404_564_650];
    let expected = [0, -24, 108_086_391_056_891_916, 9_223_372_036_854_775_800];
    for column in [
        DenseColumn::Integer(years.to_vec()),
        DenseColumn::Decimal(years.map(Decimal::from).to_vec()),
    ] {
        let exact = dense
            .execute(
                &period().to_model().unwrap(),
                batch(column.clone()),
                &["months".into()],
            )
            .unwrap();
        assert_integer(&exact.outputs["months"], &expected);
        let floating = dense
            .execute_f64(
                &period().to_model().unwrap(),
                batch(column),
                &["months".into()],
            )
            .unwrap();
        assert_integer(&floating.outputs["months"], &expected);
    }
}

#[test]
fn dense_modes_reject_float_columns_even_when_integral_and_other_invalid_types() {
    let artifact = artifact("calendar_years_to_months(years)");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).unwrap();
    for column in [
        DenseColumn::Float(vec![2.0]),
        DenseColumn::Float(vec![f64::NAN]),
        DenseColumn::Float(vec![f64::INFINITY]),
        DenseColumn::Bool(vec![true]),
        DenseColumn::Text(vec!["2".into()]),
        DenseColumn::Date(vec![period().start]),
        DenseColumn::Decimal(vec![Decimal::from_str("0.5").unwrap()]),
        DenseColumn::Decimal(vec![Decimal::MAX]),
        DenseColumn::Integer(vec![768_614_336_404_564_651]),
        DenseColumn::Integer(vec![-768_614_336_404_564_651]),
    ] {
        assert!(
            dense
                .execute(
                    &period().to_model().unwrap(),
                    batch(column.clone()),
                    &["months".into()]
                )
                .is_err(),
            "accepted {column:?}"
        );
        assert!(
            dense
                .execute_f64(
                    &period().to_model().unwrap(),
                    batch(column.clone()),
                    &["months".into()]
                )
                .is_err(),
            "accepted {column:?}"
        );
    }
}

#[test]
fn arithmetic_decimal_is_accepted_but_arithmetic_float_is_rejected() {
    let artifact = artifact("calendar_years_to_months(years + 1)");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Worker")).unwrap();
    let result = dense
        .execute(
            &period().to_model().unwrap(),
            batch(DenseColumn::Integer(vec![1, 2])),
            &["months".into()],
        )
        .unwrap();
    assert_integer(&result.outputs["months"], &[24, 36]);
    assert!(
        dense
            .execute_f64(
                &period().to_model().unwrap(),
                batch(DenseColumn::Integer(vec![1, 2])),
                &["months".into()]
            )
            .is_err()
    );
}

#[test]
fn decimal_output_context_preserves_exact_integer_literal_admission() {
    for literal in ["3", "3.0"] {
        // Money maps to the engine's Decimal declaration. It must not promote
        // the conversion's Integer argument to an inexact f64 literal.
        let source = source(&format!("calendar_years_to_months({literal})"))
            .replace("dtype: Integer", "dtype: Money");
        let compiled = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
        let dense = DenseCompiledProgram::from_artifact(&compiled, Some("Worker")).unwrap();
        let batch = || DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::new(),
        };
        let result = dense
            .execute(&period().to_model().unwrap(), batch(), &["months".into()])
            .unwrap();
        assert_integer(&result.outputs["months"], &[36]);
        let result = dense.execute_f64(&period().to_model().unwrap(), batch(), &["months".into()]);
        if literal == "3" {
            assert_integer(&result.unwrap().outputs["months"], &[36]);
        } else {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("does not accept Float years"));
        }
    }
}

#[test]
fn compiled_roundtrip_retains_operator_name_input_and_public_output_id() {
    let original = artifact("calendar_years_to_months(years)");
    let serialized = serde_json::to_string(&original).unwrap();
    assert!(serialized.contains("calendar_years_to_months"));
    let roundtrip = CompiledProgramArtifact::from_json_str(&serialized).unwrap();
    assert_eq!(roundtrip.program.derived[0].id.as_deref(), Some(MONTHS));
    let dense = DenseCompiledProgram::from_artifact(&roundtrip, Some("Worker")).unwrap();
    let result = dense
        .execute(
            &period().to_model().unwrap(),
            batch(DenseColumn::Integer(vec![2])),
            &["months".into()],
        )
        .unwrap();
    assert_integer(&result.outputs["months"], &[24]);
}

fn lifetime_artifact() -> CompiledProgramArtifact {
    retain_ids(CompiledProgramArtifact::from_rulespec_str(
        r#"format: rulespec/v1
module:
  summary: Synthetic ranked yearly observations and their calendar-unit divisor.
rules:
  - name: selected_years
    kind: derived
    entity: Worker
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: elapsed_years - excluded_years
  - name: months
    kind: derived
    entity: Worker
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: calendar_years_to_months(selected_years)
  - name: monthly_mean
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: sum_top_n_over_periods(amount, selected_years) / calendar_years_to_months(selected_years)
"#,
    ).unwrap())
}

fn year(year: i32) -> Value {
    json!({"period_kind":"tax_year", "start":format!("{year}-01-01"), "end":format!("{year}-12-31")})
}

fn lifetime_request() -> Value {
    let batches: Vec<Value> = [["1200", "600"], ["2400", "1200"], ["2400", "1800"]]
        .into_iter()
        .map(|amounts| {
            json!({
                "row_count":2, "entity_ids":["001", "9007199254740993"],
                "inputs":{
                    "us:statutes/99/1#input.amount":{"kind":"decimal","values":amounts},
                    "us:statutes/99/1#input.elapsed_years":{"kind":"integer","values":[3,4]},
                    "us:statutes/99/1#input.excluded_years":{"kind":"integer","values":[1,1]}
                }
            })
        })
        .collect();
    json!({"schema":"axiom-rules-engine/lifetime-request/v1", "entity":"Worker",
        "periods":[year(2001),year(2002),year(2003)], "batches":batches,
        "outputs":[MEAN], "output_period":year(2003)})
}

#[test]
fn lifetime_reuses_same_arithmetic_derived_count_for_tied_top_n_and_divisor() {
    let input = parse_lifetime_request(&lifetime_request().to_string()).unwrap();
    let result =
        serde_json::to_value(execute_lifetime_request(lifetime_artifact(), input).unwrap())
            .unwrap();
    assert_eq!(
        result["outputs"][MEAN]["column"],
        json!({"kind":"decimal","values":["200","100"]})
    );
    assert_eq!(result["entity_ids"], json!(["001", "9007199254740993"]));
}

#[test]
fn lifetime_conversion_can_wrap_a_reduction_without_inventing_periods() {
    let compiled = artifact("calendar_years_to_months(count_over_periods(years))");
    let dense = DenseCompiledProgram::from_artifact(&compiled, Some("Worker")).unwrap();
    let periods = [
        period().to_model().unwrap(),
        PeriodSpec {
            start: NaiveDate::from_ymd_opt(2003, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2003, 12, 31).unwrap(),
            ..period()
        }
        .to_model()
        .unwrap(),
    ];
    let result = dense
        .execute_lifetime(
            &periods,
            vec![
                batch(DenseColumn::Integer(vec![1, 0])),
                batch(DenseColumn::Integer(vec![1, 0])),
            ],
            &["months".into()],
        )
        .unwrap();
    assert_integer(&result.outputs["months"], &[24, 0]);
}

#[cfg(feature = "fs")]
mod cli {
    use super::*;
    use std::{
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "axiom-calendar-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            lifetime_artifact()
                .write_json_file(path.join("compiled.json"))
                .unwrap();
            Self(path)
        }
        fn run(&self, request: Value) -> std::process::Output {
            let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
                .args(["run-lifetime", "--artifact"])
                .arg(self.0.join("compiled.json"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn actual_cli_executes_exact_conversion_with_tied_top_n_result() {
        let output = Fixture::new().run(lifetime_request());
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["outputs"][MEAN]["column"],
            json!({"kind":"decimal","values":["200","100"]})
        );
        assert_eq!(result["outputs"][MEAN]["id"], MEAN);
        assert_eq!(result["arithmetic"], "decimal");
    }

    #[test]
    fn actual_cli_refuses_period_varying_or_fractional_selected_years() {
        let fixture = Fixture::new();
        let mut varying = lifetime_request();
        varying["batches"][0]["inputs"]["us:statutes/99/1#input.elapsed_years"]["values"] =
            json!([4, 4]);
        let mut fractional = lifetime_request();
        for batch in fractional["batches"].as_array_mut().unwrap() {
            batch["inputs"]["us:statutes/99/1#input.elapsed_years"] =
                json!({"kind":"decimal","values":["3.5","4"]});
        }
        for request in [varying, fractional] {
            let output = fixture.run(request);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["category"], "evaluation");
        }
    }
}
