//! Opt-in three-valued values in explain: the first PR of #215.
//!
//! `docs/three-valued-values.md` is the contract under test. Under
//! `MissingFacts::Indeterminate`, for now an engine option
//! (`Engine::with_options`), an input no record covers makes the result that
//! reads it unknown and names the cell; scalar operators keep the reference
//! order of evaluation, conversion and checked arithmetic; `and`/`or` are
//! strong Kleene and catch an item's failure; an `if` on an unknown condition
//! and every relation read are refused by name; and a program defect no
//! dataset can change is refused before anything is evaluated. Without the
//! option nothing changes.
//!
//! The file has two halves.
//!
//! * **Named cases**, one per behaviour the PR adds, each asserting the
//!   outcome and the exact needs.
//! * **Random properties.** A seeded proptest runner generates small programs
//!   of `Case` rules over boolean and integer input slots, with comparisons,
//!   `and`/`or`/`not`, references to earlier rules, `input_or_else` and
//!   checked arithmetic including division, and datasets that supply a cell,
//!   omit it, or supply it by overlapping spells, queried over two periods.
//!   An oracle written here, sharing no code with the engine, evaluates the
//!   generator's own expressions under section 5 of the note (the reference)
//!   and under today's rules. The engine must agree with both, and the
//!   invariants of section 6 are checked on the engine and on the reference
//!   by enumerating every total completion of a partial dataset. Failures
//!   are shrunk and printed with the program, the dataset and the results.
//!
//! Environment knobs (both optional): `AXIOM_THREE_VALUED_CASES=<n>` cases
//! per property (default [`DEFAULT_CASES`]); `AXIOM_THREE_VALUED_SEED=<u64>`
//! base seed (default fixed, so CI is deterministic).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::str::FromStr;

use axiom_rules_engine::depth::with_suspend_depth;
use axiom_rules_engine::engine::{Engine, EvalError, EvaluationOptions};
use axiom_rules_engine::model::{
    ComparisonOp, DType, DataSet, Derived, DerivedSemantics, DerivedVersion, Determination,
    IndexedParameter, Interval, JudgmentExpr, JudgmentOutcome, MissingFacts, Need, Needs,
    OverPeriodsKind, ParameterVersion, Period, PeriodKind, Program, RelatedValueRef,
    RelationDerivation, RelationSchema, ScalarExpr, ScalarValue,
};
use chrono::NaiveDate;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner};
use rust_decimal::Decimal;

use Determination::{Determined, Indeterminate};
use JudgmentOutcome::{Holds, NotHolds, Undetermined};

const MIB: usize = 1024 * 1024;
const CASE: &str = "Case";
/// How every refusal of a construct this PR does not lift begins.
const REFUSAL: &str = "type mismatch: missing_facts=indeterminate does not yet support ";

// ===========================================================================
// Building programs and datasets
// ===========================================================================

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn jan() -> Period {
    Period::month(2026, 1)
}

/// The first quarter of 2026: it contains `jan()`.
fn q1() -> Period {
    Period {
        kind: PeriodKind::Custom("quarter".to_string()),
        start: date(2026, 1, 1),
        end: date(2026, 3, 31),
    }
}

fn interval(start: NaiveDate, end: NaiveDate) -> Interval {
    Interval { start, end }
}

fn int(value: i64) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Integer(value))
}

fn dec(value: &str) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Decimal(decimal(value)))
}

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("valid decimal")
}

fn text(value: &str) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Text(value.to_string()))
}

fn boolean(value: bool) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Bool(value))
}

fn input(name: &str) -> ScalarExpr {
    ScalarExpr::Input(name.to_string())
}

fn input_or(name: &str, default: i64) -> ScalarExpr {
    ScalarExpr::InputOrElse {
        name: name.to_string(),
        default: ScalarValue::Integer(default),
    }
}

fn scalar_ref(name: &str) -> ScalarExpr {
    ScalarExpr::Derived(name.to_string())
}

fn add(items: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Add(items)
}

fn div(left: ScalarExpr, right: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Div(Box::new(left), Box::new(right))
}

fn if_then(condition: JudgmentExpr, then_expr: ScalarExpr, else_expr: ScalarExpr) -> ScalarExpr {
    ScalarExpr::If {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    }
}

fn no_match(subject: ScalarExpr, patterns: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::NoMatch {
        subject: Box::new(subject),
        patterns,
    }
}

fn cmp(left: ScalarExpr, op: ComparisonOp, right: ScalarExpr) -> JudgmentExpr {
    JudgmentExpr::Comparison { left, op, right }
}

fn holds() -> JudgmentExpr {
    cmp(int(0), ComparisonOp::Eq, int(0))
}

fn not_holds() -> JudgmentExpr {
    cmp(int(0), ComparisonOp::Eq, int(1))
}

/// `name == true`.
fn flag(name: &str) -> JudgmentExpr {
    cmp(input(name), ComparisonOp::Eq, boolean(true))
}

/// `(1 / 0 > 0)`: an item that fails with `DivisionByZero` whatever the data.
fn fails() -> JudgmentExpr {
    cmp(div(int(1), int(0)), ComparisonOp::Gt, int(0))
}

fn and(items: Vec<JudgmentExpr>) -> JudgmentExpr {
    JudgmentExpr::And(items)
}

fn or(items: Vec<JudgmentExpr>) -> JudgmentExpr {
    JudgmentExpr::Or(items)
}

fn not(item: JudgmentExpr) -> JudgmentExpr {
    JudgmentExpr::Not(Box::new(item))
}

fn judgment_ref(name: &str) -> JudgmentExpr {
    JudgmentExpr::Derived(name.to_string())
}

fn derived(name: &str, dtype: DType, semantics: DerivedSemantics) -> Derived {
    Derived {
        id: None,
        name: name.to_string(),
        entity: CASE.to_string(),
        dtype,
        unit: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: Vec::new(),
    }
}

fn scalar_rule(name: &str, expr: ScalarExpr) -> Derived {
    derived(name, DType::Decimal, DerivedSemantics::Scalar(expr))
}

fn judgment_rule(name: &str, expr: JudgmentExpr) -> Derived {
    derived(name, DType::Judgment, DerivedSemantics::Judgment(expr))
}

fn program(rules: Vec<Derived>) -> Program {
    let mut program = Program::default();
    for rule in rules {
        program.add_derived(rule).expect("distinct rule names");
    }
    program
}

/// One input record per `(slot, value)` for entity `c1`, covering 2026.
fn inputs(cells: &[(&str, ScalarValue)]) -> DataSet {
    let mut data = DataSet::default();
    for (slot, value) in cells {
        data.add_input(
            *slot,
            CASE,
            "c1",
            interval(date(2026, 1, 1), date(2026, 12, 31)),
            value.clone(),
        );
    }
    data
}

fn options(missing_facts: MissingFacts) -> EvaluationOptions {
    EvaluationOptions { missing_facts }
}

fn three_valued<'a>(program: &'a Program, data: &'a DataSet) -> Engine<'a> {
    Engine::with_options(program, data, options(MissingFacts::Indeterminate))
        .expect("the options are accepted")
}

/// `rule` for `c1` in January, under the option, on a fresh engine.
fn judge(program: &Program, data: &DataSet, rule: &str) -> String {
    format!(
        "{:?}",
        three_valued(program, data).judgment_determination(rule, "c1", &jan())
    )
}

/// `rule` for `c1` in January, without the option, on a fresh engine.
fn judge_today(program: &Program, data: &DataSet, rule: &str) -> String {
    format!(
        "{:?}",
        Engine::new(program, data).evaluate_judgment(rule, "c1", &jan())
    )
}

fn value_of(program: &Program, data: &DataSet, rule: &str) -> String {
    format!(
        "{:?}",
        three_valued(program, data).scalar_determination(rule, "c1", &jan())
    )
}

fn value_today(program: &Program, data: &DataSet, rule: &str) -> String {
    format!(
        "{:?}",
        Engine::new(program, data).evaluate_scalar(rule, "c1", &jan())
    )
}

fn need(slot: &str, entity_id: &str, period: Period) -> Need {
    Need::Input {
        name: slot.to_string(),
        entity_id: entity_id.to_string(),
        period,
    }
}

/// The needs for `slots` of `c1` in January.
fn needs(slots: &[&str]) -> Needs {
    needs_of(slots.iter().map(|slot| need(slot, "c1", jan())))
}

fn needs_of(needs: impl IntoIterator<Item = Need>) -> Needs {
    let mut needs = needs.into_iter();
    let mut set = Needs::one(needs.next().expect("at least one need"));
    for need in needs {
        set.extend(Needs::one(need));
    }
    set
}

fn ok<T: fmt::Debug>(determination: Determination<T>) -> String {
    format!("{:?}", Ok::<_, EvalError>(determination))
}

fn err(error: EvalError) -> String {
    format!("{:?}", Err::<(), _>(error))
}

fn unknown<T: fmt::Debug>(slots: &[&str]) -> String {
    ok(Indeterminate::<T>(needs(slots)))
}

fn missing_input(slot: &str) -> EvalError {
    EvalError::MissingInput {
        name: slot.to_string(),
        entity_id: "c1".to_string(),
        period_start: jan().start,
        period_end: jan().end,
    }
}

/// Run `f` on a fresh thread with a `bytes` stack. The deferral threshold is
/// per thread, so `f` sets its own.
fn on_stack<T: Send>(bytes: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(bytes)
            .spawn_scoped(scope, f)
            .expect("spawn test thread")
            .join()
            .expect("test thread")
    })
}

/// The deferral thresholds the named deferral cases compare: none, every
/// reference, a few in between, and the build's default.
const THRESHOLDS: [Option<usize>; 6] = [Some(usize::MAX), Some(1), Some(2), Some(3), Some(5), None];

fn at_threshold<T>(threshold: Option<usize>, f: impl FnOnce() -> T) -> T {
    match threshold {
        Some(levels) => with_suspend_depth(levels, f),
        None => f(),
    }
}

// ===========================================================================
// Named cases
// ===========================================================================

/// 1. A judgment over one missing input is unknown, and the need names the
/// slot, the entity and the period. Without the option it is `MissingInput`.
#[test]
fn a_judgment_over_a_missing_input_is_unknown_and_names_the_cell() {
    let program = program(vec![judgment_rule(
        "eligible",
        cmp(input("income"), ComparisonOp::Lt, int(100)),
    )]);
    let data = inputs(&[("age", ScalarValue::Integer(30))]);
    assert_eq!(
        judge(&program, &data, "eligible"),
        unknown::<JudgmentOutcome>(&["income"])
    );
    let mut engine = three_valued(&program, &data);
    let determination = engine
        .judgment_determination("eligible", "c1", &jan())
        .expect("no error");
    let Indeterminate(found) = determination else {
        panic!("unknown expected: {determination:?}");
    };
    assert_eq!(
        found.iter().collect::<Vec<_>>(),
        vec![&need("income", "c1", jan())]
    );
    assert_eq!(
        found.to_string(),
        "input `income` for entity `c1` over 2026-01-01..2026-01-31"
    );
    assert_eq!(
        format!("{:?}", engine.evaluate_judgment("eligible", "c1", &jan())),
        format!("{:?}", Ok::<_, EvalError>(Undetermined))
    );
    assert_eq!(
        judge_today(&program, &data, "eligible"),
        err(missing_input("income"))
    );
}

/// 2. A decisive item decides `and`/`or` whatever an unknown sibling is, in
/// either order. Without the option the unknown item first is an error.
#[test]
fn a_decisive_item_absorbs_an_unknown_one_in_either_order() {
    let missing = || flag("m");
    let program = program(vec![
        judgment_rule("missing_and_false", and(vec![missing(), not_holds()])),
        judgment_rule("false_and_missing", and(vec![not_holds(), missing()])),
        judgment_rule("missing_or_true", or(vec![missing(), holds()])),
        judgment_rule("true_or_missing", or(vec![holds(), missing()])),
    ]);
    let data = DataSet::default();
    for (rule, outcome) in [
        ("missing_and_false", NotHolds),
        ("false_and_missing", NotHolds),
        ("missing_or_true", Holds),
        ("true_or_missing", Holds),
    ] {
        assert_eq!(
            judge(&program, &data, rule),
            ok(Determined(outcome)),
            "{rule}"
        );
    }
    assert_eq!(
        judge_today(&program, &data, "missing_and_false"),
        err(missing_input("m"))
    );
    assert_eq!(
        judge_today(&program, &data, "false_and_missing"),
        format!("{:?}", Ok::<_, EvalError>(NotHolds))
    );
}

/// 3. The strong-Kleene tables for `and`, `or` (every pair of true, false and
/// unknown, both orders) and `not`, from inputs that are true, false or
/// missing. The left and right operands read different slots, so an unknown
/// result's needs show which side was unknown.
#[test]
fn and_or_not_follow_the_strong_kleene_tables() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum K {
        T,
        F,
        U,
    }
    let data = inputs(&[
        ("lt", ScalarValue::Bool(true)),
        ("lf", ScalarValue::Bool(false)),
        ("rt", ScalarValue::Bool(true)),
        ("rf", ScalarValue::Bool(false)),
    ]);
    let operand = |side: &str, value: K| match value {
        K::T => flag(&format!("{side}t")),
        K::F => flag(&format!("{side}f")),
        K::U => flag(&format!("{side}m")),
    };
    let expected = |value: K, slots: Vec<&str>| match value {
        K::T => ok(Determined(Holds)),
        K::F => ok(Determined(NotHolds)),
        K::U => unknown::<JudgmentOutcome>(&slots),
    };
    let values = [K::T, K::F, K::U];
    for left in values {
        for right in values {
            let and_value = match (left, right) {
                (K::F, _) | (_, K::F) => K::F,
                (K::T, K::T) => K::T,
                _ => K::U,
            };
            let or_value = match (left, right) {
                (K::T, _) | (_, K::T) => K::T,
                (K::F, K::F) => K::F,
                _ => K::U,
            };
            let unknown_slots: Vec<&str> = [(left, "lm"), (right, "rm")]
                .into_iter()
                .filter(|(value, _)| *value == K::U)
                .map(|(_, slot)| slot)
                .collect();
            let program = program(vec![
                judgment_rule("and", and(vec![operand("l", left), operand("r", right)])),
                judgment_rule("or", or(vec![operand("l", left), operand("r", right)])),
            ]);
            assert_eq!(
                judge(&program, &data, "and"),
                expected(and_value, unknown_slots.clone()),
                "{left:?} and {right:?}"
            );
            assert_eq!(
                judge(&program, &data, "or"),
                expected(or_value, unknown_slots),
                "{left:?} or {right:?}"
            );
        }
        let program = program(vec![judgment_rule("not", not(operand("l", left)))]);
        let negated = match left {
            K::T => K::F,
            K::F => K::T,
            K::U => K::U,
        };
        assert_eq!(
            judge(&program, &data, "not"),
            expected(negated, vec!["lm"]),
            "not {left:?}"
        );
    }
}

/// 4. A comparison with both operands missing, and `a + b` with both missing,
/// list both needs.
#[test]
fn both_missing_operands_are_both_needed() {
    let program = program(vec![
        judgment_rule("compare", cmp(input("a"), ComparisonOp::Lt, input("b"))),
        scalar_rule("sum", add(vec![input("a"), input("b")])),
        judgment_rule(
            "sum_positive",
            cmp(scalar_ref("sum"), ComparisonOp::Gt, int(0)),
        ),
    ]);
    let data = DataSet::default();
    assert_eq!(
        judge(&program, &data, "compare"),
        unknown::<JudgmentOutcome>(&["a", "b"])
    );
    assert_eq!(
        value_of(&program, &data, "sum"),
        unknown::<ScalarValue>(&["a", "b"])
    );
    assert_eq!(
        judge(&program, &data, "sum_positive"),
        unknown::<JudgmentOutcome>(&["a", "b"])
    );
}

/// 5. Needs propagate through rule references, and `Engine::needs` returns
/// the cached needs of every rule on the path that ended unknown.
#[test]
fn needs_propagate_through_rules_and_are_cached() {
    let program = program(vec![
        judgment_rule("resident", flag("lives_here")),
        scalar_rule("net", add(vec![input("income"), int(1)])),
        judgment_rule(
            "eligible",
            and(vec![
                judgment_ref("resident"),
                cmp(scalar_ref("net"), ComparisonOp::Gt, int(2)),
            ]),
        ),
    ]);
    let data = DataSet::default();
    let mut engine = three_valued(&program, &data);
    assert_eq!(
        format!(
            "{:?}",
            engine.judgment_determination("eligible", "c1", &jan())
        ),
        unknown::<JudgmentOutcome>(&["income", "lives_here"])
    );
    for (rule, slots) in [
        ("resident", vec!["lives_here"]),
        ("net", vec!["income"]),
        ("eligible", vec!["income", "lives_here"]),
    ] {
        assert_eq!(
            engine.needs(rule, "c1", &jan()),
            Some(needs(&slots)),
            "{rule}"
        );
        assert_eq!(engine.cached_judgment(rule, "c1", &jan()), None, "{rule}");
        assert_eq!(engine.cached_scalar(rule, "c1", &jan()), None, "{rule}");
    }
    // A later reference reads the cache: the same result again.
    assert_eq!(
        format!("{:?}", engine.scalar_determination("net", "c1", &jan())),
        unknown::<ScalarValue>(&["income"])
    );
    assert_eq!(engine.needs("eligible", "c2", &jan()), None);
}

/// 6. A failing item is caught: a decisive sibling decides in either order,
/// an unknown sibling makes the result unknown with only its needs, and with
/// neither the failure is raised again. Between two failures the one reported
/// does not depend on order, also when two errors render the same message.
#[test]
fn a_failing_item_is_caught_in_either_order() {
    let missing = || flag("m");
    let overflow = || {
        cmp(
            add(vec![
                ScalarExpr::Literal(ScalarValue::Decimal(Decimal::MAX)),
                int(1),
            ]),
            ComparisonOp::Gt,
            int(0),
        )
    };
    let cases: Vec<(&str, JudgmentExpr, String)> = vec![
        (
            "fails_and_false",
            and(vec![fails(), not_holds()]),
            ok(Determined(NotHolds)),
        ),
        (
            "false_and_fails",
            and(vec![not_holds(), fails()]),
            ok(Determined(NotHolds)),
        ),
        (
            "fails_or_true",
            or(vec![fails(), holds()]),
            ok(Determined(Holds)),
        ),
        (
            "true_or_fails",
            or(vec![holds(), fails()]),
            ok(Determined(Holds)),
        ),
        (
            "fails_and_missing",
            and(vec![fails(), missing()]),
            unknown::<JudgmentOutcome>(&["m"]),
        ),
        (
            "missing_and_fails",
            and(vec![missing(), fails()]),
            unknown::<JudgmentOutcome>(&["m"]),
        ),
        (
            "fails_or_missing",
            or(vec![fails(), missing()]),
            unknown::<JudgmentOutcome>(&["m"]),
        ),
        (
            "fails_and_true",
            and(vec![fails(), holds()]),
            err(EvalError::DivisionByZero),
        ),
        (
            "true_and_fails",
            and(vec![holds(), fails()]),
            err(EvalError::DivisionByZero),
        ),
        (
            "fails_or_false",
            or(vec![fails(), not_holds()]),
            err(EvalError::DivisionByZero),
        ),
        (
            "missing_and_fails_and_false",
            and(vec![missing(), fails(), not_holds()]),
            ok(Determined(NotHolds)),
        ),
        (
            "fails_and_false_and_missing",
            and(vec![fails(), not_holds(), missing()]),
            ok(Determined(NotHolds)),
        ),
        // Of two failures, the least by message: an overflow before a
        // division by zero, in either order.
        (
            "fails_and_overflows",
            and(vec![fails(), overflow()]),
            err(EvalError::ArithmeticOverflow("addition")),
        ),
        (
            "overflows_and_fails",
            and(vec![overflow(), fails()]),
            err(EvalError::ArithmeticOverflow("addition")),
        ),
        (
            "overflows_or_fails",
            or(vec![overflow(), fails()]),
            err(EvalError::ArithmeticOverflow("addition")),
        ),
    ];
    let program = program(
        cases
            .iter()
            .map(|(name, expr, _)| judgment_rule(name, expr.clone()))
            .collect(),
    );
    let data = DataSet::default();
    for (name, _, expected) in &cases {
        assert_eq!(&judge(&program, &data, name), expected, "{name}");
    }
    // Today the first failing item reached is the error, and a decisive item
    // decides only before it.
    assert_eq!(
        judge_today(&program, &data, "fails_and_false"),
        err(EvalError::DivisionByZero)
    );
    assert_eq!(
        judge_today(&program, &data, "false_and_fails"),
        format!("{:?}", Ok::<_, EvalError>(NotHolds))
    );
}

/// 6, continued. Two `match` failures that render the same message: one
/// raised in the root's formula, whose `rule` field is empty until it leaves
/// the root, and one raised in a rule named so that its message, which
/// carries that name, coincides. Choosing by message alone would pick by
/// position; the least by every field is the inline one, so in both orders
/// the root reports it, named for the root. (`src/engine.rs` checks the
/// choice before the error leaves any rule.)
#[test]
fn equal_messages_are_chosen_between_by_their_fields_in_either_order() {
    let inner = "` covers `a";
    let subject = "a` covers `x";
    let inline = || {
        cmp(
            no_match(input(subject), vec![int(2)]),
            ComparisonOp::Gt,
            int(0),
        )
    };
    let referenced = || cmp(scalar_ref(inner), ComparisonOp::Gt, int(0));
    let program = program(vec![
        scalar_rule(inner, no_match(input("x"), vec![int(2)])),
        judgment_rule("inline_first", and(vec![inline(), referenced()])),
        judgment_rule("referenced_first", and(vec![referenced(), inline()])),
        judgment_rule(
            "outer",
            or(vec![judgment_ref("referenced_first"), not_holds()]),
        ),
    ]);
    let data = inputs(&[
        ("x", ScalarValue::Integer(1)),
        (subject, ScalarValue::Integer(1)),
    ]);
    // The two raw errors, each alone, render the same message.
    let alone = program_with_rules(
        &program,
        vec![
            judgment_rule("inline_alone", and(vec![inline(), holds()])),
            judgment_rule("referenced_alone", and(vec![referenced(), holds()])),
        ],
    );
    let inline_error = Engine::new(&alone, &data)
        .evaluate_judgment("inline_alone", "c1", &jan())
        .expect_err("fails");
    let referenced_error = Engine::new(&alone, &data)
        .evaluate_judgment("referenced_alone", "c1", &jan())
        .expect_err("fails");
    assert_eq!(
        referenced_error.to_string(),
        "no `match` arm in `` covers `a` covers `x` = 1 (arms: 2); add an arm for it or a final `_ =>` arm"
    );
    assert!(
        matches!(&referenced_error, EvalError::NoMatchingArm { rule, .. } if rule == inner),
        "{referenced_error:?}"
    );
    // Once the inline error leaves its rule it is named for it.
    assert_eq!(
        inline_error.to_string(),
        "no `match` arm in `inline_alone` covers `a` covers `x` = 1 (arms: 2); add an arm for it or a final `_ =>` arm"
    );

    let expected = EvalError::NoMatchingArm {
        rule: "inline_first".to_string(),
        subject: subject.to_string(),
        value: "1".to_string(),
        patterns: "2".to_string(),
    };
    assert_eq!(judge(&program, &data, "inline_first"), err(expected));
    let reversed = EvalError::NoMatchingArm {
        rule: "referenced_first".to_string(),
        subject: subject.to_string(),
        value: "1".to_string(),
        patterns: "2".to_string(),
    };
    assert_eq!(
        judge(&program, &data, "referenced_first"),
        err(reversed.clone())
    );
    // After it has left one rule the error keeps that name through the next.
    assert_eq!(judge(&program, &data, "outer"), err(reversed));
}

fn program_with_rules(base: &Program, rules: Vec<Derived>) -> Program {
    let mut program = base.clone();
    for rule in rules {
        program.add_derived(rule).expect("distinct rule names");
    }
    program
}

/// 7. A rule that fails caches nothing, wherever its failure is caught: the
/// result of `(missing == 1 and R) or R` is unknown, `R` has no cached value
/// and no needs, and evaluating `R` itself later fails.
#[test]
fn a_rule_whose_failure_is_caught_caches_nothing() {
    let program = program(vec![
        judgment_rule(
            "r",
            and(vec![
                cmp(div(int(1), int(0)), ComparisonOp::Eq, int(0)),
                holds(),
            ]),
        ),
        judgment_rule(
            "root",
            or(vec![
                and(vec![
                    cmp(input("m"), ComparisonOp::Eq, int(1)),
                    judgment_ref("r"),
                ]),
                judgment_ref("r"),
            ]),
        ),
    ]);
    let data = DataSet::default();
    let mut engine = three_valued(&program, &data);
    assert_eq!(
        format!("{:?}", engine.judgment_determination("root", "c1", &jan())),
        unknown::<JudgmentOutcome>(&["m"])
    );
    assert_eq!(engine.cached_judgment("r", "c1", &jan()), None);
    assert_eq!(engine.needs("r", "c1", &jan()), None);
    assert_eq!(
        format!("{:?}", engine.judgment_determination("r", "c1", &jan())),
        err(EvalError::DivisionByZero)
    );
}

/// 8. `input_or_else` on a missing cell is indeterminate under the option and
/// its default without it; on a supplied cell it is the value in both.
#[test]
fn input_or_else_does_not_default_under_the_option() {
    let program = program(vec![
        scalar_rule("relief", input_or("blind", 0)),
        judgment_rule(
            "gets_relief",
            cmp(input_or("blind", 0), ComparisonOp::Eq, int(1)),
        ),
    ]);
    let empty = DataSet::default();
    assert_eq!(
        value_of(&program, &empty, "relief"),
        unknown::<ScalarValue>(&["blind"])
    );
    assert_eq!(
        judge(&program, &empty, "gets_relief"),
        unknown::<JudgmentOutcome>(&["blind"])
    );
    assert_eq!(
        value_today(&program, &empty, "relief"),
        format!("{:?}", Ok::<_, EvalError>(ScalarValue::Integer(0)))
    );
    assert_eq!(
        judge_today(&program, &empty, "gets_relief"),
        format!("{:?}", Ok::<_, EvalError>(NotHolds))
    );
    let supplied = inputs(&[("blind", ScalarValue::Integer(1))]);
    assert_eq!(
        value_of(&program, &supplied, "relief"),
        ok(Determined(ScalarValue::Integer(1)))
    );
    assert_eq!(
        value_today(&program, &supplied, "relief"),
        format!("{:?}", Ok::<_, EvalError>(ScalarValue::Integer(1)))
    );
}

/// 8a. Program checks refuse, before evaluating anything, a defect no dataset
/// can change, even behind a decisive item, with the error evaluation would
/// raise for it. Without the option the decisive item hides it, as on main. A
/// rule with no formula for the period is no defect: it fails only if
/// reached, so behind `false` it is `not_holds` in both modes.
#[test]
fn program_checks_refuse_defects_behind_a_decisive_item() {
    let mut rules = vec![
        scalar_rule("five", int(5)),
        {
            let mut rule = judgment_rule("measured", holds());
            rule.unit = Some("furlongs".to_string());
            rule
        },
        {
            let mut rule = judgment_rule("future", holds());
            rule.versions = vec![DerivedVersion {
                effective_from: date(2027, 1, 1),
                effective_to: None,
                semantics: DerivedSemantics::Judgment(holds()),
            }];
            rule
        },
    ];
    let defects: Vec<(&str, JudgmentExpr, EvalError)> = vec![
        (
            "undeclared_unit",
            or(vec![holds(), judgment_ref("measured")]),
            EvalError::UnknownUnit("furlongs".to_string()),
        ),
        (
            "judgment_of_scalar",
            and(vec![not_holds(), judgment_ref("five")]),
            EvalError::ExpectedJudgment("five".to_string()),
        ),
        (
            "scalar_of_judgment",
            and(vec![
                not_holds(),
                cmp(scalar_ref("measured_free"), ComparisonOp::Gt, int(0)),
            ]),
            EvalError::ExpectedScalar("measured_free".to_string()),
        ),
        (
            "unknown_rule",
            and(vec![not_holds(), judgment_ref("nowhere")]),
            EvalError::UnknownDerived("nowhere".to_string()),
        ),
        (
            "unknown_parameter",
            and(vec![
                not_holds(),
                cmp(
                    ScalarExpr::ParameterLookup {
                        parameter: "table".to_string(),
                        index: Box::new(int(1)),
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            ]),
            EvalError::UnknownParameter("table".to_string()),
        ),
        (
            "empty_max",
            and(vec![
                not_holds(),
                cmp(ScalarExpr::Max(Vec::new()), ComparisonOp::Gt, int(0)),
            ]),
            EvalError::TypeMismatch("max() requires at least one operand".to_string()),
        ),
        (
            "over_periods",
            and(vec![
                not_holds(),
                cmp(
                    ScalarExpr::OverPeriods {
                        kind: OverPeriodsKind::Sum,
                        value: Box::new(int(1)),
                        n: None,
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            ]),
            EvalError::OverPeriodsOutsideLifetime("sum_over_periods"),
        ),
        (
            "member_outside_relation",
            and(vec![
                not_holds(),
                JudgmentExpr::RelationMember {
                    relation: "members".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
            ]),
            EvalError::TypeMismatch(
                "relation predicate `members` can only be evaluated inside a derived relation"
                    .to_string(),
            ),
        ),
        (
            "unknown_relation",
            and(vec![
                not_holds(),
                cmp(
                    ScalarExpr::CountRelated {
                        relation: "nowhere".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        where_clause: None,
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            ]),
            EvalError::UnknownRelation("nowhere".to_string()),
        ),
        (
            "slot_out_of_range",
            and(vec![
                not_holds(),
                cmp(
                    ScalarExpr::CountRelated {
                        relation: "members".to_string(),
                        current_slot: 1,
                        related_slot: 2,
                        where_clause: None,
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            ]),
            EvalError::TypeMismatch(
                "relation `members` has arity 2, but slots 1 and 2 were requested".to_string(),
            ),
        ),
        // Defects reached only through a rule, and through a derived
        // relation's predicate, are found too.
        (
            "defect_in_a_rule",
            and(vec![not_holds(), judgment_ref("reads_nowhere")]),
            EvalError::UnknownDerived("nowhere".to_string()),
        ),
        (
            "defect_in_a_predicate",
            and(vec![
                not_holds(),
                cmp(
                    ScalarExpr::CountRelated {
                        relation: "filtered".to_string(),
                        current_slot: 1,
                        related_slot: 0,
                        where_clause: None,
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            ]),
            EvalError::UnknownParameter("predicate_table".to_string()),
        ),
    ];
    rules.push(judgment_rule("measured_free", holds()));
    rules.push(judgment_rule("reads_nowhere", judgment_ref("nowhere")));
    for (name, expr, _) in &defects {
        rules.push(judgment_rule(name, expr.clone()));
    }
    rules.push(judgment_rule(
        "future_behind_false",
        and(vec![not_holds(), judgment_ref("future")]),
    ));
    rules.push(judgment_rule(
        "future_before_false",
        and(vec![judgment_ref("future"), not_holds()]),
    ));
    let mut program = program(rules);
    program.add_relation("members", 2).expect("new relation");
    program
        .add_relation_schema(RelationSchema {
            name: "filtered".to_string(),
            arity: 2,
            slot_entities: Vec::new(),
            derivation: Some(RelationDerivation {
                source_relation: "members".to_string(),
                current_slot: 1,
                related_slot: 0,
                entity: None,
                member_relation: None,
                slot_entities: Vec::new(),
                predicate: cmp(
                    ScalarExpr::ParameterLookup {
                        parameter: "predicate_table".to_string(),
                        index: Box::new(int(1)),
                    },
                    ComparisonOp::Gt,
                    int(0),
                ),
            }),
        })
        .expect("new relation");
    let data = DataSet::default();
    for (name, expr, error) in &defects {
        assert_eq!(judge(&program, &data, name), err(error.clone()), "{name}");
        let decided = match expr {
            JudgmentExpr::Or(_) => Holds,
            _ => NotHolds,
        };
        assert_eq!(
            judge_today(&program, &data, name),
            format!("{:?}", Ok::<_, EvalError>(decided)),
            "{name} without the option"
        );
    }
    assert_eq!(
        judge(&program, &data, "future_behind_false"),
        ok(Determined(NotHolds))
    );
    assert_eq!(
        judge_today(&program, &data, "future_behind_false"),
        format!("{:?}", Ok::<_, EvalError>(NotHolds))
    );
    // Reached first, its failure is caught like any other.
    assert_eq!(
        judge(&program, &data, "future_before_false"),
        ok(Determined(NotHolds))
    );
    assert_eq!(
        judge_today(&program, &data, "future_before_false"),
        err(EvalError::MissingDerivedFormulaVersion {
            derived: "future".to_string(),
            at: date(2026, 1, 1),
        })
    );
}

/// 8a, continued. With several defects the least is reported, so the refusal
/// does not depend on where each sits.
#[test]
fn program_checks_report_the_least_defect_in_either_order() {
    let first = || judgment_ref("nowhere");
    let second = || {
        cmp(
            ScalarExpr::ParameterLookup {
                parameter: "table".to_string(),
                index: Box::new(int(1)),
            },
            ComparisonOp::Gt,
            int(0),
        )
    };
    let program = program(vec![
        judgment_rule("forward", and(vec![not_holds(), first(), second()])),
        judgment_rule("backward", and(vec![not_holds(), second(), first()])),
    ]);
    let data = DataSet::default();
    let expected = err(EvalError::UnknownDerived("nowhere".to_string()));
    assert_eq!(judge(&program, &data, "forward"), expected);
    assert_eq!(judge(&program, &data, "backward"), expected);
}

/// 8b. A scalar rule referenced as a judgment beside `false` and as a scalar
/// elsewhere in the same formula is refused at every deferral threshold.
/// Then the same shape with a rule that fails only by data (a division by an
/// input that is zero): referenced in an `and` beside `false` its failure is
/// caught, and referenced outside any `and`/`or` it fails the result, the
/// same at every threshold.
#[test]
fn admission_and_data_failures_do_not_depend_on_the_threshold() {
    let wrong_kind = program(vec![
        scalar_rule("s", int(5)),
        judgment_rule(
            "root",
            or(vec![
                and(vec![judgment_ref("s"), not_holds()]),
                cmp(scalar_ref("s"), ComparisonOp::Gt, int(0)),
            ]),
        ),
    ]);
    let data = DataSet::default();
    for threshold in THRESHOLDS {
        let result = on_stack(64 * MIB, || {
            at_threshold(threshold, || judge(&wrong_kind, &data, "root"))
        });
        assert_eq!(
            result,
            err(EvalError::ExpectedJudgment("s".to_string())),
            "threshold {threshold:?}"
        );
    }

    let by_data = program(vec![
        scalar_rule("s", div(int(1), input("d"))),
        scalar_rule(
            "root",
            add(vec![
                if_then(
                    and(vec![
                        cmp(scalar_ref("s"), ComparisonOp::Gt, int(0)),
                        not_holds(),
                    ]),
                    int(1),
                    int(0),
                ),
                scalar_ref("s"),
            ]),
        ),
        judgment_rule(
            "caught",
            and(vec![
                cmp(scalar_ref("s"), ComparisonOp::Gt, int(0)),
                not_holds(),
            ]),
        ),
    ]);
    for (value, expected_root, expected_caught) in [
        (
            Some(0),
            err(EvalError::DivisionByZero),
            ok(Determined(NotHolds)),
        ),
        (
            Some(2),
            ok(Determined(ScalarValue::Decimal(decimal("0.50")))),
            ok(Determined(NotHolds)),
        ),
        (
            None,
            unknown::<ScalarValue>(&["d"]),
            ok(Determined(NotHolds)),
        ),
    ] {
        let data = match value {
            Some(value) => inputs(&[("d", ScalarValue::Integer(value))]),
            None => DataSet::default(),
        };
        let undeferred = on_stack(64 * MIB, || {
            with_suspend_depth(usize::MAX, || {
                let mut engine = three_valued(&by_data, &data);
                (
                    format!(
                        "{:?}",
                        engine.judgment_determination("caught", "c1", &jan())
                    ),
                    format!("{:?}", engine.scalar_determination("root", "c1", &jan())),
                )
            })
        });
        assert_eq!(
            undeferred,
            (expected_caught.clone(), expected_root.clone()),
            "d = {value:?}"
        );
        for threshold in THRESHOLDS {
            let deferred = on_stack(64 * MIB, || {
                at_threshold(threshold, || {
                    let mut engine = three_valued(&by_data, &data);
                    (
                        format!(
                            "{:?}",
                            engine.judgment_determination("caught", "c1", &jan())
                        ),
                        format!("{:?}", engine.scalar_determination("root", "c1", &jan())),
                    )
                })
            });
            assert_eq!(
                deferred, undeferred,
                "d = {value:?}, threshold {threshold:?}"
            );
        }
    }
}

/// 8d. Two records for one cell with the same start and the decimals `1.0`
/// and `1.00` are both admitted, and which one a lookup reads depends on
/// record order. Under the option a `match` failure writes the decimal
/// normalized, so its message is the same in both orders; without it the
/// message shows the scale of whichever record was read.
#[test]
fn a_match_failure_does_not_show_which_equal_record_was_read() {
    let program = program(vec![scalar_rule(
        "band",
        if_then(
            cmp(input("x"), ComparisonOp::Eq, int(2)),
            int(10),
            no_match(input("x"), vec![dec("2.0")]),
        ),
    )]);
    let record = |value: &str| ("x", ScalarValue::Decimal(decimal(value)));
    let orders = [
        inputs(&[record("1.0"), record("1.00")]),
        inputs(&[record("1.00"), record("1.0")]),
    ];
    let under_option: Vec<String> = orders
        .iter()
        .map(|data| {
            three_valued(&program, data)
                .scalar_determination("band", "c1", &jan())
                .expect_err("no arm covers 1")
                .to_string()
        })
        .collect();
    assert_eq!(under_option[0], under_option[1]);
    assert_eq!(
        under_option[0],
        "no `match` arm in `band` covers `x` = 1 (arms: 2); add an arm for it or a final `_ =>` arm"
    );
    let today: Vec<String> = orders
        .iter()
        .map(|data| {
            Engine::new(&program, data)
                .evaluate_scalar("band", "c1", &jan())
                .expect_err("no arm covers 1")
                .to_string()
        })
        .collect();
    assert_ne!(today[0], today[1], "without the option the scale shows");
}

/// 8e. The program checks walk what evaluation can execute: a `no_match`'s
/// patterns only label its error, so a defect there is not refused.
#[test]
fn no_match_patterns_are_not_checked() {
    let program = program(vec![
        scalar_rule(
            "seven",
            if_then(
                holds(),
                int(7),
                no_match(int(0), vec![ScalarExpr::Max(Vec::new())]),
            ),
        ),
        scalar_rule(
            "unmatched",
            if_then(
                not_holds(),
                int(7),
                no_match(int(0), vec![ScalarExpr::Max(Vec::new())]),
            ),
        ),
    ]);
    let data = DataSet::default();
    assert_eq!(
        value_of(&program, &data, "seven"),
        ok(Determined(ScalarValue::Integer(7)))
    );
    assert_eq!(
        value_of(&program, &data, "unmatched"),
        err(EvalError::NoMatchingArm {
            rule: "unmatched".to_string(),
            subject: "0".to_string(),
            value: "0".to_string(),
            patterns: "the match subject".to_string(),
        })
    );
}

/// 9. Scalar operators keep the reference order. Arithmetic before the first
/// indeterminate operand happens as today, so it can overflow; operands after
/// it are still evaluated and converted, so they can fail; a divisor is
/// evaluated first.
#[test]
fn scalar_operators_keep_the_reference_order() {
    let max = || ScalarExpr::Literal(ScalarValue::Decimal(Decimal::MAX));
    let program = program(vec![
        scalar_rule(
            "overflow_then_missing",
            add(vec![max(), int(1), input("m")]),
        ),
        scalar_rule(
            "missing_then_overflow",
            add(vec![input("m"), max(), int(1)]),
        ),
        scalar_rule(
            "missing_then_division_by_zero",
            add(vec![input("m"), div(int(1), int(0))]),
        ),
        scalar_rule("missing_then_text", add(vec![input("m"), text("text")])),
        scalar_rule("failing_dividend", div(div(int(1), int(0)), input("m"))),
        scalar_rule("text_dividend", div(text("text"), input("m"))),
        scalar_rule("zero_divisor", div(input("m"), int(0))),
    ]);
    let data = DataSet::default();
    for (rule, expected) in [
        (
            "overflow_then_missing",
            err(EvalError::ArithmeticOverflow("addition")),
        ),
        ("missing_then_overflow", unknown::<ScalarValue>(&["m"])),
        (
            "missing_then_division_by_zero",
            err(EvalError::DivisionByZero),
        ),
        (
            "missing_then_text",
            err(EvalError::TypeMismatch(
                "expected numeric scalar".to_string(),
            )),
        ),
        ("failing_dividend", err(EvalError::DivisionByZero)),
        (
            "text_dividend",
            err(EvalError::TypeMismatch(
                "expected numeric scalar".to_string(),
            )),
        ),
        ("zero_divisor", err(EvalError::DivisionByZero)),
    ] {
        assert_eq!(value_of(&program, &data, rule), expected, "{rule}");
    }
    assert_eq!(
        value_today(&program, &data, "missing_then_text"),
        err(missing_input("m"))
    );
}

/// 9, continued: every operand position of every scalar operator, with an
/// indeterminate operand (the result is indeterminate with its need) and
/// with a failing one (the failure, also when another operand is
/// indeterminate).
#[test]
fn every_operand_position_is_lifted() {
    let parameter = IndexedParameter {
        id: None,
        name: "table".to_string(),
        unit: None,
        indexed_by: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        versions: vec![ParameterVersion {
            effective_from: date(2020, 1, 1),
            effective_to: None,
            values: BTreeMap::from([(1, ScalarValue::Integer(10))]),
        }],
    };
    let date_input = |name: &str| input(name);
    type Shape = fn(Vec<ScalarExpr>) -> ScalarExpr;
    let shapes: Vec<(&str, usize, Shape)> = vec![
        ("add", 3, |operands| ScalarExpr::Add(operands)),
        ("sub", 2, |mut o| {
            let right = o.pop().unwrap();
            ScalarExpr::Sub(Box::new(o.pop().unwrap()), Box::new(right))
        }),
        ("mul", 2, |mut o| {
            let right = o.pop().unwrap();
            ScalarExpr::Mul(Box::new(o.pop().unwrap()), Box::new(right))
        }),
        ("div", 2, |mut o| {
            let right = o.pop().unwrap();
            ScalarExpr::Div(Box::new(o.pop().unwrap()), Box::new(right))
        }),
        ("max", 3, |operands| ScalarExpr::Max(operands)),
        ("min", 3, |operands| ScalarExpr::Min(operands)),
        ("ceil", 1, |mut o| {
            ScalarExpr::Ceil(Box::new(o.pop().unwrap()))
        }),
        ("floor", 1, |mut o| {
            ScalarExpr::Floor(Box::new(o.pop().unwrap()))
        }),
        ("parameter", 1, |mut o| ScalarExpr::ParameterLookup {
            parameter: "table".to_string(),
            index: Box::new(o.pop().unwrap()),
        }),
        ("no_match", 1, |mut o| ScalarExpr::NoMatch {
            subject: Box::new(o.pop().unwrap()),
            patterns: vec![int(2)],
        }),
    ];
    let date_shapes: Vec<(&str, Shape)> = vec![
        ("date_add_days", |mut o| {
            let days = o.pop().unwrap();
            ScalarExpr::DateAddDays {
                date: Box::new(o.pop().unwrap()),
                days: Box::new(days),
            }
        }),
        ("date_add_months", |mut o| {
            let months = o.pop().unwrap();
            ScalarExpr::DateAddMonths {
                date: Box::new(o.pop().unwrap()),
                months: Box::new(months),
            }
        }),
        ("date_add_years", |mut o| {
            let years = o.pop().unwrap();
            ScalarExpr::DateAddYears {
                date: Box::new(o.pop().unwrap()),
                years: Box::new(years),
            }
        }),
        ("days_between", |mut o| {
            let to = o.pop().unwrap();
            ScalarExpr::DaysBetween {
                from: Box::new(o.pop().unwrap()),
                to: Box::new(to),
            }
        }),
    ];
    let failing = || div(int(1), int(0));
    let mut rules = Vec::new();
    let mut expectations = Vec::new();
    for (name, arity, shape) in &shapes {
        for position in 0..*arity {
            // Known operands are 1, which every operator accepts.
            let with = |operand: ScalarExpr| {
                (0..*arity)
                    .map(|index| {
                        if index == position {
                            operand.clone()
                        } else {
                            int(1)
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let rule = format!("{name}_{position}_missing");
            rules.push(scalar_rule(&rule, shape(with(input("m")))));
            expectations.push((rule, unknown::<ScalarValue>(&["m"])));
            let rule = format!("{name}_{position}_failing");
            rules.push(scalar_rule(&rule, shape(with(failing()))));
            expectations.push((rule, err(EvalError::DivisionByZero)));
            if *arity > 1 {
                // Every other operand missing: the failure still surfaces.
                let operands = (0..*arity)
                    .map(|index| {
                        if index == position {
                            failing()
                        } else {
                            input("m")
                        }
                    })
                    .collect();
                let rule = format!("{name}_{position}_failing_beside_missing");
                rules.push(scalar_rule(&rule, shape(operands)));
                expectations.push((rule, err(EvalError::DivisionByZero)));
            }
        }
    }
    for (name, shape) in &date_shapes {
        let known = |position: usize| {
            if *name == "days_between" || position == 0 {
                date_input("day")
            } else {
                int(1)
            }
        };
        for position in 0..2 {
            let with = |operand: ScalarExpr| {
                (0..2)
                    .map(|index| {
                        if index == position {
                            operand.clone()
                        } else {
                            known(index)
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let rule = format!("{name}_{position}_missing");
            rules.push(scalar_rule(&rule, shape(with(input("m")))));
            expectations.push((rule, unknown::<ScalarValue>(&["m"])));
            let rule = format!("{name}_{position}_failing");
            rules.push(scalar_rule(&rule, shape(with(failing()))));
            expectations.push((rule, err(EvalError::DivisionByZero)));
            let other = 1 - position;
            let operands = (0..2)
                .map(|index| {
                    if index == other {
                        input("m")
                    } else {
                        failing()
                    }
                })
                .collect();
            let rule = format!("{name}_{position}_failing_beside_missing");
            rules.push(scalar_rule(&rule, shape(operands)));
            expectations.push((rule, err(EvalError::DivisionByZero)));
        }
    }
    // A comparison, both positions.
    for position in 0..2 {
        let operands = |operand: ScalarExpr| {
            if position == 0 {
                (operand, int(1))
            } else {
                (int(1), operand)
            }
        };
        let (left, right) = operands(input("m"));
        let rule = format!("cmp_{position}_missing");
        rules.push(judgment_rule(&rule, cmp(left, ComparisonOp::Lt, right)));
        expectations.push((rule, unknown::<JudgmentOutcome>(&["m"])));
        let (left, right) = operands(failing());
        let rule = format!("cmp_{position}_failing");
        rules.push(judgment_rule(&rule, cmp(left, ComparisonOp::Lt, right)));
        expectations.push((rule, err(EvalError::DivisionByZero)));
        let (left, right) = if position == 0 {
            (failing(), input("m"))
        } else {
            (input("m"), failing())
        };
        let rule = format!("cmp_{position}_failing_beside_missing");
        rules.push(judgment_rule(&rule, cmp(left, ComparisonOp::Lt, right)));
        expectations.push((rule, err(EvalError::DivisionByZero)));
    }
    let mut program = program(rules);
    program.add_parameter(parameter).expect("new parameter");
    let data = inputs(&[("day", ScalarValue::Date(date(2026, 1, 15)))]);
    for (rule, expected) in expectations {
        let result = if rule.starts_with("cmp_") {
            judge(&program, &data, &rule)
        } else {
            value_of(&program, &data, &rule)
        };
        assert_eq!(result, expected, "{rule}");
    }
}

/// 10. Constructs this PR does not lift are refused by name under the
/// option, never answered: an `if` on an unknown condition, a `count` over a
/// relation with nothing missing, a derived relation, and `relation_member`
/// (reachable only through a derived relation's members). A refusal is not
/// a failure, so the same items beside `false` in an `and` are still
/// refused. A decisive item before one never reaches it, as on main.
#[test]
fn unlifted_constructs_are_refused_by_name() {
    let count = |relation: &str| {
        cmp(
            ScalarExpr::CountRelated {
                relation: relation.to_string(),
                current_slot: 1,
                related_slot: 0,
                where_clause: None,
            },
            ComparisonOp::Gt,
            int(0),
        )
    };
    let sum = cmp(
        ScalarExpr::SumRelated {
            relation: "members".to_string(),
            current_slot: 1,
            related_slot: 0,
            value: RelatedValueRef::Input("income".to_string()),
            where_clause: None,
        },
        ComparisonOp::Gt,
        int(0),
    );
    let unknown_if = cmp(if_then(flag("m"), int(1), int(2)), ComparisonOp::Gt, int(0));
    let mut program = program(vec![
        judgment_rule("unknown_if", unknown_if.clone()),
        judgment_rule("count", count("members")),
        judgment_rule("sum", sum.clone()),
        judgment_rule("derived", count("adults")),
        judgment_rule("member", count("heads")),
        judgment_rule(
            "unknown_if_beside_false",
            and(vec![unknown_if, not_holds()]),
        ),
        judgment_rule(
            "count_beside_false",
            and(vec![count("members"), not_holds()]),
        ),
        judgment_rule("sum_beside_false", and(vec![sum, not_holds()])),
        judgment_rule(
            "derived_beside_false",
            and(vec![count("adults"), not_holds()]),
        ),
        judgment_rule(
            "member_beside_false",
            and(vec![count("heads"), not_holds()]),
        ),
        judgment_rule(
            "false_before_count",
            and(vec![not_holds(), count("members")]),
        ),
        derived(
            "age",
            DType::Integer,
            DerivedSemantics::Scalar(input("age")),
        ),
    ]);
    program.add_relation("members", 2).expect("new relation");
    program.add_relation("head_of", 2).expect("new relation");
    for (name, predicate) in [
        ("adults", cmp(scalar_ref("age"), ComparisonOp::Gte, int(18))),
        (
            "heads",
            JudgmentExpr::RelationMember {
                relation: "head_of".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
        ),
    ] {
        program
            .add_relation_schema(RelationSchema {
                name: name.to_string(),
                arity: 2,
                slot_entities: Vec::new(),
                derivation: Some(RelationDerivation {
                    source_relation: "members".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                    entity: None,
                    member_relation: None,
                    slot_entities: Vec::new(),
                    predicate,
                }),
            })
            .expect("new relation");
    }
    let mut data = DataSet::default();
    for (person, age) in [("p1", 40), ("p2", 10)] {
        data.add_relation(
            "members",
            vec![person.to_string(), "c1".to_string()],
            interval(date(2026, 1, 1), date(2026, 12, 31)),
        );
        data.add_input(
            "age",
            CASE,
            person,
            interval(date(2026, 1, 1), date(2026, 12, 31)),
            ScalarValue::Integer(age),
        );
        data.add_input(
            "income",
            CASE,
            person,
            interval(date(2026, 1, 1), date(2026, 12, 31)),
            ScalarValue::Integer(100),
        );
    }
    data.add_relation(
        "head_of",
        vec!["p1".to_string(), "c1".to_string()],
        interval(date(2026, 1, 1), date(2026, 12, 31)),
    );
    for (rule, construct) in [
        ("unknown_if", "an `if` whose condition is undetermined"),
        ("count", "`count` over a relation"),
        ("sum", "`sum` over a relation"),
        ("derived", "`count` over a relation"),
        ("member", "`count` over a relation"),
        (
            "unknown_if_beside_false",
            "an `if` whose condition is undetermined",
        ),
        ("count_beside_false", "`count` over a relation"),
        ("sum_beside_false", "`sum` over a relation"),
        ("derived_beside_false", "`count` over a relation"),
        ("member_beside_false", "`count` over a relation"),
    ] {
        let result = three_valued(&program, &data).judgment_determination(rule, "c1", &jan());
        match result {
            Err(error) => assert_eq!(error.to_string(), format!("{REFUSAL}{construct}"), "{rule}"),
            Ok(answer) => panic!("{rule} must be refused, not answered: {answer:?}"),
        }
    }
    assert_eq!(
        judge(&program, &data, "false_before_count"),
        ok(Determined(NotHolds))
    );
    // 11. Without the option each is read as on main.
    for (rule, expected) in [
        ("count", Ok(Holds)),
        ("sum", Ok(Holds)),
        ("derived", Ok(Holds)),
        ("member", Ok(Holds)),
        ("count_beside_false", Ok(NotHolds)),
        ("member_beside_false", Ok(NotHolds)),
        ("unknown_if", Err(missing_input("m"))),
    ] {
        assert_eq!(
            judge_today(&program, &data, rule),
            format!("{:?}", expected),
            "{rule} without the option"
        );
    }
}

/// 11. Without the option the engine computes what `Engine::new` computes:
/// `MissingInput` at the same point, defaults applied, short circuits as
/// today. Both the default options and `Engine::new` are compared over every
/// rule of a program that mixes all of these, with the caches they leave.
#[test]
fn the_default_options_are_engine_new() {
    let program = program(vec![
        judgment_rule("missing_and_false", and(vec![flag("m"), not_holds()])),
        judgment_rule("false_and_missing", and(vec![not_holds(), flag("m")])),
        judgment_rule("fails_and_false", and(vec![fails(), not_holds()])),
        scalar_rule("defaulted", add(vec![input_or("m", 3), input("x")])),
        scalar_rule("missing", add(vec![input("x"), input("m")])),
        judgment_rule(
            "nested",
            or(vec![
                cmp(scalar_ref("defaulted"), ComparisonOp::Gt, int(100)),
                judgment_ref("false_and_missing"),
            ]),
        ),
    ]);
    let data = inputs(&[("x", ScalarValue::Integer(4))]);
    let rules: Vec<&String> = program.derived.keys().collect();
    let session = |mut engine: Engine<'_>| {
        let mut log = Vec::new();
        for rule in &rules {
            let result = match program.derived[*rule].semantics {
                DerivedSemantics::Judgment(_) => {
                    format!("{:?}", engine.evaluate_judgment(rule, "c1", &jan()))
                }
                DerivedSemantics::Scalar(_) => {
                    format!("{:?}", engine.evaluate_scalar(rule, "c1", &jan()))
                }
            };
            log.push(format!("{rule}: {result}"));
        }
        for rule in &rules {
            log.push(format!(
                "cache {rule}: {:?} {:?} {:?}",
                engine.cached_scalar(rule, "c1", &jan()),
                engine.cached_judgment(rule, "c1", &jan()),
                engine.needs(rule, "c1", &jan())
            ));
        }
        log
    };
    let today = session(Engine::new(&program, &data));
    let defaulted = session(
        Engine::with_options(&program, &data, EvaluationOptions::default()).expect("accepted"),
    );
    let explicit = session(
        Engine::with_options(&program, &data, options(MissingFacts::Error)).expect("accepted"),
    );
    assert_eq!(today, defaulted);
    assert_eq!(today, explicit);
    assert!(
        today.iter().any(|line| line.contains("MissingInput")),
        "{today:#?}"
    );
}

/// 12. `evaluate_scalar` cannot say "indeterminate", so an indeterminate rule
/// is `EvalError::Indeterminate`, whose message names each need, and never a
/// value; `scalar_determination` returns it as a value.
#[test]
fn evaluate_scalar_reports_an_indeterminate_rule_as_an_error() {
    let program = program(vec![scalar_rule("net", add(vec![input("b"), input("a")]))]);
    let data = DataSet::default();
    let mut engine = three_valued(&program, &data);
    let error = engine
        .evaluate_scalar("net", "c1", &jan())
        .expect_err("indeterminate is not a value");
    assert_eq!(
        format!("{error:?}"),
        format!(
            "{:?}",
            EvalError::Indeterminate {
                needs: needs(&["a", "b"])
            }
        )
    );
    assert_eq!(
        error.to_string(),
        "result is indeterminate: it needs input `a` for entity `c1` over 2026-01-01..2026-01-31; input `b` for entity `c1` over 2026-01-01..2026-01-31"
    );
    assert_eq!(
        format!("{:?}", engine.scalar_determination("net", "c1", &jan())),
        unknown::<ScalarValue>(&["a", "b"])
    );
}

/// 13. Cells are per period. A record covering January only: on one engine,
/// January has the value and the first quarter needs the cell, in both query
/// orders; an older spell covering both, overridden in January, gives the
/// quarter its value.
#[test]
fn needs_are_per_period_on_one_engine() {
    let program = program(vec![scalar_rule(
        "doubled",
        add(vec![input("x"), input("x")]),
    )]);
    let mut narrow = DataSet::default();
    narrow.add_input(
        "x",
        CASE,
        "c1",
        interval(date(2025, 12, 1), date(2026, 1, 31)),
        ScalarValue::Integer(2),
    );
    let mut overlapping = narrow.clone();
    overlapping.add_input(
        "x",
        CASE,
        "c1",
        interval(date(2025, 1, 1), date(2027, 12, 31)),
        ScalarValue::Integer(5),
    );
    let quarter_need = || {
        ok(Indeterminate::<ScalarValue>(needs_of([need(
            "x",
            "c1",
            q1(),
        )])))
    };
    let value = |amount: i64| ok(Determined(ScalarValue::Decimal(Decimal::from(amount))));
    for (data, expected_jan, expected_q1) in [
        (&narrow, value(4), quarter_need()),
        (&overlapping, value(4), value(10)),
    ] {
        for periods in [[jan(), q1()], [q1(), jan()]] {
            let mut engine = three_valued(&program, data);
            for period in periods {
                let expected = if period == jan() {
                    &expected_jan
                } else {
                    &expected_q1
                };
                assert_eq!(
                    &format!(
                        "{:?}",
                        engine.scalar_determination("doubled", "c1", &period)
                    ),
                    expected,
                    "{period:?}"
                );
            }
        }
    }
}

/// A rule deep enough to defer fails, and is referenced from `and` items: one
/// beside a decisive item, one beside an unknown item. Each failure is
/// recorded by the driver and raised again at the reference, where the item
/// catches it, so the result is the undeferred one at every threshold; a
/// later reference outside any `and`/`or` raises the error.
#[test]
fn a_deferred_failure_is_caught_where_it_is_referenced() {
    let depth = 12;
    let mut rules: Vec<Derived> = (0..depth)
        .map(|index| {
            scalar_rule(
                &format!("f{index}"),
                add(vec![scalar_ref(&format!("f{}", index + 1)), int(0)]),
            )
        })
        .collect();
    rules.push(scalar_rule(
        &format!("f{depth}"),
        div(int(1), input("zero")),
    ));
    let deep = || cmp(scalar_ref("f0"), ComparisonOp::Gt, int(0));
    rules.push(judgment_rule(
        "root",
        or(vec![
            and(vec![deep(), not_holds()]),
            and(vec![deep(), flag("m")]),
        ]),
    ));
    rules.push(judgment_rule("decided", and(vec![deep(), not_holds()])));
    rules.push(judgment_rule("later", deep()));
    let program = program(rules);
    let data = inputs(&[("zero", ScalarValue::Integer(0))]);
    let session = || {
        let mut engine = three_valued(&program, &data);
        let mut log = Vec::new();
        for rule in ["root", "decided", "later", "root"] {
            log.push(format!(
                "{rule}: {:?}",
                engine.judgment_determination(rule, "c1", &jan())
            ));
        }
        log.push(format!(
            "f0 cached: {:?}",
            engine.cached_scalar("f0", "c1", &jan())
        ));
        log
    };
    let undeferred = on_stack(64 * MIB, || with_suspend_depth(usize::MAX, session));
    assert_eq!(
        undeferred,
        vec![
            format!("root: {}", unknown::<JudgmentOutcome>(&["m"])),
            format!("decided: {}", ok(Determined(NotHolds))),
            format!("later: {}", err(EvalError::DivisionByZero)),
            format!("root: {}", unknown::<JudgmentOutcome>(&["m"])),
            "f0 cached: None".to_string(),
        ]
    );
    for threshold in THRESHOLDS {
        let deferred = on_stack(64 * MIB, || at_threshold(threshold, session));
        assert_eq!(deferred, undeferred, "threshold {threshold:?}");
    }
}

/// The program checks and evaluation both handle a long chain of rules under
/// the option without deepening the stack: the checks walk rules from a
/// worklist, and evaluation defers.
#[test]
fn a_long_chain_answers_under_the_option_on_a_small_stack() {
    let length = 20_000;
    let mut rules: Vec<Derived> = (0..length)
        .map(|index| {
            scalar_rule(
                &format!("c{index}"),
                add(vec![scalar_ref(&format!("c{}", index + 1)), int(1)]),
            )
        })
        .collect();
    rules.push(scalar_rule(&format!("c{length}"), input("start")));
    let program = program(rules);
    let supplied = inputs(&[("start", ScalarValue::Integer(0))]);
    let empty = DataSet::default();
    let (value, unknown_value) = on_stack(2 * MIB, || {
        (
            three_valued(&program, &supplied).scalar_determination("c0", "c1", &jan()),
            three_valued(&program, &empty).scalar_determination("c0", "c1", &jan()),
        )
    });
    assert_eq!(
        format!("{value:?}"),
        ok(Determined(ScalarValue::Decimal(Decimal::from(length))))
    );
    assert_eq!(
        format!("{unknown_value:?}"),
        unknown::<ScalarValue>(&["start"])
    );
}

// ===========================================================================
// Random properties: the generator
//
// The generator has its own expression type. Slot and rule operands are
// indices, resolved against the program by `resolve` (modulo what exists, so
// shrinking never leaves a dangling reference), into the resolved form the
// oracle evaluates and `lower` turns into an engine `Program`.
// ===========================================================================

/// Cases per property when `AXIOM_THREE_VALUED_CASES` is unset.
const DEFAULT_CASES: u32 = 256;
const DEFAULT_SEED: u64 = 0x3a1e_d215_0001_2026;

const MAX_SLOTS: usize = 5;
const ENTITIES: usize = 2;
const PERIOD_LABELS: [&str; 2] = ["jan", "q1"];

fn periods() -> [Period; 2] {
    [jan(), q1()]
}

/// The spells a cell's records can have, by index. Their starts are
/// distinct, so two records of one cell never tie. The generator draws from
/// the first four; the last two are older than all of them, and supply a
/// cell in `supplying_facts_keeps_a_value` without overriding any record
/// that resolves another.
const SPELLS: [((i32, u32, u32), (i32, u32, u32)); 6] = [
    // Covers January and the quarter.
    ((2025, 1, 1), (2027, 12, 31)),
    // Covers January and the quarter; overrides the first.
    ((2025, 9, 1), (2026, 3, 31)),
    // Covers January only; overrides both above there.
    ((2025, 12, 1), (2026, 1, 31)),
    // Covers neither period: a record that resolves nothing queried.
    ((2026, 2, 1), (2026, 12, 31)),
    // Filler for the quarter (and January, unless another record does).
    ((2020, 1, 1), (2026, 3, 31)),
    // Filler for January only.
    ((2020, 6, 1), (2026, 1, 31)),
];
const QUARTER_FILLER: usize = 4;
const JANUARY_FILLER: usize = 5;

fn spell(index: usize) -> Interval {
    let ((sy, sm, sd), (ey, em, ed)) = SPELLS[index];
    interval(date(sy, sm, sd), date(ey, em, ed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotType {
    Bool,
    Int,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GOp {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Ne,
}

#[derive(Clone, Debug)]
enum GS {
    Int(i64),
    Slot(u8),
    SlotOr(u8, i64),
    Rule(u8),
    Add(Vec<GS>),
    Sub(Box<GS>, Box<GS>),
    Mul(Box<GS>, Box<GS>),
    Div(Box<GS>, Box<GS>),
}

#[derive(Clone, Debug)]
enum GJ {
    Cmp(GS, GOp, GS),
    /// A boolean slot compared with a boolean literal.
    Flag(u8, bool),
    Rule(u8),
    And(Vec<GJ>),
    Or(Vec<GJ>),
    Not(Box<GJ>),
}

#[derive(Clone, Debug)]
enum GRule {
    Scalar(GS),
    Judgment(GJ),
}

/// One (slot, entity) cell's records: which of the first four spells it has,
/// and a value byte for each (read only when present).
#[derive(Clone, Debug)]
struct GCell {
    present: [bool; 4],
    values: [u8; 4],
}

#[derive(Clone, Debug)]
struct GQuery {
    rule: u8,
    entity: u8,
    period: u8,
}

#[derive(Clone, Debug)]
struct GCase {
    slot_types: Vec<SlotType>,
    rules: Vec<GRule>,
    /// `MAX_SLOTS * ENTITIES` cells, slot-major; slots past the program's are
    /// unused.
    cells: Vec<GCell>,
    queries: Vec<GQuery>,
    /// Choices the properties make from the case: withdrawals, supplies and
    /// record orders.
    picks: Vec<u8>,
}

fn op_strategy() -> impl Strategy<Value = GOp> {
    prop_oneof![
        Just(GOp::Lt),
        Just(GOp::Lte),
        Just(GOp::Gt),
        Just(GOp::Gte),
        Just(GOp::Eq),
        Just(GOp::Ne),
    ]
}

fn scalar_strategy() -> BoxedStrategy<GS> {
    let leaf = prop_oneof![
        3 => (0i64..4).prop_map(GS::Int),
        5 => any::<u8>().prop_map(GS::Slot),
        1 => (any::<u8>(), 0i64..2).prop_map(|(slot, default)| GS::SlotOr(slot, default)),
        2 => any::<u8>().prop_map(GS::Rule),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            2 => vec(inner.clone(), 2..=3).prop_map(GS::Add),
            1 => (inner.clone(), inner.clone())
                .prop_map(|(left, right)| GS::Sub(Box::new(left), Box::new(right))),
            1 => (inner.clone(), inner.clone())
                .prop_map(|(left, right)| GS::Mul(Box::new(left), Box::new(right))),
            1 => (inner.clone(), inner)
                .prop_map(|(left, right)| GS::Div(Box::new(left), Box::new(right))),
        ]
    })
    .boxed()
}

fn judgment_strategy() -> BoxedStrategy<GJ> {
    let comparison = (
        scalar_strategy(),
        op_strategy(),
        prop_oneof![3 => (0i64..4).prop_map(GS::Int), 1 => scalar_strategy()],
    )
        .prop_map(|(left, op, right)| GJ::Cmp(left, op, right));
    let leaf = prop_oneof![
        4 => (any::<u8>(), any::<bool>()).prop_map(|(slot, value)| GJ::Flag(slot, value)),
        4 => comparison,
        2 => any::<u8>().prop_map(GJ::Rule),
    ];
    leaf.prop_recursive(3, 16, 3, |inner| {
        prop_oneof![
            3 => vec(inner.clone(), 2..=4).prop_map(GJ::And),
            3 => vec(inner.clone(), 2..=4).prop_map(GJ::Or),
            1 => inner.prop_map(|item| GJ::Not(Box::new(item))),
        ]
    })
    .boxed()
}

fn cell_strategy() -> impl Strategy<Value = GCell> {
    (
        [
            prop::bool::weighted(0.38),
            prop::bool::weighted(0.38),
            prop::bool::weighted(0.38),
            prop::bool::weighted(0.2),
        ],
        any::<[u8; 4]>(),
    )
        .prop_map(|(present, values)| GCell { present, values })
}

fn case_strategy() -> impl Strategy<Value = GCase> {
    (
        vec(
            prop_oneof![Just(SlotType::Bool), Just(SlotType::Int)],
            2..=MAX_SLOTS,
        ),
        vec(
            prop_oneof![
                1 => scalar_strategy().prop_map(GRule::Scalar),
                3 => judgment_strategy().prop_map(GRule::Judgment),
            ],
            1..=6,
        ),
        vec(cell_strategy(), MAX_SLOTS * ENTITIES),
        vec(
            (any::<u8>(), 0..ENTITIES as u8, 0..2u8).prop_map(|(rule, entity, period)| GQuery {
                rule,
                entity,
                period,
            }),
            1..=3,
        ),
        vec(any::<u8>(), 64),
    )
        .prop_map(|(slot_types, rules, cells, queries, picks)| GCase {
            slot_types,
            rules,
            cells,
            queries,
            picks,
        })
}

// ===========================================================================
// Resolved programs, records and lowering
// ===========================================================================

/// A value the oracle computes with. It shares no type with the engine.
#[derive(Clone, Debug, PartialEq)]
enum V {
    Bool(bool),
    Int(i64),
    Dec(Decimal),
}

#[derive(Clone, Debug)]
enum S {
    Lit(V),
    Input(usize),
    InputOr(usize, i64),
    Rule(usize),
    Add(Vec<S>),
    Sub(Box<S>, Box<S>),
    Mul(Box<S>, Box<S>),
    Div(Box<S>, Box<S>),
}

#[derive(Clone, Debug)]
enum J {
    Cmp(S, GOp, S),
    Rule(usize),
    And(Vec<J>),
    Or(Vec<J>),
    Not(Box<J>),
}

#[derive(Clone, Debug)]
enum Body {
    Scalar(S),
    Judgment(J),
}

#[derive(Clone, Debug)]
struct Prog {
    slot_types: Vec<SlotType>,
    rules: Vec<Body>,
}

/// A query: rule, entity and period indices.
#[derive(Clone, Copy, Debug)]
struct Q {
    rule: usize,
    entity: usize,
    period: usize,
}

/// An input record: slot, entity, spell index and value.
#[derive(Clone, Debug, PartialEq)]
struct Rec {
    slot: usize,
    entity: usize,
    spell: usize,
    value: V,
}

/// A cell: slot, entity and period indices.
type Cell = (usize, usize, usize);

/// The slot an operand reads: a slot of `preferred` type, or, for about one
/// byte in twelve, any slot, so that some reads have the wrong type.
fn slot_for(slot_types: &[SlotType], byte: u8, preferred: SlotType) -> usize {
    let typed: Vec<usize> = (0..slot_types.len())
        .filter(|&slot| slot_types[slot] == preferred)
        .collect();
    if byte >= 235 || typed.is_empty() {
        usize::from(byte) % slot_types.len()
    } else {
        typed[usize::from(byte) % typed.len()]
    }
}

fn resolve(case: &GCase) -> Prog {
    let slot_types = case.slot_types.clone();
    let mut rules: Vec<Body> = Vec::with_capacity(case.rules.len());
    for rule in &case.rules {
        let scalar_rules: Vec<usize> = (0..rules.len())
            .filter(|&index| matches!(rules[index], Body::Scalar(_)))
            .collect();
        let judgment_rules: Vec<usize> = (0..rules.len())
            .filter(|&index| matches!(rules[index], Body::Judgment(_)))
            .collect();
        let context = Resolver {
            slot_types: &slot_types,
            scalar_rules: &scalar_rules,
            judgment_rules: &judgment_rules,
        };
        rules.push(match rule {
            GRule::Scalar(expr) => Body::Scalar(context.scalar(expr)),
            GRule::Judgment(expr) => Body::Judgment(context.judgment(expr)),
        });
    }
    Prog { slot_types, rules }
}

struct Resolver<'a> {
    slot_types: &'a [SlotType],
    scalar_rules: &'a [usize],
    judgment_rules: &'a [usize],
}

impl Resolver<'_> {
    fn scalar(&self, expr: &GS) -> S {
        match expr {
            GS::Int(value) => S::Lit(V::Int(*value)),
            GS::Slot(byte) => S::Input(slot_for(self.slot_types, *byte, SlotType::Int)),
            GS::SlotOr(byte, default) => {
                S::InputOr(slot_for(self.slot_types, *byte, SlotType::Int), *default)
            }
            GS::Rule(byte) => match self.scalar_rules.len() {
                0 => S::Lit(V::Int(1)),
                count => S::Rule(self.scalar_rules[usize::from(*byte) % count]),
            },
            GS::Add(items) => S::Add(items.iter().map(|item| self.scalar(item)).collect()),
            GS::Sub(left, right) => {
                S::Sub(Box::new(self.scalar(left)), Box::new(self.scalar(right)))
            }
            GS::Mul(left, right) => {
                S::Mul(Box::new(self.scalar(left)), Box::new(self.scalar(right)))
            }
            GS::Div(left, right) => {
                S::Div(Box::new(self.scalar(left)), Box::new(self.scalar(right)))
            }
        }
    }

    fn judgment(&self, expr: &GJ) -> J {
        match expr {
            GJ::Cmp(left, op, right) => J::Cmp(self.scalar(left), *op, self.scalar(right)),
            GJ::Flag(byte, value) => self.flag(*byte, *value),
            GJ::Rule(byte) => match self.judgment_rules.len() {
                0 => self.flag(*byte, true),
                count => J::Rule(self.judgment_rules[usize::from(*byte) % count]),
            },
            GJ::And(items) => J::And(items.iter().map(|item| self.judgment(item)).collect()),
            GJ::Or(items) => J::Or(items.iter().map(|item| self.judgment(item)).collect()),
            GJ::Not(item) => J::Not(Box::new(self.judgment(item))),
        }
    }

    fn flag(&self, byte: u8, value: bool) -> J {
        J::Cmp(
            S::Input(slot_for(self.slot_types, byte, SlotType::Bool)),
            GOp::Eq,
            S::Lit(V::Bool(value)),
        )
    }
}

/// A record's value for a slot: mostly of the slot's type, one byte in
/// sixteen of the other type.
fn slot_value(slot_type: SlotType, byte: u8) -> V {
    match (slot_type, byte % 16) {
        (SlotType::Bool, 15) => V::Int(0),
        (SlotType::Bool, value) => V::Bool(value < 8),
        (SlotType::Int, 15) => V::Bool(true),
        (SlotType::Int, value) => V::Int(i64::from(value % 4)),
    }
}

/// The domain a missing cell of a slot ranges over in a completion: the
/// values the generator's literals use, zero included, and one value of the
/// wrong type.
fn domain(slot_type: SlotType) -> Vec<V> {
    match slot_type {
        SlotType::Bool => vec![V::Bool(true), V::Bool(false), V::Int(0)],
        SlotType::Int => vec![V::Int(0), V::Int(1), V::Int(2), V::Int(3), V::Bool(true)],
    }
}

/// The case's records, in generation order. With `complete`, a cell with no
/// record covering both periods gets one, so every cell resolves.
fn records(case: &GCase, complete: bool) -> Vec<Rec> {
    let mut records = Vec::new();
    for (slot, slot_type) in case.slot_types.iter().enumerate() {
        for entity in 0..ENTITIES {
            let cell = &case.cells[slot * ENTITIES + entity];
            let mut present = cell.present;
            if complete && !present[0] && !present[1] {
                present[0] = true;
            }
            for (spell, present) in present.into_iter().enumerate() {
                if present {
                    records.push(Rec {
                        slot,
                        entity,
                        spell,
                        value: slot_value(*slot_type, cell.values[spell]),
                    });
                }
            }
        }
    }
    records
}

fn queries(case: &GCase, prog: &Prog) -> Vec<Q> {
    let count = prog.rules.len();
    case.queries
        .iter()
        .map(|query| Q {
            // Half the queries ask for the last rule, which references most.
            rule: if query.rule < 128 {
                count - 1
            } else {
                usize::from(query.rule) % count
            },
            entity: usize::from(query.entity),
            period: usize::from(query.period),
        })
        .collect()
}

fn contains(spell_index: usize, period: &Period) -> bool {
    let spell = spell(spell_index);
    spell.start <= period.start && spell.end >= period.end
}

/// The oracle's own reading of a cell: among the records covering the
/// period, the one with the latest start.
fn resolved(records: &[Rec], (slot, entity, period): Cell) -> Option<V> {
    let period = &periods()[period];
    records
        .iter()
        .filter(|record| record.slot == slot && record.entity == entity)
        .filter(|record| contains(record.spell, period))
        .max_by_key(|record| spell(record.spell).start)
        .map(|record| record.value.clone())
}

fn all_cells(prog: &Prog) -> Vec<Cell> {
    let mut cells = Vec::new();
    for slot in 0..prog.slot_types.len() {
        for entity in 0..ENTITIES {
            for period in 0..PERIOD_LABELS.len() {
                cells.push((slot, entity, period));
            }
        }
    }
    cells
}

fn rule_name(index: usize) -> String {
    format!("r{index}")
}

fn slot_name(index: usize) -> String {
    format!("s{index}")
}

fn entity_name(index: usize) -> String {
    format!("c{index}")
}

fn engine_value(value: &V) -> ScalarValue {
    match value {
        V::Bool(value) => ScalarValue::Bool(*value),
        V::Int(value) => ScalarValue::Integer(*value),
        V::Dec(value) => ScalarValue::Decimal(*value),
    }
}

fn engine_op(op: GOp) -> ComparisonOp {
    match op {
        GOp::Lt => ComparisonOp::Lt,
        GOp::Lte => ComparisonOp::Lte,
        GOp::Gt => ComparisonOp::Gt,
        GOp::Gte => ComparisonOp::Gte,
        GOp::Eq => ComparisonOp::Eq,
        GOp::Ne => ComparisonOp::Ne,
    }
}

fn lower_scalar(expr: &S) -> ScalarExpr {
    match expr {
        S::Lit(value) => ScalarExpr::Literal(engine_value(value)),
        S::Input(slot) => ScalarExpr::Input(slot_name(*slot)),
        S::InputOr(slot, default) => ScalarExpr::InputOrElse {
            name: slot_name(*slot),
            default: ScalarValue::Integer(*default),
        },
        S::Rule(index) => ScalarExpr::Derived(rule_name(*index)),
        S::Add(items) => ScalarExpr::Add(items.iter().map(lower_scalar).collect()),
        S::Sub(left, right) => {
            ScalarExpr::Sub(Box::new(lower_scalar(left)), Box::new(lower_scalar(right)))
        }
        S::Mul(left, right) => {
            ScalarExpr::Mul(Box::new(lower_scalar(left)), Box::new(lower_scalar(right)))
        }
        S::Div(left, right) => {
            ScalarExpr::Div(Box::new(lower_scalar(left)), Box::new(lower_scalar(right)))
        }
    }
}

fn lower_judgment(expr: &J) -> JudgmentExpr {
    match expr {
        J::Cmp(left, op, right) => JudgmentExpr::Comparison {
            left: lower_scalar(left),
            op: engine_op(*op),
            right: lower_scalar(right),
        },
        J::Rule(index) => JudgmentExpr::Derived(rule_name(*index)),
        J::And(items) => JudgmentExpr::And(items.iter().map(lower_judgment).collect()),
        J::Or(items) => JudgmentExpr::Or(items.iter().map(lower_judgment).collect()),
        J::Not(item) => JudgmentExpr::Not(Box::new(lower_judgment(item))),
    }
}

fn lower(prog: &Prog) -> Program {
    program(
        prog.rules
            .iter()
            .enumerate()
            .map(|(index, body)| match body {
                Body::Scalar(expr) => scalar_rule(&rule_name(index), lower_scalar(expr)),
                Body::Judgment(expr) => judgment_rule(&rule_name(index), lower_judgment(expr)),
            })
            .collect(),
    )
}

fn lower_records(records: &[Rec]) -> DataSet {
    let mut data = DataSet::default();
    for record in records {
        data.add_input(
            slot_name(record.slot),
            CASE,
            entity_name(record.entity),
            spell(record.spell),
            engine_value(&record.value),
        );
    }
    data
}

/// Every `and`/`or` with its items reversed, or rotated left by one.
fn reorder_judgment(expr: &J, rotate: bool) -> J {
    let reorder = |items: &[J]| {
        let mut items: Vec<J> = items
            .iter()
            .map(|item| reorder_judgment(item, rotate))
            .collect();
        if rotate {
            items.rotate_left(1);
        } else {
            items.reverse();
        }
        items
    };
    match expr {
        J::And(items) => J::And(reorder(items)),
        J::Or(items) => J::Or(reorder(items)),
        J::Not(item) => J::Not(Box::new(reorder_judgment(item, rotate))),
        other => other.clone(),
    }
}

fn reorder(prog: &Prog, rotate: bool) -> Prog {
    Prog {
        slot_types: prog.slot_types.clone(),
        rules: prog
            .rules
            .iter()
            .map(|body| match body {
                Body::Scalar(expr) => Body::Scalar(expr.clone()),
                Body::Judgment(expr) => Body::Judgment(reorder_judgment(expr, rotate)),
            })
            .collect(),
    }
}

/// The slots `rule` can read, through the rules it references.
fn reachable_slots(prog: &Prog, rule: usize) -> BTreeSet<usize> {
    fn scalar(prog: &Prog, expr: &S, slots: &mut BTreeSet<usize>) {
        match expr {
            S::Lit(_) => {}
            S::Input(slot) | S::InputOr(slot, _) => {
                slots.insert(*slot);
            }
            S::Rule(index) => body(prog, *index, slots),
            S::Add(items) => items.iter().for_each(|item| scalar(prog, item, slots)),
            S::Sub(left, right) | S::Mul(left, right) | S::Div(left, right) => {
                scalar(prog, left, slots);
                scalar(prog, right, slots);
            }
        }
    }
    fn judgment(prog: &Prog, expr: &J, slots: &mut BTreeSet<usize>) {
        match expr {
            J::Cmp(left, _, right) => {
                scalar(prog, left, slots);
                scalar(prog, right, slots);
            }
            J::Rule(index) => body(prog, *index, slots),
            J::And(items) | J::Or(items) => {
                items.iter().for_each(|item| judgment(prog, item, slots))
            }
            J::Not(item) => judgment(prog, item, slots),
        }
    }
    fn body(prog: &Prog, index: usize, slots: &mut BTreeSet<usize>) {
        match &prog.rules[index] {
            Body::Scalar(expr) => scalar(prog, expr, slots),
            Body::Judgment(expr) => judgment(prog, expr, slots),
        }
    }
    let mut slots = BTreeSet::new();
    body(prog, rule, &mut slots);
    slots
}

// ===========================================================================
// The oracle
// ===========================================================================

/// An outcome: a value, unknown with the cells it needs, or a failure with
/// the engine's message for it.
#[derive(Clone, Debug, PartialEq)]
enum Out<T> {
    Val(T),
    Unknown(BTreeSet<Cell>),
    Fail(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Section 5 of the design note.
    Reference,
    /// Today's rules: short circuits, the first failure reached, defaults.
    Today,
}

/// What reading a cell finds: its value, nothing, or (while enumerating
/// completions) a cell not assigned yet, which stops the evaluation so that
/// the caller can branch on its values.
type Read<'r> = &'r dyn Fn(Cell) -> Result<Option<V>, Cell>;

/// Things a reference evaluation did that the non-vacuity guards count.
#[derive(Clone, Copy, Debug, Default)]
struct Events {
    /// A decisive `and`/`or` item decided after an earlier item was unknown.
    absorbed_unknown: bool,
    /// A decisive `and`/`or` item decided after an earlier item failed.
    absorbed_failure: bool,
}

struct Oracle<'r> {
    prog: &'r Prog,
    mode: Mode,
    entity: usize,
    period: usize,
    read: Read<'r>,
    events: Events,
}

fn overflow(operation: &str) -> String {
    format!("arithmetic overflow: {operation} result is outside the representable decimal range")
}

fn type_mismatch(message: &str) -> String {
    format!("type mismatch: {message}")
}

fn union(mut left: BTreeSet<Cell>, right: BTreeSet<Cell>) -> BTreeSet<Cell> {
    left.extend(right);
    left
}

impl Oracle<'_> {
    fn input(&mut self, slot: usize, default: Option<i64>) -> Result<Out<V>, Cell> {
        let cell = (slot, self.entity, self.period);
        Ok(match ((self.read)(cell)?, self.mode, default) {
            (Some(value), _, _) => Out::Val(value),
            (None, Mode::Reference, _) => Out::Unknown(BTreeSet::from([cell])),
            (None, Mode::Today, Some(default)) => Out::Val(V::Int(default)),
            (None, Mode::Today, None) => {
                let period = &periods()[self.period];
                Out::Fail(format!(
                    "missing input `{}` for entity `{}` over {}..{}",
                    slot_name(slot),
                    entity_name(self.entity),
                    period.start,
                    period.end
                ))
            }
        })
    }

    fn scalar(&mut self, expr: &S) -> Result<Out<V>, Cell> {
        Ok(match expr {
            S::Lit(value) => Out::Val(value.clone()),
            S::Input(slot) => self.input(*slot, None)?,
            S::InputOr(slot, default) => self.input(*slot, Some(*default))?,
            S::Rule(index) => match &self.prog.rules[*index] {
                Body::Scalar(expr) => self.scalar(expr)?,
                Body::Judgment(_) => unreachable!("references resolve to the right kind"),
            },
            S::Add(items) => {
                // Arithmetic stops at the first unknown operand; the operands
                // after it are still evaluated and converted.
                let mut total = Some(Decimal::ZERO);
                let mut unknown = BTreeSet::new();
                for item in items {
                    match self.decimal(item)? {
                        Out::Fail(message) => return Ok(Out::Fail(message)),
                        Out::Unknown(cells) => {
                            unknown.extend(cells);
                            total = None;
                        }
                        Out::Val(value) => {
                            if let Some(sum) = total {
                                match sum.checked_add(value) {
                                    Some(sum) => total = Some(sum),
                                    None => return Ok(Out::Fail(overflow("addition"))),
                                }
                            }
                        }
                    }
                }
                match total {
                    Some(total) => Out::Val(V::Dec(total)),
                    None => Out::Unknown(unknown),
                }
            }
            S::Sub(left, right) => self.binary(left, right, |a, b| {
                a.checked_sub(b).ok_or_else(|| overflow("subtraction"))
            })?,
            S::Mul(left, right) => self.binary(left, right, |a, b| {
                a.checked_mul(b).ok_or_else(|| overflow("multiplication"))
            })?,
            S::Div(left, right) => {
                // The divisor first; a known zero fails before the dividend.
                let divisor = match self.decimal(right)? {
                    Out::Fail(message) => return Ok(Out::Fail(message)),
                    Out::Val(value) if value.is_zero() => {
                        return Ok(Out::Fail("division by zero".to_string()));
                    }
                    other => other,
                };
                let dividend = match self.decimal(left)? {
                    Out::Fail(message) => return Ok(Out::Fail(message)),
                    other => other,
                };
                match (dividend, divisor) {
                    (Out::Val(a), Out::Val(b)) => match a.checked_div(b) {
                        Some(quotient) => Out::Val(V::Dec(quotient)),
                        None => Out::Fail(overflow("division")),
                    },
                    (a, b) => Out::Unknown(union(cells_of(a), cells_of(b))),
                }
            }
        })
    }

    fn binary(
        &mut self,
        left: &S,
        right: &S,
        operation: impl Fn(Decimal, Decimal) -> Result<Decimal, String>,
    ) -> Result<Out<V>, Cell> {
        let left = match self.decimal(left)? {
            Out::Fail(message) => return Ok(Out::Fail(message)),
            other => other,
        };
        let right = match self.decimal(right)? {
            Out::Fail(message) => return Ok(Out::Fail(message)),
            other => other,
        };
        Ok(match (left, right) {
            (Out::Val(a), Out::Val(b)) => match operation(a, b) {
                Ok(value) => Out::Val(V::Dec(value)),
                Err(message) => Out::Fail(message),
            },
            (a, b) => Out::Unknown(union(cells_of(a), cells_of(b))),
        })
    }

    fn decimal(&mut self, expr: &S) -> Result<Out<Decimal>, Cell> {
        Ok(match self.scalar(expr)? {
            Out::Val(V::Int(value)) => Out::Val(Decimal::from(value)),
            Out::Val(V::Dec(value)) => Out::Val(value),
            Out::Val(V::Bool(_)) => Out::Fail(type_mismatch("expected numeric scalar")),
            Out::Unknown(cells) => Out::Unknown(cells),
            Out::Fail(message) => Out::Fail(message),
        })
    }

    fn judgment(&mut self, expr: &J) -> Result<Out<bool>, Cell> {
        Ok(match expr {
            J::Cmp(left, op, right) => {
                let left = match self.scalar(left)? {
                    Out::Fail(message) => return Ok(Out::Fail(message)),
                    other => other,
                };
                let right = match self.scalar(right)? {
                    Out::Fail(message) => return Ok(Out::Fail(message)),
                    other => other,
                };
                match (left, right) {
                    (Out::Val(a), Out::Val(b)) => match compare(&a, *op, &b) {
                        Ok(holds) => Out::Val(holds),
                        Err(message) => Out::Fail(message),
                    },
                    (a, b) => Out::Unknown(union(cells_of(a), cells_of(b))),
                }
            }
            J::Rule(index) => match &self.prog.rules[*index] {
                Body::Judgment(expr) => self.judgment(expr)?,
                Body::Scalar(_) => unreachable!("references resolve to the right kind"),
            },
            J::And(items) => self.connective(items, false)?,
            J::Or(items) => self.connective(items, true)?,
            J::Not(item) => match self.judgment(item)? {
                Out::Val(value) => Out::Val(!value),
                other => other,
            },
        })
    }

    /// `and` (`decisive` false) or `or` (`decisive` true).
    fn connective(&mut self, items: &[J], decisive: bool) -> Result<Out<bool>, Cell> {
        let mut unknown: Option<BTreeSet<Cell>> = None;
        let mut failure: Option<String> = None;
        for item in items {
            match self.judgment(item)? {
                Out::Val(value) if value == decisive => {
                    self.events.absorbed_unknown |= unknown.is_some();
                    self.events.absorbed_failure |= failure.is_some();
                    return Ok(Out::Val(decisive));
                }
                Out::Val(_) => {}
                Out::Unknown(cells) => {
                    unknown = Some(union(unknown.unwrap_or_default(), cells));
                }
                Out::Fail(message) if self.mode == Mode::Reference => {
                    // The least failure by message: nothing about the result
                    // depends on the position of the items.
                    if failure.as_ref().is_none_or(|kept| message < *kept) {
                        failure = Some(message);
                    }
                }
                Out::Fail(message) => return Ok(Out::Fail(message)),
            }
        }
        Ok(match (unknown, failure) {
            (Some(cells), _) => Out::Unknown(cells),
            (None, Some(message)) => Out::Fail(message),
            (None, None) => Out::Val(!decisive),
        })
    }
}

fn cells_of<T>(out: Out<T>) -> BTreeSet<Cell> {
    match out {
        Out::Unknown(cells) => cells,
        _ => BTreeSet::new(),
    }
}

fn numeric(value: &V) -> Option<Decimal> {
    match value {
        V::Int(value) => Some(Decimal::from(*value)),
        V::Dec(value) => Some(*value),
        V::Bool(_) => None,
    }
}

fn compare(left: &V, op: GOp, right: &V) -> Result<bool, String> {
    if let (V::Bool(left), V::Bool(right)) = (left, right) {
        return match op {
            GOp::Eq => Ok(left == right),
            GOp::Ne => Ok(left != right),
            _ => Err(type_mismatch("boolean comparisons only support == and !=")),
        };
    }
    let left =
        numeric(left).ok_or_else(|| type_mismatch("left side of comparison is not numeric"))?;
    let right =
        numeric(right).ok_or_else(|| type_mismatch("right side of comparison is not numeric"))?;
    Ok(match op {
        GOp::Lt => left < right,
        GOp::Lte => left <= right,
        GOp::Gt => left > right,
        GOp::Gte => left >= right,
        GOp::Eq => left == right,
        GOp::Ne => left != right,
    })
}

/// One query's answer, rendered so that the engine's and the oracle's can be
/// compared: the value and its kind, the needed cells, or the message.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Value(String),
    Unknown(Vec<String>),
    Fails(String),
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(value) => f.write_str(value),
            Self::Unknown(cells) => write!(f, "unknown, needs [{}]", cells.join(", ")),
            Self::Fails(message) => write!(f, "fails: {message}"),
        }
    }
}

fn render_value(value: &V) -> String {
    match value {
        V::Bool(value) => format!("bool {value}"),
        V::Int(value) => format!("integer {value}"),
        V::Dec(value) => format!("decimal {value}"),
    }
}

fn cell_label((slot, entity, period): Cell) -> String {
    format!(
        "{}@{}@{}",
        slot_name(slot),
        entity_name(entity),
        PERIOD_LABELS[period]
    )
}

fn render_out<T>(out: Out<T>, value: impl Fn(T) -> String) -> Answer {
    match out {
        Out::Val(found) => Answer::Value(value(found)),
        Out::Unknown(cells) => Answer::Unknown(cells.into_iter().map(cell_label).collect()),
        Out::Fail(message) => Answer::Fails(message),
    }
}

fn outcome_label(holds: bool) -> String {
    if holds { "holds" } else { "not_holds" }.to_string()
}

/// Evaluate `query` with the oracle, reading cells through `read`.
fn oracle(prog: &Prog, mode: Mode, read: Read<'_>, query: Q) -> Result<(Answer, Events), Cell> {
    let mut oracle = Oracle {
        prog,
        mode,
        entity: query.entity,
        period: query.period,
        read,
        events: Events::default(),
    };
    let answer = match &prog.rules[query.rule] {
        Body::Scalar(expr) => render_out(oracle.scalar(expr)?, |value| render_value(&value)),
        Body::Judgment(expr) => render_out(oracle.judgment(expr)?, outcome_label),
    };
    Ok((answer, oracle.events))
}

/// The oracle's answer on a dataset.
fn oracle_on(prog: &Prog, mode: Mode, records: &[Rec], query: Q) -> (Answer, Events) {
    oracle(prog, mode, &|cell| Ok(resolved(records, cell)), query)
        .expect("a dataset read never branches")
}

// ===========================================================================
// Running the engine
// ===========================================================================

fn need_label(need: &Need) -> String {
    match need {
        Need::Input {
            name,
            entity_id,
            period,
        } => {
            let label = periods()
                .iter()
                .position(|candidate| candidate == period)
                .map_or("other", |index| PERIOD_LABELS[index]);
            format!("{name}@{entity_id}@{label}")
        }
    }
}

fn engine_scalar(value: &ScalarValue) -> String {
    match value {
        ScalarValue::Bool(value) => format!("bool {value}"),
        ScalarValue::Integer(value) => format!("integer {value}"),
        ScalarValue::Decimal(value) => format!("decimal {value}"),
        other => format!("unexpected {other:?}"),
    }
}

fn engine_outcome(outcome: JudgmentOutcome) -> String {
    match outcome {
        Holds => "holds".to_string(),
        NotHolds => "not_holds".to_string(),
        Undetermined => "undetermined without needs".to_string(),
    }
}

fn engine_answer<T>(
    result: Result<Determination<T>, EvalError>,
    value: impl Fn(T) -> String,
) -> Answer {
    match result {
        Ok(Determined(found)) => Answer::Value(value(found)),
        Ok(Indeterminate(needs)) => Answer::Unknown(needs.iter().map(need_label).collect()),
        Err(error) => Answer::Fails(error.to_string()),
    }
}

/// The answers of `queries` on one engine, in order, and everything it
/// cached: values, judgments and needs.
#[derive(Clone, Debug, PartialEq)]
struct Session {
    answers: Vec<Answer>,
    caches: Vec<String>,
}

/// Run `queries` on one engine: with the option (`three_valued`) or
/// `Engine::new`.
fn session(
    prog: &Prog,
    program: &Program,
    data: &DataSet,
    queries: &[Q],
    three_valued: bool,
) -> Session {
    let mut engine = if three_valued {
        self::three_valued(program, data)
    } else {
        Engine::new(program, data)
    };
    let answers = queries
        .iter()
        .map(|query| {
            let rule = rule_name(query.rule);
            let entity = entity_name(query.entity);
            let period = &periods()[query.period];
            match (&prog.rules[query.rule], three_valued) {
                (Body::Scalar(_), true) => engine_answer(
                    engine.scalar_determination(&rule, &entity, period),
                    |value| engine_scalar(&value),
                ),
                (Body::Judgment(_), true) => engine_answer(
                    engine.judgment_determination(&rule, &entity, period),
                    engine_outcome,
                ),
                (Body::Scalar(_), false) => engine_answer(
                    engine
                        .evaluate_scalar(&rule, &entity, period)
                        .map(Determined),
                    |value| engine_scalar(&value),
                ),
                (Body::Judgment(_), false) => engine_answer(
                    engine
                        .evaluate_judgment(&rule, &entity, period)
                        .map(Determined),
                    engine_outcome,
                ),
            }
        })
        .collect();
    let mut caches = Vec::new();
    for rule in 0..prog.rules.len() {
        for entity in 0..ENTITIES {
            for (index, period) in periods().iter().enumerate() {
                let (name, id) = (rule_name(rule), entity_name(entity));
                let key = format!("{name}@{id}@{}", PERIOD_LABELS[index]);
                if let Some(value) = engine.cached_scalar(&name, &id, period) {
                    caches.push(format!("{key} = {}", engine_scalar(&value)));
                }
                if let Some(outcome) = engine.cached_judgment(&name, &id, period) {
                    caches.push(format!("{key} = {}", engine_outcome(outcome)));
                }
                if let Some(needs) = engine.needs(&name, &id, period) {
                    let needs: Vec<String> = needs.iter().map(need_label).collect();
                    caches.push(format!("{key} needs [{}]", needs.join(", ")));
                }
            }
        }
    }
    Session { answers, caches }
}

// ===========================================================================
// Rendering a counterexample
// ===========================================================================

fn render_scalar(expr: &S) -> String {
    match expr {
        S::Lit(V::Bool(value)) => value.to_string(),
        S::Lit(V::Int(value)) => value.to_string(),
        S::Lit(V::Dec(value)) => value.to_string(),
        S::Input(slot) => slot_name(*slot),
        S::InputOr(slot, default) => format!("input_or_else({}, {default})", slot_name(*slot)),
        S::Rule(index) => rule_name(*index),
        S::Add(items) => format!(
            "({})",
            items
                .iter()
                .map(render_scalar)
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        S::Sub(left, right) => format!("({} - {})", render_scalar(left), render_scalar(right)),
        S::Mul(left, right) => format!("({} * {})", render_scalar(left), render_scalar(right)),
        S::Div(left, right) => format!("({} / {})", render_scalar(left), render_scalar(right)),
    }
}

fn render_judgment(expr: &J) -> String {
    let op = |op: GOp| match op {
        GOp::Lt => "<",
        GOp::Lte => "<=",
        GOp::Gt => ">",
        GOp::Gte => ">=",
        GOp::Eq => "==",
        GOp::Ne => "!=",
    };
    match expr {
        J::Cmp(left, comparison, right) => format!(
            "({} {} {})",
            render_scalar(left),
            op(*comparison),
            render_scalar(right)
        ),
        J::Rule(index) => rule_name(*index),
        J::And(items) => format!(
            "and({})",
            items
                .iter()
                .map(render_judgment)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        J::Or(items) => format!(
            "or({})",
            items
                .iter()
                .map(render_judgment)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        J::Not(item) => format!("not({})", render_judgment(item)),
    }
}

fn render_case(prog: &Prog, records: &[Rec], queries: &[Q]) -> String {
    let mut text = String::from("program:");
    for (slot, slot_type) in prog.slot_types.iter().enumerate() {
        let _ = write!(text, "\n  input {}: {slot_type:?}", slot_name(slot));
    }
    for (index, body) in prog.rules.iter().enumerate() {
        let formula = match body {
            Body::Scalar(expr) => format!("scalar {}", render_scalar(expr)),
            Body::Judgment(expr) => format!("judgment {}", render_judgment(expr)),
        };
        let _ = write!(text, "\n  {} := {formula}", rule_name(index));
    }
    text.push_str("\nrecords:");
    for record in records {
        let spell = spell(record.spell);
        let _ = write!(
            text,
            "\n  {} {} {}..{} = {}",
            slot_name(record.slot),
            entity_name(record.entity),
            spell.start,
            spell.end,
            render_value(&record.value)
        );
    }
    text.push_str("\nqueries:");
    for query in queries {
        let _ = write!(
            text,
            "\n  {} for {} in {}",
            rule_name(query.rule),
            entity_name(query.entity),
            PERIOD_LABELS[query.period]
        );
    }
    text
}

// ===========================================================================
// Runner
// ===========================================================================

fn case_count() -> u32 {
    std::env::var("AXIOM_THREE_VALUED_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CASES)
}

fn seed_for(property: u64) -> [u8; 32] {
    let base = std::env::var("AXIOM_THREE_VALUED_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SEED);
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&base.to_le_bytes());
    seed[8..16].copy_from_slice(&property.to_le_bytes());
    seed[16..24].copy_from_slice(&base.rotate_left(17).to_le_bytes());
    seed[24..].copy_from_slice(&(property ^ 0x9e37_79b9_7f4a_7c15).to_le_bytes());
    seed
}

/// What a property's passing run exercised, for its non-vacuity guards.
#[derive(Debug, Default)]
struct Stats {
    cases: u32,
    /// Cases with a query whose result is a value although a cell its rule
    /// can read is missing.
    determinate_with_missing: u32,
    /// Cases with a query whose result is unknown.
    unknown: u32,
    /// Cases where a decisive item decided after an unknown item.
    absorbed_unknown: u32,
    /// Cases where a decisive item decided after a failing item.
    absorbed_failure: u32,
    /// Cases with a query that fails without the option and is a value with
    /// it.
    off_fails_on_value: u32,
    /// Cases with a query that fails in both modes.
    off_fails_on_fails: u32,
    /// Queries whose partial result was checked against two or more total
    /// completions.
    completion_checks: u32,
    /// Total completions evaluated.
    completions: u64,
    /// Deletions that were withdrawals, and checked.
    withdrawals: u32,
    /// Withdrawals after which a value became unknown.
    withdrawals_to_unknown: u32,
    /// Deletions that were not withdrawals, and so were not checked.
    rejected_deletions: u32,
    /// Supplies checked against a value.
    supplies: u32,
    /// Cases whose program reorders under the transformation.
    reordered: u32,
    /// Cases where deferral at threshold 1 did more work than none.
    deferred: u32,
}

impl Stats {
    fn summary(&self, name: &str) -> String {
        format!(
            "{name}: {} cases; determinate with missing {}; unknown {}; absorbed unknown {} / failure {}; off fails, on value {} / on fails {}; completion checks {} over {} completions; withdrawals {} (to unknown {}), rejected deletions {}; supplies {}; reordered {}; deferred {}",
            self.cases,
            self.determinate_with_missing,
            self.unknown,
            self.absorbed_unknown,
            self.absorbed_failure,
            self.off_fails_on_value,
            self.off_fails_on_fails,
            self.completion_checks,
            self.completions,
            self.withdrawals,
            self.withdrawals_to_unknown,
            self.rejected_deletions,
            self.supplies,
            self.reordered,
            self.deferred
        )
    }

    /// Assert that `count` is at least `share` of the cases. Below 100 cases
    /// the shares are too noisy to hold anyone to.
    fn at_least(&self, name: &str, count: u32, share: f64, what: &str) {
        if self.cases < 100 {
            return;
        }
        assert!(
            f64::from(count) >= share * f64::from(self.cases),
            "{name}: too few cases {what} ({count} of {}, need {:.0}%): {}",
            self.cases,
            share * 100.0,
            self.summary(name)
        );
    }
}

/// A generated case, resolved and lowered.
struct Prepared {
    prog: Prog,
    records: Vec<Rec>,
    program: Program,
    data: DataSet,
    queries: Vec<Q>,
    picks: Vec<u8>,
}

impl Prepared {
    fn new(case: &GCase, complete: bool) -> Self {
        let prog = resolve(case);
        let records = records(case, complete);
        let program = lower(&prog);
        let data = lower_records(&records);
        let queries = queries(case, &prog);
        Self {
            prog,
            records,
            program,
            data,
            queries,
            picks: case.picks.clone(),
        }
    }

    fn render(&self) -> String {
        render_case(&self.prog, &self.records, &self.queries)
    }
}

/// Run `check` over generated cases with a fixed seed, on a large stack (the
/// thresholds compared include no deferral at all). On failure proptest
/// shrinks the case, and the panic carries the minimal one: `check`'s report,
/// then the program, the records and the queries.
fn run_property(
    name: &'static str,
    property: u64,
    complete: bool,
    check: impl Fn(&Prepared, &mut Stats) -> Result<(), String> + Send,
) -> Stats {
    on_stack(256 * MIB, move || {
        let stats = RefCell::new(Stats::default());
        let config = Config {
            cases: case_count(),
            failure_persistence: None,
            max_shrink_iters: 10_000,
            ..Config::default()
        };
        let mut runner = TestRunner::new_with_rng(
            config,
            TestRng::from_seed(RngAlgorithm::ChaCha, &seed_for(property)),
        );
        let outcome = runner.run(&case_strategy(), |case| {
            let prepared = Prepared::new(&case, complete);
            let mut stats = stats.borrow_mut();
            stats.cases += 1;
            check(&prepared, &mut stats)
                .map_err(|report| TestCaseError::fail(format!("{report}\n{}", prepared.render())))
        });
        let stats = stats.into_inner();
        eprintln!("{}", stats.summary(name));
        match outcome {
            Ok(()) => stats,
            Err(TestError::Fail(reason, _)) => {
                panic!("{name}: minimal failing case after shrinking:\n{reason}")
            }
            Err(TestError::Abort(reason)) => panic!("{name}: runner aborted: {reason}"),
        }
    })
}

fn compare_answers(what: &str, expected: &[Answer], actual: &[Answer]) -> Result<(), String> {
    if expected == actual {
        return Ok(());
    }
    let mut report = format!("{what}:");
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        let marker = if expected == actual { " " } else { "!" };
        let _ = write!(
            report,
            "\n {marker} query {index}: expected {expected}\n             got      {actual}"
        );
    }
    Err(report)
}

fn is_value(answer: &Answer) -> bool {
    matches!(answer, Answer::Value(_))
}

// ===========================================================================
// Properties
// ===========================================================================

/// Conformance. On every generated dataset, missing cells included, the
/// engine under the option returns exactly the reference's answer: the same
/// value and kind, the same needs, or a failure with the same message. This
/// is what catches an engine that is safe but says "indeterminate" where
/// section 5 gives a value. Without the option it returns exactly today's
/// answer, `MissingInput` and defaults included. The non-vacuity guards for
/// the whole generator are asserted here.
#[test]
fn the_engine_conforms_to_the_reference_and_to_todays_rules() {
    let name = "conformance";
    let stats = run_property(name, 1, false, |case, stats| {
        let reference: Vec<(Answer, Events)> = case
            .queries
            .iter()
            .map(|query| oracle_on(&case.prog, Mode::Reference, &case.records, *query))
            .collect();
        let today: Vec<Answer> = case
            .queries
            .iter()
            .map(|query| oracle_on(&case.prog, Mode::Today, &case.records, *query).0)
            .collect();
        let on = session(&case.prog, &case.program, &case.data, &case.queries, true);
        let off = session(&case.prog, &case.program, &case.data, &case.queries, false);
        let expected: Vec<Answer> = reference.iter().map(|(answer, _)| answer.clone()).collect();
        compare_answers("the option against the reference", &expected, &on.answers)?;
        compare_answers("no option against today's rules", &today, &off.answers)?;

        let mut flags = [false; 6];
        for (index, query) in case.queries.iter().enumerate() {
            let (answer, events) = &reference[index];
            let missing = reachable_slots(&case.prog, query.rule)
                .into_iter()
                .any(|slot| resolved(&case.records, (slot, query.entity, query.period)).is_none());
            flags[0] |= missing && is_value(answer);
            flags[1] |= matches!(answer, Answer::Unknown(_));
            flags[2] |= events.absorbed_unknown;
            flags[3] |= events.absorbed_failure;
            flags[4] |= matches!(off.answers[index], Answer::Fails(_)) && is_value(answer);
            flags[5] |= matches!(off.answers[index], Answer::Fails(_))
                && matches!(answer, Answer::Fails(_));
        }
        stats.determinate_with_missing += u32::from(flags[0]);
        stats.unknown += u32::from(flags[1]);
        stats.absorbed_unknown += u32::from(flags[2]);
        stats.absorbed_failure += u32::from(flags[3]);
        stats.off_fails_on_value += u32::from(flags[4]);
        stats.off_fails_on_fails += u32::from(flags[5]);
        Ok(())
    });
    stats.at_least(
        name,
        stats.determinate_with_missing,
        0.20,
        "are determinate with a readable cell missing",
    );
    stats.at_least(name, stats.unknown, 0.20, "are unknown");
    stats.at_least(
        name,
        stats.absorbed_unknown,
        0.05,
        "absorb an unknown item in a decisive one",
    );
    stats.at_least(
        name,
        stats.absorbed_failure,
        0.02,
        "absorb a failing item in a decisive one",
    );
    stats.at_least(
        name,
        stats.off_fails_on_value,
        0.02,
        "fail without the option and answer with it",
    );
}

/// Every total completion of the query's cells, as the reference answers it:
/// each missing cell the evaluation reads is assigned every value of its
/// slot's domain, in turn, and evaluation reruns. A leaf reads no missing
/// cell, so it is the answer of every total completion that agrees with it
/// on what it read. `None` if there are more than `limit`.
fn completions(prog: &Prog, records: &[Rec], query: Q, limit: usize) -> Option<Vec<Answer>> {
    fn walk(
        prog: &Prog,
        records: &[Rec],
        query: Q,
        assigned: &mut BTreeMap<Cell, V>,
        leaves: &mut Vec<Answer>,
        limit: usize,
    ) -> bool {
        let read =
            |cell: Cell| match resolved(records, cell).or_else(|| assigned.get(&cell).cloned()) {
                Some(value) => Ok(Some(value)),
                None => Err(cell),
            };
        match oracle(prog, Mode::Reference, &read, query) {
            Ok((answer, _)) => {
                leaves.push(answer);
                leaves.len() <= limit
            }
            Err(cell) => {
                for value in domain(prog.slot_types[cell.0]) {
                    assigned.insert(cell, value);
                    let within = walk(prog, records, query, assigned, leaves, limit);
                    assigned.remove(&cell);
                    if !within {
                        return false;
                    }
                }
                true
            }
        }
    }
    let mut leaves = Vec::new();
    walk(
        prog,
        records,
        query,
        &mut BTreeMap::new(),
        &mut leaves,
        limit,
    )
    .then_some(leaves)
}

/// Soundness and failures are certain (invariants 1 and 2), for the engine's
/// result on the partial dataset and for the reference's: a value is the
/// value of every total completion, and a failure fails every one. A
/// failure with the reference's result would be a defect in section 5.
#[test]
fn every_total_completion_agrees_with_a_partial_result() {
    let name = "completions";
    let stats = run_property(name, 2, false, |case, stats| {
        let on = session(&case.prog, &case.program, &case.data, &case.queries, true);
        for (index, query) in case.queries.iter().enumerate() {
            let reference = oracle_on(&case.prog, Mode::Reference, &case.records, *query).0;
            // The bound is the domain's size to the number of slots, 3,125;
            // it is never reached, and would fail the case if it were.
            let leaves = completions(&case.prog, &case.records, *query, 4096)
                .ok_or_else(|| format!("query {index}: more than 4,096 completions"))?;
            stats.completions += leaves.len() as u64;
            if leaves.len() > 1 && !matches!(reference, Answer::Unknown(_)) {
                stats.completion_checks += 1;
            }
            for (source, partial) in [("engine", &on.answers[index]), ("reference", &reference)] {
                for leaf in &leaves {
                    let agrees = match partial {
                        Answer::Value(_) => leaf == partial,
                        Answer::Fails(_) => matches!(leaf, Answer::Fails(_)),
                        Answer::Unknown(_) => true,
                    };
                    if !agrees {
                        return Err(format!(
                            "query {index}: the {source}'s partial result is {partial}, but a total completion's is {leaf}"
                        ));
                    }
                }
            }
        }
        Ok(())
    });
    // A partial result that read a missing cell is mostly unknown, which any
    // completion satisfies; about a fifth of cases have one that is not.
    stats.at_least(
        name,
        stats.completion_checks,
        0.15,
        "check a value or failure against several completions",
    );
}

/// Remove every record that covers `cell`.
fn withdraw(records: &[Rec], cells: &[Cell]) -> Vec<Rec> {
    records
        .iter()
        .filter(|record| {
            !cells.iter().any(|(slot, entity, period)| {
                record.slot == *slot
                    && record.entity == *entity
                    && contains(record.spell, &periods()[*period])
            })
        })
        .cloned()
        .collect()
}

/// Whether `after` is `before` with facts withdrawn: every cell `after`
/// resolves, it resolves to the same value.
fn is_withdrawal(prog: &Prog, before: &[Rec], after: &[Rec]) -> bool {
    all_cells(prog)
        .into_iter()
        .all(|cell| match resolved(after, cell) {
            Some(value) => resolved(before, cell) == Some(value),
            None => true,
        })
}

/// No silent shrink (invariant 3). From a complete dataset whose reference
/// result is a value, withdraw facts: every record covering some chosen
/// cells, and also arbitrary sets of records. The second kind is a
/// withdrawal only when every cell left resolved keeps its value; deleting
/// the newer of two overlapping spells is not, and is rejected unchecked.
/// After a withdrawal the result, the engine's and the reference's, is the
/// value or unknown: never another value, never a failure.
#[test]
fn withdrawing_facts_never_changes_a_value() {
    let name = "no silent shrink";
    let stats = run_property(name, 3, true, |case, stats| {
        let original: Vec<Answer> = case
            .queries
            .iter()
            .map(|query| oracle_on(&case.prog, Mode::Reference, &case.records, *query).0)
            .collect();
        if !original.iter().any(is_value) {
            return Ok(());
        }
        let cells = all_cells(&case.prog);
        let mut deletions: Vec<(bool, Vec<Rec>)> = Vec::new();
        for round in 0..3 {
            let chosen: Vec<Cell> = cells
                .iter()
                .enumerate()
                .filter(|(index, _)| case.picks[(round * 20 + index) % case.picks.len()] % 4 == 0)
                .map(|(_, cell)| *cell)
                .collect();
            deletions.push((true, withdraw(&case.records, &chosen)));
            let kept: Vec<Rec> = case
                .records
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    case.picks[(round * 20 + 7 + index) % case.picks.len()] % 3 != 0
                })
                .map(|(_, record)| record.clone())
                .collect();
            deletions.push((false, kept));
        }
        for (by_cells, records) in deletions {
            if !is_withdrawal(&case.prog, &case.records, &records) {
                if by_cells {
                    return Err(
                        "removing every record that covers a cell changed another cell".to_string(),
                    );
                }
                stats.rejected_deletions += 1;
                continue;
            }
            stats.withdrawals += 1;
            let data = lower_records(&records);
            let on = session(&case.prog, &case.program, &data, &case.queries, true);
            for (index, query) in case.queries.iter().enumerate() {
                if !is_value(&original[index]) {
                    continue;
                }
                let reference = oracle_on(&case.prog, Mode::Reference, &records, *query).0;
                for (source, answer) in [("engine", &on.answers[index]), ("reference", &reference)]
                {
                    match answer {
                        Answer::Unknown(_) => stats.withdrawals_to_unknown += 1,
                        same if same == &original[index] => {}
                        other => {
                            return Err(format!(
                                "query {index}: the complete dataset's value is {}, but after withdrawing facts the {source}'s result is {other}\nremaining records:\n{}",
                                original[index],
                                render_case(&case.prog, &records, &case.queries)
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    });
    stats.at_least(name, stats.withdrawals, 0.50, "withdraw facts");
    stats.at_least(
        name,
        stats.withdrawals_to_unknown,
        0.10,
        "turn a value unknown by withdrawing facts",
    );
    stats.at_least(
        name,
        stats.rejected_deletions,
        0.10,
        "reject a deletion that is not a withdrawal",
    );
}

/// Monotone refinement (invariant 4). After supplying any subset of the
/// missing cells, without changing a resolved one, a value stays that value,
/// for the engine and for the reference. A supplied cell gets a record older
/// than every generated one, so it overrides nothing.
#[test]
fn supplying_facts_keeps_a_value() {
    let name = "monotone refinement";
    let stats = run_property(name, 4, false, |case, stats| {
        let on = session(&case.prog, &case.program, &case.data, &case.queries, true);
        let reference: Vec<Answer> = case
            .queries
            .iter()
            .map(|query| oracle_on(&case.prog, Mode::Reference, &case.records, *query).0)
            .collect();
        if !on.answers.iter().chain(&reference).any(is_value) {
            return Ok(());
        }
        for round in 0..3 {
            let pick = |index: usize| case.picks[(round * 21 + index) % case.picks.len()];
            let mut supplied = case.records.clone();
            let mut choice = 0;
            for slot in 0..case.prog.slot_types.len() {
                for entity in 0..ENTITIES {
                    for (period, filler) in [(1, QUARTER_FILLER), (0, JANUARY_FILLER)] {
                        choice += 1;
                        if resolved(&case.records, (slot, entity, period)).is_some()
                            || pick(choice) % 2 == 0
                        {
                            continue;
                        }
                        let values = domain(case.prog.slot_types[slot]);
                        supplied.push(Rec {
                            slot,
                            entity,
                            spell: filler,
                            value: values[usize::from(pick(choice + 40)) % values.len()].clone(),
                        });
                    }
                }
            }
            if !is_withdrawal(&case.prog, &supplied, &case.records) {
                return Err("supplying a missing cell changed a resolved one".to_string());
            }
            let data = lower_records(&supplied);
            let refined = session(&case.prog, &case.program, &data, &case.queries, true);
            for (index, query) in case.queries.iter().enumerate() {
                let refined_reference = oracle_on(&case.prog, Mode::Reference, &supplied, *query).0;
                for (source, before, after) in [
                    ("engine", &on.answers[index], &refined.answers[index]),
                    ("reference", &reference[index], &refined_reference),
                ] {
                    if is_value(before) {
                        stats.supplies += 1;
                        if after != before {
                            return Err(format!(
                                "query {index}: the {source}'s value is {before}, but after supplying facts it is {after}\nrecords after:\n{}",
                                render_case(&case.prog, &supplied, &case.queries)
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    });
    stats.at_least(
        name,
        stats.supplies,
        1.0,
        "check a value against supplied facts",
    );
}

/// Agreement with today's results (invariant 6). On a dataset with nothing
/// missing: where `Engine::new` returns a value, the option returns the same
/// value, and when every query does, the same cached values for every rule.
/// Where it fails, the option fails or returns a value (a decisive item
/// absorbing a failing one), never unknown; both are counted.
#[test]
fn the_option_agrees_with_todays_results_on_complete_data() {
    let name = "agreement with today";
    let stats = run_property(name, 5, true, |case, stats| {
        let off = session(&case.prog, &case.program, &case.data, &case.queries, false);
        let on = session(&case.prog, &case.program, &case.data, &case.queries, true);
        let (mut on_value, mut on_fails) = (false, false);
        for (index, (today, answer)) in off.answers.iter().zip(&on.answers).enumerate() {
            match (today, answer) {
                (Answer::Value(_), _) if answer == today => {}
                (Answer::Fails(_), Answer::Value(_)) => on_value = true,
                (Answer::Fails(_), Answer::Fails(_)) => on_fails = true,
                _ => {
                    return Err(format!(
                        "query {index}: without the option {today}, with it {answer}"
                    ));
                }
            }
        }
        if off.answers.iter().all(is_value) && off.caches != on.caches {
            return Err(format!(
                "the caches differ:\n  without the option {:#?}\n  with it {:#?}",
                off.caches, on.caches
            ));
        }
        stats.off_fails_on_value += u32::from(on_value);
        stats.off_fails_on_fails += u32::from(on_fails);
        Ok(())
    });
    stats.at_least(
        name,
        stats.off_fails_on_value,
        0.02,
        "fail without the option and answer with it",
    );
}

/// Needs are real (invariant 7): every need names a cell no record covers,
/// and an unknown result has at least one. Checked against the records
/// directly, not through the oracle's resolution.
#[test]
fn needs_name_cells_no_record_covers() {
    let name = "needs are real";
    let stats = run_property(name, 6, false, |case, stats| {
        let mut engine = three_valued(&case.program, &case.data);
        let mut saw_unknown = false;
        for query in &case.queries {
            let rule = rule_name(query.rule);
            let entity = entity_name(query.entity);
            let period = &periods()[query.period];
            let needs = match &case.prog.rules[query.rule] {
                Body::Scalar(_) => match engine.scalar_determination(&rule, &entity, period) {
                    Ok(Indeterminate(needs)) => Some(needs),
                    _ => None,
                },
                Body::Judgment(_) => match engine.judgment_determination(&rule, &entity, period) {
                    Ok(Indeterminate(needs)) => Some(needs),
                    _ => None,
                },
            };
            let Some(needs) = needs else { continue };
            saw_unknown = true;
            if needs.iter().next().is_none() {
                return Err(format!("{rule} for {entity} is unknown with no needs"));
            }
            for need in needs.iter() {
                let Need::Input {
                    name,
                    entity_id,
                    period,
                } = need;
                if let Some(record) = case.data.inputs.iter().find(|record| {
                    &record.name == name
                        && &record.entity_id == entity_id
                        && record.interval.contains_period(period)
                }) {
                    return Err(format!(
                        "{rule} for {entity} needs {need}, which {record:?} covers"
                    ));
                }
            }
        }
        stats.unknown += u32::from(saw_unknown);
        Ok(())
    });
    stats.at_least(name, stats.unknown, 0.20, "are unknown");
}

/// A permutation of the records chosen by the case.
fn shuffled(records: &[Rec], picks: &[u8]) -> Vec<Rec> {
    let mut keyed: Vec<(u8, usize, Rec)> = records
        .iter()
        .enumerate()
        .map(|(index, record)| (picks[index % picks.len()], index, record.clone()))
        .collect();
    keyed.sort_by_key(|(key, index, _)| (*key, std::cmp::Reverse(*index)));
    keyed.into_iter().map(|(_, _, record)| record).collect()
}

/// Order invariance (invariant 8). Reversing, or rotating, the items of
/// every `and`/`or`, and shuffling the input records, changes no answer: no
/// value, no unknown, no need and its place in the order, no failure's
/// message. Checked on the engine, and on the reference itself.
#[test]
fn results_do_not_depend_on_item_or_record_order() {
    let name = "order invariance";
    let stats = run_property(name, 7, false, |case, stats| {
        let original = session(&case.prog, &case.program, &case.data, &case.queries, true);
        let reference: Vec<Answer> = case
            .queries
            .iter()
            .map(|query| oracle_on(&case.prog, Mode::Reference, &case.records, *query).0)
            .collect();
        let mut reordered = false;
        for (rotate, reverse_records) in [(false, false), (true, true), (false, true)] {
            let prog = reorder(&case.prog, rotate);
            let program = lower(&prog);
            let mut records = shuffled(&case.records, &case.picks);
            if reverse_records {
                records.reverse();
            }
            let data = lower_records(&records);
            reordered |= format!("{:?}", prog.rules) != format!("{:?}", case.prog.rules);
            let variant = session(&prog, &program, &data, &case.queries, true);
            let what = format!(
                "the engine with items {} and records shuffled",
                if rotate { "rotated" } else { "reversed" }
            );
            compare_answers(&what, &original.answers, &variant.answers)?;
            let variant_reference: Vec<Answer> = case
                .queries
                .iter()
                .map(|query| oracle_on(&prog, Mode::Reference, &records, *query).0)
                .collect();
            compare_answers("the reference, reordered", &reference, &variant_reference)?;
        }
        stats.reordered += u32::from(reordered);
        Ok(())
    });
    stats.at_least(name, stats.reordered, 0.50, "reorder an `and`/`or`");
}

/// Deferral transparency (invariant 10). Every answer, and everything the
/// engine caches (values, judgments, needs), is the same with no deferral, at
/// thresholds 1 and 2, and at the build's default.
#[test]
fn results_do_not_depend_on_the_deferral_threshold() {
    let name = "deferral transparency";
    let stats = run_property(name, 8, false, |case, stats| {
        let run = |threshold: Option<usize>| {
            at_threshold(threshold, || {
                axiom_rules_engine::depth::count_visits(|| {
                    session(&case.prog, &case.program, &case.data, &case.queries, true)
                })
            })
        };
        let (undeferred, recursion_visits) = run(Some(usize::MAX));
        for threshold in [Some(1), Some(2), None] {
            let (deferred, visits) = run(threshold);
            if threshold == Some(1) && visits > recursion_visits {
                stats.deferred += 1;
            }
            if deferred != undeferred {
                return Err(format!(
                    "threshold {threshold:?} differs from no deferral:\n  no deferral {undeferred:#?}\n  deferred {deferred:#?}"
                ));
            }
        }
        Ok(())
    });
    // Only a query whose rule references another rule can defer: about 40%
    // of cases.
    stats.at_least(name, stats.deferred, 0.30, "defer at threshold 1");
}
