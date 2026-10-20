//! Explicit annual primitive reads. Declaration metadata is retained because
//! ordinary input catalogs intentionally do not encode an author's period/type.
//! Structural IR checks do not authenticate an author's legal source.
use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Period, PeriodKind, ScalarValue};
use crate::spec::{ProgramSpec, SpecError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SelectedYearInput {
    pub id: String,
    pub slot: String,
    pub entity: String,
    pub dtype: String,
    pub period: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl SelectedYearInput {
    pub(crate) fn check_value(&self, value: &ScalarValue) -> Result<(), String> {
        let valid = matches!(
            (self.dtype.as_str(), value),
            ("bool", ScalarValue::Bool(_))
                | ("integer", ScalarValue::Integer(_))
                | (
                    "decimal" | "money",
                    ScalarValue::Decimal(_) | ScalarValue::Integer(_)
                )
                | ("date", ScalarValue::Date(_))
                | ("text", ScalarValue::Text(_))
        );
        if valid {
            Ok(())
        } else {
            Err(format!(
                "{} requires {} without coercion",
                self.id, self.dtype
            ))
        }
    }
}

pub(crate) fn period(year: i32) -> Result<Period, String> {
    if !(1..=9999).contains(&year) {
        return Err("selected year must be 1..9999".into());
    }
    Ok(Period {
        kind: PeriodKind::TaxYear,
        start: NaiveDate::from_ymd_opt(year, 1, 1).ok_or("invalid selected year")?,
        end: NaiveDate::from_ymd_opt(year, 12, 31).ok_or("invalid selected year")?,
    })
}

// Walk only serialized, typed expression trees. Unknown source fields never
// enter this traversal; serde has already constructed the closed IR enum.
fn nodes(
    value: &mut Value,
    f: &mut impl FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    if value.get("kind").and_then(Value::as_str) == Some("input_for_year") {
        f(value)?;
    }
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                nodes(child, f)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                nodes(child, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate(program: &ProgramSpec) -> Result<(), SpecError> {
    let fail = SpecError::SelectedYearInput;
    let mut contracts = BTreeMap::new();
    let mut slots = BTreeSet::new();
    for item in &program.selected_year_inputs {
        let valid_id = match item.id.split_once("#input.") {
            Some((target, slot)) => {
                slot == item.slot && crate::rulespec::validate_module_target(target).is_ok()
            }
            None => item.id == item.slot,
        };
        if !valid_id
            || item.slot.is_empty()
            || item.slot.contains(['#', ':'])
            || item.slot.chars().any(char::is_whitespace)
            || item.entity.is_empty()
            || item.entity == "Scalar"
            || item.period != "Year"
            || !["bool", "integer", "decimal", "money", "date", "text"]
                .contains(&item.dtype.as_str())
        {
            return Err(fail(format!("malformed contract {}", item.id)));
        }
        if item.dtype == "money" && item.unit.is_none() {
            return Err(fail(format!("Money requires a unit: {}", item.id)));
        }
        if let Some(unit) = &item.unit {
            if !["decimal", "money"].contains(&item.dtype.as_str())
                || !program.units.iter().any(|u| {
                    &u.name == unit
                        && (item.dtype != "money"
                            || matches!(u.kind, crate::spec::UnitKindSpec::Currency { .. }))
                })
            {
                return Err(fail(format!(
                    "unknown or incompatible unit for {}",
                    item.id
                )));
            }
        }
        if contracts.insert(item.id.as_str(), item).is_some() || !slots.insert(item.slot.as_str()) {
            return Err(fail(format!("ambiguous selected input {}", item.id)));
        }
    }
    let mut used = BTreeSet::new();
    for rule in &program.derived {
        let mut tree = serde_json::to_value(rule).map_err(|e| fail(e.to_string()))?;
        nodes(&mut tree, &mut |node| {
            let id = node["input"].as_str().ok_or("missing selected input")?;
            let contract = contracts
                .get(id)
                .ok_or_else(|| format!("missing contract {id}"))?;
            let citing_origin = rule
                .id
                .as_deref()
                .and_then(|id| id.split_once('#').map(|(o, _)| o));
            let input_origin = id.split_once("#input.").map(|(o, _)| o);
            if citing_origin != input_origin {
                return Err(format!("{id} belongs to another module"));
            }
            if contract.entity != rule.entity {
                return Err(format!("{id} owner does not match {}", rule.entity));
            }
            let year = node["year"].as_i64().ok_or("year must be an integer")?;
            if !(1..=9999).contains(&year) {
                return Err("year must be 1..9999".into());
            }
            used.insert(id.to_string());
            Ok(())
        })
        .map_err(fail)?;
    }
    // Relation predicates must reach a typed derived rule; they cannot attach
    // an annual primitive to an implicit current/related entity.
    let mut relations =
        serde_json::to_value(&program.relations).map_err(|e| fail(e.to_string()))?;
    nodes(&mut relations, &mut |_| {
        Err("use a typed derived rule for selected-year reads in relations".into())
    })
    .map_err(fail)?;
    if contracts.keys().any(|id| !used.contains(*id)) {
        return Err(fail("orphan selected-year contract".into()));
    }
    Ok(())
}

pub(crate) fn bind(
    program: &mut ProgramSpec,
    inputs: &serde_yaml::Value,
    origin: Option<&str>,
    imports: &[(Option<String>, serde_yaml::Value)],
) -> Result<(), String> {
    let mut feature_present = false;
    let mut expressions =
        serde_json::to_value((&program.derived, &program.relations)).map_err(|e| e.to_string())?;
    nodes(&mut expressions, &mut |_| {
        feature_present = true;
        Ok(())
    })?;
    if !feature_present {
        return Ok(());
    }
    let blocks = std::iter::once((origin, inputs))
        .chain(imports.iter().map(|(o, v)| (o.as_deref(), v)))
        .collect::<Vec<_>>();
    let mut contracts = BTreeMap::new();
    for rule in &mut program.derived {
        let rule_origin = rule
            .id
            .as_deref()
            .and_then(|id| id.split_once('#').map(|(o, _)| o));
        let mut tree = serde_json::to_value(&*rule).map_err(|e| e.to_string())?;
        nodes(&mut tree, &mut |node| {
            let slot = node["input"]
                .as_str()
                .ok_or("missing selected input")?
                .to_string();
            let mut found = Vec::new();
            for (owner, block) in &blocks {
                if block.is_null() {
                    continue;
                }
                for item in block
                    .as_sequence()
                    .ok_or("input declarations must be a sequence")?
                {
                    if item.get("name").and_then(serde_yaml::Value::as_str) == Some(&slot) {
                        found.push((*owner, item));
                    }
                }
            }
            let [(owner, declaration)] = found.as_slice() else {
                return Err(format!(
                    "{slot} declaration is absent or shares a runtime slot"
                ));
            };
            if *owner != rule_origin {
                return Err(format!("{slot} belongs to another module"));
            }
            if declaration.get("default").is_some() {
                return Err(format!("{slot} must not declare a default"));
            }
            let text = |name: &str| declaration.get(name).and_then(serde_yaml::Value::as_str);
            if text("entity") != Some(rule.entity.as_str()) || text("period") != Some("Year") {
                return Err(format!("{slot} requires matching owner and Year period"));
            }
            let authored = text("dtype").ok_or("selected input requires dtype")?;
            let dtype = match authored {
                "Boolean" | "Bool" | "bool" | "boolean" => "bool",
                "Integer" | "integer" => "integer",
                "Money" | "money" => "money",
                "Number" | "number" | "Decimal" | "decimal" => "decimal",
                "Date" | "date" => "date",
                "Text" | "text" => "text",
                _ => return Err(format!("unsupported selected input dtype {authored}")),
            };
            let unit = text("unit").map(str::to_string);
            if matches!(authored, "Money" | "money") && unit.is_none() {
                return Err(format!("{slot} Money input requires a unit"));
            }
            let id = owner.map_or_else(|| slot.clone(), |o| format!("{o}#input.{slot}"));
            let contract = SelectedYearInput {
                id: id.clone(),
                slot,
                entity: rule.entity.clone(),
                dtype: dtype.into(),
                period: "Year".into(),
                unit,
            };
            if let Some(previous) = contracts.insert(id.clone(), contract.clone()) {
                if previous != contract {
                    return Err(format!("conflicting contract {id}"));
                }
            }
            node["input"] = Value::String(id);
            Ok(())
        })?;
        *rule = serde_json::from_value(tree).map_err(|e| e.to_string())?;
    }
    program.selected_year_inputs = contracts.into_values().collect();
    program.to_program().map(|_| ()).map_err(|e| e.to_string())
}
