//! Differential parity suite for the three evaluators.
//!
//! `docs/execution-semantics.md` is the contract under test. Explain
//! (`src/engine.rs`) is the reference: `if`/`match` are lazy, `and`/`or`
//! short-circuit, division evaluates its divisor first, and an error counts only
//! for the (query, output) whose evaluation reaches it. Fast (`src/bulk.rs`,
//! reached through `execute_request` in `fast` mode) must return exactly what
//! explain returns, value kinds and errors included, or decline and fall back to
//! explain. Dense (`src/dense.rs`) must hold explain's value on every row and
//! fail exactly when explain fails for some row, within its documented
//! representation limits.
//!
//! The file has two halves.
//!
//! * **Random properties.** A seeded proptest runner generates small, well-typed
//!   programs (a DAG of one to four household rules plus optional person rules)
//!   over integer and decimal inputs, flags, text, literals that include zero,
//!   negatives and fractions, `if` nesting, `and`/`or`/`not`, comparisons,
//!   arithmetic with divisors that are often zero, `max`/`min`/`ceil`/`floor`,
//!   indexed parameter lookups with missing and fractional keys, `count`/`sum`
//!   over a members relation with `where` clauses, and `input_or_else`. Datasets
//!   drop input records per row (or per column for dense), rows request
//!   different outputs, and person rows can share the batch. Every case runs
//!   through explain, fast and (for the dense-compatible profile) dense, and the
//!   normalized outcomes must agree. Failures are shrunk by proptest and printed
//!   as a formula-level rendering plus the exact request JSON, ready to paste
//!   into a regression test.
//! * **Named regression tests** for every divergence found in the 2026-09-24
//!   review and for the real `rulespec-us` shapes that hit them. They are not
//!   `#[ignore]`d: they are the acceptance tests for active-row masking.
//!
//! Environment knobs (all optional):
//!
//! * `AXIOM_PARITY_CASES=<n>`: cases per property (default [`DEFAULT_CASES`]).
//! * `AXIOM_PARITY_SEED=<u64>`: base seed for the runners (default fixed, so CI
//!   is deterministic).
//! * `AXIOM_PARITY_REPORT_ONLY=1`: do not fail; run every case, print how many
//!   diverged and the first few (unshrunk) divergences. Useful for measuring a
//!   partial fix.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionRequest,
    ExecutionResponse, OutputValue, RulePin, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{DType, Period, PeriodKind};
use axiom_rules_engine::spec::{
    ComparisonOpSpec, DTypeSpec, DatasetSpec, DerivedSemanticsSpec, DerivedSpec,
    IndexedParameterSpec, InputRecordSpec, IntervalSpec, JudgmentExprSpec, JudgmentOutcomeSpec,
    ParameterVersionSpec, PeriodKindSpec, PeriodSpec, ProgramSpec, RelatedValueRefSpec,
    RelationRecordSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::strategy::Union;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner};
use rust_decimal::Decimal;

// ===========================================================================
// Fixed vocabulary
// ===========================================================================

const HOUSEHOLD: &str = "Household";
const PERSON: &str = "Person";
/// `member_of_household(person, household)`; aggregations read it from the
/// household side (current slot 1) to the person side (related slot 0).
const MEMBERS: &str = "member_of_household";
const INT_TABLE: &str = "int_table";
const DEC_TABLE: &str = "dec_table";

/// Literal pool. Integer-looking entries keep integer kind only in profiles
/// that exercise kinds; elsewhere they lower to decimal literals.
const NUM_LITERALS: [&str; 9] = ["0", "1", "2", "-1", "3", "10", "0.5", "2.5", "-1.5"];
/// Input value pools. Zero appears twice so divisors are often zero.
const INT_VALUES: [i64; 7] = [0, 0, 1, 2, 3, -1, 10];
const DEC_VALUES: [&str; 7] = ["0", "0", "0.5", "1", "2.5", "-1.5", "10"];
const TEXT_VALUES: [&str; 2] = ["a", "b"];

const HH_NUM_INPUTS: u8 = 3; // x0, x1, x2
const HH_FLAG_INPUTS: u8 = 2; // f0, f1
const P_NUM_INPUTS: u8 = 2; // px0, px1
const MAX_RULES: u8 = 4;
const MAX_PERSON_RULES: u8 = 2;

/// Cases per property when `AXIOM_PARITY_CASES` is unset. Chosen so the whole
/// file stays well under a minute in a debug `cargo test` on a laptop; run
/// with `AXIOM_PARITY_CASES=5000` or more for a deeper local search.
const DEFAULT_CASES: u32 = 500;
const DEFAULT_SEED: u64 = 0x5eed_a110_c0de_2026;

// ===========================================================================
// Generator IR
//
// Strategies generate this context-free IR; `lower` resolves it against the
// rules declared so far. References to earlier rules are indices taken modulo
// the rules of the right type that exist (falling back to a literal or a flag
// when none do), so every generated program is a well-typed DAG and shrinking
// never produces a dangling reference.
// ===========================================================================

#[derive(Clone, Debug)]
enum NumG {
    Lit(u8),
    Input(u8),
    InputOrElse(u8, u8),
    Rule(u8),
    Count(Option<PBoolG>),
    Sum(PValG, Option<PBoolG>),
    Param(bool, Box<NumG>),
    Add(Vec<NumG>),
    Sub(Box<NumG>, Box<NumG>),
    Mul(Box<NumG>, Box<NumG>),
    Div(Box<NumG>, Box<NumG>),
    Max(Vec<NumG>),
    Min(Vec<NumG>),
    Ceil(Box<NumG>),
    Floor(Box<NumG>),
    If(Box<BoolG>, Box<NumG>, Box<NumG>),
}

#[derive(Clone, Debug)]
enum BoolG {
    /// `f{i} == b`
    Flag(u8, bool),
    Cmp(Box<NumG>, ComparisonOpSpec, Box<NumG>),
    /// `t0 == TEXT[i]` (or `!=`)
    Text(u8, bool),
    /// An earlier judgment rule.
    Rule(u8),
    /// `bool_rule == b` for an earlier Bool scalar rule.
    BoolRule(u8, bool),
    /// `text_rule == TEXT[i]` (or `!=`) for an earlier Text scalar rule.
    TextRule(u8, u8, bool),
    /// `f{i} <op> true` with an ordering operator: a type error in explain.
    IllTyped(u8, ComparisonOpSpec),
    And(Vec<BoolG>),
    Or(Vec<BoolG>),
    Not(Box<BoolG>),
}

#[derive(Clone, Debug)]
enum BoolValG {
    Lit(bool),
    Flag(u8),
    Rule(u8),
    If(Box<BoolG>, Box<BoolValG>, Box<BoolValG>),
}

#[derive(Clone, Debug)]
enum TextValG {
    Lit(u8),
    Input,
    Rule(u8),
    If(Box<BoolG>, Box<TextValG>, Box<TextValG>),
}

/// Person-level scalar expression (relation `where` clauses, summed values and
/// person rules).
#[derive(Clone, Debug)]
enum PNumG {
    Lit(u8),
    Input(u8),
    InputOrElse(u8, u8),
    Rule(u8),
    Add(Vec<PNumG>),
    Sub(Box<PNumG>, Box<PNumG>),
    Mul(Box<PNumG>, Box<PNumG>),
    Div(Box<PNumG>, Box<PNumG>),
    Max(Vec<PNumG>),
    If(Box<PBoolG>, Box<PNumG>, Box<PNumG>),
}

#[derive(Clone, Debug)]
enum PBoolG {
    Flag(bool),
    Cmp(Box<PNumG>, ComparisonOpSpec, Box<PNumG>),
    Rule(u8),
    And(Vec<PBoolG>),
    Or(Vec<PBoolG>),
    Not(Box<PBoolG>),
}

#[derive(Clone, Debug)]
enum PValG {
    Input(u8),
    Rule(u8),
}

#[derive(Clone, Debug)]
enum RuleG {
    Num { expr: NumG, integer_dtype: bool },
    Judg(BoolG),
    Bool(BoolValG),
    Text(TextValG),
}

#[derive(Clone, Debug)]
enum PersonRuleG {
    Num(PNumG),
    Judg(PBoolG),
}

#[derive(Clone, Debug)]
struct ProgramG {
    rules: Vec<RuleG>,
    person_rules: Vec<PersonRuleG>,
    /// Per household numeric input: integer kind (true) or decimal kind.
    integer_inputs: [bool; 3],
}

#[derive(Clone, Debug)]
struct PersonG {
    nums: [u8; 2],
    num_present: [bool; 2],
    flag: bool,
    flag_present: bool,
}

#[derive(Clone, Debug)]
struct RowG {
    nums: [u8; 3],
    num_present: [bool; 3],
    flags: [bool; 2],
    flag_present: [bool; 2],
    text: u8,
    text_present: bool,
    members: Vec<PersonG>,
}

/// Whole-column presence, used instead of per-row presence by dense profiles.
#[derive(Clone, Debug)]
struct ColumnsG {
    nums: [bool; 3],
    flags: [bool; 2],
    text: bool,
    person_nums: [bool; 2],
    person_flag: bool,
}

#[derive(Clone, Debug)]
struct RequestsG {
    /// Outputs every row requests (uniform profiles).
    shared: Vec<u8>,
    /// Outputs per row (mixed-request profiles); indexed by row.
    per_row: Vec<Vec<u8>>,
    /// `(row, member, person rules)` person queries appended to the batch.
    person_queries: Vec<(u8, u8, Vec<u8>)>,
    /// Sort keys for the metamorphic permutation check.
    order_keys: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CaseG {
    program: ProgramG,
    rows: Vec<RowG>,
    columns: ColumnsG,
    requests: RequestsG,
    /// `(rule, literal)` pin for the pin property.
    pin: (u8, u8),
}

// ===========================================================================
// Profiles
// ===========================================================================

#[derive(Clone, Copy, Debug)]
struct Profile {
    name: &'static str,
    /// Integer literals, inputs, table values and counts keep integer kind and
    /// numeric rules declare `integer` or `decimal` at random. Off: every
    /// numeric leaf is a decimal, so kind drift cannot mask evaluation-order
    /// divergences.
    integer_kinds: bool,
    relations: bool,
    parameters: bool,
    text_and_bool_rules: bool,
    /// Ordering comparisons on flags (a type error in explain).
    ill_typed_comparisons: bool,
    /// Rows request different outputs and person rows can join the batch.
    mixed_requests: bool,
    /// Drop input records per row. Off: drop whole columns (dense datasets).
    row_missing_inputs: bool,
}

/// Evaluation order in isolation: lazy `if`, short-circuit `and`/`or`, divisor
/// first, derived rules reached only through dead branches, per-row missing
/// inputs. Every numeric value is a decimal and every row requests the same
/// outputs.
const EVAL_ORDER: Profile = Profile {
    name: "eval-order",
    integer_kinds: false,
    relations: false,
    parameters: false,
    text_and_bool_rules: true,
    ill_typed_comparisons: false,
    mixed_requests: false,
    row_missing_inputs: true,
};

/// Everything the generator knows.
const FULL: Profile = Profile {
    name: "full",
    integer_kinds: true,
    relations: true,
    parameters: true,
    text_and_bool_rules: true,
    ill_typed_comparisons: true,
    mixed_requests: true,
    row_missing_inputs: true,
};

/// The dense-compatible subset: every row requests every household rule,
/// inputs are present or absent as whole columns, no parameter outputs.
const DENSE: Profile = Profile {
    name: "dense",
    integer_kinds: true,
    relations: true,
    parameters: true,
    text_and_bool_rules: true,
    ill_typed_comparisons: true,
    mixed_requests: false,
    row_missing_inputs: false,
};

// ===========================================================================
// Strategies
// ===========================================================================

fn weighted<T: std::fmt::Debug + 'static>(
    alternatives: Vec<(u32, BoxedStrategy<T>)>,
) -> BoxedStrategy<T> {
    Union::new_weighted(alternatives).boxed()
}

fn any_op() -> BoxedStrategy<ComparisonOpSpec> {
    prop_oneof![
        Just(ComparisonOpSpec::Eq),
        Just(ComparisonOpSpec::Ne),
        Just(ComparisonOpSpec::Lt),
        Just(ComparisonOpSpec::Lte),
        Just(ComparisonOpSpec::Gt),
        Just(ComparisonOpSpec::Gte),
    ]
    .boxed()
}

fn ordering_op() -> BoxedStrategy<ComparisonOpSpec> {
    prop_oneof![
        Just(ComparisonOpSpec::Lt),
        Just(ComparisonOpSpec::Lte),
        Just(ComparisonOpSpec::Gt),
        Just(ComparisonOpSpec::Gte),
    ]
    .boxed()
}

fn literal_index() -> BoxedStrategy<u8> {
    (0..NUM_LITERALS.len() as u8).boxed()
}

fn pnum_strategy() -> BoxedStrategy<PNumG> {
    let leaf = weighted(vec![
        (3, literal_index().prop_map(PNumG::Lit).boxed()),
        (5, (0..P_NUM_INPUTS).prop_map(PNumG::Input).boxed()),
        (
            1,
            (0..P_NUM_INPUTS, literal_index())
                .prop_map(|(input, default)| PNumG::InputOrElse(input, default))
                .boxed(),
        ),
        (2, (0..MAX_PERSON_RULES).prop_map(PNumG::Rule).boxed()),
    ]);
    leaf.prop_recursive(2, 8, 2, |inner| {
        let condition = pbool_over(inner.clone());
        weighted(vec![
            (2, vec(inner.clone(), 1..=2).prop_map(PNumG::Add).boxed()),
            (
                1,
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| PNumG::Sub(Box::new(a), Box::new(b)))
                    .boxed(),
            ),
            (
                1,
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| PNumG::Mul(Box::new(a), Box::new(b)))
                    .boxed(),
            ),
            (
                3,
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| PNumG::Div(Box::new(a), Box::new(b)))
                    .boxed(),
            ),
            (1, vec(inner.clone(), 1..=2).prop_map(PNumG::Max).boxed()),
            (
                2,
                (condition, inner.clone(), inner)
                    .prop_map(|(c, a, b)| PNumG::If(Box::new(c), Box::new(a), Box::new(b)))
                    .boxed(),
            ),
        ])
    })
    .boxed()
}

fn pbool_over(pnum: BoxedStrategy<PNumG>) -> BoxedStrategy<PBoolG> {
    let leaf = weighted(vec![
        (2, any::<bool>().prop_map(PBoolG::Flag).boxed()),
        (
            4,
            (pnum.clone(), any_op(), pnum)
                .prop_map(|(a, op, b)| PBoolG::Cmp(Box::new(a), op, Box::new(b)))
                .boxed(),
        ),
        (1, (0..MAX_PERSON_RULES).prop_map(PBoolG::Rule).boxed()),
    ]);
    leaf.prop_recursive(1, 4, 2, |inner| {
        weighted(vec![
            (2, vec(inner.clone(), 1..=2).prop_map(PBoolG::And).boxed()),
            (2, vec(inner.clone(), 1..=2).prop_map(PBoolG::Or).boxed()),
            (
                1,
                inner.prop_map(|item| PBoolG::Not(Box::new(item))).boxed(),
            ),
        ])
    })
    .boxed()
}

fn pbool_strategy() -> BoxedStrategy<PBoolG> {
    pbool_over(pnum_strategy())
}

fn pval_strategy() -> BoxedStrategy<PValG> {
    prop_oneof![
        (0..P_NUM_INPUTS).prop_map(PValG::Input),
        (0..MAX_PERSON_RULES).prop_map(PValG::Rule),
    ]
    .boxed()
}

fn num_strategy(profile: Profile) -> BoxedStrategy<NumG> {
    let mut leaves = vec![
        (4, literal_index().prop_map(NumG::Lit).boxed()),
        (6, (0..HH_NUM_INPUTS).prop_map(NumG::Input).boxed()),
        (
            1,
            (0..HH_NUM_INPUTS, literal_index())
                .prop_map(|(input, default)| NumG::InputOrElse(input, default))
                .boxed(),
        ),
        (3, (0..MAX_RULES).prop_map(NumG::Rule).boxed()),
    ];
    if profile.relations {
        leaves.push((
            1,
            proptest::option::of(pbool_strategy())
                .prop_map(NumG::Count)
                .boxed(),
        ));
        leaves.push((
            1,
            (pval_strategy(), proptest::option::of(pbool_strategy()))
                .prop_map(|(value, predicate)| NumG::Sum(value, predicate))
                .boxed(),
        ));
    }
    weighted(leaves)
        .prop_recursive(3, 24, 3, move |inner| {
            let condition = bool_over(profile, inner.clone());
            let pair = || (inner.clone(), inner.clone());
            let mut branches = vec![
                (2, vec(inner.clone(), 1..=3).prop_map(NumG::Add).boxed()),
                (
                    1,
                    pair()
                        .prop_map(|(a, b)| NumG::Sub(Box::new(a), Box::new(b)))
                        .boxed(),
                ),
                (
                    1,
                    pair()
                        .prop_map(|(a, b)| NumG::Mul(Box::new(a), Box::new(b)))
                        .boxed(),
                ),
                (
                    4,
                    pair()
                        .prop_map(|(a, b)| NumG::Div(Box::new(a), Box::new(b)))
                        .boxed(),
                ),
                (1, vec(inner.clone(), 1..=3).prop_map(NumG::Max).boxed()),
                (1, vec(inner.clone(), 1..=3).prop_map(NumG::Min).boxed()),
                (
                    1,
                    inner
                        .clone()
                        .prop_map(|value| NumG::Ceil(Box::new(value)))
                        .boxed(),
                ),
                (
                    1,
                    inner
                        .clone()
                        .prop_map(|value| NumG::Floor(Box::new(value)))
                        .boxed(),
                ),
                (
                    5,
                    (condition, inner.clone(), inner.clone())
                        .prop_map(|(c, a, b)| NumG::If(Box::new(c), Box::new(a), Box::new(b)))
                        .boxed(),
                ),
            ];
            if profile.parameters {
                branches.push((
                    1,
                    (any::<bool>(), inner.clone())
                        .prop_map(|(integer_table, index)| {
                            NumG::Param(integer_table, Box::new(index))
                        })
                        .boxed(),
                ));
            }
            weighted(branches)
        })
        .boxed()
}

fn bool_over(profile: Profile, num: BoxedStrategy<NumG>) -> BoxedStrategy<BoolG> {
    let mut leaves = vec![
        (
            3,
            (0..HH_FLAG_INPUTS, any::<bool>())
                .prop_map(|(flag, value)| BoolG::Flag(flag, value))
                .boxed(),
        ),
        (
            6,
            (num.clone(), any_op(), num)
                .prop_map(|(a, op, b)| BoolG::Cmp(Box::new(a), op, Box::new(b)))
                .boxed(),
        ),
        (2, (0..MAX_RULES).prop_map(BoolG::Rule).boxed()),
    ];
    if profile.text_and_bool_rules {
        leaves.push((
            1,
            (0..TEXT_VALUES.len() as u8, any::<bool>())
                .prop_map(|(text, eq)| BoolG::Text(text, eq))
                .boxed(),
        ));
        leaves.push((
            1,
            (0..MAX_RULES, any::<bool>())
                .prop_map(|(rule, value)| BoolG::BoolRule(rule, value))
                .boxed(),
        ));
        leaves.push((
            1,
            (0..MAX_RULES, 0..TEXT_VALUES.len() as u8, any::<bool>())
                .prop_map(|(rule, text, eq)| BoolG::TextRule(rule, text, eq))
                .boxed(),
        ));
    }
    if profile.ill_typed_comparisons {
        leaves.push((
            1,
            (0..HH_FLAG_INPUTS, ordering_op())
                .prop_map(|(flag, op)| BoolG::IllTyped(flag, op))
                .boxed(),
        ));
    }
    weighted(leaves)
        .prop_recursive(2, 6, 2, |inner| {
            weighted(vec![
                (2, vec(inner.clone(), 1..=3).prop_map(BoolG::And).boxed()),
                (2, vec(inner.clone(), 1..=3).prop_map(BoolG::Or).boxed()),
                (1, inner.prop_map(|item| BoolG::Not(Box::new(item))).boxed()),
            ])
        })
        .boxed()
}

fn bool_value_strategy(profile: Profile, num: BoxedStrategy<NumG>) -> BoxedStrategy<BoolValG> {
    let leaf = weighted(vec![
        (2, any::<bool>().prop_map(BoolValG::Lit).boxed()),
        (3, (0..HH_FLAG_INPUTS).prop_map(BoolValG::Flag).boxed()),
        (1, (0..MAX_RULES).prop_map(BoolValG::Rule).boxed()),
    ]);
    leaf.prop_recursive(2, 6, 2, move |inner| {
        (bool_over(profile, num.clone()), inner.clone(), inner)
            .prop_map(|(c, a, b)| BoolValG::If(Box::new(c), Box::new(a), Box::new(b)))
    })
    .boxed()
}

fn text_value_strategy(profile: Profile, num: BoxedStrategy<NumG>) -> BoxedStrategy<TextValG> {
    let leaf = weighted(vec![
        (
            2,
            (0..TEXT_VALUES.len() as u8).prop_map(TextValG::Lit).boxed(),
        ),
        (3, Just(TextValG::Input).boxed()),
        (1, (0..MAX_RULES).prop_map(TextValG::Rule).boxed()),
    ]);
    leaf.prop_recursive(2, 6, 2, move |inner| {
        (bool_over(profile, num.clone()), inner.clone(), inner)
            .prop_map(|(c, a, b)| TextValG::If(Box::new(c), Box::new(a), Box::new(b)))
    })
    .boxed()
}

fn rule_strategy(profile: Profile) -> BoxedStrategy<RuleG> {
    let num = num_strategy(profile);
    let mut alternatives = vec![
        (
            6,
            (num.clone(), any::<bool>())
                .prop_map(|(expr, integer_dtype)| RuleG::Num {
                    expr,
                    integer_dtype,
                })
                .boxed(),
        ),
        (
            3,
            bool_over(profile, num.clone())
                .prop_map(RuleG::Judg)
                .boxed(),
        ),
    ];
    if profile.text_and_bool_rules {
        alternatives.push((
            1,
            bool_value_strategy(profile, num.clone())
                .prop_map(RuleG::Bool)
                .boxed(),
        ));
        alternatives.push((
            1,
            text_value_strategy(profile, num)
                .prop_map(RuleG::Text)
                .boxed(),
        ));
    }
    weighted(alternatives)
}

fn person_rule_strategy() -> BoxedStrategy<PersonRuleG> {
    prop_oneof![
        2 => pnum_strategy().prop_map(PersonRuleG::Num),
        1 => pbool_strategy().prop_map(PersonRuleG::Judg),
    ]
    .boxed()
}

fn program_strategy(profile: Profile) -> BoxedStrategy<ProgramG> {
    let person_rules = if profile.relations {
        vec(person_rule_strategy(), 0..=MAX_PERSON_RULES as usize).boxed()
    } else {
        Just(Vec::new()).boxed()
    };
    (
        vec(rule_strategy(profile), 1..=MAX_RULES as usize),
        person_rules,
        proptest::array::uniform3(any::<bool>()),
    )
        .prop_map(|(rules, person_rules, integer_inputs)| ProgramG {
            rules,
            person_rules,
            integer_inputs,
        })
        .boxed()
}

fn value_index() -> BoxedStrategy<u8> {
    (0..INT_VALUES.len() as u8).boxed()
}

fn present() -> BoxedStrategy<bool> {
    proptest::bool::weighted(0.95).boxed()
}

fn person_strategy() -> BoxedStrategy<PersonG> {
    (
        proptest::array::uniform2(value_index()),
        proptest::array::uniform2(present()),
        any::<bool>(),
        present(),
    )
        .prop_map(|(nums, num_present, flag, flag_present)| PersonG {
            nums,
            num_present,
            flag,
            flag_present,
        })
        .boxed()
}

fn row_strategy(profile: Profile) -> BoxedStrategy<RowG> {
    let members = if profile.relations {
        vec(person_strategy(), 0..=3).boxed()
    } else {
        Just(Vec::new()).boxed()
    };
    (
        proptest::array::uniform3(value_index()),
        proptest::array::uniform3(present()),
        proptest::array::uniform2(any::<bool>()),
        proptest::array::uniform2(present()),
        0..TEXT_VALUES.len() as u8,
        present(),
        members,
    )
        .prop_map(
            |(nums, num_present, flags, flag_present, text, text_present, members)| RowG {
                nums,
                num_present,
                flags,
                flag_present,
                text,
                text_present,
                members,
            },
        )
        .boxed()
}

fn columns_strategy() -> BoxedStrategy<ColumnsG> {
    (
        proptest::array::uniform3(present()),
        proptest::array::uniform2(present()),
        present(),
        proptest::array::uniform2(present()),
        present(),
    )
        .prop_map(|(nums, flags, text, person_nums, person_flag)| ColumnsG {
            nums,
            flags,
            text,
            person_nums,
            person_flag,
        })
        .boxed()
}

fn requests_strategy() -> BoxedStrategy<RequestsG> {
    let outputs = || vec(0..MAX_RULES, 1..=3);
    (
        outputs(),
        vec(outputs(), 6),
        vec((0..6u8, 0..3u8, vec(0..MAX_PERSON_RULES, 1..=2)), 0..=2),
        vec(any::<u8>(), 12),
    )
        .prop_map(|(shared, per_row, person_queries, order_keys)| RequestsG {
            shared,
            per_row,
            person_queries,
            order_keys,
        })
        .boxed()
}

fn case_strategy(profile: Profile) -> BoxedStrategy<CaseG> {
    (
        program_strategy(profile),
        vec(row_strategy(profile), 1..=4),
        columns_strategy(),
        requests_strategy(),
        (any::<u8>(), literal_index()),
    )
        .prop_map(|(program, rows, columns, requests, pin)| CaseG {
            program,
            rows,
            columns,
            requests,
            pin,
        })
        .boxed()
}

// ===========================================================================
// Lowering IR -> ProgramSpec / DatasetSpec / queries / dense batch
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuleTag {
    Num,
    Judg,
    Bool,
    Text,
}

struct Cx<'a> {
    profile: Profile,
    /// Household rules declared before the rule being lowered.
    earlier: &'a [(String, RuleTag)],
    /// Person rules visible from here.
    person: &'a [(String, RuleTag)],
    /// Rewrite every lazy construct so explain evaluates all of its operands
    /// (used only to measure how often a case hides an error in a dead branch).
    force: bool,
}

impl Cx<'_> {
    fn pick(rules: &[(String, RuleTag)], tag: RuleTag, index: u8) -> Option<&str> {
        let candidates = rules
            .iter()
            .filter(|(_, rule_tag)| *rule_tag == tag)
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            None
        } else {
            Some(candidates[index as usize % candidates.len()])
        }
    }
}

fn num_literal(index: u8, integer_kinds: bool) -> ScalarValueSpec {
    let text = NUM_LITERALS[index as usize % NUM_LITERALS.len()];
    if integer_kinds && !text.contains('.') {
        ScalarValueSpec::Integer {
            value: text.parse().expect("integer literal"),
        }
    } else {
        ScalarValueSpec::Decimal {
            value: text.to_string(),
        }
    }
}

fn lit(value: ScalarValueSpec) -> ScalarExprSpec {
    ScalarExprSpec::Literal { value }
}

fn dec_lit(value: &str) -> ScalarExprSpec {
    lit(ScalarValueSpec::Decimal {
        value: value.to_string(),
    })
}

fn int_lit(value: i64) -> ScalarExprSpec {
    lit(ScalarValueSpec::Integer { value })
}

fn bool_lit(value: bool) -> ScalarExprSpec {
    lit(ScalarValueSpec::Bool { value })
}

fn text_lit(value: &str) -> ScalarExprSpec {
    lit(ScalarValueSpec::Text {
        value: value.to_string(),
    })
}

fn input(name: &str) -> ScalarExprSpec {
    ScalarExprSpec::Input {
        name: name.to_string(),
    }
}

fn derived(name: &str) -> ScalarExprSpec {
    ScalarExprSpec::Derived {
        name: name.to_string(),
    }
}

fn cmp(left: ScalarExprSpec, op: ComparisonOpSpec, right: ScalarExprSpec) -> JudgmentExprSpec {
    JudgmentExprSpec::Comparison {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

fn if_expr(
    condition: JudgmentExprSpec,
    then_expr: ScalarExprSpec,
    else_expr: ScalarExprSpec,
) -> ScalarExprSpec {
    ScalarExprSpec::If {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    }
}

fn div(left: ScalarExprSpec, right: ScalarExprSpec) -> ScalarExprSpec {
    ScalarExprSpec::Div {
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn always() -> JudgmentExprSpec {
    cmp(int_lit(0), ComparisonOpSpec::Eq, int_lit(0))
}

/// `and(or(x1, T), ..., or(xn, T), inner)`: evaluates every item, then yields
/// `inner`'s outcome.
fn force_all(items: &[JudgmentExprSpec], inner: JudgmentExprSpec) -> JudgmentExprSpec {
    let mut forced = items
        .iter()
        .map(|item| JudgmentExprSpec::Or {
            items: vec![item.clone(), always()],
        })
        .collect::<Vec<_>>();
    forced.push(inner);
    JudgmentExprSpec::And { items: forced }
}

/// `num` evaluated for its errors only: `a + 0 * b`.
fn force_numeric_pair(keep: ScalarExprSpec, also: ScalarExprSpec) -> ScalarExprSpec {
    ScalarExprSpec::Add {
        items: vec![
            keep,
            ScalarExprSpec::Mul {
                left: Box::new(dec_lit("0")),
                right: Box::new(also),
            },
        ],
    }
}

fn hh_num_input(index: u8) -> String {
    format!("x{}", index % HH_NUM_INPUTS)
}

fn hh_flag_input(index: u8) -> String {
    format!("f{}", index % HH_FLAG_INPUTS)
}

fn person_num_input(index: u8) -> String {
    format!("px{}", index % P_NUM_INPUTS)
}

const TEXT_INPUT: &str = "t0";
const PERSON_FLAG_INPUT: &str = "pf0";

fn lower_num(expr: &NumG, cx: &Cx<'_>) -> ScalarExprSpec {
    let integer_kinds = cx.profile.integer_kinds;
    match expr {
        NumG::Lit(index) => lit(num_literal(*index, integer_kinds)),
        NumG::Input(index) => input(&hh_num_input(*index)),
        NumG::InputOrElse(index, default) => ScalarExprSpec::InputOrElse {
            name: hh_num_input(*index),
            default: num_literal(*default, integer_kinds),
        },
        NumG::Rule(index) => match Cx::pick(cx.earlier, RuleTag::Num, *index) {
            Some(name) => derived(name),
            None => lit(num_literal(*index, integer_kinds)),
        },
        NumG::Count(predicate) => {
            let count = ScalarExprSpec::CountRelated {
                relation: MEMBERS.to_string(),
                current_slot: 1,
                related_slot: 0,
                where_clause: predicate
                    .as_ref()
                    .map(|predicate| Box::new(lower_pbool(predicate, cx))),
            };
            if integer_kinds {
                count
            } else {
                ScalarExprSpec::Add { items: vec![count] }
            }
        }
        NumG::Sum(value, predicate) => ScalarExprSpec::SumRelated {
            relation: MEMBERS.to_string(),
            current_slot: 1,
            related_slot: 0,
            value: match value {
                PValG::Input(index) => RelatedValueRefSpec::Input {
                    name: person_num_input(*index),
                },
                PValG::Rule(index) => match Cx::pick(cx.person, RuleTag::Num, *index) {
                    Some(name) => RelatedValueRefSpec::Derived {
                        name: name.to_string(),
                    },
                    None => RelatedValueRefSpec::Input {
                        name: person_num_input(*index),
                    },
                },
            },
            where_clause: predicate
                .as_ref()
                .map(|predicate| Box::new(lower_pbool(predicate, cx))),
        },
        NumG::Param(integer_table, index) => ScalarExprSpec::ParameterLookup {
            parameter: if *integer_table && integer_kinds {
                INT_TABLE
            } else {
                DEC_TABLE
            }
            .to_string(),
            index: Box::new(lower_num(index, cx)),
        },
        NumG::Add(items) => ScalarExprSpec::Add {
            items: items.iter().map(|item| lower_num(item, cx)).collect(),
        },
        NumG::Sub(a, b) => ScalarExprSpec::Sub {
            left: Box::new(lower_num(a, cx)),
            right: Box::new(lower_num(b, cx)),
        },
        NumG::Mul(a, b) => ScalarExprSpec::Mul {
            left: Box::new(lower_num(a, cx)),
            right: Box::new(lower_num(b, cx)),
        },
        NumG::Div(a, b) => div(lower_num(a, cx), lower_num(b, cx)),
        NumG::Max(items) => ScalarExprSpec::Max {
            items: items.iter().map(|item| lower_num(item, cx)).collect(),
        },
        NumG::Min(items) => ScalarExprSpec::Min {
            items: items.iter().map(|item| lower_num(item, cx)).collect(),
        },
        NumG::Ceil(value) => ScalarExprSpec::Ceil {
            value: Box::new(lower_num(value, cx)),
        },
        NumG::Floor(value) => ScalarExprSpec::Floor {
            value: Box::new(lower_num(value, cx)),
        },
        NumG::If(condition, a, b) => {
            let condition = lower_bool(condition, cx);
            let a = lower_num(a, cx);
            let b = lower_num(b, cx);
            if cx.force {
                if_expr(
                    condition,
                    force_numeric_pair(a.clone(), b.clone()),
                    force_numeric_pair(b, a),
                )
            } else {
                if_expr(condition, a, b)
            }
        }
    }
}

fn flag_is(flag: u8, value: bool) -> JudgmentExprSpec {
    cmp(
        input(&hh_flag_input(flag)),
        ComparisonOpSpec::Eq,
        bool_lit(value),
    )
}

fn text_op(eq: bool) -> ComparisonOpSpec {
    if eq {
        ComparisonOpSpec::Eq
    } else {
        ComparisonOpSpec::Ne
    }
}

fn lower_bool(expr: &BoolG, cx: &Cx<'_>) -> JudgmentExprSpec {
    match expr {
        BoolG::Flag(flag, value) => flag_is(*flag, *value),
        BoolG::Cmp(a, op, b) => cmp(lower_num(a, cx), *op, lower_num(b, cx)),
        BoolG::Text(text, eq) => cmp(
            input(TEXT_INPUT),
            text_op(*eq),
            text_lit(TEXT_VALUES[*text as usize % TEXT_VALUES.len()]),
        ),
        BoolG::Rule(index) => match Cx::pick(cx.earlier, RuleTag::Judg, *index) {
            Some(name) => JudgmentExprSpec::Derived {
                name: name.to_string(),
            },
            None => flag_is(*index, true),
        },
        BoolG::BoolRule(index, value) => match Cx::pick(cx.earlier, RuleTag::Bool, *index) {
            Some(name) => cmp(derived(name), ComparisonOpSpec::Eq, bool_lit(*value)),
            None => flag_is(*index, *value),
        },
        BoolG::TextRule(index, text, eq) => {
            let text = text_lit(TEXT_VALUES[*text as usize % TEXT_VALUES.len()]);
            match Cx::pick(cx.earlier, RuleTag::Text, *index) {
                Some(name) => cmp(derived(name), text_op(*eq), text),
                None => cmp(input(TEXT_INPUT), text_op(*eq), text),
            }
        }
        BoolG::IllTyped(flag, op) => cmp(input(&hh_flag_input(*flag)), *op, bool_lit(true)),
        BoolG::And(items) => {
            let items = items
                .iter()
                .map(|item| lower_bool(item, cx))
                .collect::<Vec<_>>();
            if cx.force {
                force_all(
                    &items,
                    JudgmentExprSpec::And {
                        items: items.clone(),
                    },
                )
            } else {
                JudgmentExprSpec::And { items }
            }
        }
        BoolG::Or(items) => {
            let items = items
                .iter()
                .map(|item| lower_bool(item, cx))
                .collect::<Vec<_>>();
            if cx.force {
                force_all(
                    &items,
                    JudgmentExprSpec::Or {
                        items: items.clone(),
                    },
                )
            } else {
                JudgmentExprSpec::Or { items }
            }
        }
        BoolG::Not(item) => JudgmentExprSpec::Not {
            item: Box::new(lower_bool(item, cx)),
        },
    }
}

fn lower_bool_value(expr: &BoolValG, cx: &Cx<'_>) -> ScalarExprSpec {
    match expr {
        BoolValG::Lit(value) => bool_lit(*value),
        BoolValG::Flag(flag) => input(&hh_flag_input(*flag)),
        BoolValG::Rule(index) => match Cx::pick(cx.earlier, RuleTag::Bool, *index) {
            Some(name) => derived(name),
            None => bool_lit(index % 2 == 0),
        },
        BoolValG::If(condition, a, b) => {
            let condition = lower_bool(condition, cx);
            let a = lower_bool_value(a, cx);
            let b = lower_bool_value(b, cx);
            let condition = if cx.force {
                force_all(
                    &[
                        cmp(a.clone(), ComparisonOpSpec::Eq, bool_lit(true)),
                        cmp(b.clone(), ComparisonOpSpec::Eq, bool_lit(true)),
                    ],
                    condition,
                )
            } else {
                condition
            };
            if_expr(condition, a, b)
        }
    }
}

fn lower_text_value(expr: &TextValG, cx: &Cx<'_>) -> ScalarExprSpec {
    match expr {
        TextValG::Lit(index) => text_lit(TEXT_VALUES[*index as usize % TEXT_VALUES.len()]),
        TextValG::Input => input(TEXT_INPUT),
        TextValG::Rule(index) => match Cx::pick(cx.earlier, RuleTag::Text, *index) {
            Some(name) => derived(name),
            None => text_lit(TEXT_VALUES[*index as usize % TEXT_VALUES.len()]),
        },
        TextValG::If(condition, a, b) => {
            let condition = lower_bool(condition, cx);
            let a = lower_text_value(a, cx);
            let b = lower_text_value(b, cx);
            let condition = if cx.force {
                force_all(
                    &[
                        cmp(a.clone(), ComparisonOpSpec::Eq, text_lit("a")),
                        cmp(b.clone(), ComparisonOpSpec::Eq, text_lit("a")),
                    ],
                    condition,
                )
            } else {
                condition
            };
            if_expr(condition, a, b)
        }
    }
}

fn lower_pnum(expr: &PNumG, cx: &Cx<'_>) -> ScalarExprSpec {
    let integer_kinds = cx.profile.integer_kinds;
    match expr {
        PNumG::Lit(index) => lit(num_literal(*index, integer_kinds)),
        PNumG::Input(index) => input(&person_num_input(*index)),
        PNumG::InputOrElse(index, default) => ScalarExprSpec::InputOrElse {
            name: person_num_input(*index),
            default: num_literal(*default, integer_kinds),
        },
        PNumG::Rule(index) => match Cx::pick(cx.person, RuleTag::Num, *index) {
            Some(name) => derived(name),
            None => lit(num_literal(*index, integer_kinds)),
        },
        PNumG::Add(items) => ScalarExprSpec::Add {
            items: items.iter().map(|item| lower_pnum(item, cx)).collect(),
        },
        PNumG::Sub(a, b) => ScalarExprSpec::Sub {
            left: Box::new(lower_pnum(a, cx)),
            right: Box::new(lower_pnum(b, cx)),
        },
        PNumG::Mul(a, b) => ScalarExprSpec::Mul {
            left: Box::new(lower_pnum(a, cx)),
            right: Box::new(lower_pnum(b, cx)),
        },
        PNumG::Div(a, b) => div(lower_pnum(a, cx), lower_pnum(b, cx)),
        PNumG::Max(items) => ScalarExprSpec::Max {
            items: items.iter().map(|item| lower_pnum(item, cx)).collect(),
        },
        PNumG::If(condition, a, b) => {
            let condition = lower_pbool(condition, cx);
            let a = lower_pnum(a, cx);
            let b = lower_pnum(b, cx);
            if cx.force {
                if_expr(
                    condition,
                    force_numeric_pair(a.clone(), b.clone()),
                    force_numeric_pair(b, a),
                )
            } else {
                if_expr(condition, a, b)
            }
        }
    }
}

fn lower_pbool(expr: &PBoolG, cx: &Cx<'_>) -> JudgmentExprSpec {
    match expr {
        PBoolG::Flag(value) => cmp(
            input(PERSON_FLAG_INPUT),
            ComparisonOpSpec::Eq,
            bool_lit(*value),
        ),
        PBoolG::Cmp(a, op, b) => cmp(lower_pnum(a, cx), *op, lower_pnum(b, cx)),
        PBoolG::Rule(index) => match Cx::pick(cx.person, RuleTag::Judg, *index) {
            Some(name) => JudgmentExprSpec::Derived {
                name: name.to_string(),
            },
            None => cmp(
                input(PERSON_FLAG_INPUT),
                ComparisonOpSpec::Eq,
                bool_lit(true),
            ),
        },
        PBoolG::And(items) => {
            let items = items
                .iter()
                .map(|item| lower_pbool(item, cx))
                .collect::<Vec<_>>();
            if cx.force {
                force_all(
                    &items,
                    JudgmentExprSpec::And {
                        items: items.clone(),
                    },
                )
            } else {
                JudgmentExprSpec::And { items }
            }
        }
        PBoolG::Or(items) => {
            let items = items
                .iter()
                .map(|item| lower_pbool(item, cx))
                .collect::<Vec<_>>();
            if cx.force {
                force_all(
                    &items,
                    JudgmentExprSpec::Or {
                        items: items.clone(),
                    },
                )
            } else {
                JudgmentExprSpec::Or { items }
            }
        }
        PBoolG::Not(item) => JudgmentExprSpec::Not {
            item: Box::new(lower_pbool(item, cx)),
        },
    }
}

fn derived_spec(
    name: &str,
    entity: &str,
    dtype: DTypeSpec,
    semantics: DerivedSemanticsSpec,
) -> DerivedSpec {
    DerivedSpec {
        id: None,
        name: name.to_string(),
        entity: entity.to_string(),
        dtype,
        unit: None,
        period: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics,
        versions: Vec::new(),
    }
}

fn parameter_tables() -> Vec<IndexedParameterSpec> {
    let table = |name: &str, values: Vec<(i64, ScalarValueSpec)>| IndexedParameterSpec {
        id: None,
        name: name.to_string(),
        unit: None,
        indexed_by: Some("bracket".to_string()),
        source: None,
        source_url: None,
        corpus_citation_path: None,
        versions: vec![ParameterVersionSpec {
            effective_from: date(2020, 1, 1),
            effective_to: None,
            values: values.into_iter().collect(),
        }],
    };
    let int = |value: i64| ScalarValueSpec::Integer { value };
    let dec = |value: &str| ScalarValueSpec::Decimal {
        value: value.to_string(),
    };
    vec![
        // Keys 3, 5+ and negatives are missing.
        table(
            INT_TABLE,
            vec![(0, int(10)), (1, int(20)), (2, int(35)), (4, int(50))],
        ),
        // Key 2 holds a zero (a divisor hazard); keys 4+ and negatives are missing.
        table(
            DEC_TABLE,
            vec![
                (0, dec("0.5")),
                (1, dec("1.25")),
                (2, dec("0")),
                (3, dec("7.5")),
            ],
        ),
    ]
}

fn members_relation() -> RelationSpec {
    RelationSpec {
        name: MEMBERS.to_string(),
        arity: 2,
        slot_entities: vec![PERSON.to_string(), HOUSEHOLD.to_string()],
        derivation: None,
    }
}

struct LoweredProgram {
    program: ProgramSpec,
    rules: Vec<(String, RuleTag)>,
    person_rules: Vec<(String, RuleTag)>,
}

fn lower_program(program: &ProgramG, profile: Profile, force: bool) -> LoweredProgram {
    let mut person_rules: Vec<(String, RuleTag)> = Vec::new();
    let mut derived_specs = Vec::new();
    for (index, rule) in program.person_rules.iter().enumerate() {
        let name = format!("pr{index}");
        let cx = Cx {
            profile,
            earlier: &[],
            person: &person_rules,
            force,
        };
        let (tag, spec) = match rule {
            PersonRuleG::Num(expr) => (
                RuleTag::Num,
                derived_spec(
                    &name,
                    PERSON,
                    DTypeSpec::Decimal,
                    DerivedSemanticsSpec::Scalar {
                        expr: lower_pnum(expr, &cx),
                    },
                ),
            ),
            PersonRuleG::Judg(expr) => (
                RuleTag::Judg,
                derived_spec(
                    &name,
                    PERSON,
                    DTypeSpec::Judgment,
                    DerivedSemanticsSpec::Judgment {
                        expr: lower_pbool(expr, &cx),
                    },
                ),
            ),
        };
        derived_specs.push(spec);
        person_rules.push((name, tag));
    }

    let mut rules: Vec<(String, RuleTag)> = Vec::new();
    for (index, rule) in program.rules.iter().enumerate() {
        let name = format!("r{index}");
        let cx = Cx {
            profile,
            earlier: &rules,
            person: &person_rules,
            force,
        };
        let (tag, spec) = match rule {
            RuleG::Num {
                expr,
                integer_dtype,
            } => (
                RuleTag::Num,
                derived_spec(
                    &name,
                    HOUSEHOLD,
                    if *integer_dtype && profile.integer_kinds {
                        DTypeSpec::Integer
                    } else {
                        DTypeSpec::Decimal
                    },
                    DerivedSemanticsSpec::Scalar {
                        expr: lower_num(expr, &cx),
                    },
                ),
            ),
            RuleG::Judg(expr) => (
                RuleTag::Judg,
                derived_spec(
                    &name,
                    HOUSEHOLD,
                    DTypeSpec::Judgment,
                    DerivedSemanticsSpec::Judgment {
                        expr: lower_bool(expr, &cx),
                    },
                ),
            ),
            RuleG::Bool(expr) => (
                RuleTag::Bool,
                derived_spec(
                    &name,
                    HOUSEHOLD,
                    DTypeSpec::Bool,
                    DerivedSemanticsSpec::Scalar {
                        expr: lower_bool_value(expr, &cx),
                    },
                ),
            ),
            RuleG::Text(expr) => (
                RuleTag::Text,
                derived_spec(
                    &name,
                    HOUSEHOLD,
                    DTypeSpec::Text,
                    DerivedSemanticsSpec::Scalar {
                        expr: lower_text_value(expr, &cx),
                    },
                ),
            ),
        };
        derived_specs.push(spec);
        rules.push((name, tag));
    }

    let mut spec = ProgramSpec {
        relations: vec![members_relation()],
        derived: derived_specs,
        ..ProgramSpec::default()
    };
    let used_parameters = referenced_parameters(&spec);
    spec.parameters = parameter_tables()
        .into_iter()
        .filter(|table| used_parameters.contains(&table.name))
        .collect();
    LoweredProgram {
        program: spec,
        rules,
        person_rules,
    }
}

// ---------------------------------------------------------------------------
// Static walks over lowered programs
// ---------------------------------------------------------------------------

fn walk_scalar(expr: &ScalarExprSpec, visit: &mut dyn FnMut(Ref<'_>)) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::Input { name } | ScalarExprSpec::InputOrElse { name, .. } => {
            visit(Ref::Input(name))
        }
        ScalarExprSpec::Derived { .. } => visit(Ref::Derived),
        ScalarExprSpec::ParameterLookup { parameter, index } => {
            visit(Ref::Parameter(parameter));
            walk_scalar(index, visit);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                walk_scalar(item, visit);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            walk_scalar(left, visit);
            walk_scalar(right, visit);
        }
        ScalarExprSpec::Ceil { value } | ScalarExprSpec::Floor { value } => {
            walk_scalar(value, visit)
        }
        ScalarExprSpec::DateAddDays { date, days: other }
        | ScalarExprSpec::DateAddMonths {
            date,
            months: other,
        }
        | ScalarExprSpec::DateAddYears { date, years: other } => {
            walk_scalar(date, visit);
            walk_scalar(other, visit);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            walk_scalar(from, visit);
            walk_scalar(to, visit);
        }
        ScalarExprSpec::CountRelated { where_clause, .. } => {
            if let Some(predicate) = where_clause {
                walk_judgment(predicate, visit);
            }
        }
        ScalarExprSpec::SumRelated {
            value,
            where_clause,
            ..
        } => {
            match value {
                RelatedValueRefSpec::Input { name } => visit(Ref::Input(name)),
                RelatedValueRefSpec::Derived { .. } => visit(Ref::Derived),
            }
            if let Some(predicate) = where_clause {
                walk_judgment(predicate, visit);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_judgment(condition, visit);
            walk_scalar(then_expr, visit);
            walk_scalar(else_expr, visit);
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            walk_scalar(value, visit);
            if let Some(n) = n {
                walk_scalar(n, visit);
            }
        }
    }
}

fn walk_judgment(expr: &JudgmentExprSpec, visit: &mut dyn FnMut(Ref<'_>)) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            walk_scalar(left, visit);
            walk_scalar(right, visit);
        }
        JudgmentExprSpec::Derived { .. } => visit(Ref::Derived),
        JudgmentExprSpec::RelationMember { .. } => {}
        JudgmentExprSpec::And { items }
        | JudgmentExprSpec::Or { items }
        | JudgmentExprSpec::ExactlyOne { items } => {
            for item in items {
                walk_judgment(item, visit);
            }
        }
        JudgmentExprSpec::Not { item } => walk_judgment(item, visit),
    }
}

enum Ref<'a> {
    Input(&'a str),
    Derived,
    Parameter(&'a str),
}

fn walk_semantics(semantics: &DerivedSemanticsSpec, visit: &mut dyn FnMut(Ref<'_>)) {
    match semantics {
        DerivedSemanticsSpec::Scalar { expr } => walk_scalar(expr, visit),
        DerivedSemanticsSpec::Judgment { expr } => walk_judgment(expr, visit),
    }
}

fn rule_inputs(rule: &DerivedSpec) -> BTreeSet<String> {
    let mut inputs = BTreeSet::new();
    walk_semantics(&rule.semantics, &mut |reference| {
        if let Ref::Input(name) = reference {
            inputs.insert(name.to_string());
        }
    });
    inputs
}

fn referenced_inputs(program: &ProgramSpec) -> BTreeSet<String> {
    program.derived.iter().flat_map(rule_inputs).collect()
}

fn referenced_parameters(program: &ProgramSpec) -> BTreeSet<String> {
    let mut parameters = BTreeSet::new();
    for rule in &program.derived {
        walk_semantics(&rule.semantics, &mut |reference| {
            if let Ref::Parameter(name) = reference {
                parameters.insert(name.to_string());
            }
        });
    }
    parameters
}

// ---------------------------------------------------------------------------
// Datasets and queries
// ---------------------------------------------------------------------------

fn household_id(row: usize) -> String {
    format!("h{row}")
}

fn person_id(row: usize, member: usize) -> String {
    format!("p{row}_{member}")
}

fn num_value(value: u8, integer: bool) -> ScalarValueSpec {
    if integer {
        ScalarValueSpec::Integer {
            value: INT_VALUES[value as usize % INT_VALUES.len()],
        }
    } else {
        ScalarValueSpec::Decimal {
            value: DEC_VALUES[value as usize % DEC_VALUES.len()].to_string(),
        }
    }
}

fn num_column(values: Vec<ScalarValueSpec>) -> DenseColumn {
    if values
        .iter()
        .all(|value| matches!(value, ScalarValueSpec::Integer { .. }))
    {
        DenseColumn::Integer(
            values
                .iter()
                .map(|value| match value {
                    ScalarValueSpec::Integer { value } => *value,
                    _ => unreachable!(),
                })
                .collect(),
        )
    } else {
        DenseColumn::Decimal(
            values
                .iter()
                .map(|value| spec_decimal(value).expect("numeric input value"))
                .collect(),
        )
    }
}

fn record(name: &str, entity: &str, entity_id: &str, value: ScalarValueSpec) -> InputRecordSpec {
    InputRecordSpec {
        name: name.to_string(),
        entity: entity.to_string(),
        entity_id: entity_id.to_string(),
        interval: period_interval(),
        value,
    }
}

/// One household or person input slot with its per-entity values.
struct Slot {
    name: String,
    entity: &'static str,
    /// `(entity id, value, present per row, present as a column)`
    cells: Vec<(String, ScalarValueSpec, bool)>,
    column_present: bool,
}

fn input_slots(case: &CaseG, profile: Profile) -> Vec<Slot> {
    let integer_kinds = profile.integer_kinds;
    let mut slots = Vec::new();
    for index in 0..HH_NUM_INPUTS {
        let integer = integer_kinds && case.program.integer_inputs[index as usize];
        slots.push(Slot {
            name: hh_num_input(index),
            entity: HOUSEHOLD,
            cells: case
                .rows
                .iter()
                .enumerate()
                .map(|(row, data)| {
                    (
                        household_id(row),
                        num_value(data.nums[index as usize], integer),
                        data.num_present[index as usize],
                    )
                })
                .collect(),
            column_present: case.columns.nums[index as usize],
        });
    }
    for index in 0..HH_FLAG_INPUTS {
        slots.push(Slot {
            name: hh_flag_input(index),
            entity: HOUSEHOLD,
            cells: case
                .rows
                .iter()
                .enumerate()
                .map(|(row, data)| {
                    (
                        household_id(row),
                        ScalarValueSpec::Bool {
                            value: data.flags[index as usize],
                        },
                        data.flag_present[index as usize],
                    )
                })
                .collect(),
            column_present: case.columns.flags[index as usize],
        });
    }
    slots.push(Slot {
        name: TEXT_INPUT.to_string(),
        entity: HOUSEHOLD,
        cells: case
            .rows
            .iter()
            .enumerate()
            .map(|(row, data)| {
                (
                    household_id(row),
                    ScalarValueSpec::Text {
                        value: TEXT_VALUES[data.text as usize % TEXT_VALUES.len()].to_string(),
                    },
                    data.text_present,
                )
            })
            .collect(),
        column_present: case.columns.text,
    });
    // Person slots: px0 has integer kind in kind-exercising profiles.
    for index in 0..P_NUM_INPUTS {
        let integer = integer_kinds && index == 0;
        slots.push(Slot {
            name: person_num_input(index),
            entity: PERSON,
            cells: case
                .rows
                .iter()
                .enumerate()
                .flat_map(|(row, data)| {
                    data.members
                        .iter()
                        .enumerate()
                        .map(move |(member, person)| {
                            (
                                person_id(row, member),
                                num_value(person.nums[index as usize], integer),
                                person.num_present[index as usize],
                            )
                        })
                })
                .collect(),
            column_present: case.columns.person_nums[index as usize],
        });
    }
    slots.push(Slot {
        name: PERSON_FLAG_INPUT.to_string(),
        entity: PERSON,
        cells: case
            .rows
            .iter()
            .enumerate()
            .flat_map(|(row, data)| {
                data.members
                    .iter()
                    .enumerate()
                    .map(move |(member, person)| {
                        (
                            person_id(row, member),
                            ScalarValueSpec::Bool { value: person.flag },
                            person.flag_present,
                        )
                    })
            })
            .collect(),
        column_present: case.columns.person_flag,
    });
    slots
}

fn slot_present(slot: &Slot, row_present: bool, profile: Profile) -> bool {
    if profile.row_missing_inputs {
        row_present
    } else {
        slot.column_present
    }
}

fn lower_dataset(case: &CaseG, profile: Profile, referenced: &BTreeSet<String>) -> DatasetSpec {
    let mut inputs = Vec::new();
    for slot in input_slots(case, profile) {
        if !referenced.contains(&slot.name) {
            continue;
        }
        for (entity_id, value, row_present) in &slot.cells {
            if slot_present(&slot, *row_present, profile) {
                inputs.push(record(&slot.name, slot.entity, entity_id, value.clone()));
            }
        }
    }
    let relations = case
        .rows
        .iter()
        .enumerate()
        .flat_map(|(row, data)| {
            (0..data.members.len()).map(move |member| RelationRecordSpec {
                name: MEMBERS.to_string(),
                tuple: vec![person_id(row, member), household_id(row)],
                interval: period_interval(),
            })
        })
        .collect();
    DatasetSpec { inputs, relations }
}

fn resolve_outputs(indices: &[u8], names: &[String]) -> Vec<String> {
    let mut outputs = Vec::new();
    for index in indices {
        let name = &names[*index as usize % names.len()];
        if !outputs.contains(name) {
            outputs.push(name.clone());
        }
    }
    outputs
}

fn lower_queries(case: &CaseG, profile: Profile, lowered: &LoweredProgram) -> Vec<ExecutionQuery> {
    let rule_names = lowered
        .rules
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let mut queries = case
        .rows
        .iter()
        .enumerate()
        .map(|(row, _)| {
            let outputs = if profile.mixed_requests {
                resolve_outputs(&case.requests.per_row[row], &rule_names)
            } else {
                resolve_outputs(&case.requests.shared, &rule_names)
            };
            query(&household_id(row), outputs)
        })
        .collect::<Vec<_>>();
    if profile.mixed_requests && !lowered.person_rules.is_empty() {
        let person_rule_names = lowered
            .person_rules
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for (row, member, outputs) in &case.requests.person_queries {
            let row = *row as usize % case.rows.len();
            let members = case.rows[row].members.len();
            if members == 0 {
                continue;
            }
            let id = person_id(row, *member as usize % members);
            if queries.iter().any(|query| query.entity_id == id) {
                continue;
            }
            queries.push(query(&id, resolve_outputs(outputs, &person_rule_names)));
        }
    }
    queries
}

/// The dense batch for a dense-profile case: household columns for every
/// referenced, present input, and the members relation with person columns.
fn lower_dense_batch(
    case: &CaseG,
    profile: Profile,
    referenced: &BTreeSet<String>,
) -> DenseBatchSpec {
    let mut inputs = HashMap::new();
    let mut person_inputs = HashMap::new();
    for slot in input_slots(case, profile) {
        if !referenced.contains(&slot.name) || !slot.column_present {
            continue;
        }
        let values = slot
            .cells
            .iter()
            .map(|(_, value, _)| value.clone())
            .collect::<Vec<_>>();
        let column = match values.first() {
            Some(ScalarValueSpec::Bool { .. }) => DenseColumn::Bool(
                values
                    .iter()
                    .map(|value| matches!(value, ScalarValueSpec::Bool { value: true }))
                    .collect(),
            ),
            Some(ScalarValueSpec::Text { .. }) => DenseColumn::Text(
                values
                    .iter()
                    .map(|value| match value {
                        ScalarValueSpec::Text { value } => value.clone(),
                        _ => unreachable!(),
                    })
                    .collect(),
            ),
            _ => num_column(values),
        };
        if slot.entity == HOUSEHOLD {
            inputs.insert(slot.name, column);
        } else {
            person_inputs.insert(slot.name, column);
        }
    }
    let mut offsets = vec![0];
    for row in &case.rows {
        offsets.push(offsets.last().copied().unwrap_or(0) + row.members.len());
    }
    DenseBatchSpec {
        row_count: case.rows.len(),
        inputs,
        relations: HashMap::from([(
            members_key(),
            DenseRelationBatchSpec {
                offsets,
                inputs: person_inputs,
            },
        )]),
    }
}

fn members_key() -> DenseRelationKey {
    DenseRelationKey {
        name: MEMBERS.to_string(),
        current_slot: 1,
        related_slot: 0,
    }
}

/// A fully lowered case.
struct Lowered {
    program: ProgramSpec,
    /// The same program with every lazy construct forced (see [`Cx::force`]).
    forced: ProgramSpec,
    rules: Vec<(String, RuleTag)>,
    dataset: DatasetSpec,
    queries: Vec<ExecutionQuery>,
}

fn lower(case: &CaseG, profile: Profile) -> Lowered {
    let lowered = lower_program(&case.program, profile, false);
    let forced = lower_program(&case.program, profile, true);
    let referenced = referenced_inputs(&lowered.program);
    let dataset = lower_dataset(case, profile, &referenced);
    let queries = lower_queries(case, profile, &lowered);
    Lowered {
        program: lowered.program,
        forced: forced.program,
        rules: lowered.rules,
        dataset,
        queries,
    }
}

// ===========================================================================
// Execution and normalization
// ===========================================================================

fn date(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: date(2026, 1, 1),
        end: date(2026, 1, 31),
    }
}

fn model_period() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: date(2026, 1, 1),
        end: date(2026, 1, 31),
    }
}

fn period_interval() -> IntervalSpec {
    IntervalSpec {
        start: period().start,
        end: period().end,
    }
}

fn query(entity_id: &str, outputs: Vec<String>) -> ExecutionQuery {
    ExecutionQuery {
        assessment_date: None,
        entity_id: entity_id.to_string(),
        period: period(),
        outputs,
    }
}

fn request(
    mode: ExecutionMode,
    program: &ProgramSpec,
    dataset: &DatasetSpec,
    queries: &[ExecutionQuery],
) -> ExecutionRequest {
    ExecutionRequest {
        mode,
        program: program.clone(),
        dataset: dataset.clone(),
        queries: queries.to_vec(),
    }
}

/// What one engine call produced, normalized for comparison.
#[derive(Clone, Debug)]
enum Outcome {
    /// Per-query results without traces, and whether fast fell back.
    Ok {
        results: Vec<serde_json::Value>,
        fell_back: bool,
    },
    Err {
        key: String,
        message: String,
    },
    Panic(String),
}

impl Outcome {
    fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok { .. })
    }

    fn render(&self) -> String {
        match self {
            Outcome::Ok { results, fell_back } => {
                let mut text = String::new();
                if *fell_back {
                    text.push_str("(fast fell back to explain) ");
                }
                for result in results {
                    let _ = write!(text, "\n      {}", render_result(result));
                }
                text
            }
            Outcome::Err { key, message } => format!("Err[{key}] {message}"),
            Outcome::Panic(message) => format!("PANIC {message}"),
        }
    }
}

fn render_result(result: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(outputs) = result["outputs"].as_object() {
        for (name, output) in outputs {
            let value = if output["kind"] == "judgment" {
                output["outcome"].as_str().unwrap_or("?").to_string()
            } else {
                let value = &output["value"];
                format!(
                    "{}:{}",
                    value["kind"].as_str().unwrap_or("?"),
                    match &value["value"] {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    }
                )
            };
            parts.push(format!("{name}={value}"));
        }
    }
    format!(
        "{}: {{{}}}",
        result["entity_id"].as_str().unwrap_or("?"),
        parts.join(", ")
    )
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

fn normalize_response(response: &ExecutionResponse) -> Outcome {
    Outcome::Ok {
        results: response
            .results
            .iter()
            .map(|result| {
                serde_json::json!({
                    "entity_id": result.entity_id,
                    "period": result.period,
                    "assessment_date": result.assessment_date,
                    "outputs": result.outputs,
                })
            })
            .collect(),
        fell_back: response.metadata.requested_mode == ExecutionMode::Fast
            && response.metadata.actual_mode != ExecutionMode::Fast,
    }
}

/// Error identity for explain/fast comparison: the variant, plus the fields
/// that identify where the reference evaluation stopped.
fn api_error_key(error: &ApiError) -> String {
    match error {
        ApiError::Eval(error) => eval_error_key(error, true),
        ApiError::Spec(error) => format!("Spec::{}", variant_name(&format!("{error:?}"))),
        other => format!("Api::{}", variant_name(&format!("{other:?}"))),
    }
}

fn eval_error_key(error: &EvalError, with_location: bool) -> String {
    match error {
        EvalError::MissingInput {
            name, entity_id, ..
        } if with_location => format!("MissingInput({name} @ {entity_id})"),
        EvalError::MissingParameterValue { parameter, key, .. } if with_location => {
            format!("MissingParameterValue({parameter}[{key}])")
        }
        other => variant_name(&format!("{other:?}")),
    }
}

fn variant_name(debug: &str) -> String {
    debug
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .next()
        .unwrap_or(debug)
        .to_string()
}

fn run_sparse(request: ExecutionRequest) -> Outcome {
    match catch_unwind(AssertUnwindSafe(|| execute_request(request))) {
        Ok(Ok(response)) => normalize_response(&response),
        Ok(Err(error)) => Outcome::Err {
            key: api_error_key(&error),
            message: error.to_string(),
        },
        Err(payload) => Outcome::Panic(panic_message(payload)),
    }
}

fn run_compiled(program: &ProgramSpec, request: CompiledExecutionRequest) -> Outcome {
    let artifact = match CompiledProgramArtifact::compile(program.clone()) {
        Ok(artifact) => artifact,
        Err(error) => {
            return Outcome::Err {
                key: format!("Compile::{}", variant_name(&format!("{error:?}"))),
                message: error.to_string(),
            };
        }
    };
    match catch_unwind(AssertUnwindSafe(|| {
        execute_compiled_request(artifact, request)
    })) {
        Ok(Ok(response)) => normalize_response(&response),
        Ok(Err(error)) => Outcome::Err {
            key: api_error_key(&error),
            message: error.to_string(),
        },
        Err(payload) => Outcome::Panic(panic_message(payload)),
    }
}

/// Explain vs fast: identical traces-stripped results (value kinds included),
/// or the same error.
fn compare_sparse(reference: &Outcome, candidate: &Outcome) -> Result<(), String> {
    match (reference, candidate) {
        (
            Outcome::Ok {
                results: expected, ..
            },
            Outcome::Ok {
                results: actual, ..
            },
        ) => {
            if expected == actual {
                Ok(())
            } else {
                let first = expected
                    .iter()
                    .zip(actual)
                    .position(|(expected, actual)| expected != actual)
                    .unwrap_or(expected.len().min(actual.len()));
                Err(format!(
                    "results differ (first difference at query {first})"
                ))
            }
        }
        (Outcome::Err { key: expected, .. }, Outcome::Err { key: actual, .. }) => {
            if expected == actual {
                Ok(())
            } else {
                Err(format!("errors differ: expected {expected}, got {actual}"))
            }
        }
        (Outcome::Ok { .. }, Outcome::Err { .. }) => {
            Err("reference succeeded, candidate failed".to_string())
        }
        (Outcome::Err { .. }, Outcome::Ok { .. }) => {
            Err("reference failed, candidate succeeded".to_string())
        }
        (_, Outcome::Panic(_)) => Err("candidate panicked".to_string()),
        (Outcome::Panic(_), _) => Err("reference panicked".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Dense
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum DenseOutcome {
    /// Per output, per row: the value as `ScalarValueSpec`, or a judgment.
    Ok(BTreeMap<String, Vec<DenseCell>>),
    Err {
        variant: String,
        message: String,
    },
    Panic(String),
    /// The program is outside the dense compiler's fragment.
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq)]
enum DenseCell {
    Number(Decimal),
    Bool(bool),
    Text(String),
    Judgment(JudgmentOutcomeSpec),
    Other(String),
}

fn dense_cells(
    result: &DenseExecutionResult,
    outputs: &[String],
    dtypes: &HashMap<String, DType>,
) -> BTreeMap<String, Vec<DenseCell>> {
    let mut cells = BTreeMap::new();
    for output in outputs {
        let values = match result.outputs.get(output) {
            Some(DenseOutputValue::Scalar(column)) => (0..result.row_count)
                .map(|row| {
                    let dtype = dtypes.get(output).cloned().unwrap_or(DType::Decimal);
                    scalar_cell(&ScalarValueSpec::from_model(
                        column.scalar_value_at(row, &dtype),
                    ))
                })
                .collect(),
            Some(DenseOutputValue::Judgment(values)) => values
                .iter()
                .map(|value| DenseCell::Judgment(JudgmentOutcomeSpec::from(*value)))
                .collect(),
            None => vec![DenseCell::Other("missing output".to_string()); result.row_count],
        };
        cells.insert(output.clone(), values);
    }
    cells
}

fn scalar_cell(value: &ScalarValueSpec) -> DenseCell {
    match value {
        ScalarValueSpec::Integer { .. } | ScalarValueSpec::Decimal { .. } => {
            DenseCell::Number(spec_decimal(value).expect("numeric value"))
        }
        ScalarValueSpec::Bool { value } => DenseCell::Bool(*value),
        ScalarValueSpec::Text { value } => DenseCell::Text(value.clone()),
        ScalarValueSpec::Date { value } => DenseCell::Other(value.to_string()),
    }
}

fn output_cell(output: &OutputValue) -> DenseCell {
    match output {
        OutputValue::Scalar { value, .. } => scalar_cell(value),
        OutputValue::Judgment { outcome, .. } => DenseCell::Judgment(*outcome),
    }
}

fn spec_decimal(value: &ScalarValueSpec) -> Option<Decimal> {
    match value {
        ScalarValueSpec::Integer { value } => Some(Decimal::from(*value)),
        ScalarValueSpec::Decimal { value } => Decimal::from_str(value).ok(),
        _ => None,
    }
}

fn run_dense(program: &ProgramSpec, batch: DenseBatchSpec, outputs: &[String]) -> DenseOutcome {
    let model = match program.to_program() {
        Ok(model) => model,
        Err(error) => return DenseOutcome::Unsupported(error.to_string()),
    };
    let dense = match DenseCompiledProgram::from_program(&model, Some(HOUSEHOLD)) {
        Ok(dense) => dense,
        Err(error) => return DenseOutcome::Unsupported(error.to_string()),
    };
    let dtypes = model
        .derived
        .values()
        .map(|rule| (rule.name.clone(), rule.dtype.clone()))
        .collect::<HashMap<_, _>>();
    match catch_unwind(AssertUnwindSafe(|| {
        dense.execute(&model_period(), batch, outputs)
    })) {
        Ok(Ok(result)) => DenseOutcome::Ok(dense_cells(&result, outputs, &dtypes)),
        Ok(Err(error)) => DenseOutcome::Err {
            variant: eval_error_key(&error, false),
            message: error.to_string(),
        },
        Err(payload) => DenseOutcome::Panic(panic_message(payload)),
    }
}

/// Explain (one query per row, every output) vs dense: numeric values by
/// value (dense columns are typed), everything else exactly; errors by variant.
fn compare_dense(
    explain: &Outcome,
    dense: &DenseOutcome,
    outputs: &[String],
) -> Result<(), String> {
    match (explain, dense) {
        (_, DenseOutcome::Unsupported(_)) => Ok(()),
        (Outcome::Ok { results, .. }, DenseOutcome::Ok(columns)) => {
            for (row, result) in results.iter().enumerate() {
                for output in outputs {
                    let value: OutputValue =
                        serde_json::from_value(result["outputs"][output].clone())
                            .map_err(|error| format!("explain output {output} missing: {error}"))?;
                    let expected = output_cell(&value);
                    let actual = &columns[output][row];
                    if &expected != actual {
                        return Err(format!(
                            "row {row} ({}) output {output}: explain {expected:?}, dense {actual:?}",
                            result["entity_id"]
                        ));
                    }
                }
            }
            Ok(())
        }
        (Outcome::Err { key, .. }, DenseOutcome::Err { variant, .. }) => {
            let expected = variant_name(key);
            if &expected == variant {
                Ok(())
            } else {
                Err(format!(
                    "errors differ: explain {expected}, dense {variant}"
                ))
            }
        }
        (Outcome::Ok { .. }, DenseOutcome::Err { .. }) => {
            Err("explain succeeded, dense failed".to_string())
        }
        (Outcome::Err { .. }, DenseOutcome::Ok(_)) => {
            Err("explain failed, dense succeeded".to_string())
        }
        (_, DenseOutcome::Panic(_)) => Err("dense panicked".to_string()),
        (Outcome::Panic(_), _) => Err("explain panicked".to_string()),
    }
}

fn render_dense(outcome: &DenseOutcome) -> String {
    match outcome {
        DenseOutcome::Ok(columns) => {
            let mut text = String::new();
            for (output, cells) in columns {
                let _ = write!(text, "\n      {output} = {cells:?}");
            }
            text
        }
        DenseOutcome::Err { variant, message } => format!("Err[{variant}] {message}"),
        DenseOutcome::Panic(message) => format!("PANIC {message}"),
        DenseOutcome::Unsupported(message) => format!("dense compiler declined: {message}"),
    }
}

// ===========================================================================
// Rendering counterexamples
// ===========================================================================

fn fmt_value(value: &ScalarValueSpec) -> String {
    match value {
        ScalarValueSpec::Integer { value } => value.to_string(),
        ScalarValueSpec::Decimal { value } if value.contains('.') => value.clone(),
        ScalarValueSpec::Decimal { value } => format!("{value}.0"),
        ScalarValueSpec::Bool { value } => value.to_string(),
        ScalarValueSpec::Text { value } => format!("{value:?}"),
        ScalarValueSpec::Date { value } => value.to_string(),
    }
}

fn fmt_scalar(expr: &ScalarExprSpec) -> String {
    let join = |items: &[ScalarExprSpec], separator: &str| {
        items
            .iter()
            .map(fmt_scalar)
            .collect::<Vec<_>>()
            .join(separator)
    };
    match expr {
        ScalarExprSpec::Literal { value } => fmt_value(value),
        ScalarExprSpec::Input { name } => name.clone(),
        ScalarExprSpec::InputOrElse { name, default } => {
            format!("input_or_else({name}, {})", fmt_value(default))
        }
        ScalarExprSpec::Derived { name } => name.clone(),
        ScalarExprSpec::ParameterLookup { parameter, index } => {
            format!("{parameter}[{}]", fmt_scalar(index))
        }
        ScalarExprSpec::Add { items } if items.len() == 1 => {
            format!("(+{})", fmt_scalar(&items[0]))
        }
        ScalarExprSpec::Add { items } => format!("({})", join(items, " + ")),
        ScalarExprSpec::Sub { left, right } => {
            format!("({} - {})", fmt_scalar(left), fmt_scalar(right))
        }
        ScalarExprSpec::Mul { left, right } => {
            format!("({} * {})", fmt_scalar(left), fmt_scalar(right))
        }
        ScalarExprSpec::Div { left, right } => {
            format!("({} / {})", fmt_scalar(left), fmt_scalar(right))
        }
        ScalarExprSpec::Max { items } => format!("max({})", join(items, ", ")),
        ScalarExprSpec::Min { items } => format!("min({})", join(items, ", ")),
        ScalarExprSpec::Ceil { value } => format!("ceil({})", fmt_scalar(value)),
        ScalarExprSpec::Floor { value } => format!("floor({})", fmt_scalar(value)),
        ScalarExprSpec::CountRelated { where_clause, .. } => match where_clause {
            Some(predicate) => format!("count(members where {})", fmt_judgment(predicate)),
            None => "count(members)".to_string(),
        },
        ScalarExprSpec::SumRelated {
            value,
            where_clause,
            ..
        } => {
            let value = match value {
                RelatedValueRefSpec::Input { name } | RelatedValueRefSpec::Derived { name } => name,
            };
            match where_clause {
                Some(predicate) => {
                    format!("sum(members.{value} where {})", fmt_judgment(predicate))
                }
                None => format!("sum(members.{value})"),
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "(if {} then {} else {})",
            fmt_judgment(condition),
            fmt_scalar(then_expr),
            fmt_scalar(else_expr)
        ),
        other => format!("{other:?}"),
    }
}

fn fmt_op(op: ComparisonOpSpec) -> &'static str {
    match op {
        ComparisonOpSpec::Lt => "<",
        ComparisonOpSpec::Lte => "<=",
        ComparisonOpSpec::Gt => ">",
        ComparisonOpSpec::Gte => ">=",
        ComparisonOpSpec::Eq => "==",
        ComparisonOpSpec::Ne => "!=",
    }
}

fn fmt_judgment(expr: &JudgmentExprSpec) -> String {
    let join = |items: &[JudgmentExprSpec], separator: &str| {
        items
            .iter()
            .map(fmt_judgment)
            .collect::<Vec<_>>()
            .join(separator)
    };
    match expr {
        JudgmentExprSpec::Comparison { left, op, right } => {
            format!("{} {} {}", fmt_scalar(left), fmt_op(*op), fmt_scalar(right))
        }
        JudgmentExprSpec::Derived { name } => name.clone(),
        JudgmentExprSpec::And { items } => format!("({})", join(items, " and ")),
        JudgmentExprSpec::Or { items } => format!("({})", join(items, " or ")),
        JudgmentExprSpec::Not { item } => format!("not {}", fmt_judgment(item)),
        other => format!("{other:?}"),
    }
}

fn render_program(program: &ProgramSpec) -> String {
    let mut text = String::new();
    for rule in &program.derived {
        let body = match &rule.semantics {
            DerivedSemanticsSpec::Scalar { expr } => fmt_scalar(expr),
            DerivedSemanticsSpec::Judgment { expr } => fmt_judgment(expr),
        };
        let _ = write!(
            text,
            "\n    {} [{}, {:?}] = {}",
            rule.name, rule.entity, rule.dtype, body
        );
    }
    for parameter in &program.parameters {
        let values = parameter
            .versions
            .first()
            .map(|version| {
                version
                    .values
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", fmt_value(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let _ = write!(text, "\n    {} = {{{values}}}", parameter.name);
    }
    text
}

fn render_dataset(dataset: &DatasetSpec) -> String {
    let mut by_entity: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for input in &dataset.inputs {
        by_entity
            .entry(input.entity_id.as_str())
            .or_default()
            .push(format!("{}={}", input.name, fmt_value(&input.value)));
    }
    let mut members: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for relation in &dataset.relations {
        members
            .entry(relation.tuple[1].as_str())
            .or_default()
            .push(relation.tuple[0].as_str());
    }
    let mut text = String::new();
    let entities = by_entity
        .keys()
        .chain(members.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for entity in entities {
        let _ = write!(
            text,
            "\n    {entity}: {}",
            by_entity
                .get(entity)
                .map(|cells| cells.join(" "))
                .unwrap_or_default()
        );
        if let Some(list) = members.get(entity) {
            let _ = write!(text, " members=[{}]", list.join(", "));
        }
    }
    text
}

fn render_queries(queries: &[ExecutionQuery]) -> String {
    queries
        .iter()
        .map(|query| format!("{} -> [{}]", query.entity_id, query.outputs.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_case(profile: Profile, lowered: &Lowered, sections: &[(&str, String)]) -> String {
    let mut text = format!(
        "profile: {}\n  program:{}",
        profile.name,
        render_program(&lowered.program)
    );
    let _ = write!(text, "\n  dataset:{}", render_dataset(&lowered.dataset));
    let _ = write!(text, "\n  queries: {}", render_queries(&lowered.queries));
    for (label, body) in sections {
        let _ = write!(text, "\n  {label}: {body}");
    }
    let request_json = serde_json::to_string(&request(
        ExecutionMode::Fast,
        &lowered.program,
        &lowered.dataset,
        &lowered.queries,
    ))
    .unwrap_or_default();
    let _ = write!(text, "\n  request JSON (fast mode): {request_json}");
    text
}

// ===========================================================================
// Runner
// ===========================================================================

fn case_count() -> u32 {
    std::env::var("AXIOM_PARITY_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CASES)
}

fn report_only() -> bool {
    std::env::var("AXIOM_PARITY_REPORT_ONLY").is_ok_and(|value| value != "0" && !value.is_empty())
}

fn seed_for(property: u64) -> [u8; 32] {
    let base = std::env::var("AXIOM_PARITY_SEED")
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

#[derive(Debug, Default)]
struct Stats {
    cases: u32,
    explain_ok: u32,
    explain_err: u32,
    /// Explain succeeded but the forced program (every branch and operand
    /// evaluated) failed: the case hides an error behind laziness.
    hazards: u32,
    fast_ok_fast_path: u32,
    fast_ok_fallback: u32,
    dense_ran: u32,
    dense_declined: u32,
    reference_panics: u32,
    divergences: u32,
    samples: Vec<String>,
}

impl Stats {
    fn summary(&self, name: &str) -> String {
        format!(
            "{name}: {} cases; explain ok {} / err {}; laziness hazards {}; fast path {} / fallback {}; dense ran {} / declined {}; reference panics {}; divergences {}",
            self.cases,
            self.explain_ok,
            self.explain_err,
            self.hazards,
            self.fast_ok_fast_path,
            self.fast_ok_fallback,
            self.dense_ran,
            self.dense_declined,
            self.reference_panics,
            self.divergences
        )
    }
}

/// Run `check` over `cases` generated cases with a fixed seed. On failure,
/// proptest shrinks the case and the panic message carries `check`'s rendering
/// of the minimal counterexample. Returns the statistics of the passing run.
fn run_property(
    name: &str,
    property: u64,
    profile: Profile,
    check: impl Fn(&CaseG, &mut Stats) -> Result<(), String>,
) -> Stats {
    let stats = RefCell::new(Stats::default());
    let report_only = report_only();
    let config = Config {
        cases: case_count(),
        failure_persistence: None,
        max_shrink_iters: 20_000,
        ..Config::default()
    };
    let mut runner = TestRunner::new_with_rng(
        config,
        TestRng::from_seed(RngAlgorithm::ChaCha, &seed_for(property)),
    );
    let outcome = runner.run(&case_strategy(profile), |case| {
        let mut stats = stats.borrow_mut();
        match check(&case, &mut stats) {
            Ok(()) => Ok(()),
            Err(report) if report_only => {
                stats.divergences += 1;
                if stats.samples.len() < 3 {
                    stats.samples.push(report);
                }
                Ok(())
            }
            Err(report) => Err(TestCaseError::fail(report)),
        }
    });
    let stats = stats.into_inner();
    eprintln!("{}", stats.summary(name));
    match outcome {
        Ok(()) => {
            if report_only {
                for sample in &stats.samples {
                    eprintln!("{name} divergence (unshrunk):\n{sample}\n");
                }
            }
            stats
        }
        Err(TestError::Fail(reason, _)) => {
            panic!("{name}: minimal failing case after shrinking:\n{reason}")
        }
        Err(TestError::Abort(reason)) => panic!("{name}: runner aborted: {reason}"),
    }
}

/// Non-vacuity: a passing run must have exercised what it claims to.
fn assert_exercised(name: &str, stats: &Stats, min_hazard_share: f64) {
    if report_only() || stats.cases < 200 {
        return;
    }
    let cases = f64::from(stats.cases);
    assert!(
        f64::from(stats.explain_ok) >= 0.25 * cases,
        "{name}: explain succeeded on too few cases to be meaningful: {}",
        stats.summary(name)
    );
    assert!(
        f64::from(stats.hazards) >= min_hazard_share * cases,
        "{name}: too few cases hide an error behind a dead branch or short circuit: {}",
        stats.summary(name)
    );
    let fast_total = stats.fast_ok_fast_path + stats.fast_ok_fallback;
    if fast_total > 0 {
        assert!(
            f64::from(stats.fast_ok_fast_path) >= 0.8 * f64::from(fast_total),
            "{name}: fast fell back to explain too often for the fast path to be under test: {}",
            stats.summary(name)
        );
    }
    assert!(
        f64::from(stats.reference_panics) <= 0.01 * cases,
        "{name}: explain panicked on too many generated cases: {}",
        stats.summary(name)
    );
}

/// Explain the original program, and the forced program to count hazards.
/// Returns `None` (case skipped) when explain itself panics, which only
/// happens on decimal overflow in deep generated arithmetic.
fn explain_reference(lowered: &Lowered, stats: &mut Stats) -> Option<Outcome> {
    stats.cases += 1;
    let explain = run_sparse(request(
        ExecutionMode::Explain,
        &lowered.program,
        &lowered.dataset,
        &lowered.queries,
    ));
    match &explain {
        Outcome::Ok { .. } => {
            stats.explain_ok += 1;
            let forced = run_sparse(request(
                ExecutionMode::Explain,
                &lowered.forced,
                &lowered.dataset,
                &lowered.queries,
            ));
            if !forced.is_ok() {
                stats.hazards += 1;
            }
        }
        Outcome::Err { .. } => stats.explain_err += 1,
        Outcome::Panic(_) => {
            stats.reference_panics += 1;
            return None;
        }
    }
    Some(explain)
}

fn record_fast(outcome: &Outcome, stats: &mut Stats) {
    if let Outcome::Ok { fell_back, .. } = outcome {
        if *fell_back {
            stats.fast_ok_fallback += 1;
        } else {
            stats.fast_ok_fast_path += 1;
        }
    }
}

// ===========================================================================
// Random properties
// ===========================================================================

fn check_explain_vs_fast(case: &CaseG, profile: Profile, stats: &mut Stats) -> Result<(), String> {
    let lowered = lower(case, profile);
    let Some(explain) = explain_reference(&lowered, stats) else {
        return Ok(());
    };
    let fast = run_sparse(request(
        ExecutionMode::Fast,
        &lowered.program,
        &lowered.dataset,
        &lowered.queries,
    ));
    record_fast(&fast, stats);
    compare_sparse(&explain, &fast).map_err(|problem| {
        render_case(
            profile,
            &lowered,
            &[
                ("divergence", problem),
                ("explain", explain.render()),
                ("fast", fast.render()),
            ],
        )
    })
}

/// Fast must return exactly what explain returns, on programs whose numeric
/// leaves are all decimals (so only evaluation order can make them differ).
#[test]
fn random_programs_fast_matches_explain_on_evaluation_order() {
    let stats = run_property(
        "random_programs_fast_matches_explain_on_evaluation_order",
        1,
        EVAL_ORDER,
        |case, stats| check_explain_vs_fast(case, EVAL_ORDER, stats),
    );
    assert_exercised("eval-order", &stats, 0.05);
}

/// Fast must return exactly what explain returns on the full generator:
/// integer/decimal kinds, relations, parameter tables, text and bool rules,
/// ill-typed comparisons, mixed per-row requests and person rows.
#[test]
fn random_programs_fast_matches_explain_on_full_generator() {
    let stats = run_property(
        "random_programs_fast_matches_explain_on_full_generator",
        2,
        FULL,
        |case, stats| check_explain_vs_fast(case, FULL, stats),
    );
    assert_exercised("full", &stats, 0.05);
}

/// Dense must hold explain's value for every row and output, and fail exactly
/// when explain fails for some row (same error variant).
#[test]
fn random_programs_dense_matches_explain() {
    let stats = run_property(
        "random_programs_dense_matches_explain",
        3,
        DENSE,
        |case, stats| {
            let lowered = lower(case, DENSE);
            let Some(explain) = explain_reference(&lowered, stats) else {
                return Ok(());
            };
            // Every row of a dense-profile case requests the same outputs; dense
            // evaluates exactly those, for every row.
            let mut outputs = Vec::new();
            for output in lowered
                .queries
                .first()
                .map_or(&[][..], |query| &query.outputs)
            {
                if !outputs.contains(output) {
                    outputs.push(output.clone());
                }
            }
            let referenced = referenced_inputs(&lowered.program);
            let batch = lower_dense_batch(case, DENSE, &referenced);
            let dense = run_dense(&lowered.program, batch, &outputs);
            if matches!(dense, DenseOutcome::Unsupported(_)) {
                stats.dense_declined += 1;
            } else {
                stats.dense_ran += 1;
            }
            compare_dense(&explain, &dense, &outputs).map_err(|problem| {
                render_case(
                    DENSE,
                    &lowered,
                    &[
                        ("divergence", problem),
                        ("explain", explain.render()),
                        ("dense", render_dense(&dense)),
                    ],
                )
            })
        },
    );
    assert_exercised("dense", &stats, 0.05);
    if !report_only() && stats.cases >= 200 {
        assert!(
            stats.dense_ran >= stats.cases * 8 / 10,
            "the dense compiler declined too many generated programs: {}",
            stats.summary("dense")
        );
    }
}

/// A pinned rule evaluates to its literal in every mode and never needs the
/// inputs its original formula reads: fast with a pin equals explain with the
/// pin, and dropping the inputs only the pinned rule reads changes nothing.
#[test]
fn random_pins_match_explain_and_never_read_original_inputs() {
    let stats = run_property(
        "random_pins_match_explain_and_never_read_original_inputs",
        4,
        FULL,
        |case, stats| {
            let mut lowered = lower(case, FULL);
            let scalar_rules = lowered
                .rules
                .iter()
                .filter(|(_, tag)| *tag != RuleTag::Judg)
                .cloned()
                .collect::<Vec<_>>();
            let Some(explain_unpinned) = explain_reference(&lowered, stats) else {
                return Ok(());
            };
            let _ = explain_unpinned;
            if scalar_rules.is_empty() {
                return Ok(());
            }
            let (rule, tag) = &scalar_rules[case.pin.0 as usize % scalar_rules.len()];
            let value = match tag {
                RuleTag::Num => num_literal(case.pin.1, true),
                RuleTag::Bool => ScalarValueSpec::Bool {
                    value: case.pin.1 % 2 == 0,
                },
                RuleTag::Text => ScalarValueSpec::Text {
                    value: TEXT_VALUES[case.pin.1 as usize % TEXT_VALUES.len()].to_string(),
                },
                RuleTag::Judg => unreachable!(),
            };
            let pins = vec![RulePin {
                rule: rule.clone(),
                value,
            }];
            // Inputs only the pinned rule's own formula reads.
            let pinned_spec = lowered
                .program
                .derived
                .iter()
                .find(|spec| &spec.name == rule)
                .expect("pinned rule exists");
            let others = lowered
                .program
                .derived
                .iter()
                .filter(|spec| &spec.name != rule)
                .flat_map(rule_inputs)
                .collect::<BTreeSet<_>>();
            let exclusive = rule_inputs(pinned_spec)
                .difference(&others)
                .cloned()
                .collect::<BTreeSet<_>>();
            let full_dataset = lowered.dataset.clone();
            let mut reduced_dataset = full_dataset.clone();
            reduced_dataset
                .inputs
                .retain(|record| !exclusive.contains(&record.name));

            let compiled = |mode: ExecutionMode, dataset: &DatasetSpec| {
                run_compiled(
                    &lowered.program,
                    CompiledExecutionRequest {
                        mode,
                        dataset: dataset.clone(),
                        queries: lowered.queries.clone(),
                        pins: pins.clone(),
                    },
                )
            };
            let explain_full = compiled(ExecutionMode::Explain, &full_dataset);
            let explain_reduced = compiled(ExecutionMode::Explain, &reduced_dataset);
            let fast_reduced = compiled(ExecutionMode::Fast, &reduced_dataset);
            record_fast(&fast_reduced, stats);
            lowered.dataset = reduced_dataset;
            let sections = |problem: String| {
                vec![
                    ("pin", format!("{rule} := {:?}", pins[0].value)),
                    (
                        "inputs only the pinned rule reads (dropped)",
                        format!("{exclusive:?}"),
                    ),
                    ("divergence", problem),
                    ("explain with pin, full dataset", explain_full.render()),
                    (
                        "explain with pin, reduced dataset",
                        explain_reduced.render(),
                    ),
                    ("fast with pin, reduced dataset", fast_reduced.render()),
                ]
            };
            compare_sparse(&explain_full, &explain_reduced)
                .map_err(|problem| {
                    render_case(
                        FULL,
                        &lowered,
                        &sections(format!(
                            "explain needed the pinned rule's inputs: {problem}"
                        )),
                    )
                })
                .and_then(|()| {
                    compare_sparse(&explain_reduced, &fast_reduced)
                        .map_err(|problem| render_case(FULL, &lowered, &sections(problem)))
                })
        },
    );
    assert_exercised("pins", &stats, 0.05);
}

/// Metamorphic checks in both sparse modes: a batch equals the concatenation
/// of its singleton queries (and fails, with the first failing singleton's
/// error, exactly when one of them fails), and permuting the queries permutes
/// the results without changing success or failure.
#[test]
fn random_batches_equal_concatenated_singletons_and_permute() {
    let stats = run_property(
        "random_batches_equal_concatenated_singletons_and_permute",
        5,
        FULL,
        |case, stats| {
            let lowered = lower(case, FULL);
            let Some(_) = explain_reference(&lowered, stats) else {
                return Ok(());
            };
            let mut order = (0..lowered.queries.len()).collect::<Vec<_>>();
            order.sort_by_key(|index| {
                case.requests.order_keys[index % case.requests.order_keys.len()]
            });
            let permuted_queries = order
                .iter()
                .map(|index| lowered.queries[*index].clone())
                .collect::<Vec<_>>();
            for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                let run = |queries: &[ExecutionQuery]| {
                    run_sparse(request(
                        mode.clone(),
                        &lowered.program,
                        &lowered.dataset,
                        queries,
                    ))
                };
                let batch = run(&lowered.queries);
                if mode == ExecutionMode::Fast {
                    record_fast(&batch, stats);
                }
                if let Outcome::Panic(_) = batch
                    && mode == ExecutionMode::Explain
                {
                    return Ok(());
                }
                let singles = lowered
                    .queries
                    .iter()
                    .map(|single| run(std::slice::from_ref(single)))
                    .collect::<Vec<_>>();
                let expected = match singles.iter().find(|single| !single.is_ok()) {
                    Some(first_failure) => first_failure.clone(),
                    None => Outcome::Ok {
                        results: singles
                            .iter()
                            .flat_map(|single| match single {
                                Outcome::Ok { results, .. } => results.clone(),
                                _ => Vec::new(),
                            })
                            .collect(),
                        fell_back: false,
                    },
                };
                let permuted = run(&permuted_queries);
                let fail = |problem: String| {
                    render_case(
                        FULL,
                        &lowered,
                        &[
                            ("mode", format!("{mode:?}")),
                            ("divergence", problem),
                            ("batch", batch.render()),
                            (
                                "singletons",
                                singles
                                    .iter()
                                    .map(Outcome::render)
                                    .collect::<Vec<_>>()
                                    .join(" | "),
                            ),
                            ("permutation", format!("{order:?}")),
                            ("permuted batch", permuted.render()),
                        ],
                    )
                };
                compare_sparse(&expected, &batch).map_err(|problem| {
                    fail(format!("batch vs concatenated singletons: {problem}"))
                })?;
                match (&batch, &permuted) {
                    (
                        Outcome::Ok { results, .. },
                        Outcome::Ok {
                            results: permuted_results,
                            ..
                        },
                    ) => {
                        let expected_permuted = order
                            .iter()
                            .map(|index| results[*index].clone())
                            .collect::<Vec<_>>();
                        if &expected_permuted != permuted_results {
                            return Err(fail(
                                "permuted batch is not the permuted results".to_string(),
                            ));
                        }
                    }
                    (Outcome::Err { .. }, Outcome::Err { .. }) => {}
                    _ => {
                        return Err(fail(
                            "permuting the queries changed success/failure".to_string(),
                        ));
                    }
                }
            }
            Ok(())
        },
    );
    assert_exercised("metamorphic", &stats, 0.05);
}
