//! `DenseCompiledProgram::from_program` compiles a raw `Program`, which
//! `ProgramSpec::to_program` builds without checking the dependency graph. The
//! dense compiler inlines a related entity's rules into `count`/`sum`
//! expressions, inlines a current-entity rule into a related one, and resolves
//! a derived relation's source chain, each recursively. A cycle along any of
//! those paths used to recurse until the stack overflowed and aborted the
//! process; it is now refused with the error a cycle among root rules gets.
//!
//! A stack overflow aborts this test binary rather than failing one test, so a
//! regression shows as the whole binary dying.

use std::collections::{BTreeMap, BTreeSet};

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::spec::ProgramSpec;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

const CYCLE_PREFIX: &str = "cyclic dense compilation dependency involving `";

fn program(relations: Vec<Value>, derived: Vec<Value>) -> ProgramSpec {
    serde_json::from_value(json!({ "relations": relations, "derived": derived }))
        .expect("a valid ProgramSpec")
}

fn compile(spec: &ProgramSpec) -> Result<DenseCompiledProgram, DenseCompileError> {
    let program = spec
        .to_program()
        .expect("to_program does not check the graph");
    DenseCompiledProgram::from_program(&program, Some("Household"))
}

/// The rule or relation a dense cycle refusal names, if `error` is one.
fn cycle_member(error: &DenseCompileError) -> Option<&str> {
    match error {
        DenseCompileError::Unsupported(message) => message
            .strip_prefix(CYCLE_PREFIX)
            .and_then(|rest| rest.strip_suffix('`')),
        _ => None,
    }
}

/// `spec` is refused as a cycle through one of `members`, and compiling it as
/// an artifact refuses it too. `Program` keeps rules in a hash map, so the root
/// rule compiled first, and with it where the cycle is entered, varies from one
/// `Program` to the next; each repetition builds a fresh one.
fn assert_cycle_refused(spec: &ProgramSpec, members: &[&str]) {
    for _ in 0..32 {
        let error = compile(spec).expect_err("a cyclic program is refused");
        let member = cycle_member(&error)
            .unwrap_or_else(|| panic!("expected a cycle refusal, got: {error}"));
        assert!(
            members.contains(&member),
            "`{member}` is not on the cycle {members:?}: {error}"
        );
    }
    let artifact = CompiledProgramArtifact::compile(spec.clone());
    assert!(
        matches!(
            artifact,
            Err(CompileError::CyclicDependency { .. }
                | CompileError::CyclicRelationDependency { .. })
        ),
        "the artifact compiler also refuses the cycle: {artifact:?}"
    );
}

fn rule(name: &str, entity: &str, semantics: &str, expr: Value) -> Value {
    let dtype = if semantics == "judgment" {
        "judgment"
    } else {
        "integer"
    };
    json!({
        "name": name,
        "entity": entity,
        "dtype": dtype,
        "unit": null,
        "semantics": semantics,
        "expr": expr,
    })
}

fn judgment(name: &str, entity: &str, expr: Value) -> Value {
    rule(name, entity, "judgment", expr)
}

fn scalar(name: &str, entity: &str, expr: Value) -> Value {
    rule(name, entity, "scalar", expr)
}

fn derived(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn positive(value: Value) -> Value {
    json!({ "kind": "comparison", "left": value, "op": "gt", "right": integer(0) })
}

fn count(relation: &str, where_clause: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "count_related",
        "relation": relation,
        "current_slot": 0,
        "related_slot": 1,
    });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn sum(relation: &str, value: &str) -> Value {
    json!({
        "kind": "sum_related",
        "relation": relation,
        "current_slot": 0,
        "related_slot": 1,
        "value": { "kind": "derived", "name": value },
    })
}

fn member_relation() -> Value {
    json!({ "name": "member", "arity": 2, "slot_entities": ["Household", "Person"] })
}

fn derived_relation(name: &str, source: &str, predicate: Value) -> Value {
    json!({
        "name": name,
        "arity": 2,
        "slot_entities": ["Household", "Person"],
        "derivation": {
            "source_relation": source,
            "current_slot": 0,
            "related_slot": 1,
            "slot_entities": ["Household", "Person"],
            "predicate": predicate,
        },
    })
}

#[test]
fn a_person_judgment_cycle_read_by_a_where_clause_is_refused() {
    let spec = program(
        vec![member_relation()],
        vec![
            judgment("pr0", "Person", derived("pr1")),
            judgment("pr1", "Person", derived("pr0")),
            scalar("n", "Household", count("member", Some(derived("pr0")))),
        ],
    );
    assert_cycle_refused(&spec, &["pr0", "pr1"]);
}

#[test]
fn a_person_scalar_cycle_read_by_a_sum_value_is_refused() {
    let spec = program(
        vec![member_relation()],
        vec![
            scalar(
                "ps0",
                "Person",
                json!({ "kind": "add", "items": [derived("ps1"), integer(1)] }),
            ),
            scalar("ps1", "Person", derived("ps0")),
            scalar("total", "Household", sum("member", "ps0")),
        ],
    );
    assert_cycle_refused(&spec, &["ps0", "ps1"]);
}

#[test]
fn a_person_cycle_through_a_judgment_and_a_scalar_is_refused() {
    let spec = program(
        vec![member_relation()],
        vec![
            judgment("eligible", "Person", positive(derived("points"))),
            scalar(
                "points",
                "Person",
                json!({
                    "kind": "if",
                    "condition": derived("eligible"),
                    "then_expr": integer(1),
                    "else_expr": integer(0),
                }),
            ),
            scalar("n", "Household", count("member", Some(derived("eligible")))),
        ],
    );
    assert_cycle_refused(&spec, &["eligible", "points"]);
}

#[test]
fn a_household_rule_cycle_read_by_a_derived_relation_predicate_is_refused() {
    // `n` reaches `ha` and `hb` only by inlining them into the predicate of
    // `eligible_member`; compiled first, `ha` or `hb` meets the cycle as root
    // rules instead. Every order must refuse it.
    let spec = program(
        vec![
            member_relation(),
            derived_relation("eligible_member", "member", derived("ha")),
        ],
        vec![
            judgment("ha", "Household", derived("hb")),
            judgment("hb", "Household", derived("ha")),
            scalar("n", "Household", count("eligible_member", None)),
        ],
    );
    assert_cycle_refused(&spec, &["ha", "hb"]);
}

#[test]
fn a_scalar_entity_cycle_read_by_a_sum_value_is_refused() {
    // Formula parameters are never root rules here, so the only way in is the
    // current-entity inlining of `sum`'s value.
    let spec = program(
        vec![member_relation()],
        vec![
            scalar("share", "Person", derived("rate_a")),
            scalar(
                "rate_a",
                "Scalar",
                json!({ "kind": "add", "items": [derived("rate_b"), integer(1)] }),
            ),
            scalar("rate_b", "Scalar", derived("rate_a")),
            scalar("total", "Household", sum("member", "share")),
        ],
    );
    assert_cycle_refused(&spec, &["rate_a", "rate_b"]);
}

#[test]
fn a_root_rule_read_back_by_its_own_where_clause_is_refused() {
    // This terminated before, but with an unrelated refusal (a current-entity
    // expression cannot aggregate); it is a cycle, and now says so.
    let spec = program(
        vec![member_relation()],
        vec![
            judgment("in_counted_household", "Person", derived("counted")),
            judgment("counted", "Household", positive(derived("n"))),
            scalar(
                "n",
                "Household",
                count("member", Some(derived("in_counted_household"))),
            ),
        ],
    );
    assert_cycle_refused(&spec, &["n", "counted", "in_counted_household"]);
}

#[test]
fn a_derived_relation_derived_from_itself_is_refused() {
    let spec = program(
        vec![
            member_relation(),
            derived_relation("loop", "loop", positive(integer(1))),
        ],
        vec![scalar("n", "Household", count("loop", None))],
    );
    assert_cycle_refused(&spec, &["loop"]);
}

#[test]
fn a_derived_relation_source_cycle_is_refused() {
    let spec = program(
        vec![
            member_relation(),
            derived_relation("ra", "rb", positive(integer(1))),
            derived_relation("rb", "ra", positive(integer(1))),
        ],
        vec![scalar("n", "Household", count("ra", None))],
    );
    assert_cycle_refused(&spec, &["ra", "rb"]);
}

#[test]
fn shared_and_repeated_dependencies_still_compile() {
    // Rules read twice along different paths, or twice in one expression, are
    // not cycles: the guard tracks the path being compiled, not every rule
    // compiled so far.
    let adult = || positive(json!({ "kind": "sub", "left": input("age"), "right": integer(17) }));
    let spec = program(
        vec![
            member_relation(),
            derived_relation(
                "adult_member",
                "member",
                json!({ "kind": "and", "items": [derived("adult"), derived("has_income")] }),
            ),
            derived_relation("adult_member_again", "adult_member", derived("adult")),
        ],
        vec![
            judgment("adult", "Person", adult()),
            judgment("counted_adult", "Person", derived("adult")),
            judgment(
                "both",
                "Person",
                json!({ "kind": "and", "items": [
                    derived("adult"),
                    derived("counted_adult"),
                    derived("has_income"),
                    derived("has_income"),
                ] }),
            ),
            scalar(
                "weight",
                "Person",
                json!({ "kind": "add", "items": [derived("base_weight"), derived("base_weight")] }),
            ),
            scalar(
                "base_weight",
                "Person",
                json!({ "kind": "add", "items": [derived("rate"), derived("income")] }),
            ),
            scalar("rate", "Scalar", integer(2)),
            scalar("income", "Household", input("income")),
            judgment("has_income", "Household", positive(derived("income"))),
            scalar("n", "Household", count("member", Some(derived("both")))),
            scalar("adults", "Household", count("adult_member_again", None)),
            scalar("total", "Household", sum("adult_member", "weight")),
            scalar(
                "combined",
                "Household",
                json!({ "kind": "add", "items": [derived("n"), derived("adults"), derived("total")] }),
            ),
        ],
    );
    for _ in 0..32 {
        let compiled = compile(&spec).expect("an acyclic program compiles");
        let outputs = compiled.output_names().into_iter().collect::<BTreeSet<_>>();
        for name in ["n", "adults", "total", "combined", "income", "has_income"] {
            assert!(outputs.contains(name), "{name} missing from {outputs:?}");
        }
    }
    CompiledProgramArtifact::compile(spec).expect("the artifact compiler accepts it too");
}

// ---------------------------------------------------------------------------
// Random rule graphs
//
// Small programs over Household (the root), Person, and formula parameters,
// with a base relation and derived relations, whose rules read one another
// through every path the dense compiler inlines: `where` clauses, `sum`
// values, related and current-entity rules, derived relation predicates, and
// derived relation sources. References are drawn at random, so many programs
// are cyclic. A graph built here, independently of the engine, decides which.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Judgment,
    Scalar,
}

struct RuleShape {
    name: String,
    entity: &'static str,
    kind: Kind,
}

/// Draws choices from a generated sequence, cycling if it runs out.
struct Choices<'c> {
    values: &'c [u32],
    next: usize,
}

impl Choices<'_> {
    fn below(&mut self, bound: usize) -> usize {
        let value = self.values[self.next % self.values.len()] as usize % bound;
        self.next += 1;
        value
    }
}

struct Generator<'c> {
    choices: Choices<'c>,
    rules: Vec<RuleShape>,
    relations: Vec<String>,
}

impl Generator<'_> {
    /// A reference to a rule of `kind`, preferring `entities`; a literal when
    /// the program has no rule of that kind.
    fn reference(&mut self, kind: Kind, entities: &[&str]) -> Value {
        let preferred = self
            .rules
            .iter()
            .filter(|rule| rule.kind == kind && entities.contains(&rule.entity))
            .map(|rule| rule.name.clone())
            .collect::<Vec<_>>();
        let any = self
            .rules
            .iter()
            .filter(|rule| rule.kind == kind)
            .map(|rule| rule.name.clone())
            .collect::<Vec<_>>();
        // Mostly well-placed references, sometimes any entity, so the
        // compiler's cross-entity refusals are exercised too.
        let pool = if preferred.is_empty() || self.choices.below(8) == 0 {
            any
        } else {
            preferred
        };
        if pool.is_empty() {
            return match kind {
                Kind::Judgment => positive(integer(1)),
                Kind::Scalar => integer(1),
            };
        }
        derived(&pool[self.choices.below(pool.len())])
    }

    fn relation(&mut self) -> String {
        let index = self.choices.below(self.relations.len() + 1);
        if index == 0 {
            "member".to_string()
        } else {
            self.relations[index - 1].clone()
        }
    }

    fn related_scalar_name(&mut self) -> Option<String> {
        let scalars = self
            .rules
            .iter()
            .filter(|rule| rule.kind == Kind::Scalar)
            .map(|rule| rule.name.clone())
            .collect::<Vec<_>>();
        (!scalars.is_empty()).then(|| scalars[self.choices.below(scalars.len())].clone())
    }

    fn scalar_expr(&mut self, entity: &str) -> Value {
        let local: &[&str] = match entity {
            "Person" => &["Person", "Household", "Scalar"],
            _ => &["Household", "Scalar"],
        };
        let aggregates = entity != "Person";
        match self.choices.below(7) {
            0 => integer(1),
            1 => input("x"),
            2 => {
                let item = self.reference(Kind::Scalar, local);
                json!({ "kind": "add", "items": [item, integer(1)] })
            }
            3 => {
                let condition = self.reference(Kind::Judgment, local);
                let then_expr = self.reference(Kind::Scalar, local);
                json!({ "kind": "if", "condition": condition, "then_expr": then_expr, "else_expr": integer(0) })
            }
            4 if aggregates => {
                let relation = self.relation();
                let where_clause = (self.choices.below(4) != 0)
                    .then(|| self.reference(Kind::Judgment, &["Person"]));
                count(&relation, where_clause)
            }
            5 if aggregates => {
                let relation = self.relation();
                match self.related_scalar_name() {
                    Some(value) => sum(&relation, &value),
                    None => count(&relation, None),
                }
            }
            _ => self.reference(Kind::Scalar, local),
        }
    }

    fn judgment_expr(&mut self, entity: &str) -> Value {
        let local: &[&str] = match entity {
            "Person" => &["Person", "Household", "Scalar"],
            _ => &["Household", "Scalar"],
        };
        match self.choices.below(4) {
            0 => positive(self.scalar_expr(entity)),
            1 => {
                let left = self.reference(Kind::Judgment, local);
                let right = self.reference(Kind::Judgment, local);
                json!({ "kind": "and", "items": [left, right] })
            }
            2 => json!({ "kind": "not", "item": self.reference(Kind::Judgment, local) }),
            _ => self.reference(Kind::Judgment, local),
        }
    }
}

#[derive(Clone, Debug)]
struct GraphCase {
    households: usize,
    persons: usize,
    parameters: usize,
    derived_relations: usize,
    choices: Vec<u32>,
}

fn graph_case() -> impl Strategy<Value = GraphCase> {
    (
        1..=3usize,
        1..=3usize,
        0..=2usize,
        0..=2usize,
        vec(any::<u32>(), 64),
    )
        .prop_map(
            |(households, persons, parameters, derived_relations, choices)| GraphCase {
                households,
                persons,
                parameters,
                derived_relations,
                choices,
            },
        )
}

fn build(case: &GraphCase) -> ProgramSpec {
    let mut generator = Generator {
        choices: Choices {
            values: &case.choices,
            next: 0,
        },
        rules: Vec::new(),
        relations: (0..case.derived_relations)
            .map(|index| format!("rel{index}"))
            .collect(),
    };
    for (prefix, entity, count) in [
        ("h", "Household", case.households),
        ("p", "Person", case.persons),
        ("s", "Scalar", case.parameters),
    ] {
        for index in 0..count {
            let kind = if generator.choices.below(2) == 0 {
                Kind::Judgment
            } else {
                Kind::Scalar
            };
            generator.rules.push(RuleShape {
                name: format!("{prefix}{index}"),
                entity,
                kind,
            });
        }
    }

    let mut relations = vec![member_relation()];
    for index in 0..case.derived_relations {
        let source = generator.relation();
        let predicate = generator.reference(Kind::Judgment, &["Person", "Household"]);
        relations.push(derived_relation(&format!("rel{index}"), &source, predicate));
    }
    let mut derived = Vec::new();
    for index in 0..generator.rules.len() {
        let (name, entity, kind) = {
            let rule = &generator.rules[index];
            (rule.name.clone(), rule.entity, rule.kind)
        };
        derived.push(match kind {
            Kind::Judgment => judgment(&name, entity, generator.judgment_expr(entity)),
            Kind::Scalar => scalar(&name, entity, generator.scalar_expr(entity)),
        });
    }
    program(relations, derived)
}

/// The spec's dependency graph: `rule:x` reads the rules and relations named
/// in its formula, and `relation:r` reads the rules its predicate names and
/// its source relation.
fn dependency_graph(spec: &Value) -> BTreeMap<String, BTreeSet<String>> {
    fn collect(value: &Value, into: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                match object.get("kind").and_then(Value::as_str) {
                    Some("derived") => {
                        into.insert(format!("rule:{}", object["name"].as_str().unwrap()));
                    }
                    Some("count_related" | "sum_related") => {
                        into.insert(format!("relation:{}", object["relation"].as_str().unwrap()));
                    }
                    _ => {}
                }
                object.values().for_each(|inner| collect(inner, into));
            }
            Value::Array(items) => items.iter().for_each(|inner| collect(inner, into)),
            _ => {}
        }
    }
    let mut graph = BTreeMap::new();
    for rule in spec["derived"].as_array().unwrap() {
        let mut edges = BTreeSet::new();
        collect(&rule["expr"], &mut edges);
        graph.insert(format!("rule:{}", rule["name"].as_str().unwrap()), edges);
    }
    for relation in spec["relations"].as_array().unwrap() {
        let mut edges = BTreeSet::new();
        if let Some(derivation) = relation.get("derivation") {
            collect(&derivation["predicate"], &mut edges);
            edges.insert(format!(
                "relation:{}",
                derivation["source_relation"].as_str().unwrap()
            ));
        }
        graph.insert(
            format!("relation:{}", relation["name"].as_str().unwrap()),
            edges,
        );
    }
    graph
}

fn reachable(graph: &BTreeMap<String, BTreeSet<String>>, from: &[String]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = from.to_vec();
    while let Some(node) = stack.pop() {
        if seen.insert(node.clone()) {
            stack.extend(graph[&node].iter().cloned());
        }
    }
    seen
}

/// The nodes that lie on a cycle: those reachable from one of their own
/// dependencies.
fn on_cycles(graph: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    graph
        .iter()
        .filter(|(node, edges)| {
            reachable(graph, &edges.iter().cloned().collect::<Vec<_>>()).contains(*node)
        })
        .map(|(node, _)| node.clone())
        .collect()
}

#[derive(Default)]
struct Tally {
    cycles: BTreeMap<&'static str, usize>,
    other_errors: usize,
    compiled: usize,
}

/// For every generated program, `from_program` returns rather than
/// overflowing, and:
///
/// * it refuses as a cycle only a rule or relation that lies on a cycle, so an
///   acyclic program never meets the new refusal;
/// * it refuses every program with a cycle its root rules reach;
/// * the artifact compiler, an independent cycle check, agrees with the graph
///   built here on whether the program is cyclic at all.
#[test]
fn from_program_refuses_exactly_the_cycles_it_reaches() {
    let config = Config {
        cases: 3000,
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]));
    let tally = std::cell::RefCell::new(Tally::default());
    let outcome = runner.run(&graph_case(), |case| {
        let spec = build(&case);
        let json = serde_json::to_value(&spec).expect("serializes");
        let graph = dependency_graph(&json);
        let cyclic = on_cycles(&graph);
        let roots = graph
            .keys()
            .filter(|node| {
                json["derived"].as_array().unwrap().iter().any(|rule| {
                    rule["entity"] == "Household"
                        && **node == format!("rule:{}", rule["name"].as_str().unwrap())
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let reaches_cycle = reachable(&graph, &roots)
            .iter()
            .any(|node| cyclic.contains(node));

        let result = compile(&spec);
        let mut tally = tally.borrow_mut();
        match &result {
            Ok(_) => tally.compiled += 1,
            Err(error) => match cycle_member(error) {
                Some(member) => {
                    let rule = format!("rule:{member}");
                    let relation = format!("relation:{member}");
                    prop_assert!(
                        cyclic.contains(&rule) || cyclic.contains(&relation),
                        "refused `{member}`, which lies on no cycle: {error}"
                    );
                    let class = if cyclic.contains(&relation) {
                        "relation"
                    } else {
                        match member.as_bytes()[0] {
                            b'h' => "household rule",
                            b'p' => "person rule",
                            _ => "formula parameter",
                        }
                    };
                    *tally.cycles.entry(class).or_default() += 1;
                }
                None => tally.other_errors += 1,
            },
        }
        if reaches_cycle {
            prop_assert!(
                result.is_err(),
                "compiled a program whose root rules reach a cycle through {cyclic:?}"
            );
        }

        let artifact = CompiledProgramArtifact::compile(spec.clone());
        match &artifact {
            Err(
                CompileError::CyclicDependency { .. }
                | CompileError::CyclicRelationDependency { .. },
            ) => prop_assert!(
                !cyclic.is_empty(),
                "the artifact compiler found a cycle the graph lacks: {artifact:?}"
            ),
            Ok(_) => prop_assert!(
                cyclic.is_empty(),
                "the artifact compiler missed the cycle through {cyclic:?}"
            ),
            Err(error) => {
                return Err(TestCaseError::fail(format!(
                    "unexpected artifact compile error: {error}"
                )));
            }
        }
        Ok(())
    });
    if let Err(error) = outcome {
        panic!("{error}");
    }

    // The generator must keep reaching each path: a cycle refused through a
    // relation's source chain, through each kind of inlined rule, and
    // acyclic programs that compile.
    let tally = tally.into_inner();
    for class in [
        "relation",
        "household rule",
        "person rule",
        "formula parameter",
    ] {
        assert!(
            tally.cycles.get(class).copied().unwrap_or(0) >= 10,
            "too few cycles refused through a {class}: {:?}",
            tally.cycles
        );
    }
    assert!(
        tally.compiled >= 100,
        "too few programs compiled: {}",
        tally.compiled
    );
    assert!(tally.other_errors > 0);
}
