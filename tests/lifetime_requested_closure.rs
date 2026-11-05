//! Inline fixtures for lifetime requests whose outputs do not reach every rule.
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::DenseCompileError;
use axiom_rules_engine::lifetime_api::{
    LifetimeApiError, execute_lifetime_wire_request, parse_lifetime_wire_request,
};
use serde_json::{Value, json};

const TOTAL: &str = "us:policies/fixture/lifetime#total";
const AMOUNT: &str = "us:policies/fixture/lifetime#input.amount";

fn fixture(relation: bool) -> CompiledProgramArtifact {
    let unrelated = if relation {
        r#"
  - name: members
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Worker]
  - name: unrelated
    kind: derived
    entity: Worker
    dtype: Integer
    effective_from: '2000-01-01'
    formula: len(members)
"#
    } else {
        r#"
  - name: person_value
    kind: derived
    entity: Person
    dtype: Integer
    effective_from: '2000-01-01'
    formula: '7'
  - name: unrelated
    kind: derived
    entity: Worker
    dtype: Integer
    effective_from: '2000-01-01'
    formula: person_value
"#
    };
    let source = format!(
        "format: rulespec/v1\nrules:\n  - name: amount_copy\n    kind: derived\n    entity: Worker\n    dtype: Money\n    effective_from: '2000-01-01'\n    formula: amount * 2\n  - name: total\n    kind: derived\n    entity: Worker\n    dtype: Money\n    effective_from: '2000-01-01'\n    formula: sum_over_periods(amount_copy)\n{unrelated}"
    );
    let mut program = CompiledProgramArtifact::from_rulespec_str(&source)
        .unwrap()
        .program;
    for rule in &mut program.derived {
        rule.id = Some(format!("us:policies/fixture/lifetime#{}", rule.name));
    }
    CompiledProgramArtifact::compile(program).unwrap()
}

fn year(y: i32) -> Value {
    json!({"period_kind":"tax_year", "start":format!("{y}-01-01"), "end":format!("{y}-12-31")})
}

fn request(calculation: bool) -> Value {
    let mut request = json!({
        "schema":"axiom-rules-engine/lifetime-request/v1",
        "entity":"Worker",
        "periods":[year(2001), year(2002)],
        "batches":[
            {"row_count":2,"entity_ids":["a","b"],"inputs":{
                AMOUNT:{"kind":"decimal","values":["1.25","10"]}}},
            {"row_count":2,"entity_ids":["a","b"],"inputs":{
                AMOUNT:{"kind":"decimal","values":["2.50","20"]}}}
        ],
        "outputs":[TOTAL],
        "output_period":year(2002)
    });
    if calculation {
        request["schema"] = json!("axiom-rules-engine/lifetime-request/v2");
        request["calculation_period"] = year(2026);
        request["output_period"] = year(2026);
    }
    request
}

fn run_body(artifact: CompiledProgramArtifact, body: &Value) -> Result<Value, LifetimeApiError> {
    let request = parse_lifetime_wire_request(&body.to_string())?;
    Ok(serde_json::to_value(execute_lifetime_wire_request(artifact, request)?).unwrap())
}

fn run(relation: bool, calculation: bool) -> Result<Value, LifetimeApiError> {
    run_body(fixture(relation), &request(calculation))
}

fn assert_total(relation: bool, calculation: bool) {
    let result = run(relation, calculation).expect("unreached rules must not refuse a request");
    assert_eq!(
        result["outputs"][TOTAL]["column"],
        json!({"kind":"decimal","values":["7.5","60"]})
    );
    assert_eq!(result["entity_ids"], json!(["a", "b"]));
    if calculation {
        let selected = result["selected_versions"].as_array().unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|rule| rule["name"] != "unrelated"));
    }
}

/// Compile a RuleSpec source with a durable ID on every derived rule.
fn compiled(source: &str) -> CompiledProgramArtifact {
    let mut program = CompiledProgramArtifact::from_rulespec_str(source)
        .unwrap()
        .program;
    for rule in &mut program.derived {
        rule.id = Some(format!("us:policies/fixture/lifetime#{}", rule.name));
    }
    CompiledProgramArtifact::compile(program).unwrap()
}

/// A program whose entity-free output needs no rule of the request's entity.
fn scalar_fixture() -> CompiledProgramArtifact {
    compiled(
        "format: rulespec/v1\nrules:\n  - name: worker_rule\n    kind: derived\n    entity: Worker\n    dtype: Money\n    effective_from: '2000-01-01'\n    formula: sum_over_periods(amount)\n  - name: flat\n    kind: derived\n    entity: Scalar\n    dtype: Money\n    effective_from: '2000-01-01'\n    formula: '7'\n  - name: flat_total\n    kind: derived\n    entity: Scalar\n    dtype: Money\n    effective_from: '2000-01-01'\n    formula: sum_over_periods(flat)\n",
    )
}

/// `total` reads only `amount`. Each other input belongs to a rule it does not
/// reach: `unit_amount` to a filtered entity of Worker's kind, `person_amount`
/// to a Person rule a Worker rule reads directly, and `lone_person_amount` to
/// a Person rule no Worker rule reads.
fn admission_fixture() -> CompiledProgramArtifact {
    compiled(
        r#"format: rulespec/v1
rules:
  - name: member_of_unit
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Worker]
  - name: eligible
    kind: derived
    entity: Person
    dtype: Judgment
    effective_from: '2000-01-01'
    formula: person_age > 17
  - name: selected_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_unit
      entity: SelectedUnit
      member_relation: members
      slot_entities: [Person, Worker]
    versions:
      - effective_from: '2000-01-01'
        formula: member_of_unit and eligible
  - name: unit_rule
    kind: derived
    entity: SelectedUnit
    dtype: Money
    effective_from: '2000-01-01'
    formula: unit_amount
  - name: person_value
    kind: derived
    entity: Person
    dtype: Money
    effective_from: '2000-01-01'
    formula: person_amount
  - name: crossing
    kind: derived
    entity: Worker
    dtype: Money
    effective_from: '2000-01-01'
    formula: person_value
  - name: lone_person_rule
    kind: derived
    entity: Person
    dtype: Money
    effective_from: '2000-01-01'
    formula: lone_person_amount
  - name: annual
    kind: derived
    entity: Worker
    dtype: Money
    effective_from: '2000-01-01'
    formula: amount
  - name: total
    kind: derived
    entity: Worker
    dtype: Money
    effective_from: '2000-01-01'
    formula: sum_over_periods(annual)
"#,
    )
}

/// `annual` read `old_amount` until 2020 and reads `amount` from then on.
fn versioned_fixture() -> CompiledProgramArtifact {
    compiled(
        r#"format: rulespec/v1
rules:
  - name: annual
    kind: derived
    entity: Worker
    dtype: Money
    versions:
      - effective_from: '2000-01-01'
        formula: old_amount
      - effective_from: '2020-01-01'
        formula: amount
  - name: total
    kind: derived
    entity: Worker
    dtype: Money
    effective_from: '2000-01-01'
    formula: sum_over_periods(annual)
"#,
    )
}

/// `body` with one more column in every batch.
fn with_column(mut body: Value, input: &str) -> Value {
    for batch in body["batches"].as_array_mut().unwrap() {
        batch["inputs"][format!("us:policies/fixture/lifetime#input.{input}")] =
            json!({"kind":"decimal","values":["100","200"]});
    }
    body
}

#[test]
fn an_entity_with_no_rules_keeps_its_refusal() {
    for calculation in [false, true] {
        for output in ["flat_total", "worker_rule"] {
            let mut body = request(calculation);
            body["entity"] = json!("Nobody");
            body["outputs"] = json!([format!("us:policies/fixture/lifetime#{output}")]);
            for batch in body["batches"].as_array_mut().unwrap() {
                batch["inputs"] = json!({});
            }
            let error =
                run_body(scalar_fixture(), &body).expect_err("no rule belongs to this entity");
            let unknown = match (calculation, &error) {
                (false, LifetimeApiError::Compile(unknown))
                | (true, LifetimeApiError::Calculation(unknown)) => unknown,
                _ => panic!("{output}, calculation {calculation}: {error:?}"),
            };
            assert!(
                matches!(unknown, DenseCompileError::UnknownEntity(name) if name == "Nobody"),
                "{output}, calculation {calculation}: {unknown:?}"
            );
        }
    }
}

#[test]
fn a_scalar_output_no_rule_of_the_entity_reaches_runs() {
    const FLAT_TOTAL: &str = "us:policies/fixture/lifetime#flat_total";
    for calculation in [false, true] {
        let mut body = request(calculation);
        body["outputs"] = json!([FLAT_TOTAL]);
        for batch in body["batches"].as_array_mut().unwrap() {
            batch["inputs"] = json!({});
        }
        let result = run_body(scalar_fixture(), &body).unwrap();
        assert_eq!(
            result["outputs"][FLAT_TOTAL]["column"],
            json!({"kind":"decimal","values":["14","14"]}),
            "calculation {calculation}"
        );
    }
}

#[test]
fn an_unknown_output_refuses_before_a_reached_rule_that_cannot_compile() {
    for calculation in [false, true] {
        let mut body = request(calculation);
        body["outputs"] = json!([
            "us:policies/fixture/lifetime#unrelated",
            "us:policies/fixture/lifetime#missing"
        ]);
        let error = run_body(fixture(false), &body).expect_err("no rule has that ID");
        assert!(
            matches!(&error, LifetimeApiError::Request { field, message }
                if field == "outputs" && message.contains("unknown public output")),
            "calculation {calculation}: {error:?}"
        );
    }
}

#[test]
fn a_column_only_an_unreached_rule_of_the_kind_reads_changes_no_output() {
    for calculation in [false, true] {
        let expected = run_body(admission_fixture(), &request(calculation)).unwrap();
        for input in ["unit_amount", "person_amount"] {
            let body = with_column(request(calculation), input);
            let result = run_body(admission_fixture(), &body)
                .unwrap_or_else(|error| panic!("{input}, calculation {calculation}: {error:?}"));
            assert_eq!(result["outputs"], expected["outputs"], "{input}");
        }
    }
}

#[test]
fn a_column_no_rule_of_the_kind_reads_refuses() {
    for calculation in [false, true] {
        let body = with_column(request(calculation), "lone_person_amount");
        let error = run_body(admission_fixture(), &body).expect_err("only Person rules read it");
        assert!(
            matches!(&error, LifetimeApiError::Request { field, message }
                if field == "batches[0]" && message.contains("unknown public input")),
            "calculation {calculation}: {error:?}"
        );
    }
}

#[test]
fn a_column_only_an_out_of_force_formula_version_reads_changes_no_output() {
    let body = with_column(request(true), "old_amount");
    let result = run_body(versioned_fixture(), &body).unwrap();
    assert_eq!(
        result["outputs"][TOTAL]["column"],
        json!({"kind":"decimal","values":["3.75","30"]})
    );
}

#[test]
fn legacy_ignores_unreached_cross_entity_read() {
    assert_total(false, false);
}

#[test]
fn calculation_ignores_unreached_cross_entity_read() {
    assert_total(false, true);
}

#[test]
fn legacy_ignores_unreached_relation_read() {
    assert_total(true, false);
}

#[test]
fn calculation_ignores_unreached_relation_read() {
    assert_total(true, true);
}
