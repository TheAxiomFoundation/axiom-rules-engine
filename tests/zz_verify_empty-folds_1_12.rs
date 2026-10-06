//! Verification probe (candidate offlens-relation-member-where-clause):
//! a `relation_member` judgment used (a) directly in a count_related /
//! sum_related where-clause, where explain has no relation context and is
//! claimed to error "relation predicate `r` can only be evaluated inside a
//! derived relation", and (b) in a derived-relation predicate that names a
//! relation OTHER than the source relation, where explain checks membership
//! of the named relation. Dense compile_related_predicate is claimed to map
//! every RelationMember to Literal(true); bulk eval_related_judgment_expr is
//! claimed to evaluate relation_contains even with no derived-relation
//! context. Prints explain, fast (with fallback metadata) and dense results.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ProgramSpec,
    RelationRecordSpec, ScalarValueSpec,
};
use rust_decimal::Decimal;

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let period = month_period();
    IntervalSpec {
        start: period.start,
        end: period.end,
    }
}

fn program() -> ProgramSpec {
    let rel_member_excluded = serde_json::json!({
        "kind": "relation_member",
        "relation": "excluded",
        "current_slot": 1,
        "related_slot": 0
    });
    let rel_member_source = serde_json::json!({
        "kind": "relation_member",
        "relation": "member_of_household",
        "current_slot": 1,
        "related_slot": 0
    });
    let not_rel_member_excluded = serde_json::json!({
        "kind": "not",
        "item": rel_member_excluded.clone()
    });
    serde_json::from_value(serde_json::json!({
        "relations": [
            {
                "name": "member_of_household",
                "arity": 2,
                "slot_entities": ["Person", "Household"]
            },
            {
                "name": "excluded",
                "arity": 2,
                "slot_entities": ["Person", "Household"]
            },
            {
                "name": "excluded_members",
                "arity": 2,
                "slot_entities": ["Person", "Household"],
                "derivation": {
                    "source_relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "slot_entities": ["Person", "Household"],
                    "predicate": rel_member_excluded.clone()
                }
            },
            {
                "name": "kept_members",
                "arity": 2,
                "slot_entities": ["Person", "Household"],
                "derivation": {
                    "source_relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "slot_entities": ["Person", "Household"],
                    "predicate": not_rel_member_excluded
                }
            },
            {
                "name": "source_members",
                "arity": 2,
                "slot_entities": ["Person", "Household"],
                "derivation": {
                    "source_relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "slot_entities": ["Person", "Household"],
                    "predicate": rel_member_source
                }
            }
        ],
        "derived": [
            {
                "name": "count_all_control",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0
                }
            },
            {
                "name": "count_where_relation_member",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": rel_member_excluded.clone()
                }
            },
            {
                "name": "sum_where_relation_member",
                "entity": "Household",
                "dtype": "decimal",
                "semantics": "scalar",
                "expr": {
                    "kind": "sum_related",
                    "relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "value": {"kind": "input", "name": "amount"},
                    "where": rel_member_excluded.clone()
                }
            },
            {
                "name": "count_derived_excluded_members",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "excluded_members",
                    "current_slot": 1,
                    "related_slot": 0
                }
            },
            {
                "name": "count_derived_kept_members",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "kept_members",
                    "current_slot": 1,
                    "related_slot": 0
                }
            },
            {
                "name": "count_derived_source_members_control",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "source_members",
                    "current_slot": 1,
                    "related_slot": 0
                }
            }
        ]
    }))
    .expect("ProgramSpec parses")
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn person_amount(person: &str, amount: &str) -> InputRecordSpec {
    InputRecordSpec {
        name: "amount".to_string(),
        entity: "Person".to_string(),
        entity_id: person.to_string(),
        interval: interval(),
        value: ScalarValueSpec::Decimal {
            value: amount.to_string(),
        },
    }
}

fn relation(name: &str, person: &str, household: &str) -> RelationRecordSpec {
    RelationRecordSpec {
        name: name.to_string(),
        tuple: vec![person.to_string(), household.to_string()],
        interval: interval(),
    }
}

fn dataset() -> DatasetSpec {
    DatasetSpec {
        inputs: vec![person_amount("p1", "5"), person_amount("p2", "7")],
        relations: vec![
            relation("member_of_household", "p1", "h1"),
            relation("member_of_household", "p2", "h1"),
            relation("excluded", "p1", "h1"),
        ],
    }
}

fn run_api(mode: ExecutionMode, artifact: &CompiledProgramArtifact, output: &str) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: dataset(),
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "h1".to_string(),
            period: month_period(),
            outputs: vec![output.to_string()],
        }],
    });
    match result {
        Ok(response) => {
            let value = response.results[0]
                .outputs
                .get(output)
                .map(fmt_output)
                .unwrap_or_else(|| "<missing>".to_string());
            format!(
                "OK {value} | actual_mode={:?} fallback_reason={:?}",
                response.metadata.actual_mode, response.metadata.fallback_reason
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn run_dense(dense: &DenseCompiledProgram, output: &str, with_excluded_batch: bool) -> String {
    let period = month_period().to_model().expect("period converts");
    let mut relations = HashMap::from([(
        DenseRelationKey {
            name: "member_of_household".to_string(),
            current_slot: 1,
            related_slot: 0,
        },
        DenseRelationBatchSpec {
            offsets: vec![0, 2],
            inputs: HashMap::from([(
                "amount".to_string(),
                DenseColumn::Decimal(vec![Decimal::from(5), Decimal::from(7)]),
            )]),
        },
    )]);
    if with_excluded_batch {
        relations.insert(
            DenseRelationKey {
                name: "excluded".to_string(),
                current_slot: 1,
                related_slot: 0,
            },
            DenseRelationBatchSpec {
                offsets: vec![0, 1],
                inputs: HashMap::new(),
            },
        );
    }
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs: HashMap::new(),
        relations,
    };
    let outputs = [output.to_string()];
    match dense.execute(&period, batch, &outputs) {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

#[test]
fn verify_offlens_relation_member_where_clause() {
    let artifact = match CompiledProgramArtifact::compile(program()) {
        Ok(artifact) => artifact,
        Err(error) => {
            println!("ARTIFACT-COMPILE-ERR {error}");
            return;
        }
    };
    println!("artifact compiles OK");
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"));
    if let Err(error) = &dense {
        println!("DENSE-COMPILE-ERR {error}");
    }
    let mut dense_divergences = 0;
    let mut fast_divergences = 0;
    for output in [
        "count_all_control",
        "count_where_relation_member",
        "sum_where_relation_member",
        "count_derived_excluded_members",
        "count_derived_kept_members",
        "count_derived_source_members_control",
    ] {
        let explain = run_api(ExecutionMode::Explain, &artifact, output);
        let fast = run_api(ExecutionMode::Fast, &artifact, output);
        let (dense_plain, dense_with_excluded) = match &dense {
            Ok(dense) => (run_dense(dense, output, false), run_dense(dense, output, true)),
            Err(error) => (
                format!("DENSE-COMPILE-ERR {error}"),
                format!("DENSE-COMPILE-ERR {error}"),
            ),
        };
        println!("[{output}] explain: {explain}");
        println!("[{output}] fast:    {fast}");
        println!("[{output}] dense:   {dense_plain}");
        println!("[{output}] dense (+excluded batch): {dense_with_excluded}");

        let explain_value = explain
            .strip_prefix("OK ")
            .map(|rest| rest.split(" | ").next().unwrap_or("").to_string());
        let fast_value = if fast.contains("actual_mode=Fast") {
            fast.strip_prefix("OK ")
                .map(|rest| rest.split(" | ").next().unwrap_or("").to_string())
        } else {
            None
        };
        if explain.starts_with("ERR") && dense_plain.starts_with("OK") {
            dense_divergences += 1;
            println!("[{output}] DENSE DIVERGENCE: explain errors, dense returns a value");
        }
        if let Some(explain_value) = &explain_value {
            if dense_plain.starts_with("OK") {
                println!("[{output}] compare explain value {explain_value} vs dense {dense_plain}");
            }
        }
        if explain.starts_with("ERR") && fast_value.is_some() {
            fast_divergences += 1;
            println!("[{output}] FAST DIVERGENCE: explain errors, fast returns without fallback");
        }
        if let (Some(e), Some(f)) = (&explain_value, &fast_value) {
            if e != f {
                fast_divergences += 1;
                println!("[{output}] FAST VALUE DIVERGENCE: explain {e} vs fast {f}");
            }
        }
    }
    println!("dense error->value divergences: {dense_divergences}");
    println!("fast divergences: {fast_divergences}");
}
