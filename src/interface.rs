//! The interface block of a format-3 compiled artifact (`program.interface`):
//! the entity roster, the input slots and the relations of a program, derived
//! from the program alone. The compiler writes it and the loader re-derives
//! and compares it (`docs/artifact-format.md`). No evaluator reads it.
//!
//! The derivation is part of format 3 for every program a format-3 engine can
//! already load. A construct older engines refuse to deserialize may extend
//! it for programs using that construct without changing published artifacts.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{
    ComparisonOp, DerivedSemantics, JudgmentExpr, Program, RelatedValueRef, RelationDerivation,
    SCALAR_ENTITY, ScalarExpr, ScalarValue, Term,
};
use crate::relation_typing;
use crate::spec::{InputDeclarationSpec, InputElection, ProgramSpec, ScalarValueSpec, SpecError};

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
#[cfg_attr(feature = "schema", schemars(transform = require_every_key))]
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
#[cfg_attr(feature = "schema", schemars(transform = require_every_key))]
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
#[cfg_attr(feature = "schema", schemars(transform = require_every_key))]
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
    /// Who chooses the value and under which provision, when a module
    /// declares the input an election; null for a fact, which a simulator
    /// must not optimize over.
    #[serde(deserialize_with = "required_nullable")]
    pub election: Option<InputElection>,
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
#[cfg_attr(feature = "schema", schemars(transform = require_every_key))]
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

/// The schema of an object whose every key must be written, a nullable one
/// included ([`required_nullable`]): every property is required, in each
/// variant of a tagged enum too. The property schemas keep their null type.
#[cfg(feature = "schema")]
pub(crate) fn require_every_key(schema: &mut schemars::Schema) {
    fn require(object: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(serde_json::Value::Array(variants)) = object.get_mut("oneOf") {
            for variant in variants {
                if let Some(variant) = variant.as_object_mut() {
                    require(variant);
                }
            }
        }
        let Some(keys) = object
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        else {
            return;
        };
        if let serde_json::Value::Array(required) = object
            .entry("required")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        {
            for key in keys {
                if !required.iter().any(|existing| existing == key.as_str()) {
                    required.push(serde_json::Value::String(key));
                }
            }
        }
    }
    if let Some(object) = schema.as_object_mut() {
        require(object);
    }
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
    derive_from_model(&program.to_program()?, &program.input_declarations)
}

/// Fails only on `declarations`: see [`slot_elections`], and an election
/// some read site defaults with `input_or_else`, since the engine never
/// defaults an election.
pub(crate) fn derive_from_model(
    program: &Program,
    declarations: &[InputDeclarationSpec],
) -> Result<ProgramInterface, SpecError> {
    let mut collector = Collector {
        program,
        slot_kinds: relation_typing::EffectiveSlotEntities::new(program),
        sites: Vec::new(),
        references: Vec::new(),
        lookup_slots: BTreeMap::new(),
        inside_over_periods: false,
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
            relation_typing::predicate_slot_kinds(&collector.slot_kinds, name, derivation);
        let routing = Routing {
            derivation,
            current_kind,
            related_kind: related_kind.clone(),
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
    let election_reads = if declarations.is_empty() {
        BTreeMap::new()
    } else {
        program.input_read_catalog()
    };
    let elections = slot_elections(&election_reads, declarations)?;
    let mut sites_by_slot = BTreeMap::<&str, Vec<&ReadSite>>::new();
    for site in &collector.sites {
        sites_by_slot.entry(&site.slot).or_default().push(site);
    }
    let inputs = sites_by_slot
        .into_iter()
        .map(|(slot, sites)| {
            let election = elections.get(slot);
            if let Some((input, _)) = election
                && sites.iter().any(|site| site.default.is_some())
            {
                return Err(SpecError::InputDeclaration {
                    input: (*input).to_string(),
                    message: format!(
                        "`{slot}` is an election, and `input_or_else` reads it with a default; the engine never defaults an election"
                    ),
                });
            }
            let entities = sites
                .iter()
                .flat_map(|site| flow_kinds(&site.flow))
                .collect::<BTreeSet<_>>();
            let dtype = slot_dtype(&sites);
            let values = slot_values(program, &sites);
            Ok(InterfaceInput {
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
                election: election.map(|(_, election)| (*election).clone()),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Inspect the program once: doing this for every relation makes a long
    // chain quadratic even when it uses no relation-role construct.
    let uses_relation_roles = program.uses_relation_roles();
    let relations = relation_names
        .iter()
        .map(|name| {
            let schema = &program.relations[*name];
            InterfaceRelation {
                name: (*name).clone(),
                arity: schema.arity,
                slot_entities: collector.slot_kinds.get(name).map(<[String]>::to_vec),
                lookup_slots: collector
                    .lookup_slots
                    .get(name.as_str())
                    .map(|slots| slots.iter().copied().collect())
                    .unwrap_or_default(),
                accepts_supplied_tuples: schema.derivation.is_none() || !uses_relation_roles,
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

    let entities = entity_roster(program, &collector.slot_kinds, &inputs);
    Ok(ProgramInterface {
        entities,
        inputs,
        relations,
    })
}

/// Each declared slot's election, with the first declaration naming it.
/// The declarations must be sorted by `input`, each input once, each naming
/// a request name from an expression read site with a well-formed election;
/// the declarations of one slot, from different modules, must agree.
fn slot_elections<'a>(
    catalog: &'a BTreeMap<String, Vec<String>>,
    declarations: &'a [InputDeclarationSpec],
) -> Result<BTreeMap<&'a str, (&'a str, &'a InputElection)>, SpecError> {
    let fail = |declaration: &InputDeclarationSpec, message: String| SpecError::InputDeclaration {
        input: declaration.input.clone(),
        message,
    };
    let slot_of = catalog
        .iter()
        .flat_map(|(slot, request_names)| {
            request_names
                .iter()
                .map(move |request_name| (request_name.as_str(), slot.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut elections = BTreeMap::<&str, (&str, &InputElection)>::new();
    for (index, declaration) in declarations.iter().enumerate() {
        if index > 0 && declarations[index - 1].input >= declaration.input {
            return Err(fail(
                declaration,
                "input declarations must be sorted by `input`, each input once".to_string(),
            ));
        }
        if let Some(message) = declaration.election.shape_error() {
            return Err(fail(declaration, message));
        }
        let Some(slot) = slot_of.get(declaration.input.as_str()) else {
            return Err(fail(
                declaration,
                "names no input the program reads".to_string(),
            ));
        };
        match elections.get(slot) {
            None => {
                elections.insert(slot, (&declaration.input, &declaration.election));
            }
            Some((_, election)) if **election == declaration.election => {}
            Some((first, election)) => {
                return Err(fail(
                    declaration,
                    format!(
                        "its election (actor `{}`, provision `{}`) disagrees with `{first}`'s (actor `{}`, provision `{}`) for the slot `{slot}`",
                        declaration.election.actor,
                        declaration.election.provision,
                        election.actor,
                        election.provision
                    ),
                ));
            }
        }
    }
    Ok(elections)
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
    related_kind: Option<String>,
}

impl Routing<'_> {
    /// The ids a read through `term` runs on: `anchor` is the current slot's
    /// id and `candidate` the related slot's.
    fn term_flow(&self, term: Term) -> Flow {
        Flow::Kind(match term {
            Term::Anchor => self.current_kind.clone(),
            Term::Candidate => self.related_kind.clone(),
        })
    }
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
    /// Every relation's effective slot kinds, resolved once: a chain of n
    /// derived relations costs n steps, not one walk per link.
    slot_kinds: relation_typing::EffectiveSlotEntities<'a>,
    sites: Vec<ReadSite>,
    references: Vec<(String, Flow)>,
    lookup_slots: BTreeMap<String, BTreeSet<usize>>,
    inside_over_periods: bool,
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
            demand: if self.inside_over_periods {
                Demand::None
            } else {
                demand
            },
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
            self.slot_kinds
                .get(relation)
                .and_then(|kinds| kinds.get(related).cloned()),
        )
    }

    /// The value an extremum or `only` reads from each member it selects,
    /// with `demand` on that value, and the selecting `where` clause.
    fn related_value(
        &mut self,
        (relation, current_slot, related_slot): (&str, usize, usize),
        value: &RelatedValueRef,
        where_clause: Option<&JudgmentExpr>,
        demand: Demand,
    ) {
        self.lookup(relation, current_slot);
        let related = self.related_flow(relation, current_slot, related_slot);
        match value {
            RelatedValueRef::Input(name) => self.site(name, &related, demand, None, None),
            RelatedValueRef::Derived(name) => self.reference(name, &related, None),
        }
        if let Some(where_clause) = where_clause {
            self.judgment(where_clause, &related, None);
        }
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
            // The selected-year contract supplies the runtime slot; `input`
            // may be its owning module's full request name.
            ScalarExpr::InputForYear { input, .. } => {
                let slot = self
                    .program
                    .selected_year_inputs
                    .get(input)
                    .map_or(input.as_str(), |declaration| declaration.slot.as_str());
                self.site(slot, flow, demand, None, index_of);
            }
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
            // `only_related` projects a Boolean input of the one related
            // entity.
            ScalarExpr::OnlyRelated {
                relation,
                current_slot,
                related_slot,
                value,
            } => {
                self.lookup(relation, *current_slot);
                let related = self.related_flow(relation, *current_slot, *related_slot);
                match value {
                    RelatedValueRef::Input(name) => {
                        self.site(name, &related, Demand::Bool, None, None);
                    }
                    RelatedValueRef::Derived(name) => self.reference(name, &related, None),
                }
            }
            // `min`/`max` over related entities read every selected member's
            // value and return the winner as it is. A losing value is only
            // compared, so a whole-number demand on the aggregate is a number
            // demand on each member: `min(1.0, 1.5)` is a whole number. Any
            // other demand carries to each member as it is, because the
            // members must all be numbers or all be dates: a member of
            // another type fails the aggregate or makes its winner fail.
            ScalarExpr::ExtremeRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
                ..
            } => {
                let member = match demand {
                    Demand::Integer => Demand::Number,
                    other => other,
                };
                let selection = (relation.as_str(), *current_slot, *related_slot);
                self.related_value(selection, value, where_clause.as_deref(), member);
            }
            // `only` over related entities reads the one selected member's
            // value and returns it as it is, so the value meets the demand
            // placed on the aggregate.
            ScalarExpr::SingleRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => {
                let selection = (relation.as_str(), *current_slot, *related_slot);
                self.related_value(selection, value, where_clause.as_deref(), demand);
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
            // `t.input` and `t.rule` run on the term's id. Relation typing
            // refuses a read through a term anywhere but a derived
            // relation's predicate, where `routing` is set, before an
            // interface is derived.
            ScalarExpr::RoleInput { term, name } => {
                let flow = routing.map_or_else(|| flow.clone(), |routing| routing.term_flow(*term));
                self.site(name, &flow, demand, None, index_of);
            }
            ScalarExpr::RoleDerived { term, name } => {
                let flow = routing.map_or_else(|| flow.clone(), |routing| routing.term_flow(*term));
                self.references.push((name.clone(), flow));
            }
            // Explain refuses an over-periods reduction, and the dense
            // lifetime surface reads its operands across periods, so no
            // position inside one demands a type here.
            ScalarExpr::OverPeriods { value, n, .. } => {
                let outer = self.inside_over_periods;
                self.inside_over_periods = true;
                self.scalar(value, flow, routing, Demand::None, None);
                if let Some(n) = n {
                    self.scalar(n, flow, routing, Demand::None, None);
                }
                self.inside_over_periods = outer;
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
            JudgmentExpr::RelationAtom { relation, terms } => {
                // Every argument is bound: a tuple atom keys on all slots,
                // in the relation's declared tuple order.
                for slot in 0..terms.len() {
                    self.lookup(relation, slot);
                }
            }
            JudgmentExpr::SameEntity { .. } => {}
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, flow, routing);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, flow, routing),
            JudgmentExpr::RoleDerived { term, name } => {
                let flow = routing.map_or_else(|| flow.clone(), |routing| routing.term_flow(*term));
                self.references.push((name.clone(), flow));
            }
        }
    }
}

/// What a comparison demands of one operand, given the other. The reference
/// comparison accepts bool with bool and text with text for `==` and `!=`
/// only, date with date, and two numbers (`compare_scalar_values`).
fn comparison_demand(op: ComparisonOp, other: &ScalarExpr) -> Demand {
    let equality = matches!(op, ComparisonOp::Eq | ComparisonOp::Ne);
    match other {
        ScalarExpr::Literal(ScalarValue::Bool(_)) | ScalarExpr::OnlyRelated { .. } if equality => {
            Demand::Bool
        }
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
        | ScalarExpr::InputForYear { .. }
        | ScalarExpr::Input(_)
        | ScalarExpr::InputOrElse { .. }
        | ScalarExpr::Derived(_)
        | ScalarExpr::RoleInput { .. }
        | ScalarExpr::RoleDerived { .. }
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
        }
        | ScalarExpr::ExtremeRelated {
            relation,
            where_clause,
            ..
        }
        | ScalarExpr::SingleRelated {
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
        ScalarExpr::OnlyRelated { relation, .. } => {
            relations.insert(relation.clone());
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
        JudgmentExpr::Derived(_)
        | JudgmentExpr::RoleDerived { .. }
        | JudgmentExpr::SameEntity { .. } => {}
        JudgmentExpr::RelationMember { relation, .. }
        | JudgmentExpr::RelationAtom { relation, .. } => {
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
fn entity_roster(
    program: &Program,
    slot_kinds: &relation_typing::EffectiveSlotEntities<'_>,
    inputs: &[InterfaceInput],
) -> Vec<InterfaceEntity> {
    let mut kinds = BTreeSet::new();
    kinds.extend(
        program
            .derived
            .values()
            .map(|derived| derived.entity.clone()),
    );
    for name in program.relations.keys() {
        kinds.extend(slot_kinds.get(name).unwrap_or_default().iter().cloned());
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
