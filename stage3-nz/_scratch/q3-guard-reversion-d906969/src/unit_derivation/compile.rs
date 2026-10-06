use std::collections::{BTreeMap, BTreeSet};

use super::types::*;

#[derive(Clone, Debug)]
pub struct CompiledConstitution {
    pub(crate) plan: ConstitutionPlan,
    pub(crate) semantics_digest: [u8; 32],
    pub(crate) derived_bools: BTreeMap<String, DerivedBool>,
    pub(crate) edges: BTreeMap<String, EdgeRule>,
    pub(crate) cuts: BTreeMap<String, CutRule>,
    pub(crate) attachments: BTreeMap<String, AttachmentRule>,
    pub(crate) bars: BTreeMap<String, BarRule>,
    pub(crate) statuses: BTreeMap<String, StatusRule>,
}

impl CompiledConstitution {
    pub fn semantics_digest(&self) -> [u8; 32] {
        self.semantics_digest
    }
}

fn insert_unique<T>(
    namespace: &'static str,
    values: &mut BTreeMap<String, T>,
    id: String,
    value: T,
) -> Result<(), UnitDerivationError> {
    match values.entry(id.clone()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(_) => {
            Err(UnitDerivationError::DuplicateNamespace { namespace, id })
        }
    }
}

pub fn compile(plan: ConstitutionPlan) -> Result<CompiledConstitution, UnitDerivationError> {
    if plan.id.is_empty() || plan.entity_type.is_empty() || plan.roster_relation.is_empty() {
        return Err(UnitDerivationError::InvalidPlan(
            "constitution id, entity type, and roster relation must be non-empty".to_string(),
        ));
    }
    if plan.relations.unit_constituent.is_empty()
        || plan.relations.participating_member.is_empty()
        || plan.relations.unit_constituent == plan.relations.participating_member
    {
        return Err(UnitDerivationError::InvalidPlan(
            "the two compile-resolved projection relation ids must be non-empty and distinct"
                .to_string(),
        ));
    }

    let mut derived_bools = BTreeMap::new();
    for derived in &plan.derived_bools {
        insert_unique(
            "derived constitution fact",
            &mut derived_bools,
            derived.id.clone(),
            derived.clone(),
        )?;
    }

    let mut edges = BTreeMap::new();
    for edge in &plan.edges {
        if edge.left == edge.right {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "edge `{}` is a self-edge",
                edge.id
            )));
        }
        let mut defeaters = BTreeMap::new();
        for defeater in &edge.defeaters {
            insert_unique(
                "edge defeater",
                &mut defeaters,
                defeater.id.clone(),
                defeater,
            )?;
        }
        insert_unique("edge rule", &mut edges, edge.id.clone(), edge.clone())?;
    }

    let mut cuts = BTreeMap::new();
    for cut in &plan.cuts {
        if cut.members.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "cut `{}` has no members",
                cut.id
            )));
        }
        let mut edge_precedence = BTreeMap::new();
        for precedence in &cut.edge_precedence {
            insert_unique(
                "cut-edge precedence",
                &mut edge_precedence,
                precedence.edge_rule.clone(),
                precedence,
            )?;
        }
        let mut cut_order = BTreeMap::new();
        for order in &cut.precedes {
            insert_unique(
                "cut precedence",
                &mut cut_order,
                order.lower_priority_cut.clone(),
                order,
            )?;
        }
        insert_unique("cut rule", &mut cuts, cut.id.clone(), cut.clone())?;
    }

    let mut attachments = BTreeMap::new();
    for attachment in &plan.attachments {
        if attachment.members.is_empty()
            || attachment.members.contains(&attachment.target_anchor)
            || attachment.actor.is_empty()
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "attachment `{}` must have an actor and a non-empty source disjoint from its target anchor",
                attachment.id
            )));
        }
        if let Some(election) = &attachment.election
            && attachment.actor != election.actor
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "attachment `{}` actor `{}` disagrees with election actor `{}`",
                attachment.id, attachment.actor, election.actor
            )));
        }
        insert_unique(
            "attachment rule",
            &mut attachments,
            attachment.id.clone(),
            attachment.clone(),
        )?;
    }

    let mut bars = BTreeMap::new();
    for bar in &plan.bars {
        if bar.members.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "bar `{}` has no members",
                bar.id
            )));
        }
        insert_unique("bar rule", &mut bars, bar.id.clone(), bar.clone())?;
    }

    let mut statuses = BTreeMap::new();
    for status in &plan.statuses {
        insert_unique(
            "status rule",
            &mut statuses,
            status.id.clone(),
            status.clone(),
        )?;
    }

    validate_exprs(&plan, &derived_bools)?;
    validate_elections(&plan, &derived_bools)?;
    validate_precedence(&edges, &cuts)?;
    validate_attachment_confluence(&attachments)?;

    let semantics_digest = super::evaluate::sha256(&canonical_plan_encoding(&plan));

    Ok(CompiledConstitution {
        plan,
        semantics_digest,
        derived_bools,
        edges,
        cuts,
        attachments,
        bars,
        statuses,
    })
}

/// Normative stage-2 compiled-plan encoding. Every item is tag-delimited and
/// length-prefixed; set-like operation collections are sorted by their stable
/// ids before encoding, so caller record order cannot affect identity.
fn canonical_plan_encoding(plan: &ConstitutionPlan) -> Vec<u8> {
    let mut encoder = CanonicalEncoder::new(b"axiom.unit-derivation.semantics.stage2\0");
    encoder.string(&plan.id);
    encoder.string(&plan.entity_type);
    encoder.string(&plan.roster_relation);
    encoder.string(&plan.relations.unit_constituent);
    encoder.string(&plan.relations.participating_member);

    let mut derived = plan.derived_bools.iter().collect::<Vec<_>>();
    derived.sort_by_key(|item| item.id.as_str());
    encoder.count(derived.len());
    for item in derived {
        encoder.string(&item.id);
        encoder.byte(match item.stratum {
            Stratum::Person => 0,
            Stratum::Candidate => 1,
            Stratum::Unit => 2,
        });
        encoder.expr(&item.expr);
    }

    let mut edges = plan.edges.iter().collect::<Vec<_>>();
    edges.sort_by_key(|item| item.id.as_str());
    encoder.count(edges.len());
    for item in edges {
        encoder.string(&item.id);
        encoder.byte(match item.kind {
            EdgeKind::Base => 0,
            EdgeKind::Combination => 1,
        });
        let (left, right) = if item.left <= item.right {
            (&item.left, &item.right)
        } else {
            (&item.right, &item.left)
        };
        encoder.string(left);
        encoder.string(right);
        encoder.expr(&item.when);
        encoder.citation(&item.citation);
        let mut defeaters = item.defeaters.iter().collect::<Vec<_>>();
        defeaters.sort_by_key(|defeater| defeater.id.as_str());
        encoder.count(defeaters.len());
        for defeater in defeaters {
            encoder.string(&defeater.id);
            encoder.expr(&defeater.when);
            encoder.citation(&defeater.citation);
        }
    }

    let mut cuts = plan.cuts.iter().collect::<Vec<_>>();
    cuts.sort_by_key(|item| item.id.as_str());
    encoder.count(cuts.len());
    for item in cuts {
        encoder.string(&item.id);
        encoder.strings(item.members.iter());
        encoder.expr(&item.when);
        encoder.citation(&item.citation);
        encoder.election(item.election.as_ref());
        let mut precedence = item.edge_precedence.iter().collect::<Vec<_>>();
        precedence.sort_by_key(|entry| entry.edge_rule.as_str());
        encoder.count(precedence.len());
        for entry in precedence {
            encoder.string(&entry.edge_rule);
            match &entry.decision {
                CutEdgeDecision::Blocked { citation } => {
                    encoder.byte(0);
                    encoder.citation(citation);
                }
                CutEdgeDecision::Overrides { citation } => {
                    encoder.byte(1);
                    encoder.citation(citation);
                }
                CutEdgeDecision::Unresolved { issue } => {
                    encoder.byte(2);
                    encoder.string(issue);
                }
            }
        }
        let mut orders = item.precedes.iter().collect::<Vec<_>>();
        orders.sort_by_key(|order| order.lower_priority_cut.as_str());
        encoder.count(orders.len());
        for order in orders {
            encoder.string(&order.lower_priority_cut);
            encoder.citation(&order.citation);
        }
    }

    let mut attachments = plan.attachments.iter().collect::<Vec<_>>();
    attachments.sort_by_key(|item| item.id.as_str());
    encoder.count(attachments.len());
    for item in attachments {
        encoder.string(&item.id);
        encoder.strings(item.members.iter());
        encoder.string(&item.target_anchor);
        encoder.expr(&item.when);
        encoder.string(&item.actor);
        encoder.election(item.election.as_ref());
        encoder.citation(&item.citation);
    }

    let mut bars = plan.bars.iter().collect::<Vec<_>>();
    bars.sort_by_key(|item| item.id.as_str());
    encoder.count(bars.len());
    for item in bars {
        encoder.string(&item.id);
        encoder.strings(item.members.iter());
        encoder.expr(&item.when);
        encoder.citation(&item.citation);
    }

    let mut statuses = plan.statuses.iter().collect::<Vec<_>>();
    statuses.sort_by_key(|item| item.id.as_str());
    encoder.count(statuses.len());
    for item in statuses {
        encoder.string(&item.id);
        encoder.string(&item.person);
        encoder.expr(&item.when);
        encoder.citation(&item.citation);
    }

    match &plan.base_chain_policy {
        Some(citation) => {
            encoder.byte(1);
            encoder.citation(citation);
        }
        None => encoder.byte(0),
    }
    encoder.finish()
}

struct CanonicalEncoder {
    bytes: Vec<u8>,
}

impl CanonicalEncoder {
    fn new(domain: &[u8]) -> Self {
        Self {
            bytes: domain.to_vec(),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn count(&mut self, value: usize) {
        self.bytes.extend_from_slice(&(value as u32).to_be_bytes());
    }

    fn string(&mut self, value: &str) {
        self.count(value.len());
        self.bytes.extend_from_slice(value.as_bytes());
    }

    fn strings<'a>(&mut self, values: impl Iterator<Item = &'a String>) {
        let values = values.collect::<Vec<_>>();
        self.count(values.len());
        for value in values {
            self.string(value);
        }
    }

    fn citation(&mut self, citation: &Citation) {
        self.string(&citation.provision);
        self.string(&citation.authority);
    }

    fn election(&mut self, election: Option<&ElectionRequirement>) {
        let Some(election) = election else {
            self.byte(0);
            return;
        };
        self.byte(1);
        self.string(&election.fact);
        self.string(&election.actor);
        match &election.missing {
            MissingElectionPolicy::Unknown => self.byte(0),
            MissingElectionPolicy::AuthorityDefault { value, citation } => {
                self.byte(1);
                self.byte(u8::from(*value));
                self.citation(citation);
            }
        }
    }

    fn expr(&mut self, expr: &BoolExpr) {
        match expr {
            BoolExpr::Literal(value) => {
                self.byte(0);
                self.byte(u8::from(*value));
            }
            BoolExpr::Fact(FactRef::Bool(name)) => {
                self.byte(1);
                self.string(name);
            }
            BoolExpr::Fact(FactRef::Relation { family, tuple }) => {
                self.byte(2);
                self.string(family);
                self.strings(tuple.iter());
            }
            BoolExpr::Derived(name) => {
                self.byte(3);
                self.string(name);
            }
            BoolExpr::And(items) => {
                self.byte(4);
                self.count(items.len());
                for item in items {
                    self.expr(item);
                }
            }
            BoolExpr::Or(items) => {
                self.byte(5);
                self.count(items.len());
                for item in items {
                    self.expr(item);
                }
            }
            BoolExpr::Not(item) => {
                self.byte(6);
                self.expr(item);
            }
        }
    }
}

fn validate_elections(
    plan: &ConstitutionPlan,
    derived: &BTreeMap<String, DerivedBool>,
) -> Result<(), UnitDerivationError> {
    let mut policies = BTreeMap::<String, (String, MissingElectionPolicy)>::new();
    let requirements = plan
        .cuts
        .iter()
        .filter_map(|rule| {
            rule.election
                .as_ref()
                .map(|item| (&rule.id, &rule.when, item))
        })
        .chain(plan.attachments.iter().filter_map(|rule| {
            rule.election
                .as_ref()
                .map(|item| (&rule.id, &rule.when, item))
        }));
    for (rule, when, election) in requirements {
        if election.actor.is_empty() || election.fact.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "election on `{rule}` must name its actor and fact"
            )));
        }
        let fact = FactRef::Bool(election.fact.clone());
        if !expr_fact_refs(when, derived).contains(&fact) {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "election fact `{}` on `{rule}` is not consumed by its guard",
                election.fact
            )));
        }
        match policies.entry(election.fact.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert((election.actor.clone(), election.missing.clone()));
            }
            std::collections::btree_map::Entry::Occupied(slot)
                if slot.get() != &(election.actor.clone(), election.missing.clone()) =>
            {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "election fact `{}` has conflicting missing-evidence policies",
                    election.fact
                )));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    Ok(())
}

fn validate_exprs(
    plan: &ConstitutionPlan,
    derived: &BTreeMap<String, DerivedBool>,
) -> Result<(), UnitDerivationError> {
    let mut roots: Vec<(&str, &BoolExpr)> = Vec::new();
    roots.extend(plan.edges.iter().map(|rule| (rule.id.as_str(), &rule.when)));
    for rule in &plan.edges {
        roots.extend(
            rule.defeaters
                .iter()
                .map(|item| (item.id.as_str(), &item.when)),
        );
    }
    roots.extend(plan.cuts.iter().map(|rule| (rule.id.as_str(), &rule.when)));
    roots.extend(
        plan.attachments
            .iter()
            .map(|rule| (rule.id.as_str(), &rule.when)),
    );
    roots.extend(plan.bars.iter().map(|rule| (rule.id.as_str(), &rule.when)));
    roots.extend(
        plan.statuses
            .iter()
            .map(|rule| (rule.id.as_str(), &rule.when)),
    );

    for (root, expr) in roots {
        let mut visiting = Vec::new();
        validate_expr(root, expr, derived, &mut visiting)?;
    }
    for rule in derived.values() {
        let mut visiting = vec![rule.id.clone()];
        validate_expr(&rule.id, &rule.expr, derived, &mut visiting)?;
    }
    Ok(())
}

fn validate_expr(
    root: &str,
    expr: &BoolExpr,
    derived: &BTreeMap<String, DerivedBool>,
    visiting: &mut Vec<String>,
) -> Result<(), UnitDerivationError> {
    match expr {
        BoolExpr::Literal(_) | BoolExpr::Fact(_) => Ok(()),
        BoolExpr::Derived(reference) => {
            let rule =
                derived
                    .get(reference)
                    .ok_or_else(|| UnitDerivationError::UnknownReference {
                        from: root.to_string(),
                        reference: reference.clone(),
                    })?;
            if let Some(position) = visiting.iter().position(|item| item == reference) {
                let mut cycle = visiting[position..].to_vec();
                cycle.push(reference.clone());
                return Err(UnitDerivationError::CyclicDependency(cycle.join(" -> ")));
            }
            let mut path = visiting.clone();
            path.push(reference.clone());
            if rule.stratum == Stratum::Unit {
                return Err(UnitDerivationError::UnitStratumDependency(
                    path.join(" -> "),
                ));
            }
            visiting.push(reference.clone());
            validate_expr(root, &rule.expr, derived, visiting)?;
            visiting.pop();
            Ok(())
        }
        BoolExpr::And(items) | BoolExpr::Or(items) => {
            for item in items {
                validate_expr(root, item, derived, visiting)?;
            }
            Ok(())
        }
        BoolExpr::Not(item) => validate_expr(root, item, derived, visiting),
    }
}

fn validate_precedence(
    edges: &BTreeMap<String, EdgeRule>,
    cuts: &BTreeMap<String, CutRule>,
) -> Result<(), UnitDerivationError> {
    for cut in cuts.values() {
        let declarations = cut
            .edge_precedence
            .iter()
            .map(|item| (item.edge_rule.as_str(), &item.decision))
            .collect::<BTreeMap<_, _>>();
        for edge in edges.values() {
            if edge.kind != EdgeKind::Combination {
                continue;
            }
            let crosses = cut.members.contains(&edge.left) != cut.members.contains(&edge.right);
            if crosses && !declarations.contains_key(edge.id.as_str()) {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "cut `{}` can cross combination edge `{}` but declares no explicit block, override, or unresolved legal branch",
                    cut.id, edge.id
                )));
            }
        }
        for declaration in &cut.edge_precedence {
            let edge = edges.get(&declaration.edge_rule).ok_or_else(|| {
                UnitDerivationError::UnknownReference {
                    from: cut.id.clone(),
                    reference: declaration.edge_rule.clone(),
                }
            })?;
            if edge.kind != EdgeKind::Combination {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "cut `{}` declares blocking precedence against non-combination edge `{}`",
                    cut.id, edge.id
                )));
            }
        }
        for order in &cut.precedes {
            if !cuts.contains_key(&order.lower_priority_cut) {
                return Err(UnitDerivationError::UnknownReference {
                    from: cut.id.clone(),
                    reference: order.lower_priority_cut.clone(),
                });
            }
        }
    }

    let graph = cuts
        .iter()
        .map(|(id, cut)| {
            (
                id.clone(),
                cut.precedes
                    .iter()
                    .map(|order| order.lower_priority_cut.clone())
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for id in graph.keys() {
        let mut visiting = Vec::new();
        validate_order_dag(id, &graph, &mut visiting, &mut BTreeSet::new())?;
    }

    let rules = cuts.values().collect::<Vec<_>>();
    for (index, left) in rules.iter().enumerate() {
        for right in rules.iter().skip(index + 1) {
            if left.members != right.members {
                continue;
            }
            let ordered = order_reaches(&graph, &left.id, &right.id)
                || order_reaches(&graph, &right.id, &left.id);
            for edge in edges.values().filter(|edge| {
                edge.kind == EdgeKind::Combination
                    && (left.members.contains(&edge.left) != left.members.contains(&edge.right))
            }) {
                let left_decision = left
                    .edge_precedence
                    .iter()
                    .find(|item| item.edge_rule == edge.id)
                    .expect("crossing precedence validated");
                let right_decision = right
                    .edge_precedence
                    .iter()
                    .find(|item| item.edge_rule == edge.id)
                    .expect("crossing precedence validated");
                if cut_decision_class(&left_decision.decision)
                    != cut_decision_class(&right_decision.decision)
                    && !ordered
                {
                    return Err(UnitDerivationError::InvalidPlan(format!(
                        "coalescible cuts `{}` and `{}` contradict each other for combination edge `{}` without explicit cut precedence",
                        left.id, right.id, edge.id
                    )));
                }
            }
        }
    }
    Ok(())
}

fn cut_decision_class(decision: &CutEdgeDecision) -> u8 {
    match decision {
        CutEdgeDecision::Blocked { .. } => 0,
        CutEdgeDecision::Overrides { .. } => 1,
        CutEdgeDecision::Unresolved { .. } => 2,
    }
}

fn order_reaches(graph: &BTreeMap<String, BTreeSet<String>>, from: &str, target: &str) -> bool {
    let mut pending = vec![from.to_string()];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        for next in graph.get(&id).into_iter().flatten() {
            if next == target {
                return true;
            }
            pending.push(next.clone());
        }
    }
    false
}

fn validate_order_dag(
    id: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    visiting: &mut Vec<String>,
    complete: &mut BTreeSet<String>,
) -> Result<(), UnitDerivationError> {
    if complete.contains(id) {
        return Ok(());
    }
    if let Some(position) = visiting.iter().position(|item| item == id) {
        let mut cycle = visiting[position..].to_vec();
        cycle.push(id.to_string());
        return Err(UnitDerivationError::CyclicDependency(cycle.join(" -> ")));
    }
    visiting.push(id.to_string());
    for next in graph.get(id).into_iter().flatten() {
        validate_order_dag(next, graph, visiting, complete)?;
    }
    visiting.pop();
    complete.insert(id.to_string());
    Ok(())
}

fn validate_attachment_confluence(
    attachments: &BTreeMap<String, AttachmentRule>,
) -> Result<(), UnitDerivationError> {
    let rules = attachments.values().collect::<Vec<_>>();
    for (index, left) in rules.iter().enumerate() {
        for right in rules.iter().skip(index + 1) {
            if !left.members.is_disjoint(&right.members)
                || left.members.contains(&right.target_anchor)
                || right.members.contains(&left.target_anchor)
            {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "attachments `{}` and `{}` can disagree without declared ordering",
                    left.id, right.id
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn expr_fact_refs(
    expr: &BoolExpr,
    derived: &BTreeMap<String, DerivedBool>,
) -> BTreeSet<FactRef> {
    let mut facts = BTreeSet::new();
    collect_expr_fact_refs(expr, derived, &mut facts, &mut BTreeSet::new());
    facts
}

fn collect_expr_fact_refs(
    expr: &BoolExpr,
    derived: &BTreeMap<String, DerivedBool>,
    facts: &mut BTreeSet<FactRef>,
    visiting: &mut BTreeSet<String>,
) {
    match expr {
        BoolExpr::Literal(_) => {}
        BoolExpr::Fact(fact) => {
            facts.insert(fact.clone());
        }
        BoolExpr::Derived(reference) => {
            if visiting.insert(reference.clone()) {
                if let Some(rule) = derived.get(reference) {
                    collect_expr_fact_refs(&rule.expr, derived, facts, visiting);
                }
                visiting.remove(reference);
            }
        }
        BoolExpr::And(items) | BoolExpr::Or(items) => {
            for item in items {
                collect_expr_fact_refs(item, derived, facts, visiting);
            }
        }
        BoolExpr::Not(item) => collect_expr_fact_refs(item, derived, facts, visiting),
    }
}
