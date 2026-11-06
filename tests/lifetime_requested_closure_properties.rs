//! Lifetime API invariants for requested closures. All programs are synthetic.
//!
//! `artifact_interface_generator` supplies the unreached rules, including its
//! mixed entities, relation reads, versioned formulas and parameter tables.
//! Its whole root always reads relations and it has no lifetime reductions,
//! so the accepted-program differential uses the inline lifetime generator
//! below: a diamond of rule dependencies, versioned formulas and tables,
//! variable rows/periods, and arbitrary nonempty subsets of its public outputs.

#[allow(dead_code)]
#[path = "support/artifact_interface_generator.rs"]
mod generator;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    CalculationLifetimePlan, DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram,
    DenseExecutionResult, DenseOutputValue,
};
use axiom_rules_engine::lifetime_api::{
    LifetimeApiError, execute_lifetime_wire_request, parse_lifetime_wire_request,
};
use axiom_rules_engine::model::{Period, PeriodKind, SCALAR_ENTITY};
use axiom_rules_engine::relation_typing::filtered_entity_kinds;
use axiom_rules_engine::spec::{DerivedSpec, ProgramSpec};
use proptest::prelude::*;
use rust_decimal::Decimal;
use serde_json::{Value, json};

const ROOT: &str = "Household";
const MODULE: &str = "us:statutes/99/lifetime-properties";
const PUBLIC_OUTPUTS: [&str; 3] = ["history", "positive_total", "combined"];

#[derive(Clone, Debug)]
struct Case {
    start: i32,
    rows: usize,
    periods: usize,
    amounts: Vec<i64>,
    coefficient: i64,
    outputs: Vec<String>,
}

fn cases() -> impl Strategy<Value = Case> {
    (
        2000..=2004_i32,
        1..=3_usize,
        1..=4_usize,
        1..=5_i64,
        1..=7_u8,
    )
        .prop_flat_map(|(start, rows, periods, coefficient, mask)| {
            proptest::collection::vec(-30..=50_i64, rows * periods).prop_map(move |amounts| Case {
                start,
                rows,
                periods,
                amounts,
                coefficient,
                outputs: PUBLIC_OUTPUTS
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & (1 << bit) != 0)
                    .map(|(_, name)| (*name).to_string())
                    .collect(),
            })
        })
}

fn year(year: i32) -> Value {
    json!({"period_kind":"tax_year", "start":format!("{year}-01-01"), "end":format!("{year}-12-31")})
}

fn period(year: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: format!("{year}-01-01").parse().unwrap(),
        end: format!("{year}-12-31").parse().unwrap(),
    }
}

fn public(name: &str) -> String {
    format!("{MODULE}#{name}")
}

fn artifact(program: ProgramSpec) -> CompiledProgramArtifact {
    CompiledProgramArtifact::compile(program).expect("the inline generated program is valid")
}

fn lifetime_program(case: &Case, calculation: bool) -> ProgramSpec {
    let source = format!(
        "format: rulespec/v1\nrules:\n\
         \x20 - name: factor\n    kind: parameter\n    dtype: Integer\n    indexed_by: k\n    versions:\n      - effective_from: '1990-01-01'\n        values:\n          1: {}\n          2: {}\n      - effective_from: '2003-01-01'\n        values:\n          1: {}\n          2: {}\n\
         \x20 - name: annual\n    kind: derived\n    entity: Household\n    dtype: Money\n    versions:\n      - effective_from: '1990-01-01'\n        formula: 'amount * factor[k] + bias'\n      - effective_from: '2003-01-01'\n        formula: 'amount * factor[k] - bias'\n\
         \x20 - name: history\n    kind: derived\n    entity: Household\n    dtype: Money\n    effective_from: '1990-01-01'\n    formula: sum_over_periods(annual)\n\
         \x20 - name: positive_total\n    kind: derived\n    entity: Household\n    dtype: Money\n    effective_from: '1990-01-01'\n    formula: sum_over_periods(max(annual, 0))\n\
         \x20 - name: combined\n    kind: derived\n    entity: Household\n    dtype: Money\n    effective_from: '1990-01-01'\n    formula: history + positive_total\n\
         \x20 - name: supported_extra\n    kind: derived\n    entity: Household\n    dtype: Money\n    effective_from: '1990-01-01'\n    formula: sum_over_periods(extra)\n",
        case.coefficient,
        case.coefficient + 1,
        case.coefficient + 2,
        case.coefficient + 3,
    );
    let mut program = CompiledProgramArtifact::from_rulespec_str(&source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"))
        .program;
    // Legacy dense deliberately refuses multi-version formulas. Keep its
    // accepted profile unbounded; fixed-law mode covers formula versioning.
    if !calculation {
        let annual = program
            .derived
            .iter_mut()
            .find(|rule| rule.name == "annual")
            .unwrap();
        annual.versions.truncate(1);
        annual.versions[0].effective_to = None;
        annual.semantics = annual.versions[0].semantics.clone();
    }
    for rule in &mut program.derived {
        rule.id = Some(public(&rule.name));
    }
    program
}

fn values(case: &Case, index: usize) -> BTreeMap<String, Vec<i64>> {
    BTreeMap::from([
        (
            "amount".into(),
            (0..case.rows)
                .map(|row| case.amounts[index * case.rows + row])
                .collect(),
        ),
        (
            "bias".into(),
            (0..case.rows).map(|row| (row + index + 1) as i64).collect(),
        ),
        (
            "k".into(),
            (0..case.rows).map(|row| 1 + (row % 2) as i64).collect(),
        ),
        // This input is read only by an unrequested rule. It was admitted by
        // the whole-root API and must remain accepted by the closure API.
        (
            "extra".into(),
            (0..case.rows)
                .map(|row| 100 + (row + index) as i64)
                .collect(),
        ),
    ])
}

fn request(case: &Case, calculation: bool) -> Value {
    let calculation_year = case.start + case.periods as i32 + 2;
    let batches = (0..case.periods)
        .map(|index| {
            let inputs = values(case, index)
                .into_iter()
                .map(|(slot, values)| {
                    (public(&format!("input.{slot}")), json!({"kind":"integer", "values":values}))
                })
                .collect::<BTreeMap<_, _>>();
            json!({
                "row_count":case.rows,
                "entity_ids":(0..case.rows).map(|row| format!("household:{row}")).collect::<Vec<_>>(),
                "inputs":inputs
            })
        })
        .collect::<Vec<_>>();
    let mut request = json!({
        "schema":if calculation {"axiom-rules-engine/lifetime-request/v2"} else {"axiom-rules-engine/lifetime-request/v1"},
        "entity":ROOT,
        "periods":(0..case.periods).map(|index| year(case.start + index as i32)).collect::<Vec<_>>(),
        "batches":batches,
        "outputs":case.outputs.iter().map(|name| public(name)).collect::<Vec<_>>(),
        "output_period":year(if calculation {calculation_year} else {case.start + case.periods as i32 - 1})
    });
    if calculation {
        request["calculation_period"] = year(calculation_year);
    }
    request
}

fn run(
    artifact: &CompiledProgramArtifact,
    case: &Case,
    calculation: bool,
) -> Result<Value, LifetimeApiError> {
    let request = parse_lifetime_wire_request(&request(case, calculation).to_string())?;
    Ok(serde_json::to_value(execute_lifetime_wire_request(artifact.clone(), request)?).unwrap())
}

fn dense_batches(case: &Case) -> Vec<DenseBatchSpec> {
    (0..case.periods)
        .map(|index| DenseBatchSpec {
            row_count: case.rows,
            inputs: values(case, index)
                .into_iter()
                .map(|(slot, values)| (slot, DenseColumn::Integer(values)))
                .collect::<HashMap<_, _>>(),
            relations: HashMap::new(),
        })
        .collect()
}

fn whole_values(
    artifact: &CompiledProgramArtifact,
    case: &Case,
    calculation: bool,
) -> DenseExecutionResult {
    let periods = (0..case.periods)
        .map(|index| period(case.start + index as i32))
        .collect::<Vec<_>>();
    if calculation {
        CalculationLifetimePlan::from_artifact(
            artifact,
            ROOT,
            period(case.start + case.periods as i32 + 2),
        )
        .expect("the whole-program calculation plan accepts the generated program")
        .execute(&periods, dense_batches(case), &case.outputs)
        .expect("the whole-program calculation plan executes")
    } else {
        DenseCompiledProgram::from_artifact(artifact, Some(ROOT))
            .expect("the whole-program compiler accepts the generated program")
            .execute_lifetime(&periods, dense_batches(case), &case.outputs)
            .expect("the whole-program lifetime path executes")
    }
}

fn decimal_values(result: &DenseExecutionResult, name: &str) -> Vec<String> {
    match &result.outputs[name] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values
            .iter()
            .map(|value| value.normalize().to_string())
            .collect(),
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values
            .iter()
            .map(|value| Decimal::from(*value).normalize().to_string())
            .collect(),
        other => panic!("generated numeric output {name}: {other:?}"),
    }
}

fn rule(name: &str, entity: &str, expr: Value) -> DerivedSpec {
    serde_json::from_value(json!({
        "name":name, "id":public(name), "entity":entity, "dtype":"decimal", "semantics":"scalar", "expr":expr
    }))
    .unwrap()
}

fn add_refusal(program: &mut ProgramSpec, relation: bool) {
    let expr = if relation {
        program.relations.push(
            serde_json::from_value(json!({
                "name":"members", "arity":2, "slot_entities":["Person","Household"]
            }))
            .unwrap(),
        );
        json!({"kind":"count_related", "relation":"members", "current_slot":1, "related_slot":0})
    } else {
        program
            .derived
            .push(rule("foreign", "Person", generator::int(7)));
        json!({"kind":"derived", "name":"foreign"})
    };
    program.derived.push(rule("refused", ROOT, expr));
    program.derived.push(rule(
        "via_refusal",
        ROOT,
        generator::add(json!({"kind":"derived", "name":"refused"})),
    ));
}

/// Include the API wrapper's variant as well as its complete display text.
fn signature(error: LifetimeApiError) -> (&'static str, &'static str, String) {
    let dense_variant = |error: &DenseCompileError| match error {
        DenseCompileError::CrossEntityDependency { .. } => "CrossEntityDependency",
        DenseCompileError::Unsupported(_) => "Unsupported",
        other => panic!("unexpected dense refusal variant: {other:?}"),
    };
    let variant = match &error {
        LifetimeApiError::Compile(error) => ("Compile", dense_variant(error)),
        LifetimeApiError::Calculation(error) => ("Calculation", dense_variant(error)),
        LifetimeApiError::Unsupported(_) => ("Unsupported", "Unsupported"),
        other => panic!("unexpected refusal variant: {other:?}"),
    };
    (variant.0, variant.1, error.to_string())
}

fn whole_refusal(
    artifact: &CompiledProgramArtifact,
    case: &Case,
    calculation: bool,
) -> LifetimeApiError {
    if calculation {
        LifetimeApiError::Calculation(
            CalculationLifetimePlan::from_artifact(
                artifact,
                ROOT,
                period(case.start + case.periods as i32 + 2),
            )
            .expect_err("the old calculation API refused the reached unsupported rule"),
        )
    } else {
        match DenseCompiledProgram::from_artifact(artifact, Some(ROOT)) {
            Err(error) => LifetimeApiError::Compile(error),
            Ok(dense) => {
                assert!(!dense.relations().is_empty());
                LifetimeApiError::Unsupported(
                    "relation context is not supported by lifetime JSON v1".into(),
                )
            }
        }
    }
}

/// Extend the interface generator with a Scalar-only input and an independent
/// lifetime output. Vary the unrelated alias's ID kind, including no alias.
fn admission_program(choices: &[u8]) -> ProgramSpec {
    let mut program = generator::generate(choices, false).program;
    // Only Scalar inputs enter the wire request below. Keep the other
    // generated inputs' module ownership and election declarations intact.
    for derived in program
        .derived
        .iter_mut()
        .filter(|derived| derived.entity == SCALAR_ENTITY)
    {
        derived.id = Some(public(&derived.name));
    }
    for declaration in &mut program.input_declarations {
        if declaration.input == "scalar_amount" {
            declaration.input = public("input.scalar_amount");
        }
    }
    program
        .input_declarations
        .sort_by(|left, right| left.input.cmp(&right.input));
    program.derived.push(rule(
        "scalar_extra",
        SCALAR_ENTITY,
        json!({"kind":"input", "name":"scalar_extra"}),
    ));
    program.derived.push(rule(
        "scalar_total",
        SCALAR_ENTITY,
        json!({"kind":"over_periods", "over":"sum", "value":generator::int(7)}),
    ));
    if choices.first().copied().unwrap_or(0) % 3 != 0 {
        let current_slot = choices.first().copied().unwrap_or(0) % 2;
        program.relations.push(
            serde_json::from_value(json!({
                "name":"scalar_alias", "arity":2,
                "derivation":{
                    "source_relation":"members", "entity":SCALAR_ENTITY,
                    "slot_entities":["Person", "Household"],
                    "current_slot":current_slot, "related_slot":1-current_slot,
                    "predicate":generator::cmp(generator::int(1), "eq", generator::int(1))
                }
            }))
            .unwrap(),
        );
    }
    program
}

/// Each input a whole-root compile reads, by entity, and whether the interface
/// lists that input for the entity's kind.
fn whole_root_inputs(artifact: &CompiledProgramArtifact) -> Vec<(String, String, bool)> {
    let interface = artifact.interface.as_ref().unwrap();
    let model = artifact.program.to_program().unwrap();
    let kinds = filtered_entity_kinds(&model);
    let entities = artifact
        .program
        .derived
        .iter()
        .map(|rule| rule.entity.clone())
        .collect::<BTreeSet<_>>();
    let mut read = Vec::new();
    for entity in entities {
        let Ok(dense) = DenseCompiledProgram::from_artifact(artifact, Some(&entity)) else {
            continue;
        };
        let kind = if entity == SCALAR_ENTITY {
            &entity
        } else {
            kinds.get(&entity).unwrap_or(&entity)
        };
        for input in dense.root_inputs() {
            let listed = interface
                .inputs
                .iter()
                .any(|slot| slot.name == *input && slot.entities.contains(kind));
            read.push((entity.clone(), input.clone(), listed));
        }
    }
    read
}

#[test]
fn the_admission_property_reads_real_whole_roots() {
    let mut original_reads = 0;
    let reads = (0..=255_u8)
        .map(|seed| {
            let choices = (0..80_u8)
                .map(|index| {
                    seed.wrapping_mul(31)
                        .wrapping_add(index.wrapping_mul(seed | 1))
                })
                .collect::<Vec<_>>();
            original_reads +=
                whole_root_inputs(&artifact(generator::generate(&choices, false).program)).len();
            whole_root_inputs(&artifact(admission_program(&choices))).len()
        })
        .sum::<usize>();
    // The original corpus has 2,227 reads; losing one guaranteed read per
    // seed drops it to 1,971. scalar_extra adds 256 to the extended corpus.
    // Both floors detect that loss, including accidentally skipping Scalar.
    assert!(
        original_reads >= 2_200,
        "original whole-root input coverage shrank: {original_reads}"
    );
    assert!(reads >= 2_400, "whole-root input coverage shrank: {reads}");
    eprintln!(
        "whole-root input reads across 256 seeds: original={original_reads} (floor 2200), extended={reads} (floor 2400)"
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn unreached_rules_change_no_lifetime_outputs(
        case in cases(),
        choices in proptest::collection::vec(any::<u8>(), 0..80),
    ) {
        for calculation in [false, true] {
            let program = lifetime_program(&case, calculation);
            let baseline = artifact(program.clone());
            let noise = generator::generate(&choices, false).program;
            let mut with_noise = program;
            with_noise.derived.extend(noise.derived);
            with_noise.parameters.extend(noise.parameters);
            with_noise.relations.extend(noise.relations);
            with_noise.input_declarations.extend(noise.input_declarations);
            // Always include both reported unsupported shapes; the reused generator
            // independently varies the additional supported and unsupported rules.
            let mut crossing = ProgramSpec::default();
            add_refusal(&mut crossing, false);
            with_noise.derived.extend(crossing.derived);
            let with_noise = artifact(with_noise);
            let before = run(&baseline, &case, calculation).expect("the baseline executes");
            let after = run(&with_noise, &case, calculation).expect("unreached rules cannot refuse");
            prop_assert_eq!(&after["outputs"], &before["outputs"]);
        }
    }

    #[test]
    fn reached_refusals_keep_the_old_variant_and_complete_message(case in cases()) {
        for relation in [false, true] {
            for transitive in [false, true] {
                let mut reached = case.clone();
                reached.outputs = vec![if transitive {"via_refusal"} else {"refused"}.into()];
                for calculation in [false, true] {
                    let mut program = lifetime_program(&case, calculation);
                    add_refusal(&mut program, relation);
                    let artifact = artifact(program);
                    let expected = whole_refusal(&artifact, &reached, calculation);
                    let actual = run(&artifact, &reached, calculation)
                        .expect_err("a requested output reaches the unsupported rule");
                    prop_assert_eq!(signature(actual), signature(expected));
                }
            }
        }
    }

    #[test]
    fn requested_lifetime_outputs_agree_with_accepted_whole_programs(case in cases()) {
        for calculation in [false, true] {
            let artifact = artifact(lifetime_program(&case, calculation));
            let whole = whole_values(&artifact, &case, calculation);
            let requested = run(&artifact, &case, calculation)
                .expect("the same previously accepted request still executes");
            for name in &case.outputs {
                prop_assert_eq!(
                    &requested["outputs"][public(name)]["column"]["values"],
                    &json!(decimal_values(&whole, name)),
                    "output {} with calculation={}", name, calculation,
                );
            }
        }
    }

    #[test]
    fn every_input_a_whole_root_reads_stays_admitted(
        choices in proptest::collection::vec(any::<u8>(), 0..80),
    ) {
        let artifact = artifact(admission_program(&choices));
        let reads = whole_root_inputs(&artifact);
        for (entity, input, listed) in &reads {
            prop_assert!(*listed, "{} reads {} outside the interface", entity, input);
        }
        // Check production admission too: a helper that preserves Scalar alone
        // must not hide a regression in execute_lifetime_wire_request.
        let interface = artifact.interface.as_ref().unwrap();
        let inputs = reads.iter()
            .filter(|(entity, _, _)| entity == SCALAR_ENTITY)
            .map(|(_, input, _)| {
                let slot = interface.inputs.iter().find(|slot| slot.name == *input).unwrap();
                (slot.legal_id.clone(), json!({"kind":"integer", "values":[1]}))
            }).collect::<BTreeMap<_, _>>();
        prop_assert!(inputs.len() >= 2, "Scalar whole root must read both inputs");
        for calculation in [false, true] {
            let batch = json!({"row_count":1, "entity_ids":["scalar"], "inputs":inputs});
            let mut body = json!({
                "schema":if calculation {"axiom-rules-engine/lifetime-request/v2"} else {"axiom-rules-engine/lifetime-request/v1"},
                "entity":SCALAR_ENTITY, "periods":[year(2001),year(2002)],
                "batches":[batch,batch], "outputs":[public("scalar_total")],
                "output_period":year(if calculation {2026} else {2002})
            });
            if calculation {
                body["calculation_period"] = year(2026);
            }
            let request = parse_lifetime_wire_request(&body.to_string()).unwrap();
            let response = execute_lifetime_wire_request(artifact.clone(), request);
            prop_assert!(response.is_ok(), "Scalar whole-root columns, calculation={}: {:?}", calculation, response);
            let response = serde_json::to_value(response.unwrap()).unwrap();
            prop_assert_eq!(&response["outputs"][public("scalar_total")]["column"]["values"], &json!(["14"]));
        }
    }

    #[test]
    fn repeated_lifetime_requests_are_deterministic(case in cases()) {
        for calculation in [false, true] {
            let mut program = lifetime_program(&case, calculation);
            add_refusal(&mut program, false);
            let artifact = artifact(program);
            let first = run(&artifact, &case, calculation).expect("the generated request executes");
            let second = run(&artifact, &case, calculation).expect("the repeated request executes");
            prop_assert_eq!(second, first);
        }
    }
}
