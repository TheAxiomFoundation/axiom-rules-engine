"""Dense batch shapes through the native extension.

The extension builds a ``DenseBatchSpec`` from numpy arrays, so every shape a
Python caller can hand it must either evaluate or raise an ordinary exception:
never a pyo3 ``PanicException`` (a ``BaseException``) and never an allocation
failure that aborts the interpreter.

These tests exercise the native ``axiom_rules_engine_dense`` extension and skip
when it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import DenseRelationBatch, NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

MONTH = {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"}

HOUSEHOLD_SIZE_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"""

# Bare membership compiles to a literal mask one entry per related row, and
# the relation has no related input to corroborate the offsets.
LISTED_UNIT_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: listed_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: ListedUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household
  - name: listed_unit_size
    kind: derived
    entity: ListedUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
"""

# A derived relation sourced from another derived relation. Both compile to a
# schema over the same source relation key, each reading its own related input.
NESTED_UNIT_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn
  - name: adult_member
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: age >= 18
  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: snap_member_eligible
  - name: adult_snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: snap_unit
      entity: AdultSnapUnit
      member_relation: adult_members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: adult_member
  - name: adult_snap_unit_size
    kind: derived
    entity: AdultSnapUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(adult_members)
"""

LIFETIME_BASE_MODULE = """\
format: rulespec/v1
rules:
  - name: shifted_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_over_periods(earnings) + base
"""


def _compile(tmp_path: Path, source: str, entity: str) -> CompiledDenseProgram:
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/batch_shapes.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(source, encoding="utf-8")
    return CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity=entity)


def _only_relation_key(program: CompiledDenseProgram) -> str:
    keys = {relation.key for relation in program.relations}
    assert len(keys) == 1, keys
    return keys.pop()


def test_unjustified_offsets_raise_instead_of_aborting(tmp_path) -> None:
    # Offsets claiming 2^50 related rows used to size the membership mask, and
    # the failed allocation aborted the interpreter.
    program = _compile(tmp_path, LISTED_UNIT_MODULE, "ListedUnit")
    key = _only_relation_key(program)
    with pytest.raises(RuntimeError, match="no related column"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                key: DenseRelationBatch(offsets=np.array([0, 2**50]), inputs={})
            },
        )


def test_count_only_relation_needs_a_stated_related_row_count(tmp_path) -> None:
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    offsets = np.array([0, 3, 5])

    with pytest.raises(RuntimeError, match="related_row_count"):
        program.execute(
            **MONTH,
            inputs={},
            relations={key: DenseRelationBatch(offsets=offsets, inputs={})},
        )

    result = program.execute(
        **MONTH,
        inputs={},
        relations={
            key: DenseRelationBatch(offsets=offsets, inputs={}, related_row_count=5)
        },
    )
    np.testing.assert_array_equal(result["outputs"]["household_size"], [3, 2])


def test_stated_related_row_count_must_match_the_offsets(tmp_path) -> None:
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    with pytest.raises(RuntimeError, match="offsets end at 5 but related_row_count is 4"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                key: DenseRelationBatch(
                    offsets=np.array([0, 3, 5]), inputs={}, related_row_count=4
                )
            },
        )


def test_negative_related_row_count_is_rejected(tmp_path) -> None:
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    with pytest.raises(ValueError, match="related_row_count must be non-negative"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                key: DenseRelationBatch(
                    offsets=np.array([0, 0]), inputs={}, related_row_count=-1
                )
            },
        )


def test_lifetime_related_row_count_is_forwarded(tmp_path) -> None:
    # The lifetime surface prepares relation batches separately; the stated
    # count has to reach the engine there too.
    program = _compile(
        tmp_path,
        HOUSEHOLD_SIZE_MODULE
        + """\
  - name: household_size_total
    kind: derived
    entity: Household
    dtype: Integer
    period: Year
    versions:
      - effective_from: 2026-01-01
        formula: |-
          sum_over_periods(household_size)
""",
        "Household",
    )
    key = _only_relation_key(program)
    periods = [
        ("year", "2026-01-01", "2026-12-31"),
        ("year", "2027-01-01", "2027-12-31"),
    ]
    batch = (
        {},
        {
            key: DenseRelationBatch(
                offsets=np.array([0, 3, 5]), inputs={}, related_row_count=5
            )
        },
    )
    result = program.execute_lifetime_f64(
        periods=periods, batches=[batch, batch], outputs=["household_size_total"]
    )
    np.testing.assert_array_equal(result["outputs"]["household_size_total"], [6, 4])


def test_nested_derived_relations_keep_every_related_column(tmp_path) -> None:
    # Both derived relations bind the source relation's batch. The extension
    # used to build that batch once per schema, so the second schema's columns
    # replaced the first's and a complete batch failed with a missing input.
    program = _compile(tmp_path, NESTED_UNIT_MODULE, "AdultSnapUnit")
    key = _only_relation_key(program)
    assert sorted(
        name for relation in program.relations for name in relation.related_inputs
    ) == ["age", "has_ssn"]
    result = program.execute(
        **MONTH,
        inputs={},
        relations={
            key: DenseRelationBatch(
                offsets=np.array([0, 3, 4]),
                inputs={
                    "has_ssn": np.array([True, True, False, True]),
                    "age": np.array([30, 12, 40, 70]),
                },
            )
        },
    )
    # Household 0: members 30 (eligible adult), 12 (eligible child), 40
    # (ineligible) -> 1. Household 1: member 70 (eligible adult) -> 1.
    np.testing.assert_array_equal(result["outputs"]["adult_snap_unit_size"], [1, 1])


def test_zero_row_lifetime_dtype_change_raises_runtime_error(tmp_path) -> None:
    # An empty bool column in one period and an empty float column in the next
    # used to panic while quoting row 0 of an empty column.
    program = _compile(tmp_path, LIFETIME_BASE_MODULE, "Worker")
    periods = [
        ("year", "2001-01-01", "2001-12-31"),
        ("year", "2002-01-01", "2002-12-31"),
    ]
    batches = [
        {"earnings": np.array([], dtype=float), "base": np.array([], dtype=bool)},
        {"earnings": np.array([], dtype=float), "base": np.array([], dtype=float)},
    ]
    with pytest.raises(RuntimeError, match="an empty bool column"):
        program.execute_lifetime_f64(
            periods=periods, batches=batches, outputs=["shifted_total"]
        )
