from __future__ import annotations

from typing import Any

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
