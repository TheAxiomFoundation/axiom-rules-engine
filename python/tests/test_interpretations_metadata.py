"""The wrapper retains fictional Lantern metadata in its opaque Program model."""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from axiom_rules_engine.loader import load_composed_program, load_program
from axiom_rules_engine.models import CompiledProgram, Dataset, ExecutionRequest


def _artifact() -> dict:
    return {
        "artifact_format_version": 3,
        "engine_version": "0.2.2",
        "engine_build": {"commit": None, "dirty": None},
        "program": {
            "module": {
                "interpretations": [
                    {
                        "id": "weekly-percentage",
                        "question": "Does the percentage apply once a week?",
                        "texts": [
                            {
                                "corpus_citation_path": "zz-fictional-lantern/manual/example-scheme/page-7",
                                "excerpt": "The prescribed daily percentage",
                            }
                        ],
                        "reading": "Apply the percentage once a week.",
                        "alternatives": [
                            {
                                "id": "daily-percentage",
                                "reading": "Apply the percentage each day.",
                            }
                        ],
                        "basis": "The weekly table supplies the taper.",
                        "status": "open",
                        "affects": ["weekly_taper"],
                    }
                ]
            },
            "units": [],
            "relations": [],
            "parameters": [],
            "derived": [],
            "interface": {"entities": [], "inputs": [], "relations": []},
        },
        "metadata": {
            "evaluation_order": [],
            "fast_path": {"strategy": "none", "compatible": True, "blockers": []},
            "input_catalog": [],
        },
    }


@pytest.mark.parametrize("loader", [load_program, load_composed_program])
def test_loader_retains_interpretations_in_execution_requests(
    monkeypatch: pytest.MonkeyPatch, loader
) -> None:
    artifact = _artifact()

    def run(command, **kwargs):
        output = Path(command[command.index("--output") + 1])
        output.write_text(json.dumps(artifact))
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr("axiom_rules_engine.loader.subprocess.run", run)
    program = loader("module.yaml", rulespec_roots=("fictional-lantern",))
    request = ExecutionRequest(
        mode="explain", program=program, dataset=Dataset(), queries=[]
    )
    payload = json.loads(request.model_dump_json(exclude_none=True))
    assert payload["program"]["module"] == artifact["program"]["module"]
    restored = ExecutionRequest.model_validate(payload)
    assert restored.program.model_dump()["module"] == artifact["program"]["module"]


def test_compiled_program_model_retains_interpretations() -> None:
    artifact = _artifact()
    compiled = CompiledProgram.model_validate(artifact)
    restored = CompiledProgram.model_validate_json(
        compiled.model_dump_json(exclude_unset=True)
    )
    assert restored.program.model_dump()["module"] == artifact["program"]["module"]
