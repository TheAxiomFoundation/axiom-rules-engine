"""Dense batch shapes through the native extension.

The extension builds a ``DenseBatchSpec`` from numpy arrays. Offsets that no
related column or stated ``related_row_count`` confirms, and a stated count
too large to allocate, must raise an ordinary exception: never a pyo3
``PanicException`` (a ``BaseException``) and never an allocation failure that
aborts the interpreter. A stated count is the caller's assertion and is trusted
up to that point.

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
      arguments: [Person, Household]
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"""

# Bare membership: a derived relation with no root or related input.
LISTED_UNIT_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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

TEXT_COPY_MODULE = """\
format: rulespec/v1
rules:
  - name: label_copy
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: label
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


def test_unconfirmed_offsets_raise_instead_of_aborting(tmp_path) -> None:
    # Binding allocates one owner per related row, so offsets claiming 2^50
    # related rows used to abort the interpreter on the failed allocation.
    program = _compile(tmp_path, LISTED_UNIT_MODULE, "ListedUnit")
    key = _only_relation_key(program)
    with pytest.raises(RuntimeError, match="no related column"):
        program.execute(
            **MONTH,
            inputs={},
            relations={key: DenseRelationBatch(offsets=np.array([0, 2**50]), inputs={})},
        )


HOUSEHOLD_INCOME_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: household_income
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.income)
"""


@pytest.mark.parametrize("module", [HOUSEHOLD_SIZE_MODULE, HOUSEHOLD_INCOME_MODULE])
def test_issue_227_unconfirmed_counts_raise(tmp_path, module) -> None:
    # #227: with no related input, or the one read input omitted, offsets
    # claiming i64::MAX or more related rows than memory (u32::MAX in the
    # issue; 2^50 here, which no host can allocate) used to panic or abort
    # while allocating owners.
    program = _compile(tmp_path, module, "Household")
    key = _only_relation_key(program)
    for claimed in (2**63 - 1, 2**50):
        with pytest.raises(RuntimeError, match=f"offsets end at {claimed}.*no related column"):
            program.execute(
                **MONTH,
                inputs={},
                relations={
                    key: DenseRelationBatch(offsets=np.array([0, claimed]), inputs={})
                },
            )


def test_stated_count_larger_than_memory_raises(tmp_path) -> None:
    # A stated count is trusted, but binding reserves its owners fallibly, so
    # one no host can allocate raises instead of aborting.
    program = _compile(tmp_path, LISTED_UNIT_MODULE, "ListedUnit")
    key = _only_relation_key(program)
    with pytest.raises(RuntimeError, match="more than this process can allocate"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                key: DenseRelationBatch(
                    offsets=np.array([0, 2**50]), inputs={}, related_row_count=2**50
                )
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


def test_an_unread_column_of_the_right_length_confirms_the_offsets(tmp_path) -> None:
    # The program reads no related column, so the extension does not convert
    # `age`; its length still confirms the five related rows.
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    result = program.execute(
        **MONTH,
        inputs={},
        relations={
            key: DenseRelationBatch(
                offsets=np.array([0, 3, 5]), inputs={"age": np.arange(5)}
            )
        },
    )
    np.testing.assert_array_equal(result["outputs"]["household_size"], [3, 2])


def test_an_unread_column_of_the_wrong_length_is_refused(tmp_path) -> None:
    # The extension passes the engine a length-only stand-in for `age`, so the
    # engine reports it as it would a Rust caller's column.
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    for stated in (None, 5):
        with pytest.raises(
            RuntimeError, match="`age` .* has length 2 but related row count is 5"
        ):
            program.execute(
                **MONTH,
                inputs={},
                relations={
                    key: DenseRelationBatch(
                        offsets=np.array([0, 3, 5]),
                        inputs={"age": np.arange(2)},
                        related_row_count=stated,
                    )
                },
            )


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


def test_zero_row_lifetime_dtype_change_does_not_panic(tmp_path) -> None:
    # An empty bool column in one period and an empty float column in the
    # next used to panic while quoting row 0 of an empty column. With no row
    # to compare, the empty result binds.
    program = _compile(tmp_path, LIFETIME_BASE_MODULE, "Worker")
    periods = [
        ("year", "2001-01-01", "2001-12-31"),
        ("year", "2002-01-01", "2002-12-31"),
    ]
    batches = [
        {"earnings": np.array([], dtype=float), "base": np.array([], dtype=bool)},
        {"earnings": np.array([], dtype=float), "base": np.array([], dtype=float)},
    ]
    result = program.execute_lifetime_f64(
        periods=periods, batches=batches, outputs=["shifted_total"]
    )
    assert result["row_count"] == 0


def test_an_empty_text_array_stays_text(tmp_path) -> None:
    # An empty array converts as an empty list of anything, so the extension
    # used to read an empty string array as a bool column.
    program = _compile(tmp_path, TEXT_COPY_MODULE, "Person")
    result = program.execute(**MONTH, inputs={"label": np.array([], dtype=str)})
    # Text columns come back as Python lists; a bool column would be an array.
    assert isinstance(result["outputs"]["label_copy"], list)
    assert result["outputs"]["label_copy"] == []
    result = program.execute(**MONTH, inputs={"label": np.array(["a", "b"])})
    assert result["outputs"]["label_copy"] == ["a", "b"]


TWO_RELATION_MODULE = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: pet_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Pet, Household]
  - name: household_total
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.income) + sum(pet_of_household.cost)
"""

INTEGER_COPY_MODULE = """\
format: rulespec/v1
rules:
  - name: x_copy
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: x
"""


def test_a_stated_count_mismatch_is_reported_before_an_unread_column(tmp_path) -> None:
    # The engine checks a key's stated count before any column no schema
    # reads; the extension no longer reports unread columns itself first.
    program = _compile(tmp_path, HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    with pytest.raises(RuntimeError, match="offsets end at 3 but related_row_count is 2"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                key: DenseRelationBatch(
                    offsets=np.array([0, 3]),
                    inputs={"age": np.array([1])},
                    related_row_count=2,
                )
            },
        )


def test_a_later_relations_read_column_is_reported_before_unread_ones(tmp_path) -> None:
    # Pets' read column is short, and so is an unread column of the members
    # relation the program reads first. A per-schema binding reports `cost`;
    # unread columns come after every schema's checks.
    program = _compile(tmp_path, TWO_RELATION_MODULE, "Household")
    keys = [relation.key for relation in program.relations]
    assert [key.rsplit(".", 1)[1].split(":")[0] for key in keys] == [
        "member_of_household",
        "pet_of_household",
    ]
    members, pets = keys
    with pytest.raises(RuntimeError, match="`cost` .* has length 0 but related row count is 1"):
        program.execute(
            **MONTH,
            inputs={},
            relations={
                members: DenseRelationBatch(
                    offsets=np.array([0, 1]),
                    inputs={"income": np.array([10]), "unused": np.array([], dtype=np.int64)},
                ),
                pets: DenseRelationBatch(
                    offsets=np.array([0, 1]), inputs={"cost": np.array([], dtype=np.int64)}
                ),
            },
        )


def test_a_two_dimensional_empty_array_is_refused(tmp_path) -> None:
    # A (2, 0) array has no elements, but it is not a column: it must not
    # become a zero-row batch.
    program = _compile(tmp_path, INTEGER_COPY_MODULE, "Person")
    with pytest.raises(ValueError, match="dense columns must be"):
        program.execute(**MONTH, inputs={"x": np.empty((2, 0), dtype=np.int64)})
