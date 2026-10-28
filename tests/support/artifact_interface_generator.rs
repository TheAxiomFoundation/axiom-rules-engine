//! Choice sequences shrink by deleting choices and replacing them with zero.
//! Expectations are recorded while constructing expressions, independently of
//! the production interface walker and relation-typing implementation.

use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::interface::{DTypeBasis, InputDType, InterfaceInput, ValuesBasis};
use axiom_rules_engine::spec::{DatasetSpec, InputElection, ProgramSpec, ScalarValueSpec};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Demand {
    None,
    Bool,
    Text,
    Date,
    Integer,
    Number,
}

#[derive(Clone, Debug)]
pub enum Context {
    Rule(String),
    Kind(String),
}

#[derive(Clone, Debug)]
pub struct Site {
    pub slot: String,
    pub demand: Demand,
    pub default: Option<ScalarValueSpec>,
    pub parameter: Option<String>,
    pub context: Context,
}

#[derive(Clone, Debug)]
pub struct Generated {
    pub program: ProgramSpec,
    /// Predicates constructed with a term read, atom or term comparison.
    /// Recorded at construction, without inspecting the resulting expression
    /// tree or the production interface.
    pub relation_role_predicates: BTreeSet<String>,
    pub sites: Vec<Site>,
    pub references: Vec<(String, Context)>,
    pub scopes: BTreeMap<String, String>,
    pub ids: BTreeMap<String, String>,
    pub values: BTreeMap<String, ScalarValueSpec>,
    pub parameter_keys: BTreeMap<String, BTreeSet<i64>>,
    pub shapes: BTreeSet<u8>,
}

pub fn literal(kind: &str, value: Value) -> Value {
    json!({"kind":"literal","value":{"kind":kind,"value":value}})
}
pub fn int(value: i64) -> Value {
    literal("integer", json!(value))
}
pub fn boolean(value: bool) -> Value {
    literal("bool", json!(value))
}
pub fn cmp(left: Value, op: &str, right: Value) -> Value {
    json!({"kind":"comparison","left":left,"op":op,"right":right})
}
pub fn add(value: Value) -> Value {
    json!({"kind":"add","items":[value,int(1)]})
}
fn scalar(value: Value) -> ScalarValueSpec {
    serde_json::from_value(value).unwrap()
}

struct Builder<'a> {
    compatible_inputs: bool,
    choices: &'a [u8],
    pos: usize,
    rules: Vec<Value>,
    sites: Vec<Site>,
    references: Vec<(String, Context)>,
    scopes: BTreeMap<String, String>,
    values: BTreeMap<String, ScalarValueSpec>,
    shapes: BTreeSet<u8>,
}
impl Builder<'_> {
    fn pick(&mut self, bound: u8) -> u8 {
        let choice = self.choices.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        choice % bound
    }
    fn read(
        &mut self,
        slot: &str,
        demand: Demand,
        context: Context,
        default: Option<Value>,
        parameter: Option<&str>,
    ) -> Value {
        let value = match demand {
            Demand::Bool => json!({"kind":"bool","value":true}),
            Demand::Text => json!({"kind":"text","value":"a"}),
            Demand::Date => json!({"kind":"date","value":"2026-01-05"}),
            _ => json!({"kind":"integer","value":1}),
        };
        self.values
            .entry(slot.to_string())
            .or_insert_with(|| scalar(value));
        self.sites.push(Site {
            slot: slot.to_string(),
            demand,
            context,
            default: default.clone().map(scalar),
            parameter: parameter.map(str::to_string),
        });
        match default {
            Some(default) => json!({"kind":"input_or_else","name":slot,"default":default}),
            None => json!({"kind":"input","name":slot}),
        }
    }
    fn reference(&mut self, rule: &str, context: Context) -> Value {
        self.references.push((rule.to_string(), context));
        json!({"kind":"derived","name":rule})
    }
    fn rule(&mut self, name: &str, entity: &str, expr: Value, judgment: bool) {
        self.scopes.insert(
            name.to_string(),
            if entity == "Selected" {
                "Household"
            } else {
                entity
            }
            .to_string(),
        );
        self.rules.push(json!({"name":name,"entity":entity,"dtype":if judgment {"judgment"} else {"decimal"},"unit":null,"semantics":if judgment {"judgment"} else {"scalar"},"expr":expr}));
    }
    fn shape(&mut self, shape: u8, ordinal: usize, entity: &str) {
        self.shapes.insert(shape);
        let rule = format!("r{ordinal}");
        let context = Context::Rule(rule.clone());
        // The agreement profile permits conflicting demands. The soundness
        // profile shares only values that satisfy all of their demand rows.
        let shared = self.pick(6) == 0;
        let demand = match shape {
            0 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 => Demand::Number,
            1 => Demand::Bool,
            2 => Demand::Text,
            3 | 15 | 16 | 17 | 18 => Demand::Date,
            4 | 19 | 20 | 21 => Demand::Integer,
            _ => Demand::None,
        };
        let slot = if !shared {
            format!("x{ordinal}")
        } else if self.compatible_inputs {
            match demand {
                Demand::Bool => "shared_bool",
                Demand::Text => "shared_text",
                Demand::Date => "shared_date",
                _ => "shared_number",
            }
            .to_string()
        } else {
            "shared".to_string()
        };
        let parameter = (shape == 4).then_some(if self.pick(2) == 0 {
            "table"
        } else {
            "other_table"
        });
        let default = if self.pick(4) == 0 {
            Some(match demand {
                Demand::Bool => json!({"kind":"bool","value":false}),
                Demand::Text => json!({"kind":"text","value":"b"}),
                Demand::Date => json!({"kind":"date","value":"2025-12-31"}),
                _ => json!({"kind":"integer","value":i64::from(self.pick(2))}),
            })
        } else {
            None
        };
        let value = self.read(&slot, demand, context.clone(), default, parameter);
        let (expr, judgment) = match shape {
            0 => (add(value), false),
            1 => (cmp(value, "eq", boolean(true)), true),
            2 => (cmp(literal("text", json!("a")), "ne", value), true),
            3 => (
                json!({"kind":"date_add_days","date":value,"days":int(1)}),
                false,
            ),
            4 => (
                json!({"kind":"parameter_lookup","parameter":parameter.unwrap(),"index":value}),
                false,
            ),
            5 => (value, false),
            6 => (json!({"kind":"sub","left":value,"right":int(1)}), false),
            7 => (json!({"kind":"mul","left":value,"right":int(2)}), false),
            8 => (json!({"kind":"div","left":value,"right":int(2)}), false),
            9 => (json!({"kind":"max","items":[value,int(2)]}), false),
            10 => (json!({"kind":"min","items":[value,int(2)]}), false),
            11 => (json!({"kind":"ceil","value":value}), false),
            12 => (json!({"kind":"floor","value":value}), false),
            13 => (cmp(value, "gte", literal("decimal", json!("0.5"))), true),
            14 => (cmp(value, "lte", add(int(2))), true),
            15 => (
                json!({"kind":"days_between","from":value,"to":{"kind":"period_end"}}),
                false,
            ),
            16 => (cmp(value, "lt", json!({"kind":"period_start"})), true),
            17 => (
                cmp(value, "gte", literal("date", json!("2026-01-01"))),
                true,
            ),
            18 => (
                cmp(
                    value,
                    "lt",
                    json!({"kind":"date_add_months","date":{"kind":"period_end"},"months":int(1)}),
                ),
                true,
            ),
            19 => (
                json!({"kind":"date_add_days","date":{"kind":"period_start"},"days":value}),
                false,
            ),
            20 => (
                json!({"kind":"date_add_months","date":{"kind":"period_start"},"months":value}),
                false,
            ),
            21 => (
                json!({"kind":"date_add_years","date":{"kind":"period_start"},"years":value}),
                false,
            ),
            22 => (
                json!({"kind":"if","condition":cmp(int(1),"eq",int(1)),"then_expr":value,"else_expr":int(0)}),
                false,
            ),
            23 => {
                let right = self.read(&format!("peer{ordinal}"), Demand::None, context, None, None);
                (cmp(value, "eq", right), true)
            }
            // An ordered boolean comparison always errors: keep this rare.
            24 => (cmp(value, "lt", boolean(true)), true),
            25 => (
                json!({"kind":"if","condition":cmp(int(1),"eq",int(1)),"then_expr":int(1),"else_expr":{"kind":"no_match","subject":value,"patterns":[int(9)]}}),
                false,
            ),
            26 => (
                json!({"kind":"if","condition":cmp(int(1),"eq",int(1)),"then_expr":int(1),"else_expr":{"kind":"over_periods","over":"sum","value":value}}),
                false,
            ),
            27 => (
                cmp(
                    value,
                    "eq",
                    json!({"kind":"parameter_lookup","parameter":"table","index":int(1)}),
                ),
                true,
            ),
            28 => (
                cmp(
                    value,
                    "eq",
                    json!({"kind":"if","condition":cmp(int(1),"eq",int(1)),"then_expr":int(1),"else_expr":int(0)}),
                ),
                true,
            ),
            29 => (
                json!({"kind":"if","condition":cmp(int(1),"eq",int(1)),"then_expr":int(1),"else_expr":{"kind":"over_periods","over":"sum","value":add(value)}}),
                false,
            ),
            _ => unreachable!(),
        };
        self.rule(&rule, entity, expr, judgment);
        // Base and selected version are both potential read sites.
        if self.pick(5) == 1 {
            let version_slot = format!("version{ordinal}");
            let value = self.read(
                &version_slot,
                if judgment {
                    Demand::Bool
                } else {
                    Demand::Number
                },
                Context::Rule(rule),
                None,
                None,
            );
            let expr = if judgment {
                cmp(value, "eq", boolean(true))
            } else {
                add(value)
            };
            self.rules.last_mut().unwrap()["versions"] = json!([{"effective_from":"2024-01-01","semantics":if judgment {"judgment"} else {"scalar"},"expr":expr}]);
        }
    }
}

pub fn generate(choices: &[u8], plain_reads: bool) -> Generated {
    generate_profile(choices, plain_reads, false)
}

pub fn generate_for_soundness(choices: &[u8]) -> Generated {
    generate_profile(choices, false, true)
}

fn generate_profile(choices: &[u8], plain_reads: bool, compatible_inputs: bool) -> Generated {
    let mut relation_role_predicates = BTreeSet::new();
    let mut b = Builder {
        compatible_inputs,
        choices,
        pos: 0,
        rules: vec![],
        sites: vec![],
        references: vec![],
        scopes: BTreeMap::new(),
        values: BTreeMap::new(),
        shapes: BTreeSet::new(),
    };
    let kinds = 2 + usize::from(b.pick(3));
    let physical = ["Household", "Person", "TaxUnit", "Payment"];
    // Every case covers every demand class. Optional shapes cover the table's
    // individual expression positions and use different combinations of slots.
    for (ordinal, shape) in [0, 1, 2, 3, 4, 5].into_iter().enumerate() {
        b.shape(shape, ordinal, physical[ordinal % kinds]);
    }
    let extras = usize::from(b.pick(9));
    for offset in 0..extras {
        let shape = b.pick(30);
        let entity = physical[usize::from(b.pick(kinds as u8))];
        b.shape(shape, 6 + offset, entity);
    }
    let p = b.read(
        "person_amount",
        Demand::Number,
        Context::Rule("person_value".into()),
        None,
        None,
    );
    b.rule("person_value", "Person", add(p), false);
    let flag = b.read(
        "person_flag",
        Demand::Bool,
        Context::Rule("person_ok".into()),
        None,
        None,
    );
    b.rule("person_ok", "Person", cmp(flag, "eq", boolean(true)), true);
    let flag = b.read(
        "household_flag",
        Demand::Bool,
        Context::Rule("household_ok".into()),
        None,
        None,
    );
    b.rule(
        "household_ok",
        "Household",
        cmp(flag, "eq", boolean(true)),
        true,
    );
    let rate = b.read(
        "scalar_amount",
        Demand::Number,
        Context::Rule("scalar_value".into()),
        None,
        None,
    );
    b.rule("scalar_value", "Scalar", add(rate), false);
    let rate = b.reference("scalar_value", Context::Rule("scaled".into()));
    b.rule("scaled", "Household", add(rate), false);
    let scaled = b.reference("scaled", Context::Rule("z_person_scaled".into()));
    b.rule("z_person_scaled", "Person", add(scaled), false);
    if choices.first().copied().unwrap_or(0) & 1 == 1 {
        let value = b.reference("person_value", Context::Rule("cross_kind".into()));
        b.rule("cross_kind", "Household", add(value), false);
    }
    let predicate_age = b.read(
        "predicate_age",
        Demand::Number,
        Context::Kind("Person".into()),
        None,
        None,
    );
    let person_ok = b.reference("person_ok", Context::Kind("Person".into()));
    let household_ok = b.reference("household_ok", Context::Kind("Household".into()));
    let predicate = json!({"kind":"and","items":[person_ok,household_ok,cmp(predicate_age,"gte",int(0)),{"kind":"relation_member","relation":"members","current_slot":1,"related_slot":0}]});
    let sum = b.read(
        "member_amount",
        Demand::Number,
        Context::Kind("Person".into()),
        None,
        None,
    );
    let where_flag = b.read(
        "where_flag",
        Demand::Bool,
        Context::Kind("Person".into()),
        None,
        None,
    );
    b.rule("selected_total","Selected",json!({"kind":"sum_related","relation":"selected_members","current_slot":1,"related_slot":0,"value":sum,"where":cmp(where_flag,"eq",boolean(true))}),false);
    b.rule("selected_count", "Household", json!({"kind":"count_related","relation":"selected_members","current_slot":1,"related_slot":0}), false);
    let selected_value = b.read(
        "selected_value",
        Demand::Number,
        Context::Rule("selected_bonus".into()),
        None,
        None,
    );
    b.rule("selected_bonus", "Selected", add(selected_value), false);
    let reference = b.reference("person_value", Context::Kind("Person".into()));
    b.rule("member_total","Household",json!({"kind":"sum_related","relation":"members","current_slot":1,"related_slot":0,"value":reference}),false);
    b.rule("self_count","Household",json!({"kind":"count_related","relation":"selected_links","current_slot":0,"related_slot":1}),false);
    // `max` and `min` read every member's value but return only the winner,
    // so a whole-number demand on `max` is a number demand on each member;
    // a date demand on `min` carries to each member. `only` returns its one
    // member's value, which keeps the index's whole-number demand. Neither
    // closes a domain through the aggregate. An invalid `0.5` loses the
    // `max` to a valid `1`, so the soundness probes reach a losing member.
    // The `where` clauses of `max` and `only` are read at each member.
    let rank = b.read(
        "member_rank",
        Demand::Number,
        Context::Kind("Person".into()),
        None,
        None,
    );
    let rank_counted = b.read(
        "rank_counted",
        Demand::Bool,
        Context::Kind("Person".into()),
        None,
        None,
    );
    b.rule("greatest_rank","Household",json!({"kind":"parameter_lookup","parameter":"table","index":{"kind":"extreme_related","extreme":"max","relation":"members","current_slot":1,"related_slot":0,"value":rank,"where":cmp(rank_counted,"eq",boolean(true))}}),false);
    let day = b.read(
        "member_day",
        Demand::Date,
        Context::Kind("Person".into()),
        None,
        None,
    );
    let earliest = json!({"kind":"extreme_related","extreme":"min","relation":"members","current_slot":1,"related_slot":0,"value":day});
    b.rule(
        "earliest_day",
        "Household",
        cmp(earliest, "gte", literal("date", json!("2026-01-01"))),
        true,
    );
    let link_rank = b.read(
        "link_rank",
        Demand::Integer,
        Context::Kind("Household".into()),
        None,
        None,
    );
    let link_counted = b.read(
        "link_counted",
        Demand::Bool,
        Context::Kind("Household".into()),
        None,
        None,
    );
    b.rule("linked_rank","Household",json!({"kind":"parameter_lookup","parameter":"table","index":{"kind":"single_related","relation":"links","current_slot":0,"related_slot":1,"value":link_rank,"where":cmp(link_counted,"eq",boolean(true))}}),false);
    let self_flag = b.read(
        "self_flag",
        Demand::Bool,
        Context::Kind("Household".into()),
        None,
        None,
    );
    // Reads through a term run on the term's id: `anchor` on the
    // derivation's current kind, `candidate` on its related kind. Only a
    // Person rule reads this relation, so dense still compiles a Household
    // root.
    let mut anchor_amount = b.read(
        "anchor_amount",
        Demand::Number,
        Context::Kind("Person".into()),
        None,
        None,
    );
    anchor_amount["kind"] = json!("role_input");
    anchor_amount["term"] = json!("anchor");
    relation_role_predicates.insert("member_households".to_string());
    let mut candidate_ok = b.reference("household_ok", Context::Kind("Household".into()));
    candidate_ok["kind"] = json!("role_derived");
    candidate_ok["term"] = json!("candidate");
    let term_predicate =
        json!({"kind":"and","items":[cmp(anchor_amount,"gte",int(0)),candidate_ok]});
    b.rule("person_households","Person",json!({"kind":"count_related","relation":"member_households","current_slot":0,"related_slot":1}),false);
    let relations = json!([
        {"name":"members","arity":2,"slot_entities":["Person","Household"]},
        {"name":"member_households","arity":2,"derivation":{"source_relation":"members","current_slot":0,"related_slot":1,"entity":"Person","slot_entities":["Person","Household"],"predicate":term_predicate}},
        {"name":"selected_members","arity":2,"derivation":{"source_relation":"members","current_slot":1,"related_slot":0,"entity":"Selected","slot_entities":["Person","Household"],"predicate":predicate}},
        {"name":"links","arity":2,"slot_entities":["Household","Household"]},
        {"name":"selected_links","arity":2,"derivation":{"source_relation":"links","current_slot":0,"related_slot":1,"entity":"Household","slot_entities":["Household","Household"],"predicate":cmp(self_flag,"eq",boolean(true))}}
    ]);
    if plain_reads {
        for rule in &mut b.rules {
            remove_defaults(rule);
        }
        for site in &mut b.sites {
            site.default = None;
        }
    }
    let mut declarations = vec![];
    for slot in b.values.keys().cloned().collect::<Vec<_>>() {
        if b.sites
            .iter()
            .filter(|s| s.slot == slot)
            .all(|s| s.default.is_none())
            && b.pick(5) == 1
        {
            declarations.push(json!({"input":slot,"election":{"actor":physical[usize::from(b.pick(kinds as u8))],"provision":"us/regulation/7/273/1/b/3"}}));
        }
    }
    let parameters = json!([
        {"name":"table","indexed_by":"catalog_only","unit":null,"versions":[{"effective_from":"2020-01-01","values":{"1":{"kind":"integer","value":10},"3":{"kind":"integer","value":30}}},{"effective_from":"2024-01-01","values":{"1":{"kind":"integer","value":11},"2":{"kind":"integer","value":20}}}]},
        {"name":"other_table","indexed_by":"other_catalog_only","unit":null,"versions":[{"effective_from":"2020-01-01","values":{"1":{"kind":"integer","value":7},"4":{"kind":"integer","value":40}}}]}
    ]);
    let program=serde_json::from_value(json!({"relations":relations,"parameters":parameters,"derived":b.rules,"input_declarations":declarations})).unwrap();
    let mut ids = BTreeMap::from([("Scalar:1".into(), "Scalar".into())]);
    for kind in &physical[..kinds] {
        for i in 1..=2 {
            ids.insert(format!("{kind}:{i}"), (*kind).into());
        }
    }
    Generated {
        program,
        relation_role_predicates,
        sites: b.sites,
        references: b.references,
        scopes: b.scopes,
        ids,
        values: b.values,
        parameter_keys: BTreeMap::from([
            ("table".into(), BTreeSet::from([1, 2, 3])),
            ("other_table".into(), BTreeSet::from([1, 4])),
        ]),
        shapes: b.shapes,
    }
}

fn remove_defaults(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("kind").and_then(Value::as_str) == Some("input_or_else") {
                object.insert("kind".into(), json!("input"));
                object.remove("default");
            }
            for value in object.values_mut() {
                remove_defaults(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                remove_defaults(value);
            }
        }
        _ => {}
    }
}

impl Generated {
    pub fn expected_inputs(&self) -> Vec<InterfaceInput> {
        let mut kinds = self
            .scopes
            .iter()
            .map(|(rule, kind)| (rule.clone(), BTreeSet::from([kind.clone()])))
            .collect::<BTreeMap<_, _>>();
        loop {
            let mut changed = false;
            for (rule, context) in &self.references {
                let additions = match context {
                    Context::Rule(from) => kinds[from].clone(),
                    Context::Kind(kind) => BTreeSet::from([kind.clone()]),
                };
                for kind in additions {
                    changed |= kinds.get_mut(rule).unwrap().insert(kind);
                }
            }
            if !changed {
                break;
            }
        }
        let declarations = self
            .program
            .input_declarations
            .iter()
            .map(|d| (d.input.as_str(), &d.election))
            .collect::<BTreeMap<_, _>>();
        self.values
            .keys()
            .map(|slot| {
                let sites = self
                    .sites
                    .iter()
                    .filter(|site| &site.slot == slot)
                    .collect::<Vec<_>>();
                let demands = sites
                    .iter()
                    .map(|site| site.demand)
                    .collect::<BTreeSet<_>>();
                let dtype = if demands == BTreeSet::from([Demand::Bool]) {
                    Some(InputDType::Bool)
                } else if demands == BTreeSet::from([Demand::Text]) {
                    Some(InputDType::Text)
                } else if demands == BTreeSet::from([Demand::Date]) {
                    Some(InputDType::Date)
                } else if demands == BTreeSet::from([Demand::Integer]) {
                    Some(InputDType::Integer)
                } else if demands.contains(&Demand::Number)
                    && demands
                        .iter()
                        .all(|d| matches!(d, Demand::Integer | Demand::Number))
                {
                    Some(InputDType::Decimal)
                } else {
                    None
                };
                let default = sites
                    .first()
                    .and_then(|site| site.default.clone())
                    .filter(|first| {
                        sites
                            .iter()
                            .all(|site| site.default.as_ref() == Some(first))
                    });
                let values: Option<Vec<i64>> = sites
                    .iter()
                    .map(|site| {
                        site.parameter
                            .as_ref()
                            .map(|name| self.parameter_keys[name].clone())
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|sets| {
                        sets.into_iter()
                            .flatten()
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect()
                    });
                let entities = sites
                    .iter()
                    .flat_map(|site| match &site.context {
                        Context::Rule(rule) => kinds[rule].clone(),
                        Context::Kind(kind) => BTreeSet::from([kind.clone()]),
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                InterfaceInput {
                    name: slot.clone(),
                    legal_id: slot.clone(),
                    entities,
                    dtype,
                    dtype_basis: dtype.map(|_| DTypeBasis::Usage),
                    default,
                    values_basis: values.as_ref().map(|_| ValuesBasis::ParameterIndex),
                    values,
                    election: declarations
                        .get(slot.as_str())
                        .map(|e| InputElection::clone(e)),
                }
            })
            .collect()
    }
    pub fn query_id(&self, rule: &str) -> String {
        format!("{}:1", self.scopes[rule])
    }
    pub fn dataset(
        &self,
        omit: Option<&str>,
        replacement: Option<(&str, &ScalarValueSpec)>,
    ) -> DatasetSpec {
        let mut inputs = vec![];
        for (slot, value) in &self.values {
            if omit == Some(slot.as_str()) {
                continue;
            }
            let value = replacement
                .filter(|(name, _)| *name == slot)
                .map(|(_, v)| v)
                .unwrap_or(value);
            for (id, kind) in &self.ids {
                inputs.push(json!({"name":slot,"entity":kind,"entity_id":id,"interval":{"start":"2020-01-01","end":"2030-12-31"},"value":value}));
            }
        }
        serde_json::from_value(json!({"inputs":inputs,"relations":[
            {"name":"members","tuple":["Person:1","Household:1"],"interval":{"start":"2020-01-01","end":"2030-12-31"}},
            {"name":"members","tuple":["Person:2","Household:1"],"interval":{"start":"2020-01-01","end":"2030-12-31"}},
            {"name":"links","tuple":["Household:1","Household:2"],"interval":{"start":"2020-01-01","end":"2030-12-31"}}
        ]})).unwrap()
    }
    pub fn dense_batch(&self) -> DenseBatchSpec {
        let columns = |count| {
            self.values
                .iter()
                .map(|(name, v)| (name.clone(), column(v, count)))
                .collect::<HashMap<_, _>>()
        };
        DenseBatchSpec {
            row_count: 1,
            inputs: columns(1),
            relations: HashMap::from([
                (
                    DenseRelationKey {
                        name: "members".into(),
                        current_slot: 1,
                        related_slot: 0,
                    },
                    DenseRelationBatchSpec {
                        offsets: vec![0, 2],
                        inputs: columns(2),
                    },
                ),
                (
                    DenseRelationKey {
                        name: "links".into(),
                        current_slot: 0,
                        related_slot: 1,
                    },
                    DenseRelationBatchSpec {
                        offsets: vec![0, 1],
                        inputs: columns(1),
                    },
                ),
            ]),
        }
    }
}
fn column(value: &ScalarValueSpec, count: usize) -> DenseColumn {
    match value {
        ScalarValueSpec::Bool { value } => DenseColumn::Bool(vec![*value; count]),
        ScalarValueSpec::Integer { value } => DenseColumn::Integer(vec![*value; count]),
        ScalarValueSpec::Decimal { value } => {
            DenseColumn::Decimal(vec![value.parse().unwrap(); count])
        }
        ScalarValueSpec::Text { value } => DenseColumn::Text(vec![value.clone(); count]),
        ScalarValueSpec::Date { value } => DenseColumn::Date(vec![*value; count]),
    }
}
