//! Mandatory entity typing for relations that a program executes.
//!
//! Entity ids are untyped strings and relation aggregation looks tuples up by
//! `(relation, current_slot, id)`. A relation whose positions carry no entity
//! kinds therefore cannot be checked against dataset tuples: a tuple stored in
//! the other orientation silently aggregates nothing. This module is the
//! typing judgment every compile, artifact load, and direct request enforces:
//! each relation an executable node reads declares one entity kind per slot,
//! and the slot a node reads from holds the kind of the entity evaluating it.
use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    JudgmentExpr, Program, RelatedValueRef, RelationDerivation, RelationSchema, SCALAR_ENTITY,
    ScalarExpr, Term,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelationTypingCode {
    OnlyRelatedShape,
    /// An executed relation declares no slot entity kinds.
    UntypedRelation,
    /// A declared slot entity list does not have one kind per slot.
    SlotEntityCount,
    /// An aggregation reads a slot outside the relation's arity.
    SlotOutOfRange,
    /// The slot an aggregation keys its entity id on holds another kind.
    CurrentSlotEntityMismatch,
    /// A related rule is evaluated on ids of a different declared kind.
    RelatedSlotEntityMismatch,
    /// A membership test reads slots whose kinds contradict its context.
    MembershipSlotEntityMismatch,
    /// A derived relation declares slot kinds its source contradicts.
    DerivedRelationSourceConflict,
    /// An aggregation over a derived relation addresses slots other than the
    /// ones the derivation traverses, so execution modes disagree.
    DerivedRelationSlotsDiverge,
    /// A rule a derived relation's predicate reads runs on an id of another
    /// kind than its entity.
    PredicateEntityMismatch,
    /// Derived relations filtering to the same entity disagree on the kind
    /// of the ids that entity is queried with.
    FilteredEntityKindConflict,
    /// An executed node names a relation the program does not declare.
    UnknownRelation,
    /// A relation slot declares a filtered entity (a derived relation's
    /// `entity`) instead of the kind of the ids that entity is queried with.
    FilteredEntitySlotKind,
    /// A derived relation's predicate reads through a term (`anchor.x`), but
    /// the derivation declares no slot kind for that term.
    RelationRolesNeedSlotKinds,
    /// A rule read through a term has an entity other than the term's kind.
    RelationRoleEntityMismatch,
    /// A read through a term sits outside a derived relation's predicate, or
    /// inside an aggregation's `where` clause, where no term is bound.
    RelationRoleOutsideScope,
    RelationAtomNeedsTypedRelation,
    RelationAtomKindMismatch,
    RelationAtomArityMismatch,
    DerivedRelationAtomArityUnsupported,
    RelationNameShadowsBuiltin,
    RelationTermKindMismatch,
    RelationVariableOutOfScope,
    RelationExistsVariableCount,
    InverseDerivedTraversalUnsupported,
    RelationPathRequiresBinaryDeclaration,
    RelationPathStepArity,
    RelationPathStepSlots,
    RelationPathKindMismatch,
}

impl RelationTypingCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnlyRelatedShape => "only_related_shape",
            Self::UntypedRelation => "untyped_relation",
            Self::SlotEntityCount => "relation_slot_entity_count",
            Self::SlotOutOfRange => "relation_slot_out_of_range",
            Self::CurrentSlotEntityMismatch => "relation_current_slot_entity_mismatch",
            Self::RelatedSlotEntityMismatch => "relation_related_slot_entity_mismatch",
            Self::MembershipSlotEntityMismatch => "relation_membership_slot_entity_mismatch",
            Self::DerivedRelationSourceConflict => "derived_relation_source_slot_conflict",
            Self::DerivedRelationSlotsDiverge => "derived_relation_slots_diverge",
            Self::PredicateEntityMismatch => "relation_predicate_entity_mismatch",
            Self::FilteredEntityKindConflict => "filtered_entity_kind_conflict",
            Self::UnknownRelation => "unknown_relation",
            Self::FilteredEntitySlotKind => "relation_slot_kind_is_filtered_entity",
            Self::RelationRolesNeedSlotKinds => "relation_roles_need_slot_kinds",
            Self::RelationRoleEntityMismatch => "relation_role_entity_mismatch",
            Self::RelationRoleOutsideScope => "relation_role_outside_scope",
            Self::RelationAtomNeedsTypedRelation => "relation_atom_needs_typed_relation",
            Self::RelationAtomKindMismatch => "relation_atom_kind_mismatch",
            Self::RelationAtomArityMismatch => "relation_atom_arity_mismatch",
            Self::DerivedRelationAtomArityUnsupported => "derived_relation_atom_arity_unsupported",
            Self::RelationNameShadowsBuiltin => "relation_name_shadows_builtin",
            Self::RelationTermKindMismatch => "relation_term_kind_mismatch",
            Self::RelationVariableOutOfScope => "relation_variable_out_of_scope",
            Self::RelationExistsVariableCount => "relation_exists_variable_count",
            Self::InverseDerivedTraversalUnsupported => "inverse_derived_traversal_unsupported",
            Self::RelationPathRequiresBinaryDeclaration => {
                "relation_path_requires_binary_declaration"
            }
            Self::RelationPathStepArity => "relation_path_step_arity",
            Self::RelationPathStepSlots => "relation_path_step_slots",
            Self::RelationPathKindMismatch => "relation_path_kind_mismatch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelationTypingViolation {
    pub code: RelationTypingCode,
    pub relation: String,
    /// The derived rule or derived relation whose executable node is ill-typed.
    pub citing: String,
    pub message: String,
}

impl std::fmt::Display for RelationTypingViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "[{}] {}", self.code.as_str(), self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationTypingReport {
    pub violations: Vec<RelationTypingViolation>,
}

impl RelationTypingReport {
    /// Relations the report says are untyped, in name order.
    pub fn untyped_relations(&self) -> BTreeSet<&str> {
        self.violations
            .iter()
            .filter(|violation| violation.code == RelationTypingCode::UntypedRelation)
            .map(|violation| violation.relation.as_str())
            .collect()
    }
}

impl std::fmt::Display for RelationTypingReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, violation) in self.violations.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            write!(formatter, "{violation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RelationTypingReport {}

/// Check that every relation the program executes is entity-typed and read
/// from the slots its declared kinds say. Violations are sorted and unique,
/// so the same program always reports the same text.
pub fn check_program(program: &Program) -> Result<(), RelationTypingReport> {
    let mut checker = Checker::for_typing(program);
    checker.filtered_entity_conflicts();
    checker.walk();
    checker.relation_roles();
    if checker.violations.is_empty() {
        Ok(())
    } else {
        Err(RelationTypingReport {
            violations: checker.violations.into_iter().collect(),
        })
    }
}

/// Every relation a node explain executes reads: aggregated or tested
/// directly, or reached as a derived relation's source or through its
/// predicate. Like explain, it reads a versioned rule's versions only, so it
/// is the evidence an artifact's execution gives; `check_program` also types
/// the base semantics, which the dense compiler reads.
pub fn executed_relations(program: &Program) -> BTreeSet<String> {
    let mut checker = Checker::for_execution(program);
    checker.walk();
    checker.used_relations
}

/// The entity kind of each tuple slot of `relation`, or `None` when the
/// relation (or, for a single-source derivation, every relation it filters)
/// declares none. A single-source derivation inherits the source's kinds when
/// it declares none of its own. A path declares its endpoint kinds directly;
/// its intermediate relations never supply inherited kinds.
///
/// This follows one chain of sources. To ask about many relations of one
/// program, build [`EffectiveSlotEntities`] once: asking this per relation
/// follows a shared chain again from every relation on it, which is
/// quadratic in the chain length.
pub fn effective_slot_entities(program: &Program, relation: &str) -> Option<Vec<String>> {
    let mut visited = BTreeSet::new();
    let mut current = relation;
    loop {
        if !visited.insert(current) {
            return None;
        }
        match own_slot_entities(program.relations.get(current)?) {
            OwnSlotEntities::Settled(kinds) => return kinds.map(<[String]>::to_vec),
            OwnSlotEntities::Inherited(source) => current = source,
        }
    }
}

/// What a relation's own declaration says about its slot kinds.
enum OwnSlotEntities<'p> {
    /// The declaration settles the kinds: the ones it declares, or none.
    Settled(Option<&'p [String]>),
    /// A derived relation that declares nothing has its source's kinds.
    Inherited(&'p str),
}

fn own_slot_entities(schema: &RelationSchema) -> OwnSlotEntities<'_> {
    match &schema.derivation {
        Some(derivation) if !derivation.slot_entities.is_empty() => {
            OwnSlotEntities::Settled(Some(&derivation.slot_entities))
        }
        Some(derivation) if schema.slot_entities.is_empty() => {
            // A path joins new pairs rather than filtering one source's
            // tuples. Its kinds must be declared, never inherited from a step.
            match derivation.source_relation.as_deref() {
                Some(source) if derivation.source_path.is_none() => {
                    OwnSlotEntities::Inherited(source)
                }
                _ => OwnSlotEntities::Settled(None),
            }
        }
        _ => OwnSlotEntities::Settled(
            (!schema.slot_entities.is_empty()).then_some(schema.slot_entities.as_slice()),
        ),
    }
}

/// [`effective_slot_entities`] of every relation of one program, resolved
/// together. A relation's chain of sources is followed only until it meets a
/// relation already resolved, so each relation is visited once however many
/// chains share it, and nothing recurses: building this is linear in the
/// number of relations (times a map lookup), not in the sum of chain lengths.
pub struct EffectiveSlotEntities<'p> {
    kinds: BTreeMap<&'p str, Option<&'p [String]>>,
}

impl<'p> EffectiveSlotEntities<'p> {
    pub fn new(program: &'p Program) -> Self {
        let kinds =
            follow_chains(
                program.relations.keys().map(String::as_str),
                None,
                |name| match program.relations.get(name).map(own_slot_entities) {
                    None => ChainStep::End(None),
                    Some(OwnSlotEntities::Settled(declared)) => ChainStep::End(declared),
                    Some(OwnSlotEntities::Inherited(source)) => ChainStep::Next(source),
                },
            );
        Self { kinds }
    }

    /// [`effective_slot_entities`] of `relation`, borrowed from the program.
    pub fn get(&self, relation: &str) -> Option<&'p [String]> {
        self.kinds.get(relation).copied().flatten()
    }
}

/// The slots an aggregation over `relation` traverses: a derived relation's
/// own slots, whichever slots the node names, else the node's.
pub(crate) fn traversed_slots(
    program: &Program,
    relation: &str,
    current_slot: usize,
    related_slot: usize,
) -> (usize, usize) {
    match program
        .relations
        .get(relation)
        .and_then(|schema| schema.derivation.as_ref())
    {
        Some(derivation) => (derivation.current_slot, derivation.related_slot),
        None => (current_slot, related_slot),
    }
}

/// The kinds of the current and related ids a derived relation's predicate
/// is evaluated with: its effective slot kinds at the derivation's slots.
/// `slot_kinds` is shared across calls, so a chain of n derived relations
/// costs n steps rather than one walk per link.
pub(crate) fn predicate_slot_kinds(
    slot_kinds: &EffectiveSlotEntities<'_>,
    name: &str,
    derivation: &RelationDerivation,
) -> (Option<String>, Option<String>) {
    let kinds = slot_kinds.get(name);
    let slot = |slot: usize| kinds.and_then(|kinds| kinds.get(slot)).cloned();
    (slot(derivation.current_slot), slot(derivation.related_slot))
}

/// Whether `derivation`'s predicate evaluates a rule of `entity` on the
/// current id: its entity is the derivation's declared current kind
/// (`RelationEvalContext::entity_id_for` in the explain evaluator). Every
/// other rule the predicate reads runs on the related id.
pub(crate) fn predicate_routes_to_current(derivation: &RelationDerivation, entity: &str) -> bool {
    derivation
        .slot_entities
        .get(derivation.current_slot)
        .is_some_and(|kind| kind == entity)
}

/// The known current-slot kinds of every derived relation filtering to each
/// entity (a derived relation's `entity`, e.g. `SnapUnit`), and the entities
/// whose derived relations' current kinds are all unknown.
fn filtered_entity_current_kinds(
    program: &Program,
    slot_kinds: &EffectiveSlotEntities<'_>,
    among: Option<&BTreeSet<String>>,
) -> (BTreeMap<String, BTreeSet<String>>, BTreeSet<String>) {
    let mut known = BTreeMap::<String, BTreeSet<String>>::new();
    let mut seen = BTreeSet::new();
    for (name, schema) in &program.relations {
        if among.is_some_and(|among| !among.contains(name)) {
            continue;
        }
        let Some(derivation) = &schema.derivation else {
            continue;
        };
        let Some(entity) = derivation.entity.as_ref() else {
            continue;
        };
        seen.insert(entity.clone());
        if let Some(kind) = slot_kinds
            .get(name)
            .and_then(|kinds| kinds.get(derivation.current_slot).cloned())
        {
            known.entry(entity.clone()).or_default().insert(kind);
        }
    }
    let unresolved = seen
        .into_iter()
        .filter(|entity| !known.contains_key(entity))
        .collect();
    (known, unresolved)
}

/// Filtered entities (a derived relation's `entity`, e.g. `SnapUnit`) are
/// queried with the source relation's current-slot ids. Map each to the kind
/// those ids have, so dataset evidence about a `SnapUnit` id counts as
/// evidence about a `Household` id. A filter of a filter resolves to the
/// innermost kind; an entity whose derived relations disagree, or whose
/// chain cycles, is left unmapped.
pub fn filtered_entity_kinds(program: &Program) -> BTreeMap<String, String> {
    filtered_entity_kinds_with(program, &EffectiveSlotEntities::new(program))
}

/// [`filtered_entity_kinds`], reading slot kinds from a table already built
/// for `program`.
pub(crate) fn filtered_entity_kinds_with(
    program: &Program,
    slot_kinds: &EffectiveSlotEntities<'_>,
) -> BTreeMap<String, String> {
    resolve_filtered_kinds(filtered_entity_current_kinds(program, slot_kinds, None).0)
}

/// [`filtered_entity_kinds`] from only the derived relations in `among`.
pub fn filtered_entity_kinds_among(
    program: &Program,
    among: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let slot_kinds = EffectiveSlotEntities::new(program);
    resolve_filtered_kinds(filtered_entity_current_kinds(program, &slot_kinds, Some(among)).0)
}

fn resolve_filtered_kinds(known: BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, String> {
    let direct = known
        .into_iter()
        .filter(|(_, kinds)| kinds.len() == 1)
        .filter_map(|(entity, kinds)| kinds.into_iter().next().map(|kind| (entity, Some(kind))))
        .collect::<BTreeMap<_, _>>();
    innermost_kinds(&direct)
}

/// Follow each key of `direct` (an entity to the kind of the ids it is
/// queried with, `None` when that kind is unsettled) through the entities
/// that kind is itself a filter of, to the innermost kind: one that is no
/// filter, or a filter to itself. A key reaching an unsettled kind or a
/// cycle maps to nothing.
///
/// Each entity's chain is followed until it meets an entity already
/// resolved, so the work is linear in the number of entities however long
/// the chains of filters are.
pub(crate) fn innermost_kinds(
    direct: &BTreeMap<String, Option<String>>,
) -> BTreeMap<String, String> {
    let innermost = follow_chains(direct.keys().map(String::as_str), None, |kind| match direct
        .get(kind)
    {
        None => ChainStep::End(Some(kind)),
        Some(None) => ChainStep::End(None),
        Some(Some(next)) if next == kind => ChainStep::End(Some(kind)),
        Some(Some(next)) => ChainStep::Next(next.as_str()),
    });
    direct
        .keys()
        .filter_map(|entity| {
            innermost[entity.as_str()].map(|kind| (entity.clone(), kind.to_string()))
        })
        .collect()
}

/// One step along a chain in which every node has at most one successor:
/// a relation's source, or the kind a filtered entity is queried with.
pub(crate) enum ChainStep<'k, R> {
    /// The chain ends at this node, with this result.
    End(R),
    /// This node's result is the next node's.
    Next(&'k str),
}

/// The result of following every node of `starts` along its chain to the
/// end, where `step` says how a node ends its chain or which node it defers
/// to. A chain that returns to a node it passed ends with `on_cycle`.
///
/// Every node a walk passes takes the result the walk ends with, and a later
/// walk stops at the first node already resolved. So a node's result depends
/// only on the chain from that node, exactly as walking separately from each
/// start finds it, while `step` runs once per node and nothing recurses: the
/// work is linear in the number of nodes (times a map lookup) where separate
/// walks are quadratic in the length of a shared chain. The map holds every
/// node a walk reached, starts included.
pub(crate) fn follow_chains<'k, R: Copy>(
    starts: impl IntoIterator<Item = &'k str>,
    on_cycle: R,
    mut step: impl FnMut(&'k str) -> ChainStep<'k, R>,
) -> BTreeMap<&'k str, R> {
    let mut resolved = BTreeMap::<&'k str, R>::new();
    let mut path = Vec::<&'k str>::new();
    let mut on_path = BTreeSet::<&'k str>::new();
    for start in starts {
        let mut node = start;
        let result = loop {
            if let Some(&known) = resolved.get(node) {
                break known;
            }
            if !on_path.insert(node) {
                break on_cycle;
            }
            path.push(node);
            match step(node) {
                ChainStep::End(result) => break result,
                ChainStep::Next(next) => node = next,
            }
        };
        for node in path.drain(..) {
            resolved.insert(node, result);
        }
        on_path.clear();
    }
    resolved
}

#[cfg(test)]
thread_local! {
    // Leaves, branching nodes, nodes with three or more edges, and back
    // edges, counted only after the graph resolver performs the work.
    static SOURCE_GRAPH_WORK: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Whether every reachable relation meets a local condition and the source
/// graph is acyclic. Unlike `follow_chains`, this visits every step of a path.
/// A node and each of its source edges are examined once, with an explicit
/// stack so shared tails and long chains do not repeat work or grow the stack.
/// `None` means the local condition fails; an empty successor list is a leaf.
pub(crate) fn all_source_edges<'k>(
    starts: impl IntoIterator<Item = &'k str>,
    mut successors: impl FnMut(&'k str) -> Option<Vec<&'k str>>,
) -> BTreeMap<&'k str, bool> {
    struct Frame<'k> {
        name: &'k str,
        next: usize,
        edges: Vec<&'k str>,
        valid: bool,
    }
    let mut resolved = BTreeMap::new();
    let mut on_path = BTreeSet::new();
    let mut stack = Vec::<Frame<'k>>::new();
    for start in starts {
        if resolved.contains_key(start) {
            continue;
        }
        let edges = successors(start);
        on_path.insert(start);
        stack.push(Frame {
            name: start,
            next: 0,
            valid: edges.is_some(),
            edges: edges.unwrap_or_default(),
        });
        while let Some(frame) = stack.last_mut() {
            if let Some(&next) = frame.edges.get(frame.next) {
                frame.next += 1;
                if let Some(&known) = resolved.get(next) {
                    frame.valid &= known;
                } else if on_path.contains(next) {
                    #[cfg(test)]
                    SOURCE_GRAPH_WORK.with(|work| {
                        let mut counts = work.get();
                        counts[3] += 1;
                        work.set(counts);
                    });
                    frame.valid = false;
                } else {
                    let edges = successors(next);
                    on_path.insert(next);
                    stack.push(Frame {
                        name: next,
                        next: 0,
                        valid: edges.is_some(),
                        edges: edges.unwrap_or_default(),
                    });
                }
            } else {
                let frame = stack.pop().expect("a source frame");
                #[cfg(test)]
                SOURCE_GRAPH_WORK.with(|work| {
                    let mut counts = work.get();
                    counts[0] += usize::from(frame.next == 0);
                    counts[1] += usize::from(frame.next > 1);
                    counts[2] += usize::from(frame.next >= 3);
                    work.set(counts);
                });
                on_path.remove(frame.name);
                resolved.insert(frame.name, frame.valid);
                if let Some(parent) = stack.last_mut() {
                    parent.valid &= frame.valid;
                }
            }
        }
    }
    resolved
}

/// Filtered entities that alias ids of another kind: an entity some derived
/// relation filters to over a source whose current kind is known and is not
/// that entity. Such a name cannot be a relation slot's kind. An entity whose
/// derived relations' current kinds are all unknown is not called an alias,
/// and one whose only known current kind is itself (a filter of households
/// to households) is a physical kind.
pub fn filtered_entity_names(program: &Program) -> BTreeSet<String> {
    let slot_kinds = EffectiveSlotEntities::new(program);
    alias_names(filtered_entity_current_kinds(program, &slot_kinds, None).0)
}

/// [`filtered_entity_names`] from only the derived relations in `among`.
pub fn filtered_entity_names_among(
    program: &Program,
    among: &BTreeSet<String>,
) -> BTreeSet<String> {
    let slot_kinds = EffectiveSlotEntities::new(program);
    alias_names(filtered_entity_current_kinds(program, &slot_kinds, Some(among)).0)
}

fn alias_names(known: BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    known
        .into_iter()
        .filter(|(entity, kinds)| kinds.iter().any(|kind| kind != entity))
        .map(|(entity, _)| entity)
        .collect()
}

/// Filtered entities none of whose derived relations has a known current
/// kind yet: a use naming one says nothing about the ids it reads.
pub fn unresolved_filtered_entities(program: &Program) -> BTreeSet<String> {
    let slot_kinds = EffectiveSlotEntities::new(program);
    filtered_entity_current_kinds(program, &slot_kinds, None).1
}

/// Every entity some derived relation filters to.
pub fn filtered_entities(program: &Program) -> BTreeSet<String> {
    program
        .relations
        .values()
        .filter_map(|schema| schema.derivation.as_ref()?.entity.clone())
        .collect()
}

#[derive(Clone, Copy)]
struct Context<'a> {
    /// The kind of the id the expression is evaluated on, when known.
    entity: Option<&'a str>,
    /// Inside a derived-relation predicate: the kinds of the current and
    /// related ids a membership test compares.
    membership: Option<(Option<&'a str>, Option<&'a str>)>,
    citing: &'a str,
}

struct Checker<'a> {
    program: &'a Program,
    /// Each relation's effective slot kinds, resolved once for the program.
    slot_kinds: EffectiveSlotEntities<'a>,
    /// Filtered entity -> the kind of the source ids it is queried with.
    filtered: BTreeMap<String, String>,
    /// Filtered entities that alias another kind; never a slot kind.
    filtered_names: BTreeSet<String>,
    /// Module-qualified relations by short name (`<module>#relation.<short>`),
    /// built the first time an untyped relation needs its namesakes named.
    namesakes: Option<BTreeMap<&'a str, Vec<&'a str>>>,
    /// Whether a versioned rule's base semantics is walked too.
    base_semantics: bool,
    violations: BTreeSet<RelationTypingViolation>,
    used_relations: BTreeSet<String>,
    /// Used relations whose derivation, if any, is not checked yet.
    unchecked: Vec<String>,
}

impl<'a> Checker<'a> {
    /// A checker for [`check_program`]: it maps filtered entities to the
    /// kinds of their ids and walks a versioned rule's base semantics too.
    fn for_typing(program: &'a Program) -> Self {
        let slot_kinds = EffectiveSlotEntities::new(program);
        let current_kinds = filtered_entity_current_kinds(program, &slot_kinds, None).0;
        Self {
            filtered: resolve_filtered_kinds(current_kinds.clone()),
            filtered_names: alias_names(current_kinds),
            base_semantics: true,
            ..Self::for_execution(program)
        }
    }

    /// A checker for [`executed_relations`]: what explain executes only.
    fn for_execution(program: &'a Program) -> Self {
        Self {
            program,
            slot_kinds: EffectiveSlotEntities::new(program),
            filtered: BTreeMap::new(),
            filtered_names: BTreeSet::new(),
            namesakes: None,
            base_semantics: false,
            violations: BTreeSet::new(),
            used_relations: BTreeSet::new(),
            unchecked: Vec::new(),
        }
    }

    /// Record that an executable node reads `relation`, queuing it for
    /// [`Self::derived_relations`] the first time.
    fn use_relation(&mut self, relation: &str) {
        if self.used_relations.insert(relation.to_string()) {
            self.unchecked.push(relation.to_string());
        }
    }

    /// The module-qualified relations a short relation name is not: every
    /// `<module>#relation.<relation>`, as code formatted in name order.
    fn namesakes(&mut self, relation: &str) -> Vec<String> {
        if relation.contains('#') {
            return Vec::new();
        }
        let program = self.program;
        let index = self
            .namesakes
            .get_or_insert_with(|| namesake_index(program));
        let mut namesakes = index
            .get(relation)
            .into_iter()
            .flatten()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>();
        namesakes.sort();
        namesakes
    }

    fn push(&mut self, code: RelationTypingCode, relation: &str, citing: &str, message: String) {
        self.violations.insert(RelationTypingViolation {
            code,
            relation: relation.to_string(),
            citing: citing.to_string(),
            message,
        });
    }

    /// Visit every executed node of every derived rule, then the derived
    /// relations they reach.
    fn walk(&mut self) {
        self.walk_rules();
        self.derived_relations();
    }

    /// Visit every executed node of every derived rule.
    fn walk_rules(&mut self) {
        let mut names = self.program.derived.keys().collect::<Vec<_>>();
        names.sort();
        for name in names {
            let derived = &self.program.derived[name];
            let citing = derived.id.as_deref().unwrap_or(&derived.name);
            // Explain selects among the versions, but the dense compiler reads
            // the base semantics, so the check walks both.
            let base =
                (self.base_semantics || derived.versions.is_empty()).then_some(&derived.semantics);
            let semantics = base
                .into_iter()
                .chain(derived.versions.iter().map(|version| &version.semantics));
            for semantics in semantics {
                let context = Context {
                    entity: Some(derived.entity.as_str()),
                    membership: None,
                    citing,
                };
                match semantics {
                    crate::model::DerivedSemantics::Scalar(expr) => self.scalar(expr, context),
                    crate::model::DerivedSemantics::Judgment(expr) => self.judgment(expr, context),
                }
            }
        }
    }

    /// The kind a rule's entity has as an id: a filtered entity's ids are
    /// its source's current-slot ids.
    fn id_kind<'b>(&'b self, entity: &'b str) -> &'b str {
        self.filtered.get(entity).map_or(entity, String::as_str)
    }

    /// Report filtered entities whose derived relations disagree on the kind
    /// of the ids the entity is queried with.
    fn filtered_entity_conflicts(&mut self) {
        let mut kinds = BTreeMap::<String, BTreeMap<String, Vec<String>>>::new();
        for (name, schema) in &self.program.relations {
            let Some(derivation) = &schema.derivation else {
                continue;
            };
            let Some(entity) = &derivation.entity else {
                continue;
            };
            if let Some(kind) = self
                .slot_kinds
                .get(name)
                .and_then(|kinds| kinds.get(derivation.current_slot).cloned())
            {
                kinds
                    .entry(entity.clone())
                    .or_default()
                    .entry(kind)
                    .or_default()
                    .push(name.clone());
            }
        }
        for (entity, by_kind) in kinds {
            if by_kind.len() < 2 {
                continue;
            }
            let detail = by_kind
                .iter()
                .map(|(kind, relations)| {
                    let mut relations = relations.clone();
                    relations.sort();
                    format!("`{kind}` by {}", relations.join(", "))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let first = by_kind
                .values()
                .flatten()
                .min()
                .cloned()
                .unwrap_or_default();
            self.push(
                RelationTypingCode::FilteredEntityKindConflict,
                &first,
                &entity,
                format!(
                    "filtered entity `{entity}` is queried with ids of more than one kind ({detail}); every derived relation filtering to `{entity}` must key on the same source kind"
                ),
            );
        }
    }

    /// The declared kinds of an executed relation, reporting a violation when
    /// the relation is unknown or untyped, or its kind list does not match
    /// its arity.
    fn typed_slots(&mut self, relation: &str, citing: &str) -> Option<Vec<String>> {
        let Some(schema) = self.program.relations.get(relation) else {
            self.push(
                RelationTypingCode::UnknownRelation,
                relation,
                citing,
                format!(
                    "`{citing}` executes relation `{relation}`, which the program does not declare"
                ),
            );
            return None;
        };
        let arity = schema.arity;
        let Some(kinds) = self.slot_kinds.get(relation).map(<[String]>::to_vec) else {
            // A short name aggregated by a module that does not declare it is
            // its own relation, distinct from same-named declarations.
            let namesakes = self.namesakes(relation);
            let namesakes = if namesakes.is_empty() {
                String::new()
            } else {
                format!(
                    " It is not declared in the module that aggregates it, so it is a separate relation from the same-named {}; declare it there with `arguments`, or aggregate the declared relation through a module that declares it.",
                    namesakes.join(", ")
                )
            };
            self.push(
                RelationTypingCode::UntypedRelation,
                relation,
                citing,
                format!(
                    "relation `{relation}` (arity {arity}) is executed by `{citing}` but declares no entity kind for its tuple slots. Relation entity typing is mandatory: without slot kinds a dataset tuple stored in the other orientation aggregates nothing and yields a silent zero. Declare `data_relation.arguments` (one entity kind per slot, in tuple order) and recompile, or type an existing artifact with `axiom-rules-engine migrate artifact`.{namesakes}"
                ),
            );
            return None;
        };
        if kinds.len() != arity {
            self.push(
                RelationTypingCode::SlotEntityCount,
                relation,
                citing,
                format!(
                    "relation `{relation}` has arity {arity} but declares {} slot entity kinds {}",
                    kinds.len(),
                    format_kinds(&kinds)
                ),
            );
            return None;
        }
        let aliased = kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| self.filtered_names.contains(kind.as_str()))
            .map(|(slot, kind)| (slot, kind.clone()))
            .collect::<Vec<_>>();
        if !aliased.is_empty() {
            for (slot, kind) in aliased {
                let source_kind = self
                    .filtered
                    .get(&kind)
                    .map(|source| format!("`{source}`"))
                    .unwrap_or_else(|| "its source's kind".to_string());
                self.push(
                    RelationTypingCode::FilteredEntitySlotKind,
                    relation,
                    citing,
                    format!(
                        "slot {slot} of relation `{relation}` declares the filtered entity `{kind}` (slot kinds {}); a filtered entity is queried with its source's ids, so declare the kind of those ids ({source_kind})",
                        format_kinds(&kinds)
                    ),
                );
            }
            return None;
        }
        Some(kinds)
    }

    #[allow(clippy::too_many_arguments)]
    fn aggregation(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        value: Option<&RelatedValueRef>,
        where_clause: Option<&JudgmentExpr>,
        context: Context<'_>,
    ) {
        self.use_relation(relation);
        let citing = context.citing;
        let kinds = self.typed_slots(relation, citing);
        let derivation = self
            .program
            .relations
            .get(relation)
            .and_then(|schema| schema.derivation.as_ref());

        // A derived relation's runtime traversal reads its source with the
        // derivation's slots; only tuples supplied under the derived
        // relation's own name use the aggregate's. The two must agree or
        // explain and the bulk path read different positions.
        let (current, related) =
            traversed_slots(self.program, relation, current_slot, related_slot);
        if let Some(derivation) = derivation
            && (current_slot, related_slot) != (current, related)
        {
            self.push(
                RelationTypingCode::DerivedRelationSlotsDiverge,
                relation,
                citing,
                format!(
                    "`{citing}` aggregates derived relation `{relation}` with slots ({current_slot}, {related_slot}), but the derivation traverses its source with slots ({}, {}); recompile so the aggregate uses the derivation's slots",
                    derivation.current_slot, derivation.related_slot
                ),
            );
        }

        let mut related_entity = None;
        if let Some(kinds) = kinds.as_ref() {
            if current >= kinds.len() || related >= kinds.len() {
                self.push(
                    RelationTypingCode::SlotOutOfRange,
                    relation,
                    citing,
                    format!(
                        "`{citing}` reads relation `{relation}` at slots ({current}, {related}), outside its arity {}",
                        kinds.len()
                    ),
                );
            } else {
                let current_kind = kinds[current].as_str();
                related_entity = Some(kinds[related].clone());
                if let Some(entity) = context.entity
                    && entity != current_kind
                    && self.id_kind(entity) != current_kind
                {
                    self.push(
                        RelationTypingCode::CurrentSlotEntityMismatch,
                        relation,
                        citing,
                        if entity == SCALAR_ENTITY {
                            format!(
                                "`{citing}` has no entity (`{SCALAR_ENTITY}`), so the id it aggregates relation `{relation}` from has no kind to check against slot {current}, which declares `{current_kind}` (slot kinds {}). Give the rule the entity it is evaluated for",
                                format_kinds(kinds)
                            )
                        } else {
                            format!(
                                "`{citing}` evaluates on `{entity}` ids and aggregates relation `{relation}` keyed on slot {current}, which declares `{current_kind}` (slot kinds {}), so the lookup can never match those ids. Declare the kinds in tuple order and recompile",
                                format_kinds(kinds)
                            )
                        },
                    );
                }
                let related_kind = kinds[related].as_str();
                let mut referenced = BTreeSet::new();
                if let Some(RelatedValueRef::Derived(name)) = value {
                    referenced.insert(name.clone());
                }
                if let Some(where_clause) = where_clause {
                    collect_judgment_derived(where_clause, &mut referenced);
                }
                for name in referenced {
                    let Some(rule) = self.program.derived.get(&name) else {
                        continue;
                    };
                    if rule.entity == SCALAR_ENTITY
                        || rule.entity == related_kind
                        || self.id_kind(&rule.entity) == related_kind
                    {
                        continue;
                    }
                    self.push(
                        RelationTypingCode::RelatedSlotEntityMismatch,
                        relation,
                        citing,
                        format!(
                            "`{citing}` evaluates `{name}` (entity `{}`) on the ids in slot {related} of relation `{relation}`, which declares `{related_kind}` (slot kinds {})",
                            rule.entity,
                            format_kinds(kinds)
                        ),
                    );
                }
            }
        }

        if let Some(where_clause) = where_clause {
            let entity = related_entity.as_deref();
            self.judgment(
                where_clause,
                Context {
                    entity,
                    membership: None,
                    citing,
                },
            );
        }
    }

    fn membership(
        &mut self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        context: Context<'_>,
    ) {
        self.use_relation(relation);
        let citing = context.citing;
        let Some(kinds) = self.typed_slots(relation, citing) else {
            return;
        };
        if let Some(derivation) = self
            .program
            .relations
            .get(relation)
            .and_then(|schema| schema.derivation.as_ref())
            && (current_slot, related_slot) != (derivation.current_slot, derivation.related_slot)
        {
            let (derived_current, derived_related) =
                (derivation.current_slot, derivation.related_slot);
            self.push(
                RelationTypingCode::DerivedRelationSlotsDiverge,
                relation,
                citing,
                format!(
                    "`{citing}` tests membership in derived relation `{relation}` with slots ({current_slot}, {related_slot}), but the derivation traverses its source with slots ({derived_current}, {derived_related}); recompile so the test uses the derivation's slots"
                ),
            );
        }
        if current_slot >= kinds.len() || related_slot >= kinds.len() {
            self.push(
                RelationTypingCode::SlotOutOfRange,
                relation,
                citing,
                format!(
                    "`{citing}` tests membership in relation `{relation}` at slots ({current_slot}, {related_slot}), outside its arity {}",
                    kinds.len()
                ),
            );
            return;
        }
        let Some((current_kind, related_kind)) = context.membership else {
            return;
        };
        let expected = [(current_slot, current_kind), (related_slot, related_kind)];
        for (slot, expected_kind) in expected {
            if let Some(expected_kind) = expected_kind
                && kinds[slot] != expected_kind
            {
                self.push(
                    RelationTypingCode::MembershipSlotEntityMismatch,
                    relation,
                    citing,
                    format!(
                        "`{citing}` tests whether a `{expected_kind}` id is in slot {slot} of relation `{relation}`, which declares `{}` (slot kinds {})",
                        kinds[slot],
                        format_kinds(&kinds)
                    ),
                );
            }
        }
    }

    /// Check the source and predicate of every derived relation an
    /// executable node reaches, transitively through sources and membership
    /// tests.
    fn derived_relations(&mut self) {
        // A worklist: each used relation is queued once, when first used, so
        // a chain of n derived relations is checked in n steps rather than
        // rescanning every used relation once per link.
        while let Some(name) = self.unchecked.pop() {
            self.derived_relation(name);
        }
    }

    /// Check the source and predicate of one used relation, if it is
    /// derived, using every relation they read.
    fn derived_relation(&mut self, name: String) {
        let Some(schema) = self.program.relations.get(&name) else {
            return;
        };
        let Some(derivation) = schema.derivation.clone() else {
            return;
        };
        let schema_kinds = schema.slot_entities.clone();
        if derivation.source_path.is_some() {
            self.path_source(&name, schema.arity, &schema_kinds, &derivation);
        } else if let Some(source_relation) = derivation.source_relation.as_deref() {
            self.use_relation(source_relation);
            let source_kinds = self.typed_slots(source_relation, &name);
            if let Some(source_kinds) = source_kinds.as_ref() {
                for declared in [&derivation.slot_entities, &schema_kinds] {
                    if declared.is_empty() || declared == source_kinds {
                        continue;
                    }
                    self.push(
                    RelationTypingCode::DerivedRelationSourceConflict,
                    &name,
                    &name,
                    format!(
                        "derived relation `{name}` declares slot kinds {} but its source `{}` declares {}; a derived relation filters its source's tuples, so the kinds must agree",
                        format_kinds(declared),
                        source_relation,
                        format_kinds(source_kinds)
                    ),
                );
                }
            }
            // A derived source traverses its own source with its own
            // slots, whatever slots reach it.
            if let Some(source) = self
                .program
                .relations
                .get(source_relation)
                .and_then(|schema| schema.derivation.as_ref())
                && (source.current_slot, source.related_slot)
                    != (derivation.current_slot, derivation.related_slot)
            {
                self.push(
                RelationTypingCode::DerivedRelationSlotsDiverge,
                &name,
                &name,
                format!(
                    "derived relation `{name}` reads its derived source `{}` with slots ({}, {}), but that source traverses its own source with slots ({}, {}); a chain of derived relations must keep one direction",
                    source_relation,
                    derivation.current_slot,
                    derivation.related_slot,
                    source.current_slot,
                    source.related_slot
                ),
            );
            }
        }
        self.predicate_rules(&name, &derivation);
        let kinds = self.slot_kinds.get(&name);
        let slot = |slot: usize| kinds.and_then(|kinds| kinds.get(slot)).map(String::as_str);
        let current_kind = slot(derivation.current_slot).map(str::to_string);
        let related_kind = slot(derivation.related_slot).map(str::to_string);
        self.judgment(
            &derivation.predicate,
            Context {
                entity: related_kind.as_deref(),
                membership: Some((current_kind.as_deref(), related_kind.as_deref())),
                citing: &name,
            },
        );
    }

    /// A path reads every step relation, but only its first and last kinds
    /// describe the new pairs. No intermediate slot is inherited by the
    /// derived relation's declaration.
    fn path_source(
        &mut self,
        name: &str,
        arity: usize,
        schema_kinds: &[String],
        derivation: &RelationDerivation,
    ) {
        let Some(path) = derivation.source_path.as_ref() else {
            return;
        };
        let valid_declaration = arity == 2
            && derivation.current_slot < 2
            && derivation.related_slot < 2
            && derivation.current_slot != derivation.related_slot
            && derivation.slot_entities.len() == 2
            && (schema_kinds.is_empty() || schema_kinds == derivation.slot_entities);
        if !valid_declaration {
            self.push(
                RelationTypingCode::RelationPathRequiresBinaryDeclaration, name, name,
                format!("path-sourced derived relation `{name}` requires arity 2, two declared slot kinds and distinct current_slot and related_slot in 0..2"),
            );
        }
        let declared_kind = |slot: usize| derivation.slot_entities.get(slot).cloned();
        let mut previous_kind = declared_kind(derivation.current_slot);
        for (index, step) in path.iter().enumerate() {
            self.use_relation(&step.relation);
            let schema = self.program.relations.get(&step.relation);
            let step_arity = schema.map(|schema| schema.arity);
            let source_slots = schema
                .and_then(|schema| schema.derivation.as_ref())
                .map(|source| (source.current_slot, source.related_slot));
            if step_arity.is_some_and(|arity| arity != 2) {
                self.push(
                    RelationTypingCode::RelationPathStepArity, &step.relation, name,
                    format!("path step {index} of derived relation `{name}` reads non-binary relation `{}`", step.relation),
                );
            }
            let kinds = self.typed_slots(&step.relation, name);
            if step.from_slot >= 2 || step.to_slot >= 2 || step.from_slot == step.to_slot {
                self.push(
                    RelationTypingCode::RelationPathStepSlots, &step.relation, name,
                    format!("path step {index} of derived relation `{name}` requires distinct binary slots 0 and 1; found ({}, {})", step.from_slot, step.to_slot),
                );
            }
            if let Some((current_slot, related_slot)) = source_slots
                && (step.from_slot, step.to_slot) != (current_slot, related_slot)
            {
                self.push(
                    RelationTypingCode::InverseDerivedTraversalUnsupported, &step.relation, name,
                    format!("path step {index} of derived relation `{name}` must traverse derived relation `{}` forward from slot {} to slot {}", step.relation, current_slot, related_slot),
                );
            }
            let kind = |slot: usize| kinds.as_ref().and_then(|kinds| kinds.get(slot)).cloned();
            let from_kind = kind(step.from_slot);
            if let (Some(expected), Some(actual)) = (previous_kind.as_ref(), from_kind.as_ref())
                && expected != actual
            {
                self.push(
                    RelationTypingCode::RelationPathKindMismatch, &step.relation, name,
                    format!("path step {index} of derived relation `{name}` starts at kind `{actual}`, but the preceding endpoint has kind `{expected}`"),
                );
            }
            previous_kind = kind(step.to_slot);
        }
        if let (Some(actual), Some(expected)) =
            (previous_kind, declared_kind(derivation.related_slot))
            && actual != expected
        {
            self.push(
                RelationTypingCode::RelationPathKindMismatch, name, name,
                format!("path of derived relation `{name}` ends at kind `{actual}`, but its candidate kind is `{expected}`"),
            );
        }
    }

    /// A derived relation's predicate runs each rule it reads on the current
    /// id when the rule's entity is the derivation's declared current kind,
    /// and on the related id otherwise. Report rules that would run on an id
    /// of another kind.
    fn predicate_rules(&mut self, name: &str, derivation: &crate::model::RelationDerivation) {
        let Some(effective_related) = self
            .slot_kinds
            .get(name)
            .and_then(|kinds| kinds.get(derivation.related_slot))
            .cloned()
        else {
            return;
        };
        let routed_related = derivation.slot_entities.get(derivation.related_slot);
        let mut referenced = BTreeSet::new();
        collect_judgment_derived(&derivation.predicate, &mut referenced);
        for rule_name in referenced {
            let Some(rule) = self.program.derived.get(&rule_name) else {
                continue;
            };
            let entity = rule.entity.as_str();
            if entity == SCALAR_ENTITY
                || predicate_routes_to_current(derivation, entity)
                || routed_related.is_some_and(|kind| kind == entity)
                || entity == effective_related
                || self.id_kind(entity) == effective_related
            {
                continue;
            }
            let hint = if derivation.slot_entities.is_empty() {
                " Declare `slot_entities` on the derivation (`derived_relation.slot_entities` in RuleSpec) so the predicate can read the current id"
            } else {
                ""
            };
            self.push(
                RelationTypingCode::PredicateEntityMismatch,
                name,
                name,
                format!(
                    "the predicate of derived relation `{name}` reads `{rule_name}` (entity `{entity}`), which runs on the related id of kind `{effective_related}`.{hint}"
                ),
            );
        }
    }

    fn scalar(&mut self, expr: &ScalarExpr, context: Context<'_>) {
        match expr {
            ScalarExpr::OnlyRelated {
                relation,
                current_slot,
                related_slot,
                value,
            } => {
                if current_slot == related_slot
                    || *current_slot > 1
                    || *related_slot > 1
                    || !matches!(value, RelatedValueRef::Input(_))
                    || self
                        .program
                        .relations
                        .get(relation)
                        .is_some_and(|schema| schema.arity != 2)
                {
                    self.push(RelationTypingCode::OnlyRelatedShape, relation, context.citing,
                        "only_related requires an input reference and distinct slots of a binary relation".into());
                }
                self.aggregation(
                    relation,
                    *current_slot,
                    *related_slot,
                    Some(value),
                    None,
                    context,
                );
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => self.aggregation(
                relation,
                *current_slot,
                *related_slot,
                None,
                where_clause.as_deref(),
                context,
            ),
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            }
            | ScalarExpr::ExtremeRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
                ..
            }
            | ScalarExpr::SingleRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => self.aggregation(
                relation,
                *current_slot,
                *related_slot,
                Some(value),
                where_clause.as_deref(),
                context,
            ),
            ScalarExpr::ParameterLookup { index, .. }
            | ScalarExpr::Ceil(index)
            | ScalarExpr::Floor(index) => self.scalar(index, context),
            ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
                for item in items {
                    self.scalar(item, context);
                }
            }
            ScalarExpr::Sub(left, right)
            | ScalarExpr::Mul(left, right)
            | ScalarExpr::Div(left, right)
            | ScalarExpr::DateAddDays {
                date: left,
                days: right,
            }
            | ScalarExpr::DateAddMonths {
                date: left,
                months: right,
            }
            | ScalarExpr::DateAddYears {
                date: left,
                years: right,
            }
            | ScalarExpr::DaysBetween {
                from: left,
                to: right,
            } => {
                self.scalar(left, context);
                self.scalar(right, context);
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, context);
                self.scalar(then_expr, context);
                self.scalar(else_expr, context);
            }
            // As in explain, a derived relation's membership binding does not
            // reach a match fallback's pattern labels or an over-periods
            // reduction.
            ScalarExpr::NoMatch { subject, patterns } => {
                self.scalar(subject, context);
                let unbound = Context {
                    membership: None,
                    ..context
                };
                for pattern in patterns {
                    self.scalar(pattern, unbound);
                }
            }
            ScalarExpr::OverPeriods { value, n, .. } => {
                let unbound = Context {
                    membership: None,
                    ..context
                };
                self.scalar(value, unbound);
                if let Some(n) = n {
                    self.scalar(n, unbound);
                }
            }
            ScalarExpr::Literal(_)
            | ScalarExpr::InputForYear { .. }
            | ScalarExpr::Input(_)
            | ScalarExpr::InputOrElse { .. }
            | ScalarExpr::Derived(_)
            | ScalarExpr::PeriodStart
            | ScalarExpr::PeriodEnd => {}
            // Reads through a term are checked by `relation_roles`, over every
            // rule and derived relation, used or not.
            ScalarExpr::RoleInput { .. } | ScalarExpr::RoleDerived { .. } => {}
        }
    }

    fn judgment(&mut self, expr: &JudgmentExpr, context: Context<'_>) {
        match expr {
            JudgmentExpr::Comparison { left, right, .. } => {
                self.scalar(left, context);
                self.scalar(right, context);
            }
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => self.membership(relation, *current_slot, *related_slot, context),
            JudgmentExpr::RelationAtom { relation, .. } => self.use_relation(relation),
            JudgmentExpr::Exists { relation, body, .. } => {
                self.use_relation(relation);
                self.judgment(body, context);
            }
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, context);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, context),
            JudgmentExpr::Derived(_)
            | JudgmentExpr::RoleDerived { .. }
            | JudgmentExpr::SameEntity { .. } => {}
        }
    }

    /// Check every use of a term in every rule and derived relation, used
    /// or not, so a program is refused the same way whatever it executes.
    /// A qualified read, atom or term comparison must sit in a
    /// derived relation's predicate, outside any aggregation's `where`
    /// clause; that derivation must declare the term's slot kind; and a rule
    /// read through a term must have the term's kind.
    fn relation_roles(&mut self) {
        let mut names = self.program.derived.keys().collect::<Vec<_>>();
        names.sort();
        for name in names {
            let derived = &self.program.derived[name];
            let citing = derived.id.as_deref().unwrap_or(&derived.name);
            let mut reads = Vec::new();
            for semantics in std::iter::once(&derived.semantics)
                .chain(derived.versions.iter().map(|version| &version.semantics))
            {
                match semantics {
                    crate::model::DerivedSemantics::Scalar(expr) => {
                        collect_scalar_role_reads(expr, None, &mut reads)
                    }
                    crate::model::DerivedSemantics::Judgment(expr) => {
                        collect_judgment_role_reads(expr, None, &mut reads)
                    }
                }
            }
            for read in reads {
                let place = match read.aggregated {
                    Some(relation) => format!(
                        "inside the `where` clause of an aggregation over `{relation}`, where no term is bound"
                    ),
                    None => "outside any derived relation's predicate".to_string(),
                };
                let description = read.construct.description();
                self.push(
                    RelationTypingCode::RelationRoleOutsideScope,
                    read.aggregated.unwrap_or_default(),
                    citing,
                    format!(
                        "`{citing}` uses {description} {place}; relation terms may appear only in a derived relation's predicate, outside any aggregation's `where` clause"
                    ),
                );
            }
        }
        let program = self.program;
        for (name, schema) in &program.relations {
            let Some(derivation) = &schema.derivation else {
                continue;
            };
            if derivation.source_path.is_some() {
                self.path_source(name, schema.arity, &schema.slot_entities, derivation);
            }
            let mut reads = Vec::new();
            collect_judgment_role_reads(&derivation.predicate, None, &mut reads);
            for read in reads {
                if let Some(relation) = read.aggregated {
                    let description = read.construct.description();
                    self.push(
                        RelationTypingCode::RelationRoleOutsideScope,
                        relation,
                        name,
                        format!(
                            "the predicate of derived relation `{name}` uses {description} inside the `where` clause of an aggregation over `{relation}`, where no term is bound; use it outside the aggregation"
                        ),
                    );
                    continue;
                }
                self.role_construct(name, derivation, read.construct, &read.bindings);
            }
        }
    }

    /// A variable's kind comes from its generator slot; anchor and candidate
    /// always use the containing derivation's explicit slot declarations.
    fn term_kind(
        &mut self,
        citing: &str,
        derivation: &RelationDerivation,
        term: Term,
        bindings: &[(&str, usize)],
    ) -> Option<String> {
        let slot = match term {
            Term::Anchor => derivation.current_slot,
            Term::Candidate => derivation.related_slot,
            Term::Var(index) => {
                let Some((relation, slot)) = bindings.iter().rev().nth(index) else {
                    self.push(
                        RelationTypingCode::RelationVariableOutOfScope,
                        citing,
                        citing,
                        format!(
                            "variable index {index} in `{citing}` has no enclosing exists binding"
                        ),
                    );
                    return None;
                };
                return self.program.relations.get(*relation).and_then(|schema| {
                    schema
                        .derivation
                        .as_ref()
                        .filter(|derived| !derived.slot_entities.is_empty())
                        .map(|derived| &derived.slot_entities)
                        .unwrap_or(&schema.slot_entities)
                        .get(*slot)
                        .cloned()
                });
            }
            #[cfg(test)]
            Term::Id(_) => return None,
        };
        match derivation.slot_entities.get(slot) {
            Some(kind) => Some(kind.clone()),
            None => {
                self.push(
                    RelationTypingCode::RelationRolesNeedSlotKinds,
                    citing,
                    citing,
                    format!(
                        "the predicate of derived relation `{citing}` uses `{}`, but its derivation declares no kind for slot {slot}; declare `derived_relation.slot_entities`",
                        term.as_str()
                    ),
                );
                None
            }
        }
    }

    fn role_construct(
        &mut self,
        citing: &str,
        derivation: &RelationDerivation,
        construct: RoleConstruct<'_>,
        bindings: &[(&str, usize)],
    ) {
        match construct {
            RoleConstruct::Atom { relation, terms } => {
                self.role_atom(citing, derivation, relation, terms, None, bindings);
            }
            RoleConstruct::Exists {
                relation,
                terms,
                variable_slot,
            } => {
                if terms.get(variable_slot) != Some(&Term::Var(0)) {
                    self.push(
                        RelationTypingCode::RelationExistsVariableCount,
                        relation,
                        citing,
                        format!(
                            "exists generator `{relation}` must bind exactly one variable slot"
                        ),
                    );
                }
                if derivation.slot_entities.is_empty() {
                    self.push(
                        RelationTypingCode::RelationRolesNeedSlotKinds,
                        citing,
                        citing,
                        format!(
                            "exists in `{citing}` requires declared derived_relation.slot_entities"
                        ),
                    );
                }
                if self
                    .program
                    .relations
                    .get(relation)
                    .and_then(|schema| schema.derivation.as_ref())
                    .is_some_and(|generator| variable_slot != generator.related_slot)
                {
                    self.push(
                        RelationTypingCode::InverseDerivedTraversalUnsupported,
                        relation,
                        citing,
                        format!(
                            "exists cannot traverse derived relation `{relation}` backwards; bind its related slot"
                        ),
                    );
                }
                self.role_atom(
                    citing,
                    derivation,
                    relation,
                    terms,
                    Some(variable_slot),
                    bindings,
                );
            }
            RoleConstruct::SameEntity { left, right } => {
                let left_kind = self.term_kind(citing, derivation, left, bindings);
                let right_kind = self.term_kind(citing, derivation, right, bindings);
                if let (Some(left_kind), Some(right_kind)) = (left_kind, right_kind)
                    && left_kind != right_kind
                {
                    self.push(
                        RelationTypingCode::RelationTermKindMismatch,
                        citing,
                        citing,
                        format!(
                            "term comparison in `{citing}` compares `{}` of kind `{left_kind}` with `{}` of kind `{right_kind}`; both terms must have the same kind",
                            left.as_str(),
                            right.as_str()
                        ),
                    );
                }
            }
            RoleConstruct::Read { term, name, rule } => {
                let t = term.as_str();
                let slot = term_slot(derivation, term);
                if let Some(slot) = slot
                    && derivation.slot_entities.get(slot).is_none()
                {
                    self.push(
                        RelationTypingCode::RelationRolesNeedSlotKinds,
                        citing,
                        citing,
                        format!(
                            "the predicate of derived relation `{citing}` reads `{t}.{name}`, but the derivation declares no kind for slot {slot}, so `{t}` has none. Declare `slot_entities` on the derivation (`derived_relation.slot_entities` in RuleSpec); kinds declared on the relation or taken from its source are not stamped onto a derivation whose slots share one kind"
                        ),
                    );
                    return;
                }
                let Some(kind) = self.term_kind(citing, derivation, term, bindings) else {
                    return;
                };
                if !rule {
                    return;
                }
                let Some(derived) = self.program.derived.get(name) else {
                    return;
                };
                let entity = derived.entity.as_str();
                if entity == kind || self.id_kind(entity) == kind {
                    return;
                }
                let detail = if entity == SCALAR_ENTITY {
                    format!(
                        "`{name}` has no entity (`{SCALAR_ENTITY}`), so it reads no id; read it unqualified"
                    )
                } else {
                    "a rule read through a term must have that term's kind".to_string()
                };
                let at_slot = slot.map_or(String::new(), |slot| format!(" (slot {slot})"));
                self.push(
                    RelationTypingCode::RelationRoleEntityMismatch,
                    citing,
                    citing,
                    format!(
                        "the predicate of derived relation `{citing}` reads `{t}.{name}`: `{name}` has entity `{entity}`, but `{t}` is a `{kind}` id{at_slot}; {detail}"
                    ),
                );
            }
        }
    }

    fn role_atom(
        &mut self,
        citing: &str,
        derivation: &RelationDerivation,
        relation: &str,
        terms: &[Term],
        variable_slot: Option<usize>,
        bindings: &[(&str, usize)],
    ) {
        let program = self.program;
        let Some(schema) = program.relations.get(relation) else {
            self.push(
                RelationTypingCode::UnknownRelation,
                relation,
                citing,
                format!("`{citing}` uses an atom on undeclared relation `{relation}`"),
            );
            return;
        };
        let short_name = relation
            .rsplit_once("#relation.")
            .map_or(relation, |(_, short)| short);
        if crate::formula::is_builtin_function(short_name) {
            self.push(
                RelationTypingCode::RelationNameShadowsBuiltin,
                relation,
                citing,
                format!(
                    "relation `{relation}` has a built-in function name and cannot be used as an atom"
                ),
            );
        }
        if schema.derivation.is_some() && schema.arity != 2 {
            self.push(
                RelationTypingCode::DerivedRelationAtomArityUnsupported,
                relation,
                citing,
                format!(
                    "an atom on derived relation `{relation}` requires arity 2, but it declares {}",
                    schema.arity
                ),
            );
        }
        if terms.len() != schema.arity {
            self.push(
                RelationTypingCode::RelationAtomArityMismatch,
                relation,
                citing,
                format!(
                    "atom `{relation}` has {} arguments but the relation has arity {}",
                    terms.len(),
                    schema.arity
                ),
            );
        }
        // Atoms require an explicit kind declaration on the named relation;
        // inheriting its source's kinds is insufficient.
        let kinds = schema
            .derivation
            .as_ref()
            .filter(|derived| !derived.slot_entities.is_empty())
            .map(|derived| &derived.slot_entities)
            .unwrap_or(&schema.slot_entities);
        if kinds.is_empty() {
            self.push(
                RelationTypingCode::RelationAtomNeedsTypedRelation,
                relation,
                citing,
                format!("atom `{relation}` requires declared slot entity kinds"),
            );
        } else {
            // Preserve the general checks for malformed/aliased slot kinds as
            // well as the atom's term-to-slot kind checks.
            self.typed_slots(relation, citing);
        }
        for (slot, term) in terms.iter().enumerate() {
            if variable_slot == Some(slot) {
                continue;
            }
            if let (Some(actual), Some(expected)) = (
                self.term_kind(citing, derivation, *term, bindings),
                kinds.get(slot),
            ) && actual != *expected
            {
                self.push(
                    RelationTypingCode::RelationAtomKindMismatch,
                    relation,
                    citing,
                    format!(
                        "atom `{relation}` argument {slot} is `{}` of kind `{actual}`, but the slot declares `{expected}`",
                        term.as_str()
                    ),
                );
            }
        }
    }
}

/// The derivation slot `term` names; a variable names none.
fn term_slot(derivation: &RelationDerivation, term: Term) -> Option<usize> {
    match term {
        Term::Anchor => Some(derivation.current_slot),
        Term::Candidate => Some(derivation.related_slot),
        Term::Var(_) => None,
        #[cfg(test)]
        Term::Id(_) => None,
    }
}

/// A construct using terms, found by [`collect_judgment_role_reads`].
struct RoleRead<'p> {
    construct: RoleConstruct<'p>,
    /// The relation of the innermost aggregation whose `where` clause holds
    /// the read, or `None` when no aggregation encloses it.
    aggregated: Option<&'p str>,
    bindings: Vec<(&'p str, usize)>,
}

#[derive(Clone, Copy)]
enum RoleConstruct<'p> {
    Read {
        term: Term,
        name: &'p str,
        rule: bool,
    },
    Atom {
        relation: &'p str,
        terms: &'p [Term],
    },
    SameEntity {
        left: Term,
        right: Term,
    },
    Exists {
        relation: &'p str,
        terms: &'p [Term],
        variable_slot: usize,
    },
}

impl RoleConstruct<'_> {
    fn description(self) -> String {
        match self {
            Self::Read { term, name, .. } => format!("`{}.{name}`", term.as_str()),
            Self::Atom { relation, .. } => format!("relation atom `{relation}`"),
            Self::Exists { relation, .. } => format!("exists over relation `{relation}`"),
            Self::SameEntity { .. } => "a term comparison".to_string(),
        }
    }
}

/// Every construct using terms in `expr`, with the relation of the innermost
/// aggregation whose `where` clause holds it. `aggregated` is that relation
/// for the expression itself.
fn collect_judgment_role_reads<'p>(
    expr: &'p JudgmentExpr,
    aggregated: Option<&'p str>,
    out: &mut Vec<RoleRead<'p>>,
) {
    collect_judgment_role_reads_in(expr, aggregated, out, &[]);
}

fn collect_scalar_role_reads<'p>(
    expr: &'p ScalarExpr,
    aggregated: Option<&'p str>,
    out: &mut Vec<RoleRead<'p>>,
) {
    collect_scalar_role_reads_in(expr, aggregated, out, &[]);
}

fn collect_judgment_role_reads_in<'p>(
    expr: &'p JudgmentExpr,
    aggregated: Option<&'p str>,
    out: &mut Vec<RoleRead<'p>>,
    bindings: &[(&'p str, usize)],
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_role_reads_in(left, aggregated, out, bindings);
            collect_scalar_role_reads_in(right, aggregated, out, bindings);
        }
        JudgmentExpr::RoleDerived { term, name } => out.push(RoleRead {
            construct: RoleConstruct::Read {
                term: *term,
                name,
                rule: true,
            },
            aggregated,
            bindings: bindings.to_vec(),
        }),
        JudgmentExpr::RelationAtom { relation, terms } => out.push(RoleRead {
            construct: RoleConstruct::Atom { relation, terms },
            aggregated,
            bindings: bindings.to_vec(),
        }),
        JudgmentExpr::Exists {
            relation,
            terms,
            variable_slot,
            body,
        } => {
            out.push(RoleRead {
                construct: RoleConstruct::Exists {
                    relation,
                    terms,
                    variable_slot: *variable_slot,
                },
                aggregated,
                bindings: bindings.to_vec(),
            });
            let mut nested = bindings.to_vec();
            nested.push((relation, *variable_slot));
            collect_judgment_role_reads_in(body, aggregated, out, &nested);
        }
        JudgmentExpr::SameEntity { left, right, .. } => out.push(RoleRead {
            construct: RoleConstruct::SameEntity {
                left: *left,
                right: *right,
            },
            aggregated,
            bindings: bindings.to_vec(),
        }),
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_role_reads_in(item, aggregated, out, bindings);
            }
        }
        JudgmentExpr::Not(item) => collect_judgment_role_reads_in(item, aggregated, out, bindings),
        JudgmentExpr::Derived(_) | JudgmentExpr::RelationMember { .. } => {}
    }
}

fn collect_scalar_role_reads_in<'p>(
    expr: &'p ScalarExpr,
    aggregated: Option<&'p str>,
    out: &mut Vec<RoleRead<'p>>,
    bindings: &[(&'p str, usize)],
) {
    match expr {
        ScalarExpr::RoleInput { term, name } => out.push(RoleRead {
            construct: RoleConstruct::Read {
                term: *term,
                name,
                rule: false,
            },
            aggregated,
            bindings: bindings.to_vec(),
        }),
        ScalarExpr::RoleDerived { term, name } => out.push(RoleRead {
            construct: RoleConstruct::Read {
                term: *term,
                name,
                rule: true,
            },
            aggregated,
            bindings: bindings.to_vec(),
        }),
        ScalarExpr::CountRelated {
            relation,
            where_clause,
            ..
        }
        | ScalarExpr::SumRelated {
            relation,
            where_clause,
            ..
        }
        | ScalarExpr::ExtremeRelated {
            relation,
            where_clause,
            ..
        }
        | ScalarExpr::SingleRelated {
            relation,
            where_clause,
            ..
        } => {
            if let Some(where_clause) = where_clause {
                collect_judgment_role_reads_in(where_clause, Some(relation), out, bindings);
            }
        }
        ScalarExpr::ParameterLookup { index, .. }
        | ScalarExpr::Ceil(index)
        | ScalarExpr::Floor(index) => {
            collect_scalar_role_reads_in(index, aggregated, out, bindings)
        }
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_role_reads_in(item, aggregated, out, bindings);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right)
        | ScalarExpr::DateAddDays {
            date: left,
            days: right,
        }
        | ScalarExpr::DateAddMonths {
            date: left,
            months: right,
        }
        | ScalarExpr::DateAddYears {
            date: left,
            years: right,
        }
        | ScalarExpr::DaysBetween {
            from: left,
            to: right,
        } => {
            collect_scalar_role_reads_in(left, aggregated, out, bindings);
            collect_scalar_role_reads_in(right, aggregated, out, bindings);
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_role_reads_in(condition, aggregated, out, bindings);
            collect_scalar_role_reads_in(then_expr, aggregated, out, bindings);
            collect_scalar_role_reads_in(else_expr, aggregated, out, bindings);
        }
        ScalarExpr::NoMatch { subject, patterns } => {
            collect_scalar_role_reads_in(subject, aggregated, out, bindings);
            for pattern in patterns {
                collect_scalar_role_reads_in(pattern, aggregated, out, bindings);
            }
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_role_reads_in(value, aggregated, out, bindings);
            if let Some(n) = n {
                collect_scalar_role_reads_in(n, aggregated, out, bindings);
            }
        }
        // `only_related` reads a named input at the related id, and an
        // aggregated value is a name, not an expression, so neither holds a
        // term.
        ScalarExpr::OnlyRelated { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::InputForYear { .. }
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::Derived(_)
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
    }
}

/// Module-qualified relation names by short name: `<module>#relation.<short>`
/// under `<short>`, for every `short` without `#`. A name ends with
/// `#relation.<short>` for such a `short` exactly when its fragment after
/// its last `#` is `relation.<short>`, so one pass over the names finds every
/// name a scan for that suffix would.
fn namesake_index(program: &Program) -> BTreeMap<&str, Vec<&str>> {
    let mut index = BTreeMap::<&str, Vec<&str>>::new();
    for name in program.relations.keys() {
        if let Some(short) = name
            .rsplit_once('#')
            .and_then(|(_, fragment)| fragment.strip_prefix("relation."))
        {
            index.entry(short).or_default().push(name);
        }
    }
    index
}

/// Derived rules a `where` clause reads on the related id: every reference
/// outside a nested aggregation.
pub(crate) fn judgment_rule_references(expr: &JudgmentExpr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_judgment_derived(expr, &mut out);
    out
}

/// Derived rules a related predicate evaluates on the related id: every
/// reference outside a nested aggregation, whose own predicate and value
/// run on that aggregation's related ids instead.
fn collect_judgment_derived(expr: &JudgmentExpr, out: &mut BTreeSet<String>) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_derived(left, out);
            collect_scalar_derived(right, out);
        }
        JudgmentExpr::Derived(name) => {
            out.insert(name.clone());
        }
        // A rule read through a term runs on that term's id, not a routed
        // one; `Checker::relation_roles` checks its kind.
        JudgmentExpr::RelationMember { .. }
        | JudgmentExpr::RoleDerived { .. }
        | JudgmentExpr::RelationAtom { .. }
        | JudgmentExpr::SameEntity { .. } => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_derived(item, out);
            }
        }
        JudgmentExpr::Not(item) | JudgmentExpr::Exists { body: item, .. } => {
            collect_judgment_derived(item, out)
        }
    }
}

fn collect_scalar_derived(expr: &ScalarExpr, out: &mut BTreeSet<String>) {
    match expr {
        ScalarExpr::Derived(name) => {
            out.insert(name.clone());
        }
        ScalarExpr::ParameterLookup { index, .. }
        | ScalarExpr::Ceil(index)
        | ScalarExpr::Floor(index) => collect_scalar_derived(index, out),
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_derived(item, out);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right)
        | ScalarExpr::DateAddDays {
            date: left,
            days: right,
        }
        | ScalarExpr::DateAddMonths {
            date: left,
            months: right,
        }
        | ScalarExpr::DateAddYears {
            date: left,
            years: right,
        }
        | ScalarExpr::DaysBetween {
            from: left,
            to: right,
        } => {
            collect_scalar_derived(left, out);
            collect_scalar_derived(right, out);
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_derived(condition, out);
            collect_scalar_derived(then_expr, out);
            collect_scalar_derived(else_expr, out);
        }
        ScalarExpr::NoMatch { subject, patterns } => {
            collect_scalar_derived(subject, out);
            for pattern in patterns {
                collect_scalar_derived(pattern, out);
            }
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_derived(value, out);
            if let Some(n) = n {
                collect_scalar_derived(n, out);
            }
        }
        ScalarExpr::OnlyRelated { .. } => {}
        // As in `collect_judgment_derived`, a read through a term is not
        // routed.
        ScalarExpr::RoleInput { .. } | ScalarExpr::RoleDerived { .. } => {}
        ScalarExpr::CountRelated { .. }
        | ScalarExpr::SumRelated { .. }
        | ScalarExpr::ExtremeRelated { .. }
        | ScalarExpr::SingleRelated { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::InputForYear { .. }
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
    }
}

fn format_kinds(kinds: &[String]) -> String {
    format!("[{}]", kinds.join(", "))
}

#[cfg(test)]
mod membership_context_tests {
    //! Ported from the usage-inference tests of #205: the typing check, which
    //! replaced that inference, must keep a derived relation's membership
    //! context in exactly the positions explain evaluates on the same ids.
    use super::{RelationTypingCode, check_program};
    use crate::spec::ProgramSpec;
    use serde_json::{Value, json};

    fn int(value: i64) -> Value {
        json!({"kind": "literal", "value": {"kind": "integer", "value": value}})
    }

    fn member(current_slot: usize, related_slot: usize) -> Value {
        json!({
            "kind": "relation_member",
            "relation": "head",
            "current_slot": current_slot,
            "related_slot": related_slot
        })
    }

    /// 1 when `head` holds for the IDs in scope, else 0.
    fn indicator(member: Value) -> Value {
        json!({"kind": "if", "condition": member, "then_expr": int(1), "else_expr": int(0)})
    }

    fn equals_one(scalar: Value) -> Value {
        json!({"kind": "comparison", "left": scalar, "op": "eq", "right": int(1)})
    }

    /// Every scalar position that engine.rs evaluates on the same ID as its
    /// parent, holding `operand`. Static collection does not evaluate, so
    /// types and values do not matter; every position must keep the context.
    fn same_id_positions(operand: &Value) -> Vec<(&'static str, Value)> {
        let date = json!({"kind": "period_start"});
        vec![
            (
                "parameter index",
                json!({"kind": "parameter_lookup", "parameter": "rate", "index": operand}),
            ),
            ("add", json!({"kind": "add", "items": [int(0), operand]})),
            (
                "sub left",
                json!({"kind": "sub", "left": operand, "right": int(0)}),
            ),
            (
                "sub right",
                json!({"kind": "sub", "left": int(0), "right": operand}),
            ),
            (
                "mul left",
                json!({"kind": "mul", "left": operand, "right": int(1)}),
            ),
            (
                "mul right",
                json!({"kind": "mul", "left": int(1), "right": operand}),
            ),
            (
                "div left",
                json!({"kind": "div", "left": operand, "right": int(1)}),
            ),
            (
                "div right",
                json!({"kind": "div", "left": int(1), "right": operand}),
            ),
            ("max", json!({"kind": "max", "items": [int(0), operand]})),
            ("min", json!({"kind": "min", "items": [operand, int(1)]})),
            ("ceil", json!({"kind": "ceil", "value": operand})),
            ("floor", json!({"kind": "floor", "value": operand})),
            (
                "date_add_days date",
                json!({"kind": "date_add_days", "date": operand, "days": int(0)}),
            ),
            (
                "date_add_days days",
                json!({"kind": "date_add_days", "date": date, "days": operand}),
            ),
            (
                "date_add_months date",
                json!({"kind": "date_add_months", "date": operand, "months": int(0)}),
            ),
            (
                "date_add_months months",
                json!({"kind": "date_add_months", "date": date, "months": operand}),
            ),
            (
                "date_add_years date",
                json!({"kind": "date_add_years", "date": operand, "years": int(0)}),
            ),
            (
                "date_add_years years",
                json!({"kind": "date_add_years", "date": date, "years": operand}),
            ),
            (
                "days_between from",
                json!({"kind": "days_between", "from": operand, "to": date}),
            ),
            (
                "days_between to",
                json!({"kind": "days_between", "from": date, "to": operand}),
            ),
            (
                "if condition",
                json!({"kind": "if", "condition": equals_one(operand.clone()), "then_expr": int(1), "else_expr": int(0)}),
            ),
            (
                "if then",
                json!({"kind": "if", "condition": equals_one(int(1)), "then_expr": operand, "else_expr": int(0)}),
            ),
            (
                "if else",
                json!({"kind": "if", "condition": equals_one(int(1)), "then_expr": int(0), "else_expr": operand}),
            ),
            (
                "not",
                indicator(json!({"kind": "not", "item": equals_one(operand.clone())})),
            ),
            (
                "and",
                indicator(
                    json!({"kind": "and", "items": [equals_one(int(1)), equals_one(operand.clone())]}),
                ),
            ),
            (
                "or",
                indicator(
                    json!({"kind": "or", "items": [equals_one(operand.clone()), equals_one(int(1))]}),
                ),
            ),
            (
                "comparison right",
                indicator(
                    json!({"kind": "comparison", "left": int(1), "op": "eq", "right": operand}),
                ),
            ),
            (
                "no_match subject",
                json!({"kind": "no_match", "subject": operand, "patterns": [int(2)]}),
            ),
        ]
    }

    /// Citing rules of `relation_membership_slot_entity_mismatch` reports on
    /// `head`, for a program whose derived relation `heads` keeps the members
    /// of a household (current ID, slot 1) that satisfy `predicate`, which a
    /// Household rule `n` counts, plus the given derived rules. `head` is
    /// declared `[Person, Household]`, so slots (1, 0) agree with the
    /// predicate's context and (0, 1) contradict it.
    fn head_mismatches(predicate: Value, derived: Value) -> Vec<String> {
        let mut derived = derived.as_array().cloned().unwrap_or_default();
        derived.push(json!({
            "name": "n", "entity": "Household", "dtype": "integer",
            "semantics": "scalar",
            "expr": {"kind": "count_related", "relation": "heads",
                     "current_slot": 1, "related_slot": 0}
        }));
        let spec: ProgramSpec = serde_json::from_value(json!({
            "relations": [
                {"name": "member", "arity": 2, "slot_entities": ["Person", "Household"]},
                {"name": "head", "arity": 2, "slot_entities": ["Person", "Household"]},
                {
                    "name": "heads",
                    "arity": 2,
                    "slot_entities": ["Person", "Household"],
                    "derivation": {
                        "source_relation": "member",
                        "current_slot": 1,
                        "related_slot": 0,
                        "slot_entities": ["Person", "Household"],
                        "predicate": predicate
                    }
                }
            ],
            "derived": derived
        }))
        .expect("program spec parses");
        let program = spec.to_program().expect("program builds");
        let mut citing = check_program(&program)
            .err()
            .map(|report| report.violations)
            .unwrap_or_default()
            .into_iter()
            .filter(|violation| {
                violation.relation == "head"
                    && violation.code == RelationTypingCode::MembershipSlotEntityMismatch
            })
            .map(|violation| violation.citing)
            .collect::<Vec<_>>();
        citing.dedup();
        citing
    }

    #[test]
    fn membership_keeps_derived_relation_context_in_every_same_id_position() {
        // Slots agreeing with the context pass; contradicting ones are caught,
        // which shows the context reached the membership test.
        let agreeing = indicator(member(1, 0));
        let contradicting = indicator(member(0, 1));
        assert!(head_mismatches(equals_one(agreeing.clone()), json!([])).is_empty());
        assert_eq!(
            head_mismatches(equals_one(contradicting.clone()), json!([])),
            ["heads"]
        );
        for (operand, expected) in [(&agreeing, vec![]), (&contradicting, vec!["heads"])] {
            for (inner_name, inner) in same_id_positions(operand) {
                for (outer_name, outer) in same_id_positions(&inner) {
                    assert_eq!(
                        head_mismatches(equals_one(outer), json!([])),
                        expected,
                        "{outer_name} holding {inner_name}"
                    );
                }
            }
        }
    }

    #[test]
    fn membership_outside_a_derived_relation_predicate_is_not_checked_against_its_context() {
        // Slots (0, 1) under a Household owner: taking the owner as the current
        // kind would record `[Household, Person]`, the reverse of the
        // declaration, so a leaked usage cannot pass for the declared one.
        let operand = indicator(member(0, 1));
        let always = equals_one(int(1));
        for (name, scalar) in same_id_positions(&operand)
            .into_iter()
            .chain([("bare", operand.clone())])
        {
            let rule = json!([{
                "name": "headed",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": scalar
            }]);
            assert_eq!(
                head_mismatches(always.clone(), rule),
                Vec::<String>::new(),
                "rule, {name}"
            );

            let rule_where = json!([{
                "name": "headed",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {
                    "kind": "count_related",
                    "relation": "member",
                    "current_slot": 1,
                    "related_slot": 0,
                    "where": equals_one(scalar.clone())
                }
            }]);
            assert_eq!(
                head_mismatches(always.clone(), rule_where),
                Vec::<String>::new(),
                "rule where, {name}"
            );

            // A nested aggregation's `where` clause runs on the aggregation's
            // related IDs with no relation context, even inside a predicate.
            let predicate_where = equals_one(json!({
                "kind": "sum_related",
                "relation": "member",
                "current_slot": 0,
                "related_slot": 1,
                "value": {"kind": "input", "name": "hh_size"},
                "where": equals_one(scalar)
            }));
            assert_eq!(
                head_mismatches(predicate_where, json!([])),
                Vec::<String>::new(),
                "predicate where, {name}"
            );
        }
        let judgment_rule = json!([{
            "name": "headed",
            "entity": "Household",
            "dtype": "judgment",
            "semantics": "judgment",
            "expr": member(0, 1)
        }]);
        assert_eq!(
            head_mismatches(always, judgment_rule),
            Vec::<String>::new(),
            "judgment rule"
        );
    }

    #[test]
    fn membership_in_operands_no_predicate_evaluates_is_not_checked_against_its_context() {
        // `no_match` patterns only label an error, and no evaluator runs a
        // reduction inside a predicate. Slots (0, 1) under the predicate's
        // context would record `[Household, Person]` if either leaked.
        let operand = indicator(member(0, 1));
        let pattern = json!({"kind": "no_match", "subject": int(0), "patterns": [operand]});
        let unselected = json!({
            "kind": "if", "condition": equals_one(int(1)), "then_expr": int(1), "else_expr": pattern
        });
        assert!(head_mismatches(equals_one(unselected), json!([])).is_empty());
        for reduction in [
            json!({"kind": "over_periods", "over": "sum", "value": operand}),
            json!({"kind": "over_periods", "over": "sum_top_n", "value": int(1), "n": operand}),
        ] {
            assert!(head_mismatches(equals_one(reduction), json!([])).is_empty());
        }
        // A rule-level reduction still runs its aggregations in lifetime mode,
        // so an untyped relation there is refused.
        let spec: ProgramSpec = serde_json::from_value(json!({
            "relations": [{"name": "member", "arity": 2}],
            "derived": [{
                "name": "members_over_time",
                "entity": "Household",
                "dtype": "integer",
                "semantics": "scalar",
                "expr": {"kind": "over_periods", "over": "sum", "value": {
                    "kind": "count_related", "relation": "member", "current_slot": 1, "related_slot": 0
                }}
            }]
        }))
        .expect("program spec parses");
        let program = spec.to_program().expect("program builds");
        let report = check_program(&program).expect_err("an untyped relation is refused");
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.relation == "member"
                    && violation.code == RelationTypingCode::UntypedRelation),
            "aggregations under a rule-level reduction are executable uses: {report}"
        );
    }

    #[test]
    fn membership_beside_a_nested_aggregation_keeps_the_outer_context() {
        // The aggregation resets context for its own clause only; a sibling
        // operand of the same comparison still runs in the derived relation.
        let predicate = |nested: Value, sibling: Value| {
            json!({
                "kind": "comparison",
                "left": {"kind": "add", "items": [
                    {"kind": "count_related", "relation": "member",
                     "current_slot": 0, "related_slot": 1, "where": nested},
                    indicator(sibling)
                ]},
                "op": "gt",
                "right": int(0)
            })
        };
        // The nested clause has no context, so contradicting slots there pass.
        assert!(head_mismatches(predicate(member(0, 1), member(1, 0)), json!([])).is_empty());
        // The sibling keeps it, so contradicting slots there are caught.
        assert_eq!(
            head_mismatches(predicate(member(1, 0), member(0, 1)), json!([])),
            ["heads"]
        );
    }
}

/// Random programs for checking chain resolution against the per-relation
/// walks it replaced, shared with `migrate`'s tests.
#[cfg(test)]
pub(crate) mod chain_test_programs {
    use super::*;
    use crate::model::{ComparisonOp, RelationDerivation, ScalarValue};
    use proptest::collection::vec;
    use proptest::prelude::*;

    /// Kinds that are also filtered entity names, so filters of filters, and
    /// cycles of them, are common.
    const KINDS: [&str; 5] = ["Household", "Person", "U0", "U1", "U2"];

    fn kinds() -> impl Strategy<Value = Vec<String>> + Clone {
        prop_oneof![
            3 => Just(Vec::new()),
            4 => vec(prop::sample::select(KINDS.to_vec()).prop_map(str::to_string), 0..=3),
        ]
    }

    fn relation(index: usize, count: usize) -> impl Strategy<Value = RelationSchema> {
        // Sources lean toward the next relation, so chains are long, and
        // reach back often enough to close cycles and share tails; a few
        // name nothing the program declares.
        let source = prop_oneof![
            4 => Just(format!("r{}", index + 1)),
            3 => (0..count).prop_map(|source| format!("r{source}")),
            1 => Just("ghost".to_string()),
        ];
        let entity = prop::option::of(prop::sample::select(vec!["U0", "U1", "U2", "Household"]));
        let derivation = (source, 0_usize..3, 0_usize..3, entity, kinds()).prop_map(
            |(source_relation, current_slot, related_slot, entity, slot_entities)| {
                RelationDerivation {
                    source_relation: Some(source_relation),
                    source_path: None,
                    current_slot,
                    related_slot,
                    entity: entity.map(str::to_string),
                    member_relation: None,
                    slot_entities,
                    predicate: JudgmentExpr::Comparison {
                        left: ScalarExpr::Literal(ScalarValue::Integer(1)),
                        op: ComparisonOp::Eq,
                        right: ScalarExpr::Literal(ScalarValue::Integer(1)),
                    },
                }
            },
        );
        (
            0_usize..4,
            kinds(),
            prop::option::weighted(0.75, derivation),
        )
            .prop_map(move |(arity, slot_entities, derivation)| RelationSchema {
                name: format!("r{index}"),
                arity,
                slot_entities,
                derivation,
            })
    }

    /// Relations `r0`..`r{n-1}`, inserted directly so that declarations
    /// whose kind count disagrees with the arity are generated too.
    pub(crate) fn program() -> impl Strategy<Value = Program> {
        (1_usize..10)
            .prop_flat_map(|count| {
                (0..count)
                    .map(|index| relation(index, count))
                    .collect::<Vec<_>>()
            })
            .prop_map(|relations| {
                let mut program = Program::default();
                for relation in relations {
                    program.relations.insert(relation.name.clone(), relation);
                }
                program
            })
    }

    /// A relation a rule or predicate names: usually the program's, now
    /// and then one it does not declare.
    fn relation_named(index: usize, count: usize) -> String {
        if index < count {
            format!("r{index}")
        } else {
            "ghost".to_string()
        }
    }

    /// [`program`] with rules that count, sum over, and test membership in
    /// its relations, and derivation predicates that read rules, test
    /// membership and count, so typing reaches derived relations through
    /// every kind of use and checks them transitively.
    pub(crate) fn program_with_rules() -> impl Strategy<Value = Program> {
        let entity = prop::sample::select(vec![
            "Household",
            "Person",
            "U0",
            crate::model::SCALAR_ENTITY,
        ]);
        let rule = (
            entity,
            0_usize..12,
            0_usize..3,
            0_usize..3,
            prop::option::of(0_usize..5),
            0_usize..3,
        );
        let predicate = (
            0_usize..12,
            0_usize..12,
            0_usize..3,
            0_usize..3,
            0_usize..5,
            0_usize..4,
        );
        (program(), vec(rule, 0..6), vec(predicate, 0..10)).prop_map(
            |(mut program, rules, predicates)| {
                let count = program.relations.len();
                let literal = |value| ScalarExpr::Literal(ScalarValue::Integer(value));
                let rule_names = rules.len().max(1);
                let rule_ref = move |index: usize| format!("x{}", index % rule_names);
                for (index, (entity, relation, current_slot, related_slot, read, shape)) in
                    rules.into_iter().enumerate()
                {
                    let relation = relation_named(relation, count);
                    let where_clause =
                        read.map(|read| Box::new(JudgmentExpr::Derived(rule_ref(read))));
                    let expr = match shape {
                        0 => ScalarExpr::CountRelated {
                            relation,
                            current_slot,
                            related_slot,
                            where_clause,
                        },
                        1 => ScalarExpr::SumRelated {
                            relation,
                            current_slot,
                            related_slot,
                            value: crate::model::RelatedValueRef::Derived(rule_ref(index + 1)),
                            where_clause,
                        },
                        _ => ScalarExpr::If {
                            condition: Box::new(JudgmentExpr::RelationMember {
                                relation,
                                current_slot,
                                related_slot,
                            }),
                            then_expr: Box::new(literal(1)),
                            else_expr: Box::new(literal(0)),
                        },
                    };
                    let name = format!("x{index}");
                    program.derived.insert(
                        name.clone(),
                        crate::model::Derived {
                            id: None,
                            name,
                            entity: entity.to_string(),
                            dtype: crate::model::DType::Integer,
                            unit: None,
                            rounding: None,
                            source: None,
                            source_url: None,
                            corpus_citation_path: None,
                            semantics: crate::model::DerivedSemantics::Scalar(expr),
                            versions: Vec::new(),
                        },
                    );
                }
                for (target, other, current_slot, related_slot, read, shape) in predicates {
                    let Some(derivation) = program
                        .relations
                        .get_mut(&format!("r{target}"))
                        .and_then(|relation| relation.derivation.as_mut())
                    else {
                        continue;
                    };
                    let relation = relation_named(other, count);
                    let member = JudgmentExpr::RelationMember {
                        relation: relation.clone(),
                        current_slot,
                        related_slot,
                    };
                    derivation.predicate = match shape {
                        0 => member,
                        1 => JudgmentExpr::Derived(rule_ref(read)),
                        2 => JudgmentExpr::Comparison {
                            left: ScalarExpr::CountRelated {
                                relation,
                                current_slot,
                                related_slot,
                                where_clause: None,
                            },
                            op: ComparisonOp::Gte,
                            right: literal(1),
                        },
                        _ => JudgmentExpr::And(vec![member, JudgmentExpr::Derived(rule_ref(read))]),
                    };
                }
                program
            },
        )
    }

    /// The recursive lookup [`EffectiveSlotEntities`] and the iterative
    /// [`effective_slot_entities`] replaced, kept as the reference.
    pub(crate) fn reference_effective_slot_entities(
        program: &Program,
        relation: &str,
    ) -> Option<Vec<String>> {
        fn inner(
            program: &Program,
            relation: &str,
            visited: &mut BTreeSet<String>,
        ) -> Option<Vec<String>> {
            if !visited.insert(relation.to_string()) {
                return None;
            }
            let schema = program.relations.get(relation)?;
            if let Some(derivation) = &schema.derivation {
                if !derivation.slot_entities.is_empty() {
                    return Some(derivation.slot_entities.clone());
                }
                if !schema.slot_entities.is_empty() {
                    return Some(schema.slot_entities.clone());
                }
                return inner(program, derivation.source_relation.as_deref()?, visited);
            }
            (!schema.slot_entities.is_empty()).then(|| schema.slot_entities.clone())
        }
        inner(program, relation, &mut BTreeSet::new())
    }
}

#[cfg(test)]
mod chain_resolution_tests {
    use super::chain_test_programs::{program, reference_effective_slot_entities};
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    use std::cell::Cell;

    fn runner(cases: u32) -> TestRunner {
        TestRunner::new_with_rng(
            Config {
                cases,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[11; 32]),
        )
    }

    /// The per-entity alias walk [`innermost_kinds`] replaced, in migration's
    /// form, where an entity's kind may be unsettled.
    fn reference_innermost_kinds(
        direct: &BTreeMap<String, Option<String>>,
    ) -> BTreeMap<String, String> {
        direct
            .keys()
            .filter_map(|entity| {
                let mut kind = entity.clone();
                let mut visited = BTreeSet::new();
                loop {
                    let Some(entry) = direct.get(&kind) else {
                        return Some((entity.clone(), kind));
                    };
                    let next = entry.clone()?;
                    if next == kind {
                        return Some((entity.clone(), kind));
                    }
                    if !visited.insert(kind.clone()) {
                        return None;
                    }
                    kind = next;
                }
            })
            .collect()
    }

    /// Typing's per-entity alias walk, over settled kinds only.
    fn reference_resolve_filtered_kinds(
        known: BTreeMap<String, BTreeSet<String>>,
    ) -> BTreeMap<String, String> {
        let direct = known
            .into_iter()
            .filter(|(_, kinds)| kinds.len() == 1)
            .filter_map(|(entity, kinds)| kinds.into_iter().next().map(|kind| (entity, kind)))
            .collect::<BTreeMap<_, _>>();
        direct
            .keys()
            .filter_map(|entity| {
                let mut kind = entity.clone();
                let mut visited = BTreeSet::new();
                while let Some(next) = direct.get(&kind) {
                    if next == &kind {
                        break;
                    }
                    if !visited.insert(kind.clone()) {
                        return None;
                    }
                    kind = next.clone();
                }
                Some((entity.clone(), kind))
            })
            .collect()
    }

    /// Every derived relation's filtered entity, with the current kinds and
    /// the unresolved entities the per-relation lookups found.
    fn reference_current_kinds(
        program: &Program,
    ) -> (BTreeMap<String, BTreeSet<String>>, BTreeSet<String>) {
        let mut known = BTreeMap::<String, BTreeSet<String>>::new();
        let mut seen = BTreeSet::new();
        for (name, schema) in &program.relations {
            let Some(derivation) = &schema.derivation else {
                continue;
            };
            let Some(entity) = derivation.entity.as_ref() else {
                continue;
            };
            seen.insert(entity.clone());
            if let Some(kind) = reference_effective_slot_entities(program, name)
                .and_then(|kinds| kinds.get(derivation.current_slot).cloned())
            {
                known.entry(entity.clone()).or_default().insert(kind);
            }
        }
        let unresolved = seen
            .into_iter()
            .filter(|entity| !known.contains_key(entity))
            .collect();
        (known, unresolved)
    }

    #[test]
    fn slot_kind_table_matches_the_recursive_lookup() {
        let (inherited, cyclic, unsettled) =
            (Cell::new(0_usize), Cell::new(0_usize), Cell::new(0_usize));
        runner(4000)
            .run(&program(), |program| {
                let table = EffectiveSlotEntities::new(&program);
                let names = program
                    .relations
                    .keys()
                    .cloned()
                    .chain(["ghost".to_string(), "r99".to_string()]);
                for name in names {
                    let expected = reference_effective_slot_entities(&program, &name);
                    let from_table = table.get(&name).map(<[String]>::to_vec);
                    let walked = effective_slot_entities(&program, &name);
                    if from_table != expected || walked != expected {
                        return Err(TestCaseError::fail(format!(
                            "{name}: table {from_table:?}, walk {walked:?}, reference {expected:?}\n{program:#?}"
                        )));
                    }
                    let Some(schema) = program.relations.get(&name) else {
                        continue;
                    };
                    let Some(derivation) = &schema.derivation else {
                        continue;
                    };
                    if derivation.slot_entities.is_empty() && schema.slot_entities.is_empty() {
                        let counter = if expected.is_some() {
                            &inherited
                        } else if inherits_into_a_cycle(&program, &name) {
                            &cyclic
                        } else {
                            &unsettled
                        };
                        counter.set(counter.get() + 1);
                    }
                }
                Ok(())
            })
            .unwrap();
        // The generator must reach kinds through derived relations that
        // declare none, through chains that cycle, and through chains that
        // end at a relation declaring nothing or none at all.
        let (inherited, cyclic, unsettled) = (inherited.get(), cyclic.get(), unsettled.get());
        assert!(inherited > 2000, "only {inherited} inherited lookups");
        assert!(cyclic > 300, "only {cyclic} chains into a cycle");
        assert!(unsettled > 1000, "only {unsettled} chains ending unsettled");
    }

    /// Whether following `relation`'s inherited kinds returns to a relation
    /// it passed, as the reference lookup finds a cycle.
    fn inherits_into_a_cycle(program: &Program, relation: &str) -> bool {
        let mut visited = BTreeSet::new();
        let mut current = relation;
        loop {
            if !visited.insert(current) {
                return true;
            }
            match program.relations.get(current).map(own_slot_entities) {
                Some(OwnSlotEntities::Inherited(source)) => current = source,
                _ => return false,
            }
        }
    }

    #[test]
    fn filtered_entities_match_the_per_relation_lookups() {
        let aliased = Cell::new(0_usize);
        runner(4000)
            .run(&program(), |program| {
                let (known, unresolved) = reference_current_kinds(&program);
                let kinds = reference_resolve_filtered_kinds(known.clone());
                aliased.set(
                    aliased.get()
                        + kinds
                            .iter()
                            .filter(|(entity, kind)| {
                                // Resolved through another filter: the kind
                                // reached is not the entity's own one.
                                known.get(*entity).is_some_and(|own| !own.contains(*kind))
                            })
                            .count(),
                );
                let checks = [
                    (
                        format!("{:?}", filtered_entity_kinds(&program)),
                        format!("{kinds:?}"),
                    ),
                    (
                        format!("{:?}", filtered_entity_names(&program)),
                        format!("{:?}", alias_names(known)),
                    ),
                    (
                        format!("{:?}", unresolved_filtered_entities(&program)),
                        format!("{unresolved:?}"),
                    ),
                ];
                for (actual, expected) in checks {
                    if actual != expected {
                        return Err(TestCaseError::fail(format!(
                            "actual {actual}\nreference {expected}\n{program:#?}"
                        )));
                    }
                }
                Ok(())
            })
            .unwrap();
        let aliased = aliased.get();
        assert!(aliased > 100, "only {aliased} filters of filters resolved");
    }

    #[test]
    fn innermost_kinds_match_the_per_entity_walk() {
        let names = || prop::sample::select(vec!["a", "b", "c", "d", "e", "f", "g"]);
        let direct = prop::collection::btree_map(
            names().prop_map(str::to_string),
            prop::option::weighted(0.85, names().prop_map(str::to_string)),
            0..7,
        );
        let (cycles, deep) = (Cell::new(0_usize), Cell::new(0_usize));
        runner(6000)
            .run(&direct, |direct| {
                let expected = reference_innermost_kinds(&direct);
                // Entities whose walk returns to an entity it passed, not
                // those that merely reach an unsettled kind.
                let cyclic = direct
                    .keys()
                    .filter(|entity| {
                        let mut visited = BTreeSet::new();
                        let mut kind = entity.as_str();
                        while let Some(Some(next)) = direct.get(kind) {
                            if next == kind {
                                return false;
                            }
                            if !visited.insert(kind) {
                                return true;
                            }
                            kind = next;
                        }
                        false
                    })
                    .count();
                cycles.set(cycles.get() + cyclic);
                deep.set(
                    deep.get()
                        + expected
                            .iter()
                            .filter(|(entity, kind)| {
                                direct.get(*entity).and_then(Option::as_deref)
                                    != Some(kind.as_str())
                                    && entity != kind
                            })
                            .count(),
                );
                let actual = innermost_kinds(&direct);
                if actual != expected {
                    return Err(TestCaseError::fail(format!(
                        "actual {actual:?}\nreference {expected:?}\ndirect {direct:?}"
                    )));
                }
                Ok(())
            })
            .unwrap();
        let (cycles, deep) = (cycles.get(), deep.get());
        assert!(
            cycles > 400 && deep > 1500,
            "{cycles} in cycles, {deep} deep"
        );
    }

    /// The fixed-point loop the worklist replaced: rescan every used
    /// relation for ones not yet checked until none is left.
    fn check_used_relations_by_rescanning(checker: &mut Checker<'_>) {
        let mut checked = BTreeSet::new();
        loop {
            let pending = checker
                .used_relations
                .iter()
                .filter(|name| !checked.contains(*name))
                .cloned()
                .collect::<Vec<_>>();
            if pending.is_empty() {
                break;
            }
            for name in pending {
                checked.insert(name.clone());
                checker.derived_relation(name);
            }
        }
        checker.unchecked.clear();
    }

    #[test]
    fn the_worklist_checks_what_the_fixed_point_loop_checked() {
        let (reached, violations) = (Cell::new(0_usize), Cell::new(0_usize));
        runner(3000)
            .run(&super::chain_test_programs::program_with_rules(), |program| {
                for typing in [true, false] {
                    let new = |program| {
                        if typing {
                            Checker::for_typing(program)
                        } else {
                            Checker::for_execution(program)
                        }
                    };
                    let (mut worklist, mut rescanning) = (new(&program), new(&program));
                    if typing {
                        worklist.filtered_entity_conflicts();
                        rescanning.filtered_entity_conflicts();
                    }
                    worklist.walk();
                    rescanning.walk_rules();
                    check_used_relations_by_rescanning(&mut rescanning);
                    if worklist.used_relations != rescanning.used_relations
                        || worklist.violations != rescanning.violations
                    {
                        return Err(TestCaseError::fail(format!(
                            "typing {typing}: used {:?} != {:?}\nviolations {:#?}\n!= {:#?}\n{program:#?}",
                            worklist.used_relations,
                            rescanning.used_relations,
                            worklist.violations,
                            rescanning.violations
                        )));
                    }
                    if typing {
                        // Derived relations reached only through another
                        // relation's source or predicate, not a rule.
                        let mut direct = Checker::for_execution(&program);
                        direct.walk_rules();
                        reached.set(
                            reached.get()
                                + worklist
                                    .used_relations
                                    .difference(&direct.used_relations)
                                    .count(),
                        );
                        violations.set(violations.get() + worklist.violations.len());
                    }
                }
                Ok(())
            })
            .unwrap();
        let (reached, violations) = (reached.get(), violations.get());
        assert!(
            reached > 1000,
            "only {reached} relations reached transitively"
        );
        assert!(violations > 3000, "only {violations} violations");
    }

    #[test]
    fn follow_chains_matches_a_walk_from_each_start() {
        // Each node ends its chain, defers to another node, or (when absent
        // from the map) ends with its own name.
        let names = || prop::sample::select(vec!["a", "b", "c", "d", "e", "f"]);
        let graph = prop::collection::btree_map(
            names(),
            prop_oneof![1 => Just(None), 3 => names().prop_map(Some)],
            0..6,
        );
        runner(6000)
            .run(&graph, |graph| {
                let step = |node: &'static str| match graph.get(node) {
                    None => ChainStep::End(Some(node)),
                    Some(None) => ChainStep::End(None),
                    Some(Some(next)) => ChainStep::Next(*next),
                };
                let starts = ["a", "b", "c", "d", "e", "f", "g"];
                let resolved = follow_chains(starts, None, step);
                for start in starts {
                    let mut visited = BTreeSet::new();
                    let mut node = start;
                    let expected = loop {
                        if !visited.insert(node) {
                            break None;
                        }
                        match step(node) {
                            ChainStep::End(result) => break result,
                            ChainStep::Next(next) => node = next,
                        }
                    };
                    if resolved.get(start).copied() != Some(expected) {
                        return Err(TestCaseError::fail(format!(
                            "{start}: {:?} != {expected:?} in {graph:?}",
                            resolved.get(start)
                        )));
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn source_graph_visits_long_chains_and_shared_tails_once() {
        let names = (0..50_000)
            .map(|index| format!("r{index:07}"))
            .collect::<Vec<_>>();
        for branching in [false, true] {
            let mut calls = 0;
            let resolved = all_source_edges(names.iter().map(String::as_str), |name| {
                calls += 1;
                let index = name[1..].parse::<usize>().unwrap();
                let Some(next) = names.get(index + 1) else {
                    return Some(Vec::new());
                };
                Some(if branching {
                    vec![next.as_str(), names.last().unwrap().as_str(), next.as_str()]
                } else {
                    vec![next.as_str()]
                })
            });
            assert_eq!(calls, names.len());
            assert_eq!(resolved.len(), names.len());
            assert!(resolved.values().all(|valid| *valid));
        }
    }

    #[test]
    fn source_graph_matches_independent_reachability_and_cycle_oracle() {
        let names = || prop::sample::select(vec!["a", "b", "c", "d", "e", "f"]);
        let graph = prop::collection::btree_map(
            names(),
            (any::<bool>(), prop::collection::vec(names(), 0..=4)),
            0..=6,
        );
        let (branched, long_paths, cyclic, settled, leaves) = (
            Cell::new(0_usize),
            Cell::new(0_usize),
            Cell::new(0_usize),
            Cell::new(0_usize),
            Cell::new(0_usize),
        );
        runner(2000)
            .run(&graph, |graph| {
                let starts = ["a", "b", "c", "d", "e", "f", "g"];
                let mut visits = BTreeMap::new();
                SOURCE_GRAPH_WORK.with(|work| work.set([0; 4]));
                let actual = all_source_edges(starts, |node| {
                    *visits.entry(node).or_insert(0_usize) += 1;
                    let Some((valid, edges)) = graph.get(node) else {
                        return Some(Vec::new());
                    };
                    if *valid { Some(edges.clone()) } else { None }
                });
                SOURCE_GRAPH_WORK.with(|work| {
                    let counts = work.get();
                    leaves.set(leaves.get() + counts[0]);
                    branched.set(branched.get() + counts[1]);
                    long_paths.set(long_paths.get() + counts[2]);
                    cyclic.set(cyclic.get() + counts[3]);
                });
                assert!(visits.values().all(|visits| *visits == 1));
                for start in starts {
                    // Independent oracle: collect the reachable subgraph, then
                    // remove its leaves. A remaining node belongs to, or reaches,
                    // a cycle. This does no recursive or memoized source walk.
                    let mut reachable = BTreeSet::new();
                    let mut pending = vec![start];
                    let mut locally_valid = true;
                    while let Some(node) = pending.pop() {
                        if !reachable.insert(node) {
                            continue;
                        }
                        if let Some((valid, edges)) = graph.get(node) {
                            locally_valid &= *valid;
                            if *valid {
                                pending.extend(edges.iter().copied());
                            }
                        }
                    }
                    let mut remaining = reachable.clone();
                    loop {
                        let leaves = remaining
                            .iter()
                            .copied()
                            .filter(|node| {
                                graph.get(node).is_none_or(|(valid, edges)| {
                                    !valid || edges.iter().all(|edge| !remaining.contains(edge))
                                })
                            })
                            .collect::<Vec<_>>();
                        if leaves.is_empty() {
                            break;
                        }
                        for node in leaves {
                            remaining.remove(node);
                        }
                    }
                    let expected = locally_valid && remaining.is_empty();
                    settled.set(settled.get() + usize::from(expected));
                    prop_assert_eq!(actual[start], expected, "{} in {:?}", start, graph);
                }
                Ok(())
            })
            .unwrap();
        let counts = (
            branched.get(),
            long_paths.get(),
            cyclic.get(),
            settled.get(),
            leaves.get(),
        );
        eprintln!(
            "source graph work counts (branching, 3+ edges, cycles, settled, leaves): {counts:?}"
        );
        assert!(
            counts.0 > 1000
                && counts.1 > 700
                && counts.2 > 500
                && counts.3 > 2000
                && counts.4 > 3000,
            "graph coverage {counts:?}"
        );
    }

    #[test]
    fn namesake_index_finds_what_a_suffix_scan_finds() {
        let part = prop::sample::select(vec!["m", "#", "relation.", "x", "y", "#relation.x", ""]);
        let name = prop::collection::vec(part, 1..5).prop_map(|parts| parts.concat());
        let names = prop::collection::btree_set(name.clone(), 0..8);
        runner(4000)
            .run(
                &(names, prop::collection::vec(name, 1..4)),
                |(names, queries)| {
                    let mut program = Program::default();
                    for name in &names {
                        program.relations.insert(
                            name.clone(),
                            RelationSchema {
                                name: name.clone(),
                                arity: 1,
                                slot_entities: Vec::new(),
                                derivation: None,
                            },
                        );
                    }
                    let mut checker = Checker {
                        program: &program,
                        slot_kinds: EffectiveSlotEntities::new(&program),
                        filtered: BTreeMap::new(),
                        filtered_names: BTreeSet::new(),
                        namesakes: None,
                        base_semantics: true,
                        violations: BTreeSet::new(),
                        used_relations: BTreeSet::new(),
                        unchecked: Vec::new(),
                    };
                    let candidates = names
                        .iter()
                        .cloned()
                        .chain(queries)
                        .chain(["x", "y", ""].map(str::to_string));
                    for relation in candidates {
                        // The scan `typed_slots` ran for every untyped relation.
                        let suffix = format!("#relation.{relation}");
                        let mut expected = program
                            .relations
                            .keys()
                            .filter(|name| !relation.contains('#') && name.ends_with(&suffix))
                            .map(|name| format!("`{name}`"))
                            .collect::<Vec<_>>();
                        expected.sort();
                        let actual = checker.namesakes(&relation);
                        if actual != expected {
                            return Err(TestCaseError::fail(format!(
                                "{relation:?}: {actual:?} != {expected:?} among {names:?}"
                            )));
                        }
                    }
                    Ok(())
                },
            )
            .unwrap();
    }
}
