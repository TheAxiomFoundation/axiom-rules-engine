use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

pub type PersonId = String;
pub type UnitId = String;
pub type FactId = String;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Citation {
    pub provision: String,
    pub authority: String,
}

impl Citation {
    pub fn new(provision: impl Into<String>, authority: impl Into<String>) -> Self {
        Self {
            provision: provision.into(),
            authority: authority.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Evidence {
    pub id: String,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObservedBool {
    pub value: bool,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationFact {
    pub tuple: Vec<String>,
    pub observation: ObservedBool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationFamilyInput {
    pub name: String,
    pub scope: String,
    /// Evidence that this family is complete for exactly `scope`. Absence is
    /// not completeness; missing tuples remain Unknown.
    pub completeness: Option<Evidence>,
    pub facts: Vec<RelationFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoolFactInput {
    pub name: String,
    pub observations: Vec<ObservedBool>,
    /// A caller can explicitly bind Unknown. Authority defaults apply only to
    /// an absent record, never to this state or to a Conflict.
    pub explicit_unknown: Option<Evidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterInput {
    pub relation: String,
    pub scope: String,
    pub persons: Vec<PersonId>,
    pub completeness: Option<Evidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppliedEntity {
    pub entity_type: String,
    pub id: String,
    pub evidence: Evidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstitutionInput {
    pub roster: RosterInput,
    pub segment: String,
    /// False means facts or parameters change inside the requested period and
    /// the caller must segment before constitution evaluation.
    pub segment_complete: bool,
    pub relation_families: Vec<RelationFamilyInput>,
    pub bool_facts: Vec<BoolFactInput>,
    pub supplied_entities: Vec<SuppliedEntity>,
    pub integrity_constraints: Vec<IntegrityConstraint>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactRef {
    Bool(String),
    Relation { family: String, tuple: Vec<String> },
}

impl FactRef {
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Bool(name) => format!("bool:{name}"),
            Self::Relation { family, tuple } => {
                format!("relation:{family}:{}", tuple.join("\u{1f}"))
            }
        }
    }

    pub(crate) fn named_persons(&self) -> BTreeSet<PersonId> {
        match self {
            Self::Bool(_) => BTreeSet::new(),
            Self::Relation { tuple, .. } => tuple.iter().cloned().collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoolExpr {
    Literal(bool),
    Fact(FactRef),
    Derived(String),
    And(Vec<BoolExpr>),
    Or(Vec<BoolExpr>),
    Not(Box<BoolExpr>),
}

impl BoolExpr {
    pub fn fact(fact: FactRef) -> Self {
        Self::Fact(fact)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stratum {
    Person,
    Candidate,
    Unit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedBool {
    pub id: String,
    pub stratum: Stratum,
    pub expr: BoolExpr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrityConstraint {
    pub id: String,
    pub expr: BoolExpr,
    pub citation: Citation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Base,
    Combination,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeDefeater {
    pub id: String,
    pub when: BoolExpr,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeRule {
    pub id: String,
    pub kind: EdgeKind,
    pub left: PersonId,
    pub right: PersonId,
    pub when: BoolExpr,
    pub citation: Citation,
    /// Guard-level edge defeaters run before the frozen edge set is made.
    pub defeaters: Vec<EdgeDefeater>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CutEdgeDecision {
    /// The named combination provision blocks the cut.
    Blocked { citation: Citation },
    /// The cut crosses the named combination provision.
    Overrides { citation: Citation },
    /// The legal text does not compel either branch. Evaluation ranges over
    /// both, so affected people are Indeterminate unless the result is equal.
    Unresolved { issue: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutEdgePrecedence {
    pub edge_rule: String,
    pub decision: CutEdgeDecision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutOrder {
    pub lower_priority_cut: String,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingElectionPolicy {
    Unknown,
    AuthorityDefault { value: bool, citation: Citation },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElectionRequirement {
    pub fact: String,
    pub actor: String,
    pub missing: MissingElectionPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutRule {
    pub id: String,
    pub members: BTreeSet<PersonId>,
    pub when: BoolExpr,
    pub citation: Citation,
    pub election: Option<ElectionRequirement>,
    pub edge_precedence: Vec<CutEdgePrecedence>,
    /// Explicit cut-vs-cut precedence declarations. Undeclared co-active
    /// overlap is a constitution_overlap_conflict.
    pub precedes: Vec<CutOrder>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRule {
    pub id: String,
    pub members: BTreeSet<PersonId>,
    /// A person identifying the provider candidate before attachments run.
    pub target_anchor: PersonId,
    pub when: BoolExpr,
    pub actor: String,
    pub election: Option<ElectionRequirement>,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarRule {
    pub id: String,
    pub members: BTreeSet<PersonId>,
    pub when: BoolExpr,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRule {
    pub id: String,
    pub person: PersonId,
    pub when: BoolExpr,
    pub citation: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmissionRelations {
    /// Compile-resolved canonical id; the emitter uses this exact string.
    pub unit_constituent: String,
    /// Compile-resolved canonical id; the emitter uses this exact string.
    pub participating_member: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstitutionPlan {
    pub id: String,
    pub entity_type: String,
    pub roster_relation: String,
    pub relations: EmissionRelations,
    pub derived_bools: Vec<DerivedBool>,
    pub edges: Vec<EdgeRule>,
    pub cuts: Vec<CutRule>,
    pub attachments: Vec<AttachmentRule>,
    pub bars: Vec<BarRule>,
    pub statuses: Vec<StatusRule>,
    /// A named, effective, cited § 273.1(c)-style policy. None is the default;
    /// pairwise base-edge chains then remain Indeterminate.
    pub base_chain_policy: Option<Citation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitDerivationConfig {
    pub enabled: bool,
    pub semantics_version: String,
    pub max_admissible_worlds: usize,
}

impl Default for UnitDerivationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            semantics_version: super::EXPERIMENTAL_SEMANTICS_VERSION.to_string(),
            max_admissible_worlds: 4096,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Projection {
    UnitConstituent,
    ParticipatingMember,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectionRole {
    Member,
    Excluded,
    BarredIndependent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    Supplied {
        evidence_id: String,
    },
    Derived {
        constitution: String,
        trace_root: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedUnit {
    pub entity_type: String,
    pub id: UnitId,
    pub members: Vec<PersonId>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipTuple {
    pub relation: String,
    pub unit: UnitId,
    pub person: PersonId,
    pub projection: Projection,
    pub role: ProjectionRole,
    pub citations: BTreeSet<Citation>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndeterminateKind {
    Membership,
    Role(Projection),
    ConstitutionOverlapConflict,
    DiscretionNotEncoded,
    RosterNotAssertedComplete,
    InconsistentInputs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndeterminatePerson {
    pub person: PersonId,
    pub kind: IndeterminateKind,
    pub unresolved_facts: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TraceEvent {
    FrozenEdge {
        rule: String,
        left: String,
        right: String,
    },
    CutApplied {
        rule: String,
        actor: Option<String>,
        request_evidence: BTreeSet<String>,
    },
    CutBlocked {
        cut: String,
        edge_rule: String,
        actor: Option<String>,
        request_evidence: BTreeSet<String>,
    },
    CutOverlapConflict {
        left: String,
        right: String,
    },
    Attachment {
        rule: String,
        target_anchor: String,
        actor: String,
        request_evidence: BTreeSet<String>,
    },
    IndependentBar {
        rule: String,
    },
    StatusExcluded {
        rule: String,
        person: String,
    },
    ElectionDefault {
        fact: String,
        actor: String,
        value: bool,
        citation: Citation,
    },
    DiscretionNotEncoded {
        persons: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationTrace {
    pub root: String,
    pub worlds_evaluated: usize,
    pub events: BTreeSet<TraceEvent>,
    pub completeness_evidence: BTreeSet<String>,
    pub input_conflicts: BTreeSet<String>,
    pub input_conflict_impacts: BTreeSet<InputConflictImpact>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputConflictImpact {
    pub fact: String,
    pub persons: BTreeSet<String>,
    pub projections: BTreeSet<Projection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitDerivationResult {
    pub relations: EmissionRelations,
    pub units: Vec<DerivedUnit>,
    pub memberships: Vec<MembershipTuple>,
    pub indeterminate: Vec<IndeterminatePerson>,
    pub trace: DerivationTrace,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum UnitDerivationError {
    #[error("unit derivation is disabled; enable the runtime flag explicitly")]
    Disabled,
    #[error("unsupported experimental semantics version `{0}`")]
    UnsupportedExperimentalVersion(String),
    #[error("duplicate {namespace} id `{id}`")]
    DuplicateNamespace { namespace: &'static str, id: String },
    #[error("unknown reference `{reference}` from `{from}`")]
    UnknownReference { from: String, reference: String },
    #[error("cyclic dependency: {0}")]
    CyclicDependency(String),
    #[error("constitution dependency reaches unit stratum: {0}")]
    UnitStratumDependency(String),
    #[error("invalid constitution plan: {0}")]
    InvalidPlan(String),
    #[error("evaluation requires segmentation before parameter/fact selection")]
    RequiresSegmentation,
    #[error("constitution has {worlds} admissible worlds, exceeding configured maximum {maximum}")]
    UncheckableWorldSpace { worlds: usize, maximum: usize },
    #[error("supplied and derived membership both bind relation `{0}`; use the shadow channel")]
    SuppliedAndDerivedMembership(String),
    #[error(
        "shadow comparison for fixture `{fixture}` projection {projection:?} has no frozen ledger entry"
    )]
    MissingLedgerEntry {
        fixture: String,
        projection: Projection,
    },
    #[error(
        "entity instance id collision for `{id}`: existing {existing_type}, incoming {incoming_type}"
    )]
    EntityCollision {
        id: String,
        existing_type: String,
        incoming_type: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiftedTruth {
    Holds,
    DoesNotHold,
    Unknown { reasons: BTreeSet<String> },
    Conflict { reasons: BTreeSet<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiftedCandidate {
    pub id: String,
    pub predicate: LiftedTruth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiftedRelation {
    pub members: Vec<String>,
    pub unresolved: BTreeMap<String, LiftedTruth>,
}

/// An unresolved candidate tuple carried across the phase-1/phase-2 barrier.
/// Known members remain ordinary `RelationRecord`s; only Unknown/Conflict
/// candidates live here so the existing closed-world dataset stays unchanged
/// when the prototype feature is disabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationKnowledgeRecord {
    pub name: String,
    pub tuple: Vec<String>,
    pub interval: crate::model::Interval,
    pub truth: LiftedTruth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompleteReduction<T> {
    Determined(T),
    Indeterminate { reasons: BTreeSet<String> },
}

impl LiftedRelation {
    pub fn count_complete(&self) -> CompleteReduction<usize> {
        if self.unresolved.is_empty() {
            CompleteReduction::Determined(self.members.len())
        } else {
            let reasons = self
                .unresolved
                .values()
                .flat_map(|truth| match truth {
                    LiftedTruth::Unknown { reasons } | LiftedTruth::Conflict { reasons } => {
                        reasons.iter().cloned().collect::<Vec<_>>()
                    }
                    LiftedTruth::Holds | LiftedTruth::DoesNotHold => Vec::new(),
                })
                .collect();
            CompleteReduction::Indeterminate { reasons }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppliedMembershipTuple {
    pub relation: String,
    pub unit: String,
    pub person: String,
    pub projection: Projection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerExpectation {
    Equal,
    Different,
    Indeterminate,
    Conflict,
    OutOfPilot { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub fixture: String,
    pub projection: Projection,
    pub expectation: LedgerExpectation,
    pub statutory_basis: Citation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservedComparison {
    Equal,
    Different,
    Indeterminate,
    Conflict,
    OutOfPilot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonResult {
    pub fixture: String,
    pub projection: Projection,
    pub observed: ObservedComparison,
    pub expected: LedgerExpectation,
    pub conforms_to_ledger: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrototypeRun {
    pub derivation: UnitDerivationResult,
    pub comparisons: Vec<ComparisonResult>,
}
