//! Properties of dense programs compiled for requested outputs (#266), over
//! the programs `support/artifact_interface_generator.rs` builds. They mix
//! rules of up to four entities with relations, derived relations, versioned
//! rules and parameter tables. When the first choice is odd they also hold
//! `cross_kind`, a `Household` rule that reads the `Person` rule
//! `person_value` directly; no other rule reads it.
//!
//! For each program `P`, its `Household` rules and `W`, the whole-root
//! `Household` program of `P` without `cross_kind`:
//!
//! 1. The whole root of `P` refuses `cross_kind` when `P` holds it, and `W`
//!    compiles.
//! 2. The requested closure of `cross_kind` refuses it with the whole root's
//!    error, naming `members`, the one data relation `P` declares between
//!    `Household` and `Person`; derived relations over it are not named.
//! 3. For every other rule `r`, the requested closure of `[r]` compiles from
//!    `P`; its outputs, root inputs and relations are among `W`'s, it holds
//!    `r`, and it executes `r` to `W`'s exact result.
//! 4. The requested closure of every `Household` rule but `cross_kind` has
//!    `W`'s outputs, root inputs and relations, and executes each output to
//!    `W`'s exact result.
//!
//! Results are compared on two batches: the generator's own, and
//! [`varied_batch`], whose every slot holds its own values on each row, so
//! a program that read another slot or row of the same type would differ.

#[allow(dead_code)]
#[path = "support/artifact_interface_generator.rs"]
mod generator;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseRelationBatchSpec,
    DenseRelationKey,
};
use axiom_rules_engine::spec::{PeriodSpec, ScalarValueSpec};
use generator::{Generated, generate};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rust_decimal::Decimal;
use serde_json::json;

const ROOT: &str = "Household";
const CROSSING: &str = "cross_kind";

fn runner(cases: u32, seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

fn failure(message: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(message.into())
}

fn period() -> axiom_rules_engine::model::Period {
    serde_json::from_value::<PeriodSpec>(
        json!({"period_kind":"month","start":"2026-01-01","end":"2026-01-31"}),
    )
    .unwrap()
    .to_model()
    .unwrap()
}

fn compile(
    program: &axiom_rules_engine::spec::ProgramSpec,
) -> Result<CompiledProgramArtifact, TestCaseError> {
    CompiledProgramArtifact::compile(program.clone())
        .map_err(|error| failure(format!("generated program refused: {error}")))
}

fn set(names: impl IntoIterator<Item = impl Into<String>>) -> BTreeSet<String> {
    names.into_iter().map(Into::into).collect()
}

fn relation_keys(program: &DenseCompiledProgram) -> BTreeSet<String> {
    program
        .relations()
        .iter()
        .map(|relation| {
            format!(
                "{}/{}/{}",
                relation.key.name, relation.key.current_slot, relation.key.related_slot
            )
        })
        .collect()
}

/// A column of `count` values of `template`'s type, chosen by `seed` and the
/// cell: integers 1 to 3 (the parameter tables' keys and more), Booleans,
/// texts and dates that differ between neighbouring cells and slots.
fn varied_column(template: &ScalarValueSpec, seed: usize, count: usize) -> DenseColumn {
    let pick = |cell: usize| (seed + cell) % 3;
    match template {
        ScalarValueSpec::Bool { .. } => {
            DenseColumn::Bool((0..count).map(|c| pick(c) != 1).collect())
        }
        ScalarValueSpec::Integer { .. } => {
            DenseColumn::Integer((0..count).map(|c| 1 + pick(c) as i64).collect())
        }
        ScalarValueSpec::Decimal { .. } => DenseColumn::Decimal(
            (0..count)
                .map(|c| Decimal::new(5 + 10 * pick(c) as i64, 1))
                .collect(),
        ),
        ScalarValueSpec::Text { .. } => DenseColumn::Text(
            (0..count)
                .map(|c| Arc::from(["a", "b", "c"][pick(c)]))
                .collect(),
        ),
        ScalarValueSpec::Date { value } => DenseColumn::Date(
            (0..count)
                .map(|c| *value + chrono::Days::new(pick(c) as u64 * 20))
                .collect(),
        ),
    }
}

/// Two root rows whose every slot holds its own values: row 0 has two
/// members and row 1 one, and each links one household.
fn varied_batch(g: &Generated) -> DenseBatchSpec {
    let columns = |count: usize, offset: usize| {
        g.values
            .iter()
            .enumerate()
            .map(|(seed, (name, value))| (name.clone(), varied_column(value, seed + offset, count)))
            .collect::<HashMap<_, _>>()
    };
    DenseBatchSpec {
        row_count: 2,
        inputs: columns(2, 0),
        relations: HashMap::from([
            (
                DenseRelationKey {
                    name: "members".into(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: vec![0, 2, 3],
                    inputs: columns(3, 1),
                    ..Default::default()
                },
            ),
            (
                DenseRelationKey {
                    name: "links".into(),
                    current_slot: 0,
                    related_slot: 1,
                },
                DenseRelationBatchSpec {
                    offsets: vec![0, 1, 2],
                    inputs: columns(2, 2),
                    ..Default::default()
                },
            ),
        ]),
    }
}

/// `program`'s execution of `output` alone on the generator's batch and on
/// [`varied_batch`], as text: the values, or the error.
fn executed(program: &DenseCompiledProgram, g: &Generated, output: &str) -> [String; 2] {
    let outputs = [output.to_string()];
    [g.dense_batch(), varied_batch(g)]
        .map(|batch| format!("{:?}", program.execute(&period(), batch, &outputs)))
}

#[derive(Default)]
struct Health {
    programs: u32,
    with_crossing: u32,
    single_closures: u32,
    proper_single_closures: u32,
    executed_values: u32,
    varied_values: u32,
}

#[test]
fn requested_closures_agree_with_the_whole_root_on_generated_programs() {
    let health = std::cell::RefCell::new(Health::default());
    runner(128, 166)
        .run(&proptest::collection::vec(any::<u8>(), 0..100), |choices| {
            let g = generate(&choices, false);
            let artifact = compile(&g.program)?;
            let rules = g
                .program
                .derived
                .iter()
                .filter(|rule| rule.entity == ROOT)
                .map(|rule| rule.name.clone())
                .collect::<Vec<String>>();
            let has_crossing = rules.iter().any(|rule| rule == CROSSING);
            let mut h = health.borrow_mut();
            h.programs += 1;

            // 1. The whole root refuses the crossing; without it, it compiles.
            let mut without = g.program.clone();
            without.derived.retain(|rule| rule.name != CROSSING);
            let whole = DenseCompiledProgram::from_artifact(&compile(&without)?, Some(ROOT))
                .map_err(|error| {
                    failure(format!(
                        "the whole root without {CROSSING} refused: {error}"
                    ))
                })?;
            let whole_with_crossing = DenseCompiledProgram::from_artifact(&artifact, Some(ROOT));
            if has_crossing {
                h.with_crossing += 1;
                let refusal = whole_with_crossing
                    .err()
                    .ok_or_else(|| failure(format!("the whole root compiled {CROSSING}")))?;
                // 2. Requested alone, the crossing refuses with the same error.
                let requested = DenseCompiledProgram::from_artifact_for_outputs(
                    &artifact,
                    Some(ROOT),
                    &[CROSSING.to_string()],
                )
                .err()
                .ok_or_else(|| failure(format!("the requested closure compiled {CROSSING}")))?;
                prop_assert_eq!(requested.to_string(), refusal.to_string());
                match &requested {
                    DenseCompileError::CrossEntityDependency {
                        derived,
                        dependency,
                        entity,
                        reader_entity,
                        relations,
                    } => {
                        prop_assert_eq!(derived, CROSSING);
                        prop_assert_eq!(dependency, "person_value");
                        prop_assert_eq!(entity, "Person");
                        prop_assert_eq!(reader_entity, ROOT);
                        prop_assert_eq!(relations, &["members"]);
                    }
                    other => {
                        return Err(failure(format!(
                            "expected a cross-entity refusal, got {other}"
                        )));
                    }
                }
            } else {
                prop_assert!(whole_with_crossing.is_ok());
            }

            // 3. Each other rule's requested closure.
            let whole_outputs = set(whole.output_names());
            let whole_inputs = set(whole.root_inputs().iter().cloned());
            let whole_relations = relation_keys(&whole);
            let others = rules
                .iter()
                .filter(|rule| *rule != CROSSING)
                .cloned()
                .collect::<Vec<String>>();
            for rule in &others {
                let closure = DenseCompiledProgram::from_artifact_for_outputs(
                    &artifact,
                    Some(ROOT),
                    std::slice::from_ref(rule),
                )
                .map_err(|error| {
                    failure(format!(
                        "the requested closure of `{rule}` refused: {error}"
                    ))
                })?;
                h.single_closures += 1;
                let outputs = set(closure.output_names());
                prop_assert!(outputs.contains(rule));
                prop_assert!(
                    outputs.is_subset(&whole_outputs),
                    "{outputs:?} not in {whole_outputs:?}"
                );
                prop_assert!(set(closure.root_inputs().iter().cloned()).is_subset(&whole_inputs));
                prop_assert!(relation_keys(&closure).is_subset(&whole_relations));
                if outputs.len() < whole_outputs.len() {
                    h.proper_single_closures += 1;
                }
                let [result, varied] = executed(&closure, &g, rule);
                if result.starts_with("Ok") {
                    h.executed_values += 1;
                }
                if varied.starts_with("Ok") {
                    h.varied_values += 1;
                }
                prop_assert_eq!(
                    [result, varied],
                    executed(&whole, &g, rule),
                    "rule `{}`",
                    rule
                );
            }

            // 4. The closure of every rule but the crossing is the whole root.
            let all =
                DenseCompiledProgram::from_artifact_for_outputs(&artifact, Some(ROOT), &others)
                    .map_err(|error| {
                        failure(format!(
                            "the requested closure of every rule refused: {error}"
                        ))
                    })?;
            prop_assert_eq!(set(all.output_names()), whole_outputs);
            prop_assert_eq!(set(all.root_inputs().iter().cloned()), whole_inputs);
            prop_assert_eq!(relation_keys(&all), whole_relations);
            for output in all.output_names() {
                prop_assert_eq!(executed(&all, &g, &output), executed(&whole, &g, &output));
            }
            Ok(())
        })
        .unwrap();
    let h = health.borrow();
    eprintln!(
        "requested closures: {} programs, {} with the crossing; {} single-rule closures, {} smaller than the whole root, {} executed to values on the generator's batch and {} on the varied one",
        h.programs,
        h.with_crossing,
        h.single_closures,
        h.proper_single_closures,
        h.executed_values,
        h.varied_values
    );
    assert!(
        h.with_crossing >= 32 && h.programs - h.with_crossing >= 32,
        "the run must cover programs with and without the crossing"
    );
    assert!(
        h.proper_single_closures >= h.single_closures / 2,
        "most single-rule closures must leave rules out"
    );
    assert!(
        h.executed_values >= h.single_closures / 4 && h.varied_values >= h.single_closures / 4,
        "a useful share of single-rule closures must execute to values on both batches"
    );
}
