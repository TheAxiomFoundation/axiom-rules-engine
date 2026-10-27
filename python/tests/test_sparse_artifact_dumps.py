"""Every dump of a compiled artifact is one the engine loads.

The engine's loader refuses an artifact that lacks a key its schema requires,
so the Python model must require that key too. A key the model defaults is
left out of an ``exclude_defaults`` dump while it holds its default, and out
of an ``exclude_unset`` dump when it was never set. The schema test walks
each artifact model against the schema the engine publishes for its format
(``schemas/compiled-artifact.v<N>.schema.json``, which ``cargo test --features
schema`` keeps equal to the Rust types). The end-to-end tests run the engine
binary the repository builds (``target/debug/axiom-rules-engine``). They skip
without it, except in CI, which builds it.
"""
from __future__ import annotations

import json
import os
import typing
from pathlib import Path

import pytest
from pydantic import BaseModel, ValidationError

from axiom_rules_engine import (
    AxiomRulesEngine,
    CompiledExecutionRequest,
    CompiledProgram,
    FastPathMetadata,
    models,
)

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / "target" / "debug" / "axiom-rules-engine"
SCHEMAS = REPO / "schemas"

needs_binary = pytest.mark.skipif(
    not BINARY.exists() and not os.environ.get("CI"),
    reason="the engine binary is not built (cargo build)",
)

ARTIFACT_MODELS = sorted(
    (
        model
        for model in vars(models).values()
        if isinstance(model, type)
        and issubclass(model, BaseModel)
        and "artifact_format_version" in model.model_fields
    ),
    key=lambda model: model.__name__,
)


def _resolve(node: dict, defs: dict) -> dict:
    while "$ref" in node:
        rest = {key: value for key, value in node.items() if key != "$ref"}
        node = {**defs[node["$ref"].rsplit("/", 1)[-1]], **rest}
    return node


def _object_schemas(node: dict, defs: dict) -> list[dict]:
    """The object schemas a value may take, through references, unions,
    ``allOf`` parts and array items."""
    node = _resolve(node, defs)
    found = [node] if "properties" in node else []
    for key in ("anyOf", "oneOf", "allOf"):
        for branch in node.get(key, []):
            found += _object_schemas(branch, defs)
    if isinstance(node.get("items"), dict):
        found += _object_schemas(node["items"], defs)
    return found


def _nested_models(annotation: typing.Any) -> list[type[BaseModel]]:
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        return [annotation]
    return [model for arg in typing.get_args(annotation) for model in _nested_models(arg)]


def _may_describe(model: type[BaseModel], schema: dict, defs: dict) -> bool:
    """False when one of the model's literal fields contradicts the schema's
    constant for that key, as for the other branches of a tagged union."""
    for name, field in model.model_fields.items():
        if typing.get_origin(field.annotation) is not typing.Literal:
            continue
        allowed = _resolve(schema.get("properties", {}).get(field.alias or name, {}), defs)
        constants = set(allowed.get("enum", [])) | (
            {allowed["const"]} if "const" in allowed else set()
        )
        if constants and not constants & set(typing.get_args(field.annotation)):
            return False
    return True


def _defaulted_but_required(
    model: type[BaseModel], schema: dict, defs: dict, path: str
) -> list[str]:
    """Every key ``schema`` requires that ``model`` defaults, at any depth."""
    required = set(schema.get("required", []))
    properties = schema.get("properties", {})
    found = []
    for name, field in model.model_fields.items():
        key = field.alias or name
        if key in required and not field.is_required():
            found.append(f"{path}.{key}")
        if key not in properties:
            continue
        for nested in _nested_models(field.annotation):
            for candidate in _object_schemas(properties[key], defs):
                if _may_describe(nested, candidate, defs):
                    found += _defaulted_but_required(nested, candidate, defs, f"{path}.{key}")
    return found


@pytest.mark.parametrize("model", ARTIFACT_MODELS, ids=lambda model: model.__name__)
def test_the_model_requires_every_key_the_engine_requires(model) -> None:
    (version,) = typing.get_args(model.model_fields["artifact_format_version"].annotation)
    schema_path = SCHEMAS / f"compiled-artifact.v{version}.schema.json"
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    defs = schema.get("$defs") or schema.get("definitions") or {}
    roots = [root for root in _object_schemas(schema, defs) if _may_describe(model, root, defs)]
    assert roots, f"{schema_path.name} has no object schema for {model.__name__}"
    assert [
        key for root in roots for key in _defaulted_but_required(model, root, defs, model.__name__)
    ] == []


def test_fast_path_metadata_requires_blockers() -> None:
    with pytest.raises(ValidationError, match="blockers"):
        FastPathMetadata.model_validate({"strategy": "generic_bulk", "compatible": True})
    assert FastPathMetadata.model_validate(
        {"strategy": "generic_bulk", "compatible": True, "blockers": []}
    ).blockers == []


MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: Sparse artifact dump test fixture.
rules:
  - name: sparse_dump_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          0.25
  - name: sparse_dump_tax
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          sparse_dump_income * sparse_dump_rate
"""

TARGET = "us:policies/tests/sparse-dumps"
OUTPUT_ID = f"{TARGET}#sparse_dump_tax"


@pytest.fixture()
def compiled_artifact(tmp_path: Path) -> tuple[Path, CompiledProgram]:
    root = (tmp_path / "rulespec-us").resolve()
    module = root / "us/policies/tests/sparse-dumps.yaml"
    module.parent.mkdir(parents=True)
    module.write_text(MODULE_SOURCE, encoding="utf-8")
    written = tmp_path / "compiled.json"
    compiled = AxiomRulesEngine(binary_path=BINARY).compile(
        program_path=module, rulespec_roots=(root,), output_path=written
    )
    return written, compiled


def _request(compiled: CompiledProgram) -> CompiledExecutionRequest:
    (income,) = compiled.metadata.input_catalog
    return CompiledExecutionRequest.model_validate(
        {
            "mode": "explain",
            "dataset": {
                "inputs": [
                    {
                        "name": income.canonical_request_name,
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
@pytest.mark.parametrize(
    "dump",
    [{}, {"exclude_defaults": True}, {"exclude_unset": True}],
    ids=["default", "exclude_defaults", "exclude_unset"],
)
def test_the_engine_runs_every_dump_of_a_compiled_artifact(
    compiled_artifact, tmp_path: Path, dump: dict
) -> None:
    written, compiled = compiled_artifact
    # An empty list is what `exclude_defaults` would drop were it the default.
    assert compiled.metadata.fast_path.blockers == []
    engine = AxiomRulesEngine(binary_path=BINARY)
    request = _request(compiled)
    expected = engine.execute_compiled(artifact_path=written, request=request)
    (result,) = expected.results
    assert result.outputs[OUTPUT_ID].value.value == "250"
    dumped = tmp_path / "dumped.json"
    dumped.write_text(compiled.model_dump_json(**dump), encoding="utf-8")
    assert engine.execute_compiled(artifact_path=dumped, request=request) == expected
    assert json.loads(dumped.read_text(encoding="utf-8"))["metadata"]["fast_path"]["blockers"] == []


@needs_binary
def test_the_engine_and_the_model_both_refuse_an_artifact_without_blockers(
    compiled_artifact, tmp_path: Path
) -> None:
    written, compiled = compiled_artifact
    artifact = json.loads(written.read_text(encoding="utf-8"))
    del artifact["metadata"]["fast_path"]["blockers"]
    stripped = tmp_path / "without-blockers.json"
    stripped.write_text(json.dumps(artifact), encoding="utf-8")
    with pytest.raises(RuntimeError, match="missing field `blockers`"):
        AxiomRulesEngine(binary_path=BINARY).execute_compiled(
            artifact_path=stripped, request=_request(compiled)
        )
    with pytest.raises(ValidationError, match="blockers"):
        CompiledProgram.model_validate(artifact)
