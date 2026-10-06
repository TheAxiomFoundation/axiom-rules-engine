//! Verification probe (dense-generic-numeric-type-text): dense's
//! "expected decimal-compatible dense column" vs explain's distinct
//! type-mismatch messages for arithmetic, comparison and related sum.
//! Prints explain / fast (with fallback metadata) / dense Decimal / dense f64.

use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::model::{Period, PeriodKind};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};

fn month_period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn interval() -> IntervalSpec {
    let p = month_period();
    IntervalSpec {
        start: p.start,
        end: p.end,
    }
}

fn fmt_output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar { value, .. } => format!("{value:?}"),
        OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
    }
}

fn module(name: &str, entity: &str, dtype: &str, formula: &str, extra: &str) -> String {
    format!(
        r#"
format: rulespec/v1
rules:
{extra}
  - name: {name}
    kind: derived
    entity: {entity}
    dtype: {dtype}
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: '{formula}'
"#
    )
}

fn run_api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    output: &str,
    entity_id: &str,
    dataset: DatasetSpec,
) -> String {
    let result = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset,
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: entity_id.to_string(),
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

fn run_dense(
    dense: &DenseCompiledProgram,
    output: &str,
    make_batch: &dyn Fn() -> DenseBatchSpec,
    f64_mode: bool,
) -> String {
    let period = month_period().to_model().expect("period converts");
    let outputs = [output.to_string()];
    let result = if f64_mode {
        dense.execute_f64(&period, make_batch(), &outputs)
    } else {
        dense.execute(&period, make_batch(), &outputs)
    };
    match result {
        Ok(result) => format!("OK {:?}", result.outputs.get(output)),
        Err(error) => format!("ERR {error}"),
    }
}

struct Case {
    label: &'static str,
    rulespec: String,
    entity: &'static str,
    output: &'static str,
    query_id: &'static str,
    dataset: DatasetSpec,
    batch: Box<dyn Fn() -> DenseBatchSpec>,
}

fn household_case(
    label: &'static str,
    dtype: &str,
    formula: &str,
    inputs: Vec<(&'static str, ScalarValueSpec, DenseColumn)>,
) -> Case {
    let dataset = DatasetSpec {
        inputs: inputs
            .iter()
            .map(|(name, value, _)| InputRecordSpec {
                name: (*name).to_string(),
                entity: "Household".to_string(),
                entity_id: "household-1".to_string(),
                interval: interval(),
                value: value.clone(),
            })
            .collect(),
        relations: vec![],
    };
    let columns: Vec<(String, DenseColumn)> = inputs
        .into_iter()
        .map(|(name, _, column)| (name.to_string(), column))
        .collect();
    Case {
        label,
        rulespec: module("out", "Household", dtype, formula, ""),
        entity: "Household",
        output: "out",
        query_id: "household-1",
        dataset,
        batch: Box::new(move || DenseBatchSpec {
            row_count: 1,
            inputs: columns.iter().cloned().collect(),
            relations: HashMap::new(),
        }),
    }
}

fn family_case(
    label: &'static str,
    formula: &str,
    extra: &str,
    child_input: &'static str,
    child_value: ScalarValueSpec,
    child_column: DenseColumn,
) -> Case {
    let mut dataset = DatasetSpec::default();
    dataset.inputs.push(InputRecordSpec {
        name: child_input.to_string(),
        entity: "Person".to_string(),
        entity_id: "child-1".to_string(),
        interval: interval(),
        value: child_value,
    });
    dataset.relations.push(RelationRecordSpec {
        name: "member_of_family".to_string(),
        tuple: vec!["child-1".to_string(), "family-1".to_string()],
        interval: interval(),
    });
    let extra_rules = format!(
        r#"  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
{extra}"#
    );
    Case {
        label,
        rulespec: module("out", "Family", "Money", formula, &extra_rules),
        entity: "Family",
        output: "out",
        query_id: "family-1",
        dataset,
        batch: Box::new(move || DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: HashMap::from([(
                DenseRelationKey {
                    name: "member_of_family".to_string(),
                    current_slot: 1,
                    related_slot: 0,
                },
                DenseRelationBatchSpec {
                    offsets: vec![0, 1],
                    inputs: HashMap::from([(child_input.to_string(), child_column.clone())]),
                },
            )]),
        }),
    }
}

fn date(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).expect("date")
}

#[test]
fn verify_dense_generic_numeric_type_text() {
    let cases = vec![
        household_case(
            "A root arithmetic: flag + 1 (flag=Bool true)",
            "Money",
            "flag + 1",
            vec![(
                "flag",
                ScalarValueSpec::Bool { value: true },
                DenseColumn::Bool(vec![true]),
            )],
        ),
        household_case(
            "B root ceil: ceil(s) (s=Text abc)",
            "Money",
            "ceil(s)",
            vec![(
                "s",
                ScalarValueSpec::Text {
                    value: "abc".to_string(),
                },
                DenseColumn::Text(vec!["abc".to_string()]),
            )],
        ),
        household_case(
            "C root max: max(flag, 1) (flag=Bool true)",
            "Money",
            "max(flag, 1)",
            vec![(
                "flag",
                ScalarValueSpec::Bool { value: true },
                DenseColumn::Bool(vec![true]),
            )],
        ),
        household_case(
            "D judgment bare integer input: n (n=Integer 1) -> n == true",
            "Judgment",
            "n",
            vec![(
                "n",
                ScalarValueSpec::Integer { value: 1 },
                DenseColumn::Integer(vec![1]),
            )],
        ),
        household_case(
            "E judgment compare: dt > 0 (dt=Date)",
            "Judgment",
            "dt > 0",
            vec![(
                "dt",
                ScalarValueSpec::Date {
                    value: date(2026, 1, 15),
                },
                DenseColumn::Date(vec![date(2026, 1, 15)]),
            )],
        ),
        household_case(
            "F judgment compare: s == period_start (s=Text)",
            "Judgment",
            "s == period_start",
            vec![(
                "s",
                ScalarValueSpec::Text {
                    value: "abc".to_string(),
                },
                DenseColumn::Text(vec!["abc".to_string()]),
            )],
        ),
        family_case(
            "G related sum of Text input: sum(member_of_family.child_amount)",
            "sum(member_of_family.child_amount)",
            "",
            "child_amount",
            ScalarValueSpec::Text {
                value: "abc".to_string(),
            },
            DenseColumn::Text(vec!["abc".to_string()]),
        ),
        family_case(
            "H related executor arithmetic: sum(member_of_family.child_calc), child_calc = child_flag + 1",
            "sum(member_of_family.child_calc)",
            r#"  - name: child_calc
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: 'child_flag + 1'
"#,
            "child_flag",
            ScalarValueSpec::Bool { value: true },
            DenseColumn::Bool(vec![true]),
        ),
    ];

    for case in &cases {
        println!("=== {} ===", case.label);
        let artifact = match CompiledProgramArtifact::from_rulespec_str(&case.rulespec) {
            Ok(artifact) => artifact,
            Err(error) => {
                println!("  COMPILE ERR {error}");
                continue;
            }
        };
        let explain = run_api(
            ExecutionMode::Explain,
            &artifact,
            case.output,
            case.query_id,
            case.dataset.clone(),
        );
        let fast = run_api(
            ExecutionMode::Fast,
            &artifact,
            case.output,
            case.query_id,
            case.dataset.clone(),
        );
        println!("  explain      : {explain}");
        println!("  fast         : {fast}");
        match DenseCompiledProgram::from_artifact(&artifact, Some(case.entity)) {
            Ok(dense) => {
                println!(
                    "  dense decimal: {}",
                    run_dense(&dense, case.output, case.batch.as_ref(), false)
                );
                println!(
                    "  dense f64    : {}",
                    run_dense(&dense, case.output, case.batch.as_ref(), true)
                );
            }
            Err(error) => println!("  DENSE COMPILE ERR {error}"),
        }
    }

    // Lifetime executor (no explain equivalent for over-periods reductions;
    // printed for completeness of the dense message across executors).
    println!("=== I lifetime: sum_over_periods(flag + 1) (flag=Bool true) ===");
    let rulespec = module("out", "Household", "Money", "sum_over_periods(flag + 1)", "");
    match CompiledProgramArtifact::from_rulespec_str(&rulespec) {
        Ok(artifact) => match DenseCompiledProgram::from_artifact(&artifact, Some("Household")) {
            Ok(dense) => {
                let periods = vec![
                    Period {
                        kind: PeriodKind::TaxYear,
                        start: date(2026, 1, 1),
                        end: date(2026, 12, 31),
                    },
                    Period {
                        kind: PeriodKind::TaxYear,
                        start: date(2027, 1, 1),
                        end: date(2027, 12, 31),
                    },
                ];
                let mk = || DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::from([("flag".to_string(), DenseColumn::Bool(vec![true]))]),
                    relations: HashMap::new(),
                };
                let outputs = ["out".to_string()];
                let dec = dense.execute_lifetime(&periods, vec![mk(), mk()], &outputs);
                let f = dense.execute_lifetime_f64(&periods, vec![mk(), mk()], &outputs);
                println!(
                    "  dense lifetime decimal: {}",
                    match dec {
                        Ok(r) => format!("OK {:?}", r.outputs.get("out")),
                        Err(e) => format!("ERR {e}"),
                    }
                );
                println!(
                    "  dense lifetime f64    : {}",
                    match f {
                        Ok(r) => format!("OK {:?}", r.outputs.get("out")),
                        Err(e) => format!("ERR {e}"),
                    }
                );
            }
            Err(error) => println!("  DENSE COMPILE ERR {error}"),
        },
        Err(error) => println!("  COMPILE ERR {error}"),
    }
}
