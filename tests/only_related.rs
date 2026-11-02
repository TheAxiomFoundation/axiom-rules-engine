//! The new expression fails closed on cardinality and type in ordinary API modes.
use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use serde_json::{Value, json};

fn request(mode: &str, targets: &[&str], value: Value) -> ExecutionRequest {
    serde_json::from_value(json!({
        "mode":mode,
        "program":{"relations":[{"name":"decision","arity":2,"slot_entities":["Person","Household"]}],
          "derived":[{"name":"projected","entity":"Person","dtype":"boolean","semantics":"scalar",
            "expr":{"kind":"only_related","relation":"decision","current_slot":0,"related_slot":1,"value":{"kind":"input","name":"refused"}}}]},
        "dataset":{"inputs":targets.iter().map(|id|json!({"name":"refused","entity":"Household","entity_id":id,"interval":{"start":"2026-01-01","end":"2026-01-31"},"value":value})).collect::<Vec<_>>(),
          "relations":targets.iter().map(|id|json!({"name":"decision","tuple":["p",id],"interval":{"start":"2026-01-01","end":"2026-01-31"}})).collect::<Vec<_>>()},
        "queries":[{"entity_id":"p","period":{"period_kind":"month","start":"2026-01-01","end":"2026-01-31"},"outputs":["projected"]}]
    })).unwrap()
}

#[test]
fn ordinary_modes_enforce_cardinality_and_boolean() {
    for mode in ["explain", "fast"] {
        for value in [false, true] {
            let response =
                execute_request(request(mode, &["h"], json!({"kind":"bool","value":value})))
                    .unwrap();
            let serialized = serde_json::to_value(response).unwrap();
            assert_eq!(
                serialized["results"][0]["outputs"]["projected"]["value"]["value"],
                value
            );
        }
        let missing =
            execute_request(request(mode, &[], json!({"kind":"bool","value":false}))).unwrap_err();
        assert!(
            format!("{missing:?}").contains("MissingRelation"),
            "{missing:?}"
        );
        let multiple = execute_request(request(
            mode,
            &["h1", "h2"],
            json!({"kind":"bool","value":false}),
        ))
        .unwrap_err();
        assert!(
            format!("{multiple:?}").contains("AmbiguousRelation"),
            "{multiple:?}"
        );
        let wrong = execute_request(request(mode, &["h"], json!({"kind":"integer","value":0})))
            .unwrap_err();
        assert!(format!("{wrong:?}").contains("TypeMismatch"), "{wrong:?}");
    }
}

#[test]
fn rulespec_requires_boolean_related_owner_declaration() {
    use axiom_rules_engine::rulespec::lower_rulespec_str;
    let source = r#"
format: rulespec/v1
inputs:
  - name: refused
    entity: Household
    dtype: Boolean
rules:
  - name: decision
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: projected
    kind: derived
    entity: Person
    dtype: Boolean
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: only_related(decision, refused)
"#;
    let program = lower_rulespec_str(source).unwrap();
    let text = serde_json::to_string(&program).unwrap();
    assert!(text.contains("only_related"));
    assert!(text.contains("\"current_slot\":0"));
    for bad in [
        source.replacen("dtype: Boolean", "dtype: Integer", 1),
        source.replacen("entity: Household", "entity: Person", 1),
        source.replacen("name: refused", "name: other", 1),
        source.replace(
            "arguments: [Person, Household]",
            "arguments: [Person, Person]",
        ),
    ] {
        assert!(lower_rulespec_str(&bad).is_err(), "accepted {bad}");
    }
    // Reverse the declared direction without changing the semantics.
    let reverse = lower_rulespec_str(&source.replace(
        "arguments: [Person, Household]",
        "arguments: [Household, Person]",
    ))
    .unwrap();
    assert!(
        serde_json::to_string(&reverse)
            .unwrap()
            .contains("\"current_slot\":1")
    );
}

#[test]
fn sparse_period_missing_duplicate_and_laziness() {
    for mode in ["explain", "fast"] {
        let mut duplicate = request(mode, &["h"], json!({"kind":"bool","value":true}));
        duplicate
            .dataset
            .relations
            .push(duplicate.dataset.relations[0].clone());
        assert!(execute_request(duplicate).is_ok());
        let mut missing = request(mode, &["h"], json!({"kind":"bool","value":true}));
        missing.dataset.inputs.clear();
        assert!(format!("{:?}", execute_request(missing).unwrap_err()).contains("MissingInput"));
        let mut wrong_period =
            serde_json::to_value(request(mode, &["h"], json!({"kind":"bool","value":true})))
                .unwrap();
        wrong_period["dataset"]["relations"][0]["interval"]["end"] = json!("2026-01-15");
        assert!(
            format!(
                "{:?}",
                execute_request(serde_json::from_value(wrong_period).unwrap()).unwrap_err()
            )
            .contains("MissingRelation")
        );
        let mut lazy =
            serde_json::to_value(request(mode, &[], json!({"kind":"bool","value":true}))).unwrap();
        let expr = lazy["program"]["derived"][0]["expr"].clone();
        let boolean = |v| json!({"kind":"literal","value":{"kind":"bool","value":v}});
        lazy["program"]["derived"][0]["expr"] = json!({"kind":"if","condition":{"kind":"comparison","left":boolean(false),"op":"eq","right":boolean(true)},"then_expr":expr,"else_expr":boolean(false)});
        assert!(execute_request(serde_json::from_value(lazy).unwrap()).is_ok());
    }
}

#[test]
fn dense_projection_cardinality_type_and_missing() {
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    use axiom_rules_engine::dense::{
        DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    };
    let req = request("explain", &[], json!({"kind":"bool","value":false}));
    let artifact = CompiledProgramArtifact::compile(req.program).unwrap();
    let compiled = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).unwrap();
    let key = compiled.relations()[0].key.clone();
    for (count, column, expected) in [
        (1, Some(DenseColumn::Bool(vec![true])), None),
        (0, Some(DenseColumn::Bool(vec![])), Some("MissingRelation")),
        (
            2,
            Some(DenseColumn::Bool(vec![false, false])),
            Some("AmbiguousRelation"),
        ),
        (1, Some(DenseColumn::Integer(vec![1])), Some("TypeMismatch")),
        (1, None, Some("MissingInput")),
    ] {
        // With the column omitted, nothing else confirms the related row,
        // so state the count to reach the missing-input check.
        let related_row_count = column.is_none().then_some(count);
        let batch = DenseBatchSpec {
            row_count: 1,
            inputs: Default::default(),
            relations: [(
                key.clone(),
                DenseRelationBatchSpec {
                    offsets: vec![0, count],
                    inputs: column
                        .map(|v| [("refused".into(), v)].into())
                        .unwrap_or_default(),
                    related_row_count,
                    ..Default::default()
                },
            )]
            .into(),
        };
        let result = compiled.execute(
            &req.queries[0].period.to_model().unwrap(),
            batch,
            &["projected".into()],
        );
        if let Some(expected) = expected {
            assert!(format!("{:?}", result.unwrap_err()).contains(expected));
        } else {
            let result = result.unwrap();
            assert!(
                matches!(&result.outputs["projected"],DenseOutputValue::Scalar(DenseColumn::Bool(v)) if v==&[true])
            );
        }
    }
}

#[test]
fn raw_spec_rejects_unsupported_shape() {
    for (current, related) in [(0, 0), (1, 1), (0, 2), (2, 1)] {
        let mut req = request("explain", &["h"], json!({"kind":"bool","value":true}));
        let mut value = serde_json::to_value(&req.program).unwrap();
        value["derived"][0]["expr"]["current_slot"] = json!(current);
        value["derived"][0]["expr"]["related_slot"] = json!(related);
        req.program = serde_json::from_value(value).unwrap();
        assert!(execute_request(req).is_err());
    }
    let mut value = serde_json::to_value(request(
        "explain",
        &["h"],
        json!({"kind":"bool","value":true}),
    ))
    .unwrap();
    value["program"]["derived"][0]["expr"]["value"]["kind"] = json!("derived");
    assert!(execute_request(serde_json::from_value(value).unwrap()).is_err());
}

#[test]
fn input_declarations_belong_to_the_citing_module() {
    use axiom_rules_engine::rulespec::load_rulespec_with_source;
    use axiom_rules_engine::source::{ModuleSource, SourceError};
    struct Modules(std::collections::HashMap<String, String>);
    impl ModuleSource for Modules {
        fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
            Ok(self.0.get(target).cloned())
        }
    }
    let leaf = r#"format: rulespec/v1
inputs:
  - name: refused
    entity: Household
    dtype: Boolean
rules:
  - name: decision
    kind: data_relation
    data_relation: { arity: 2, arguments: [Person, Household] }
  - name: imported_projection
    kind: derived
    entity: Person
    dtype: Boolean
    versions:
      - effective_from: '2026-01-01'
        formula: only_related(decision, refused)
"#;
    let root = "format: rulespec/v1\nimports: [us:policies/leaf]\nrules: []\n";
    let mut modules = Modules(
        [
            ("us:policies/root".into(), root.into()),
            ("us:policies/leaf".into(), leaf.into()),
        ]
        .into(),
    );
    load_rulespec_with_source("us:policies/root", &modules).unwrap();
    // An unrelated same-named declaration is not borrowed, nor does it make
    // the imported rule's own declaration ambiguous.
    modules.0.insert(
        "us:policies/root".into(),
        format!("{root}inputs:\n  - name: refused\n    entity: Person\n    dtype: Integer\n"),
    );
    load_rulespec_with_source("us:policies/root", &modules).unwrap();
    let local = leaf
        .replace("imported_projection", "local_projection")
        .replace(
            "inputs:\n  - name: refused\n    entity: Household\n    dtype: Boolean\n",
            "",
        )
        .replace(
            "format: rulespec/v1",
            "format: rulespec/v1\nimports: [us:policies/declaration]",
        );
    modules.0.insert("us:policies/root".into(), local);
    modules.0.insert("us:policies/declaration".into(),"format: rulespec/v1\ninputs:\n  - name: refused\n    entity: Household\n    dtype: Boolean\nrules: []\n".into());
    assert!(load_rulespec_with_source("us:policies/root", &modules).is_err());
}

#[test]
fn derived_relation_filters_before_cardinality_and_tracks_dependencies() {
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    let mut raw = serde_json::to_value(request(
        "explain",
        &["h1", "h2"],
        json!({"kind":"bool","value":true}),
    ))
    .unwrap();
    raw["program"]["derived"][0]["expr"]["relation"] = json!("selected");
    raw["program"]["derived"].as_array_mut().unwrap().push(json!({"name":"eligible","entity":"Household","dtype":"judgment","semantics":"judgment","expr":{"kind":"comparison","left":{"kind":"input","name":"eligible_flag"},"op":"eq","right":{"kind":"literal","value":{"kind":"bool","value":true}}}}));
    raw["program"]["relations"].as_array_mut().unwrap().push(json!({"name":"selected","arity":2,"slot_entities":["Person","Household"],"derivation":{"source_relation":"decision","current_slot":0,"related_slot":1,"slot_entities":["Person","Household"],"predicate":{"kind":"derived","name":"eligible"}}}));
    for (id, value) in [("h1", true), ("h2", false)] {
        raw["dataset"]["inputs"].as_array_mut().unwrap().push(json!({"name":"eligible_flag","entity":"Household","entity_id":id,"interval":{"start":"2026-01-01","end":"2026-01-31"},"value":{"kind":"bool","value":value}}));
    }
    for mode in ["explain", "fast"] {
        raw["mode"] = json!(mode);
        let req: ExecutionRequest = serde_json::from_value(raw.clone()).unwrap();
        let artifact = CompiledProgramArtifact::compile(req.program.clone()).unwrap();
        let result = serde_json::to_value(execute_request(req.clone()).unwrap()).unwrap();
        assert_eq!(
            result["results"][0]["outputs"]["projected"]["value"]["value"],
            true
        );
        use axiom_rules_engine::dense::{
            DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
            DenseRelationBatchSpec,
        };
        let compiled = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).unwrap();
        let key = compiled.relations()[0].key.clone();
        let batch = DenseBatchSpec {
            row_count: 1,
            inputs: Default::default(),
            relations: [(
                key,
                DenseRelationBatchSpec {
                    offsets: vec![0, 2],
                    inputs: [
                        ("eligible_flag".into(), DenseColumn::Bool(vec![true, false])),
                        ("refused".into(), DenseColumn::Bool(vec![true, false])),
                    ]
                    .into(),
                    ..Default::default()
                },
            )]
            .into(),
        };
        let result = compiled
            .execute(
                &req.queries[0].period.to_model().unwrap(),
                batch,
                &["projected".into()],
            )
            .unwrap();
        assert!(
            matches!(&result.outputs["projected"], DenseOutputValue::Scalar(DenseColumn::Bool(v)) if v==&[true])
        );
    }
    // Make the predicate reach projected again: the ordinary dependency
    // graph must reject a cycle rather than recurse in either evaluator.
    raw["program"]["derived"][1]["expr"] = json!({"kind":"comparison","left":{"kind":"derived","name":"projected"},"op":"eq","right":{"kind":"literal","value":{"kind":"bool","value":true}}});
    let req: ExecutionRequest = serde_json::from_value(raw).unwrap();
    let error = CompiledProgramArtifact::compile(req.program).unwrap_err();
    assert!(matches!(
        error,
        axiom_rules_engine::compile::CompileError::CyclicDependency { .. }
    ));
}

#[test]
fn public_document_construction_remains_available() {
    let document = axiom_rules_engine::rulespec::RulesDocument {
        rules: vec![],
        ..Default::default()
    };
    assert!(document.to_program_spec().is_ok());
}

#[test]
fn dense_mask_and_f64_do_not_reach_missing_relation() {
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    use axiom_rules_engine::dense::{
        DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    };
    let req = request("explain", &[], json!({"kind":"bool","value":false}));
    let mut program = serde_json::to_value(req.program).unwrap();
    let projection = program["derived"][0]["expr"].clone();
    program["derived"][0]["expr"] = json!({"kind":"if","condition":{"kind":"comparison","left":{"kind":"input","name":"reach"},"op":"eq","right":{"kind":"literal","value":{"kind":"bool","value":true}}},"then_expr":projection,"else_expr":{"kind":"literal","value":{"kind":"bool","value":false}}});
    let artifact =
        CompiledProgramArtifact::compile(serde_json::from_value(program).unwrap()).unwrap();
    let compiled = DenseCompiledProgram::from_artifact(&artifact, Some("Person")).unwrap();
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: [("reach".into(), DenseColumn::Bool(vec![false, true]))].into(),
        relations: [(
            compiled.relations()[0].key.clone(),
            DenseRelationBatchSpec {
                offsets: vec![0, 0, 1],
                inputs: [("refused".into(), DenseColumn::Bool(vec![true]))].into(),
                ..Default::default()
            },
        )]
        .into(),
    };
    let period = req.queries[0].period.to_model().unwrap();
    for result in [
        compiled.execute(&period, batch.clone(), &["projected".into()]),
        compiled.execute_f64(&period, batch, &["projected".into()]),
    ] {
        assert!(
            matches!(&result.unwrap().outputs["projected"],DenseOutputValue::Scalar(DenseColumn::Bool(v)) if v==&[false,true])
        );
    }
}

#[test]
fn input_interval_ambiguity_and_entity_fail_closed() {
    for mode in ["explain", "fast"] {
        let original =
            serde_json::to_value(request(mode, &["h"], json!({"kind":"bool","value":true})))
                .unwrap();
        let mut partial = original.clone();
        partial["dataset"]["inputs"][0]["interval"]["end"] = json!("2026-01-15");
        assert!(
            format!(
                "{:?}",
                execute_request(serde_json::from_value(partial).unwrap()).unwrap_err()
            )
            .contains("MissingInput")
        );
        let mut conflicting = original.clone();
        let mut other = conflicting["dataset"]["inputs"][0].clone();
        other["value"]["value"] = json!(false);
        conflicting["dataset"]["inputs"]
            .as_array_mut()
            .unwrap()
            .push(other);
        assert!(
            format!(
                "{:?}",
                execute_request(serde_json::from_value(conflicting).unwrap()).unwrap_err()
            )
            .contains("AmbiguousInput")
        );
        let mut wrong_kind = original;
        wrong_kind["dataset"]["inputs"][0]["entity"] = json!("Person");
        assert!(execute_request(serde_json::from_value(wrong_kind).unwrap()).is_err());
    }
}

#[test]
fn ordinary_anchors_order_and_compiled_roundtrip() {
    use axiom_rules_engine::api::{CompiledExecutionRequest, execute_compiled_request};
    use axiom_rules_engine::compile::CompiledProgramArtifact;
    let mut raw = serde_json::to_value(request(
        "explain",
        &["h"],
        json!({"kind":"bool","value":true}),
    ))
    .unwrap();
    raw["program"]["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"unrelated","arity":2,"slot_entities":["Person","Household"]}));
    let interval = json!({"start":"2026-01-01","end":"2026-01-31"});
    raw["dataset"]["inputs"].as_array_mut().unwrap().push(json!({"name":"refused","entity":"Household","entity_id":"h2","interval":interval,"value":{"kind":"bool","value":false}}));
    raw["dataset"]["relations"].as_array_mut().unwrap().extend([
        json!({"name":"decision","tuple":["p2","h2"],"interval":interval}),
        json!({"name":"unrelated","tuple":["p","h2"],"interval":interval}),
    ]);
    let mut query = raw["queries"][0].clone();
    query["entity_id"] = json!("p2");
    raw["queries"].as_array_mut().unwrap().push(query);
    for reverse in [false, true] {
        if reverse {
            for field in ["inputs", "relations"] {
                raw["dataset"][field].as_array_mut().unwrap().reverse();
            }
            raw["queries"].as_array_mut().unwrap().reverse();
        }
        for mode in ["explain", "fast"] {
            raw["mode"] = json!(mode);
            let req: ExecutionRequest = serde_json::from_value(raw.clone()).unwrap();
            let artifact = CompiledProgramArtifact::compile(req.program.clone()).unwrap();
            let artifact =
                CompiledProgramArtifact::from_json_str(&serde_json::to_string(&artifact).unwrap())
                    .unwrap();
            let mut compiled_request = raw.clone();
            compiled_request.as_object_mut().unwrap().remove("program");
            let compiled_request: CompiledExecutionRequest =
                serde_json::from_value(compiled_request).unwrap();
            for response in [
                execute_request(req).unwrap(),
                execute_compiled_request(artifact, compiled_request).unwrap(),
            ] {
                for result in serde_json::to_value(response).unwrap()["results"]
                    .as_array()
                    .unwrap()
                {
                    assert_eq!(
                        result["outputs"]["projected"]["value"]["value"],
                        result["entity_id"] == "p"
                    );
                }
            }
        }
    }
}

#[test]
fn archived_expression_schema_rejects_new_tag() {
    let old: Value =
        serde_json::from_str(include_str!("../schemas/compiled-artifact.v1.schema.json")).unwrap();
    // Validate the expression itself, so refusal cannot be attributed to the
    // artifact version or a newer metadata envelope.
    let schema = json!({"$defs":old["$defs"],"$ref":"#/$defs/ScalarExprSpec"});
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&json!({"kind":"literal","value":{"kind":"bool","value":true}})));
    let req = serde_json::to_value(request(
        "explain",
        &[],
        json!({"kind":"bool","value":false}),
    ))
    .unwrap();
    assert!(!validator.is_valid(&req["program"]["derived"][0]["expr"]));
}
