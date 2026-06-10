from __future__ import annotations

from datetime import date
from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Field


ExecutionMode = Literal["explain", "fast"]


class Program(BaseModel):
    model_config = ConfigDict(extra="allow")

    units: list[dict[str, Any]] = Field(default_factory=list)
    relations: list[dict[str, Any]] = Field(default_factory=list)
    parameters: list[dict[str, Any]] = Field(default_factory=list)
    derived: list[dict[str, Any]] = Field(default_factory=list)


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


class CompiledProgramMetadata(BaseModel):
    evaluation_order: list[str]
    fast_path: FastPathMetadata


class CompiledProgram(BaseModel):
    artifact_format_version: int = 0
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
