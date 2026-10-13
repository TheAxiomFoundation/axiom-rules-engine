"""Compiled artifact format 3 on the Python surface.

``CompiledProgram`` is format 3, whose identity and interface objects have
exactly their keys; ``LegacyCompiledProgramV2`` is format 2. The client and
the dense loader read a format-2 artifact only when the caller passes
``accept_artifact_format=2``. The end-to-end tests run the engine binary the
repository builds (``target/debug/axiom-rules-engine``) and skip without it;
the dense ones also need the native extension.
"""
from __future__ import annotations

import json
import subprocess
from decimal import Decimal
from pathlib import Path

import pytest
from pydantic import ValidationError

from axiom_rules_engine import (
    AxiomRulesEngine,
    CompiledDenseProgram,
    CompiledExecutionRequest,
    CompiledProgram,
    Dataset,
    ExecutionQuery,
    LegacyCompiledProgramV2,
)
from axiom_rules_engine.dense import NativeCompiledDenseProgram
from axiom_rules_engine.models import Period

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / "target" / "debug" / "axiom-rules-engine"

needs_binary = pytest.mark.skipif(
    not BINARY.exists(), reason="the engine binary is not built (cargo build)"
)
needs_dense = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: Format-3 surface test fixture.
rules:
  - name: format_test_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          0.25
  - name: format_test_tax
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          format_test_income * format_test_rate
"""

TARGET = "us:policies/tests/format-three"


def _interface_input(**overrides):
    return {
        "name": "x",
        "legal_id": "x",
        "entities": ["Person"],
        "dtype": "decimal",
        "dtype_basis": "usage",
        "default": None,
        "values": None,
        "values_basis": None,
        **overrides,
    }


def _format_3(**interface):
    return {
        "artifact_format_version": 3,
        "engine_version": "0.2.2",
        "engine_build": {"commit": None, "dirty": None},
        "program": {
            "units": [],
            "relations": [],
            "parameters": [],
            "derived": [],
            "interface": {"entities": [], "inputs": [], "relations": [], **interface},
        },
        "metadata": {
            "evaluation_order": [],
            "fast_path": {"strategy": "generic_bulk", "compatible": True},
            "input_catalog": [],
        },
    }


def test_format_3_interface_objects_have_exactly_their_keys() -> None:
    CompiledProgram.model_validate(_format_3(inputs=[_interface_input()]))
    entity = {"kind": "Person", "id_template": "Person:{index}", "instances": {"source": "supplied"}}
    CompiledProgram.model_validate(_format_3(entities=[entity]))
    relation = {
        "name": "member_of",
        "arity": 2,
        "slot_entities": None,
        "lookup_slots": [],
        "accepts_supplied_tuples": True,
        "provenance": {"kind": "supplied"},
    }
    CompiledProgram.model_validate(_format_3(relations=[relation]))
    # Every nullable key must be written, as null when it has no value.
    for nullable in ["dtype", "dtype_basis", "default", "values", "values_basis"]:
        missing = _interface_input()
        del missing[nullable]
        with pytest.raises(ValidationError):
            CompiledProgram.model_validate(_format_3(inputs=[missing]))
    for field, interface in [
        ("entities", {key: value for key, value in entity.items() if key != "id_template"}),
        ("relations", {key: value for key, value in relation.items() if key != "slot_entities"}),
    ]:
        with pytest.raises(ValidationError):
            CompiledProgram.model_validate(_format_3(**{field: [interface]}))
    with pytest.raises(ValidationError):
        CompiledProgram.model_validate(_format_3(inputs=[_interface_input(unexpected=1)]))
    with pytest.raises(ValidationError):
        CompiledProgram.model_validate(
            _format_3(entities=[{**entity, "id_template": None, "instances": {"source": "alias", "via": []}}])
        )
    for build in [{"commit": None}, {"commit": None, "dirty": None, "host": "ci"}]:
        with pytest.raises(ValidationError):
            CompiledProgram.model_validate({**_format_3(), "engine_build": build})
    without_interface = _format_3()
    del without_interface["program"]["interface"]
    with pytest.raises(ValidationError):
        CompiledProgram.model_validate(without_interface)
    # The top-level keys are closed, and a source closure is omitted rather
    # than null.
    for extra in [{"unexpected": True}, {"source_closure": None}]:
        with pytest.raises(ValidationError):
            CompiledProgram.model_validate({**_format_3(), **extra})


def _request() -> CompiledExecutionRequest:
    return CompiledExecutionRequest(
        mode="explain",
        dataset=Dataset(),
        queries=[
            ExecutionQuery(
                entity_id="p1",
                period=Period(period_kind="tax_year", start="2025-01-01", end="2025-12-31"),
                outputs=[],
            )
        ],
    )


def test_the_client_passes_the_format_2_opt_in(monkeypatch: pytest.MonkeyPatch) -> None:
    commands = []

    def run(command, **kwargs):
        commands.append(command)
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=json.dumps(
                {
                    "metadata": {
                        "requested_mode": "explain",
                        "actual_mode": "explain",
                        "relation_binding": "strict",
                    },
                    "results": [],
                }
            ),
            stderr="",
        )

    monkeypatch.setattr("axiom_rules_engine.client.subprocess.run", run)
    engine = AxiomRulesEngine()
    engine.execute_compiled(artifact_path="a.json", request=_request())
    engine.execute_compiled(
        artifact_path="a.json", request=_request(), accept_artifact_format=2
    )
    engine.run_compiled(
        mode="explain",
        artifact_path="a.json",
        dataset=Dataset(),
        queries=[],
        accept_artifact_format=2,
    )
    assert "--accept-artifact-format" not in commands[0]
    for command in commands[1:]:
        flag = command.index("--accept-artifact-format")
        assert command[flag + 1] == "2"
    for wrong in [3, 1]:
        with pytest.raises(ValueError, match="accepts only 2"):
            engine.execute_compiled(
                artifact_path="a.json", request=_request(), accept_artifact_format=wrong
            )
    assert len(commands) == 3


def _as_format_2(artifact: dict) -> dict:
    """The format-2 artifact of the same program: format 3 without its three
    additions, stamped 2."""
    legacy = {key: value for key, value in artifact.items() if key not in ("engine_build", "source_closure")}
    legacy["artifact_format_version"] = 2
    legacy["program"] = {key: value for key, value in artifact["program"].items() if key != "interface"}
    return legacy


@pytest.fixture()
def compiled_artifacts(tmp_path: Path) -> tuple[Path, Path, CompiledProgram]:
    root = (tmp_path / "rulespec-us").resolve()
    module = root / "us/policies/tests/format-three.yaml"
    module.parent.mkdir(parents=True)
    module.write_text(MODULE_SOURCE, encoding="utf-8")
    format_3 = tmp_path / "format-3.json"
    compiled = AxiomRulesEngine(binary_path=BINARY).compile(
        program_path=module, rulespec_roots=(root,), output_path=format_3
    )
    format_2 = tmp_path / "format-2.json"
    format_2.write_text(json.dumps(_as_format_2(json.loads(format_3.read_text()))))
    return format_3, format_2, compiled


OUTPUT_ID = f"{TARGET}#format_test_tax"


def _income_request(input_name: str) -> CompiledExecutionRequest:
    return CompiledExecutionRequest.model_validate(
        {
            "mode": "explain",
            "dataset": {
                "inputs": [
                    {
                        "name": input_name,
                        "entity": "Person",
                        "entity_id": "p1",
                        "interval": {"start": "2025-01-01", "end": "2025-12-31"},
                        "value": {"kind": "decimal", "value": "1000"},
                    }
                ]
            },
            "queries": [
                {
                    "entity_id": "p1",
                    "period": {"period_kind": "tax_year", "start": "2025-01-01", "end": "2025-12-31"},
                    "outputs": [OUTPUT_ID],
                }
            ],
        }
    )


@needs_binary
def test_compile_output_is_a_format_3_compiled_program(compiled_artifacts) -> None:
    format_3, format_2, compiled = compiled_artifacts
    assert compiled.artifact_format_version == 3
    assert compiled.source_closure is not None
    assert compiled.source_closure.root == TARGET
    assert [entry.name for entry in compiled.program.interface.inputs] == ["format_test_income"]
    LegacyCompiledProgramV2.model_validate_json(format_2.read_text())
    with pytest.raises(ValidationError):
        CompiledProgram.model_validate_json(format_2.read_text())


@needs_binary
def test_the_client_runs_format_2_only_on_request(compiled_artifacts) -> None:
    format_3, format_2, compiled = compiled_artifacts
    # The interface names the input's request name.
    (income,) = compiled.program.interface.inputs
    request = _income_request(income.legal_id)
    engine = AxiomRulesEngine(binary_path=BINARY)
    current = engine.execute_compiled(artifact_path=format_3, request=request)
    with pytest.raises(RuntimeError, match="is older than this engine"):
        engine.execute_compiled(artifact_path=format_2, request=request)
    legacy = engine.execute_compiled(
        artifact_path=format_2, request=request, accept_artifact_format=2
    )
    assert legacy == current
    assert Decimal(current.results[0].outputs[OUTPUT_ID].value.value) == 250


@needs_binary
@needs_dense
def test_the_dense_loader_reads_format_2_only_on_request(compiled_artifacts) -> None:
    format_3, format_2, _ = compiled_artifacts
    current = CompiledDenseProgram.from_artifact_file(format_3, entity="Person")
    assert current.artifact_format_version == 3
    with pytest.raises(ValueError, match="is older than this engine"):
        CompiledDenseProgram.from_artifact_file(format_2, entity="Person")
    with pytest.warns(UserWarning, match="format-2 compatibility loader"):
        legacy = CompiledDenseProgram.from_artifact_file(
            format_2, entity="Person", accept_artifact_format=2
        )
    assert legacy.artifact_format_version == 2
    assert legacy.output_names == current.output_names
    assert legacy.root_inputs == current.root_inputs
    with pytest.raises(ValueError, match="accepts only 2"):
        CompiledDenseProgram.from_artifact_file(format_3, accept_artifact_format=3)
