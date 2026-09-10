//! Synthetic fixtures through the real Decimal executor and compiled CLI.
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseBatchSpec, DenseColumn, DenseCompiledProgram};
use axiom_rules_engine::lifetime_api::{
    LifetimeApiError, MAX_COLUMNS, MAX_PERIODS, MAX_REQUEST_BYTES, MAX_ROWS, MAX_STRING_BYTES,
    execute_lifetime_request, parse_lifetime_artifact, parse_lifetime_request,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DerivedSemanticsSpec, OverPeriodsKindSpec, ScalarExprSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const OUTPUT: &str = "us:statutes/99/1#history_total";
const INPUT: &str = "us:statutes/99/1#input.amount";

fn artifact(formula: &str, dtype: &str) -> CompiledProgramArtifact {
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: history_total\n    kind: derived\n    entity: Worker\n    dtype: {dtype}\n    versions:\n      - effective_from: '2000-01-01'\n        formula: '{formula}'\n"
    );
    let mut program = CompiledProgramArtifact::from_rulespec_str(&source)
        .unwrap()
        .program;
    program.derived[0].id = Some(OUTPUT.into());
    CompiledProgramArtifact::compile(program).unwrap()
}

fn year(year: i32) -> Value {
    json!({"period_kind": "tax_year", "start": format!("{year}-01-01"), "end": format!("{year}-12-31")})
}

fn request() -> Value {
    json!({
        "schema": "axiom-rules-engine/lifetime-request/v1",
        "entity": "Worker",
        "periods": [year(2001), year(2002)],
        "batches": [
            {"row_count": 2, "entity_ids": ["001", "worker-2"], "inputs": {
                INPUT: {"kind": "decimal", "values": ["0.1", "11.11"]}
            }},
            {"row_count": 2, "entity_ids": ["001", "worker-2"], "inputs": {
                INPUT: {"kind": "decimal", "values": ["0.2", "22.22"]}
            }}
        ],
        "outputs": [OUTPUT],
        "output_period": year(2002)
    })
}

fn run_request(artifact: CompiledProgramArtifact, value: Value) -> Result<Value, LifetimeApiError> {
    let request = parse_lifetime_request(&value.to_string())?;
    Ok(serde_json::to_value(execute_lifetime_request(artifact, request)?).unwrap())
}

fn rejection(value: Value) -> LifetimeApiError {
    run_request(artifact("sum_over_periods(amount)", "Money"), value).unwrap_err()
}

#[test]
fn decimal_results_preserve_public_ids_and_row_alignment() {
    let value = run_request(artifact("sum_over_periods(amount)", "Money"), request()).unwrap();
    assert_eq!(
        value["outputs"][OUTPUT],
        json!({
            "id": OUTPUT, "name": "history_total", "dtype": "decimal", "unit": null,
            "column": {"kind": "decimal", "values": ["0.3", "33.33"]}
        })
    );
    assert_eq!(value["entity_ids"], json!(["001", "worker-2"]));
    assert_eq!(value["reference_period"], year(2002));
    assert_eq!(value["output_period"], year(2002));
    assert_eq!(value["arithmetic"], "decimal");
    assert_eq!(value["engine_version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn integer_bool_and_judgment_columns_stay_typed() {
    let integer =
        run_request(artifact("count_over_periods(amount)", "Integer"), request()).unwrap();
    assert_eq!(
        integer["outputs"][OUTPUT]["column"],
        json!({"kind":"integer", "values":[2,2]})
    );
    let judgment = run_request(
        artifact("sum_over_periods(amount) > 1", "Judgment"),
        request(),
    )
    .unwrap();
    assert_eq!(
        judgment["outputs"][OUTPUT]["column"],
        json!({"kind":"judgment", "values":["not_holds","holds"]})
    );
    let mut boolean = request();
    for batch in boolean["batches"].as_array_mut().unwrap() {
        batch["inputs"][INPUT] = json!({"kind":"bool","values":[false,true]});
    }
    let value = run_request(artifact("count_over_periods(amount)", "Integer"), boolean).unwrap();
    assert_eq!(value["outputs"][OUTPUT]["column"]["values"], json!([0, 2]));
}

#[test]
fn money_outputs_widen_integer_columns_exactly_without_other_coercions() {
    // Count returns an Integer column from the actual lifetime executor.
    let count = run_request(artifact("count_over_periods(amount)", "Money"), request()).unwrap();
    assert_eq!(
        count["outputs"][OUTPUT]["column"],
        json!({"kind":"decimal","values":["2","2"]})
    );
    // Integer input extremes also survive the complete Decimal wire path.
    let mut input = request();
    input["periods"] = json!([year(2001)]);
    input["output_period"] = year(2001);
    input["batches"].as_array_mut().unwrap().truncate(1);
    input["batches"][0]["inputs"][INPUT] = json!({"kind":"integer","values":[i64::MIN, i64::MAX]});
    let result = run_request(artifact("max_over_periods(amount)", "Money"), input).unwrap();
    assert_eq!(result["outputs"][OUTPUT]["dtype"], "decimal");
    assert_eq!(
        result["outputs"][OUTPUT]["column"],
        json!({"kind":"decimal","values":[i64::MIN.to_string(),i64::MAX.to_string()]})
    );
    for dtype in ["Integer", "Bool", "Text", "Date"] {
        let error =
            run_request(artifact("sum_over_periods(amount)", dtype), request()).unwrap_err();
        assert!(
            matches!(error, LifetimeApiError::Output(_)),
            "{dtype}: {error}"
        );
        assert_eq!(error.diagnostic()["category"], "evaluation");
    }
}

#[test]
fn required_missing_inputs_remain_engine_errors() {
    let mut value = request();
    value["batches"][0]["inputs"] = json!({});
    assert!(matches!(rejection(value), LifetimeApiError::Evaluation(_)));
}

#[test]
fn omitted_optional_inputs_use_the_declared_engine_default() {
    let mut value = request();
    for batch in value["batches"].as_array_mut().unwrap() {
        batch["inputs"] = json!({});
    }
    // InputOrElse is an existing compiled expression, not a formula builtin.
    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    let semantics = DerivedSemanticsSpec::Scalar {
        expr: ScalarExprSpec::OverPeriods {
            over: OverPeriodsKindSpec::Sum,
            value: Box::new(ScalarExprSpec::InputOrElse {
                name: "amount".into(),
                default: ScalarValueSpec::Decimal { value: "2".into() },
            }),
            n: None,
        },
    };
    program.derived[0].semantics = semantics.clone();
    program.derived[0].versions[0].semantics = semantics;
    let result = run_request(CompiledProgramArtifact::compile(program).unwrap(), value).unwrap();
    assert_eq!(
        result["outputs"][OUTPUT]["column"]["values"],
        json!(["4", "4"])
    );
}

#[test]
fn zero_rows_are_preserved_without_inventing_entities() {
    let mut value = request();
    for batch in value["batches"].as_array_mut().unwrap() {
        batch["row_count"] = json!(0);
        batch["entity_ids"] = json!([]);
        batch["inputs"][INPUT]["values"] = json!([]);
    }
    let result = run_request(artifact("sum_over_periods(amount)", "Money"), value).unwrap();
    assert_eq!(result["row_count"], 0);
    assert_eq!(result["outputs"][OUTPUT]["column"]["values"], json!([]));
}

#[test]
fn public_input_and_output_aliases_are_not_silently_accepted() {
    let mut unknown = request();
    unknown["batches"][0]["inputs"]["ignored"] = json!({"kind":"integer","values":[1,2]});
    assert!(
        rejection(unknown)
            .to_string()
            .contains("unknown public input")
    );
    let mut bare = request();
    let column = bare["batches"][0]["inputs"]
        .as_object_mut()
        .unwrap()
        .remove(INPUT)
        .unwrap();
    bare["batches"][0]["inputs"]["amount"] = column;
    assert!(rejection(bare).to_string().contains("unknown public input"));
    for outputs in [
        json!(["history_total"]),
        json!(["us:statutes/99/2#history_total"]),
        json!([OUTPUT, OUTPUT]),
    ] {
        let mut value = request();
        value["outputs"] = outputs;
        assert!(matches!(rejection(value), LifetimeApiError::Request { .. }));
    }
}

#[test]
fn originless_rules_do_not_enable_bare_lifetime_references() {
    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    program.derived[0].id = None;
    let compiled = CompiledProgramArtifact::compile(program).unwrap();
    let mut value = request();
    value["outputs"] = json!(["history_total"]);
    let error = run_request(compiled, value).unwrap_err();
    assert!(error.to_string().contains("full durable public output IDs"));
    // An originless internal name spelled like an ID is not a retained ID.
    let mut program = artifact("sum_over_periods(1)", "Money").program;
    program.derived[0].id = None;
    program.derived[0].name = OUTPUT.into();
    let mut value = request();
    for batch in value["batches"].as_array_mut().unwrap() {
        batch["inputs"] = json!({});
    }
    let error = run_request(CompiledProgramArtifact::compile(program).unwrap(), value).unwrap_err();
    assert!(error.to_string().contains("unknown public output"));
    // A mixed artifact cannot expose another originless rule's bare fact.
    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    let mut originless = artifact("sum_over_periods(other)", "Money")
        .program
        .derived
        .remove(0);
    originless.name = "other_total".into();
    originless.id = None;
    program.derived.push(originless);
    let mut value = request();
    value["batches"][0]["inputs"]["other"] = json!({"kind":"integer","values":[1,2]});
    let error = run_request(CompiledProgramArtifact::compile(program).unwrap(), value).unwrap_err();
    assert!(error.to_string().contains("full durable input IDs"));
}

#[test]
fn two_owning_input_names_cannot_overwrite_one_slot() {
    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    let mut second = program.derived[0].clone();
    second.name = "other_total".into();
    second.id = Some("us:statutes/99/2#other_total".into());
    program.derived.push(second);
    let artifact = CompiledProgramArtifact::compile(program).unwrap();
    let mut value = request();
    value["batches"][0]["inputs"]["us:statutes/99/2#input.amount"] =
        json!({"kind":"decimal","values":["0.1","11.11"]});
    let error = run_request(artifact, value).unwrap_err();
    assert!(error.to_string().contains("same slot"));
}

#[test]
fn computed_public_ids_cannot_bind_a_same_named_input_slot() {
    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    let mut computed = artifact("7", "Money").program.derived.remove(0);
    computed.name = "amount".into();
    computed.id = Some("us:statutes/99/1#amount".into());
    program.derived.push(computed);
    let compiled = CompiledProgramArtifact::compile(program).unwrap();
    // The explicit input and computed rule are distinct existing engine nodes.
    assert!(run_request(compiled.clone(), request()).is_ok());
    let mut bad = request();
    let column = bad["batches"][0]["inputs"]
        .as_object_mut()
        .unwrap()
        .remove(INPUT)
        .unwrap();
    bad["batches"][0]["inputs"]["us:statutes/99/1#amount"] = column;
    assert!(
        run_request(compiled, bad)
            .unwrap_err()
            .to_string()
            .contains("unknown public input")
    );
}

#[test]
fn declared_row_misalignment_and_bad_columns_fail() {
    for replacement in [
        json!(["worker-2", "001"]),
        json!(["001", "001"]),
        json!(["001"]),
        json!(["001", ""]),
    ] {
        let mut value = request();
        value["batches"][1]["entity_ids"] = replacement;
        assert!(matches!(rejection(value), LifetimeApiError::Request { .. }));
    }
    for count in [0, 1, 3, MAX_ROWS + 1] {
        let mut value = request();
        value["batches"][0]["row_count"] = json!(count);
        assert!(matches!(rejection(value), LifetimeApiError::Request { .. }));
    }
    let mut short = request();
    short["batches"][0]["inputs"][INPUT]["values"] = json!(["1"]);
    assert!(rejection(short).to_string().contains("length differs"));
}

#[test]
fn periods_are_explicit_without_sorting_padding_or_relabeling() {
    for periods in [
        json!([]),
        json!([year(2001)]),
        json!([year(2002), year(2001)]),
        json!([year(2002), year(2002)]),
    ] {
        let mut value = request();
        value["periods"] = periods;
        assert!(rejection(value).diagnostic()["category"].is_string());
    }
    let mut mismatch = request();
    mismatch["output_period"] = year(2003);
    assert!(matches!(
        rejection(mismatch),
        LifetimeApiError::Unsupported(_)
    ));
    let mut inverted = request();
    inverted["periods"][0]["end"] = json!("2000-01-01");
    assert!(
        rejection(inverted)
            .to_string()
            .contains("start must not follow end")
    );
    let mut overlap = request();
    overlap["periods"][0]["end"] = json!("2002-01-02");
    assert!(rejection(overlap).to_string().contains("without overlap"));
    let mut mixed = request();
    mixed["periods"][0]["period_kind"] = json!("month");
    assert!(rejection(mixed).to_string().contains("same kind"));
    let mut gap = request();
    gap["periods"][0] = year(2000);
    assert!(run_request(artifact("sum_over_periods(amount)", "Money"), gap).is_ok());
}

#[test]
fn parameters_use_each_inner_period_and_the_final_outer_period() {
    let source = r#"
format: rulespec/v1
rules:
  - name: factor
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: '1'
      - effective_from: '2002-01-01'
        formula: '2'
  - name: history_total
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: sum_over_periods(amount * factor) + factor
"#;
    let mut program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap()
        .program;
    program.derived[0].id = Some(OUTPUT.into());
    let result = run_request(
        CompiledProgramArtifact::compile(program).unwrap(),
        request(),
    )
    .unwrap();
    assert_eq!(
        result["outputs"][OUTPUT]["column"]["values"],
        json!(["2.5", "57.55"])
    );
    assert_eq!(result["reference_period"], year(2002));
}

#[test]
fn compiled_relations_and_version_selection_remain_unsupported() {
    let source = r#"
format: rulespec/v1
rules:
  - name: members
    kind: data_relation
    data_relation:
      arity: 2
  - name: history_total
    kind: derived
    entity: Worker
    dtype: Integer
    versions:
      - effective_from: '2000-01-01'
        formula: sum_over_periods(sum(members.amount))
"#;
    let mut program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap()
        .program;
    program.derived[0].id = Some(OUTPUT.into());
    let error = run_request(
        CompiledProgramArtifact::compile(program).unwrap(),
        request(),
    )
    .unwrap_err();
    assert!(matches!(error, LifetimeApiError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("relation context"));

    let mut program = artifact("sum_over_periods(amount)", "Money").program;
    let mut later = program.derived[0].versions[0].clone();
    later.effective_from = "2002-01-01".parse().unwrap();
    program.derived[0].versions[0].effective_to = Some("2001-12-31".parse().unwrap());
    program.derived[0].versions.push(later);
    let error = run_request(
        CompiledProgramArtifact::compile(program).unwrap(),
        request(),
    )
    .unwrap_err();
    assert!(matches!(error, LifetimeApiError::Compile(_)), "{error}");
    assert!(error.to_string().contains("versioned derived formulas"));
}

#[test]
fn resource_bounds_fail_before_execution() {
    let mut periods = request();
    periods["periods"] = json!(vec![year(2001); MAX_PERIODS + 1]);
    assert!(rejection(periods).to_string().contains("period count"));
    let mut outputs = request();
    outputs["outputs"] = json!(vec![OUTPUT; MAX_COLUMNS + 1]);
    assert!(rejection(outputs).to_string().contains("output count"));
    let mut inputs = request();
    inputs["batches"][0]["inputs"] = Value::Object(
        (0..=MAX_COLUMNS)
            .map(|i| {
                (
                    format!("input{i}"),
                    json!({"kind":"integer","values":[1,2]}),
                )
            })
            .collect(),
    );
    assert!(rejection(inputs).to_string().contains("input column count"));
    let mut ids = request();
    for batch in ids["batches"].as_array_mut().unwrap() {
        batch["entity_ids"][0] = json!("x".repeat(MAX_STRING_BYTES + 1));
    }
    assert!(rejection(ids).to_string().contains("string exceeds"));
}

#[test]
fn json_types_unknown_fields_and_nonfinite_values_fail() {
    for column in [
        json!({"kind":"decimal","values":[0.1,0.2]}),
        json!({"kind":"decimal","values":[null,"1"]}),
        json!({"kind":"integer","values":[1.0,2]}),
        json!({"kind":"integer","values":[true,2]}),
        json!({"kind":"integer","values":[18446744073709551615u64,2]}),
        json!({"kind":"decimal","values":["NaN","1"]}),
        json!({"kind":"decimal","values":["0.00000000000000000000000000001","1"]}),
        json!({"kind":"decimal","values":["79228162514264337593543950336","1"]}),
        json!({"kind":"float","values":[0.1,0.2]}),
        json!({"kind":"date","values":["2001-02-30","2001-01-01"]}),
    ] {
        let mut value = request();
        value["batches"][0]["inputs"][INPUT] = column;
        assert!(run_request(artifact("sum_over_periods(amount)", "Money"), value).is_err());
    }
    for location in ["request", "batch", "column", "period"] {
        let mut value = request();
        match location {
            "request" => value["unknown"] = json!(true),
            "batch" => value["batches"][0]["relations"] = json!({}),
            "column" => value["batches"][0]["inputs"][INPUT]["unknown"] = json!(0),
            _ => value["periods"][0]["name"] = json!("ignored"),
        }
        assert!(parse_lifetime_request(&value.to_string()).is_err());
    }
    let mut fast = request();
    fast["arithmetic"] = json!("f64");
    assert!(parse_lifetime_request(&fast.to_string()).is_err());
    assert!(
        parse_lifetime_request("{\"schema\":1,\"schema\":2}")
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(
        parse_lifetime_request("{\"nested\":{\"x\":1,\"x\":2}}")
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(parse_lifetime_request("{\"value\":1e400}").is_err());
    assert!(parse_lifetime_request(&" ".repeat(MAX_REQUEST_BYTES + 1)).is_err());
}

#[test]
fn artifact_admission_is_preserved_for_json_and_rust_callers() {
    let valid = artifact("sum_over_periods(amount)", "Money");
    let mut raw = serde_json::to_value(&valid).unwrap();
    raw["artifact_format_version"] = json!(1);
    assert!(parse_lifetime_artifact(&raw.to_string()).is_err());
    let mut corrupt = valid;
    corrupt.metadata.input_catalog.clear();
    assert!(matches!(
        run_request(corrupt, request()),
        Err(LifetimeApiError::Artifact(_))
    ));
    assert!(parse_lifetime_artifact("{\"x\":1,\"x\":2}").is_err());
}

#[test]
fn lifetime_errors_keep_the_actual_engine_cause() {
    for formula in [
        "amount",
        "sum_top_n_over_periods(amount, 3)",
        "sum_over_periods(amount) + amount",
    ] {
        let error = run_request(artifact(formula, "Money"), request()).unwrap_err();
        assert!(
            matches!(error, LifetimeApiError::Evaluation(_)),
            "{formula}: {error}"
        );
        assert!(std::error::Error::source(&error).is_some());
    }
}

#[test]
fn decimal_and_f64_lifetime_enforce_scalar_commencement() {
    let compiled = artifact("sum_over_periods(amount)", "Money");
    let dense = DenseCompiledProgram::from_artifact(&compiled, Some("Worker")).unwrap();
    let period = Period {
        kind: PeriodKind::TaxYear,
        start: "1999-01-01".parse().unwrap(),
        end: "1999-12-31".parse().unwrap(),
    };
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::from([("amount".into(), DenseColumn::Decimal(vec![Decimal::ONE]))]),
        relations: HashMap::new(),
    };
    let scalar = dense
        .execute(&period, batch.clone(), &[])
        .unwrap_err()
        .to_string();
    assert!(scalar.contains("history_total"));
    assert_eq!(
        dense
            .execute_lifetime(
                std::slice::from_ref(&period),
                vec![batch.clone()],
                &["history_total".into()]
            )
            .unwrap_err()
            .to_string(),
        scalar
    );
    assert_eq!(
        dense
            .execute_lifetime_f64(&[period], vec![batch], &["history_total".into()])
            .unwrap_err()
            .to_string(),
        scalar
    );
    let mut value = request();
    value["periods"][0] = year(1999);
    assert!(rejection(value).to_string().contains("history_total"));
}

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(artifact: &CompiledProgramArtifact) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "axiom-lifetime-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        artifact
            .write_json_file(directory.join("compiled.json"))
            .unwrap();
        Self(directory)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn cli(fixture: &Fixture, command: &str, source: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([command, "--artifact"])
        .arg(fixture.0.join("compiled.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn compiled_cli_emits_exact_columns_and_structured_failure() {
    let fixture = Fixture::new(&artifact("sum_over_periods(amount)", "Money"));
    let result = cli(&fixture, "run-lifetime", &request().to_string());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["outputs"][OUTPUT]["column"]["values"],
        json!(["0.3", "33.33"])
    );
    let failed = cli(&fixture, "run-lifetime", "{\"x\":1,\"x\":2}");
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&failed.stderr).unwrap()["category"],
        "invalid_request"
    );
}

#[test]
fn existing_numeric_overflow_is_a_process_failure_without_a_result() {
    let fixture = Fixture::new(&artifact("sum_over_periods(amount)", "Money"));
    let mut input = request();
    for batch in input["batches"].as_array_mut().unwrap() {
        batch["inputs"][INPUT]["values"] =
            json!([Decimal::MAX.to_string(), Decimal::MAX.to_string()]);
    }
    let failed = cli(&fixture, "run-lifetime", &input.to_string());
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
}

#[test]
fn help_and_scalar_command_remain_distinct() {
    let help = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(["run-lifetime", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Decimal only")
    );
    let fixture = Fixture::new(&artifact("sum_over_periods(amount)", "Money"));
    let scalar = cli(&fixture, "run-compiled", &request().to_string());
    assert!(!scalar.status.success());
    let scalar_fixture = Fixture::new(&artifact("amount", "Money"));
    let scalar_request = json!({
        "mode":"explain",
        "dataset":{"inputs":[{"name":INPUT,"entity":"Worker","entity_id":"001","interval":{"start":"2001-01-01","end":"2001-12-31"},"value":{"kind":"decimal","value":"0.1"}}]},
        "queries":[{"entity_id":"001","period":year(2001),"outputs":[OUTPUT]}]
    });
    let result = cli(&scalar_fixture, "run-compiled", &scalar_request.to_string());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["results"][0]["outputs"][OUTPUT]["value"],
        json!({"kind":"decimal","value":"0.1"})
    );
    let bad = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(["run-lifetime", "--artifact"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&bad.stderr).unwrap()["field"],
        "arguments"
    );
}

#[cfg(feature = "schema")]
#[test]
fn lifetime_schemas_match_wire_and_companion_case_branches() {
    use axiom_rules_engine::schema::{
        all_schemas, lifetime_request_schema, lifetime_response_schema, rulespec_test_schema,
        to_pretty_string,
    };
    let input = request();
    let response =
        run_request(artifact("sum_over_periods(amount)", "Money"), input.clone()).unwrap();
    let mut missing_id = response.clone();
    missing_id["outputs"][OUTPUT]["id"] = Value::Null;
    assert!(
        !jsonschema::draft7::new(&lifetime_response_schema())
            .unwrap()
            .is_valid(&missing_id)
    );
    for (schema, value) in [
        (lifetime_request_schema(), input.clone()),
        (lifetime_response_schema(), response),
    ] {
        let validator = jsonschema::draft7::new(&schema).unwrap();
        assert!(
            validator.is_valid(&value),
            "{:?}",
            validator.iter_errors(&value).collect::<Vec<_>>()
        );
    }
    for named in all_schemas() {
        assert_eq!(
            std::fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("schemas")
                    .join(named.file_name)
            )
            .unwrap(),
            to_pretty_string(&named.schema)
        );
    }
    let companion = rulespec_test_schema();
    let validator = jsonschema::draft7::new(&companion).unwrap();
    let case = json!({"name":"synthetic aligned history","period":input["output_period"],"output":{OUTPUT:["0.3","33.33"]},"lifetime":{
        "entity":input["entity"],"periods":input["periods"],"batches":input["batches"]
    }});
    assert!(validator.is_valid(&json!([case.clone()])));
    for extra in ["input", "tables", "oracle_inputs", "unknown"] {
        let mut bad = case.clone();
        bad[extra] = json!({});
        assert!(!validator.is_valid(&json!([bad])), "{extra}");
    }
    let mut bad = case.clone();
    bad["lifetime"]["unknown"] = json!(1);
    assert!(!validator.is_valid(&json!([bad])));
    let mut bad = case;
    bad["output"][OUTPUT] = json!([0.3, 33.33]);
    assert!(!validator.is_valid(&json!([bad])));
    // The preexisting permissive scalar shape keeps tables, extra metadata,
    // null expected values and object/list expectations exactly as before.
    assert!(validator.is_valid(&json!([{"name":"scalar","period":"2026","tables":{},"extra":true,"output":{"a":null,"b":{"x":1},"c":[1,2]}}])));
}
