//! A backslash escape in a formula string literal decodes one whole
//! character, whatever its UTF-8 width. The lexer once decoded only the first
//! byte of a multibyte escaped character and left its cursor inside that
//! character, so its next slice of the source panicked: the CLI exited 101,
//! the PyO3 extension raised `PanicException`, and wasm trapped.

use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules_engine::spec::ScalarValueSpec;
use std::process::Command;

fn text_parameter_module(formula: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
  - name: escaped_label
    kind: parameter
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: |-
          {formula}
"#
    )
}

fn lowered_text(formula: &str) -> String {
    let program = lower_rulespec_str(&text_parameter_module(formula))
        .unwrap_or_else(|error| panic!("{formula} should lower: {error}"));
    let parameter = program
        .parameters
        .iter()
        .find(|parameter| parameter.name == "escaped_label")
        .expect("escaped_label parameter");
    match &parameter.versions[0].values[&0] {
        ScalarValueSpec::Text { value } => value.clone(),
        other => panic!("{formula} lowered to {other:?}, not text"),
    }
}

#[test]
fn escaped_multibyte_character_decodes_to_that_character() {
    for (formula, expected) in [
        // The audit reproduction: the closing-quote slice started inside `é`.
        (r#""\é""#, "é"),
        // The slice before a second escape started inside `é`.
        (r#""\é\n""#, "é\n"),
        (r#"'\é'"#, "é"),
        (r#""caf\é au lait""#, "café au lait"),
        // Three- and four-byte characters.
        (r#""\€5""#, "€5"),
        (r#""\𝄞""#, "𝄞"),
        (r#""\é\ü\ß""#, "éüß"),
    ] {
        assert_eq!(lowered_text(formula), expected, "formula {formula}");
    }
}

#[test]
fn unknown_escape_decodes_to_the_escaped_character() {
    // Multibyte escapes follow the rule ASCII escapes already had: an escape
    // the lexer does not name stands for the character after the backslash.
    assert_eq!(lowered_text(r#""\q\u""#), "qu");
    assert_eq!(lowered_text(r#""\"\'\\\n\r\t""#), "\"'\\\n\r\t");
}

#[test]
fn unterminated_multibyte_escape_is_a_parse_error() {
    let error = lower_rulespec_str(&text_parameter_module(r#""\é"#))
        .expect_err("a string with no closing quote must not lower");
    assert!(
        matches!(error, RuleSpecError::Formula(_)),
        "expected a formula error, got {error:?}"
    );
    assert!(error.to_string().contains("unterminated string"), "{error}");
}

#[test]
fn cli_compiles_a_multibyte_escape_instead_of_panicking() {
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-string-escape-test-{}",
            std::process::id()
        ));
    let rulespec_root = temp_root.join("rulespec-us");
    let program_path = rulespec_root.join("us/policies/tests/escape.yaml");
    let artifact_path = temp_root.join("escape.compiled.json");
    std::fs::create_dir_all(program_path.parent().expect("program parent"))
        .expect("temp dir created");
    std::fs::write(&program_path, text_parameter_module(r#""\é""#))
        .expect("RuleSpec module written");

    let output = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args([
            "compile",
            "--program",
            program_path.to_str().expect("utf8 path"),
            "--rulespec-root",
            rulespec_root.to_str().expect("utf8 root"),
            "--output",
            artifact_path.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("compile command runs");

    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&artifact_path).expect("compiled artefact written"),
    )
    .expect("artefact is JSON");
    let value = artifact["program"]["parameters"]
        .as_array()
        .expect("parameters array")
        .iter()
        .find(|parameter| {
            parameter["name"]
                .as_str()
                .is_some_and(|name| name.ends_with("escaped_label"))
        })
        .expect("escaped_label parameter")["versions"][0]["values"]["0"]["value"]
        .clone();
    assert_eq!(value, serde_json::json!("é"));

    let _ = std::fs::remove_dir_all(&temp_root);
}
