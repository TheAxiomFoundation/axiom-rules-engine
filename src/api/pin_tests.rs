//! Tests for `apply_pins` after it stopped searching the program once per
//! pin.
//!
//! For every generated program and pin list, `apply_pins` leaves the program
//! exactly as the frozen pre-change implementation (`reference`) does and
//! returns the same result. That covers what the change must keep: pins apply
//! in request order, the first failing pin reports (an unknown rule or a
//! judgment rule), a rule pinned twice is wrapped twice with the later pin
//! outermost, and with duplicate rule names the first is pinned. The named
//! tests pin those cases and the audit's 20,000-pin shape in time.

use std::time::{Duration, Instant};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use super::{ApiError, RulePin, apply_pins};
use crate::spec::{
    ComparisonOpSpec, DTypeSpec, DerivedSemanticsSpec, DerivedSpec, DerivedVersionSpec,
    JudgmentExprSpec, ProgramSpec, ScalarExprSpec, ScalarValueSpec,
};

/// `apply_pins` as `origin/main` had it at 5e6a8fd (unchanged since the
/// 2026-09-26 audit), one linear search per pin. Frozen: the body is the
/// code as it stood.
mod reference {
    use super::super::{ApiError, RulePin};
    use crate::spec::{
        ComparisonOpSpec, DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, ScalarExprSpec,
        ScalarValueSpec,
    };

    pub(super) fn apply_pins(program: &mut ProgramSpec, pins: &[RulePin]) -> Result<(), ApiError> {
        for pin in pins {
            let rule = program
                .derived
                .iter_mut()
                .find(|derived| derived.name == pin.rule)
                .ok_or_else(|| ApiError::UnknownPinnedRule {
                    rule: pin.rule.clone(),
                })?;
            let judgment = matches!(rule.semantics, DerivedSemanticsSpec::Judgment { .. })
                || rule.versions.iter().any(|version| {
                    matches!(version.semantics, DerivedSemanticsSpec::Judgment { .. })
                });
            if judgment {
                return Err(ApiError::JudgmentRuleNotPinnable {
                    rule: pin.rule.clone(),
                });
            }
            let pin_expr = |original: ScalarExprSpec| DerivedSemanticsSpec::Scalar {
                expr: ScalarExprSpec::If {
                    condition: Box::new(JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Integer { value: 0 },
                        }),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(ScalarExprSpec::Literal {
                            value: ScalarValueSpec::Integer { value: 0 },
                        }),
                    }),
                    then_expr: Box::new(ScalarExprSpec::Literal {
                        value: pin.value.clone(),
                    }),
                    else_expr: Box::new(original),
                },
            };
            if let DerivedSemanticsSpec::Scalar { expr } = rule.semantics.clone() {
                rule.semantics = pin_expr(expr);
            }
            for version in &mut rule.versions {
                if let DerivedSemanticsSpec::Scalar { expr } = version.semantics.clone() {
                    version.semantics = pin_expr(expr);
                }
            }
        }
        Ok(())
    }
}

const RULES: [&str; 4] = ["a", "b", "c", "d"];
const MISSING: &str = "missing";

fn integer(value: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Integer { value },
    }
}

fn semantics() -> impl Strategy<Value = DerivedSemanticsSpec> {
    prop_oneof![
        3 => (0..5_i64).prop_map(|value| DerivedSemanticsSpec::Scalar { expr: integer(value) }),
        1 => select(RULES.to_vec()).prop_map(|name| DerivedSemanticsSpec::Scalar {
            expr: ScalarExprSpec::Derived { name: name.to_string() },
        }),
        1 => select(RULES.to_vec()).prop_map(|name| DerivedSemanticsSpec::Judgment {
            expr: JudgmentExprSpec::Derived { name: name.to_string() },
        }),
    ]
}

fn rule(
    name: &str,
    semantics: DerivedSemanticsSpec,
    versions: Vec<DerivedSemanticsSpec>,
) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: "Household".to_string(),
        dtype: DTypeSpec::Integer,
        unit: None,
        period: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: versions
            .into_iter()
            .zip(2020..)
            .map(|(semantics, year)| DerivedVersionSpec {
                effective_from: chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("valid date"),
                effective_to: None,
                semantics,
            })
            .collect(),
    }
}

/// Programs of up to eight rules named from a pool of four, so names repeat
/// often (only an unvalidated artifact can hold that), with scalar and
/// judgment semantics on the rule and on its versions.
fn program() -> impl Strategy<Value = ProgramSpec> {
    vec(
        (select(RULES.to_vec()), semantics(), vec(semantics(), 0..=2))
            .prop_map(|(name, semantics, versions)| rule(name, semantics, versions)),
        0..=8,
    )
    .prop_map(|derived| ProgramSpec {
        derived,
        ..ProgramSpec::default()
    })
}

fn pins() -> impl Strategy<Value = Vec<RulePin>> {
    let value = prop_oneof![
        (0..3_i64).prop_map(|value| ScalarValueSpec::Integer { value }),
        Just(ScalarValueSpec::Decimal {
            value: "1.5".to_string()
        }),
        any::<bool>().prop_map(|value| ScalarValueSpec::Bool { value }),
    ];
    let names = RULES.iter().copied().chain([MISSING]).collect::<Vec<_>>();
    vec(
        (select(names), value).prop_map(|(rule, value)| RulePin {
            rule: rule.to_string(),
            value,
        }),
        0..6,
    )
}

/// The program `apply_pins` leaves, and its result with the error rendered,
/// since neither type has `Eq`.
fn applied(
    apply: fn(&mut ProgramSpec, &[RulePin]) -> Result<(), ApiError>,
    program: &ProgramSpec,
    pins: &[RulePin],
) -> (serde_json::Value, Result<(), String>) {
    let mut program = program.clone();
    let result = apply(&mut program, pins).map_err(|error| format!("{error:?}"));
    (
        serde_json::to_value(&program).expect("a program spec serializes"),
        result,
    )
}

#[test]
fn apply_pins_matches_the_reference() {
    let cases = std::env::var("AXIOM_PIN_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4_000);
    assert!(cases > 0, "AXIOM_PIN_CASES must be positive");
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]));
    let outcomes = std::cell::RefCell::new([0_u32; 3]);
    let result = runner.run(&(program(), pins()), |(program, pins)| {
        let actual = applied(apply_pins, &program, &pins);
        let expected = applied(reference::apply_pins, &program, &pins);
        prop_assert_eq!(&actual, &expected, "pins: {:?}", pins);
        let outcome = match &actual.1 {
            Ok(()) => 0,
            Err(error) if error.starts_with("UnknownPinnedRule") => 1,
            Err(_) => 2,
        };
        outcomes.borrow_mut()[outcome] += 1;
        Ok(())
    });
    if let Err(error) = result {
        panic!("{error}");
    }
    // Without this, a generator drifting to one outcome would leave the
    // property passing on nothing.
    let [ok, unknown, judgment] = outcomes.into_inner();
    let share = |count: u32| f64::from(count) / f64::from(cases);
    assert!(
        share(ok) >= 0.2 && share(unknown) >= 0.1 && share(judgment) >= 0.1,
        "ok {ok}, unknown {unknown}, judgment {judgment} of {cases}"
    );
}

/// `if 0 == 0 then <value> else <original>`, the shape a pin wraps a rule in.
fn pinned(value: i64, original: ScalarExprSpec) -> ScalarExprSpec {
    ScalarExprSpec::If {
        condition: Box::new(JudgmentExprSpec::Comparison {
            left: Box::new(integer(0)),
            op: ComparisonOpSpec::Eq,
            right: Box::new(integer(0)),
        }),
        then_expr: Box::new(integer(value)),
        else_expr: Box::new(original),
    }
}

fn pin(rule: &str, value: i64) -> RulePin {
    RulePin {
        rule: rule.to_string(),
        value: ScalarValueSpec::Integer { value },
    }
}

fn scalar(name: &str, value: i64) -> DerivedSpec {
    rule(
        name,
        DerivedSemanticsSpec::Scalar {
            expr: integer(value),
        },
        Vec::new(),
    )
}

fn expr(rule: &DerivedSpec) -> serde_json::Value {
    serde_json::to_value(&rule.semantics).expect("semantics serialize")
}

fn scalar_semantics(expr: ScalarExprSpec) -> serde_json::Value {
    serde_json::to_value(DerivedSemanticsSpec::Scalar { expr }).expect("semantics serialize")
}

#[test]
fn a_rule_pinned_twice_is_wrapped_twice_with_the_later_pin_outermost() {
    let mut program = ProgramSpec {
        derived: vec![scalar("a", 1), scalar("b", 2)],
        ..ProgramSpec::default()
    };
    apply_pins(&mut program, &[pin("a", 10), pin("b", 20), pin("a", 30)]).expect("both rules pin");
    assert_eq!(
        expr(&program.derived[0]),
        scalar_semantics(pinned(30, pinned(10, integer(1))))
    );
    assert_eq!(
        expr(&program.derived[1]),
        scalar_semantics(pinned(20, integer(2)))
    );
}

/// Only an unvalidated artifact can repeat a name; the first rule with it is
/// the one pinned, as a linear search found it.
#[test]
fn with_a_repeated_rule_name_the_first_rule_is_pinned() {
    let mut program = ProgramSpec {
        derived: vec![scalar("other", 0), scalar("a", 1), scalar("a", 2)],
        ..ProgramSpec::default()
    };
    apply_pins(&mut program, &[pin("a", 10)]).expect("the rule pins");
    assert_eq!(expr(&program.derived[0]), scalar_semantics(integer(0)));
    assert_eq!(
        expr(&program.derived[1]),
        scalar_semantics(pinned(10, integer(1)))
    );
    assert_eq!(expr(&program.derived[2]), scalar_semantics(integer(2)));
}

#[test]
fn the_first_failing_pin_in_request_order_is_reported() {
    let program = ProgramSpec {
        derived: vec![
            scalar("a", 1),
            rule(
                "eligible",
                DerivedSemanticsSpec::Judgment {
                    expr: JudgmentExprSpec::Derived {
                        name: "a".to_string(),
                    },
                },
                Vec::new(),
            ),
        ],
        ..ProgramSpec::default()
    };
    let first_error = |pins: &[RulePin]| {
        let mut program = program.clone();
        apply_pins(&mut program, pins).map_err(|error| format!("{error:?}"))
    };
    assert_eq!(
        first_error(&[pin("a", 1), pin("eligible", 1), pin(MISSING, 1)]),
        Err(r#"JudgmentRuleNotPinnable { rule: "eligible" }"#.to_string())
    );
    assert_eq!(
        first_error(&[pin(MISSING, 1), pin("eligible", 1)]),
        Err(r#"UnknownPinnedRule { rule: "missing" }"#.to_string())
    );
    assert_eq!(first_error(&[]), Ok(()));
}

/// The audit's shape (2026-09-26, `apply-pins-linear-find`), scaled up: pins
/// on every rule, then one unknown pin, which errors only after every other
/// pin has been looked up. A linear search per pin took 2.2 s at 20,000 pins
/// in a release build and grows as pins * rules. At 20,000 a debug build of
/// that search still fits in the bound (about 3 s for both runs below), so
/// this runs 80,000, where it took 104 s for the first run alone; one pass
/// takes under 0.1 s. The bound leaves room for a loaded CI runner.
#[test]
fn eighty_thousand_pins_apply_in_linear_time() {
    const RULES: usize = 80_000;
    const BOUND: Duration = Duration::from_secs(5);
    let names = (0..RULES)
        .map(|index| format!("rule_{index:06}"))
        .collect::<Vec<_>>();
    let program = ProgramSpec {
        derived: names.iter().map(|name| scalar(name, 0)).collect(),
        ..ProgramSpec::default()
    };
    // Pinned in reverse, so a linear search walks most of the program each
    // time.
    let mut pins = names
        .iter()
        .rev()
        .map(|name| pin(name, 1))
        .collect::<Vec<_>>();

    let mut all_pinned = program.clone();
    let started = Instant::now();
    apply_pins(&mut all_pinned, &pins).expect("every rule pins");
    let elapsed = started.elapsed();
    eprintln!("{RULES} pins: {elapsed:?}");
    assert!(elapsed < BOUND, "{RULES} pins took {elapsed:?}");
    assert!(
        all_pinned
            .derived
            .iter()
            .all(|rule| expr(rule) == scalar_semantics(pinned(1, integer(0))))
    );

    pins.push(pin(MISSING, 1));
    let mut refused = program;
    let started = Instant::now();
    let error = apply_pins(&mut refused, &pins).expect_err("the last pin names no rule");
    let elapsed = started.elapsed();
    eprintln!("{RULES} pins and a miss: {elapsed:?}");
    assert!(elapsed < BOUND, "{RULES} pins and a miss took {elapsed:?}");
    assert!(matches!(error, ApiError::UnknownPinnedRule { rule } if rule == MISSING));
}
