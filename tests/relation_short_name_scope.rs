//! A relation name that a module reads without declaring it (#226).
//!
//! RuleSpec scopes a declared relation by its module: `member_of_household`
//! declared in `us:policies/e1` is the relation
//! `us:policies/e1#relation.member_of_household`, and a dataset supplies its
//! tuples under that legal id. A module that reads the short name without
//! declaring it must read the declaration its imports supply, or compiling
//! must refuse and name the candidates. It must never read a separate,
//! formula-synthesized relation of the same short name, which no dataset
//! tuple reaches.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use axiom_rules_engine::api::{
    ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
};
use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::rulespec::CanonicalRuleSpecRoots;
use axiom_rules_engine::source::{ModuleSource, SourceError};
use axiom_rules_engine::spec::{
    DatasetSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, RelationRecordSpec, ScalarValueSpec,
};

struct Modules(HashMap<String, String>);

impl Modules {
    fn new(modules: &[(&str, &str)]) -> Self {
        Self(
            modules
                .iter()
                .map(|(target, text)| ((*target).to_string(), (*text).to_string()))
                .collect(),
        )
    }
}

impl ModuleSource for Modules {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.0.get(target).cloned())
    }
}

fn compile(modules: &[(&str, &str)], root: &str) -> Result<CompiledProgramArtifact, CompileError> {
    CompiledProgramArtifact::from_rulespec_with_source(root, &Modules::new(modules))
}

const E1: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: household_size_a
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;

const E1_TARGET: &str = "us:policies/e1";
const E1_RELATION: &str = "us:policies/e1#relation.member_of_household";

/// A second declaration of the same short name in another module.
const E1B: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
"#;

const E1B_TARGET: &str = "us:policies/e1b";
const E1B_RELATION: &str = "us:policies/e1b#relation.member_of_household";

const E2: &str = r#"
format: rulespec/v1
imports:
  - us:policies/e1
rules:
  - name: household_size_b
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;

const E2_TARGET: &str = "us:policies/e2";

fn period() -> PeriodSpec {
    PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: "2026-01-01".parse().expect("valid date"),
        end: "2026-01-31".parse().expect("valid date"),
    }
}

/// `count` tuples of `relation`, each a distinct person in `household-1`.
fn members(relation: &str, count: usize) -> Vec<RelationRecordSpec> {
    let period = period();
    (0..count)
        .map(|index| RelationRecordSpec {
            name: relation.to_string(),
            tuple: vec![
                format!("person-{relation}-{index}"),
                "household-1".to_string(),
            ],
            interval: IntervalSpec {
                start: period.start,
                end: period.end,
            },
        })
        .collect()
}

/// An integer-dtype output's value. Arithmetic over a count can carry the
/// value as an integral decimal.
fn integer(output: &OutputValue) -> i64 {
    match output {
        OutputValue::Scalar {
            value: ScalarValueSpec::Integer { value },
            ..
        } => *value,
        OutputValue::Scalar {
            value: ScalarValueSpec::Decimal { value },
            ..
        } => value
            .parse()
            .unwrap_or_else(|_| panic!("expected an integral value, got {value}")),
        other => panic!("expected an integer output, got {other:?}"),
    }
}

/// Run `outputs` for `household-1` in `mode` and return each as an integer.
fn run(
    artifact: &CompiledProgramArtifact,
    mode: ExecutionMode,
    relations: Vec<RelationRecordSpec>,
    outputs: &[&str],
) -> BTreeMap<String, i64> {
    let response = execute_request(ExecutionRequest {
        relation_binding: Default::default(),
        mode,
        program: artifact.program.clone(),
        dataset: DatasetSpec {
            inputs: vec![],
            relations,
        },
        queries: vec![ExecutionQuery {
            assessment_date: None,
            entity_id: "household-1".to_string(),
            period: period(),
            outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
        }],
    })
    .expect("the request executes");
    response.results[0]
        .outputs
        .iter()
        .map(|(name, output)| (name.clone(), integer(output)))
        .collect()
}

fn assert_counts(
    artifact: &CompiledProgramArtifact,
    relations: Vec<RelationRecordSpec>,
    expected: &[(&str, i64)],
) {
    let outputs = expected.iter().map(|(name, _)| *name).collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(|(name, count)| ((*name).to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        assert_eq!(
            run(artifact, mode.clone(), relations.clone(), &outputs),
            expected,
            "{mode:?}"
        );
    }
}

fn relation_names(artifact: &CompiledProgramArtifact) -> Vec<&str> {
    artifact
        .program
        .relations
        .iter()
        .map(|relation| relation.name.as_str())
        .collect()
}

/// The example from #226: `e2` imports `e1` and aggregates `e1`'s relation by
/// its short name. Tuples supplied under `e1`'s legal id reach both rules.
#[test]
fn an_undeclared_short_name_reads_the_one_declaration_its_imports_supply() {
    let artifact = compile(&[(E1_TARGET, E1), (E2_TARGET, E2)], E2_TARGET)
        .expect("the short name binds to e1's declaration");
    assert_eq!(relation_names(&artifact), [E1_RELATION]);
    assert_counts(
        &artifact,
        members(E1_RELATION, 2),
        &[
            ("us:policies/e1#household_size_a", 2),
            ("us:policies/e2#household_size_b", 2),
        ],
    );
}

/// The import closure is transitive: `e4` imports `e2`, which imports `e1`.
#[test]
fn a_declaration_reached_through_an_import_of_an_import_binds() {
    let e4 = r#"
format: rulespec/v1
imports:
  - us:policies/e2
rules:
  - name: household_size_d
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household) + 10
"#;
    let artifact = compile(
        &[(E1_TARGET, E1), (E2_TARGET, E2), ("us:policies/e4", e4)],
        "us:policies/e4",
    )
    .expect("the short name binds through the transitive closure");
    assert_eq!(relation_names(&artifact), [E1_RELATION]);
    assert_counts(
        &artifact,
        members(E1_RELATION, 3),
        &[("us:policies/e4#household_size_d", 13)],
    );
}

/// Two declarations of the short name are in scope, so the name does not say
/// whose tuples to read. Compiling refuses and names both.
#[test]
fn an_ambiguous_short_name_is_refused_naming_every_declaration() {
    let e3 = r#"
format: rulespec/v1
imports:
  - us:policies/e1
  - us:policies/e1b
rules:
  - name: household_size_c
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;
    let error = compile(
        &[(E1_TARGET, E1), (E1B_TARGET, E1B), ("us:policies/e3", e3)],
        "us:policies/e3",
    )
    .expect_err("two declarations are in scope");
    let message = error.to_string();
    assert!(
        message.contains("[ambiguous_relation_reference]"),
        "{message}"
    );
    assert!(
        message.contains("us:policies/e3#household_size_c"),
        "{message}"
    );
    assert!(
        message.contains(&format!("`{E1_RELATION}`"))
            && message.contains(&format!("`{E1B_RELATION}`")),
        "{message}"
    );
}

/// Each module resolves the short name in its own imports. The root imports
/// `e2` (which imports `e1`) and, separately, `e1b`: two declarations are in
/// the program, but only `e1`'s is in `e2`'s closure, so `e2` reads `e1`'s
/// tuples and none of `e1b`'s.
#[test]
fn a_module_reads_the_declaration_in_its_own_imports() {
    let root = r#"
format: rulespec/v1
imports:
  - us:policies/e2
  - us:policies/e1b
rules: []
"#;
    let artifact = compile(
        &[
            (E1_TARGET, E1),
            (E1B_TARGET, E1B),
            (E2_TARGET, E2),
            ("us:policies/root", root),
        ],
        "us:policies/root",
    )
    .expect("e2's closure holds one declaration");
    let mut relations = members(E1_RELATION, 2);
    relations.extend(members(E1B_RELATION, 5));
    assert_counts(
        &artifact,
        relations,
        &[
            ("us:policies/e1#household_size_a", 2),
            ("us:policies/e2#household_size_b", 2),
        ],
    );
}

/// A module whose imports declare no relation of the name reads the one
/// declaration elsewhere in the program, as relation atoms, `exists` and
/// `count_where` over an imported rule already did. `e5` imports nothing; the
/// root imports `e1` and `e5`, so `e1`'s is the program's only declaration.
#[test]
fn a_read_whose_imports_declare_nothing_binds_the_program_declaration() {
    let e5 = r#"
format: rulespec/v1
rules:
  - name: household_size_e5
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;
    let root = r#"
format: rulespec/v1
imports:
  - us:policies/e1
  - us:policies/e5
rules: []
"#;
    let artifact = compile(
        &[
            (E1_TARGET, E1),
            ("us:policies/e5", e5),
            ("us:policies/root", root),
        ],
        "us:policies/root",
    )
    .expect("the program declares the name once");
    assert_eq!(relation_names(&artifact), [E1_RELATION]);
    assert_counts(
        &artifact,
        members(E1_RELATION, 4),
        &[("us:policies/e5#household_size_e5", 4)],
    );

    // A second declaration elsewhere makes the read ambiguous.
    let root = r#"
format: rulespec/v1
imports:
  - us:policies/e1
  - us:policies/e1b
  - us:policies/e5
rules: []
"#;
    let message = compile(
        &[
            (E1_TARGET, E1),
            (E1B_TARGET, E1B),
            ("us:policies/e5", e5),
            ("us:policies/root", root),
        ],
        "us:policies/root",
    )
    .expect_err("two declarations are in the program")
    .to_string();
    assert!(
        message.contains("[ambiguous_relation_reference] `us:policies/e5#household_size_e5`")
            && message.contains("neither its module nor its imports declare"),
        "{message}"
    );
}

/// A module's own declaration of the name wins over an imported one, as
/// before: the module reads its own tuples.
#[test]
fn a_local_declaration_shadows_an_imported_one() {
    let local = r#"
format: rulespec/v1
imports:
  - us:policies/e1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: household_size_local
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;
    let artifact = compile(
        &[(E1_TARGET, E1), ("us:policies/local", local)],
        "us:policies/local",
    )
    .expect("the local declaration is typed");
    let mut relations = members(E1_RELATION, 2);
    relations.extend(members("us:policies/local#relation.member_of_household", 4));
    assert_counts(
        &artifact,
        relations,
        &[
            ("us:policies/e1#household_size_a", 2),
            ("us:policies/local#household_size_local", 4),
        ],
    );
}

const PEOPLE: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: person_weight
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: weight
  - name: counts_as_member
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: weight > 0
"#;

const PEOPLE_RELATION: &str = "us:policies/people#relation.member_of_household";

/// Every aggregation form reads the imported declaration, whether or not its
/// `where` clause or value reads an imported rule.
#[test]
fn every_aggregation_form_reads_the_imported_declaration() {
    let household = r#"
format: rulespec/v1
imports:
  - us:policies/people
rules:
  - name: local_counts_as_member
    kind: derived
    entity: Person
    dtype: Judgment
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: person_weight > 1
  - name: local_weight
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: person_weight * 10
  - name: by_len
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
  - name: by_count_where_imported
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, counts_as_member)
  - name: by_count_where_local
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, local_counts_as_member)
  - name: by_sum_imported
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.person_weight)
  - name: by_sum_local
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.local_weight)
  - name: by_sum_where_local
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: sum_where(member_of_household, local_weight, local_counts_as_member)
  - name: by_max_local
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if len(member_of_household) > 0: max(member_of_household.local_weight)
          else: 0
"#;
    let artifact = compile(
        &[
            ("us:policies/people", PEOPLE),
            ("us:policies/household", household),
        ],
        "us:policies/household",
    )
    .expect("every form binds to the imported declaration");
    assert_eq!(relation_names(&artifact), [PEOPLE_RELATION]);

    let period = period();
    let weights = [0_i64, 1, 2, 3];
    let relations = members(PEOPLE_RELATION, weights.len());
    let inputs = relations
        .iter()
        .zip(weights)
        .map(
            |(tuple, weight)| axiom_rules_engine::spec::InputRecordSpec {
                name: "us:policies/people#input.weight".to_string(),
                entity: "Person".to_string(),
                entity_id: tuple.tuple[0].clone(),
                interval: IntervalSpec {
                    start: period.start,
                    end: period.end,
                },
                value: ScalarValueSpec::Integer { value: weight },
            },
        )
        .collect::<Vec<_>>();
    let outputs = [
        ("by_len", 4),
        ("by_count_where_imported", 3),
        ("by_count_where_local", 2),
        ("by_sum_imported", 6),
        ("by_sum_local", 60),
        ("by_sum_where_local", 50),
        ("by_max_local", 30),
    ];
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        let response = execute_request(ExecutionRequest {
            relation_binding: Default::default(),
            mode: mode.clone(),
            program: artifact.program.clone(),
            dataset: DatasetSpec {
                inputs: inputs.clone(),
                relations: relations.clone(),
            },
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "household-1".to_string(),
                period: period.clone(),
                outputs: outputs
                    .iter()
                    .map(|(name, _)| format!("us:policies/household#{name}"))
                    .collect(),
            }],
        })
        .expect("the request executes");
        for (name, expected) in outputs {
            let output = &response.results[0].outputs[&format!("us:policies/household#{name}")];
            assert_eq!(integer(output), expected, "{name} in {mode:?}");
        }
    }
}

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// A fresh directory holding a `rulespec-us` checkout and a separate
/// directory for generated compositions, as `compile-composed` requires.
struct Checkout {
    temp: PathBuf,
    root: PathBuf,
}

impl Checkout {
    fn new(modules: &[(&str, &str)]) -> Self {
        let temp = std::env::temp_dir()
            .canonicalize()
            .expect("system temp directory has an exact path")
            .join(format!(
                "axiom-rules-engine-short-name-{}-{}",
                std::process::id(),
                TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::remove_dir_all(&temp).ok();
        let root = temp.join("rulespec-us");
        for (target, text) in modules {
            let relative = target
                .strip_prefix("us:")
                .expect("test modules are under us:");
            let path = root.join("us").join(format!("{relative}.yaml"));
            std::fs::create_dir_all(path.parent().expect("module has a parent"))
                .expect("create module directory");
            std::fs::write(&path, text).expect("write module");
        }
        Self { temp, root }
    }

    fn composition(&self, text: &str) -> PathBuf {
        let path = self.temp.join("generated/composition.yaml");
        std::fs::create_dir_all(path.parent().expect("composition has a parent"))
            .expect("create composition directory");
        std::fs::write(&path, text).expect("write composition");
        path
    }

    fn compile_composed(&self, path: &Path) -> Result<CompiledProgramArtifact, CompileError> {
        let roots = CanonicalRuleSpecRoots::new([&self.root]).expect("canonical root");
        CompiledProgramArtifact::from_composed_rulespec_file(path, &roots)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.temp).ok();
    }
}

fn composition(imports: &[&str], formula: &str) -> String {
    let imports = imports
        .iter()
        .map(|import| format!("  - {import}\n"))
        .collect::<String>();
    format!(
        r#"
format: rulespec/v1
module:
  kind: composition
  summary: Test composition.
imports:
{imports}rules:
  - name: household_size_root
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: {formula}
"#
    )
}

/// An originless composition root resolves the short name the same way: its
/// imports hold one declaration, so the root reads that declaration's tuples.
#[test]
fn a_composition_root_reads_the_one_imported_declaration() {
    let checkout = Checkout::new(&[(E1_TARGET, E1)]);
    let path = checkout.composition(&composition(&[E1_TARGET], "len(member_of_household)"));
    let artifact = checkout
        .compile_composed(&path)
        .expect("the root's short name binds to e1's declaration");
    assert_eq!(relation_names(&artifact), [E1_RELATION]);
    assert_counts(
        &artifact,
        members(E1_RELATION, 3),
        &[
            ("us:policies/e1#household_size_a", 3),
            ("household_size_root", 3),
        ],
    );
}

/// A composition root whose imports declare the name twice is refused, naming
/// both declarations.
#[test]
fn a_composition_root_refuses_an_ambiguous_short_name() {
    let checkout = Checkout::new(&[(E1_TARGET, E1), (E1B_TARGET, E1B)]);
    let path = checkout.composition(&composition(
        &[E1_TARGET, E1B_TARGET],
        "len(member_of_household)",
    ));
    let error = checkout
        .compile_composed(&path)
        .expect_err("two declarations are in scope");
    let message = error.to_string();
    assert!(
        message.contains("[ambiguous_relation_reference]"),
        "{message}"
    );
    assert!(message.contains("`household_size_root`"), "{message}");
    assert!(
        message.contains(&format!("`{E1_RELATION}`"))
            && message.contains(&format!("`{E1B_RELATION}`")),
        "{message}"
    );
}

/// A composition root's own declaration of the name keeps its bare name and
/// wins over an imported declaration.
#[test]
fn a_composition_root_reads_its_own_declaration_first() {
    let checkout = Checkout::new(&[(E1_TARGET, E1)]);
    let text = composition(&[E1_TARGET], "len(member_of_household)").replace(
        "rules:\n",
        "rules:\n  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n",
    );
    let path = checkout.composition(&text);
    let artifact = checkout
        .compile_composed(&path)
        .expect("the root's own declaration is typed");
    let mut relations = members(E1_RELATION, 2);
    relations.extend(members("member_of_household", 6));
    assert_counts(
        &artifact,
        relations,
        &[
            ("us:policies/e1#household_size_a", 2),
            ("household_size_root", 6),
        ],
    );
}

/// The root's own declaration is not in an imported module's scope. `e2`
/// imports `e1` and reads `member_of_household` without declaring it, and the
/// root declares a bare `member_of_household` of its own. Before #226 `e2`'s
/// read stayed on the bare short name, so it read the root's relation and
/// returned zero for tuples supplied under `e1`'s legal id, with no error.
#[test]
fn an_imported_module_never_reads_the_composition_root_declaration() {
    let checkout = Checkout::new(&[(E1_TARGET, E1), (E2_TARGET, E2)]);
    let text = composition(&[E2_TARGET], "len(member_of_household)").replace(
        "rules:\n",
        "rules:\n  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n",
    );
    let path = checkout.composition(&text);
    let artifact = checkout
        .compile_composed(&path)
        .expect("each module's read is typed");
    let mut relations = members(E1_RELATION, 2);
    relations.extend(members("member_of_household", 6));
    assert_counts(
        &artifact,
        relations.clone(),
        &[
            ("us:policies/e1#household_size_a", 2),
            ("us:policies/e2#household_size_b", 2),
            ("household_size_root", 6),
        ],
    );
    // With only e1's tuples, e2 still sees them and the root sees none.
    assert_counts(
        &artifact,
        members(E1_RELATION, 2),
        &[
            ("us:policies/e2#household_size_b", 2),
            ("household_size_root", 0),
        ],
    );
}

// ---------------------------------------------------------------------------
// Property: over generated module graphs, a compile that succeeds gives every
// aggregate the tuples supplied under the legal id of the declaration it
// resolves to, and explain, fast and dense agree.
// ---------------------------------------------------------------------------

use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseOutputValue, DenseRelationBatchSpec,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use std::collections::BTreeSet;

/// How a module's rule reads `member_of_household`.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// `len(member_of_household)`.
    Len,
    /// `sum(member_of_household.unit_weight_<module>)`, where the module's
    /// own Person rule is `1`: a related-derived sum that counts members.
    SumOfLocalRule,
}

/// One module of a generated graph.
#[derive(Clone, Debug)]
struct GeneratedModule {
    /// Indices of earlier modules this one imports.
    imports: Vec<usize>,
    declares: bool,
    reads: Option<Form>,
}

#[derive(Clone, Debug)]
struct Graph {
    modules: Vec<GeneratedModule>,
    /// The root: atomic module `us:policies/root`, or an originless
    /// composition.
    root: GeneratedModule,
    composition: bool,
}

fn form() -> impl Strategy<Value = Form> {
    prop_oneof![Just(Form::Len), Just(Form::SumOfLocalRule)]
}

fn graph() -> impl Strategy<Value = Graph> {
    (2usize..=5)
        .prop_flat_map(|count| {
            (
                proptest::collection::vec(
                    (any::<u8>(), any::<bool>(), proptest::option::of(form())),
                    count,
                ),
                (any::<u8>(), any::<bool>(), proptest::option::of(form())),
                any::<bool>(),
            )
        })
        .prop_map(
            |(modules, (root_imports, root_declares, root_reads), composition)| {
                let count = modules.len();
                let modules = modules
                    .into_iter()
                    .enumerate()
                    .map(|(index, (imports, declares, reads))| GeneratedModule {
                        imports: (0..index).filter(|j| imports & (1 << j) != 0).collect(),
                        declares,
                        reads,
                    })
                    .collect();
                let mut imports = (0..count)
                    .filter(|j| root_imports & (1 << j) != 0)
                    .collect::<Vec<_>>();
                if imports.is_empty() {
                    imports.push(count - 1);
                }
                Graph {
                    modules,
                    root: GeneratedModule {
                        imports,
                        declares: root_declares,
                        reads: root_reads,
                    },
                    composition,
                }
            },
        )
}

/// A module in a generated program: one of the graph's modules, or the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Module(usize),
    Root,
}

impl Graph {
    fn module(&self, node: Node) -> &GeneratedModule {
        match node {
            Node::Module(index) => &self.modules[index],
            Node::Root => &self.root,
        }
    }

    fn target(&self, node: Node) -> Option<String> {
        match node {
            Node::Module(index) => Some(format!("us:policies/g{index}")),
            Node::Root if self.composition => None,
            Node::Root => Some("us:policies/root".to_string()),
        }
    }

    fn suffix(node: Node) -> String {
        match node {
            Node::Module(index) => format!("g{index}"),
            Node::Root => "root".to_string(),
        }
    }

    /// The name a dataset supplies the node's declaration under.
    fn relation(&self, node: Node) -> String {
        self.target(node).map_or_else(
            || "member_of_household".to_string(),
            |target| format!("{target}#relation.member_of_household"),
        )
    }

    /// The id a query names the node's reading rule by.
    fn output(&self, node: Node) -> String {
        let name = format!("size_{}", Self::suffix(node));
        self.target(node)
            .map_or_else(|| name.clone(), |target| format!("{target}#{name}"))
    }

    /// The modules the node imports, directly or transitively.
    fn closure(&self, node: Node) -> BTreeSet<usize> {
        let mut closure = BTreeSet::new();
        let mut pending = self.module(node).imports.clone();
        while let Some(index) = pending.pop() {
            if closure.insert(index) {
                pending.extend(self.modules[index].imports.iter().copied());
            }
        }
        closure
    }

    /// Every node the compiled program holds: the root and its closure.
    fn program(&self) -> Vec<Node> {
        self.closure(Node::Root)
            .into_iter()
            .map(Node::Module)
            .chain([Node::Root])
            .collect()
    }

    /// How many tuples the dataset supplies for the node's declaration: a
    /// different count per declaration, so a read of the wrong one shows.
    fn tuple_count(node: Node) -> usize {
        match node {
            Node::Module(index) => index + 1,
            Node::Root => 10,
        }
    }

    fn text(&self, node: Node) -> String {
        let module = self.module(node);
        let suffix = Self::suffix(node);
        let mut text = String::from("format: rulespec/v1\n");
        if node == Node::Root && self.composition {
            text.push_str("module:\n  kind: composition\n  summary: Generated composition.\n");
        }
        if !module.imports.is_empty() {
            text.push_str("imports:\n");
            for import in &module.imports {
                text.push_str(&format!("  - us:policies/g{import}\n"));
            }
        }
        let mut rules = Vec::new();
        if module.declares {
            rules.push(
                "  - name: member_of_household\n    kind: data_relation\n    data_relation:\n      arity: 2\n      arguments: [Person, Household]\n"
                    .to_string(),
            );
        }
        if let Some(form) = module.reads {
            let formula = match form {
                Form::Len => "len(member_of_household)".to_string(),
                Form::SumOfLocalRule => {
                    rules.push(format!(
                        "  - name: unit_weight_{suffix}\n    kind: derived\n    entity: Person\n    dtype: Integer\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"1\"\n"
                    ));
                    format!("sum(member_of_household.unit_weight_{suffix})")
                }
            };
            rules.push(format!(
                "  - name: size_{suffix}\n    kind: derived\n    entity: Household\n    dtype: Integer\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: {formula}\n"
            ));
        }
        if rules.is_empty() {
            text.push_str("rules: []\n");
        } else {
            text.push_str("rules:\n");
            for rule in rules {
                text.push_str(&rule);
            }
        }
        text
    }
}

/// What the read in `node` resolves to, independently of the engine: the
/// node's own declaration, else the one declaration in its imports, else
/// (when its imports declare none) the one declaration in the program.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Expected {
    Reads(Node),
    Ambiguous,
    Undeclared,
}

fn expected(graph: &Graph, node: Node) -> Expected {
    if graph.module(node).declares {
        return Expected::Reads(node);
    }
    let declaring = |nodes: &mut dyn Iterator<Item = Node>| {
        nodes
            .filter(|candidate| graph.module(*candidate).declares)
            .collect::<Vec<_>>()
    };
    let imported = declaring(&mut graph.closure(node).into_iter().map(Node::Module));
    let candidates = if imported.is_empty() {
        declaring(
            &mut graph
                .program()
                .into_iter()
                .filter(|candidate| *candidate != node),
        )
    } else {
        imported
    };
    match candidates.as_slice() {
        [only] => Expected::Reads(*only),
        [] => Expected::Undeclared,
        _ => Expected::Ambiguous,
    }
}

/// Compile the graph's root through the surface its kind takes.
fn compile_graph(graph: &Graph) -> Result<CompiledProgramArtifact, CompileError> {
    let modules = (0..graph.modules.len())
        .map(|index| {
            let node = Node::Module(index);
            (
                graph.target(node).expect("graph modules have origins"),
                graph.text(node),
            )
        })
        .collect::<Vec<_>>();
    if graph.composition {
        let borrowed = modules
            .iter()
            .map(|(target, text)| (target.as_str(), text.as_str()))
            .collect::<Vec<_>>();
        let checkout = Checkout::new(&borrowed);
        let path = checkout.composition(&graph.text(Node::Root));
        checkout.compile_composed(&path)
    } else {
        let mut modules = modules;
        modules.push(("us:policies/root".to_string(), graph.text(Node::Root)));
        let borrowed = modules
            .iter()
            .map(|(target, text)| (target.as_str(), text.as_str()))
            .collect::<Vec<_>>();
        compile(&borrowed, "us:policies/root")
    }
}

fn dense_integer(output: &DenseOutputValue) -> i64 {
    match output {
        DenseOutputValue::Scalar(DenseColumn::Integer(values)) => values[0],
        DenseOutputValue::Scalar(DenseColumn::Decimal(values)) => values[0]
            .to_string()
            .parse()
            .expect("an integral dense value"),
        other => panic!("expected an integer dense output, got {other:?}"),
    }
}

fn check_graph(graph: &Graph) -> Result<(), TestCaseError> {
    let program = graph.program();
    let readers = program
        .iter()
        .copied()
        .filter(|node| graph.module(*node).reads.is_some())
        .collect::<Vec<_>>();
    let expectations = readers
        .iter()
        .map(|node| (*node, expected(graph, *node)))
        .collect::<Vec<_>>();
    let compiled = compile_graph(graph);

    let ambiguous = expectations
        .iter()
        .filter(|(_, expected)| *expected == Expected::Ambiguous)
        .collect::<Vec<_>>();
    if !ambiguous.is_empty() {
        let message = compiled
            .err()
            .ok_or_else(|| TestCaseError::fail("an ambiguous read compiled"))?
            .to_string();
        prop_assert!(
            message.contains("[ambiguous_relation_reference]"),
            "{}",
            message
        );
        for (node, _) in ambiguous {
            prop_assert!(
                message.contains(&format!("`{}`", graph.output(*node))),
                "{}",
                message
            );
        }
        return Ok(());
    }
    if expectations
        .iter()
        .any(|(_, expected)| *expected == Expected::Undeclared)
    {
        let message = compiled
            .err()
            .ok_or_else(|| TestCaseError::fail("a read of an undeclared relation compiled"))?
            .to_string();
        prop_assert!(message.contains("[untyped_relation]"), "{}", message);
        return Ok(());
    }
    let artifact = compiled.map_err(|error| TestCaseError::fail(error.to_string()))?;

    // Every relation left in the program is a declaration a dataset can
    // supply by name: no formula-synthesized relation survives.
    let declared = program
        .iter()
        .filter(|node| graph.module(**node).declares)
        .map(|node| graph.relation(*node))
        .collect::<BTreeSet<_>>();
    let relations = relation_names(&artifact)
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    prop_assert_eq!(&relations, &declared);

    let tuples = program
        .iter()
        .filter(|node| graph.module(**node).declares)
        .flat_map(|node| members(&graph.relation(*node), Graph::tuple_count(*node)))
        .collect::<Vec<_>>();
    let expected_counts = expectations
        .iter()
        .map(|(node, expected)| {
            let Expected::Reads(declaration) = expected else {
                unreachable!("only resolved reads remain");
            };
            (graph.output(*node), Graph::tuple_count(*declaration) as i64)
        })
        .collect::<BTreeMap<_, _>>();
    if expected_counts.is_empty() {
        return Ok(());
    }
    let outputs = expected_counts
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    for mode in [ExecutionMode::Explain, ExecutionMode::Fast] {
        prop_assert_eq!(
            &run(&artifact, mode.clone(), tuples.clone(), &outputs),
            &expected_counts,
            "{:?}",
            mode
        );
    }

    // Dense reads each relation by the key it compiled, so its batch is
    // keyed by the declarations' legal ids too.
    let dense = DenseCompiledProgram::from_artifact(&artifact, Some("Household"))
        .map_err(|error| TestCaseError::fail(format!("dense compile: {error}")))?;
    let mut batch_relations = HashMap::new();
    for schema in dense.relations() {
        let count = program
            .iter()
            .find(|node| graph.module(**node).declares && graph.relation(**node) == schema.key.name)
            .map(|node| Graph::tuple_count(*node))
            .ok_or_else(|| {
                TestCaseError::fail(format!(
                    "dense reads `{}`, which no module declares",
                    schema.key.name
                ))
            })?;
        batch_relations
            .entry(schema.key.clone())
            .or_insert_with(|| DenseRelationBatchSpec {
                offsets: vec![0, count],
                related_row_count: Some(count),
                ..Default::default()
            });
    }
    let names = expected_counts
        .keys()
        .map(|output| {
            output
                .rsplit_once('#')
                .map_or(output.as_str(), |(_, name)| name)
                .to_string()
        })
        .collect::<Vec<_>>();
    let result = dense
        .execute(
            &period().to_model().expect("the period converts"),
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: batch_relations,
            },
            &names,
        )
        .map_err(|error| TestCaseError::fail(format!("dense execute: {error}")))?;
    for (output, name) in expected_counts.keys().zip(&names) {
        prop_assert_eq!(
            dense_integer(&result.outputs[name]),
            expected_counts[output],
            "dense {}",
            output
        );
    }
    Ok(())
}

fn runner(cases: u32, seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

/// Generated graphs, atomic roots and composition roots alike: wherever the
/// compile succeeds, each aggregate counts exactly the tuples supplied under
/// its declaration's legal id in explain, fast and dense; wherever two
/// declarations could answer a read, or none could, the compile refuses.
#[test]
fn generated_module_graphs_read_the_declaration_in_scope() {
    let outcomes = std::cell::RefCell::new(BTreeMap::<&str, usize>::new());
    let mut runner = runner(256, 226);
    runner
        .run(&graph(), |graph| {
            let readers = graph
                .program()
                .into_iter()
                .filter(|node| graph.module(*node).reads.is_some())
                .map(|node| expected(&graph, node))
                .collect::<Vec<_>>();
            let outcome = if readers.contains(&Expected::Ambiguous) {
                "ambiguous"
            } else if readers.contains(&Expected::Undeclared) {
                "undeclared"
            } else if readers.is_empty() {
                "nothing read"
            } else {
                "reads"
            };
            *outcomes.borrow_mut().entry(outcome).or_default() += 1;
            check_graph(&graph)
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    // Every outcome the property distinguishes is reached.
    let outcomes = outcomes.into_inner();
    for outcome in ["ambiguous", "undeclared", "reads"] {
        assert!(
            outcomes.get(outcome).copied().unwrap_or(0) >= 8,
            "too few `{outcome}` cases: {outcomes:?}"
        );
    }
}
