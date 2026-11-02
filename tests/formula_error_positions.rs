//! A RuleSpec formula parse error's line and column point into the failing
//! formula's own text, trimmed of surrounding whitespace, not into the YAML
//! file that holds it, and a column counts Unicode scalar values (Rust
//! `char`s) from 1. docs/rulespec-format.md ("Formula parse errors") states
//! this rule and quotes the first test's error, so these tests hold the doc
//! to the code.

use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use proptest::prelude::*;

const REFERENCE: &str = include_str!("../docs/rulespec-format.md");

/// A module with `before` valid rules, then a derived rule whose one version
/// has `formula`. A JSON string is a YAML double-quoted scalar, so the
/// formula reaches the loader exactly, leading and trailing whitespace too.
fn module(before: usize, formula: &str) -> String {
    let mut yaml = String::from("format: rulespec/v1\nrules:\n");
    for index in 0..before {
        yaml.push_str(&format!(
            "  - name: earlier_{index}\n    kind: parameter\n    dtype: Decimal\n    versions:\n      - effective_from: '2026-01-01'\n        formula: |-\n          # Zürich: 5 €\n          {index}\n"
        ));
    }
    yaml.push_str(&format!(
        "  - name: subject\n    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Year\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {}\n",
        serde_json::to_string(formula).expect("a string serializes"),
    ));
    yaml
}

/// A module whose one derived rule's formula is a YAML block scalar with
/// `indicator` (`|-`, `>-`) and one indented line per entry of `lines`.
fn block_module(indicator: &str, lines: &[&str]) -> String {
    let mut yaml = format!(
        "format: rulespec/v1\nrules:\n  - name: subject\n    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Year\n    versions:\n      - effective_from: '2026-01-01'\n        formula: {indicator}\n"
    );
    for line in lines {
        yaml.push_str(&format!("          {line}\n"));
    }
    yaml
}

/// The error lowering `yaml` reports. It must be a formula error.
fn formula_error(yaml: &str) -> String {
    match lower_rulespec_str(yaml) {
        Err(error @ RuleSpecError::Formula(_)) => error.to_string(),
        other => panic!("expected a formula error, got {other:?}\n---\n{yaml}"),
    }
}

/// The (line, col) a formula parse error reports.
fn position(yaml: &str) -> (usize, usize) {
    let message = formula_error(yaml);
    let at = message
        .split_once(" at line ")
        .and_then(|(_, rest)| rest.split_once(": "))
        .and_then(|(position, _)| position.split_once(", col "))
        .unwrap_or_else(|| panic!("no position in {message:?}"));
    (
        at.0.parse().expect("a line number"),
        at.1.parse().expect("a column number"),
    )
}

#[test]
fn the_documented_example_reports_the_documented_error() {
    let formula = "if canton == 'Zürich': base × 1.2\nelse: base";
    let message = formula_error(&module(0, formula));
    assert_eq!(
        message,
        "failed to parse RuleSpec formula: formula parse error at line 1, col 29: unexpected char '×'"
    );
    assert!(
        REFERENCE.contains(&format!("```text\n{formula}\n```")),
        "docs/rulespec-format.md should show the example formula"
    );
    assert!(
        REFERENCE.contains(&format!("```text\n{message}\n```")),
        "docs/rulespec-format.md should quote the error the example reports"
    );
}

#[test]
fn a_position_names_the_offending_character_or_token() {
    for (formula, expected) in [
        // `ü` is two bytes, one column.
        ("if canton == 'Zürich': base × 1.2\nelse: base", (1, 29)),
        // Line 2 of the formula, whatever lines of YAML come before it.
        ("if x > 0:\n  'é' $\nelse: 0", (2, 7)),
        // An unterminated string: its opening quote.
        ("'é' + 'abc", (1, 7)),
        // A token the parser rejects: its first character.
        ("'é' + )", (1, 7)),
        // `e` then U+0301 is one grapheme but two columns, and three bytes.
        ("'e\u{301}' $", (1, 6)),
        // A tab and a one-code-point emoji are one column each.
        ("'😀'\t$", (1, 5)),
        ("'×' $", (1, 5)),
        // 👍🏽 is two code points (a thumb and a skin-tone modifier).
        ("'👍🏽' $", (1, 6)),
        // Only a line feed starts a line: a lone carriage return is a column.
        ("'é'\r$", (1, 5)),
        ("'é'\r\n$", (2, 1)),
    ] {
        for before in [0, 2] {
            assert_eq!(
                position(&module(before, formula)),
                expected,
                "{formula:?} after {before} rules"
            );
        }
    }
}

#[test]
fn a_formula_that_ends_too_early_points_one_past_its_last_character() {
    for (formula, expected) in [
        ("1 +", (1, 4)),
        ("1 +\n\n", (1, 4)),
        ("if x > 0:\n  1 +", (2, 6)),
    ] {
        assert_eq!(position(&module(0, formula)), expected, "{formula:?}");
    }
}

#[test]
fn whitespace_around_a_formula_does_not_count() {
    // The engine trims the formula before it parses it.
    for formula in ["'é' $", "\n\n'é' $", "   'é' $", " \n\t 'é' $ \n"] {
        assert_eq!(position(&module(0, formula)), (1, 5), "{formula:?}");
    }
}

#[test]
fn a_line_break_in_a_string_literal_counts_before_trimming() {
    // The second literal's opening quote: line 2, column 9 in the untrimmed
    // text, though trimming would put it at line 1, column 7.
    let yaml = module(0, "  \n  'é' + 'a\nb'");
    match lower_rulespec_str(&yaml) {
        Err(RuleSpecError::MultilineStringLiteral { line, col, .. }) => {
            assert_eq!((line, col), (2, 9));
        }
        other => panic!("expected a line-break-in-literal error, got {other:?}"),
    }
}

#[test]
fn a_yaml_block_scalar_counts_in_the_text_yaml_reads() {
    // A literal block keeps its line breaks and drops its indentation.
    assert_eq!(position(&block_module("|-", &["1 +", "'é' $"])), (2, 5));
    // YAML reads CRLF and CR line breaks in the file as line feeds.
    for line_break in ["\r\n", "\r"] {
        let yaml = block_module("|-", &["1 +", "'é' $"]).replace('\n', line_break);
        assert_eq!(position(&yaml), (2, 5), "{line_break:?} line breaks");
    }
    // A folded block joins its lines with spaces: `1 + 2 $` on one line.
    assert_eq!(position(&block_module(">-", &["1 +", "2 $"])), (1, 7));
}

#[test]
fn a_derived_relation_predicate_counts_the_same_way() {
    let relation = |predicate_lines: &str| {
        format!(
            "format: rulespec/v1\nrules:\n  - name: relative_of\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Person]\n  - name: pair\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: relative_of\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n    versions:\n      - effective_from: '2026-01-01'\n        formula: |-\n\n          'é' == 'a' and\n          {predicate_lines}\n"
        )
    };
    // The relation is well formed: only the predicate's last line differs.
    if let Err(error) = lower_rulespec_str(&relation("'ü' == 'b'")) {
        panic!("the well-formed relation should lower: {error}");
    }
    assert_eq!(position(&relation("'ü' $")), (2, 5));
}

#[test]
fn a_lowering_error_carries_no_position() {
    let message = formula_error(&module(0, "ceil(1, 2)"));
    assert!(
        message.starts_with("failed to parse RuleSpec formula: formula lower error: "),
        "{message}"
    );
    assert!(!message.contains(" at line "), "{message}");
}

/// Lines that lex cleanly, several holding multibyte characters.
fn clean_line() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "# Zürich: 5 € 😀",
        "'é' + 'e\u{301}'",
        "base\t* 2",
        "\"\"\"𝄞 docstring\"\"\"",
        "min(a, b) - 'ß'",
        "",
    ])
}

/// Characters outside the formula grammar, in every UTF-8 width.
fn offending_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['$', '@', '?', '`', ';', '×', '€', '中', '😀'])
}

/// Whitespace the engine trims from either end of a formula.
fn padding() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["", "\n", "\n\n", "  ", " \n\t", "\t\n  \n"])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Earlier formula lines, a clean prefix and then a character the lexer
    /// rejects: the reported position names that character in the trimmed
    /// formula, however many rules come first and whatever whitespace pads
    /// the formula.
    #[test]
    fn the_position_names_the_offending_character_in_the_trimmed_formula(
        earlier in prop::collection::vec(clean_line(), 0..3),
        prefix in clean_line().prop_filter("no comment", |line| !line.starts_with('#')),
        offending in offending_char(),
        before in 0usize..3,
        leading in padding(),
        trailing in padding(),
    ) {
        let mut body = String::new();
        for line in &earlier {
            body.push_str(line);
            body.push('\n');
        }
        body.push_str(prefix);
        body.push(' ');
        body.push(offending);
        let formula = format!("{leading}{body}{trailing}");

        let trimmed = formula.trim();
        let at = trimmed.rfind(offending).expect("the offending character");
        let line_start = trimmed[..at].rfind('\n').map_or(0, |index| index + 1);
        let expected = (
            trimmed[..at].matches('\n').count() + 1,
            trimmed[line_start..at].chars().count() + 1,
        );
        prop_assert_eq!(position(&module(before, &formula)), expected, "{:?}", formula);
    }
}
