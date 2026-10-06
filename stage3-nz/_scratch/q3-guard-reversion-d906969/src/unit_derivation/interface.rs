use std::collections::{BTreeMap, BTreeSet};

use super::compile::CompiledConstitution;
use super::evaluate::derive_units;
use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryEntry {
    pub entity_type: String,
    pub id: String,
    pub provenance: Provenance,
}

/// Typed runtime entity-instance registry. The key is the instance id rather
/// than `(type, id)`, so a cross-type collision cannot pass unnoticed.
#[derive(Clone, Debug, Default)]
pub struct EntityRegistry {
    entries: BTreeMap<String, RegistryEntry>,
}

impl EntityRegistry {
    pub fn bind_supplied(
        &mut self,
        entity_type: impl Into<String>,
        id: impl Into<String>,
        evidence_id: impl Into<String>,
    ) -> Result<(), UnitDerivationError> {
        let entity_type = entity_type.into();
        let id = id.into();
        self.insert_unique(RegistryEntry {
            entity_type,
            id,
            provenance: Provenance::Supplied {
                evidence_id: evidence_id.into(),
            },
        })
    }

    pub fn insert_derived(&mut self, unit: &DerivedUnit) -> Result<(), UnitDerivationError> {
        self.insert_unique(RegistryEntry {
            entity_type: unit.entity_type.clone(),
            id: unit.id.clone(),
            provenance: unit.provenance.clone(),
        })
    }

    pub fn get(&self, id: &str) -> Option<&RegistryEntry> {
        self.entries.get(id)
    }

    fn insert_unique(&mut self, entry: RegistryEntry) -> Result<(), UnitDerivationError> {
        match self.entries.entry(entry.id.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(existing) => {
                if existing.get().entity_type == entry.entity_type
                    && matches!(existing.get().provenance, Provenance::Supplied { .. })
                    && matches!(entry.provenance, Provenance::Supplied { .. })
                {
                    return Ok(());
                }
                Err(UnitDerivationError::EntityCollision {
                    id: entry.id,
                    existing_type: existing.get().entity_type.clone(),
                    incoming_type: entry.entity_type,
                })
            }
        }
    }
}

/// The barrier-owned phase-2 dataset and its Knowledge-aware relation index.
/// The typed registry is retained with the materialization so callers cannot
/// accidentally separate the checked entity universe from the indexed data.
#[derive(Clone, Debug)]
pub struct PhaseTwoDataSet {
    dataset: crate::model::DataSet,
    input_knowledge: Vec<InputKnowledgeRecord>,
    relation_knowledge: Vec<RelationKnowledgeRecord>,
    registry: EntityRegistry,
}

/// Feature-gated phase-2 evaluator pairing the unchanged v2 engine with the
/// barrier's unresolved relation candidates. Complete reductions are
/// Knowledge-valued; determined execution delegates to the ordinary engine.
pub struct PhaseTwoEngine<'a> {
    ordinary: crate::engine::Engine<'a>,
    program: &'a crate::model::Program,
    relation_knowledge: &'a [RelationKnowledgeRecord],
}

#[derive(Default)]
struct ReductionReasonTraversal {
    derived: BTreeSet<String>,
    relations: BTreeSet<String>,
}

impl<'a> PhaseTwoEngine<'a> {
    pub fn evaluate_scalar(
        &mut self,
        derived_name: &str,
        entity_id: &str,
        period: &crate::model::Period,
    ) -> Result<CompleteReduction<crate::model::ScalarValue>, crate::engine::EvalError> {
        let mut reasons = BTreeSet::new();
        self.collect_derived_reduction_reasons(
            derived_name,
            entity_id,
            period,
            &mut ReductionReasonTraversal::default(),
            &mut reasons,
        );
        if !reasons.is_empty() {
            return Ok(CompleteReduction::Indeterminate { reasons });
        }
        self.ordinary
            .evaluate_scalar(derived_name, entity_id, period)
            .map(CompleteReduction::Determined)
    }

    fn collect_derived_reduction_reasons(
        &self,
        name: &str,
        entity_id: &str,
        period: &crate::model::Period,
        visiting: &mut ReductionReasonTraversal,
        reasons: &mut BTreeSet<String>,
    ) {
        if !visiting.derived.insert(name.to_string()) {
            return;
        }
        if let Some(derived) = self.program.derived.get(name)
            && let Some(semantics) = derived.semantics_at(period)
        {
            match semantics {
                crate::model::DerivedSemantics::Scalar(expr) => self
                    .collect_scalar_reduction_reasons(expr, entity_id, period, visiting, reasons),
                crate::model::DerivedSemantics::Judgment(expr) => self
                    .collect_judgment_reduction_reasons(expr, entity_id, period, visiting, reasons),
            }
        }
        visiting.derived.remove(name);
    }

    fn collect_scalar_reduction_reasons(
        &self,
        expr: &crate::model::ScalarExpr,
        entity_id: &str,
        period: &crate::model::Period,
        visiting: &mut ReductionReasonTraversal,
        reasons: &mut BTreeSet<String>,
    ) {
        use crate::model::ScalarExpr;
        match expr {
            ScalarExpr::Derived(name) => {
                self.collect_derived_reduction_reasons(name, entity_id, period, visiting, reasons)
            }
            ScalarExpr::ParameterLookup { index, .. }
            | ScalarExpr::Ceil(index)
            | ScalarExpr::Floor(index) => {
                self.collect_scalar_reduction_reasons(index, entity_id, period, visiting, reasons)
            }
            ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
                for item in items {
                    self.collect_scalar_reduction_reasons(
                        item, entity_id, period, visiting, reasons,
                    );
                }
            }
            ScalarExpr::Sub(left, right)
            | ScalarExpr::Mul(left, right)
            | ScalarExpr::Div(left, right)
            | ScalarExpr::DaysBetween {
                from: left,
                to: right,
            }
            | ScalarExpr::DateAddDays {
                date: left,
                days: right,
            } => {
                self.collect_scalar_reduction_reasons(left, entity_id, period, visiting, reasons);
                self.collect_scalar_reduction_reasons(right, entity_id, period, visiting, reasons);
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                where_clause,
                ..
            }
            | ScalarExpr::SumRelated {
                relation,
                current_slot,
                where_clause,
                ..
            } => {
                self.collect_relation_reasons(
                    relation,
                    *current_slot,
                    entity_id,
                    period,
                    visiting,
                    reasons,
                );
                if let Some(predicate) = where_clause {
                    self.collect_judgment_reduction_reasons(
                        predicate, entity_id, period, visiting, reasons,
                    );
                }
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.collect_judgment_reduction_reasons(
                    condition, entity_id, period, visiting, reasons,
                );
                self.collect_scalar_reduction_reasons(
                    then_expr, entity_id, period, visiting, reasons,
                );
                self.collect_scalar_reduction_reasons(
                    else_expr, entity_id, period, visiting, reasons,
                );
            }
            ScalarExpr::OverPeriods { value, n, .. } => {
                self.collect_scalar_reduction_reasons(value, entity_id, period, visiting, reasons);
                if let Some(n) = n {
                    self.collect_scalar_reduction_reasons(n, entity_id, period, visiting, reasons);
                }
            }
            ScalarExpr::Literal(_)
            | ScalarExpr::Input(_)
            | ScalarExpr::InputOrElse { .. }
            | ScalarExpr::PeriodStart
            | ScalarExpr::PeriodEnd => {}
        }
    }

    fn collect_judgment_reduction_reasons(
        &self,
        expr: &crate::model::JudgmentExpr,
        entity_id: &str,
        period: &crate::model::Period,
        visiting: &mut ReductionReasonTraversal,
        reasons: &mut BTreeSet<String>,
    ) {
        use crate::model::JudgmentExpr;
        match expr {
            JudgmentExpr::Comparison { left, right, .. } => {
                self.collect_scalar_reduction_reasons(left, entity_id, period, visiting, reasons);
                self.collect_scalar_reduction_reasons(right, entity_id, period, visiting, reasons);
            }
            JudgmentExpr::Derived(name) => {
                self.collect_derived_reduction_reasons(name, entity_id, period, visiting, reasons)
            }
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.collect_judgment_reduction_reasons(
                        item, entity_id, period, visiting, reasons,
                    );
                }
            }
            JudgmentExpr::Not(item) => {
                self.collect_judgment_reduction_reasons(item, entity_id, period, visiting, reasons)
            }
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                ..
            } => self.collect_relation_reasons(
                relation,
                *current_slot,
                entity_id,
                period,
                visiting,
                reasons,
            ),
        }
    }

    fn collect_relation_reasons(
        &self,
        relation: &str,
        current_slot: usize,
        entity_id: &str,
        period: &crate::model::Period,
        visiting: &mut ReductionReasonTraversal,
        reasons: &mut BTreeSet<String>,
    ) {
        if !visiting.relations.insert(relation.to_string()) {
            return;
        }
        for record in self.relation_knowledge.iter().filter(|record| {
            record.name == relation
                && record.interval.contains_period(period)
                && record
                    .tuple
                    .get(current_slot)
                    .is_some_and(|id| id == entity_id)
        }) {
            match &record.truth {
                LiftedTruth::Unknown {
                    reasons: unresolved,
                }
                | LiftedTruth::Conflict {
                    reasons: unresolved,
                } => reasons.extend(unresolved.iter().cloned()),
                LiftedTruth::Holds | LiftedTruth::DoesNotHold => {}
            }
        }
        if let Some(derivation) = self
            .program
            .relations
            .get(relation)
            .and_then(|schema| schema.derivation.as_ref())
        {
            self.collect_relation_reasons(
                &derivation.source_relation,
                derivation.current_slot,
                entity_id,
                period,
                visiting,
                reasons,
            );
            self.collect_judgment_reduction_reasons(
                &derivation.predicate,
                entity_id,
                period,
                visiting,
                reasons,
            );
        }
        visiting.relations.remove(relation);
    }
}

impl PhaseTwoDataSet {
    pub fn dataset(&self) -> &crate::model::DataSet {
        &self.dataset
    }

    pub fn relation_knowledge(&self) -> &[RelationKnowledgeRecord] {
        &self.relation_knowledge
    }

    pub fn input_complete(
        &self,
        name: &str,
        entity_id: &str,
        period: &crate::model::Period,
    ) -> CompleteReduction<crate::model::ScalarValue> {
        if let Some(record) = self.input_knowledge.iter().find(|record| {
            record.name == name
                && record.entity_id == entity_id
                && record.interval.contains_period(period)
        }) {
            return record.reduction.clone();
        }
        let mut values = self
            .dataset
            .inputs
            .iter()
            .filter(|record| {
                record.name == name
                    && record.entity_id == entity_id
                    && record.interval.contains_period(period)
            })
            .map(|record| record.value.clone());
        let Some(first) = values.next() else {
            return CompleteReduction::Indeterminate {
                reasons: BTreeSet::from([format!("input:{entity_id}:{name}:missing")]),
            };
        };
        if values.any(|value| value != first) {
            return CompleteReduction::Indeterminate {
                reasons: BTreeSet::from([format!("input:{entity_id}:{name}:conflict")]),
            };
        }
        CompleteReduction::Determined(first)
    }

    pub fn registry(&self) -> &EntityRegistry {
        &self.registry
    }

    pub fn engine<'a>(&'a self, program: &'a crate::model::Program) -> PhaseTwoEngine<'a> {
        PhaseTwoEngine {
            ordinary: crate::engine::Engine::new(program, &self.dataset),
            program,
            relation_knowledge: &self.relation_knowledge,
        }
    }
}

/// Immutable-by-construction expected-behavior ledger. There are no mutation
/// methods, so a Prototype must receive its ledger before any result exists.
#[derive(Clone, Debug)]
pub struct FrozenLedger {
    entries: BTreeMap<(String, Projection), LedgerEntry>,
}

impl FrozenLedger {
    pub fn new(entries: Vec<LedgerEntry>) -> Result<Self, UnitDerivationError> {
        let mut frozen = BTreeMap::new();
        for entry in entries {
            let key = (entry.fixture.clone(), entry.projection);
            match frozen.entry(key) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(entry);
                }
                std::collections::btree_map::Entry::Occupied(existing) => {
                    return Err(UnitDerivationError::DuplicateNamespace {
                        namespace: "frozen comparison ledger",
                        id: format!("{}:{:?}", existing.key().0, existing.key().1),
                    });
                }
            }
        }
        Ok(Self { entries: frozen })
    }

    fn get(&self, fixture: &str, projection: Projection) -> Option<&LedgerEntry> {
        self.entries.get(&(fixture.to_string(), projection))
    }
}

/// Supplied membership held outside all evaluable indexes. Its records are
/// private and can only be consumed by the partition-normalized comparator.
#[derive(Clone, Debug)]
pub struct ShadowChannel {
    fixture: String,
    records: Vec<SuppliedMembershipTuple>,
}

#[derive(Clone, Debug)]
pub struct Prototype {
    compiled: CompiledConstitution,
    config: UnitDerivationConfig,
    ledger: FrozenLedger,
}

impl Prototype {
    pub fn new(
        compiled: CompiledConstitution,
        config: UnitDerivationConfig,
        ledger: FrozenLedger,
    ) -> Self {
        Self {
            compiled,
            config,
            ledger,
        }
    }

    /// Checked comparison binder. It accepts only the two compile-resolved
    /// canonical relation ids and stores them in the non-evaluable channel.
    pub fn bind_shadow(
        &self,
        fixture: impl Into<String>,
        mut records: Vec<SuppliedMembershipTuple>,
    ) -> Result<ShadowChannel, UnitDerivationError> {
        for record in &records {
            let expected = match record.projection {
                Projection::UnitConstituent => &self.compiled.plan.relations.unit_constituent,
                Projection::ParticipatingMember => {
                    &self.compiled.plan.relations.participating_member
                }
            };
            if &record.relation != expected {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "shadow tuple relation `{}` is not the compile-resolved id `{expected}` for {:?}",
                    record.relation, record.projection
                )));
            }
        }
        records.sort_by(|left, right| {
            (
                left.projection,
                left.unit.as_str(),
                left.person.as_str(),
                left.relation.as_str(),
            )
                .cmp(&(
                    right.projection,
                    right.unit.as_str(),
                    right.person.as_str(),
                    right.relation.as_str(),
                ))
        });
        records.dedup();
        Ok(ShadowChannel {
            fixture: fixture.into(),
            records,
        })
    }

    pub fn run(
        &self,
        input: &ConstitutionInput,
        directly_supplied_membership: &[SuppliedMembershipTuple],
        shadow: Option<&ShadowChannel>,
    ) -> Result<PrototypeRun, UnitDerivationError> {
        if let Some(record) = directly_supplied_membership.iter().find(|record| {
            record.relation == self.compiled.plan.relations.unit_constituent
                || record.relation == self.compiled.plan.relations.participating_member
        }) {
            return Err(UnitDerivationError::SuppliedAndDerivedMembership(
                record.relation.clone(),
            ));
        }
        let derivation = derive_units(&self.compiled, input, &self.config)?;
        let comparisons = shadow.map_or(Ok(Vec::new()), |shadow| {
            self.compare_shadow(&derivation, shadow)
        })?;
        Ok(PrototypeRun {
            derivation,
            comparisons,
        })
    }

    fn compare_shadow(
        &self,
        derivation: &UnitDerivationResult,
        shadow: &ShadowChannel,
    ) -> Result<Vec<ComparisonResult>, UnitDerivationError> {
        let mut comparisons = Vec::new();
        for projection in [Projection::UnitConstituent, Projection::ParticipatingMember] {
            let ledger = self
                .ledger
                .get(&shadow.fixture, projection)
                .ok_or_else(|| UnitDerivationError::MissingLedgerEntry {
                    fixture: shadow.fixture.clone(),
                    projection,
                })?;
            let observed = if matches!(ledger.expectation, LedgerExpectation::OutOfPilot { .. }) {
                ObservedComparison::OutOfPilot
            } else if derivation
                .trace
                .input_conflict_impacts
                .iter()
                .any(|impact| impact.projections.contains(&projection))
                || derivation
                    .indeterminate
                    .iter()
                    .any(|item| matches!(item.kind, IndeterminateKind::ConstitutionOverlapConflict))
            {
                ObservedComparison::Conflict
            } else if projection_indeterminate(derivation, projection) {
                ObservedComparison::Indeterminate
            } else {
                let derived = normalize_derived_partition(derivation, projection);
                let supplied = normalize_supplied_partition(shadow, projection);
                if derived == supplied {
                    ObservedComparison::Equal
                } else {
                    ObservedComparison::Different
                }
            };
            let conforms_to_ledger = matches!(
                (&observed, &ledger.expectation),
                (ObservedComparison::Equal, LedgerExpectation::Equal)
                    | (ObservedComparison::Different, LedgerExpectation::Different)
                    | (
                        ObservedComparison::Indeterminate,
                        LedgerExpectation::Indeterminate
                    )
                    | (ObservedComparison::Conflict, LedgerExpectation::Conflict)
                    | (
                        ObservedComparison::OutOfPilot,
                        LedgerExpectation::OutOfPilot { .. }
                    )
            );
            comparisons.push(ComparisonResult {
                fixture: shadow.fixture.clone(),
                projection,
                observed,
                expected: ledger.expectation.clone(),
                conforms_to_ledger,
            });
        }
        Ok(comparisons)
    }
}

fn projection_indeterminate(result: &UnitDerivationResult, projection: Projection) -> bool {
    result
        .indeterminate
        .iter()
        .any(|item| match (&item.kind, projection) {
            (IndeterminateKind::Role(affected), projection) => *affected == projection,
            (IndeterminateKind::Membership, _)
            | (IndeterminateKind::DiscretionNotEncoded, _)
            | (IndeterminateKind::RosterNotAssertedComplete, _)
            | (IndeterminateKind::InconsistentInputs, _) => true,
            (IndeterminateKind::ConstitutionOverlapConflict, _) => false,
        })
}

fn normalize_derived_partition(
    result: &UnitDerivationResult,
    projection: Projection,
) -> Vec<Vec<String>> {
    let mut by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    for tuple in &result.memberships {
        if tuple.projection == projection && tuple.role == ProjectionRole::Member {
            by_unit
                .entry(tuple.unit.clone())
                .or_default()
                .insert(tuple.person.clone());
        }
    }
    let mut blocks = by_unit
        .into_values()
        .filter(|block| !block.is_empty())
        .map(|block| block.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    blocks.sort();
    blocks
}

fn normalize_supplied_partition(
    shadow: &ShadowChannel,
    projection: Projection,
) -> Vec<Vec<String>> {
    let mut by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    for tuple in &shadow.records {
        if tuple.projection == projection {
            by_unit
                .entry(tuple.unit.clone())
                .or_default()
                .insert(tuple.person.clone());
        }
    }
    let mut blocks = by_unit
        .into_values()
        .filter(|block| !block.is_empty())
        .map(|block| block.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    blocks.sort();
    blocks
}

/// Knowledge-lifted `derived_relation` filter. Unknown and Conflict candidates
/// are retained as unresolved rather than silently dropped.
pub fn lift_derived_relation(mut candidates: Vec<LiftedCandidate>) -> LiftedRelation {
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    let mut members = Vec::new();
    let mut unresolved = BTreeMap::new();
    for candidate in candidates {
        match candidate.predicate {
            LiftedTruth::Holds => members.push(candidate.id),
            LiftedTruth::DoesNotHold => {}
            predicate @ (LiftedTruth::Unknown { .. } | LiftedTruth::Conflict { .. }) => {
                unresolved.insert(candidate.id, predicate);
            }
        }
    }
    members.sort();
    members.dedup();
    LiftedRelation {
        members,
        unresolved,
    }
}

/// Canonical result-only serialization for the named experimental channel.
/// This is deliberately not `CompiledProgramArtifact` v2 and cannot be given a
/// release-line format identity by the caller.
pub fn serialize_experimental_run(run: &PrototypeRun) -> Result<Vec<u8>, serde_json::Error> {
    let units = run
        .derivation
        .units
        .iter()
        .map(|unit| {
            serde_json::json!({
                "entity_type": unit.entity_type,
                "id": unit.id,
                "members": unit.members,
                "provenance": provenance_json(&unit.provenance),
            })
        })
        .collect::<Vec<_>>();
    let memberships = run
        .derivation
        .memberships
        .iter()
        .map(|tuple| {
            serde_json::json!({
                "relation": tuple.relation,
                "unit": tuple.unit,
                "person": tuple.person,
                "projection": projection_name(tuple.projection),
                "role": role_name(tuple.role),
                "citations": tuple.citations.iter().map(citation_json).collect::<Vec<_>>(),
                "provenance": provenance_json(&tuple.provenance),
            })
        })
        .collect::<Vec<_>>();
    let indeterminate = run
        .derivation
        .indeterminate
        .iter()
        .map(|item| {
            serde_json::json!({
                "person": item.person,
                "kind": format!("{:?}", item.kind),
                "unresolved_facts": item.unresolved_facts,
            })
        })
        .collect::<Vec<_>>();
    let comparisons = run
        .comparisons
        .iter()
        .map(|item| {
            serde_json::json!({
                "fixture": item.fixture,
                "projection": projection_name(item.projection),
                "observed": format!("{:?}", item.observed),
                "expected": format!("{:?}", item.expected),
                "conforms_to_ledger": item.conforms_to_ledger,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&serde_json::json!({
        "experimental_semantics_version": super::EXPERIMENTAL_SEMANTICS_VERSION,
        "derivation": {
            "units": units,
            "memberships": memberships,
            "indeterminate": indeterminate,
            "trace": {
                "root": run.derivation.trace.root,
                "worlds_evaluated": run.derivation.trace.worlds_evaluated,
                "events": run.derivation.trace.events.iter().map(|event| format!("{event:?}")).collect::<Vec<_>>(),
                "completeness_evidence": run.derivation.trace.completeness_evidence,
                "input_conflicts": run.derivation.trace.input_conflicts,
                "input_conflict_impacts": run.derivation.trace.input_conflict_impacts.iter().map(|impact| serde_json::json!({
                    "fact": impact.fact,
                    "persons": impact.persons,
                    "projections": impact.projections.iter().map(|projection| projection_name(*projection)).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }
        },
        "comparisons": comparisons,
    }))
}

/// Materialization barrier into the ordinary evaluator's relation dataset.
/// Only Determined derived tuples with `Member` role enter evaluable indexes;
/// excluded/barred role records stay in the audited prototype result, and the
/// private shadow channel is structurally unavailable here.
pub fn materialize_phase_two_dataset(
    base: &crate::model::DataSet,
    phase_two_program: &crate::model::Program,
    input: &ConstitutionInput,
    run: &PrototypeRun,
    interval: crate::model::Interval,
) -> Result<PhaseTwoDataSet, UnitDerivationError> {
    materialize_phase_two_dataset_with_knowledge(
        base,
        Vec::new(),
        phase_two_program,
        input,
        run,
        interval,
    )
}

/// Materialization barrier variant for callers that have explicit
/// Knowledge-valued scalar inputs. Only unresolved inputs belong in
/// `input_knowledge`; determined values must be present in `base.inputs`.
pub fn materialize_phase_two_dataset_with_knowledge(
    base: &crate::model::DataSet,
    mut input_knowledge: Vec<InputKnowledgeRecord>,
    phase_two_program: &crate::model::Program,
    input: &ConstitutionInput,
    run: &PrototypeRun,
    interval: crate::model::Interval,
) -> Result<PhaseTwoDataSet, UnitDerivationError> {
    let derived_relations = BTreeSet::from([
        run.derivation.relations.unit_constituent.as_str(),
        run.derivation.relations.participating_member.as_str(),
    ]);
    if let Some(conflict) = base
        .relations
        .iter()
        .find(|record| derived_relations.contains(record.name.as_str()))
    {
        return Err(UnitDerivationError::SuppliedAndDerivedMembership(
            conflict.name.clone(),
        ));
    }

    // Reconstruct the complete typed supplied registry before cloning or
    // extending relation storage. A collision therefore fails atomically at
    // the barrier, before any phase-2 index can be built.
    let mut registry = EntityRegistry::default();
    for supplied in &input.supplied_entities {
        registry.bind_supplied(
            supplied.entity_type.clone(),
            supplied.id.clone(),
            supplied.evidence.id.clone(),
        )?;
    }
    let roster_evidence = input
        .roster
        .completeness
        .as_ref()
        .map_or("roster-without-completeness", |evidence| {
            evidence.id.as_str()
        });
    for person in &input.roster.persons {
        let entity_type = input
            .supplied_entities
            .iter()
            .find(|entity| entity.id == *person)
            .map_or("person", |entity| entity.entity_type.as_str());
        registry.bind_supplied(entity_type, person.clone(), roster_evidence)?;
    }
    for unit in &run.derivation.units {
        registry.insert_derived(unit)?;
    }
    for record in &base.inputs {
        bind_materialized_entity(
            &mut registry,
            &record.entity,
            &record.entity_id,
            &format!("dataset-input:{}", record.name),
        )?;
    }
    input_knowledge.sort_by(|left, right| {
        (
            left.entity_id.as_str(),
            left.name.as_str(),
            &left.interval.start,
            &left.interval.end,
        )
            .cmp(&(
                right.entity_id.as_str(),
                right.name.as_str(),
                &right.interval.start,
                &right.interval.end,
            ))
    });
    for window in input_knowledge.windows(2) {
        if window[0].name == window[1].name
            && window[0].entity_id == window[1].entity_id
            && window[0].interval == window[1].interval
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "duplicate Knowledge input `{}` for `{}`",
                window[0].name, window[0].entity_id
            )));
        }
    }
    for record in &input_knowledge {
        if matches!(record.reduction, CompleteReduction::Determined(_)) {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "determined Knowledge input `{}` for `{}` must use the ordinary dataset",
                record.name, record.entity_id
            )));
        }
        bind_materialized_entity(
            &mut registry,
            &record.entity,
            &record.entity_id,
            &format!("knowledge-input:{}", record.name),
        )?;
    }
    for record in &base.relations {
        let schema = phase_two_program
            .relations
            .get(&record.name)
            .ok_or_else(|| {
                UnitDerivationError::InvalidPlan(format!(
                    "base relation `{}` has no typed phase-2 schema",
                    record.name
                ))
            })?;
        if record.tuple.len() != schema.arity || schema.slot_entities.len() != schema.arity {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "base relation `{}` cannot bind every tuple slot to a declared entity type",
                record.name
            )));
        }
        for (slot, id) in record.tuple.iter().enumerate() {
            registry.bind_supplied(
                schema.slot_entities[slot].clone(),
                id.clone(),
                format!("dataset-relation:{}:{slot}", record.name),
            )?;
        }
    }
    let mut materialized = base.clone();
    let records = run
        .derivation
        .memberships
        .iter()
        .filter(|tuple| tuple.role == ProjectionRole::Member)
        .map(|tuple| {
            (
                tuple.relation.clone(),
                vec![tuple.unit.clone(), tuple.person.clone()],
            )
        })
        .collect::<BTreeSet<_>>();
    materialized
        .relations
        .extend(
            records
                .into_iter()
                .map(|(name, tuple)| crate::model::RelationRecord {
                    name,
                    tuple,
                    interval: interval.clone(),
                }),
        );

    let constituent_unit_by_person = run
        .derivation
        .memberships
        .iter()
        .filter(|tuple| tuple.projection == Projection::UnitConstituent)
        .map(|tuple| (tuple.person.clone(), tuple.unit.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut relation_knowledge = run
        .derivation
        .indeterminate
        .iter()
        .filter_map(|item| {
            if item.kind != IndeterminateKind::Role(Projection::ParticipatingMember) {
                return None;
            }
            let unit = constituent_unit_by_person.get(&item.person)?;
            let conflict = item
                .unresolved_facts
                .iter()
                .any(|fact| run.derivation.trace.input_conflicts.contains(fact));
            let truth = if conflict {
                LiftedTruth::Conflict {
                    reasons: item.unresolved_facts.clone(),
                }
            } else {
                LiftedTruth::Unknown {
                    reasons: item.unresolved_facts.clone(),
                }
            };
            Some(RelationKnowledgeRecord {
                name: run.derivation.relations.participating_member.clone(),
                tuple: vec![unit.clone(), item.person.clone()],
                interval: interval.clone(),
                truth,
            })
        })
        .collect::<Vec<_>>();
    relation_knowledge.sort_by(|left, right| {
        (left.name.as_str(), left.tuple.as_slice())
            .cmp(&(right.name.as_str(), right.tuple.as_slice()))
    });
    relation_knowledge.dedup();

    Ok(PhaseTwoDataSet {
        dataset: materialized,
        input_knowledge,
        relation_knowledge,
        registry,
    })
}

fn bind_materialized_entity(
    registry: &mut EntityRegistry,
    entity_type: &str,
    id: &str,
    evidence_id: &str,
) -> Result<(), UnitDerivationError> {
    if let Some(existing) = registry.get(id) {
        if existing.entity_type != entity_type {
            return Err(UnitDerivationError::EntityCollision {
                id: id.to_string(),
                existing_type: existing.entity_type.clone(),
                incoming_type: entity_type.to_string(),
            });
        }
        return Ok(());
    }
    registry.bind_supplied(entity_type, id, evidence_id)
}

fn projection_name(projection: Projection) -> &'static str {
    match projection {
        Projection::UnitConstituent => "unit_constituent",
        Projection::ParticipatingMember => "participating_member",
    }
}

fn role_name(role: ProjectionRole) -> &'static str {
    match role {
        ProjectionRole::Member => "member",
        ProjectionRole::Excluded => "excluded",
        ProjectionRole::BarredIndependent => "barred_independent",
    }
}

fn citation_json(citation: &Citation) -> serde_json::Value {
    serde_json::json!({
        "provision": citation.provision,
        "authority": citation.authority,
    })
}

fn provenance_json(provenance: &Provenance) -> serde_json::Value {
    match provenance {
        Provenance::Supplied { evidence_id } => serde_json::json!({
            "kind": "supplied",
            "evidence_id": evidence_id,
        }),
        Provenance::Derived {
            constitution,
            trace_root,
        } => serde_json::json!({
            "kind": "derived",
            "constitution": constitution,
            "trace_root": trace_root,
        }),
    }
}
