//! Strict checks and capability selection for the compatibility-only operation.
//! Existing relation operators retain the base release's contracts.
use crate::spec::{
    DerivedSemanticsSpec as S, JudgmentExprSpec as J, ProgramSpec, RelatedValueRefSpec,
    ScalarExprSpec as X,
};
use std::collections::HashMap;

#[derive(Debug)]
pub(crate) struct Error {
    pub citing: String,
    pub input: String,
    pub message: String,
}
type Kinds = HashMap<String, Vec<String>>;

fn kinds(program: &ProgramSpec) -> Kinds {
    let mut result: Kinds = program
        .relations
        .iter()
        .filter(|r| r.arity == 2 && r.slot_entities.len() == 2)
        .map(|r| (r.name.clone(), r.slot_entities.clone()))
        .collect();
    loop {
        let mut added = false;
        for relation in &program.relations {
            if result.contains_key(&relation.name) || relation.arity != 2 {
                continue;
            }
            if let Some(d) = &relation.derivation {
                if let Some(source) = result.get(&d.source_relation).cloned() {
                    if d.current_slot < 2 && d.related_slot < 2 && d.current_slot != d.related_slot
                    {
                        result.insert(
                            relation.name.clone(),
                            vec![
                                source[d.current_slot].clone(),
                                source[d.related_slot].clone(),
                            ],
                        );
                        added = true;
                    }
                }
            }
        }
        if !added {
            return result;
        }
    }
}

pub(crate) fn lower(
    program: &mut ProgramSpec,
    inputs: &serde_yaml::Value,
    origin: Option<&str>,
    imported: &[(Option<String>, serde_yaml::Value)],
) -> Result<(), Error> {
    if !spec_contains(program) {
        return Ok(());
    }
    let slots = kinds(program);
    let mut blocks: HashMap<Option<&str>, Vec<&serde_yaml::Value>> = HashMap::new();
    for (key, value) in std::iter::once((origin, inputs))
        .chain(imported.iter().map(|(key, value)| (key.as_deref(), value)))
    {
        if !value.is_null() {
            blocks.entry(key).or_default().push(value);
        }
    }
    visit_program(program, &slots, &mut |expr, owner, citing| {
        let X::OnlyRelated {
            relation,
            current_slot,
            related_slot,
            value,
        } = expr
        else {
            unreachable!()
        };
        let input = match value {
            RelatedValueRefSpec::Input { name } => name.clone(),
            _ => String::new(),
        };
        let fail = |message: &str| Error {
            citing: citing.into(),
            input: input.clone(),
            message: message.into(),
        };
        let pair = slots
            .get(relation)
            .ok_or_else(|| fail("a typed binary relation is required"))?;
        let positions: Vec<_> = pair
            .iter()
            .enumerate()
            .filter(|(_, kind)| kind.as_str() == owner)
            .map(|(i, _)| i)
            .collect();
        let [position] = positions.as_slice() else {
            return Err(fail(
                "current owner must identify exactly one declared slot",
            ));
        };
        *current_slot = *position;
        *related_slot = 1 - position;
        if input.is_empty() {
            return Err(fail("only an Input operand is supported"));
        }
        let mut declarations = Vec::new();
        for block in blocks
            .get(&citing.split_once('#').map(|(module, _)| module))
            .into_iter()
            .flatten()
        {
            let items = block
                .as_sequence()
                .ok_or_else(|| fail("inputs must be a sequence"))?;
            declarations.extend(items.iter().filter(|item| {
                item.get("name").and_then(serde_yaml::Value::as_str) == Some(input.as_str())
            }));
        }
        let [declaration] = declarations.as_slice() else {
            return Err(fail("input declaration is missing or ambiguous"));
        };
        if !matches!(
            declaration.get("dtype").and_then(serde_yaml::Value::as_str),
            Some("Boolean" | "boolean" | "Bool" | "bool")
        ) {
            return Err(fail("input dtype must be Boolean"));
        }
        if declaration
            .get("entity")
            .and_then(serde_yaml::Value::as_str)
            != Some(pair[*related_slot].as_str())
        {
            return Err(fail(
                "input entity must equal the declared related-slot entity",
            ));
        }
        Ok(())
    })
}

pub(crate) fn validate(program: &ProgramSpec) -> Result<(), String> {
    if !spec_contains(program) {
        return Ok(());
    }
    let slots = kinds(program);
    let mut copy = program.clone();
    visit_program(&mut copy, &slots, &mut |expr, owner, citing| {
        let X::OnlyRelated {
            relation,
            current_slot,
            related_slot,
            value,
        } = expr
        else {
            unreachable!()
        };
        let pair = slots
            .get(relation)
            .ok_or_else(|| format!("{citing}: only_related requires a typed binary relation"))?;
        if *current_slot >= 2
            || *related_slot >= 2
            || current_slot == related_slot
            || pair[0].is_empty()
            || pair[1].is_empty()
            || pair[0] == pair[1]
            || pair[*current_slot] != owner
            || !matches!(value, RelatedValueRefSpec::Input { name } if !name.is_empty())
        {
            return Err(format!(
                "{citing}: invalid only_related slots, owner, or Input operand"
            ));
        }
        Ok(())
    })
}

fn visit_program<E>(
    program: &mut ProgramSpec,
    kinds: &Kinds,
    visitor: &mut impl FnMut(&mut X, &str, &str) -> Result<(), E>,
) -> Result<(), E> {
    for rule in &mut program.derived {
        let citing = rule.id.as_deref().unwrap_or(&rule.name);
        visit_semantics(&mut rule.semantics, &rule.entity, citing, kinds, visitor)?;
        for version in &mut rule.versions {
            visit_semantics(&mut version.semantics, &rule.entity, citing, kinds, visitor)?;
        }
    }
    for relation in &mut program.relations {
        if let Some(d) = &mut relation.derivation {
            let owner = kinds
                .get(&d.source_relation)
                .and_then(|s| s.get(d.related_slot))
                .map(String::as_str)
                .unwrap_or("");
            visit_j(&mut d.predicate, owner, &relation.name, kinds, visitor)?;
        }
    }
    Ok(())
}
fn visit_semantics<E>(
    s: &mut S,
    owner: &str,
    citing: &str,
    kinds: &Kinds,
    f: &mut impl FnMut(&mut X, &str, &str) -> Result<(), E>,
) -> Result<(), E> {
    match s {
        S::Scalar { expr } => visit_x(expr, owner, citing, kinds, f),
        S::Judgment { expr } => visit_j(expr, owner, citing, kinds, f),
    }
}
fn visit_j<E>(
    j: &mut J,
    owner: &str,
    citing: &str,
    kinds: &Kinds,
    f: &mut impl FnMut(&mut X, &str, &str) -> Result<(), E>,
) -> Result<(), E> {
    match j {
        J::Comparison { left, right, .. } => {
            visit_x(left, owner, citing, kinds, f)?;
            visit_x(right, owner, citing, kinds, f)?;
        }
        J::And { items } | J::Or { items } | J::ExactlyOne { items } => {
            for item in items {
                visit_j(item, owner, citing, kinds, f)?;
            }
        }
        J::Not { item } => visit_j(item, owner, citing, kinds, f)?,
        J::Derived { .. } | J::RelationMember { .. } => {}
    }
    Ok(())
}
fn visit_x<E>(
    x: &mut X,
    owner: &str,
    citing: &str,
    kinds: &Kinds,
    f: &mut impl FnMut(&mut X, &str, &str) -> Result<(), E>,
) -> Result<(), E> {
    match x {
        X::OnlyRelated { .. } => f(x, owner, citing)?,
        X::If {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_j(condition, owner, citing, kinds, f)?;
            visit_x(then_expr, owner, citing, kinds, f)?;
            visit_x(else_expr, owner, citing, kinds, f)?;
        }
        X::Add { items } | X::Max { items } | X::Min { items } => {
            for item in items {
                visit_x(item, owner, citing, kinds, f)?;
            }
        }
        X::Sub { left, right }
        | X::Mul { left, right }
        | X::Div { left, right }
        | X::DaysBetween {
            from: left,
            to: right,
        } => {
            visit_x(left, owner, citing, kinds, f)?;
            visit_x(right, owner, citing, kinds, f)?;
        }
        X::DateAddDays { date, days }
        | X::DateAddMonths { date, months: days }
        | X::DateAddYears { date, years: days } => {
            visit_x(date, owner, citing, kinds, f)?;
            visit_x(days, owner, citing, kinds, f)?;
        }
        X::Ceil { value } | X::Floor { value } | X::ParameterLookup { index: value, .. } => {
            visit_x(value, owner, citing, kinds, f)?
        }
        X::OverPeriods { value, n, .. } => {
            visit_x(value, owner, citing, kinds, f)?;
            if let Some(n) = n {
                visit_x(n, owner, citing, kinds, f)?;
            }
        }
        X::CountRelated {
            relation,
            related_slot,
            where_clause,
            ..
        }
        | X::SumRelated {
            relation,
            related_slot,
            where_clause,
            ..
        } => {
            let related = kinds
                .get(relation)
                .and_then(|s| s.get(*related_slot))
                .map(String::as_str)
                .unwrap_or("");
            if let Some(predicate) = where_clause {
                visit_j(predicate, related, citing, kinds, f)?;
            }
        }
        X::Literal { .. }
        | X::InputForYear { .. }
        | X::Input { .. }
        | X::InputOrElse { .. }
        | X::Derived { .. }
        | X::PeriodStart
        | X::PeriodEnd => {}
    }
    Ok(())
}

fn spec_contains(program: &ProgramSpec) -> bool {
    // Read-only typed traversal avoids cloning/serializing old programs.
    program.derived.iter().any(|r| {
        semantics_has(&r.semantics) || r.versions.iter().any(|v| semantics_has(&v.semantics))
    }) || program
        .relations
        .iter()
        .filter_map(|r| r.derivation.as_ref())
        .any(|d| judgment_has(&d.predicate))
}
fn semantics_has(s: &S) -> bool {
    match s {
        S::Scalar { expr } => scalar_has(expr),
        S::Judgment { expr } => judgment_has(expr),
    }
}
fn judgment_has(j: &J) -> bool {
    match j {
        J::Comparison { left, right, .. } => scalar_has(left) || scalar_has(right),
        J::And { items } | J::Or { items } | J::ExactlyOne { items } => {
            items.iter().any(judgment_has)
        }
        J::Not { item } => judgment_has(item),
        J::Derived { .. } | J::RelationMember { .. } => false,
    }
}
fn scalar_has(x: &X) -> bool {
    match x {
        X::OnlyRelated { .. } => true,
        X::If {
            condition,
            then_expr,
            else_expr,
        } => judgment_has(condition) || scalar_has(then_expr) || scalar_has(else_expr),
        X::Add { items } | X::Min { items } | X::Max { items } => items.iter().any(scalar_has),
        X::Sub { left, right }
        | X::Mul { left, right }
        | X::Div { left, right }
        | X::DaysBetween {
            from: left,
            to: right,
        } => scalar_has(left) || scalar_has(right),
        X::DateAddDays { date, days }
        | X::DateAddMonths { date, months: days }
        | X::DateAddYears { date, years: days } => scalar_has(date) || scalar_has(days),
        X::Ceil { value } | X::Floor { value } | X::ParameterLookup { index: value, .. } => {
            scalar_has(value)
        }
        X::OverPeriods { value, n, .. } => {
            scalar_has(value) || n.as_ref().is_some_and(|n| scalar_has(n))
        }
        X::CountRelated { where_clause, .. } | X::SumRelated { where_clause, .. } => {
            where_clause.as_ref().is_some_and(|j| judgment_has(j))
        }
        _ => false,
    }
}

pub(crate) fn program_contains(program: &crate::model::Program) -> bool {
    use crate::model::DerivedSemantics;
    let semantics = |s: &DerivedSemantics| match s {
        DerivedSemantics::Scalar(x) => model_scalar_has(x),
        DerivedSemantics::Judgment(j) => model_judgment_has(j),
    };
    program
        .derived
        .values()
        .any(|r| semantics(&r.semantics) || r.versions.iter().any(|v| semantics(&v.semantics)))
        || program
            .relations
            .values()
            .filter_map(|r| r.derivation.as_ref())
            .any(|d| model_judgment_has(&d.predicate))
}
fn model_judgment_has(j: &crate::model::JudgmentExpr) -> bool {
    use crate::model::JudgmentExpr as J;
    match j {
        J::Comparison { left, right, .. } => model_scalar_has(left) || model_scalar_has(right),
        J::And(items) | J::Or(items) => items.iter().any(model_judgment_has),
        J::Not(j) => model_judgment_has(j),
        _ => false,
    }
}
fn model_scalar_has(x: &crate::model::ScalarExpr) -> bool {
    use crate::model::ScalarExpr as X;
    match x {
        X::OnlyRelated { .. } => true,
        X::If {
            condition,
            then_expr,
            else_expr,
        } => {
            model_judgment_has(condition)
                || model_scalar_has(then_expr)
                || model_scalar_has(else_expr)
        }
        X::Add(items) | X::Min(items) | X::Max(items) => items.iter().any(model_scalar_has),
        X::Sub(a, b)
        | X::Mul(a, b)
        | X::Div(a, b)
        | X::DaysBetween { from: a, to: b }
        | X::DateAddDays { date: a, days: b }
        | X::DateAddMonths { date: a, months: b }
        | X::DateAddYears { date: a, years: b } => model_scalar_has(a) || model_scalar_has(b),
        X::Ceil(x) | X::Floor(x) | X::ParameterLookup { index: x, .. } => model_scalar_has(x),
        X::OverPeriods { value, n, .. } => {
            model_scalar_has(value) || n.as_ref().is_some_and(|x| model_scalar_has(x))
        }
        X::CountRelated { where_clause, .. } | X::SumRelated { where_clause, .. } => {
            where_clause.as_ref().is_some_and(|j| model_judgment_has(j))
        }
        _ => false,
    }
}

/// Resolve only declared slot provenance, including a derived relation's source.
pub(crate) fn related_kind<'a>(
    program: &'a crate::model::Program,
    relation: &str,
    slot: usize,
) -> Option<&'a str> {
    fn resolve<'a>(
        program: &'a crate::model::Program,
        relation: &str,
        slot: usize,
        seen: &mut std::collections::HashSet<String>,
    ) -> Option<&'a str> {
        if !seen.insert(relation.into()) {
            return None;
        }
        let schema = program.relations.get(relation)?;
        if let Some(kind) = schema
            .slot_entities
            .get(slot)
            .filter(|kind| !kind.is_empty())
        {
            return Some(kind);
        }
        let derived = schema.derivation.as_ref()?;
        let source_slot = match slot {
            0 => derived.current_slot,
            1 => derived.related_slot,
            _ => return None,
        };
        resolve(program, &derived.source_relation, source_slot, seen)
    }
    resolve(program, relation, slot, &mut Default::default())
}
