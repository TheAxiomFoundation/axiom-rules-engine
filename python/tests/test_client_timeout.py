"""The engine wrapper must bound subprocess time so a hung engine cannot
hang the caller (web apps, microsim batches)."""
from __future__ import annotations

import stat
import subprocess
from pathlib import Path

import pytest
from pydantic import ValidationError

from axiom_rules_engine.client import DEFAULT_TIMEOUT_SECONDS, AxiomRulesEngine
from axiom_rules_engine.models import (
    CompiledProgram,
    Dataset,
    LegacyCompiledProgramV2,
    ExecutionQuery,
    ExecutionRequest,
    Period,
    Program,
)


def _hanging_binary(tmp_path: Path) -> Path:
    binary = tmp_path / "hanging-engine"
    binary.write_text("#!/bin/sh\nsleep 60\n")
    binary.chmod(binary.stat().st_mode | stat.S_IEXEC)
    return binary


def _minimal_request() -> ExecutionRequest:
    period = Period(period_kind="Month", start="2026-01-01", end="2026-01-31")
    return ExecutionRequest(
        mode="explain",
        program=Program(),
        dataset=Dataset(),
        queries=[ExecutionQuery(entity_id="h1", period=period, outputs=[])],
    )


def test_execute_times_out_instead_of_hanging(tmp_path: Path) -> None:
    engine = AxiomRulesEngine(binary_path=_hanging_binary(tmp_path), timeout=0.5)
    with pytest.raises(subprocess.TimeoutExpired):
        engine.execute(_minimal_request())


def test_compile_times_out_instead_of_hanging(tmp_path: Path) -> None:
    engine = AxiomRulesEngine(binary_path=_hanging_binary(tmp_path), timeout=0.5)
    with pytest.raises(subprocess.TimeoutExpired):
        engine.compile(
            program_path=tmp_path / "rules.yaml",
            rulespec_roots=(tmp_path / "rulespec-us",),
            output_path=tmp_path / "out.json",
        )


def test_compile_composed_times_out_instead_of_hanging(tmp_path: Path) -> None:
    engine = AxiomRulesEngine(binary_path=_hanging_binary(tmp_path), timeout=0.5)
    with pytest.raises(subprocess.TimeoutExpired):
        engine.compile_composed(
            program_path=tmp_path / "composition.yaml",
            rulespec_roots=(tmp_path / "rulespec-us",),
            output_path=tmp_path / "out.json",
        )


def test_timeout_defaults_on_and_is_configurable() -> None:
    assert AxiomRulesEngine().timeout == DEFAULT_TIMEOUT_SECONDS
    assert AxiomRulesEngine(timeout=None).timeout is None


def test_compiled_program_rejects_missing_and_wrong_artifact_versions() -> None:
    legacy = {
        "program": {"units": [], "relations": [], "parameters": [], "derived": []},
        "metadata": {
            "evaluation_order": [],
            "fast_path": {"strategy": "generic_bulk", "compatible": True, "blockers": []},
            "input_catalog": [],
        },
    }
    format_3 = {
        **legacy,
        "artifact_format_version": 3,
        "engine_version": "0.2.2",
        "engine_build": {"commit": None, "dirty": None},
        "program": {
            **legacy["program"],
            "interface": {"entities": [], "inputs": [], "relations": []},
        },
    }
    # ``CompiledProgram`` is format 3; the format-2 shape is
    # ``LegacyCompiledProgramV2``.
    for model, valid in [(CompiledProgram, format_3), (LegacyCompiledProgramV2, legacy)]:
        with pytest.raises(ValidationError):
            model.model_validate(
                {key: value for key, value in valid.items() if key != "artifact_format_version"}
            )
    for wrong_version in [0, 1, 2, 4]:
        with pytest.raises(ValidationError):
            CompiledProgram.model_validate(
                {**format_3, "artifact_format_version": wrong_version}
            )
    for wrong_version in [0, 1, 3]:
        with pytest.raises(ValidationError):
            LegacyCompiledProgramV2.model_validate(
                {**legacy, "artifact_format_version": wrong_version}
            )

    compiled = CompiledProgram.model_validate(format_3)
    assert compiled.artifact_format_version == 3
    legacy_compiled = LegacyCompiledProgramV2.model_validate(
        {**legacy, "artifact_format_version": 2}
    )
    assert legacy_compiled.artifact_format_version == 2
