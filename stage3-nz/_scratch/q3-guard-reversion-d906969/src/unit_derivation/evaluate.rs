use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::compile::{CompiledConstitution, expr_fact_refs};
use super::interface::EntityRegistry;
use super::types::*;

#[derive(Clone, Debug)]
enum NormalizedBool {
    Known(bool),
    Unknown,
    Conflict(BTreeSet<bool>),
}

#[derive(Clone, Debug)]
struct BoundFacts {
    values: BTreeMap<String, NormalizedBool>,
    conflicts: BTreeSet<String>,
    completeness_evidence: BTreeSet<String>,
    election_defaults: BTreeMap<String, (String, bool, Citation)>,
    request_evidence: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug)]
struct FrozenEdge {
    id: String,
    kind: EdgeKind,
    left: String,
    right: String,
    citation: Citation,
}

#[derive(Clone, Debug)]
struct WorldPerson {
    block: Option<Vec<String>>,
    role: Option<ProjectionRole>,
    kind: Option<IndeterminateKind>,
    citations: BTreeSet<Citation>,
}

#[derive(Clone, Debug)]
struct WorldResult {
    persons: BTreeMap<String, WorldPerson>,
    events: BTreeSet<TraceEvent>,
}

#[derive(Clone, Debug)]
struct ActiveCut {
    ids: BTreeSet<String>,
    members: BTreeSet<String>,
    citations: BTreeSet<Citation>,
}

type WorldVariables = Vec<(String, Vec<bool>)>;

pub fn derive_units(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    config: &UnitDerivationConfig,
) -> Result<UnitDerivationResult, UnitDerivationError> {
    if !config.enabled {
        return Err(UnitDerivationError::Disabled);
    }
    if config.semantics_version != super::EXPERIMENTAL_SEMANTICS_VERSION {
        return Err(UnitDerivationError::UnsupportedExperimentalVersion(
            config.semantics_version.clone(),
        ));
    }
    if !input.segment_complete {
        return Err(UnitDerivationError::RequiresSegmentation);
    }
    validate_roster(compiled, input)?;

    let persons = input
        .roster
        .persons
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if input.roster.completeness.is_none() {
        return Ok(roster_incomplete_result(compiled, input, &persons));
    }

    let referenced_facts = all_fact_refs(compiled, input);
    let bound = bind_facts(compiled, input, &referenced_facts)?;
    let (mut variables, fixed) = world_variables(compiled, &bound);
    variables.sort_by(|left, right| left.0.cmp(&right.0));

    let worlds = variable_world_count(&variables)?;
    if worlds > config.max_admissible_worlds {
        return Err(UnitDerivationError::UncheckableWorldSpace {
            worlds,
            maximum: config.max_admissible_worlds,
        });
    }

    let mut enumerated = Vec::with_capacity(worlds);
    enumerate_worlds(&variables, 0, fixed, &mut enumerated);
    let mut valuations = Vec::new();
    for valuation in enumerated {
        let mut admissible = true;
        for constraint in &input.integrity_constraints {
            if !eval_expr(
                &constraint.expr,
                &valuation,
                &compiled.derived_bools,
                &mut BTreeSet::new(),
            )? {
                admissible = false;
                break;
            }
        }
        if admissible {
            valuations.push(valuation);
        }
    }

    if valuations.is_empty() {
        return Ok(inconsistent_result(compiled, input, &persons, &bound));
    }

    let influence = influence_sets(compiled, input, &bound);
    let mut results = Vec::with_capacity(valuations.len());
    for valuation in &valuations {
        results.push(evaluate_world(
            compiled, &persons, valuation, &influence, &bound,
        )?);
    }
    assemble_result(compiled, input, &persons, &bound, &influence, results)
}

fn validate_roster(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
) -> Result<(), UnitDerivationError> {
    if input.roster.relation != compiled.plan.roster_relation {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "roster bound `{}` but compiled constitution requires `{}`",
            input.roster.relation, compiled.plan.roster_relation
        )));
    }
    if input.roster.scope.is_empty() || input.segment.is_empty() {
        return Err(UnitDerivationError::InvalidPlan(
            "roster scope and evaluation segment must be non-empty".to_string(),
        ));
    }
    let roster = input
        .roster
        .persons
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if roster.len() != input.roster.persons.len() {
        return Err(UnitDerivationError::DuplicateNamespace {
            namespace: "roster person",
            id: input
                .roster
                .persons
                .iter()
                .find(|person| {
                    input
                        .roster
                        .persons
                        .iter()
                        .filter(|candidate| *candidate == *person)
                        .count()
                        > 1
                })
                .cloned()
                .unwrap_or_default(),
        });
    }
    if roster.is_empty() {
        return Err(UnitDerivationError::InvalidPlan(
            "roster must contain at least one person".to_string(),
        ));
    }

    let mut referenced = BTreeSet::new();
    for edge in compiled.edges.values() {
        referenced.insert(edge.left.clone());
        referenced.insert(edge.right.clone());
    }
    for cut in compiled.cuts.values() {
        referenced.extend(cut.members.iter().cloned());
    }
    for attachment in compiled.attachments.values() {
        referenced.extend(attachment.members.iter().cloned());
        referenced.insert(attachment.target_anchor.clone());
    }
    for bar in compiled.bars.values() {
        referenced.extend(bar.members.iter().cloned());
    }
    for status in compiled.statuses.values() {
        referenced.insert(status.person.clone());
    }
    if let Some(missing) = referenced.difference(&roster).next() {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "operation names person `{missing}` outside the roster"
        )));
    }
    Ok(())
}

fn all_fact_refs(compiled: &CompiledConstitution, input: &ConstitutionInput) -> BTreeSet<FactRef> {
    let mut facts = BTreeSet::new();
    for edge in compiled.edges.values() {
        facts.extend(expr_fact_refs(&edge.when, &compiled.derived_bools));
        for defeater in &edge.defeaters {
            facts.extend(expr_fact_refs(&defeater.when, &compiled.derived_bools));
        }
    }
    for cut in compiled.cuts.values() {
        facts.extend(expr_fact_refs(&cut.when, &compiled.derived_bools));
    }
    for attachment in compiled.attachments.values() {
        facts.extend(expr_fact_refs(&attachment.when, &compiled.derived_bools));
    }
    for bar in compiled.bars.values() {
        facts.extend(expr_fact_refs(&bar.when, &compiled.derived_bools));
    }
    for status in compiled.statuses.values() {
        facts.extend(expr_fact_refs(&status.when, &compiled.derived_bools));
    }
    for constraint in &input.integrity_constraints {
        facts.extend(expr_fact_refs(&constraint.expr, &compiled.derived_bools));
    }
    facts
}

fn bind_facts(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    referenced: &BTreeSet<FactRef>,
) -> Result<BoundFacts, UnitDerivationError> {
    let mut families = BTreeMap::new();
    for family in &input.relation_families {
        insert_unique_ref(
            "relation family",
            &mut families,
            family.name.clone(),
            family,
        )?;
    }
    let mut bool_facts = BTreeMap::new();
    for fact in &input.bool_facts {
        if fact.explicit_unknown.is_some() && !fact.observations.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "boolean fact `{}` supplies both explicit Unknown and observations",
                fact.name
            )));
        }
        insert_unique_ref("boolean fact", &mut bool_facts, fact.name.clone(), fact)?;
    }

    let request_evidence = bool_facts
        .iter()
        .map(|(name, fact)| {
            let evidence = fact
                .observations
                .iter()
                .map(|observation| observation.evidence.id.clone())
                .chain(
                    fact.explicit_unknown
                        .iter()
                        .map(|evidence| evidence.id.clone()),
                )
                .collect();
            (name.clone(), evidence)
        })
        .collect();

    let mut values = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    let mut completeness_evidence = BTreeSet::new();
    let election_policies = election_policies(compiled);
    let mut election_defaults = BTreeMap::new();
    if let Some(evidence) = &input.roster.completeness {
        completeness_evidence.insert(evidence.id.clone());
    }

    for fact in referenced {
        let normalized = match fact {
            FactRef::Bool(name) => match bool_facts.get(name) {
                Some(input) if input.explicit_unknown.is_some() => NormalizedBool::Unknown,
                Some(input) if !input.observations.is_empty() => {
                    normalize_observations(&input.observations)
                }
                _ => match election_policies.get(name) {
                    Some((actor, MissingElectionPolicy::AuthorityDefault { value, citation })) => {
                        election_defaults
                            .insert(name.clone(), (actor.clone(), *value, citation.clone()));
                        NormalizedBool::Known(*value)
                    }
                    Some((_, MissingElectionPolicy::Unknown)) | None => NormalizedBool::Unknown,
                },
            },
            FactRef::Relation { family, tuple } => {
                let Some(family) = families.get(family) else {
                    values.insert(fact.key(), NormalizedBool::Unknown);
                    continue;
                };
                let observations = family
                    .facts
                    .iter()
                    .filter(|record| &record.tuple == tuple)
                    .map(|record| record.observation.clone())
                    .collect::<Vec<_>>();
                if observations.is_empty() {
                    if family.scope == input.roster.scope {
                        if let Some(evidence) = &family.completeness {
                            completeness_evidence.insert(evidence.id.clone());
                            NormalizedBool::Known(false)
                        } else {
                            NormalizedBool::Unknown
                        }
                    } else {
                        NormalizedBool::Unknown
                    }
                } else if family.scope == input.roster.scope {
                    if let Some(evidence) = &family.completeness {
                        completeness_evidence.insert(evidence.id.clone());
                    }
                    normalize_observations(&observations)
                } else {
                    NormalizedBool::Unknown
                }
            }
        };
        if matches!(normalized, NormalizedBool::Conflict(_)) {
            conflicts.insert(fact.key());
        }
        values.insert(fact.key(), normalized);
    }
    Ok(BoundFacts {
        values,
        conflicts,
        completeness_evidence,
        election_defaults,
        request_evidence,
    })
}

fn election_policies(
    compiled: &CompiledConstitution,
) -> BTreeMap<String, (String, MissingElectionPolicy)> {
    let mut policies = BTreeMap::new();
    for election in compiled
        .cuts
        .values()
        .filter_map(|rule| rule.election.as_ref())
        .chain(
            compiled
                .attachments
                .values()
                .filter_map(|rule| rule.election.as_ref()),
        )
    {
        let value = (election.actor.clone(), election.missing.clone());
        match policies.entry(election.fact.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(value);
            }
            std::collections::btree_map::Entry::Occupied(slot) => {
                debug_assert_eq!(slot.get(), &value, "compile validated election policies");
            }
        }
    }
    policies
}

fn insert_unique_ref<'a, T>(
    namespace: &'static str,
    values: &mut BTreeMap<String, &'a T>,
    id: String,
    value: &'a T,
) -> Result<(), UnitDerivationError> {
    match values.entry(id.clone()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(_) => {
            Err(UnitDerivationError::DuplicateNamespace { namespace, id })
        }
    }
}

fn normalize_observations(observations: &[ObservedBool]) -> NormalizedBool {
    let distinct = observations.iter().cloned().collect::<BTreeSet<_>>();
    if distinct.is_empty() {
        NormalizedBool::Unknown
    } else if distinct.len() == 1 {
        NormalizedBool::Known(distinct.iter().next().expect("one observation").value)
    } else {
        NormalizedBool::Conflict(distinct.iter().map(|item| item.value).collect())
    }
}

fn world_variables(
    compiled: &CompiledConstitution,
    bound: &BoundFacts,
) -> (WorldVariables, BTreeMap<String, bool>) {
    let mut variables = Vec::new();
    let mut fixed = BTreeMap::new();
    for (key, value) in &bound.values {
        match value {
            NormalizedBool::Known(value) => {
                fixed.insert(key.clone(), *value);
            }
            NormalizedBool::Unknown => variables.push((key.clone(), vec![false, true])),
            NormalizedBool::Conflict(candidates) => {
                variables.push((key.clone(), candidates.iter().copied().collect()));
            }
        }
    }
    for cut in compiled.cuts.values() {
        for precedence in &cut.edge_precedence {
            if matches!(precedence.decision, CutEdgeDecision::Unresolved { .. }) {
                variables.push((
                    legal_precedence_key(&cut.id, &precedence.edge_rule),
                    vec![false, true],
                ));
            }
        }
    }
    (variables, fixed)
}

fn variable_world_count(variables: &[(String, Vec<bool>)]) -> Result<usize, UnitDerivationError> {
    variables.iter().try_fold(1usize, |count, (_, values)| {
        count
            .checked_mul(values.len())
            .ok_or(UnitDerivationError::UncheckableWorldSpace {
                worlds: usize::MAX,
                maximum: usize::MAX,
            })
    })
}

fn enumerate_worlds(
    variables: &[(String, Vec<bool>)],
    index: usize,
    valuation: BTreeMap<String, bool>,
    worlds: &mut Vec<BTreeMap<String, bool>>,
) {
    if index == variables.len() {
        worlds.push(valuation);
        return;
    }
    let (key, values) = &variables[index];
    for value in values {
        let mut branch = valuation.clone();
        branch.insert(key.clone(), *value);
        enumerate_worlds(variables, index + 1, branch, worlds);
    }
}

fn eval_expr(
    expr: &BoolExpr,
    valuation: &BTreeMap<String, bool>,
    derived: &BTreeMap<String, DerivedBool>,
    visiting: &mut BTreeSet<String>,
) -> Result<bool, UnitDerivationError> {
    match expr {
        BoolExpr::Literal(value) => Ok(*value),
        BoolExpr::Fact(fact) => valuation.get(&fact.key()).copied().ok_or_else(|| {
            UnitDerivationError::UnknownReference {
                from: "valuation".to_string(),
                reference: fact.key(),
            }
        }),
        BoolExpr::Derived(reference) => {
            if !visiting.insert(reference.clone()) {
                return Err(UnitDerivationError::CyclicDependency(reference.clone()));
            }
            let rule =
                derived
                    .get(reference)
                    .ok_or_else(|| UnitDerivationError::UnknownReference {
                        from: "constitution expression".to_string(),
                        reference: reference.clone(),
                    })?;
            let result = eval_expr(&rule.expr, valuation, derived, visiting);
            visiting.remove(reference);
            result
        }
        BoolExpr::And(items) => {
            for item in items {
                if !eval_expr(item, valuation, derived, visiting)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        BoolExpr::Or(items) => {
            for item in items {
                if eval_expr(item, valuation, derived, visiting)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        BoolExpr::Not(item) => Ok(!eval_expr(item, valuation, derived, visiting)?),
    }
}

fn evaluate_world(
    compiled: &CompiledConstitution,
    persons: &BTreeSet<String>,
    valuation: &BTreeMap<String, bool>,
    influence: &BTreeMap<String, BTreeSet<String>>,
    bound: &BoundFacts,
) -> Result<WorldResult, UnitDerivationError> {
    let mut events = BTreeSet::new();
    let mut frozen = Vec::new();
    for edge in compiled.edges.values() {
        let active = eval_expr(
            &edge.when,
            valuation,
            &compiled.derived_bools,
            &mut BTreeSet::new(),
        )?;
        let mut defeated = false;
        for defeater in &edge.defeaters {
            if eval_expr(
                &defeater.when,
                valuation,
                &compiled.derived_bools,
                &mut BTreeSet::new(),
            )? {
                defeated = true;
                break;
            }
        }
        if active && !defeated {
            events.insert(TraceEvent::FrozenEdge {
                rule: edge.id.clone(),
                left: edge.left.clone(),
                right: edge.right.clone(),
            });
            frozen.push(FrozenEdge {
                id: edge.id.clone(),
                kind: edge.kind,
                left: edge.left.clone(),
                right: edge.right.clone(),
                citation: edge.citation.clone(),
            });
        }
    }

    let mut forced_indeterminate = BTreeMap::new();
    if compiled.plan.base_chain_policy.is_none() {
        for component in graph_components(
            persons,
            frozen.iter().filter(|edge| edge.kind == EdgeKind::Base),
        ) {
            if component.len() > 2 && !is_edge_clique(&component, &frozen, EdgeKind::Base) {
                let mut listed = component.iter().cloned().collect::<Vec<_>>();
                listed.sort();
                events.insert(TraceEvent::DiscretionNotEncoded {
                    persons: listed.clone(),
                });
                for person in influence_closure_from_seed(compiled, &component, influence) {
                    forced_indeterminate.insert(person, IndeterminateKind::DiscretionNotEncoded);
                }
            }
        }
    }

    let mut active_cuts = Vec::new();
    for cut in compiled.cuts.values() {
        if eval_expr(
            &cut.when,
            valuation,
            &compiled.derived_bools,
            &mut BTreeSet::new(),
        )? {
            active_cuts.push(ActiveCut {
                ids: BTreeSet::from([cut.id.clone()]),
                members: cut.members.clone(),
                citations: BTreeSet::from([cut.citation.clone()]),
            });
        }
    }
    apply_cut_precedence(compiled, &mut active_cuts);
    let mut coalesced = BTreeMap::<BTreeSet<String>, ActiveCut>::new();
    for cut in active_cuts {
        let entry = coalesced
            .entry(cut.members.clone())
            .or_insert_with(|| ActiveCut {
                ids: BTreeSet::new(),
                members: cut.members.clone(),
                citations: BTreeSet::new(),
            });
        entry.ids.extend(cut.ids);
        entry.citations.extend(cut.citations);
    }
    let mut active_cuts = coalesced.into_values().collect::<Vec<_>>();

    let mut rejected = BTreeSet::new();
    for left_index in 0..active_cuts.len() {
        for right_index in (left_index + 1)..active_cuts.len() {
            let left = &active_cuts[left_index];
            let right = &active_cuts[right_index];
            if !left.members.is_disjoint(&right.members) {
                let left_id = left.ids.iter().next().expect("active cut id").clone();
                let right_id = right.ids.iter().next().expect("active cut id").clone();
                events.insert(TraceEvent::CutOverlapConflict {
                    left: left_id,
                    right: right_id,
                });
                let seed = left
                    .members
                    .union(&right.members)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                for person in influence_closure_from_seed(compiled, &seed, influence) {
                    forced_indeterminate
                        .insert(person, IndeterminateKind::ConstitutionOverlapConflict);
                }
                rejected.insert(left_index);
                rejected.insert(right_index);
            }
        }
    }
    active_cuts = active_cuts
        .into_iter()
        .enumerate()
        .filter_map(|(index, cut)| (!rejected.contains(&index)).then_some(cut))
        .collect();

    let mut applied = Vec::new();
    for cut in active_cuts {
        let mut blocked = false;
        for edge in frozen
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Combination)
        {
            if cut.members.contains(&edge.left) == cut.members.contains(&edge.right) {
                continue;
            }
            for cut_id in &cut.ids {
                let rule = &compiled.cuts[cut_id];
                let precedence = rule
                    .edge_precedence
                    .iter()
                    .find(|item| item.edge_rule == edge.id)
                    .expect("compile requires crossing precedence");
                let edge_blocks = match &precedence.decision {
                    CutEdgeDecision::Blocked { .. } => true,
                    CutEdgeDecision::Overrides { .. } => false,
                    CutEdgeDecision::Unresolved { .. } => {
                        !valuation[&legal_precedence_key(&rule.id, &precedence.edge_rule)]
                    }
                };
                if edge_blocks {
                    blocked = true;
                    events.insert(TraceEvent::CutBlocked {
                        cut: rule.id.clone(),
                        edge_rule: edge.id.clone(),
                        actor: rule
                            .election
                            .as_ref()
                            .map(|election| election.actor.clone()),
                        request_evidence: rule
                            .election
                            .as_ref()
                            .and_then(|election| bound.request_evidence.get(&election.fact))
                            .cloned()
                            .unwrap_or_default(),
                    });
                }
            }
        }
        if !blocked {
            for id in &cut.ids {
                let rule = &compiled.cuts[id];
                events.insert(TraceEvent::CutApplied {
                    rule: id.clone(),
                    actor: rule
                        .election
                        .as_ref()
                        .map(|election| election.actor.clone()),
                    request_evidence: rule
                        .election
                        .as_ref()
                        .and_then(|election| bound.request_evidence.get(&election.fact))
                        .cloned()
                        .unwrap_or_default(),
                });
            }
            applied.push(cut);
        }
    }

    let excluded = forced_indeterminate
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut cut_members = BTreeSet::new();
    let mut blocks = Vec::<BTreeSet<String>>::new();
    for cut in &applied {
        let members = cut
            .members
            .difference(&excluded)
            .cloned()
            .collect::<BTreeSet<_>>();
        cut_members.extend(members.iter().cloned());
        if !members.is_empty() {
            blocks.push(members);
        }
    }
    let residue = persons
        .difference(&excluded)
        .filter(|person| !cut_members.contains(*person))
        .cloned()
        .collect::<BTreeSet<_>>();
    blocks.extend(graph_components(
        &residue,
        frozen
            .iter()
            .filter(|edge| residue.contains(&edge.left) && residue.contains(&edge.right)),
    ));

    let mut attached = BTreeSet::new();
    for attachment in compiled.attachments.values() {
        if !eval_expr(
            &attachment.when,
            valuation,
            &compiled.derived_bools,
            &mut BTreeSet::new(),
        )? {
            continue;
        }
        let Some(target_index) = blocks
            .iter()
            .position(|block| block.contains(&attachment.target_anchor))
        else {
            continue;
        };
        let moving = attachment
            .members
            .difference(&excluded)
            .cloned()
            .collect::<BTreeSet<_>>();
        for block in &mut blocks {
            block.retain(|person| !moving.contains(person));
        }
        blocks[target_index].extend(moving.iter().cloned());
        blocks.retain(|block| !block.is_empty());
        attached.extend(moving);
        events.insert(TraceEvent::Attachment {
            rule: attachment.id.clone(),
            target_anchor: attachment.target_anchor.clone(),
            actor: attachment.actor.clone(),
            request_evidence: attachment
                .election
                .as_ref()
                .and_then(|election| bound.request_evidence.get(&election.fact))
                .cloned()
                .unwrap_or_default(),
        });
    }
    canonicalize_blocks(&mut blocks);

    let mut roles = persons
        .iter()
        .map(|person| (person.clone(), ProjectionRole::Member))
        .collect::<BTreeMap<_, _>>();
    for bar in compiled.bars.values() {
        if eval_expr(
            &bar.when,
            valuation,
            &compiled.derived_bools,
            &mut BTreeSet::new(),
        )? && !bar.members.is_subset(&attached)
        {
            for person in &bar.members {
                roles.insert(person.clone(), ProjectionRole::BarredIndependent);
            }
            events.insert(TraceEvent::IndependentBar {
                rule: bar.id.clone(),
            });
        }
    }
    for status in compiled.statuses.values() {
        if eval_expr(
            &status.when,
            valuation,
            &compiled.derived_bools,
            &mut BTreeSet::new(),
        )? {
            roles.insert(status.person.clone(), ProjectionRole::Excluded);
            events.insert(TraceEvent::StatusExcluded {
                rule: status.id.clone(),
                person: status.person.clone(),
            });
        }
    }

    let mut world_persons = BTreeMap::new();
    for person in persons {
        if let Some(kind) = forced_indeterminate.get(person) {
            world_persons.insert(
                person.clone(),
                WorldPerson {
                    block: None,
                    role: None,
                    kind: Some(kind.clone()),
                    citations: BTreeSet::new(),
                },
            );
            continue;
        }
        let block = blocks
            .iter()
            .find(|block| block.contains(person))
            .expect("every determined roster person is partitioned");
        let block_vec = block.iter().cloned().collect::<Vec<_>>();
        let mut citations = BTreeSet::new();
        for edge in &frozen {
            if block.contains(&edge.left) && block.contains(&edge.right) {
                citations.insert(edge.citation.clone());
            }
        }
        for cut in &applied {
            let residual_of_cut = frozen.iter().any(|edge| {
                (block.contains(&edge.left) && cut.members.contains(&edge.right))
                    || (block.contains(&edge.right) && cut.members.contains(&edge.left))
            });
            if cut.members.contains(person) || residual_of_cut {
                citations.extend(cut.citations.iter().cloned());
            }
        }
        for attachment in compiled.attachments.values() {
            if block.contains(&attachment.target_anchor)
                && attachment
                    .members
                    .iter()
                    .any(|member| block.contains(member))
            {
                citations.insert(attachment.citation.clone());
            }
        }
        for bar in compiled.bars.values() {
            if roles.get(person) == Some(&ProjectionRole::BarredIndependent)
                && bar.members.contains(person)
            {
                citations.insert(bar.citation.clone());
            }
        }
        for status in compiled.statuses.values() {
            if roles.get(person) == Some(&ProjectionRole::Excluded) && status.person == *person {
                citations.insert(status.citation.clone());
            }
        }
        world_persons.insert(
            person.clone(),
            WorldPerson {
                block: Some(block_vec),
                role: roles.get(person).copied(),
                kind: None,
                citations,
            },
        );
    }
    Ok(WorldResult {
        persons: world_persons,
        events,
    })
}

fn graph_components<'a>(
    vertices: &BTreeSet<String>,
    edges: impl Iterator<Item = &'a FrozenEdge>,
) -> Vec<BTreeSet<String>> {
    let mut adjacency = vertices
        .iter()
        .map(|person| (person.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        if vertices.contains(&edge.left) && vertices.contains(&edge.right) {
            adjacency
                .get_mut(&edge.left)
                .expect("edge endpoint in vertices")
                .insert(edge.right.clone());
            adjacency
                .get_mut(&edge.right)
                .expect("edge endpoint in vertices")
                .insert(edge.left.clone());
        }
    }
    let mut remaining = vertices.clone();
    let mut components = Vec::new();
    while let Some(seed) = remaining.iter().next().cloned() {
        let mut component = BTreeSet::new();
        let mut queue = VecDeque::from([seed]);
        while let Some(person) = queue.pop_front() {
            if !remaining.remove(&person) {
                continue;
            }
            component.insert(person.clone());
            queue.extend(adjacency[&person].iter().cloned());
        }
        components.push(component);
    }
    canonicalize_blocks(&mut components);
    components
}

fn canonicalize_blocks(blocks: &mut Vec<BTreeSet<String>>) {
    blocks.sort_by(|left, right| left.iter().cmp(right.iter()));
    blocks.dedup();
}

fn is_edge_clique(component: &BTreeSet<String>, edges: &[FrozenEdge], kind: EdgeKind) -> bool {
    let members = component.iter().collect::<Vec<_>>();
    for (index, left) in members.iter().enumerate() {
        for right in members.iter().skip(index + 1) {
            if !edges.iter().any(|edge| {
                edge.kind == kind
                    && ((&edge.left == *left && &edge.right == *right)
                        || (&edge.left == *right && &edge.right == *left))
            }) {
                return false;
            }
        }
    }
    true
}

fn apply_cut_precedence(compiled: &CompiledConstitution, cuts: &mut Vec<ActiveCut>) {
    let mut rejected = BTreeSet::new();
    for left_index in 0..cuts.len() {
        for right_index in (left_index + 1)..cuts.len() {
            if cuts[left_index]
                .members
                .is_disjoint(&cuts[right_index].members)
            {
                continue;
            }
            let left_precedes =
                cut_set_precedes(compiled, &cuts[left_index].ids, &cuts[right_index].ids);
            let right_precedes =
                cut_set_precedes(compiled, &cuts[right_index].ids, &cuts[left_index].ids);
            match (left_precedes, right_precedes) {
                (true, false) => {
                    rejected.insert(right_index);
                }
                (false, true) => {
                    rejected.insert(left_index);
                }
                _ => {}
            }
        }
    }
    *cuts = cuts
        .drain(..)
        .enumerate()
        .filter_map(|(index, cut)| (!rejected.contains(&index)).then_some(cut))
        .collect();
}

fn cut_set_precedes(
    compiled: &CompiledConstitution,
    higher: &BTreeSet<String>,
    lower: &BTreeSet<String>,
) -> bool {
    let mut pending = higher.iter().cloned().collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        for order in &compiled.cuts[&id].precedes {
            if lower.contains(&order.lower_priority_cut) {
                return true;
            }
            pending.push(order.lower_priority_cut.clone());
        }
    }
    false
}

fn influence_sets(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    bound: &BoundFacts,
) -> BTreeMap<String, BTreeSet<String>> {
    let roster = input
        .roster
        .persons
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let unresolved = bound
        .values
        .iter()
        .filter_map(|(key, value)| {
            (!matches!(value, NormalizedBool::Known(_))).then_some(key.clone())
        })
        .chain(compiled.cuts.values().flat_map(|cut| {
            cut.edge_precedence
                .iter()
                .filter(|precedence| {
                    matches!(precedence.decision, CutEdgeDecision::Unresolved { .. })
                })
                .map(|precedence| legal_precedence_key(&cut.id, &precedence.edge_rule))
        }))
        .collect::<BTreeSet<_>>();
    let facts_by_key = all_fact_refs(compiled, input)
        .into_iter()
        .map(|fact| (fact.key(), fact))
        .collect::<BTreeMap<_, _>>();
    let constraint_facts = input
        .integrity_constraints
        .iter()
        .flat_map(|constraint| expr_fact_refs(&constraint.expr, &compiled.derived_bools))
        .map(|fact| fact.key())
        .collect::<BTreeSet<_>>();

    let mut result = BTreeMap::new();
    for key in unresolved {
        let mut closure = facts_by_key
            .get(&key)
            .map_or_else(BTreeSet::new, |fact| fact.named_persons());
        if constraint_facts.contains(&key) {
            closure.extend(roster.iter().cloned());
        }
        for edge in compiled.edges.values() {
            if expression_uses_key(&edge.when, key.as_str(), compiled)
                || edge
                    .defeaters
                    .iter()
                    .any(|item| expression_uses_key(&item.when, key.as_str(), compiled))
            {
                closure.insert(edge.left.clone());
                closure.insert(edge.right.clone());
            }
        }
        for cut in compiled.cuts.values() {
            if expression_uses_key(&cut.when, key.as_str(), compiled)
                || cut
                    .edge_precedence
                    .iter()
                    .any(|precedence| legal_precedence_key(&cut.id, &precedence.edge_rule) == key)
            {
                closure.extend(cut.members.iter().cloned());
                for precedence in &cut.edge_precedence {
                    if legal_precedence_key(&cut.id, &precedence.edge_rule) == key {
                        let edge = &compiled.edges[&precedence.edge_rule];
                        closure.insert(edge.left.clone());
                        closure.insert(edge.right.clone());
                    }
                }
            }
        }
        for attachment in compiled.attachments.values() {
            if expression_uses_key(&attachment.when, key.as_str(), compiled) {
                closure.extend(attachment.members.iter().cloned());
                closure.insert(attachment.target_anchor.clone());
            }
        }
        for bar in compiled.bars.values() {
            if expression_uses_key(&bar.when, key.as_str(), compiled) {
                closure.extend(bar.members.iter().cloned());
            }
        }
        for status in compiled.statuses.values() {
            if expression_uses_key(&status.when, key.as_str(), compiled) {
                closure.insert(status.person.clone());
            }
        }
        close_potential_influence(compiled, &mut closure);
        result.insert(key, closure);
    }
    result
}

fn expression_uses_key(expr: &BoolExpr, key: &str, compiled: &CompiledConstitution) -> bool {
    expr_fact_refs(expr, &compiled.derived_bools)
        .iter()
        .any(|fact| fact.key() == key)
}

fn close_potential_influence(compiled: &CompiledConstitution, closure: &mut BTreeSet<String>) {
    loop {
        let before = closure.len();
        for edge in compiled.edges.values() {
            if closure.contains(&edge.left) || closure.contains(&edge.right) {
                closure.insert(edge.left.clone());
                closure.insert(edge.right.clone());
            }
        }
        for cut in compiled.cuts.values() {
            if !closure.is_disjoint(&cut.members) {
                closure.extend(cut.members.iter().cloned());
            }
        }
        for attachment in compiled.attachments.values() {
            let touches = closure.contains(&attachment.target_anchor)
                || !closure.is_disjoint(&attachment.members);
            if touches {
                closure.extend(attachment.members.iter().cloned());
                closure.insert(attachment.target_anchor.clone());
            }
        }
        for bar in compiled.bars.values() {
            if !closure.is_disjoint(&bar.members) {
                closure.extend(bar.members.iter().cloned());
            }
        }
        if closure.len() == before {
            break;
        }
    }
}

fn influence_closure_from_seed(
    compiled: &CompiledConstitution,
    seed: &BTreeSet<String>,
    influence: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut closure = seed.clone();
    loop {
        let before = closure.len();
        for affected in influence.values() {
            if !closure.is_disjoint(affected) {
                closure.extend(affected.iter().cloned());
            }
        }
        if closure.len() == before {
            close_potential_influence(compiled, &mut closure);
            return closure;
        }
    }
}

fn assemble_result(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    persons: &BTreeSet<String>,
    bound: &BoundFacts,
    influence: &BTreeMap<String, BTreeSet<String>>,
    worlds: Vec<WorldResult>,
) -> Result<UnitDerivationResult, UnitDerivationError> {
    let mut events = BTreeSet::new();
    for world in &worlds {
        events.extend(world.events.iter().cloned());
    }
    events.extend(
        bound
            .election_defaults
            .iter()
            .map(
                |(fact, (actor, value, citation))| TraceEvent::ElectionDefault {
                    fact: fact.clone(),
                    actor: actor.clone(),
                    value: *value,
                    citation: citation.clone(),
                },
            ),
    );
    let trace_root = trace_root(compiled, input, worlds.len(), &events);
    let provenance = Provenance::Derived {
        constitution: compiled.plan.id.clone(),
        trace_root: trace_root.clone(),
    };

    let mut determined_blocks = BTreeMap::<String, Vec<String>>::new();
    let mut determined_roles = BTreeMap::<String, ProjectionRole>::new();
    let mut indeterminate = Vec::new();
    let mut citations_by_person = BTreeMap::<String, BTreeSet<Citation>>::new();

    for person in persons {
        let states = worlds
            .iter()
            .map(|world| &world.persons[person])
            .collect::<Vec<_>>();
        let first_block = states[0].block.clone();
        let block_invariant =
            first_block.is_some() && states.iter().all(|state| state.block == first_block);
        if block_invariant {
            determined_blocks.insert(person.clone(), first_block.expect("checked Some"));
            let role = states[0].role;
            if let Some(role) = role
                && states.iter().all(|state| state.role == Some(role))
            {
                determined_roles.insert(person.clone(), role);
            } else {
                indeterminate.push(IndeterminatePerson {
                    person: person.clone(),
                    kind: IndeterminateKind::Role(Projection::ParticipatingMember),
                    unresolved_facts: unresolved_for_person(person, influence),
                });
            }
        } else {
            let kind = states
                .iter()
                .find_map(|state| state.kind.clone())
                .unwrap_or(IndeterminateKind::Membership);
            indeterminate.push(IndeterminatePerson {
                person: person.clone(),
                kind,
                unresolved_facts: unresolved_for_person(person, influence),
            });
        }
        let citations = citations_by_person.entry(person.clone()).or_default();
        for state in states {
            citations.extend(state.citations.iter().cloned());
        }
    }

    let invariant_blocks = determined_blocks
        .values()
        .filter(|block| {
            block.iter().all(|member| {
                determined_blocks
                    .get(member)
                    .is_some_and(|candidate| candidate == *block)
            })
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut registry = EntityRegistry::default();
    for entity in &input.supplied_entities {
        registry.bind_supplied(
            entity.entity_type.clone(),
            entity.id.clone(),
            entity.evidence.id.clone(),
        )?;
    }
    let roster_evidence = input
        .roster
        .completeness
        .as_ref()
        .expect("incomplete roster returned before assembly")
        .id
        .clone();
    for person in persons {
        let entity_type = input
            .supplied_entities
            .iter()
            .find(|entity| entity.id == *person)
            .map_or("person", |entity| entity.entity_type.as_str());
        registry.bind_supplied(entity_type, person.clone(), roster_evidence.clone())?;
    }

    let mut units = Vec::new();
    let mut ids_by_block = BTreeMap::new();
    for block in invariant_blocks {
        let digest_id = unit_id(
            compiled.semantics_digest,
            &compiled.plan.relations.unit_constituent,
            &input.roster.scope,
            &input.segment,
            &block,
        );
        let id = format!("{}:{digest_id}", compiled.plan.entity_type);
        ids_by_block.insert(block.clone(), id.clone());
        let unit = DerivedUnit {
            entity_type: compiled.plan.entity_type.clone(),
            id,
            members: block,
            provenance: provenance.clone(),
        };
        registry.insert_derived(&unit)?;
        units.push(unit);
    }
    units.sort_by(|left, right| left.id.cmp(&right.id));

    let mut memberships = Vec::new();
    for (person, block) in &determined_blocks {
        let Some(unit) = ids_by_block.get(block) else {
            continue;
        };
        memberships.push(MembershipTuple {
            relation: compiled.plan.relations.unit_constituent.clone(),
            unit: unit.clone(),
            person: person.clone(),
            projection: Projection::UnitConstituent,
            role: ProjectionRole::Member,
            citations: citations_by_person[person].clone(),
            provenance: provenance.clone(),
        });
        if let Some(role) = determined_roles.get(person) {
            memberships.push(MembershipTuple {
                relation: compiled.plan.relations.participating_member.clone(),
                unit: unit.clone(),
                person: person.clone(),
                projection: Projection::ParticipatingMember,
                role: *role,
                citations: citations_by_person[person].clone(),
                provenance: provenance.clone(),
            });
        }
    }
    memberships.sort_by(|left, right| {
        (
            left.relation.as_str(),
            left.unit.as_str(),
            left.person.as_str(),
            left.projection,
            left.role,
        )
            .cmp(&(
                right.relation.as_str(),
                right.unit.as_str(),
                right.person.as_str(),
                right.projection,
                right.role,
            ))
    });
    memberships.dedup();
    indeterminate.sort_by(|left, right| {
        (left.person.as_str(), format!("{:?}", left.kind))
            .cmp(&(right.person.as_str(), format!("{:?}", right.kind)))
    });
    indeterminate.dedup();

    let input_conflict_impacts = conflict_impacts(compiled, input, bound, influence);

    Ok(UnitDerivationResult {
        relations: compiled.plan.relations.clone(),
        units,
        memberships,
        indeterminate,
        trace: DerivationTrace {
            root: trace_root,
            worlds_evaluated: worlds.len(),
            events,
            completeness_evidence: bound.completeness_evidence.clone(),
            input_conflicts: bound.conflicts.clone(),
            input_conflict_impacts,
        },
    })
}

fn unresolved_for_person(
    person: &str,
    influence: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    influence
        .iter()
        .filter_map(|(fact, persons)| persons.contains(person).then_some(fact.clone()))
        .collect()
}

fn roster_incomplete_result(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    persons: &BTreeSet<String>,
) -> UnitDerivationResult {
    UnitDerivationResult {
        relations: compiled.plan.relations.clone(),
        units: Vec::new(),
        memberships: Vec::new(),
        indeterminate: persons
            .iter()
            .map(|person| IndeterminatePerson {
                person: person.clone(),
                kind: IndeterminateKind::RosterNotAssertedComplete,
                unresolved_facts: BTreeSet::from(["roster_not_asserted_complete".to_string()]),
            })
            .collect(),
        trace: DerivationTrace {
            root: trace_root(compiled, input, 0, &BTreeSet::new()),
            worlds_evaluated: 0,
            events: BTreeSet::new(),
            completeness_evidence: BTreeSet::new(),
            input_conflicts: BTreeSet::new(),
            input_conflict_impacts: BTreeSet::new(),
        },
    }
}

fn inconsistent_result(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    persons: &BTreeSet<String>,
    bound: &BoundFacts,
) -> UnitDerivationResult {
    UnitDerivationResult {
        relations: compiled.plan.relations.clone(),
        units: Vec::new(),
        memberships: Vec::new(),
        indeterminate: persons
            .iter()
            .map(|person| IndeterminatePerson {
                person: person.clone(),
                kind: IndeterminateKind::InconsistentInputs,
                unresolved_facts: bound.values.keys().cloned().collect(),
            })
            .collect(),
        trace: DerivationTrace {
            root: trace_root(compiled, input, 0, &BTreeSet::new()),
            worlds_evaluated: 0,
            events: BTreeSet::new(),
            completeness_evidence: bound.completeness_evidence.clone(),
            input_conflicts: bound.conflicts.clone(),
            input_conflict_impacts: BTreeSet::new(),
        },
    }
}

fn conflict_impacts(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    bound: &BoundFacts,
    influence: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<InputConflictImpact> {
    let constraint_keys = input
        .integrity_constraints
        .iter()
        .flat_map(|constraint| expr_fact_refs(&constraint.expr, &compiled.derived_bools))
        .map(|fact| fact.key())
        .collect::<BTreeSet<_>>();
    bound
        .conflicts
        .iter()
        .map(|fact| {
            let affects_composition = compiled.edges.values().any(|rule| {
                expression_uses_key(&rule.when, fact, compiled)
                    || rule
                        .defeaters
                        .iter()
                        .any(|item| expression_uses_key(&item.when, fact, compiled))
            }) || compiled
                .cuts
                .values()
                .any(|rule| expression_uses_key(&rule.when, fact, compiled))
                || compiled
                    .attachments
                    .values()
                    .any(|rule| expression_uses_key(&rule.when, fact, compiled))
                || constraint_keys.contains(fact);
            let affects_participation = affects_composition
                || compiled
                    .bars
                    .values()
                    .any(|rule| expression_uses_key(&rule.when, fact, compiled))
                || compiled
                    .statuses
                    .values()
                    .any(|rule| expression_uses_key(&rule.when, fact, compiled));
            let mut projections = BTreeSet::new();
            if affects_composition {
                projections.insert(Projection::UnitConstituent);
            }
            if affects_participation {
                projections.insert(Projection::ParticipatingMember);
            }
            InputConflictImpact {
                fact: fact.clone(),
                persons: influence.get(fact).cloned().unwrap_or_default(),
                projections,
            }
        })
        .collect()
}

fn trace_root(
    compiled: &CompiledConstitution,
    input: &ConstitutionInput,
    worlds: usize,
    events: &BTreeSet<TraceEvent>,
) -> String {
    let mut encoder = TraceEncoder::new(b"axiom.unit-derivation.trace.stage2\0");
    encoder.fixed_bytes(&compiled.semantics_digest);
    encoder.string(&compiled.plan.id);
    encoder.constitution_input(input);
    encoder.usize(worlds);
    encoder.sorted_blobs(
        events
            .iter()
            .map(|event| trace_blob(|encoder| encoder.trace_event(event)))
            .collect(),
    );
    format!("sha256:{}", hex(&sha256(&encoder.finish())))
}

/// Structural, canonical encoding for the stage-2 trace commitment. Set-like
/// request collections are sorted by their encoded bytes and exact duplicate
/// records are removed, matching the binder's normalization. Tuple slots and
/// expression children retain their authored order because that order is
/// semantically meaningful. No `Debug` representation enters the preimage.
struct TraceEncoder {
    bytes: Vec<u8>,
}

impl TraceEncoder {
    fn new(domain: &[u8]) -> Self {
        Self {
            bytes: domain.to_vec(),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.byte(u8::from(value));
    }

    fn usize(&mut self, value: usize) {
        self.bytes.extend_from_slice(
            &u64::try_from(value)
                .expect("usize always fits into the trace encoding's u64 length")
                .to_be_bytes(),
        );
    }

    fn fixed_bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.usize(value.len());
        self.fixed_bytes(value);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn strings_in_order<'a>(&mut self, values: impl IntoIterator<Item = &'a String>) {
        let values = values.into_iter().collect::<Vec<_>>();
        self.usize(values.len());
        for value in values {
            self.string(value);
        }
    }

    fn sorted_strings<'a>(&mut self, values: impl IntoIterator<Item = &'a String>) {
        let mut values = values.into_iter().collect::<Vec<_>>();
        values.sort();
        values.dedup();
        self.usize(values.len());
        for value in values {
            self.string(value);
        }
    }

    fn sorted_blobs(&mut self, mut values: Vec<Vec<u8>>) {
        values.sort();
        values.dedup();
        self.usize(values.len());
        for value in values {
            self.bytes(&value);
        }
    }

    fn option_string(&mut self, value: Option<&String>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.string(value);
            }
            None => self.byte(0),
        }
    }

    fn citation(&mut self, citation: &Citation) {
        self.string(&citation.provision);
        self.string(&citation.authority);
    }

    fn evidence(&mut self, evidence: &Evidence) {
        self.string(&evidence.id);
        self.citation(&evidence.citation);
    }

    fn option_evidence(&mut self, evidence: Option<&Evidence>) {
        match evidence {
            Some(evidence) => {
                self.byte(1);
                self.evidence(evidence);
            }
            None => self.byte(0),
        }
    }

    fn observed_bool(&mut self, observation: &ObservedBool) {
        self.bool(observation.value);
        self.evidence(&observation.evidence);
    }

    fn bool_expr(&mut self, expr: &BoolExpr) {
        match expr {
            BoolExpr::Literal(value) => {
                self.byte(0);
                self.bool(*value);
            }
            BoolExpr::Fact(FactRef::Bool(name)) => {
                self.byte(1);
                self.string(name);
            }
            BoolExpr::Fact(FactRef::Relation { family, tuple }) => {
                self.byte(2);
                self.string(family);
                self.strings_in_order(tuple);
            }
            BoolExpr::Derived(name) => {
                self.byte(3);
                self.string(name);
            }
            BoolExpr::And(items) => {
                self.byte(4);
                self.usize(items.len());
                for item in items {
                    self.bool_expr(item);
                }
            }
            BoolExpr::Or(items) => {
                self.byte(5);
                self.usize(items.len());
                for item in items {
                    self.bool_expr(item);
                }
            }
            BoolExpr::Not(item) => {
                self.byte(6);
                self.bool_expr(item);
            }
        }
    }

    fn constitution_input(&mut self, input: &ConstitutionInput) {
        self.string(&input.roster.relation);
        self.string(&input.roster.scope);
        self.sorted_strings(&input.roster.persons);
        self.option_evidence(input.roster.completeness.as_ref());
        self.string(&input.segment);
        self.bool(input.segment_complete);

        self.sorted_blobs(
            input
                .relation_families
                .iter()
                .map(|family| {
                    trace_blob(|encoder| {
                        encoder.string(&family.name);
                        encoder.string(&family.scope);
                        encoder.option_evidence(family.completeness.as_ref());
                        encoder.sorted_blobs(
                            family
                                .facts
                                .iter()
                                .map(|fact| {
                                    trace_blob(|encoder| {
                                        encoder.strings_in_order(&fact.tuple);
                                        encoder.observed_bool(&fact.observation);
                                    })
                                })
                                .collect(),
                        );
                    })
                })
                .collect(),
        );

        self.sorted_blobs(
            input
                .bool_facts
                .iter()
                .map(|fact| {
                    trace_blob(|encoder| {
                        encoder.string(&fact.name);
                        encoder.sorted_blobs(
                            fact.observations
                                .iter()
                                .map(|observation| {
                                    trace_blob(|encoder| encoder.observed_bool(observation))
                                })
                                .collect(),
                        );
                        encoder.option_evidence(fact.explicit_unknown.as_ref());
                    })
                })
                .collect(),
        );

        self.sorted_blobs(
            input
                .supplied_entities
                .iter()
                .map(|entity| {
                    trace_blob(|encoder| {
                        encoder.string(&entity.entity_type);
                        encoder.string(&entity.id);
                        encoder.evidence(&entity.evidence);
                    })
                })
                .collect(),
        );

        self.sorted_blobs(
            input
                .integrity_constraints
                .iter()
                .map(|constraint| {
                    trace_blob(|encoder| {
                        encoder.string(&constraint.id);
                        encoder.bool_expr(&constraint.expr);
                        encoder.citation(&constraint.citation);
                    })
                })
                .collect(),
        );
    }

    fn trace_event(&mut self, event: &TraceEvent) {
        match event {
            TraceEvent::FrozenEdge { rule, left, right } => {
                self.byte(0);
                self.string(rule);
                self.string(left);
                self.string(right);
            }
            TraceEvent::CutApplied {
                rule,
                actor,
                request_evidence,
            } => {
                self.byte(1);
                self.string(rule);
                self.option_string(actor.as_ref());
                self.sorted_strings(request_evidence);
            }
            TraceEvent::CutBlocked {
                cut,
                edge_rule,
                actor,
                request_evidence,
            } => {
                self.byte(2);
                self.string(cut);
                self.string(edge_rule);
                self.option_string(actor.as_ref());
                self.sorted_strings(request_evidence);
            }
            TraceEvent::CutOverlapConflict { left, right } => {
                self.byte(3);
                self.string(left);
                self.string(right);
            }
            TraceEvent::Attachment {
                rule,
                target_anchor,
                actor,
                request_evidence,
            } => {
                self.byte(4);
                self.string(rule);
                self.string(target_anchor);
                self.string(actor);
                self.sorted_strings(request_evidence);
            }
            TraceEvent::IndependentBar { rule } => {
                self.byte(5);
                self.string(rule);
            }
            TraceEvent::StatusExcluded { rule, person } => {
                self.byte(6);
                self.string(rule);
                self.string(person);
            }
            TraceEvent::ElectionDefault {
                fact,
                actor,
                value,
                citation,
            } => {
                self.byte(7);
                self.string(fact);
                self.string(actor);
                self.bool(*value);
                self.citation(citation);
            }
            TraceEvent::DiscretionNotEncoded { persons } => {
                self.byte(8);
                self.sorted_strings(persons);
            }
        }
    }
}

fn trace_blob(encode: impl FnOnce(&mut TraceEncoder)) -> Vec<u8> {
    let mut encoder = TraceEncoder::new(&[]);
    encode(&mut encoder);
    encoder.finish()
}

/// Content-derived identity from the fixed stage-2 preimage:
///
/// `domain_tag || semantics_digest || LP(relation) || LP(scope) ||
/// LP(segment) || member_count(u32-be) || LP(sorted canonical member id)*`.
///
/// `LP` is a four-byte big-endian byte length followed by UTF-8 bytes. The
/// returned value is the digest portion (`sha256:...`); emission prefixes the
/// declared entity type. Record order, evidence, shadow tuples, and labels are
/// absent from the preimage.
pub fn unit_id(
    semantics_digest: [u8; 32],
    canonical_relation_id: &str,
    roster_scope: &str,
    segment: &str,
    members: &[String],
) -> String {
    let mut canonical_members = members.to_vec();
    canonical_members.sort();
    canonical_members.dedup();
    let mut preimage = b"axiom.unit-derivation.stage2\0".to_vec();
    preimage.extend_from_slice(&semantics_digest);
    push_len_prefixed(&mut preimage, canonical_relation_id.as_bytes());
    push_len_prefixed(&mut preimage, roster_scope.as_bytes());
    push_len_prefixed(&mut preimage, segment.as_bytes());
    preimage.extend_from_slice(&(canonical_members.len() as u32).to_be_bytes());
    for member in canonical_members {
        push_len_prefixed(&mut preimage, member.as_bytes());
    }
    format!("sha256:{}", hex(&sha256(&preimage)))
}

fn push_len_prefixed(preimage: &mut Vec<u8>, bytes: &[u8]) {
    preimage.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    preimage.extend_from_slice(bytes);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn legal_precedence_key(cut: &str, edge: &str) -> String {
    format!("legal-precedence:{cut}:{edge}")
}

// Small, dependency-free SHA-256 implementation for the experimental identity
// preimage. Keeping it local avoids changing the default dependency surface.
pub(crate) fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let big1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(big1)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let big0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = big0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut output = [0u8; 32];
    for (index, word) in state.into_iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}
