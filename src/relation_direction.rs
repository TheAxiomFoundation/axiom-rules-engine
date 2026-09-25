//! Resolve RuleSpec aggregation direction from explicit entity declarations.
//!
//! Formula lowering has no entity context, so every relation node leaves it
//! with the legacy slots (current 1, related 0). Once relation aliases are
//! rewritten, this pass rewrites those slots from the declared entity kinds:
//! - an aggregate over a two-slot data relation keys on the one slot whose
//!   kind is the enclosing entity; when both slots have that kind the
//!   direction is ambiguous and lowering fails, because guessing would
//!   silently aggregate the wrong side;
//! - an aggregate over a derived relation uses the derivation's own slots,
//!   which are what every runtime traverses;
//! - a membership test inside a derived-relation predicate keys the current
//!   and related ids on the slots of their kinds.
//!
//! Relations with no declared kinds keep the legacy slots here; the
//! mandatory typing check (`relation_typing`) rejects executing them.
use crate::spec::{DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, ScalarExprSpec};
use std::collections::{BTreeSet, HashMap};

/// A same-kind relation read from an entity occupying more than one slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmbiguousRelationDirection {
    pub relation: String,
    pub citing: String,
    pub entity: String,
    pub slot_entities: Vec<String>,
}

#[derive(Clone)]
struct DerivedSlots {
    current_slot: usize,
    related_slot: usize,
    kinds: Option<Vec<String>>,
}

struct Relations {
    typed: HashMap<String, Vec<String>>,
    derived: HashMap<String, DerivedSlots>,
}

pub(crate) fn resolve(program: &mut ProgramSpec) -> Result<()> {
    let relations = relations(program);
    for rule in &mut program.derived {
        let citing = rule.id.clone().unwrap_or_else(|| rule.name.clone());
        let mut resolver = Resolver {
            relations: &relations,
            citing: &citing,
        };
        resolver.semantics(&mut rule.semantics, &rule.entity)?;
        for version in &mut rule.versions {
            resolver.semantics(&mut version.semantics, &rule.entity)?;
        }
    }
    for relation in &mut program.relations {
        let Some(slots) = relations.derived.get(&relation.name).cloned() else {
            continue;
        };
        let Some(derivation) = relation.derivation.as_mut() else {
            continue;
        };
        let kind = |slot: usize| {
            slots
                .kinds
                .as_ref()
                .and_then(|kinds| kinds.get(slot))
                .cloned()
        };
        let current = kind(slots.current_slot);
        let related = kind(slots.related_slot);
        let citing = relation.name.clone();
        let mut resolver = Resolver {
            relations: &relations,
            citing: &citing,
        };
        resolver.judgment(
            &mut derivation.predicate,
            related.as_deref().unwrap_or(""),
            Some((current.as_deref(), related.as_deref())),
        )?;
    }
    Ok(())
}

fn relations(program: &ProgramSpec) -> Relations {
    let declared: HashMap<&str, &crate::spec::RelationSpec> = program
        .relations
        .iter()
        .map(|relation| (relation.name.as_str(), relation))
        .collect();
    let kinds_of = |name: &str| {
        let mut visited = BTreeSet::new();
        let mut name = name.to_string();
        loop {
            if !visited.insert(name.clone()) {
                return None;
            }
            let relation = declared.get(name.as_str())?;
            if let Some(derivation) = &relation.derivation {
                if !derivation.slot_entities.is_empty() {
                    return Some(derivation.slot_entities.clone());
                }
                if !relation.slot_entities.is_empty() {
                    return Some(relation.slot_entities.clone());
                }
                name = derivation.source_relation.clone();
                continue;
            }
            return (!relation.slot_entities.is_empty()).then(|| relation.slot_entities.clone());
        }
    };
    let mut typed = HashMap::new();
    let mut derived = HashMap::new();
    for relation in &program.relations {
        match &relation.derivation {
            Some(derivation) => {
                derived.insert(
                    relation.name.clone(),
                    DerivedSlots {
                        current_slot: derivation.current_slot,
                        related_slot: derivation.related_slot,
                        kinds: kinds_of(&relation.name),
                    },
                );
            }
            None if relation.slot_entities.len() == 2 => {
                typed.insert(relation.name.clone(), relation.slot_entities.clone());
            }
            None => {}
        }
    }
    Relations { typed, derived }
}

struct Resolver<'a> {
    relations: &'a Relations,
    citing: &'a str,
}

type Result<T> = std::result::Result<T, AmbiguousRelationDirection>;

impl Resolver<'_> {
    fn semantics(&mut self, expr: &mut DerivedSemanticsSpec, entity: &str) -> Result<()> {
        match expr {
            DerivedSemanticsSpec::Scalar { expr } => self.scalar(expr, entity),
            DerivedSemanticsSpec::Judgment { expr } => self.judgment(expr, entity, None),
        }
    }

    /// Rewrite an aggregate's slots and return the entity kind of the ids its
    /// predicate is evaluated on.
    fn aggregate(
        &self,
        relation: &str,
        current_slot: &mut usize,
        related_slot: &mut usize,
        entity: &str,
    ) -> Result<String> {
        if let Some(derived) = self.relations.derived.get(relation) {
            *current_slot = derived.current_slot;
            *related_slot = derived.related_slot;
            return Ok(derived
                .kinds
                .as_ref()
                .and_then(|kinds| kinds.get(derived.related_slot))
                .cloned()
                .unwrap_or_default());
        }
        let Some(kinds) = self.relations.typed.get(relation) else {
            return Ok(String::new());
        };
        let matches = kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| kind.as_str() == entity)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => {
                *current_slot = *index;
                *related_slot = 1 - index;
            }
            [_, _] => {
                return Err(AmbiguousRelationDirection {
                    relation: relation.to_string(),
                    citing: self.citing.to_string(),
                    entity: entity.to_string(),
                    slot_entities: kinds.clone(),
                });
            }
            // No slot holds the entity: keep the slots so the typing check
            // reports the mismatch against the declaration.
            _ => {}
        }
        Ok(kinds.get(*related_slot).cloned().unwrap_or_default())
    }

    fn scalar(&mut self, expr: &mut ScalarExprSpec, entity: &str) -> Result<()> {
        match expr {
            ScalarExprSpec::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            }
            | ScalarExprSpec::SumRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
                ..
            } => {
                let related_entity = self.aggregate(relation, current_slot, related_slot, entity)?;
                if let Some(predicate) = where_clause {
                    self.judgment(predicate, &related_entity, None)?;
                }
            }
            ScalarExprSpec::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, entity, None)?;
                self.scalar(then_expr, entity)?;
                self.scalar(else_expr, entity)?;
            }
            ScalarExprSpec::Add { items }
            | ScalarExprSpec::Min { items }
            | ScalarExprSpec::Max { items } => {
                for item in items {
                    self.scalar(item, entity)?;
                }
            }
            ScalarExprSpec::Sub { left, right }
            | ScalarExprSpec::Mul { left, right }
            | ScalarExprSpec::Div { left, right } => {
                self.scalar(left, entity)?;
                self.scalar(right, entity)?;
            }
            ScalarExprSpec::DateAddDays { date, days }
            | ScalarExprSpec::DateAddMonths { date, months: days }
            | ScalarExprSpec::DateAddYears { date, years: days } => {
                self.scalar(date, entity)?;
                self.scalar(days, entity)?;
            }
            ScalarExprSpec::DaysBetween { from, to } => {
                self.scalar(from, entity)?;
                self.scalar(to, entity)?;
            }
            ScalarExprSpec::ParameterLookup { index, .. }
            | ScalarExprSpec::Ceil { value: index }
            | ScalarExprSpec::Floor { value: index } => self.scalar(index, entity)?,
            ScalarExprSpec::OverPeriods { value, n, .. } => {
                self.scalar(value, entity)?;
                if let Some(n) = n {
                    self.scalar(n, entity)?;
                }
            }
            ScalarExprSpec::Literal { .. }
            | ScalarExprSpec::Input { .. }
            | ScalarExprSpec::InputOrElse { .. }
            | ScalarExprSpec::Derived { .. }
            | ScalarExprSpec::PeriodStart
            | ScalarExprSpec::PeriodEnd => {}
        }
        Ok(())
    }

    /// `membership` carries the current and related kinds of a derived
    /// relation's predicate context; it is `None` everywhere else.
    fn judgment(
        &mut self,
        expr: &mut JudgmentExprSpec,
        entity: &str,
        membership: Option<(Option<&str>, Option<&str>)>,
    ) -> Result<()> {
        match expr {
            JudgmentExprSpec::Comparison { left, right, .. } => {
                self.scalar(left, entity)?;
                self.scalar(right, entity)?;
            }
            JudgmentExprSpec::And { items }
            | JudgmentExprSpec::Or { items }
            | JudgmentExprSpec::ExactlyOne { items } => {
                for item in items {
                    self.judgment(item, entity, membership)?;
                }
            }
            JudgmentExprSpec::Not { item } => self.judgment(item, entity, membership)?,
            JudgmentExprSpec::RelationMember {
                relation,
                current_slot,
                related_slot,
            } => {
                let Some((Some(current), Some(related))) = membership else {
                    return Ok(());
                };
                let Some(kinds) = self.relations.typed.get(relation.as_str()) else {
                    return Ok(());
                };
                if current == related {
                    if kinds.iter().all(|kind| kind == current) {
                        return Err(AmbiguousRelationDirection {
                            relation: relation.clone(),
                            citing: self.citing.to_string(),
                            entity: current.to_string(),
                            slot_entities: kinds.clone(),
                        });
                    }
                    return Ok(());
                }
                let slot_of = |kind: &str| {
                    let slots = kinds
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| candidate.as_str() == kind)
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    (slots.len() == 1).then(|| slots[0])
                };
                if let (Some(current_index), Some(related_index)) =
                    (slot_of(current), slot_of(related))
                {
                    *current_slot = current_index;
                    *related_slot = related_index;
                }
            }
            JudgmentExprSpec::Derived { .. } => {}
        }
        Ok(())
    }
}
