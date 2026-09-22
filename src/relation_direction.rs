//! Resolve RuleSpec aggregation direction from explicit entity declarations.
//! Undeclared and ambiguous relations retain the legacy direction.
use crate::spec::{DerivedSemanticsSpec, JudgmentExprSpec, ProgramSpec, ScalarExprSpec};
use std::collections::HashMap;

pub(crate) fn resolve(program: &mut ProgramSpec) {
    let slots: HashMap<_, _> = program
        .relations
        .iter()
        .filter(|r| r.slot_entities.len() == 2)
        .map(|r| (r.name.clone(), r.slot_entities.clone()))
        .collect();
    for rule in &mut program.derived {
        semantics(&mut rule.semantics, &rule.entity, &slots);
        for version in &mut rule.versions {
            semantics(&mut version.semantics, &rule.entity, &slots);
        }
    }
}

type Slots = HashMap<String, Vec<String>>;
fn semantics(expr: &mut DerivedSemanticsSpec, entity: &str, slots: &Slots) {
    match expr {
        DerivedSemanticsSpec::Scalar { expr } => scalar(expr, entity, slots),
        DerivedSemanticsSpec::Judgment { expr } => judgment(expr, entity, slots),
    }
}
fn scalar(expr: &mut ScalarExprSpec, entity: &str, slots: &Slots) {
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
            let declared = slots.get(relation);
            if let Some(kinds) = declared {
                let matches: Vec<_> = kinds
                    .iter()
                    .enumerate()
                    .filter(|(_, kind)| kind.as_str() == entity)
                    .map(|(i, _)| i)
                    .collect();
                if let [index] = matches.as_slice() {
                    *current_slot = *index;
                    *related_slot = 1 - index;
                }
            }
            if let Some(predicate) = where_clause {
                let related_entity = declared
                    .and_then(|kinds| kinds.get(*related_slot))
                    .map(String::as_str)
                    .unwrap_or("");
                judgment(predicate, related_entity, slots);
            }
        }
        ScalarExprSpec::If {
            condition,
            then_expr,
            else_expr,
        } => {
            judgment(condition, entity, slots);
            scalar(then_expr, entity, slots);
            scalar(else_expr, entity, slots);
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Min { items }
        | ScalarExprSpec::Max { items } => {
            for item in items {
                scalar(item, entity, slots);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            scalar(left, entity, slots);
            scalar(right, entity, slots);
        }
        ScalarExprSpec::DateAddDays { date, days }
        | ScalarExprSpec::DateAddMonths { date, months: days }
        | ScalarExprSpec::DateAddYears { date, years: days } => {
            scalar(date, entity, slots);
            scalar(days, entity, slots);
        }
        ScalarExprSpec::DaysBetween { from, to } => {
            scalar(from, entity, slots);
            scalar(to, entity, slots);
        }
        ScalarExprSpec::ParameterLookup { index, .. }
        | ScalarExprSpec::Ceil { value: index }
        | ScalarExprSpec::CalendarYearsToMonths { years: index }
        | ScalarExprSpec::Floor { value: index } => scalar(index, entity, slots),
        ScalarExprSpec::OverPeriods { value, n, .. } => {
            scalar(value, entity, slots);
            if let Some(n) = n {
                scalar(n, entity, slots);
            }
        }
        ScalarExprSpec::Literal { .. }
        | ScalarExprSpec::Input { .. }
        | ScalarExprSpec::InputOrElse { .. }
        | ScalarExprSpec::Derived { .. }
        | ScalarExprSpec::PeriodStart
        | ScalarExprSpec::PeriodEnd => {}
    }
}
fn judgment(expr: &mut JudgmentExprSpec, entity: &str, slots: &Slots) {
    match expr {
        JudgmentExprSpec::Comparison { left, right, .. } => {
            scalar(left, entity, slots);
            scalar(right, entity, slots);
        }
        JudgmentExprSpec::And { items }
        | JudgmentExprSpec::Or { items }
        | JudgmentExprSpec::ExactlyOne { items } => {
            for item in items {
                judgment(item, entity, slots);
            }
        }
        JudgmentExprSpec::Not { item } => judgment(item, entity, slots),
        JudgmentExprSpec::Derived { .. } | JudgmentExprSpec::RelationMember { .. } => {}
    }
}
