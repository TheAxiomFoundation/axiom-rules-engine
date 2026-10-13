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
//! - a filtered entity (a derived relation's `entity`, e.g. `SnapUnit`) is
//!   queried with its source's current-slot ids, so it keys on that kind;
//! - a membership test inside a derived-relation predicate keys the current
//!   and related ids on the slots of their kinds.
//!
//! A derived relation that leaves its slots to the legacy default over a
//! source whose two slots share a kind has no readable direction, and
//! lowering fails. Over a source with distinct kinds, the derivation is
//! stamped with the source's kinds, so the runtimes route each rule its
//! predicate reads to the id of that rule's entity.
//!
//! Relations with no declared kinds keep the legacy slots here; the
//! mandatory typing check (`relation_typing`) rejects executing them.
use crate::relation_typing::ChainStep;
use crate::spec::{DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, ScalarExprSpec};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A direction lowering cannot read from declared kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DirectionError {
    /// A same-kind relation read from an entity occupying more than one slot.
    Aggregate {
        relation: String,
        citing: String,
        entity: String,
        slot_entities: Vec<String>,
    },
    /// A derived relation that omits its slots over a source whose defaulted
    /// slots share a kind.
    DerivedSlots {
        relation: String,
        slot_entities: Vec<String>,
    },
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
    /// Filtered entity -> the kind of the source ids it is queried with, when
    /// every derived relation filtering to that entity agrees.
    filtered: HashMap<String, String>,
}

/// `defaulted_slots` names the derived relations whose `current_slot` or
/// `related_slot` the source left to the legacy default.
pub(crate) fn resolve(program: &mut ProgramSpec, defaulted_slots: &HashSet<String>) -> Result<()> {
    let relations = relations(program);
    let mut names = defaulted_slots.iter().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let Some(slots) = relations.derived.get(name) else {
            continue;
        };
        if let Some(kinds) = &slots.kinds
            && let (Some(current), Some(related)) =
                (kinds.get(slots.current_slot), kinds.get(slots.related_slot))
            && current == related
        {
            return Err(DirectionError::DerivedSlots {
                relation: name.clone(),
                slot_entities: kinds.clone(),
            });
        }
    }
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
        // Runtimes route each rule a predicate reads by the derivation's own
        // kinds; with none, every rule runs on the related id. Stamp the
        // source's kinds so a rule of the current entity reads the current
        // id. A same-kind source stays unstamped: stamping would move every
        // rule of that kind onto the current id.
        if derivation.slot_entities.is_empty()
            && let Some(kinds) = &slots.kinds
            && current.is_some()
            && current != related
        {
            derivation.slot_entities = kinds.clone();
            if relation.slot_entities.is_empty() {
                relation.slot_entities = kinds.clone();
            }
        }
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
    // A derived relation declaring no kinds has its source's. Resolve every
    // relation once: asking per relation would walk a shared chain again from
    // each relation on it.
    let effective =
        crate::relation_typing::follow_chains(
            declared.keys().copied(),
            None,
            |name| match declared.get(name) {
                None => ChainStep::End(None),
                Some(relation) => match &relation.derivation {
                    Some(derivation) if !derivation.slot_entities.is_empty() => {
                        ChainStep::End(Some(derivation.slot_entities.as_slice()))
                    }
                    Some(derivation) if relation.slot_entities.is_empty() => {
                        ChainStep::Next(&derivation.source_relation)
                    }
                    _ => ChainStep::End(
                        (!relation.slot_entities.is_empty())
                            .then_some(relation.slot_entities.as_slice()),
                    ),
                },
            },
        );
    let kinds_of = |name: &str| {
        effective
            .get(name)
            .copied()
            .flatten()
            .map(<[String]>::to_vec)
    };
    let mut typed = HashMap::new();
    let mut derived = HashMap::new();
    let mut filtered = BTreeMap::<String, BTreeSet<String>>::new();
    for relation in &program.relations {
        match &relation.derivation {
            Some(derivation) => {
                if let Some(entity) = &derivation.entity
                    && let Some(kind) = kinds_of(&relation.name)
                        .and_then(|kinds| kinds.get(derivation.current_slot).cloned())
                {
                    filtered.entry(entity.clone()).or_default().insert(kind);
                }
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
    let filtered = filtered
        .into_iter()
        .filter(|(_, kinds)| kinds.len() == 1)
        .filter_map(|(entity, kinds)| kinds.into_iter().next().map(|kind| (entity, kind)))
        .collect();
    Relations {
        typed,
        derived,
        filtered,
    }
}

struct Resolver<'a> {
    relations: &'a Relations,
    citing: &'a str,
}

type Result<T> = std::result::Result<T, DirectionError>;

/// The current and related kinds a derived relation's predicate binds while
/// it runs; `None` outside that scope. As in explain, the binding reaches
/// through comparisons, conditions and branches, and arithmetic, but not into
/// a nested aggregation's `where`, a match fallback's patterns, or an
/// over-periods reduction.
type Membership<'a> = Option<(Option<&'a str>, Option<&'a str>)>;

impl Resolver<'_> {
    fn semantics(&mut self, expr: &mut DerivedSemanticsSpec, entity: &str) -> Result<()> {
        match expr {
            DerivedSemanticsSpec::Scalar { expr } => self.scalar(expr, entity, None),
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
        // A filtered entity's ids are its source's current-slot ids. Slot
        // kinds are never filtered entities (the typing check refuses one).
        let entity_kind = self
            .relations
            .filtered
            .get(entity)
            .map_or(entity, String::as_str);
        let matches = kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| kind.as_str() == entity_kind)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => {
                *current_slot = *index;
                *related_slot = 1 - index;
            }
            [_, _] => {
                return Err(DirectionError::Aggregate {
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

    fn scalar(
        &mut self,
        expr: &mut ScalarExprSpec,
        entity: &str,
        membership: Membership<'_>,
    ) -> Result<()> {
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
            }
            | ScalarExprSpec::ExtremeRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
                ..
            }
            | ScalarExprSpec::OnlyRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
                ..
            } => {
                let related_entity =
                    self.aggregate(relation, current_slot, related_slot, entity)?;
                if let Some(predicate) = where_clause {
                    self.judgment(predicate, &related_entity, None)?;
                }
            }
            ScalarExprSpec::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, entity, membership)?;
                self.scalar(then_expr, entity, membership)?;
                self.scalar(else_expr, entity, membership)?;
            }
            ScalarExprSpec::NoMatch { subject, patterns } => {
                self.scalar(subject, entity, membership)?;
                for pattern in patterns {
                    self.scalar(pattern, entity, None)?;
                }
            }
            ScalarExprSpec::Add { items }
            | ScalarExprSpec::Min { items }
            | ScalarExprSpec::Max { items } => {
                for item in items {
                    self.scalar(item, entity, membership)?;
                }
            }
            ScalarExprSpec::Sub { left, right }
            | ScalarExprSpec::Mul { left, right }
            | ScalarExprSpec::Div { left, right } => {
                self.scalar(left, entity, membership)?;
                self.scalar(right, entity, membership)?;
            }
            ScalarExprSpec::DateAddDays { date, days }
            | ScalarExprSpec::DateAddMonths { date, months: days }
            | ScalarExprSpec::DateAddYears { date, years: days } => {
                self.scalar(date, entity, membership)?;
                self.scalar(days, entity, membership)?;
            }
            ScalarExprSpec::DaysBetween { from, to } => {
                self.scalar(from, entity, membership)?;
                self.scalar(to, entity, membership)?;
            }
            ScalarExprSpec::ParameterLookup { index, .. }
            | ScalarExprSpec::Ceil { value: index }
            | ScalarExprSpec::Floor { value: index } => self.scalar(index, entity, membership)?,
            ScalarExprSpec::OverPeriods { value, n, .. } => {
                self.scalar(value, entity, None)?;
                if let Some(n) = n {
                    self.scalar(n, entity, None)?;
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
        membership: Membership<'_>,
    ) -> Result<()> {
        match expr {
            JudgmentExprSpec::Comparison { left, right, .. } => {
                self.scalar(left, entity, membership)?;
                self.scalar(right, entity, membership)?;
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
                // A derived relation's traversal reads its source with the
                // derivation's own slots; a membership test can only use them.
                if let Some(derived) = self.relations.derived.get(relation.as_str()) {
                    *current_slot = derived.current_slot;
                    *related_slot = derived.related_slot;
                    return Ok(());
                }
                let Some((Some(current), Some(related))) = membership else {
                    return Ok(());
                };
                let Some(kinds) = self.relations.typed.get(relation.as_str()) else {
                    return Ok(());
                };
                if current == related {
                    if kinds.iter().all(|kind| kind == current) {
                        return Err(DirectionError::Aggregate {
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

#[cfg(test)]
mod chain_tests {
    //! The kinds this pass reads, against the per-relation walk they replaced.
    use super::relations;
    use crate::relation_typing::chain_test_programs::program;
    use crate::spec::ProgramSpec;
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    fn always() -> Value {
        json!({
            "kind": "comparison",
            "left": {"kind": "literal", "value": {"kind": "integer", "value": 1}},
            "op": "eq",
            "right": {"kind": "literal", "value": {"kind": "integer", "value": 1}}
        })
    }

    /// The model program's relations as a program spec, in name order.
    fn spec_of(program: &crate::model::Program) -> ProgramSpec {
        let mut names = program.relations.keys().collect::<Vec<_>>();
        names.sort();
        let relations = names
            .into_iter()
            .map(|name| {
                let relation = &program.relations[name];
                let mut spec = json!({
                    "name": relation.name,
                    "arity": relation.arity,
                    "slot_entities": relation.slot_entities,
                });
                if let Some(derivation) = &relation.derivation {
                    spec["derivation"] = json!({
                        "source_relation": derivation.source_relation,
                        "current_slot": derivation.current_slot,
                        "related_slot": derivation.related_slot,
                        "entity": derivation.entity,
                        "slot_entities": derivation.slot_entities,
                        "predicate": always(),
                    });
                }
                spec
            })
            .collect::<Vec<_>>();
        serde_json::from_value(json!({"relations": relations})).expect("relations parse")
    }

    /// The per-relation walk `relations` ran for each derived relation.
    fn reference_kinds_of(program: &ProgramSpec, name: &str) -> Option<Vec<String>> {
        let declared: HashMap<&str, &crate::spec::RelationSpec> = program
            .relations
            .iter()
            .map(|relation| (relation.name.as_str(), relation))
            .collect();
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
    }

    #[test]
    fn direction_kinds_match_the_per_relation_walk() {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 4000,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[17; 32]),
        );
        let inherited = std::cell::Cell::new(0_usize);
        runner
            .run(&program(), |model| {
                let spec = spec_of(&model);
                let resolved = relations(&spec);
                let mut filtered = BTreeMap::<String, BTreeSet<String>>::new();
                for relation in &spec.relations {
                    let Some(derivation) = &relation.derivation else {
                        continue;
                    };
                    let expected = reference_kinds_of(&spec, &relation.name);
                    let actual = resolved.derived[&relation.name].kinds.clone();
                    if actual != expected {
                        return Err(TestCaseError::fail(format!(
                            "{}: {actual:?} != {expected:?}\n{spec:#?}",
                            relation.name
                        )));
                    }
                    if derivation.slot_entities.is_empty()
                        && relation.slot_entities.is_empty()
                        && expected.is_some()
                    {
                        inherited.set(inherited.get() + 1);
                    }
                    if let Some(entity) = &derivation.entity
                        && let Some(kind) = expected
                            .as_ref()
                            .and_then(|kinds| kinds.get(derivation.current_slot))
                    {
                        filtered
                            .entry(entity.clone())
                            .or_default()
                            .insert(kind.clone());
                    }
                }
                let expected = filtered
                    .into_iter()
                    .filter(|(_, kinds)| kinds.len() == 1)
                    .filter_map(|(entity, kinds)| {
                        kinds.into_iter().next().map(|kind| (entity, kind))
                    })
                    .collect::<HashMap<_, _>>();
                if resolved.filtered != expected {
                    return Err(TestCaseError::fail(format!(
                        "filtered {:?} != {expected:?}\n{spec:#?}",
                        resolved.filtered
                    )));
                }
                Ok(())
            })
            .unwrap();
        let inherited = inherited.get();
        assert!(inherited > 1500, "only {inherited} inherited kinds");
    }

    #[test]
    fn direction_kinds_are_linear_in_a_long_chain() {
        // Each derived relation walked its chain to the bottom, twice, cloning
        // a name per link: quadratic in the chain length.
        let links = 50_000;
        let mut relations_json =
            vec![json!({"name": "base", "arity": 2, "slot_entities": ["Household", "Person"]})];
        for link in 0..links {
            let source = if link + 1 < links {
                format!("rel{:07}", link + 1)
            } else {
                "base".to_string()
            };
            relations_json.push(json!({
                "name": format!("rel{link:07}"),
                "arity": 2,
                "derivation": {"source_relation": source, "current_slot": 0, "related_slot": 1,
                               "entity": "Unit", "predicate": always()}
            }));
        }
        let spec: ProgramSpec =
            serde_json::from_value(json!({"relations": relations_json})).expect("relations parse");
        let started = std::time::Instant::now();
        let resolved = relations(&spec);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 10, "{elapsed:?}");
        assert_eq!(resolved.derived.len(), links);
        assert!(resolved.derived.values().all(|slots| {
            slots.kinds.as_deref() == Some(&["Household".to_string(), "Person".to_string()][..])
        }));
        assert_eq!(
            resolved.filtered.get("Unit").map(String::as_str),
            Some("Household")
        );
    }
}
