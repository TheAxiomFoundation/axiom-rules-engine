//! Exercise every string boundary in RuleSpec's synthesized formula source.

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::rulespec::RuleSpecError;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{ScalarValueSpec, UnitKindSpec};
use serde_json::{Value, json};

const MARKERS: [&str; 4] = [
    "value\nsneaky:\n    from 2020-01-01:\n        42",
    "value\"quote'",
    "value\"\"\"",
    "value#comment",
];

fn document() -> Value {
    json!({
        "format": "rulespec/v1",
        "rules": [{
            "name": "amount",
            "kind": "parameter",
            "dtype": "Integer",
            "effective_from": "2020-01-01",
            "formula": "7"
        }]
    })
}

fn source_text(document: &Value) -> String {
    let mut source = serde_yaml::to_value(document).unwrap();
    if let Some(rules) = source
        .get_mut("rules")
        .and_then(serde_yaml::Value::as_sequence_mut)
    {
        for rule in rules {
            if let Some(versions) = rule
                .get_mut("versions")
                .and_then(serde_yaml::Value::as_sequence_mut)
            {
                for version in versions {
                    if let Some(values) = version
                        .get_mut("values")
                        .and_then(serde_yaml::Value::as_mapping_mut)
                    {
                        let entries = std::mem::take(values);
                        for (key, value) in entries {
                            let index = key.as_str().unwrap().parse::<i64>().unwrap();
                            values.insert(serde_yaml::to_value(index).unwrap(), value);
                        }
                    }
                }
            }
        }
    }
    serde_yaml::to_string(&source).unwrap()
}

fn compile(document: &Value) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(&source_text(document))
        .expect("audited string remains confined to its field")
}

fn is_invalid_formula_field(error: &CompileError, expected_field: &str, marker: &str) -> bool {
    matches!(
        error,
        CompileError::RuleSpec {
            error: RuleSpecError::InvalidFormulaField { field, value, .. },
            ..
        } if *field == expected_field && value == marker
    )
}

fn assert_only_amount(artifact: &CompiledProgramArtifact) {
    assert_eq!(artifact.program.parameters.len(), 1);
    assert_eq!(artifact.program.parameters[0].name, "amount");
    assert_eq!(
        artifact.program.parameters[0].versions[0].values[&0],
        ScalarValueSpec::Integer { value: 7 }
    );
    assert!(artifact.program.derived.is_empty());
    assert!(artifact.program.relations.is_empty());
}

#[test]
fn every_quoted_rule_metadata_field_confines_source_markers() {
    for field in [
        "dtype",
        "period",
        "unit",
        "label",
        "description",
        "indexed_by",
        "status",
        "source",
        "source_url",
    ] {
        for marker in MARKERS {
            let mut source = document();
            source["rules"][0][field] = json!(marker);
            let artifact = compile(&source);
            assert_only_amount(&artifact);
            let parameter = &artifact.program.parameters[0];
            match field {
                "unit" => assert_eq!(parameter.unit.as_deref(), Some(marker)),
                "indexed_by" => assert_eq!(parameter.indexed_by.as_deref(), Some(marker)),
                "source" => assert_eq!(parameter.source.as_deref(), Some(marker)),
                "source_url" => assert_eq!(parameter.source_url.as_deref(), Some(marker)),
                _ => {}
            }
        }
    }
}

#[test]
fn module_comment_fields_confine_source_markers() {
    for field in ["title", "status", "summary"] {
        for marker in MARKERS {
            let mut source = document();
            source["module"] = json!({field: marker});
            assert_only_amount(&compile(&source));
        }
    }
}

#[test]
fn header_only_modules_cannot_acquire_rules_and_keep_legacy_default_units() {
    let without_headers = compile(&json!({"format": "rulespec/v1", "rules": []}));
    assert!(without_headers.program.units.is_empty());
    for field in ["title", "status", "summary"] {
        let baseline = compile(&json!({
            "format": "rulespec/v1",
            "module": {field: "ordinary header"},
            "rules": []
        }));
        assert!(!baseline.program.units.is_empty());
        for marker in MARKERS {
            let artifact = compile(&json!({
                "format": "rulespec/v1",
                "module": {field: marker},
                "rules": []
            }));
            assert!(artifact.program.parameters.is_empty());
            assert!(artifact.program.derived.is_empty());
            assert!(artifact.program.relations.is_empty());
            assert_eq!(
                serde_json::to_value(&artifact.program.units).unwrap(),
                serde_json::to_value(&baseline.program.units).unwrap()
            );
        }
    }
}

#[test]
fn source_list_values_are_quoted_and_explicit_fields_take_precedence() {
    for marker in MARKERS {
        let mut source = document();
        source["rules"][0]["sources"] = json!([{"citation": marker, "url": marker}]);
        let artifact = compile(&source);
        assert_only_amount(&artifact);
        assert_eq!(
            artifact.program.parameters[0].source.as_deref(),
            Some(marker)
        );
        assert_eq!(
            artifact.program.parameters[0].source_url.as_deref(),
            Some(marker)
        );

        source["rules"][0]["source"] = json!("explicit citation");
        source["rules"][0]["source_url"] = json!("https://example.test/source");
        let artifact = compile(&source);
        assert_only_amount(&artifact);
        assert_eq!(
            artifact.program.parameters[0].source.as_deref(),
            Some("explicit citation")
        );
        assert_eq!(
            artifact.program.parameters[0].source_url.as_deref(),
            Some("https://example.test/source")
        );
    }
}

#[test]
fn raw_rule_names_reject_each_source_marker_in_normal_and_table_writers() {
    for marker in MARKERS.into_iter().chain([
        "amount:\n    from 2020-01-01:\n        42\nsneaky",
        "\"\"\"",
        "amount # ignored suffix",
        "amount \"\"\" ignored suffix \"\"\"",
        // Later stages match rules by the exact YAML name, so a name the
        // lexer would trim (a table stub would survive as an extra
        // parameter) is rejected too.
        " amount",
        "amount\n",
        "from",
        "amount\\\u{e9}",
    ]) {
        for body in ["inline", "version", "table"] {
            let mut source = document();
            source["rules"][0]["name"] = json!(marker);
            if body == "version" {
                source["rules"][0]["versions"] = json!([{
                    "effective_from": "2020-01-01",
                    "formula": "7"
                }]);
            } else if body == "table" {
                source["rules"][0]["indexed_by"] = json!("bucket");
                source["rules"][0]["versions"] = json!([{
                    "effective_from": "2020-01-01",
                    "values": {"0": 7}
                }]);
            }
            let error = CompiledProgramArtifact::from_rulespec_str(&source_text(&source))
                .expect_err("an unsafe name must fail");
            assert!(
                is_invalid_formula_field(&error, "name", marker),
                "unsafe name {marker:?}, body={body}: {error:?}"
            );
        }
    }
}

#[test]
fn raw_entity_rejects_each_source_marker_and_single_line_token_injection() {
    for marker in MARKERS.into_iter().chain([
        "Person\n    from 2020-01-01:\n        42\nsneaky:\n    entity: Person",
        "Person\n    from 2020-01-01:\n        42\n\"\"\"",
        "Person # ignored suffix",
        "Person \"\"\" ignored suffix \"\"\"",
        "Person from 2020-01-01: 42 sneaky: entity: Person",
        "from",
        "Person\\\u{e9}",
    ]) {
        for kind in ["parameter", "derived"] {
            let mut source = document();
            source["rules"][0]["kind"] = json!(kind);
            source["rules"][0]["entity"] = json!(marker);
            let error = CompiledProgramArtifact::from_rulespec_str(&source_text(&source))
                .expect_err("an unsafe entity must fail");
            assert!(
                is_invalid_formula_field(&error, "entity", marker),
                "unsafe entity {marker:?}, kind={kind}: {error:?}"
            );
        }
    }
}

#[test]
fn a_malformed_name_is_reported_before_a_missing_formula() {
    // The name is checked without lexing it, so a backslash before a
    // multibyte character cannot reach the lexer's escape handling.
    let mut source = document();
    source["rules"][0]["name"] = json!("amount\\\u{e9}");
    source["rules"][0]
        .as_object_mut()
        .unwrap()
        .remove("formula");
    let error = CompiledProgramArtifact::from_rulespec_str(&source_text(&source))
        .expect_err("a malformed name must fail");
    assert!(
        is_invalid_formula_field(&error, "name", "amount\\\u{e9}"),
        "{error:?}"
    );
}

#[test]
fn legal_names_paths_and_whitespace_around_entities_keep_their_meaning() {
    for name in ["amount", "_amount2", "policy/amount"] {
        let mut source = document();
        source["rules"][0]["name"] = json!(name);
        let artifact = compile(&source);
        assert_eq!(artifact.program.parameters[0].name, name);
    }
    for entity in ["Person", " Person ", "\tPerson\t"] {
        let mut source = document();
        source["rules"][0]["kind"] = json!("derived");
        source["rules"][0]["entity"] = json!(entity);
        let artifact = compile(&source);
        assert_eq!(artifact.program.derived[0].entity, "Person");
    }
}

struct ImportSource {
    root: String,
}

impl ModuleSource for ImportSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(match target {
            "us:policies/root" => Some(self.root.clone()),
            "us:policies/base" => Some(document().to_string()),
            _ => None,
        })
    }
}

#[test]
fn imports_are_resolved_as_targets_instead_of_interpolated_as_source() {
    for marker in MARKERS {
        let source = ImportSource {
            root: json!({
                "format": "rulespec/v1",
                "imports": [format!("us:policies/base{marker}")],
                "rules": []
            })
            .to_string(),
        };
        assert!(
            CompiledProgramArtifact::from_rulespec_with_source("us:policies/root", &source)
                .is_err()
        );
    }
    let source = ImportSource {
        root: json!({
            "format": "rulespec/v1",
            "imports": ["us:policies/base#amount"],
            "rules": []
        })
        .to_string(),
    };
    assert_only_amount(
        &CompiledProgramArtifact::from_rulespec_with_source("us:policies/root", &source).unwrap(),
    );
}

#[test]
fn table_text_values_and_enum_type_labels_cannot_become_declarations() {
    for marker in MARKERS {
        let mut source = document();
        source["rules"][0]["dtype"] = json!(format!("Enum[{marker}]"));
        source["rules"][0]["indexed_by"] = json!("bucket");
        source["rules"][0]["versions"] = json!([{
            "effective_from": "2020-01-01",
            "values": {"0": marker}
        }]);
        let artifact = compile(&source);
        assert_eq!(artifact.program.parameters.len(), 1);
        assert!(artifact.program.derived.is_empty());
        assert_eq!(
            artifact.program.parameters[0].versions[0].values[&0],
            ScalarValueSpec::Text {
                value: marker.to_string()
            }
        );
    }
}

#[test]
fn formulas_reject_extra_declarations_and_unclosed_docstrings() {
    for formula in ["7\nsneaky:\n    from 2020-01-01:\n        42", "7\n\"\"\""] {
        let mut source = document();
        source["rules"][0]["formula"] = json!(formula);
        source["rules"].as_array_mut().unwrap().push(json!({
            "name": "later",
            "kind": "parameter",
            "effective_from": "2020-01-01",
            "formula": "9"
        }));
        let error = CompiledProgramArtifact::from_rulespec_str(&source_text(&source))
            .expect_err("a formula must not escape its rule");
        let expected = if formula.contains("\"\"\"") {
            matches!(
                &error,
                CompileError::RuleSpec {
                    error: RuleSpecError::UnterminatedFormulaBlock { name },
                    ..
                } if name == "amount"
            )
        } else {
            matches!(
                &error,
                CompileError::RuleSpec {
                    error: RuleSpecError::Formula(_),
                    ..
                }
            )
        };
        assert!(expected, "formula {formula:?}: {error:?}");
    }
}

#[test]
fn table_stub_metadata_quotes_all_source_markers() {
    for marker in MARKERS {
        let mut source = document();
        for field in ["dtype", "unit", "indexed_by", "source", "source_url"] {
            source["rules"][0][field] = json!(marker);
        }
        source["rules"][0]["versions"] = json!([{
            "effective_from": "2020-01-01",
            "values": {"0": 7}
        }]);
        let artifact = compile(&source);
        assert_only_amount(&artifact);
        let parameter = &artifact.program.parameters[0];
        assert_eq!(parameter.unit.as_deref(), Some(marker));
        assert_eq!(parameter.indexed_by.as_deref(), Some(marker));
        assert_eq!(parameter.source.as_deref(), Some(marker));
        assert_eq!(parameter.source_url.as_deref(), Some(marker));
    }
}

#[test]
fn typed_module_metadata_stays_data_and_provenance_constraints_still_reject() {
    for marker in MARKERS {
        let mut source = document();
        source["module"] = json!({
            "source_verification": {
                "corpus_citation_path": "us/statute/7/2017/a",
                "upstream_source_check": {
                    "status": marker,
                    "checked_paths": [marker],
                    "rationale": marker
                }
            },
            "encoding_provenance": {
                "encoder": marker,
                "model": marker,
                "run_id": marker,
                "reviewed_by": marker
            },
            "validation": [{"oracle": marker, "status": "pending"}]
        });
        let artifact = compile(&source);
        assert_only_amount(&artifact);
        let module = serde_json::to_value(artifact.program.module.as_ref().unwrap()).unwrap();
        assert_eq!(
            module["source_verification"],
            source["module"]["source_verification"]
        );
        assert_eq!(
            module["encoding_provenance"],
            source["module"]["encoding_provenance"]
        );
        assert_eq!(module["validation"], source["module"]["validation"]);

        for field in ["corpus_citation_path", "source_sha256"] {
            let mut invalid = source.clone();
            invalid["module"]["source_verification"][field] = json!(marker);
            assert!(CompiledProgramArtifact::from_rulespec_str(&source_text(&invalid)).is_err());
        }
        source["module"]["validation"][0]["status"] = json!(marker);
        assert!(CompiledProgramArtifact::from_rulespec_str(&source_text(&source)).is_err());
    }
}

#[test]
fn custom_unit_names_and_labels_stay_typed_data() {
    for marker in MARKERS {
        let mut source = document();
        source["units"] = json!([{"name": marker, "kind": "custom", "label": marker}]);
        let artifact = compile(&source);
        assert_only_amount(&artifact);
        let unit = artifact
            .program
            .units
            .iter()
            .find(|unit| unit.name == marker)
            .unwrap();
        assert!(matches!(&unit.kind, UnitKindSpec::Custom { label } if label == marker));
    }
}

#[test]
fn structural_relation_names_entities_and_references_stay_typed_data() {
    for marker in MARKERS {
        let mut source = document();
        source["rules"].as_array_mut().unwrap().extend([
            json!({
                "name": marker,
                "kind": "data_relation",
                "data_relation": {"arity": 2}
            }),
            json!({
                "name": "filtered",
                "kind": "derived_relation",
                "derived_relation": {
                    "arity": 2,
                    "source_relation": marker,
                    "entity": marker,
                    "member_relation": marker,
                    "slot_entities": [marker, "Other"],
                    "current_slot": 0,
                    "related_slot": 1
                },
                "effective_from": "2020-01-01",
                "formula": "true"
            }),
        ]);
        let artifact = compile(&source);
        assert_eq!(artifact.program.parameters.len(), 1);
        assert_eq!(artifact.program.parameters[0].name, "amount");
        assert!(artifact.program.derived.is_empty());
        assert_eq!(artifact.program.relations.len(), 2);
        assert!(
            artifact
                .program
                .relations
                .iter()
                .any(|relation| relation.name == marker)
        );
        let filtered = artifact
            .program
            .relations
            .iter()
            .find(|relation| relation.name == "filtered")
            .unwrap();
        let derivation = filtered.derivation.as_ref().unwrap();
        assert_eq!(derivation.source_relation.as_deref(), Some(marker));
        assert_eq!(derivation.entity.as_deref(), Some(marker));
        assert_eq!(derivation.member_relation.as_deref(), Some(marker));
        assert_eq!(derivation.slot_entities, [marker, "Other"]);

        let mut arguments = document();
        arguments["rules"].as_array_mut().unwrap().push(json!({
            "name": "audited_relation",
            "kind": "data_relation",
            "data_relation": {"arity": 1, "arguments": [marker]}
        }));
        let artifact = compile(&arguments);
        assert_eq!(artifact.program.parameters.len(), 1);
        assert_eq!(artifact.program.parameters[0].name, "amount");
        assert!(artifact.program.derived.is_empty());
        assert_eq!(artifact.program.relations.len(), 1);
        assert!(artifact.program.relations[0].slot_entities.is_empty());
        assert!(
            artifact
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "invalid_relation_argument_entity_shape")
        );

        arguments["rules"][1]["data_relation"]["arguments"] =
            json!([{"name": marker, "entity": "Person"}]);
        let artifact = compile(&arguments);
        assert_eq!(artifact.program.parameters.len(), 1);
        assert!(artifact.program.derived.is_empty());
        assert_eq!(artifact.program.relations.len(), 1);
        assert_eq!(artifact.program.relations[0].slot_entities, ["Person"]);
    }
}

#[test]
fn source_relation_references_stay_structural_or_fail_validation() {
    for marker in MARKERS {
        let mut source = document();
        source["rules"].as_array_mut().unwrap().push(json!({
            "name": marker,
            "kind": "source_relation",
                "source_relation": {
                "type": "restates",
                "target": "us:policies/base#amount",
                "authority": marker,
                "amendment": {"operation": marker, "effective": marker},
            },
            "verification": {"note": marker},
        }));
        assert_only_amount(&compile(&source));
        for field in [
            "target",
            "value",
            "basis.delegation",
            "amendment.superseding_rule",
        ] {
            let mut reference = source.clone();
            let value = json!(format!("us:policies/base#{marker}"));
            match field {
                "basis.delegation" => {
                    reference["rules"][1]["source_relation"]["basis"] = json!({"delegation": value})
                }
                "amendment.superseding_rule" => {
                    reference["rules"][1]["source_relation"]["amendment"]["superseding_rule"] =
                        value
                }
                _ => reference["rules"][1]["source_relation"][field] = value,
            }
            if marker.contains('\n') {
                let error = CompiledProgramArtifact::from_rulespec_str(&source_text(&reference))
                    .expect_err("a source reference cannot contain whitespace");
                assert!(matches!(
                    error,
                    CompileError::RuleSpec {
                        error: RuleSpecError::InvalidSourceRelationReference {
                            field: rejected_field,
                            value: rejected_value,
                            ..
                        },
                        ..
                    } if rejected_field == field && rejected_value == format!("us:policies/base#{marker}")
                ));
            } else {
                assert_only_amount(&compile(&reference));
            }
        }
    }
}

#[test]
fn kind_discriminator_default_and_dates_reject_source_markers() {
    for marker in MARKERS {
        for field in [
            "kind",
            "default",
            "rounding",
            "effective_from",
            "effective_to",
        ] {
            let mut source = document();
            source["rules"][0][field] = json!(marker);
            assert!(CompiledProgramArtifact::from_rulespec_str(&source_text(&source)).is_err());
        }
        let mut source = document();
        source["format"] = json!(marker);
        assert!(CompiledProgramArtifact::from_rulespec_str(&source_text(&source)).is_err());
    }
}
