//! Election inputs (#155): a module's `inputs:` block may mark an input it
//! reads as an election, `{actor, provision}`. The program carries the
//! declaration in `input_declarations`, and the interface reports it on the
//! input's slot; every other input is a fact. Every other key of the block
//! stays inert.

use std::collections::BTreeMap;

use axiom_rules_engine::api::{ExecutionRequest, execute_request};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::interface::{ProgramInterface, derive_interface};
use axiom_rules_engine::rulespec::{RuleSpecError, lower_rulespec_str};
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{InputDeclarationSpec, InputElection, ProgramSpec, SpecError};
use proptest::prelude::*;
use serde_json::{Value, json};

const SNAP_BOARDER: &str =
    include_str!("fixtures/artifacts/interface/snap_boarder_election.rulespec.yaml");
const SNAP_TARGET: &str = "us:regulations/test/snap-household";
const ELECTION_SLOT: &str = "provider_requests_boarder_inclusion";
const FACT_SLOT: &str = "purchases_and_prepares_with_household";

fn boarder_election() -> InputElection {
    InputElection {
        actor: "Household".to_string(),
        provision: "us/regulation/7/273/1/b/3".to_string(),
    }
}

fn document(source: &str) -> Value {
    serde_yaml::from_str(source).expect("fixture YAML parses")
}

fn yaml(document: &Value) -> String {
    serde_yaml::to_string(document).expect("document serializes as YAML")
}

fn compile(source: &str) -> CompiledProgramArtifact {
    CompiledProgramArtifact::from_rulespec_str(source).expect("module compiles")
}

/// The artifact as JSON without its source closure, which digests the
/// module text and so differs between spellings of one module.
fn without_source_closure(artifact: &CompiledProgramArtifact) -> Value {
    let mut artifact = artifact.clone();
    artifact.source_closure = None;
    serde_json::to_value(artifact).expect("artifact serializes")
}

fn interface(artifact: &CompiledProgramArtifact) -> &ProgramInterface {
    artifact
        .interface
        .as_ref()
        .expect("format 3 has an interface")
}

fn slot<'a>(
    interface: &'a ProgramInterface,
    name: &str,
) -> &'a axiom_rules_engine::interface::InterfaceInput {
    interface
        .inputs
        .iter()
        .find(|input| input.name == name)
        .unwrap_or_else(|| panic!("no input `{name}`"))
}

/// `inputs:` in mapping form, the name as the key.
fn mapping_form(list: &Value) -> Value {
    let mut map = serde_json::Map::new();
    for entry in list.as_array().expect("list form") {
        let mut entry = entry.clone();
        let name = entry
            .as_object_mut()
            .unwrap()
            .remove("name")
            .expect("entry has a name");
        map.insert(name.as_str().unwrap().to_string(), entry);
    }
    Value::Object(map)
}

/// An edit to a module or artifact, as JSON.
type Edit = Box<dyn Fn(&mut Value)>;
type OnceEdit<'a> = Box<dyn FnOnce(&mut Value) + 'a>;

struct Modules(BTreeMap<String, String>);

impl ModuleSource for Modules {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).cloned())
    }
}

fn modules(entries: &[(&str, String)]) -> Modules {
    Modules(
        entries
            .iter()
            .map(|(target, text)| (target.to_string(), text.clone()))
            .collect(),
    )
}

/// A one-rule module reading `slot` on `Person`, declaring it with
/// `election` when one is given.
fn reader(rule: &str, slot: &str, election: Option<Value>, imports: &[&str]) -> String {
    let mut entry =
        json!({"name": slot, "entity": "Person", "dtype": "Boolean", "period": "Month"});
    if let Some(election) = election {
        entry["election"] = election;
    }
    reader_declaring(rule, slot, json!([entry]), imports)
}

/// A one-rule module reading `slot` on `Person`, with `inputs` as given.
fn reader_declaring(rule: &str, slot: &str, inputs: Value, imports: &[&str]) -> String {
    yaml(&json!({
        "format": "rulespec/v1",
        "imports": imports,
        "inputs": inputs,
        "rules": [{
            "name": rule, "kind": "derived", "entity": "Person", "dtype": "Judgment",
            "period": "Month",
            "versions": [{"effective_from": "2026-01-01", "formula": slot}]
        }]
    }))
}

fn rulespec_message(error: CompileError) -> String {
    match error {
        CompileError::RuleSpec { error, .. } => error.to_string(),
        other => panic!("expected a RuleSpec error, got {other}"),
    }
}

/// An election declared in either form reaches `program.input_declarations`
/// under the declaring module's request name, and the interface reports it
/// on the slot; an input declared without one, and an input not declared at
/// all, are facts. The two forms compile to the same artifact.
#[test]
fn an_election_in_either_form_reaches_the_interface() {
    let listed = compile(SNAP_BOARDER);
    assert_eq!(
        listed.program.input_declarations,
        [InputDeclarationSpec {
            input: ELECTION_SLOT.to_string(),
            election: boarder_election(),
        }]
    );
    let listed_interface = interface(&listed);
    assert_eq!(
        slot(listed_interface, ELECTION_SLOT).election,
        Some(boarder_election())
    );
    assert_eq!(slot(listed_interface, ELECTION_SLOT).default, None);
    assert_eq!(slot(listed_interface, FACT_SLOT).election, None);
    assert_eq!(slot(listed_interface, "is_boarder").election, None);
    let elections = listed_interface
        .inputs
        .iter()
        .filter(|input| input.election.is_some())
        .count();
    assert_eq!(elections, 1);

    let mut mapped = document(SNAP_BOARDER);
    mapped["inputs"] = mapping_form(&mapped["inputs"]);
    assert!(mapped["inputs"].is_object());
    assert_eq!(
        without_source_closure(&compile(&yaml(&mapped))),
        without_source_closure(&listed)
    );

    let atomic = CompiledProgramArtifact::from_rulespec_with_source(
        SNAP_TARGET,
        &modules(&[(SNAP_TARGET, SNAP_BOARDER.to_string())]),
    )
    .expect("the atomic module compiles");
    let request_name = format!("{SNAP_TARGET}#input.{ELECTION_SLOT}");
    assert_eq!(
        atomic.program.input_declarations,
        [InputDeclarationSpec {
            input: request_name.clone(),
            election: boarder_election(),
        }]
    );
    let election = slot(interface(&atomic), ELECTION_SLOT);
    assert_eq!(election.legal_id, request_name);
    assert_eq!(election.election, Some(boarder_election()));
}

/// The fixture's interface `inputs`, exactly: one election, two facts.
#[test]
fn the_snap_boarder_fixture_has_one_election_and_two_facts() {
    let inputs = serde_json::to_value(&interface(&compile(SNAP_BOARDER)).inputs).unwrap();
    assert_eq!(
        inputs,
        json!([
            {"name": "is_boarder", "legal_id": "is_boarder", "entities": ["Person"],
             "dtype": "bool", "dtype_basis": "usage", "default": null, "values": null,
             "values_basis": null, "election": null},
            {"name": ELECTION_SLOT, "legal_id": ELECTION_SLOT, "entities": ["Person"],
             "dtype": "bool", "dtype_basis": "usage", "default": null, "values": null,
             "values_basis": null,
             "election": {"actor": "Household", "provision": "us/regulation/7/273/1/b/3"}},
            {"name": FACT_SLOT, "legal_id": FACT_SLOT, "entities": ["Person"],
             "dtype": "bool", "dtype_basis": "usage", "default": null, "values": null,
             "values_basis": null, "election": null}
        ])
    );
}

fn inert_key() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "name",
        "entity",
        "dtype",
        "period",
        "description",
        "unit",
        "default",
        "values",
        "elections",
        "Election",
        ELECTION_SLOT,
    ])
    .prop_map(str::to_string)
}

fn inert_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-3i64..40).prop_map(|value| json!(value)),
        prop::sample::select(vec![
            "Person",
            "Household",
            "Count",
            "Number",
            "Enum[a, b]",
            "Money",
            "Year",
            "a: b",
            ELECTION_SLOT,
            FACT_SLOT,
        ])
        .prop_map(|text| json!(text)),
    ]
}

/// YAML values whose mappings never use the key `election`.
fn inert_value() -> impl Strategy<Value = Value> {
    inert_leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(inert_key(), inner, 0..4)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// For every `inputs:` block in which no mapping carries the key
    /// `election`, whatever its shape (wrong dtypes, stray keys, a scalar, a
    /// mapping without `election`), the module compiles to the artifact it
    /// compiles to without the block: the same `program` (with
    /// `input_declarations` empty, hence omitted), `metadata` and interface.
    #[test]
    fn an_inputs_block_without_an_election_is_inert(block in inert_value()) {
        let mut absent = document(SNAP_BOARDER);
        absent.as_object_mut().unwrap().remove("inputs");
        let baseline = without_source_closure(&compile(&yaml(&absent)));
        prop_assert!(baseline["program"].get("input_declarations").is_none());
        let mut present = absent.clone();
        present["inputs"] = block;
        prop_assert_eq!(without_source_closure(&compile(&yaml(&present))), baseline);
    }
}

/// The fixture without its election compiles exactly as without the block.
#[test]
fn the_fixtures_facts_alone_are_inert() {
    let mut facts = document(SNAP_BOARDER);
    facts["inputs"][1]
        .as_object_mut()
        .unwrap()
        .remove("election");
    let mut absent = facts.clone();
    absent.as_object_mut().unwrap().remove("inputs");
    assert_eq!(
        without_source_closure(&compile(&yaml(&facts))),
        without_source_closure(&compile(&yaml(&absent)))
    );
}

fn refusal(edit: impl FnOnce(&mut Value)) -> String {
    let mut module = document(SNAP_BOARDER);
    edit(&mut module);
    match lower_rulespec_str(&yaml(&module)) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("the module must be refused"),
    }
}

fn set_election(module: &mut Value, index: usize, name: &str, election: Value) {
    module["inputs"][index]["name"] = json!(name);
    module["inputs"][index]["election"] = election;
}

/// An election must name an input slot its own module reads, and must be
/// `{actor, provision}` exactly, well formed. Each refusal names the module
/// and the entry.
#[test]
fn malformed_or_misplaced_elections_are_refused() {
    let good = json!({"actor": "Household", "provision": "us/regulation/7/273/1/b/3"});
    let cases: Vec<(&str, OnceEdit<'_>, &str)> = vec![
        (
            "an input nothing reads",
            Box::new(|m| set_election(m, 1, "not_read_anywhere", good.clone())),
            "RuleSpec module `<root>` input `not_read_anywhere`: carries an election, but no rule or derived-relation predicate of this module reads an input of that name",
        ),
        (
            "a rule name",
            Box::new(|m| set_election(m, 1, "in_snap_household", good.clone())),
            "RuleSpec module `<root>` input `in_snap_household`: carries an election",
        ),
        (
            "a relation name",
            Box::new(|m| set_election(m, 1, "member_of_household", good.clone())),
            "RuleSpec module `<root>` input `member_of_household`: carries an election",
        ),
        (
            "a lowercase actor",
            Box::new(|m| {
                m["inputs"][1]["election"]["actor"] = json!("household");
            }),
            "input `provider_requests_boarder_inclusion`: election actor `household` is not an UpperCamelCase entity kind",
        ),
        (
            "an actor with a space",
            Box::new(|m| {
                m["inputs"][1]["election"]["actor"] = json!("Provider Household");
            }),
            "election actor `Provider Household` is not an UpperCamelCase entity kind",
        ),
        (
            "a provision that is not a citation path",
            Box::new(|m| {
                m["inputs"][1]["election"]["provision"] = json!("7 CFR 273.1(b)(3)");
            }),
            "election provision `7 CFR 273.1(b)(3)` is not a canonical corpus citation path",
        ),
        (
            "an unknown key",
            Box::new(|m| {
                m["inputs"][1]["election"]["default"] = json!(false);
            }),
            "input `provider_requests_boarder_inclusion`: invalid election: unknown field `default`",
        ),
        (
            "a missing provision",
            Box::new(|m| {
                m["inputs"][1]["election"]
                    .as_object_mut()
                    .unwrap()
                    .remove("provision");
            }),
            "invalid election: missing field `provision`",
        ),
        (
            "a null election",
            Box::new(|m| {
                m["inputs"][1]["election"] = Value::Null;
            }),
            "input `provider_requests_boarder_inclusion`: invalid election",
        ),
        (
            "an unnamed entry",
            Box::new(|m| {
                m["inputs"][1].as_object_mut().unwrap().remove("name");
            }),
            "RuleSpec module `<root>` `inputs` entry 1 carries an `election` but no name",
        ),
        (
            "two elections for one input",
            Box::new(|m| {
                let mut second = m["inputs"][1].clone();
                second["election"]["actor"] = json!("Person");
                m["inputs"].as_array_mut().unwrap().push(second);
            }),
            "input `provider_requests_boarder_inclusion`: is declared with two different elections",
        ),
    ];
    for (case, edit, expected) in cases {
        let message = refusal(edit);
        assert!(
            message.contains(expected),
            "{case}: `{message}` does not contain `{expected}`"
        );
    }
}

/// An election may be repeated with the same value, which declares it once.
#[test]
fn a_repeated_identical_election_is_one_declaration() {
    let mut module = document(SNAP_BOARDER);
    let repeat = module["inputs"][1].clone();
    module["inputs"].as_array_mut().unwrap().push(repeat);
    assert_eq!(
        without_source_closure(&compile(&yaml(&module))),
        without_source_closure(&compile(SNAP_BOARDER))
    );
}

/// A parameter's name is not an input slot, but the input a parameter is
/// indexed by is.
#[test]
fn a_parameter_name_is_not_an_input() {
    let mut module = document(SNAP_BOARDER);
    module["rules"].as_array_mut().unwrap().push(json!({
        "name": "boarder_allowance", "kind": "parameter", "dtype": "Integer",
        "versions": [{"effective_from": "2026-01-01", "formula": "1"}]
    }));
    set_election(
        &mut module,
        1,
        "boarder_allowance",
        json!({"actor": "Household", "provision": "us/regulation/7/273/1/b/3"}),
    );
    let message = lower_rulespec_str(&yaml(&module))
        .expect_err("a parameter is not an input")
        .to_string();
    assert!(
        message.contains("input `boarder_allowance`: carries an election, but no rule"),
        "{message}"
    );
}

/// An imported module's elections are checked against that module's own
/// rules, and kept under its own request name.
#[test]
fn elections_are_checked_against_their_own_module() {
    let importer = "us:policies/test/importer";
    let election = json!({"actor": "Household", "provision": "us/regulation/7/273/1/b/3"});
    // The importer declares an election on an input only the imported
    // module reads.
    let source = modules(&[
        (
            importer,
            reader_declaring(
                "importer_flag",
                "importer_input",
                json!([
                    {"name": "importer_input"},
                    {"name": ELECTION_SLOT, "election": election.clone()}
                ]),
                &[SNAP_TARGET],
            ),
        ),
        (SNAP_TARGET, SNAP_BOARDER.to_string()),
    ]);
    let message = rulespec_message(
        CompiledProgramArtifact::from_rulespec_with_source(importer, &source)
            .expect_err("the importer does not read the input"),
    );
    assert_eq!(
        message,
        format!(
            "RuleSpec module `{importer}` input `{ELECTION_SLOT}`: carries an election, but no rule or derived-relation predicate of this module reads an input of that name"
        )
    );

    // An imported module's own election survives the import, a fragment
    // import included: a fragment selects nothing at load, so the module's
    // rules are all there.
    for import in [
        SNAP_TARGET.to_string(),
        format!("{SNAP_TARGET}#in_snap_household"),
    ] {
        let source = modules(&[
            (
                importer,
                reader(
                    "importer_flag",
                    "importer_input",
                    Some(election.clone()),
                    &[&import],
                ),
            ),
            (SNAP_TARGET, SNAP_BOARDER.to_string()),
        ]);
        let artifact = CompiledProgramArtifact::from_rulespec_with_source(importer, &source)
            .expect("the graph compiles");
        let inputs = artifact
            .program
            .input_declarations
            .iter()
            .map(|declaration| declaration.input.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            [
                format!("{importer}#input.importer_input"),
                format!("{SNAP_TARGET}#input.{ELECTION_SLOT}"),
            ],
            "{import}"
        );
        assert_eq!(
            slot(interface(&artifact), ELECTION_SLOT).election,
            Some(boarder_election())
        );
    }
}

/// Two modules reading one slot may both declare it an election only if
/// they agree; the slot then reports that election once.
#[test]
fn declarations_of_one_slot_must_agree() {
    let root = "us:policies/test/b";
    let other = "us:policies/test/a";
    let household = json!({"actor": "Household", "provision": "us/regulation/7/273/1/b/3"});
    let person = json!({"actor": "Person", "provision": "us/regulation/7/273/1/b/3"});
    let graph = |root_election: Value| {
        modules(&[
            (
                root,
                reader("b_flag", "shared_choice", Some(root_election), &[other]),
            ),
            (
                other,
                reader("a_flag", "shared_choice", Some(household.clone()), &[]),
            ),
        ])
    };
    let agreed =
        CompiledProgramArtifact::from_rulespec_with_source(root, &graph(household.clone()))
            .expect("agreeing declarations compile");
    assert_eq!(agreed.program.input_declarations.len(), 2);
    assert_eq!(
        slot(interface(&agreed), "shared_choice").election,
        Some(InputElection {
            actor: "Household".to_string(),
            provision: "us/regulation/7/273/1/b/3".to_string(),
        })
    );

    let error = CompiledProgramArtifact::from_rulespec_with_source(root, &graph(person))
        .expect_err("disagreeing declarations are refused");
    let CompileError::Spec(SpecError::InputDeclaration { input, message }) = error else {
        panic!("expected an input declaration error, got {error}");
    };
    assert_eq!(input, format!("{root}#input.shared_choice"));
    assert_eq!(
        message,
        format!(
            "its election (actor `Person`, provision `us/regulation/7/273/1/b/3`) disagrees with `{other}#input.shared_choice`'s (actor `Household`, provision `us/regulation/7/273/1/b/3`) for the slot `shared_choice`"
        )
    );
}

/// A judgment rule on `Person` that is true when `read` is.
fn flag(name: &str, read: Value) -> Value {
    json!({
        "name": name, "entity": "Person", "dtype": "judgment", "unit": null,
        "semantics": "judgment",
        "expr": {"kind": "comparison", "left": read, "op": "eq",
                 "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}}
    })
}

fn raw_program(declarations: Value, read: Value) -> ProgramSpec {
    serde_json::from_value(json!({
        "derived": [flag("flag", read)],
        "input_declarations": declarations
    }))
    .expect("raw program deserializes")
}

fn choice_read() -> Value {
    json!({"kind": "input", "name": "choice"})
}

fn declaration(input: &str, actor: &str) -> Value {
    json!({"input": input, "election": {"actor": actor, "provision": "us/regulation/7/273/1/b/3"}})
}

fn derive_error(program: &ProgramSpec) -> (String, String) {
    match derive_interface(program) {
        Err(SpecError::InputDeclaration { input, message }) => (input, message),
        other => panic!("expected an input declaration error, got {other:?}"),
    }
}

/// The engine never defaults an election: an election slot with an
/// `input_or_else` read site is refused, by derivation and so by compile.
#[test]
fn an_election_with_a_default_is_refused() {
    let read = json!({"kind": "input_or_else", "name": "choice",
                      "default": {"kind": "bool", "value": false}});
    let program = raw_program(json!([declaration("choice", "Household")]), read.clone());
    let (input, message) = derive_error(&program);
    assert_eq!(input, "choice");
    assert_eq!(
        message,
        "`choice` is an election, and `input_or_else` reads it with a default; the engine never defaults an election"
    );
    assert!(matches!(
        CompiledProgramArtifact::compile(program),
        Err(CompileError::Spec(SpecError::InputDeclaration { .. }))
    ));
    // The same read without a declaration is a fact with a default.
    let fact = derive_interface(&raw_program(json!([]), read)).expect("a fact may default");
    assert!(fact.inputs[0].default.is_some());
    assert_eq!(fact.inputs[0].election, None);
}

/// `program.input_declarations` from a raw `ProgramSpec` is held to the
/// form compilation writes: sorted by `input`, each once, naming an input
/// the program reads, with a well-formed election.
#[test]
fn raw_declarations_must_be_canonical() {
    let cases = [
        (
            json!([declaration("unread", "Household")]),
            "unread",
            "names no input the program reads",
        ),
        (
            json!([
                declaration("choice", "Household"),
                declaration("choice", "Household")
            ]),
            "choice",
            "input declarations must be sorted by `input`, each input once",
        ),
        (
            json!([declaration("choice", "household")]),
            "choice",
            "election actor `household` is not an UpperCamelCase entity kind",
        ),
    ];
    for (declarations, expected_input, expected_message) in cases {
        let (input, message) = derive_error(&raw_program(declarations, choice_read()));
        assert_eq!(
            (input.as_str(), message.as_str()),
            (expected_input, expected_message)
        );
    }
    let unsorted: ProgramSpec = serde_json::from_value(json!({
        "derived": [
            flag("a", json!({"kind": "input", "name": "x"})),
            flag("b", json!({"kind": "input", "name": "y"}))
        ],
        "input_declarations": [declaration("y", "Household"), declaration("x", "Household")]
    }))
    .unwrap();
    let (input, message) = derive_error(&unsorted);
    assert_eq!(input, "x");
    assert_eq!(
        message,
        "input declarations must be sorted by `input`, each input once"
    );
    assert!(
        serde_json::from_value::<ProgramSpec>(json!({
            "input_declarations": [{"input": "choice", "election": {"actor": "Household",
              "provision": "us/regulation/7/273/1/b/3"}, "default": false}]
        }))
        .is_err(),
        "an unknown key in a declaration is refused"
    );
}

/// A loaded artifact's elections must be the ones its program declares:
/// changing, removing or adding a key in an interface `election`, or in
/// `program.input_declarations`, is refused at load.
#[test]
fn tampered_elections_are_refused_at_load() {
    let artifact = serde_json::to_value(compile(SNAP_BOARDER)).unwrap();
    let election_index = artifact["program"]["interface"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .position(|input| input["name"] == ELECTION_SLOT)
        .unwrap();
    let interface_election = format!("/program/interface/inputs/{election_index}/election");
    let fact_election = "/program/interface/inputs/0/election".to_string();
    let declared = "/program/input_declarations/0/election".to_string();
    let set = |pointer: String, value: Value| -> Edit {
        Box::new(move |artifact: &mut Value| {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            artifact
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.to_string(), value.clone());
        })
    };
    let remove = |pointer: String, key: &'static str| -> Edit {
        Box::new(move |artifact: &mut Value| {
            artifact
                .pointer_mut(&pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
        })
    };
    let edits: Vec<(&str, Edit)> = vec![
        (
            "interface actor changed",
            set(format!("{interface_election}/actor"), json!("Person")),
        ),
        (
            "interface provision removed",
            remove(interface_election.clone(), "provision"),
        ),
        (
            "interface key added",
            set(format!("{interface_election}/note"), json!("x")),
        ),
        (
            "interface election removed",
            set(interface_election.clone(), Value::Null),
        ),
        (
            "interface election key removed",
            remove(
                format!("/program/interface/inputs/{election_index}"),
                "election",
            ),
        ),
        (
            "interface fact made an election",
            set(
                fact_election,
                json!({"actor": "Household", "provision": "us/regulation/7/273/1/b/3"}),
            ),
        ),
        (
            "declared actor changed",
            set(format!("{declared}/actor"), json!("Person")),
        ),
        (
            "declared key added",
            set(format!("{declared}/note"), json!("x")),
        ),
        (
            "declaration key added",
            set(
                "/program/input_declarations/0/default".to_string(),
                json!(false),
            ),
        ),
        (
            "declarations removed",
            remove("/program".to_string(), "input_declarations"),
        ),
    ];
    let pristine = serde_json::to_string(&artifact).unwrap();
    CompiledProgramArtifact::from_json_str(&pristine).expect("the untouched artifact loads");
    for (case, edit) in edits {
        let mut tampered = artifact.clone();
        edit(&mut tampered);
        assert!(
            CompiledProgramArtifact::from_json_str(&serde_json::to_string(&tampered).unwrap())
                .is_err(),
            "{case}: the tampered artifact must be refused"
        );
    }
}

/// `input_declarations` is a format-3 addition, so the format-2
/// compatibility loader refuses a format-2 payload carrying it, even empty.
#[test]
fn a_format_2_payload_cannot_carry_input_declarations() {
    let frozen = include_str!(
        "fixtures/artifacts/format2/compiled/tests__fixtures__only_related.compiled.json"
    );
    CompiledProgramArtifact::from_legacy_format_2_json_str(frozen).expect("the fixture loads");
    let mut payload: Value = serde_json::from_str(frozen).unwrap();
    payload["program"]["input_declarations"] = json!([]);
    let message = CompiledProgramArtifact::from_legacy_format_2_json_str(
        &serde_json::to_string(&payload).unwrap(),
    )
    .expect_err("the addition is refused")
    .to_string();
    assert!(
        message.contains("a format-2 artifact cannot carry `program.input_declarations`"),
        "{message}"
    );
}

fn execute(program: &ProgramSpec, mode: &str) -> Vec<u8> {
    let interval = json!({"start": "2026-01-01", "end": "2026-01-31"});
    let boolean = |name: &str, entity: &str, id: &str, value: bool| {
        json!({"name": name, "entity": entity, "entity_id": id, "interval": interval,
               "value": {"kind": "bool", "value": value}})
    };
    let request: ExecutionRequest = serde_json::from_value(json!({
        "mode": mode,
        "program": program,
        "dataset": {
            "inputs": [
                boolean(FACT_SLOT, "Person", "p1", true),
                boolean("is_boarder", "Person", "p1", false),
                boolean(ELECTION_SLOT, "Person", "p1", false),
                boolean(FACT_SLOT, "Person", "p2", true),
                boolean("is_boarder", "Person", "p2", true),
                boolean(ELECTION_SLOT, "Person", "p2", true)
            ],
            "relations": [
                {"name": "member_of_household", "tuple": ["p1", "h1"], "interval": interval},
                {"name": "member_of_household", "tuple": ["p2", "h1"], "interval": interval}
            ]
        },
        "queries": [
            {"entity_id": "h1",
             "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
             "outputs": ["snap_household_size"]},
            {"entity_id": "p2",
             "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
             "outputs": ["in_snap_household"]}
        ]
    }))
    .expect("request deserializes");
    serde_json::to_vec(&execute_request(request).expect("request executes")).unwrap()
}

/// Execution never reads `input_declarations`: with and without them,
/// explain and fast give byte-identical responses.
#[test]
fn execution_never_reads_input_declarations() {
    let declared = lower_rulespec_str(SNAP_BOARDER).expect("fixture lowers");
    assert!(!declared.input_declarations.is_empty());
    let mut undeclared = declared.clone();
    undeclared.input_declarations.clear();
    for mode in ["explain", "fast"] {
        assert_eq!(
            execute(&declared, mode),
            execute(&undeclared, mode),
            "{mode}"
        );
    }
}

/// A RuleSpec lowering error is a load error, not a compile-time panic: the
/// public error type carries the module and the entry.
#[test]
fn the_rulespec_error_names_module_and_entry() {
    let mut module = document(SNAP_BOARDER);
    module["inputs"][1]["election"]["actor"] = json!("household");
    match lower_rulespec_str(&yaml(&module)) {
        Err(RuleSpecError::InputElection { module, input, .. }) => {
            assert_eq!((module.as_str(), input.as_str()), ("<root>", ELECTION_SLOT));
        }
        other => panic!("expected an election error, got {other:?}"),
    }
}

fn module_schema() -> jsonschema::Validator {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/rulespec-module.v1.schema.json"))
            .expect("published module schema is JSON");
    jsonschema::draft7::new(&schema).expect("module schema compiles")
}

/// The published module schema agrees with the loader on everything about
/// an `inputs:` block it can see: it accepts both forms and inert entries of
/// any shape, and refuses exactly the election shapes the loader refuses.
/// That an election names an input its module reads, and that one input's
/// elections agree, need the loader.
#[test]
fn the_module_schema_agrees_with_the_loader_on_election_shape() {
    let schema = module_schema();
    let mut accepted = vec![document(SNAP_BOARDER)];
    let mut mapped = document(SNAP_BOARDER);
    mapped["inputs"] = mapping_form(&mapped["inputs"]);
    accepted.push(mapped);
    for inert in [
        json!(3),
        json!("text"),
        json!([1, null, "x"]),
        json!({"a": 1, "b": [2]}),
    ] {
        let mut module = document(SNAP_BOARDER);
        module["inputs"] = inert;
        accepted.push(module);
    }
    for module in &accepted {
        assert!(lower_rulespec_str(&yaml(module)).is_ok(), "{module}");
        assert!(
            schema.is_valid(module),
            "the schema must accept {}",
            module["inputs"]
        );
    }
    let edits: Vec<(&str, Edit)> = vec![
        (
            "lowercase actor",
            Box::new(|m| m["inputs"][1]["election"]["actor"] = json!("household")),
        ),
        (
            "spaced actor",
            Box::new(|m| m["inputs"][1]["election"]["actor"] = json!("Provider Household")),
        ),
        (
            "bad provision",
            Box::new(|m| m["inputs"][1]["election"]["provision"] = json!("7 CFR 273.1(b)(3)")),
        ),
        (
            "unknown key",
            Box::new(|m| m["inputs"][1]["election"]["default"] = json!(false)),
        ),
        (
            "missing actor",
            Box::new(|m| {
                m["inputs"][1]["election"]
                    .as_object_mut()
                    .unwrap()
                    .remove("actor");
            }),
        ),
        (
            "null election",
            Box::new(|m| m["inputs"][1]["election"] = Value::Null),
        ),
        (
            "unnamed entry",
            Box::new(|m| {
                m["inputs"][1].as_object_mut().unwrap().remove("name");
            }),
        ),
        (
            "non-string name",
            Box::new(|m| m["inputs"][1]["name"] = json!(7)),
        ),
        (
            "mapping form, bad actor",
            Box::new(|m| {
                m["inputs"] = mapping_form(&m["inputs"]);
                m["inputs"][ELECTION_SLOT]["election"]["actor"] = json!("household");
            }),
        ),
    ];
    for (case, edit) in edits {
        let mut module = document(SNAP_BOARDER);
        edit(&mut module);
        assert!(
            lower_rulespec_str(&yaml(&module)).is_err(),
            "{case}: the loader must refuse"
        );
        assert!(!schema.is_valid(&module), "{case}: the schema must refuse");
    }
}
