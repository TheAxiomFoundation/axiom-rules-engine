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
        return self._native.execute(
            period_kind,
            start,
            end,
            prepared_inputs,
            prepared_relations,
            outputs,
        )
