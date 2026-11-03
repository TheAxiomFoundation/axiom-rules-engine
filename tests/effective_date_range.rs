//! An effective date outside 0000-01-01..=9999-12-31 is a date error that
//! names the rule and the date, never a formula parse error (#288). The
//! formula lexer reads a date as four digits, a dash, two, a dash and two, so
//! a signed year such as `+10000` or `-0001` cannot be written into the
//! formula source the loader generates for a derived rule or a parameter.
//! The loader writes no date for a relation, a table's `effective_to` or a
//! table's later versions, but it checks those too, so one rule holds for
//! every date a rule declares.
//!
//! The tests walk every rule kind below and every place a date can sit in
//! it: a version's two fields, a later version's two fields, the rule-level
//! pair that stands in for `versions:`, and a rule-level `effective_from`
//! beside a `versions:` list.

use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use chrono::{Datelike, NaiveDate};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A derived rule: `write_formula_definition` writes both dates.
    Derived,
    /// A scalar parameter: also `write_formula_definition`.
    Parameter,
    /// A parameter table: `write_formula_stub_definition` writes its first
    /// version's effective_from and nothing else.
    Table,
    /// A derived relation: one version, whose dates no writer emits.
    DerivedRelation,
    /// A data relation: rule-level dates only, which no writer emits.
    DataRelation,
}

/// Where the date sits in the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    /// `versions[0].effective_from`, the only version.
    FirstFrom,
    /// `versions[0].effective_to`, the only version.
    FirstTo,
    /// `versions[1].effective_from`, after a valid first version.
    LaterFrom,
    /// `versions[1].effective_to`, after a valid first version.
    LaterTo,
    /// The rule-level `effective_from`, with no `versions:` list.
    RuleFrom,
    /// The rule-level `effective_to`, with no `versions:` list.
    RuleTo,
    /// A rule-level `effective_from` beside a valid `versions:` list.
    RuleBesideVersions,
}

impl Position {
    fn field(self) -> &'static str {
        match self {
            Position::FirstTo | Position::LaterTo | Position::RuleTo => "effective_to",
            _ => "effective_from",
        }
    }

    /// An in-range date for this position, for the baseline.
    fn valid_date(self) -> NaiveDate {
        if self.field() == "effective_to" {
            NaiveDate::from_ymd_opt(2027, 12, 31).expect("a date")
        } else {
            NaiveDate::from_ymd_opt(2026, 6, 1).expect("a date")
        }
    }
}

const ALL_POSITIONS: [Position; 7] = [
    Position::FirstFrom,
    Position::FirstTo,
    Position::LaterFrom,
    Position::LaterTo,
    Position::RuleFrom,
    Position::RuleTo,
    Position::RuleBesideVersions,
];

/// The places a date can sit in a rule of `kind` that otherwise lowers. A
/// table needs `values:` inside a version, a derived relation takes exactly
/// one version, and a data relation has no versions.
fn positions(kind: Kind) -> &'static [Position] {
    match kind {
        Kind::Derived | Kind::Parameter => &ALL_POSITIONS,
        Kind::Table => &[
            Position::FirstFrom,
            Position::FirstTo,
            Position::LaterFrom,
            Position::LaterTo,
            Position::RuleBesideVersions,
        ],
        Kind::DerivedRelation => &[
            Position::FirstFrom,
            Position::FirstTo,
            Position::RuleFrom,
            Position::RuleTo,
            Position::RuleBesideVersions,
        ],
        Kind::DataRelation => &[Position::RuleFrom, Position::RuleTo],
    }
}

const ALL_KINDS: [Kind; 5] = [
    Kind::Derived,
    Kind::Parameter,
    Kind::Table,
    Kind::DerivedRelation,
    Kind::DataRelation,
];

/// Spell `date` as chrono's `Display` does, which `serde` reads back: a year
/// outside 0..=9999 carries its sign.
fn spelled(date: NaiveDate) -> String {
    date.to_string()
}

/// A module with one valid parameter and a data relation `relative_of`, then
/// the rule `subject` of `kind` with `date` at `position`.
fn module(kind: Kind, position: Position, date: NaiveDate) -> String {
    let date = spelled(date);
    let (header, body) = match kind {
        Kind::Derived => (
            "    kind: derived\n    entity: Person\n    dtype: Decimal\n    period: Year\n",
            "        formula: '1'\n",
        ),
        Kind::Parameter => (
            "    kind: parameter\n    dtype: Decimal\n",
            "        formula: '1'\n",
        ),
        Kind::Table => (
            "    kind: parameter\n    dtype: Integer\n    indexed_by: age\n",
            "        values:\n          0: 1\n",
        ),
        Kind::DerivedRelation => (
            "    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: relative_of\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n",
            "        formula: \"'a' == 'a'\"\n",
        ),
        Kind::DataRelation => (
            "    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Person]\n",
            "",
        ),
    };
    let version = |from: &str, to: Option<&str>| {
        let to = to
            .map(|to| format!("        effective_to: '{to}'\n"))
            .unwrap_or_default();
        format!("      - effective_from: '{from}'\n{to}{body}")
    };
    // A rule-level formula sits at four spaces, not under a version.
    let rule_body = body.replacen("        ", "    ", 1);
    let dates = match position {
        Position::FirstFrom => format!("    versions:\n{}", version(&date, None)),
        Position::FirstTo => format!("    versions:\n{}", version("2026-01-01", Some(&date))),
        Position::LaterFrom => format!(
            "    versions:\n{}{}",
            version("2025-01-01", Some("2025-12-31")),
            version(&date, None)
        ),
        Position::LaterTo => format!(
            "    versions:\n{}{}",
            version("2025-01-01", Some("2025-12-31")),
            version("2026-01-01", Some(&date))
        ),
        Position::RuleFrom => format!("    effective_from: '{date}'\n{rule_body}"),
        Position::RuleTo => {
            format!("    effective_from: '2026-01-01'\n    effective_to: '{date}'\n{rule_body}")
        }
        Position::RuleBesideVersions => format!(
            "    effective_from: '{date}'\n    versions:\n{}",
            version("2026-01-01", None)
        ),
    };
    format!(
        "format: rulespec/v1\nrules:\n  - name: earlier\n    kind: parameter\n    dtype: Decimal\n    versions:\n      - effective_from: '2026-01-01'\n        formula: '2'\n  - name: relative_of\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Person]\n  - name: subject\n{header}{dates}"
    )
}

/// Lowering `yaml` must fail with the out-of-range date error for `subject`,
/// `field` and `date`. Returns its message.
fn date_error(yaml: &str, field: &str, date: NaiveDate) -> String {
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
            assert_eq!(reported, field, "{error}");
            assert_eq!(reported_date, date, "{error}");
            error.to_string()
        }
        other => panic!("expected a date error, got {other:?}\n---\n{yaml}"),
    }
}

#[test]
fn every_position_lowers_with_an_in_range_date() {
    // The baseline: each module differs from a refused one only in the date,
    // so a refusal below is the date's.
    for kind in ALL_KINDS {
        for &position in positions(kind) {
            let yaml = module(kind, position, position.valid_date());
            if let Err(error) = lower_rulespec_str(&yaml) {
                panic!("{kind:?} {position:?} should lower: {error}\n---\n{yaml}");
            }
        }
    }
}

#[test]
fn every_position_refuses_the_issue_examples() {
    for kind in ALL_KINDS {
        for &position in positions(kind) {
            for date in [(10000, 1, 1), (-1, 12, 31)] {
                let date = NaiveDate::from_ymd_opt(date.0, date.1, date.2).expect("a date");
                let message = date_error(&module(kind, position, date), position.field(), date);
                assert!(
                    !message.contains("parse error"),
                    "{kind:?} {position:?}: {message}"
                );
            }
        }
    }
}

#[test]
fn the_documented_message_is_the_one_the_engine_prints() {
    let date = NaiveDate::from_ymd_opt(10000, 1, 1).expect("a date");
    let message = date_error(
        &module(Kind::Derived, Position::FirstFrom, date),
        "effective_from",
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

#[test]
fn an_unsupported_kind_is_reported_before_its_date() {
    // The kind decides what the rule is; a date on a rule the loader cannot
    // read is not the first thing wrong with it.
    let yaml = "format: rulespec/v1\nrules:\n  - name: subject\n    kind: mystery\n    versions:\n      - effective_from: '+10000-01-01'\n        formula: '1'\n";
    match lower_rulespec_str(yaml) {
        Err(RuleSpecError::UnsupportedRuleKind { name, kind }) => {
            assert_eq!((name.as_str(), kind.as_str()), ("subject", "mystery"));
        }
        other => panic!("expected the unsupported kind, got {other:?}"),
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

/// A rule kind and a place a date can sit in it.
fn kind_and_position() -> impl Strategy<Value = (Kind, Position)> {
    prop::sample::select(
        ALL_KINDS
            .iter()
            .flat_map(|&kind| {
                positions(kind)
                    .iter()
                    .map(move |&position| (kind, position))
            })
            .collect::<Vec<_>>(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any date outside the supported range, in any rule kind and at any
    /// place a date can sit, lowering returns the date error naming the rule,
    /// never a parse error.
    #[test]
    fn an_out_of_range_date_is_a_date_error_naming_the_rule(
        date in out_of_range_date(),
        (kind, position) in kind_and_position(),
    ) {
        prop_assume!(date.year() < 0 || date.year() > 9999);
        let message = date_error(&module(kind, position, date), position.field(), date);
        prop_assert!(message.contains("rule `subject`"), "{}", message);
        prop_assert!(message.contains(&spelled(date)), "{}", message);
        prop_assert!(!message.contains("parse error"), "{}", message);
    }
}
