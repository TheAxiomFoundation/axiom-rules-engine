use std::collections::HashMap;

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
};

const OUTPUT: &str = "pension_credit_guarantee_council_tax_reduction";

fn scalar(name: &str) -> Option<&'static str> {
    match name {
        "applicant_capital" => Some("0"),
        "council_tax_liability" => Some("1.005"),
        "maximum_council_tax_reduction" => Some("0"),
        "excess_income_over_allowed_living_expenses" => Some("0"),
        _ => None,
    }
}

fn boolean(name: &str) -> bool {
    name == "pensioner_receives_state_pension_credit_guarantee_part"
}

fn api(
    mode: ExecutionMode,
    artifact: &CompiledProgramArtifact,
    names: &[String],
    period: &PeriodSpec,
) -> (ExecutionMode, String) {
    let inputs = names
        .iter()
        .map(|name| InputRecordSpec {
            name: name.clone(),
            entity: "Household".into(),
            entity_id: "h1".into(),
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
            value: match scalar(name) {
                Some(value) => ScalarValueSpec::Decimal {
                    value: value.into(),
                },
                None => ScalarValueSpec::Bool {
                    value: boolean(name),
                },
            },
        })
        .collect();
    let response = execute_request(ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs,
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            entity_id: "h1".into(),
            period: period.clone(),
            outputs: vec![OUTPUT.into()],
            assessment_date: None,
        }],
    })
    .unwrap();
    let value = match &response.results[0].outputs[OUTPUT] {
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => value.clone(),
        other => panic!("unexpected {other:?}"),
    };
    (response.metadata.actual_mode, value)
}

fn batch(names: &[String]) -> DenseBatchSpec {
    DenseBatchSpec {
        row_count: 1,
        inputs: names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    match scalar(name) {
                        Some(value) => DenseColumn::Decimal(vec![value.parse().unwrap()]),
                        None => DenseColumn::Bool(vec![boolean(name)]),
                    },
                )
            })
            .collect::<HashMap<_, _>>(),
        relations: HashMap::new(),
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bristol-live-rounding <published RuleSpec path>");
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(&std::fs::read_to_string(path).unwrap())
            .unwrap();
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household")).unwrap();
    let names = dense.root_inputs();
    let period = PeriodSpec {
        kind: PeriodKindSpec::TaxYear,
        start: "2026-04-01".parse().unwrap(),
        end: "2027-03-31".parse().unwrap(),
    };

    let (mode, value) = api(ExecutionMode::Explain, &artifact, names, &period);
    println!("explain actual={mode:?} value={value}");
    let (mode, value) = api(ExecutionMode::Fast, &artifact, names, &period);
    println!("fast actual={mode:?} value={value}");

    let exact = dense
        .execute(&period.to_model().unwrap(), batch(names), &[OUTPUT.into()])
        .unwrap();
    let float = dense
        .execute_f64(&period.to_model().unwrap(), batch(names), &[OUTPUT.into()])
        .unwrap();
    match &exact.outputs[OUTPUT] {
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
            println!("dense_decimal value={}", values[0])
        }
        other => panic!("unexpected {other:?}"),
    }
    match &float.outputs[OUTPUT] {
        DenseOutputValue::Scalar(DenseColumn::Float(values)) => {
            println!("dense_f64 value={:?}", values[0])
        }
        other => panic!("unexpected {other:?}"),
    }
}
