//! Format-3 acceptance over one explicitly configured RuleSpec country root.
//! Set `AXIOM_RULESPEC_ROOT` to a checkout or an archive export, and optionally
//! `AXIOM_ARTIFACT_BASELINE_DIR` to a main-written `dump_artifacts corpus`
//! baseline. Roots are validated once and every module compiles in this process.

#![cfg(feature = "fs")]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::process::Command;

use axiom_rules_engine::compile::{CompileError, CompiledProgramArtifact};
use axiom_rules_engine::interface::{EntityInstances, RelationProvenance, RelationReads};
use axiom_rules_engine::relation_typing::EffectiveSlotEntities;
use axiom_rules_engine::rulespec::{CanonicalRuleSpecRoots, RULESPEC_ATOMIC_ROOTS};
use serde_json::{Value, json};

fn is_module(path: &str) -> bool {
    let parts = path.split('/').collect::<Vec<_>>();
    parts.len() >= 3
        && RULESPEC_ATOMIC_ROOTS.contains(&parts[1])
        && path.ends_with(".yaml")
        && !path.ends_with(".test.yaml")
}

fn module_paths(root: &Path) -> Vec<String> {
    if root.join(".git").exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-z"])
            .output()
            .expect("git lists tracked corpus files");
        assert!(output.status.success(), "git ls-files failed: {output:?}");
        let mut modules = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8(path.to_vec()).expect("UTF-8 corpus path"))
            .filter(|path| is_module(path))
            .collect::<Vec<_>>();
        modules.sort();
        return modules;
    }

    // Archive exports have no .git; every exported file is tracked. Scope the
    // walk to <jurisdiction>/<atomic-root>, including nested citation paths.
    let mut pending = Vec::new();
    for entry in std::fs::read_dir(root).expect("read configured corpus root") {
        let jurisdiction = entry.expect("read jurisdiction entry").path();
        if jurisdiction.is_dir() {
            for name in RULESPEC_ATOMIC_ROOTS {
                let content = jurisdiction.join(name);
                if content.is_dir() {
                    pending.push(content);
                }
            }
        }
    }
    let mut modules = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read atomic content directory") {
            let path = entry.expect("read atomic content entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("walk stays under corpus root")
                    .to_str()
                    .expect("UTF-8 corpus path")
                    .to_string();
                if is_module(&relative) {
                    modules.push(relative);
                }
            }
        }
    }
    modules.sort();
    modules
}

fn baseline_results(directory: &Path) -> BTreeMap<String, bool> {
    let text = std::fs::read_to_string(directory.join("results.tsv"))
        .expect("baseline results.tsv is readable");
    let mut results = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let module = fields.next().expect("baseline module");
        let compiled = match fields.next() {
            Some("0") => true,
            Some("1") => false,
            status => panic!("invalid baseline status {status:?} on line {}", index + 1),
        };
        assert!(is_module(module), "invalid baseline module: {module}");
        assert!(
            results.insert(module.to_string(), compiled).is_none(),
            "duplicate baseline module: {module}"
        );
    }
    results
}

fn equal<T: PartialEq + Debug>(actual: T, expected: T, label: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected:?}; got {actual:?}"))
    }
}

#[derive(Default)]
struct ExpressionReads {
    inputs: BTreeSet<String>,
    rules: BTreeSet<String>,
    relations: BTreeSet<String>,
    lookup_slots: BTreeMap<String, BTreeSet<usize>>,
    /// The slots each atom and `exists` binds. Neither names a current slot.
    bound_slots: BTreeMap<String, BTreeSet<usize>>,
    uses_relation_roles: bool,
}

impl ExpressionReads {
    // This structural walk is independent of the interface collector. It
    // visits base expressions, versions and predicates, including dead arms.
    fn collect(&mut self, value: &Value) {
        match value {
            Value::Object(object) => {
                let kind = object.get("kind").and_then(Value::as_str);
                self.uses_relation_roles |= matches!(
                    kind,
                    Some(
                        "role_input" | "role_derived" | "relation_atom" | "exists" | "same_entity"
                    )
                );
                match kind {
                    Some("input" | "input_or_else" | "role_input") => {
                        self.inputs
                            .insert(object["name"].as_str().unwrap().to_string());
                    }
                    Some("input_for_year") => {
                        let input = object["input"].as_str().unwrap();
                        let slot = input.rsplit_once("#input.").map_or(input, |(_, slot)| slot);
                        self.inputs.insert(slot.to_string());
                    }
                    Some("derived" | "role_derived") => {
                        self.rules
                            .insert(object["name"].as_str().unwrap().to_string());
                    }
                    _ => {}
                }
                if let Some(relation) = object.get("relation").and_then(Value::as_str) {
                    self.relations.insert(relation.to_string());
                    if let Some(slot) = object.get("current_slot").and_then(Value::as_u64) {
                        self.lookup_slots
                            .entry(relation.to_string())
                            .or_default()
                            .insert(slot as usize);
                    }
                    if let Some(terms) = object.get("terms").and_then(Value::as_array) {
                        let variable = object.get("variable_slot").and_then(Value::as_u64);
                        self.bound_slots
                            .entry(relation.to_string())
                            .or_default()
                            .extend((0..terms.len()).filter(|slot| variable != Some(*slot as u64)));
                    }
                }
                for value in object.values() {
                    self.collect(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.collect(value);
                }
            }
            _ => {}
        }
    }
}

fn check_agreement(artifact: &CompiledProgramArtifact) -> Result<(), String> {
    let interface = artifact.interface.as_ref().ok_or("missing interface")?;
    let program = artifact
        .program
        .to_program()
        .map_err(|error| error.to_string())?;
    let slot_kinds = EffectiveSlotEntities::new(&program);
    let mut all_reads = ExpressionReads::default();
    let mut predicates = BTreeMap::new();
    for rule in &artifact.program.derived {
        all_reads.collect(&serde_json::to_value(&rule.semantics).unwrap());
        for version in &rule.versions {
            all_reads.collect(&serde_json::to_value(&version.semantics).unwrap());
        }
    }
    for relation in &artifact.program.relations {
        if let Some(derivation) = &relation.derivation {
            let predicate = serde_json::to_value(&derivation.predicate).unwrap();
            let mut reads = ExpressionReads::default();
            reads.collect(&predicate);
            predicates.insert(relation.name.as_str(), reads);
            all_reads.collect(&predicate);
            if let Some(source) = &derivation.source_relation {
                all_reads
                    .lookup_slots
                    .entry(source.clone())
                    .or_default()
                    .insert(derivation.current_slot);
            }
            for step in derivation.source_path.iter().flatten() {
                all_reads
                    .lookup_slots
                    .entry(step.relation.clone())
                    .or_default()
                    .insert(step.from_slot);
                all_reads.uses_relation_roles = true;
            }
        }
    }
    // A derived atom resolves members at its relation's current slot. A tuple
    // atom keys on every slot it binds.
    for (relation, slots) in std::mem::take(&mut all_reads.bound_slots) {
        let slots = match &program.relations[&relation].derivation {
            Some(derivation) => BTreeSet::from([derivation.current_slot]),
            None => slots,
        };
        all_reads
            .lookup_slots
            .entry(relation)
            .or_default()
            .extend(slots);
    }
    equal(
        interface
            .inputs
            .iter()
            .map(|input| input.name.clone())
            .collect::<Vec<_>>(),
        all_reads.inputs.into_iter().collect::<Vec<_>>(),
        "input read slots",
    )?;
    let roster = interface
        .entities
        .iter()
        .map(|entity| entity.kind.as_str())
        .collect::<BTreeSet<_>>();
    equal(
        roster.len(),
        interface.entities.len(),
        "unique entity kinds",
    )?;
    let catalog = artifact
        .metadata
        .input_catalog
        .iter()
        .map(|entry| (entry.slot.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    for input in &interface.inputs {
        let entry = catalog
            .get(input.name.as_str())
            .ok_or_else(|| format!("uncatalogued input {}", input.name))?;
        equal(
            &input.legal_id,
            &entry.canonical_request_name,
            "canonical request name",
        )?;
        for kind in &input.entities {
            equal(
                roster.contains(kind.as_str()),
                true,
                "input kind is in roster",
            )?;
        }
        let elections = artifact
            .program
            .input_declarations
            .iter()
            .filter(|declaration| entry.request_names.contains(&declaration.input))
            .map(|declaration| &declaration.election)
            .collect::<Vec<_>>();
        equal(
            input.election.as_ref(),
            elections.first().copied(),
            "input election",
        )?;
        for election in elections {
            equal(
                Some(election),
                input.election.as_ref(),
                "agreeing election declarations",
            )?;
        }
        if input.election.is_some() {
            equal(input.default.is_none(), true, "elections have no default")?;
        }
    }
    equal(
        interface.relations.len(),
        program.relations.len(),
        "relation count",
    )?;
    equal(
        interface
            .relations
            .iter()
            .map(|relation| relation.name.as_str())
            .collect::<Vec<_>>(),
        program
            .relations
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        "relation names",
    )?;
    for relation in &interface.relations {
        let schema = &program.relations[&relation.name];
        equal(relation.arity, schema.arity, "relation arity")?;
        equal(
            relation.slot_entities.as_deref(),
            slot_kinds.get(&relation.name),
            "effective relation slot kinds",
        )?;
        equal(
            relation.accepts_supplied_tuples,
            schema.derivation.is_none() || !all_reads.uses_relation_roles,
            "supplied relation tuples",
        )?;
        for kind in relation.slot_entities.iter().flatten() {
            equal(
                roster.contains(kind.as_str()),
                true,
                "relation kind is in roster",
            )?;
        }
        let lookups = all_reads
            .lookup_slots
            .get(&relation.name)
            .cloned()
            .unwrap_or_default();
        equal(
            relation.lookup_slots.clone(),
            lookups.into_iter().collect(),
            "relation lookup slots",
        )?;
        let derived_reads = |reads: &RelationReads, steps: BTreeSet<String>| {
            let expected = &predicates[relation.name.as_str()];
            equal(
                reads.inputs.clone(),
                expected.inputs.iter().cloned().collect(),
                "predicate inputs",
            )?;
            equal(
                reads.rules.clone(),
                expected
                    .rules
                    .iter()
                    .map(|rule| program.public_derived_key(rule))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                "predicate rules",
            )?;
            equal(
                reads.relations.clone(),
                expected
                    .relations
                    .iter()
                    .cloned()
                    .chain(steps)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                "predicate relations",
            )
        };
        match (&relation.provenance, &schema.derivation) {
            (RelationProvenance::Supplied, None) => {}
            (
                RelationProvenance::Derived {
                    rule,
                    source_relation,
                    reads,
                },
                Some(derivation),
            ) => {
                equal(rule, &relation.name, "derived relation rule")?;
                equal(
                    Some(source_relation),
                    derivation.source_relation.as_ref(),
                    "derived relation source",
                )?;
                derived_reads(reads, BTreeSet::new())?;
            }
            (
                RelationProvenance::SourcePath {
                    rule,
                    source_path,
                    reads,
                },
                Some(derivation),
            ) => {
                equal(rule, &relation.name, "path relation rule")?;
                let steps = derivation.source_path.iter().flatten();
                equal(
                    source_path
                        .iter()
                        .map(|step| (&step.relation, step.from_slot, step.to_slot))
                        .collect::<Vec<_>>(),
                    steps
                        .clone()
                        .map(|step| (&step.relation, step.from_slot, step.to_slot))
                        .collect(),
                    "path relation steps",
                )?;
                derived_reads(reads, steps.map(|step| step.relation.clone()).collect())?;
            }
            _ => return Err(format!("relation provenance disagrees: {}", relation.name)),
        }
    }
    Ok(())
}

#[derive(Default)]
struct Tally {
    compiled: usize,
    gained: usize,
    slots: usize,
    multi_entity: usize,
    scalar: usize,
    typed: usize,
    defaults: usize,
    values: usize,
    dtype: BTreeMap<String, usize>,
    instances: BTreeMap<String, usize>,
    provenance: BTreeMap<String, usize>,
    elections: BTreeMap<String, usize>,
    failures: BTreeMap<String, usize>,
    examples: BTreeMap<String, String>,
}

fn bump(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

impl Tally {
    fn artifact(&mut self, artifact: &CompiledProgramArtifact) {
        self.compiled += 1;
        let interface = artifact.interface.as_ref().expect("compiled interface");
        self.gained += 1;
        for input in &interface.inputs {
            self.slots += 1;
            self.multi_entity += usize::from(input.entities.len() > 1);
            self.scalar += usize::from(input.entities.iter().any(|kind| kind == "Scalar"));
            self.defaults += usize::from(input.default.is_some());
            self.values += usize::from(input.values.is_some());
            if let Some(dtype) = input.dtype {
                self.typed += 1;
                bump(
                    &mut self.dtype,
                    serde_json::to_value(dtype).unwrap().as_str().unwrap(),
                );
            }
            if let Some(election) = &input.election {
                bump(&mut self.elections, &election.actor);
            }
        }
        for entity in &interface.entities {
            bump(
                &mut self.instances,
                match entity.instances {
                    EntityInstances::Supplied => "supplied",
                    EntityInstances::Alias { .. } => "alias",
                    EntityInstances::Unscoped => "unscoped",
                },
            );
        }
        for relation in &interface.relations {
            bump(
                &mut self.provenance,
                match relation.provenance {
                    RelationProvenance::Supplied => "supplied",
                    RelationProvenance::Derived { .. } => "derived",
                    RelationProvenance::SourcePath { .. } => "source_path",
                },
            );
        }
    }

    fn failure(&mut self, module: &str, error: &CompileError) {
        let debug = match error {
            CompileError::RuleSpec { error, .. } => format!("RuleSpec::{error:?}"),
            _ => format!("{error:?}"),
        };
        let cause = debug.split([' ', '{', '(']).next().unwrap().to_string();
        bump(&mut self.failures, &cause);
        self.examples
            .entry(cause)
            .or_insert_with(|| format!("{module}: {error}"));
    }

    fn report(
        &self,
        root: &Path,
        modules: usize,
        differences: &[String],
        baseline: Option<&BTreeMap<String, bool>>,
    ) {
        eprintln!("\n=== corpus interface acceptance: {} ===", root.display());
        eprintln!(
            "modules: {modules} compiled: {} failed: {} artifacts_gaining_block: {} differences: {}",
            self.compiled,
            modules - self.compiled,
            self.gained,
            differences.len()
        );
        if let Some(baseline) = baseline {
            let baseline_compiled = baseline.values().filter(|compiled| **compiled).count();
            eprintln!(
                "baseline_modules: {} baseline_compiled: {} baseline_failed: {}",
                baseline.len(),
                baseline_compiled,
                baseline.len() - baseline_compiled
            );
        }
        eprintln!(
            "slots: {} multi_entity: {} Scalar: {} dtype: {} default: {} values: {}",
            self.slots, self.multi_entity, self.scalar, self.typed, self.defaults, self.values
        );
        eprintln!("dtype_by_type: {:?}", self.dtype);
        eprintln!("entities_by_instances_source: {:?}", self.instances);
        eprintln!("relations_by_provenance: {:?}", self.provenance);
        eprintln!(
            "elections: {} by_actor: {:?}",
            self.elections.values().sum::<usize>(),
            self.elections
        );
        eprintln!("failed_by_cause: {:?}", self.failures);
        for (cause, example) in &self.examples {
            eprintln!("  {cause}: {}", example.lines().next().unwrap_or(example));
        }
        for difference in differences {
            eprintln!("  difference: {difference}");
        }
    }
}

/// Invariants 1, 2(a), and 3: every compiled corpus module keeps main's
/// executable program and metadata, round-trips byte for byte, and emits an
/// interface agreeing with its read sites, catalog, relations and elections.
/// The compiling module set must equal main's set when a baseline is supplied.
#[test]
fn corpus_artifacts_preserve_executable_content_and_interface_contract() {
    let Some(configured_root) = std::env::var_os("AXIOM_RULESPEC_ROOT").map(PathBuf::from) else {
        eprintln!(
            "skipping corpus acceptance: set AXIOM_RULESPEC_ROOT (optionally AXIOM_ARTIFACT_BASELINE_DIR)"
        );
        return;
    };
    let root = std::fs::canonicalize(&configured_root).unwrap_or_else(|error| {
        panic!(
            "cannot resolve AXIOM_RULESPEC_ROOT {}: {error}",
            configured_root.display()
        )
    });
    let roots = CanonicalRuleSpecRoots::new([&root]).unwrap_or_else(|error| {
        panic!("AXIOM_RULESPEC_ROOT must name one exact rulespec-<country> root: {error}")
    });
    let modules = module_paths(&root);
    assert!(
        !modules.is_empty(),
        "no atomic modules discovered in {}",
        root.display()
    );
    let baseline_dir = std::env::var_os("AXIOM_ARTIFACT_BASELINE_DIR").map(PathBuf::from);
    let baseline = baseline_dir.as_deref().map(baseline_results);
    let mut tally = Tally::default();
    let mut differences = Vec::new();
    let mut compiled = BTreeSet::new();
    if let Some(baseline) = &baseline {
        let found = modules.iter().cloned().collect::<BTreeSet<_>>();
        let expected = baseline.keys().cloned().collect::<BTreeSet<_>>();
        if found != expected {
            differences.push(format!(
                "module list: added {:?}; missing {:?}",
                found.difference(&expected).collect::<Vec<_>>(),
                expected.difference(&found).collect::<Vec<_>>()
            ));
        }
    }
    for module in &modules {
        let artifact = match CompiledProgramArtifact::from_rulespec_file(root.join(module), &roots)
        {
            Ok(artifact) => artifact,
            Err(error) => {
                tally.failure(module, &error);
                continue;
            }
        };
        compiled.insert(module.clone());
        tally.artifact(&artifact);
        let serialized =
            serde_json::to_string_pretty(&artifact).expect("serialize compiled artifact");
        match CompiledProgramArtifact::from_json_str(&serialized) {
            Ok(reloaded) => {
                if serde_json::to_string_pretty(&reloaded).unwrap() != serialized {
                    differences.push(format!("{module}: invariant 2(a): round-trip bytes differ"));
                }
            }
            Err(error) => differences.push(format!(
                "{module}: invariants 2(a)/3: loader refused compiled artifact: {error}"
            )),
        }
        if let Err(error) = check_agreement(&artifact) {
            differences.push(format!("{module}: invariant 3: {error}"));
        }
        if let (Some(directory), Some(baseline)) = (&baseline_dir, &baseline)
            && baseline.get(module) == Some(&true)
        {
            let path = directory.join("artifacts").join(format!(
                "{}.json",
                module.trim_end_matches(".yaml").replace('/', "__")
            ));
            let result = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())
                });
            match result {
                Ok(expected) => {
                    let mut actual: Value = serde_json::from_str(&serialized).unwrap();
                    actual["program"]
                        .as_object_mut()
                        .unwrap()
                        .remove("interface");
                    for field in ["program", "metadata"] {
                        if actual[field] != expected[field] {
                            differences
                                .push(format!("{module}: invariant 1: {field} differs from main"));
                        }
                    }
                }
                Err(error) => differences.push(format!(
                    "{module}: cannot read baseline {}: {error}",
                    path.display()
                )),
            }
        }
    }
    if let Some(baseline) = &baseline {
        let expected = baseline
            .iter()
            .filter(|(_, ok)| **ok)
            .map(|(module, _)| module.clone())
            .collect::<BTreeSet<_>>();
        for module in compiled.difference(&expected) {
            differences.push(format!("{module}: branch compiles; main refused"));
        }
        for module in expected.difference(&compiled) {
            differences.push(format!("{module}: main compiles; branch refused"));
        }
    }
    tally.report(&root, modules.len(), &differences, baseline.as_ref());
    assert!(!compiled.is_empty(), "no corpus modules compiled");
    assert!(
        differences.is_empty(),
        "{} corpus acceptance differences; see tally above",
        differences.len()
    );
}

#[test]
fn the_agreement_oracle_reads_paths_atoms_and_exists() {
    // No corpus module uses these yet. A correct interface must still pass
    // when the first one does.
    let slots = json!(["Person", "Person"]);
    let positive = json!({
        "kind": "comparison", "op": "gt",
        "left": {"kind": "role_derived", "term": "candidate", "name": "allowed"},
        "right": {"kind": "literal", "value": {"kind": "integer", "value": 0}},
    });
    let spec = serde_json::from_value(json!({
        "relations": [
            {"name": "edge", "arity": 2, "slot_entities": slots},
            {"name": "selected", "arity": 2, "slot_entities": slots, "derivation": {
                "source_path": [{"relation": "edge", "from_slot": 0, "to_slot": 1}],
                "current_slot": 0, "related_slot": 1, "slot_entities": slots,
                "predicate": positive,
            }},
            {"name": "filtered", "arity": 2, "slot_entities": slots, "derivation": {
                "source_relation": "edge",
                "current_slot": 0, "related_slot": 1, "slot_entities": slots,
                "predicate": {
                    "kind": "exists", "relation": "edge", "terms": ["anchor", {"var": 0}],
                    "variable_slot": 1,
                    "body": {"kind": "relation_atom", "relation": "selected",
                        "terms": [{"var": 0}, "candidate"]},
                },
            }},
        ],
        "derived": [
            {"name": "allowed", "entity": "Person", "dtype": "integer", "semantics": "scalar",
                "expr": {"kind": "input", "name": "allow"}},
            {"name": "total", "entity": "Person", "dtype": "integer", "semantics": "scalar",
                "expr": {"kind": "count_related", "relation": "filtered",
                    "current_slot": 0, "related_slot": 1}},
        ],
    }))
    .expect("a valid program");
    let artifact = CompiledProgramArtifact::compile(spec).expect("the program compiles");
    check_agreement(&artifact).expect("the oracle agrees with the interface");
}

#[test]
fn archive_discovery_includes_nested_modules_and_excludes_companions_and_programs() {
    let root = std::env::temp_dir().join(format!(
        "axiom-corpus-interface-discovery-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    for path in [
        "us/statutes/26/32/a.yaml",
        "us/statutes/26/32/a.test.yaml",
        "us-ca/policies/dss/deep/module.yaml",
        "us/programs/composition.yaml",
        "us/statutes/alias.yml",
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "format: [").unwrap();
    }
    assert_eq!(
        module_paths(&root),
        vec![
            "us-ca/policies/dss/deep/module.yaml",
            "us/statutes/26/32/a.yaml"
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}
