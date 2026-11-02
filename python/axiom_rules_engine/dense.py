from __future__ import annotations

import warnings
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Literal

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
    """One relation's rows for a batch.

    Root row ``r``'s related rows are ``offsets[r]:offsets[r + 1]`` of every
    column in ``inputs``. ``offsets`` has one entry per root row plus one,
    starts at 0 and never decreases; its last entry is the related row count,
    which every column in ``inputs`` must have, whether or not the program
    reads it.

    ``related_row_count`` states that count. When given it must equal
    ``offsets[-1]``. It is required when the offsets claim related rows but
    ``inputs`` is empty (for example a ``len`` over the relation, or a filter
    that reads only the current entity), because nothing else confirms the
    offsets. It is the caller's assertion, not a check: count the related rows
    the offsets index, one per root and member pair (a related entity two roots
    share counts twice), independently of the offsets, rather than copying
    ``offsets[-1]``.
    """

    offsets: np.ndarray
    inputs: dict[str, np.ndarray]
    related_row_count: int | None = None


LifetimeBatch = (
    dict[str, np.ndarray]
    | tuple[dict[str, np.ndarray]]
    | tuple[dict[str, np.ndarray], dict[str, DenseRelationBatch] | None]
    | tuple[dict[str, np.ndarray], dict[str, DenseRelationBatch] | None, int | None]
)
"""One period's batch for :meth:`CompiledDenseProgram.execute_lifetime_f64`:
a bare inputs dict, or an ``(inputs[, relations[, row_count]])`` tuple."""


def _requested_outputs(outputs: Iterable[str] | None) -> list[str] | None:
    """The rule names a compile entry point passes the extension."""
    if outputs is None:
        return None
    if isinstance(outputs, str):
        raise TypeError(
            "outputs takes an iterable of rule names, not a single string; "
            f"pass [{outputs!r}]"
        )
    return list(outputs)


def _prepare_relations(
    relations: dict[str, DenseRelationBatch],
) -> dict[str, dict[str, Any]]:
    """The relation dicts the native extension reads."""
    return {
        key: {
            "offsets": np.asarray(batch.offsets),
            "inputs": {
                name: np.asarray(values) for name, values in batch.inputs.items()
            },
            "related_row_count": batch.related_row_count,
        }
        for key, batch in relations.items()
    }


class CompiledDenseProgram:
    def __init__(self, native_program: Any) -> None:
        self._native = native_program

    @classmethod
    def from_file(
        cls,
        path: str | Path,
        *,
        rulespec_roots: Iterable[str | Path],
        entity: str | None = None,
        outputs: Iterable[str] | None = None,
    ) -> "CompiledDenseProgram":
        """Compile a RuleSpec module for dense execution.

        Without ``outputs``, this compiles every rule of the root entity that
        the module's import closure holds, so any one of them that dense
        cannot compile refuses the program, even one no output you want
        reaches.

        With ``outputs``, it compiles only those rules and the rules they
        reach. A rule none of them reaches is not compiled and cannot refuse
        the program; a rule they do reach refuses it as before. Name outputs
        by rule name, as :meth:`execute` takes them. Each must be a rule of
        the root entity or an entity-free ``Scalar`` rule; with no ``entity``,
        the outputs' entity is the root entity. :attr:`output_names`,
        :attr:`root_inputs` and :attr:`relations` then list only what those
        rules read, so a batch needs nothing else. For every output both
        compile, the values and errors are the same either way.
        """
        if NativeCompiledDenseProgram is None:
            raise RuntimeError(
                "axiom_rules_engine_dense is not installed. Build it with "
                "`maturin develop --release --manifest-path python-ext/Cargo.toml`."
            )
        roots = [str(Path(root)) for root in rulespec_roots]
        return cls(
            NativeCompiledDenseProgram.from_file(
                str(Path(path)), roots, entity, _requested_outputs(outputs)
            )
        )

    @classmethod
    def from_composed_file(
        cls,
        path: str | Path,
        *,
        rulespec_roots: Iterable[str | Path],
        entity: str | None = None,
        outputs: Iterable[str] | None = None,
    ) -> "CompiledDenseProgram":
        """Compile an originless composition emitted by axiom-compose.

        ``entity`` and ``outputs`` are as for :meth:`from_file`.
        """
        if NativeCompiledDenseProgram is None:
            raise RuntimeError(
                "axiom_rules_engine_dense is not installed. Build it with "
                "`maturin develop --release --manifest-path python-ext/Cargo.toml`."
            )
        roots = [str(Path(root)) for root in rulespec_roots]
        return cls(
            NativeCompiledDenseProgram.from_composed_file(
                str(Path(path)), roots, entity, _requested_outputs(outputs)
            )
        )

    @classmethod
    def from_artifact_file(
        cls,
        path: str | Path,
        *,
        entity: str | None = None,
        accept_artifact_format: Literal[2] | None = None,
        outputs: Iterable[str] | None = None,
    ) -> "CompiledDenseProgram":
        """Load a compiled artifact (``compile``'s output) for dense execution.

        The artifact must be format 3. ``accept_artifact_format=2`` also loads
        a format-2 artifact, unchanged, through the engine's format-2
        compatibility loader, with a warning. ``entity`` and ``outputs`` are
        as for :meth:`from_file`.
        """
        if NativeCompiledDenseProgram is None:
            raise RuntimeError(
                "axiom_rules_engine_dense is not installed. Build it with "
                "`maturin develop --release --manifest-path python-ext/Cargo.toml`."
            )
        if accept_artifact_format not in (None, 2):
            raise ValueError(
                "accept_artifact_format accepts only 2, the format the engine "
                "reads through its compatibility loader"
            )
        native = NativeCompiledDenseProgram.from_artifact_file(
            str(Path(path)), entity, accept_artifact_format, _requested_outputs(outputs)
        )
        if native.artifact_format_version == 2:
            warnings.warn(
                f"read {path} as artifact format 2 through the format-2 "
                "compatibility loader; recompile it with this engine for format 3",
                stacklevel=2,
            )
        return cls(native)

    @property
    def artifact_format_version(self) -> int:
        """The format of the artifact this program was loaded or compiled as."""
        return self._native.artifact_format_version

    @property
    def root_entity(self) -> str:
        return self._native.root_entity

    @property
    def root_inputs(self) -> list[str]:
        return list(self._native.root_inputs())

    @property
    def output_names(self) -> list[str]:
        """The rules this program compiled, which :meth:`execute` computes.

        Every rule of the root entity and the ``Scalar`` rules they reach, or,
        when compiled with ``outputs``, those rules and the rules they reach.
        """
        return list(self._native.output_names())

    @property
    def input_catalog(self) -> dict[str, str]:
        """Preferred canonical request name keyed by runtime slot."""
        return dict(self._native.input_catalog())

    @property
    def input_request_names(self) -> dict[str, tuple[str, ...]]:
        """Every accepted owner name keyed by runtime slot."""
        return {
            slot: tuple(request_names)
            for slot, request_names in self._native.input_request_names().items()
        }

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
        row_count: int | None = None,
    ) -> dict[str, Any]:
        """Execute one period's batch in exact Decimal arithmetic.

        ``row_count`` states how many root rows the batch has. It must agree
        with every supplied root column and with each relation's offsets, and
        must be given when nothing else gives the count: when the program
        reads no relation and no root input column is supplied (an input-free
        program, or one called without its root inputs). Without it, the first
        supplied root input column gives the count, else the first relation's
        offsets; when neither does, this raises ``ValueError`` naming the
        program's root inputs. It never defaults to zero rows.
        """
        return self._run(
            self._native.execute,
            period_kind,
            start,
            end,
            inputs,
            relations,
            outputs,
            row_count,
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
        row_count: int | None = None,
    ) -> dict[str, Any]:
        """Execute in f64 arithmetic (faster; floating-point rounding).

        Intended for microsimulation-style batch workloads; exact legal
        determinations should use :meth:`execute`. ``row_count`` is as for
        :meth:`execute`.
        """
        return self._run(
            self._native.execute_f64,
            period_kind,
            start,
            end,
            inputs,
            relations,
            outputs,
            row_count,
        )

    def execute_lifetime_f64(
        self,
        *,
        periods: list[tuple[str, str, str]],
        batches: list[LifetimeBatch],
        outputs: list[str] | None = None,
    ) -> dict[str, Any]:
        """Execute over an entity's lifetime in f64 arithmetic.

        One positionally aligned input batch per period, so formulas can reduce
        over the period axis with ``sum_over_periods`` / ``max_over_periods`` /
        ``count_over_periods`` / ``sum_top_n_over_periods``.

        ``periods`` is a list of ``(period_kind, start, end)`` triples;
        ``batches`` is a same-length list where each entry is either a bare
        ``inputs`` dict (no relations) or an ``(inputs[, relations[,
        row_count]])`` tuple, using the same column and relation shapes and
        the same stated ``row_count`` :meth:`execute` accepts. Row ``i`` must
        be the same entity in every batch (identical row order and count).
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
    def _prepare_lifetime_batch(batch: LifetimeBatch) -> Any:
        """The ``(inputs, relations, row_count)`` tuple the extension reads."""
        if isinstance(batch, dict):
            batch = (batch,)
        if not 1 <= len(batch) <= 3:
            raise ValueError(
                "each lifetime batch must be an inputs dict or an "
                "(inputs[, relations[, row_count]]) tuple"
            )
        inputs = batch[0]
        relations = batch[1] if len(batch) > 1 else None
        row_count = batch[2] if len(batch) > 2 else None
        prepared_inputs = {name: np.asarray(values) for name, values in inputs.items()}
        prepared_relations = None
        if relations is not None:
            prepared_relations = _prepare_relations(relations)
        return (prepared_inputs, prepared_relations, row_count)

    def _run(
        self,
        native_execute: Any,
        period_kind: str,
        start: str,
        end: str,
        inputs: dict[str, np.ndarray],
        relations: dict[str, DenseRelationBatch] | None,
        outputs: list[str] | None,
        row_count: int | None,
    ) -> dict[str, Any]:
        prepared_inputs = {name: np.asarray(values) for name, values in inputs.items()}
        prepared_relations = None
        if relations is not None:
            prepared_relations = _prepare_relations(relations)
        return native_execute(
            period_kind,
            start,
            end,
            prepared_inputs,
            prepared_relations,
            outputs,
            row_count=row_count,
        )
