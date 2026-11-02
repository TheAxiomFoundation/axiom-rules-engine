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
