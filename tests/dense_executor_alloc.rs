//! Dense execution reserves every buffer it sizes by a row count fallibly.
//!
//! Binding bounds each count by `MAX_DENSE_ROWS` and reserves a relation's
//! owners fallibly (#255), but it trusts a count up to that bound. Execution
//! then sizes its per-row buffers by the root `row_count` or by a relation's
//! related count: broadcast literals, operator results, row masks,
//! projections and cached rule columns. When nothing backs a count (a
//! `row_count` with no root column, or a stated `related_row_count` with no
//! related column), those buffers could ask for more than the host has. Each
//! is now reserved with `try_reserve_exact`, so a refused buffer is a
//! `TypeMismatch` naming its count, not an aborted process.
//!
//! Every case here would abort on regression, so they live in their own test
//! binary. Two allocators refuse:
//!
//! - **The host.** A batch of 2^50 or more rows asks for at least a pebibyte,
//!   which no host grants.
//! - **This binary's allocator.** It refuses any single allocation above a
//!   cap that a test sets. A relation's 8-byte owners can fit under the cap
//!   while a wider buffer for the same rows does not. Binding never sees that
//!   case, and here it is reachable with small, real batches.
//!
//! The allocator also refuses anything that would take this binary's live
//! bytes past 1 GiB. A regression that walks the rows allocating as it goes,
//! such as recording an error for each of 2^50 rows, then aborts promptly
//! instead of exhausting the host.
//!
//! Invariants, checked by the properties below:
//!
//! 1. **Refusal.** For every generated program and every root row count from
//!    2^50 to 2^57, with nothing backing the count, `execute`, `execute_f64`
//!    and `execute_lifetime[_f64]` return the refusal naming that count. They
//!    never panic, abort or walk the rows.
//! 2. **Differential.** For every generated program and small batch,
//!    execution under any cap returns exactly what it returns uncapped, or a
//!    refusal naming one of the batch's counts whose widest buffer exceeds the
//!    cap. When every count's widest buffer fits under the cap, it returns
//!    exactly what it returns uncapped.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::ProgramSpec;
use chrono::NaiveDate;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::{Config, TestRunner};
use rust_decimal::Decimal;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// The allocator. Every test takes `SERIAL`, so a cap one test sets never
// refuses another test's allocations.
// ---------------------------------------------------------------------------

struct Guarded;

/// The largest single allocation granted; `usize::MAX` grants everything.
static CAP: AtomicUsize = AtomicUsize::new(usize::MAX);
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// More live bytes than any case here needs by two orders of magnitude.
const LIVE_CEILING: usize = 1 << 30;
static SERIAL: Mutex<()> = Mutex::new(());

impl Guarded {
    fn over_cap(size: usize) -> bool {
        size > CAP.load(Ordering::SeqCst)
    }

    /// Count `size` more live bytes, unless that passes the ceiling.
    fn admit(size: usize) -> bool {
        let before = LIVE.fetch_add(size, Ordering::SeqCst);
        if before.saturating_add(size) > LIVE_CEILING {
            LIVE.fetch_sub(size, Ordering::SeqCst);
            return false;
        }
        true
    }
}

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::over_cap(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator, which decides
        // first, so a request no host can grant is the host's refusal.
        let pointer = unsafe { System.alloc(layout) };
        if pointer.is_null() || Self::admit(layout.size()) {
            return pointer;
        }
        // SAFETY: `pointer` was just allocated by `System` with `layout`.
        unsafe { System.dealloc(pointer, layout) };
        std::ptr::null_mut()
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if Self::over_cap(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: as in `alloc`.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if pointer.is_null() || Self::admit(layout.size()) {
            return pointer;
        }
        // SAFETY: `pointer` was just allocated by `System` with `layout`.
        unsafe { System.dealloc(pointer, layout) };
        std::ptr::null_mut()
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if Self::over_cap(new_size) {
            return std::ptr::null_mut();
        }
        let old_size = layout.size();
        if new_size > old_size && !Self::admit(new_size - old_size) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if new_size > old_size {
            if moved.is_null() {
                LIVE.fetch_sub(new_size - old_size, Ordering::SeqCst);
            }
        } else if !moved.is_null() {
            LIVE.fetch_sub(old_size - new_size, Ordering::SeqCst);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lifts the cap when dropped, so a failing assertion never leaves it set.
struct CapGuard;

impl Drop for CapGuard {
    fn drop(&mut self) {
        CAP.store(usize::MAX, Ordering::SeqCst);
    }
}

/// Run `run` with every single allocation above `cap` bytes refused.
fn capped<T>(cap: usize, run: impl FnOnce() -> T) -> T {
    let _lift = CapGuard;
    CAP.store(cap, Ordering::SeqCst);
    run()
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// The count a refusal names: execution's refused buffer, or binding's
/// refused owners. `None` for any other result.
fn refused_count(error: &EvalError) -> Option<usize> {
    let EvalError::TypeMismatch(message) = error else {
        return None;
    };
    let head = message.strip_suffix(" rows, more than this process can allocate")?;
    let head = head.strip_suffix(" related").unwrap_or(head);
    head.rsplit(' ').next()?.parse().ok()
}

fn assert_refused<T: std::fmt::Debug>(result: Result<T, EvalError>, count: usize, case: &str) {
    match result {
        Err(error) => assert_eq!(
            refused_count(&error),
            Some(count),
            "{case}: expected the refusal naming {count} rows, got {error:?}"
        ),
        Ok(value) => panic!("{case}: expected the refusal naming {count} rows, got {value:?}"),
    }
}

/// Root row counts no host can allocate a buffer for, all within the bound
/// binding enforces (`isize::MAX` over the widest element).
const HUGE_ROWS: [usize; 3] = [1 << 50, 1 << 55, 1 << 57];

/// The widest element a dense buffer holds per row (a `String` or a
/// `ScalarValue`), so `count * WIDEST` bounds any count-sized buffer.
const WIDEST: usize = 24;

// ---------------------------------------------------------------------------
// Programs, written as compiled-program JSON on a `Household` root
// ---------------------------------------------------------------------------

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn decimal(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "decimal", "value": value } })
}

fn boolean(value: bool) -> Value {
    json!({ "kind": "literal", "value": { "kind": "bool", "value": value } })
}

fn text(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "text", "value": value } })
}

fn date(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "date", "value": value } })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn input_or_else(name: &str, default: i64) -> Value {
    json!({ "kind": "input_or_else", "name": name, "default": { "kind": "integer", "value": default } })
}

fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn binary(kind: &str, left: Value, right: Value) -> Value {
    json!({ "kind": kind, "left": left, "right": right })
}

fn items(kind: &str, items: Vec<Value>) -> Value {
    json!({ "kind": kind, "items": items })
}

fn unary(kind: &str, value: Value) -> Value {
    json!({ "kind": kind, "value": value })
}

fn date_shift(kind: &str, date: Value, count: Value) -> Value {
    let field = match kind {
        "date_add_days" => "days",
        "date_add_months" => "months",
        _ => "years",
    };
    json!({ "kind": kind, "date": date, field: count })
}

fn days_between(from: Value, to: Value) -> Value {
    json!({ "kind": "days_between", "from": from, "to": to })
}

fn if_then_else(condition: Value, then_expr: Value, else_expr: Value) -> Value {
    json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": else_expr })
}

fn no_match(subject: Value, patterns: Vec<Value>) -> Value {
    json!({ "kind": "no_match", "subject": subject, "patterns": patterns })
}

fn lookup(parameter: &str, index: Value) -> Value {
    json!({ "kind": "parameter_lookup", "parameter": parameter, "index": index })
}

fn over_periods(over: &str, value: Value, n: Option<Value>) -> Value {
    match n {
        Some(n) => json!({ "kind": "over_periods", "over": over, "value": value, "n": n }),
        None => json!({ "kind": "over_periods", "over": over, "value": value }),
    }
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn count_related(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1 });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn sum_related(relation: &str, value: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "sum_related", "relation": relation, "current_slot": 0, "related_slot": 1,
        "value": { "kind": "input", "name": value },
    });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn member_of(relation: &str) -> Value {
    json!({ "kind": "relation_member", "relation": relation, "current_slot": 0, "related_slot": 1 })
}

fn scalar_rule(name: &str, dtype: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": "Household", "dtype": dtype, "unit": null, "semantics": "scalar", "expr": expr })
}

fn judgment_rule(name: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": "Household", "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

/// `rule`, commencing on `from`.
fn commencing(mut rule: Value, from: &str) -> Value {
    rule["versions"] = json!([{
        "effective_from": from,
        "semantics": rule["semantics"].clone(),
        "expr": rule["expr"].clone(),
    }]);
    rule
}

/// `rule`, in dollars rounded half up to cents.
fn rounded(mut rule: Value) -> Value {
    rule["unit"] = json!("USD");
    rule["rounding"] = json!("half_up");
    rule
}

/// A program over `rules`. `member` relates households to people, and
/// `adult` is the members whose `age` is at least 18.
fn program(rules: Vec<Value>, relations: bool) -> Value {
    let pair = json!(["Household", "Person"]);
    let relations = if relations {
        json!([
            { "name": "member", "arity": 2, "slot_entities": pair },
            {
                "name": "adult", "arity": 2, "slot_entities": pair,
                "derivation": {
                    "source_relation": "member", "current_slot": 0, "related_slot": 1,
                    "slot_entities": pair,
                    "predicate": compare(input("age"), "gte", integer(18)),
                },
            },
        ])
    } else {
        json!([])
    };
    let table = |values: Value| json!([{ "effective_from": "2000-01-01", "values": values }]);
    json!({
        "units": [{ "name": "USD", "kind": "currency", "minor_units": 2 }],
        "relations": relations,
        "parameters": [
            { "name": "p", "unit": null, "versions": table(json!({
                "0": { "kind": "integer", "value": 10 },
                "1": { "kind": "integer", "value": 20 },
                "2": { "kind": "integer", "value": 30 },
            })) },
            { "name": "q", "unit": null, "versions": table(json!({
                "0": { "kind": "decimal", "value": "0.5" },
                "1": { "kind": "decimal", "value": "1.25" },
            })) },
            { "name": "t", "unit": null, "versions": table(json!({
                "0": { "kind": "text", "value": "zero" },
                "1": { "kind": "text", "value": "one" },
            })) },
        ],
        "derived": rules,
    })
}

fn compile(program: &Value) -> Result<DenseCompiledProgram, String> {
    let spec: ProgramSpec =
        serde_json::from_value(program.clone()).map_err(|error| error.to_string())?;
    let artifact = CompiledProgramArtifact::compile(spec).map_err(|error| error.to_string())?;
    DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .map_err(|error| error.to_string())
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("date")
}

fn month() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: day("2026-01-01"),
        end: day("2026-01-31"),
    }
}

fn years(count: usize) -> Vec<Period> {
    (0..count)
        .map(|index| {
            let year = 2024 + i32::try_from(index).expect("few periods");
            Period {
                kind: PeriodKind::TaxYear,
                start: NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
                end: NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
            }
        })
        .collect()
}

fn member_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member".to_string(),
        current_slot: 0,
        related_slot: 1,
    }
}

fn empty_batch(row_count: usize) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count,
        inputs: HashMap::new(),
        relations: HashMap::new(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Decimal,
    Float,
}

const MODES: [Mode; 2] = [Mode::Decimal, Mode::Float];

fn execute(
    program: &DenseCompiledProgram,
    mode: Mode,
    batch: DenseBatchSpec,
    outputs: &[String],
) -> Result<DenseExecutionResult, EvalError> {
    match mode {
        Mode::Decimal => program.execute(&month(), batch, outputs),
        Mode::Float => program.execute_f64(&month(), batch, outputs),
    }
}

fn execute_lifetime(
    program: &DenseCompiledProgram,
    mode: Mode,
    batches: Vec<DenseBatchSpec>,
    outputs: &[String],
) -> Result<DenseExecutionResult, EvalError> {
    let periods = years(batches.len());
    match mode {
        Mode::Decimal => program.execute_lifetime(&periods, batches, outputs),
        Mode::Float => program.execute_lifetime_f64(&periods, batches, outputs),
    }
}

/// A result in comparable form: each output's values, or the error.
fn summary(
    result: &Result<DenseExecutionResult, EvalError>,
) -> Result<BTreeMap<String, String>, String> {
    match result {
        Ok(result) => Ok(result
            .outputs
            .iter()
            .map(|(name, value)| (name.clone(), format!("{value:?}")))
            .collect()),
        Err(error) => Err(format!("{error:?}")),
    }
}

fn names(rules: &[Value]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| rule["name"].as_str().expect("rule name").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// A huge root row count with nothing backing it
// ---------------------------------------------------------------------------

/// One rule for each way the per-period executor fills a root-row buffer.
fn root_cases() -> Vec<(&'static str, Vec<Value>)> {
    let start = || json!({ "kind": "period_start" });
    let end = || json!({ "kind": "period_end" });
    let judgment = |expr: Value| vec![judgment_rule("out", expr)];
    let scalar = |dtype: &str, expr: Value| vec![scalar_rule("out", dtype, expr)];
    vec![
        ("integer literal", scalar("integer", integer(5))),
        ("decimal literal", scalar("decimal", decimal("2.5"))),
        ("bool literal", scalar("bool", boolean(true))),
        ("text literal", scalar("text", text("a"))),
        ("date literal", scalar("date", date("2026-01-15"))),
        ("period start", scalar("date", start())),
        ("period end", scalar("date", end())),
        ("missing input", scalar("integer", input("x"))),
        ("input or else", scalar("integer", input_or_else("x", 3))),
        ("parameter", scalar("integer", lookup("p", integer(1)))),
        (
            "decimal parameter",
            scalar("decimal", lookup("q", integer(1))),
        ),
        ("text parameter", scalar("text", lookup("t", integer(1)))),
        (
            "add",
            scalar("integer", items("add", vec![integer(5), integer(1)])),
        ),
        (
            "sub",
            scalar("integer", binary("sub", integer(5), integer(1))),
        ),
        (
            "mul",
            scalar("integer", binary("mul", integer(5), integer(2))),
        ),
        (
            "div",
            scalar("decimal", binary("div", integer(5), integer(2))),
        ),
        (
            "max",
            scalar("integer", items("max", vec![integer(5), integer(1)])),
        ),
        (
            "min",
            scalar("integer", items("min", vec![integer(5), integer(1)])),
        ),
        ("ceil", scalar("integer", unary("ceil", decimal("2.5")))),
        ("floor", scalar("integer", unary("floor", decimal("2.5")))),
        (
            "date add days",
            scalar("date", date_shift("date_add_days", start(), integer(3))),
        ),
        (
            "date add months",
            scalar("date", date_shift("date_add_months", start(), integer(3))),
        ),
        (
            "date add years",
            scalar("date", date_shift("date_add_years", start(), integer(3))),
        ),
        (
            "days between",
            scalar("integer", days_between(start(), end())),
        ),
        (
            "if",
            scalar(
                "integer",
                if_then_else(
                    compare(integer(5), "gt", integer(3)),
                    integer(1),
                    integer(2),
                ),
            ),
        ),
        (
            "no match",
            scalar(
                "integer",
                no_match(integer(5), vec![integer(1), integer(2)]),
            ),
        ),
        (
            "comparison",
            judgment(compare(integer(5), "gt", integer(3))),
        ),
        (
            "text comparison",
            judgment(compare(text("a"), "eq", text("a"))),
        ),
        (
            "and",
            judgment(items(
                "and",
                vec![
                    compare(integer(5), "gt", integer(3)),
                    compare(integer(1), "lt", integer(2)),
                ],
            )),
        ),
        (
            "or",
            judgment(items(
                "or",
                vec![
                    compare(integer(5), "lt", integer(3)),
                    compare(integer(1), "lt", integer(2)),
                ],
            )),
        ),
        (
            "not",
            judgment(json!({ "kind": "not", "item": compare(integer(5), "gt", integer(3)) })),
        ),
        (
            "derived chain",
            vec![
                scalar_rule("five", "integer", integer(5)),
                scalar_rule(
                    "out",
                    "integer",
                    items("add", vec![derived("five"), integer(1)]),
                ),
            ],
        ),
        (
            "derived judgment",
            vec![
                judgment_rule("big", compare(integer(5), "gt", integer(3))),
                judgment_rule("out", json!({ "kind": "not", "item": derived("big") })),
            ],
        ),
        (
            "rounded",
            vec![rounded(scalar_rule("out", "decimal", decimal("2.555")))],
        ),
        // Before commencement every row that reaches the rule fails. The
        // formula is typed first, under no rows, and its column is the first
        // buffer, so no per-row error is recorded for rows that cannot exist.
        (
            "before commencement",
            vec![commencing(
                scalar_rule("out", "integer", integer(5)),
                "2027-01-01",
            )],
        ),
        (
            "judgment before commencement",
            vec![commencing(
                judgment_rule("out", compare(integer(5), "gt", integer(3))),
                "2027-01-01",
            )],
        ),
        (
            "after commencement",
            vec![commencing(
                scalar_rule("out", "integer", integer(5)),
                "2020-01-01",
            )],
        ),
    ]
}

#[test]
fn a_huge_root_row_count_is_refused_by_every_node_kind() {
    let _serial = serial();
    for (case, rules) in root_cases() {
        let program =
            compile(&program(rules, false)).unwrap_or_else(|error| panic!("{case}: {error}"));
        let outputs = vec!["out".to_string()];
        for mode in MODES {
            for rows in HUGE_ROWS {
                assert_refused(
                    execute(&program, mode, empty_batch(rows), &outputs),
                    rows,
                    &format!("{case}, {mode:?}, {rows} rows"),
                );
            }
        }
    }
}

/// One rule for each way the lifetime executor fills a root-row buffer.
fn lifetime_cases() -> Vec<(&'static str, Value)> {
    let five = || integer(5);
    vec![
        ("sum over periods", over_periods("sum", five(), None)),
        (
            "max over periods",
            over_periods("max", decimal("2.5"), None),
        ),
        ("count over periods", over_periods("count", five(), None)),
        (
            "sum of the top n",
            over_periods("sum_top_n", five(), Some(integer(1))),
        ),
        ("top n with no n", over_periods("sum_top_n", five(), None)),
        (
            "period-invariant input",
            items(
                "add",
                vec![over_periods("sum", five(), None), input("base")],
            ),
        ),
        (
            "literal beside a reduction",
            items("add", vec![over_periods("sum", five(), None), five()]),
        ),
        (
            "parameter beside a reduction",
            items(
                "add",
                vec![over_periods("sum", five(), None), lookup("p", integer(1))],
            ),
        ),
        (
            "ambiguous leaf",
            items(
                "add",
                vec![
                    over_periods("sum", five(), None),
                    days_between(
                        json!({ "kind": "period_start" }),
                        json!({ "kind": "period_end" }),
                    ),
                ],
            ),
        ),
        (
            "if beside a reduction",
            if_then_else(
                compare(over_periods("sum", five(), None), "gt", five()),
                five(),
                integer(2),
            ),
        ),
    ]
}

#[test]
fn a_huge_lifetime_row_count_is_refused_by_every_reduction() {
    let _serial = serial();
    for (case, expr) in lifetime_cases() {
        let rules = vec![scalar_rule("out", "decimal", expr)];
        let program =
            compile(&program(rules, false)).unwrap_or_else(|error| panic!("{case}: {error}"));
        let outputs = vec!["out".to_string()];
        for mode in MODES {
            for periods in [1, 3] {
                for rows in HUGE_ROWS {
                    let batches = (0..periods).map(|_| empty_batch(rows)).collect();
                    assert_refused(
                        execute_lifetime(&program, mode, batches, &outputs),
                        rows,
                        &format!("{case}, {mode:?}, {periods} periods, {rows} rows"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A count whose narrow buffers fit and whose wide ones do not
// ---------------------------------------------------------------------------

/// Related rows stated for one household, with no related column. The
/// owners (8 bytes a row) fit under `RELATED_CAP`; a decimal (16 bytes) or
/// text (24 bytes) buffer for the same rows does not.
const STATED_RELATED: usize = 1 << 16;
const RELATED_CAP: usize = 12 * STATED_RELATED;

fn stated_relation_batch(related: usize) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            member_key(),
            DenseRelationBatchSpec {
                offsets: vec![0, related],
                inputs: HashMap::new(),
                related_row_count: Some(related),
            },
        )]),
    }
}

/// Programs over the related rows, each with the output it gives uncapped
/// for one household of `STATED_RELATED` members, and whether its buffers
/// are wider than the owners in each mode.
struct RelatedCase {
    name: &'static str,
    rules: Vec<Value>,
    /// Decimal mode, then `f64` mode: `true` when a buffer is wider than 8
    /// bytes a row, so the cap refuses it.
    wider: [bool; 2],
}

fn related_cases() -> Vec<RelatedCase> {
    let count_where = |where_clause: Value| {
        vec![scalar_rule(
            "out",
            "integer",
            count_related("member", Some(where_clause)),
        )]
    };
    vec![
        RelatedCase {
            name: "text literal on related rows",
            rules: count_where(compare(text("a"), "eq", text("a"))),
            wider: [true, true],
        },
        RelatedCase {
            name: "decimal literal on related rows",
            rules: count_where(compare(decimal("1.5"), "gt", integer(1))),
            wider: [true, false],
        },
        RelatedCase {
            // Decimal mode compares integers as decimals.
            name: "integer literal on related rows",
            rules: count_where(compare(integer(2), "gt", integer(1))),
            wider: [true, false],
        },
        RelatedCase {
            name: "decimal root rule projected to related rows",
            rules: vec![
                scalar_rule("rate", "decimal", decimal("1.5")),
                scalar_rule(
                    "out",
                    "integer",
                    count_related("member", Some(compare(derived("rate"), "gt", integer(1)))),
                ),
            ],
            wider: [true, false],
        },
        RelatedCase {
            name: "text root rule projected to related rows",
            rules: vec![
                scalar_rule("label", "text", text("a")),
                scalar_rule(
                    "out",
                    "integer",
                    count_related("member", Some(compare(derived("label"), "eq", text("a")))),
                ),
            ],
            wider: [true, true],
        },
        RelatedCase {
            name: "root judgment projected to related rows",
            rules: vec![
                judgment_rule("eligible", compare(integer(5), "gt", integer(3))),
                scalar_rule(
                    "out",
                    "integer",
                    count_related("member", Some(derived("eligible"))),
                ),
            ],
            wider: [false, false],
        },
        RelatedCase {
            name: "missing related input",
            rules: vec![scalar_rule(
                "out",
                "decimal",
                sum_related("member", "income", None),
            )],
            wider: [true, false],
        },
        RelatedCase {
            name: "relation member outside a derived relation",
            rules: count_where(member_of("member")),
            wider: [false, false],
        },
        RelatedCase {
            name: "date shift on related rows",
            rules: count_where(compare(
                date_shift(
                    "date_add_days",
                    json!({ "kind": "period_start" }),
                    integer(1),
                ),
                "gt",
                json!({ "kind": "period_start" }),
            )),
            wider: [false, false],
        },
    ]
}

#[test]
fn a_stated_related_count_that_fits_the_owners_but_not_a_wider_buffer_is_refused() {
    let _serial = serial();
    // The owners fit, so binding accepts the count; no wider buffer does.
    assert!(STATED_RELATED * 8 <= RELATED_CAP && STATED_RELATED * 16 > RELATED_CAP);
    for case in related_cases() {
        let program = compile(&program(case.rules, true))
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let outputs = vec!["out".to_string()];
        for (mode, wider) in MODES.into_iter().zip(case.wider) {
            let label = format!("{}, {mode:?}", case.name);
            let uncapped = execute(
                &program,
                mode,
                stated_relation_batch(STATED_RELATED),
                &outputs,
            );
            let batch = stated_relation_batch(STATED_RELATED);
            let result = capped(RELATED_CAP, || execute(&program, mode, batch, &outputs));
            if wider {
                assert_refused(result, STATED_RELATED, &label);
            } else {
                assert_eq!(summary(&result), summary(&uncapped), "{label}");
            }
        }
    }
}

#[test]
fn a_root_column_backs_its_count_but_not_a_wider_buffer() {
    let _serial = serial();
    // An integer root column is 8 bytes a row, under the cap. A decimal or
    // text buffer over the same rows is not.
    let rows = 1 << 14;
    let cap = 12 * rows;
    let start = || json!({ "kind": "period_start" });
    let end = || json!({ "kind": "period_end" });
    let program = compile(&program(
        vec![
            scalar_rule("copied", "integer", input("a")),
            judgment_rule("dated", compare(start(), "lt", end())),
            scalar_rule("doubled", "integer", binary("mul", input("a"), integer(2))),
            judgment_rule("named", compare(text("a"), "eq", text("a"))),
        ],
        false,
    ))
    .expect("program compiles");
    let batch = || DenseBatchSpec {
        row_count: rows,
        inputs: HashMap::from([("a".to_string(), DenseColumn::Integer(vec![1; rows]))]),
        relations: HashMap::new(),
    };
    // Whether each output needs a buffer wider than 8 bytes a row, in
    // Decimal mode and then `f64` mode. Decimal mode multiplies integers as
    // decimals.
    for (output, wider) in [
        ("copied", [false, false]),
        ("dated", [false, false]),
        ("doubled", [true, false]),
        ("named", [true, true]),
    ] {
        let outputs = vec![output.to_string()];
        for (mode, wider) in MODES.into_iter().zip(wider) {
            let label = format!("{output}, {mode:?}");
            let uncapped = execute(&program, mode, batch(), &outputs);
            assert!(uncapped.is_ok(), "{label}: {uncapped:?}");
            let input = batch();
            let result = capped(cap, || execute(&program, mode, input, &outputs));
            if wider {
                assert_refused(result, rows, &label);
            } else {
                assert_eq!(summary(&result), summary(&uncapped), "{label}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Generated programs
// ---------------------------------------------------------------------------

/// The earlier rules a generated formula may read, by kind, and whether
/// the program has relations.
#[derive(Clone, Debug, Default)]
struct Scope {
    numbers: Vec<String>,
    dates: Vec<String>,
    judgments: Vec<String>,
    relations: bool,
}

fn op() -> impl Strategy<Value = &'static str> {
    select(vec!["lt", "lte", "gt", "gte", "eq", "ne"])
}

fn union(strategies: Vec<BoxedStrategy<Value>>) -> BoxedStrategy<Value> {
    proptest::strategy::Union::new(strategies).boxed()
}

fn number(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        (-3_i64..=5).prop_map(integer).boxed(),
        select(vec!["1.5", "-0.25", "0", "1000000.125"])
            .prop_map(decimal)
            .boxed(),
        select(vec!["a", "b"]).prop_map(input).boxed(),
        Just(input_or_else("c", 2)).boxed(),
    ];
    if !scope.numbers.is_empty() {
        leaves.push(
            select(scope.numbers.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    if scope.relations {
        let predicate = related_judgment(scope, 1);
        leaves.push(
            (
                select(vec!["member", "adult"]),
                prop::option::of(predicate.clone()),
            )
                .prop_map(|(relation, predicate)| count_related(relation, predicate))
                .boxed(),
        );
        leaves.push(
            (
                select(vec!["member", "adult"]),
                select(vec!["income", "age"]),
                prop::option::of(predicate),
            )
                .prop_map(|(relation, value, predicate)| sum_related(relation, value, predicate))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = number(scope, depth - 1);
    let dates = date_expr(scope, depth - 1);
    let condition = judgment(scope, depth - 1);
    prop_oneof![
        4 => leaf,
        1 => prop::collection::vec(inner.clone(), 1..=3).prop_map(|operands| items("add", operands)),
        2 => (select(vec!["sub", "mul", "div"]), inner.clone(), inner.clone())
            .prop_map(|(kind, left, right)| binary(kind, left, right)),
        1 => (select(vec!["max", "min"]), prop::collection::vec(inner.clone(), 1..=3))
            .prop_map(|(kind, operands)| items(kind, operands)),
        1 => (select(vec!["ceil", "floor"]), inner.clone()).prop_map(|(kind, value)| unary(kind, value)),
        2 => (condition, inner.clone(), inner.clone())
            .prop_map(|(condition, then_expr, else_expr)| if_then_else(condition, then_expr, else_expr)),
        1 => (dates.clone(), dates).prop_map(|(from, to)| days_between(from, to)),
        1 => (inner.clone(), prop::collection::vec((-3_i64..=5).prop_map(integer), 1..=2))
            .prop_map(|(subject, patterns)| no_match(subject, patterns)),
        1 => (select(vec!["p", "q"]), inner).prop_map(|(parameter, index)| lookup(parameter, index)),
    ]
    .boxed()
}

fn date_expr(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        Just(json!({ "kind": "period_start" })).boxed(),
        Just(json!({ "kind": "period_end" })).boxed(),
        Just(date("2026-01-15")).boxed(),
        Just(input("d")).boxed(),
    ];
    if !scope.dates.is_empty() {
        leaves.push(
            select(scope.dates.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = date_expr(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        1 => (
            select(vec!["date_add_days", "date_add_months", "date_add_years"]),
            inner,
            number(scope, depth - 1),
        )
            .prop_map(|(kind, date, count)| date_shift(kind, date, count)),
    ]
    .boxed()
}

fn judgment(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        (number(scope, 0), op(), number(scope, 0))
            .prop_map(|(left, op, right)| compare(left, op, right))
            .boxed(),
        (date_expr(scope, 0), op(), date_expr(scope, 0))
            .prop_map(|(left, op, right)| compare(left, op, right))
            .boxed(),
        (
            select(vec!["eq", "ne"]),
            select(vec!["zero", "one"]),
            number(scope, 0),
        )
            .prop_map(|(op, label, index)| compare(lookup("t", index), op, text(label)))
            .boxed(),
    ];
    if !scope.judgments.is_empty() {
        leaves.push(
            select(scope.judgments.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = judgment(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        1 => (number(scope, depth - 1), op(), number(scope, depth - 1))
            .prop_map(|(left, op, right)| compare(left, op, right)),
        1 => (select(vec!["and", "or"]), prop::collection::vec(inner.clone(), 1..=3))
            .prop_map(|(kind, operands)| items(kind, operands)),
        1 => inner.prop_map(|item| json!({ "kind": "not", "item": item })),
    ]
    .boxed()
}

/// A value on the related rows: a related input, a literal, or a root rule
/// projected to its members.
fn related_number(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let mut leaves = vec![
        (-3_i64..=40).prop_map(integer).boxed(),
        select(vec!["1.5", "-0.25"]).prop_map(decimal).boxed(),
        select(vec!["income", "age"]).prop_map(input).boxed(),
        Just(input_or_else("age", 30)).boxed(),
    ];
    if !scope.numbers.is_empty() {
        leaves.push(
            select(scope.numbers.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = related_number(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        1 => (select(vec!["sub", "mul", "div"]), inner.clone(), inner.clone())
            .prop_map(|(kind, left, right)| binary(kind, left, right)),
        1 => prop::collection::vec(inner.clone(), 1..=2).prop_map(|operands| items("max", operands)),
        1 => (related_judgment(scope, depth - 1), inner.clone(), inner)
            .prop_map(|(condition, then_expr, else_expr)| if_then_else(condition, then_expr, else_expr)),
    ]
    .boxed()
}

fn related_judgment(scope: &Scope, depth: u32) -> BoxedStrategy<Value> {
    let number_scope = Scope {
        relations: false,
        ..scope.clone()
    };
    let mut leaves = vec![
        (
            related_number(&number_scope, 0),
            op(),
            related_number(&number_scope, 0),
        )
            .prop_map(|(left, op, right)| compare(left, op, right))
            .boxed(),
        Just(compare(text("a"), "eq", text("a"))).boxed(),
        Just(member_of("member")).boxed(),
        Just(compare(
            date_shift(
                "date_add_days",
                json!({ "kind": "period_start" }),
                input("age"),
            ),
            "gt",
            json!({ "kind": "period_end" }),
        ))
        .boxed(),
    ];
    if !scope.judgments.is_empty() {
        leaves.push(
            select(scope.judgments.clone())
                .prop_map(|name| derived(&name))
                .boxed(),
        );
    }
    let leaf = union(leaves);
    if depth == 0 {
        return leaf;
    }
    let inner = related_judgment(scope, depth - 1);
    prop_oneof![
        3 => leaf,
        1 => (related_number(&number_scope, depth - 1), op(), related_number(&number_scope, depth - 1))
            .prop_map(|(left, op, right)| compare(left, op, right)),
        1 => (select(vec!["and", "or"]), prop::collection::vec(inner.clone(), 1..=2))
            .prop_map(|(kind, operands)| items(kind, operands)),
        1 => inner.prop_map(|item| json!({ "kind": "not", "item": item })),
    ]
    .boxed()
}

/// When a generated rule commences: always, before the period, or after it.
fn commencement() -> impl Strategy<Value = Option<&'static str>> {
    prop_oneof![
        6 => Just(None),
        1 => Just(Some("2020-01-01")),
        1 => Just(Some("2027-01-01")),
    ]
}

fn finish(rule: Value, from: Option<&str>, round: bool) -> Value {
    let rule = if round && rule["dtype"] == "decimal" {
        rounded(rule)
    } else {
        rule
    };
    match from {
        Some(from) => commencing(rule, from),
        None => rule,
    }
}

/// Six rules, each reading only earlier ones: numbers `r0`, `r3` and `r5`,
/// judgments `r1` and `r4`, and the date `r2`.
fn rules(relations: bool) -> impl Strategy<Value = Vec<Value>> {
    let scopes = [
        Scope {
            relations,
            ..Scope::default()
        },
        Scope {
            numbers: vec!["r0".into()],
            relations,
            ..Scope::default()
        },
        Scope {
            numbers: vec!["r0".into()],
            judgments: vec!["r1".into()],
            relations,
            ..Scope::default()
        },
        Scope {
            numbers: vec!["r0".into()],
            dates: vec!["r2".into()],
            judgments: vec!["r1".into()],
            relations,
        },
        Scope {
            numbers: vec!["r0".into(), "r3".into()],
            dates: vec!["r2".into()],
            judgments: vec!["r1".into()],
            relations,
        },
        Scope {
            numbers: vec!["r0".into(), "r3".into()],
            dates: vec!["r2".into()],
            judgments: vec!["r1".into(), "r4".into()],
            relations,
        },
    ];
    let expressions = (
        number(&scopes[0], 2),
        judgment(&scopes[1], 2),
        date_expr(&scopes[2], 2),
        number(&scopes[3], 2),
        judgment(&scopes[4], 2),
        number(&scopes[5], 2),
    );
    let finishing = prop::collection::vec((commencement(), any::<bool>()), 6);
    (expressions, finishing).prop_map(|((r0, r1, r2, r3, r4, r5), finishing)| {
        [
            scalar_rule("r0", "decimal", r0),
            judgment_rule("r1", r1),
            scalar_rule("r2", "date", r2),
            scalar_rule("r3", "decimal", r3),
            judgment_rule("r4", r4),
            scalar_rule("r5", "decimal", r5),
        ]
        .into_iter()
        .zip(finishing)
        .map(|(rule, (from, round))| finish(rule, from, round))
        .collect()
    })
}

/// A lifetime output: a reduction of a generated number, with a leaf
/// beside it, over helper rules `r0` to `r5`.
fn lifetime_rules() -> impl Strategy<Value = Vec<Value>> {
    let scope = Scope {
        numbers: vec!["r0".into(), "r3".into()],
        dates: vec!["r2".into()],
        judgments: vec!["r1".into()],
        relations: false,
    };
    let reduction = (
        select(vec!["sum", "max", "count", "sum_top_n"]),
        number(&scope, 1),
        prop::option::of(prop_oneof![(0_i64..=3).prop_map(integer), Just(input("n"))]),
    )
        .prop_map(|(over, value, n)| {
            over_periods(over, value, if over == "sum_top_n" { n } else { None })
        });
    let beside = prop_oneof![
        Just(None),
        Just(Some(input("base"))),
        Just(Some(input_or_else("c", 2))),
        (-3_i64..=5).prop_map(|value| Some(integer(value))),
        Just(Some(lookup("p", integer(1)))),
        Just(Some(derived("r0"))),
        Just(Some(json!({ "kind": "period_start" }))),
    ];
    (rules(false), reduction, beside).prop_map(|(mut rules, reduction, beside)| {
        let expr = match beside {
            Some(beside) => items("add", vec![reduction, beside]),
            None => reduction,
        };
        rules.push(scalar_rule("life", "decimal", expr));
        rules
    })
}

// ---------------------------------------------------------------------------
// Generated batches
// ---------------------------------------------------------------------------

fn integer_column(rows: usize) -> BoxedStrategy<DenseColumn> {
    prop::collection::vec(-3_i64..=40, rows)
        .prop_map(DenseColumn::Integer)
        .boxed()
}

fn number_column(rows: usize) -> BoxedStrategy<DenseColumn> {
    prop_oneof![
        prop::collection::vec(-300_i64..=4000, rows).prop_map(|values| DenseColumn::Decimal(
            values
                .into_iter()
                .map(|cents| Decimal::new(cents, 2))
                .collect()
        )),
        prop::collection::vec(-30.0_f64..=40.0, rows).prop_map(DenseColumn::Float),
        integer_column(rows),
    ]
    .boxed()
}

fn date_column(rows: usize) -> BoxedStrategy<DenseColumn> {
    prop::collection::vec(0_i64..=400, rows)
        .prop_map(|days| {
            DenseColumn::Date(
                days.into_iter()
                    .map(|offset| day("2025-06-01") + chrono::Days::new(offset as u64))
                    .collect(),
            )
        })
        .boxed()
}

/// Columns by name, each present or not.
fn columns(
    named: Vec<(&'static str, BoxedStrategy<DenseColumn>)>,
) -> BoxedStrategy<HashMap<String, DenseColumn>> {
    named
        .into_iter()
        .map(|(name, column)| {
            prop::option::weighted(0.75, column).prop_map(move |column| (name, column))
        })
        .collect::<Vec<_>>()
        .prop_map(|columns| {
            columns
                .into_iter()
                .filter_map(|(name, column)| column.map(|column| (name.to_string(), column)))
                .collect()
        })
        .boxed()
}

fn root_rows() -> impl Strategy<Value = usize> {
    prop_oneof![0_usize..=4, 5_usize..=64, 65_usize..=1500]
}

fn root_columns(rows: usize) -> BoxedStrategy<HashMap<String, DenseColumn>> {
    columns(vec![
        ("a", integer_column(rows)),
        ("b", number_column(rows)),
        ("c", integer_column(rows)),
        ("d", date_column(rows)),
        ("n", integer_column(rows)),
        ("base", integer_column(rows)),
    ])
}

/// A batch of `rows` households, each with up to six members.
fn batch(relations: bool) -> impl Strategy<Value = DenseBatchSpec> {
    root_rows().prop_flat_map(move |rows| {
        let members = prop::collection::vec(0_usize..=6, rows);
        (Just(rows), root_columns(rows), members, any::<bool>()).prop_flat_map(
            move |(rows, inputs, members, state_count)| {
                let mut offsets = vec![0];
                for count in &members {
                    offsets.push(offsets.last().copied().unwrap_or(0) + count);
                }
                let related = offsets.last().copied().unwrap_or(0);
                let related_columns = columns(vec![
                    ("income", number_column(related)),
                    ("age", integer_column(related)),
                ]);
                (
                    Just(rows),
                    Just(inputs),
                    Just(offsets),
                    related_columns,
                    Just(state_count),
                )
                    .prop_map(
                        move |(rows, inputs, offsets, related_inputs, state_count)| {
                            let related = offsets.last().copied().unwrap_or(0);
                            // Binding requires a stated count when no related
                            // column confirms one.
                            let state_count =
                                state_count || (related > 0 && related_inputs.is_empty());
                            DenseBatchSpec {
                                row_count: rows,
                                inputs: inputs.clone(),
                                relations: if relations {
                                    HashMap::from([(
                                        member_key(),
                                        DenseRelationBatchSpec {
                                            offsets,
                                            inputs: related_inputs,
                                            related_row_count: state_count.then_some(related),
                                        },
                                    )])
                                } else {
                                    HashMap::new()
                                },
                            }
                        },
                    )
            },
        )
    })
}

/// One to three periods of the same households, each with its own columns.
fn lifetime_batches() -> impl Strategy<Value = Vec<DenseBatchSpec>> {
    (root_rows(), 1_usize..=3).prop_flat_map(|(rows, periods)| {
        prop::collection::vec(root_columns(rows), periods).prop_map(move |periods| {
            periods
                .into_iter()
                .map(|inputs| DenseBatchSpec {
                    row_count: rows,
                    inputs,
                    relations: HashMap::new(),
                })
                .collect()
        })
    })
}

/// The counts a batch sizes buffers by: its root rows and, when it has the
/// relation, its related rows.
fn counts(batch: &DenseBatchSpec) -> Vec<usize> {
    let mut counts = vec![batch.row_count];
    counts.extend(
        batch
            .relations
            .values()
            .filter_map(|relation| relation.offsets.last().copied()),
    );
    counts
}

/// A cap from far below a small batch's narrowest buffer to above its
/// widest, or none.
fn cap() -> impl Strategy<Value = Option<usize>> {
    prop_oneof![
        1 => Just(None),
        4 => (4096_usize..=(1 << 20)).prop_map(Some),
    ]
}

/// The differential: the capped result is the uncapped one, or a refusal
/// naming a count whose widest buffer the cap refuses. When every count's
/// widest buffer fits, it is exactly the uncapped one.
fn check_differential(
    counts: &[usize],
    cap: Option<usize>,
    uncapped: &Result<DenseExecutionResult, EvalError>,
    result: &Result<DenseExecutionResult, EvalError>,
) -> Result<(), TestCaseError> {
    if let Err(error) = uncapped {
        prop_assert!(
            refused_count(error).is_none(),
            "an uncapped small batch was refused: {error:?}"
        );
    }
    let Some(cap) = cap else {
        prop_assert_eq!(summary(result), summary(uncapped));
        return Ok(());
    };
    if let Err(error) = result
        && let Some(count) = refused_count(error)
    {
        prop_assert!(
            counts.contains(&count),
            "refused {count} rows, not a count of {counts:?}"
        );
        prop_assert!(
            count * WIDEST > cap,
            "refused {count} rows, whose widest buffer fits under the {cap}-byte cap"
        );
        return Ok(());
    }
    prop_assert_eq!(summary(result), summary(uncapped));
    Ok(())
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    })
}

#[test]
fn every_generated_program_refuses_a_huge_root_row_count() {
    let _serial = serial();
    let strategy = (
        rules(false),
        select(MODES.to_vec()),
        select(HUGE_ROWS.to_vec()),
    );
    runner(512)
        .run(&strategy, |(rules, mode, rows)| {
            let outputs = names(&rules);
            let Ok(program) = compile(&program(rules, false)) else {
                return Err(TestCaseError::reject("dense does not compile this program"));
            };
            let result = execute(&program, mode, empty_batch(rows), &outputs);
            match &result {
                Err(error) => prop_assert_eq!(refused_count(error), Some(rows), "{:?}", error),
                Ok(_) => prop_assert!(false, "{rows} rows were not refused"),
            }
            Ok(())
        })
        .expect("every program refuses the count");
}

#[test]
fn every_generated_lifetime_program_refuses_a_huge_root_row_count() {
    let _serial = serial();
    let strategy = (
        lifetime_rules(),
        select(MODES.to_vec()),
        select(HUGE_ROWS.to_vec()),
        1_usize..=3,
    );
    runner(256)
        .run(&strategy, |(rules, mode, rows, periods)| {
            let Ok(program) = compile(&program(rules, false)) else {
                return Err(TestCaseError::reject("dense does not compile this program"));
            };
            let batches = (0..periods).map(|_| empty_batch(rows)).collect();
            let result = execute_lifetime(&program, mode, batches, &["life".to_string()]);
            match &result {
                Err(error) => prop_assert_eq!(refused_count(error), Some(rows), "{:?}", error),
                Ok(_) => prop_assert!(false, "{rows} rows were not refused"),
            }
            Ok(())
        })
        .expect("every lifetime program refuses the count");
}

#[test]
fn a_cap_changes_a_result_only_by_refusing_a_count() {
    let _serial = serial();
    let strategy = (any::<bool>())
        .prop_flat_map(|relations| (rules(relations), batch(relations), Just(relations)))
        .prop_flat_map(|(rules, batch, relations)| {
            (
                Just(rules),
                Just(batch),
                Just(relations),
                select(MODES.to_vec()),
                cap(),
            )
        });
    runner(512)
        .run(&strategy, |(rules, batch, relations, mode, cap)| {
            let outputs = names(&rules);
            let Ok(program) = compile(&program(rules, relations)) else {
                return Err(TestCaseError::reject("dense does not compile this program"));
            };
            let counts = counts(&batch);
            let uncapped = execute(&program, mode, batch.clone(), &outputs);
            let result = capped(cap.unwrap_or(usize::MAX), || {
                execute(&program, mode, batch, &outputs)
            });
            if let Some(cap) = cap
                && counts.iter().all(|count| count * WIDEST <= cap)
            {
                prop_assert_eq!(summary(&result), summary(&uncapped));
            }
            check_differential(&counts, cap, &uncapped, &result)
        })
        .expect("a cap only ever refuses a count");
}

#[test]
fn a_cap_changes_a_lifetime_result_only_by_refusing_a_count() {
    let _serial = serial();
    let strategy = (
        lifetime_rules(),
        lifetime_batches(),
        select(MODES.to_vec()),
        cap(),
    );
    runner(256)
        .run(&strategy, |(rules, batches, mode, cap)| {
            let Ok(program) = compile(&program(rules, false)) else {
                return Err(TestCaseError::reject("dense does not compile this program"));
            };
            let counts = vec![batches[0].row_count];
            let outputs = vec!["life".to_string()];
            let uncapped = execute_lifetime(&program, mode, batches.clone(), &outputs);
            let result = capped(cap.unwrap_or(usize::MAX), || {
                execute_lifetime(&program, mode, batches, &outputs)
            });
            check_differential(&counts, cap, &uncapped, &result)
        })
        .expect("a cap only ever refuses a count");
}
