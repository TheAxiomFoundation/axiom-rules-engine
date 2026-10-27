//! Declaration names (`docs/rulespec.md`, "Declaration names").
//!
//! A bare name in a formula reads one thing: a built-in, a term, or the one
//! declaration of that name. RuleSpec lowering refuses, over a module and
//! everything it imports:
//!
//! * a parameter, derived rule, data relation, derived relation or input named
//!   `period_start`, `period_end`, `anchor`, `candidate` or a formula keyword
//!   such as `true` (`reserved_declaration_name`);
//! * one name declared with two kinds: a parameter and a derived rule, a
//!   relation and either, or an input and any of them
//!   (`declaration_name_clash`).
//!
//! The property states the invariant: no name a program can declare has two
//! meanings, or loses its bare read to a built-in or a term. A module passes
//! the check exactly when every declared name is unreserved and has one kind,
//! and then a bare read of each declared name, in every position where its
//! kind is read bare, lowers to a read of that declaration.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::rulespec::load_rulespec_with_source;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};

const RESERVED: [&str; 4] = ["period_start", "period_end", "anchor", "candidate"];

/// What a bare read of a declared name reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Parameter,
    Rule,
    Relation,
    Input,
}

/// Every way a RuleSpec module can declare a name a formula reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `kind: parameter` with a literal value.
    Parameter,
    /// `kind: parameter` with `values`.
    ParameterTable,
    /// `kind: derived` with no entity and a literal value.
    LiteralRule,
    /// `kind: derived` on `Person`.
    Rule,
    /// `kind: derived` with no entity and a computed value.
    EntityFreeRule,
    /// `kind: parameter` on `Person`.
    EntityParameter,
    DataRelation,
    DerivedRelation,
    /// An item of `inputs:`.
    Input,
}

const KINDS: [Kind; 9] = [
    Kind::Parameter,
    Kind::ParameterTable,
    Kind::LiteralRule,
    Kind::Rule,
    Kind::EntityFreeRule,
    Kind::EntityParameter,
    Kind::DataRelation,
    Kind::DerivedRelation,
    Kind::Input,
];

impl Kind {
    fn role(self) -> Role {
        match self {
            Kind::Parameter | Kind::ParameterTable | Kind::LiteralRule => Role::Parameter,
            Kind::Rule | Kind::EntityFreeRule | Kind::EntityParameter => Role::Rule,
            Kind::DataRelation | Kind::DerivedRelation => Role::Relation,
            Kind::Input => Role::Input,
        }
    }

    /// How the refusal names this kind.
    fn label(self) -> &'static str {
        match self {
            Kind::Parameter | Kind::ParameterTable => "a parameter",
            Kind::LiteralRule => {
                "a derived rule with no entity and a literal value (a parameter once lowered)"
            }
            Kind::Rule | Kind::EntityFreeRule => "a derived rule",
            Kind::EntityParameter => {
                "a parameter with an entity or a computed formula (a derived rule once lowered)"
            }
            Kind::DataRelation => "a data relation",
            Kind::DerivedRelation => "a derived relation",
            Kind::Input => "an input",
        }
    }

    /// The `rules:` entry declaring `name`, or the `inputs:` item for an input.
    fn declare(self, name: &str) -> String {
        let versions = |formula: &str| {
            format!(
                "    versions:\n      - effective_from: 2026-01-01\n        formula: {formula}\n"
            )
        };
        match self {
            Kind::Parameter => format!(
                "  - name: {name}\n    kind: parameter\n    dtype: Bool\n{}",
                versions("\"true\"")
            ),
            Kind::ParameterTable => format!(
                "  - name: {name}\n    kind: parameter\n    dtype: Integer\n    indexed_by: age\n    versions:\n      - effective_from: 2026-01-01\n        values:\n          0: 1\n"
            ),
            Kind::LiteralRule => format!(
                "  - name: {name}\n    kind: derived\n    dtype: Bool\n{}",
                versions("\"true\"")
            ),
            Kind::Rule => format!(
                "  - name: {name}\n    kind: derived\n    entity: Person\n    dtype: Judgment\n    period: Month\n{}",
                versions("age >= 18")
            ),
            Kind::EntityFreeRule => format!(
                "  - name: {name}\n    kind: derived\n    dtype: Integer\n{}",
                versions("1 + 1")
            ),
            Kind::EntityParameter => format!(
                "  - name: {name}\n    kind: parameter\n    entity: Person\n    dtype: Judgment\n    period: Month\n{}",
                versions("age >= 18")
            ),
            // Distinct slot entities, so a bare membership read has one
            // direction (a `[Person, Person]` data relation read bare is
            // refused as ambiguous, whatever its name).
            Kind::DataRelation => format!(
                "  - name: {name}\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n"
            ),
            Kind::DerivedRelation => format!(
                "  - name: {name}\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: relative_of\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n{}",
                versions("candidate.age > 0")
            ),
            Kind::Input => format!(
                "  - name: {name}\n    entity: Person\n    dtype: Boolean\n    period: Month\n"
            ),
        }
    }
}

/// A module with `relative_of` over `[Person, Person]` and `member_of` over
/// `[Person, Household]`, the declarations in
/// order (rules, then inputs), and `extra` rules.
fn module(declarations: &[(&str, Kind)], extra: &str) -> String {
    let mut rules = String::from(
        "  - name: relative_of\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Person]\n  - name: member_of\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n",
    );
    let mut inputs = String::new();
    for (name, kind) in declarations {
        if *kind == Kind::Input {
            inputs += &kind.declare(name);
        } else {
            rules += &kind.declare(name);
        }
    }
    rules += extra;
    let inputs = if inputs.is_empty() {
        String::new()
    } else {
        format!("inputs:\n{inputs}")
    };
    format!("format: rulespec/v1\n{inputs}rules:\n{rules}")
}

fn compile(source: &str) -> Result<Value, String> {
    let artifact = CompiledProgramArtifact::from_rulespec_str(source).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&artifact.program).expect("program serializes"))
}

fn reserved_refusal(name: &str, label: &str) -> String {
    format!(
        "[reserved_declaration_name] `{name}` is declared in `<memory>` as {label}, but the name is reserved"
    )
}

fn clash_refusal(name: &str, first: &str, second: &str) -> String {
    format!(
        "[declaration_name_clash] `{name}` is declared in `<memory>` as {first} and in `<memory>` as {second}; each bare `{name}` would read just one of them"
    )
}

fn is_declaration_refusal(error: &str) -> bool {
    error.contains("[reserved_declaration_name]") || error.contains("[declaration_name_clash]")
}

// ---------------------------------------------------------------------------
// Examples
// ---------------------------------------------------------------------------

/// The #269 review's probe: a Boolean parameter and a Person rule share
/// `enabled`. Bare reads went to the parameter while the rule still compiled
/// and could be requested. In either order, it is refused.
#[test]
fn a_parameter_and_a_rule_of_one_name_are_refused() {
    for (first, second) in [(Kind::Parameter, Kind::Rule), (Kind::Rule, Kind::Parameter)] {
        let error = compile(&module(&[("enabled", first), ("enabled", second)], ""))
            .expect_err("one name, two meanings");
        assert!(
            error.contains(&clash_refusal("enabled", first.label(), second.label())),
            "{error}"
        );
    }
}

/// Every pair of kinds sharing a name: refused when the two read differently,
/// and otherwise left to the checks that already handle a duplicate (a
/// compatible parameter merges; a second rule or relation fails to compile).
#[test]
fn every_pair_of_kinds_sharing_a_name_is_refused_when_their_reads_differ() {
    for first in KINDS {
        for second in KINDS {
            let result = compile(&module(&[("shared", first), ("shared", second)], ""));
            // Rules come before inputs in a module, so an input is reported second.
            let (earlier, later) = if first == Kind::Input {
                (second, first)
            } else {
                (first, second)
            };
            let case = format!("{first:?} then {second:?}");
            if first.role() == second.role() {
                if let Err(error) = &result {
                    assert!(!is_declaration_refusal(error), "{case}: {error}");
                }
            } else {
                let error = result.expect_err(&case);
                assert!(
                    error.contains(&clash_refusal("shared", earlier.label(), later.label())),
                    "{case}: {error}"
                );
            }
        }
    }
}

/// Each reserved name, declared as each kind, is refused before lowering.
#[test]
fn every_kind_of_declaration_is_refused_a_reserved_name() {
    for name in RESERVED {
        for kind in KINDS {
            let error =
                compile(&module(&[(name, kind)], "")).expect_err(&format!("{name} {kind:?}"));
            assert!(
                error.contains(&reserved_refusal(name, kind.label())),
                "{name} {kind:?}: {error}"
            );
        }
    }
}

/// A formula keyword is never read as a name: the lexer reads `true` and
/// `false` as literals and the rest as syntax. A parameter or rule could
/// never take one, because its name is written into the formula source, so
/// it is still refused: a parameter table by this check, which classifies
/// it without writing it, and any other parameter or rule by that one. A
/// relation or an input could take one, and is now refused as for a
/// built-in. Names are quoted, so YAML keeps `true` a string.
#[test]
fn a_formula_keyword_is_refused_as_a_declaration_name() {
    const KEYWORDS: [&str; 15] = [
        "entity", "amend", "from", "to", "match", "if", "elif", "else", "and", "or", "not", "true",
        "True", "false", "False",
    ];
    for name in KEYWORDS {
        let quoted = format!("'{name}'");
        for kind in KINDS {
            let error = compile(&module(&[(quoted.as_str(), kind)], ""))
                .expect_err(&format!("{name} {kind:?}"));
            let refused = match kind.role() {
                Role::Relation | Role::Input => error.contains(&reserved_refusal(name, kind.label())),
                Role::Parameter | Role::Rule => {
                    error.contains(&reserved_refusal(name, kind.label()))
                        || error.contains(&format!(
                            "RuleSpec rule {name:?} has name {name:?}; it must be exactly one identifier or path"
                        ))
                }
            };
            assert!(refused, "{name} {kind:?}: {error}");
        }
    }
}

/// The trap the #269 review found: a data relation named `anchor`, read
/// through `.` in a predicate, compiled to the anchor's own `age` once
/// `anchor` became a term. The declaration is now refused, so the predicate
/// can never silently change meaning.
#[test]
fn a_relation_named_after_a_term_is_refused_before_its_reads_change_meaning() {
    let extra = "  - name: aged_relative_of\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: relative_of\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n    versions:\n      - effective_from: 2026-01-01\n        formula: min(anchor.age) >= 3\n";
    let error = compile(&module(&[("anchor", Kind::DataRelation)], extra))
        .expect_err("a relation may not be named after a term");
    assert!(
        error.contains(&reserved_refusal("anchor", "a data relation")),
        "{error}"
    );
    // Without the declaration, `anchor` is the term, as documented.
    let program = compile(&module(&[], extra)).expect("the term reads the anchor's age");
    let predicate = program["relations"]
        .as_array()
        .expect("relations")
        .iter()
        .find(|relation| relation["name"] == "aged_relative_of")
        .expect("relation")["derivation"]["predicate"]
        .clone();
    assert_eq!(
        predicate["left"]["items"][0],
        json!({"kind": "role_input", "term": "anchor", "name": "age"})
    );
}

/// A source relation is a provenance record that no formula reads by name, so
/// its name is not checked: it may share a rule's name or take a reserved one.
#[test]
fn a_source_relation_name_is_not_checked() {
    let cites = |name: &str| {
        format!(
            "  - name: {name}\n    kind: source_relation\n    source_relation:\n      type: cites\n      target: us:statutes/7/2014#shared\n"
        )
    };
    for name in ["shared", "anchor", "period_start"] {
        compile(&module(&[("shared", Kind::Rule)], &cites(name)))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

/// An undeclared name is not a declaration: a bare `anchor` in a rule body
/// still reads an input of that name, as `docs/rulespec.md` documents.
#[test]
fn an_undeclared_reserved_name_keeps_its_documented_reads() {
    let extra = "  - name: uses_anchor\n    kind: derived\n    entity: Person\n    dtype: Integer\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: anchor + 1\n";
    let program = compile(&module(&[], extra)).expect("compiles");
    let rule = program["derived"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|rule| rule["name"] == "uses_anchor")
        .expect("rule")
        .clone();
    assert_eq!(
        rule["expr"]["items"][0],
        json!({"kind": "input", "name": "anchor"})
    );
}

/// A `parameter` record that lowers to a rule (one with an entity, or a
/// computed formula) is a rule in a predicate, bare and through a term, as it
/// is in a rule body. Before, a predicate read each of these as an input
/// (`input`, `role_input`) while a rule body read the rule.
#[test]
fn a_parameter_that_lowers_to_a_rule_is_a_rule_in_a_predicate() {
    let computed = "  - name: computed\n    kind: parameter\n    dtype: Integer\n    versions:\n      - effective_from: 2026-01-01\n        formula: 1 + 1\n";
    let tested = "  - name: tested\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: relative_of\n      slot_entities: [Person, Person]\n      current_slot: 0\n      related_slot: 1\n    versions:\n      - effective_from: 2026-01-01\n        formula: level and candidate.level and computed > 1\n";
    let extra = format!("{computed}{tested}");
    let program = compile(&module(&[("level", Kind::EntityParameter)], &extra)).expect("compiles");
    let predicate = &find(&program["relations"], "tested")["derivation"]["predicate"];
    assert_eq!(
        predicate,
        &json!({"kind": "and", "items": [
            {"kind": "and", "items": [
                {"kind": "derived", "name": "level"},
                {"kind": "role_derived", "term": "candidate", "name": "level"},
            ]},
            {"kind": "comparison", "left": {"kind": "derived", "name": "computed"}, "op": "gt",
             "right": {"kind": "literal", "value": {"kind": "integer", "value": 1}}},
        ]})
    );
    assert_eq!(find(&program["derived"], "level")["entity"], "Person");
}

struct InMemoryModuleSource(HashMap<String, String>);

impl ModuleSource for InMemoryModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).cloned())
    }
}

/// The corpus case: a state module declares a parameter table under the name
/// of a federal derived rule it imports. The clash spans modules, and the
/// refusal names both.
const FEDERAL: &str = r#"
format: rulespec/v1
inputs:
  - name: household_size
    entity: Household
    dtype: Integer
    period: Month
rules:
  - name: snap_standard_deduction_48_states_dc
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: "209"
  - name: snap_standard_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_standard_deduction_48_states_dc
"#;

const STATE: &str = r#"
format: rulespec/v1
imports:
  - us:policies/usda/snap/deductions
rules:
  - name: snap_standard_deduction
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2025-10-01
        values:
          1: 142
          6: 205
"#;

#[test]
fn a_clash_across_imports_is_refused_and_names_both_modules() {
    let source = InMemoryModuleSource(HashMap::from([
        (
            "us:policies/usda/snap/deductions".to_string(),
            FEDERAL.to_string(),
        ),
        (
            "us-tn:regulations/snap/standard-deduction".to_string(),
            STATE.to_string(),
        ),
    ]));
    let error = load_rulespec_with_source("us-tn:regulations/snap/standard-deduction", &source)
        .expect_err("the state table shadows the federal rule")
        .to_string();
    assert!(
        error.contains(
            "[declaration_name_clash] `snap_standard_deduction` is declared in `us:policies/usda/snap/deductions` as a derived rule and in `us-tn:regulations/snap/standard-deduction` as a parameter"
        ),
        "{error}"
    );
    // The federal module alone has one meaning per name.
    load_rulespec_with_source("us:policies/usda/snap/deductions", &source)
        .expect("the federal module compiles alone");

    // An imported input and a local parameter clash the same way.
    let state_input = STATE.replace("snap_standard_deduction", "household_size");
    let source = InMemoryModuleSource(HashMap::from([
        (
            "us:policies/usda/snap/deductions".to_string(),
            FEDERAL.to_string(),
        ),
        (
            "us-tn:regulations/snap/standard-deduction".to_string(),
            state_input,
        ),
    ]));
    let error = load_rulespec_with_source("us-tn:regulations/snap/standard-deduction", &source)
        .expect_err("an input and a parameter share a name")
        .to_string();
    assert!(
        error.contains(
            "[declaration_name_clash] `household_size` is declared in `us-tn:regulations/snap/standard-deduction` as a parameter and in `us:policies/usda/snap/deductions` as an input"
        ),
        "{error}"
    );
}

static NONCE: AtomicU64 = AtomicU64::new(0);

/// The CLI reports the refusal and writes no artifact.
#[test]
fn the_cli_refuses_a_clash_and_writes_no_artifact() {
    let temp: PathBuf = std::env::temp_dir()
        .canonicalize()
        .expect("system temp directory has an exact path")
        .join(format!(
            "axiom-rules-engine-declaration-names-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
    let root = temp.join("rulespec-us");
    let federal = root.join("us/policies/usda/snap/deductions.yaml");
    let state = root.join("us-tn/regulations/snap/standard-deduction.yaml");
    for (path, text) in [(&federal, FEDERAL), (&state, STATE)] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("module dir");
        std::fs::write(path, text).expect("module");
    }
    let artifact = temp.join("program.compiled.json");
    let output = Command::new(env!("CARGO_BIN_EXE_axiom-rules-engine"))
        .args(["compile", "--program"])
        .arg(&state)
        .arg("--rulespec-root")
        .arg(&root)
        .arg("--output")
        .arg(&artifact)
        .output()
        .expect("CLI runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("[declaration_name_clash] `snap_standard_deduction`"),
        "{stderr}"
    );
    assert!(!artifact.exists());
    std::fs::remove_dir_all(&temp).expect("temp dir removed");
}

// ---------------------------------------------------------------------------
// Property
// ---------------------------------------------------------------------------

/// Names a generated module may declare: two ordinary ones and every reserved
/// one.
const POOL: [&str; 6] = [
    "flag",
    "level",
    "period_start",
    "period_end",
    "anchor",
    "candidate",
];

/// The refusal the check must give for `declarations`, in module order (rules,
/// then inputs): the first declaration that takes a reserved name or differs
/// in kind from an earlier declaration of its name.
fn expected_refusal(declarations: &[(&str, Kind)]) -> Option<String> {
    let ordered = declarations
        .iter()
        .filter(|(_, kind)| *kind != Kind::Input)
        .chain(declarations.iter().filter(|(_, kind)| *kind == Kind::Input));
    let mut first = BTreeMap::<&str, Kind>::new();
    for (name, kind) in ordered {
        if RESERVED.contains(name) {
            return Some(reserved_refusal(name, kind.label()));
        }
        match first.get(name) {
            Some(earlier) if earlier.role() != kind.role() => {
                return Some(clash_refusal(name, earlier.label(), kind.label()));
            }
            Some(_) => {}
            None => {
                first.insert(name, *kind);
            }
        }
    }
    None
}

/// Probes reading `name` bare: a Judgment rule, a Bool rule (scalar
/// position) and a derived relation's predicate, each named after `index`.
/// A data relation's predicate probe runs over `member_of`, whose slots
/// match the relation's, so the membership read has one direction.
fn probes(index: usize, name: &str, kind: Kind) -> String {
    let rule = |probe: &str, dtype: &str| {
        format!(
            "  - name: {probe}_{index}\n    kind: derived\n    entity: Person\n    dtype: {dtype}\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: {name}\n"
        )
    };
    let (source, slots) = if kind == Kind::DataRelation {
        ("member_of", "Person, Household")
    } else {
        ("relative_of", "Person, Person")
    };
    let mut probes = format!(
        "  - name: predicate_probe_{index}\n    kind: derived_relation\n    derived_relation:\n      arity: 2\n      source_relation: {source}\n      slot_entities: [{slots}]\n      current_slot: 0\n      related_slot: 1\n    versions:\n      - effective_from: 2026-01-01\n        formula: {name}\n"
    );
    // A relation is read bare only as membership, in a predicate.
    if kind.role() != Role::Relation {
        probes += &rule("judgment_probe", "Judgment");
        probes += &rule("value_probe", "Bool");
    }
    probes
}

fn parameter_lookup(name: &str) -> Value {
    json!({"kind": "parameter_lookup", "parameter": name,
           "index": {"kind": "literal", "value": {"kind": "integer", "value": 0}}})
}

fn is_true(read: Value) -> Value {
    json!({"kind": "comparison", "left": read, "op": "eq",
           "right": {"kind": "literal", "value": {"kind": "bool", "value": true}}})
}

/// What a bare read of `name`, declared once with `role`, lowers to: as a
/// judgment, and as a value.
fn expected_reads(name: &str, role: Role) -> (Value, Option<Value>) {
    match role {
        Role::Parameter => (
            is_true(parameter_lookup(name)),
            Some(parameter_lookup(name)),
        ),
        Role::Rule => (
            json!({"kind": "derived", "name": name}),
            Some(json!({"kind": "derived", "name": name})),
        ),
        Role::Input => (
            is_true(json!({"kind": "input", "name": name})),
            Some(json!({"kind": "input", "name": name})),
        ),
        Role::Relation => (json!({"kind": "relation_member"}), None),
    }
}

fn find<'a>(items: &'a Value, name: &str) -> &'a Value {
    items
        .as_array()
        .expect("a list")
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}`"))
}

/// Declarations for one generated module: per name, up to two kinds of
/// distinct roles, so a name never has two declarations of one kind.
fn declarations() -> impl Strategy<Value = Vec<(&'static str, Kind)>> {
    let kinds_of_one_name = prop_oneof![
        3 => Just(Vec::new()),
        3 => prop::sample::select(KINDS.to_vec()).prop_map(|kind| vec![kind]),
        1 => (prop::sample::select(KINDS.to_vec()), prop::sample::select(KINDS.to_vec()))
            .prop_filter("two roles", |(a, b)| a.role() != b.role())
            .prop_map(|(a, b)| vec![a, b]),
    ];
    // Reserved names take part in a third of modules, so most modules reach
    // the read checks.
    (
        prop::collection::vec(kinds_of_one_name, POOL.len()),
        prop::bool::weighted(0.33),
    )
        .prop_flat_map(|(kinds, reserved)| {
            let declarations = POOL
                .iter()
                .zip(kinds)
                .filter(|(name, _)| reserved || !RESERVED.contains(name))
                .flat_map(|(name, kinds)| kinds.into_iter().map(move |kind| (*name, kind)))
                .collect::<Vec<_>>();
            Just(declarations).prop_shuffle()
        })
}

#[test]
fn every_declared_name_has_one_meaning_and_keeps_its_bare_read() {
    let mut runner = TestRunner::new(Config {
        cases: 256,
        ..Config::default()
    });
    // `run` takes an `Fn`, so the outcome counts sit in a `RefCell`.
    let counts = std::cell::RefCell::new(BTreeMap::<&str, usize>::new());
    runner
        .run(&declarations(), |declarations| {
            let mut extra = String::new();
            // An accepted module declares each name once, so its kind
            // fixes its reads; a refused one's probes only need to parse.
            let mut declared = BTreeMap::<&str, Kind>::new();
            for (name, kind) in &declarations {
                declared.insert(name, *kind);
            }
            for (index, (name, kind)) in declared.iter().enumerate() {
                extra += &probes(index, name, *kind);
            }
            let result = compile(&module(&declarations, &extra));
            match expected_refusal(&declarations) {
                Some(refusal) => {
                    let error = result.expect_err("the check refuses");
                    prop_assert!(error.contains(&refusal), "{refusal}\n{error}");
                    *counts
                        .borrow_mut()
                        .entry(if refusal.contains("reserved") {
                            "reserved"
                        } else {
                            "clash"
                        })
                        .or_default() += 1;
                }
                None => {
                    let program = result.map_err(|error| {
                        TestCaseError::fail(format!("{declarations:?}: {error}"))
                    })?;
                    for (index, (name, kind)) in declared.iter().enumerate() {
                        let role = kind.role();
                        let (judgment, value) = expected_reads(name, role);
                        let predicate = &find(
                            &program["relations"],
                            &format!("predicate_probe_{index}"),
                        )["derivation"]["predicate"];
                        if role == Role::Relation {
                            prop_assert_eq!(&predicate["kind"], &judgment["kind"]);
                            prop_assert_eq!(&predicate["relation"], &json!(name));
                        } else {
                            prop_assert_eq!(predicate, &judgment, "{} predicate", name);
                            let rule = &find(
                                &program["derived"],
                                &format!("judgment_probe_{index}"),
                            )["expr"];
                            prop_assert_eq!(rule, &judgment, "{} judgment", name);
                            let rule =
                                &find(&program["derived"], &format!("value_probe_{index}"))["expr"];
                            prop_assert_eq!(Some(rule), value.as_ref(), "{} value", name);
                        }
                        *counts.borrow_mut().entry("read").or_default() += 1;
                    }
                    *counts.borrow_mut().entry("accepted").or_default() += 1;
                }
            }
            Ok(())
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    // Each outcome occurred, so no branch passed vacuously.
    let counts = counts.into_inner();
    for outcome in ["accepted", "read", "reserved", "clash"] {
        assert!(counts.get(outcome).is_some_and(|n| *n > 0), "{counts:?}");
    }
}
