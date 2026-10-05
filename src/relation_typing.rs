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
        base_semantics: true,
        violations: BTreeSet::new(),
        used_relations: BTreeSet::new(),
    };
    checker.filtered_entity_conflicts();
    checker.walk();
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
    let mut checker = Checker {
        program,
        filtered: BTreeMap::new(),
        filtered_names: BTreeSet::new(),
        base_semantics: false,
        violations: BTreeSet::new(),
        used_relations: BTreeSet::new(),
    };
    checker.walk();
    checker.used_relations
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
    resolve_filtered_kinds(filtered_entity_current_kinds(program, None).0)
}

/// [`filtered_entity_kinds`] from only the derived relations in `among`.
pub fn filtered_entity_kinds_among(
    program: &Program,
    among: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    resolve_filtered_kinds(filtered_entity_current_kinds(program, Some(among)).0)
}

fn resolve_filtered_kinds(known: BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, String> {
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
    alias_names(filtered_entity_current_kinds(program, None).0)
}

/// [`filtered_entity_names`] from only the derived relations in `among`.
pub fn filtered_entity_names_among(
    program: &Program,
    among: &BTreeSet<String>,
) -> BTreeSet<String> {
    alias_names(filtered_entity_current_kinds(program, Some(among)).0)
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
    filtered_entity_current_kinds(program, None).1
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
    /// Filtered entity -> the kind of the source ids it is queried with.
    filtered: BTreeMap<String, String>,
    /// Filtered entities that alias another kind; never a slot kind.
    filtered_names: BTreeSet<String>,
    /// Whether a versioned rule's base semantics is walked too.
    base_semantics: bool,
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

    /// Visit every executed node of every derived rule, then the derived
    /// relations they reach.
    fn walk(&mut self) {
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
        self.derived_relations();
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
