from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

try:
    from axiom_rules_engine_dense import CompiledDenseProgram as NativeCompiledDenseProgram
except ImportError:  # pragma: no cover - exercised only when the extension is missing
    NativeCompiledDenseProgram = None


@dataclass(frozen=True)
class DenseRelationSchema:
    key: str
    name: str
    current_slot: int
    related_slot: int
    related_inputs: tuple[str, ...]


@dataclass(frozen=True)
class DerivedMetadata:
    """Authoring-level metadata for one derived rule.

    Captured from the RuleSpec module before lowering (the runtime model
    drops ``period``). ``dtype`` is the engine vocabulary: ``judgment`` /
    ``bool`` / ``integer`` / ``decimal`` / ``text`` / ``date``; ``period``
    is the authored granularity (``Year`` / ``Month`` / ...) when declared.
    """

    name: str
    id: str | None
    entity: str
    dtype: str
    unit: str | None
    period: str | None
    source: str | None


@dataclass(frozen=True)
class DenseRelationBatch:
    offsets: np.ndarray
    inputs: dict[str, np.ndarray]


class CompiledDenseProgram:
    def __init__(self, native_program: Any) -> None:
        self._native = native_program

    @classmethod
    def from_file(
        cls, path: str | Path, *, entity: str | None = None
    ) -> "CompiledDenseProgram":
        if NativeCompiledDenseProgram is None:
            raise RuntimeError(
                "axiom_rules_engine_dense is not installed. Build it with "
                "`maturin develop --release --manifest-path python-ext/Cargo.toml`."
            )
        return cls(NativeCompiledDenseProgram.from_file(str(Path(path)), entity))

    @property
    def root_entity(self) -> str:
        return self._native.root_entity

    @property
    def root_inputs(self) -> list[str]:
        return list(self._native.root_inputs())

    @property
    def output_names(self) -> list[str]:
        return list(self._native.output_names())

    @property
    def relations(self) -> list[DenseRelationSchema]:
        return [
            DenseRelationSchema(
                key=item.key,
                name=item.name,
                current_slot=item.current_slot,
                related_slot=item.related_slot,
                related_inputs=tuple(item.related_inputs),
            )
            for item in self._native.relations()
        ]

    @property
    def derived_metadata(self) -> list[DerivedMetadata]:
        """Metadata for every derived rule in the module, all entities."""
        return [
            DerivedMetadata(
                name=item.name,
                id=item.id,
                entity=item.entity,
                dtype=item.dtype,
                unit=item.unit,
                period=item.period,
                source=item.source,
            )
            for item in self._native.derived_metadata()
        ]

    def execute(
        self,
        *,
        period_kind: str,
        start: str,
        end: str,
        inputs: dict[str, np.ndarray],
        relations: dict[str, DenseRelationBatch] | None = None,
        outputs: list[str] | None = None,
    ) -> dict[str, Any]:
        return self._run(
            self._native.execute, period_kind, start, end, inputs, relations, outputs
        )

    def execute_f64(
        self,
        *,
        period_kind: str,
        start: str,
        end: str,
        inputs: dict[str, np.ndarray],
        relations: dict[str, DenseRelationBatch] | None = None,
        outputs: list[str] | None = None,
    ) -> dict[str, Any]:
        """Execute in f64 arithmetic (faster; floating-point rounding).

        Intended for microsimulation-style batch workloads; exact legal
        determinations should use :meth:`execute`.
        """
        return self._run(
            self._native.execute_f64,
            period_kind,
            start,
            end,
            inputs,
            relations,
            outputs,
        )

    def execute_lifetime_f64(
        self,
        *,
        periods: list[tuple[str, str, str]],
        batches: list[dict[str, np.ndarray]]
        | list[tuple[dict[str, np.ndarray], dict[str, DenseRelationBatch] | None]],
        outputs: list[str] | None = None,
    ) -> dict[str, Any]:
        """Execute over an entity's lifetime in f64 arithmetic.

        One positionally aligned input batch per period, so formulas can reduce
        over the period axis with ``sum_over_periods`` / ``max_over_periods`` /
        ``count_over_periods`` / ``sum_top_n_over_periods``.

        ``periods`` is a list of ``(period_kind, start, end)`` triples;
        ``batches`` is a same-length list where each entry is either a bare
        ``inputs`` dict (no relations) or an ``(inputs, relations)`` tuple, using
        the same column and relation shapes :meth:`execute` accepts. Row ``i``
        must be the same entity in every batch (identical row order and count).
        Every requested output's formula must contain an over-periods reduction;
        period-specific outputs should use :meth:`execute_f64`.
        """
        if len(periods) != len(batches):
            raise ValueError(
                "execute_lifetime_f64 needs one batch per period: "
                f"got {len(periods)} periods and {len(batches)} batches"
            )
        prepared_batches = [
            self._prepare_lifetime_batch(batch) for batch in batches
        ]
        prepared_periods = [
            (str(kind), str(start), str(end)) for kind, start, end in periods
        ]
        return self._native.execute_lifetime_f64(
            prepared_periods, prepared_batches, outputs
        )

    @staticmethod
    def _prepare_lifetime_batch(
        batch: dict[str, np.ndarray]
        | tuple[dict[str, np.ndarray], dict[str, DenseRelationBatch] | None],
    ) -> Any:
        if isinstance(batch, dict):
            inputs: dict[str, np.ndarray] = batch
            relations: dict[str, DenseRelationBatch] | None = None
        else:
            inputs, relations = batch
        prepared_inputs = {name: np.asarray(values) for name, values in inputs.items()}
        if relations is None:
            return (prepared_inputs, None)
        prepared_relations = {
            key: {
                "offsets": np.asarray(rel.offsets),
                "inputs": {
                    name: np.asarray(values) for name, values in rel.inputs.items()
                },
            }
            for key, rel in relations.items()
        }
        return (prepared_inputs, prepared_relations)

    def _run(
        self,
        native_execute: Any,
        period_kind: str,
        start: str,
        end: str,
        inputs: dict[str, np.ndarray],
        relations: dict[str, DenseRelationBatch] | None,
        outputs: list[str] | None,
    ) -> dict[str, Any]:
        prepared_inputs = {name: np.asarray(values) for name, values in inputs.items()}
        prepared_relations = None
        if relations is not None:
            prepared_relations = {
                key: {
                    "offsets": np.asarray(batch.offsets),
                    "inputs": {
                        name: np.asarray(values)
                        for name, values in batch.inputs.items()
                    },
                }
                for key, batch in relations.items()
            }
        return native_execute(
            period_kind,
            start,
            end,
            prepared_inputs,
            prepared_relations,
            outputs,
        )
