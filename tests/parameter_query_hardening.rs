//! Parameter query outputs on IR that did not come from the compiler (#163).
//!
//! A non-indexed parameter is a statutory amount a query may name directly:
//! it holds one value per version, under key 0. Compiled RuleSpec keeps that
//! shape, but a raw program or an artifact can arrive without the compiler,
//! so loading checks it. A query reference must also name one kind of
//! declaration, and each query's trace must show what that query evaluated.

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, ExecutionResponse, OutputValue,
    execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    ScalarValueSpec,
};
use serde_json::{Value, json};

fn integer(value: i64) -> Value {
    json!({"kind": "integer", "value": value})
}

fn literal(value: i64) -> Value {
    json!({"kind": "literal", "value": integer(value)})
}

fn parameter(name: &str, indexed_by: Option<&str>, versions: Value) -> Value {
    json!({"name": name, "unit": null, "indexed_by": indexed_by, "versions": versions})
}

fn version(from: &str, values: &[(i64, i64)]) -> Value {
    let values = values
        .iter()
        .map(|(key, value)| (key.to_string(), integer(*value)))
        .collect::<serde_json::Map<_, _>>();
    json!({"effective_from": from, "values": values})
}

fn scalar_rule(name: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": "Household", "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": expr
    })
}

fn lookup(parameter: &str, key: i64) -> Value {
    json!({"kind": "parameter_lookup", "parameter": parameter, "index": {"kind": "literal", "value": integer(key)}})
}

fn program(value: Value) -> ProgramSpec {
    serde_json::from_value(value).expect("test program is a valid ProgramSpec")
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    }
}

fn request(program: ProgramSpec, outputs: &[&str]) -> ExecutionRequest {
    ExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec::default(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: period(),
            outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
        }],
    }
}

/// Every place raw IR enters the engine: compiling it, executing it in a
/// request, and loading it as a format-3 artifact. `twin` is a program that
/// compiles and has `raw`'s derived rules; the artifact test compiles it and
/// swaps in `raw`'s parameters, which the artifact's derived metadata does
/// not depend on.
fn assert_refused_everywhere(raw: Value, twin: Value, outputs: &[&str], expected: &str) {
    let spec = program(raw.clone());
    let compile = CompiledProgramArtifact::compile(spec.clone())
        .expect_err("compiling refuses the program")
        .to_string();
    assert!(compile.contains(expected), "compile: {compile}");

    let execute = execute_request(request(spec, outputs))
        .expect_err("a request refuses the program")
        .to_string();
    assert!(execute.contains(expected), "request: {execute}");

    let artifact = CompiledProgramArtifact::compile(program(twin)).expect("the twin compiles");
    let mut document = serde_json::to_value(&artifact).expect("artifact serializes");
    document["program"]["parameters"] = raw["parameters"].clone();
    let load = CompiledProgramArtifact::from_json_str(&document.to_string())
        .expect_err("loading refuses the artifact")
        .to_string();
    assert!(load.contains(expected), "load: {load}");
}

/// A non-indexed parameter holds exactly key 0. With a second key a direct
/// query used to return the key-0 cell and ignore the other one.
#[test]
fn a_non_indexed_parameter_with_another_key_is_refused_on_load() {
    let raw = json!({
        "parameters": [parameter("amount", None, json!([version("2026-01-01", &[(0, 10), (1, 20)])]))],
        "derived": [scalar_rule("doubled", lookup("amount", 0))],
    });
    let mut twin = raw.clone();
    twin["parameters"][0]["versions"] = json!([version("2026-01-01", &[(0, 10)])]);
    assert_refused_everywhere(
        raw,
        twin,
        &["amount"],
        "[parameter_key_shape] parameter `amount`",
    );
}

#[test]
fn a_non_indexed_parameter_without_key_zero_is_refused_on_load() {
    let raw = json!({
        "parameters": [parameter("amount", None, json!([version("2026-01-01", &[(1, 20)])]))],
        "derived": [scalar_rule("read", lookup("amount", 1))],
    });
    let mut twin = raw.clone();
    twin["parameters"][0]["versions"] = json!([version("2026-01-01", &[(0, 10)])]);
    assert_refused_everywhere(
        raw,
        twin,
        &["amount"],
        "[parameter_key_shape] parameter `amount`",
    );
}

#[test]
fn a_parameter_without_versions_or_values_is_refused_on_load() {
    for (label, parameter) in [
        ("no versions", parameter("amount", None, json!([]))),
        (
            "an empty version",
            parameter("amount", None, json!([version("2026-01-01", &[])])),
        ),
        (
            "an indexed parameter with an empty version",
            parameter(
                "amount",
                Some("household_size"),
                json!([version("2025-01-01", &[(1, 5)]), version("2026-01-01", &[])]),
            ),
        ),
    ] {
        let raw = json!({
            "parameters": [parameter],
            "derived": [scalar_rule("read", lookup("amount", 1))],
        });
        let spec = program(raw.clone());
        let error = CompiledProgramArtifact::compile(spec)
            .expect_err(label)
            .to_string();
        assert!(
            error.contains("[parameter_key_shape] parameter `amount`"),
            "{label}: {error}"
        );
    }
}

/// An indexed table and a scalar that keeps the shape still load.
#[test]
fn well_shaped_parameters_still_load() {
    let raw = json!({
        "parameters": [
            parameter("amount", None, json!([version("2025-01-01", &[(0, 10)]), version("2026-01-01", &[(0, 12)])])),
            parameter("table", Some("household_size"), json!([version("2026-01-01", &[(1, 5), (2, 9)])])),
        ],
        "derived": [scalar_rule("read", lookup("table", 2))],
    });
    let response = execute_request(request(program(raw), &["amount", "read"]))
        .expect("well-shaped parameters execute");
    let outputs = &response.results[0].outputs;
    assert_eq!(scalar(&outputs["amount"]), 12);
    assert_eq!(scalar(&outputs["read"]), 9);
}

fn scalar(output: &OutputValue) -> i64 {
    match output {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => *value,
        other => panic!("expected an integer output, got {other:?}"),
    }
}

/// A parameter and a derived rule that share a name, or a public id, would
/// answer one query reference; the derived rule won, and the parameter could
/// not be queried. Loading refuses the program instead.
#[test]
fn a_reference_shared_by_a_parameter_and_a_derived_rule_is_refused_on_load() {
    let shared_name = json!({
        "parameters": [parameter("amount", None, json!([version("2026-01-01", &[(0, 10)])]))],
        "derived": [scalar_rule("amount", literal(99))],
    });
    let mut twin = shared_name.clone();
    twin["parameters"] = json!([]);
    assert_refused_everywhere(
        shared_name,
        twin,
        &["amount"],
        "[parameter_derived_collision] `amount`",
    );

    let mut shared_id = json!({
        "parameters": [parameter("amount", None, json!([version("2026-01-01", &[(0, 10)])]))],
        "derived": [scalar_rule("amount_rule", literal(99))],
    });
    shared_id["parameters"][0]["id"] = json!("us:policies/example#amount");
    shared_id["derived"][0]["id"] = json!("us:policies/example#amount");
    let spec = program(shared_id.clone());
    let error = execute_request(request(spec, &["us:policies/example#amount"]))
        .expect_err("one id names a parameter and a derived rule")
        .to_string();
    assert!(
        error.contains("[parameter_derived_collision] `us:policies/example#amount`"),
        "{error}"
    );
}

const TRACED: &str = r#"
format: rulespec/v1
rules:
  - name: base_amount
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: "10"
  - name: adjusted_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: base_amount + amount
  - name: doubled_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: adjusted_amount * 2
  - name: unrelated_amount
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount + 1
"#;

fn traced_request(queries: &[(&str, &[&str])]) -> ExecutionRequest {
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(TRACED).expect("RuleSpec lowers");
    let period = period();
    let inputs = ["household-1", "household-2"]
        .iter()
        .map(|household| InputRecordSpec {
            name: "amount".to_string(),
            entity: "Household".to_string(),
            entity_id: (*household).to_string(),
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
            value: ScalarValueSpec::Integer { value: 5 },
        })
        .collect();
    ExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs,
            relations: vec![],
        },
        queries: queries
            .iter()
            .map(|(entity, outputs)| ExecutionQuery {
                assessment_date: None,
                entity_id: (*entity).to_string(),
                period: period.clone(),
                outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
            })
            .collect(),
    }
}

fn traced_rules(response: &ExecutionResponse, query: usize) -> Vec<String> {
    response.results[query].trace.keys().cloned().collect()
}

/// One engine answers a request's queries. A parameter-only query after a
/// derived query for the same entity and period used to inherit that query's
/// cached trace nodes.
#[test]
fn a_parameter_only_query_does_not_inherit_an_earlier_query_trace() {
    let response = execute_request(traced_request(&[
        ("household-1", &["adjusted_amount"]),
        ("household-1", &["base_amount"]),
    ]))
    .expect("the request executes");
    assert_eq!(traced_rules(&response, 0), ["adjusted_amount"]);
    assert_eq!(
        traced_rules(&response, 1),
        Vec::<String>::new(),
        "a parameter output evaluates no rule"
    );
}

/// The same holds between derived queries: each query's trace holds the
/// rules its own outputs reached.
#[test]
fn a_derived_query_does_not_inherit_an_earlier_query_trace() {
    let response = execute_request(traced_request(&[
        ("household-1", &["doubled_amount"]),
        ("household-1", &["unrelated_amount"]),
        ("household-1", &["adjusted_amount"]),
    ]))
    .expect("the request executes");
    assert_eq!(
        traced_rules(&response, 0),
        ["adjusted_amount", "doubled_amount"]
    );
    assert_eq!(traced_rules(&response, 1), ["unrelated_amount"]);
    // Served from the first query's cache, the node is still this query's.
    assert_eq!(traced_rules(&response, 2), ["adjusted_amount"]);
}

/// Each query's trace in a batch equals its trace when it runs alone, over
/// generated query sequences.
#[test]
fn every_query_trace_matches_the_query_run_alone() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

    const OUTPUTS: [&str; 4] = [
        "base_amount",
        "adjusted_amount",
        "doubled_amount",
        "unrelated_amount",
    ];
    let query = (0usize..2, proptest::collection::btree_set(0usize..4, 1..=3));
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 128,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[163; 32]),
    );
    runner
        .run(&proptest::collection::vec(query, 1..=5), |queries| {
            let queries = queries
                .iter()
                .map(|(entity, outputs)| {
                    (
                        ["household-1", "household-2"][*entity],
                        outputs
                            .iter()
                            .map(|index| OUTPUTS[*index])
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>();
            let borrowed = queries
                .iter()
                .map(|(entity, outputs)| (*entity, outputs.as_slice()))
                .collect::<Vec<_>>();
            let batch = execute_request(traced_request(&borrowed))
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            for (index, query) in borrowed.iter().enumerate() {
                let alone = execute_request(traced_request(std::slice::from_ref(query)))
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                let batch_trace =
                    serde_json::to_value(&batch.results[index].trace).expect("trace serializes");
                let alone_trace =
                    serde_json::to_value(&alone.results[0].trace).expect("trace serializes");
                prop_assert_eq!(batch_trace, alone_trace, "query {}", index);
                prop_assert_eq!(
                    serde_json::to_value(&batch.results[index].outputs).unwrap(),
                    serde_json::to_value(&alone.results[0].outputs).unwrap()
                );
            }
            Ok(())
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
}

/// Parameter keys must keep every version's shape, over generated shapes:
/// loading accepts exactly the parameters that keep it, and a direct query of
/// an accepted non-indexed parameter returns its covering version's value.
#[test]
fn generated_parameter_shapes_load_exactly_when_well_shaped() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

    let versions = proptest::collection::vec(
        proptest::collection::btree_map(-1i64..3, -50i64..50, 0..3),
        0..3,
    );
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 256,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[62; 32]),
    );
    let counts = std::cell::RefCell::new((0usize, 0usize));
    runner
        .run(&(any::<bool>(), versions), |(indexed, versions)| {
            let years = ["2024-01-01", "2025-01-01", "2026-01-01"];
            let versions_json = versions
                .iter()
                .zip(years)
                .map(|(values, from)| {
                    version(
                        from,
                        &values
                            .iter()
                            .map(|(key, value)| (*key, *value))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>();
            let raw = json!({
                "parameters": [parameter("amount", indexed.then_some("household_size"), json!(versions_json))],
                "derived": [scalar_rule("read", literal(1))],
            });
            let well_shaped = !versions.is_empty()
                && versions.iter().all(|values| {
                    !values.is_empty()
                        && (indexed || values.keys().copied().collect::<Vec<_>>() == [0])
                });
            let compiled = CompiledProgramArtifact::compile(program(raw.clone()));
            prop_assert_eq!(compiled.is_ok(), well_shaped, "{:?}", compiled.err());
            if well_shaped {
                counts.borrow_mut().0 += 1;
                if !indexed {
                    let response = execute_request(request(program(raw), &["amount"]))
                        .map_err(|error| TestCaseError::fail(error.to_string()))?;
                    let latest = versions.last().expect("a version")[&0];
                    prop_assert_eq!(scalar(&response.results[0].outputs["amount"]), latest);
                }
            } else {
                counts.borrow_mut().1 += 1;
            }
            Ok(())
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    let (accepted, refused) = counts.into_inner();
    assert!(
        accepted >= 32 && refused >= 32,
        "accepted {accepted}, refused {refused}"
    );
}
