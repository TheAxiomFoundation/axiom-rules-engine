//! Tests for `evaluation_order` after derived relations became nodes of its
//! graph.
//!
//! The order is stored in artifact metadata and compared exactly when an
//! artifact loads, so the change must not move it. For every generated
//! program:
//!
//! 1. **Differential.** `evaluation_order` returns what the frozen pre-change
//!    implementation (`evaluation_order_reference`) returns: the same order,
//!    or the same error with the same fields.
//! 2. **Specification.** An order lists every rule once. Its i-th rule is the
//!    smallest name among the rules not yet listed whose waits are all
//!    listed, where a rule waits for each rule it reads and for each rule read
//!    by the predicate of a derived relation it counts or sums over. When that
//!    walk stalls before the rules run out, the error is `CyclicDependency`
//!    naming exactly the rules it never reached.
//! 3. **Position independence.** Reordering `program.derived` changes neither
//!    an order nor a cycle report.
//!
//! Programs come from a generator with a fixed seed, so CI is deterministic,
//! in three profiles: layered (acyclic and well formed by construction),
//! near miss (layered, with rare stray references and duplicate names), and
//! free (no layering). `the_generator_reaches_every_outcome` keeps the
//! properties from passing vacuously. `AXIOM_EVAL_ORDER_CASES=<n>` sets the
//! cases per profile and property (default [`DEFAULT_CASES`]).
//!
//! The named tests below pin the audit's fan-out shape, in time and in
//! outcome, and the edge cases of routing a wait through a relation node.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::{Union, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};

use super::{
    CompileError, derived_dependencies, evaluation_order, evaluation_order_reference,
    relation_derivation_dependencies,
};
use crate::spec::{
    ComparisonOpSpec, DTypeSpec, DerivedSemanticsSpec, DerivedSpec, DerivedVersionSpec,
    JudgmentExprSpec, OverPeriodsKindSpec, ProgramSpec, RelatedValueRefSpec,
    RelationDerivationSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec,
};

const DEFAULT_CASES: u32 = 1_000;

/// Rule names whose lexicographic order has nothing to do with the positions
/// they take (`b1` < `b10` < `b2`), so an order rarely matches program order.
const RULE_NAMES: [&str; 10] = [
    "tax", "agi", "credit", "b2", "b10", "a", "zeta", "benefit", "b1", "eligible",
];
/// Names no rule takes: one sorts before every rule name, one among them, one
/// after them.
const UNKNOWN_RULES: [&str; 3] = ["_missing", "c_missing", "zz_missing"];
const RELATION_NAMES: [&str; 4] = ["members", "adults", "kids", "units"];
/// The base relation every generated program declares.
const BASE: &str = "base";
const UNKNOWN_RELATION: &str = "missing_relation";

#[derive(Clone, Copy, Debug)]
struct Profile {
    name: &'static str,
    /// Weight, in percent, of a reference that ignores the layering: any rule
    /// (the referring one included) or an unknown name; any relation or an
    /// unknown one.
    stray: u32,
    /// Chance, in percent, that a rule or relation reuses an earlier one's
    /// name.
    duplicate: u32,
}

/// Layering: rule `i` (by position) reads only rules before it. Relation `k`
/// has a threshold, ascending in `k`; its predicate reads only rules before
/// its threshold and names only `base` and relations before `k`, and it
/// sources `base` or a relation before `k`. Rule `i` counts, sums or tests
/// membership only in `base` and relations whose threshold is at most `i`.
/// Every edge then runs forward in (2 * position + 1 for a rule, 2 *
/// threshold for a relation; relation index breaking ties), so the program
/// is acyclic, and it names nothing unknown.
const LAYERED: Profile = Profile {
    name: "layered",
    stray: 0,
    duplicate: 0,
};
/// Layered, with rare strays: small cycles with rules downstream of them,
/// unknown names, and duplicate names.
const NEAR_MISS: Profile = Profile {
    name: "near_miss",
    stray: 4,
    duplicate: 4,
};
const FREE: Profile = Profile {
    name: "free",
    stray: 100,
    duplicate: 10,
};
const PROFILES: [Profile; 3] = [LAYERED, NEAR_MISS, FREE];

/// What one expression may name.
#[derive(Clone)]
struct Scope {
    /// Rules a reference may name within the layering.
    rules: Vec<String>,
    /// Relations an aggregation or membership test may name within it.
    relations: Vec<String>,
    /// Every rule name, and the unknown ones, for stray references.
    any_rules: Vec<String>,
    /// Every relation name, and an unknown one, for stray references.
    any_relations: Vec<String>,
    stray: u32,
}

/// A strategy for a reference, with the weight in percent its leaf should
/// get: a scope with nothing layered to name makes only strays, at the stray
/// rate.
fn pick(layered: &[String], any: &[String], stray: u32) -> Option<(u32, BoxedStrategy<String>)> {
    let layered = (stray < 100 && !layered.is_empty()).then(|| select(layered.to_vec()).boxed());
    let strays = (stray > 0).then(|| select(any.to_vec()).boxed());
    match (layered, strays) {
        (Some(layered), Some(strays)) => Some((
            100,
            Union::new_weighted(vec![(100 - stray, layered), (stray, strays)]).boxed(),
        )),
        (Some(layered), None) => Some((100, layered)),
        (None, Some(strays)) => Some((stray, strays)),
        (None, None) => None,
    }
}

fn integer(value: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Integer { value },
    }
}

fn count_related(relation: String, where_clause: Option<JudgmentExprSpec>) -> ScalarExprSpec {
    ScalarExprSpec::CountRelated {
        relation,
        current_slot: 0,
        related_slot: 1,
        where_clause: where_clause.map(Box::new),
    }
}

fn sum_related(
    relation: String,
    value: RelatedValueRefSpec,
    where_clause: Option<JudgmentExprSpec>,
) -> ScalarExprSpec {
    ScalarExprSpec::SumRelated {
        relation,
        current_slot: 0,
        related_slot: 1,
        value,
        where_clause: where_clause.map(Box::new),
    }
}

fn relation_member(relation: String) -> JudgmentExprSpec {
    JudgmentExprSpec::RelationMember {
        relation,
        current_slot: 0,
        related_slot: 1,
    }
}

fn reads(name: &str) -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(ScalarExprSpec::Derived {
            name: name.to_string(),
        }),
        op: ComparisonOpSpec::Gte,
        right: Box::new(integer(0)),
    }
}

fn comparison_op() -> impl Strategy<Value = ComparisonOpSpec> {
    select(vec![
        ComparisonOpSpec::Lt,
        ComparisonOpSpec::Lte,
        ComparisonOpSpec::Gt,
        ComparisonOpSpec::Gte,
        ComparisonOpSpec::Eq,
        ComparisonOpSpec::Ne,
    ])
}

/// A scalar expression at most `depth` levels deep, over every variant.
fn scalar(scope: &Scope, depth: u32) -> BoxedStrategy<ScalarExprSpec> {
    let rule = pick(&scope.rules, &scope.any_rules, scope.stray);
    let relation = pick(&scope.relations, &scope.any_relations, scope.stray);
    let mut leaves = vec![
        (200, Just(integer(1)).boxed()),
        (
            100,
            Just(ScalarExprSpec::Input {
                name: "income".to_string(),
            })
            .boxed(),
        ),
        (
            50,
            Just(ScalarExprSpec::InputOrElse {
                name: "size".to_string(),
                default: ScalarValueSpec::Integer { value: 1 },
            })
            .boxed(),
        ),
        (25, Just(ScalarExprSpec::PeriodStart).boxed()),
        (25, Just(ScalarExprSpec::PeriodEnd).boxed()),
    ];
    if let Some((percent, rule)) = &rule {
        leaves.push((
            6 * percent,
            rule.clone()
                .prop_map(|name| ScalarExprSpec::Derived { name })
                .boxed(),
        ));
    }
    if let Some((percent, relation)) = &relation {
        leaves.push((
            8 * percent,
            relation
                .clone()
                .prop_map(|relation| count_related(relation, None))
                .boxed(),
        ));
    }
    let leaf = Union::new_weighted(leaves).boxed();
    if depth == 0 {
        return leaf;
    }
    let inner = scalar(scope, depth - 1);
    let condition = judgment(scope, depth - 1);
    let pair = (inner.clone(), inner.clone());
    let mut nodes = vec![
        (400, leaf),
        (
            100,
            vec(inner.clone(), 0..3)
                .prop_map(|items| ScalarExprSpec::Add { items })
                .boxed(),
        ),
        (
            30,
            vec(inner.clone(), 1..3)
                .prop_map(|items| ScalarExprSpec::Max { items })
                .boxed(),
        ),
        (
            30,
            vec(inner.clone(), 1..3)
                .prop_map(|items| ScalarExprSpec::Min { items })
                .boxed(),
        ),
        (
            30,
            pair.clone()
                .prop_map(|(left, right)| ScalarExprSpec::Sub {
                    left: Box::new(left),
                    right: Box::new(right),
                })
                .boxed(),
        ),
        (
            30,
            pair.clone()
                .prop_map(|(left, right)| ScalarExprSpec::Mul {
                    left: Box::new(left),
                    right: Box::new(right),
                })
                .boxed(),
        ),
        (
            30,
            pair.clone()
                .prop_map(|(left, right)| ScalarExprSpec::Div {
                    left: Box::new(left),
                    right: Box::new(right),
                })
                .boxed(),
        ),
        (
            20,
            inner
                .clone()
                .prop_map(|value| ScalarExprSpec::Ceil {
                    value: Box::new(value),
                })
                .boxed(),
        ),
        (
            20,
            inner
                .clone()
                .prop_map(|value| ScalarExprSpec::Floor {
                    value: Box::new(value),
                })
                .boxed(),
        ),
        (
            30,
            inner
                .clone()
                .prop_map(|index| ScalarExprSpec::ParameterLookup {
                    parameter: "table".to_string(),
                    index: Box::new(index),
                })
                .boxed(),
        ),
        (
            10,
            pair.clone()
                .prop_map(|(date, days)| ScalarExprSpec::DateAddDays {
                    date: Box::new(date),
                    days: Box::new(days),
                })
                .boxed(),
        ),
        (
            10,
            pair.clone()
                .prop_map(|(date, months)| ScalarExprSpec::DateAddMonths {
                    date: Box::new(date),
                    months: Box::new(months),
                })
                .boxed(),
        ),
        (
            10,
            pair.clone()
                .prop_map(|(date, years)| ScalarExprSpec::DateAddYears {
                    date: Box::new(date),
                    years: Box::new(years),
                })
                .boxed(),
        ),
        (
            10,
            pair.prop_map(|(from, to)| ScalarExprSpec::DaysBetween {
                from: Box::new(from),
                to: Box::new(to),
            })
            .boxed(),
        ),
        (
            100,
            (condition.clone(), inner.clone(), inner.clone())
                .prop_map(|(condition, then_expr, else_expr)| ScalarExprSpec::If {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                })
                .boxed(),
        ),
        (
            20,
            (inner.clone(), vec(inner.clone(), 0..3))
                .prop_map(|(subject, patterns)| ScalarExprSpec::NoMatch {
                    subject: Box::new(subject),
                    patterns,
                })
                .boxed(),
        ),
        (
            20,
            (inner.clone(), proptest::option::of(inner))
                .prop_map(|(value, n)| ScalarExprSpec::OverPeriods {
                    over: OverPeriodsKindSpec::Sum,
                    value: Box::new(value),
                    n: n.map(Box::new),
                })
                .boxed(),
        ),
    ];
    if let Some((percent, relation)) = relation {
        nodes.push((
            8 * percent,
            (relation.clone(), proptest::option::of(condition.clone()))
                .prop_map(|(relation, where_clause)| count_related(relation, where_clause))
                .boxed(),
        ));
        let mut values = vec![(
            100,
            Just(RelatedValueRefSpec::Input {
                name: "income".to_string(),
            })
            .boxed(),
        )];
        if let Some((rule_percent, rule)) = rule {
            values.push((
                rule_percent,
                rule.prop_map(|name| RelatedValueRefSpec::Derived { name })
                    .boxed(),
            ));
        }
        nodes.push((
            8 * percent,
            (
                relation,
                Union::new_weighted(values),
                proptest::option::of(condition),
            )
                .prop_map(|(relation, value, where_clause)| {
                    sum_related(relation, value, where_clause)
                })
                .boxed(),
        ));
    }
    Union::new_weighted(nodes).boxed()
}

/// A judgment expression at most `depth` levels deep, over every variant.
fn judgment(scope: &Scope, depth: u32) -> BoxedStrategy<JudgmentExprSpec> {
    let rule = pick(&scope.rules, &scope.any_rules, scope.stray);
    let relation = pick(&scope.relations, &scope.any_relations, scope.stray);
    let mut leaves = vec![(
        100,
        Just(JudgmentExprSpec::Comparison {
            left: Box::new(integer(1)),
            op: ComparisonOpSpec::Eq,
            right: Box::new(integer(1)),
        })
        .boxed(),
    )];
    if let Some((percent, rule)) = rule {
        leaves.push((
            4 * percent,
            rule.prop_map(|name| JudgmentExprSpec::Derived { name })
                .boxed(),
        ));
    }
    if let Some((percent, relation)) = relation {
        leaves.push((2 * percent, relation.prop_map(relation_member).boxed()));
    }
    let leaf = Union::new_weighted(leaves).boxed();
    if depth == 0 {
        return leaf;
    }
    let operand = scalar(scope, depth - 1);
    let inner = judgment(scope, depth - 1);
    Union::new_weighted(vec![
        (300, leaf),
        (
            300,
            (operand.clone(), comparison_op(), operand)
                .prop_map(|(left, op, right)| JudgmentExprSpec::Comparison {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                })
                .boxed(),
        ),
        (
            100,
            vec(inner.clone(), 0..3)
                .prop_map(|items| JudgmentExprSpec::And { items })
                .boxed(),
        ),
        (
            100,
            vec(inner.clone(), 0..3)
                .prop_map(|items| JudgmentExprSpec::Or { items })
                .boxed(),
        ),
        (
            50,
            vec(inner.clone(), 0..3)
                .prop_map(|items| JudgmentExprSpec::ExactlyOne { items })
                .boxed(),
        ),
        (
            100,
            inner
                .prop_map(|item| JudgmentExprSpec::Not {
                    item: Box::new(item),
                })
                .boxed(),
        ),
    ])
    .boxed()
}

fn semantics(scope: &Scope) -> BoxedStrategy<DerivedSemanticsSpec> {
    prop_oneof![
        scalar(scope, 3).prop_map(|expr| DerivedSemanticsSpec::Scalar { expr }),
        judgment(scope, 3).prop_map(|expr| DerivedSemanticsSpec::Judgment { expr }),
    ]
    .boxed()
}

fn rule(
    name: String,
    semantics: DerivedSemanticsSpec,
    versions: Vec<DerivedSemanticsSpec>,
) -> DerivedSpec {
    let dtype = match semantics {
        DerivedSemanticsSpec::Scalar { .. } => DTypeSpec::Integer,
        DerivedSemanticsSpec::Judgment { .. } => DTypeSpec::Judgment,
    };
    DerivedSpec {
        id: None,
        name,
        entity: "Household".to_string(),
        dtype,
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
                effective_from: NaiveDate::from_ymd_opt(year, 1, 1).expect("valid date"),
                effective_to: None,
                semantics,
            })
            .collect(),
    }
}

fn base_relation(name: &str) -> RelationSpec {
    RelationSpec {
        name: name.to_string(),
        arity: 2,
        slot_entities: Vec::new(),
        derivation: None,
    }
}

fn derived_relation(name: &str, source: &str, predicate: JudgmentExprSpec) -> RelationSpec {
    RelationSpec {
        name: name.to_string(),
        arity: 2,
        slot_entities: Vec::new(),
        derivation: Some(RelationDerivationSpec {
            source_relation: source.to_string(),
            current_slot: 0,
            related_slot: 1,
            entity: None,
            member_relation: None,
            slot_entities: Vec::new(),
            predicate,
        }),
    }
}

/// Names for `rolls.len()` rules or relations: position `i` takes `pool[i]`,
/// or, when its roll is under `duplicate`, an earlier position's name.
fn names(pool: &[String], rolls: &[u32], duplicate: u32) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(rolls.len());
    for (position, &roll) in rolls.iter().enumerate() {
        let name = if position > 0 && roll < duplicate {
            names[roll as usize % position].clone()
        } else {
            pool[position].clone()
        };
        names.push(name);
    }
    names
}

fn program(profile: Profile) -> BoxedStrategy<ProgramSpec> {
    // At least one relation besides `base`; programs whose relations are all
    // base relations still come up when every one of them rolls base.
    (1..=RULE_NAMES.len(), 1..=RELATION_NAMES.len())
        .prop_flat_map(|(rules, relations)| {
            (
                Just(RULE_NAMES.map(String::from).to_vec()).prop_shuffle(),
                // Thresholds in the lower half: predicates read early rules,
                // and later rules can count or sum over the relation.
                vec(0..=rules.div_ceil(2), relations),
                vec(0..100_u32, rules),
                vec(0..100_u32, relations),
                // A quarter of the relations are base relations.
                vec(proptest::bool::weighted(0.75), relations),
            )
        })
        .prop_flat_map(
            move |(pool, mut thresholds, rule_rolls, relation_rolls, derived)| {
                thresholds.sort_unstable();
                let rule_names = names(&pool, &rule_rolls, profile.duplicate);
                let relation_names = names(
                    &RELATION_NAMES.map(String::from),
                    &relation_rolls,
                    profile.duplicate,
                );
                let any_rules = rule_names
                    .iter()
                    .cloned()
                    .chain(UNKNOWN_RULES.map(String::from))
                    .collect::<Vec<_>>();
                let any_relations = relation_names
                    .iter()
                    .cloned()
                    .chain([BASE.to_string(), UNKNOWN_RELATION.to_string()])
                    .collect::<Vec<_>>();
                let scope = |rules: &[String], relations: Vec<String>| Scope {
                    rules: rules.to_vec(),
                    relations,
                    any_rules: any_rules.clone(),
                    any_relations: any_relations.clone(),
                    stray: profile.stray,
                };

                let relations = relation_names
                    .iter()
                    .enumerate()
                    .map(|(k, name)| {
                        if !derived[k] {
                            return Just(base_relation(name)).boxed();
                        }
                        let earlier = std::iter::once(BASE.to_string())
                            .chain(relation_names[..k].iter().cloned())
                            .collect::<Vec<_>>();
                        let scope = scope(&rule_names[..thresholds[k]], earlier.clone());
                        let (_, source) = pick(&earlier, &scope.any_relations, scope.stray)
                            .expect("`base` is always in scope");
                        let name = name.clone();
                        (source, judgment(&scope, 2))
                            .prop_map(move |(source, predicate)| {
                                derived_relation(&name, &source, predicate)
                            })
                            .boxed()
                    })
                    .collect::<Vec<_>>();
                let rules = rule_names
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        let relations = std::iter::once(BASE.to_string())
                            .chain(
                                relation_names
                                    .iter()
                                    .zip(&thresholds)
                                    .filter(|(_, threshold)| **threshold <= position)
                                    .map(|(relation, _)| relation.clone()),
                            )
                            .collect();
                        let semantics = semantics(&scope(&rule_names[..position], relations));
                        let name = name.clone();
                        (semantics.clone(), vec(semantics, 0..=2))
                            .prop_map(move |(semantics, versions)| {
                                rule(name.clone(), semantics, versions)
                            })
                            .boxed()
                    })
                    .collect::<Vec<_>>();
                (relations, rules).prop_map(|(relations, derived)| ProgramSpec {
                    relations: std::iter::once(base_relation(BASE))
                        .chain(relations)
                        .collect(),
                    derived,
                    ..ProgramSpec::default()
                })
            },
        )
        .boxed()
}

fn cases() -> u32 {
    std::env::var("AXIOM_EVAL_ORDER_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CASES)
}

fn runner(property: &str, profile: Profile, cases: u32) -> TestRunner {
    let mut seed = [0_u8; 32];
    for (index, byte) in format!("{property}/{}", profile.name).bytes().enumerate() {
        seed[index % 32] ^= byte.rotate_left(index as u32 % 8);
    }
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &seed))
}

/// Run `check` on `cases()` programs from every profile.
fn check_every_profile<S: Strategy>(
    property: &str,
    strategy: impl Fn(Profile) -> S,
    check: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    for profile in PROFILES {
        let mut runner = runner(property, profile, cases());
        if let Err(error) = runner.run(&strategy(profile), &check) {
            panic!("{property} ({} profile): {error}", profile.name);
        }
    }
}

/// An outcome with the error rendered, since `CompileError` has no `Eq`.
fn outcome(result: Result<Vec<String>, CompileError>) -> Result<Vec<String>, String> {
    result.map_err(|error| format!("{error:?}"))
}

fn json(program: &ProgramSpec) -> String {
    serde_json::to_string(program).expect("a program spec serializes")
}

/// What each rule waits for, from the definition.
struct Waits {
    rule: String,
    /// The rules it reads.
    direct: BTreeSet<String>,
    /// Those, and the rules read by the predicate of each derived relation it
    /// counts or sums over.
    all: BTreeSet<String>,
}

/// Each rule's waits. Only for programs that passed the name checks, which
/// every program that orders or reports a rule cycle has.
fn waits(program: &ProgramSpec) -> Vec<Waits> {
    let names = program
        .derived
        .iter()
        .map(|derived| derived.name.clone())
        .collect::<HashSet<_>>();
    let predicate_rules = relation_derivation_dependencies(program, &names)
        .expect("relation predicates read known rules");
    program
        .derived
        .iter()
        .map(|derived| {
            let (direct, relations) = derived_dependencies(derived);
            let direct = direct.into_iter().collect::<BTreeSet<_>>();
            let mut all = direct.clone();
            for relation in &relations {
                if let Some(rules) = predicate_rules.get(relation) {
                    all.extend(rules.iter().cloned());
                }
            }
            Waits {
                rule: derived.name.clone(),
                direct,
                all,
            }
        })
        .collect()
}

/// The order the definition gives: repeatedly list the smallest unlisted rule
/// whose waits are all listed. It stops short of the rules when they wait on
/// one another in a cycle.
fn smallest_ready_order(waits: &[Waits]) -> Vec<String> {
    let mut listed = BTreeSet::new();
    let mut order = Vec::new();
    while let Some(next) = waits
        .iter()
        .filter(|rule| !listed.contains(&rule.rule) && rule.all.is_subset(&listed))
        .map(|rule| rule.rule.clone())
        .min()
    {
        listed.insert(next.clone());
        order.push(next);
    }
    order
}

#[test]
fn evaluation_order_matches_the_reference() {
    check_every_profile("differential", program, |program| {
        let expected = outcome(evaluation_order_reference::evaluation_order(&program));
        let actual = outcome(evaluation_order(&program));
        prop_assert_eq!(&actual, &expected, "program: {}", json(&program));
        Ok(())
    });
}

#[test]
fn evaluation_order_lists_the_smallest_ready_rule_first() {
    check_every_profile("specification", program, |program| {
        let actual = evaluation_order(&program);
        let comparable = matches!(&actual, Ok(_) | Err(CompileError::CyclicDependency { .. }));
        if !comparable {
            return Ok(());
        }
        let waits = waits(&program);
        let walked = smallest_ready_order(&waits);
        match actual {
            Ok(order) => {
                prop_assert_eq!(&order, &walked, "program: {}", json(&program));
                prop_assert_eq!(order.len(), program.derived.len());
            }
            // A walk that lists every rule leaves the cycle to the routed
            // check, which this specification does not restate.
            Err(CompileError::CyclicDependency { cycle }) if walked.len() < waits.len() => {
                let unreached = waits
                    .iter()
                    .map(|rule| rule.rule.as_str())
                    .filter(|rule| !walked.iter().any(|listed| listed == rule))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ");
                prop_assert_eq!(cycle, unreached, "program: {}", json(&program));
            }
            Err(_) => {}
        }
        Ok(())
    });
}

#[test]
fn evaluation_order_ignores_rule_positions() {
    let with_permutation = |profile: Profile| {
        program(profile).prop_flat_map(|program| {
            let positions = (0..program.derived.len()).collect::<Vec<_>>();
            (Just(program), Just(positions).prop_shuffle())
        })
    };
    check_every_profile(
        "positions",
        with_permutation,
        |(program, positions): (ProgramSpec, Vec<usize>)| {
            let original = evaluation_order(&program);
            if !matches!(
                &original,
                Ok(_) | Err(CompileError::CyclicDependency { .. })
            ) {
                return Ok(());
            }
            let mut permuted = program.clone();
            permuted.derived = positions
                .iter()
                .map(|&position| program.derived[position].clone())
                .collect();
            prop_assert_eq!(
                outcome(evaluation_order(&permuted)),
                outcome(original),
                "program: {}",
                json(&program)
            );
            Ok(())
        },
    );
}

/// Without this, a generator drifting to one outcome would leave the
/// properties above passing on nothing.
#[test]
fn the_generator_reaches_every_outcome() {
    const SAMPLES: u32 = 2_000;
    let mut report = Vec::new();
    for profile in PROFILES {
        let mut runner = runner("coverage", profile, SAMPLES);
        let strategy = program(profile);
        let mut outcomes = BTreeMap::<&str, u32>::new();
        for _ in 0..SAMPLES {
            let program = strategy
                .new_tree(&mut runner)
                .expect("the strategy generates")
                .current();
            let result = evaluation_order_reference::evaluation_order(&program);
            let kind = match &result {
                Ok(_) => "ok",
                Err(CompileError::CyclicDependency { .. }) => "cyclic_dependency",
                Err(CompileError::UnknownDerivedDependency { .. }) => "unknown_dependency",
                Err(CompileError::DuplicateDerivedRule { .. }) => "duplicate_rule",
                Err(CompileError::CyclicRelationDependency { .. }) => "cyclic_relation",
                Err(CompileError::UnknownRelationDependency { .. }) => "unknown_relation",
                Err(_) => "other",
            };
            *outcomes.entry(kind).or_default() += 1;
            match &result {
                // Some rule waits on a rule only through a relation, which is
                // what the relation nodes carry.
                Ok(_) => {
                    let routed = waits(&program)
                        .iter()
                        .any(|rule| !rule.all.is_subset(&rule.direct));
                    *outcomes.entry("ok_routed_wait").or_default() += u32::from(routed);
                }
                // Only the routed check finds the cycle: the waits alone order.
                Err(CompileError::CyclicDependency { .. }) => {
                    let waits = waits(&program);
                    let routed = smallest_ready_order(&waits).len() == waits.len();
                    *outcomes.entry("routed_cycle").or_default() += u32::from(routed);
                }
                Err(_) => {}
            }
        }
        eprintln!("{}: {outcomes:?} of {SAMPLES}", profile.name);
        report.push((profile.name, outcomes));
    }
    for (profile, outcomes) in &report {
        let share =
            |kind: &str| f64::from(outcomes.get(kind).copied().unwrap_or(0)) / f64::from(SAMPLES);
        // Floors well under what the fixed seed gives (layered: a third of
        // programs wait through a relation; near miss: about half order, a
        // tenth have rule cycles). Free programs mostly fail before the
        // order, so near miss carries the cycle coverage.
        let minimums: &[(&str, f64)] = match *profile {
            "layered" => &[("ok", 1.0), ("ok_routed_wait", 0.25)],
            "near_miss" => &[
                ("ok", 0.3),
                ("ok_routed_wait", 0.1),
                ("cyclic_dependency", 0.05),
                ("routed_cycle", 0.002),
                ("unknown_dependency", 0.05),
                ("duplicate_rule", 0.05),
            ],
            _ => &[
                ("ok", 0.002),
                ("cyclic_dependency", 0.005),
                ("unknown_dependency", 0.05),
                ("duplicate_rule", 0.1),
                ("cyclic_relation", 0.05),
                ("unknown_relation", 0.1),
            ],
        };
        for (kind, minimum) in minimums {
            assert!(
                share(kind) >= *minimum,
                "{profile}: {kind} in {} of programs, under {minimum}",
                share(kind)
            );
        }
    }
}

/// The audit's shape (2026-09-26, `evaluation-order-relation-dep-fanout`):
/// `aggregators` rules each count or sum over one derived relation whose
/// predicate reads `predicates` rules. Half count, half sum with a `where`.
fn fan_out(aggregators: usize, predicates: usize) -> ProgramSpec {
    let predicate_rule = |index: usize| format!("predicate_{index:05}");
    let filter = JudgmentExprSpec::And {
        items: (0..predicates)
            .map(|index| reads(&predicate_rule(index)))
            .collect(),
    };
    let mut derived = (0..predicates)
        .map(|index| {
            rule(
                predicate_rule(index),
                DerivedSemanticsSpec::Scalar { expr: integer(1) },
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    derived.extend((0..aggregators).map(|index| {
        let expr = if index % 2 == 0 {
            count_related("filtered".to_string(), None)
        } else {
            sum_related(
                "filtered".to_string(),
                RelatedValueRefSpec::Input {
                    name: "income".to_string(),
                },
                Some(JudgmentExprSpec::Comparison {
                    left: Box::new(integer(1)),
                    op: ComparisonOpSpec::Eq,
                    right: Box::new(integer(1)),
                }),
            )
        };
        rule(
            format!("aggregate_{index:05}"),
            DerivedSemanticsSpec::Scalar { expr },
            Vec::new(),
        )
    }));
    ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation("filtered", "members", filter),
        ],
        derived,
        ..ProgramSpec::default()
    }
}

/// Every predicate rule reads nothing, so all are ready at once and listed by
/// name; every aggregator waits for all of them through the relation, so they
/// follow, although `aggregate_*` sorts first.
fn fan_out_order(aggregators: usize, predicates: usize) -> Vec<String> {
    (0..predicates)
        .map(|index| format!("predicate_{index:05}"))
        .chain((0..aggregators).map(|index| format!("aggregate_{index:05}")))
        .collect()
}

/// m = k = 4,000 took 5.9 s in a release build before relations became
/// nodes (the frozen reference takes about 37 s here in a debug build), and
/// grows as m * k. Linear now, it takes milliseconds even in a debug build;
/// the bound leaves room for a loaded CI runner.
#[test]
fn a_relation_fan_out_orders_in_linear_time() {
    const SIDE: usize = 4_000;
    const BOUND: Duration = Duration::from_secs(5);
    let program = fan_out(SIDE, SIDE);
    let started = Instant::now();
    let order = evaluation_order(&program).expect("the fan-out is acyclic");
    let elapsed = started.elapsed();
    eprintln!("fan-out m = k = {SIDE}: {elapsed:?}");
    assert_eq!(order, fan_out_order(SIDE, SIDE));
    assert!(
        elapsed < BOUND,
        "ordering {SIDE} rules over a relation reading {SIDE} rules took {elapsed:?}"
    );
}

#[test]
fn a_small_fan_out_orders_as_the_reference_does() {
    let program = fan_out(40, 30);
    let order = evaluation_order(&program).expect("the fan-out is acyclic");
    assert_eq!(order, fan_out_order(40, 30));
    assert_eq!(
        Ok(order),
        evaluation_order_reference::evaluation_order(&program).map_err(|error| error.to_string())
    );
}

fn scalar_rule(name: &str, expr: ScalarExprSpec) -> DerivedSpec {
    rule(
        name.to_string(),
        DerivedSemanticsSpec::Scalar { expr },
        Vec::new(),
    )
}

fn derived(name: &str) -> ScalarExprSpec {
    ScalarExprSpec::Derived {
        name: name.to_string(),
    }
}

fn assert_matches_reference(program: &ProgramSpec) -> Result<Vec<String>, String> {
    let actual = outcome(evaluation_order(program));
    assert_eq!(
        actual,
        outcome(evaluation_order_reference::evaluation_order(program))
    );
    actual
}

/// `total` reads `x` directly and through `R`, whose predicate also reads
/// `y`: it waits for both once, and is ready when the later of them is
/// listed, not before.
#[test]
fn a_rule_reading_a_rule_directly_and_through_a_relation_waits_for_it_once() {
    let program = ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation(
                "R",
                "members",
                JudgmentExprSpec::And {
                    items: vec![reads("x"), reads("y")],
                },
            ),
        ],
        derived: vec![
            scalar_rule(
                "total",
                ScalarExprSpec::Add {
                    items: vec![derived("x"), count_related("R".to_string(), None)],
                },
            ),
            scalar_rule("y", derived("x")),
            scalar_rule("x", integer(1)),
            scalar_rule("a", integer(1)),
        ],
        ..ProgramSpec::default()
    };
    assert_eq!(
        assert_matches_reference(&program),
        Ok(vec![
            "a".to_string(),
            "x".to_string(),
            "y".to_string(),
            "total".to_string()
        ])
    );
}

/// A base relation, a relation the program does not declare, and a derived
/// relation whose predicate reads no rule hold no rule back.
#[test]
fn aggregating_over_relations_that_read_no_rules_waits_for_nothing() {
    let program = ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation(
                "adults",
                "members",
                JudgmentExprSpec::Comparison {
                    left: Box::new(ScalarExprSpec::Input {
                        name: "age".to_string(),
                    }),
                    op: ComparisonOpSpec::Gte,
                    right: Box::new(integer(18)),
                },
            ),
        ],
        derived: vec![
            scalar_rule("z", integer(1)),
            scalar_rule("c", count_related("adults".to_string(), None)),
            scalar_rule("b", count_related("undeclared".to_string(), None)),
            scalar_rule("a", count_related("members".to_string(), None)),
        ],
        ..ProgramSpec::default()
    };
    assert_eq!(
        assert_matches_reference(&program),
        Ok(["a", "b", "c", "z"].map(String::from).to_vec())
    );
}

/// `e` counts over `R`, `R`'s predicate reads `f`, and `f` reads `e`; `g`
/// reads `e` and so never becomes ready either. The report names all three,
/// not `ok`, which is ordered.
#[test]
fn a_cycle_through_a_relation_reports_the_rules_on_and_after_it() {
    let program = ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation("R", "members", reads("f")),
        ],
        derived: vec![
            scalar_rule("e", count_related("R".to_string(), None)),
            scalar_rule("f", derived("e")),
            scalar_rule("g", derived("e")),
            scalar_rule("ok", integer(1)),
        ],
        ..ProgramSpec::default()
    };
    assert_eq!(
        assert_matches_reference(&program),
        Err(format!(
            "{:?}",
            CompileError::CyclicDependency {
                cycle: "e, f, g".to_string()
            }
        ))
    );
}

/// The order follows only the predicate of the relation a rule aggregates
/// over. Here `e` counts over `R`, which sources `S`, whose predicate reads
/// `e`: the order lists `e` and then `d`, which reads it, and the routed
/// check, now given only the rule-to-rule edges, still refuses both.
#[test]
fn a_cycle_only_the_routed_check_sees_is_still_refused() {
    let program = ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation("S", "members", reads("e")),
            derived_relation("R", "S", JudgmentExprSpec::And { items: Vec::new() }),
        ],
        derived: vec![
            scalar_rule("e", count_related("R".to_string(), None)),
            scalar_rule("d", derived("e")),
        ],
        ..ProgramSpec::default()
    };
    assert_eq!(
        assert_matches_reference(&program),
        Err(format!(
            "{:?}",
            CompileError::CyclicDependency {
                cycle: "d, e".to_string()
            }
        ))
    );
}

/// The first rule in program order with an unknown dependency reports its
/// smallest one, though the relation it counts over adds known rules that
/// sort earlier.
#[test]
fn an_unknown_dependency_is_reported_as_before() {
    let program = ProgramSpec {
        relations: vec![
            base_relation("members"),
            derived_relation("R", "members", reads("a")),
        ],
        derived: vec![
            scalar_rule("a", integer(1)),
            scalar_rule(
                "total",
                ScalarExprSpec::Add {
                    items: vec![
                        derived("zz"),
                        derived("m"),
                        count_related("R".to_string(), None),
                    ],
                },
            ),
            scalar_rule("later", derived("b")),
        ],
        ..ProgramSpec::default()
    };
    assert_eq!(
        assert_matches_reference(&program),
        Err(format!(
            "{:?}",
            CompileError::UnknownDerivedDependency {
                derived: "total".to_string(),
                dependency: "m".to_string()
            }
        ))
    );
}
