//! A shared inlined body's cache copies its column fallibly.
//!
//! Dense compiles a rule it inlines once per site and caches the column of a
//! body read by more than one node (`tests/dense_inline_sharing.rs`). Each
//! read gets a copy of the cached column. That copy is as long as the rows
//! the body runs on, so it must reserve its buffer fallibly, as #256 made
//! every per-row buffer: a batch can state more related rows than it supplies
//! columns for, and an infallible copy of a column for them would abort the
//! process where the engine should return a refusal.
//!
//! These cases run in their own binary, under the allocator in
//! `tests/support/guarded_alloc.rs`. Each reads one rule twice from one site,
//! so its body is shared and cached, one case per cache: a member rule on the
//! related rows (a scalar and a judgment) and a household rule on the root
//! rows that a derived relation's predicate reads (a scalar and a judgment).
//!
//! - Under a 16 KiB cap, execution returns the refusal naming the batch's
//!   rows, in both numeric modes, or, where every column fits, the answer.
//!   With 1,500 members, their 8-byte owners fit and a 16-byte column for
//!   them does not.
//! - Refusing each allocation of at least 4 KiB in turn, execution returns
//!   the refusal or the uncapped answer every time, and never aborts. This
//!   reaches a judgment cache too, whose 1-byte rows the cap cannot single
//!   out from the 8-byte buffers its rows need first: 5,000 rows make its
//!   column a large allocation.
//!
//! An infallible copy of a cached column aborts this binary.

use std::collections::HashMap;

use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::ProgramSpec;
use chrono::NaiveDate;
use serde_json::{Value, json};

#[path = "support/guarded_alloc.rs"]
mod guarded_alloc;

use guarded_alloc::{Guarded, capped, refusing_large, serial};

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

/// The cap: 1,500 members' 8-byte owners (12,000 bytes) fit under it, and a
/// 16-byte column for them (24,000 bytes) does not.
const CAP: usize = 1 << 14;

/// The threshold of a numbered large allocation.
const LARGE: usize = 1 << 12;

/// Members whose 16-byte column the cap refuses and whose owners it admits.
const MEMBERS: usize = 1_500;

/// Rows whose 1-byte judgment column is a large allocation.
const MANY: usize = 5_000;

/// The length of the member rule's text: four times the cap, so no copy of
/// its bytes fits either.
const LONG: usize = 1 << 16;

fn text(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "text", "value": value } })
}

fn decimal(value: &str) -> Value {
    json!({ "kind": "literal", "value": { "kind": "decimal", "value": value } })
}

fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn add(items: Vec<Value>) -> Value {
    json!({ "kind": "add", "items": items })
}

fn count_where(predicate: Value) -> Value {
    json!({ "kind": "count_related", "relation": "member", "current_slot": 0, "related_slot": 1, "where": predicate })
}

fn sum_of(rule: &str) -> Value {
    json!({
        "kind": "sum_related", "relation": "member", "current_slot": 0, "related_slot": 1,
        "value": { "kind": "derived", "name": rule },
    })
}

fn rule(name: &str, entity: &str, dtype: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": dtype, "unit": null, "semantics": "scalar", "expr": expr })
}

fn judgment(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

fn and(items: Vec<Value>) -> Value {
    json!({ "kind": "and", "items": items })
}

fn count_chosen() -> Value {
    json!({ "kind": "count_related", "relation": "chosen", "current_slot": 0, "related_slot": 1 })
}

/// A household program whose `out` reads `shared` twice from one site:
/// `member` relates households to people, and `chosen`, with `predicate`,
/// filters it.
fn program(shared: Value, out: Value, predicate: Option<Value>) -> DenseCompiledProgram {
    let pair = json!(["Household", "Person"]);
    let mut relations = vec![json!({ "name": "member", "arity": 2, "slot_entities": pair })];
    if let Some(predicate) = predicate {
        relations.push(json!({
            "name": "chosen", "arity": 2, "slot_entities": pair,
            "derivation": {
                "source_relation": "member", "current_slot": 0, "related_slot": 1,
                "slot_entities": pair, "predicate": predicate,
            },
        }));
    }
    let program = json!({
        "relations": relations,
        "derived": [shared, rule("out", "Household", "decimal", out)],
    });
    let spec: ProgramSpec = serde_json::from_value(program).expect("program parses");
    let program = spec.to_program().expect("program converts");
    let compiled =
        DenseCompiledProgram::from_program(&program, Some("Household")).expect("dense compiles");
    // The shared rule's body is read by two nodes, so it is cached.
    assert!(
        format!("{compiled:?}").contains("references: 2"),
        "the rule's body is not shared"
    );
    compiled
}

/// `households` households of `members_each` members each, with no column.
fn stated_members(households: usize, members_each: usize) -> DenseBatchSpec {
    let related = households * members_each;
    DenseBatchSpec {
        row_count: households,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "member".to_string(),
                current_slot: 0,
                related_slot: 1,
            },
            DenseRelationBatchSpec {
                offsets: (0..=households).map(|row| row * members_each).collect(),
                related_row_count: Some(related),
                ..Default::default()
            },
        )]),
    }
}

fn month() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Decimal,
    Float,
}

fn execute(
    program: &DenseCompiledProgram,
    mode: Mode,
    batch: DenseBatchSpec,
) -> Result<DenseExecutionResult, EvalError> {
    let outputs = ["out".to_string()];
    match mode {
        Mode::Decimal => program.execute(&month(), batch, &outputs),
        Mode::Float => program.execute_f64(&month(), batch, &outputs),
    }
}

/// The count a refusal names, or `None` for any other result.
fn refused_count(error: &EvalError) -> Option<usize> {
    let EvalError::TypeMismatch(message) = error else {
        return None;
    };
    let head = message.strip_suffix(" rows, more than this process can allocate")?;
    let head = head.strip_suffix(" related").unwrap_or(head);
    head.rsplit(' ').next()?.parse().ok()
}

/// `out`'s value as text, or the error.
fn summary(result: &Result<DenseExecutionResult, EvalError>) -> Result<String, String> {
    match result {
        Ok(result) => match &result.outputs["out"] {
            DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => Ok(values[0].to_string()),
            DenseOutputValue::Scalar(DenseColumn::Float(values)) => Ok(values[0].to_string()),
            DenseOutputValue::Scalar(DenseColumn::Integer(values)) => Ok(values[0].to_string()),
            other => Err(format!("unexpected column {other:?}")),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// Under the cap, and with each large allocation refused in turn, `program`
/// returns its uncapped answer or a refusal naming one of `counts`, in both
/// numeric modes, and never aborts. Returns, per mode, whether the cap
/// refused it.
fn refuses_or_answers(
    case: &str,
    program: &DenseCompiledProgram,
    stated_members: impl Fn() -> DenseBatchSpec,
    counts: &[usize],
) -> Vec<bool> {
    [Mode::Decimal, Mode::Float]
        .into_iter()
        .map(|mode| {
            let label = format!("{case}, {mode:?}");
            let uncapped = execute(program, mode, stated_members());
            assert!(uncapped.is_ok(), "{label}: uncapped: {uncapped:?}");
            let accepted = |result: &Result<DenseExecutionResult, EvalError>, how: &str| {
                if let Err(error) = result
                    && refused_count(error).is_some()
                {
                    let count = refused_count(error).expect("a refusal");
                    assert!(counts.contains(&count), "{label}, {how}: {error}");
                } else {
                    assert_eq!(summary(result), summary(&uncapped), "{label}, {how}");
                }
            };
            // Each batch is built outside the armed closure, so the test's own
            // allocations are never refused.
            let batch = stated_members();
            let result = capped(CAP, || execute(program, mode, batch));
            accepted(&result, "capped");
            let refused = result.is_err();

            let batch = stated_members();
            let ((), large) = refusing_large(LARGE, 0, || {
                execute(program, mode, batch).expect("no allocation refused");
            });
            assert!(large > 0, "{label}: no large allocation to refuse");
            for number in 1..=large {
                let batch = stated_members();
                let (result, _) = refusing_large(LARGE, number, || execute(program, mode, batch));
                accepted(
                    &result,
                    &format!("large allocation {number} of {large} refused"),
                );
            }
            refused
        })
        .collect()
}

/// A text member rule, read by two `where` clauses: its body is cached on
/// the related rows, and each read copies the 1,500-row column of shared
/// texts. The text is never copied per row, so the column is 1,500 pointers,
/// which the cap refuses in both numeric modes.
#[test]
fn a_shared_text_body_over_many_related_rows_copies_its_cache_fallibly() {
    let _serial = serial();
    let long = "x".repeat(LONG);
    let program = program(
        rule("tag", "Person", "text", text(&long)),
        add(vec![
            count_where(compare(derived("tag"), "eq", text(&long))),
            count_where(compare(derived("tag"), "ne", text("a"))),
        ]),
        None,
    );
    let batch = || stated_members(1, MEMBERS);
    assert_eq!(
        refuses_or_answers("shared text body", &program, batch, &[MEMBERS]),
        vec![true, true]
    );
}

/// A decimal member rule summed twice: its body is cached on the related
/// rows. Its 16-byte decimal column is refused under the cap; an `f64`
/// column (12,000 bytes) fits and answers.
#[test]
fn a_shared_decimal_body_over_many_related_rows_copies_its_cache_fallibly() {
    let _serial = serial();
    let program = program(
        rule("share", "Person", "decimal", decimal("1.5")),
        add(vec![sum_of("share"), sum_of("share")]),
        None,
    );
    let batch = || stated_members(1, MEMBERS);
    assert_eq!(
        refuses_or_answers("shared decimal body", &program, batch, &[MEMBERS]),
        vec![true, false]
    );
}

/// A member judgment read by two `where` clauses: its body is cached on the
/// related rows as a column of 5,000 booleans, a large allocation that each
/// read copies.
#[test]
fn a_shared_judgment_body_over_many_related_rows_copies_its_cache_fallibly() {
    let _serial = serial();
    let program = program(
        judgment("adult", "Person", compare(decimal("1"), "gt", decimal("0"))),
        add(vec![
            count_where(derived("adult")),
            count_where(derived("adult")),
        ]),
        None,
    );
    let batch = || stated_members(1, MANY);
    refuses_or_answers("shared related judgment", &program, batch, &[MANY]);
}

/// A household scalar a derived relation's predicate reads twice: its body is
/// cached on the 5,000 root rows, and each read copies the column.
#[test]
fn a_shared_root_scalar_body_over_many_households_copies_its_cache_fallibly() {
    let _serial = serial();
    let program = program(
        rule("level", "Household", "decimal", decimal("2.5")),
        count_chosen(),
        Some(and(vec![
            compare(derived("level"), "gt", decimal("0")),
            compare(derived("level"), "lt", decimal("10")),
        ])),
    );
    let batch = || stated_members(MANY, 1);
    refuses_or_answers("shared root scalar", &program, batch, &[MANY]);
}

/// A household judgment a derived relation's predicate reads twice: its body
/// is cached on the 5,000 root rows as a large column of outcomes.
#[test]
fn a_shared_root_judgment_body_over_many_households_copies_its_cache_fallibly() {
    let _serial = serial();
    let program = program(
        judgment(
            "eligible",
            "Household",
            compare(decimal("1"), "gt", decimal("0")),
        ),
        count_chosen(),
        Some(and(vec![derived("eligible"), derived("eligible")])),
    );
    let batch = || stated_members(MANY, 1);
    refuses_or_answers("shared root judgment", &program, batch, &[MANY]);
}
