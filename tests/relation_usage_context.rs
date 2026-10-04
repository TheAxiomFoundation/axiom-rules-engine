//! Relation membership typing follows the evaluator's relation context (#203).
//!
//! `relation_member` tests the two IDs a derived relation binds while its
//! predicate runs. Explain keeps that binding through comparisons, `if`
//! conditions and branches, and arithmetic, and drops it inside a nested
//! aggregation's `where` clause, where the test fails at run time. It never
//! evaluates a `match` fallback's pattern labels, and it refuses an
//! over-periods reduction outside lifetime execution without evaluating it.
//! Relation entity typing is mandatory (`src/relation_typing.rs`), and the
//! check must scope the binding the same way: a membership test explain
//! reaches must read the slots whose declared kinds are the IDs the derived
//! relation binds, and a test it cannot reach is held to nothing but its
//! relation's declaration and arity. Dataset binding expects the declared
//! kinds, whatever the program's uses. (#203 and #205 inferred the expected
//! tuple order from those uses, and compilation only warned when a use
//! reversed the declaration.)
//!
//! The fixture is the reproduction from #203: `head` and `member` are both
//! declared `[Person, Household]`, and the derived relation `heads` keeps the
//! members of a household who head it. `p1` and `p2` belong to `h1`; only `p1`
//! heads it, so `n` (the size of `heads` for `h1`) is 1.
//!
//! Invariants under test, for every predicate shape:
//!
//! * A membership test explain can reach: slots (1, 0) read `head` as
//!   declared, so the program compiles cleanly, the declared-order tuple binds
//!   without diagnostics and is the one explain consumes (`n == 1`). Slots
//!   (0, 1) would read the household from `head`'s person slot and the person
//!   from its household slot, so compilation and every request fail with
//!   `relation_membership_slot_entity_mismatch`.
//! * A membership test explain cannot reach implies no orientation: the
//!   program compiles with either slots, and binding expects the declared
//!   order.
//! * Strict binding rejects exactly the datasets default binding warns about,
//!   and a request is refused exactly when compilation is, for the same
//!   violations.

use axiom_rules_engine::api::{
    ApiError, ExecutionMode, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::relation_typing::{RelationTypingCode, RelationTypingReport};
use axiom_rules_engine::spec::{
    DatasetBindingOptions, DatasetSpec, ProgramSpec, RelationRecordSpec, ScalarValueSpec,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};

const OUT_OF_CONTEXT_ERROR: &str =
    "relation predicate `head` can only be evaluated inside a derived relation";
/// Explain's error at a `match` fallback, whose patterns it never evaluates.
const NO_MATCHING_ARM_ERROR: &str = "no `match` arm in";
/// Explain's error at an over-periods reduction, which it never evaluates.
const OVER_PERIODS_ERROR: &str = "is valid only under lifetime execution";
const DECLARED_ORDER: [&str; 2] = ["p1", "h1"];
const REVERSED_ORDER: [&str; 2] = ["h1", "p1"];

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/execution/relation-member-nested-if.json"
    ))
    .expect("#203 reproduction parses")
}

/// The fixture with `heads`' predicate replaced.
fn with_predicate(predicate: Value) -> Value {
    let mut request = fixture();
    request["program"]["relations"][2]["derivation"]["predicate"] = predicate;
    request
}

/// Replace the fixture's single `head` tuple.
fn with_head_tuple(mut request: Value, tuple: [&str; 2]) -> Value {
    let head = request["dataset"]["relations"]
        .as_array_mut()
        .expect("dataset relations")
        .iter_mut()
        .find(|record| record["name"] == "head")
        .expect("fixture has a head tuple");
    head["tuple"] = json!(tuple);
    request
}

fn member(current_slot: usize, related_slot: usize) -> Value {
    json!({
        "kind": "relation_member",
        "relation": "head",
        "current_slot": current_slot,
        "related_slot": related_slot
    })
}

fn int(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({"kind": "comparison", "left": left, "op": op, "right": right})
}

fn constant(holds: bool) -> Value {
    compare(int(1), if holds { "eq" } else { "ne" }, int(1))
}

/// 1 when `condition` holds, else 0.
fn indicator(condition: Value) -> Value {
    json!({"kind": "if", "condition": condition, "then_expr": int(1), "else_expr": int(0)})
}

/// A comparison whose left side is a `match` fallback with `judgment` (as an
/// indicator) for its pattern label. Explain fails at the fallback without
/// evaluating the label.
fn match_fallback(judgment: Value) -> Value {
    compare(
        json!({"kind": "no_match", "subject": int(1), "patterns": [indicator(judgment)]}),
        "eq",
        int(1),
    )
}

/// A comparison over an over-periods sum of `judgment` (as an indicator).
/// Explain refuses the reduction outside lifetime execution without
/// evaluating it.
fn over_periods(judgment: Value) -> Value {
    compare(
        json!({"kind": "over_periods", "over": "sum", "value": indicator(judgment)}),
        "gt",
        int(0),
    )
}

/// `member` read from a person to their households, or from a household to
/// its members, as (current_slot, related_slot).
fn member_slots(from_person: bool) -> (usize, usize) {
    if from_person { (0, 1) } else { (1, 0) }
}

/// `count_related` over `member`, keeping related IDs where `clause` holds.
/// The clause runs on each related ID with no relation context.
fn count_where(from_person: bool, clause: Value) -> Value {
    let (current_slot, related_slot) = member_slots(from_person);
    json!({
        "kind": "count_related",
        "relation": "member",
        "current_slot": current_slot,
        "related_slot": related_slot,
        "where": clause
    })
}

/// `sum_related` of the related IDs' positive input where `clause` holds.
fn sum_where(from_person: bool, clause: Value) -> Value {
    let (current_slot, related_slot) = member_slots(from_person);
    let value = if from_person { "hh_size" } else { "p_age" };
    json!({
        "kind": "sum_related",
        "relation": "member",
        "current_slot": current_slot,
        "related_slot": related_slot,
        "value": {"kind": "input", "name": value},
        "where": clause
    })
}

/// A relation typing violation as (code, relation, citing).
type Violation = (RelationTypingCode, String, String);

fn violations(report: &RelationTypingReport) -> Vec<Violation> {
    report
        .violations
        .iter()
        .map(|violation| {
            (
                violation.code,
                violation.relation.clone(),
                violation.citing.clone(),
            )
        })
        .collect()
}

/// The violations of a membership test in `heads`' predicate that reads
/// `head` at slots (0, 1): one per slot.
fn membership_mismatches() -> Vec<Violation> {
    let mismatch = (
        RelationTypingCode::MembershipSlotEntityMismatch,
        "head".to_string(),
        "heads".to_string(),
    );
    vec![mismatch.clone(), mismatch]
}

/// What each consumer concludes about one request.
#[derive(Debug)]
struct Observed {
    /// `n` for `h1` in explain, or the explain error.
    explain: Result<i64, String>,
    /// The relation typing violations that refuse the program; empty when it
    /// compiles.
    typing: Vec<Violation>,
    /// Default dataset binding diagnostics as (relation, slot, expected, actual).
    binding: Vec<(String, usize, String, String)>,
}

fn observe(request: &Value) -> Observed {
    let program: ProgramSpec =
        serde_json::from_value(request["program"].clone()).expect("program parses");
    let dataset: DatasetSpec =
        serde_json::from_value(request["dataset"].clone()).expect("dataset parses");
    // Binding reads the declarations, so a program compilation refuses still
    // binds against them.
    let (typing, runtime) = match CompiledProgramArtifact::compile(program.clone()) {
        Ok(artifact) => {
            assert!(
                artifact.diagnostics.is_empty(),
                "the program compiles cleanly: {:?}",
                artifact.diagnostics
            );
            let runtime = artifact
                .program
                .to_program()
                .expect("runtime program builds");
            (Vec::new(), runtime)
        }
        Err(CompileError::RelationTyping { report, .. }) => (
            violations(&report),
            program.to_program().expect("runtime program builds"),
        ),
        Err(error) => panic!("compilation fails only for relation typing: {error}"),
    };
    let binding = dataset
        .to_dataset_for_program_with_options(&runtime, DatasetBindingOptions::default())
        .expect("default binding only warns")
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.relation,
                diagnostic.slot,
                diagnostic.expected_entity,
                diagnostic.actual_entity,
            )
        })
        .collect::<Vec<_>>();
    let strict =
        dataset.to_dataset_for_program_with_options(&runtime, DatasetBindingOptions::strict());
    assert_eq!(
        strict.is_ok(),
        binding.is_empty(),
        "strict binding must reject exactly the datasets default binding warns about: {binding:?}"
    );
    let explain = run(request, ExecutionMode::Explain);
    let refused = match &explain {
        Err(ApiError::RelationTyping(report)) => violations(report),
        _ => Vec::new(),
    };
    assert_eq!(
        refused, typing,
        "a request is refused exactly when compilation is: {explain:?}"
    );
    Observed {
        explain: explain.map_err(|error| error.to_string()),
        typing,
        binding,
    }
}

/// `n` for `h1` in `mode`, or the request's error.
fn run(request: &Value, mode: ExecutionMode) -> Result<i64, ApiError> {
    let mut request: ExecutionRequest =
        serde_json::from_value(request.clone()).expect("request parses");
    request.mode = mode;
    let response = execute_request(request)?;
    let OutputValue::Scalar {
        value: ScalarValueSpec::Integer { value },
        ..
    } = &response.results[0].outputs["n"]
    else {
        panic!("n is an integer count");
    };
    Ok(*value)
}

fn count(request: &Value, mode: ExecutionMode) -> Result<i64, String> {
    run(request, mode).map_err(|error| error.to_string())
}

/// Both declared-order kind mismatches for a reversed `head` tuple.
fn reversed_head_diagnostics() -> Vec<(String, usize, String, String)> {
    let expected = ["Person", "Household"];
    let actual = [expected[1], expected[0]];
    (0..2)
        .map(|slot| {
            (
                "head".to_string(),
                slot,
                expected[slot].to_string(),
                actual[slot].to_string(),
            )
        })
        .collect()
}

#[test]
fn issue_203_nested_membership_binds_strictly_and_compiles_cleanly() {
    let request = fixture();
    let observed = observe(&request);
    assert!(observed.typing.is_empty(), "{:?}", observed.typing);
    assert!(
        observed.binding.is_empty(),
        "the correctly oriented dataset must bind strictly: {:?}",
        observed.binding
    );
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        assert_eq!(count(&request, mode.clone()), Ok(1), "{mode:?}");
    }
}

#[test]
fn issue_203_nested_membership_still_diagnoses_the_reversed_tuple() {
    let observed = observe(&with_head_tuple(fixture(), REVERSED_ORDER));
    assert_eq!(observed.explain, Ok(0), "explain finds no head in [h1, p1]");
    assert_eq!(observed.binding, reversed_head_diagnostics());
}

/// Predicates that reach `head` through context-keeping nodes only.
fn context_keeping_wrappers(head: Value) -> Vec<(&'static str, Value)> {
    vec![
        ("direct", head.clone()),
        (
            "if condition (#203)",
            compare(indicator(head.clone()), "eq", int(1)),
        ),
        (
            "arithmetic operand",
            compare(
                json!({"kind": "add", "items": [indicator(head.clone()), int(0)]}),
                "eq",
                int(1),
            ),
        ),
        (
            "if inside an if branch",
            compare(
                json!({
                    "kind": "if",
                    "condition": constant(true),
                    "then_expr": indicator(json!({"kind": "not", "item": {"kind": "not", "item": head.clone()}})),
                    "else_expr": int(0)
                }),
                "gt",
                int(0),
            ),
        ),
        (
            "and/or items",
            json!({"kind": "or", "items": [constant(false), {"kind": "and", "items": [constant(true), head.clone()]}]}),
        ),
        (
            "max, min, ceil, floor, mul, div, sub",
            compare(
                json!({"kind": "max", "items": [int(0), {"kind": "min", "items": [int(1), {
                    "kind": "ceil", "value": {"kind": "floor", "value": {
                        "kind": "sub",
                        "left": {"kind": "div", "left": {"kind": "mul", "left": indicator(head), "right": int(1)}, "right": int(1)},
                        "right": int(0)
                    }}
                }]}]}),
                "ne",
                int(0),
            ),
        ),
    ]
}

#[test]
fn membership_keeps_its_orientation_through_every_context_keeping_wrapper() {
    assert_eq!(
        context_keeping_wrappers(member(1, 0))[1].1,
        fixture()["program"]["relations"][2]["derivation"]["predicate"],
        "the second wrapper is the #203 reproduction"
    );
    // Slots (1, 0) consume `head` in its declared order. Slots (0, 1) consume
    // the reverse, which every consumer must refuse, so a wrapper that lost
    // the context (and with it the check) fails here too.
    for (name, predicate) in context_keeping_wrappers(member(1, 0)) {
        let request = with_predicate(predicate);
        let observed = observe(&with_head_tuple(request.clone(), DECLARED_ORDER));
        assert!(observed.typing.is_empty(), "{name}: {:?}", observed.typing);
        assert!(
            observed.binding.is_empty(),
            "{name}: the declared order must bind strictly: {:?}",
            observed.binding
        );
        for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
            assert_eq!(
                count(
                    &with_head_tuple(request.clone(), DECLARED_ORDER),
                    mode.clone()
                ),
                Ok(1),
                "{name}, {mode:?}"
            );
        }
        let observed = observe(&with_head_tuple(request, REVERSED_ORDER));
        assert_eq!(observed.explain, Ok(0), "{name}");
        assert_eq!(observed.binding, reversed_head_diagnostics(), "{name}");
    }
    for (name, predicate) in context_keeping_wrappers(member(0, 1)) {
        let request = with_predicate(predicate);
        for tuple in [DECLARED_ORDER, REVERSED_ORDER] {
            let request = with_head_tuple(request.clone(), tuple);
            let observed = observe(&request);
            assert_eq!(observed.typing, membership_mismatches(), "{name} {tuple:?}");
            assert_eq!(
                count(&request, ExecutionMode::Fast),
                observed.explain,
                "{name} {tuple:?}: fast is refused as explain is"
            );
            // The declaration, not the refused use, decides the tuple order.
            assert_eq!(
                observed.binding.is_empty(),
                tuple == DECLARED_ORDER,
                "{name} {tuple:?}: {:?}",
                observed.binding
            );
        }
    }
}

#[test]
fn membership_explain_cannot_reach_implies_no_orientation() {
    // Each site tests `head` where explain never evaluates it with a derived
    // relation's IDs bound: a rule (no derived relation binds IDs), a nested
    // aggregation's `where` clause (the binding is dropped), a `match`
    // fallback's pattern label (never evaluated) or an over-periods reduction
    // (refused outside lifetime execution). The slots are chosen so that
    // treating the site's entity as the current kind (the old behaviour)
    // inferred `[Household, Person]`, the reverse of the declaration, and so
    // that, in `heads`' predicate, they contradict the IDs it binds.
    let shapes = [
        (
            "rule judgment",
            json!({
                "name": "headed",
                "entity": "Household",
                "dtype": "judgment",
                "semantics": "judgment",
                "expr": member(0, 1)
            }),
            None,
            OUT_OF_CONTEXT_ERROR,
        ),
        (
            "rule if condition",
            json!({
                "name": "headed",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": indicator(member(0, 1))
            }),
            None,
            OUT_OF_CONTEXT_ERROR,
        ),
        (
            "rule where clause",
            json!({
                "name": "headed",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": member(1, 0)
                }
            }),
            None,
            OUT_OF_CONTEXT_ERROR,
        ),
        (
            "where clause of an aggregation inside a derived relation's predicate",
            Value::Null,
            Some(compare(count_where(true, member(0, 1)), "gt", int(0))),
            OUT_OF_CONTEXT_ERROR,
        ),
        (
            "match fallback pattern inside a derived relation's predicate",
            Value::Null,
            Some(match_fallback(member(0, 1))),
            NO_MATCHING_ARM_ERROR,
        ),
        (
            "over-periods reduction inside a derived relation's predicate",
            Value::Null,
            Some(over_periods(member(0, 1))),
            OVER_PERIODS_ERROR,
        ),
    ];
    for (name, rule, predicate, explain_error) in shapes {
        // Without the site, nothing else uses `head`.
        let mut request = with_predicate(predicate.clone().unwrap_or_else(|| constant(true)));
        if !rule.is_null() {
            request["program"]["derived"]
                .as_array_mut()
                .expect("derived rules")
                .push(rule);
            request["queries"][0]["outputs"] = json!(["n", "headed"]);
        }
        let observed = observe(&request);
        assert!(
            observed.typing.is_empty(),
            "{name}: a test explain cannot reach implies no orientation: {:?}",
            observed.typing
        );
        let explain = observed.explain.expect_err(name);
        assert!(explain.contains(explain_error), "{name}: {explain}");
        assert!(
            observed.binding.is_empty(),
            "{name}: the declared order must bind: {:?}",
            observed.binding
        );
        let reversed = observe(&with_head_tuple(request, REVERSED_ORDER));
        assert_eq!(
            reversed.binding,
            reversed_head_diagnostics(),
            "{name}: the declaration stays the authority"
        );
    }
}

#[test]
fn rulespec_membership_in_an_if_condition_keeps_its_orientation() {
    let artifact = CompiledProgramArtifact::from_rulespec_str(
        r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: head_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: household_heads
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: "(if head_of_household: 1 else: 0) == 1"
  - name: head_count
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(household_heads)
  - name: household_size
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: hh_size
  - name: person_age
    kind: derived
    entity: Person
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: p_age
"#,
    )
    .expect("RuleSpec compiles");
    let relation = artifact
        .program
        .relations
        .iter()
        .find(|relation| relation.name == "household_heads")
        .expect("derived relation is emitted");
    let predicate = serde_json::to_value(
        &relation
            .derivation
            .as_ref()
            .expect("household_heads is derived")
            .predicate,
    )
    .expect("predicate serializes");
    assert_eq!(
        predicate["left"]["condition"]["kind"], "relation_member",
        "the formula must lower to the #203 shape: {predicate}"
    );
    // Formula lowering gives a membership test the default slots (1, 0),
    // which read `head_of_household` as declared here. Lowering does not
    // re-key a test inside a comparison from the declared kinds; the
    // household-first test below covers a declaration the default reads
    // backwards.
    assert_eq!(
        (
            &predicate["left"]["condition"]["current_slot"],
            &predicate["left"]["condition"]["related_slot"],
        ),
        (&json!(1), &json!(0)),
        "{predicate}"
    );
    assert!(
        artifact.diagnostics.is_empty(),
        "{:?}",
        artifact.diagnostics
    );

    let runtime = artifact
        .program
        .to_program()
        .expect("runtime program builds");
    let fixture = fixture();
    let dataset_with_head = |tuple: [&str; 2]| {
        let mut dataset: DatasetSpec =
            serde_json::from_value(fixture["dataset"].clone()).expect("dataset parses");
        for record in &mut dataset.relations {
            record.name = format!("{}_of_household", record.name);
        }
        dataset
            .relations
            .retain(|record| record.name != "head_of_household");
        let interval = dataset.relations[0].interval.clone();
        dataset.relations.push(RelationRecordSpec {
            name: "head_of_household".to_string(),
            tuple: tuple.map(str::to_string).to_vec(),
            interval,
        });
        dataset
    };
    dataset_with_head(DECLARED_ORDER)
        .to_dataset_for_program_with_options(&runtime, DatasetBindingOptions::strict())
        .expect("the correctly oriented dataset binds strictly");
    let reversed = dataset_with_head(REVERSED_ORDER)
        .to_dataset_for_program_with_options(&runtime, DatasetBindingOptions::default())
        .expect("default binding only warns");
    assert_eq!(reversed.diagnostics.len(), 2, "{:?}", reversed.diagnostics);
}

/// A context-keeping step between a scalar and its enclosing scalar. Each
/// preserves the value (1 or 0) and evaluates its operand, so the membership
/// test is always reached and always decides the outcome.
#[derive(Clone, Copy, Debug)]
enum ScalarStep {
    ThenBranch,
    ElseBranch,
    AddZero,
    SubZero,
    MulOne,
    DivOne,
    MaxZero,
    MinOne,
    Ceil,
    Floor,
}

#[derive(Clone, Copy, Debug)]
enum Comparison {
    EqualsOne,
    OneEquals,
    AboveZero,
    NotZero,
}

/// A step between a judgment and its enclosing judgment, innermost first.
#[derive(Clone, Debug)]
enum Step {
    NotNot,
    AndTrue {
        member_first: bool,
    },
    OrFalse {
        member_first: bool,
    },
    Compare(Vec<ScalarStep>, Comparison),
    /// Into a nested aggregation's `where` clause: the evaluator drops the
    /// relation context here, so the membership test fails at run time. The
    /// predicate runs on a person; each aggregation turns to the other kind,
    /// so every `where` clause has IDs to run on.
    CountWhere,
    SumWhere,
    /// Into a `match` fallback's pattern label, which explain never evaluates:
    /// it fails at the fallback instead.
    MatchPattern,
    /// Into an over-periods reduction, which explain refuses outside lifetime
    /// execution without evaluating it.
    OverPeriods,
}

/// How far explain gets into a wrapped membership test.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reach {
    /// It evaluates the test with the derived relation's IDs bound.
    Membership,
    /// It fails first with an error containing this text.
    Fails(&'static str),
}

fn scalar_step() -> impl Strategy<Value = ScalarStep> {
    prop_oneof![
        Just(ScalarStep::ThenBranch),
        Just(ScalarStep::ElseBranch),
        Just(ScalarStep::AddZero),
        Just(ScalarStep::SubZero),
        Just(ScalarStep::MulOne),
        Just(ScalarStep::DivOne),
        Just(ScalarStep::MaxZero),
        Just(ScalarStep::MinOne),
        Just(ScalarStep::Ceil),
        Just(ScalarStep::Floor),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    let comparison = prop_oneof![
        Just(Comparison::EqualsOne),
        Just(Comparison::OneEquals),
        Just(Comparison::AboveZero),
        Just(Comparison::NotZero),
    ];
    prop_oneof![
        3 => Just(Step::NotNot),
        3 => any::<bool>().prop_map(|member_first| Step::AndTrue { member_first }),
        3 => any::<bool>().prop_map(|member_first| Step::OrFalse { member_first }),
        6 => (vec(scalar_step(), 0..4), comparison)
            .prop_map(|(steps, comparison)| Step::Compare(steps, comparison)),
        1 => Just(Step::CountWhere),
        1 => Just(Step::SumWhere),
        1 => Just(Step::MatchPattern),
        1 => Just(Step::OverPeriods),
    ]
}

/// Wrap `judgment` in `steps`; also report how far explain gets into it.
/// Every other step evaluates its operand, so explain reaches the membership
/// test unless a step drops the relation context on the way (it then fails at
/// the test) or a `match` fallback or over-periods reduction stops it first
/// (the outermost one fails).
fn wrap(mut judgment: Value, steps: &[Step]) -> (Value, Reach) {
    let mut outer_aggregations = steps
        .iter()
        .filter(|step| matches!(step, Step::CountWhere | Step::SumWhere))
        .count();
    let mut reach = Reach::Membership;
    for step in steps {
        reach = match step {
            Step::CountWhere | Step::SumWhere if reach == Reach::Membership => {
                Reach::Fails(OUT_OF_CONTEXT_ERROR)
            }
            Step::MatchPattern => Reach::Fails(NO_MATCHING_ARM_ERROR),
            Step::OverPeriods => Reach::Fails(OVER_PERIODS_ERROR),
            _ => reach,
        };
        judgment = match step {
            Step::NotNot => json!({"kind": "not", "item": {"kind": "not", "item": judgment}}),
            Step::AndTrue { member_first } => {
                let items = if *member_first {
                    [judgment, constant(true)]
                } else {
                    [constant(true), judgment]
                };
                json!({"kind": "and", "items": items})
            }
            Step::OrFalse { member_first } => {
                let items = if *member_first {
                    [judgment, constant(false)]
                } else {
                    [constant(false), judgment]
                };
                json!({"kind": "or", "items": items})
            }
            Step::Compare(steps, comparison) => {
                let mut scalar = indicator(judgment);
                for step in steps {
                    scalar = match step {
                        ScalarStep::ThenBranch => json!({
                            "kind": "if", "condition": constant(true), "then_expr": scalar, "else_expr": int(0)
                        }),
                        ScalarStep::ElseBranch => json!({
                            "kind": "if", "condition": constant(false), "then_expr": int(0), "else_expr": scalar
                        }),
                        ScalarStep::AddZero => json!({"kind": "add", "items": [scalar, int(0)]}),
                        ScalarStep::SubZero => {
                            json!({"kind": "sub", "left": scalar, "right": int(0)})
                        }
                        ScalarStep::MulOne => {
                            json!({"kind": "mul", "left": scalar, "right": int(1)})
                        }
                        ScalarStep::DivOne => {
                            json!({"kind": "div", "left": scalar, "right": int(1)})
                        }
                        ScalarStep::MaxZero => json!({"kind": "max", "items": [int(0), scalar]}),
                        ScalarStep::MinOne => json!({"kind": "min", "items": [scalar, int(1)]}),
                        ScalarStep::Ceil => json!({"kind": "ceil", "value": scalar}),
                        ScalarStep::Floor => json!({"kind": "floor", "value": scalar}),
                    };
                }
                match comparison {
                    Comparison::EqualsOne => compare(scalar, "eq", int(1)),
                    Comparison::OneEquals => compare(int(1), "eq", scalar),
                    Comparison::AboveZero => compare(scalar, "gt", int(0)),
                    Comparison::NotZero => compare(scalar, "ne", int(0)),
                }
            }
            Step::CountWhere | Step::SumWhere => {
                outer_aggregations -= 1;
                let from_person = outer_aggregations % 2 == 0;
                let aggregation = if matches!(step, Step::CountWhere) {
                    count_where(from_person, judgment)
                } else {
                    sum_where(from_person, judgment)
                };
                compare(aggregation, "gt", int(0))
            }
            Step::MatchPattern => match_fallback(judgment),
            Step::OverPeriods => over_periods(judgment),
        };
    }
    (judgment, reach)
}

fn check_case(steps: &[Step], slots: (usize, usize)) -> Result<(), TestCaseError> {
    let (predicate, reach) = wrap(member(slots.0, slots.1), steps);
    // Explain's membership lookup reads `head` as [related, current] for slots
    // (1, 0) and as [current, related] for (0, 1). Under the current kind
    // `Household` and related kind `Person`, (0, 1) contradicts the declared
    // `[Person, Household]`.
    let contradicts_declaration = slots == (0, 1);
    for tuple in [DECLARED_ORDER, REVERSED_ORDER] {
        let request = with_head_tuple(with_predicate(predicate.clone()), tuple);
        let observed = observe(&request);
        prop_assert_eq!(
            observed.binding.is_empty(),
            tuple == DECLARED_ORDER,
            "binding expects the declared order whatever the predicate: {:?}\n{}",
            observed.binding,
            predicate
        );
        match reach {
            Reach::Membership if contradicts_declaration => {
                prop_assert_eq!(
                    &observed.typing,
                    &membership_mismatches(),
                    "a reached test reading the reversed slots is refused\n{}",
                    predicate
                );
            }
            Reach::Membership => {
                prop_assert!(
                    observed.typing.is_empty(),
                    "a reached test reading the declared slots compiles: {:?}\n{}",
                    observed.typing,
                    predicate
                );
                let consumed = match &observed.explain {
                    Ok(1) => true,
                    Ok(0) => false,
                    other => {
                        return Err(TestCaseError::fail(format!(
                            "explain must count 0 or 1 heads, got {other:?} for {predicate}"
                        )));
                    }
                };
                prop_assert_eq!(
                    consumed,
                    tuple == DECLARED_ORDER,
                    "explain consumes exactly the declared order\n{}",
                    predicate
                );
            }
            Reach::Fails(error) => {
                prop_assert!(
                    observed.typing.is_empty(),
                    "a test explain cannot reach implies no orientation: {:?}\n{}",
                    observed.typing,
                    predicate
                );
                let explain = observed.explain.as_ref().err().cloned().unwrap_or_default();
                prop_assert!(
                    explain.contains(error),
                    "explain must fail with `{}`: {:?}\n{}",
                    error,
                    observed.explain,
                    predicate
                );
            }
        }
    }
    Ok(())
}

#[test]
fn binding_and_compile_agree_with_explain_on_generated_predicates() {
    let cases = std::env::var("AXIOM_RELATION_USAGE_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(256);
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&203_u64.to_le_bytes());
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &seed),
    );
    let strategy = (vec(step(), 0..5), prop_oneof![Just((1, 0)), Just((0, 1))]);
    let reached = std::cell::RefCell::new(std::collections::BTreeMap::new());
    if let Err(error) = runner.run(&strategy, |(steps, slots)| {
        let reach = wrap(member(slots.0, slots.1), &steps).1;
        *reached
            .borrow_mut()
            .entry((format!("{reach:?}"), slots))
            .or_insert(0) += 1;
        check_case(&steps, slots)
    }) {
        panic!("{error}");
    }
    // The generator keeps reaching the membership test with both slots and
    // stopping at each kind of site explain cannot see past. Checked at the
    // default case count and above, where every combination is common.
    let reached = reached.into_inner();
    for reach in [
        Reach::Membership,
        Reach::Fails(OUT_OF_CONTEXT_ERROR),
        Reach::Fails(NO_MATCHING_ARM_ERROR),
        Reach::Fails(OVER_PERIODS_ERROR),
    ] {
        for slots in [(1, 0), (0, 1)] {
            let seen = reached
                .get(&(format!("{reach:?}"), slots))
                .copied()
                .unwrap_or(0);
            assert!(
                cases < 256 || seen * 100 >= cases as usize,
                "too few cases reach {reach:?} with slots {slots:?}: {reached:?}"
            );
        }
    }
}
