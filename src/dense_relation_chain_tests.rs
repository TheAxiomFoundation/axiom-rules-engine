//! Exercise dense relation walks without invoking typing or dataset binding.

use super::*;
use crate::model::{Derived, RelationDerivation, RelationSchema};
use proptest::prelude::*;

const LINKS: usize = 20_000;

#[derive(Clone, Debug)]
struct Link {
    parent: Option<usize>,
    threshold: i64,
    test_base: bool,
    test_ancestor: Option<usize>,
    divide: bool,
}

fn integer(value: i64) -> ScalarExpr {
    ScalarExpr::Literal(ScalarValue::Integer(value))
}

fn comparison(left: ScalarExpr, op: ComparisonOp, right: ScalarExpr) -> JudgmentExpr {
    JudgmentExpr::Comparison { left, op, right }
}

fn count(index: usize) -> ScalarExpr {
    ScalarExpr::CountRelated {
        relation: format!("link{index}"),
        current_slot: 0,
        related_slot: 1,
        where_clause: None,
    }
}

fn output(name: String, expr: ScalarExpr) -> Derived {
    Derived {
        id: None,
        name,
        entity: "Household".to_string(),
        dtype: DType::Integer,
        unit: None,
        rounding: None,
        source: None,
        source_url: None,
        corpus_citation_path: None,
        semantics: DerivedSemantics::Scalar(expr),
        versions: Vec::new(),
    }
}

fn program(links: &[Link], outputs: &[usize], masked: bool) -> Program {
    let mut program = Program::default();
    let slots = vec!["Household".to_string(), "Person".to_string()];
    program.relations.insert(
        "base".to_string(),
        RelationSchema {
            name: "base".to_string(),
            arity: 2,
            slot_entities: slots.clone(),
            derivation: None,
        },
    );
    for (index, link) in links.iter().enumerate() {
        let mut predicates = vec![comparison(
            ScalarExpr::Input("value".to_string()),
            ComparisonOp::Gte,
            integer(link.threshold),
        )];
        if link.test_base {
            predicates.push(JudgmentExpr::RelationMember {
                relation: "base".to_string(),
                current_slot: 0,
                related_slot: 1,
            });
        }
        if let Some(ancestor) = link.test_ancestor {
            predicates.push(JudgmentExpr::RelationMember {
                relation: format!("link{ancestor}"),
                current_slot: 0,
                related_slot: 1,
            });
        }
        if link.divide {
            predicates.push(comparison(
                ScalarExpr::Div(
                    Box::new(integer(1)),
                    Box::new(ScalarExpr::Input("value".to_string())),
                ),
                ComparisonOp::Gt,
                integer(0),
            ));
        }
        let name = format!("link{index}");
        program.relations.insert(
            name.clone(),
            RelationSchema {
                name,
                arity: 2,
                slot_entities: slots.clone(),
                derivation: Some(RelationDerivation {
                    source_relation: link
                        .parent
                        .map_or_else(|| "base".to_string(), |parent| format!("link{parent}")),
                    current_slot: 0,
                    related_slot: 1,
                    entity: None,
                    member_relation: None,
                    slot_entities: slots.clone(),
                    predicate: JudgmentExpr::And(predicates),
                }),
            },
        );
    }
    for &index in outputs {
        let name = format!("count{index}");
        program
            .derived
            .insert(name.clone(), output(name, count(index)));
    }
    if masked {
        // Both branches query the same relation for different root masks.
        // Equal-sized masks must never reuse each other's resolved members.
        let expr = ScalarExpr::If {
            condition: Box::new(comparison(
                ScalarExpr::Input("choose".to_string()),
                ComparisonOp::Eq,
                integer(1),
            )),
            then_expr: Box::new(count(links.len() - 1)),
            else_expr: Box::new(count(links.len() - 1)),
        };
        program
            .derived
            .insert("masked".to_string(), output("masked".to_string(), expr));
    }
    program
}

fn compile(program: &Program) -> Result<DenseCompiledProgram, DenseCompileError> {
    let mut compiler = DenseCompiler::new(program, "Household".to_string())?;
    // Compile directly: depth tests must reach this walk independently of
    // the public entry point's typing check.
    for name in program.derived.keys() {
        compiler.compile_derived(name)?;
    }
    // This walk compiles every rule itself, so nothing is left out.
    Ok(compiler.finish(std::collections::BTreeMap::new()))
}

fn batch(households: &[Vec<i64>]) -> DenseBatchSpec {
    let mut offsets = vec![0];
    let mut values = Vec::new();
    for household in households {
        values.extend_from_slice(household);
        offsets.push(values.len());
    }
    DenseBatchSpec {
        row_count: households.len(),
        inputs: HashMap::from([(
            "choose".to_string(),
            DenseColumn::Integer((0..households.len()).map(|row| (row % 2) as i64).collect()),
        )]),
        relations: HashMap::from([(
            DenseRelationKey {
                name: "base".to_string(),
                current_slot: 0,
                related_slot: 1,
            },
            DenseRelationBatchSpec {
                offsets,
                inputs: HashMap::from([("value".to_string(), DenseColumn::Integer(values))]),
            },
        )]),
    }
}

fn counts(result: &DenseExecutionResult, name: &str) -> Vec<i64> {
    match &result.outputs[name] {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values.clone(),
        other => panic!("expected integer counts, got {other:?}"),
    }
}

fn chain(length: usize) -> Vec<Link> {
    (0..length)
        .map(|index| Link {
            parent: index.checked_sub(1),
            threshold: 0,
            test_base: true,
            test_ancestor: (index > 0).then_some(index / 2),
            divide: false,
        })
        .collect()
}

#[test]
fn dense_compiler_walks_20_000_source_links_directly() {
    let program = program(&chain(LINKS), &[LINKS - 1], false);
    let dense = compile(&program).expect("a deep source chain compiles");
    assert_eq!(dense.relations.len(), LINKS);
}

#[test]
fn review_dense_candidate_membership_cache_is_linear_in_source_links() {
    let mut measurements = Vec::new();
    for length in [128_usize, 512] {
        let mut links = chain(length);
        for link in &mut links {
            link.test_base = false;
        }
        let candidate = program(&links, &[length - 1], false);
        let mut compiler = DenseCompiler::new(&candidate, "Household".to_string()).unwrap();
        compiler
            .compile_derived(&format!("count{}", length - 1))
            .expect("all queried membership relations are source ancestors");
        let retained = compiler.candidate_membership.len();
        let (index_entries, visits) =
            compiler
                .candidate_ancestry
                .as_ref()
                .map_or((0, usize::MAX), |ancestry| {
                    let branches = ancestry
                        .scopes
                        .values()
                        .map(|scope| scope.children.len())
                        .sum::<usize>();
                    (ancestry.scopes.len() + branches, ancestry.visits)
                });
        eprintln!(
            "membership-cache links={length} entries={retained} ancestry_entries={index_entries} visits={visits}"
        );
        measurements.push((length, retained, index_entries, visits));
    }
    for (length, retained, index_entries, visits) in measurements {
        assert!(
            retained <= length,
            "retain only actual queries: {retained} entries for {length} links"
        );
        assert!(
            index_entries <= 2 * length + 1,
            "linear source index: {index_entries} entries for {length} links"
        );
        assert!(
            visits <= 4 * length,
            "linear indexing and query work: {visits} visits for {length} links"
        );
    }
}

#[test]
fn empty_dense_source_chains_share_masks_and_skip_later_filters() {
    let length = 1_000;
    let width = 1_000;
    let mut dense = compile(&program(&chain(length), &[length - 1], false)).unwrap();
    dense.relations[0].filter = Some(CompiledRelatedJudgmentExpr::Literal(false));
    // A root expression still traverses compiled nodes for an empty mask
    // unless relation resolution skips filters after the source empties.
    for relation in &mut dense.relations[1..] {
        relation.filter = Some(CompiledRelatedJudgmentExpr::RootJudgment(Box::new(
            CompiledJudgmentExpr::Comparison {
                left: CompiledScalarExpr::Literal(ScalarValue::Integer(1)),
                op: ComparisonOp::Eq,
                right: CompiledScalarExpr::Literal(ScalarValue::Integer(1)),
            },
        )));
    }
    let bound = dense.bind_batch(batch(&[vec![-1; width]])).unwrap();
    let period = Period::month(2026, 1);
    let mut executor = DenseExecutor::<Decimal>::new(&dense, &period, bound, false);
    let (members, errors) = executor
        .relation_members(length - 1, &RowMask::all(width))
        .unwrap();
    assert!(members.is_empty());
    assert!(errors.is_empty());
    let cached = executor
        .relation_members
        .iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(cached.len(), length);
    let backing_masks = 1 + cached
        .windows(2)
        .filter(|pair| !pair[0].members.shares_storage_with(&pair[1].members))
        .count();
    eprintln!(
        "empty-chain links={length} width={width} cached_masks={} backing_masks={backing_masks} predicate_visits={}",
        cached.len(),
        executor.visits
    );
    assert!(
        cached
            .iter()
            .all(|entry| entry.members.shares_storage_with(&members))
    );
    assert_eq!(
        executor.visits, 0,
        "no later predicate expression is reached"
    );
}

#[test]
fn dense_executor_walks_20_000_parent_links_directly() {
    // Construct a deep compiled chain from a single compiled link so this
    // test reaches the evaluator even if the compiler's walk regresses.
    let mut dense = compile(&program(&chain(1), &[0], false)).expect("one link compiles");
    let schema = dense.relations[0].clone();
    dense.relations = (0..LINKS)
        .map(|index| DenseRelationSchema {
            parent_relation: index.checked_sub(1),
            ..schema.clone()
        })
        .collect();
    dense.derived[0].semantics = CompiledSemantics::Scalar(CompiledScalarExpr::CountRelated {
        relation: LINKS - 1,
        predicate: None,
    });
    let households = vec![vec![1, -1, 2], vec![], vec![3]];
    let result = dense
        .execute(
            &Period::month(2026, 1),
            batch(&households),
            &["count0".to_string()],
        )
        .expect("a deep parent chain evaluates");
    assert_eq!(counts(&result, "count0"), vec![2, 0, 1]);
}

#[test]
fn dense_compiler_refuses_a_long_source_cycle_with_the_existing_diagnostic() {
    let mut program = program(&chain(LINKS), &[LINKS - 1], false);
    program
        .relations
        .get_mut("link0")
        .unwrap()
        .derivation
        .as_mut()
        .unwrap()
        .source_relation = format!("link{}", LINKS - 1);
    let error = compile(&program).expect_err("a long source cycle is refused");
    assert!(
        matches!(error, DenseCompileError::Unsupported(ref message)
        if message == &format!("cyclic dense compilation dependency involving `link{}`", LINKS - 1)),
        "the cycle keeps its established diagnostic: {error}"
    );
}

/// The original recursive source/filter semantics, kept only for small
/// generated graphs. Each parent is resolved first; a removed candidate is
/// never evaluated by a later predicate, and a zero divisor stops its root.
fn recursive_members(links: &[Link], index: usize, values: &[i64]) -> Result<Vec<usize>, ()> {
    let link = &links[index];
    let candidates = match link.parent {
        Some(parent) => recursive_members(links, parent, values)?,
        None => (0..values.len()).collect(),
    };
    let mut members = Vec::new();
    for candidate in candidates {
        let value = values[candidate];
        if value < link.threshold {
            continue;
        }
        if link.divide {
            if value == 0 {
                return Err(());
            }
            if value < 0 {
                continue;
            }
        }
        members.push(candidate);
    }
    Ok(members)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn dense_chains_and_shared_parent_dags_match_recursive_semantics(
        raw in prop::collection::vec((any::<usize>(), -4_i64..6, any::<bool>(), any::<usize>(), 0_u8..8), 1..12),
        households in prop::collection::vec(prop::collection::vec(-4_i64..6, 0..6), 1..7),
        use_chain in any::<bool>(),
    ) {
        let mut links = Vec::<Link>::new();
        for (index, (parent, threshold, test_base, ancestor, divide)) in raw.into_iter().enumerate() {
            let parent = if use_chain { index } else { parent % (index + 1) };
            let parent = parent.checked_sub(1);
            let mut ancestors = Vec::new();
            let mut next = parent;
            while let Some(index) = next {
                ancestors.push(index);
                next = links[index].parent;
            }
            let test_ancestor = (!ancestors.is_empty()).then(|| ancestors[ancestor % ancestors.len()]);
            links.push(Link { parent, threshold, test_base, test_ancestor, divide: divide == 0 });
        }
        // Compare interval lookup with the original source walk for sibling,
        // descendant and ancestor queries, including different edge slots.
        let mut oriented = program(&links, &[], false);
        for index in 0..links.len() {
            let derivation = oriented.relations.get_mut(&format!("link{index}"))
                .unwrap().derivation.as_mut().unwrap();
            derivation.current_slot = index % 2;
            derivation.related_slot = 1 - index % 2;
        }
        let mut compiler = DenseCompiler::new(&oriented, "Household".to_string()).unwrap();
        for scope in 0..links.len() {
            for target in std::iter::once("base".to_string()).chain((0..links.len()).map(|index| format!("link{index}"))) {
                for current_slot in 0..2 {
                    let related_slot = 1 - current_slot;
                    let scope = format!("link{scope}");
                    let mut name = scope.as_str();
                    let expected = loop {
                        let Some(derivation) = oriented.relations.get(name).and_then(|relation| relation.derivation.as_ref()) else { break false; };
                        if derivation.source_relation == target {
                            break derivation.current_slot == current_slot && derivation.related_slot == related_slot;
                        }
                        name = &derivation.source_relation;
                    };
                    prop_assert_eq!(compiler.holds_for_every_candidate(&scope, &target, current_slot, related_slot), expected);
                }
            }
        }
        let outputs = (0..links.len()).collect::<Vec<_>>();
        let dense = compile(&program(&links, &outputs, true)).expect("an acyclic graph compiles");
        let requested = outputs.iter().map(|index| format!("count{index}"))
            .chain(std::iter::once("masked".to_string())).collect::<Vec<_>>();
        let expected = outputs.iter().map(|&index| {
            households.iter().map(|values| recursive_members(&links, index, values)
                .map(|members| members.len() as i64)).collect::<Result<Vec<_>, _>>()
        }).collect::<Result<Vec<_>, _>>();
        for result in [
            dense.execute(&Period::month(2026, 1), batch(&households), &requested),
            dense.execute_f64(&Period::month(2026, 1), batch(&households), &requested),
        ] {
            match (&expected, result) {
                (Ok(expected), Ok(result)) => {
                    for (&index, expected) in outputs.iter().zip(expected) {
                        prop_assert_eq!(counts(&result, &format!("count{index}")), expected.clone());
                    }
                    prop_assert_eq!(counts(&result, "masked"), expected.last().unwrap().clone());
                }
                (Err(()), Err(EvalError::DivisionByZero)) => {}
                (expected, result) => prop_assert!(false, "recursive result {expected:?}, dense result {result:?}"),
            }
        }
    }

    #[test]
    fn dense_source_cycles_of_random_lengths_are_refused(length in 1_usize..32) {
        let mut program = program(&chain(length), &[length - 1], false);
        program.relations.get_mut("link0").unwrap().derivation.as_mut().unwrap().source_relation =
            format!("link{}", length - 1);
        let error = compile(&program).expect_err("a source cycle is refused");
        prop_assert!(matches!(error, DenseCompileError::Unsupported(ref message)
            if message == &format!("cyclic dense compilation dependency involving `link{}`", length - 1)), "unexpected cycle diagnostic: {}", error);
    }
}
