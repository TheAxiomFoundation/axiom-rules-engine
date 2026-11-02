"""Every dump of a compiled artifact is one the engine loads, and the model
accepts every artifact the engine loads.

The engine's loader refuses an artifact that lacks a key its schema requires,
so the Python model must require that key too. A key the model defaults is
left out of an ``exclude_defaults`` dump while it holds its default, and out
of an ``exclude_unset`` dump when it was never set. In the other direction, a
key the schema leaves optional is one the engine reads when it is absent, so
the model must not require it either. The schema tests walk each artifact
model against the schema the engine publishes for its format
(``schemas/compiled-artifact.v<N>.schema.json``: ``cargo test --features
schema`` keeps format 3's equal to the Rust types, closed to what the format-3
loader requires; format 2's is archived as the engine wrote it, since the
format-2 loader still reads that format). They see only which keys a field's
type and default require, not a requirement a validator adds. The end-to-end
tests run the engine binary the repository builds
(``target/debug/axiom-rules-engine``). They skip without it, except in CI,
which builds it.
"""
from __future__ import annotations

import copy
import itertools
import json
import os
import typing
from pathlib import Path

import pytest
from pydantic import BaseModel, ValidationError, create_model

from axiom_rules_engine import (
    AxiomRulesEngine,
    CompiledExecutionRequest,
    CompiledProgram,
    CompiledProgramMetadata,
    Dataset,
    ExecutionResponse,
    FastPathMetadata,
    LegacyCompiledProgramMetadataV2,
    LegacyCompiledProgramV2,
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


def _conjoin(node: dict, defs: dict) -> dict:
    """``node`` with its ``allOf`` parts folded in: their properties and
    required keys apply to the node itself, so a part stays under the
    discriminator of the union branch that holds it."""
    node = _resolve(node, defs)
    parts = [_conjoin(part, defs) for part in node.get("allOf", [])]
    if not parts:
        return node
    merged = {key: value for key, value in node.items() if key != "allOf"}
    for key in ("properties", "required", "anyOf", "oneOf"):
        empty = {} if key == "properties" else []
        combined = [node.get(key, empty), *(part.get(key, empty) for part in parts)]
        if key == "properties":
            folded = {name: value for each in combined for name, value in each.items()}
        else:
            folded = [item for each in combined for item in each]
        if folded:
            merged[key] = folded
    for key in ("items", "additionalProperties"):
        if key not in merged:
            merged.update({key: part[key] for part in parts if key in part})
    return merged


def _object_schemas(node: dict, defs: dict, *, branch: bool = False) -> list[dict]:
    """The object schemas a value may take, through references, ``allOf``
    parts, unions, array items and map values. A union branch with no
    properties is a conditional requirement with no discriminator to select
    it, so it does not count."""
    node = _conjoin(node, defs)
    found = [node] if "properties" in node or (not branch and "required" in node) else []
    for key in ("anyOf", "oneOf"):
        for alternative in node.get(key, []):
            found += _object_schemas(alternative, defs, branch=True)
    for key in ("items", "additionalProperties"):
        if isinstance(node.get(key), dict):
            found += _object_schemas(node[key], defs)
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


def _keys_where(
    differs: typing.Callable[[bool, bool], bool],
    model: type[BaseModel],
    schema: dict,
    defs: dict,
    path: str,
) -> list[str]:
    """Every key, at any depth, for which ``differs(schema requires it,
    model requires it)``."""
    required = set(schema.get("required", []))
    properties = schema.get("properties", {})
    found = []
    for name, field in model.model_fields.items():
        key = field.alias or name
        if differs(key in required, field.is_required()):
            found.append(f"{path}.{key}")
        if key not in properties:
            continue
        for nested in _nested_models(field.annotation):
            for candidate in _object_schemas(properties[key], defs):
                if _may_describe(nested, candidate, defs):
                    found += _keys_where(differs, nested, candidate, defs, f"{path}.{key}")
    return found


def _defaulted_but_required(
    model: type[BaseModel], schema: dict, defs: dict, path: str
) -> list[str]:
    """Every key ``schema`` requires that ``model`` defaults, at any depth."""
    return _keys_where(
        lambda schema_requires, model_requires: schema_requires and not model_requires,
        model,
        schema,
        defs,
        path,
    )


def _required_but_optional(
    model: type[BaseModel], schema: dict, defs: dict, path: str
) -> list[str]:
    """Every key ``model`` requires that ``schema`` leaves optional, at any
    depth."""
    return _keys_where(
        lambda schema_requires, model_requires: model_requires and not schema_requires,
        model,
        schema,
        defs,
        path,
    )


def _mismatches(model: type[BaseModel], walk) -> list[str]:
    """``walk`` over ``model`` and the schema the engine publishes for the
    model's artifact format."""
    (version,) = typing.get_args(model.model_fields["artifact_format_version"].annotation)
    schema_path = SCHEMAS / f"compiled-artifact.v{version}.schema.json"
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    defs = schema.get("$defs") or schema.get("definitions") or {}
    roots = [root for root in _object_schemas(schema, defs) if _may_describe(model, root, defs)]
    assert roots, f"{schema_path.name} has no object schema for {model.__name__}"
    return [key for root in roots for key in walk(model, root, defs, model.__name__)]


@pytest.mark.parametrize("model", ARTIFACT_MODELS, ids=lambda model: model.__name__)
def test_the_model_requires_every_key_the_engine_requires(model) -> None:
    assert _mismatches(model, _defaulted_but_required) == []


@pytest.mark.parametrize("model", ARTIFACT_MODELS, ids=lambda model: model.__name__)
def test_the_model_requires_only_keys_the_engine_requires(model) -> None:
    assert _mismatches(model, _required_but_optional) == []


def test_the_reverse_walk_finds_a_format_2_model_that_requires_the_catalog() -> None:
    # The format-2 model as it was, sharing format 3's metadata.
    required_catalog = create_model(
        "RequiredCatalogV2",
        __base__=LegacyCompiledProgramV2,
        metadata=(CompiledProgramMetadata, ...),
    )
    assert _mismatches(required_catalog, _required_but_optional) == [
        "RequiredCatalogV2.metadata.input_catalog"
    ]


class _Leaf(BaseModel):
    key: str = "default"


class _Tagged(BaseModel):
    kind: typing.Literal["tagged"]
    key: str = "default"


_LEAF_REF = {"$ref": "#/$defs/Leaf"}
_LEAF_DEFS = {"Leaf": {"properties": {"key": {"type": "string"}}, "required": ["key"]}}
_KEY = {"key": {"type": "string"}}


@pytest.mark.parametrize(
    ("annotation", "child", "found"),
    [
        (_Leaf, _LEAF_REF, True),
        (_Leaf | None, {"anyOf": [_LEAF_REF, {"type": "null"}]}, True),
        (list[_Leaf], {"type": "array", "items": _LEAF_REF}, True),
        (dict[str, _Leaf], {"type": "object", "additionalProperties": _LEAF_REF}, True),
        (_Leaf, {"properties": _KEY, "allOf": [{"required": ["key"]}]}, True),
        (_Leaf, {"allOf": [_LEAF_REF]}, True),
        (_Leaf, {"properties": _KEY, "oneOf": [{"required": ["key"]}, {}]}, False),
        (_Leaf, {"properties": _KEY, "oneOf": [{"allOf": [{"required": ["key"]}]}, {}]}, False),
        (
            _Tagged,
            {
                "oneOf": [
                    {"properties": {"kind": {"const": "other"}, **_KEY}, "required": ["kind", "key"]},
                    {"properties": {"kind": {"const": "tagged"}, **_KEY}, "required": ["kind"]},
                ]
            },
            False,
        ),
        (
            _Tagged,
            {
                "oneOf": [
                    {"properties": {"kind": {"const": "other"}, **_KEY}, "required": ["kind"]},
                    {"properties": {"kind": {"const": "tagged"}, **_KEY}, "required": ["kind", "key"]},
                ]
            },
            True,
        ),
        (
            _Tagged,
            {
                "oneOf": [
                    {
                        "properties": {"kind": {"const": "other"}, **_KEY},
                        "allOf": [{"required": ["kind", "key"]}],
                    },
                    {"properties": {"kind": {"const": "tagged"}, **_KEY}, "required": ["kind"]},
                ]
            },
            False,
        ),
    ],
    ids=[
        "ref",
        "optional",
        "array-items",
        "map-values",
        "required-only-allOf-part",
        "allOf-of-a-ref",
        "conditional-union-branch",
        "conditional-union-branch-of-allOf",
        "other-branch-of-tagged-union",
        "own-branch-of-tagged-union",
        "allOf-part-in-other-branch-of-tagged-union",
    ],
)
def test_the_schema_walk_reaches_a_defaulted_required_key_in_each_shape(
    annotation, child: dict, found: bool
) -> None:
    holder = create_model("Holder", child=(annotation, ...))
    schema = {"properties": {"child": child}, "required": ["child"]}
    assert _defaulted_but_required(holder, schema, _LEAF_DEFS, "Holder") == (
        ["Holder.child.key"] if found else []
    )


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


@needs_binary
@pytest.mark.parametrize(
    ("catalog", "refusal"),
    [
        ("absent", "format 3 requires `metadata.input_catalog`"),
        (None, "invalid type: null, expected a sequence"),
    ],
    ids=["absent", "null"],
)
def test_the_engine_and_the_model_both_refuse_a_format_3_artifact_without_a_catalog(
    compiled_artifact, tmp_path: Path, catalog, refusal: str
) -> None:
    written, compiled = compiled_artifact
    artifact = json.loads(written.read_text(encoding="utf-8"))
    if catalog == "absent":
        del artifact["metadata"]["input_catalog"]
    else:
        artifact["metadata"]["input_catalog"] = catalog
    stripped = tmp_path / "without-catalog.json"
    stripped.write_text(json.dumps(artifact), encoding="utf-8")
    with pytest.raises(RuntimeError, match=refusal):
        AxiomRulesEngine(binary_path=BINARY).execute_compiled(
            artifact_path=stripped, request=_request(compiled)
        )
    with pytest.raises(ValidationError, match="input_catalog"):
        CompiledProgram.model_validate(artifact)


# Format 2. Engines that predate the catalog wrote no `input_catalog`, and the
# engine's format-2 loader fills an absent one from the program. It refuses a
# null one, and checks a present one, `[]` included, against the program. So
# the format-2 model accepts an absent catalog, and every dump keeps it absent:
# a default of `[]` would turn an artifact the engine runs into one it refuses.

FORMAT_2 = REPO / "tests" / "fixtures" / "artifacts" / "format2"
# Real format-2 compiler output, and payloads built from it in the older
# format-2 shapes the directory's README describes, two of them without a
# catalog.
FORMAT_2_ARTIFACTS = sorted(
    [
        *(FORMAT_2 / "compiled").glob("*.json"),
        *(FORMAT_2 / "legacy-reload" / "inputs").glob("*.json"),
    ]
)
FLAT_TAX = (
    FORMAT_2 / "compiled" / "tests__fixtures__rulespec__other__flat_tax__rules.compiled.json"
)
DUMPS = {
    "default": {},
    "exclude_defaults": {"exclude_defaults": True},
    "exclude_unset": {"exclude_unset": True},
    "exclude_none": {"exclude_none": True},
}
ABSENT = object()


def _with_catalog(artifact: dict, catalog: typing.Any) -> dict:
    """``artifact`` with ``catalog`` as its catalog, or without one."""
    changed = copy.deepcopy(artifact)
    changed["metadata"].pop("input_catalog", None)
    if catalog is not ABSENT:
        changed["metadata"]["input_catalog"] = catalog
    return changed


def _catalogs(artifact: dict) -> dict[str, typing.Any]:
    """The catalog as written, when there is one, none, and an empty one."""
    catalogs = {"absent": ABSENT, "empty": []}
    if "input_catalog" in artifact["metadata"]:
        catalogs["as-written"] = artifact["metadata"]["input_catalog"]
    return catalogs


def _dump(model: BaseModel, mode: str, options: dict) -> str:
    if mode == "json":
        return model.model_dump_json(**options)
    return json.dumps(model.model_dump(**options))


def test_the_format_2_artifacts_include_ones_without_a_catalog() -> None:
    without = [
        path.name
        for path in FORMAT_2_ARTIFACTS
        if "input_catalog" not in json.loads(path.read_text(encoding="utf-8"))["metadata"]
    ]
    assert FLAT_TAX in FORMAT_2_ARTIFACTS
    assert "dialect-absent-catalog.json" in without
    assert "dialect-v0.1-maintenance.json" in without


@pytest.mark.parametrize("path", FORMAT_2_ARTIFACTS, ids=lambda path: path.name)
def test_every_format_2_dump_writes_the_catalog_it_read(path: Path) -> None:
    # Every combination of the dump options, in both modes: the catalog is
    # written exactly when one was read, as it was read, never as null.
    artifact = json.loads(path.read_text(encoding="utf-8"))
    flags = ["exclude_defaults", "exclude_unset", "exclude_none"]
    for catalog in _catalogs(artifact).values():
        legacy = LegacyCompiledProgramV2.model_validate(_with_catalog(artifact, catalog))
        for chosen in itertools.product([False, True], repeat=len(flags)):
            for mode in ["python", "json"]:
                written = json.loads(_dump(legacy, mode, dict(zip(flags, chosen))))
                if catalog is ABSENT:
                    assert "input_catalog" not in written["metadata"]
                else:
                    assert written["metadata"]["input_catalog"] == catalog
                assert LegacyCompiledProgramV2.model_validate(written).metadata == legacy.metadata


def test_the_format_2_model_refuses_a_null_catalog() -> None:
    artifact = _with_catalog(json.loads(FLAT_TAX.read_text(encoding="utf-8")), None)
    with pytest.raises(ValidationError, match="never null"):
        LegacyCompiledProgramV2.model_validate(artifact)
    with pytest.raises(ValidationError, match="never null"):
        LegacyCompiledProgramV2.model_validate_json(json.dumps(artifact))


def test_format_3_metadata_still_requires_the_catalog() -> None:
    metadata = {
        "evaluation_order": [],
        "fast_path": {"strategy": "generic_bulk", "compatible": True, "blockers": []},
    }
    with pytest.raises(ValidationError, match="input_catalog"):
        CompiledProgramMetadata.model_validate(metadata)
    # Format-2 metadata is a different class, not a subclass, so an instance
    # without a catalog cannot pass for format 3's unvalidated.
    with pytest.raises(ValidationError):
        CompiledProgramMetadata.model_validate(LegacyCompiledProgramMetadataV2.model_validate(metadata))


def test_the_format_2_metadata_serialization_schema_is_the_models_own() -> None:
    serialization = LegacyCompiledProgramMetadataV2.model_json_schema(mode="serialization")
    validation = LegacyCompiledProgramMetadataV2.model_json_schema(mode="validation")
    assert serialization["properties"].keys() == validation["properties"].keys()
    assert "input_catalog" in serialization["properties"]


def _flat_tax_request() -> CompiledExecutionRequest:
    return CompiledExecutionRequest.model_validate(
        {
            "mode": "explain",
            "dataset": {
                "inputs": [
                    {
                        "name": "income",
                        "entity": "Person",
                        "entity_id": "p1",
                        "interval": {"start": "2025-01-01", "end": "2025-12-31"},
                        "value": {"kind": "decimal", "value": "5000"},
                    }
                ]
            },
            "queries": [
                {
                    "entity_id": "p1",
                    "period": {"period_kind": "tax_year", "start": "2025-01-01", "end": "2025-12-31"},
                    "outputs": ["income_tax", "net_income", "high_income"],
                }
            ],
        }
    )


@needs_binary
@pytest.mark.parametrize("mode", ["python", "json"])
@pytest.mark.parametrize("dump", DUMPS.values(), ids=DUMPS.keys())
def test_the_engine_runs_every_dump_of_a_format_2_artifact_without_a_catalog(
    tmp_path: Path, dump: dict, mode: str
) -> None:
    engine = AxiomRulesEngine(binary_path=BINARY)
    request = _flat_tax_request()
    expected = engine.execute_compiled(
        artifact_path=FLAT_TAX, request=request, accept_artifact_format=2
    )
    (result,) = expected.results
    assert result.outputs["income_tax"].value.value == "800"
    assert result.outputs["high_income"].outcome == "holds"
    artifact = json.loads(FLAT_TAX.read_text(encoding="utf-8"))
    del artifact["metadata"]["input_catalog"]
    legacy = LegacyCompiledProgramV2.model_validate(artifact)
    assert legacy.metadata.input_catalog is None
    dumped = tmp_path / "dumped.json"
    dumped.write_text(_dump(legacy, mode, dump), encoding="utf-8")
    assert "input_catalog" not in json.loads(dumped.read_text(encoding="utf-8"))["metadata"]
    assert (
        engine.execute_compiled(artifact_path=dumped, request=request, accept_artifact_format=2)
        == expected
    )


def _outcome(engine: AxiomRulesEngine, path: Path) -> ExecutionResponse | str:
    """What the format-2 loader makes of ``path``: the response to an empty
    request, or the refusal with the path taken out."""
    empty = CompiledExecutionRequest(mode="explain", dataset=Dataset(), queries=[])
    try:
        return engine.execute_compiled(artifact_path=path, request=empty, accept_artifact_format=2)
    except RuntimeError as error:
        return str(error).replace(str(path), "<artifact>")


@needs_binary
@pytest.mark.parametrize("path", FORMAT_2_ARTIFACTS, ids=lambda path: path.name)
def test_the_engine_reads_every_format_2_dump_as_it_reads_the_artifact(
    path: Path, tmp_path: Path
) -> None:
    # For each catalog, every dump loads exactly when the artifact does: an
    # absent catalog and the one written load, and an empty one over a
    # program with inputs is refused both ways.
    engine = AxiomRulesEngine(binary_path=BINARY)
    artifact = json.loads(path.read_text(encoding="utf-8"))
    for name, catalog in _catalogs(artifact).items():
        source = tmp_path / f"{name}.json"
        changed = _with_catalog(artifact, catalog)
        source.write_text(json.dumps(changed), encoding="utf-8")
        expected = _outcome(engine, source)
        if name != "empty":
            assert not isinstance(expected, str), expected
        legacy = LegacyCompiledProgramV2.model_validate(changed)
        for dump, options in DUMPS.items():
            for mode in ["python", "json"]:
                dumped = tmp_path / f"{name}-{dump}-{mode}.json"
                dumped.write_text(_dump(legacy, mode, options), encoding="utf-8")
                assert _outcome(engine, dumped) == expected, (name, dump, mode)
