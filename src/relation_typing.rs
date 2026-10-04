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

use crate::model::{JudgmentExpr, Program, RelatedValueRef, SCALAR_ENTITY, ScalarExpr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelationTypingCode {
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
}

impl RelationTypingCode {
    pub const fn as_str(self) -> &'static str {
        match self {
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
    let mut checker = Checker {
        program,
        filtered: filtered_entity_kinds(program),
        filtered_names: filtered_entity_names(program),
        violations: BTreeSet::new(),
        used_relations: BTreeSet::new(),
    };
    checker.filtered_entity_conflicts();
    let mut names = program.derived.keys().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let derived = &program.derived[name];
        let citing = derived.id.as_deref().unwrap_or(&derived.name);
        // Explain selects among the versions, but the dense compiler reads the
        // base semantics, so both are checked.
        let semantics = std::iter::once(&derived.semantics)
            .chain(derived.versions.iter().map(|version| &version.semantics));
        for semantics in semantics {
            let context = Context {
                entity: Some(derived.entity.as_str()),
                membership: None,
                citing,
            };
            match semantics {
                crate::model::DerivedSemantics::Scalar(expr) => checker.scalar(expr, context),
                crate::model::DerivedSemantics::Judgment(expr) => checker.judgment(expr, context),
            }
        }
    }
    checker.derived_relations();
    if checker.violations.is_empty() {
        Ok(())
    } else {
        Err(RelationTypingReport {
            violations: checker.violations.into_iter().collect(),
        })
    }
}

/// The entity kind of each tuple slot of `relation`, or `None` when the
/// relation (or, for a derived relation, every relation it filters) declares
/// none. A derived relation's tuples are its source's tuples, so it inherits
/// the source's kinds when it declares none of its own.
pub fn effective_slot_entities(program: &Program, relation: &str) -> Option<Vec<String>> {
    let mut visited = BTreeSet::new();
    effective_slot_entities_inner(program, relation, &mut visited)
}

fn effective_slot_entities_inner(
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
        return effective_slot_entities_inner(program, &derivation.source_relation, visited);
    }
    (!schema.slot_entities.is_empty()).then(|| schema.slot_entities.clone())
}

/// The known current-slot kinds of every derived relation filtering to each
/// entity (a derived relation's `entity`, e.g. `SnapUnit`), and the entities
/// whose derived relations' current kinds are all unknown.
fn filtered_entity_current_kinds(
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
        if let Some(kind) = effective_slot_entities(program, name)
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
    let (known, _) = filtered_entity_current_kinds(program);
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

/// Filtered entities that alias ids of another kind: an entity some derived
/// relation filters to over a source whose current kind is known and is not
/// that entity. Such a name cannot be a relation slot's kind. An entity whose
/// derived relations' current kinds are all unknown is not called an alias,
/// and one whose only known current kind is itself (a filter of households
/// to households) is a physical kind.
pub fn filtered_entity_names(program: &Program) -> BTreeSet<String> {
    let (known, _) = filtered_entity_current_kinds(program);
    known
        .into_iter()
        .filter(|(entity, kinds)| kinds.iter().any(|kind| kind != entity))
        .map(|(entity, _)| entity)
        .collect()
}

/// Filtered entities none of whose derived relations has a known current
/// kind yet: a use naming one says nothing about the ids it reads.
pub fn unresolved_filtered_entities(program: &Program) -> BTreeSet<String> {
    filtered_entity_current_kinds(program).1
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
    /// Filtered entity -> the kind of the source ids it is queried with.
    filtered: BTreeMap<String, String>,
    /// Filtered entities that alias another kind; never a slot kind.
    filtered_names: BTreeSet<String>,
    violations: BTreeSet<RelationTypingViolation>,
    used_relations: BTreeSet<String>,
}

impl<'a> Checker<'a> {
    fn push(&mut self, code: RelationTypingCode, relation: &str, citing: &str, message: String) {
        self.violations.insert(RelationTypingViolation {
            code,
            relation: relation.to_string(),
            citing: citing.to_string(),
            message,
        });
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
            if let Some(kind) = effective_slot_entities(self.program, name)
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
        let Some(kinds) = effective_slot_entities(self.program, relation) else {
            // A short name aggregated by a module that does not declare it is
            // its own relation, distinct from same-named declarations.
            let suffix = format!("#relation.{relation}");
            let mut namesakes = self
                .program
                .relations
                .keys()
                .filter(|name| !relation.contains('#') && name.ends_with(&suffix))
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>();
            namesakes.sort();
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
        self.used_relations.insert(relation.to_string());
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
        let (current, related) = match derivation {
            Some(derivation) => {
                if (current_slot, related_slot)
                    != (derivation.current_slot, derivation.related_slot)
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
                (derivation.current_slot, derivation.related_slot)
            }
            None => (current_slot, related_slot),
        };

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
        self.used_relations.insert(relation.to_string());
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
        let mut checked = BTreeSet::new();
        loop {
            let pending = self
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
                let Some(schema) = self.program.relations.get(&name) else {
                    continue;
                };
                let Some(derivation) = schema.derivation.clone() else {
                    continue;
                };
                let schema_kinds = schema.slot_entities.clone();
                self.used_relations
                    .insert(derivation.source_relation.clone());
                let source_kinds = self.typed_slots(&derivation.source_relation, &name);
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
                                derivation.source_relation,
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
                    .get(&derivation.source_relation)
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
                            derivation.source_relation,
                            derivation.current_slot,
                            derivation.related_slot,
                            source.current_slot,
                            source.related_slot
                        ),
                    );
                }
                self.predicate_rules(&name, &derivation);
                let kinds = effective_slot_entities(self.program, &name);
                let slot = |slot: usize| {
                    kinds
                        .as_ref()
                        .and_then(|kinds| kinds.get(slot))
                        .map(String::as_str)
                };
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
        }
    }

    /// A derived relation's predicate runs each rule it reads on the current
    /// id when the rule's entity is the derivation's declared current kind,
    /// and on the related id otherwise. Report rules that would run on an id
    /// of another kind.
    fn predicate_rules(&mut self, name: &str, derivation: &crate::model::RelationDerivation) {
        let effective = effective_slot_entities(self.program, name);
        let Some(effective_related) = effective
            .as_ref()
            .and_then(|kinds| kinds.get(derivation.related_slot))
            .cloned()
        else {
            return;
        };
        let routed_current = derivation.slot_entities.get(derivation.current_slot);
        let routed_related = derivation.slot_entities.get(derivation.related_slot);
        let mut referenced = BTreeSet::new();
        collect_judgment_derived(&derivation.predicate, &mut referenced);
        for rule_name in referenced {
            let Some(rule) = self.program.derived.get(&rule_name) else {
                continue;
            };
            let entity = rule.entity.as_str();
            if entity == SCALAR_ENTITY
                || routed_current.is_some_and(|kind| kind == entity)
                || routed_related.is_some_and(|kind| kind == entity)
                || entity == effective_related
                || self.id_kind(entity) == effective_related
            {
                continue;
            }
            let hint = if derivation.slot_entities.is_empty() {
                " Declare the derived relation's `slot_entities` so the predicate can read the current id"
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
            | ScalarExpr::Input(_)
            | ScalarExpr::InputOrElse { .. }
            | ScalarExpr::Derived(_)
            | ScalarExpr::PeriodStart
            | ScalarExpr::PeriodEnd => {}
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
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, context);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, context),
            JudgmentExpr::Derived(_) => {}
        }
    }
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
        JudgmentExpr::RelationMember { .. } => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_derived(item, out);
            }
        }
        JudgmentExpr::Not(item) => collect_judgment_derived(item, out),
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
        ScalarExpr::CountRelated { .. }
        | ScalarExpr::SumRelated { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
    }
}

fn format_kinds(kinds: &[String]) -> String {
    format!("[{}]", kinds.join(", "))
}
