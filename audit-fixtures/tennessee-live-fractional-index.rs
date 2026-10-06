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

const TARGET: &str = "us-tn:regulations/1240-01/04/27/block-1";
const INPUT: &str = "household_member_count";
const OUTPUT: &str = "snap_standard_utility_allowance_state_value";

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().unwrap(),
        end: "2026-01-31".parse().unwrap(),
    }
}

fn request(mode: ExecutionMode, artifact: &CompiledProgramArtifact) -> ExecutionRequest {
    let period = period();
    ExecutionRequest {
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![InputRecordSpec {
                name: format!("{TARGET}#input.{INPUT}"),
                entity: "Household".into(),
                entity_id: "household-1".into(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Decimal {
                    value: "1.5".into(),
                },
            }],
            relations: vec![],
        },
        queries: vec![ExecutionQuery {
            entity_id: "household-1".into(),
            period,
            outputs: vec![format!("{TARGET}#{OUTPUT}")],
            assessment_date: None,
        }],
    }
}

fn output(value: &OutputValue) -> String {
    match value {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => value.to_string(),
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => value.clone(),
        other => panic!("unexpected output: {other:?}"),
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: tennessee-live-fractional-index <published RuleSpec path>");
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(&std::fs::read_to_string(path).unwrap())
            .unwrap();

    let explain = execute_request(request(ExecutionMode::Explain, &artifact));
    println!("explain={explain:?}");

    let fast = execute_request(request(ExecutionMode::Fast, &artifact)).unwrap();
    let fast_output = &fast.results[0].outputs[&format!("{TARGET}#{OUTPUT}")];
    println!(
        "fast actual={:?} value={}",
        fast.metadata.actual_mode,
        output(fast_output)
    );

    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household")).unwrap();
    let inputs = dense
        .root_inputs()
        .iter()
        .map(|name| {
            let value = if name == INPUT {
                "1.5".parse().unwrap()
            } else {
                "0".parse().unwrap()
            };
            (name.clone(), DenseColumn::Decimal(vec![value]))
        })
        .collect::<HashMap<_, _>>();
    let batch = DenseBatchSpec {
        row_count: 1,
        inputs,
        relations: HashMap::new(),
    };
    let exact = dense.execute(
        &period().to_model().unwrap(),
        batch.clone(),
        &[OUTPUT.into()],
    );
    match exact {
        Ok(result) => match &result.outputs[OUTPUT] {
            DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
                println!("dense_decimal value={}", values[0])
            }
            DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                println!("dense_decimal value={}", values[0])
            }
            other => panic!("unexpected dense output: {other:?}"),
        },
        Err(error) => println!("dense_decimal error={error}"),
    }
    let float = dense.execute_f64(&period().to_model().unwrap(), batch, &[OUTPUT.into()]);
    println!("dense_f64={float:?}");
}
