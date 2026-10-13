//! The interface block of a format-3 compiled artifact (`program.interface`):
//! the entity roster, the input slots and the relations of a program, derived
//! from the program alone. The compiler writes it and the loader re-derives
//! and compares it (`docs/artifact-format.md`). No evaluator reads it.
//!
//! The derivation is part of format 3: changing what any program's interface
//! contains is a new format version.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{
    ComparisonOp, DerivedSemantics, JudgmentExpr, Program, RelatedValueRef, RelationDerivation,
    SCALAR_ENTITY, ScalarExpr, ScalarValue,
};
use crate::relation_typing;
use crate::spec::{ProgramSpec, ScalarValueSpec, SpecError};

/// `program.interface`. Every list is sorted except a relation's
/// `slot_entities`, which keeps tuple order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProgramInterface {
    /// One entry per entity kind the program can evaluate on, by kind.
    pub entities: Vec<InterfaceEntity>,
    /// One entry per input slot some expression reads, by name.
    pub inputs: Vec<InterfaceInput>,
    /// One entry per declared relation, by name.
    pub relations: Vec<InterfaceRelation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceEntity {
    pub kind: String,
    /// `<kind>:{index}`, counting from 1, for a supplied kind; the template of
    /// the aliased kind for an alias; null for `Scalar` and for an alias whose
    /// kind the program does not establish.
    #[serde(deserialize_with = "required_nullable")]
    pub id_template: Option<String>,
    pub instances: EntityInstances,
}

/// Where a kind's instances come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum EntityInstances {
    /// The caller supplies the instances.
    Supplied,
    /// A filtered entity whose ids are the ids of kind `of` (null when the
    /// program does not establish it); `via` names the derived relations
    /// that filter to it.
    Alias {
        of: Option<String>,
        via: Vec<String>,
    },
    /// The entity-free scope, `Scalar`.
    Unscoped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceInput {
    /// The runtime slot.
    pub name: String,
    /// The slot's `canonical_request_name` in `metadata.input_catalog`.
    pub legal_id: String,
    /// Every entity kind whose ids a read site of the slot can be evaluated
    /// on.
    pub entities: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub dtype: Option<InputDType>,
    #[serde(deserialize_with = "required_nullable")]
    pub dtype_basis: Option<DTypeBasis>,
    /// The literal every read site defaults to, when they all agree.
    #[serde(deserialize_with = "required_nullable")]
    pub default: Option<ScalarValueSpec>,
    /// A closed domain of whole numbers.
    #[serde(deserialize_with = "required_nullable")]
    pub values: Option<Vec<i64>>,
    #[serde(deserialize_with = "required_nullable")]
    pub values_basis: Option<ValuesBasis>,
}

/// The narrowest type every read site of a slot demands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputDType {
    Bool,
    Text,
    Date,
    /// A whole number: a tagged integer, or a decimal with zero fraction.
    Integer,
    /// A number: a tagged integer or a decimal.
    Decimal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DTypeBasis {
    /// Inferred from the read sites.
    Usage,
    /// Reserved for the day RuleSpec declares input types.
    Declared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ValuesBasis {
    /// The table keys of the parameters every read site indexes.
    ParameterIndex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceRelation {
    pub name: String,
    pub arity: usize,
    /// The effective slot kinds, in tuple order; null for a relation that
    /// declares none.
    #[serde(deserialize_with = "required_nullable")]
    pub slot_entities: Option<Vec<String>>,
    /// The slots aggregates and membership tests key on, with a derived
    /// relation's traversal of its source counted against the source.
    pub lookup_slots: Vec<usize>,
    /// Whether the evaluator reads tuples supplied under the relation's own
    /// name.
    pub accepts_supplied_tuples: bool,
    pub provenance: RelationProvenance,
}

/// What the declaration makes a relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationProvenance {
    /// A data relation.
    Supplied,
    /// A `derived_relation`.
    Derived {
        /// The id of the rule that derives it.
        rule: String,
        source_relation: String,
        reads: RelationReads,
    },
}

/// What a derived relation's predicate mentions directly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RelationReads {
    /// Input slots.
    pub inputs: Vec<String>,
    /// Rule ids (the public id, or the name of a rule without one).
    pub rules: Vec<String>,
    /// Relation names.
    pub relations: Vec<String>,
}

/// A key whose value may be null but must be written: absence is an error.
pub(crate) fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Read an internally tagged object whose keys must be exactly `tag` plus
/// the keys its variant lists in `variants`.
fn tagged_object<'de, D: Deserializer<'de>>(
    deserializer: D,
    tag: &'static str,
    variants: &'static [(&'static str, &'static [&'static str])],
) -> Result<(String, serde_json::Map<String, serde_json::Value>), D::Error> {
    let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
    let variant = match object.remove(tag) {
        Some(serde_json::Value::String(variant)) => variant,
        Some(_) => return Err(D::Error::custom(format!("`{tag}` must be a string"))),
        None => return Err(D::Error::missing_field(tag)),
    };
    let Some((_, keys)) = variants.iter().find(|(name, _)| *name == variant) else {
        let expected = variants
            .iter()
            .map(|(name, _)| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(D::Error::custom(format!(
            "unknown {tag} `{variant}`, expected one of {expected}"
        )));
    };
    if let Some(unknown) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(D::Error::unknown_field(unknown, keys));
    }
    if let Some(missing) = keys.iter().find(|key| !object.contains_key(**key)) {
        return Err(D::Error::missing_field(missing));
    }
    Ok((variant, object))
}

fn field<T: serde::de::DeserializeOwned, E: serde::de::Error>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<T, E> {
    let value = object.remove(key).unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|error| E::custom(format!("`{key}`: {error}")))
}

impl<'de> Deserialize<'de> for EntityInstances {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (variant, mut object) = tagged_object(
            deserializer,
            "source",
            &[
                ("supplied", &[]),
                ("alias", &["of", "via"]),
                ("unscoped", &[]),
            ],
        )?;
        Ok(match variant.as_str() {
            "supplied" => Self::Supplied,
            "alias" => Self::Alias {
                of: field::<Option<String>, D::Error>(&mut object, "of")?,
                via: field::<Vec<String>, D::Error>(&mut object, "via")?,
            },
            _ => Self::Unscoped,
        })
    }
}

impl<'de> Deserialize<'de> for RelationProvenance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (variant, mut object) = tagged_object(
            deserializer,
            "kind",
            &[
                ("supplied", &[]),
                ("derived", &["rule", "source_relation", "reads"]),
            ],
        )?;
        Ok(match variant.as_str() {
            "supplied" => Self::Supplied,
            _ => Self::Derived {
                rule: field::<String, D::Error>(&mut object, "rule")?,
                source_relation: field::<String, D::Error>(&mut object, "source_relation")?,
                reads: field::<RelationReads, D::Error>(&mut object, "reads")?,
            },
        })
    }
}

/// Derive the interface of `program`. Deterministic: the same program always
/// yields the same interface.
pub fn derive_interface(program: &ProgramSpec) -> Result<ProgramInterface, SpecError> {
    Ok(derive_from_model(&program.to_program()?))
}

pub(crate) fn derive_from_model(program: &Program) -> ProgramInterface {
    let mut collector = Collector {
        program,
        sites: Vec::new(),
        references: Vec::new(),
        lookup_slots: BTreeMap::new(),
    };
    let mut rule_names = program.derived.keys().collect::<Vec<_>>();
    rule_names.sort();
    for name in &rule_names {
        let derived = &program.derived[*name];
        let flow = Flow::Rule(derived.name.clone());
        for semantics in std::iter::once(&derived.semantics)
            .chain(derived.versions.iter().map(|version| &version.semantics))
        {
            match semantics {
                DerivedSemantics::Scalar(expr) => {
                    collector.scalar(expr, &flow, None, Demand::None, None);
                }
                DerivedSemantics::Judgment(expr) => collector.judgment(expr, &flow, None),
            }
        }
    }
    let mut relation_names = program.relations.keys().collect::<Vec<_>>();
    relation_names.sort();
    let mut predicate_reads = BTreeMap::new();
    for name in &relation_names {
        let Some(derivation) = &program.relations[*name].derivation else {
            continue;
        };
        // A derived relation traverses its source keyed on its current slot.
        collector.lookup(&derivation.source_relation, derivation.current_slot);
        let (current_kind, related_kind) =
            relation_typing::predicate_slot_kinds(program, name, derivation);
        let routing = Routing {
            derivation,
            current_kind,
        };
        let flow = Flow::Kind(related_kind);
        let first_site = collector.sites.len();
        let first_reference = collector.references.len();
        collector.judgment(&derivation.predicate, &flow, Some(&routing));
        let mut relations = BTreeSet::new();
        collect_judgment_relations(&derivation.predicate, &mut relations);
        predicate_reads.insert(
            (*name).clone(),
            RelationReads {
                inputs: collector.sites[first_site..]
                    .iter()
                    .map(|site| site.slot.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                rules: collector.references[first_reference..]
                    .iter()
                    .map(|(rule, _)| program.public_derived_key(rule))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                relations: relations.into_iter().collect(),
            },
        );
    }

    let kinds = rule_kinds(program, &collector.references);
    let flow_kinds = |flow: &Flow| -> BTreeSet<String> {
        match flow {
            Flow::Rule(rule) => kinds.get(rule).cloned().unwrap_or_default(),
            Flow::Kind(kind) => kind.iter().cloned().collect(),
        }
    };

    let catalog = program.input_catalog();
    let mut sites_by_slot = BTreeMap::<&str, Vec<&ReadSite>>::new();
    for site in &collector.sites {
        sites_by_slot.entry(&site.slot).or_default().push(site);
    }
    let inputs = sites_by_slot
        .into_iter()
        .map(|(slot, sites)| {
            let entities = sites
                .iter()
                .flat_map(|site| flow_kinds(&site.flow))
                .collect::<BTreeSet<_>>();
            let dtype = slot_dtype(&sites);
            let values = slot_values(program, &sites);
            InterfaceInput {
                name: slot.to_string(),
                legal_id: catalog
                    .get(slot)
                    .map(|request_names| {
                        crate::compile::canonical_request_name(slot, request_names).to_string()
                    })
                    .unwrap_or_else(|| slot.to_string()),
                entities: entities.into_iter().collect(),
                dtype,
                dtype_basis: dtype.map(|_| DTypeBasis::Usage),
                default: slot_default(&sites),
                values_basis: values.as_ref().map(|_| ValuesBasis::ParameterIndex),
                values,
            }
        })
        .collect::<Vec<_>>();

    let relations = relation_names
        .iter()
        .map(|name| {
            let schema = &program.relations[*name];
            InterfaceRelation {
                name: (*name).clone(),
                arity: schema.arity,
                slot_entities: relation_typing::effective_slot_entities(program, name),
                lookup_slots: collector
                    .lookup_slots
                    .get(name.as_str())
                    .map(|slots| slots.iter().copied().collect())
                    .unwrap_or_default(),
                accepts_supplied_tuples: true,
                provenance: match &schema.derivation {
                    None => RelationProvenance::Supplied,
                    Some(derivation) => RelationProvenance::Derived {
                        rule: (*name).clone(),
                        source_relation: derivation.source_relation.clone(),
                        reads: predicate_reads
                            .remove(name.as_str())
                            .unwrap_or(RelationReads {
                                inputs: Vec::new(),
                                rules: Vec::new(),
                                relations: Vec::new(),
                            }),
                    },
                },
            }
        })
        .collect::<Vec<_>>();

    let entities = entity_roster(program, &inputs);
    ProgramInterface {
        entities,
        inputs,
        relations,
    }
}

/// The kind of the id an expression is evaluated on: every kind its
/// enclosing rule can be evaluated on, or one kind fixed by a relation slot
/// (`None` when the relation declares no kinds, so nothing evaluates it).
#[derive(Clone, Debug)]
enum Flow {
    Rule(String),
    Kind(Option<String>),
}

/// Inside a derived relation's predicate, a referenced rule runs on the
/// current id when its entity is the derivation's declared current kind.
struct Routing<'a> {
    derivation: &'a RelationDerivation,
    current_kind: Option<String>,
}

/// What a read site's position demands of the value it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Demand {
    None,
    Bool,
    Text,
    Date,
    Integer,
    Number,
}

struct ReadSite {
    slot: String,
    flow: Flow,
    demand: Demand,
    /// The default of an `input_or_else`; `None` for a plain read.
    default: Option<ScalarValue>,
    /// The parameter this site is the index of.
    index_of: Option<String>,
}

struct Collector<'a> {
    program: &'a Program,
    sites: Vec<ReadSite>,
    references: Vec<(String, Flow)>,
    lookup_slots: BTreeMap<String, BTreeSet<usize>>,
}

impl Collector<'_> {
    fn lookup(&mut self, relation: &str, current_slot: usize) {
        if self.program.relations.contains_key(relation) {
            self.lookup_slots
                .entry(relation.to_string())
                .or_default()
                .insert(current_slot);
        }
    }

    fn site(
        &mut self,
        slot: &str,
        flow: &Flow,
        demand: Demand,
        default: Option<&ScalarValue>,
        index_of: Option<&str>,
    ) {
        self.sites.push(ReadSite {
            slot: slot.to_string(),
            flow: flow.clone(),
            demand,
            default: default.cloned(),
            index_of: index_of.map(str::to_string),
        });
    }

    fn reference(&mut self, rule: &str, flow: &Flow, routing: Option<&Routing<'_>>) {
        let flow = match (routing, self.program.derived.get(rule)) {
            (Some(routing), Some(derived))
                if relation_typing::predicate_routes_to_current(
                    routing.derivation,
                    &derived.entity,
                ) =>
            {
                Flow::Kind(routing.current_kind.clone())
            }
            _ => flow.clone(),
        };
        self.references.push((rule.to_string(), flow));
    }

    /// The ids an aggregation's `where` clause and summed value run on.
    fn related_flow(&self, relation: &str, current_slot: usize, related_slot: usize) -> Flow {
        let (_, related) =
            relation_typing::traversed_slots(self.program, relation, current_slot, related_slot);
        Flow::Kind(
            relation_typing::effective_slot_entities(self.program, relation)
                .and_then(|kinds| kinds.get(related).cloned()),
        )
    }

    fn scalar(
        &mut self,
        expr: &ScalarExpr,
        flow: &Flow,
        routing: Option<&Routing<'_>>,
        demand: Demand,
        index_of: Option<&str>,
    ) {
        match expr {
            ScalarExpr::Literal(_) | ScalarExpr::PeriodStart | ScalarExpr::PeriodEnd => {}
            ScalarExpr::Input(name) => self.site(name, flow, demand, None, index_of),
            ScalarExpr::InputOrElse { name, default } => {
                self.site(name, flow, demand, Some(default), index_of);
            }
            ScalarExpr::Derived(name) => self.reference(name, flow, routing),
            ScalarExpr::ParameterLookup { parameter, index } => {
                self.scalar(index, flow, routing, Demand::Integer, Some(parameter));
            }
            ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
                for item in items {
                    self.scalar(item, flow, routing, Demand::Number, None);
                }
            }
            ScalarExpr::Sub(left, right)
            | ScalarExpr::Mul(left, right)
            | ScalarExpr::Div(left, right) => {
                self.scalar(left, flow, routing, Demand::Number, None);
                self.scalar(right, flow, routing, Demand::Number, None);
            }
            ScalarExpr::Ceil(value) | ScalarExpr::Floor(value) => {
                self.scalar(value, flow, routing, Demand::Number, None);
            }
            ScalarExpr::DateAddDays { date, days: offset }
            | ScalarExpr::DateAddMonths {
                date,
                months: offset,
            }
            | ScalarExpr::DateAddYears {
                date,
                years: offset,
            } => {
                self.scalar(date, flow, routing, Demand::Date, None);
                self.scalar(offset, flow, routing, Demand::Integer, None);
            }
            ScalarExpr::DaysBetween { from, to } => {
                self.scalar(from, flow, routing, Demand::Date, None);
                self.scalar(to, flow, routing, Demand::Date, None);
            }
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => {
                self.lookup(relation, *current_slot);
                let related = self.related_flow(relation, *current_slot, *related_slot);
                if let Some(where_clause) = where_clause {
                    self.judgment(where_clause, &related, None);
                }
            }
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                self.lookup(relation, *current_slot);
                let related = self.related_flow(relation, *current_slot, *related_slot);
                match value {
                    RelatedValueRef::Input(name) => {
                        self.site(name, &related, Demand::Number, None, None);
                    }
                    RelatedValueRef::Derived(name) => self.reference(name, &related, None),
                }
                if let Some(where_clause) = where_clause {
                    self.judgment(where_clause, &related, None);
                }
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, flow, routing);
                self.scalar(then_expr, flow, routing, Demand::None, None);
                self.scalar(else_expr, flow, routing, Demand::None, None);
            }
            ScalarExpr::NoMatch { subject, patterns } => {
                self.scalar(subject, flow, routing, Demand::None, None);
                for pattern in patterns {
                    self.scalar(pattern, flow, routing, Demand::None, None);
                }
            }
            // Explain refuses an over-periods reduction, and the dense
            // lifetime surface reads its operands across periods, so no
            // position inside one demands a type here.
            ScalarExpr::OverPeriods { value, n, .. } => {
                self.scalar(value, flow, routing, Demand::None, None);
                if let Some(n) = n {
                    self.scalar(n, flow, routing, Demand::None, None);
                }
            }
        }
    }

    fn judgment(&mut self, expr: &JudgmentExpr, flow: &Flow, routing: Option<&Routing<'_>>) {
        match expr {
            JudgmentExpr::Comparison { left, op, right } => {
                self.scalar(left, flow, routing, comparison_demand(*op, right), None);
                self.scalar(right, flow, routing, comparison_demand(*op, left), None);
            }
            JudgmentExpr::Derived(name) => self.reference(name, flow, routing),
            JudgmentExpr::RelationMember {
                relation,
                current_slot,
                ..
            } => self.lookup(relation, *current_slot),
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, flow, routing);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, flow, routing),
        }
    }
}

/// What a comparison demands of one operand, given the other. The reference
/// comparison accepts bool with bool and text with text for `==` and `!=`
/// only, date with date, and two numbers (`compare_scalar_values`).
fn comparison_demand(op: ComparisonOp, other: &ScalarExpr) -> Demand {
    let equality = matches!(op, ComparisonOp::Eq | ComparisonOp::Ne);
    match other {
        ScalarExpr::Literal(ScalarValue::Bool(_)) if equality => Demand::Bool,
        ScalarExpr::Literal(ScalarValue::Text(_)) if equality => Demand::Text,
        ScalarExpr::Literal(ScalarValue::Date(_))
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd
        | ScalarExpr::DateAddDays { .. }
        | ScalarExpr::DateAddMonths { .. }
        | ScalarExpr::DateAddYears { .. } => Demand::Date,
        ScalarExpr::Literal(ScalarValue::Integer(_) | ScalarValue::Decimal(_))
        | ScalarExpr::Add(_)
        | ScalarExpr::Sub(..)
        | ScalarExpr::Mul(..)
        | ScalarExpr::Div(..)
        | ScalarExpr::Max(_)
        | ScalarExpr::Min(_)
        | ScalarExpr::Ceil(_)
        | ScalarExpr::Floor(_)
        | ScalarExpr::CountRelated { .. }
        | ScalarExpr::SumRelated { .. }
        | ScalarExpr::DaysBetween { .. } => Demand::Number,
        _ => Demand::None,
    }
}

/// `K(R)` for every rule: the least sets holding each rule's own scope and,
/// for every reference to a rule, the kinds of the id that reference
/// evaluates on.
fn rule_kinds(
    program: &Program,
    references: &[(String, Flow)],
) -> BTreeMap<String, BTreeSet<String>> {
    let id_kinds = relation_typing::filtered_entity_kinds(program);
    let mut kinds = program
        .derived
        .values()
        .map(|derived| {
            let scope = if derived.entity == SCALAR_ENTITY {
                SCALAR_ENTITY.to_string()
            } else {
                id_kinds
                    .get(&derived.entity)
                    .cloned()
                    .unwrap_or_else(|| derived.entity.clone())
            };
            (derived.name.clone(), BTreeSet::from([scope]))
        })
        .collect::<BTreeMap<_, _>>();
    loop {
        let mut changed = false;
        for (rule, flow) in references {
            let added = match flow {
                Flow::Rule(from) => kinds.get(from).cloned().unwrap_or_default(),
                Flow::Kind(kind) => kind.iter().cloned().collect(),
            };
            if let Some(target) = kinds.get_mut(rule) {
                for kind in added {
                    changed |= target.insert(kind);
                }
            }
        }
        if !changed {
            return kinds;
        }
    }
}

fn slot_dtype(sites: &[&ReadSite]) -> Option<InputDType> {
    let demands = sites
        .iter()
        .map(|site| site.demand)
        .collect::<BTreeSet<_>>();
    let only = |demand| demands.len() == 1 && demands.contains(&demand);
    if only(Demand::Bool) {
        Some(InputDType::Bool)
    } else if only(Demand::Text) {
        Some(InputDType::Text)
    } else if only(Demand::Date) {
        Some(InputDType::Date)
    } else if only(Demand::Integer) {
        Some(InputDType::Integer)
    } else if demands.contains(&Demand::Number)
        && demands
            .iter()
            .all(|demand| matches!(demand, Demand::Integer | Demand::Number))
    {
        Some(InputDType::Decimal)
    } else {
        None
    }
}

fn slot_default(sites: &[&ReadSite]) -> Option<ScalarValueSpec> {
    let first = sites.first()?.default.as_ref()?;
    sites
        .iter()
        .all(|site| site.default.as_ref() == Some(first))
        .then(|| ScalarValueSpec::from_model(first.clone()))
}

fn slot_values(program: &Program, sites: &[&ReadSite]) -> Option<Vec<i64>> {
    let mut keys = BTreeSet::new();
    for site in sites {
        let parameter = site.index_of.as_deref()?;
        if let Some(parameter) = program.parameters.get(parameter) {
            for version in &parameter.versions {
                keys.extend(version.values.keys().copied());
            }
        }
    }
    Some(keys.into_iter().collect())
}

fn collect_scalar_relations(expr: &ScalarExpr, relations: &mut BTreeSet<String>) {
    match expr {
        ScalarExpr::Literal(_)
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::Derived(_)
        | ScalarExpr::PeriodStart
        | ScalarExpr::PeriodEnd => {}
        ScalarExpr::ParameterLookup { index: value, .. }
        | ScalarExpr::Ceil(value)
        | ScalarExpr::Floor(value) => collect_scalar_relations(value, relations),
        ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
            for item in items {
                collect_scalar_relations(item, relations);
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
            collect_scalar_relations(left, relations);
            collect_scalar_relations(right, relations);
        }
        ScalarExpr::CountRelated {
            relation,
            where_clause,
            ..
        }
        | ScalarExpr::SumRelated {
            relation,
            where_clause,
            ..
        } => {
            relations.insert(relation.clone());
            if let Some(where_clause) = where_clause {
                collect_judgment_relations(where_clause, relations);
            }
        }
        ScalarExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_judgment_relations(condition, relations);
            collect_scalar_relations(then_expr, relations);
            collect_scalar_relations(else_expr, relations);
        }
        ScalarExpr::NoMatch { subject, patterns } => {
            collect_scalar_relations(subject, relations);
            for pattern in patterns {
                collect_scalar_relations(pattern, relations);
            }
        }
        ScalarExpr::OverPeriods { value, n, .. } => {
            collect_scalar_relations(value, relations);
            if let Some(n) = n {
                collect_scalar_relations(n, relations);
            }
        }
    }
}

fn collect_judgment_relations(expr: &JudgmentExpr, relations: &mut BTreeSet<String>) {
    match expr {
        JudgmentExpr::Comparison { left, right, .. } => {
            collect_scalar_relations(left, relations);
            collect_scalar_relations(right, relations);
        }
        JudgmentExpr::Derived(_) => {}
        JudgmentExpr::RelationMember { relation, .. } => {
            relations.insert(relation.clone());
        }
        JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
            for item in items {
                collect_judgment_relations(item, relations);
            }
        }
        JudgmentExpr::Not(item) => collect_judgment_relations(item, relations),
    }
}

/// One entry per kind a derived rule declares, a relation's effective slots
/// hold, or a derived relation filters to, and `Scalar` when an input lists
/// it.
fn entity_roster(program: &Program, inputs: &[InterfaceInput]) -> Vec<InterfaceEntity> {
    let mut kinds = BTreeSet::new();
    kinds.extend(
        program
            .derived
            .values()
            .map(|derived| derived.entity.clone()),
    );
    for name in program.relations.keys() {
        kinds.extend(relation_typing::effective_slot_entities(program, name).unwrap_or_default());
    }
    kinds.extend(relation_typing::filtered_entities(program));
    if inputs
        .iter()
        .any(|input| input.entities.iter().any(|kind| kind == SCALAR_ENTITY))
    {
        kinds.insert(SCALAR_ENTITY.to_string());
    }

    let mut aliases = relation_typing::filtered_entity_names(program);
    aliases.extend(relation_typing::unresolved_filtered_entities(program));
    let aliased_kinds = relation_typing::filtered_entity_kinds(program);
    let mut via = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, schema) in &program.relations {
        if let Some(entity) = schema
            .derivation
            .as_ref()
            .and_then(|derivation| derivation.entity.as_ref())
        {
            via.entry(entity.clone()).or_default().insert(name.clone());
        }
    }
    let template = |kind: &str| format!("{kind}:{{index}}");
    kinds
        .into_iter()
        .map(|kind| {
            let (id_template, instances) = if kind == SCALAR_ENTITY {
                (None, EntityInstances::Unscoped)
            } else if aliases.contains(&kind) {
                let of = aliased_kinds.get(&kind).cloned();
                (
                    of.as_deref().map(template),
                    EntityInstances::Alias {
                        of,
                        via: via
                            .get(&kind)
                            .map(|names| names.iter().cloned().collect())
                            .unwrap_or_default(),
                    },
                )
            } else {
                (Some(template(&kind)), EntityInstances::Supplied)
            };
            InterfaceEntity {
                kind,
                id_template,
                instances,
            }
        })
        .collect()
}
