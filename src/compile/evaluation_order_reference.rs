//! The rule evaluation order as `origin/main` computed it at 04315d9, before
//! derived relations became nodes of the order's graph. It is the oracle for
//! the differential tests in `evaluation_order_tests`: the order is stored in
//! artifact metadata and compared exactly when an artifact loads, so the
//! production path must return what this returns, errors included, for every
//! program.
//!
//! Frozen: everything below the imports is the code as it stood, except that
//! `evaluation_order` is `pub(super)`. It copies a derived relation's
//! predicate rules into every rule that counts or sums over the relation, so
//! it is quadratic in that fan-out; keep it out of timing tests. The helpers
//! this change did not touch (`validate_relation_derivation_graph` and the
//! relation-member collectors) are shared with the production path.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::{
    CompileError, collect_relation_members_from_judgment, collect_relation_members_from_scalar,
    validate_relation_derivation_graph,
};
use crate::spec::{
    DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, RelatedValueRefSpec, ScalarExprSpec,
};

pub(super) fn evaluation_order(program: &ProgramSpec) -> Result<Vec<String>, CompileError> {
    let mut derived_names = HashSet::new();
    for derived in &program.derived {
        if !derived_names.insert(derived.name.clone()) {
            return Err(CompileError::DuplicateDerivedRule {
                name: derived.name.clone(),
            });
        }
    }
    validate_relation_derivation_graph(program)?;
    let relation_dependencies = relation_derivation_dependencies(program, &derived_names)?;

    let mut incoming_counts = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for derived in &program.derived {
        // Sorted, so a rule with several unknown dependencies always reports
        // the same one.
        let dependencies = derived_dependencies(derived, &relation_dependencies)
            .into_iter()
            .collect::<BTreeSet<String>>();
        incoming_counts.insert(derived.name.clone(), dependencies.len());

        for dependency in dependencies {
            if !derived_names.contains(&dependency) {
                return Err(CompileError::UnknownDerivedDependency {
                    derived: derived.name.clone(),
                    dependency,
                });
            }
            dependents
                .entry(dependency)
                .or_default()
                .push(derived.name.clone());
        }
    }

    for next in dependents.values_mut() {
        next.sort();
    }

    let mut ready = incoming_counts
        .iter()
        .filter_map(|(name, count)| (*count == 0).then_some(name.clone()))
        .collect::<BTreeSet<String>>();
    let mut order = Vec::with_capacity(program.derived.len());

    while let Some(name) = ready.pop_first() {
        order.push(name.clone());
        if let Some(next) = dependents.get(&name) {
            for dependent in next {
                if let Some(count) = incoming_counts.get_mut(dependent) {
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(dependent.clone());
                    }
                }
            }
        }
    }

    if order.len() != program.derived.len() {
        let cycle = incoming_counts
            .into_iter()
            .filter_map(|(name, count)| (count > 0).then_some(name))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect::<Vec<String>>()
            .join(", ");
        return Err(CompileError::CyclicDependency { cycle });
    }

    // The order above is stored in artifact metadata and compared exactly
    // when an artifact is loaded, so it stays as computed; this further check
    // only refuses programs, never reorders them.
    reject_relation_routed_cycles(program, &dependents)?;
    Ok(order)
}

/// Refuse a rule that depends on itself through derived relations: `e`
/// counts over `R`, and `R`'s membership depends on `e` through its predicate,
/// its source relation, or a relation its predicate names (by
/// `relation_member`, `count_related` or `sum_related`). The order above
/// follows only a derived relation's own predicate, so such a cycle passed it
/// and the evaluators recursed until the stack overflowed. This walks one
/// graph of rules and derived relations; every version of a rule counts, as it
/// does above. Base relations depend on nothing, so a program without derived
/// relations has no such cycle.
///
/// `rule_dependents` is the rule graph the order above was sorted from. Each
/// edge there is a rule another rule reads, or a rule read by the predicate of
/// a relation another rule aggregates over; the second kind is also a path
/// through that relation here, so reusing the edges changes no verdict.
fn reject_relation_routed_cycles(
    program: &ProgramSpec,
    rule_dependents: &HashMap<String, Vec<String>>,
) -> Result<(), CompileError> {
    if program
        .relations
        .iter()
        .all(|relation| relation.derivation.is_none())
    {
        return Ok(());
    }
    // Nodes are rules, then derived relations, by position. Rules and
    // relations have separate namespaces.
    let rule_nodes = program
        .derived
        .iter()
        .enumerate()
        .map(|(index, derived)| (derived.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let derived_relations = program
        .relations
        .iter()
        .filter(|relation| relation.derivation.is_some())
        .collect::<Vec<_>>();
    let relation_nodes = derived_relations
        .iter()
        .enumerate()
        .map(|(offset, relation)| (relation.name.as_str(), program.derived.len() + offset))
        .collect::<HashMap<_, _>>();
    let node_count = program.derived.len() + derived_relations.len();
    let rule_index = |name: &str| rule_nodes.get(name).copied();
    let relation_index = |name: &str| relation_nodes.get(name).copied();

    // dependents[x] lists the nodes that depend on node x.
    let mut dependents = vec![Vec::new(); node_count];
    let mut incoming = vec![0_usize; node_count];
    let mut add_edge = |dependency: Option<usize>, dependent: usize| {
        if let Some(dependency) = dependency {
            dependents[dependency].push(dependent);
            incoming[dependent] += 1;
        }
    };
    for (dependency, dependents) in rule_dependents {
        for dependent in dependents {
            if let Some(dependent) = rule_index(dependent) {
                add_edge(rule_index(dependency), dependent);
            }
        }
    }
    let no_relation_dependencies = HashMap::new();
    for (index, derived) in program.derived.iter().enumerate() {
        let mut relations = HashSet::new();
        for semantics in std::iter::once(&derived.semantics)
            .chain(derived.versions.iter().map(|version| &version.semantics))
        {
            match semantics {
                DerivedSemanticsSpec::Scalar { expr } => {
                    collect_relation_members_from_scalar(expr, &mut relations);
                }
                DerivedSemanticsSpec::Judgment { expr } => {
                    collect_relation_members_from_judgment(expr, &mut relations);
                }
            }
        }
        for relation in relations {
            add_edge(relation_index(&relation), index);
        }
    }
    for (offset, relation) in derived_relations.iter().enumerate() {
        let index = program.derived.len() + offset;
        let derivation = relation.derivation.as_ref().expect("derived relation");
        let mut rules = HashSet::new();
        collect_judgment_dependencies(&derivation.predicate, &mut rules, &no_relation_dependencies);
        for rule in rules {
            add_edge(rule_index(&rule), index);
        }
        let mut relations = HashSet::new();
        collect_relation_members_from_judgment(&derivation.predicate, &mut relations);
        relations.insert(derivation.source_relation.clone());
        for source in relations {
            add_edge(relation_index(&source), index);
        }
    }

    let mut ready = (0..node_count)
        .filter(|&node| incoming[node] == 0)
        .collect::<Vec<_>>();
    while let Some(node) = ready.pop() {
        for &dependent in &dependents[node] {
            incoming[dependent] -= 1;
            if incoming[dependent] == 0 {
                ready.push(dependent);
            }
        }
    }
    let cycle = program
        .derived
        .iter()
        .enumerate()
        .filter(|(index, _)| incoming[*index] > 0)
        .map(|(_, derived)| derived.name.as_str())
        .collect::<BTreeSet<_>>();
    if cycle.is_empty() {
        return Ok(());
    }
    Err(CompileError::CyclicDependency {
        cycle: cycle.into_iter().collect::<Vec<_>>().join(", "),
    })
}

fn relation_derivation_dependencies(
    program: &ProgramSpec,
    derived_names: &HashSet<String>,
) -> Result<HashMap<String, HashSet<String>>, CompileError> {
    let mut dependencies_by_relation = HashMap::new();
    for relation in &program.relations {
        let Some(derivation) = &relation.derivation else {
            continue;
        };
        let mut dependencies = HashSet::new();
        collect_judgment_dependencies(&derivation.predicate, &mut dependencies, &HashMap::new());
        // The smallest name, so a predicate naming several unknown rules
        // always reports the same one.
        if let Some(dependency) = dependencies
            .iter()
            .filter(|dependency| !derived_names.contains(*dependency))
            .min()
        {
            return Err(CompileError::UnknownDerivedDependency {
                derived: relation.name.clone(),
                dependency: dependency.clone(),
            });
        }
        dependencies_by_relation.insert(relation.name.clone(), dependencies);
    }
    Ok(dependencies_by_relation)
}

fn derived_dependencies(
    derived: &crate::spec::DerivedSpec,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) -> HashSet<String> {
    let mut dependencies = HashSet::new();
    match &derived.semantics {
        DerivedSemanticsSpec::Scalar { expr } => {
            collect_scalar_dependencies(expr, &mut dependencies, relation_dependencies);
        }
        DerivedSemanticsSpec::Judgment { expr } => {
            collect_judgment_dependencies(expr, &mut dependencies, relation_dependencies);
        }
    }
    for version in &derived.versions {
        match &version.semantics {
            DerivedSemanticsSpec::Scalar { expr } => {
                collect_scalar_dependencies(expr, &mut dependencies, relation_dependencies);
            }
            DerivedSemanticsSpec::Judgment { expr } => {
                collect_judgment_dependencies(expr, &mut dependencies, relation_dependencies);
            }
        }
    }
    dependencies
}

fn collect_scalar_dependencies(
    expr: &ScalarExprSpec,
    dependencies: &mut HashSet<String>,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) {
    match expr {
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. } => {}
        ScalarExprSpec::CountRelated {
            relation,
            where_clause,
            ..
        } => {
            if let Some(relation_dependencies) = relation_dependencies.get(relation) {
                dependencies.extend(relation_dependencies.iter().cloned());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_dependencies(predicate, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::Derived { name } => {
            dependencies.insert(name.clone());
        }
        ScalarExprSpec::ParameterLookup { index, .. } => {
            collect_scalar_dependencies(index, dependencies, relation_dependencies);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                collect_scalar_dependencies(item, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            collect_scalar_dependencies(left, dependencies, relation_dependencies);
            collect_scalar_dependencies(right, dependencies, relation_dependencies);
        }
        ScalarExprSpec::Ceil { value } | ScalarExprSpec::Floor { value } => {
            collect_scalar_dependencies(value, dependencies, relation_dependencies);
        }
        ScalarExprSpec::PeriodStart | ScalarExprSpec::PeriodEnd => {}
        ScalarExprSpec::DateAddDays { date, days } => {
            collect_scalar_dependencies(date, dependencies, relation_dependencies);
            collect_scalar_dependencies(days, dependencies, relation_dependencies);
        }
        ScalarExprSpec::DateAddMonths { date, months } => {
            collect_scalar_dependencies(date, dependencies, relation_dependencies);
            collect_scalar_dependencies(months, dependencies, relation_dependencies);
        }
        ScalarExprSpec::DateAddYears { date, years } => {
            collect_scalar_dependencies(date, dependencies, relation_dependencies);
            collect_scalar_dependencies(years, dependencies, relation_dependencies);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            collect_scalar_dependencies(from, dependencies, relation_dependencies);
            collect_scalar_dependencies(to, dependencies, relation_dependencies);
        }
        ScalarExprSpec::SumRelated {
            value,
            relation,
            where_clause,
            ..
        } => {
            if let Some(relation_dependencies) = relation_dependencies.get(relation) {
                dependencies.extend(relation_dependencies.iter().cloned());
            }
            if let RelatedValueRefSpec::Derived { name } = value {
                dependencies.insert(name.clone());
            }
            if let Some(predicate) = where_clause {
                collect_judgment_dependencies(predicate, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_dependencies(condition, dependencies, relation_dependencies);
            collect_scalar_dependencies(then_expr, dependencies, relation_dependencies);
            collect_scalar_dependencies(else_expr, dependencies, relation_dependencies);
        }
        ScalarExprSpec::NoMatch { subject, patterns } => {
            collect_scalar_dependencies(subject, dependencies, relation_dependencies);
            for pattern in patterns {
                collect_scalar_dependencies(pattern, dependencies, relation_dependencies);
            }
        }
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            collect_scalar_dependencies(value, dependencies, relation_dependencies);
            if let Some(n) = n {
                collect_scalar_dependencies(n, dependencies, relation_dependencies);
            }
        }
    }
}

fn collect_judgment_dependencies(
    expr: &JudgmentExprSpec,
    dependencies: &mut HashSet<String>,
    relation_dependencies: &HashMap<String, HashSet<String>>,
) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            collect_scalar_dependencies(left, dependencies, relation_dependencies);
            collect_scalar_dependencies(right, dependencies, relation_dependencies);
        }
        JudgmentExprSpec::Derived { name } => {
            dependencies.insert(name.clone());
        }
        JudgmentExprSpec::RelationMember { .. } => {}
        JudgmentExprSpec::And { items }
        | JudgmentExprSpec::Or { items }
        | JudgmentExprSpec::ExactlyOne { items } => {
            for item in items {
                collect_judgment_dependencies(item, dependencies, relation_dependencies);
            }
        }
        JudgmentExprSpec::Not { item } => {
            collect_judgment_dependencies(item, dependencies, relation_dependencies);
        }
    }
}
