use std::collections::BTreeMap;

use axiom_rules_engine::compile::{
    CompileError, CompiledInputKind, CompiledNodeMetadata, CompiledNodeState,
    CompiledProgramArtifact,
};
use axiom_rules_engine::spec::{
    DerivedSemanticsSpec, InputStateSpec, NodeKindSpec, NodeProvenanceSpec, ScalarExprSpec,
};

const GRAPH_RULESPEC: &str = r#"
format: rulespec/v1
module:
  source_verification:
    corpus_citation_path: us/statutes/26/32/j
rules:
  - name: amount_by_size
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2026-01-01
        values:
          1: 10
          2: 20
  - name: used_intermediate
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: amount_by_size[household_size] + observed_income
  - name: dated_intermediate
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2025-01-01
        effective_to: 2025-12-31
        formula: old_input
      - effective_from: 2026-01-01
        formula: new_input
  - name: result
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: used_intermediate + dated_intermediate + upstream_amount + missing_calc
  - name: unused
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: unused_input
"#;

fn annotated_program() -> axiom_rules_engine::spec::ProgramSpec {
    let mut program = CompiledProgramArtifact::from_rulespec_str(GRAPH_RULESPEC)
        .expect("fixture lowers")
        .program;
    program.outputs = Some(vec!["result".to_string()]);
    program.input_states = BTreeMap::from([
        ("household_size".to_string(), InputStateSpec::Exogenous),
        ("missing_calc".to_string(), InputStateSpec::Pending),
        ("new_input".to_string(), InputStateSpec::Exogenous),
        ("observed_income".to_string(), InputStateSpec::Exogenous),
        ("old_input".to_string(), InputStateSpec::Exogenous),
        ("upstream_amount".to_string(), InputStateSpec::PolicyDerived),
        ("unused_input".to_string(), InputStateSpec::Exogenous),
    ]);
    program
}

fn node<'a>(
    nodes: &'a [CompiledNodeMetadata],
    kind: NodeKindSpec,
    name: &str,
) -> &'a CompiledNodeMetadata {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("{kind:?} node `{name}`"))
}

#[test]
fn complete_contract_emits_state_reachability_and_provenance() {
    let artifact =
        CompiledProgramArtifact::compile(annotated_program()).expect("annotated graph compiles");
    let nodes = artifact
        .metadata
        .nodes
        .as_deref()
        .expect("complete contract emits nodes");

    let observed = node(nodes, NodeKindSpec::Input, "observed_income");
    assert_eq!(observed.state, CompiledNodeState::Input);
    assert_eq!(observed.input_kind, Some(CompiledInputKind::Exogenous));
    assert!(observed.reachable);
    assert_eq!(observed.provenance, NodeProvenanceSpec::Unverified);

    let upstream = node(nodes, NodeKindSpec::Input, "upstream_amount");
    assert_eq!(upstream.state, CompiledNodeState::Input);
    assert_eq!(upstream.input_kind, Some(CompiledInputKind::PolicyDerived));
    assert!(upstream.reachable);

    let pending = node(nodes, NodeKindSpec::Input, "missing_calc");
    assert_eq!(pending.state, CompiledNodeState::Pending);
    assert_eq!(pending.input_kind, None);
    assert!(pending.reachable);

    assert!(
        node(nodes, NodeKindSpec::Input, "old_input").reachable,
        "dependencies from every dated version are structurally reachable"
    );
    assert!(node(nodes, NodeKindSpec::Input, "new_input").reachable);
    assert!(!node(nodes, NodeKindSpec::Input, "unused_input").reachable);
    assert!(node(nodes, NodeKindSpec::Parameter, "amount_by_size").reachable);
    assert!(node(nodes, NodeKindSpec::Derived, "used_intermediate").reachable);
    assert!(!node(nodes, NodeKindSpec::Derived, "unused").reachable);

    for name in [
        "amount_by_size",
        "used_intermediate",
        "dated_intermediate",
        "result",
        "unused",
    ] {
        let kind = if name == "amount_by_size" {
            NodeKindSpec::Parameter
        } else {
            NodeKindSpec::Derived
        };
        assert_eq!(
            node(nodes, kind, name).provenance,
            NodeProvenanceSpec::ProvisionBacked,
            "the in-memory atomic surface remains distinguishable from a composition"
        );
        assert_eq!(
            node(nodes, kind, name).corpus_citation_path.as_deref(),
            Some("us/statutes/26/32/j")
        );
    }

    let mut sorted = nodes.to_vec();
    sorted.sort_by(|left, right| {
        (left.kind, left.id.as_str(), left.name.as_str()).cmp(&(
            right.kind,
            right.id.as_str(),
            right.name.as_str(),
        ))
    });
    assert_eq!(nodes, sorted, "node catalog order is deterministic");
}

#[test]
fn legacy_artifact_without_contract_loads_without_inventing_annotations() {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(GRAPH_RULESPEC).expect("legacy source compiles");
    assert_eq!(artifact.metadata.nodes, None);

    let mut value = serde_json::to_value(&artifact).expect("artifact serializes");
    value["program"]
        .as_object_mut()
        .expect("program object")
        .remove("node_provenance");
    value["metadata"]
        .as_object_mut()
        .expect("metadata object")
        .remove("nodes");
    let loaded = CompiledProgramArtifact::from_json_str(
        &serde_json::to_string(&value).expect("legacy JSON serializes"),
    )
    .expect("legacy v2 artifact still loads");
    assert_eq!(loaded.metadata.nodes, None);
}

#[test]
fn declared_outputs_must_be_nonempty_known_and_unique() {
    let mut empty = annotated_program();
    empty.outputs = Some(vec![]);
    assert!(matches!(
        CompiledProgramArtifact::compile(empty),
        Err(CompileError::EmptyDeclaredOutputs)
    ));

    let mut unknown = annotated_program();
    unknown.outputs = Some(vec!["does_not_exist".to_string()]);
    assert!(matches!(
        CompiledProgramArtifact::compile(unknown),
        Err(CompileError::UnknownDeclaredOutput { .. })
    ));

    let mut duplicate = annotated_program();
    duplicate.outputs = Some(vec!["result".to_string(), "result".to_string()]);
    assert!(matches!(
        CompiledProgramArtifact::compile(duplicate),
        Err(CompileError::DuplicateDeclaredOutput { .. })
    ));
}

#[test]
fn input_state_declarations_must_cover_exactly_the_runtime_slots() {
    let mut without_outputs = annotated_program();
    without_outputs.outputs = None;
    assert!(matches!(
        CompiledProgramArtifact::compile(without_outputs),
        Err(CompileError::InputStatesWithoutOutputs)
    ));

    let mut missing = annotated_program();
    missing.input_states.remove("missing_calc");
    assert!(matches!(
        CompiledProgramArtifact::compile(missing),
        Err(CompileError::MissingInputStates { .. })
    ));

    let mut unknown = annotated_program();
    unknown
        .input_states
        .insert("not_a_slot".to_string(), InputStateSpec::Exogenous);
    assert!(matches!(
        CompiledProgramArtifact::compile(unknown),
        Err(CompileError::UnknownInputStates { .. })
    ));
}

#[test]
fn relation_state_declarations_must_cover_exactly_the_runtime_data_relations() {
    let relation = axiom_rules_engine::spec::RelationSpec {
        name: "member_of_household".to_string(),
        arity: 2,
        derivation: None,
    };

    let mut without_outputs = annotated_program();
    without_outputs.outputs = None;
    without_outputs.input_states.clear();
    without_outputs
        .relation_states
        .insert(relation.name.clone(), InputStateSpec::Exogenous);
    assert!(matches!(
        CompiledProgramArtifact::compile(without_outputs),
        Err(CompileError::RelationStatesWithoutOutputs)
    ));

    let mut missing = annotated_program();
    missing.relations.push(relation);
    assert!(matches!(
        CompiledProgramArtifact::compile(missing),
        Err(CompileError::MissingRelationStates { .. })
    ));

    let mut unknown = annotated_program();
    unknown
        .relation_states
        .insert("not_a_relation".to_string(), InputStateSpec::Exogenous);
    assert!(matches!(
        CompiledProgramArtifact::compile(unknown),
        Err(CompileError::UnknownRelationStates { .. })
    ));
}

#[test]
fn duplicate_parameter_and_relation_nodes_are_rejected() {
    let mut duplicate_parameter = annotated_program();
    duplicate_parameter
        .parameters
        .push(duplicate_parameter.parameters[0].clone());
    assert!(matches!(
        CompiledProgramArtifact::compile(duplicate_parameter),
        Err(CompileError::DuplicateParameterNode { .. })
    ));

    let mut duplicate_relation = annotated_program();
    let relation = axiom_rules_engine::spec::RelationSpec {
        name: "member_of_household".to_string(),
        arity: 2,
        derivation: None,
    };
    duplicate_relation.relations.push(relation.clone());
    duplicate_relation.relations.push(relation);
    duplicate_relation
        .relation_states
        .insert("member_of_household".to_string(), InputStateSpec::Exogenous);
    assert!(matches!(
        CompiledProgramArtifact::compile(duplicate_relation),
        Err(CompileError::DuplicateRelationNode { .. })
    ));
}

#[test]
fn node_provenance_declarations_must_resolve_uniquely() {
    let mut undeclared = annotated_program();
    undeclared
        .node_provenance
        .retain(|entry| !(entry.kind == NodeKindSpec::Parameter && entry.name == "amount_by_size"));
    let artifact = CompiledProgramArtifact::compile(undeclared)
        .expect("missing sidecar provenance remains fail-closed");
    let parameter = node(
        artifact.metadata.nodes.as_deref().expect("node metadata"),
        NodeKindSpec::Parameter,
        "amount_by_size",
    );
    assert_eq!(parameter.provenance, NodeProvenanceSpec::Unverified);
    assert_eq!(parameter.corpus_citation_path, None);

    let mut duplicate = annotated_program();
    let existing = duplicate
        .node_provenance
        .first()
        .expect("RuleSpec lowering supplies provenance")
        .clone();
    duplicate.node_provenance.push(existing);
    assert!(matches!(
        CompiledProgramArtifact::compile(duplicate),
        Err(CompileError::DuplicateNodeProvenance { .. })
    ));

    let mut unknown = annotated_program();
    unknown
        .node_provenance
        .push(axiom_rules_engine::spec::NodeProvenanceEntrySpec {
            kind: NodeKindSpec::Derived,
            name: "not_a_node".to_string(),
            provenance: NodeProvenanceSpec::ProvisionBacked,
            corpus_citation_path: Some("us/statutes/26/32/j".to_string()),
        });
    assert!(matches!(
        CompiledProgramArtifact::compile(unknown),
        Err(CompileError::UnknownNodeProvenance { .. })
    ));

    let mut ungrounded = annotated_program();
    ungrounded.node_provenance[0].corpus_citation_path = None;
    assert!(matches!(
        CompiledProgramArtifact::compile(ungrounded),
        Err(CompileError::InvalidNodeProvenance { .. })
    ));

    let mut mismatched = annotated_program();
    mismatched.node_provenance[0].corpus_citation_path = Some("us/statutes/7/2014/e".to_string());
    assert!(matches!(
        CompiledProgramArtifact::compile(mismatched),
        Err(CompileError::InvalidNodeProvenance { .. })
    ));

    let mut invalid_relation_path = annotated_program();
    invalid_relation_path
        .relations
        .push(axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            derivation: None,
        });
    invalid_relation_path
        .relation_states
        .insert("member_of_household".to_string(), InputStateSpec::Exogenous);
    invalid_relation_path
        .node_provenance
        .push(axiom_rules_engine::spec::NodeProvenanceEntrySpec {
            kind: NodeKindSpec::DataRelation,
            name: "member_of_household".to_string(),
            provenance: NodeProvenanceSpec::ProvisionBacked,
            corpus_citation_path: Some("not canonical".to_string()),
        });
    let error = CompiledProgramArtifact::compile(invalid_relation_path)
        .expect_err("non-canonical relation backing must fail");
    assert!(
        matches!(error, CompileError::InvalidNodeProvenance { .. }),
        "{error:?}"
    );
}

#[test]
fn present_node_metadata_is_recomputed_and_tampering_is_rejected() {
    let artifact =
        CompiledProgramArtifact::compile(annotated_program()).expect("annotated graph compiles");
    let mut value = serde_json::to_value(&artifact).expect("artifact serializes");
    let reachable = value["metadata"]["nodes"][0]["reachable"]
        .as_bool()
        .expect("reachable boolean");
    value["metadata"]["nodes"][0]["reachable"] = serde_json::json!(!reachable);
    let error = CompiledProgramArtifact::from_json_str(
        &serde_json::to_string(&value).expect("tampered JSON serializes"),
    )
    .expect_err("tampered derived metadata must fail");
    assert!(matches!(
        error,
        CompileError::InvalidArtifactContract { .. }
    ));
}

#[test]
fn reachability_traverses_relations_and_related_inputs() {
    let mut program = annotated_program();
    program
        .relations
        .push(axiom_rules_engine::spec::RelationSpec {
            name: "member_of_household".to_string(),
            arity: 2,
            derivation: None,
        });
    program
        .relations
        .push(axiom_rules_engine::spec::RelationSpec {
            name: "future_relation".to_string(),
            arity: 2,
            derivation: None,
        });
    program.relation_states.insert(
        "member_of_household".to_string(),
        InputStateSpec::PolicyDerived,
    );
    program
        .relation_states
        .insert("future_relation".to_string(), InputStateSpec::Pending);
    let result = program
        .derived
        .iter_mut()
        .find(|derived| derived.name == "result")
        .expect("result rule");
    let DerivedSemanticsSpec::Scalar { expr } = &mut result.semantics else {
        panic!("scalar result");
    };
    let ScalarExprSpec::Add { items } = expr else {
        panic!("additive result");
    };
    items.push(ScalarExprSpec::SumRelated {
        relation: "member_of_household".to_string(),
        current_slot: 1,
        related_slot: 0,
        value: axiom_rules_engine::spec::RelatedValueRefSpec::Input {
            name: "related_amount".to_string(),
        },
        where_clause: None,
    });
    program
        .input_states
        .insert("related_amount".to_string(), InputStateSpec::Exogenous);

    let artifact = CompiledProgramArtifact::compile(program).expect("relation graph compiles");
    let nodes = artifact.metadata.nodes.as_deref().expect("node catalog");
    let relation = node(nodes, NodeKindSpec::DataRelation, "member_of_household");
    assert!(relation.reachable);
    assert_eq!(relation.state, CompiledNodeState::Input);
    assert_eq!(relation.input_kind, Some(CompiledInputKind::PolicyDerived));
    let pending_relation = node(nodes, NodeKindSpec::DataRelation, "future_relation");
    assert_eq!(pending_relation.state, CompiledNodeState::Pending);
    assert_eq!(pending_relation.input_kind, None);
    assert!(!pending_relation.reachable);
    assert!(node(nodes, NodeKindSpec::Input, "related_amount").reachable);
}
