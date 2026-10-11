//! The format-3 wire gate (`docs/artifact-format.md`, "Loading"): the
//! default loader reads format 3 only, names the older side of a mismatch,
//! closes and requires the format's keys, checks the identities' shapes, and
//! refuses any `program.interface` other than the one it derives.

use axiom_rules_engine::compile::{ARTIFACT_FORMAT_VERSION, CompiledProgramArtifact, EngineBuild};
use serde_json::{Value, json};

const MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: member_income
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income + 0
  - name: household_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.member_income)
"#;

fn compiled() -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(MODULE).expect("the module compiles")
}

fn load(value: &Value) -> Result<CompiledProgramArtifact, String> {
    CompiledProgramArtifact::from_json_str(&value.to_string()).map_err(|error| error.to_string())
}

fn refused(value: &Value, case: &str) -> String {
    match load(value) {
        Ok(_) => panic!("{case}: the default loader accepted {value}"),
        Err(error) => error,
    }
}

/// The program the tamper checks use: every interface object shape, and
/// every input key with a non-null value somewhere (a uniform default needs
/// `input_or_else`, which the formula language cannot write).
fn every_shape() -> CompiledProgramArtifact {
    let int = |value: i64| json!({"kind": "literal", "value": {"kind": "integer", "value": value}});
    let input = |name: &str| json!({"kind": "input", "name": name});
    let program = json!({
        "relations": [
            {"name": "member_of_household", "arity": 2, "slot_entities": ["Person", "Household"]},
            {"name": "snap_unit", "arity": 2, "derivation": {
                "source_relation": "member_of_household",
                "current_slot": 1, "related_slot": 0,
                "entity": "SnapUnit", "slot_entities": ["Person", "Household"],
                "predicate": {"kind": "and", "items": [
                    {"kind": "derived", "name": "member_eligible"},
                    {"kind": "comparison", "left": input("age"), "op": "gte", "right": int(18)}
                ]}
            }}
        ],
        "parameters": [{
            "name": "table", "unit": null, "indexed_by": "size",
            "versions": [{"effective_from": "2020-01-01", "values": {
                "1": {"kind": "integer", "value": 10},
                "2": {"kind": "integer", "value": 20}
            }}]
        }],
        "derived": [
            {"name": "member_eligible", "entity": "Person", "dtype": "judgment", "unit": null,
             "semantics": "judgment",
             "expr": {"kind": "comparison", "left": input("eligible"), "op": "eq",
                      "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}}},
            {"name": "unit_size", "entity": "SnapUnit", "dtype": "integer", "unit": null,
             "semantics": "scalar",
             "expr": {"kind": "count_related", "relation": "snap_unit",
                      "current_slot": 1, "related_slot": 0}},
            {"name": "allotment", "entity": "Household", "dtype": "decimal", "unit": null,
             "semantics": "scalar",
             "expr": {"kind": "add", "items": [
                 {"kind": "parameter_lookup", "parameter": "table", "index": input("size")},
                 {"kind": "input_or_else", "name": "bonus",
                  "default": {"kind": "integer", "value": 0}}
             ]}},
            {"name": "rate", "entity": "Scalar", "dtype": "decimal", "unit": null,
             "semantics": "scalar",
             "expr": {"kind": "add", "items": [input("scalar_input"), int(1)]}}
        ]
    });
    CompiledProgramArtifact::compile(serde_json::from_value(program).unwrap())
        .expect("the every-shape program compiles")
}

/// A compiled artifact loads and serializes back to the same bytes, pretty
/// and compact (invariant 2(a) for this file's programs).
#[test]
fn a_format_3_artifact_round_trips_through_the_default_loader() {
    for artifact in [compiled(), every_shape()] {
        assert_eq!(artifact.artifact_format_version, ARTIFACT_FORMAT_VERSION);
        assert_eq!(ARTIFACT_FORMAT_VERSION, 3);
        for text in [
            serde_json::to_string_pretty(&artifact).unwrap(),
            serde_json::to_string(&artifact).unwrap(),
        ] {
            let loaded = CompiledProgramArtifact::from_json_str(&text).expect("it loads");
            let again = if text.contains('\n') {
                serde_json::to_string_pretty(&loaded).unwrap()
            } else {
                serde_json::to_string(&loaded).unwrap()
            };
            assert_eq!(again, text);
            assert_eq!(loaded.interface, artifact.interface);
        }
    }
}

/// What a format-2 compile of the same source looks like on the wire: the
/// format-3 artifact without its three additions, stamped 2.
fn as_format_2(artifact: &CompiledProgramArtifact) -> Value {
    let mut value = serde_json::to_value(artifact).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("engine_build");
    object.remove("source_closure");
    object.insert("artifact_format_version".to_string(), json!(2));
    value["program"]
        .as_object_mut()
        .unwrap()
        .remove("interface");
    value
}

/// A mismatch names the older side, both formats, the artifact's engine and
/// this engine's version and build, and the ways forward.
#[test]
fn a_version_mismatch_names_the_older_side() {
    let artifact = compiled();
    let engine = format!(
        "this engine ({}, build {})",
        env!("CARGO_PKG_VERSION"),
        EngineBuild::current()
    );

    let older = refused(&as_format_2(&artifact), "format 2");
    for expected in [
        "compiled artefact `<memory>` is older than this engine".to_string(),
        format!(
            "it is artifact format 2 (written by engine {})",
            env!("CARGO_PKG_VERSION")
        ),
        format!("and {engine} reads format 3"),
        "Recompile it with this engine".to_string(),
        "format-2 compatibility loader".to_string(),
        "--accept-artifact-format 2".to_string(),
        "from_legacy_format_2_json_str".to_string(),
    ] {
        assert!(
            older.contains(&expected),
            "missing `{expected}` in: {older}"
        );
    }

    let mut unversioned = as_format_2(&artifact);
    unversioned
        .as_object_mut()
        .unwrap()
        .remove("engine_version");
    let message = refused(&unversioned, "format 2 without engine_version");
    assert!(
        message.contains("written by an engine that did not record its version"),
        "{message}"
    );

    let mut format_1 = as_format_2(&artifact);
    format_1["artifact_format_version"] = json!(1);
    let message = refused(&format_1, "format 1");
    assert!(message.contains("is older than this engine"), "{message}");
    assert!(
        message.contains("no loader in this engine reads format 1"),
        "{message}"
    );

    let mut newer = serde_json::to_value(&artifact).unwrap();
    newer["artifact_format_version"] = json!(4);
    let message = refused(&newer, "format 4");
    assert!(
        message.starts_with(&format!(
            "{engine} is older than compiled artefact `<memory>` (format 4, written by engine {})",
            env!("CARGO_PKG_VERSION")
        )),
        "{message}"
    );
    assert!(message.ends_with("upgrade the engine"), "{message}");
}

/// The top-level keys are a closed set; `engine_version`, `engine_build`,
/// `program.interface` and `metadata.input_catalog` are required; and
/// `program.extends` is refused outright, a null one included.
#[test]
fn format_3_closes_and_requires_its_keys() {
    let base = serde_json::to_value(compiled()).unwrap();
    load(&base).expect("the untouched artifact loads");

    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut unknown = base.clone();
    unknown["diagnostics"] = json!([]);
    cases.push(("unknown top-level key", unknown));
    for key in ["engine_version", "engine_build", "program", "metadata"] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push((key, missing));
    }
    let mut null_build = base.clone();
    null_build["engine_build"] = Value::Null;
    cases.push(("null engine_build", null_build));
    let mut numeric_version = base.clone();
    numeric_version["engine_version"] = json!(2);
    cases.push(("engine_version not a string", numeric_version));
    let mut null_closure = base.clone();
    null_closure["source_closure"] = Value::Null;
    cases.push(("null source_closure", null_closure));
    let mut no_interface = base.clone();
    no_interface["program"]
        .as_object_mut()
        .unwrap()
        .remove("interface");
    cases.push(("program.interface", no_interface));
    let mut null_interface = base.clone();
    null_interface["program"]["interface"] = Value::Null;
    cases.push(("null program.interface", null_interface));
    let mut no_catalog = base.clone();
    no_catalog["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("input_catalog");
    cases.push(("metadata.input_catalog", no_catalog));
    let mut empty_catalog = base.clone();
    empty_catalog["metadata"]["input_catalog"] = json!([]);
    cases.push(("an empty catalog is still checked", empty_catalog));
    for extends in [Value::Null, json!("us:policies/base")] {
        let mut value = base.clone();
        value["program"]["extends"] = extends;
        cases.push(("program.extends", value));
    }
    for (case, value) in cases {
        refused(&value, case);
    }
}

/// `engine_build` and `source_closure` are recorded, not compared with this
/// engine's, but their shape is the format's: exact keys, a 40-digit
/// lowercase commit or null, `dirty` null when the commit is, canonical
/// sorted targets, 64-digit lowercase digests, and a digest of the listed
/// modules.
#[test]
fn format_3_checks_the_shape_of_its_identities() {
    let base = serde_json::to_value(compiled()).unwrap();
    assert!(base["source_closure"].is_object(), "{base}");

    let mut other_build = base.clone();
    other_build["engine_build"] =
        json!({"commit": "0123456789abcdef0123456789abcdef01234567", "dirty": true});
    other_build["engine_version"] = json!("9.9.9");
    let loaded = load(&other_build).expect("another engine's identity is recorded as given");
    assert_eq!(
        loaded.engine_build,
        Some(EngineBuild {
            commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            dirty: Some(true),
        })
    );
    let mut unknown_build = base.clone();
    unknown_build["engine_build"] = json!({"commit": null, "dirty": null});
    load(&unknown_build).expect("a build that could not know its commit loads");

    let build_cases = [
        json!({"commit": "0123456789ABCDEF0123456789ABCDEF01234567", "dirty": false}),
        json!({"commit": "0123456", "dirty": false}),
        json!({"commit": null, "dirty": false}),
        json!({"commit": null}),
        json!({"dirty": null}),
        json!({"commit": null, "dirty": null, "branch": "main"}),
    ];
    for engine_build in build_cases {
        let mut value = base.clone();
        value["engine_build"] = engine_build.clone();
        refused(&value, &format!("engine_build {engine_build}"));
    }

    let closure = base["source_closure"].clone();
    let mut closure_cases = Vec::new();
    let mut digest = closure.clone();
    digest["digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    closure_cases.push(digest);
    let mut module_digest = closure.clone();
    module_digest["modules"][0]["sha256"] = json!("0".repeat(64));
    closure_cases.push(module_digest);
    let mut short_digest = closure.clone();
    short_digest["modules"][0]["sha256"] = json!("abc");
    closure_cases.push(short_digest);
    let mut root = closure.clone();
    root["root"] = json!("us:policies/elsewhere");
    closure_cases.push(root);
    let mut target = closure.clone();
    target["modules"][0]["target"] = json!("not a target");
    closure_cases.push(target);
    let mut unknown_key = closure.clone();
    unknown_key["modules"][0]["path"] = json!("rules.yaml");
    closure_cases.push(unknown_key);
    let mut missing_key = closure.clone();
    missing_key.as_object_mut().unwrap().remove("root");
    closure_cases.push(missing_key);
    let mut repeated = closure.clone();
    let first = repeated["modules"][0].clone();
    repeated["modules"].as_array_mut().unwrap().push(first);
    closure_cases.push(repeated);
    for source_closure in closure_cases {
        let mut value = base.clone();
        value["source_closure"] = source_closure.clone();
        refused(&value, &format!("source_closure {source_closure}"));
    }

    // Omitted is a program compiled from IR; that loads.
    let mut omitted = base.clone();
    omitted.as_object_mut().unwrap().remove("source_closure");
    load(&omitted).expect("an artifact without a RuleSpec source loads");
}

/// Invariant 5. For every key of every interface object, changing its value,
/// removing it (nullable keys included) and adding an unknown key are each
/// refused at load. The keys are enumerated from the emitted block, not
/// sampled; the every-shape program has every object shape and every input
/// key with a non-null value somewhere.
#[test]
fn every_interface_key_is_tamper_checked() {
    let base = serde_json::to_value(every_shape()).unwrap();
    load(&base).expect("the untouched artifact loads");
    let interface = base["program"]["interface"].clone();
    assert!(
        interface["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entity| entity["instances"]["source"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            == ["alias", "supplied", "unscoped"].into_iter().collect(),
        "{interface:#}"
    );
    for key in ["default", "values", "dtype"] {
        assert!(
            interface["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|input| !input[key].is_null()),
            "some input has a non-null `{key}`: {interface:#}"
        );
    }

    let mut objects = Vec::new();
    collect_objects(&interface, String::new(), &mut objects);
    let mut checked = 0;
    for pointer in objects {
        let object = interface.pointer(&pointer).unwrap().as_object().unwrap();
        let location = format!("program.interface{pointer}");
        let mut extra = base.clone();
        interface_object(&mut extra, &pointer).insert("unexpected".to_string(), json!(null));
        refused(&extra, &format!("unknown key at {location}"));
        for (key, value) in object {
            let mut changed = base.clone();
            interface_object(&mut changed, &pointer).insert(key.clone(), tampered(value));
            refused(&changed, &format!("changed `{key}` at {location}"));
            let mut removed = base.clone();
            interface_object(&mut removed, &pointer).remove(key);
            refused(&removed, &format!("removed `{key}` at {location}"));
            checked += 1;
        }
    }
    assert!(checked > 40, "{checked} keys checked");
}

fn interface_object<'a>(
    artifact: &'a mut Value,
    pointer: &str,
) -> &'a mut serde_json::Map<String, Value> {
    artifact["program"]["interface"]
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .expect("pointer names an object")
}

fn collect_objects(value: &Value, pointer: String, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            out.push(pointer.clone());
            for (key, nested) in map {
                collect_objects(nested, format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                collect_objects(nested, format!("{pointer}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// A different value of the same JSON type where there is one.
fn tampered(value: &Value) -> Value {
    match value {
        Value::Null => json!("tampered"),
        Value::Bool(flag) => json!(!flag),
        Value::Number(number) => json!(number.as_u64().map_or(1, |number| number + 1)),
        Value::String(text) => json!(format!("{text}-tampered")),
        Value::Array(items) if items.is_empty() => json!(["tampered"]),
        Value::Array(items) => Value::Array(items[..items.len() - 1].to_vec()),
        Value::Object(_) => json!({}),
    }
}

/// The comparison is on the JSON itself: a block that reads back to the same
/// values but is written differently (a decimal written as a number, keys in
/// another order inside an array) is refused too.
#[test]
fn the_interface_is_compared_as_written() {
    let base = serde_json::to_value(every_shape()).unwrap();
    let inputs = base["program"]["interface"]["inputs"].as_array().unwrap();
    let bonus = inputs
        .iter()
        .position(|input| input["name"] == "bonus")
        .expect("bonus is an input");
    assert_eq!(
        base["program"]["interface"]["inputs"][bonus]["default"],
        json!({"kind": "integer", "value": 0})
    );
    let mut respelled = base.clone();
    respelled["program"]["interface"]["inputs"][bonus]["default"] =
        json!({"kind": "integer", "value": 0, "note": "same value"});
    refused(&respelled, "an extra key inside a default literal");

    let mut reordered = base.clone();
    reordered["program"]["interface"]["entities"]
        .as_array_mut()
        .unwrap()
        .reverse();
    refused(&reordered, "entities out of order");
}
