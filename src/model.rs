use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};

/// Pseudo-entity assigned to formula parameters with no declared entity.
/// Rules at this entity are row-constant.
pub const SCALAR_ENTITY: &str = "Scalar";

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

/// Rounding mode a currency rule applies to its output. Named for the
/// statutory conventions encoders declare from source text: `HalfUp`
/// (round-half-away-from-zero, the SNAP/tax default), `HalfEven` (banker's
/// rounding), `Floor` (toward negative infinity), and `Ceil` (toward positive
/// infinity). See DECISIONS.md (2026-07-03) for the opt-in contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundingMode {
    HalfUp,
    HalfEven,
    Floor,
    Ceil,
}

impl RoundingMode {
    /// The `rust_decimal` strategy that realizes this mode. `HalfUp` maps to
    /// `MidpointAwayFromZero` so a `-0.5` midpoint rounds to `-1` (away from
    /// zero), matching how benefit and tax tables treat magnitudes.
    fn strategy(self) -> RoundingStrategy {
        match self {
            Self::HalfUp => RoundingStrategy::MidpointAwayFromZero,
            Self::HalfEven => RoundingStrategy::MidpointNearestEven,
            Self::Floor => RoundingStrategy::ToNegativeInfinity,
            Self::Ceil => RoundingStrategy::ToPositiveInfinity,
        }
    }

    /// Round a decimal to `minor_units` decimal places under this mode. This is
    /// the single definition of the rounding operation; every execution path
    /// (explain, bulk fast, dense decimal) routes through it so the three paths
    /// are byte-identical on the same value.
    pub fn round_decimal(self, value: Decimal, minor_units: u8) -> Decimal {
        value.round_dp_with_strategy(u32::from(minor_units), self.strategy())
    }

    /// Canonical serialized name (the RuleSpec `rounding:` vocabulary).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HalfUp => "half_up",
            Self::HalfEven => "half_even",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
        }
    }
}

/// A rule's declared output-rounding contract. Present only when an encoder
/// explicitly declares `rounding:` on the rule; absent means today's behavior
/// (no rounding). `minor_units` is resolved at compile time from the rule's
/// currency unit, so the interpreter needs no unit lookup at evaluation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rounding {
    pub mode: RoundingMode,
    pub minor_units: u8,
}

impl Rounding {
    pub fn apply(&self, value: Decimal) -> Decimal {
        self.mode.round_decimal(value, self.minor_units)
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

/// Which reduction an [`ScalarExpr::OverPeriods`] applies across an entity's
/// own period axis. Valid only under the lifetime execution surface
/// (`DenseCompiledProgram::execute_lifetime`); the per-period execution paths
/// reject these because a single period has no period axis to reduce over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverPeriodsKind {
    /// Sum of the inner value across all supplied periods.
    Sum,
    /// Maximum of the inner value across all supplied periods.
    Max,
    /// Count, per entity, of the supplied periods whose inner value is nonzero
    /// (a `Bool` inner value counts `true`). The inner value IS evaluated per
    /// period and tested against zero — this is not a bare period count.
    Count,
    /// Sum of the `n` largest per-period inner values. `n` must satisfy
    /// `1 <= n <= the supplied period count` (the strict n contract): an
    /// over-length `n` would only pad with zeros — an arithmetic no-op — so it
    /// is rejected as a likely data error rather than silently summing every
    /// period. `n` must also be period-invariant.
    SumTopN,
}

impl OverPeriodsKind {
    /// The formula builtin name that lowers to this reduction, used in
    /// diagnostics (e.g. rejecting the node under per-period execution).
    pub fn as_call_name(self) -> &'static str {
        match self {
            Self::Sum => "sum_over_periods",
            Self::Max => "max_over_periods",
            Self::Count => "count_over_periods",
            Self::SumTopN => "sum_top_n_over_periods",
        }
    }
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
    /// Reduction over an entity's own period axis (lifetime execution only).
    /// `value` is the inner per-period expression, evaluated once per supplied
    /// period; `n` is present only for [`OverPeriodsKind::SumTopN`] and gives
    /// the number of largest per-period values to sum. The per-period execution
    /// paths reject this node — it is meaningful only when a batch is supplied
    /// per period through `execute_lifetime`.
    OverPeriods {
        kind: OverPeriodsKind,
        value: Box<ScalarExpr>,
        n: Option<Box<ScalarExpr>>,
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
    RelationMember {
        relation: String,
        current_slot: usize,
        related_slot: usize,
    },
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
pub struct DerivedVersion {
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub semantics: DerivedSemantics,
}

impl DerivedVersion {
    pub fn applies_at(&self, date: NaiveDate) -> bool {
        self.effective_from <= date && self.effective_to.is_none_or(|end| date <= end)
    }
}

#[derive(Clone, Debug)]
pub struct Derived {
    pub id: Option<String>,
    pub name: String,
    pub entity: String,
    pub dtype: DType,
    pub unit: Option<String>,
    /// Output-rounding contract, resolved at compile time when the rule
    /// declares `rounding:` AND its `unit` is `Currency`. `None` means today's
    /// behavior: no rounding is applied. Held on the model (not looked up from
    /// the unit at evaluation time) so every execution path applies exactly the
    /// same operation without re-resolving units.
    pub rounding: Option<Rounding>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    /// Corpus provision path of the rule's origin module, for joining the
    /// rule to its legal source. Descriptive only; never read by execution.
    pub corpus_citation_path: Option<String>,
    pub semantics: DerivedSemantics,
    pub versions: Vec<DerivedVersion>,
}

impl Derived {
    pub fn semantics_at(&self, period: &Period) -> Option<&DerivedSemantics> {
        if self.versions.is_empty() {
            return Some(&self.semantics);
        }
        self.versions
            .iter()
            .filter(|version| version.applies_at(period.start))
            .max_by_key(|version| version.effective_from)
            .map(|version| &version.semantics)
    }
}

#[derive(Clone, Debug)]
pub struct RelationSchema {
    pub name: String,
    pub arity: usize,
    pub derivation: Option<RelationDerivation>,
}

#[derive(Clone, Debug)]
pub struct RelationDerivation {
    pub source_relation: String,
    pub current_slot: usize,
    pub related_slot: usize,
    pub entity: Option<String>,
    pub member_relation: Option<String>,
    pub slot_entities: Vec<String>,
    pub predicate: JudgmentExpr,
}

#[derive(Clone, Debug)]
pub struct ParameterVersion {
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub values: BTreeMap<i64, ScalarValue>,
}

impl ParameterVersion {
    pub fn applies_at(&self, date: NaiveDate) -> bool {
        self.effective_from <= date && self.effective_to.is_none_or(|end| date <= end)
    }
}

#[derive(Clone, Debug)]
pub struct IndexedParameter {
    pub id: Option<String>,
    pub name: String,
    pub unit: Option<String>,
    pub indexed_by: Option<String>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    /// Corpus provision path of the parameter's origin module, for joining
    /// the parameter to its legal source. Descriptive only; never read by
    /// execution.
    pub corpus_citation_path: Option<String>,
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
        self.add_relation_schema(RelationSchema {
            name: name.into(),
            arity,
            derivation: None,
        });
    }

    pub fn add_relation_schema(&mut self, schema: RelationSchema) {
        let name = schema.name.clone();
        self.relations.insert(name, schema);
    }

    pub fn add_parameter(&mut self, parameter: IndexedParameter) {
        self.parameters.insert(parameter.name.clone(), parameter);
    }

    pub fn add_derived(&mut self, derived: Derived) {
        self.derived.insert(derived.name.clone(), derived);
    }

    /// `minor_units` of a declared currency unit by name, or `None` if the unit
    /// is undeclared or is not a currency. Used to resolve a rule's rounding
    /// scale from its `unit` at compile time.
    pub fn currency_minor_units(&self, unit_name: &str) -> Option<u8> {
        match self.units.get(unit_name).map(|unit| &unit.kind) {
            Some(UnitKind::Currency { minor_units }) => Some(*minor_units),
            _ => None,
        }
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
        let input_catalog = self.input_catalog();
        self.resolve_input_name_with_catalog(reference, &input_catalog)
    }

    pub(crate) fn resolve_input_name_with_catalog(
        &self,
        reference: &str,
        input_catalog: &BTreeMap<String, Vec<String>>,
    ) -> Option<String> {
        if !reference.contains('#') {
            return input_catalog
                .get(reference)
                .is_some_and(|request_names| request_names.iter().any(|name| name == reference))
                .then(|| reference.to_string());
        }

        let public_reference = PublicReference::parse(reference)?;
        if let Some(input_name) = public_reference.fragment.strip_prefix("input.") {
            return input_catalog
                .get(input_name)
                .is_some_and(|request_names| request_names.iter().any(|name| name == reference))
                .then(|| input_name.to_string());
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

        None
    }

    pub fn resolve_relation_name(&self, reference: &str) -> Option<String> {
        if self.relations.contains_key(reference) {
            return Some(reference.to_string());
        }
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

    /// Canonical request names for every runtime input slot. Originless
    /// synthesized rules expose the bare slot; atomic rules expose only the
    /// exact owning `<module>#input.<slot>` name. A shared slot may therefore
    /// have multiple allowed request names.
    pub fn input_catalog(&self) -> BTreeMap<String, Vec<String>> {
        let mut catalog: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for derived in self.derived.values() {
            let mut slots = HashSet::new();
            collect_input_slots_from_semantics(&derived.semantics, &mut slots);
            for version in &derived.versions {
                collect_input_slots_from_semantics(&version.semantics, &mut slots);
            }
            for slot in slots {
                let request_name = derived
                    .id
                    .as_deref()
                    .and_then(public_rule_target)
                    .map_or_else(
                        || slot.to_string(),
                        |target| format!("{target}#input.{slot}"),
                    );
                catalog
                    .entry(slot.to_string())
                    .or_default()
                    .insert(request_name);
            }
        }
        for parameter in self.parameters.values() {
            let Some(slot) = parameter.indexed_by.as_deref() else {
                continue;
            };
            let request_name = parameter
                .id
                .as_deref()
                .and_then(public_rule_target)
                .map_or_else(
                    || slot.to_string(),
                    |target| format!("{target}#input.{slot}"),
                );
            catalog
                .entry(slot.to_string())
                .or_default()
                .insert(request_name);
        }
        catalog
            .into_iter()
            .map(|(slot, request_names)| (slot, request_names.into_iter().collect()))
            .collect()
    }
}

struct PublicReference<'a> {
    fragment: &'a str,
}

impl<'a> PublicReference<'a> {
    fn parse(reference: &'a str) -> Option<Self> {
        let (target, fragment) = reference.split_once('#')?;
        if reference != reference.trim()
            || fragment.is_empty()
            || fragment.contains('#')
            || crate::rulespec::validate_module_target(target).is_err()
        {
            return None;
        }
        Some(Self { fragment })
    }
}

fn public_rule_target(id: &str) -> Option<&str> {
    let (target, fragment) = id.split_once('#')?;
    (!fragment.is_empty()).then_some(target)
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
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_input_slots_from_scalar_expr(value, slots);
            if let Some(n) = n {
                collect_input_slots_from_scalar_expr(n, slots);
            }
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
        JudgmentExpr::Derived(_) | JudgmentExpr::RelationMember { .. } => {}
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

#[cfg(test)]
mod rounding_tests {
    use super::{Rounding, RoundingMode};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(value: &str) -> Decimal {
        Decimal::from_str(value).expect("valid decimal")
    }

    /// Round `value` to whole dollars (`minor_units = 0`) under `mode`, the SNAP
    /// case: whole-dollar allotments.
    fn round0(mode: RoundingMode, value: &str) -> String {
        Rounding {
            mode,
            minor_units: 0,
        }
        .apply(dec(value))
        .normalize()
        .to_string()
    }

    /// Round `value` to cents (`minor_units = 2`) under `mode`.
    fn round2(mode: RoundingMode, value: &str) -> String {
        Rounding {
            mode,
            minor_units: 2,
        }
        .apply(dec(value))
        .to_string()
    }

    #[test]
    fn half_up_rounds_midpoint_away_from_zero() {
        // Exact .5 midpoints go away from zero, positive and negative.
        assert_eq!(round0(RoundingMode::HalfUp, "0.5"), "1");
        assert_eq!(round0(RoundingMode::HalfUp, "1.5"), "2");
        assert_eq!(round0(RoundingMode::HalfUp, "2.5"), "3");
        assert_eq!(round0(RoundingMode::HalfUp, "-0.5"), "-1");
        assert_eq!(round0(RoundingMode::HalfUp, "-2.5"), "-3");
        // Off-midpoint rounds to nearest.
        assert_eq!(round0(RoundingMode::HalfUp, "2.4"), "2");
        assert_eq!(round0(RoundingMode::HalfUp, "2.6"), "3");
        assert_eq!(round0(RoundingMode::HalfUp, "-2.4"), "-2");
        // Cents.
        assert_eq!(round2(RoundingMode::HalfUp, "1.005"), "1.01");
        assert_eq!(round2(RoundingMode::HalfUp, "-1.005"), "-1.01");
    }

    #[test]
    fn half_even_rounds_midpoint_to_even() {
        // Banker's rounding: .5 midpoints go to the nearest even digit.
        assert_eq!(round0(RoundingMode::HalfEven, "0.5"), "0");
        assert_eq!(round0(RoundingMode::HalfEven, "1.5"), "2");
        assert_eq!(round0(RoundingMode::HalfEven, "2.5"), "2");
        assert_eq!(round0(RoundingMode::HalfEven, "3.5"), "4");
        assert_eq!(round0(RoundingMode::HalfEven, "-0.5"), "0");
        assert_eq!(round0(RoundingMode::HalfEven, "-2.5"), "-2");
        assert_eq!(round0(RoundingMode::HalfEven, "-3.5"), "-4");
        // Off-midpoint rounds to nearest, same as any mode.
        assert_eq!(round0(RoundingMode::HalfEven, "2.6"), "3");
        assert_eq!(round2(RoundingMode::HalfEven, "1.005"), "1.00");
        assert_eq!(round2(RoundingMode::HalfEven, "1.015"), "1.02");
    }

    #[test]
    fn floor_rounds_toward_negative_infinity() {
        assert_eq!(round0(RoundingMode::Floor, "2.9"), "2");
        assert_eq!(round0(RoundingMode::Floor, "2.5"), "2");
        assert_eq!(round0(RoundingMode::Floor, "2.1"), "2");
        // Negative values go DOWN (more negative), not toward zero.
        assert_eq!(round0(RoundingMode::Floor, "-2.1"), "-3");
        assert_eq!(round0(RoundingMode::Floor, "-2.5"), "-3");
        assert_eq!(round2(RoundingMode::Floor, "1.009"), "1.00");
        assert_eq!(round2(RoundingMode::Floor, "-1.001"), "-1.01");
    }

    #[test]
    fn ceil_rounds_toward_positive_infinity() {
        assert_eq!(round0(RoundingMode::Ceil, "2.1"), "3");
        assert_eq!(round0(RoundingMode::Ceil, "2.5"), "3");
        assert_eq!(round0(RoundingMode::Ceil, "2.9"), "3");
        // Negative values go UP (toward zero) under ceil.
        assert_eq!(round0(RoundingMode::Ceil, "-2.9"), "-2");
        assert_eq!(round0(RoundingMode::Ceil, "-2.5"), "-2");
        assert_eq!(round2(RoundingMode::Ceil, "1.001"), "1.01");
        assert_eq!(round2(RoundingMode::Ceil, "-1.009"), "-1.00");
    }

    #[test]
    fn already_scaled_values_are_unchanged() {
        // A value already at the target scale is a fixed point under every mode.
        for mode in [
            RoundingMode::HalfUp,
            RoundingMode::HalfEven,
            RoundingMode::Floor,
            RoundingMode::Ceil,
        ] {
            assert_eq!(round0(mode, "7"), "7");
            assert_eq!(round2(mode, "7.00"), "7.00");
            assert_eq!(round0(mode, "-7"), "-7");
        }
    }

    #[test]
    fn as_str_round_trips_the_mode_vocabulary() {
        assert_eq!(RoundingMode::HalfUp.as_str(), "half_up");
        assert_eq!(RoundingMode::HalfEven.as_str(), "half_even");
        assert_eq!(RoundingMode::Floor.as_str(), "floor");
        assert_eq!(RoundingMode::Ceil.as_str(), "ceil");
    }
}
