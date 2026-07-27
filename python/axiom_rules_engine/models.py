from __future__ import annotations

import re
from datetime import date
from typing import Annotated, Any, Literal, Self

from pydantic import BaseModel, ConfigDict, Field, model_validator


ExecutionMode = Literal["explain", "fast"]
InputState = Literal["exogenous", "policy_derived", "pending"]
NodeKind = Literal[
    "input",
    "parameter",
    "derived",
    "data_relation",
    "derived_relation",
]
NodeProvenance = Literal["provision_backed", "synthesized", "unverified"]
_CORPUS_JURISDICTION = re.compile(r"[a-z]{2,3}(?:-[a-z0-9]+)*")
_CORPUS_DOCUMENT_CLASS = re.compile(r"[a-z][a-z0-9-]*")
_CORPUS_PATH_SEGMENT = re.compile(r"[A-Za-z0-9][A-Za-z0-9 .:–-]*")


def _is_canonical_corpus_citation_path(value: str) -> bool:
    segments = value.split("/")
    return (
        value == value.strip()
        and len(segments) >= 3
        and _CORPUS_JURISDICTION.fullmatch(segments[0]) is not None
        and _CORPUS_DOCUMENT_CLASS.fullmatch(segments[1]) is not None
        and all(
            segment == segment.strip()
            and _CORPUS_PATH_SEGMENT.fullmatch(segment) is not None
            for segment in segments[2:]
        )
    )


def _validate_provenance_citation(
    provenance: NodeProvenance,
    corpus_citation_path: str | None,
) -> None:
    if provenance == "provision_backed" and corpus_citation_path is None:
        raise ValueError("provision_backed requires corpus_citation_path")
    if provenance != "provision_backed" and corpus_citation_path is not None:
        raise ValueError("only provision_backed may carry corpus_citation_path")
    if (
        corpus_citation_path is not None
        and not _is_canonical_corpus_citation_path(corpus_citation_path)
    ):
        raise ValueError(
            f"non-canonical corpus_citation_path {corpus_citation_path!r}"
        )


class NodeProvenanceEntry(BaseModel):
    kind: NodeKind
    name: str
    provenance: NodeProvenance
    corpus_citation_path: str | None = None

    @model_validator(mode="after")
    def validate_citation_grounding(self) -> Self:
        _validate_provenance_citation(
            self.provenance,
            self.corpus_citation_path,
        )
        return self


class Program(BaseModel):
    model_config = ConfigDict(extra="allow")

    units: list[dict[str, Any]] = Field(default_factory=list)
    relations: list[dict[str, Any]] = Field(default_factory=list)
    parameters: list[dict[str, Any]] = Field(default_factory=list)
    derived: list[dict[str, Any]] = Field(default_factory=list)
    outputs: list[str] | None = None
    input_states: dict[str, InputState] = Field(default_factory=dict)
    relation_states: dict[str, InputState] = Field(default_factory=dict)
    node_provenance: list[NodeProvenanceEntry] = Field(default_factory=list)

    @model_validator(mode="after")
    def validate_annotation_roots(self) -> Self:
        if self.outputs is None:
            if self.input_states:
                raise ValueError("input_states requires typed outputs")
            if self.relation_states:
                raise ValueError("relation_states requires typed outputs")
        elif not self.outputs:
            raise ValueError("compiled node annotations require a declared output")
        return self


class Interval(BaseModel):
    start: date
    end: date


class Period(BaseModel):
    period_kind: str
    start: date
    end: date
    name: str | None = None


class ScalarValue(BaseModel):
    kind: Literal["bool", "integer", "decimal", "text", "date"]
    value: bool | int | str


class InputRecord(BaseModel):
    name: str
    entity: str
    entity_id: str
    interval: Interval
    value: ScalarValue


class RelationRecord(BaseModel):
    name: str
    tuple: list[str]
    interval: Interval


class Dataset(BaseModel):
    inputs: list[InputRecord] = Field(default_factory=list)
    relations: list[RelationRecord] = Field(default_factory=list)


class ExecutionQuery(BaseModel):
    entity_id: str
    period: Period
    outputs: list[str]
    # Decision/assessment time: the date the determination is made, as opposed
    # to `period` (valid time — the benefit period the law governs). Reserved
    # for the bitemporal semantics in docs/bitemporal.md. The engine parses and
    # validates it (it must be on or after `period.start`) but it has NO effect
    # on evaluation yet.
    assessment_date: date | None = None


class ExecutionRequest(BaseModel):
    mode: ExecutionMode
    program: Program
    dataset: Dataset
    queries: list[ExecutionQuery]


class FastPathMetadata(BaseModel):
    strategy: str
    compatible: bool
    blockers: list[str] = Field(default_factory=list)


class CompiledInputCatalogEntry(BaseModel):
    slot: str
    canonical_request_name: str
    request_names: list[str]


class CompiledNodeMetadata(BaseModel):
    id: str
    name: str
    kind: NodeKind
    state: Literal["input", "derived", "pending"]
    input_kind: Literal["exogenous", "policy_derived"] | None = None
    reachable: bool
    provenance: NodeProvenance
    corpus_citation_path: str | None = None

    @model_validator(mode="after")
    def validate_node_contract(self) -> Self:
        allowed_states = {
            "input": {"input", "pending"},
            "data_relation": {"input", "pending"},
            "parameter": {"derived"},
            "derived": {"derived"},
            "derived_relation": {"derived"},
        }
        if self.state not in allowed_states[self.kind]:
            raise ValueError(
                f"{self.kind} node cannot have state {self.state}"
            )
        if (self.state == "input") != (self.input_kind is not None):
            raise ValueError(
                "input_kind is required exactly when state is input"
            )
        if self.kind == "input" and self.provenance != "unverified":
            raise ValueError("implicit input nodes must have unverified provenance")
        _validate_provenance_citation(
            self.provenance,
            self.corpus_citation_path,
        )
        return self


class CompiledProgramMetadata(BaseModel):
    evaluation_order: list[str]
    fast_path: FastPathMetadata
    input_catalog: list[CompiledInputCatalogEntry]
    nodes: list[CompiledNodeMetadata] | None = None


class CompiledProgram(BaseModel):
    artifact_format_version: Literal[2]
    engine_version: str | None = None
    program: Program
    metadata: CompiledProgramMetadata


class CompiledExecutionRequest(BaseModel):
    mode: ExecutionMode
    dataset: Dataset
    queries: list[ExecutionQuery]


class ScalarOutput(BaseModel):
    kind: Literal["scalar"]
    name: str
    id: str | None = None
    dtype: str
    unit: str | None = None
    value: ScalarValue


class JudgmentOutput(BaseModel):
    kind: Literal["judgment"]
    name: str
    id: str | None = None
    unit: str | None = None
    outcome: Literal["holds", "not_holds", "undetermined"]


OutputValue = Annotated[ScalarOutput | JudgmentOutput, Field(discriminator="kind")]


class ScalarTraceNode(BaseModel):
    kind: Literal["scalar"]
    name: str
    id: str | None = None
    dtype: str
    unit: str | None = None
    value: ScalarValue
    source: str | None = None
    source_url: str | None = None
    dependencies: list[str] = Field(default_factory=list)


class JudgmentTraceNode(BaseModel):
    kind: Literal["judgment"]
    name: str
    id: str | None = None
    unit: str | None = None
    outcome: Literal["holds", "not_holds", "undetermined"]
    source: str | None = None
    source_url: str | None = None
    dependencies: list[str] = Field(default_factory=list)


DerivedTraceNode = Annotated[
    ScalarTraceNode | JudgmentTraceNode, Field(discriminator="kind")
]


class QueryResult(BaseModel):
    entity_id: str
    period: Period
    # Echo of the query's `assessment_date` (see docs/bitemporal.md).
    assessment_date: date | None = None
    outputs: dict[str, OutputValue]
    trace: dict[str, DerivedTraceNode] = Field(default_factory=dict)


class ExecutionMetadata(BaseModel):
    requested_mode: ExecutionMode
    actual_mode: ExecutionMode
    fallback_reason: str | None = None


class ExecutionResponse(BaseModel):
    metadata: ExecutionMetadata
    results: list[QueryResult]
