from __future__ import annotations

from datetime import date
from typing import Annotated, Any, Literal

from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    SerializerFunctionWrapHandler,
    field_validator,
    model_serializer,
)


ExecutionMode = Literal["explain", "fast"]
RelationBinding = Literal["strict", "lenient"]


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
    relation_binding: RelationBinding = "strict"
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


class CompiledProgramMetadata(BaseModel):
    evaluation_order: list[str]
    fast_path: FastPathMetadata
    input_catalog: list[CompiledInputCatalogEntry]


class _Exact(BaseModel):
    """Format-3 identity and interface objects have exactly their keys: an
    unknown key is refused, and so is a missing one, a nullable one
    included."""

    model_config = ConfigDict(extra="forbid")


class EngineBuild(_Exact):
    """The build of the engine that compiled an artifact."""

    commit: str | None
    dirty: bool | None


class SourceClosureModule(_Exact):
    target: str | None
    sha256: str


class SourceClosure(_Exact):
    """The RuleSpec module texts an artifact was compiled from."""

    root: str | None
    modules: list[SourceClosureModule]
    digest: str


class SuppliedInstances(_Exact):
    source: Literal["supplied"]


class AliasInstances(_Exact):
    source: Literal["alias"]
    of: str | None
    via: list[str]


class UnscopedInstances(_Exact):
    source: Literal["unscoped"]


EntityInstances = Annotated[
    SuppliedInstances | AliasInstances | UnscopedInstances,
    Field(discriminator="source"),
]


class InterfaceEntity(_Exact):
    kind: str
    id_template: str | None
    instances: EntityInstances


class InputElection(_Exact):
    """A choice the law gives ``actor``, an entity kind, under
    ``provision``, a corpus citation path. The engine evaluates the value it
    is given and never defaults one."""

    actor: str
    provision: str


class InterfaceInput(_Exact):
    name: str
    legal_id: str
    entities: list[str]
    dtype: Literal["bool", "text", "date", "integer", "decimal"] | None
    dtype_basis: Literal["usage", "declared"] | None
    default: ScalarValue | None
    values: list[int] | None
    values_basis: Literal["parameter_index"] | None
    # Null for a fact, which a simulator must not optimize over.
    election: InputElection | None


class RelationReads(_Exact):
    inputs: list[str]
    rules: list[str]
    relations: list[str]


class SuppliedRelation(_Exact):
    kind: Literal["supplied"]


class DerivedRelation(_Exact):
    kind: Literal["derived"]
    rule: str
    source_relation: str
    reads: RelationReads


RelationProvenance = Annotated[
    SuppliedRelation | DerivedRelation, Field(discriminator="kind")
]


class InterfaceRelation(_Exact):
    name: str
    arity: int
    slot_entities: list[str] | None
    lookup_slots: list[int]
    accepts_supplied_tuples: bool
    provenance: RelationProvenance


class ProgramInterface(_Exact):
    """``program.interface``: the entity roster, input slots and relations
    the engine derived from the program. Execution never reads it."""

    entities: list[InterfaceEntity]
    inputs: list[InterfaceInput]
    relations: list[InterfaceRelation]


class CompiledProgramBody(Program):
    interface: ProgramInterface


class CompiledProgram(BaseModel):
    """A format-3 compiled artifact, as ``compile`` writes it. Its top-level
    keys are a closed set, as the engine's loader requires."""

    model_config = ConfigDict(extra="forbid")

    artifact_format_version: Literal[3]
    engine_version: str
    engine_build: EngineBuild
    # Omitted, never null, for a program compiled without RuleSpec source.
    source_closure: SourceClosure | None = None
    program: CompiledProgramBody
    metadata: CompiledProgramMetadata

    @field_validator("source_closure", mode="before")
    @classmethod
    def _source_closure_is_omitted_not_null(cls, value: Any) -> Any:
        if value is None:
            raise ValueError("source_closure is omitted when there is none, never null")
        return value

    @model_serializer(mode="wrap")
    def _omit_absent_source_closure(self, handler: SerializerFunctionWrapHandler) -> dict[str, Any]:
        # Every dump omits an absent closure, so a loaded artifact saves as
        # one the engine and this model accept.
        data = handler(self)
        if self.source_closure is None:
            data.pop("source_closure", None)
        return data


class LegacyCompiledProgramV2(BaseModel):
    """A format-2 compiled artifact. The engine runs one only on request,
    through its format-2 compatibility loader (``accept_artifact_format=2``)."""

    artifact_format_version: Literal[2]
    engine_version: str | None = None
    program: Program
    metadata: CompiledProgramMetadata


class CompiledExecutionRequest(BaseModel):
    mode: ExecutionMode
    relation_binding: RelationBinding = "strict"
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
    relation_binding: RelationBinding | None = None
    fallback_reason: str | None = None


class ExecutionResponse(BaseModel):
    metadata: ExecutionMetadata
    results: list[QueryResult]
