use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

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
    /// Declared entity kind for each tuple position. Empty means the relation
    /// predates slot typing or otherwise leaves its positions undeclared.
    pub slot_entities: Vec<String>,
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("duplicate {namespace} name `{name}`")]
pub struct NamespaceCollisionError {
    pub namespace: &'static str,
    pub name: String,
}

fn insert_unique<T>(
    namespace: &'static str,
    values: &mut HashMap<String, T>,
    name: String,
    value: T,
) -> Result<(), NamespaceCollisionError> {
    match values.entry(name.clone()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(_) => {
            Err(NamespaceCollisionError { namespace, name })
        }
    }
}

impl Program {
    pub fn add_unit(&mut self, unit: UnitDef) -> Result<(), NamespaceCollisionError> {
        insert_unique("unit", &mut self.units, unit.name.clone(), unit)
    }

    pub fn add_relation(
        &mut self,
        name: impl Into<String>,
        arity: usize,
    ) -> Result<(), NamespaceCollisionError> {
        self.add_relation_schema(RelationSchema {
            name: name.into(),
            arity,
            slot_entities: Vec::new(),
            derivation: None,
        })
    }

    pub fn add_relation_schema(
        &mut self,
        schema: RelationSchema,
    ) -> Result<(), NamespaceCollisionError> {
        insert_unique("relation", &mut self.relations, schema.name.clone(), schema)
    }

    pub fn add_parameter(
        &mut self,
        parameter: IndexedParameter,
    ) -> Result<(), NamespaceCollisionError> {
        insert_unique(
            "parameter",
            &mut self.parameters,
            parameter.name.clone(),
            parameter,
        )
    }

    pub fn add_derived(&mut self, derived: Derived) -> Result<(), NamespaceCollisionError> {
        insert_unique(
            "derived rule",
            &mut self.derived,
            derived.name.clone(),
            derived,
        )
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

/// One executable use of a relation, expressed as entity-kind constraints on
/// tuple positions. Unknown positions remain `None`; the declaration is used
/// only as an unordered kind multiset to complete a single missing binary
/// position, never as positional authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelationUsage {
    pub relation: String,
    pub slot_entities: Vec<Option<String>>,
    pub citing_rule: String,
}

/// Consensus executable orientation for a used relation. A position stays
/// unknown when uses do not constrain it or when different uses conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelationUsageOrientation {
    pub slot_entities: Vec<Option<String>>,
}

pub(crate) fn relation_usage_records(program: &Program) -> Vec<RelationUsage> {
    let mut usages = Vec::new();
    let mut derived_names = program.derived.keys().cloned().collect::<Vec<_>>();
    derived_names.sort();

    for name in derived_names {
        let derived = &program.derived[&name];
        let citing_rule = derived.id.as_deref().unwrap_or(&derived.name);
        if derived.versions.is_empty() {
            collect_semantics_relation_usages(
                program,
                &derived.semantics,
                &derived.entity,
                citing_rule,
                &mut usages,
            );
        } else {
            // Runtime version selection ignores the base semantics whenever
            // explicit versions exist; orientation validation must do likewise.
            for version in &derived.versions {
                collect_semantics_relation_usages(
                    program,
                    &version.semantics,
                    &derived.entity,
                    citing_rule,
                    &mut usages,
                );
            }
        }
    }

    // A derived relation executes its source relation and may test membership
    // in another relation inside its predicate. Its declared structural slot
    // kinds describe the two runtime IDs in that predicate context.
    let mut relation_names = program.relations.keys().cloned().collect::<Vec<_>>();
    relation_names.sort();
    for relation_name in relation_names {
        let schema = &program.relations[&relation_name];
        let Some(derivation) = schema.derivation.as_ref() else {
            continue;
        };
        let current_kind = derivation
            .slot_entities
            .get(derivation.current_slot)
            .cloned();
        let related_kind = derivation
            .slot_entities
            .get(derivation.related_slot)
            .cloned();
        let mut stack = Vec::new();
        add_relation_usage(
            program,
            &derivation.source_relation,
            derivation.current_slot,
            derivation.related_slot,
            current_kind.clone(),
            related_kind.clone(),
            &relation_name,
            &mut usages,
            &mut stack,
        );
        collect_judgment_relation_usages(
            program,
            &derivation.predicate,
            related_kind.as_deref(),
            Some((current_kind.as_deref(), related_kind.as_deref())),
            &relation_name,
            &mut usages,
        );
    }

    usages.sort_by(|left, right| {
        left.relation
            .cmp(&right.relation)
            .then_with(|| left.citing_rule.cmp(&right.citing_rule))
            .then_with(|| left.slot_entities.cmp(&right.slot_entities))
    });
    usages.dedup();
    usages
}

pub(crate) fn relation_usage_orientations(
    program: &Program,
) -> BTreeMap<String, RelationUsageOrientation> {
    let mut candidates = BTreeMap::<String, Vec<BTreeSet<String>>>::new();
    for usage in relation_usage_records(program) {
        let slots = candidates
            .entry(usage.relation)
            .or_insert_with(|| vec![BTreeSet::new(); usage.slot_entities.len()]);
        if slots.len() < usage.slot_entities.len() {
            slots.resize_with(usage.slot_entities.len(), BTreeSet::new);
        }
        for (slot, entity) in usage.slot_entities.into_iter().enumerate() {
            if let Some(entity) = entity {
                slots[slot].insert(entity);
            }
        }
    }

    candidates
        .into_iter()
        .map(|(relation, slots)| {
            let slot_entities = slots
                .into_iter()
                .map(|entities| {
                    (entities.len() == 1)
                        .then(|| entities.into_iter().next())
                        .flatten()
                })
                .collect();
            (relation, RelationUsageOrientation { slot_entities })
        })
        .collect()
}

fn collect_semantics_relation_usages(
    program: &Program,
    semantics: &DerivedSemantics,
    entity: &str,
    citing_rule: &str,
    usages: &mut Vec<RelationUsage>,
) {
    match semantics {
        DerivedSemantics::Scalar(expr) => {
            collect_scalar_relation_usages(program, expr, Some(entity), citing_rule, usages);
        }
        DerivedSemantics::Judgment(expr) => {
            collect_judgment_relation_usages(
                program,
                expr,
                Some(entity),
                None,
                citing_rule,
                usages,
            );
        }
    }
}

fn collect_scalar_relation_usages(
    program: &Program,
    expr: &ScalarExpr,
    entity: Option<&str>,
    citing_rule: &str,
    usages: &mut Vec<RelationUsage>,
) {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::Derived(_)
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
        ScalarExpr::ParameterLookup { index, .. }
        | ScalarExpr::Ceil(index)
        | ScalarExpr::Floor(index) => {
            collect_scalar_relation_usages(program, index, entity, citing_rule, usages);
        }
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_relation_usages(program, item, entity, citing_rule, usages);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right) => {
            collect_scalar_relation_usages(program, left, entity, citing_rule, usages);
            collect_scalar_relation_usages(program, right, entity, citing_rule, usages);
        }
        ScalarExpr::DateAddDays { date, days } => {
            collect_scalar_relation_usages(program, date, entity, citing_rule, usages);
            collect_scalar_relation_usages(program, days, entity, citing_rule, usages);
        }
        ScalarExpr::DaysBetween { from, to } => {
            collect_scalar_relation_usages(program, from, entity, citing_rule, usages);
            collect_scalar_relation_usages(program, to, entity, citing_rule, usages);
        }
        ScalarExpr::CountRelated {
            relation,
            current_slot,
            related_slot,
            where_clause,
        } => {
            let current_kind = executable_current_kind(program, relation, *current_slot, entity);
            let related_kind = related_entity_kind(program, None, where_clause.as_deref());
            let mut stack = Vec::new();
            let slots = add_relation_usage(
                program,
                relation,
                *current_slot,
                *related_slot,
                current_kind,
                related_kind,
                citing_rule,
                usages,
                &mut stack,
            );
            if let Some(where_clause) = where_clause {
                collect_judgment_relation_usages(
                    program,
                    where_clause,
                    slots.get(*related_slot).and_then(|kind| kind.as_deref()),
                    None,
                    citing_rule,
                    usages,
                );
            }
        }
        ScalarExpr::SumRelated {
            relation,
            current_slot,
            related_slot,
            value,
            where_clause,
        } => {
            let current_kind = executable_current_kind(program, relation, *current_slot, entity);
            let related_kind = related_entity_kind(program, Some(value), where_clause.as_deref());
            let mut stack = Vec::new();
            let slots = add_relation_usage(
                program,
                relation,
                *current_slot,
                *related_slot,
                current_kind,
                related_kind,
                citing_rule,
                usages,
                &mut stack,
            );
            if let Some(where_clause) = where_clause {
                collect_judgment_relation_usages(
                    program,
                    where_clause,
                    slots.get(*related_slot).and_then(|kind| kind.as_deref()),
                    None,
                    citing_rule,
                    usages,
                );
            }
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_relation_usages(program, condition, entity, None, citing_rule, usages);
            collect_scalar_relation_usages(program, then_expr, entity, citing_rule, usages);
            collect_scalar_relation_usages(program, else_expr, entity, citing_rule, usages);
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_relation_usages(program, value, entity, citing_rule, usages);
            if let Some(n) = n {
                collect_scalar_relation_usages(program, n, entity, citing_rule, usages);
            }
        }
    }
}

fn collect_judgment_relation_usages(
    program: &Program,
    expr: &JudgmentExpr,
    entity: Option<&str>,
    relation_context: Option<(Option<&str>, Option<&str>)>,
    citing_rule: &str,
    usages: &mut Vec<RelationUsage>,
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            // Even without an enclosing entity kind, nested aggregates may
            // independently constrain their related slot through a predicate
            // or value rule. Preserve that partial usage instead of treating
            // the nested relation as unused.
            collect_scalar_relation_usages(program, left, entity, citing_rule, usages);
            collect_scalar_relation_usages(program, right, entity, citing_rule, usages);
        }
        JudgmentExpr::Derived(_) => {}
        JudgmentExpr::RelationMember {
            relation,
            current_slot,
            related_slot,
        } => {
            let (current_kind, related_kind) = relation_context.unwrap_or((entity, None));
            let mut stack = Vec::new();
            add_relation_usage(
                program,
                relation,
                *current_slot,
                *related_slot,
                current_kind.map(str::to_string),
                related_kind.map(str::to_string),
                citing_rule,
                usages,
                &mut stack,
            );
        }
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_relation_usages(
                    program,
                    item,
                    entity,
                    relation_context,
                    citing_rule,
                    usages,
                );
            }
        }
        JudgmentExpr::Not(item) => {
            collect_judgment_relation_usages(
                program,
                item,
                entity,
                relation_context,
                citing_rule,
                usages,
            );
        }
    }
}

fn executable_current_kind(
    program: &Program,
    relation: &str,
    current_slot: usize,
    owner_entity: Option<&str>,
) -> Option<String> {
    let schema = program.relations.get(relation)?;
    if let Some(derivation) = schema.derivation.as_ref()
        && let Some(owner_entity) = owner_entity
        && derivation.entity.as_deref() == Some(owner_entity)
    {
        return derivation.slot_entities.get(current_slot).cloned();
    }
    owner_entity.map(str::to_string)
}

fn related_entity_kind(
    program: &Program,
    value: Option<&RelatedValueRef>,
    where_clause: Option<&JudgmentExpr>,
) -> Option<String> {
    let mut entities = BTreeSet::new();
    if let Some(RelatedValueRef::Derived(name)) = value
        && let Some(derived) = program.derived.get(name)
        && derived.entity != SCALAR_ENTITY
    {
        entities.insert(derived.entity.clone());
    }
    if let Some(where_clause) = where_clause {
        collect_referenced_derived_entities(program, where_clause, &mut entities);
    }
    (entities.len() == 1)
        .then(|| entities.into_iter().next())
        .flatten()
}

fn collect_referenced_derived_entities(
    program: &Program,
    expr: &JudgmentExpr,
    entities: &mut BTreeSet<String>,
) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_derived_entities(program, left, entities);
            collect_scalar_derived_entities(program, right, entities);
        }
        JudgmentExpr::Derived(name) => {
            if let Some(derived) = program.derived.get(name)
                && derived.entity != SCALAR_ENTITY
            {
                entities.insert(derived.entity.clone());
            }
        }
        JudgmentExpr::RelationMember { .. } => {}
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_referenced_derived_entities(program, item, entities);
            }
        }
        JudgmentExpr::Not(item) => {
            collect_referenced_derived_entities(program, item, entities);
        }
    }
}

fn collect_scalar_derived_entities(
    program: &Program,
    expr: &ScalarExpr,
    entities: &mut BTreeSet<String>,
) {
    match expr {
        ScalarExpr::Derived(name) => {
            if let Some(derived) = program.derived.get(name)
                && derived.entity != SCALAR_ENTITY
            {
                entities.insert(derived.entity.clone());
            }
        }
        ScalarExpr::ParameterLookup { index, .. }
        | ScalarExpr::Ceil(index)
        | ScalarExpr::Floor(index) => {
            collect_scalar_derived_entities(program, index, entities);
        }
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_derived_entities(program, item, entities);
            }
        }
        ScalarExpr::Sub(left, right)
        | ScalarExpr::Mul(left, right)
        | ScalarExpr::Div(left, right) => {
            collect_scalar_derived_entities(program, left, entities);
            collect_scalar_derived_entities(program, right, entities);
        }
        ScalarExpr::DateAddDays { date, days } => {
            collect_scalar_derived_entities(program, date, entities);
            collect_scalar_derived_entities(program, days, entities);
        }
        ScalarExpr::DaysBetween { from, to } => {
            collect_scalar_derived_entities(program, from, entities);
            collect_scalar_derived_entities(program, to, entities);
        }
        // A nested aggregate's predicate and value execute on that aggregate's
        // related ID, not on the enclosing predicate's entity. Its own usage
        // traversal handles those references after the enclosing orientation
        // is established.
        ScalarExpr::CountRelated { .. } | ScalarExpr::SumRelated { .. } => {}
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_referenced_derived_entities(program, condition, entities);
            collect_scalar_derived_entities(program, then_expr, entities);
            collect_scalar_derived_entities(program, else_expr, entities);
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_derived_entities(program, value, entities);
            if let Some(n) = n {
                collect_scalar_derived_entities(program, n, entities);
            }
        }
        ScalarExpr::Literal(_)
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn add_relation_usage(
    program: &Program,
    relation: &str,
    current_slot: usize,
    related_slot: usize,
    current_kind: Option<String>,
    related_kind: Option<String>,
    citing_rule: &str,
    usages: &mut Vec<RelationUsage>,
    stack: &mut Vec<String>,
) -> Vec<Option<String>> {
    let Some(schema) = program.relations.get(relation) else {
        return Vec::new();
    };
    let mut slot_entities = vec![None; schema.arity];
    if current_slot < schema.arity {
        slot_entities[current_slot] = current_kind;
    }
    if related_slot < schema.arity {
        slot_entities[related_slot] = related_kind;
    }
    complete_single_unknown_slot(&schema.slot_entities, &mut slot_entities);
    usages.push(RelationUsage {
        relation: relation.to_string(),
        slot_entities: slot_entities.clone(),
        citing_rule: citing_rule.to_string(),
    });

    if stack.iter().any(|item| item == relation) {
        return slot_entities;
    }
    let Some(derivation) = schema.derivation.as_ref() else {
        return slot_entities;
    };
    stack.push(relation.to_string());
    add_relation_usage(
        program,
        &derivation.source_relation,
        derivation.current_slot,
        derivation.related_slot,
        slot_entities.get(current_slot).cloned().flatten(),
        slot_entities.get(related_slot).cloned().flatten(),
        citing_rule,
        usages,
        stack,
    );
    stack.pop();
    slot_entities
}

fn complete_single_unknown_slot(
    declared_entities: &[String],
    slot_entities: &mut [Option<String>],
) {
    if declared_entities.len() != slot_entities.len() {
        return;
    }
    let unknown_slots = slot_entities
        .iter()
        .enumerate()
        .filter_map(|(slot, entity)| entity.is_none().then_some(slot))
        .collect::<Vec<_>>();
    if unknown_slots.len() != 1 {
        return;
    }
    let mut remaining = declared_entities.to_vec();
    for entity in slot_entities.iter().flatten() {
        let Some(position) = remaining.iter().position(|candidate| candidate == entity) else {
            return;
        };
        remaining.remove(position);
    }
    if remaining.len() == 1 {
        slot_entities[unknown_slots[0]] = remaining.pop();
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
