from __future__ import annotations

from typing import Any

import pytest
from pydantic import ValidationError

from axiom_rules_engine import (
    CompiledDenseProgram,
    CompiledNodeMetadata,
    NodeMetadata,
    NodeProvenanceEntry,
)
from axiom_rules_engine.dense import NativeCompiledDenseProgram
from axiom_rules_engine.models import (
    CompiledProgram,
    Program,
)


def _artifact(metadata: dict[str, Any]) -> dict[str, Any]:
    return {
        "artifact_format_version": 2,
        "program": {
            "units": [],
            "relations": [],
            "parameters": [],
            "derived": [],
        },
        "metadata": metadata,
    }


def _legacy_metadata() -> dict[str, Any]:
    return {
        "evaluation_order": [],
        "fast_path": {
            "strategy": "generic_bulk",
            "compatible": True,
            "blockers": [],
        },
        "input_catalog": [],
    }


def test_legacy_metadata_without_node_catalog_round_trips() -> None:
    compiled = CompiledProgram.model_validate(_artifact(_legacy_metadata()))

    assert compiled.metadata.nodes is None
    dumped = compiled.model_dump(mode="json", exclude_none=True)
    assert "nodes" not in dumped["metadata"]
    assert CompiledProgram.model_validate(dumped) == compiled


def test_annotated_node_catalog_round_trips_as_typed_models() -> None:
    nodes = [
        {
            "id": "us:policies/example#input.reported_income",
            "name": "reported_income",
            "kind": "input",
            "state": "input",
            "input_kind": "policy_derived",
            "reachable": True,
            "provenance": "unverified",
        },
        {
            "id": "us:statute/example#benefit",
            "name": "benefit",
            "kind": "derived",
            "state": "derived",
            "reachable": True,
            "provenance": "provision_backed",
            "corpus_citation_path": "us/statutes/7/2014/e",
        },
        {
            "id": "future_intermediate",
            "name": "future_intermediate",
            "kind": "input",
            "state": "pending",
            "reachable": False,
            "provenance": "unverified",
        },
    ]
    metadata = {**_legacy_metadata(), "nodes": nodes}

    compiled = CompiledProgram.model_validate(_artifact(metadata))

    assert compiled.metadata.nodes is not None
    assert all(
        isinstance(node, CompiledNodeMetadata) for node in compiled.metadata.nodes
    )
    assert compiled.metadata.nodes[0].input_kind == "policy_derived"
    assert compiled.metadata.nodes[1].input_kind is None
    assert (
        compiled.metadata.nodes[1].corpus_citation_path
        == "us/statutes/7/2014/e"
    )
    dumped = compiled.model_dump(mode="json", exclude_none=True)
    assert dumped["metadata"]["nodes"] == nodes
    assert CompiledProgram.model_validate(dumped) == compiled


def test_program_annotation_fields_are_typed_and_round_trip() -> None:
    program = Program.model_validate(
        {
            "outputs": ["benefit"],
            "input_states": {
                "observed_income": "exogenous",
                "upstream_result": "policy_derived",
                "future_intermediate": "pending",
            },
            "relation_states": {"household_members": "exogenous"},
            "node_provenance": [
                {
                    "kind": "derived",
                    "name": "benefit",
                    "provenance": "provision_backed",
                    "corpus_citation_path": "us/statutes/7/2014/e",
                }
            ],
        }
    )

    assert program.outputs == ["benefit"]
    assert program.input_states["upstream_result"] == "policy_derived"
    assert isinstance(program.node_provenance[0], NodeProvenanceEntry)
    assert Program.model_validate(program.model_dump(mode="json")) == program


@pytest.mark.parametrize(
    "fields, message",
    [
        (
            {
                "kind": "derived",
                "state": "input",
                "input_kind": "exogenous",
                "provenance": "unverified",
            },
            "derived node cannot have state input",
        ),
        (
            {
                "kind": "input",
                "state": "input",
                "input_kind": None,
                "provenance": "unverified",
            },
            "input_kind is required exactly when state is input",
        ),
        (
            {
                "kind": "input",
                "state": "pending",
                "input_kind": "policy_derived",
                "provenance": "unverified",
            },
            "input_kind is required exactly when state is input",
        ),
        (
            {
                "kind": "input",
                "state": "input",
                "input_kind": "exogenous",
                "provenance": "synthesized",
            },
            "implicit input nodes must have unverified provenance",
        ),
        (
            {
                "kind": "derived",
                "state": "derived",
                "input_kind": None,
                "provenance": "provision_backed",
            },
            "provision_backed requires corpus_citation_path",
        ),
        (
            {
                "kind": "derived",
                "state": "derived",
                "input_kind": None,
                "provenance": "synthesized",
                "corpus_citation_path": "us/statutes/7/2014/e",
            },
            "only provision_backed may carry corpus_citation_path",
        ),
    ],
)
def test_compiled_node_metadata_rejects_impossible_combinations(
    fields: dict[str, Any],
    message: str,
) -> None:
    with pytest.raises(ValidationError, match=message):
        CompiledNodeMetadata.model_validate(
            {
                "id": "node",
                "name": "node",
                "reachable": True,
                **fields,
            }
        )


@pytest.mark.parametrize(
    "fields, message",
    [
        (
            {"input_states": {"observed": "exogenous"}},
            "input_states requires typed outputs",
        ),
        (
            {"relation_states": {"members": "pending"}},
            "relation_states requires typed outputs",
        ),
        (
            {"outputs": []},
            "compiled node annotations require a declared output",
        ),
    ],
)
def test_program_rejects_incomplete_annotation_roots(
    fields: dict[str, Any],
    message: str,
) -> None:
    with pytest.raises(ValidationError, match=message):
        Program.model_validate(fields)


@pytest.mark.parametrize(
    "provenance, corpus_citation_path, message",
    [
        (
            "provision_backed",
            None,
            "provision_backed requires corpus_citation_path",
        ),
        (
            "unverified",
            "us/statutes/7/2014/e",
            "only provision_backed may carry corpus_citation_path",
        ),
        (
            "provision_backed",
            "not a canonical path",
            "non-canonical corpus_citation_path",
        ),
    ],
)
def test_authoring_provenance_rejects_ungrounded_citation_combinations(
    provenance: str,
    corpus_citation_path: str | None,
    message: str,
) -> None:
    with pytest.raises(ValidationError, match=message):
        NodeProvenanceEntry.model_validate(
            {
                "kind": "derived",
                "name": "benefit",
                "provenance": provenance,
                "corpus_citation_path": corpus_citation_path,
            }
        )


ANNOTATED_MODULE = """\
format: rulespec/v1
module:
  source_verification:
    corpus_citation_path: us/statutes/tests/node-metadata
outputs:
  - result
input_states:
  observed: exogenous
  upstream_result: policy_derived
  future_intermediate: pending
rules:
  - name: result
    kind: derived
    entity: Household
    dtype: Number
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: observed + upstream_result + future_intermediate
"""

LEGACY_MODULE = """\
format: rulespec/v1
rules:
  - name: legacy_result
    kind: derived
    entity: Household
    dtype: Number
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: observed
"""


@pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)
def test_dense_wrapper_converts_native_catalog_and_preserves_legacy_absence(
    tmp_path,
) -> None:
    root = (tmp_path / "rulespec-us").resolve()
    annotated_path = root / "us/policies/tests/node_metadata.yaml"
    legacy_path = root / "us/policies/tests/legacy_node_metadata.yaml"
    annotated_path.parent.mkdir(parents=True)
    annotated_path.write_text(ANNOTATED_MODULE, encoding="utf-8")
    legacy_path.write_text(LEGACY_MODULE, encoding="utf-8")

    annotated = CompiledDenseProgram.from_file(
        annotated_path,
        rulespec_roots=[root],
        entity="Household",
    )
    legacy = CompiledDenseProgram.from_file(
        legacy_path,
        rulespec_roots=[root],
        entity="Household",
    )

    assert legacy.node_metadata is None
    assert annotated.node_metadata is not None
    assert all(isinstance(node, NodeMetadata) for node in annotated.node_metadata)
    by_name = {node.name: node for node in annotated.node_metadata}
    assert by_name["observed"] == NodeMetadata(
        id="us:policies/tests/node_metadata#input.observed",
        name="observed",
        kind="input",
        state="input",
        input_kind="exogenous",
        reachable=True,
        provenance="unverified",
        corpus_citation_path=None,
    )
    assert by_name["upstream_result"].input_kind == "policy_derived"
    assert by_name["future_intermediate"].state == "pending"
    assert by_name["future_intermediate"].input_kind is None
    assert by_name["result"].kind == "derived"
    assert by_name["result"].provenance == "provision_backed"
    assert (
        by_name["result"].corpus_citation_path
        == "us/statutes/tests/node-metadata"
    )
    assert by_name["result"].reachable is True
