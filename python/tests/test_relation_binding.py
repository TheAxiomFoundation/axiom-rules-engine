"""The JSON client preserves the relation binding policy in both directions."""
from __future__ import annotations

import json
import subprocess

import pytest
from pydantic import ValidationError

from axiom_rules_engine import (
    AxiomRulesEngine,
    CompiledExecutionRequest,
    Dataset,
    ExecutionMetadata,
    ExecutionRequest,
    Program,
)


@pytest.mark.parametrize("compiled", [False, True])
@pytest.mark.parametrize("convenience", [False, True])
@pytest.mark.parametrize("binding", [None, "lenient"])
def test_client_sends_relation_binding_and_retains_response_policy(
    monkeypatch: pytest.MonkeyPatch,
    compiled: bool,
    convenience: bool,
    binding: str | None,
) -> None:
    expected_binding = binding or "strict"
    captured = []

    def run(command, **kwargs):
        captured.append(json.loads(kwargs["input"]))
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=json.dumps(
                {
                    "metadata": {
                        "requested_mode": "explain",
                        "actual_mode": "explain",
                        "relation_binding": expected_binding,
                    },
                    "results": [],
                }
            ),
            stderr="",
        )

    monkeypatch.setattr("axiom_rules_engine.client.subprocess.run", run)
    engine = AxiomRulesEngine()
    values = {"mode": "explain", "dataset": Dataset(), "queries": []}
    if binding is not None:
        values["relation_binding"] = binding
    if compiled:
        if convenience:
            response = engine.run_compiled(artifact_path="artifact.json", **values)
        else:
            response = engine.execute_compiled(
                artifact_path="artifact.json",
                request=CompiledExecutionRequest(**values),
            )
    else:
        values["program"] = Program()
        if convenience:
            response = engine.run(**values)
        else:
            response = engine.execute(ExecutionRequest(**values))

    assert captured[0]["relation_binding"] == expected_binding
    assert response.metadata.relation_binding == expected_binding


def test_response_without_relation_binding_keeps_policy_unknown() -> None:
    metadata = ExecutionMetadata(requested_mode="explain", actual_mode="explain")
    assert metadata.relation_binding is None


@pytest.mark.parametrize("request_type", [ExecutionRequest, CompiledExecutionRequest])
def test_request_rejects_unknown_relation_binding(request_type) -> None:
    values = {
        "mode": "explain",
        "dataset": Dataset(),
        "queries": [],
        "relation_binding": "ignored",
    }
    if request_type is ExecutionRequest:
        values["program"] = Program()
    with pytest.raises(ValidationError, match="relation_binding"):
        request_type(**values)
