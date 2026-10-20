use axiom_rules_engine::{
    api::{ExecutionRequest, execute_request},
    rulespec::lower_rulespec_str,
};
use serde_json::{Value, json};

const SOURCE: &str = r#"
format: rulespec/v1
units: [{name: CAD, kind: currency, minor_units: 2}]
inputs: [{name: income, entity: Person, dtype: Money, unit: CAD, period: Year}]
rules:
- name: out
  kind: derived
  entity: Person
  dtype: Money
  unit: CAD
  period: Month
  versions:
  - effective_from: '2026-07-01'
    effective_to: '2026-07-31'
    formula: input_for_year(income, 2025)
"#;
fn request(mode: &str, inputs: Value) -> ExecutionRequest {
    let program = lower_rulespec_str(SOURCE).unwrap();
    serde_json::from_value(json!({"mode":mode,"program":program,
        "dataset":{"inputs":inputs,"relations":[]},
        "queries":[{"entity_id":"p","period":{"period_kind":"month","start":"2026-07-01","end":"2026-07-31"},"outputs":["out"]}]
    })).unwrap()
}
fn fact(year: i32, amount: &str) -> Value {
    json!({"name":"income","entity":"Person","entity_id":"p", "interval":{"start":format!("{year}-01-01"),"end":format!("{year}-12-31")},"value":{"kind":"decimal","value":amount}})
}
#[test]
fn source_compiled_historical_lookup_keeps_exact_amount_in_both_modes() {
    for mode in ["explain", "fast"] {
        for inputs in [
            json!([fact(2025, "1234.567890123456789")]),
            json!([
                fact(2024, "10"),
                fact(2025, "1234.567890123456789"),
                fact(2026, "99")
            ]),
        ] {
            let response =
                serde_json::to_value(execute_request(request(mode, inputs)).unwrap()).unwrap();
            assert_eq!(
                response["results"][0]["outputs"]["out"]["value"]["value"], "1234.567890123456789",
                "{response}"
            );
            assert_eq!(response["metadata"]["actual_mode"], "explain");
        }
    }
}
#[test]
fn missing_year_wrong_person_type_and_owner_fail() {
    for mode in ["explain", "fast"] {
        let mut wrong_person = fact(2025, "10");
        wrong_person["entity_id"] = json!("other");
        let mut wrong_owner = fact(2025, "10");
        wrong_owner["entity"] = json!("Household");
        let mut wrong_type = fact(2025, "10");
        wrong_type["value"] = json!({"kind":"bool","value":false});
        for inputs in [
            json!([]),
            json!([fact(2026, "99")]),
            json!([wrong_person]),
            json!([wrong_owner]),
            json!([wrong_type]),
        ] {
            assert!(execute_request(request(mode, inputs)).is_err());
        }
    }
}
#[test]
fn source_declarations_fail_closed() {
    for bad in [
        SOURCE.replace("period: Year", "period: Month"),
        SOURCE.replacen("entity: Person", "entity: Household", 1),
        SOURCE.replace("unit: CAD, period: Year", "period: Year"),
        SOURCE.replace("kind: currency, minor_units: 2", "kind: count"),
        SOURCE.replace("period: Year}", "period: Year, default: 0}"),
        SOURCE.replace(
            "input_for_year(income, 2025)",
            "input_for_year(missing, 2025)",
        ),
        SOURCE.replace("income, 2025)", "income, 2025.5)"),
        SOURCE.replace("income, 2025)", "income, true)"),
        SOURCE.replace("income, 2025)", "income, 0)"),
        SOURCE.replace("income, 2025)", "income, 10000)"),
    ] {
        assert!(lower_rulespec_str(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn covering_precedence_conflicts_laziness_and_trace() {
    for mode in ["explain", "fast"] {
        let mut older = fact(2025, "1");
        older["interval"]["start"] = json!("2024-01-01");
        let response = serde_json::to_value(
            execute_request(request(mode, json!([older, fact(2025, "0")]))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            response["results"][0]["outputs"]["out"]["value"]["value"],
            "0"
        );
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(encoded.contains("selected_year_reads"), "{encoded}");
        assert!(encoded.contains("2025-12-31"), "{encoded}");
        assert!(execute_request(request(mode, json!([fact(2025, "1"), fact(2025, "2")]))).is_err());
        let mut lazy = request(mode, json!([]));
        lazy.program = lower_rulespec_str(&SOURCE.replace(
            "input_for_year(income, 2025)",
            "\"if true: 0 else: input_for_year(income, 2025)\"",
        ))
        .unwrap();
        assert!(execute_request(lazy).is_ok());
    }
}

#[test]
fn ir_contracts_cannot_change_origin_or_omit_required_metadata() {
    let program = serde_json::to_value(lower_rulespec_str(SOURCE).unwrap()).unwrap();
    for field in ["id", "entity", "dtype", "period"] {
        let mut bad = program.clone();
        bad["selected_year_inputs"][0][field] = json!("invalid");
        let spec: axiom_rules_engine::spec::ProgramSpec = serde_json::from_value(bad).unwrap();
        assert!(spec.to_program().is_err(), "accepted invalid {field}");
    }
    let mut foreign = program.clone();
    foreign["selected_year_inputs"][0]["id"] = json!("ca:policies/tests/foreign#input.income");
    foreign["derived"][0]["versions"][0]["expr"]["input"] =
        json!("ca:policies/tests/foreign#input.income");
    let spec: axiom_rules_engine::spec::ProgramSpec = serde_json::from_value(foreign).unwrap();
    assert!(spec.to_program().is_err());
    let mut collision = program;
    collision["derived"].as_array_mut().unwrap().push(json!({"name":"other","id":"ca:policies/tests/other#other","entity":"Person","dtype":"decimal","semantics":"scalar","expr":{"kind":"input","name":"income"}}));
    let spec: axiom_rules_engine::spec::ProgramSpec = serde_json::from_value(collision).unwrap();
    assert!(spec.to_program().is_err());
}

#[test]
fn compiled_roundtrip_and_dense_refusal() {
    use axiom_rules_engine::{compile::CompiledProgramArtifact, dense::DenseCompiledProgram};
    let artifact = CompiledProgramArtifact::compile(lower_rulespec_str(SOURCE).unwrap()).unwrap();
    let text = serde_json::to_string(&artifact).unwrap();
    assert!(text.contains("selected-year input requires Explain"));
    let restored: CompiledProgramArtifact = serde_json::from_str(&text).unwrap();
    assert!(restored.program.to_program().is_ok());
    assert!(DenseCompiledProgram::from_artifact(&restored, Some("Person")).is_err());
}

#[test]
fn related_read_uses_current_membership_and_selected_person_year() {
    let source = format!(
        "{SOURCE}\n{}",
        r#"
- name: members
  kind: data_relation
  data_relation:
    arity: 2
    arguments: [Person, Household]
- name: total
  kind: derived
  entity: Household
  dtype: Money
  unit: CAD
  period: Month
  versions:
  - effective_from: '2026-07-01'
    formula: sum(members.out)
"#
    );
    let program = lower_rulespec_str(&source).unwrap();
    for mode in ["explain", "fast"] {
        let mut req = serde_json::to_value(request(mode, json!([fact(2025, "123.45")]))).unwrap();
        req["program"] = serde_json::to_value(&program).unwrap();
        req["queries"][0]["entity_id"] = json!("h");
        req["queries"][0]["outputs"] = json!(["total"]);
        req["dataset"]["relations"] = json!([
            {"name":"members","tuple":["p","h"],"interval":{"start":"2026-07-01","end":"2026-07-31"}},
            {"name":"members","tuple":["old","h"],"interval":{"start":"2025-01-01","end":"2025-12-31"}}
        ]);
        let response =
            serde_json::to_value(execute_request(serde_json::from_value(req).unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            response["results"][0]["outputs"]["total"]["value"]["value"],
            "123.45"
        );
    }
}

#[test]
fn equal_precedence_owner_conflicts_fail_in_both_orders() {
    let valid = fact(2025, "10");
    let mut other = valid.clone();
    other["entity"] = json!("Household");
    for mode in ["explain", "fast"] {
        for inputs in [json!([valid, other]), json!([other, valid])] {
            assert!(execute_request(request(mode, inputs)).is_err());
        }
    }
}

#[test]
fn real_import_binding_preserves_owner_and_rejects_runtime_alias_collision() {
    use axiom_rules_engine::{
        compile::CompiledProgramArtifact,
        source::{ModuleSource, SourceError},
    };
    struct Sources(std::collections::HashMap<String, String>);
    impl ModuleSource for Sources {
        fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
            Ok(self.0.get(target).cloned())
        }
    }
    let root = r#"
format: rulespec/v1
imports: [ca:policies/tests/annual]
rules:
- name: result
  kind: derived
  entity: Person
  dtype: Money
  unit: CAD
  period: Month
  versions:
  - effective_from: '2026-07-01'
    formula: out
"#;
    let mut source = Sources(
        [
            ("ca:policies/tests/root".into(), root.into()),
            ("ca:policies/tests/annual".into(), SOURCE.into()),
        ]
        .into(),
    );
    let artifact =
        CompiledProgramArtifact::from_rulespec_with_source("ca:policies/tests/root", &source)
            .unwrap();
    assert_eq!(
        artifact.program.selected_year_inputs[0].id,
        "ca:policies/tests/annual#input.income"
    );
    for mode in ["explain", "fast"] {
        let mut req = serde_json::to_value(request(mode, json!([fact(2025, "42.125")]))).unwrap();
        req["program"] = serde_json::to_value(&artifact.program).unwrap();
        req["dataset"]["inputs"][0]["name"] = json!("ca:policies/tests/annual#input.income");
        req["queries"][0]["outputs"] = json!(["ca:policies/tests/root#result"]);
        let result =
            serde_json::to_value(execute_request(serde_json::from_value(req).unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            result["results"][0]["outputs"]["ca:policies/tests/root#result"]["value"]["value"],
            "42.125"
        );
    }
    source.0.insert(
        "ca:policies/tests/root".into(),
        root.replace(
            "imports: [ca:policies/tests/annual]",
            "imports: [ca:policies/tests/annual, ca:policies/tests/foreign]",
        ),
    );
    source.0.insert(
        "ca:policies/tests/foreign".into(),
        r#"
format: rulespec/v1
rules:
- name: foreign_amount
  kind: derived
  entity: Person
  dtype: Money
  period: Month
  unit: CAD
  versions:
  - effective_from: '2026-07-01'
    formula: income
"#
        .into(),
    );
    let error =
        CompiledProgramArtifact::from_rulespec_with_source("ca:policies/tests/root", &source)
            .unwrap_err();
    assert!(error.to_string().contains("runtime slot"), "{error}");
}

#[test]
fn compatibility_capability_is_feature_scoped_and_preserves_lazy_reads() {
    for (formula, expected_mode) in [
        ("0", "fast"),
        ("if true: 0 else: input_for_year(income, 2025)", "explain"),
    ] {
        let source = SOURCE.replace("input_for_year(income, 2025)", &format!("\"{formula}\""));
        let mut req = request("fast", json!([]));
        req.program = lower_rulespec_str(&source).unwrap();
        let result = serde_json::to_value(execute_request(req).unwrap()).unwrap();
        assert_eq!(result["metadata"]["actual_mode"], expected_mode);
        assert_eq!(
            result["results"][0]["outputs"]["out"]["value"]["value"]
                .to_string()
                .trim_matches('"'),
            "0"
        );
    }
}

#[test]
fn annual_read_and_required_boolean_relation_share_explicit_capability() {
    let source = SOURCE
        .replace("inputs: [{name: income, entity: Person, dtype: Money, unit: CAD, period: Year}]", "inputs: [{name: income, entity: Person, dtype: Money, unit: CAD, period: Year}, {name: decision, entity: Subject, dtype: Boolean}]")
        .replace("rules:\n", "rules:\n- name: subject\n  kind: data_relation\n  data_relation:\n    arity: 2\n    arguments: [Person, Subject]\n")
        .replace("formula: input_for_year(income, 2025)", "formula: 'if only_related(subject, decision) == true: input_for_year(income, 2025) else: 0'");
    for mode in ["explain", "fast"] {
        let mut req = serde_json::to_value(request(mode, json!([fact(2025, "12.34")]))).unwrap();
        req["program"] = serde_json::to_value(lower_rulespec_str(&source).unwrap()).unwrap();
        req["dataset"]["relations"] = json!([{ "name":"subject", "tuple":["p","s"],"interval":{"start":"2026-07-01","end":"2026-07-31"}}]);
        req["dataset"]["inputs"].as_array_mut().unwrap().push(json!({"name":"decision","entity":"Subject","entity_id":"s","interval":{"start":"2026-07-01","end":"2026-07-31"},"value":{"kind":"bool","value":true}}));
        let response = serde_json::to_value(
            execute_request(serde_json::from_value(req.clone()).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            response["results"][0]["outputs"]["out"]["value"]["value"],
            "12.34"
        );
        assert_eq!(response["metadata"]["actual_mode"], "explain");
        req["dataset"]["inputs"].as_array_mut().unwrap().pop();
        assert!(execute_request(serde_json::from_value(req).unwrap()).is_err());
    }
}

#[test]
fn repeated_queries_keep_selected_year_trace_through_cached_derived_wrapper() {
    let source = format!(
        "{SOURCE}\n{}",
        r#"
- name: wrapper
  kind: derived
  entity: Person
  dtype: Money
  unit: CAD
  period: Month
  versions:
  - effective_from: '2026-07-01'
    formula: out + out
"#
    );
    for mode in ["explain", "fast"] {
        let mut req = serde_json::to_value(request(mode, json!([fact(2025, "12.34")]))).unwrap();
        req["program"] = serde_json::to_value(lower_rulespec_str(&source).unwrap()).unwrap();
        req["queries"][0]["outputs"] = json!(["wrapper"]);
        let query = req["queries"][0].clone();
        req["queries"].as_array_mut().unwrap().push(query);
        let response =
            serde_json::to_value(execute_request(serde_json::from_value(req).unwrap()).unwrap())
                .unwrap();
        for result in response["results"].as_array().unwrap() {
            assert_eq!(result["outputs"]["wrapper"]["value"]["value"], "24.68");
            let trace = serde_json::to_string(result).unwrap();
            assert!(trace.contains("selected_year_reads"), "{trace}");
            assert!(trace.contains("2025-12-31"), "{trace}");
        }
        assert_eq!(response["results"][0], response["results"][1]);
    }
}
