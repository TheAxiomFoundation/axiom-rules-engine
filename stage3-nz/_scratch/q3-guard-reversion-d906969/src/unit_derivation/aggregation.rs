use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use super::types::*;

pub const COMPILED_AGGREGATION_ARTIFACT_FORMAT: &str = "axiom/compiled-unit-aggregation-stage3/1";

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationPlan {
    pub schema: String,
    pub id: String,
    pub decimal_precision: u32,
    pub constitution: AggregationConstitution,
    #[serde(default)]
    pub inputs: Vec<AggregationInput>,
    pub membership_relations: Vec<AggregationMembershipRelation>,
    pub partner_relation: String,
    pub child_relation: String,
    #[serde(default)]
    pub partner_presence: PartnerPresenceRule,
    #[serde(default)]
    pub adult_selection: AdultSelectionRule,
    #[serde(default)]
    pub age_18_conditions: Age18Conditions,
    #[serde(default)]
    pub care: CareSemantics,
    #[serde(default)]
    pub scalar_aggregations: Vec<ScalarAggregation>,
    #[serde(default)]
    pub child_counts: Vec<ChildCount>,
    #[serde(default)]
    pub child_minima: Vec<ChildMinimum>,
    #[serde(default)]
    pub family_predicates: Vec<FamilyPredicate>,
    #[serde(default)]
    pub family_shape_scalars: Vec<FamilyShapeScalar>,
    #[serde(default)]
    pub child_agreements: Vec<ChildAgreement>,
    #[serde(default)]
    pub child_projections: Vec<ChildProjection>,
    #[serde(default)]
    pub broadcasts: Vec<ChildBroadcast>,
    #[serde(default)]
    pub family_reductions: Vec<FamilyReduction>,
    #[serde(default)]
    pub limitations: Vec<DeclaredLimitation>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationConstitution {
    pub id: String,
    pub entity_type: String,
    pub roster_relation: String,
    pub unit_constituent_relation: String,
    pub participating_member_relation: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AggregationInputScope {
    Adult,
    Child,
    Family,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AggregationInputKind {
    AdditiveAmount,
    ChildGrossAmount,
    FamilyAdjustment,
    FamilyAmount,
    DecimalRate,
    CareFraction,
    EligibilityBoolean,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationInput {
    pub name: String,
    pub scope: AggregationInputScope,
    pub kind: AggregationInputKind,
    #[serde(default)]
    pub reduction_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_computation: Option<EngineComputationBinding>,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EngineComputationBinding {
    pub rule_id: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineComputationStage {
    Gross,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationDirection {
    Directed,
    Symmetric,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    Caregiver,
    Child,
    Partner,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationMembershipRelation {
    pub relation: String,
    #[serde(default)]
    pub direction: Option<RelationDirection>,
    #[serde(default)]
    pub symmetric: Option<bool>,
    #[serde(default)]
    pub left_role: Option<MembershipRole>,
    #[serde(default)]
    pub right_role: Option<MembershipRole>,
    pub citation: AggregationCitation,
}

impl AggregationMembershipRelation {
    fn effective_direction(&self) -> Option<RelationDirection> {
        self.direction.or_else(|| {
            self.symmetric.map(|value| {
                if value {
                    RelationDirection::Symmetric
                } else {
                    RelationDirection::Directed
                }
            })
        })
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartnerPresenceRule {
    pub reference_person_input: String,
    pub citation: AggregationCitation,
    pub caller_determination: String,
}

impl Default for PartnerPresenceRule {
    fn default() -> Self {
        Self {
            reference_person_input: String::new(),
            citation: AggregationCitation::default(),
            caller_determination: String::new(),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdultSelectionRule {
    pub operation: AdultSelectionOperation,
    pub citation: AggregationCitation,
    pub caller_determination: String,
}

impl Default for AdultSelectionRule {
    fn default() -> Self {
        Self {
            operation: AdultSelectionOperation::ExplicitPersonRole,
            citation: AggregationCitation::default(),
            caller_determination: String::new(),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdultSelectionOperation {
    ExplicitPersonRole,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Age18Conditions {
    pub age: i64,
    pub not_financially_independent_input: String,
    pub attending_school_or_tertiary_input: String,
    pub commissioner_period_input: String,
    pub citation: AggregationCitation,
}

impl Default for Age18Conditions {
    fn default() -> Self {
        Self {
            age: 18,
            not_financially_independent_input: String::new(),
            attending_school_or_tertiary_input: String::new(),
            commissioner_period_input: String::new(),
            citation: AggregationCitation::default(),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CareSemantics {
    pub principal_care_input: String,
    pub claimant_care_fraction_input: String,
    pub citation: AggregationCitation,
}

impl Default for CareSemantics {
    fn default() -> Self {
        Self {
            principal_care_input: String::new(),
            claimant_care_fraction_input: String::new(),
            citation: AggregationCitation::default(),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AggregationCitation {
    pub provision: String,
    pub authority: String,
}

impl From<&AggregationCitation> for Citation {
    fn from(value: &AggregationCitation) -> Self {
        Citation::new(value.provision.clone(), value.authority.clone())
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredLimitation {
    pub id: String,
    pub statement: String,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AggregateSelector {
    Adults,
    // Retained solely so an old unsafe document reaches semantic validation
    // and receives a named scope/provenance error rather than a YAML typo.
    AllMembers,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalarAggregation {
    pub output: String,
    pub input: String,
    pub selector: AggregateSelector,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildCount {
    pub output: String,
    #[serde(default)]
    pub minimum_age: Option<i64>,
    #[serde(default)]
    pub maximum_age: Option<i64>,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildMinimum {
    pub output: String,
    pub input: String,
    pub empty_value: i64,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyPredicate {
    pub output: String,
    pub operation: FamilyPredicateOperation,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FamilyPredicateOperation {
    CountAtLeast {
        count: String,
        minimum: i64,
    },
    YoungestAgeAtLeast {
        youngest: String,
        minimum: i64,
    },
    SoleParentWithChildren {
        count: String,
    },
    SoleParentYoungestAgeAtLeast {
        count: String,
        youngest: String,
        minimum: i64,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyShapeScalar {
    pub output: String,
    pub operation: FamilyShapeScalarOperation,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FamilyShapeScalarOperation {
    EldestCareUnits {
        count: String,
    },
    SubsequentCareUnits {
        count: String,
    },
    FixedIfChildren {
        count: String,
        present: String,
        absent: String,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildAgreement {
    pub output: String,
    pub input: String,
    #[serde(default)]
    pub eligible_if: Option<String>,
    pub empty_value: String,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildProjection {
    pub output: String,
    pub input: String,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildBroadcast {
    pub output: String,
    pub family_input: String,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyReduction {
    pub output: String,
    pub gross_output: String,
    pub operation: FamilyReductionOperation,
    #[serde(default)]
    pub reduction_key: Option<String>,
    #[serde(default)]
    pub child_gross_input: Option<String>,
    #[serde(default)]
    pub family_adjustment_input: Option<String>,
    // Legacy unsafe spellings are parsed deliberately and rejected by the
    // typed validator with a named error.
    #[serde(default)]
    pub child_value: Option<String>,
    #[serde(default)]
    pub family_once_value: Option<String>,
    pub care_fraction_input: Option<String>,
    #[serde(default)]
    pub minimum_age: Option<i64>,
    #[serde(default)]
    pub maximum_age: Option<i64>,
    #[serde(default)]
    pub continuous: Option<ContinuousFamilyReduction>,
    pub citation: AggregationCitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FamilyReductionOperation {
    SumChildrenThenSubtractFamilyOnce,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousFamilyReduction {
    pub output: String,
    pub family_income_input: String,
    pub threshold_input: String,
    pub rate_input: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AggregationEvidence {
    pub id: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AggregationObservation<T> {
    pub value: T,
    pub evidence: AggregationEvidence,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AggregationKnowledge<T> {
    Known {
        value: T,
        evidence: AggregationEvidence,
    },
    Unknown {
        evidence: AggregationEvidence,
    },
    Observations {
        observations: Vec<AggregationObservation<T>>,
    },
    Conflict {
        observations: Vec<AggregationObservation<T>>,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationRequest {
    pub scope: String,
    pub segment: String,
    pub roster_completeness: Option<AggregationEvidence>,
    pub segment_completeness: Option<AggregationEvidence>,
    pub persons: Vec<AggregationPerson>,
    #[serde(default)]
    pub relations: Vec<AggregationRelationFamily>,
    #[serde(default)]
    pub family_inputs: Vec<AggregationFamilyInput>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationFamilyInput {
    pub anchor_person: String,
    pub evidence: AggregationEvidence,
    #[serde(default)]
    pub named_people: BTreeMap<String, AggregationKnowledge<String>>,
    #[serde(default)]
    pub scalars: BTreeMap<String, AggregationKnowledge<String>>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AggregationPersonRole {
    Adult,
    Child,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationPerson {
    pub id: String,
    pub role: AggregationPersonRole,
    pub evidence: AggregationEvidence,
    #[serde(default)]
    pub age_years: Option<AggregationKnowledge<i64>>,
    #[serde(default)]
    pub scalars: BTreeMap<String, AggregationKnowledge<String>>,
    #[serde(default)]
    pub facts: BTreeMap<String, AggregationKnowledge<bool>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engine_computations: BTreeMap<String, EngineComputationRequest>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineComputationRequest {
    pub dataset: crate::spec::DatasetSpec,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationRelationFamily {
    pub name: String,
    pub scope: String,
    pub completeness: Option<AggregationEvidence>,
    #[serde(default)]
    pub facts: Vec<AggregationRelationFact>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationRelationFact {
    pub tuple: [String; 2],
    pub knowledge: AggregationKnowledge<bool>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AggregationValue<T> {
    Determined { value: T },
    Indeterminate { reasons: BTreeSet<String> },
}

impl<T> AggregationValue<T> {
    fn determined(value: T) -> Self {
        Self::Determined { value }
    }

    fn indeterminate(reasons: BTreeSet<String>) -> Self {
        Self::Indeterminate { reasons }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AggregationFamilyKnowledge {
    Determined {
        value: Vec<AggregatedFamily>,
    },
    Indeterminate {
        reasons: BTreeSet<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Vec<AggregatedFamily>>,
    },
}

impl AggregationFamilyKnowledge {
    fn determined(value: Vec<AggregatedFamily>) -> Self {
        Self::Determined { value }
    }

    fn indeterminate(reasons: BTreeSet<String>) -> Self {
        Self::Indeterminate {
            reasons,
            value: None,
        }
    }

    fn indeterminate_with_value(reasons: BTreeSet<String>, value: Vec<AggregatedFamily>) -> Self {
        Self::Indeterminate {
            reasons,
            value: Some(value),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AggregationResult {
    pub schema: String,
    pub plan: String,
    pub plan_digest: String,
    pub trace_root: String,
    pub families: AggregationFamilyKnowledge,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AggregatedFamily {
    pub id: String,
    pub members: Vec<String>,
    pub partner_present: AggregationValue<bool>,
    pub scalars: BTreeMap<String, AggregationValue<String>>,
    pub counts: BTreeMap<String, AggregationValue<i64>>,
    pub predicates: BTreeMap<String, AggregationValue<bool>>,
    pub children: Vec<AggregatedChild>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AggregatedChild {
    pub person: String,
    pub family: String,
    pub age_years: AggregationValue<i64>,
    pub age_bands: BTreeMap<String, AggregationValue<bool>>,
    pub scalars: BTreeMap<String, AggregationValue<String>>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledAggregationArtifact {
    pub format: String,
    pub semantics_version: String,
    pub plan_digest: String,
    pub constitution_digest: String,
    pub source_artifact_digest: String,
    pub plan: AggregationPlan,
    pub source_artifact: crate::compile::CompiledProgramArtifact,
    pub phase_two_artifact: crate::compile::CompiledProgramArtifact,
}

impl CompiledAggregationArtifact {
    pub fn plan_id(&self) -> &str {
        &self.plan.id
    }

    pub fn to_json_pretty(&self) -> Result<String, UnitDerivationError> {
        serde_json::to_string_pretty(self).map_err(|error| {
            UnitDerivationError::InvalidAggregationArtifact(format!(
                "could not serialize compiled artifact: {error}"
            ))
        })
    }

    pub fn from_json_str(source: &str) -> Result<Self, UnitDerivationError> {
        let artifact: Self = serde_json::from_str(source).map_err(|error| {
            UnitDerivationError::InvalidAggregationArtifact(format!(
                "could not deserialize compiled artifact: {error}"
            ))
        })?;
        validate_compiled_artifact(artifact)
    }

    fn constitution_digest_bytes(&self) -> Result<[u8; 32], UnitDerivationError> {
        decode_digest(&self.constitution_digest)
    }
}

#[derive(Clone, Debug, Default)]
pub struct UnitDerivationDocumentRegistry {
    aggregation: BTreeMap<String, CompiledAggregationArtifact>,
}

impl UnitDerivationDocumentRegistry {
    pub fn register_aggregation_source(
        &mut self,
        source: &str,
        source_artifact: &crate::compile::CompiledProgramArtifact,
    ) -> Result<&CompiledAggregationArtifact, UnitDerivationError> {
        let artifact = compile_aggregation_plan(source, source_artifact)?;
        self.register_aggregation_artifact(artifact)
    }

    pub fn register_aggregation_json(
        &mut self,
        source: &str,
    ) -> Result<&CompiledAggregationArtifact, UnitDerivationError> {
        let artifact = CompiledAggregationArtifact::from_json_str(source)?;
        self.register_aggregation_artifact(artifact)
    }

    pub fn register_aggregation_artifact(
        &mut self,
        artifact: CompiledAggregationArtifact,
    ) -> Result<&CompiledAggregationArtifact, UnitDerivationError> {
        let artifact = validate_compiled_artifact(artifact)?;
        let id = artifact.plan.id.clone();
        if self.aggregation.contains_key(&id) {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "compiled unit-derivation document",
                id,
            });
        }
        self.aggregation.insert(id.clone(), artifact);
        Ok(&self.aggregation[&id])
    }

    pub fn aggregation(&self, id: &str) -> Option<&CompiledAggregationArtifact> {
        self.aggregation.get(id)
    }

    pub fn execute_aggregation(
        &self,
        id: &str,
        request: &AggregationRequest,
        config: &UnitDerivationConfig,
    ) -> Result<AggregationResult, UnitDerivationError> {
        let artifact =
            self.aggregation
                .get(id)
                .ok_or_else(|| UnitDerivationError::UnknownReference {
                    from: "unit-derivation document registry".to_string(),
                    reference: id.to_string(),
                })?;
        execute_aggregation_plan(artifact, request, config)
    }
}

pub(crate) fn compile_aggregation_plan(
    source: &str,
    source_artifact: &crate::compile::CompiledProgramArtifact,
) -> Result<CompiledAggregationArtifact, UnitDerivationError> {
    let plan: AggregationPlan = serde_yaml::from_str(source).map_err(|error| {
        UnitDerivationError::InvalidPlan(format!("invalid aggregation plan YAML: {error}"))
    })?;
    compile_aggregation_plan_value(plan, source_artifact)
}

fn compile_aggregation_plan_value(
    plan: AggregationPlan,
    source_artifact: &crate::compile::CompiledProgramArtifact,
) -> Result<CompiledAggregationArtifact, UnitDerivationError> {
    let source_artifact = validate_source_artifact(source_artifact.clone())?;
    validate_aggregation_plan(&plan, &source_artifact)?;
    let source_artifact_digest = source_artifact_digest(&source_artifact)?;
    let phase_two_artifact = compile_phase_two_artifact(&plan)?;
    let plan_digest = aggregation_plan_digest(&plan, &source_artifact_digest)?;
    let constitution_digest = digest_text(&canonical_constitution_bytes(&plan)?);
    Ok(CompiledAggregationArtifact {
        format: COMPILED_AGGREGATION_ARTIFACT_FORMAT.to_string(),
        semantics_version: super::EXPERIMENTAL_SEMANTICS_VERSION.to_string(),
        plan_digest,
        constitution_digest,
        source_artifact_digest,
        plan,
        source_artifact,
        phase_two_artifact,
    })
}

fn validate_compiled_artifact(
    mut artifact: CompiledAggregationArtifact,
) -> Result<CompiledAggregationArtifact, UnitDerivationError> {
    if artifact.format != COMPILED_AGGREGATION_ARTIFACT_FORMAT {
        return Err(UnitDerivationError::InvalidAggregationArtifact(format!(
            "unsupported format `{}`",
            artifact.format
        )));
    }
    if artifact.semantics_version != super::EXPERIMENTAL_SEMANTICS_VERSION {
        return Err(UnitDerivationError::InvalidAggregationArtifact(format!(
            "unsupported semantics version `{}`",
            artifact.semantics_version
        )));
    }
    artifact.source_artifact = validate_source_artifact(artifact.source_artifact)?;
    let actual_source_digest = source_artifact_digest(&artifact.source_artifact)?;
    if artifact.source_artifact_digest != actual_source_digest {
        return Err(UnitDerivationError::InvalidAggregationArtifact(
            "advertised source artifact digest does not match the embedded source artifact"
                .to_string(),
        ));
    }
    let expected =
        compile_aggregation_plan_value(artifact.plan.clone(), &artifact.source_artifact)?;
    if artifact.plan_digest != expected.plan_digest {
        return Err(UnitDerivationError::InvalidAggregationArtifact(
            "advertised plan digest does not match the embedded plan".to_string(),
        ));
    }
    if artifact.constitution_digest != expected.constitution_digest {
        return Err(UnitDerivationError::InvalidAggregationArtifact(
            "advertised constitution digest does not match the embedded plan".to_string(),
        ));
    }
    if artifact.source_artifact_digest != expected.source_artifact_digest {
        return Err(UnitDerivationError::InvalidAggregationArtifact(
            "embedded source artifact does not match the registered plan binding".to_string(),
        ));
    }
    let actual_phase = serde_json::to_value(&artifact.phase_two_artifact).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not inspect embedded phase-two artifact: {error}"
        ))
    })?;
    let expected_phase = serde_json::to_value(&expected.phase_two_artifact).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not inspect regenerated phase-two artifact: {error}"
        ))
    })?;
    if actual_phase != expected_phase {
        return Err(UnitDerivationError::InvalidAggregationArtifact(
            "embedded phase-two artifact does not match the registered plan".to_string(),
        ));
    }
    // Run the production artifact loader as the final registry boundary. This
    // is the same metadata/version/program validation used by run-compiled.
    let phase_source = serde_json::to_string(&artifact.phase_two_artifact).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not serialize embedded phase-two artifact: {error}"
        ))
    })?;
    crate::compile::CompiledProgramArtifact::from_json_str(&phase_source).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "production phase-two artifact validation failed: {error}"
        ))
    })?;
    Ok(artifact)
}

fn validate_source_artifact(
    artifact: crate::compile::CompiledProgramArtifact,
) -> Result<crate::compile::CompiledProgramArtifact, UnitDerivationError> {
    let source = serde_json::to_string(&artifact).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not serialize embedded source artifact: {error}"
        ))
    })?;
    crate::compile::CompiledProgramArtifact::from_json_str(&source).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "production source artifact validation failed: {error}"
        ))
    })
}

fn source_artifact_digest(
    artifact: &crate::compile::CompiledProgramArtifact,
) -> Result<String, UnitDerivationError> {
    let bytes = serde_json::to_vec(artifact).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not canonically encode embedded source artifact: {error}"
        ))
    })?;
    Ok(digest_text(&bytes))
}

fn compile_phase_two_artifact(
    plan: &AggregationPlan,
) -> Result<crate::compile::CompiledProgramArtifact, UnitDerivationError> {
    let mut relations = plan
        .membership_relations
        .iter()
        .map(|relation| crate::spec::RelationSpec {
            name: relation.relation.clone(),
            arity: 2,
            slot_entities: vec!["Person".to_string(), "Person".to_string()],
            derivation: None,
        })
        .collect::<Vec<_>>();
    relations.push(crate::spec::RelationSpec {
        name: plan.constitution.unit_constituent_relation.clone(),
        arity: 2,
        slot_entities: vec![plan.constitution.entity_type.clone(), "Person".to_string()],
        derivation: None,
    });
    relations.push(crate::spec::RelationSpec {
        name: plan.constitution.participating_member_relation.clone(),
        arity: 2,
        slot_entities: vec![plan.constitution.entity_type.clone(), "Person".to_string()],
        derivation: None,
    });
    relations.sort_by(|left, right| left.name.cmp(&right.name));
    let spec = crate::spec::ProgramSpec {
        relations,
        ..Default::default()
    };
    let compiled = crate::compile::CompiledProgramArtifact::compile(spec).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "phase-two program did not compile through the production path: {error}"
        ))
    })?;
    let serialized = serde_json::to_string(&compiled).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "phase-two program did not serialize: {error}"
        ))
    })?;
    crate::compile::CompiledProgramArtifact::from_json_str(&serialized).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "phase-two program did not reload through the production path: {error}"
        ))
    })
}

fn validate_aggregation_plan(
    plan: &AggregationPlan,
    source_artifact: &crate::compile::CompiledProgramArtifact,
) -> Result<(), UnitDerivationError> {
    if plan.schema != super::EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "aggregation plan schema `{}` is unsupported",
            plan.schema
        )));
    }
    if plan.id.is_empty()
        || plan.constitution.id.is_empty()
        || plan.constitution.entity_type.is_empty()
        || plan.constitution.roster_relation.is_empty()
        || plan.constitution.unit_constituent_relation.is_empty()
        || plan.constitution.participating_member_relation.is_empty()
    {
        return Err(UnitDerivationError::InvalidPlan(
            "aggregation plan and constitution identities must be non-empty".to_string(),
        ));
    }
    if plan.decimal_precision == 0 {
        return Err(UnitDerivationError::InvalidPlan(
            "aggregation decimal_precision must be positive".to_string(),
        ));
    }

    // This check deliberately precedes all new-schema required-field checks.
    // The committed reviewer fixture used the old accepted shape and must be
    // rejected for the semantic grammar hole, not merely for missing new keys.
    let mut reduction_consumers = BTreeMap::<String, String>::new();
    for reduction in &plan.family_reductions {
        let key = reduction
            .reduction_key
            .clone()
            .unwrap_or_else(|| "untyped-family-scope".to_string());
        if let Some(first) = reduction_consumers.insert(key.clone(), reduction.output.clone()) {
            return Err(UnitDerivationError::DuplicateFamilyScopeReduction {
                key,
                first,
                second: reduction.output.clone(),
            });
        }
    }

    validate_citation(&plan.partner_presence.citation, "partner presence")?;
    validate_citation(&plan.adult_selection.citation, "adult selection")?;
    validate_citation(&plan.age_18_conditions.citation, "age-18 conditions")?;
    validate_citation(&plan.care.citation, "care semantics")?;
    if plan.partner_presence.reference_person_input.is_empty()
        || plan.partner_presence.caller_determination.is_empty()
    {
        return Err(UnitDerivationError::InvalidPlan(
            "partner presence requires an explicit family-unit reference-person input and caller-determination statement"
                .to_string(),
        ));
    }
    if plan.adult_selection.operation != AdultSelectionOperation::ExplicitPersonRole
        || plan.adult_selection.caller_determination.is_empty()
    {
        return Err(UnitDerivationError::InvalidPlan(
            "adult selection requires an explicit evidence-bearing person role and caller-determination statement"
                .to_string(),
        ));
    }
    if plan.age_18_conditions.age != 18
        || plan
            .age_18_conditions
            .not_financially_independent_input
            .is_empty()
        || plan
            .age_18_conditions
            .attending_school_or_tertiary_input
            .is_empty()
        || plan.age_18_conditions.commissioner_period_input.is_empty()
    {
        return Err(UnitDerivationError::InvalidPlan(
            "MC 9 age-18 conditions must explicitly name financial-dependence, education, and Commissioner-period inputs"
                .to_string(),
        ));
    }
    if plan.care.principal_care_input.is_empty()
        || plan.care.claimant_care_fraction_input.is_empty()
    {
        return Err(UnitDerivationError::InvalidPlan(
            "care semantics must explicitly name principal-care and claimant-care-fraction inputs"
                .to_string(),
        ));
    }

    let mut inputs = BTreeMap::new();
    for input in &plan.inputs {
        validate_citation(&input.citation, &format!("input `{}`", input.name))?;
        if input.name.is_empty() || inputs.insert(input.name.clone(), input).is_some() {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "aggregation input",
                id: input.name.clone(),
            });
        }
        if matches!(
            input.kind,
            AggregationInputKind::ChildGrossAmount | AggregationInputKind::FamilyAdjustment
        ) && input.reduction_key.as_deref().is_none_or(str::is_empty)
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "typed reduction input `{}` needs a non-empty reduction_key",
                input.name
            )));
        }
        match (&input.kind, &input.engine_computation) {
            (AggregationInputKind::ChildGrossAmount, Some(binding)) => {
                validate_engine_computation_binding(input, binding, source_artifact)?;
            }
            (AggregationInputKind::ChildGrossAmount, None) => {
                return Err(UnitDerivationError::InvalidAggregationOperand {
                    operation: input.name.clone(),
                    input: input.name.clone(),
                    expected: "engine-computed gross binding".to_string(),
                    found: "plan-authored ChildGrossAmount without upstream binding".to_string(),
                });
            }
            (_, Some(_)) => {
                return Err(UnitDerivationError::InvalidAggregationOperand {
                    operation: input.name.clone(),
                    input: input.name.clone(),
                    expected: "engine computation only on ChildGrossAmount".to_string(),
                    found: format!("{:?}/{:?}", input.scope, input.kind),
                });
            }
            (_, None) => {}
        }
    }

    validate_input_ref(
        &inputs,
        &plan.age_18_conditions.not_financially_independent_input,
        "MC 9 financial-dependence condition",
        AggregationInputScope::Child,
        AggregationInputKind::EligibilityBoolean,
    )?;
    validate_input_ref(
        &inputs,
        &plan.age_18_conditions.attending_school_or_tertiary_input,
        "MC 9 education condition",
        AggregationInputScope::Child,
        AggregationInputKind::EligibilityBoolean,
    )?;
    validate_input_ref(
        &inputs,
        &plan.age_18_conditions.commissioner_period_input,
        "MC 9 Commissioner-period condition",
        AggregationInputScope::Child,
        AggregationInputKind::EligibilityBoolean,
    )?;
    validate_input_ref(
        &inputs,
        &plan.care.principal_care_input,
        "principal-care condition",
        AggregationInputScope::Child,
        AggregationInputKind::EligibilityBoolean,
    )?;
    validate_input_ref(
        &inputs,
        &plan.care.claimant_care_fraction_input,
        "MG 2(5) claimant-care fraction",
        AggregationInputScope::Child,
        AggregationInputKind::CareFraction,
    )?;

    let mut relations = BTreeMap::new();
    for relation in &plan.membership_relations {
        validate_citation(
            &relation.citation,
            &format!("membership relation `{}`", relation.relation),
        )?;
        if relation.relation.is_empty()
            || relations
                .insert(relation.relation.clone(), relation)
                .is_some()
        {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "aggregation membership relation",
                id: relation.relation.clone(),
            });
        }
        if relation.direction.is_some() && relation.symmetric.is_some() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "membership relation `{}` supplies both direction and legacy symmetric",
                relation.relation
            )));
        }
        if relation.effective_direction().is_none()
            || relation.left_role.is_none()
            || relation.right_role.is_none()
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "membership relation `{}` must declare direction and both tuple roles",
                relation.relation
            )));
        }
    }
    let partner = relations.get(&plan.partner_relation).ok_or_else(|| {
        UnitDerivationError::UnknownReference {
            from: plan.id.clone(),
            reference: plan.partner_relation.clone(),
        }
    })?;
    if partner.effective_direction() != Some(RelationDirection::Symmetric)
        || partner.left_role != Some(MembershipRole::Partner)
        || partner.right_role != Some(MembershipRole::Partner)
    {
        return Err(UnitDerivationError::InvalidPlan(
            "partner relation must be symmetric with partner/partner roles".to_string(),
        ));
    }
    let child = relations.get(&plan.child_relation).ok_or_else(|| {
        UnitDerivationError::UnknownReference {
            from: plan.id.clone(),
            reference: plan.child_relation.clone(),
        }
    })?;
    if child.effective_direction() != Some(RelationDirection::Directed)
        || child.left_role != Some(MembershipRole::Caregiver)
        || child.right_role != Some(MembershipRole::Child)
    {
        return Err(UnitDerivationError::InvalidPlan(
            "child relation must be directed caregiver-to-child".to_string(),
        ));
    }

    let mut outputs = BTreeSet::new();
    for aggregation in &plan.scalar_aggregations {
        validate_citation(&aggregation.citation, &aggregation.output)?;
        insert_output(&mut outputs, &aggregation.output)?;
        if aggregation.selector != AggregateSelector::Adults {
            return Err(UnitDerivationError::InvalidAggregationOperand {
                operation: aggregation.output.clone(),
                input: aggregation.input.clone(),
                expected: "adult-scoped additive amount".to_string(),
                found: "all-members scalar aggregation can consume child-carried reductions"
                    .to_string(),
            });
        }
        validate_input_ref(
            &inputs,
            &aggregation.input,
            &aggregation.output,
            AggregationInputScope::Adult,
            AggregationInputKind::AdditiveAmount,
        )?;
    }
    for count in &plan.child_counts {
        validate_citation(&count.citation, &count.output)?;
        validate_age_range(count.minimum_age, count.maximum_age, &count.output)?;
        insert_output(&mut outputs, &count.output)?;
    }
    for minimum in &plan.child_minima {
        validate_citation(&minimum.citation, &minimum.output)?;
        if minimum.input != "age_years" {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "child minimum `{}` uses unsupported input `{}`",
                minimum.output, minimum.input
            )));
        }
        insert_output(&mut outputs, &minimum.output)?;
    }
    let count_outputs = plan
        .child_counts
        .iter()
        .map(|count| count.output.as_str())
        .collect::<BTreeSet<_>>();
    let minimum_outputs = plan
        .child_minima
        .iter()
        .map(|minimum| minimum.output.as_str())
        .collect::<BTreeSet<_>>();
    for predicate in &plan.family_predicates {
        validate_citation(&predicate.citation, &predicate.output)?;
        let (count, youngest) = match &predicate.operation {
            FamilyPredicateOperation::CountAtLeast { count, .. }
            | FamilyPredicateOperation::SoleParentWithChildren { count } => {
                (Some(count.as_str()), None)
            }
            FamilyPredicateOperation::YoungestAgeAtLeast { youngest, .. } => {
                (None, Some(youngest.as_str()))
            }
            FamilyPredicateOperation::SoleParentYoungestAgeAtLeast {
                count, youngest, ..
            } => (Some(count.as_str()), Some(youngest.as_str())),
        };
        if count.is_some_and(|name| !count_outputs.contains(name))
            || youngest.is_some_and(|name| !minimum_outputs.contains(name))
        {
            return Err(UnitDerivationError::UnknownReference {
                from: predicate.output.clone(),
                reference: count.or(youngest).unwrap_or_default().to_string(),
            });
        }
        insert_output(&mut outputs, &predicate.output)?;
    }
    for scalar in &plan.family_shape_scalars {
        validate_citation(&scalar.citation, &scalar.output)?;
        let count = match &scalar.operation {
            FamilyShapeScalarOperation::EldestCareUnits { count }
            | FamilyShapeScalarOperation::SubsequentCareUnits { count }
            | FamilyShapeScalarOperation::FixedIfChildren { count, .. } => count,
        };
        if !count_outputs.contains(count.as_str()) {
            return Err(UnitDerivationError::UnknownReference {
                from: scalar.output.clone(),
                reference: count.clone(),
            });
        }
        insert_output(&mut outputs, &scalar.output)?;
    }
    for agreement in &plan.child_agreements {
        validate_citation(&agreement.citation, &agreement.output)?;
        insert_output(&mut outputs, &agreement.output)?;
        validate_input_ref(
            &inputs,
            &agreement.input,
            &agreement.output,
            AggregationInputScope::Child,
            AggregationInputKind::CareFraction,
        )?;
        if let Some(condition) = &agreement.eligible_if {
            validate_input_ref(
                &inputs,
                condition,
                &agreement.output,
                AggregationInputScope::Child,
                AggregationInputKind::EligibilityBoolean,
            )?;
        }
    }
    for projection in &plan.child_projections {
        validate_citation(&projection.citation, &projection.output)?;
        insert_output(&mut outputs, &projection.output)?;
        let input =
            inputs
                .get(&projection.input)
                .ok_or_else(|| UnitDerivationError::UnknownReference {
                    from: projection.output.clone(),
                    reference: projection.input.clone(),
                })?;
        if input.scope != AggregationInputScope::Child
            || input.kind == AggregationInputKind::EligibilityBoolean
        {
            return invalid_operand(
                &projection.output,
                &projection.input,
                "Child-scoped scalar input",
                input,
            );
        }
    }
    for broadcast in &plan.broadcasts {
        validate_citation(&broadcast.citation, &broadcast.output)?;
        insert_output(&mut outputs, &broadcast.output)?;
        let input = inputs.get(&broadcast.family_input).ok_or_else(|| {
            UnitDerivationError::UnknownReference {
                from: broadcast.output.clone(),
                reference: broadcast.family_input.clone(),
            }
        })?;
        if input.scope != AggregationInputScope::Family {
            return invalid_operand(
                &broadcast.output,
                &broadcast.family_input,
                "family-scoped input",
                input,
            );
        }
    }
    for reduction in &plan.family_reductions {
        validate_citation(&reduction.citation, &reduction.output)?;
        validate_age_range(
            reduction.minimum_age,
            reduction.maximum_age,
            &reduction.output,
        )?;
        insert_output(&mut outputs, &reduction.output)?;
        insert_output(&mut outputs, &reduction.gross_output)?;
        let key = reduction.reduction_key.as_deref().unwrap_or_default();
        if key.is_empty()
            || reduction.child_value.is_some()
            || reduction.family_once_value.is_some()
            || reduction
                .child_gross_input
                .as_deref()
                .is_none_or(str::is_empty)
            || reduction
                .family_adjustment_input
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(UnitDerivationError::InvalidAggregationOperand {
                operation: reduction.output.clone(),
                input: reduction
                    .family_once_value
                    .clone()
                    .or_else(|| reduction.family_adjustment_input.clone())
                    .unwrap_or_default(),
                expected:
                    "one registered Family-scoped adjustment and one typed ChildGross operand"
                        .to_string(),
                found: "legacy child-carried or untyped family-once operand".to_string(),
            });
        }
        let child_input = inputs
            .get(reduction.child_gross_input.as_deref().unwrap())
            .ok_or_else(|| UnitDerivationError::UnknownReference {
                from: reduction.output.clone(),
                reference: reduction.child_gross_input.clone().unwrap_or_default(),
            })?;
        if child_input.scope != AggregationInputScope::Child
            || child_input.kind != AggregationInputKind::ChildGrossAmount
            || child_input.reduction_key.as_deref() != Some(key)
        {
            return invalid_operand(
                &reduction.output,
                &child_input.name,
                &format!("ChildGross({key})"),
                child_input,
            );
        }
        let adjustment = inputs
            .get(reduction.family_adjustment_input.as_deref().unwrap())
            .ok_or_else(|| UnitDerivationError::UnknownReference {
                from: reduction.output.clone(),
                reference: reduction
                    .family_adjustment_input
                    .clone()
                    .unwrap_or_default(),
            })?;
        if adjustment.scope != AggregationInputScope::Family
            || adjustment.kind != AggregationInputKind::FamilyAdjustment
            || adjustment.reduction_key.as_deref() != Some(key)
        {
            return invalid_operand(
                &reduction.output,
                &adjustment.name,
                &format!("FamilyAdjustment({key})"),
                adjustment,
            );
        }
        if reduction.care_fraction_input.as_deref()
            != Some(plan.care.claimant_care_fraction_input.as_str())
        {
            return Err(UnitDerivationError::InvalidAggregationOperand {
                operation: reduction.output.clone(),
                input: reduction.care_fraction_input.clone().unwrap_or_default(),
                expected: format!(
                    "MG 2(5) care input `{}`",
                    plan.care.claimant_care_fraction_input
                ),
                found: "missing or different care operand".to_string(),
            });
        }
        if let Some(continuous) = &reduction.continuous {
            insert_output(&mut outputs, &continuous.output)?;
            validate_input_ref(
                &inputs,
                &continuous.family_income_input,
                &continuous.output,
                AggregationInputScope::Family,
                AggregationInputKind::FamilyAmount,
            )?;
            validate_input_ref(
                &inputs,
                &continuous.threshold_input,
                &continuous.output,
                AggregationInputScope::Family,
                AggregationInputKind::FamilyAmount,
            )?;
            validate_input_ref(
                &inputs,
                &continuous.rate_input,
                &continuous.output,
                AggregationInputScope::Family,
                AggregationInputKind::DecimalRate,
            )?;
        }
    }
    for limitation in &plan.limitations {
        validate_citation(
            &limitation.citation,
            &format!("limitation `{}`", limitation.id),
        )?;
        if limitation.id.is_empty() || limitation.statement.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(
                "declared limitations require non-empty ids and statements".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_engine_computation_binding(
    input: &AggregationInput,
    binding: &EngineComputationBinding,
    source_artifact: &crate::compile::CompiledProgramArtifact,
) -> Result<(), UnitDerivationError> {
    if binding.rule_id.is_empty() {
        return Err(UnitDerivationError::InvalidAggregationOperand {
            operation: input.name.clone(),
            input: input.name.clone(),
            expected: "non-empty public upstream rule id at gross stage".to_string(),
            found: "empty engine-computation rule id".to_string(),
        });
    }
    let Some(rule) = source_artifact
        .program
        .derived
        .iter()
        .find(|rule| rule.id.as_deref() == Some(binding.rule_id.as_str()))
    else {
        return Err(UnitDerivationError::UnknownReference {
            from: format!("engine computation for `{}`", input.name),
            reference: binding.rule_id.clone(),
        });
    };
    if input.scope != AggregationInputScope::Child
        || rule.entity != "Child"
        || !matches!(
            rule.semantics,
            crate::spec::DerivedSemanticsSpec::Scalar { .. }
        )
        || !matches!(
            rule.dtype,
            crate::spec::DTypeSpec::Decimal | crate::spec::DTypeSpec::Integer
        )
    {
        return Err(UnitDerivationError::InvalidAggregationOperand {
            operation: input.name.clone(),
            input: binding.rule_id.clone(),
            expected: "Child-scoped numeric upstream rule for engine-computed gross".to_string(),
            found: format!("{:?}/{}/{:?}", input.scope, rule.entity, rule.dtype),
        });
    }
    Ok(())
}

fn validate_input_ref(
    inputs: &BTreeMap<String, &AggregationInput>,
    name: &str,
    operation: &str,
    scope: AggregationInputScope,
    kind: AggregationInputKind,
) -> Result<(), UnitDerivationError> {
    let input = inputs
        .get(name)
        .ok_or_else(|| UnitDerivationError::UnknownReference {
            from: operation.to_string(),
            reference: name.to_string(),
        })?;
    if input.scope != scope || input.kind != kind {
        return invalid_operand(operation, name, &format!("{scope:?}/{kind:?}"), input);
    }
    Ok(())
}

fn invalid_operand<T>(
    operation: &str,
    input_name: &str,
    expected: &str,
    input: &AggregationInput,
) -> Result<T, UnitDerivationError> {
    Err(UnitDerivationError::InvalidAggregationOperand {
        operation: operation.to_string(),
        input: input_name.to_string(),
        expected: expected.to_string(),
        found: format!("{:?}/{:?}", input.scope, input.kind),
    })
}

fn validate_citation(
    citation: &AggregationCitation,
    operation: &str,
) -> Result<(), UnitDerivationError> {
    if citation.provision.is_empty() || citation.authority.is_empty() {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "aggregation operation `{operation}` needs a complete citation"
        )));
    }
    Ok(())
}

fn insert_output(outputs: &mut BTreeSet<String>, output: &str) -> Result<(), UnitDerivationError> {
    if output.is_empty() || !outputs.insert(output.to_string()) {
        return Err(UnitDerivationError::DuplicateNamespace {
            namespace: "aggregation output",
            id: output.to_string(),
        });
    }
    Ok(())
}

fn validate_age_range(
    minimum: Option<i64>,
    maximum: Option<i64>,
    output: &str,
) -> Result<(), UnitDerivationError> {
    if minimum
        .zip(maximum)
        .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "age range for `{output}` is reversed"
        )));
    }
    Ok(())
}

fn normalized_plan(plan: &AggregationPlan) -> AggregationPlan {
    let mut normalized = plan.clone();
    normalized
        .inputs
        .sort_by(|left, right| left.name.cmp(&right.name));
    normalized
        .membership_relations
        .sort_by(|left, right| left.relation.cmp(&right.relation));
    normalized
        .scalar_aggregations
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .child_counts
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .child_minima
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .family_predicates
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .family_shape_scalars
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .child_agreements
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .child_projections
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .broadcasts
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .family_reductions
        .sort_by(|left, right| left.output.cmp(&right.output));
    normalized
        .limitations
        .sort_by(|left, right| left.id.cmp(&right.id));
    normalized
}

fn canonical_plan_bytes(plan: &AggregationPlan) -> Result<Vec<u8>, UnitDerivationError> {
    let normalized = normalized_plan(plan);
    let mut bytes = b"axiom.unit-aggregation.plan.stage3\0".to_vec();
    bytes.extend(serde_json::to_vec(&normalized).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not canonically encode plan: {error}"
        ))
    })?);
    Ok(bytes)
}

fn aggregation_plan_digest(
    plan: &AggregationPlan,
    source_artifact_digest: &str,
) -> Result<String, UnitDerivationError> {
    let mut preimage = b"axiom.unit-aggregation.bound-plan.stage3\0".to_vec();
    push_len_prefixed(&mut preimage, &canonical_plan_bytes(plan)?);
    push_len_prefixed(&mut preimage, source_artifact_digest.as_bytes());
    Ok(digest_text(&preimage))
}

fn canonical_constitution_bytes(plan: &AggregationPlan) -> Result<Vec<u8>, UnitDerivationError> {
    let normalized = normalized_plan(plan);
    let value = serde_json::json!({
        "constitution": normalized.constitution,
        "membership_relations": normalized.membership_relations,
        "partner_relation": normalized.partner_relation,
        "child_relation": normalized.child_relation,
        "partner_presence": normalized.partner_presence,
        "adult_selection": normalized.adult_selection,
        "age_18_conditions": normalized.age_18_conditions,
        "care": normalized.care,
    });
    let mut bytes = b"axiom.unit-aggregation.constitution.stage3\0".to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not canonically encode constitution: {error}"
        ))
    })?);
    Ok(bytes)
}

fn digest_text(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&super::evaluate::sha256(bytes)))
}

fn decode_digest(value: &str) -> Result<[u8; 32], UnitDerivationError> {
    let raw = value.strip_prefix("sha256:").ok_or_else(|| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "digest `{value}` lacks sha256 prefix"
        ))
    })?;
    if raw.len() != 64 {
        return Err(UnitDerivationError::InvalidAggregationArtifact(format!(
            "digest `{value}` is not 32 bytes"
        )));
    }
    let mut output = [0_u8; 32];
    for (index, slot) in output.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).map_err(|_| {
            UnitDerivationError::InvalidAggregationArtifact(format!(
                "digest `{value}` is not hexadecimal"
            ))
        })?;
    }
    Ok(output)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn execute_aggregation_plan(
    artifact: &CompiledAggregationArtifact,
    request: &AggregationRequest,
    config: &UnitDerivationConfig,
) -> Result<AggregationResult, UnitDerivationError> {
    if !config.enabled {
        return Err(UnitDerivationError::Disabled);
    }
    let artifact = validate_compiled_artifact(artifact.clone())?;
    let plan = &artifact.plan;
    let people = validate_request(plan, request)?;

    let relation_reasons = request
        .relations
        .iter()
        .flat_map(|family| {
            family.facts.iter().flat_map(|fact| {
                resolve_knowledge(
                    Some(&fact.knowledge),
                    &format!(
                        "relation:{}:{}:{}",
                        family.name, fact.tuple[0], fact.tuple[1]
                    ),
                )
                .err()
                .into_iter()
                .flatten()
            })
        })
        .collect::<BTreeSet<_>>();
    if !relation_reasons.is_empty() {
        let families = AggregationFamilyKnowledge::indeterminate(relation_reasons);
        let trace_root = aggregation_trace_root(&artifact, request, "", &families)?;
        return Ok(AggregationResult {
            schema: super::EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA.to_string(),
            plan: plan.id.clone(),
            plan_digest: artifact.plan_digest.clone(),
            trace_root,
            families,
        });
    }

    if request.segment_completeness.is_none() {
        let reasons = BTreeSet::from([format!("segment-completeness:{}", request.segment)]);
        let families = AggregationFamilyKnowledge::indeterminate(reasons);
        let trace_root = aggregation_trace_root(&artifact, request, "", &families)?;
        return Ok(AggregationResult {
            schema: super::EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA.to_string(),
            plan: plan.id.clone(),
            plan_digest: artifact.plan_digest.clone(),
            trace_root,
            families,
        });
    }

    let (mut compiled, input) = bind_constitution(&artifact, request, &people)?;
    // Roster grounding is request-specific, but family identity is defined by
    // the registered constitution semantics, not by the N-person grounding.
    compiled.semantics_digest = artifact.constitution_digest_bytes()?;
    let prototype = super::Prototype::new(
        compiled,
        config.clone(),
        super::FrozenLedger::new(Vec::new())?,
    );
    let run = prototype.run(&input, &[], None)?;
    if !run.derivation.indeterminate.is_empty() {
        let reasons = run
            .derivation
            .indeterminate
            .iter()
            .flat_map(|item| item.unresolved_facts.iter().cloned())
            .collect::<BTreeSet<_>>();
        let families = AggregationFamilyKnowledge::indeterminate(if reasons.is_empty() {
            BTreeSet::from(["indeterminate-family-membership".to_string()])
        } else {
            reasons
        });
        let trace_root =
            aggregation_trace_root(&artifact, request, &run.derivation.trace.root, &families)?;
        return Ok(AggregationResult {
            schema: super::EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA.to_string(),
            plan: plan.id.clone(),
            plan_digest: artifact.plan_digest.clone(),
            trace_root,
            families,
        });
    }

    let program = artifact
        .phase_two_artifact
        .program
        .to_program()
        .map_err(|error| {
            UnitDerivationError::InvalidAggregationArtifact(format!(
                "registered phase-two program did not materialize: {error}"
            ))
        })?;
    let interval = parse_segment_interval(&request.segment)?;
    let (base, input_knowledge, provenance) = bind_aggregation_dataset(
        &artifact,
        request,
        &people,
        &run.derivation.units,
        &interval,
    )?;
    let phase_two = super::materialize_phase_two_dataset_with_knowledge(
        &base,
        input_knowledge,
        &program,
        &input,
        &run,
        interval.clone(),
    )?;
    for person in people.keys() {
        if phase_two.registry().get(person).is_none() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "typed materialization registry omitted person `{person}`"
            )));
        }
    }
    for unit in &run.derivation.units {
        if phase_two.registry().get(&unit.id).is_none() {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "typed materialization registry omitted family `{}`",
                unit.id
            )));
        }
    }

    let period = crate::model::Period {
        kind: crate::model::PeriodKind::Custom("unit-aggregation".to_string()),
        start: interval.start,
        end: interval.end,
    };
    let role_index = RelationshipRoleIndex::new(plan, &phase_two, &period)?;
    let mut families = run
        .derivation
        .units
        .iter()
        .map(|unit| {
            aggregate_family(
                plan,
                &phase_two,
                &period,
                &role_index,
                &provenance,
                &artifact.source_artifact_digest,
                unit,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    families.sort_by(|left, right| left.id.cmp(&right.id));
    let reasons = families
        .iter()
        .flat_map(aggregated_family_reasons)
        .collect::<BTreeSet<_>>();
    let families = if reasons.is_empty() {
        AggregationFamilyKnowledge::determined(families)
    } else {
        AggregationFamilyKnowledge::indeterminate_with_value(reasons, families)
    };
    let trace_root =
        aggregation_trace_root(&artifact, request, &run.derivation.trace.root, &families)?;
    Ok(AggregationResult {
        schema: super::EXPERIMENTAL_AGGREGATION_PLAN_SCHEMA.to_string(),
        plan: plan.id.clone(),
        plan_digest: artifact.plan_digest,
        trace_root,
        families,
    })
}

fn validate_request<'a>(
    plan: &AggregationPlan,
    request: &'a AggregationRequest,
) -> Result<BTreeMap<String, &'a AggregationPerson>, UnitDerivationError> {
    if request.scope.is_empty() || request.segment.is_empty() || request.persons.is_empty() {
        return Err(UnitDerivationError::InvalidPlan(
            "aggregation scope, segment, and roster must be non-empty".to_string(),
        ));
    }
    let mut people = BTreeMap::new();
    for person in &request.persons {
        if person.id.is_empty() || person.evidence.id.is_empty() {
            return Err(UnitDerivationError::InvalidPlan(
                "every aggregation person needs a non-empty id and evidence id".to_string(),
            ));
        }
        if people.insert(person.id.clone(), person).is_some() {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "aggregation person",
                id: person.id.clone(),
            });
        }
    }
    let declared_inputs = plan
        .inputs
        .iter()
        .map(|input| (input.name.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    for person in people.values() {
        if let Some(age) = &person.age_years {
            validate_knowledge_evidence(age, &format!("person `{}` age", person.id))?;
        }
        for name in person.scalars.keys().chain(person.facts.keys()) {
            if !declared_inputs.contains_key(name.as_str()) {
                return Err(UnitDerivationError::UnknownReference {
                    from: format!("aggregation person `{}`", person.id),
                    reference: name.clone(),
                });
            }
        }
        for name in person.engine_computations.keys() {
            let input = declared_inputs.get(name.as_str()).ok_or_else(|| {
                UnitDerivationError::UnknownReference {
                    from: format!("aggregation person `{}` engine computation", person.id),
                    reference: name.clone(),
                }
            })?;
            if person.role != AggregationPersonRole::Child
                || input.scope != AggregationInputScope::Child
                || input.kind != AggregationInputKind::ChildGrossAmount
                || input.engine_computation.is_none()
            {
                return invalid_operand(
                    &format!("person `{}` engine computation", person.id),
                    name,
                    "bound ChildGrossAmount on an explicit child role",
                    input,
                );
            }
        }
        for name in person.facts.keys() {
            let input = declared_inputs[name.as_str()];
            validate_knowledge_evidence(
                &person.facts[name],
                &format!("person `{}` fact `{name}`", person.id),
            )?;
            if input.kind != AggregationInputKind::EligibilityBoolean {
                return invalid_operand(
                    &format!("person `{}` fact", person.id),
                    name,
                    "eligibility boolean",
                    input,
                );
            }
            if input.scope != AggregationInputScope::Child
                || person.role != AggregationPersonRole::Child
            {
                return invalid_operand(
                    &format!("person `{}` fact", person.id),
                    name,
                    "Child-scoped eligibility boolean on an explicit child role",
                    input,
                );
            }
        }
        for name in person.scalars.keys() {
            let input = declared_inputs[name.as_str()];
            validate_knowledge_evidence(
                &person.scalars[name],
                &format!("person `{}` scalar `{name}`", person.id),
            )?;
            if input.kind == AggregationInputKind::EligibilityBoolean {
                return invalid_operand(
                    &format!("person `{}` scalar", person.id),
                    name,
                    "numeric scalar",
                    input,
                );
            }
            let expected_role = match input.scope {
                AggregationInputScope::Adult => AggregationPersonRole::Adult,
                AggregationInputScope::Child => AggregationPersonRole::Child,
                AggregationInputScope::Family => {
                    return invalid_operand(
                        &format!("person `{}` scalar", person.id),
                        name,
                        "Adult or Child scope",
                        input,
                    );
                }
            };
            if person.role != expected_role {
                return Err(UnitDerivationError::InvalidRelationshipRole {
                    relation: "explicit_person_role".to_string(),
                    person: person.id.clone(),
                    role: format!("{:?}", person.role),
                });
            }
        }
    }
    let mut family_anchors = BTreeSet::new();
    for family in &request.family_inputs {
        if family.anchor_person.is_empty()
            || family.evidence.id.is_empty()
            || !people.contains_key(&family.anchor_person)
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "family input anchor `{}` and its evidence must identify a roster person",
                family.anchor_person
            )));
        }
        if !family_anchors.insert(family.anchor_person.clone()) {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "aggregation family input anchor",
                id: family.anchor_person.clone(),
            });
        }
        if family.named_people.len() != 1
            || !family
                .named_people
                .contains_key(&plan.partner_presence.reference_person_input)
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "family input `{}` must bind exactly the plan input `{}`",
                family.anchor_person, plan.partner_presence.reference_person_input
            )));
        }
        let holder_knowledge = &family.named_people[&plan.partner_presence.reference_person_input];
        validate_knowledge_evidence(
            holder_knowledge,
            &format!("family `{}` reference person", family.anchor_person),
        )?;
        if let Ok(holder) = resolve_knowledge(
            Some(holder_knowledge),
            &format!("family:{}:reference-person", family.anchor_person),
        ) {
            let Some(person) = people.get(&holder) else {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "family-unit reference person `{holder}` is outside the roster"
                )));
            };
            if person.role != AggregationPersonRole::Adult {
                return Err(UnitDerivationError::InvalidRelationshipRole {
                    relation: plan.partner_relation.clone(),
                    person: holder,
                    role: "reference_person_must_be_adult".to_string(),
                });
            }
        }
        for name in family.scalars.keys() {
            let input = declared_inputs.get(name.as_str()).ok_or_else(|| {
                UnitDerivationError::UnknownReference {
                    from: format!("aggregation family `{}` scalar", family.anchor_person),
                    reference: name.clone(),
                }
            })?;
            if input.scope != AggregationInputScope::Family {
                return invalid_operand(
                    &format!("aggregation family `{}` scalar", family.anchor_person),
                    name,
                    "Family scope",
                    input,
                );
            }
            validate_knowledge_evidence(
                &family.scalars[name],
                &format!("family `{}` scalar `{name}`", family.anchor_person),
            )?;
        }
    }

    let declared_relations = plan
        .membership_relations
        .iter()
        .map(|relation| relation.relation.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen_relations = BTreeSet::new();
    for family in &request.relations {
        if !declared_relations.contains(family.name.as_str()) {
            return Err(UnitDerivationError::UnknownReference {
                from: "aggregation request relation".to_string(),
                reference: family.name.clone(),
            });
        }
        if !seen_relations.insert(family.name.clone()) {
            return Err(UnitDerivationError::DuplicateNamespace {
                namespace: "aggregation request relation family",
                id: family.name.clone(),
            });
        }
        if family
            .completeness
            .as_ref()
            .is_some_and(|evidence| evidence.id.is_empty())
        {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "relation `{}` completeness evidence id must be non-empty",
                family.name
            )));
        }
        for fact in &family.facts {
            if fact.tuple[0] == fact.tuple[1]
                || !people.contains_key(&fact.tuple[0])
                || !people.contains_key(&fact.tuple[1])
            {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "relation `{}` contains invalid roster tuple {:?}",
                    family.name, fact.tuple
                )));
            }
            validate_knowledge_evidence(
                &fact.knowledge,
                &format!("relation `{}` tuple {:?}", family.name, fact.tuple),
            )?;
            if family.name == plan.child_relation {
                if people[&fact.tuple[0]].role != AggregationPersonRole::Adult {
                    return Err(UnitDerivationError::InvalidRelationshipRole {
                        relation: family.name.clone(),
                        person: fact.tuple[0].clone(),
                        role: "caregiver_must_be_adult".to_string(),
                    });
                }
                if people[&fact.tuple[1]].role != AggregationPersonRole::Child {
                    return Err(UnitDerivationError::InvalidRelationshipRole {
                        relation: family.name.clone(),
                        person: fact.tuple[1].clone(),
                        role: "child_must_have_child_role".to_string(),
                    });
                }
            }
            if family.name == plan.partner_relation
                && (people[&fact.tuple[0]].role != AggregationPersonRole::Adult
                    || people[&fact.tuple[1]].role != AggregationPersonRole::Adult)
            {
                return Err(UnitDerivationError::InvalidRelationshipRole {
                    relation: family.name.clone(),
                    person: format!("{}:{}", fact.tuple[0], fact.tuple[1]),
                    role: "partners_must_be_adults".to_string(),
                });
            }
        }
    }
    Ok(people)
}

fn bind_constitution(
    artifact: &CompiledAggregationArtifact,
    request: &AggregationRequest,
    people: &BTreeMap<String, &AggregationPerson>,
) -> Result<(super::CompiledConstitution, ConstitutionInput), UnitDerivationError> {
    let plan = &artifact.plan;
    let mut constitution = ConstitutionPlan {
        id: plan.constitution.id.clone(),
        entity_type: plan.constitution.entity_type.clone(),
        roster_relation: plan.constitution.roster_relation.clone(),
        relations: EmissionRelations {
            unit_constituent: plan.constitution.unit_constituent_relation.clone(),
            participating_member: plan.constitution.participating_member_relation.clone(),
        },
        derived_bools: Vec::new(),
        edges: Vec::new(),
        cuts: Vec::new(),
        attachments: Vec::new(),
        bars: Vec::new(),
        statuses: Vec::new(),
        base_chain_policy: None,
    };
    let person_ids = people.keys().cloned().collect::<Vec<_>>();
    for relation in &plan.membership_relations {
        for left_index in 0..person_ids.len() {
            for right_index in (left_index + 1)..person_ids.len() {
                let left = &person_ids[left_index];
                let right = &person_ids[right_index];
                let when = BoolExpr::Or(vec![
                    BoolExpr::fact(FactRef::Relation {
                        family: relation.relation.clone(),
                        tuple: vec![left.clone(), right.clone()],
                    }),
                    BoolExpr::fact(FactRef::Relation {
                        family: relation.relation.clone(),
                        tuple: vec![right.clone(), left.clone()],
                    }),
                ]);
                constitution.edges.push(EdgeRule {
                    id: format!("{}:{left}:{right}", relation.relation),
                    kind: EdgeKind::Combination,
                    left: left.clone(),
                    right: right.clone(),
                    when,
                    citation: (&relation.citation).into(),
                    defeaters: Vec::new(),
                });
            }
        }
    }
    let compiled = super::compile(constitution)?;
    let request_relations = request
        .relations
        .iter()
        .map(|family| (family.name.as_str(), family))
        .collect::<BTreeMap<_, _>>();
    let relation_families = plan
        .membership_relations
        .iter()
        .map(|relation| {
            let supplied = request_relations.get(relation.relation.as_str());
            let contains_unknown = supplied.is_some_and(|family| {
                family
                    .facts
                    .iter()
                    .any(|fact| matches!(fact.knowledge, AggregationKnowledge::Unknown { .. }))
            });
            let facts = supplied
                .into_iter()
                .flat_map(|family| family.facts.iter())
                .flat_map(|fact| relation_observations(fact, &relation.citation))
                .collect::<Vec<_>>();
            RelationFamilyInput {
                name: relation.relation.clone(),
                scope: supplied
                    .map_or_else(|| request.scope.clone(), |family| family.scope.clone()),
                completeness: if contains_unknown {
                    None
                } else {
                    supplied
                        .and_then(|family| family.completeness.as_ref())
                        .map(|evidence| Evidence {
                            id: evidence.id.clone(),
                            citation: (&relation.citation).into(),
                        })
                },
                facts,
            }
        })
        .collect();
    let input = ConstitutionInput {
        roster: RosterInput {
            relation: plan.constitution.roster_relation.clone(),
            scope: request.scope.clone(),
            persons: person_ids,
            completeness: request
                .roster_completeness
                .as_ref()
                .map(|evidence| Evidence {
                    id: evidence.id.clone(),
                    citation: Citation::new(
                        "explicit scenario roster",
                        format!("{} roster input", plan.id),
                    ),
                }),
        },
        segment: request.segment.clone(),
        segment_complete: request.segment_completeness.is_some(),
        relation_families,
        bool_facts: Vec::new(),
        supplied_entities: people
            .values()
            .map(|person| SuppliedEntity {
                entity_type: "Person".to_string(),
                id: person.id.clone(),
                evidence: Evidence {
                    id: person.evidence.id.clone(),
                    citation: Citation::new(
                        "explicit scenario person",
                        format!("{} request", plan.id),
                    ),
                },
            })
            .collect(),
        integrity_constraints: Vec::new(),
    };
    Ok((compiled, input))
}

fn relation_observations(
    fact: &AggregationRelationFact,
    citation: &AggregationCitation,
) -> Vec<RelationFact> {
    match &fact.knowledge {
        AggregationKnowledge::Known { value, evidence } => vec![RelationFact {
            tuple: fact.tuple.to_vec(),
            observation: ObservedBool {
                value: *value,
                evidence: Evidence {
                    id: evidence.id.clone(),
                    citation: citation.into(),
                },
            },
        }],
        AggregationKnowledge::Unknown { .. } => Vec::new(),
        AggregationKnowledge::Observations { observations }
        | AggregationKnowledge::Conflict { observations } => observations
            .iter()
            .map(|observation| RelationFact {
                tuple: fact.tuple.to_vec(),
                observation: ObservedBool {
                    value: observation.value,
                    evidence: Evidence {
                        id: observation.evidence.id.clone(),
                        citation: citation.into(),
                    },
                },
            })
            .collect(),
    }
}

fn parse_segment_interval(segment: &str) -> Result<crate::model::Interval, UnitDerivationError> {
    let (start, end) = segment.split_once('/').ok_or_else(|| {
        UnitDerivationError::InvalidPlan(format!(
            "aggregation segment `{segment}` is not start/end"
        ))
    })?;
    let start = NaiveDate::parse_from_str(start, "%Y-%m-%d").map_err(|error| {
        UnitDerivationError::InvalidPlan(format!(
            "aggregation segment start `{start}` is invalid: {error}"
        ))
    })?;
    let end = NaiveDate::parse_from_str(end, "%Y-%m-%d").map_err(|error| {
        UnitDerivationError::InvalidPlan(format!(
            "aggregation segment end `{end}` is invalid: {error}"
        ))
    })?;
    if end < start {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "aggregation segment `{segment}` is reversed"
        )));
    }
    Ok(crate::model::Interval { start, end })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OperandProvenance {
    RequestSupplied,
    EngineComputed {
        source_artifact_digest: String,
        rule_id: String,
        stage: EngineComputationStage,
    },
}

impl OperandProvenance {
    fn label(&self) -> String {
        match self {
            Self::RequestSupplied => "request_supplied".to_string(),
            Self::EngineComputed {
                source_artifact_digest,
                rule_id,
                stage,
            } => format!(
                "engine_computed(source_artifact={source_artifact_digest},rule_id={rule_id},stage={})",
                match stage {
                    EngineComputationStage::Gross => "gross",
                }
            ),
        }
    }
}

type OperandProvenanceIndex = BTreeMap<(String, String), OperandProvenance>;

fn bind_aggregation_dataset(
    artifact: &CompiledAggregationArtifact,
    request: &AggregationRequest,
    people: &BTreeMap<String, &AggregationPerson>,
    units: &[DerivedUnit],
    interval: &crate::model::Interval,
) -> Result<
    (
        crate::model::DataSet,
        Vec<InputKnowledgeRecord>,
        OperandProvenanceIndex,
    ),
    UnitDerivationError,
> {
    let plan = &artifact.plan;
    let mut dataset = crate::model::DataSet::default();
    let mut unresolved = Vec::new();
    let mut provenance = OperandProvenanceIndex::new();

    for person in people.values() {
        dataset.add_input(
            "__aggregation_person_role",
            "Person",
            person.id.clone(),
            interval.clone(),
            crate::model::ScalarValue::Text(match person.role {
                AggregationPersonRole::Adult => "adult".to_string(),
                AggregationPersonRole::Child => "child".to_string(),
            }),
        );
        if let Some(age) = &person.age_years {
            bind_known_input(
                &mut dataset,
                &mut unresolved,
                "age_years",
                "Person",
                &person.id,
                interval,
                age,
                crate::model::ScalarValue::Integer,
            );
        }
        for (name, value) in &person.scalars {
            provenance.insert(
                (name.clone(), person.id.clone()),
                OperandProvenance::RequestSupplied,
            );
            bind_known_input(
                &mut dataset,
                &mut unresolved,
                name,
                "Person",
                &person.id,
                interval,
                value,
                crate::model::ScalarValue::Text,
            );
        }
        for (name, recipe) in &person.engine_computations {
            // An asserted scalar never gains computed provenance merely because
            // the same request also supplies a computation recipe. The sole
            // family-reduction guard below will reject that request value.
            if person.scalars.contains_key(name) {
                continue;
            }
            let input = plan
                .inputs
                .iter()
                .find(|input| input.name == *name)
                .expect("request engine-computation names were validated");
            let binding = input
                .engine_computation
                .as_ref()
                .expect("request engine computations require plan bindings");
            let value =
                execute_engine_computation(artifact, input, binding, &person.id, recipe, interval)?;
            dataset.add_input(
                name,
                "Person",
                person.id.clone(),
                interval.clone(),
                crate::model::ScalarValue::Text(value),
            );
            provenance.insert(
                (name.clone(), person.id.clone()),
                OperandProvenance::EngineComputed {
                    source_artifact_digest: artifact.source_artifact_digest.clone(),
                    rule_id: binding.rule_id.clone(),
                    // The stage is assigned by this materialization path, not
                    // deserialized from either the plan or the request.
                    stage: EngineComputationStage::Gross,
                },
            );
        }
        for (name, value) in &person.facts {
            bind_known_input(
                &mut dataset,
                &mut unresolved,
                name,
                "Person",
                &person.id,
                interval,
                value,
                crate::model::ScalarValue::Bool,
            );
        }
    }

    for relation in &request.relations {
        for fact in &relation.facts {
            if resolve_knowledge(
                Some(&fact.knowledge),
                &format!(
                    "relation:{}:{}:{}",
                    relation.name, fact.tuple[0], fact.tuple[1]
                ),
            )
            .expect("indeterminate relations return before materialization")
            {
                dataset.relations.push(crate::model::RelationRecord {
                    name: relation.name.clone(),
                    tuple: fact.tuple.to_vec(),
                    interval: interval.clone(),
                });
            }
        }
    }

    let mut claimed_units = BTreeMap::<String, String>::new();
    for family in &request.family_inputs {
        let unit = units
            .iter()
            .find(|unit| unit.members.contains(&family.anchor_person))
            .ok_or_else(|| {
                UnitDerivationError::InvalidPlan(format!(
                    "family input anchor `{}` was not materialized into a family",
                    family.anchor_person
                ))
            })?;
        if let Some(first) = claimed_units.insert(unit.id.clone(), family.anchor_person.clone()) {
            return Err(UnitDerivationError::InvalidPlan(format!(
                "derived family `{}` contains more than one family-input anchor: {first},{}",
                unit.id, family.anchor_person
            )));
        }
        if let Ok(holder) = resolve_knowledge(
            family
                .named_people
                .get(&plan.partner_presence.reference_person_input),
            &format!("family:{}:reference-person", family.anchor_person),
        ) && !unit.members.contains(&holder)
        {
            return Err(UnitDerivationError::InvalidRelationshipRole {
                relation: plan.partner_relation.clone(),
                person: holder,
                role: "reference_person_must_belong_to_anchored_family".to_string(),
            });
        }
        for (name, value) in &family.named_people {
            bind_known_input(
                &mut dataset,
                &mut unresolved,
                name,
                &plan.constitution.entity_type,
                &unit.id,
                interval,
                value,
                crate::model::ScalarValue::Text,
            );
        }
        for (name, value) in &family.scalars {
            bind_known_input(
                &mut dataset,
                &mut unresolved,
                name,
                &plan.constitution.entity_type,
                &unit.id,
                interval,
                value,
                crate::model::ScalarValue::Text,
            );
        }
    }

    Ok((dataset, unresolved, provenance))
}

fn execute_engine_computation(
    artifact: &CompiledAggregationArtifact,
    input: &AggregationInput,
    binding: &EngineComputationBinding,
    person_id: &str,
    recipe: &EngineComputationRequest,
    interval: &crate::model::Interval,
) -> Result<String, UnitDerivationError> {
    let request = crate::api::CompiledExecutionRequest {
        mode: crate::api::ExecutionMode::Explain,
        dataset: recipe.dataset.clone(),
        queries: vec![crate::api::ExecutionQuery {
            entity_id: person_id.to_string(),
            period: crate::spec::PeriodSpec {
                kind: crate::spec::PeriodKindSpec::TaxYear,
                start: interval.start,
                end: interval.end,
            },
            outputs: vec![binding.rule_id.clone()],
            assessment_date: None,
        }],
    };
    let response = crate::api::execute_compiled_request(artifact.source_artifact.clone(), request)
        .map_err(|error| {
            UnitDerivationError::InvalidPlan(format!(
                "engine computation `{}` for child `{person_id}` failed: {error}",
                binding.rule_id
            ))
        })?;
    let output = response
        .results
        .first()
        .and_then(|result| result.outputs.get(&binding.rule_id))
        .ok_or_else(|| {
            UnitDerivationError::InvalidPlan(format!(
                "engine computation `{}` for child `{person_id}` returned no bound output",
                binding.rule_id
            ))
        })?;
    match output {
        crate::api::OutputValue::Scalar {
            value: crate::spec::ScalarValueSpec::Decimal { value },
            ..
        } => Ok(value.clone()),
        crate::api::OutputValue::Scalar {
            value: crate::spec::ScalarValueSpec::Integer { value },
            ..
        } => Ok(value.to_string()),
        _ => Err(UnitDerivationError::InvalidAggregationOperand {
            operation: input.name.clone(),
            input: binding.rule_id.clone(),
            expected: "engine-computed decimal or integer gross amount".to_string(),
            found: "non-numeric upstream output".to_string(),
        }),
    }
}

fn bind_known_input<T: Clone + Ord>(
    dataset: &mut crate::model::DataSet,
    unresolved: &mut Vec<InputKnowledgeRecord>,
    name: &str,
    entity: &str,
    entity_id: &str,
    interval: &crate::model::Interval,
    knowledge: &AggregationKnowledge<T>,
    convert: impl FnOnce(T) -> crate::model::ScalarValue,
) {
    match resolve_knowledge(Some(knowledge), &format!("input:{entity_id}:{name}")) {
        Ok(value) => dataset.add_input(name, entity, entity_id, interval.clone(), convert(value)),
        Err(reasons) => unresolved.push(InputKnowledgeRecord {
            name: name.to_string(),
            entity: entity.to_string(),
            entity_id: entity_id.to_string(),
            interval: interval.clone(),
            reduction: CompleteReduction::Indeterminate { reasons },
        }),
    }
}

fn bound_text(
    data: &super::PhaseTwoDataSet,
    name: &str,
    entity_id: &str,
    period: &crate::model::Period,
) -> Result<String, BTreeSet<String>> {
    match data.input_complete(name, entity_id, period) {
        CompleteReduction::Determined(crate::model::ScalarValue::Text(value)) => Ok(value),
        CompleteReduction::Determined(_) => Err(BTreeSet::from([format!(
            "input:{entity_id}:{name}:expected-text"
        )])),
        CompleteReduction::Indeterminate { reasons } => Err(reasons),
    }
}

fn bound_integer(
    data: &super::PhaseTwoDataSet,
    name: &str,
    entity_id: &str,
    period: &crate::model::Period,
) -> Result<i64, BTreeSet<String>> {
    match data.input_complete(name, entity_id, period) {
        CompleteReduction::Determined(crate::model::ScalarValue::Integer(value)) => Ok(value),
        CompleteReduction::Determined(_) => Err(BTreeSet::from([format!(
            "input:{entity_id}:{name}:expected-integer"
        )])),
        CompleteReduction::Indeterminate { reasons } => Err(reasons),
    }
}

fn bound_bool(
    data: &super::PhaseTwoDataSet,
    name: &str,
    entity_id: &str,
    period: &crate::model::Period,
) -> Result<bool, BTreeSet<String>> {
    match data.input_complete(name, entity_id, period) {
        CompleteReduction::Determined(crate::model::ScalarValue::Bool(value)) => Ok(value),
        CompleteReduction::Determined(_) => Err(BTreeSet::from([format!(
            "input:{entity_id}:{name}:expected-bool"
        )])),
        CompleteReduction::Indeterminate { reasons } => Err(reasons),
    }
}

fn validate_knowledge_evidence<T>(
    knowledge: &AggregationKnowledge<T>,
    label: &str,
) -> Result<(), UnitDerivationError> {
    let valid = match knowledge {
        AggregationKnowledge::Known { evidence, .. }
        | AggregationKnowledge::Unknown { evidence } => !evidence.id.is_empty(),
        AggregationKnowledge::Observations { observations }
        | AggregationKnowledge::Conflict { observations } => observations
            .iter()
            .all(|observation| !observation.evidence.id.is_empty()),
    };
    if valid {
        Ok(())
    } else {
        Err(UnitDerivationError::InvalidPlan(format!(
            "{label} has an empty evidence id"
        )))
    }
}

fn resolve_knowledge<T: Clone + Ord>(
    knowledge: Option<&AggregationKnowledge<T>>,
    label: &str,
) -> Result<T, BTreeSet<String>> {
    match knowledge {
        Some(AggregationKnowledge::Known { value, .. }) => Ok(value.clone()),
        Some(AggregationKnowledge::Unknown { evidence }) => {
            Err(BTreeSet::from([format!("{label}:unknown:{}", evidence.id)]))
        }
        Some(AggregationKnowledge::Observations { observations }) => {
            let distinct = observations
                .iter()
                .map(|observation| &observation.value)
                .collect::<BTreeSet<_>>();
            if distinct.len() == 1 {
                Ok((*distinct.into_iter().next().expect("one observation")).clone())
            } else {
                let mut reasons = observations
                    .iter()
                    .map(|observation| format!("{label}:conflict:{}", observation.evidence.id))
                    .collect::<BTreeSet<_>>();
                if reasons.is_empty() {
                    reasons.insert(format!("{label}:missing-observations"));
                }
                Err(reasons)
            }
        }
        Some(AggregationKnowledge::Conflict { observations }) => {
            let mut reasons = observations
                .iter()
                .map(|observation| format!("{label}:conflict:{}", observation.evidence.id))
                .collect::<BTreeSet<_>>();
            if reasons.is_empty() {
                reasons.insert(format!("{label}:conflict"));
            }
            Err(reasons)
        }
        None => Err(BTreeSet::from([format!("{label}:missing")])),
    }
}

#[derive(Clone, Debug)]
struct RelationshipRoleIndex {
    children: BTreeSet<String>,
    partner_pairs: BTreeSet<(String, String)>,
}

impl RelationshipRoleIndex {
    fn new(
        plan: &AggregationPlan,
        data: &super::PhaseTwoDataSet,
        _period: &crate::model::Period,
    ) -> Result<Self, UnitDerivationError> {
        let mut children = BTreeSet::new();
        let mut partner_pairs = BTreeSet::new();
        for fact in &data.dataset().relations {
            if fact.tuple.len() != 2 {
                continue;
            }
            if fact.name == plan.child_relation {
                children.insert(fact.tuple[1].clone());
            }
            if fact.name == plan.partner_relation {
                let (left, right) = if fact.tuple[0] <= fact.tuple[1] {
                    (fact.tuple[0].clone(), fact.tuple[1].clone())
                } else {
                    (fact.tuple[1].clone(), fact.tuple[0].clone())
                };
                partner_pairs.insert((left, right));
            }
        }
        Ok(Self {
            children,
            partner_pairs,
        })
    }
}

fn aggregate_family(
    plan: &AggregationPlan,
    data: &super::PhaseTwoDataSet,
    period: &crate::model::Period,
    roles: &RelationshipRoleIndex,
    provenance: &OperandProvenanceIndex,
    source_artifact_digest: &str,
    unit: &DerivedUnit,
) -> Result<AggregatedFamily, UnitDerivationError> {
    let member_set = unit.members.iter().cloned().collect::<BTreeSet<_>>();
    let family_children = roles
        .children
        .intersection(&member_set)
        .cloned()
        .collect::<BTreeSet<_>>();
    let adults = member_set
        .iter()
        .filter(|person| {
            bound_text(data, "__aggregation_person_role", person, period).as_deref() == Ok("adult")
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let partner_present = family_partner_present(plan, data, period, roles, unit);
    let mut scalars = BTreeMap::new();
    for aggregation in &plan.scalar_aggregations {
        let result = sum_person_scalars(
            data,
            period,
            &adults,
            &aggregation.input,
            plan.decimal_precision,
        )?;
        scalars.insert(aggregation.output.clone(), result);
    }

    let mut counts = BTreeMap::new();
    for count in &plan.child_counts {
        let mut value = 0_i64;
        let mut reasons = BTreeSet::new();
        for child_id in &family_children {
            match countable_child(plan, data, child_id, period) {
                Ok(false) => {}
                Ok(true) => match child_age(data, child_id, period) {
                    Ok(age) if age_in_range(age, count.minimum_age, count.maximum_age) => {
                        value += 1;
                    }
                    Ok(_) => {}
                    Err(unresolved) => reasons.extend(unresolved),
                },
                Err(unresolved) => reasons.extend(unresolved),
            }
        }
        counts.insert(
            count.output.clone(),
            if reasons.is_empty() {
                AggregationValue::determined(value)
            } else {
                AggregationValue::indeterminate(reasons)
            },
        );
    }
    for minimum in &plan.child_minima {
        let mut values = Vec::new();
        let mut reasons = BTreeSet::new();
        for child_id in &family_children {
            match countable_child(plan, data, child_id, period) {
                Ok(false) => {}
                Ok(true) => match child_age(data, child_id, period) {
                    Ok(age) => values.push(age),
                    Err(unresolved) => reasons.extend(unresolved),
                },
                Err(unresolved) => reasons.extend(unresolved),
            }
        }
        counts.insert(
            minimum.output.clone(),
            if reasons.is_empty() {
                AggregationValue::determined(
                    values.into_iter().min().unwrap_or(minimum.empty_value),
                )
            } else {
                AggregationValue::indeterminate(reasons)
            },
        );
    }

    let mut predicates = BTreeMap::new();
    for predicate in &plan.family_predicates {
        let value = match &predicate.operation {
            FamilyPredicateOperation::CountAtLeast { count, minimum } => {
                map_value(counts.get(count), &predicate.output, |value| {
                    value >= *minimum
                })
            }
            FamilyPredicateOperation::YoungestAgeAtLeast { youngest, minimum } => {
                map_value(counts.get(youngest), &predicate.output, |value| {
                    value >= *minimum
                })
            }
            FamilyPredicateOperation::SoleParentWithChildren { count } => combine_values(
                counts.get(count),
                Some(&partner_present),
                &predicate.output,
                |count, partnered| count > 0 && !partnered,
            ),
            FamilyPredicateOperation::SoleParentYoungestAgeAtLeast {
                count,
                youngest,
                minimum,
            } => combine_three_values(
                counts.get(count),
                counts.get(youngest),
                Some(&partner_present),
                &predicate.output,
                |count, youngest, partnered| count > 0 && youngest >= *minimum && !partnered,
            ),
        };
        predicates.insert(predicate.output.clone(), value);
    }

    for shape in &plan.family_shape_scalars {
        let value = match &shape.operation {
            FamilyShapeScalarOperation::EldestCareUnits { count } => {
                map_value(counts.get(count), &shape.output, |value| {
                    if value > 0 {
                        "1".to_string()
                    } else {
                        "0".to_string()
                    }
                })
            }
            FamilyShapeScalarOperation::SubsequentCareUnits { count } => {
                map_value(counts.get(count), &shape.output, |value| {
                    (value - 1).max(0).to_string()
                })
            }
            FamilyShapeScalarOperation::FixedIfChildren {
                count,
                present,
                absent,
            } => map_value(counts.get(count), &shape.output, |value| {
                if value > 0 {
                    present.clone()
                } else {
                    absent.clone()
                }
            }),
        };
        scalars.insert(shape.output.clone(), value);
    }

    for agreement in &plan.child_agreements {
        let mut values = BTreeSet::new();
        let mut reasons = BTreeSet::new();
        for child_id in &family_children {
            match countable_child(plan, data, child_id, period) {
                Ok(false) => {}
                Ok(true) => {
                    if let Some(condition) = &agreement.eligible_if {
                        match bound_bool(data, condition, child_id, period) {
                            Ok(false) => continue,
                            Ok(true) => {}
                            Err(unresolved) => {
                                reasons.extend(unresolved);
                                continue;
                            }
                        }
                    }
                    match bound_text(data, &agreement.input, child_id, period) {
                        Ok(value) => {
                            values.insert(value);
                        }
                        Err(unresolved) => reasons.extend(unresolved),
                    }
                }
                Err(unresolved) => reasons.extend(unresolved),
            }
        }
        if values.len() > 1 {
            reasons.insert(format!(
                "child-agreement:{}:values:{}",
                agreement.output,
                values.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        scalars.insert(
            agreement.output.clone(),
            if reasons.is_empty() {
                AggregationValue::determined(
                    values
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| agreement.empty_value.clone()),
                )
            } else {
                AggregationValue::indeterminate(reasons)
            },
        );
    }

    let mut children = Vec::new();
    for child_id in &family_children {
        let age_years = match child_age(data, child_id, period) {
            Ok(value) => AggregationValue::determined(value),
            Err(reasons) => AggregationValue::indeterminate(reasons),
        };
        let eligibility = countable_child(plan, data, child_id, period);
        let mut age_bands = BTreeMap::new();
        for count in &plan.child_counts {
            let value = match (&eligibility, child_age(data, child_id, period)) {
                (Ok(false), _) => AggregationValue::determined(false),
                (Ok(true), Ok(age)) => AggregationValue::determined(age_in_range(
                    age,
                    count.minimum_age,
                    count.maximum_age,
                )),
                (Err(left), Err(right)) => {
                    let mut reasons = left.clone();
                    reasons.extend(right);
                    AggregationValue::indeterminate(reasons)
                }
                (Err(reasons), _) => AggregationValue::indeterminate(reasons.clone()),
                (_, Err(reasons)) => AggregationValue::indeterminate(reasons),
            };
            age_bands.insert(count.output.clone(), value);
        }
        let mut child_scalars = BTreeMap::new();
        for projection in &plan.child_projections {
            let value = bound_text(data, &projection.input, child_id, period);
            child_scalars.insert(projection.output.clone(), result_value(value));
        }
        for broadcast in &plan.broadcasts {
            let value = bound_text(data, &broadcast.family_input, &unit.id, period);
            child_scalars.insert(broadcast.output.clone(), result_value(value));
        }
        children.push(AggregatedChild {
            person: child_id.clone(),
            family: unit.id.clone(),
            age_years,
            age_bands,
            scalars: child_scalars,
        });
    }
    children.sort_by(|left, right| left.person.cmp(&right.person));

    for reduction in &plan.family_reductions {
        apply_family_reduction(
            plan,
            data,
            period,
            &unit.id,
            &family_children,
            provenance,
            source_artifact_digest,
            reduction,
            &mut scalars,
        )?;
    }

    Ok(AggregatedFamily {
        id: unit.id.clone(),
        members: unit.members.clone(),
        partner_present,
        scalars,
        counts,
        predicates,
        children,
    })
}

fn family_partner_present(
    plan: &AggregationPlan,
    data: &super::PhaseTwoDataSet,
    period: &crate::model::Period,
    roles: &RelationshipRoleIndex,
    unit: &DerivedUnit,
) -> AggregationValue<bool> {
    let members = unit.members.iter().collect::<BTreeSet<_>>();
    let holder = bound_text(
        data,
        &plan.partner_presence.reference_person_input,
        &unit.id,
        period,
    );
    match holder {
        Ok(holder) => {
            AggregationValue::determined(roles.partner_pairs.iter().any(|(left, right)| {
                members.contains(&holder)
                    && members.contains(left)
                    && members.contains(right)
                    && (left == &holder || right == &holder)
            }))
        }
        Err(reasons) => AggregationValue::indeterminate(reasons),
    }
}

fn countable_child(
    plan: &AggregationPlan,
    data: &super::PhaseTwoDataSet,
    person_id: &str,
    period: &crate::model::Period,
) -> Result<bool, BTreeSet<String>> {
    let principal_care = bound_bool(data, &plan.care.principal_care_input, person_id, period)?;
    if !principal_care {
        return Ok(false);
    }
    let age = child_age(data, person_id, period)?;
    if age != plan.age_18_conditions.age {
        return Ok(true);
    }
    let mut reasons = BTreeSet::new();
    let mut all_hold = true;
    for input in [
        &plan.age_18_conditions.not_financially_independent_input,
        &plan.age_18_conditions.attending_school_or_tertiary_input,
        &plan.age_18_conditions.commissioner_period_input,
    ] {
        match bound_bool(data, input, person_id, period) {
            Ok(value) => all_hold &= value,
            Err(unresolved) => reasons.extend(unresolved),
        }
    }
    if reasons.is_empty() {
        Ok(all_hold)
    } else {
        Err(reasons)
    }
}

fn child_age(
    data: &super::PhaseTwoDataSet,
    person_id: &str,
    period: &crate::model::Period,
) -> Result<i64, BTreeSet<String>> {
    bound_integer(data, "age_years", person_id, period)
}

fn sum_person_scalars(
    data: &super::PhaseTwoDataSet,
    period: &crate::model::Period,
    selected: &BTreeSet<String>,
    input: &str,
    precision: u32,
) -> Result<AggregationValue<String>, UnitDerivationError> {
    let mut total = ExactDecimal::zero();
    let mut reasons = BTreeSet::new();
    for person_id in selected {
        match bound_text(data, input, person_id, period) {
            Ok(raw) => total.add_assign(
                parse_decimal(&raw, &format!("person `{person_id}` scalar `{input}`"))?,
                precision,
            ),
            Err(unresolved) => reasons.extend(unresolved),
        }
    }
    Ok(if reasons.is_empty() {
        AggregationValue::determined(decimal_text(total))
    } else {
        AggregationValue::indeterminate(reasons)
    })
}

fn map_value<T, U, F>(
    value: Option<&AggregationValue<T>>,
    label: &str,
    map: F,
) -> AggregationValue<U>
where
    T: Clone,
    F: FnOnce(T) -> U,
{
    match value {
        Some(AggregationValue::Determined { value }) => {
            AggregationValue::determined(map(value.clone()))
        }
        Some(AggregationValue::Indeterminate { reasons }) => {
            AggregationValue::indeterminate(reasons.clone())
        }
        None => AggregationValue::indeterminate(BTreeSet::from([format!(
            "derived-output:{label}:missing"
        )])),
    }
}

fn combine_values<T, U, V, F>(
    left: Option<&AggregationValue<T>>,
    right: Option<&AggregationValue<U>>,
    label: &str,
    map: F,
) -> AggregationValue<V>
where
    T: Clone,
    U: Clone,
    F: FnOnce(T, U) -> V,
{
    match (left, right) {
        (
            Some(AggregationValue::Determined { value: left }),
            Some(AggregationValue::Determined { value: right }),
        ) => AggregationValue::determined(map(left.clone(), right.clone())),
        _ => {
            let mut reasons = BTreeSet::new();
            for value in [
                left.and_then(|value| match value {
                    AggregationValue::Indeterminate { reasons } => Some(reasons),
                    AggregationValue::Determined { .. } => None,
                }),
                right.and_then(|value| match value {
                    AggregationValue::Indeterminate { reasons } => Some(reasons),
                    AggregationValue::Determined { .. } => None,
                }),
            ]
            .into_iter()
            .flatten()
            {
                reasons.extend(value.iter().cloned());
            }
            if reasons.is_empty() {
                reasons.insert(format!("derived-output:{label}:missing"));
            }
            AggregationValue::indeterminate(reasons)
        }
    }
}

fn combine_three_values<T, U, V, W, F>(
    first: Option<&AggregationValue<T>>,
    second: Option<&AggregationValue<U>>,
    third: Option<&AggregationValue<V>>,
    label: &str,
    map: F,
) -> AggregationValue<W>
where
    T: Clone,
    U: Clone,
    V: Clone,
    F: FnOnce(T, U, V) -> W,
{
    match (first, second, third) {
        (
            Some(AggregationValue::Determined { value: first }),
            Some(AggregationValue::Determined { value: second }),
            Some(AggregationValue::Determined { value: third }),
        ) => AggregationValue::determined(map(first.clone(), second.clone(), third.clone())),
        _ => {
            let mut reasons = BTreeSet::new();
            extend_value_reasons(first, &mut reasons);
            extend_value_reasons(second, &mut reasons);
            extend_value_reasons(third, &mut reasons);
            if reasons.is_empty() {
                reasons.insert(format!("derived-output:{label}:missing"));
            }
            AggregationValue::indeterminate(reasons)
        }
    }
}

fn extend_value_reasons<T>(value: Option<&AggregationValue<T>>, reasons: &mut BTreeSet<String>) {
    if let Some(AggregationValue::Indeterminate {
        reasons: unresolved,
    }) = value
    {
        reasons.extend(unresolved.iter().cloned());
    }
}

fn aggregated_family_reasons(family: &AggregatedFamily) -> BTreeSet<String> {
    let mut reasons = BTreeSet::new();
    extend_value_reasons(Some(&family.partner_present), &mut reasons);
    for value in family.scalars.values() {
        extend_value_reasons(Some(value), &mut reasons);
    }
    for value in family.counts.values() {
        extend_value_reasons(Some(value), &mut reasons);
    }
    for value in family.predicates.values() {
        extend_value_reasons(Some(value), &mut reasons);
    }
    for child in &family.children {
        extend_value_reasons(Some(&child.age_years), &mut reasons);
        for value in child.age_bands.values() {
            extend_value_reasons(Some(value), &mut reasons);
        }
        for value in child.scalars.values() {
            extend_value_reasons(Some(value), &mut reasons);
        }
    }
    reasons
}

fn result_value<T>(result: Result<T, BTreeSet<String>>) -> AggregationValue<T> {
    match result {
        Ok(value) => AggregationValue::determined(value),
        Err(reasons) => AggregationValue::indeterminate(reasons),
    }
}

fn apply_family_reduction(
    plan: &AggregationPlan,
    data: &super::PhaseTwoDataSet,
    period: &crate::model::Period,
    family_id: &str,
    children: &BTreeSet<String>,
    provenance: &OperandProvenanceIndex,
    source_artifact_digest: &str,
    reduction: &FamilyReduction,
    scalars: &mut BTreeMap<String, AggregationValue<String>>,
) -> Result<(), UnitDerivationError> {
    let child_input = reduction.child_gross_input.as_deref().unwrap();
    let adjustment_input = reduction.family_adjustment_input.as_deref().unwrap();
    let care_input = reduction.care_fraction_input.as_deref().unwrap();
    let binding = plan
        .inputs
        .iter()
        .find(|input| input.name == child_input)
        .and_then(|input| input.engine_computation.as_ref())
        .expect("validated child-gross inputs have an engine computation binding");
    let expected_provenance = OperandProvenance::EngineComputed {
        source_artifact_digest: source_artifact_digest.to_string(),
        rule_id: binding.rule_id.clone(),
        stage: EngineComputationStage::Gross,
    };
    let mut gross = ExactDecimal::zero();
    let mut reasons = BTreeSet::new();
    for child_id in children {
        let observed_provenance = provenance.get(&(child_input.to_string(), child_id.clone()));
        if observed_provenance != Some(&expected_provenance) {
            return Err(UnitDerivationError::InvalidChildGrossProvenance {
                operation: reduction.output.clone(),
                input: child_input.to_string(),
                child: child_id.clone(),
                expected: expected_provenance.label(),
                found: observed_provenance
                    .map(OperandProvenance::label)
                    .unwrap_or_else(|| "missing".to_string()),
            });
        }
        let eligible = match countable_child(plan, data, child_id, period) {
            Ok(false) => continue,
            Ok(true) => match child_age(data, child_id, period) {
                Ok(age) => age_in_range(age, reduction.minimum_age, reduction.maximum_age),
                Err(unresolved) => {
                    reasons.extend(unresolved);
                    continue;
                }
            },
            Err(unresolved) => {
                reasons.extend(unresolved);
                continue;
            }
        };
        if !eligible {
            continue;
        }
        let raw = bound_text(data, child_input, child_id, period);
        let care = bound_text(data, care_input, child_id, period);
        match (raw, care) {
            (Ok(raw), Ok(care)) => {
                let raw =
                    parse_decimal(&raw, &format!("child `{child_id}` scalar `{child_input}`"))?;
                let care =
                    parse_decimal(&care, &format!("child `{child_id}` scalar `{care_input}`"))?;
                if !care.is_fraction() {
                    return Err(UnitDerivationError::InvalidPlan(format!(
                        "child `{child_id}` care fraction `{care_input}` must be between 0 and 1"
                    )));
                }
                gross.add_assign(
                    raw.multiply(care, plan.decimal_precision),
                    plan.decimal_precision,
                );
            }
            (Err(left), Err(right)) => {
                reasons.extend(left);
                reasons.extend(right);
            }
            (Err(unresolved), _) | (_, Err(unresolved)) => reasons.extend(unresolved),
        }
    }
    let adjustment = bound_text(data, adjustment_input, family_id, period);
    let adjustment = match adjustment {
        Ok(raw) => Some(parse_decimal(
            &raw,
            &format!("family scalar `{adjustment_input}`"),
        )?),
        Err(unresolved) => {
            reasons.extend(unresolved);
            None
        }
    };
    if !reasons.is_empty() {
        scalars.insert(
            reduction.gross_output.clone(),
            AggregationValue::indeterminate(reasons.clone()),
        );
        scalars.insert(
            reduction.output.clone(),
            AggregationValue::indeterminate(reasons.clone()),
        );
        if let Some(continuous) = &reduction.continuous {
            scalars.insert(
                continuous.output.clone(),
                AggregationValue::indeterminate(reasons),
            );
        }
        return Ok(());
    }
    let adjustment = adjustment.expect("known adjustment when no unresolved reasons");
    let reduced = match reduction.operation {
        FamilyReductionOperation::SumChildrenThenSubtractFamilyOnce => gross
            .clone()
            .subtract(adjustment)
            .round_significant(plan.decimal_precision)
            .max_zero(),
    };
    scalars.insert(
        reduction.gross_output.clone(),
        AggregationValue::determined(decimal_text(gross.clone())),
    );
    scalars.insert(
        reduction.output.clone(),
        AggregationValue::determined(decimal_text(reduced)),
    );
    if let Some(continuous) = &reduction.continuous {
        let continuous_value =
            continuous_reduction(plan, data, period, family_id, &gross, continuous)?;
        scalars.insert(continuous.output.clone(), continuous_value);
    }
    Ok(())
}

fn continuous_reduction(
    plan: &AggregationPlan,
    data: &super::PhaseTwoDataSet,
    period: &crate::model::Period,
    family_id: &str,
    gross: &ExactDecimal,
    continuous: &ContinuousFamilyReduction,
) -> Result<AggregationValue<String>, UnitDerivationError> {
    let mut reasons = BTreeSet::new();
    let mut read = |name: &str| -> Result<Option<ExactDecimal>, UnitDerivationError> {
        match bound_text(data, name, family_id, period) {
            Ok(raw) => Ok(Some(parse_decimal(
                &raw,
                &format!("family scalar `{name}`"),
            )?)),
            Err(unresolved) => {
                reasons.extend(unresolved);
                Ok(None)
            }
        }
    };
    let income = read(&continuous.family_income_input)?;
    let threshold = read(&continuous.threshold_input)?;
    let rate = read(&continuous.rate_input)?;
    if !reasons.is_empty() {
        return Ok(AggregationValue::indeterminate(reasons));
    }
    let excess = income.unwrap().subtract(threshold.unwrap()).max_zero();
    let abatement = excess.multiply(rate.unwrap(), plan.decimal_precision);
    Ok(AggregationValue::determined(decimal_text(
        gross
            .clone()
            .subtract(abatement)
            .round_significant(plan.decimal_precision)
            .max_zero(),
    )))
}

fn age_in_range(age: i64, minimum: Option<i64>, maximum: Option<i64>) -> bool {
    minimum.is_none_or(|minimum| age >= minimum) && maximum.is_none_or(|maximum| age <= maximum)
}

fn aggregation_trace_root(
    artifact: &CompiledAggregationArtifact,
    request: &AggregationRequest,
    constitution_trace: &str,
    families: &AggregationFamilyKnowledge,
) -> Result<String, UnitDerivationError> {
    let request = normalized_request(request);
    let mut preimage = b"axiom.unit-aggregation.trace.stage3\0".to_vec();
    push_len_prefixed(&mut preimage, artifact.plan_digest.as_bytes());
    push_len_prefixed(&mut preimage, artifact.source_artifact_digest.as_bytes());
    push_len_prefixed(&mut preimage, constitution_trace.as_bytes());
    let request_bytes = serde_json::to_vec(&request).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not encode aggregation request trace: {error}"
        ))
    })?;
    let result_bytes = serde_json::to_vec(families).map_err(|error| {
        UnitDerivationError::InvalidAggregationArtifact(format!(
            "could not encode aggregation result trace: {error}"
        ))
    })?;
    push_len_prefixed(&mut preimage, &request_bytes);
    push_len_prefixed(&mut preimage, &result_bytes);
    Ok(digest_text(&preimage))
}

fn normalized_request(request: &AggregationRequest) -> AggregationRequest {
    let mut normalized = request.clone();
    normalized
        .persons
        .sort_by(|left, right| left.id.cmp(&right.id));
    for person in &mut normalized.persons {
        if let Some(age) = &mut person.age_years {
            normalize_knowledge(age);
        }
        for value in person.scalars.values_mut() {
            normalize_knowledge(value);
        }
        for value in person.facts.values_mut() {
            normalize_knowledge(value);
        }
    }
    normalized
        .relations
        .sort_by(|left, right| left.name.cmp(&right.name));
    for family in &mut normalized.relations {
        for fact in &mut family.facts {
            normalize_knowledge(&mut fact.knowledge);
        }
        family.facts.sort_by(|left, right| {
            left.tuple.cmp(&right.tuple).then_with(|| {
                serde_json::to_vec(&left.knowledge)
                    .expect("relation Knowledge is serializable")
                    .cmp(
                        &serde_json::to_vec(&right.knowledge)
                            .expect("relation Knowledge is serializable"),
                    )
            })
        });
        family
            .facts
            .dedup_by(|left, right| left.tuple == right.tuple && left.knowledge == right.knowledge);
    }
    normalized
        .family_inputs
        .sort_by(|left, right| left.anchor_person.cmp(&right.anchor_person));
    for family in &mut normalized.family_inputs {
        for value in family.named_people.values_mut() {
            normalize_knowledge(value);
        }
        for value in family.scalars.values_mut() {
            normalize_knowledge(value);
        }
    }
    normalized
}

fn normalize_knowledge<T: Ord>(knowledge: &mut AggregationKnowledge<T>) {
    if let AggregationKnowledge::Observations { observations }
    | AggregationKnowledge::Conflict { observations } = knowledge
    {
        observations.sort_by(|left, right| {
            (&left.value, &left.evidence.id).cmp(&(&right.value, &right.evidence.id))
        });
        observations.dedup_by(|left, right| {
            left.value == right.value && left.evidence.id == right.evidence.id
        });
    }
}

fn push_len_prefixed(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactDecimal {
    coefficient: BigInt,
    scale: u32,
}

impl ExactDecimal {
    fn zero() -> Self {
        Self {
            coefficient: BigInt::from(0),
            scale: 0,
        }
    }

    fn one() -> Self {
        Self {
            coefficient: BigInt::from(1),
            scale: 0,
        }
    }

    fn new(mut coefficient: BigInt, mut scale: u32) -> Self {
        while scale > 0 && (&coefficient % 10_u8) == BigInt::from(0) {
            coefficient /= 10_u8;
            scale -= 1;
        }
        Self { coefficient, scale }
    }

    fn ten_to(power: u32) -> BigInt {
        BigInt::from(10_u8).pow(power)
    }

    fn aligned_coefficient(&self, scale: u32) -> BigInt {
        &self.coefficient * Self::ten_to(scale - self.scale)
    }

    fn add_assign(&mut self, other: Self, precision: u32) {
        let scale = self.scale.max(other.scale);
        *self = Self::new(
            self.aligned_coefficient(scale) + other.aligned_coefficient(scale),
            scale,
        )
        .round_significant(precision);
    }

    fn subtract(self, other: Self) -> Self {
        let scale = self.scale.max(other.scale);
        Self::new(
            self.aligned_coefficient(scale) - other.aligned_coefficient(scale),
            scale,
        )
    }

    fn multiply(self, other: Self, precision: u32) -> Self {
        Self::new(
            self.coefficient * other.coefficient,
            self.scale.saturating_add(other.scale),
        )
        .round_significant(precision)
    }

    fn max_zero(self) -> Self {
        if self.coefficient < BigInt::from(0) {
            Self::zero()
        } else {
            self
        }
    }

    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        let scale = self.scale.max(other.scale);
        self.aligned_coefficient(scale)
            .cmp(&other.aligned_coefficient(scale))
    }

    fn is_fraction(&self) -> bool {
        self.compare(&Self::zero()) != std::cmp::Ordering::Less
            && self.compare(&Self::one()) != std::cmp::Ordering::Greater
    }

    /// Apply a finite significant-digit context with round-half-even. The NZ
    /// comparison records 40 as an explicit reproducibility input.
    fn round_significant(self, precision: u32) -> Self {
        let negative = self.coefficient < BigInt::from(0);
        let absolute = if negative {
            -self.coefficient.clone()
        } else {
            self.coefficient.clone()
        };
        let digits = absolute.to_string().len() as u32;
        if digits <= precision {
            return self;
        }
        let dropped = digits - precision;
        let divisor = Self::ten_to(dropped);
        let mut quotient = &absolute / &divisor;
        let remainder = &absolute % &divisor;
        let doubled = remainder * 2_u8;
        let odd = (&quotient % 2_u8) != BigInt::from(0);
        if doubled > divisor || (doubled == divisor && odd) {
            quotient += 1_u8;
        }
        if negative {
            quotient = -quotient;
        }
        if dropped <= self.scale {
            Self::new(quotient, self.scale - dropped)
        } else {
            Self::new(quotient * Self::ten_to(dropped - self.scale), 0)
        }
    }

    fn to_text(&self) -> String {
        if self.coefficient == BigInt::from(0) {
            return "0".to_string();
        }
        let signed = self.coefficient.to_string();
        let (sign, digits) = signed
            .strip_prefix('-')
            .map_or(("", signed.as_str()), |digits| ("-", digits));
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        let scale = self.scale as usize;
        if digits.len() <= scale {
            format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
        } else {
            let split = digits.len() - scale;
            format!("{sign}{}.{}", &digits[..split], &digits[split..])
        }
    }
}

fn parse_decimal(raw: &str, label: &str) -> Result<ExactDecimal, UnitDerivationError> {
    let raw = raw.trim();
    let (mantissa, exponent) = match raw.find(['e', 'E']) {
        Some(index) => {
            if raw[index + 1..].contains(['e', 'E']) {
                return Err(UnitDerivationError::InvalidPlan(format!(
                    "{label} is not a decimal: repeated exponent"
                )));
            }
            let exponent = raw[index + 1..].parse::<i64>().map_err(|error| {
                UnitDerivationError::InvalidPlan(format!(
                    "{label} is not a decimal: invalid exponent: {error}"
                ))
            })?;
            (&raw[..index], exponent)
        }
        None => (raw, 0),
    };
    let (negative, unsigned) = match mantissa.as_bytes().first() {
        Some(b'-') => (true, &mantissa[1..]),
        Some(b'+') => (false, &mantissa[1..]),
        _ => (false, mantissa),
    };
    let mut parts = unsigned.split('.');
    let integer = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || (integer.is_empty() && fraction.is_empty())
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(UnitDerivationError::InvalidPlan(format!(
            "{label} is not a decimal: invalid digits"
        )));
    }
    let digits = format!("{integer}{fraction}");
    let mut coefficient = BigInt::parse_bytes(digits.as_bytes(), 10).ok_or_else(|| {
        UnitDerivationError::InvalidPlan(format!("{label} is not a decimal: invalid coefficient"))
    })?;
    if negative {
        coefficient = -coefficient;
    }
    let scale = i64::try_from(fraction.len())
        .ok()
        .and_then(|fraction| fraction.checked_sub(exponent))
        .ok_or_else(|| {
            UnitDerivationError::InvalidPlan(format!("{label} is not a decimal: scale overflow"))
        })?;
    if scale < 0 {
        let power = u32::try_from(-scale).map_err(|_| {
            UnitDerivationError::InvalidPlan(format!("{label} is not a decimal: scale overflow"))
        })?;
        coefficient *= ExactDecimal::ten_to(power);
        Ok(ExactDecimal::new(coefficient, 0))
    } else {
        let scale = u32::try_from(scale).map_err(|_| {
            UnitDerivationError::InvalidPlan(format!("{label} is not a decimal: scale overflow"))
        })?;
        Ok(ExactDecimal::new(coefficient, scale))
    }
}

fn decimal_text(value: ExactDecimal) -> String {
    value.to_text()
}
