//! Dense evaluates the rules a related expression reads for the entity explain
//! evaluates them for.
//!
//! Explain (`src/engine.rs`, the reference) evaluates a `count`/`sum` `where`
//! clause, a `sum`'s value and every rule body with no relation context, for
//! the related entity: a derived rule referenced there is evaluated for the
//! related entity's id whatever entity the rule declares. Only a derived
//! relation's own predicate has a context, in which a referenced rule's
//! declared entity selects the current record (the relation's current slot
//! entity) or else the related record (`RelationEvalContext::entity_id_for`).
//!
//! Dense compiled any rule of the root entity (or, in a derived relation, of
//! its current entity, or an entity-free `Scalar` rule) on the root row and
//! projected it to the related rows, in `where` clauses, summed values and the
//! bodies of related rules alike. So dense answered where explain fails (a
//! `Household` rule in a `where` clause over household members), and, worse,
//! answered differently on well-typed programs over a relation between two
//! entities of the root's kind: a `Person` rule in a `where` clause over a
//! person-to-person relation was evaluated for the root person instead of each
//! related person. `rulespec-us`'s
//! `us/policies/medicaid/magi_household_income_pipeline.yaml` has that shape.
//!
//! Every program runs through explain, fast and the artifact path the PyO3
//! extension uses (`CompiledProgramArtifact::compile`, then
//! `DenseCompiledProgram::from_artifact`), and all three must agree with the
//! answer each test states.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use axiom_rules_engine::api::{ApiError, ExecutionRequest, OutputValue, execute_request};
use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompileError, DenseCompiledProgram, DenseOutputValue,
    DenseRelationBatchSpec,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::spec::{
    JudgmentOutcomeSpec, PeriodSpec, ProgramSpec, ScalarValueSpec, SpecError,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Program builders
// ---------------------------------------------------------------------------

fn period() -> Value {
    json!({ "period_kind": "month", "start": "2026-01-01", "end": "2026-01-31" })
}

fn interval() -> Value {
    json!({ "start": "2026-01-01", "end": "2026-01-31" })
}

fn input(name: &str) -> Value {
    json!({ "kind": "input", "name": name })
}

fn derived_ref(name: &str) -> Value {
    json!({ "kind": "derived", "name": name })
}

fn integer(value: i64) -> Value {
    json!({ "kind": "literal", "value": { "kind": "integer", "value": value } })
}

fn boolean(value: bool) -> Value {
    json!({ "kind": "literal", "value": { "kind": "bool", "value": value } })
}

fn compare(left: Value, op: &str, right: Value) -> Value {
    json!({ "kind": "comparison", "left": left, "op": op, "right": right })
}

fn is_true(name: &str) -> Value {
    compare(input(name), "eq", boolean(true))
}

fn or(items: Vec<Value>) -> Value {
    json!({ "kind": "or", "items": items })
}

fn not(item: Value) -> Value {
    json!({ "kind": "not", "item": item })
}

fn judgment_rule(name: &str, entity: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": "judgment", "unit": null, "semantics": "judgment", "expr": expr })
}

fn rule(name: &str, entity: &str, dtype: &str, expr: Value) -> Value {
    json!({ "name": name, "entity": entity, "dtype": dtype, "unit": null, "semantics": "scalar", "expr": expr })
}

fn count(relation: &str, slots: (usize, usize), where_clause: Option<Value>) -> Value {
    let mut expr = json!({ "kind": "count_related", "relation": relation, "current_slot": slots.0, "related_slot": slots.1 });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn sum(relation: &str, slots: (usize, usize), value: Value, where_clause: Option<Value>) -> Value {
    let mut expr = json!({
        "kind": "sum_related",
        "relation": relation,
        "current_slot": slots.0,
        "related_slot": slots.1,
        "value": value,
    });
    if let Some(where_clause) = where_clause {
        expr["where"] = where_clause;
    }
    expr
}

fn relation(name: &str, slot_entities: &[&str]) -> Value {
    json!({ "name": name, "arity": 2, "slot_entities": slot_entities })
}

fn derived_relation(
    name: &str,
    source: &str,
    slots: (usize, usize),
    slot_entities: &[&str],
    predicate: Value,
) -> Value {
    json!({
        "name": name,
        "arity": 2,
        "slot_entities": slot_entities,
        "derivation": {
            "source_relation": source,
            "current_slot": slots.0,
            "related_slot": slots.1,
            "slot_entities": slot_entities,
            "predicate": predicate,
        },
    })
}

fn program(relations: Vec<Value>, derived: Vec<Value>) -> Value {
    json!({ "relations": relations, "derived": derived })
}

// ---------------------------------------------------------------------------
// Data: root rows, each with its related rows in one base relation
// ---------------------------------------------------------------------------

fn int(value: i64) -> Value {
    json!({ "kind": "integer", "value": value })
}

fn flag(value: bool) -> Value {
    json!({ "kind": "bool", "value": value })
}

struct Related {
    id: &'static str,
    inputs: Vec<(&'static str, Value)>,
}

struct Row {
    id: &'static str,
    inputs: Vec<(&'static str, Value)>,
    related: Vec<Related>,
}

/// The rows of a batch, related to their related rows through `relation`,
/// read from slot `slots.0` (the root) to slot `slots.1`.
struct Data {
    root: &'static str,
    related_entity: &'static str,
    relation: &'static str,
    slots: (usize, usize),
    rows: Vec<Row>,
}

fn row(id: &'static str, inputs: Vec<(&'static str, Value)>, related: Vec<Related>) -> Row {
    Row {
        id,
        inputs,
        related,
    }
}

fn related(id: &'static str, inputs: Vec<(&'static str, Value)>) -> Related {
    Related { id, inputs }
}

impl Data {
    fn dataset(&self) -> Value {
        let record = |name: &str, entity: &str, id: &str, value: &Value| json!({ "name": name, "entity": entity, "entity_id": id, "interval": interval(), "value": value });
        let mut inputs = Vec::new();
        let mut tuples = Vec::new();
        for row in &self.rows {
            for (name, value) in &row.inputs {
                inputs.push(record(name, self.root, row.id, value));
            }
            for member in &row.related {
                for (name, value) in &member.inputs {
                    inputs.push(record(name, self.related_entity, member.id, value));
                }
                let mut tuple = vec![""; 2];
                tuple[self.slots.0] = row.id;
                tuple[self.slots.1] = member.id;
                tuples
                    .push(json!({ "name": self.relation, "tuple": tuple, "interval": interval() }));
            }
        }
        json!({ "inputs": inputs, "relations": tuples })
    }

    /// Dense columns: an input is a column when every row (or related row)
    /// carries it, and absent (missing for every row) otherwise.
    fn batch(&self, compiled: &DenseCompiledProgram) -> DenseBatchSpec {
        fn columns<'a>(
            entries: impl Iterator<Item = &'a Vec<(&'static str, Value)>> + Clone,
        ) -> HashMap<String, DenseColumn> {
            let count = entries.clone().count();
            let mut values: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
            for entry in entries {
                for (name, value) in entry {
                    values.entry(name).or_default().push(value);
                }
            }
            values
                .into_iter()
                .filter(|(_, values)| values.len() == count && count > 0)
                .map(|(name, values)| {
                    let column = if values[0]["kind"] == "bool" {
                        DenseColumn::Bool(
                            values.iter().map(|value| value["value"] == true).collect(),
                        )
                    } else {
                        DenseColumn::Integer(
                            values
                                .iter()
                                .map(|value| value["value"].as_i64().expect("integer input"))
                                .collect(),
                        )
                    };
                    (name.to_string(), column)
                })
                .collect()
        }
        let mut offsets = vec![0];
        for row in &self.rows {
            offsets.push(offsets.last().copied().unwrap_or(0) + row.related.len());
        }
        let related_columns = columns(
            self.rows
                .iter()
                .flat_map(|row| row.related.iter().map(|member| &member.inputs)),
        );
        // Derived relations are keyed to their base relation, so one batch
        // per distinct key.
        let relations = compiled
            .relations()
            .iter()
            .map(|schema| {
                assert_eq!(
                    schema.key.name, self.relation,
                    "one base relation per fixture"
                );
                let inputs = schema
                    .related_inputs
                    .iter()
                    .filter_map(|name| {
                        related_columns
                            .get(name)
                            .map(|column| (name.clone(), column.clone()))
                    })
                    .collect();
                (
                    schema.key.clone(),
                    DenseRelationBatchSpec {
                        offsets: offsets.clone(),
                        inputs,
                    },
                )
            })
            .collect();
        DenseBatchSpec {
            row_count: self.rows.len(),
            inputs: columns(self.rows.iter().map(|row| &row.inputs)),
            relations,
        }
    }
}

// ---------------------------------------------------------------------------
// Running the three modes
// ---------------------------------------------------------------------------

/// Every row's outputs in batch order (numbers normalised, judgments by
/// outcome), or the first error as its variant and, for a missing input, the
/// input's name. Dense names a related row's missing input by its relation
/// where explain names the entity id, so the id is left out.
#[derive(Clone, Debug, PartialEq)]
enum Answer {
    Rows(Vec<Vec<String>>),
    Failed(String),
}

fn error_key(error: &EvalError) -> String {
    match error {
        EvalError::MissingInput { name, .. } => format!("MissingInput({name})"),
        other => {
            let debug = format!("{other:?}");
            debug
                .split(|character: char| !character.is_alphanumeric())
                .next()
                .unwrap_or(&debug)
                .to_string()
        }
    }
}

fn number(value: Decimal) -> String {
    value.normalize().to_string()
}

fn sparse(mode: &str, program: &Value, data: &Data, outputs: &[&str]) -> Answer {
    sparse_bound(mode, program, data, outputs, "strict")
}

/// Explain or fast under `"lenient"` relation binding. Two tests here pin
/// how the modes evaluate a program whose relation usage contradicts the
/// dataset's entity labels (a `Household` rule read for each `Person` member,
/// a `SnapUnit` id in a `Household` slot). The default strict binding refuses
/// those requests before evaluation (see `strict_binding_refuses`), so they
/// opt out to reach the evaluators.
fn sparse_lenient(mode: &str, program: &Value, data: &Data, outputs: &[&str]) -> Answer {
    sparse_bound(mode, program, data, outputs, "lenient")
}

fn request_json(
    mode: &str,
    program: &Value,
    data: &Data,
    outputs: &[&str],
    binding: &str,
) -> Value {
    json!({
        "relation_binding": binding,
        "mode": mode,
        "program": program,
        "dataset": data.dataset(),
        "queries": data
            .rows
            .iter()
            .map(|row| json!({ "entity_id": row.id, "period": period(), "outputs": outputs }))
            .collect::<Vec<_>>(),
    })
}

/// Strict binding (the default) refuses the request with a dataset binding
/// error naming the relation, in explain and fast alike.
fn strict_binding_refuses(label: &str, program: &Value, data: &Data, outputs: &[&str]) {
    for mode in ["explain", "fast"] {
        let request: ExecutionRequest =
            serde_json::from_value(request_json(mode, program, data, outputs, "strict"))
                .expect("request JSON parses");
        match execute_request(request) {
            Err(ApiError::Spec(SpecError::StrictDatasetBindingDiagnostics(report))) => assert!(
                report.to_string().contains(data.relation),
                "{label}: {mode}: binding error should name `{}`: {report}",
                data.relation
            ),
            other => panic!("{label}: {mode}: expected a strict binding refusal, got {other:?}"),
        }
    }
}

fn sparse_bound(
    mode: &str,
    program: &Value,
    data: &Data,
    outputs: &[&str],
    binding: &str,
) -> Answer {
    let request: ExecutionRequest =
        serde_json::from_value(request_json(mode, program, data, outputs, binding))
            .expect("request JSON parses");
    match execute_request(request) {
        Ok(response) => Answer::Rows(
            response
                .results
                .iter()
                .map(|result| {
                    outputs
                        .iter()
                        .map(|output| match &result.outputs[*output] {
                            OutputValue::Scalar { value, .. } => match value {
                                ScalarValueSpec::Integer { value } => number(Decimal::from(*value)),
                                ScalarValueSpec::Decimal { value } => {
                                    number(Decimal::from_str(value).expect("decimal output"))
                                }
                                other => panic!("unexpected scalar {other:?}"),
                            },
                            OutputValue::Judgment { outcome, .. } => format!("{outcome:?}"),
                        })
                        .collect()
                })
                .collect(),
        ),
        Err(ApiError::Eval(error)) => Answer::Failed(error_key(&error)),
        Err(other) => panic!("{mode} refused the request: {other}"),
    }
}

fn dense(program: &Value, data: &Data, outputs: &[&str]) -> Result<Answer, DenseCompileError> {
    let spec: ProgramSpec = serde_json::from_value(program.clone()).expect("program JSON parses");
    let artifact = CompiledProgramArtifact::compile(spec).expect("artifact compiles");
    let compiled = DenseCompiledProgram::from_artifact(&artifact, Some(data.root))?;
    let period: PeriodSpec = serde_json::from_value(period()).expect("period parses");
    let outputs = outputs
        .iter()
        .map(|output| output.to_string())
        .collect::<Vec<_>>();
    let result = match compiled.execute(
        &period.to_model().expect("period converts"),
        data.batch(&compiled),
        &outputs,
    ) {
        Ok(result) => result,
        Err(error) => return Ok(Answer::Failed(error_key(&error))),
    };
    Ok(Answer::Rows(
        (0..result.row_count)
            .map(|row| {
                outputs
                    .iter()
                    .map(|output| match &result.outputs[output] {
                        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => {
                            number(Decimal::from(values[row]))
                        }
                        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => {
                            number(values[row])
                        }
                        DenseOutputValue::Judgment(values) => {
                            format!("{:?}", JudgmentOutcomeSpec::from(values[row]))
                        }
                        DenseOutputValue::Scalar(other) => panic!("unexpected column {other:?}"),
                    })
                    .collect()
            })
            .collect(),
    ))
}

/// Explain answers `expected`, and fast and dense answer the same.
fn assert_all_modes(label: &str, program: &Value, data: &Data, outputs: &[&str], expected: Answer) {
    let explain = sparse("explain", program, data, outputs);
    assert_eq!(explain, expected, "{label}: explain");
    assert_eq!(
        sparse("fast", program, data, outputs),
        explain,
        "{label}: fast and explain differ"
    );
    let dense = dense(program, data, outputs)
        .unwrap_or_else(|error| panic!("{label}: dense declined the program: {error}"));
    assert_eq!(dense, explain, "{label}: dense and explain differ");
}

/// `assert_all_modes` with explain and fast under lenient relation binding.
fn assert_all_modes_lenient(
    label: &str,
    program: &Value,
    data: &Data,
    outputs: &[&str],
    expected: Answer,
) {
    let explain = sparse_lenient("explain", program, data, outputs);
    assert_eq!(explain, expected, "{label}: explain");
    assert_eq!(
        sparse_lenient("fast", program, data, outputs),
        explain,
        "{label}: fast and explain differ"
    );
    let dense = dense(program, data, outputs)
        .unwrap_or_else(|error| panic!("{label}: dense declined the program: {error}"));
    assert_eq!(dense, explain, "{label}: dense and explain differ");
}

fn rows(values: &[&[&str]]) -> Answer {
    Answer::Rows(
        values
            .iter()
            .map(|row| row.iter().map(|value| value.to_string()).collect())
            .collect(),
    )
}

fn missing(input: &str) -> Answer {
    Answer::Failed(format!("MissingInput({input})"))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `member(household, person)`; h1 has members p1 and p2, h2 has none.
fn households(member_inputs: [Vec<(&'static str, Value)>; 2]) -> Data {
    let [first, second] = member_inputs;
    Data {
        root: "Household",
        related_entity: "Person",
        relation: "member",
        slots: (0, 1),
        rows: vec![
            row(
                "h1",
                vec![("f", flag(true))],
                vec![related("p1", first), related("p2", second)],
            ),
            row("h2", vec![("f", flag(true))], vec![]),
        ],
    }
}

/// `parent_of(person, person)`: a (40) has children c1 (10) and c2 (20); b
/// (16, a minor) has child d1 (1); e (30) has none.
fn parents() -> Data {
    let age = |value: i64| vec![("age", int(value))];
    Data {
        root: "Person",
        related_entity: "Person",
        relation: "parent_of",
        slots: (0, 1),
        rows: vec![
            row(
                "a",
                age(40),
                vec![related("c1", age(10)), related("c2", age(20))],
            ),
            row("b", age(16), vec![related("d1", age(1))]),
            row("e", age(30), vec![]),
        ],
    }
}

fn parent_of() -> Value {
    relation("parent_of", &["Person", "Person"])
}

fn is_minor() -> Value {
    judgment_rule(
        "is_minor",
        "Person",
        compare(input("age"), "lt", integer(18)),
    )
}

// ---------------------------------------------------------------------------
// `where` clauses and summed values
// ---------------------------------------------------------------------------

/// The shape the issue reports: a `where` clause over household members reads
/// a `Household` rule. Explain evaluates it for each member, so it fails on the
/// first member without the input, reads the members' own values when they
/// have one, and is never reached for a household without members. Dense
/// evaluated the household's own value and counted 2.
#[test]
fn where_clause_reads_a_household_rule_for_each_member() {
    let hh_flag = judgment_rule("hh_flag", "Household", is_true("f"));
    let n = rule(
        "n",
        "Household",
        "integer",
        count("member", (0, 1), Some(derived_ref("hh_flag"))),
    );
    let program = program(
        vec![relation("member", &["Household", "Person"])],
        vec![hh_flag, n],
    );
    assert_all_modes_lenient(
        "members without the input",
        &program,
        &households([vec![], vec![]]),
        &["n"],
        missing("f"),
    );
    assert_all_modes_lenient(
        "members with the input",
        &program,
        &households([vec![("f", flag(false))], vec![("f", flag(true))]]),
        &["n"],
        rows(&[&["1"], &["0"]]),
    );
    // When the members' input records label them `Person` while the `where`
    // clause evaluates a `Household` rule for each of them, the default strict
    // binding refuses the request before evaluation. (Members with no input
    // records have no known kind, so strict binding leaves them alone and the
    // first case above fails in evaluation either way.)
    strict_binding_refuses(
        "typed relation, labelled members",
        &program,
        &households([vec![("f", flag(false))], vec![("f", flag(true))]]),
        &["n"],
    );
    // Untyped: the same, with no declared slot entities.
    let untyped = program_with_untyped_member(&program);
    assert_all_modes_lenient(
        "untyped relation, members without the input",
        &untyped,
        &households([vec![], vec![]]),
        &["n"],
        missing("f"),
    );
}

fn program_with_untyped_member(program: &Value) -> Value {
    let mut untyped = program.clone();
    untyped["relations"][0] = json!({ "name": "member", "arity": 2 });
    untyped
}

/// A well-typed program over a person-to-person relation: the `where` clause
/// reads a `Person` rule, which explain evaluates for each child. Dense
/// evaluated it for the parent, answering 0, 1 and 0 (a's age, b's age).
#[test]
fn where_clause_reads_a_same_entity_rule_for_each_related_entity() {
    let n = rule(
        "n",
        "Person",
        "integer",
        count("parent_of", (0, 1), Some(derived_ref("is_minor"))),
    );
    assert_all_modes(
        "judgment rule",
        &program(vec![parent_of()], vec![is_minor(), n]),
        &parents(),
        &["n"],
        rows(&[&["1"], &["1"], &["0"]]),
    );
    // A scalar rule in a comparison takes the same path.
    let age_years = rule("age_years", "Person", "integer", input("age"));
    let n = rule(
        "n",
        "Person",
        "integer",
        count(
            "parent_of",
            (0, 1),
            Some(compare(derived_ref("age_years"), "lt", integer(18))),
        ),
    );
    assert_all_modes(
        "scalar rule in a comparison",
        &program(vec![parent_of()], vec![age_years, n]),
        &parents(),
        &["n"],
        rows(&[&["1"], &["1"], &["0"]]),
    );
}

/// A `sum`'s value is a rule explain evaluates for each related entity. Dense
/// summed the parent's value once per child: 80, 16 and 0.
#[test]
fn sum_value_rule_is_evaluated_for_each_related_entity() {
    let age_years = rule("age_years", "Person", "integer", input("age"));
    let total = rule(
        "total",
        "Person",
        "decimal",
        sum("parent_of", (0, 1), derived_ref("age_years"), None),
    );
    let minors_total = rule(
        "minors_total",
        "Person",
        "decimal",
        sum(
            "parent_of",
            (0, 1),
            derived_ref("age_years"),
            Some(derived_ref("is_minor")),
        ),
    );
    assert_all_modes(
        "sum of a same-entity rule",
        &program(
            vec![parent_of()],
            vec![age_years, is_minor(), total, minors_total],
        ),
        &parents(),
        &["total", "minors_total"],
        rows(&[&["30", "10"], &["1", "1"], &["0", "0"]]),
    );
}

/// An entity-free `Scalar` rule that reads an input reads it for the related
/// entity in explain; dense read the root's value.
#[test]
fn scalar_entity_rule_in_a_where_clause_reads_the_related_entity() {
    let threshold = rule("threshold", "Scalar", "integer", input("age"));
    let n = rule(
        "n",
        "Person",
        "integer",
        count(
            "parent_of",
            (0, 1),
            Some(compare(derived_ref("threshold"), "lt", integer(18))),
        ),
    );
    assert_all_modes(
        "Scalar rule reading an input",
        &program(vec![parent_of()], vec![threshold, n]),
        &parents(),
        &["n"],
        rows(&[&["1"], &["1"], &["0"]]),
    );
}

// ---------------------------------------------------------------------------
// Derived relations
// ---------------------------------------------------------------------------

/// A `where` clause over a derived relation has no relation context either:
/// the `Household` rule is evaluated for each adult member.
#[test]
fn where_clause_over_a_derived_relation_reads_rules_for_the_related_entity() {
    let adult = judgment_rule(
        "is_adult",
        "Person",
        compare(input("age"), "gte", integer(18)),
    );
    let hh_flag = judgment_rule("hh_flag", "Household", is_true("f"));
    let n = rule(
        "n",
        "Household",
        "integer",
        count("adult_member", (0, 1), Some(derived_ref("hh_flag"))),
    );
    let program = program(
        vec![
            relation("member", &["Household", "Person"]),
            derived_relation(
                "adult_member",
                "member",
                (0, 1),
                &["Household", "Person"],
                derived_ref("is_adult"),
            ),
        ],
        vec![adult, hh_flag, n],
    );
    assert_all_modes(
        "adult members without the input",
        &program,
        &households([vec![("age", int(30))], vec![("age", int(5))]]),
        &["n"],
        missing("f"),
    );
    assert_all_modes(
        "adult members with the input",
        &program,
        &households([
            vec![("age", int(30)), ("f", flag(true))],
            vec![("age", int(40)), ("f", flag(false))],
        ]),
        &["n"],
        rows(&[&["1"], &["0"]]),
    );
}

/// In a derived relation's own predicate a rule of the current entity reads
/// the current record, as before; a related rule's body has no context, so a
/// `Household` rule it reads is evaluated for the member. Dense evaluated that
/// one for the household and kept both members.
#[test]
fn derived_relation_predicate_reads_rules_as_explain_scopes_them() {
    let hh_rule = judgment_rule("hh_rule", "Household", is_true("f"));
    let p_rule = judgment_rule("p_rule", "Person", derived_ref("hh_rule"));
    let n = rule("n", "Household", "integer", count("kept", (0, 1), None));
    let with_predicate = |predicate: Value| {
        program(
            vec![
                relation("member", &["Household", "Person"]),
                derived_relation(
                    "kept",
                    "member",
                    (0, 1),
                    &["Household", "Person"],
                    predicate,
                ),
            ],
            vec![hh_rule.clone(), p_rule.clone(), n.clone()],
        )
    };
    let current = with_predicate(derived_ref("hh_rule"));
    assert_all_modes(
        "current-entity rule in the predicate",
        &current,
        &households([vec![], vec![]]),
        &["n"],
        rows(&[&["2"], &["0"]]),
    );
    let through_related = with_predicate(derived_ref("p_rule"));
    assert_all_modes(
        "related rule reading a household rule, members without the input",
        &through_related,
        &households([vec![], vec![]]),
        &["n"],
        missing("f"),
    );
    assert_all_modes(
        "related rule reading a household rule, members with the input",
        &through_related,
        &households([vec![("f", flag(false))], vec![("f", flag(false))]]),
        &["n"],
        rows(&[&["0"], &["0"]]),
    );
}

/// The shape of `rulespec-us`'s Medicaid MAGI household pipeline: an untyped
/// data relation of candidate rows for each applicant (current slot 1), an
/// untyped derived relation over it, and `Person` rules for the applicant that
/// count and sum over it with `Person` rules for each row. Dense evaluated the
/// row rules for the applicant (the root), so it counted every row or none,
/// and summed the applicant's income once per row.
#[test]
fn untyped_person_rows_read_row_rules_for_each_row() {
    let program = program(
        vec![
            json!({ "name": "candidate_row", "arity": 2 }),
            json!({
                "name": "member_of_applicant",
                "arity": 2,
                "derivation": {
                    "source_relation": "candidate_row",
                    "current_slot": 1,
                    "related_slot": 0,
                    "predicate": or(vec![
                        derived_ref("row_is_applicant"),
                        not(derived_ref("row_is_applicant")),
                    ]),
                },
            }),
        ],
        vec![
            judgment_rule("row_is_applicant", "Person", is_true("applicant")),
            judgment_rule("row_is_member", "Person", is_true("member")),
            rule("row_income", "Person", "integer", input("income")),
            rule(
                "counted_rows",
                "Person",
                "integer",
                count(
                    "member_of_applicant",
                    (1, 0),
                    Some(derived_ref("row_is_member")),
                ),
            ),
            rule(
                "household_income",
                "Person",
                "decimal",
                sum(
                    "member_of_applicant",
                    (1, 0),
                    derived_ref("row_income"),
                    Some(derived_ref("row_is_member")),
                ),
            ),
        ],
    );
    let person = |applicant: bool, member: bool, income: i64| {
        vec![
            ("applicant", flag(applicant)),
            ("member", flag(member)),
            ("income", int(income)),
        ]
    };
    let data = Data {
        root: "Person",
        related_entity: "Person",
        relation: "candidate_row",
        slots: (1, 0),
        rows: vec![
            row(
                "x",
                person(true, false, 100),
                vec![
                    related("x_self", person(true, true, 100)),
                    related("x_spouse", person(false, true, 50)),
                    related("x_lodger", person(false, false, 900)),
                ],
            ),
            row(
                "y",
                person(true, true, 7),
                vec![related("y_self", person(true, false, 7))],
            ),
        ],
    };
    assert_all_modes(
        "MAGI-style candidate rows",
        &program,
        &data,
        &["counted_rows", "household_income"],
        rows(&[&["2", "150"], &["0", "0"]]),
    );
}

/// In a derived relation's predicate, a rule of an entity that is neither the
/// relation's current nor its related slot entity (here the derivation's own
/// `SnapUnit`) is evaluated for the related entity by explain's fallback.
/// Dense declines it rather than evaluate it for either record; it used to
/// evaluate a root-entity rule on the root row.
#[test]
fn derived_relation_predicate_declines_a_rule_of_neither_slot_entity() {
    let program = program(
        vec![
            relation("member_of_household", &["Person", "Household"]),
            json!({
                "name": "snap_unit",
                "arity": 2,
                "slot_entities": ["Person", "Household"],
                "derivation": {
                    "source_relation": "member_of_household",
                    "current_slot": 1,
                    "related_slot": 0,
                    "entity": "SnapUnit",
                    "slot_entities": ["Person", "Household"],
                    "predicate": derived_ref("unit_flag"),
                },
            }),
        ],
        vec![
            judgment_rule("unit_flag", "SnapUnit", is_true("g")),
            rule(
                "snap_unit_size",
                "SnapUnit",
                "integer",
                count("snap_unit", (1, 0), None),
            ),
        ],
    );
    let data = Data {
        root: "SnapUnit",
        related_entity: "Person",
        relation: "member_of_household",
        slots: (1, 0),
        rows: vec![row(
            "h1",
            vec![("g", flag(true))],
            vec![related("p1", vec![]), related("p2", vec![])],
        )],
    };
    let outputs = ["snap_unit_size"];
    // `h1` is labelled `SnapUnit` (the query root) but sits in the
    // `Household` slot of `member_of_household`, so the default strict
    // binding refuses the request; lenient binding reaches the evaluators.
    strict_binding_refuses("SnapUnit id in a Household slot", &program, &data, &outputs);
    let explain = sparse_lenient("explain", &program, &data, &outputs);
    assert_eq!(explain, missing("g"), "explain reads `g` for the member");
    assert_eq!(sparse_lenient("fast", &program, &data, &outputs), explain);
    match dense(&program, &data, &outputs) {
        Err(DenseCompileError::Unsupported(message)) => assert!(
            message.contains(
                "`unit_flag` has entity `SnapUnit`, which is neither current nor related"
            ),
            "unexpected decline: {message}"
        ),
        other => panic!("dense should decline, got {other:?}"),
    }
}
