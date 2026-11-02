//! An effective date outside 0000-01-01..=9999-12-31 is a date error that
//! names the rule and the date, never a formula parse error. The formula
//! lexer reads a date as four digits, a dash, two, a dash and two, so a
//! signed year such as `+10000` or `-0001` cannot be written into the
//! formula source the loader generates (#288).

use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use chrono::{Datelike, NaiveDate};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A derived rule: `write_formula_definition` writes both dates.
    Derived,
    /// A scalar parameter: also `write_formula_definition`.
    Parameter,
    /// A parameter table: `write_formula_stub_definition` writes its
    /// effective_from.
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    EffectiveFrom,
    EffectiveTo,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::EffectiveFrom => "effective_from",
            Field::EffectiveTo => "effective_to",
        }
    }
}

/// Spell `date` as chrono's `Display` does, which `serde` reads back: a year
/// outside 0..=9999 carries its sign.
fn spelled(date: NaiveDate) -> String {
    date.to_string()
}

/// A module with one valid rule, then the rule `subject` of `kind`, whose one
/// version has `field` set to `date`. When `field` is `effective_to`, the
/// version starts on 2026-01-01.
fn module(kind: Kind, field: Field, date: NaiveDate) -> String {
    let date = spelled(date);
    let dates = match field {
        Field::EffectiveFrom => format!("      - effective_from: '{date}'\n"),
        Field::EffectiveTo => {
            format!("      - effective_from: '2026-01-01'\n        effective_to: '{date}'\n")
        }
    };
    let subject = match kind {
        Kind::Derived => format!(
            "  - name: subject\n    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Year\n    versions:\n{dates}        formula: '1'\n"
        ),
        Kind::Parameter => format!(
            "  - name: subject\n    kind: parameter\n    dtype: Decimal\n    versions:\n{dates}        formula: '1'\n"
        ),
        Kind::Table => format!(
            "  - name: subject\n    kind: parameter\n    dtype: Integer\n    indexed_by: age\n    versions:\n{dates}        values:\n          0: 1\n"
        ),
    };
    format!(
        "format: rulespec/v1\nrules:\n  - name: earlier\n    kind: parameter\n    dtype: Decimal\n    versions:\n      - effective_from: '2026-01-01'\n        formula: '2'\n{subject}"
    )
}

/// Lowering `yaml` must fail with the out-of-range date error for `subject`,
/// `field` and `date`. Returns its message.
fn date_error(yaml: &str, field: Field, date: NaiveDate) -> String {
    match lower_rulespec_str(yaml) {
        Err(
            ref error @ RuleSpecError::EffectiveDateOutOfRange {
                ref name,
                field: reported,
                date: reported_date,
                ..
            },
        ) => {
            assert_eq!(name, "subject", "{error}");
            assert_eq!(reported, field.name(), "{error}");
            assert_eq!(reported_date, date, "{error}");
            error.to_string()
        }
        other => panic!("expected a date error, got {other:?}\n---\n{yaml}"),
    }
}

#[test]
fn the_issue_examples_get_a_date_error() {
    for (date, kind, field) in [
        ((10000, 1, 1), Kind::Derived, Field::EffectiveFrom),
        ((-1, 1, 1), Kind::Derived, Field::EffectiveFrom),
        ((10000, 1, 1), Kind::Derived, Field::EffectiveTo),
        ((10000, 1, 1), Kind::Parameter, Field::EffectiveFrom),
        ((-1, 12, 31), Kind::Table, Field::EffectiveFrom),
    ] {
        let date = NaiveDate::from_ymd_opt(date.0, date.1, date.2).expect("a date");
        let message = date_error(&module(kind, field, date), field, date);
        assert!(!message.contains("parse error"), "{message}");
    }
}

#[test]
fn the_documented_message_is_the_one_the_engine_prints() {
    let date = NaiveDate::from_ymd_opt(10000, 1, 1).expect("a date");
    let message = date_error(
        &module(Kind::Derived, Field::EffectiveFrom, date),
        Field::EffectiveFrom,
        date,
    );
    assert_eq!(
        message,
        "RuleSpec file `<memory>` rule `subject` has effective_from +10000-01-01, outside the dates a RuleSpec version can take (0000-01-01 to 9999-12-31)"
    );
    let reference = include_str!("../docs/rulespec-format.md");
    assert!(
        reference.contains(&format!("```text\n{message}\n```")),
        "docs/rulespec-format.md should quote the date error"
    );
}

#[test]
fn the_ends_of_the_supported_range_still_lower() {
    for (from, to) in [
        ("0000-01-01", None),
        ("9999-12-31", None),
        ("0000-01-01", Some("9999-12-31")),
    ] {
        let to = to
            .map(|to| format!("        effective_to: '{to}'\n"))
            .unwrap_or_default();
        let yaml = format!(
            "format: rulespec/v1\nrules:\n  - name: subject\n    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Year\n    versions:\n      - effective_from: '{from}'\n{to}        formula: '1'\n"
        );
        if let Err(error) = lower_rulespec_str(&yaml) {
            panic!("{from} {to:?} should lower: {error}");
        }
    }
}

/// Any date chrono represents whose year lies outside 0..=9999.
fn out_of_range_date() -> impl Strategy<Value = NaiveDate> {
    let min = NaiveDate::MIN.num_days_from_ce();
    let max = NaiveDate::MAX.num_days_from_ce();
    let first_supported = NaiveDate::from_ymd_opt(0, 1, 1)
        .expect("a date")
        .num_days_from_ce();
    let last_supported = NaiveDate::from_ymd_opt(9999, 12, 31)
        .expect("a date")
        .num_days_from_ce();
    prop_oneof![min..first_supported, (last_supported + 1)..=max]
        .prop_map(|days| NaiveDate::from_num_days_from_ce_opt(days).expect("a date in range"))
}

fn kind() -> impl Strategy<Value = Kind> {
    prop::sample::select(vec![Kind::Derived, Kind::Parameter, Kind::Table])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any date outside the supported range, in either field and in a
    /// rule of any kind whose dates reach the generated formula source,
    /// lowering returns the date error naming the rule, never a parse error.
    #[test]
    fn an_out_of_range_date_is_a_date_error_naming_the_rule(
        date in out_of_range_date(),
        kind in kind(),
        to in any::<bool>(),
    ) {
        // A table writes only its effective_from into the source.
        let field = if to && !matches!(kind, Kind::Table) {
            Field::EffectiveTo
        } else {
            Field::EffectiveFrom
        };
        prop_assume!(date.year() < 0 || date.year() > 9999);
        let message = date_error(&module(kind, field, date), field, date);
        prop_assert!(message.contains("rule `subject`"), "{}", message);
        prop_assert!(message.contains(&spelled(date)), "{}", message);
        prop_assert!(!message.contains("parse error"), "{}", message);
    }
}
