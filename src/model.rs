use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PeriodKind {
    Month,
    BenefitWeek,
    TaxYear,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Period {
    pub kind: PeriodKind,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl Period {
    pub fn month(year: i32, month: u32) -> Self {
        let start = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month start");
        let (next_year, next_month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        let next_start =
            NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("valid next month");
        let end = next_start - Duration::days(1);
        Self {
            kind: PeriodKind::Month,
            start,
            end,
        }
    }

    pub fn benefit_week(start: NaiveDate) -> Self {
        Self {
            kind: PeriodKind::BenefitWeek,
            start,
            end: start + Duration::days(6),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Interval {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl Interval {
    pub fn covering(period: &Period) -> Self {
        Self {
            start: period.start,
            end: period.end,
        }
    }

    pub fn contains_period(&self, period: &Period) -> bool {
        self.start <= period.start && self.end >= period.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Currency { minor_units: u8 },
    Count,
    Ratio,
    Duration,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitDef {
    pub name: String,
    pub kind: UnitKind,
}

impl UnitDef {
    pub fn currency(name: impl Into<String>, minor_units: u8) -> Self {
        Self {
            name: name.into(),
            kind: UnitKind::Currency { minor_units },
        }
    }

    pub fn count(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: UnitKind::Count,
        }
    }

    pub fn custom(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: UnitKind::Custom(kind.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DType {
    Judgment,
    Bool,
    Integer,
    Decimal,
    Text,
    Date,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScalarValue {
    Bool(bool),
    Integer(i64),
    Decimal(Decimal),
    Text(String),
    Date(NaiveDate),
}

impl ScalarValue {
    pub fn as_decimal(&self) -> Option<Decimal> {
        match self {
            ScalarValue::Integer(value) => Some(Decimal::from(*value)),
            ScalarValue::Decimal(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ScalarValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_date(&self) -> Option<NaiveDate> {
        match self {
            ScalarValue::Date(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_index(&self) -> Option<i64> {
        match self {
            ScalarValue::Integer(value) => Some(*value),
            ScalarValue::Decimal(value) if value.fract().is_zero() => value.to_i64(),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgmentOutcome {
    Holds,
    NotHolds,
    Undetermined,
}

impl JudgmentOutcome {
    pub fn is_holds(self) -> bool {
        matches!(self, JudgmentOutcome::Holds)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComparisonOp {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Ne,
}

#[derive(Clone, Debug)]
pub enum RelatedValueRef {
    Input(String),
    Derived(String),
}

#[derive(Clone, Debug)]
pub enum ScalarExpr {
    Literal(ScalarValue),
    Input(String),
    /// Look up an entity's input, returning `default` if no record covers the
    /// query period. Lets RuleSpec modules evolve their input surface without
    /// forcing every caller to supply every flag or amount — common when a
    /// calculation has many optional reliefs (blind person's allowance,
    /// marriage allowance transfer, country of residence, Gift Aid, etc.).
    InputOrElse {
        name: String,
        default: ScalarValue,
    },
    Derived(String),
    ParameterLookup {
        parameter: String,
        index: Box<ScalarExpr>,
    },
    Add(Vec<ScalarExpr>),
    Sub(Box<ScalarExpr>, Box<ScalarExpr>),
    Mul(Box<ScalarExpr>, Box<ScalarExpr>),
    Div(Box<ScalarExpr>, Box<ScalarExpr>),
    Max(Vec<ScalarExpr>),
    Min(Vec<ScalarExpr>),
    Ceil(Box<ScalarExpr>),
    Floor(Box<ScalarExpr>),
    PeriodStart,
    PeriodEnd,
    DateAddDays {
        date: Box<ScalarExpr>,
        days: Box<ScalarExpr>,
    },
    DaysBetween {
        from: Box<ScalarExpr>,
        to: Box<ScalarExpr>,
    },
    CountRelated {
        relation: String,
        current_slot: usize,
        related_slot: usize,
        where_clause: Option<Box<JudgmentExpr>>,
    },
    SumRelated {
        relation: String,
        current_slot: usize,
        related_slot: usize,
        value: RelatedValueRef,
        where_clause: Option<Box<JudgmentExpr>>,
    },
    If {
        condition: Box<JudgmentExpr>,
        then_expr: Box<ScalarExpr>,
        else_expr: Box<ScalarExpr>,
    },
}

#[derive(Clone, Debug)]
pub enum JudgmentExpr {
    Comparison {
        left: ScalarExpr,
        op: ComparisonOp,
        right: ScalarExpr,
    },
    Derived(String),
    And(Vec<JudgmentExpr>),
    Or(Vec<JudgmentExpr>),
    Not(Box<JudgmentExpr>),
}

#[derive(Clone, Debug)]
pub enum DerivedSemantics {
    Scalar(ScalarExpr),
    Judgment(JudgmentExpr),
}

#[derive(Clone, Debug)]
pub struct Derived {
    pub id: Option<String>,
    pub name: String,
    pub entity: String,
    pub dtype: DType,
    pub unit: Option<String>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub semantics: DerivedSemantics,
}

#[derive(Clone, Debug)]
pub struct RelationSchema {
    pub name: String,
    pub arity: usize,
}

#[derive(Clone, Debug)]
pub struct ParameterVersion {
    pub effective_from: NaiveDate,
    pub values: BTreeMap<i64, ScalarValue>,
}

#[derive(Clone, Debug)]
pub struct IndexedParameter {
    pub id: Option<String>,
    pub name: String,
    pub unit: Option<String>,
    pub indexed_by: Option<String>,
    pub versions: Vec<ParameterVersion>,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub units: HashMap<String, UnitDef>,
    pub relations: HashMap<String, RelationSchema>,
    pub parameters: HashMap<String, IndexedParameter>,
    pub derived: HashMap<String, Derived>,
}

impl Program {
    pub fn add_unit(&mut self, unit: UnitDef) {
        self.units.insert(unit.name.clone(), unit);
    }

    pub fn add_relation(&mut self, name: impl Into<String>, arity: usize) {
        let name = name.into();
        self.relations
            .insert(name.clone(), RelationSchema { name, arity });
    }

    pub fn add_parameter(&mut self, parameter: IndexedParameter) {
        self.parameters.insert(parameter.name.clone(), parameter);
    }

    pub fn add_derived(&mut self, derived: Derived) {
        self.derived.insert(derived.name.clone(), derived);
    }

    pub fn resolve_derived_name(&self, reference: &str) -> Option<String> {
        if let Some(derived) = self
            .derived
            .values()
            .find(|derived| derived.id.as_deref() == Some(reference))
        {
            return Some(derived.name.clone());
        }
        let derived = self.derived.get(reference)?;
        if derived.id.is_none() {
            Some(reference.to_string())
        } else {
            None
        }
    }

    pub fn resolve_input_name(&self, reference: &str) -> Option<String> {
        if !self.has_public_ids() {
            return Some(reference.to_string());
        }

        let public_reference = PublicReference::parse(reference)?;
        let input_slots = self.input_slots();
        if let Some(input_name) = public_reference.fragment.strip_prefix("input.") {
            if input_slots.contains(input_name) {
                return Some(input_name.to_string());
            }
            return None;
        }

        if let Some(derived) = self
            .derived
            .values()
            .find(|derived| derived.id.as_deref() == Some(reference))
        {
            return Some(derived.name.clone());
        }

        if let Some(parameter) = self
            .parameters
            .values()
            .find(|parameter| parameter.id.as_deref() == Some(reference))
        {
            return Some(parameter.name.clone());
        }

        if input_slots.contains(public_reference.fragment) {
            return Some(public_reference.fragment.to_string());
        }

        None
    }

    pub fn resolve_relation_name(&self, reference: &str) -> Option<String> {
        if !self.has_public_ids() {
            return self
                .relations
                .contains_key(reference)
                .then(|| reference.to_string());
        }

        let public_reference = PublicReference::parse(reference)?;
        let relation_name = public_reference.fragment.strip_prefix("relation.")?;
        self.relations
            .contains_key(relation_name)
            .then(|| relation_name.to_string())
    }

    pub fn public_derived_key(&self, name: &str) -> String {
        self.derived
            .get(name)
            .and_then(|derived| derived.id.clone())
            .unwrap_or_else(|| name.to_string())
    }

    fn has_public_ids(&self) -> bool {
        self.derived.values().any(|derived| derived.id.is_some())
            || self
                .parameters
                .values()
                .any(|parameter| parameter.id.is_some())
    }

    fn input_slots(&self) -> HashSet<&str> {
        let mut slots = HashSet::new();
        for derived in self.derived.values() {
            collect_input_slots_from_semantics(&derived.semantics, &mut slots);
        }
        slots
    }
}

struct PublicReference<'a> {
    fragment: &'a str,
}

impl<'a> PublicReference<'a> {
    fn parse(reference: &'a str) -> Option<Self> {
        let (target, fragment) = reference.split_once('#')?;
        if !target.contains(':') || target.trim().is_empty() || fragment.trim().is_empty() {
            return None;
        }
        Some(Self { fragment })
    }
}

fn collect_input_slots_from_semantics<'a>(
    semantics: &'a DerivedSemantics,
    slots: &mut HashSet<&'a str>,
) {
    match semantics {
        DerivedSemantics::Scalar(expr) => collect_input_slots_from_scalar_expr(expr, slots),
        DerivedSemantics::Judgment(expr) => collect_input_slots_from_judgment_expr(expr, slots),
    }
}

fn collect_input_slots_from_scalar_expr<'a>(expr: &'a ScalarExpr, slots: &mut HashSet<&'a str>) {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::Derived(_)
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
        ScalarExpr::Input(name) => {
            slots.insert(name.as_str());
        }
        ScalarExpr::InputOrElse { name, .. } => {
            slots.insert(name.as_str());
        }
        ScalarExpr::ParameterLookup { index, .. } => {
            collect_input_slots_from_scalar_expr(index, slots);
        }
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_input_slots_from_scalar_expr(item, slots);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right) => {
            collect_input_slots_from_scalar_expr(left, slots);
            collect_input_slots_from_scalar_expr(right, slots);
        }
        ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => {
            collect_input_slots_from_scalar_expr(value, slots);
        }
        ScalarExpr::DateAddDays { date, days } => {
            collect_input_slots_from_scalar_expr(date, slots);
            collect_input_slots_from_scalar_expr(days, slots);
        }
        ScalarExpr::DaysBetween { from, to } => {
            collect_input_slots_from_scalar_expr(from, slots);
            collect_input_slots_from_scalar_expr(to, slots);
        }
        ScalarExpr::CountRelated { where_clause, .. } => {
            if let Some(where_clause) = where_clause {
                collect_input_slots_from_judgment_expr(where_clause, slots);
            }
        }
        ScalarExpr::SumRelated {
            value,
            where_clause,
            ..
        } => {
            if let RelatedValueRef::Input(name) = value {
                slots.insert(name.as_str());
            }
            if let Some(where_clause) = where_clause {
                collect_input_slots_from_judgment_expr(where_clause, slots);
            }
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_input_slots_from_judgment_expr(condition, slots);
            collect_input_slots_from_scalar_expr(then_expr, slots);
            collect_input_slots_from_scalar_expr(else_expr, slots);
        }
    }
}

fn collect_input_slots_from_judgment_expr<'a>(
    expr: &'a JudgmentExpr,
    slots: &mut HashSet<&'a str>,
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_input_slots_from_scalar_expr(left, slots);
            collect_input_slots_from_scalar_expr(right, slots);
        }
        JudgmentExpr::Derived(_) => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_input_slots_from_judgment_expr(item, slots);
            }
        }
        JudgmentExpr::Not(item) => {
            collect_input_slots_from_judgment_expr(item, slots);
        }
    }
}

#[derive(Clone, Debug)]
pub struct InputRecord {
    pub name: String,
    pub entity: String,
    pub entity_id: String,
    pub interval: Interval,
    pub value: ScalarValue,
}

#[derive(Clone, Debug)]
pub struct RelationRecord {
    pub name: String,
    pub tuple: Vec<String>,
    pub interval: Interval,
}

#[derive(Clone, Debug, Default)]
pub struct DataSet {
    pub inputs: Vec<InputRecord>,
    pub relations: Vec<RelationRecord>,
}

impl DataSet {
    pub fn add_input(
        &mut self,
        name: impl Into<String>,
        entity: impl Into<String>,
        entity_id: impl Into<String>,
        interval: Interval,
        value: ScalarValue,
    ) {
        self.inputs.push(InputRecord {
            name: name.into(),
            entity: entity.into(),
            entity_id: entity_id.into(),
            interval,
            value,
        });
    }

    pub fn add_relation(
        &mut self,
        name: impl Into<String>,
        tuple: Vec<String>,
        interval: Interval,
    ) {
        self.relations.push(RelationRecord {
            name: name.into(),
            tuple,
            interval,
        });
    }
}

pub fn year_start(year: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, 1, 1).expect("valid year start")
}

pub fn year_of(period: &Period) -> i32 {
    period.start.year()
}
