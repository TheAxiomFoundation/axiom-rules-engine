//! Explain mode reports the parameters a calculation applied
//! (`docs/execution-semantics.md`, "Parameters in the trace"): each traced
//! rule lists the parameter cells it read in `parameter_reads`, a scalar
//! parameter and the resolved row of a table alike, at the query's own
//! instance and at member instances.

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, QueryResult, execute_request,
};
use axiom_rules_engine::spec::{
    DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec,
    ScalarValueSpec,
};
use serde_json::{Value, json};

const MODULE: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: standard_deduction
    kind: parameter
    dtype: Money
    unit: USD
    source: 7 CFR 273.9(d)(1)
    versions:
      - effective_from: 2024-10-01
        formula: 204
      - effective_from: 2025-10-01
        formula: 209
  - name: maximum_allotment
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2024-10-01
        values:
          1: 292
          2: 536
          3: 768
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
          3: 785
  - name: earned_income_deduction_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2024-10-01
        formula: 0.2
  - name: member_earned_income_deduction
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2024-10-01
        formula: earned_income * earned_income_deduction_rate
  - name: household_earned_income_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2024-10-01
        formula: sum(member_of_household.member_earned_income_deduction)
  - name: allotment_ceiling
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2024-10-01
        formula: maximum_allotment[household_size]
  - name: income_after_standard_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2024-10-01
        formula: gross_income - standard_deduction
"#;

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    }
}

/// A three-person household, queried in January 2026 (the second version of
/// each two-version parameter is in force).
fn explain(outputs: &[&str]) -> QueryResult {
    let period = period();
    let interval = IntervalSpec {
        start: period.start,
        end: period.end,
    };
    let input =
        |name: &str, entity: &str, entity_id: &str, value: ScalarValueSpec| InputRecordSpec {
            name: name.to_string(),
            entity: entity.to_string(),
            entity_id: entity_id.to_string(),
            interval: interval.clone(),
            value,
        };
    let decimal = |value: &str| ScalarValueSpec::Decimal {
        value: value.to_string(),
    };
    let member = |id: &str| RelationRecordSpec {
        name: "member_of_household".to_string(),
        tuple: vec![id.to_string(), "household-1".to_string()],
        interval: interval.clone(),
    };
    let program =
        axiom_rules_engine::rulespec::lower_rulespec_str(MODULE).expect("the module lowers");
    let mut response = execute_request(ExecutionRequest {
        relation_binding: Default::default(),
        mode: ExecutionMode::Explain,
        program,
        dataset: DatasetSpec {
            inputs: vec![
                input(
                    "household_size",
                    "Household",
                    "household-1",
                    ScalarValueSpec::Integer { value: 3 },
                ),
                input("gross_income", "Household", "household-1", decimal("1500")),
                input("earned_income", "Person", "person-1", decimal("1000")),
                input("earned_income", "Person", "person-2", decimal("500")),
            ],
            relations: vec![member("person-1"), member("person-2")],
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period,
            outputs: outputs.iter().map(|output| output.to_string()).collect(),
        }],
    })
    .expect("the request succeeds");
    response.results.remove(0)
}

fn trace_node(result: &QueryResult, key: &str) -> Value {
    let node = result
        .trace
        .get(key)
        .unwrap_or_else(|| panic!("`{key}` is traced: {:?}", result.trace.keys()));
    serde_json::to_value(node).expect("the node serialises")
}

/// A rule that reads a scalar parameter lists it: its identity, the value
/// that applied and the dates of the version in force. It carries no
/// `indexed_by`, and the parameter is not a rule: it has no trace node and
/// is not a dependency.
#[test]
fn a_scalar_parameter_read_is_listed_on_the_rule_that_read_it() {
    let result = explain(&["income_after_standard_deduction"]);
    let node = trace_node(&result, "income_after_standard_deduction");

    assert_eq!(node["value"]["value"], "1291");
    assert_eq!(
        node["parameter_reads"],
        json!([{
            "parameter": "standard_deduction",
            "name": "standard_deduction",
            "index": 0,
            "value": {"kind": "integer", "value": 209},
            "unit": "USD",
            "effective_from": "2025-10-01",
            "source": "7 CFR 273.9(d)(1)",
        }])
    );
    assert_eq!(node["dependencies"], json!([]));
    assert!(!result.trace.contains_key("standard_deduction"));
}

/// A table read resolves to one cell: `indexed_by` names the key, `index`
/// is the key the household had, and `value` is the cell of the version in
/// force, not the whole table and not an earlier version's cell.
#[test]
fn a_table_read_reports_the_key_and_the_cell_that_applied() {
    let result = explain(&["allotment_ceiling"]);
    let node = trace_node(&result, "allotment_ceiling");

    assert_eq!(node["value"]["value"], 785);
    assert_eq!(
        node["parameter_reads"],
        json!([{
            "parameter": "maximum_allotment",
            "name": "maximum_allotment",
            "indexed_by": "household_size",
            "index": 3,
            "value": {"kind": "integer", "value": 785},
            "unit": "USD",
            "effective_from": "2025-10-01",
        }])
    );
    assert!(!result.trace.contains_key("maximum_allotment"));
}

/// A member rule's read is on the member's node, at each member, in both
/// `instance_trace` and `trace`. The household rule that sums the member
/// rule read no parameter itself, so it has no `parameter_reads`.
#[test]
fn a_member_rule_lists_its_parameter_read_at_each_member() {
    let result = explain(&["household_earned_income_deduction"]);
    let expected = json!([{
        "parameter": "earned_income_deduction_rate",
        "name": "earned_income_deduction_rate",
        "index": 0,
        "value": {"kind": "decimal", "value": "0.2"},
        "effective_from": "2024-10-01",
    }]);

    let instances = result
        .instance_trace
        .get("member_earned_income_deduction")
        .expect("the member rule has a per-instance view");
    for (member, deduction) in [("person-1", "200"), ("person-2", "100")] {
        let node = serde_json::to_value(&instances[member]).expect("the node serialises");
        assert_eq!(node["entity_id"], member);
        assert_eq!(node["value"]["value"], deduction);
        assert_eq!(node["parameter_reads"], expected, "at {member}");
    }

    let household = trace_node(&result, "household_earned_income_deduction");
    assert!(household.get("parameter_reads").is_none());
    for dependency in household["dependencies"]
        .as_array()
        .expect("the household rule has dependencies")
    {
        let node = trace_node(&result, dependency.as_str().expect("a trace key"));
        assert_eq!(node["parameter_reads"], expected);
    }
    assert_eq!(household["dependencies"].as_array().map(Vec::len), Some(2));
}

/// The new `indexed_by` key is optional on the wire: a response written
/// before it existed still deserialises.
#[test]
fn a_parameter_read_without_indexed_by_still_deserialises() {
    let read: axiom_rules_engine::api::TraceParameterRead = serde_json::from_value(json!({
        "parameter": "maximum_allotment",
        "name": "maximum_allotment",
        "index": 3,
        "value": {"kind": "integer", "value": 785},
        "effective_from": "2025-10-01",
    }))
    .expect("an older read deserialises");
    assert_eq!(read.indexed_by, None);
}
