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

const FORMAT_2_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/artifacts/format2"
);
const PRE_LAZY_FAST_PATH: &str =
    include_str!("fixtures/artifacts/related_derived_sums.pre-lazy-fast-path.compiled.json");
const INCONSISTENT_FILTER: &str = include_str!(
    "fixtures/artifacts/relation_typing/inconsistent_filter_contests_slot.v0.2.2-6031295.json"
);

fn read(path: impl AsRef<std::path::Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn files_in(dir: &str) -> Vec<std::path::PathBuf> {
    let mut files = std::fs::read_dir(format!("{FORMAT_2_DIR}/{dir}"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// `(RuleSpec fixture, the format-2 artifact main wrote for it)` for every
/// in-repo RuleSpec fixture that compiles.
fn frozen_fixtures() -> Vec<(String, String)> {
    let fixtures = files_in("compiled")
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            let source = format!(
                "{}.yaml",
                name.strip_suffix(".compiled.json")
                    .unwrap()
                    .replace("__", "/")
            );
            (source, read(&path))
        })
        .collect::<Vec<_>>();
    assert_eq!(fixtures.len(), 22, "every compiling fixture is frozen");
    fixtures
}

fn compile_fixture(source: &str) -> CompiledProgramArtifact {
    let text = read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(source));
    CompiledProgramArtifact::from_rulespec_str(&text)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn legacy(source: &str) -> Result<CompiledProgramArtifact, String> {
    CompiledProgramArtifact::from_legacy_format_2_json_str(source)
        .map_err(|error| error.to_string())
}

/// Invariant 1. For every in-repo RuleSpec fixture, the format-3 artifact's
/// `program` without `interface`, and its `metadata`, equal (as JSON values)
/// the format-2 artifact the unmodified engine compiled at 5d63e8f3.
#[test]
fn executable_content_equals_what_format_2_compiled() {
    for (source, frozen) in frozen_fixtures() {
        let mut actual = serde_json::to_value(compile_fixture(&source)).unwrap();
        actual["program"]
            .as_object_mut()
            .unwrap()
            .remove("interface")
            .expect("format 3 carries the interface");
        let expected: Value = serde_json::from_str(&frozen).unwrap();
        assert_eq!(expected["artifact_format_version"], json!(2), "{source}");
        assert_eq!(actual["program"], expected["program"], "{source}");
        assert_eq!(actual["metadata"], expected["metadata"], "{source}");
    }
}

/// Invariant 2(a) on every fixture: a format-3 artifact this code line
/// compiled serializes, loads and serializes to the same bytes.
#[test]
fn every_fixture_round_trips_as_format_3() {
    for (source, _) in frozen_fixtures() {
        let artifact = compile_fixture(&source);
        let pretty = serde_json::to_string_pretty(&artifact).unwrap();
        let loaded = CompiledProgramArtifact::from_json_str(&pretty)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            serde_json::to_string_pretty(&loaded).unwrap(),
            pretty,
            "{source}"
        );
        let compact = serde_json::to_string(&artifact).unwrap();
        let loaded = CompiledProgramArtifact::from_json_str(&compact).unwrap();
        assert_eq!(serde_json::to_string(&loaded).unwrap(), compact, "{source}");
    }
}

/// `(name, input, what main's loader returned)` for every payload in the
/// legacy-reload baseline: the twenty-two compiled fixtures and the pre-lazy
/// fast-path fixture, which main reloaded byte for byte, and the constructed
/// dialect payloads, which it normalized.
fn legacy_reloads() -> Vec<(String, String, String)> {
    let mut cases = frozen_fixtures()
        .into_iter()
        .map(|(source, frozen)| (source, frozen.clone(), frozen))
        .collect::<Vec<_>>();
    cases.push((
        "related_derived_sums.pre-lazy-fast-path.compiled.json".to_string(),
        PRE_LAZY_FAST_PATH.to_string(),
        PRE_LAZY_FAST_PATH.to_string(),
    ));
    for input in files_in("legacy-reload/inputs") {
        let name = input.file_name().unwrap().to_str().unwrap().to_string();
        let reloaded = read(format!(
            "{FORMAT_2_DIR}/legacy-reload/reloaded/{name}.reloaded.json"
        ));
        cases.push((name, read(&input), reloaded));
    }
    assert_eq!(cases.len(), 28);
    cases
}

/// Invariant 2(b). For every format-2 payload main's loader accepted, the
/// compatibility loader's result serialized with `to_string_pretty` equals
/// main's; the payload main refused is refused with the same error text. Set
/// `AXIOM_LEGACY_RELOAD_BASELINE` to a directory with `inputs/` and
/// `reloaded/` laid out the same way to check more payloads (the published
/// axiom-api artifact in the baseline the fixtures were taken from, for one).
#[test]
fn the_compatibility_loader_returns_what_the_format_2_loader_returned() {
    let mut cases = legacy_reloads();
    if let Some(dir) = std::env::var_os("AXIOM_LEGACY_RELOAD_BASELINE") {
        let dir = std::path::PathBuf::from(dir);
        let mut inputs = std::fs::read_dir(dir.join("inputs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        inputs.sort();
        for input in inputs {
            let name = input.file_name().unwrap().to_str().unwrap().to_string();
            let reloaded = dir.join("reloaded").join(format!("{name}.reloaded.json"));
            if reloaded.exists() {
                cases.push((name, read(&input), read(&reloaded)));
            } else {
                let expected = read(dir.join("reloaded").join(format!("{name}.error.txt")));
                assert_eq!(legacy(&read(&input)).expect_err(&name), expected, "{name}");
            }
        }
    }
    for (name, input, expected) in cases {
        let loaded = legacy(&input).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(loaded.artifact_format_version, 2, "{name}");
        assert_eq!(loaded.interface, None, "{name}");
        assert_eq!(loaded.engine_build, None, "{name}");
        assert_eq!(loaded.source_closure, None, "{name}");
        assert_eq!(
            serde_json::to_string_pretty(&loaded).unwrap(),
            expected,
            "{name}"
        );
    }
    assert_eq!(
        legacy(INCONSISTENT_FILTER).expect_err("main refused it"),
        read(format!(
            "{FORMAT_2_DIR}/legacy-reload/reloaded/inconsistent_filter_contests_slot.v0.2.2-6031295.json.error.txt"
        ))
    );
}

/// Invariant 2(c). Normalization is idempotent: what the compatibility
/// loader returns loads back to itself.
#[test]
fn normalization_is_idempotent() {
    for (name, input, _) in legacy_reloads() {
        let once = serde_json::to_string_pretty(&legacy(&input).unwrap()).unwrap();
        let twice = serde_json::to_string_pretty(&legacy(&once).unwrap()).unwrap();
        assert_eq!(once, twice, "{name}");
    }
}

/// Invariant 4. Format 2 is never read as format 3. The default loader
/// refuses every format-2 payload and names the artifact as the older side;
/// the compatibility loader returns format 2 with no interface and refuses
/// format 3; a format-2 payload restamped 3 and a format-3 payload restamped
/// 2 are both refused; a format-4 stamp names the engine as the older side.
#[test]
fn format_2_is_never_read_as_format_3() {
    let mut format_2 = legacy_reloads()
        .into_iter()
        .map(|(name, input, _)| (name, input))
        .collect::<Vec<_>>();
    format_2.push((
        "inconsistent_filter".to_string(),
        INCONSISTENT_FILTER.to_string(),
    ));
    for (name, input) in &format_2 {
        let error = CompiledProgramArtifact::from_json_str(input)
            .expect_err("the default loader reads format 3 only");
        assert!(
            matches!(
                error,
                axiom_rules_engine::compile::CompileError::UnsupportedArtifactFormatVersion {
                    found: 2,
                    supported: 3,
                    ..
                }
            ),
            "{name}: {error}"
        );
        assert!(
            error.to_string().contains("is older than this engine"),
            "{name}: {error}"
        );

        // Restamped 3, it is not format 3: it lacks what format 3 requires.
        let mut restamped: Value = serde_json::from_str(input).unwrap();
        restamped["artifact_format_version"] = json!(3);
        refused(&restamped, &format!("{name} restamped 3"));
    }

    for artifact in [compiled(), every_shape()] {
        let format_3 = serde_json::to_string(&artifact).unwrap();
        let error = legacy(&format_3).expect_err("the compatibility loader reads format 2 only");
        assert!(
            error.contains("is artifact format 3, and the format-2 compatibility loader reads format 2 only; load it with the default loader"),
            "{error}"
        );
        // Restamped 2, it carries what only format 3 has; each addition
        // alone is enough to refuse it.
        let mut restamped: Value = serde_json::from_str(&format_3).unwrap();
        restamped["artifact_format_version"] = json!(2);
        assert!(legacy(&restamped.to_string()).is_err());
        let additions: [(&str, fn(&mut Value)); 3] = [
            ("engine_build", |value| {
                value.as_object_mut().unwrap().remove("engine_build");
            }),
            ("source_closure", |value| {
                value.as_object_mut().unwrap().remove("source_closure");
            }),
            ("program.interface", |value| {
                value["program"]
                    .as_object_mut()
                    .unwrap()
                    .remove("interface");
            }),
        ];
        for (kept, _) in &additions {
            let mut value = restamped.clone();
            for (other, remove) in &additions {
                if other != kept {
                    remove(&mut value);
                }
            }
            if *kept == "source_closure" && value.get("source_closure").is_none() {
                continue;
            }
            let error = legacy(&value.to_string()).expect_err(kept);
            assert!(
                error.contains(&format!("a format-2 artifact cannot carry `{kept}`")),
                "{error}"
            );
        }
        let mut stripped = restamped.clone();
        for (_, remove) in &additions {
            remove(&mut stripped);
        }
        let loaded = legacy(&stripped.to_string()).expect("the format-2 projection loads");
        assert_eq!(loaded.artifact_format_version, 2);
        assert_eq!(loaded.interface, None);

        let mut newer = serde_json::to_value(&artifact).unwrap();
        newer["artifact_format_version"] = json!(4);
        let message = refused(&newer, "format 4");
        assert!(message.starts_with("this engine ("), "{message}");
        assert!(
            message.contains("is older than compiled artefact"),
            "{message}"
        );
        let error = legacy(&newer.to_string()).expect_err("format 4 is not format 2");
        assert!(error.contains("reads format 2 only"), "{error}");
    }
}

/// `run-compiled` reads format 3. With `--accept-artifact-format 2` it also
/// reads a format-2 artifact, unchanged, through the compatibility loader,
/// says so on stderr and answers as the format-3 artifact of the same source
/// does; without the flag it refuses one and names the artifact the older
/// side. The flag takes only 2, and it changes nothing for a format-3
/// artifact.
#[test]
fn run_compiled_reads_format_2_only_on_request() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let dir =
        std::env::temp_dir().join(format!("axiom-artifact-format-cli-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    let artifact = compiled();
    let format_3 = dir.join("format-3.json");
    let format_2 = dir.join("format-2.json");
    std::fs::write(&format_3, serde_json::to_string_pretty(&artifact).unwrap()).unwrap();
    std::fs::write(
        &format_2,
        serde_json::to_string_pretty(&as_format_2(&artifact)).unwrap(),
    )
    .unwrap();
    let interval = json!({"start": "2026-01-01", "end": "2026-01-31"});
    let request = json!({
        "mode": "explain",
        "dataset": {
            "inputs": [{"name": "earned_income", "entity": "Person", "entity_id": "p1",
                        "interval": interval, "value": {"kind": "decimal", "value": "120"}}],
            "relations": [{"name": "member_of_household", "tuple": ["p1", "h1"],
                           "interval": interval}]
        },
        "queries": [{"entity_id": "h1",
                     "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
                     "outputs": ["household_income"]}]
    })
    .to_string();
    let run = |artifact: &std::path::Path, flag: &[&str]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
            .args(["run-compiled", "--artifact"])
            .arg(artifact)
            .args(flag)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A refusal can exit before reading stdin.
        if let Err(error) = child.stdin.take().unwrap().write_all(request.as_bytes()) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}");
        }
        let output = child.wait_with_output().unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    const NOTICE: &str = "notice[legacy_artifact_format]";
    const ACCEPT: [&str; 2] = ["--accept-artifact-format", "2"];

    let (ok, current, stderr) = run(&format_3, &[]);
    assert!(ok, "{stderr}");
    assert!(!stderr.contains(NOTICE), "{stderr}");
    let response: Value = serde_json::from_str(&current).unwrap();
    let income = response["results"][0]["outputs"]["household_income"]["value"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("{response}"));
    assert_eq!(income.parse::<f64>().unwrap(), 120.0, "{response}");
    let (ok, flagged, stderr) = run(&format_3, &ACCEPT);
    assert!(ok, "{stderr}");
    assert!(!stderr.contains(NOTICE), "{stderr}");
    assert_eq!(flagged, current);

    let (ok, _, stderr) = run(&format_2, &[]);
    assert!(!ok);
    assert!(
        stderr.contains("is older than this engine: it is artifact format 2")
            && stderr.contains("--accept-artifact-format 2"),
        "{stderr}"
    );
    let (ok, legacy, stderr) = run(&format_2, &ACCEPT);
    assert!(ok, "{stderr}");
    assert!(stderr.contains(NOTICE), "{stderr}");
    assert_eq!(legacy, current);

    for (flag, refusal) in [
        (&["--accept-artifact-format", "3"][..], "accepts only 2"),
        (
            &["--accept-artifact-format"][..],
            "missing `--accept-artifact-format` value",
        ),
    ] {
        let (ok, _, stderr) = run(&format_2, flag);
        assert!(!ok, "{flag:?}");
        assert!(stderr.contains(refusal), "{flag:?}: {stderr}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
