from __future__ import annotations

from typing import Any

import pytest

from axiom_rules_engine import CompiledDenseProgram, NodeMetadata
from axiom_rules_engine.dense import NativeCompiledDenseProgram
from axiom_rules_engine.models import CompiledNodeMetadata, CompiledProgram


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
    dumped = compiled.model_dump(mode="json", exclude_none=True)
    assert dumped["metadata"]["nodes"] == nodes
    assert CompiledProgram.model_validate(dumped) == compiled


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
    )
    assert by_name["upstream_result"].input_kind == "policy_derived"
    assert by_name["future_intermediate"].state == "pending"
    assert by_name["future_intermediate"].input_kind is None
    assert by_name["result"].kind == "derived"
    assert by_name["result"].provenance == "provision_backed"
    assert by_name["result"].reachable is True
