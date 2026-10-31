//! Independent path reachability and nested-exists differential properties.
use std::cell::Cell;
use std::collections::BTreeSet;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};

const IDS: usize = 5;
fn id(index: usize) -> String {
    format!("p{index}")
}
fn integer(value: i64) -> Value {
    json!({"kind":"literal","value":{"kind":"integer","value":value}})
}
fn yes() -> Value {
    json!({"kind":"comparison","left":integer(0),"op":"eq","right":integer(0)})
}
fn positive(term: Value, name: &str) -> Value {
    json!({"kind":"comparison","left":{"kind":"role_derived","term":term,"name":name},"op":"gt","right":integer(0)})
}
fn variable(index: usize) -> Value {
    json!({"var":index})
}
fn atom(name: &str, from: Value, to: Value, reverse: bool) -> Value {
    let terms = if reverse {
        vec![to, from]
    } else {
        vec![from, to]
    };
    json!({"kind":"relation_atom","relation":name,"terms":terms})
}
fn tuple(name: &str, from: usize, to: usize, reverse: bool, full: bool) -> Value {
    let ids = if reverse {
        vec![id(to), id(from)]
    } else {
        vec![id(from), id(to)]
    };
    json!({"name":name,"tuple":ids,"interval":{"start":"2026-01-01","end":if full {"2026-12-31"} else {"2026-01-15"}}})
}
fn input(index: usize, name: &str, value: i64) -> Value {
    json!({"entity":"Person","entity_id":id(index),"name":name,"interval":{"start":"2026-01-01","end":"2026-12-31"},
        "value":{"kind":"integer","value":value}})
}
#[derive(Clone, Debug)]
struct Case {
    length: usize,
    derived: u8,
    reverse: u8,
    rows: Vec<(usize, usize, usize, bool)>,
    allows: Vec<u8>,
    keep: u8,
    missing: Option<usize>,
    shift: usize,
}
fn cases() -> impl Strategy<Value = Case> {
    (
        1usize..5,
        any::<u8>(),
        any::<u8>(),
        prop::collection::vec((0usize..4, 0usize..IDS, 0usize..IDS, any::<bool>()), 0..45),
        prop::collection::vec(any::<u8>(), 4),
        any::<u8>(),
        prop::option::of(0usize..IDS),
        0usize..64,
        prop_oneof![3 => Just(None),1 => Just(Some(false)),2 => Just(Some(true))],
    )
        .prop_map(
            |(length, derived, reverse, mut rows, mut allows, keep, missing, shift, shape)| {
                // Retain unconstrained graphs, and weight guaranteed empty/non-empty
                // cases so deeper joins execute often without replacing broad cases.
                match shape {
                    Some(false) => rows.clear(),
                    Some(true) => {
                        for step in 0..length {
                            rows.push((step, step % IDS, (step + 1) % IDS, true));
                            allows[step] |= 1 << ((step + 1) % IDS);
                        }
                    }
                    None => {}
                }
                Case {
                    length,
                    derived,
                    reverse,
                    rows,
                    allows,
                    keep,
                    missing,
                    shift,
                }
            },
        )
}
fn is_derived(case: &Case, step: usize) -> bool {
    case.derived & (1 << step) != 0
}
fn reversed(case: &Case, step: usize) -> bool {
    case.reverse & (1 << step) != 0
}
fn relation_name(case: &Case, step: usize) -> String {
    format!(
        "{}_{step}",
        if is_derived(case, step) {
            "filtered"
        } else {
            "edge"
        }
    )
}
fn path(case: &Case) -> Vec<Value> {
    (0..case.length)
        .map(|step| {
            json!({"relation":relation_name(case,step),
        "from_slot":usize::from(reversed(case,step)),"to_slot":usize::from(!reversed(case,step))})
        })
        .collect()
}
fn derived(name: &str, source: &str, current: usize, predicate: Value) -> Value {
    json!({"name":name,"arity":2,"slot_entities":["Person","Person"],"derivation":{
        "source_relation":source,"current_slot":current,"related_slot":1-current,
        "slot_entities":["Person","Person"],"predicate":predicate}})
}
/// `from` is anchor at step zero, otherwise the innermost existential.
/// Each recursion adds a binding, so previous variables move outward by one.
fn nested_exists(case: &Case, step: usize) -> Value {
    let from = if step == 0 {
        json!("anchor")
    } else {
        variable(0)
    };
    let name = relation_name(case, step);
    if step + 1 == case.length {
        atom(&name, from, json!("candidate"), reversed(case, step))
    } else {
        let terms = if reversed(case, step) {
            vec![variable(0), from]
        } else {
            vec![from, variable(0)]
        };
        json!({"kind":"exists","relation":name,"terms":terms,
            "variable_slot":usize::from(!reversed(case,step)),"body":nested_exists(case,step+1)})
    }
}
fn program(case: &Case, use_exists: bool, predicate: bool) -> Value {
    let mut relations = vec![
        json!({"name":"all_ids","arity":2,"slot_entities":["Person","Person"]}),
        json!({"name":"gate","arity":2,"slot_entities":["Person","Person"]}),
    ];
    for step in 0..case.length {
        let name = format!("edge_{step}");
        relations.push(json!({"name":name,"arity":2,"slot_entities":["Person","Person"]}));
        if is_derived(case, step) {
            let guard = json!({"kind":"exists","relation":"gate","terms":["candidate",variable(0)],"variable_slot":1,
                "body":positive(variable(0),&format!("allow_rule_{step}"))});
            relations.push(derived(
                &format!("filtered_{step}"),
                &name,
                usize::from(reversed(case, step)),
                guard,
            ));
        }
    }
    let filter = if predicate {
        json!({"kind":"comparison","left":{"kind":"role_derived","term":"candidate","name":"keep_rule"},
            "op":"gt","right":integer(0)})
    } else {
        yes()
    };
    let selected = if use_exists {
        derived(
            "selected",
            "all_ids",
            0,
            json!({"kind":"and","items":[nested_exists(case,0),filter]}),
        )
    } else {
        json!({"name":"selected","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_path":path(case),"current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":filter}})
    };
    relations.push(selected);
    let mut derived_rules = json!([
        {"name":"count","entity":"Person","dtype":"integer","semantics":"scalar",
            "expr":{"kind":"count_related","relation":"selected","current_slot":0,"related_slot":1}},
        {"name":"mask","entity":"Person","dtype":"integer","semantics":"scalar",
            "expr":{"kind":"sum_related","relation":"selected","current_slot":0,"related_slot":1,"value":{"kind":"derived","name":"token"}}},
        {"name":"token","entity":"Person","dtype":"integer","semantics":"scalar","expr":{"kind":"input","name":"token"}}
    ]);
    if predicate {
        derived_rules.as_array_mut().unwrap().push(json!({
            "name":"keep_rule","entity":"Person","dtype":"integer","semantics":"scalar",
            "expr":{"kind":"input","name":"keep"}
        }));
    }
    for step in 0..case.length {
        if is_derived(case, step) {
            derived_rules.as_array_mut().unwrap().push(json!({
                "name":format!("allow_rule_{step}"),"entity":"Person","dtype":"integer","semantics":"scalar",
                "expr":{"kind":"input","name":format!("allow_{step}")}
            }));
        }
    }
    json!({"relations":relations,"derived":derived_rules})
}
fn dataset(case: &Case, predicate: bool) -> Value {
    let mut records = vec![];
    let mut inputs = vec![];
    for index in 0..IDS {
        records.push(tuple("all_ids", 0, index, false, true));
        records.push(tuple("gate", index, index, false, true));
        inputs.push(input(index, "token", 1 << index));
        if predicate && case.missing != Some(index) {
            inputs.push(input(
                index,
                "keep",
                i64::from(case.keep & (1 << index) != 0),
            ));
        }
        for step in 0..case.length {
            if is_derived(case, step) {
                inputs.push(input(
                    index,
                    &format!("allow_{step}"),
                    i64::from(case.allows[step] & (1 << index) != 0),
                ));
            }
        }
    }
    for &(step, from, to, full) in &case.rows {
        let step = step % case.length;
        let row = tuple(
            &format!("edge_{step}"),
            from,
            to,
            reversed(case, step),
            full,
        );
        records.push(row.clone());
        records.push(row); // duplicates must not multiply candidates or values.
    }
    json!({"inputs":inputs,"relations":records})
}
/// The oracle scans supplied records directly. It shares no engine relation
/// lookup, candidate generation, or exists implementation.
fn frontiers(case: &Case, data: &Value) -> Vec<BTreeSet<usize>> {
    let mut current = BTreeSet::from([0]);
    let mut frontiers = vec![current.clone()];
    for step in 0..case.length {
        let mut next = BTreeSet::new();
        for record in data["relations"].as_array().unwrap() {
            if record["name"] != format!("edge_{step}") || record["interval"]["end"] != "2026-12-31"
            {
                continue;
            }
            let from = record["tuple"][usize::from(reversed(case, step))]
                .as_str()
                .unwrap()[1..]
                .parse::<usize>()
                .unwrap();
            let to = record["tuple"][usize::from(!reversed(case, step))]
                .as_str()
                .unwrap()[1..]
                .parse::<usize>()
                .unwrap();
            if current.contains(&from)
                && (!is_derived(case, step) || case.allows[step] & (1 << to) != 0)
            {
                next.insert(to);
            }
        }
        current = next;
        frontiers.push(current.clone());
    }
    frontiers
}
fn oracle(case: &Case, data: &Value, predicate: bool) -> Result<BTreeSet<usize>, usize> {
    let current = frontiers(case, data).pop().unwrap();
    if predicate {
        let mut selected = BTreeSet::new();
        for candidate in current {
            if case.missing == Some(candidate) {
                return Err(candidate);
            }
            if case.keep & (1 << candidate) != 0 {
                selected.insert(candidate);
            }
        }
        Ok(selected)
    } else {
        Ok(current)
    }
}
/// Runs at depth 1, which defers every rule read, and at `usize::MAX`, which
/// defers none. A failure reaches a path by a different route at each depth,
/// so every caller also checks invariant 7: outputs, traces and errors match.
fn run(mode: &str, p: &Value, data: &Value) -> Result<Value, String> {
    let request: ExecutionRequest=serde_json::from_value(json!({"mode":mode,"program":p,"dataset":data,
        "queries":[{"entity_id":"p0","period":{"period_kind":"month","start":"2026-01-01","end":"2026-01-31"},"outputs":["count","mask"]}]})).unwrap();
    let at = |depth| {
        axiom_rules_engine::depth::with_suspend_depth(depth, || execute_request(request.clone()))
            .map(|response| serde_json::to_value(response).unwrap())
            .map_err(|error| error.to_string())
    };
    let deferred = at(1);
    assert_eq!(
        deferred,
        at(usize::MAX),
        "{mode}: deferral changed the result"
    );
    deferred
}
fn number(value: &Value) -> i64 {
    value["value"]
        .as_i64()
        .unwrap_or_else(|| value["value"].as_str().unwrap().parse().unwrap())
}
fn members(response: &Value) -> BTreeSet<usize> {
    let outputs = &response["results"][0]["outputs"];
    let mask = number(&outputs["mask"]["value"]);
    let members = (0..IDS)
        .filter(|index| mask & (1 << index) != 0)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        number(&outputs["count"]["value"]),
        members.len() as i64,
        "candidate multiplicity differs from set membership"
    );
    members
}
fn assert_oracle(result: &Result<Value, String>, expected: &Result<BTreeSet<usize>, usize>) {
    match (result, expected) {
        (Ok(response), Ok(expected)) => assert_eq!(&members(response), expected),
        (Err(error), Err(id)) => assert!(
            error.contains("keep") && error.contains(&self::id(*id)),
            "{error}"
        ),
        (actual, expected) => {
            panic!("path and independent oracle disagree: {actual:?}, expected {expected:?}")
        }
    }
}
fn runner(seed: u8, cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}
/// The same properties are compiled inside the engine, where this callback
/// drains counters recorded at completed path lookups and generation. The
/// integration wrapper supplies `None`, so public API tests need no hook.
#[derive(Default)]
struct ActualCoverage {
    enabled: Cell<bool>,
    // resumed, empty, nonempty, long, derived at positions 0..3.
    counts: Cell<[usize; 8]>,
    failures: Cell<usize>,
}
impl ActualCoverage {
    fn reset(&self) {
        super::actual_path_work::take();
    }
    /// Call after the result has passed its independent oracle.
    fn observe(&self, failed: bool) -> Option<[usize; 8]> {
        let (resumed, empty, nonempty, long, positions) = super::actual_path_work::take()?;
        self.enabled.set(true);
        let work = [
            resumed,
            empty,
            nonempty,
            long,
            positions[0],
            positions[1],
            positions[2],
            positions[3],
        ];
        let mut counts = self.counts.get();
        for (total, value) in counts.iter_mut().zip(work) {
            *total += value;
        }
        self.counts.set(counts);
        self.failures.set(self.failures.get() + usize::from(failed));
        Some(work)
    }
    fn floors(&self, name: &str, required: [usize; 8], failures: usize) {
        if !self.enabled.get() {
            return;
        }
        for ((label, count), floor) in [
            "resumed generation",
            "empty",
            "nonempty",
            "3+ steps",
            "derived at 0",
            "derived at 1",
            "derived at 2",
            "derived at 3",
        ]
        .into_iter()
        .zip(self.counts.get())
        .zip(required)
        {
            eprintln!("{name} actual {label}={count}, floor={floor}");
            assert!(
                count >= floor,
                "{name} actual {label}: {count}, floor {floor}"
            );
        }
        eprintln!(
            "{name} checked failures={}, floor={failures}",
            self.failures.get()
        );
        assert!(self.failures.get() >= failures);
    }
}

#[test]
fn paths_agree_with_nested_exists_and_raw_records_in_explain_and_fast_including_failures() {
    let coverage = ActualCoverage::default();
    runner(41, 128)
        .run(&cases(), |case| {
            let data = dataset(&case, true);
            let expected = oracle(&case, &data, true);
            let p = program(&case, false, true);
            let expanded = program(&case, true, true);
            let mut outputs = vec![];
            for mode in ["explain", "fast"] {
                coverage.reset();
                let result = run(mode, &p, &data);
                assert_oracle(&result, &expected);
                coverage.observe(result.is_err());
                let exists = run(mode, &expanded, &data);
                assert_oracle(&exists, &expected);
                match (&result, &exists) {
                    (Ok(a), Ok(b)) => {
                        prop_assert_eq!(&a["results"][0]["outputs"], &b["results"][0]["outputs"])
                    }
                    (Err(a), Err(b)) => prop_assert_eq!(a, b),
                    _ => prop_assert!(false, "path and exists differ"),
                }
                // Fast intentionally omits trace records; compare returned values
                // across modes, and compare traces within each mode below.
                outputs.push(result.map(|response| response["results"][0]["outputs"].clone()));
            }
            prop_assert_eq!(&outputs[0], &outputs[1]);
            Ok(())
        })
        .unwrap();
    coverage.floors("paths/exists", [8, 12, 8, 30, 25, 8, 5, 2], 5);
}

#[test]
fn a_forward_one_step_path_equals_its_single_source_with_values_order_and_failures() {
    let coverage = ActualCoverage::default();
    let step_kinds = Cell::new([0_usize; 2]);
    runner(42, 96)
        .run(&cases(), |mut case| {
            case.length = 1;
            case.reverse = 0;
            let data = dataset(&case, true);
            let expected = oracle(&case, &data, true);
            let p = program(&case, false, true);
            let mut single = p.clone();
            let selected = single["relations"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            selected["derivation"]
                .as_object_mut()
                .unwrap()
                .remove("source_path");
            selected["derivation"]["source_relation"] = json!(relation_name(&case, 0));
            for mode in ["explain", "fast"] {
                coverage.reset();
                let result = run(mode, &p, &data);
                assert_oracle(&result, &expected);
                if let Some(work) = coverage.observe(result.is_err()) {
                    let mut kinds = step_kinds.get();
                    // A one-step path always looks up its anchor. The
                    // engine's derived-step counter identifies its source.
                    kinds[usize::from(work[4] != 0)] += work[1] + work[2];
                    step_kinds.set(kinds);
                }
                prop_assert_eq!(&result, &run(mode, &single, &data));
            }
            Ok(())
        })
        .unwrap();
    // A one-step source cannot exercise later positions or a long path.
    coverage.floors("one step", [8, 15, 15, 0, 30, 0, 0, 0], 0);
    if coverage.enabled.get() {
        for (name, count) in ["data", "derived"].into_iter().zip(step_kinds.get()) {
            eprintln!("one step actual {name}={count}, floor=30");
            assert!(count >= 30, "one step actual {name}: {count}");
        }
    }
}

#[test]
fn record_roster_and_input_order_and_repeated_runs_preserve_outputs_and_traces() {
    let coverage = ActualCoverage::default();
    runner(43, 128)
        .run(&cases(), |mut case| {
            case.missing = None;
            let p = program(&case, false, true);
            let data = dataset(&case, true);
            let expected = oracle(&case, &data, true);
            let mut permuted = data.clone();
            for key in ["relations", "inputs"] {
                let records = permuted[key].as_array_mut().unwrap();
                records.reverse();
                if !records.is_empty() {
                    let length = records.len();
                    records.rotate_left(case.shift % length);
                }
            }
            for mode in ["explain", "fast"] {
                coverage.reset();
                let first = run(mode, &p, &data);
                assert_oracle(&first, &expected);
                coverage.observe(false);
                coverage.reset();
                let repeated = run(mode, &p, &data);
                prop_assert_eq!(&first, &repeated);
                prop_assert_eq!(
                    serde_json::to_vec(&first).unwrap(),
                    serde_json::to_vec(&repeated).unwrap()
                );
                coverage.observe(false);
                coverage.reset();
                let reordered = run(mode, &p, &permuted);
                prop_assert_eq!(&first, &reordered);
                prop_assert_eq!(
                    serde_json::to_vec(&first).unwrap(),
                    serde_json::to_vec(&reordered).unwrap()
                );
                coverage.observe(false);
            }
            Ok(())
        })
        .unwrap();
    coverage.floors("order/determinism", [8, 12, 8, 30, 25, 8, 5, 2], 0);
}

#[test]
fn adding_base_tuples_is_monotone_with_exact_finite_candidate_sets() {
    // §7.6 exclusions: no derived reads, aggregating rule predicates, count
    // comparisons or bare membership tests. This predicate is true; this
    // property concerns candidates reached through data steps only.
    let coverage = ActualCoverage::default();
    let growth = Cell::new(0_usize);
    runner(44, 96)
        .run(&cases(), |mut case| {
            case.derived = 0;
            case.missing = None;
            let p = program(&case, false, false);
            let old = dataset(&case, false);
            let mut enlarged = case.clone();
            for step in 0..case.length {
                enlarged
                    .rows
                    .push((step, step % IDS, (step + 1) % IDS, true));
            }
            let new = dataset(&enlarged, false);
            let before = oracle(&case, &old, false).unwrap();
            let after = oracle(&enlarged, &new, false).unwrap();
            prop_assert!(before.is_subset(&after));
            prop_assert!(after.len() <= IDS);
            for mode in ["explain", "fast"] {
                coverage.reset();
                let a = run(mode, &p, &old).unwrap();
                prop_assert_eq!(members(&a), before.clone());
                coverage.observe(false);
                coverage.reset();
                let b = run(mode, &p, &new).unwrap();
                prop_assert_eq!(members(&b), after.clone());
                coverage.observe(false);
            }
            growth.set(growth.get() + usize::from(before.len() < after.len()));
            Ok(())
        })
        .unwrap();
    // The monotonicity scope deliberately excludes derived steps, so no
    // derived lookup or candidate-generation resumption is required here.
    coverage.floors("monotone", [0, 15, 15, 25, 0, 0, 0, 0], 0);
    eprintln!("monotone exact strict growth={}, floor=15", growth.get());
    assert!(growth.get() >= 15);
}

#[test]
fn every_generated_path_has_finite_exact_candidates_and_refuses_a_cycle_through_each_step() {
    let coverage = ActualCoverage::default();
    let refused_positions = Cell::new([0usize; 4]);
    runner(45, 96)
        .run(&cases(), |mut case| {
            // The positive control is evaluated before closing the graph. A
            // successful compiler result alone would not show that the path
            // can run or that only final frontier ids become candidates.
            case.reverse = 0;
            case.missing = None;
            let p = program(&case, false, false);
            let data = dataset(&case, false);
            let expected = oracle(&case, &data, false).unwrap();
            prop_assert!(expected.len() <= IDS);
            for mode in ["explain", "fast"] {
                coverage.reset();
                let result = run(mode, &p, &data);
                assert_oracle(&result, &Ok(expected.clone()));
                coverage.observe(false);
            }
            for cycle_step in 0..case.length {
                let mut cyclic = p.clone();
                let name = format!("edge_{cycle_step}");
                let step_relation = cyclic["relations"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|relation| relation["name"] == name)
                    .unwrap();
                *step_relation = derived(&name, "selected", 0, yes());
                let spec = serde_json::from_value(cyclic).unwrap();
                let error = CompiledProgramArtifact::compile(spec)
                    .err()
                    .expect("a dependency cycle through a path step must be refused");
                prop_assert!(
                    matches!(error, CompileError::CyclicRelationDependency { .. }),
                    "{error}"
                );
                let mut positions = refused_positions.get();
                positions[cycle_step] += 1;
                refused_positions.set(positions);
            }
            Ok(())
        })
        .unwrap();
    coverage.floors("termination/finite", [8, 15, 15, 25, 25, 8, 5, 2], 0);
    for (position, count) in refused_positions.get().into_iter().enumerate() {
        let floor = [80, 50, 30, 10][position];
        eprintln!("termination refused cycle at step {position}={count}, floor={floor}");
        assert!(
            count >= floor,
            "cycle at step {position}: {count}, floor {floor}"
        );
    }
}

#[test]
fn dense_refuses_paths_by_name_and_issue() {
    let case = Case {
        length: 1,
        derived: 0,
        reverse: 0,
        rows: vec![(0, 0, 1, true)],
        allows: vec![31; 4],
        keep: 31,
        missing: None,
        shift: 0,
    };
    let spec = serde_json::from_value(program(&case, false, false)).unwrap();
    let artifact = CompiledProgramArtifact::compile(spec).unwrap();
    let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .err()
        .unwrap();
    assert!(
        matches!(error, DenseCompileError::Unsupported(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("path source") && error.to_string().contains("#218"),
        "{error}"
    );
}

#[test]
fn dense_names_a_path_behind_a_membership_test_and_a_chain() {
    let edge = json!({"name":"edge","arity":2,"slot_entities":["Person","Person"]});
    let step = json!({"relation":"edge","from_slot":0,"to_slot":1});
    let path = json!({"name":"path","arity":2,"slot_entities":["Person","Person"],"derivation":{
        "source_path":[step],"current_slot":0,"related_slot":1,
        "slot_entities":["Person","Person"],"predicate":yes()}});
    let member = |relation: &str| json!({"kind":"relation_member","relation":relation,"current_slot":0,"related_slot":1});
    let filter = |relation: &str| derived("filter", "edge", 0, member(relation));
    let total = json!({"name":"total","entity":"Person","dtype":"integer","semantics":"scalar",
        "expr":{"kind":"count_related","relation":"filter","current_slot":0,"related_slot":1}});
    for relations in [
        vec![edge.clone(), path.clone(), filter("path")],
        vec![
            edge.clone(),
            path.clone(),
            derived("chain", "path", 0, yes()),
            filter("chain"),
        ],
    ] {
        let spec = serde_json::from_value(json!({"relations":relations,"derived":[total.clone()]}))
            .unwrap();
        let artifact = CompiledProgramArtifact::compile(spec).unwrap();
        let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("path source of derived relation `path`") && error.contains("#218"),
            "{error}"
        );
    }
}

#[test]
fn household_paths_cover_alone_multiple_and_absent_households() {
    let p = json!({"relations":[
        {"name":"home","arity":2,"slot_entities":["Person","Household"]},
        {"name":"selected","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_path":[{"relation":"home","from_slot":0,"to_slot":1},{"relation":"home","from_slot":1,"to_slot":0}],
            "current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":yes()}}
    ],"derived":[{"name":"count","entity":"Person","dtype":"integer","semantics":"scalar",
        "expr":{"kind":"count_related","relation":"selected","current_slot":0,"related_slot":1}},
        {"name":"mask","entity":"Person","dtype":"integer","semantics":"scalar",
        "expr":{"kind":"sum_related","relation":"selected","current_slot":0,"related_slot":1,"value":{"kind":"derived","name":"token"}}},
        {"name":"token","entity":"Person","dtype":"integer","semantics":"scalar","expr":{"kind":"input","name":"token"}}]});
    let home = |person: &str, household: &str| json!({"name":"home","tuple":[person,household],"interval":{"start":"2026-01-01","end":"2026-12-31"}});
    for records in [
        vec![],
        vec![home("p0", "h0")],
        vec![
            home("p0", "h0"),
            home("p1", "h0"),
            home("p0", "h1"),
            home("p2", "h1"),
        ],
    ] {
        let expected = match records.len() {
            0 => BTreeSet::new(),
            1 => BTreeSet::from([0]),
            _ => BTreeSet::from([0, 1, 2]),
        };
        let data = json!({"inputs":(0..3).map(|i|input(i,"token",1<<i)).collect::<Vec<_>>(),"relations":records});
        for mode in ["explain", "fast"] {
            assert_eq!(members(&run(mode, &p, &data).unwrap()), expected);
        }
    }
}

#[test]
fn a_two_step_path_resolves_a_derived_second_step_whose_predicate_has_exists() {
    let case = Case {
        length: 2,
        derived: 2,
        reverse: 0,
        rows: vec![
            (0, 0, 4, true),
            (0, 0, 1, true),
            (1, 1, 3, false),
            (1, 1, 2, true),
            (1, 4, 1, true),
        ],
        allows: vec![31; 4],
        keep: 31,
        missing: None,
        shift: 0,
    };
    let p = program(&case, false, true);
    let data = dataset(&case, true);
    let expected = BTreeSet::from([1, 2]);
    for mode in ["explain", "fast"] {
        assert_eq!(members(&run(mode, &p, &data).unwrap()), expected);
        let mut missing = data.clone();
        missing["inputs"]
            .as_array_mut()
            .unwrap()
            .retain(|input| !(input["name"] == "allow_1" && input["entity_id"] == "p2"));
        let error = run(mode, &p, &missing).unwrap_err();
        assert!(
            error.contains("allow_1") && error.contains("p2"),
            "{mode}: {error}"
        );
        // A partial-period tuple does not reach p3, so that candidate's
        // missing predicate input must remain unread in this evaluation.
        let mut partial = data.clone();
        partial["inputs"]
            .as_array_mut()
            .unwrap()
            .retain(|input| !(input["name"] == "allow_1" && input["entity_id"] == "p3"));
        assert_eq!(members(&run(mode, &p, &partial).unwrap()), expected);
    }
}

#[test]
fn a_three_step_path_changes_entity_kind_twice_and_drops_both_intermediate_frontiers() {
    let case = Case {
        length: 1,
        derived: 0,
        reverse: 0,
        rows: vec![],
        allows: vec![31; 4],
        keep: 31,
        missing: None,
        shift: 0,
    };
    let mut p = program(&case, false, false);
    p["relations"] = json!([
        {"name":"home","arity":2,"slot_entities":["Person","Household"]},
        {"name":"linked_home","arity":2,"slot_entities":["Household","Household"]},
        {"name":"selected","arity":2,"slot_entities":["Person","Person"],"derivation":{
            "source_path":[{"relation":"home","from_slot":0,"to_slot":1},
                {"relation":"linked_home","from_slot":0,"to_slot":1},
                {"relation":"home","from_slot":1,"to_slot":0}],
            "current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":yes()}}
    ]);
    let relation = |name: &str, left: &str, right: &str| {
        json!({"name":name,"tuple":[left,right],
        "interval":{"start":"2026-01-01","end":"2026-12-31"}})
    };
    let data = json!({"inputs":[input(2,"token",4)],"relations":[
        relation("home","p0","h0"),relation("home","p2","h1"),relation("linked_home","h0","h1")
    ]});
    let spec = serde_json::from_value(p.clone()).unwrap();
    CompiledProgramArtifact::compile(spec).unwrap();
    for mode in ["explain", "fast"] {
        assert_eq!(members(&run(mode, &p, &data).unwrap()), BTreeSet::from([2]));
    }
}

#[test]
fn a_failure_at_a_reached_id_fails_a_path_where_exists_stops_early() {
    // Invariant 5 holds when neither side fails. Both p1 and p2 reach p3, and
    // only p2 lacks `allow`. The path resolves its whole frontier first, so
    // p2 fails it. `exists` succeeds at p1 and never reads p2.
    let relation = |name: &str| json!({"name":name,"arity":2,"slot_entities":["Person","Person"]});
    let path = json!({"name":"selected","arity":2,"slot_entities":["Person","Person"],"derivation":{
        "source_path":[{"relation":"first","from_slot":0,"to_slot":1},
            {"relation":"checked","from_slot":0,"to_slot":1}],
        "current_slot":0,"related_slot":1,"slot_entities":["Person","Person"],"predicate":yes()}});
    let exists = derived(
        "selected",
        "only",
        0,
        json!({"kind":"exists","relation":"first","terms":["anchor",variable(0)],"variable_slot":1,
            "body":atom("checked",variable(0),json!("candidate"),false)}),
    );
    let program = |selected: Value| {
        json!({"relations":[
            relation("first"),relation("second"),relation("only"),
            derived("checked","second",0,positive(json!("anchor"),"allowed")),
            selected
        ],"derived":[
            {"name":"count","entity":"Person","dtype":"integer","semantics":"scalar",
                "expr":{"kind":"count_related","relation":"selected","current_slot":0,"related_slot":1}},
            {"name":"mask","entity":"Person","dtype":"integer","semantics":"scalar",
                "expr":{"kind":"sum_related","relation":"selected","current_slot":0,"related_slot":1,"value":{"kind":"derived","name":"token"}}},
            {"name":"token","entity":"Person","dtype":"integer","semantics":"scalar","expr":{"kind":"input","name":"token"}},
            {"name":"allowed","entity":"Person","dtype":"integer","semantics":"scalar","expr":{"kind":"input","name":"allow"}}
        ]})
    };
    let records = [
        tuple("first", 0, 1, false, true),
        tuple("first", 0, 2, false, true),
        tuple("second", 1, 3, false, true),
        tuple("second", 2, 3, false, true),
        tuple("only", 0, 3, false, true),
    ];
    let failing = json!({"inputs":[input(3,"token",8),input(1,"allow",1)],"relations":records});
    let complete = json!({"inputs":[input(3,"token",8),input(1,"allow",1),input(2,"allow",1)],
        "relations":records});
    for mode in ["explain", "fast"] {
        let error = run(mode, &program(path.clone()), &failing).unwrap_err();
        assert!(
            error.contains("allow") && error.contains("p2"),
            "{mode}: {error}"
        );
        assert_eq!(
            members(&run(mode, &program(exists.clone()), &failing).unwrap()),
            BTreeSet::from([3]),
            "{mode}"
        );
        for selected in [&path, &exists] {
            assert_eq!(
                members(&run(mode, &program(selected.clone()), &complete).unwrap()),
                BTreeSet::from([3]),
                "{mode}"
            );
        }
    }
}
