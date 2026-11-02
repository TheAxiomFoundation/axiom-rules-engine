//! Lexer error positions count characters, not UTF-8 bytes (#239): a
//! reported column names the offending character on its line, however many
//! multibyte characters come before it.

use proptest::prelude::*;

use super::{FormulaError, Lexer};

/// The (line, col) of a lexer error.
fn error_position(src: &str) -> (usize, usize) {
    match Lexer::new(src).tokenise() {
        Err(FormulaError::Parse { line, col, .. }) => (line, col),
        other => panic!("{src:?} should fail to lex, got {other:?}"),
    }
}

/// The character at a 1-based (line, col) of `src`.
fn char_at(src: &str, line: usize, col: usize) -> Option<char> {
    src.split('\n').nth(line - 1)?.chars().nth(col - 1)
}

#[test]
fn a_multibyte_character_before_an_error_counts_once() {
    // `é` is two bytes and `😀` four; each is one column.
    for (src, expected) in [
        ("'é' $", (1, 5)),
        ("'😀😀' @", (1, 6)),
        ("x # é\n'中' + €", (2, 7)),
        ("\"\"\"𝄞\"\"\" ?", (1, 9)),
    ] {
        let (line, col) = error_position(src);
        assert_eq!((line, col), expected, "{src:?}");
        assert!(
            Lexer::new(src)
                .tokenise()
                .expect_err("lex error")
                .to_string()
                .contains(&format!("line {line}, col {col}")),
            "{src:?}"
        );
    }
}

/// Characters a string literal or comment may hold without ending it,
/// weighted toward multibyte ones.
fn body_char() -> impl Strategy<Value = char> {
    prop_oneof![
        3 => prop::sample::select(vec!['é', 'ß', '€', '中', '𝄞', '😀', '\u{301}', '\u{feff}']),
        2 => prop::char::range(' ', '~'),
        1 => any::<char>(),
    ]
    .prop_filter("stays inside the literal", |c| {
        !matches!(c, '\'' | '"' | '\\' | '\n' | '\r')
    })
}

/// One token that lexes cleanly on a single line.
fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(body_char(), 0..6)
            .prop_map(|chars| format!("'{}'", chars.into_iter().collect::<String>())),
        prop::collection::vec(body_char(), 0..6)
            .prop_map(|chars| format!("\"\"\"{}\"\"\"", chars.into_iter().collect::<String>())),
        "[a-z_][a-z0-9_]{0,6}",
        "[0-9]{1,4}",
        prop::sample::select(vec!["+", "-", "*", "<", ">", "(", ")", "[", "]", ",", "=>"])
            .prop_map(str::to_string),
    ]
}

/// A line of clean tokens, separated by spaces or tabs.
fn clean_line() -> impl Strategy<Value = String> {
    prop::collection::vec((segment(), prop::sample::select(vec![" ", "\t", "  "])), 0..6)
        .prop_map(|parts| {
            parts
                .into_iter()
                .map(|(segment, gap)| format!("{segment}{gap}"))
                .collect()
        })
}

/// Characters outside the formula grammar, in every UTF-8 width.
fn offending_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['$', '@', '?', '`', ';', 'é', '€', '中', '𝄞', '😀'])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Earlier lines (each may end in a comment) and a clean prefix, then a
    /// character the lexer rejects: the error's line and column name that
    /// character.
    #[test]
    fn reported_column_names_the_offending_character(
        earlier in prop::collection::vec(
            (clean_line(), prop::option::of(prop::collection::vec(body_char(), 0..6))),
            0..3,
        ),
        prefix in clean_line(),
        offending in offending_char(),
    ) {
        let mut src = String::new();
        for (line, comment) in &earlier {
            src.push_str(line);
            if let Some(comment) = comment {
                src.push('#');
                src.extend(comment);
            }
            src.push('\n');
        }
        src.push_str(&prefix);
        src.push(offending);

        let error = Lexer::new(&src).tokenise().expect_err("the last character is rejected");
        prop_assert!(
            error.to_string().contains(&format!("unexpected char {offending:?}")),
            "{:?}: {}", src, error
        );
        let (line, col) = error_position(&src);
        prop_assert_eq!(line, earlier.len() + 1, "{:?}", src);
        prop_assert_eq!(col, prefix.chars().count() + 1, "{:?}", src);
        prop_assert_eq!(char_at(&src, line, col), Some(offending), "{:?}", src);
    }
}
