//! Module metadata and formula string literals cannot change the compiled
//! program (issues 237 and 238).
//!
//! RuleSpec lowers YAML by writing formula source text and parsing it. These
//! tests pin metadata confinement, literal fidelity, and preservation of the
//! declared rule set when arbitrary names and entities are validated before
//! source synthesis.

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules_engine::spec::ScalarValueSpec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

/// Issue 237's first report: a title line that opens a `"""` block.
const ERASE_RULES: &str = "Hi\n\"\"\"";
/// Issue 237's second report: a title line that declares a parameter.
const ADD_RULE: &str = "Hi\nsneaky:\n    from 2020-01-01:\n        42";
/// Issue 238's report: a Text literal with trailing spaces and a raw line break.
const MULTILINE_TEXT: &str = "'a  \nb'";

fn module(formula: &str, dtype: &str) -> Value {
    json!({
        "format": "rulespec/v1",
        "rules": [{
            "name": "sentinel",
            "kind": "parameter",
            "dtype": dtype,
            "versions": [{"effective_from": "2020-01-01", "formula": formula}]
        }]
    })
}

fn source(module: &Value) -> String {
    serde_yaml::to_string(module).expect("serialize RuleSpec fixture")
}

fn compile(module: &Value) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::from_rulespec_str(&source(module))
}

/// The serialized artifact without the module metadata it carries through,
/// and without `source_closure`, which hashes the module text and so records
/// the metadata by design.
fn program_without_module_metadata(artifact: &CompiledProgramArtifact) -> Value {
    let mut artifact = serde_json::to_value(artifact).expect("serialize artifact");
    artifact["program"]
        .as_object_mut()
        .expect("artifact program is an object")
        .remove("module");
    artifact
        .as_object_mut()
        .expect("artifact is an object")
        .remove("source_closure");
    artifact
}

fn assert_program_unchanged(field: &str, payload: &str) {
    let baseline = module("7", "Integer");
    let expected = compile(&baseline).expect("baseline compiles");
    let mut with_metadata = baseline;
    with_metadata["module"] = json!({field: payload});
    let actual = compile(&with_metadata).expect("metadata must not prevent compilation");
    assert_eq!(
        program_without_module_metadata(&actual),
        program_without_module_metadata(&expected),
        "module.{field} changed the compiled program"
    );
    let carried = serde_json::to_value(actual.program.module).expect("serialize module");
    assert_eq!(carried[field], payload, "module.{field} must stay data");
}

#[test]
fn title_cannot_erase_rules_with_a_triple_quote() {
    assert_program_unchanged("title", ERASE_RULES);
}

#[test]
fn title_cannot_add_a_rule() {
    assert_program_unchanged("title", ADD_RULE);
}

#[test]
fn module_status_cannot_erase_rules_with_a_triple_quote() {
    assert_program_unchanged("status", ERASE_RULES);
}

#[test]
fn module_status_cannot_add_a_rule() {
    assert_program_unchanged("status", ADD_RULE);
}

#[test]
fn crlf_title_and_status_cannot_add_a_rule() {
    let payload = ADD_RULE.replace('\n', "\r\n");
    assert_program_unchanged("title", &payload);
    assert_program_unchanged("status", &payload);
}

fn text_value(artifact: &CompiledProgramArtifact) -> &str {
    assert_eq!(artifact.program.parameters.len(), 1);
    let parameter = &artifact.program.parameters[0];
    assert_eq!(parameter.name, "sentinel");
    assert_eq!(parameter.versions.len(), 1);
    assert_eq!(parameter.versions[0].values.len(), 1);
    let value = parameter.versions[0]
        .values
        .values()
        .next()
        .expect("literal parameter has a value");
    match value {
        ScalarValueSpec::Text { value } => value,
        other => panic!("expected Text value, got {other:?}"),
    }
}

/// The error a raw line break inside a string literal must produce.
fn is_multiline_literal_error(error: &CompileError) -> bool {
    matches!(
        error,
        CompileError::RuleSpec {
            error: RuleSpecError::MultilineStringLiteral { name, .. },
            ..
        } if name == "sentinel"
    )
}

#[test]
fn multiline_text_literal_is_rejected_with_the_rule_and_the_escape() {
    let error = compile(&module(MULTILINE_TEXT, "Text"))
        .expect_err("a literal the formula writer would change must not compile");
    assert!(
        matches!(
            &error,
            CompileError::RuleSpec {
                error: RuleSpecError::MultilineStringLiteral { name, line: 1, col: 1 },
                ..
            } if name == "sentinel"
        ),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("`sentinel`"), "{message}");
    assert!(message.contains("\\n escape"), "{message}");
}

#[test]
fn multiline_literal_error_points_at_the_literal_as_written() {
    // A leading blank line, then the second literal on line 4 starts at
    // character 17 (byte 18: the earlier literal holds a two-byte character).
    let formula = "\nmatch 2:\n    1 => 'no'\n    _ => '\u{e9}' == 'a\nb'";
    let error = compile(&module(formula, "Boolean")).expect_err("raw line break in a literal");
    assert!(
        matches!(
            &error,
            CompileError::RuleSpec {
                error: RuleSpecError::MultilineStringLiteral { name, line: 4, col: 17 },
                ..
            } if name == "sentinel"
        ),
        "{error:?}"
    );
}

#[test]
fn escaped_line_break_in_a_text_literal_keeps_its_value() {
    let artifact =
        compile(&module("'a  \\nb'", "Text")).expect("the recommended newline escape must compile");
    assert_eq!(text_value(&artifact), "a  \nb");
}

#[test]
fn backslash_before_a_raw_line_break_is_rejected() {
    for formula in ["'a\\\nb'", "'a\\\r\nb'"] {
        let error = compile(&module(formula, "Text"))
            .expect_err("an escaped raw line break is still rewritten");
        assert!(is_multiline_literal_error(&error), "{error:?}");
    }
}

#[test]
fn lone_carriage_return_in_a_text_literal_keeps_its_value() {
    // The writer splits lines only at LF, so a CR inside a literal survives.
    let artifact = compile(&module("'a \r b'", "Text")).expect("a lone CR is not a line break");
    assert_eq!(text_value(&artifact), "a \r b");
}

#[test]
fn line_breaks_outside_string_literals_still_compile() {
    // A closed `"""` block, a comment and a literal each on their own lines.
    let formula = "\"\"\"\nA note that it's 'quoted'\n\"\"\"\n'a  b'  # comment 'x\n";
    let artifact = compile(&module(formula, "Text")).expect("multi-line formula compiles");
    assert_eq!(text_value(&artifact), "a  b");
}

#[test]
fn unclosed_triple_quote_in_a_formula_is_rejected() {
    for formula in ["7\n\"\"\"", "7 \"\"\" note"] {
        let error = compile(&module(formula, "Integer"))
            .expect_err("an unclosed block would hide later rules");
        assert!(
            matches!(
                &error,
                CompileError::RuleSpec {
                    error: RuleSpecError::UnterminatedFormulaBlock { name },
                    ..
                } if name == "sentinel"
            ),
            "{error:?}"
        );
    }
}

/// Strings that look like formula source once they reach a line of their own.
fn metadata_payloads() -> Vec<String> {
    [
        "",
        "\n",
        "\r\n",
        "\r",
        "\"",
        "'",
        "\"\"\"",
        "#",
        "\\",
        ERASE_RULES,
        ADD_RULE,
        "\nsentinel:\n    from 2019-01-01:\n        9\n",
        "\nentity Hijacked:\n    name: Text\n",
        "\nstatus: draft\n",
        "\namend sentinel:\n    from 2021-01-01:\n        8\n",
        "\nsneaky:\n    entity: Person\n    from 2020-01-01:\n        1\n",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn metadata_string() -> BoxedStrategy<String> {
    prop_oneof![
        3 => proptest::collection::vec(any::<char>(), 0..64)
            .prop_map(|characters| characters.into_iter().collect()),
        2 => proptest::sample::select(metadata_payloads()),
        3 => proptest::collection::vec(
            prop_oneof![
                2 => proptest::sample::select(metadata_payloads()),
                1 => proptest::collection::vec(any::<char>(), 0..8)
                    .prop_map(|characters| characters.into_iter().collect::<String>()),
            ],
            0..8,
        )
        .prop_map(|parts| parts.concat()),
    ]
    .boxed()
}

fn runner(cases: u32, seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

/// Property: for any title, status and summary, the compiled program equals
/// the program compiled with those fields removed, and the fields themselves
/// are carried through unchanged as module metadata.
#[test]
fn arbitrary_module_metadata_cannot_change_the_program() {
    let baseline = compile(&module("7", "Integer")).expect("property baseline compiles");
    let expected = program_without_module_metadata(&baseline);
    runner(256, 237)
        .run(
            &(metadata_string(), metadata_string(), metadata_string()),
            |(title, status, summary)| {
                let mut input = module("7", "Integer");
                input["module"] = json!({"title": title, "status": status, "summary": summary});
                let artifact = compile(&input).map_err(|error| {
                    TestCaseError::fail(format!("metadata prevented compilation: {error}"))
                })?;
                prop_assert_eq!(program_without_module_metadata(&artifact), expected.clone());
                let carried = artifact.program.module.expect("module metadata is carried");
                prop_assert_eq!(carried.title.as_deref(), Some(title.as_str()));
                prop_assert_eq!(carried.status.as_deref(), Some(status.as_str()));
                prop_assert_eq!(carried.summary.as_deref(), Some(summary.as_str()));
                Ok(())
            },
        )
        .expect("module metadata must not affect the compiled program");
}

/// Valid identifiers keep the property exercising successful lowering as
/// well as rejection of arbitrary Unicode and source-shaped values.
fn source_identifier(allow_path: bool) -> BoxedStrategy<(String, bool)> {
    let valid = if allow_path {
        "rule_[A-Za-z0-9_]{0,12}(/[A-Za-z_][A-Za-z0-9_]{0,12})?"
    } else {
        "[ \\t\\r\\n\\x0C]{0,4}Entity_[A-Za-z0-9_]{0,12}[ \\t\\r\\n\\x0C]{0,4}"
    };
    prop_oneof![
        2 => valid.prop_map(|value| (value, true)),
        3 => metadata_string().prop_map(|value| (value, false)),
    ]
    .boxed()
}

/// For any metadata, rule name and entity, lowering either preserves exactly
/// the declared rule or explicitly rejects an unsafe unquoted field. Exercise
/// both the ordinary formula writer and the indexed-parameter stub writer.
#[test]
fn arbitrary_metadata_and_identifiers_preserve_the_declared_rule_set() {
    runner(256, 246)
        .run(
            &(
                source_identifier(true),
                source_identifier(false),
                metadata_string(),
                metadata_string(),
                metadata_string(),
            ),
            |((name, valid_name), (entity, valid_entity), title, status, summary)| {
                for table in [false, true] {
                    let mut input = module("7", "Integer");
                    input["module"] = json!({"title": title, "status": status, "summary": summary});
                    input["rules"][0]["name"] = json!(name);
                    if table {
                        input["rules"][0]["indexed_by"] = json!("bucket");
                        input["rules"][0]["versions"][0]
                            .as_object_mut()
                            .expect("version fixture")
                            .remove("formula");
                    } else {
                        input["rules"][0]["kind"] = json!("derived");
                        input["rules"][0]["entity"] = json!(entity);
                    }
                    let mut yaml = serde_yaml::to_value(&input).expect("fixture YAML value");
                    if table {
                        // YAML table indices are integers, unlike JSON object keys.
                        yaml["rules"][0]["versions"][0]["values"] =
                            serde_yaml::to_value(std::collections::BTreeMap::from([(0_i64, 7)]))
                                .expect("integer table fixture");
                    }
                    let lowered = lower_rulespec_str(
                        &serde_yaml::to_string(&yaml).expect("serialize fixture YAML"),
                    );
                    if valid_name && (table || valid_entity) {
                        prop_assert!(lowered.is_ok(), "valid fields failed: {lowered:?}");
                    }
                    match lowered {
                        Ok(program) => {
                            let names = program
                                .parameters
                                .iter()
                                .map(|rule| rule.name.as_str())
                                .chain(program.derived.iter().map(|rule| rule.name.as_str()))
                                .chain(program.relations.iter().map(|rule| rule.name.as_str()))
                                .collect::<Vec<_>>();
                            prop_assert_eq!(names, vec![name.as_str()]);
                            if !table && !entity.is_empty() {
                                prop_assert_eq!(
                                    program.derived[0].entity.as_str(),
                                    entity.trim_matches(
                                        |character: char| character.is_ascii_whitespace()
                                    )
                                );
                            }
                            let carried = program.module.expect("module metadata is carried");
                            prop_assert_eq!(carried.title.as_deref(), Some(title.as_str()));
                            prop_assert_eq!(carried.status.as_deref(), Some(status.as_str()));
                            prop_assert_eq!(carried.summary.as_deref(), Some(summary.as_str()));
                        }
                        Err(RuleSpecError::InvalidFormulaField {
                            name: rule,
                            field,
                            value,
                            ..
                        }) => {
                            prop_assert_eq!(rule, name.clone());
                            match field {
                                "name" => prop_assert_eq!(value, name.clone()),
                                "entity" if !table => prop_assert_eq!(value, entity.clone()),
                                _ => return Err(TestCaseError::fail("unexpected invalid field")),
                            }
                        }
                        Err(error) => return Err(TestCaseError::fail(error.to_string())),
                    }
                }
                Ok(())
            },
        )
        .expect("metadata and identifiers must preserve the declared rule set or fail explicitly");
}

#[derive(Clone, Debug)]
enum Piece {
    Character(char),
    RawLineFeed,
    RawCarriageReturn,
    /// A backslash escape and the character the lexer decodes it to.
    Escape(&'static str, char),
}

/// A quoted literal the lexer accepts, and its value. The value is tracked
/// piece by piece from the lexer's escape table, independently of how RuleSpec
/// writes the formula.
#[derive(Clone, Debug)]
struct TextLiteral {
    formula: String,
    value: String,
}

fn text_literal() -> BoxedStrategy<TextLiteral> {
    let piece = prop_oneof![
        6 => any::<char>().prop_map(Piece::Character),
        3 => proptest::sample::select(vec![' ', '\t', '#', '\'', '"', '\\', ':'])
            .prop_map(Piece::Character),
        1 => Just(Piece::RawLineFeed),
        1 => Just(Piece::RawCarriageReturn),
        2 => proptest::sample::select(vec![
            Piece::Escape("\\\\", '\\'),
            Piece::Escape("\\\"", '"'),
            Piece::Escape("\\'", '\''),
            Piece::Escape("\\n", '\n'),
            Piece::Escape("\\r", '\r'),
            Piece::Escape("\\t", '\t'),
            Piece::Escape("\\a", 'a'),
            Piece::Escape("\\#", '#'),
            Piece::Escape("\\ ", ' '),
        ]),
    ];
    (any::<bool>(), proptest::collection::vec(piece, 0..64))
        .prop_map(|(double_quoted, pieces)| {
            let quote = if double_quoted { '"' } else { '\'' };
            let mut formula = String::from(quote);
            let mut value = String::new();
            for piece in pieces {
                match piece {
                    Piece::Character(character) => {
                        if character == quote || character == '\\' {
                            formula.push('\\');
                        }
                        formula.push(character);
                        value.push(character);
                    }
                    Piece::RawLineFeed => {
                        formula.push('\n');
                        value.push('\n');
                    }
                    Piece::RawCarriageReturn => {
                        formula.push('\r');
                        value.push('\r');
                    }
                    Piece::Escape(escape, decoded) => {
                        formula.push_str(escape);
                        value.push(decoded);
                    }
                }
            }
            formula.push(quote);
            TextLiteral { formula, value }
        })
        .boxed()
}

/// Property: a Text literal compiles to exactly its value, or, when it holds
/// a raw line break (which the formula writer would rewrite), compilation
/// fails with the error that names the rule and the escape.
#[test]
fn arbitrary_text_literals_keep_their_value_or_fail_explicitly() {
    runner(512, 238)
        .run(&text_literal(), |literal| {
            let input = module(&literal.formula, "Text");
            let parsed: Value = serde_yaml::from_str(&source(&input)).expect("fixture YAML");
            prop_assert_eq!(
                parsed["rules"][0]["versions"][0]["formula"].as_str(),
                Some(literal.formula.as_str()),
                "the YAML fixture must carry the formula unchanged"
            );
            match compile(&input) {
                Ok(artifact) => {
                    prop_assert!(!literal.formula.contains('\n'));
                    prop_assert_eq!(text_value(&artifact), literal.value.as_str());
                }
                Err(error) => {
                    prop_assert!(literal.formula.contains('\n'), "{error}");
                    prop_assert!(is_multiline_literal_error(&error), "{error:?}");
                }
            }
            Ok(())
        })
        .expect("a Text literal must keep its value or fail explicitly");
}

#[cfg(feature = "fs")]
mod cli {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
        program: PathBuf,
        output: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .canonicalize()
                .expect("system temp directory has an exact path")
                .join(format!(
                    "axiom-rules-engine-source-integrity-{}-{}",
                    std::process::id(),
                    NONCE.fetch_add(1, Ordering::Relaxed)
                ));
            let root = directory.join("rulespec-us");
            let program = root.join("us/policies/source-integrity.yaml");
            fs::create_dir_all(program.parent().expect("fixture parent"))
                .expect("create CLI RuleSpec root");
            let output = directory.join("compiled.json");
            Self {
                directory,
                root,
                program,
                output,
            }
        }

        fn compile(&self, module: &Value) -> Output {
            fs::write(&self.program, source(module)).expect("write CLI RuleSpec fixture");
            Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
                .arg("compile")
                .arg("--program")
                .arg(&self.program)
                .arg("--rulespec-root")
                .arg(&self.root)
                .arg("--output")
                .arg(&self.output)
                .output()
                .expect("run compile CLI")
        }

        fn artifact(&self) -> CompiledProgramArtifact {
            serde_json::from_slice(&fs::read(&self.output).expect("read CLI artifact"))
                .expect("deserialize CLI artifact")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn assert_cli_program_unchanged(field: &str, payload: &str) {
        let fixture = Fixture::new();
        let mut with_metadata = module("7", "Integer");
        let baseline_output = fixture.compile(&with_metadata);
        assert!(
            baseline_output.status.success(),
            "baseline CLI compilation failed: {}",
            String::from_utf8_lossy(&baseline_output.stderr)
        );
        let baseline = fixture.artifact();
        with_metadata["module"] = json!({field: payload});
        let output = fixture.compile(&with_metadata);
        assert!(
            output.status.success(),
            "metadata prevented CLI compilation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            program_without_module_metadata(&fixture.artifact()),
            program_without_module_metadata(&baseline),
            "module.{field} changed the CLI's compiled program"
        );
    }

    #[test]
    fn title_cannot_erase_rules_with_a_triple_quote() {
        assert_cli_program_unchanged("title", ERASE_RULES);
    }

    #[test]
    fn title_cannot_add_a_rule() {
        assert_cli_program_unchanged("title", ADD_RULE);
    }

    #[test]
    fn module_status_cannot_erase_rules_with_a_triple_quote() {
        assert_cli_program_unchanged("status", ERASE_RULES);
    }

    #[test]
    fn module_status_cannot_add_a_rule() {
        assert_cli_program_unchanged("status", ADD_RULE);
    }

    #[test]
    fn multiline_text_literal_is_rejected_with_the_rule_and_the_escape() {
        let fixture = Fixture::new();
        let output = fixture.compile(&module(MULTILINE_TEXT, "Text"));
        assert!(
            !output.status.success(),
            "a literal the formula writer would change must not compile"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("`sentinel`"), "{error}");
        assert!(error.contains("\\n escape"), "{error}");
    }
}
