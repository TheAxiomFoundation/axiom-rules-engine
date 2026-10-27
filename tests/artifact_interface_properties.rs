//! Independent construction records test the interface's precision; evaluator
//! probes test its soundness. Choice sequences have ordinary proptest shrinking.

#[path = "support/artifact_interface_generator.rs"]
mod generator;

use axiom_rules_engine::api::{
    ApiError, CompiledExecutionRequest, ExecutionMode, ExecutionQuery, ExecutionResponse,
    OutputValue, RelationBinding, execute_compiled_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{DenseCompileError, DenseCompiledProgram};
use axiom_rules_engine::engine::{Engine, EvalError};
use axiom_rules_engine::interface::{
    EntityInstances, InputDType, InterfaceInput, RelationProvenance,
};
use axiom_rules_engine::model::{DataSet, DerivedSemantics, Program};
use axiom_rules_engine::spec::{DatasetSpec, JudgmentOutcomeSpec, PeriodSpec, ScalarValueSpec};
use generator::{Generated, generate, generate_for_soundness};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

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
fn period() -> PeriodSpec {
    serde_json::from_value(json!({"period_kind":"month","start":"2026-01-01","end":"2026-01-31"}))
        .unwrap()
}
fn query(g: &Generated, rule: &str) -> ExecutionQuery {
    ExecutionQuery {
        entity_id: g.query_id(rule),
        period: period(),
        outputs: vec![rule.into()],
        assessment_date: None,
    }
}
fn execute(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    dataset: DatasetSpec,
    query: ExecutionQuery,
) -> Result<ExecutionResponse, ApiError> {
    execute_queries(artifact, mode, dataset, vec![query])
}
fn execute_queries(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    dataset: DatasetSpec,
    queries: Vec<ExecutionQuery>,
) -> Result<ExecutionResponse, ApiError> {
    execute_compiled_request(
        artifact.clone(),
        CompiledExecutionRequest {
            relation_binding: RelationBinding::Strict,
            mode,
            dataset,
            queries,
            pins: vec![],
        },
    )
}
fn failure(message: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(message.into())
}
fn compile(g: &Generated) -> Result<CompiledProgramArtifact, TestCaseError> {
    CompiledProgramArtifact::compile(g.program.clone()).map_err(|e| {
        failure(format!(
            "generated program refused: {e}\n{}",
            serde_json::to_string_pretty(&g.program).unwrap()
        ))
    })
}
fn outcome(response: &Result<ExecutionResponse, ApiError>) -> Value {
    match response {
        Ok(response) => json!(
            response
                .results
                .iter()
                .map(|r| &r.outputs)
                .collect::<Vec<_>>()
        ),
        Err(error) => json!({"error":error.to_string()}),
    }
}
fn legacy(artifact: &CompiledProgramArtifact) -> Result<CompiledProgramArtifact, TestCaseError> {
    let mut value = serde_json::to_value(artifact).unwrap();
    value["artifact_format_version"] = json!(2);
    value.as_object_mut().unwrap().remove("engine_build");
    value.as_object_mut().unwrap().remove("source_closure");
    value["program"]
        .as_object_mut()
        .unwrap()
        .remove("interface");
    // Elections did not exist in format 2. Execution does not read them.
    value["program"]
        .as_object_mut()
        .unwrap()
        .remove("input_declarations");
    CompiledProgramArtifact::from_legacy_format_2_json_str(&value.to_string())
        .map_err(|e| failure(e.to_string()))
}

#[derive(Default, Debug)]
struct Health {
    started: Option<Instant>,
    cases: usize,
    compiled: usize,
    queries: usize,
    values: usize,
    fast_values: usize,
    fast_batches: usize,
    fast_batch_fallbacks: usize,
    dense_compiled: usize,
    dense_refused: usize,
    dense_cross_entity: usize,
    dense_values: usize,
    missing_probes: usize,
    typed_probes: usize,
    domain_probes: usize,
    reached_typed: BTreeMap<String, usize>,
    reached_invalid_kinds: BTreeMap<String, BTreeSet<String>>,
    reached_domain: usize,
    /// Second-instance probes whose cell a query read as a related member.
    reached_members: usize,
    observed_kinds: BTreeSet<String>,
    shapes: BTreeSet<u8>,
    elections: usize,
}
impl Health {
    fn new() -> Self {
        Self {
            started: Some(Instant::now()),
            ..Self::default()
        }
    }
    fn compiled(&mut self, g: &Generated) {
        self.cases += 1;
        self.compiled += 1;
        self.shapes.extend(&g.shapes);
        self.elections += g.program.input_declarations.len();
    }
    fn report(&self, label: &str, expected_cases: usize) {
        eprintln!(
            "interface generator {label}: elapsed={:.3}s {self:?}",
            self.started.unwrap().elapsed().as_secs_f64()
        );
        assert_eq!(
            self.compiled, self.cases,
            "every generated program must compile"
        );
        assert_eq!(self.cases, expected_cases, "every configured case must run");
    }
}

/// Invariant 2(a): compile, serialize, load, serialize is byte-identical for
/// every generated format-3 program, including elections and versions.
#[test]
fn generated_format_3_round_trips_byte_identically() {
    let health = RefCell::new(Health::new());
    runner(64, 155)
        .run(&proptest::collection::vec(any::<u8>(), 0..100), |choices| {
            let g = generate(&choices, false);
            let artifact = compile(&g)?;
            health.borrow_mut().compiled(&g);
            let bytes =
                serde_json::to_string_pretty(&artifact).map_err(|e| failure(e.to_string()))?;
            let loaded = CompiledProgramArtifact::from_json_str(&bytes)
                .map_err(|e| failure(e.to_string()))?;
            prop_assert_eq!(bytes, serde_json::to_string_pretty(&loaded).unwrap());
            Ok(())
        })
        .unwrap();
    health.borrow().report("round trip", 64);
}

/// Invariant 3: catalog names, slots, dtypes, defaults, parameter domains,
/// elections and evaluation-flow kinds agree with the independently recorded
/// construction sites; relation declarations and the entity roster agree too.
#[test]
fn generated_interface_agrees_with_construction_records() {
    let health = RefCell::new(Health::new());
    runner(128, 156)
        .run(&proptest::collection::vec(any::<u8>(), 0..120), |choices| {
            let g = generate(&choices, false);
            let artifact = compile(&g)?;
            health.borrow_mut().compiled(&g);
            let interface = artifact.interface.as_ref().unwrap();
            let expected_inputs = g.expected_inputs();
            prop_assert_eq!(interface.inputs.len(), expected_inputs.len());
            for (actual, expected) in interface.inputs.iter().zip(&expected_inputs) {
                prop_assert_eq!(actual, expected);
            }
            let wire = serde_json::to_value(&artifact).unwrap();
            let catalog = wire["metadata"]["input_catalog"].as_array().unwrap();
            let listed = interface
                .inputs
                .iter()
                .map(|i| i.name.as_str())
                .collect::<BTreeSet<_>>();
            let catalog_slots = catalog
                .iter()
                .map(|i| i["slot"].as_str().unwrap())
                .collect::<BTreeSet<_>>();
            prop_assert!(listed.is_subset(&catalog_slots));
            for input in &interface.inputs {
                let entry = catalog.iter().find(|e| e["slot"] == input.name).unwrap();
                prop_assert_eq!(
                    &input.legal_id,
                    entry["canonical_request_name"].as_str().unwrap()
                );
            }
            prop_assert!(!listed.contains("catalog_only"));
            prop_assert!(!listed.contains("other_catalog_only"));
            let roster = interface
                .entities
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<BTreeSet<_>>();
            for input in &interface.inputs {
                for kind in &input.entities {
                    prop_assert!(roster.contains(kind.as_str()));
                }
            }
            prop_assert_eq!(interface.relations.len(), g.program.relations.len());
            for relation in &interface.relations {
                let declared = g
                    .program
                    .relations
                    .iter()
                    .find(|r| r.name == relation.name)
                    .unwrap();
                prop_assert_eq!(relation.arity, declared.arity);
                let slots = if relation.name.contains("links") {
                    vec!["Household".to_string(); 2]
                } else {
                    vec!["Person".to_string(), "Household".to_string()]
                };
                prop_assert_eq!(relation.slot_entities.as_ref(), Some(&slots));
                for kind in slots {
                    prop_assert!(roster.contains(kind.as_str()));
                }
                prop_assert!(relation.accepts_supplied_tuples);
                prop_assert_eq!(
                    &relation.lookup_slots,
                    &if relation.name.contains("links") {
                        vec![0]
                    } else {
                        vec![1]
                    }
                );
                match (&declared.derivation, &relation.provenance) {
                    (None, RelationProvenance::Supplied) => {}
                    (
                        Some(derivation),
                        RelationProvenance::Derived {
                            rule,
                            source_relation,
                            reads,
                        },
                    ) => {
                        prop_assert_eq!(rule, &relation.name);
                        prop_assert_eq!(source_relation, &derivation.source_relation);
                        let (inputs, rules, relations) = if relation.name == "selected_members" {
                            (
                                vec!["predicate_age"],
                                vec!["household_ok", "person_ok"],
                                vec!["members"],
                            )
                        } else {
                            (vec!["self_flag"], vec![], vec![])
                        };
                        prop_assert_eq!(
                            reads.inputs.iter().map(String::as_str).collect::<Vec<_>>(),
                            inputs
                        );
                        prop_assert_eq!(
                            reads.rules.iter().map(String::as_str).collect::<Vec<_>>(),
                            rules
                        );
                        prop_assert_eq!(
                            reads
                                .relations
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>(),
                            relations
                        );
                    }
                    other => prop_assert!(false, "relation provenance disagrees: {other:?}"),
                }
            }
            let selected = interface
                .entities
                .iter()
                .find(|e| e.kind == "Selected")
                .unwrap();
            prop_assert_eq!(
                &selected.instances,
                &EntityInstances::Alias {
                    of: Some("Household".into()),
                    via: vec!["selected_members".into()]
                }
            );
            prop_assert_eq!(selected.id_template.as_deref(), Some("Household:{index}"));
            let household = interface
                .entities
                .iter()
                .find(|e| e.kind == "Household")
                .unwrap();
            prop_assert_eq!(&household.instances, &EntityInstances::Supplied);
            CompiledProgramArtifact::from_json_str(
                &serde_json::to_string_pretty(&artifact).unwrap(),
            )
            .map_err(|e| failure(e.to_string()))?;
            Ok(())
        })
        .unwrap();
    let health = health.borrow();
    health.report("agreement", 128);
    assert!(
        health.elections >= 16,
        "election declarations need generated coverage"
    );
    assert_eq!(
        health.shapes,
        (0..30).collect(),
        "fixed seed must exercise every generated expression position"
    );
}

fn incompatible_values(dtype: InputDType) -> Vec<ScalarValueSpec> {
    [
        ScalarValueSpec::Bool { value: true },
        ScalarValueSpec::Integer { value: 1 },
        ScalarValueSpec::Decimal {
            value: "0.5".into(),
        },
        ScalarValueSpec::Text {
            value: "invalid".into(),
        },
        ScalarValueSpec::Date {
            value: "2026-01-01".parse().unwrap(),
        },
    ]
    .into_iter()
    .filter(|value| {
        !matches!(
            (dtype, value),
            (InputDType::Bool, ScalarValueSpec::Bool { .. })
                | (InputDType::Text, ScalarValueSpec::Text { .. })
                | (InputDType::Date, ScalarValueSpec::Date { .. })
                | (InputDType::Integer, ScalarValueSpec::Integer { .. })
                | (
                    InputDType::Decimal,
                    ScalarValueSpec::Integer { .. } | ScalarValueSpec::Decimal { .. }
                )
        )
    })
    .collect()
}

fn invalid_kind(value: &ScalarValueSpec) -> &'static str {
    match value {
        ScalarValueSpec::Bool { .. } => "bool",
        ScalarValueSpec::Integer { .. } => "integer",
        // Every decimal probe is fractional, including at whole-number sites.
        ScalarValueSpec::Decimal { .. } => "fractional_decimal",
        ScalarValueSpec::Text { .. } => "text",
        ScalarValueSpec::Date { .. } => "date",
    }
}

fn out_of_type(dtype: InputDType, rotation: usize) -> Vec<ScalarValueSpec> {
    let candidates = incompatible_values(dtype);
    (0..2)
        .map(|offset| candidates[(rotation + offset) % candidates.len()].clone())
        .collect()
}

type Observation = Result<Value, EvalError>;

fn observed(result: &Observation) -> Value {
    match result {
        Ok(value) => value.clone(),
        Err(error) => json!({"error":error.to_string()}),
    }
}

fn rejected(result: &Observation, domain: bool) -> bool {
    matches!(result, Err(EvalError::TypeMismatch(_)))
        || domain && matches!(result, Err(EvalError::MissingParameterValue { .. }))
}

fn check_sound_pair(
    before: &Observation,
    after: &Observation,
    slot: &str,
    domain: bool,
) -> Result<(), TestCaseError> {
    if matches!(before, Err(EvalError::MissingInput{name, ..}) if name == slot) {
        prop_assert!(
            rejected(after, domain),
            "reached `{slot}` accepted an invalid value: {}",
            observed(after)
        );
    } else if !rejected(after, domain) {
        prop_assert_eq!(
            observed(before),
            observed(after),
            "invalid `{}` changed evaluation",
            slot
        );
    }
    Ok(())
}

/// The entire syntactic query closure: all versions and lazy arms, rules
/// referenced by them, and predicates along every referenced relation chain.
/// No interface claim participates in deciding which other records to retain.
fn potential_slots(program: &Value, rule: &str) -> BTreeSet<String> {
    let rules = program["derived"].as_array().unwrap();
    let relations = program["relations"].as_array().unwrap();
    let mut seen_rules = BTreeSet::from([rule.to_string()]);
    let mut seen_relations = BTreeSet::new();
    let mut pending = vec![rules.iter().find(|entry| entry["name"] == rule).unwrap()];
    let mut slots = BTreeSet::new();
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                match object.get("kind").and_then(Value::as_str) {
                    Some("input" | "input_or_else") => {
                        slots.insert(object["name"].as_str().unwrap().to_string());
                    }
                    Some("derived") => {
                        let name = object["name"].as_str().unwrap();
                        if seen_rules.insert(name.to_string()) {
                            pending.push(rules.iter().find(|entry| entry["name"] == name).unwrap());
                        }
                    }
                    _ => {}
                }
                for key in ["relation", "source_relation"] {
                    if let Some(name) = object.get(key).and_then(Value::as_str)
                        && seen_relations.insert(name.to_string())
                    {
                        pending.push(
                            relations
                                .iter()
                                .find(|entry| entry["name"] == name)
                                .unwrap(),
                        );
                    }
                }
                pending.extend(object.values());
            }
            Value::Array(array) => pending.extend(array),
            _ => {}
        }
    }
    slots
}

fn pruned_dataset(full: &DatasetSpec, slots: &BTreeSet<String>) -> DatasetSpec {
    DatasetSpec {
        inputs: full
            .inputs
            .iter()
            .filter(|record| slots.contains(&record.name))
            .cloned()
            .collect(),
        relations: full.relations.clone(),
    }
}

fn explain_observation(program: &Program, data: &DataSet, query: &ExecutionQuery) -> Observation {
    let name = &query.outputs[0];
    let mut engine = Engine::new(program, data);
    let period = query.period.to_model().unwrap();
    match &program.derived[name].semantics {
        DerivedSemantics::Scalar(_) => engine
            .evaluate_scalar(name, &query.entity_id, &period)
            .map(|value| serde_json::to_value(ScalarValueSpec::from_model(value)).unwrap()),
        DerivedSemantics::Judgment(_) => engine
            .evaluate_judgment(name, &query.entity_id, &period)
            .map(|value| serde_json::to_value(JudgmentOutcomeSpec::from(value)).unwrap()),
    }
}

fn api_observation(
    result: Result<ExecutionResponse, ApiError>,
) -> Result<(Observation, bool), TestCaseError> {
    match result {
        Ok(response) => {
            let output = response.results[0].outputs.values().next().unwrap();
            let value = match output {
                OutputValue::Scalar { value, .. } => serde_json::to_value(value).unwrap(),
                OutputValue::Judgment { outcome, .. } => serde_json::to_value(outcome).unwrap(),
            };
            Ok((
                Ok(value),
                response.metadata.actual_mode == ExecutionMode::Fast,
            ))
        }
        Err(ApiError::Eval(error)) => Ok((Err(error), false)),
        Err(error) => Err(failure(format!("probe failed before evaluation: {error}"))),
    }
}

struct PreparedQuery {
    query: ExecutionQuery,
    kind: String,
    slots: BTreeSet<String>,
    data: DatasetSpec,
    bound: DataSet,
}

fn check_and_record_sound_pair(
    health: &RefCell<Health>,
    input: &InterfaceInput,
    bad: &ScalarValueSpec,
    domain: bool,
    before: &Observation,
    after: &Observation,
    actual_fast: bool,
) -> Result<(), TestCaseError> {
    let mut counts = health.borrow_mut();
    counts.queries += 1;
    if matches!(before, Err(EvalError::MissingInput { name, .. }) if name == &input.name) {
        if domain {
            counts.reached_domain += 1;
        } else {
            let dtype = format!("{:?}", input.dtype.unwrap());
            *counts.reached_typed.entry(dtype.clone()).or_default() += 1;
            counts
                .reached_invalid_kinds
                .entry(dtype)
                .or_default()
                .insert(invalid_kind(bad).to_string());
        }
    }
    if domain {
        counts.domain_probes += 1;
    } else {
        counts.typed_probes += 1;
    }
    if after.is_ok() {
        counts.values += 1;
        if actual_fast {
            counts.fast_values += 1;
        }
    }
    drop(counts);
    check_sound_pair(before, after, &input.name, domain)
}

/// A successful request contains every result, so every query was evaluated.
/// An error can stop explain fallback at the first query; callers then rerun
/// every member individually rather than treating the request error as theirs.
fn successful_batch_observations(
    result: Result<ExecutionResponse, ApiError>,
) -> Result<Option<Vec<(Observation, bool)>>, TestCaseError> {
    match result {
        Ok(response) => {
            let actual_fast = response.metadata.actual_mode == ExecutionMode::Fast;
            Ok(Some(
                response
                    .results
                    .into_iter()
                    .map(|result| {
                        let output = result.outputs.into_values().next().unwrap();
                        let value = match output {
                            OutputValue::Scalar { value, .. } => {
                                serde_json::to_value(value).unwrap()
                            }
                            OutputValue::Judgment { outcome, .. } => {
                                serde_json::to_value(outcome).unwrap()
                            }
                        };
                        (Ok(value), actual_fast)
                    })
                    .collect(),
            ))
        }
        Err(ApiError::Eval(_)) => Ok(None),
        Err(error) => Err(failure(format!("batch failed before evaluation: {error}"))),
    }
}

/// Invariant 6: an out-of-dtype input cannot be read successfully, and a
/// whole number outside a closed parameter domain cannot be read successfully.
/// Each invalid value replaces every cell of its slot, and separately only the
/// second instance of each kind, which a query reads only as a related member
/// beside a valid first instance. Every rule is probed with all potentially
/// read other inputs supplied. The
/// original program is used in both modes; unreachable records are pruned,
/// and each invalid target remains supplied even when the query cannot read it.
/// Successful unrelated rules are batched by entity kind using the union of
/// their independent closures. Every successful batch returns every result;
/// any batch error causes every member to be retried individually. Successful
/// fast responses additionally prove that path did not fall back.
/// Bulk deliberately falls back for every reached error; invalid-value error
/// responses therefore exercise that existing explain fallback.
#[test]
fn generated_dtype_and_closed_domains_are_sound() {
    let health = RefCell::new(Health::new());
    runner(16, 157)
        .run(&proptest::collection::vec(any::<u8>(), 0..80), |choices| {
            let g = generate_for_soundness(&choices);
            let artifact = compile(&g)?;
            let model = artifact
                .program
                .to_program()
                .map_err(|e| failure(e.to_string()))?;
            let source = serde_json::to_value(&g.program).unwrap();
            let full = g.dataset(None, None);
            let full_bound = full
                .to_dataset_for_program(&model)
                .map_err(|e| failure(e.to_string()))?;
            let mut prepared = Vec::new();
            for rule in &g.program.derived {
                let query = query(&g, &rule.name);
                let slots = potential_slots(&source, &rule.name);
                let data = pruned_dataset(&full, &slots);
                let bound = data
                    .to_dataset_for_queries(
                        &model,
                        &[(query.entity_id.clone(), rule.entity.clone())],
                    )
                    .map_err(|e| failure(e.to_string()))?;
                // The original full program remains the evaluator's program.
                // Pruning only unreachable records must preserve its baseline.
                prop_assert_eq!(
                    observed(&explain_observation(&model, &full_bound, &query)),
                    observed(&explain_observation(&model, &bound, &query))
                );
                prepared.push(PreparedQuery {
                    query,
                    kind: rule.entity.clone(),
                    slots,
                    data,
                    bound,
                });
            }
            health.borrow_mut().compiled(&g);
            let rotation = usize::from(choices.first().copied().unwrap_or(0));
            for input in &artifact.interface.as_ref().unwrap().inputs {
                let probes = input
                    .dtype
                    .map(|dtype| out_of_type(dtype, rotation))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|v| (v, false))
                    .chain(
                        input
                            .values
                            .as_ref()
                            .map(|_| (ScalarValueSpec::Integer { value: 999 }, true)),
                    );
                // Each value is probed in every cell of the slot, and in the
                // second instance of each kind alone. Queries run on first
                // instances, so the second reaches a query only as a related
                // member, beside first-instance members that stay valid: an
                // aggregate must reject one invalid member among valid ones.
                let probes = probes.flat_map(|(bad, domain)| {
                    [false, true].map(|second_only| (bad.clone(), domain, second_only))
                });
                for (bad, domain, second_only) in probes {
                    let target = |name: &str, entity_id: &str| {
                        name == input.name && (!second_only || entity_id.ends_with(":2"))
                    };
                    let bad_records = full
                        .inputs
                        .iter()
                        .filter(|record| target(&record.name, &record.entity_id))
                        .cloned()
                        .map(|mut record| {
                            record.value = bad.clone();
                            record
                        })
                        .collect::<Vec<_>>();
                    let bad_bound = DatasetSpec {
                        inputs: bad_records.clone(),
                        relations: vec![],
                    }
                    .to_dataset_for_program(&model)
                    .map_err(|e| failure(e.to_string()))?;
                    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                    for (index, prepared) in prepared.iter().enumerate() {
                        let mut absent_bound = prepared.bound.clone();
                        absent_bound
                            .inputs
                            .retain(|record| !target(&record.name, &record.entity_id));
                        let mut invalid_bound = absent_bound.clone();
                        // Retain the invalid target even outside the query closure:
                        // an erroneous eager read must remain observable.
                        invalid_bound
                            .inputs
                            .extend(bad_bound.inputs.iter().cloned());
                        let before = explain_observation(&model, &absent_bound, &prepared.query);
                        let after = explain_observation(&model, &invalid_bound, &prepared.query);
                        check_and_record_sound_pair(
                            &health, input, &bad, domain, &before, &after, false,
                        )?;
                        if second_only
                            && matches!(&before, Err(EvalError::MissingInput { name, .. }) if name == &input.name)
                        {
                            health.borrow_mut().reached_members += 1;
                        }
                        if !prepared.slots.contains(&input.name) && before.is_ok() {
                            groups.entry(prepared.kind.clone()).or_default().push(index);
                        } else {
                            let mut absent = prepared.data.clone();
                            absent
                                .inputs
                                .retain(|record| !target(&record.name, &record.entity_id));
                            let mut invalid = absent.clone();
                            invalid.inputs.extend(bad_records.iter().cloned());
                            let (before, _) = api_observation(execute(
                                &artifact,
                                ExecutionMode::Fast,
                                absent,
                                prepared.query.clone(),
                            ))?;
                            let (after, actual_fast) = api_observation(execute(
                                &artifact,
                                ExecutionMode::Fast,
                                invalid,
                                prepared.query.clone(),
                            ))?;
                            check_and_record_sound_pair(
                                &health,
                                input,
                                &bad,
                                domain,
                                &before,
                                &after,
                                actual_fast,
                            )?;
                        }
                    }
                    for members in groups.into_values() {
                        let slots = members
                            .iter()
                            .flat_map(|&index| prepared[index].slots.iter().cloned())
                            .collect();
                        let absent = pruned_dataset(&full, &slots);
                        let mut invalid = absent.clone();
                        // Include all invalid target cells even though every member
                        // independently excludes that slot from its potential reads.
                        invalid.inputs.extend(bad_records.iter().cloned());
                        let queries = members
                            .iter()
                            .map(|&index| prepared[index].query.clone())
                            .collect::<Vec<_>>();
                        health.borrow_mut().fast_batches += 1;
                        let before = successful_batch_observations(execute_queries(
                            &artifact,
                            ExecutionMode::Fast,
                            absent.clone(),
                            queries.clone(),
                        ))?;
                        let after = successful_batch_observations(execute_queries(
                            &artifact,
                            ExecutionMode::Fast,
                            invalid.clone(),
                            queries,
                        ))?;
                        match (before, after) {
                            (Some(before), Some(after)) => {
                                prop_assert_eq!(before.len(), members.len());
                                prop_assert_eq!(after.len(), members.len());
                                for ((before, _), (after, actual_fast)) in
                                    before.into_iter().zip(after)
                                {
                                    check_and_record_sound_pair(
                                        &health,
                                        input,
                                        &bad,
                                        domain,
                                        &before,
                                        &after,
                                        actual_fast,
                                    )?;
                                }
                            }
                            _ => {
                                health.borrow_mut().fast_batch_fallbacks += 1;
                                // A request error identifies only its first failing
                                // query. Evaluate every query to preserve coverage.
                                for index in members {
                                    let query = prepared[index].query.clone();
                                    let (before, _) = api_observation(execute(
                                        &artifact,
                                        ExecutionMode::Fast,
                                        absent.clone(),
                                        query.clone(),
                                    ))?;
                                    let (after, actual_fast) = api_observation(execute(
                                        &artifact,
                                        ExecutionMode::Fast,
                                        invalid.clone(),
                                        query,
                                    ))?;
                                    check_and_record_sound_pair(
                                        &health,
                                        input,
                                        &bad,
                                        domain,
                                        &before,
                                        &after,
                                        actual_fast,
                                    )?;
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    let health = health.borrow();
    health.report("dtype and domains", 16);
    assert!(
        health.reached_domain >= 8,
        "closed-domain target reads must be reached"
    );
    assert!(
        health.reached_members >= 8,
        "an invalid member beside valid ones must be reached"
    );
    for dtype in [
        InputDType::Bool,
        InputDType::Text,
        InputDType::Date,
        InputDType::Integer,
        InputDType::Decimal,
    ] {
        let name = format!("{dtype:?}");
        assert!(
            health.reached_typed.get(&name).copied().unwrap_or_default() >= 8,
            "target reads must be reached for {dtype:?}: {:?}",
            health.reached_typed
        );
        let expected = incompatible_values(dtype)
            .iter()
            .map(|value| invalid_kind(value).to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            health.reached_invalid_kinds.get(&name),
            Some(&expected),
            "every incompatible scalar kind must reach a {dtype:?} site"
        );
    }
    assert!(
        health.typed_probes > 1000 && health.domain_probes > 100,
        "typed and domain probes must reach the evaluator"
    );
    assert!(
        health.values > health.queries / 3,
        "unrelated successful rules must remain observable"
    );
    assert!(
        health.fast_values > 100,
        "fast execution must succeed without fallback"
    );
}

/// Invariant 7: every missing input reported by explain is read on an id
/// whose kind the interface lists. The id map comes from generated datasets;
/// repeated missing-input probes fill only the evaluator's reported cell.
#[test]
fn generated_entities_cover_every_observed_input_read() {
    let health = RefCell::new(Health::new());
    runner(64, 158)
        .run(&proptest::collection::vec(any::<u8>(), 0..100), |choices| {
            let g = generate(&choices, true);
            let artifact = compile(&g)?;
            health.borrow_mut().compiled(&g);
            let complete_dataset = g.dataset(None, None);
            for rule in &g.program.derived {
                let mut dataset = complete_dataset.clone();
                dataset.inputs.clear();
                let mut visited = BTreeSet::new();
                loop {
                    health.borrow_mut().queries += 1;
                    match execute(
                        &artifact,
                        ExecutionMode::Explain,
                        dataset.clone(),
                        query(&g, &rule.name),
                    ) {
                        Ok(_) => {
                            health.borrow_mut().values += 1;
                            break;
                        }
                        Err(ApiError::Eval(EvalError::MissingInput {
                            name, entity_id, ..
                        })) => {
                            let kind = g.ids.get(&entity_id).ok_or_else(|| {
                                failure(format!("unknown generated id {entity_id}"))
                            })?;
                            let input = artifact
                                .interface
                                .as_ref()
                                .unwrap()
                                .inputs
                                .iter()
                                .find(|i| i.name == name)
                                .ok_or_else(|| failure(format!("unlisted read {name}")))?;
                            prop_assert!(
                                input.entities.contains(kind),
                                "`{name}` read on {entity_id} ({kind}) absent from {:?}",
                                input.entities
                            );
                            prop_assert!(
                                visited.insert((name.clone(), entity_id.clone())),
                                "filling an input must advance evaluation"
                            );
                            health.borrow_mut().missing_probes += 1;
                            health.borrow_mut().observed_kinds.insert(kind.clone());
                            let record = complete_dataset
                                .inputs
                                .iter()
                                .cloned()
                                .find(|r| r.name == name && r.entity_id == entity_id)
                                .unwrap();
                            dataset.inputs.push(record);
                        }
                        // Mixed demands and ordered Boolean comparisons can fail
                        // even after every reached missing input has been supplied.
                        Err(ApiError::Eval(EvalError::TypeMismatch(_))) => break,
                        Err(error) => {
                            return Err(failure(format!("unexpected probe error: {error}")));
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    let health = health.borrow();
    health.report("entities", 64);
    for kind in ["Household", "Person", "Scalar"] {
        assert!(
            health.observed_kinds.contains(kind),
            "missing-input probes must observe {kind}"
        );
    }
    assert!(
        health.missing_probes > health.compiled * 10,
        "relation predicates and both kinds must surface reads"
    );
    assert!(
        health.values > health.compiled * 8,
        "typing must leave many evaluable rules"
    );
}

/// Invariant 8: format 2 through its named legacy loader and format 3 have
/// identical outcomes within explain, fast and dense. Dense either refuses
/// both with the same error or compiles both and executes identical batches.
#[test]
fn generated_formats_execute_identically_in_each_mode() {
    let health = RefCell::new(Health::new());
    runner(32, 159)
        .run(&proptest::collection::vec(any::<u8>(), 0..100), |choices| {
            let g = generate(&choices, false);
            let artifact = compile(&g)?;
            let old = legacy(&artifact)?;
            health.borrow_mut().compiled(&g);
            let full = g.dataset(None, None);
            let source = serde_json::to_value(&g.program).unwrap();
            for rule in &g.program.derived {
                let dataset = pruned_dataset(&full, &potential_slots(&source, &rule.name));
                for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
                    let a = execute(
                        &artifact,
                        mode.clone(),
                        dataset.clone(),
                        query(&g, &rule.name),
                    );
                    let b = execute(&old, mode, dataset.clone(), query(&g, &rule.name));
                    health.borrow_mut().queries += 1;
                    prop_assert_eq!(outcome(&a), outcome(&b));
                    if let (Ok(a), Ok(b)) = (&a, &b) {
                        prop_assert_eq!(
                            serde_json::to_value(a).unwrap(),
                            serde_json::to_value(b).unwrap()
                        );
                        let mut h = health.borrow_mut();
                        h.values += 1;
                        if a.metadata.actual_mode == ExecutionMode::Fast {
                            h.fast_values += 1;
                        }
                    }
                }
            }
            match (
                DenseCompiledProgram::from_artifact(&artifact, Some("Household")),
                DenseCompiledProgram::from_artifact(&old, Some("Household")),
            ) {
                (Err(a), Err(b)) => {
                    health.borrow_mut().dense_refused += 1;
                    if matches!(&a, DenseCompileError::CrossEntityDependency { .. }) {
                        health.borrow_mut().dense_cross_entity += 1;
                    }
                    prop_assert_eq!(a.to_string(), b.to_string());
                }
                (Ok(a), Ok(b)) => {
                    health.borrow_mut().dense_compiled += 1;
                    for rule in g.program.derived.iter().filter(|r| r.entity == "Household") {
                        let outputs = vec![rule.name.clone()];
                        let a = a.execute(&period().to_model().unwrap(), g.dense_batch(), &outputs);
                        let b = b.execute(&period().to_model().unwrap(), g.dense_batch(), &outputs);
                        if a.is_ok() {
                            health.borrow_mut().dense_values += 1;
                        }
                        prop_assert_eq!(format!("{a:?}"), format!("{b:?}"));
                    }
                }
                _ => return Err(failure("only one format compiled for dense")),
            }
            Ok(())
        })
        .unwrap();
    let h = health.borrow();
    h.report("execution parity", 32);
    assert!(
        h.dense_refused >= 8 && h.dense_cross_entity >= 8,
        "dense must also exercise matching cross-entity refusals"
    );
    assert!(
        h.values > h.compiled * 12,
        "a useful share of queries must return values"
    );
    assert!(h.fast_values > 100, "fast must execute without fallback");
    assert!(
        h.dense_compiled >= 8 && h.dense_values >= 16,
        "dense parity needs actual execution"
    );
}
