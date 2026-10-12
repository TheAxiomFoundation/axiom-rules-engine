//! Interpretation metadata validates at load time and remains inert thereafter.

use std::collections::HashMap;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::rulespec::{
    InterpretationStatus, ModuleMetadata, RuleSpecError, load_rulespec_with_source,
    lower_rulespec_str,
};
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{ProgramSpec, SpecError};
use proptest::prelude::*;
use serde_json::{Value, json};

const ENTRY_ID: &str = "weekly-percentage";
const MODULE_TARGET: &str = "uk:policies/council/interpretation-test";
const FLAT_TAX: &str = include_str!("fixtures/rulespec/other/flat_tax/rules.yaml");

fn ruling() -> Value {
    json!({
        "by": "Axiom Foundation",
        "date": "2026-10-10",
        "record": "https://github.com/TheAxiomFoundation/rulespec-uk/pull/449"
    })
}

fn confirmation() -> Value {
    json!({
        "by": "Example District Council",
        "date": "2026-10-20",
        "excerpt": "The percentage is applied once a week.",
        "record": "https://example.org/published-reply"
    })
}

fn valid_entry(status: &str) -> Value {
    let mut entry = json!({
        "id": ENTRY_ID,
        "question": "Is the percentage applied weekly or daily?",
        "texts": [{
            "corpus_citation_path": "uk-babergh/manual/council-tax-reduction-scheme-2026-2027/page-87",
            "excerpt": "The prescribed daily percentage"
        }],
        "reading": "The percentage is applied once a week.",
        "alternatives": [{
            "reading": "The percentage is applied each day.",
            "effect": "Support ends at a lower income."
        }],
        "basis": "The weekly table uses the same percentage.",
        "status": status,
        "affects": ["rate"]
    });
    if status != "open" {
        entry["ruling"] = ruling();
    }
    if matches!(status, "confirmed" | "contradicted") {
        entry["confirmation"] = confirmation();
    }
    entry
}

fn document(entries: Vec<Value>) -> Value {
    json!({
        "format": "rulespec/v1",
        "module": { "interpretations": entries },
        "rules": [{
            "name": "rate",
            "kind": "parameter",
            "dtype": "Rate",
            "versions": [{"effective_from": "2026-01-01", "formula": "0.20"}]
        }]
    })
}

fn yaml(document: &Value) -> String {
    serde_yaml::to_string(document).expect("test document serializes")
}

fn load_entry(entry: Value) -> Result<ProgramSpec, RuleSpecError> {
    lower_rulespec_str(&yaml(&document(vec![entry])))
}

fn assert_loads(entry: Value) {
    load_entry(entry).expect("valid interpretation loads");
}

fn assert_context(path: &str, id: &str) {
    assert_eq!(path, "<memory>");
    assert_eq!(id, ENTRY_ID);
}

fn set(entry: &mut Value, pointer: &str, value: Value) {
    *entry.pointer_mut(pointer).expect("test field exists") = value;
}

fn remove(entry: &mut Value, object: &str, field: &str) {
    entry
        .pointer_mut(object)
        .expect("test object exists")
        .as_object_mut()
        .expect("test value is an object")
        .remove(field)
        .expect("test field exists");
}

#[test]
fn ids_are_lowercase_kebab_case() {
    for id in ["a", "123", "a1-b2-3", "daily-twenty-per-cent"] {
        let mut entry = valid_entry("open");
        entry["id"] = json!(id);
        assert_loads(entry);
    }
    for id in ["", "-a", "a-", "a--b", "A", "a_b", "a b", "é", "a.b"] {
        let mut entry = valid_entry("open");
        entry["id"] = json!(id);
        let error = load_entry(entry).expect_err("invalid interpretation id is refused");
        match error {
            RuleSpecError::InvalidInterpretationId { path, id: value } => {
                assert_eq!(path, "<memory>");
                assert_eq!(value, id);
            }
            other => panic!("expected invalid id for {id:?}, got {other:?}"),
        }
    }
}

#[test]
fn ids_are_unique_within_each_document() {
    let first = valid_entry("open");
    let mut second = first.clone();
    second["id"] = json!("another-reading");
    lower_rulespec_str(&yaml(&document(vec![first.clone(), second]))).expect("distinct ids load");
    let error = lower_rulespec_str(&yaml(&document(vec![first.clone(), first])))
        .expect_err("duplicate ids are refused");
    match error {
        RuleSpecError::DuplicateInterpretationId { path, id } => assert_context(&path, &id),
        other => panic!("expected duplicate id, got {other:?}"),
    }
}

#[test]
fn required_prose_and_organisation_fields_are_nonblank() {
    let fields = [
        ("/question", "question"),
        ("/reading", "reading"),
        ("/basis", "basis"),
        ("/texts/0/excerpt", "texts[0].excerpt"),
        ("/alternatives/0/reading", "alternatives[0].reading"),
        ("/ruling/by", "ruling.by"),
        ("/confirmation/by", "confirmation.by"),
        ("/confirmation/excerpt", "confirmation.excerpt"),
    ];
    for (pointer, expected_field) in fields {
        let mut entry = valid_entry("confirmed");
        set(&mut entry, pointer, json!("  Nonblank text.\n"));
        assert_loads(entry);
        for blank in ["", " \t\n", "\u{2003}"] {
            let mut entry = valid_entry("confirmed");
            set(&mut entry, pointer, json!(blank));
            let error = load_entry(entry).expect_err("blank required field is refused");
            match error {
                RuleSpecError::BlankInterpretationField {
                    path,
                    id,
                    field,
                    value,
                } => {
                    assert_context(&path, &id);
                    assert_eq!(field, expected_field);
                    assert_eq!(value, blank);
                }
                other => panic!("expected blank field {pointer}, got {other:?}"),
            }
        }
    }
    // The validator must visit every item, including entries after the first.
    for (collection, expected_field) in [
        ("texts", "texts[1].excerpt"),
        ("alternatives", "alternatives[1].reading"),
    ] {
        let mut entry = valid_entry("open");
        let mut second = entry[collection][0].clone();
        second[if collection == "texts" {
            "excerpt"
        } else {
            "reading"
        }] = json!(" ");
        entry[collection].as_array_mut().unwrap().push(second);
        assert!(matches!(
            load_entry(entry).expect_err("later blank item is refused"),
            RuleSpecError::BlankInterpretationField { field, .. } if field == expected_field
        ));
    }
}

#[test]
fn texts_alternatives_and_affects_are_nonempty() {
    assert_loads(valid_entry("open"));
    for field in ["texts", "alternatives", "affects"] {
        let mut entry = valid_entry("open");
        entry[field] = json!([]);
        let error = load_entry(entry).expect_err("empty required collection is refused");
        match error {
            RuleSpecError::EmptyInterpretationCollection {
                path,
                id,
                field: value,
            } => {
                assert_context(&path, &id);
                assert_eq!(value, field);
            }
            other => panic!("expected empty {field}, got {other:?}"),
        }
    }
}

#[test]
fn text_citations_are_canonical_corpus_paths() {
    for citation in [
        "uk-babergh/manual/scheme/page-87",
        "us/statute/7/2017/a",
        "uk/ukpga/1992/4/section/1",
        "uk/manual/scheme.md",
    ] {
        let mut entry = valid_entry("open");
        entry["texts"][0]["corpus_citation_path"] = json!(citation);
        assert_loads(entry);
    }
    for citation in [
        "",
        "uk",
        "uk:manual/scheme/page-87",
        "/uk/manual/scheme",
        "uk/manual/../scheme",
        "uk//manual/scheme",
        "uk/manual/scheme/",
        "https://example.org/scheme",
        "uk/manual/scheme#paragraph-1",
        "uk/manual/scheme?version=1",
    ] {
        let mut entry = valid_entry("open");
        entry["texts"][0]["corpus_citation_path"] = json!(citation);
        let error = load_entry(entry).expect_err("noncanonical text citation is refused");
        match error {
            RuleSpecError::InvalidInterpretationCitationPath { path, id, value } => {
                assert_context(&path, &id);
                assert_eq!(value, citation);
            }
            other => panic!("expected invalid citation for {citation:?}, got {other:?}"),
        }
    }
}

#[test]
fn dates_have_exact_iso_shape_and_exist_in_the_calendar() {
    for field in ["ruling", "confirmation"] {
        for date in ["2024-02-29", "2000-02-29", "2026-12-31", "0001-01-01"] {
            let mut entry = valid_entry("confirmed");
            entry[field]["date"] = json!(date);
            assert_loads(entry);
        }
        for date in [
            "",
            "2026-2-01",
            "2026-02-1",
            "26-02-01",
            "2026/02/01",
            "2026-02-29",
            "1900-02-29",
            "2026-04-31",
            "2026-00-01",
            "2026-13-01",
            "2026-01-00",
            "2026-01-32",
            "2026-01-01T00:00:00Z",
            " 2026-01-01",
        ] {
            let mut entry = valid_entry("confirmed");
            entry[field]["date"] = json!(date);
            let error = load_entry(entry).expect_err("invalid date is refused");
            match error {
                RuleSpecError::InvalidInterpretationDate {
                    path,
                    id,
                    field: value_field,
                    value,
                } => {
                    assert_context(&path, &id);
                    assert_eq!(value_field, format!("{field}.date"));
                    assert_eq!(value, date);
                }
                other => panic!("expected invalid {field} date {date:?}, got {other:?}"),
            }
        }
    }
}

#[test]
fn public_records_have_https_prefix_and_confirmation_record_is_optional() {
    for field in ["ruling", "confirmation"] {
        let mut entry = valid_entry("confirmed");
        entry[field]["record"] = json!("https://example.org/public-record#reply");
        assert_loads(entry);
        for record in [
            "",
            "http://example.org/reply",
            "ftp://example.org/reply",
            "example.org/reply",
            "HTTPS://example.org/reply",
            " https://example.org/reply",
        ] {
            let mut entry = valid_entry("confirmed");
            entry[field]["record"] = json!(record);
            let error = load_entry(entry).expect_err("non-https record is refused");
            match error {
                RuleSpecError::InvalidInterpretationRecord {
                    path,
                    id,
                    field: value_field,
                    value,
                } => {
                    assert_context(&path, &id);
                    assert_eq!(value_field, format!("{field}.record"));
                    assert_eq!(value, record);
                }
                other => panic!("expected invalid {field} record, got {other:?}"),
            }
        }
    }
    let mut entry = valid_entry("confirmed");
    remove(&mut entry, "/confirmation", "record");
    assert_loads(entry);
}

#[test]
fn status_controls_ruling_and_confirmation_presence() {
    for (status, expected_status) in [
        ("open", InterpretationStatus::Open),
        ("ruled", InterpretationStatus::Ruled),
        ("confirmed", InterpretationStatus::Confirmed),
        ("contradicted", InterpretationStatus::Contradicted),
    ] {
        for has_ruling in [false, true] {
            for has_confirmation in [false, true] {
                let mut entry = valid_entry("open");
                entry["status"] = json!(status);
                if has_ruling {
                    entry["ruling"] = ruling();
                }
                if has_confirmation {
                    entry["confirmation"] = confirmation();
                }
                let required_ruling = status != "open";
                let required_confirmation = matches!(status, "confirmed" | "contradicted");
                if has_ruling == required_ruling && has_confirmation == required_confirmation {
                    assert_loads(entry);
                    continue;
                }
                let (expected_field, required) = if has_ruling != required_ruling {
                    ("ruling", required_ruling)
                } else {
                    ("confirmation", required_confirmation)
                };
                let error =
                    load_entry(entry).expect_err("invalid status field combination is refused");
                match error {
                    RuleSpecError::InvalidInterpretationStatusFields {
                        path,
                        id,
                        status: value_status,
                        field,
                        requirement,
                    } => {
                        assert_context(&path, &id);
                        assert_eq!(value_status, expected_status);
                        assert_eq!(field, expected_field);
                        assert_eq!(requirement, if required { "required" } else { "forbidden" });
                    }
                    other => panic!("expected status presence error, got {other:?}"),
                }
            }
        }
    }
}

#[test]
fn status_is_a_closed_lowercase_enum() {
    for status in ["open", "ruled", "confirmed", "contradicted"] {
        assert_loads(valid_entry(status));
    }
    for status in ["", "Open", "RUled", "resolved", "pending"] {
        let mut entry = valid_entry("open");
        entry["status"] = json!(status);
        assert!(matches!(
            load_entry(entry).expect_err("unknown status is refused"),
            RuleSpecError::Yaml(_)
        ));
    }
}

#[test]
fn unknown_keys_are_rejected_at_every_interpretation_level() {
    assert_loads(valid_entry("confirmed"));
    for pointer in [
        "",
        "/texts/0",
        "/alternatives/0",
        "/ruling",
        "/confirmation",
    ] {
        let mut entry = valid_entry("confirmed");
        entry
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown_key".into(), json!("unexpected"));
        let error = load_entry(entry).expect_err("unknown nested key is refused");
        assert!(matches!(error, RuleSpecError::Yaml(_)), "got {error:?}");
        assert!(error.to_string().contains("unknown field `unknown_key`"));
    }
}

fn required_fields() -> Vec<(&'static str, &'static str)> {
    vec![
        ("", "id"),
        ("", "question"),
        ("", "texts"),
        ("", "reading"),
        ("", "alternatives"),
        ("", "basis"),
        ("", "status"),
        ("", "affects"),
        ("/texts/0", "corpus_citation_path"),
        ("/texts/0", "excerpt"),
        ("/alternatives/0", "reading"),
        ("/ruling", "by"),
        ("/ruling", "date"),
        ("/ruling", "record"),
        ("/confirmation", "by"),
        ("/confirmation", "date"),
        ("/confirmation", "excerpt"),
    ]
}

#[test]
fn every_required_field_must_be_present_and_nonnull() {
    assert_loads(valid_entry("confirmed"));
    for (object, field) in required_fields() {
        let mut entry = valid_entry("confirmed");
        remove(&mut entry, object, field);
        let error = load_entry(entry).expect_err("missing required field is refused");
        assert!(
            matches!(error, RuleSpecError::Yaml(_)),
            "{object}/{field}: {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains(&format!("missing field `{field}`"))
        );

        let mut entry = valid_entry("confirmed");
        set(&mut entry, &format!("{object}/{field}"), Value::Null);
        let error = load_entry(entry).expect_err("null required field is refused");
        assert!(
            matches!(error, RuleSpecError::Yaml(_)),
            "{object}/{field}: {error:?}"
        );
    }
}

#[test]
fn required_strings_are_not_coerced_from_scalars() {
    for (object, field) in required_fields() {
        if matches!(field, "texts" | "alternatives" | "affects" | "status") {
            continue;
        }
        for value in [json!(123), json!(false)] {
            let mut entry = valid_entry("confirmed");
            set(&mut entry, &format!("{object}/{field}"), value);
            assert!(matches!(
                load_entry(entry).expect_err("required string does not coerce a scalar"),
                RuleSpecError::Yaml(_)
            ));
        }
    }
    for value in [json!(123), json!(false), Value::Null] {
        let mut entry = valid_entry("open");
        entry["affects"] = json!([value]);
        assert!(matches!(
            load_entry(entry).expect_err("affects items must be strings"),
            RuleSpecError::Yaml(_)
        ));
    }
}

#[test]
fn optional_effect_and_confirmation_record_can_be_omitted() {
    let mut entry = valid_entry("confirmed");
    remove(&mut entry, "/alternatives/0", "effect");
    remove(&mut entry, "/confirmation", "record");
    assert_loads(entry);
    for pointer in ["/alternatives/0/effect", "/confirmation/record"] {
        for value in [Value::Null, json!(123), json!(false)] {
            let mut entry = valid_entry("confirmed");
            set(&mut entry, pointer, value);
            assert!(matches!(
                load_entry(entry).expect_err("optional strings must be strings when supplied"),
                RuleSpecError::Yaml(_)
            ));
        }
    }
}

#[test]
fn supplied_null_status_records_are_rejected() {
    for status in ["open", "ruled", "confirmed", "contradicted"] {
        assert_loads(valid_entry(status));
        for field in ["ruling", "confirmation"] {
            let mut entry = valid_entry(status);
            entry[field] = Value::Null;
            assert!(matches!(
                load_entry(entry).expect_err("supplied null is not absent metadata"),
                RuleSpecError::Yaml(_)
            ));
        }
    }
}

#[test]
fn all_unresolved_affects_names_are_reported_together() {
    assert_loads(valid_entry("open"));
    let mut entry = valid_entry("open");
    entry["affects"] = json!(["unknown_one", "rate", "unknown_two"]);
    let error = load_entry(entry).expect_err("all affects names must resolve");
    match error {
        RuleSpecError::UnresolvedInterpretationAffects { path, id, names } => {
            assert_context(&path, &id);
            assert_eq!(names, ["unknown_one", "unknown_two"]);
        }
        other => panic!("expected unresolved affects, got {other:?}"),
    }
}

struct MemorySource(HashMap<String, String>);

impl ModuleSource for MemorySource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).cloned())
    }
}

#[test]
fn affects_cannot_resolve_to_an_imported_rule_or_deferred_output() {
    let mut entry = valid_entry("open");
    entry["affects"] = json!(["imported_rate", "deferred_rate"]);
    let mut root = document(vec![entry]);
    root["imports"] = json!(["uk:policies/council/imported-test"]);
    root["module"]["deferred_outputs"] = json!(["deferred_rate"]);
    let mut imported = document(vec![]);
    imported["rules"][0]["name"] = json!("imported_rate");
    let source = MemorySource(HashMap::from([
        (MODULE_TARGET.into(), yaml(&root)),
        ("uk:policies/council/imported-test".into(), yaml(&imported)),
    ]));
    let error = load_rulespec_with_source(MODULE_TARGET, &source)
        .expect_err("imported and deferred-only names are outside document scope");
    match error {
        RuleSpecError::UnresolvedInterpretationAffects { path, id, names } => {
            assert_eq!(path, MODULE_TARGET);
            assert_eq!(id, ENTRY_ID);
            assert_eq!(names, ["imported_rate", "deferred_rate"]);
        }
        other => panic!("expected unresolved affects, got {other:?}"),
    }
    let mut local = root;
    local["module"]["interpretations"][0]["affects"] = json!(["rate"]);
    let mut source = source;
    source.0.insert(MODULE_TARGET.into(), yaml(&local));
    load_rulespec_with_source(MODULE_TARGET, &source)
        .expect("a document-local rule is accepted in the same import closure");
}

#[test]
fn imported_interpretations_validate_before_metadata_is_discarded() {
    let mut root = document(vec![]);
    root["imports"] = json!(["uk:policies/council/imported-test"]);
    root["rules"] = json!([]);
    let mut entry = valid_entry("open");
    entry["affects"] = json!(["missing_imported_rule"]);
    let source = MemorySource(HashMap::from([
        (MODULE_TARGET.into(), yaml(&root)),
        (
            "uk:policies/council/imported-test".into(),
            yaml(&document(vec![entry])),
        ),
    ]));
    let error = load_rulespec_with_source(MODULE_TARGET, &source)
        .expect_err("invalid interpretation in an import is refused");
    assert!(matches!(
        error,
        RuleSpecError::UnresolvedInterpretationAffects { path, id, names }
            if path == "uk:policies/council/imported-test"
            && id == ENTRY_ID
            && names == ["missing_imported_rule"]
    ));
}

#[test]
fn interpretation_ids_are_independent_between_imported_documents() {
    let mut root = document(vec![valid_entry("open")]);
    root["imports"] = json!(["uk:policies/council/imported-test"]);
    let source = MemorySource(HashMap::from([
        (MODULE_TARGET.into(), yaml(&root)),
        (
            "uk:policies/council/imported-test".into(),
            yaml(&document(vec![valid_entry("open")])),
        ),
    ]));
    let program = load_rulespec_with_source(MODULE_TARGET, &source)
        .expect("the same interpretation id in different documents is accepted");
    assert_eq!(program.module.unwrap().interpretations.len(), 1);
}

#[test]
fn empty_interpretations_default_and_are_omitted_when_serialized() {
    let empty: ModuleMetadata = serde_json::from_value(json!({})).expect("metadata defaults");
    assert!(empty.interpretations.is_empty());
    assert!(
        serde_json::to_value(empty)
            .unwrap()
            .get("interpretations")
            .is_none()
    );
    let mut empty_document = document(vec![]);
    let explicit = lower_rulespec_str(&yaml(&empty_document)).expect("empty block loads");
    remove(&mut empty_document, "/module", "interpretations");
    let absent = lower_rulespec_str(&yaml(&empty_document)).expect("absent block loads");
    assert_eq!(
        serde_json::to_vec(&explicit).unwrap(),
        serde_json::to_vec(&absent).unwrap()
    );
}

fn execute_flat_tax(program: ProgramSpec, mode: &str) -> Vec<u8> {
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": mode,
        "program": program,
        "dataset": {
            "inputs": [{
                "name": "income", "entity": "Person", "entity_id": "person-1",
                "interval": {"start": "2025-01-01", "end": "2025-12-31"},
                "value": {"kind": "decimal", "value": "5000"}
            }],
            "relations": []
        },
        "queries": [{
            "entity_id": "person-1",
            "period": {"period_kind": "tax_year", "start": "2025-01-01", "end": "2025-12-31"},
            "outputs": ["income_tax", "net_income", "high_income"]
        }]
    }))
    .expect("execution request deserializes");
    let requested_mode = request.mode.clone();
    let response = execute_request(request).expect("existing flat-tax fixture executes");
    assert_eq!(response.metadata.requested_mode, requested_mode);
    assert_eq!(response.metadata.actual_mode, requested_mode);
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].outputs.len(), 3);
    serde_json::to_vec(&response).expect("response serializes")
}

#[test]
fn interpretations_roundtrip_and_leave_lowering_compilation_and_execution_byte_identical() {
    let baseline = lower_rulespec_str(FLAT_TAX).expect("existing fixture lowers");
    let baseline_artifact =
        CompiledProgramArtifact::from_rulespec_str(FLAT_TAX).expect("existing fixture compiles");
    for status in ["open", "ruled", "confirmed", "contradicted"] {
        let mut fixture: Value = serde_yaml::from_str(FLAT_TAX).expect("existing fixture parses");
        let mut entry = valid_entry(status);
        entry["affects"] = json!(["income_tax", "tax_rate"]);
        fixture["module"]["interpretations"] = json!([entry.clone()]);
        let fixture = yaml(&fixture);
        let annotated = lower_rulespec_str(&fixture).expect("annotated fixture lowers");
        assert_eq!(
            serde_json::to_value(&annotated.module.as_ref().unwrap().interpretations).unwrap(),
            json!([entry])
        );
        let program_json = serde_json::to_string(&annotated).unwrap();
        let roundtripped: ProgramSpec = serde_json::from_str(&program_json).unwrap();
        assert_eq!(program_json, serde_json::to_string(&roundtripped).unwrap());

        let mut baseline_program = baseline.clone();
        let mut annotated_program = annotated.clone();
        baseline_program.module = None;
        annotated_program.module = None;
        assert_eq!(
            serde_json::to_vec(&baseline_program).unwrap(),
            serde_json::to_vec(&annotated_program).unwrap(),
            "{status}: lowered program differs outside carried metadata"
        );

        let artifact = CompiledProgramArtifact::from_rulespec_str(&fixture)
            .expect("annotated fixture compiles");
        let artifact_json = serde_json::to_string(&artifact).unwrap();
        let reloaded = CompiledProgramArtifact::from_json_str(&artifact_json)
            .expect("compiled artifact reloads");
        assert_eq!(artifact_json, serde_json::to_string(&reloaded).unwrap());
        assert_eq!(
            annotated.module.as_ref().unwrap().interpretations,
            reloaded.program.module.as_ref().unwrap().interpretations
        );
        let mut baseline_compiled = baseline_artifact.clone();
        let mut annotated_compiled = artifact;
        baseline_compiled.program.module = None;
        annotated_compiled.program.module = None;
        assert_eq!(
            serde_json::to_vec(&baseline_compiled).unwrap(),
            serde_json::to_vec(&annotated_compiled).unwrap(),
            "{status}: compiled artifact differs outside carried metadata"
        );
        for mode in ["explain", "fast"] {
            assert_eq!(
                execute_flat_tax(baseline.clone(), mode),
                execute_flat_tax(annotated.clone(), mode),
                "{status}: {mode} execution differs"
            );
        }
    }
}

#[test]
fn compiled_artifact_loads_validate_interpretation_shape() {
    let source = yaml(&document(vec![valid_entry("confirmed")]));
    let artifact = CompiledProgramArtifact::from_rulespec_str(&source)
        .expect("valid interpreted artifact compiles");
    let artifact = serde_json::to_value(artifact).unwrap();
    CompiledProgramArtifact::from_json_str(&serde_json::to_string(&artifact).unwrap())
        .expect("valid interpretation metadata reloads");
    for (pointer, replacement, expected) in mutation_errors() {
        // Document-local membership has already been checked before lowering;
        // the compiled IR cannot reconstruct the original document boundary.
        if matches!(expected, MutationError::Affects) {
            continue;
        }
        let mut mutated = artifact.clone();
        set(
            &mut mutated["program"]["module"]["interpretations"][0],
            pointer,
            replacement,
        );
        let error =
            CompiledProgramArtifact::from_json_str(&serde_json::to_string(&mutated).unwrap())
                .expect_err("malformed interpretation in an artifact is refused");
        match error {
            CompileError::InvalidArtifactContract { path, message }
                if matches!(expected, MutationError::Citation) =>
            {
                assert_eq!(path, "<memory>");
                assert_eq!(message, "non-canonical corpus_citation_path `bad:path`");
            }
            CompileError::Spec(SpecError::InvalidModuleMetadata(error)) => {
                assert!(
                    is_expected_error(&error, &expected),
                    "{pointer}: got {error:?}"
                );
                assert!(error.to_string().contains("program.module"));
                assert!(
                    error.to_string().contains(ENTRY_ID) || matches!(expected, MutationError::Id)
                );
            }
            CompileError::DeserializeArtifact { .. } if matches!(expected, MutationError::Yaml) => {
            }
            other => panic!("{pointer}: expected {expected:?}, got {other:?}"),
        }
    }
    for field in ["ruling", "confirmation"] {
        let mut mutated = artifact.clone();
        remove(
            &mut mutated["program"]["module"]["interpretations"][0],
            "",
            field,
        );
        let error =
            CompiledProgramArtifact::from_json_str(&serde_json::to_string(&mutated).unwrap())
                .expect_err("missing required status object in an artifact is refused");
        assert!(matches!(
            error,
            CompileError::Spec(SpecError::InvalidModuleMetadata(error))
                if matches!(*error, RuleSpecError::InvalidInterpretationStatusFields { .. })
        ));
    }
}

fn generated_rule_name(stem: &str) -> String {
    format!("rule_{stem}")
}

#[test]
fn generated_rule_names_keep_formula_keywords_as_safe_identifiers() {
    let mut reserved = valid_entry("open");
    reserved["affects"] = json!(["to"]);
    lower_rulespec_str(&yaml(&document_for_entry(reserved)))
        .expect_err("a reserved keyword is not a rule identifier");

    // Valid names reach formula parsing even when a loader rejects keyword
    // names before generating formula source.
    let mut keyword_formula = document(vec![valid_entry("open")]);
    keyword_formula["rules"][0]["versions"][0]["formula"] = json!("to");
    let error = lower_rulespec_str(&yaml(&keyword_formula))
        .expect_err("a reserved keyword is not a formula identifier");
    assert!(matches!(&error, RuleSpecError::Formula(_)), "{error:?}");
    assert!(error.to_string().contains("To (to)"), "{error}");

    for keyword in [
        "to", "entity", "amend", "from", "match", "if", "elif", "else", "and", "or", "not", "true",
        "True", "false", "False",
    ] {
        let mut entry = valid_entry("open");
        entry["affects"] = json!([generated_rule_name(keyword)]);
        lower_rulespec_str(&yaml(&document_for_entry(entry)))
            .unwrap_or_else(|error| panic!("generated name for {keyword:?} must load: {error}"));
    }
}

fn valid_entries() -> impl Strategy<Value = Value> {
    (
        "[a-z0-9]{1,8}(-[a-z0-9]{1,8}){0,3}",
        0usize..4,
        1901u16..2100,
        1u8..13,
        1u8..29,
        "[a-z][a-z0-9_]{0,12}",
        "[a-zA-Z0-9 £é]{1,64}",
        any::<bool>(),
    )
        .prop_map(|(id, status, year, month, day, rule, text, optional)| {
            let mut entry = valid_entry(["open", "ruled", "confirmed", "contradicted"][status]);
            entry["id"] = json!(id);
            entry["affects"] = json!([generated_rule_name(&rule)]);
            entry["question"] = json!(format!("What is the reading of {text}?"));
            entry["reading"] = json!(format!("Adopted reading: {text}"));
            entry["basis"] = json!(format!("Basis: {text}"));
            entry["texts"][0]["excerpt"] = json!(format!("Provision: {text}"));
            entry["alternatives"][0]["reading"] = json!(format!("Other reading: {text}"));
            let date = format!("{year:04}-{month:02}-{day:02}");
            if status > 0 {
                entry["ruling"]["date"] = json!(date);
            }
            if status > 1 {
                entry["confirmation"]["date"] = json!(date);
                entry["confirmation"]["excerpt"] = json!(format!("Publisher reply: {text}"));
                if !optional {
                    remove(&mut entry, "/confirmation", "record");
                }
            }
            if !optional {
                remove(&mut entry, "/alternatives/0", "effect");
            }
            entry
        })
}

fn document_for_entry(entry: Value) -> Value {
    let name = entry["affects"][0].clone();
    let mut document = document(vec![entry]);
    document["rules"][0]["name"] = name;
    document
}

#[derive(Debug)]
enum MutationError {
    Id,
    Blank,
    Collection,
    Citation,
    Date,
    Record,
    Status,
    Affects,
    Yaml,
}

fn mutation_errors() -> Vec<(&'static str, Value, MutationError)> {
    vec![
        ("/id", json!("Bad_Id"), MutationError::Id),
        ("/question", json!(" \n"), MutationError::Blank),
        ("/reading", json!(" \n"), MutationError::Blank),
        ("/basis", json!(" \n"), MutationError::Blank),
        ("/texts", json!([]), MutationError::Collection),
        ("/alternatives", json!([]), MutationError::Collection),
        ("/affects", json!([]), MutationError::Collection),
        (
            "/texts/0/corpus_citation_path",
            json!("bad:path"),
            MutationError::Citation,
        ),
        ("/texts/0/excerpt", json!(" \n"), MutationError::Blank),
        (
            "/alternatives/0/reading",
            json!(" \n"),
            MutationError::Blank,
        ),
        ("/status", json!("unknown"), MutationError::Yaml),
        ("/ruling", Value::Null, MutationError::Yaml),
        ("/ruling/by", json!(" \n"), MutationError::Blank),
        ("/ruling/date", json!("2026-02-30"), MutationError::Date),
        (
            "/ruling/record",
            json!("http://example.org/reply"),
            MutationError::Record,
        ),
        ("/confirmation", Value::Null, MutationError::Yaml),
        ("/confirmation/by", json!(" \n"), MutationError::Blank),
        (
            "/confirmation/date",
            json!("2026-02-30"),
            MutationError::Date,
        ),
        ("/confirmation/excerpt", json!(" \n"), MutationError::Blank),
        (
            "/confirmation/record",
            json!("http://example.org/reply"),
            MutationError::Record,
        ),
        ("/affects", json!(["missing-rule"]), MutationError::Affects),
    ]
}

fn is_expected_error(error: &RuleSpecError, expected: &MutationError) -> bool {
    matches!(
        (error, expected),
        (
            RuleSpecError::InvalidInterpretationId { .. },
            MutationError::Id
        ) | (
            RuleSpecError::BlankInterpretationField { .. },
            MutationError::Blank
        ) | (
            RuleSpecError::EmptyInterpretationCollection { .. },
            MutationError::Collection
        ) | (
            RuleSpecError::InvalidInterpretationCitationPath { .. },
            MutationError::Citation
        ) | (
            RuleSpecError::InvalidInterpretationDate { .. },
            MutationError::Date
        ) | (
            RuleSpecError::InvalidInterpretationRecord { .. },
            MutationError::Record
        ) | (
            RuleSpecError::InvalidInterpretationStatusFields { .. },
            MutationError::Status
        ) | (
            RuleSpecError::UnresolvedInterpretationAffects { .. },
            MutationError::Affects
        ) | (RuleSpecError::Yaml(_), MutationError::Yaml)
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_interpretations_leave_execution_byte_identical(mut entry in valid_entries()) {
        let baseline = lower_rulespec_str(FLAT_TAX).expect("existing fixture lowers");
        let mut fixture: Value = serde_yaml::from_str(FLAT_TAX).unwrap();
        entry["affects"] = json!(["income_tax", "tax_rate"]);
        fixture["module"]["interpretations"] = json!([entry]);
        let annotated = lower_rulespec_str(&yaml(&fixture)).expect("annotated fixture lowers");
        for mode in ["explain", "fast"] {
            prop_assert_eq!(
                execute_flat_tax(baseline.clone(), mode),
                execute_flat_tax(annotated.clone(), mode)
            );
        }
    }

    #[test]
    fn valid_generated_interpretations_load_and_roundtrip(entry in valid_entries()) {
        let source = yaml(&document_for_entry(entry.clone()));
        let program = lower_rulespec_str(&source).expect("generated valid interpretation loads");
        prop_assert_eq!(
            serde_json::to_value(&program.module.as_ref().unwrap().interpretations).unwrap(),
            json!([entry])
        );
        let serialized = serde_json::to_string(&program).unwrap();
        let reloaded: ProgramSpec = serde_json::from_str(&serialized).unwrap();
        prop_assert_eq!(serialized, serde_json::to_string(&reloaded).unwrap());
        let artifact = CompiledProgramArtifact::from_rulespec_str(&source).unwrap();
        let serialized = serde_json::to_string(&artifact).unwrap();
        let reloaded = CompiledProgramArtifact::from_json_str(&serialized).unwrap();
        prop_assert_eq!(serialized, serde_json::to_string(&reloaded).unwrap());
    }

    #[test]
    fn one_field_mutations_are_refused_with_the_expected_error(entry in valid_entries()) {
        // Each generated valid base exercises every independent semantic mutation.
        for (pointer, replacement, expected) in mutation_errors() {
            if entry.pointer(pointer).is_none() {
                continue;
            }
            let mut document = document_for_entry(entry.clone());
            set(&mut document["module"]["interpretations"][0], pointer, replacement);
            let error = lower_rulespec_str(&yaml(&document)).expect_err("one invalid field must fail");
            prop_assert!(is_expected_error(&error, &expected), "{pointer}: expected {expected:?}, got {error:?}");
        }
        // Removing one required field at a time fails deserialization, independent
        // of its contents. Status-required objects have separate semantic errors.
        for (object, field) in required_fields() {
            if entry.pointer(&format!("{object}/{field}")).is_none() {
                continue;
            }
            let mut document = document_for_entry(entry.clone());
            remove(&mut document["module"]["interpretations"][0], object, field);
            let error = lower_rulespec_str(&yaml(&document)).expect_err("missing required field must fail");
            prop_assert!(matches!(error, RuleSpecError::Yaml(_)), "{object}/{field}: {error:?}");
        }
        for field in ["ruling", "confirmation"] {
            if entry.get(field).is_none() {
                continue;
            }
            let mut document = document_for_entry(entry.clone());
            remove(&mut document["module"]["interpretations"][0], "", field);
            let error = lower_rulespec_str(&yaml(&document)).expect_err("missing status-required object must fail");
            prop_assert!(is_expected_error(&error, &MutationError::Status), "{field}: {error:?}");
        }
    }
}
