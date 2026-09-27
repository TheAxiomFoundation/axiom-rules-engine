//! Dense binds each relation key's batch once, however many schemas share it
//! (audit finding dense-bind-batch-per-link-copy, 2026-09-26).
//!
//! Every link of a derived-relation chain is keyed to the chain's base
//! relation, so `DenseCompiledProgram::relations` lists one schema per link,
//! all with one key. Binding used to copy that key's offsets, owners and every
//! column a link reads once per link: a chain of n links over M related rows
//! held n * M owners for the whole execution. It now binds each key once and
//! the links share it.
//!
//! Invariants tested here:
//! * **Memory.** Binding a batch allocates the same whatever the chain's
//!   length: peak allocation during an execution that binds n links is
//!   within a small constant of one link's.
//! * **Semantics.** For random households and random chains, each link
//!   reading its own input, dense answers exactly what explain answers.
//! * **Errors.** A malformed batch fails with the error a per-schema binding
//!   reports, in schema order: a missing key names the first schema's key,
//!   bad offsets name the key, and each schema's columns are length-checked
//!   in its own input order.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axiom_rules_engine::api::{ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::spec::{PeriodSpec, ProgramSpec, ScalarValueSpec};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use rust_decimal::Decimal;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Allocation accounting. Every test here takes `SERIAL`, so the counters see
// one test's allocations at a time.
// ---------------------------------------------------------------------------

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Bytes allocated at the peak of `run`, beyond what was live before it.
fn peak_allocation<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = CURRENT.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let value = run();
    (value, PEAK.load(Ordering::SeqCst) - before)
}

// ---------------------------------------------------------------------------
// Programs: a chain `link0 <- link1 <- ... <- member`, household to person.
// ---------------------------------------------------------------------------

const INPUTS: [&str; 3] = ["ssn", "age", "income"];

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn rule(name: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": "Household", "dtype": "integer", "unit": null, "semantics": "scalar", "expr": expr })
}

fn relation(name: &str, source: Option<&str>, predicate: Option<Value>) -> Value {
    let mut schema = json!({ "name": name, "arity": 2, "slot_entities": ["Household", "Person"] });
    if let (Some(source), Some(predicate)) = (source, predicate) {
        schema["derivation"] = json!({
            "source_relation": source,
            "current_slot": 0,
            "related_slot": 1,
            "slot_entities": ["Household", "Person"],
            "predicate": predicate,
        });
    }
    schema
}

/// A link's predicate: `input >= threshold`.
#[derive(Clone, Debug)]
struct Link {
    input: usize,
    threshold: i64,
}

/// `links[0]` is the chain's head; the last link sources `member`. The
/// outputs count the head's members and sum `income` over them, and `one`
/// reads no relation.
fn chain_program(links: &[Link]) -> Value {
    let mut relations = vec![relation("member", None, None)];
    for (index, link) in links.iter().enumerate() {
        let source = if index + 1 == links.len() {
            "member".to_string()
        } else {
            format!("link{}", index + 1)
        };
        relations.push(relation(
            &format!("link{index}"),
            Some(&source),
            Some(compare(
                input(INPUTS[link.input]),
                "gte",
                integer(link.threshold),
            )),
        ));
    }
    json!({
        "relations": relations,
        "derived": [
            rule("head_count", json!({ "kind": "count_related", "relation": "link0", "current_slot": 0, "related_slot": 1 })),
            rule("head_income", json!({ "kind": "sum_related", "relation": "link0", "current_slot": 0, "related_slot": 1, "value": input("income") })),
            rule("one", integer(1)),
        ],
    })
}

fn compile_dense(program: &Value) -> DenseCompiledProgram {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program JSON parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("artifact compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some("Household")).expect("dense compiles")
}

fn member_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member".to_string(),
        current_slot: 0,
        related_slot: 1,
    }
}

/// One household's members, each `[ssn, age, income]`.
type Households = Vec<Vec<[i64; 3]>>;

fn relation_batch(households: &Households) -> DenseRelationBatchSpec {
    let mut offsets = vec![0];
    let mut columns = vec![Vec::new(); INPUTS.len()];
    for members in households {
        offsets.push(offsets.last().copied().unwrap_or(0) + members.len());
        for member in members {
            for (column, value) in columns.iter_mut().zip(member) {
                column.push(*value);
            }
        }
    }
    DenseRelationBatchSpec {
        offsets,
        inputs: INPUTS
            .iter()
            .zip(columns)
            .map(|(name, values)| (name.to_string(), DenseColumn::Integer(values)))
            .collect(),
    }
}

fn execute(
    compiled: &DenseCompiledProgram,
    row_count: usize,
    relation: DenseRelationBatchSpec,
    outputs: &[&str],
) -> Result<Vec<Vec<i64>>, String> {
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let outputs = outputs
        .iter()
        .map(|output| output.to_string())
        .collect::<Vec<_>>();
    let result = compiled
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count,
                inputs: HashMap::new(),
                relations: HashMap::from([(member_key(), relation)]),
            },
            &outputs,
        )
        .map_err(|error| error.to_string())?;
    Ok((0..row_count)
        .map(|row| {
            outputs
                .iter()
                .map(|output| match &result.outputs[output] {
                    DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values[row],
                    // `sum` totals in the executor's numeric mode.
                    DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                        values[row].try_into().expect("integral output")
                    }
                    other => panic!("unexpected output {other:?}"),
                })
                .collect()
        })
        .collect())
}

/// Explain's answer. Explain refuses an input no rule reads, so the dataset
/// carries only the inputs `program` reads.
fn explain(
    program: &Value,
    households: &Households,
    outputs: &[&str],
) -> Result<Vec<Vec<i64>>, String> {
    let interval = json!({ "start": "2026-01-01", "end": "2026-01-31" });
    let text = program.to_string();
    let read = INPUTS.map(|name| text.contains(&format!(r#""name":"{name}""#)));
    let mut inputs = Vec::new();
    let mut tuples = Vec::new();
    for (household, members) in households.iter().enumerate() {
        for (member, values) in members.iter().enumerate() {
            let person = format!("h{household}p{member}");
            for ((name, value), _) in INPUTS
                .iter()
                .zip(values)
                .zip(read)
                .filter(|(_, read)| *read)
            {
                inputs.push(json!({
                    "name": name,
                    "entity": "Person",
                    "entity_id": person,
                    "interval": interval,
                    "value": { "kind": "integer", "value": value },
                }));
            }
            tuples.push(json!({ "name": "member", "tuple": [format!("h{household}"), person], "interval": interval }));
        }
    }
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": "explain",
        "program": program,
        "dataset": { "inputs": inputs, "relations": tuples },
        "queries": (0..households.len())
            .map(|household| json!({ "entity_id": format!("h{household}"), "period": period(), "outputs": outputs }))
            .collect::<Vec<_>>(),
    }))
    .expect("request JSON parses");
    let response = execute_request(request).map_err(|error| error.to_string())?;
    Ok(response
        .results
        .iter()
        .map(|result| {
            outputs
                .iter()
                .map(|output| match &result.outputs[*output] {
                    OutputValue::Scalar { value, .. } => match value {
                        ScalarValueSpec::Integer { value } => *value,
                        ScalarValueSpec::Decimal { value } => Decimal::from_str(value)
                            .expect("decimal output")
                            .try_into()
                            .expect("integral output"),
                        other => panic!("unexpected scalar {other:?}"),
                    },
                    other => panic!("unexpected output {other:?}"),
                })
                .collect()
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// A chain of `links` links over one household of `members` members binds
/// in the same memory as one link: the links share the key's rows and
/// columns. Before, each link held its own copy of the owners and of every
/// column it read, 64 times one link's bytes at 64 links.
#[test]
fn binding_a_chain_allocates_the_same_as_binding_one_link() {
    let _serial = serial();
    let members = 20_000;
    let households: Households = vec![vec![[1, 30, 5]; members]];
    let peak_for = |links: usize| {
        let program = chain_program(
            &(0..links)
                .map(|index| Link {
                    input: index % INPUTS.len(),
                    threshold: 0,
                })
                .collect::<Vec<_>>(),
        );
        let compiled = compile_dense(&program);
        assert_eq!(compiled.relations().len(), links, "one schema per link");
        assert!(
            compiled
                .relations()
                .iter()
                .all(|schema| schema.key == member_key()),
            "every link is keyed to the base relation"
        );
        let batch = relation_batch(&households);
        // `one` reads no relation, so the execution is the binding.
        let (result, peak) = peak_allocation(|| execute(&compiled, 1, batch, &["one"]));
        assert_eq!(result, Ok(vec![vec![1]]));
        peak
    };
    let one_link = peak_for(1);
    let many_links = peak_for(64);
    // Owners are `members` usizes; allow a few kilobytes for the per-link
    // views, far below a second copy of anything.
    assert!(
        many_links <= one_link + 16 * 1024,
        "binding 64 links peaked at {many_links} bytes, one link at {one_link}"
    );
}

// ---------------------------------------------------------------------------
// Semantics
// ---------------------------------------------------------------------------

fn households_strategy() -> impl Strategy<Value = Households> {
    prop::collection::vec(
        prop::collection::vec(
            (0_i64..=1, 0_i64..=90, 0_i64..=50).prop_map(|(ssn, age, income)| [ssn, age, income]),
            0..6,
        ),
        1..6,
    )
}

fn links_strategy() -> impl Strategy<Value = Vec<Link>> {
    prop::collection::vec(
        (0..INPUTS.len(), 0_i64..=40).prop_map(|(input, threshold)| Link { input, threshold }),
        1..12,
    )
}

/// For random chains whose links read different inputs, and random
/// households, dense answers exactly what explain answers.
#[test]
fn chains_of_links_reading_different_inputs_match_explain() {
    let _serial = serial();
    let mut runner = TestRunner::new(Config {
        cases: 48,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(
            &(links_strategy(), households_strategy()),
            |(links, households)| {
                let program = chain_program(&links);
                let outputs = ["head_count", "head_income"];
                let expected = explain(&program, &households, &outputs);
                let actual = execute(
                    &compile_dense(&program),
                    households.len(),
                    relation_batch(&households),
                    &outputs,
                );
                prop_assert_eq!(actual, expected, "links {:?}", links);
                Ok(())
            },
        )
        .expect("dense matches explain");
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `link0` reads `ssn` and `link1` reads `age`, both keyed to `member`.
fn two_links() -> DenseCompiledProgram {
    compile_dense(&chain_program(&[
        Link {
            input: 0,
            threshold: 1,
        },
        Link {
            input: 1,
            threshold: 18,
        },
    ]))
}

fn one_household() -> Households {
    vec![vec![[1, 30, 5], [0, 40, 7]]]
}

#[test]
fn a_missing_key_is_reported_once_for_the_first_schema() {
    let _serial = serial();
    let compiled = two_links();
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let error = compiled
        .execute(
            &period.to_model().expect("period converts"),
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: HashMap::new(),
            },
            &["head_count".to_string()],
        )
        .expect_err("no batch for `member`");
    assert_eq!(error.to_string(), "unknown relation: member::0/1/Household");
}

#[test]
fn malformed_offsets_name_the_shared_key() {
    let _serial = serial();
    let compiled = two_links();
    let cases = [
        (
            vec![0, 2, 2],
            "dense relation `member` offsets must have length 2",
        ),
        (
            vec![1, 2],
            "dense relation `member` offsets must start at 0",
        ),
    ];
    for (offsets, message) in cases {
        let mut batch = relation_batch(&one_household());
        batch.offsets = offsets;
        let error =
            execute(&compiled, 1, batch, &["head_count"]).expect_err("offsets are malformed");
        assert!(error.contains(message), "{error}");
    }
}

/// Each schema checks its own columns, in schema order: a short column read
/// only by the second schema is reported although the first schema bound the
/// key, and when a column the first schema reads is short too, that one is.
#[test]
fn every_schema_checks_its_own_columns_in_schema_order() {
    let _serial = serial();
    let compiled = two_links();
    let schemas = compiled.relations();
    assert_eq!(schemas.len(), 2);
    let first = &schemas[0].related_inputs;
    let second = &schemas[1].related_inputs;
    let only_second = second
        .iter()
        .find(|name| !first.contains(name))
        .expect("the second schema reads a column the first does not");
    let short = |names: &[&String]| {
        let mut batch = relation_batch(&one_household());
        for name in names {
            batch
                .inputs
                .insert(name.to_string(), DenseColumn::Integer(vec![1]));
        }
        execute(&compiled, 1, batch, &["head_count", "head_income"]).expect_err("a column is short")
    };
    let message = |name: &str| {
        format!(
            "dense relation input `{name}` for `member` has length 1 but related row count is 2"
        )
    };
    let error = short(&[only_second]);
    assert!(error.contains(&message(only_second)), "{error}");
    let error = short(&[only_second, &first[0]]);
    assert!(error.contains(&message(&first[0])), "{error}");
}
