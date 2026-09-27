//! Long chains of derived relations.
//!
//! A review on 2026-09-25 timed an inline request carrying a chain
//! `rel0 <- rel1 <- ... <- base`, each relation's `source_relation` the next,
//! with nothing querying it: 0.8 s at 500 links, 34.6 s at 2,000, over a
//! minute at 4,000, and a stack overflow at 50,000. Binding walked the chain
//! from every relation, each walk recursing once per link; validation and
//! every evaluator recursed once per link too, and a chain linked through
//! predicates (`relation_member`, `count_related`) re-resolved each link for
//! every candidate, which is exponential in the chain length.
//!
//! The invariant these tests hold: binding, validation and evaluation are
//! linear in the number of relations and never recurse once per link. Each
//! case runs a 20,000-link chain in a debug build on the 2 MiB stack a test
//! thread gets, where the recursive walks overflowed at a few thousand links.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionRequest, ExecutionResponse,
    OutputValue, execute_compiled_request, execute_request,
};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::model::Period;
use axiom_rules_engine::spec::{ProgramSpec, ScalarValueSpec};
use serde_json::{Value, json};

const LINKS: usize = 20_000;

/// Generous for a debug build, where each case takes a few seconds at most.
/// The code this replaced needed minutes at this size, when it did not
/// overflow the stack first.
const BOUND: Duration = Duration::from_secs(90);

fn relation_name(link: usize) -> String {
    format!("rel{link:07}")
}

fn literal(value: i64) -> Value {
    json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
}

fn always() -> Value {
    json!({"kind": "comparison", "left": literal(1), "op": "eq", "right": literal(1)})
}

fn count_related(relation: &str) -> Value {
    json!({"kind": "count_related", "relation": relation, "current_slot": 0, "related_slot": 1})
}

fn rule(name: &str, expr: Value) -> Value {
    json!({
        "name": name, "entity": "Household", "dtype": "integer", "unit": null,
        "semantics": "scalar", "expr": expr,
    })
}

/// How each link of the chain reaches the next.
#[derive(Clone, Copy)]
enum Link {
    /// `rel_i.source_relation = rel_{i+1}`, with an always-true predicate.
    Source,
    /// `rel_i.source_relation = rel_{i+1}`, and its predicate tests membership
    /// in `base`, which only the last link reads directly.
    SourceTestingBase,
    /// `rel_i` draws on `base`, and its predicate is `relation_member` of
    /// `rel_{i+1}` for the same pair.
    Member,
    /// `rel_i` draws on `base`, and its predicate counts `rel_{i+1}` at the
    /// candidate.
    Counted,
}

/// A chain of `LINKS` derived relations over the data relation `base`, typed
/// `[Household, Person]` throughout when `typed`.
fn chain_relations(link: Link, typed: bool) -> Vec<Value> {
    let slots = json!(["Household", "Person"]);
    let mut base = json!({"name": "base", "arity": 2});
    if typed {
        base["slot_entities"] = slots.clone();
    }
    let mut relations = vec![base];
    for index in 0..LINKS {
        let next = (index + 1 < LINKS).then(|| relation_name(index + 1));
        let (source, predicate) = match (link, next) {
            (Link::Source, Some(next)) => (next, always()),
            (Link::SourceTestingBase, Some(next)) => (
                next,
                json!({"kind": "relation_member", "relation": "base", "current_slot": 0, "related_slot": 1}),
            ),
            (Link::Member, Some(next)) => (
                "base".to_string(),
                json!({"kind": "relation_member", "relation": next, "current_slot": 0, "related_slot": 1}),
            ),
            (Link::Counted, Some(next)) => (
                "base".to_string(),
                json!({"kind": "comparison", "left": count_related(&next), "op": "gte", "right": literal(0)}),
            ),
            (_, None) => ("base".to_string(), always()),
        };
        let mut relation = json!({
            "name": relation_name(index),
            "arity": 2,
            "derivation": {
                "source_relation": source, "current_slot": 0, "related_slot": 1,
                "predicate": predicate,
            },
        });
        if typed {
            relation["slot_entities"] = slots.clone();
            relation["derivation"]["slot_entities"] = slots.clone();
        }
        relations.push(relation);
    }
    relations
}

fn program(relations: Vec<Value>, derived: Vec<Value>) -> ProgramSpec {
    serde_json::from_value(json!({"relations": relations, "derived": derived}))
        .expect("a valid program")
}

fn period() -> Value {
    json!({"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"})
}

fn dataset(tuples: &[(&str, &str)]) -> Value {
    let interval = json!({"start": "2026-01-01", "end": "2026-12-31"});
    json!({
        "inputs": [],
        "relations": tuples
            .iter()
            .map(|(left, right)| json!({"name": "base", "tuple": [left, right], "interval": interval}))
            .collect::<Vec<_>>(),
    })
}

fn request(
    program: &ProgramSpec,
    mode: &str,
    tuples: &[(&str, &str)],
    entity: &str,
    outputs: &[&str],
) -> ExecutionRequest {
    serde_json::from_value(json!({
        "mode": mode,
        "program": program,
        "dataset": dataset(tuples),
        "queries": [{"entity_id": entity, "period": period(), "outputs": outputs}],
    }))
    .expect("a valid request")
}

fn timed<T>(label: &str, run: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = run();
    let elapsed = started.elapsed();
    assert!(
        elapsed < BOUND,
        "{label} took {elapsed:?} for {LINKS} links"
    );
    result
}

fn integer_output(response: &ExecutionResponse, output: &str) -> i64 {
    let value = response.results[0]
        .outputs
        .get(output)
        .unwrap_or_else(|| panic!("output `{output}` is present"));
    match value {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => *value,
        other => panic!("`{output}` is an integer, found {other:?}"),
    }
}

/// Runs `request` in explain and fast mode, each within the bound, and
/// returns both responses.
fn explain_and_fast(
    program: &ProgramSpec,
    tuples: &[(&str, &str)],
    entity: &str,
    outputs: &[&str],
    label: &str,
) -> [ExecutionResponse; 2] {
    ["explain", "fast"].map(|mode| {
        timed(&format!("{label} ({mode})"), || {
            execute_request(request(program, mode, tuples, entity, outputs))
                .unwrap_or_else(|error| panic!("{label} ({mode}) failed: {error}"))
        })
    })
}

#[test]
fn an_unqueried_chain_binds_and_validates_in_linear_time() {
    // The reviewer's shape: nothing reads the chain, yet binding walked it
    // from every relation.
    let program = program(
        chain_relations(Link::Source, false),
        vec![rule("k", literal(3))],
    );
    for response in explain_and_fast(&program, &[], "h1", &["k"], "unqueried chain") {
        assert_eq!(integer_output(&response, "k"), 3);
    }
}

#[test]
fn a_queried_typed_chain_evaluates_in_linear_time() {
    // Typed slots make binding orient every link; querying the head makes
    // every evaluator resolve the whole chain.
    let program = program(
        chain_relations(Link::Source, true),
        vec![rule("head", count_related(&relation_name(0)))],
    );
    let tuples = [("h1", "p1"), ("h1", "p2")];
    for response in explain_and_fast(&program, &tuples, "h1", &["head"], "queried chain") {
        assert_eq!(integer_output(&response, "head"), 2);
    }
}

#[test]
fn counting_every_link_of_a_chain_resolves_each_link_once() {
    // One aggregation per link: resolving each from scratch walks the chain
    // below it again, which is quadratic in the chain length.
    let program = program(
        chain_relations(Link::Source, true),
        vec![rule(
            "all",
            json!({"kind": "add", "items": (0..LINKS).map(|link| count_related(&relation_name(link))).collect::<Vec<_>>()}),
        )],
    );
    let expected = i64::try_from(LINKS).expect("fits") * 2;
    for response in explain_and_fast(
        &program,
        &[("h1", "p1"), ("h1", "p2")],
        "h1",
        &["all"],
        "every link counted",
    ) {
        let OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } = &response.results[0].outputs["all"]
        else {
            panic!("`all` is a decimal sum");
        };
        assert_eq!(value.to_string(), expected.to_string());
    }
}

#[test]
fn a_chain_through_relation_member_predicates_is_linear_not_exponential() {
    // Two candidates per link: re-resolving the next link for each candidate
    // doubled the work per link.
    let program = program(
        chain_relations(Link::Member, true),
        vec![rule("head", count_related(&relation_name(0)))],
    );
    let tuples = [("h1", "p1"), ("h1", "p2")];
    for response in explain_and_fast(&program, &tuples, "h1", &["head"], "member chain") {
        assert_eq!(integer_output(&response, "head"), 2);
    }
}

#[test]
fn a_chain_through_count_related_predicates_is_linear_not_exponential() {
    // Each link counts the next at its candidate, a different entity, so the
    // resolutions nest across entities as well as links.
    let program = program(
        chain_relations(Link::Counted, false),
        vec![rule("head", count_related(&relation_name(0)))],
    );
    let tuples = [("n1", "n1"), ("n1", "n2"), ("n2", "n1"), ("n2", "n2")];
    for response in explain_and_fast(&program, &tuples, "n1", &["head"], "counted chain") {
        assert_eq!(integer_output(&response, "head"), 2);
    }
}

#[test]
fn a_chain_closed_into_a_cycle_is_refused_without_recursing() {
    let mut relations = chain_relations(Link::Source, false);
    relations.last_mut().expect("the chain has links")["derivation"]["source_relation"] =
        json!(relation_name(0));
    let program = program(relations, vec![rule("k", literal(3))]);
    let error = timed("cyclic chain", || {
        execute_request(request(&program, "explain", &[], "h1", &["k"]))
            .expect_err("a cycle of source relations is refused")
    });
    let ApiError::InvalidProgram(error) = error else {
        panic!("expected an invalid program, found {error}");
    };
    let CompileError::CyclicRelationDependency { cycle } = *error else {
        panic!("expected a relation cycle, found {error}");
    };
    assert_eq!(cycle.split(", ").count(), LINKS);
}

#[test]
fn a_compiled_chain_loads_and_runs_in_linear_time() {
    // Loading an artifact runs the orientation diagnostic and the cycle
    // search over the whole chain.
    let program = program(
        chain_relations(Link::Source, true),
        vec![rule("head", count_related(&relation_name(0)))],
    );
    let artifact = timed("compile", || {
        CompiledProgramArtifact::compile(program).expect("the chain compiles")
    });
    let request: CompiledExecutionRequest = serde_json::from_value(json!({
        "mode": "explain",
        "dataset": dataset(&[("h1", "p1"), ("h1", "p2")]),
        "queries": [{"entity_id": "h1", "period": period(), "outputs": ["head"]}],
    }))
    .expect("a valid compiled request");
    assert_eq!(request.mode, ExecutionMode::Explain);
    let response = timed("compiled run", || {
        execute_compiled_request(artifact, request).expect("the compiled chain runs")
    });
    assert_eq!(integer_output(&response, "head"), 2);
}

#[test]
fn a_chain_testing_its_base_relation_at_every_link_is_linear() {
    let program = program(
        chain_relations(Link::SourceTestingBase, true),
        vec![rule("head", count_related(&relation_name(0)))],
    );
    let tuples = [("h1", "p1"), ("h1", "p2")];
    for response in explain_and_fast(&program, &tuples, "h1", &["head"], "base-testing chain") {
        assert_eq!(integer_output(&response, "head"), 2);
    }
}

#[test]
fn a_dense_chain_compiles_and_runs_in_linear_time() {
    // Every link's predicate tests membership in `base`, which dense accepts
    // after checking that the link's chain of sources reads `base` so.
    let program = program(
        chain_relations(Link::SourceTestingBase, true),
        vec![
            rule("head", count_related(&relation_name(0))),
            rule("tail", count_related(&relation_name(LINKS - 1))),
        ],
    )
    .to_program()
    .expect("the chain lowers");
    let dense = timed("dense compile", || {
        DenseCompiledProgram::from_program(&program, Some("Household"))
            .expect("dense compiles the chain")
    });
    // Two households: the first has two members, the second one.
    let batch = DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::new(),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "base".to_string(),
                current_slot: 0,
                related_slot: 1,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 2, 3],
                inputs: HashMap::new(),
            },
        )]),
    };
    let result = timed("dense run", || {
        dense
            .execute(
                &Period::month(2026, 1),
                batch,
                &["head".to_string(), "tail".to_string()],
            )
            .expect("dense runs the chain")
    });
    for output in ["head", "tail"] {
        let Some(DenseOutputValue::Scalar(DenseColumn::Integer(counts))) =
            result.outputs.get(output)
        else {
            panic!("`{output}` is an integer column");
        };
        assert_eq!(counts, &vec![2, 1], "{output}");
    }
}
