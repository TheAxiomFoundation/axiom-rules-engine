"""Dense columns read every numpy layout the way numpy reads it.

The extension copies each numpy column into an engine column. It used to
borrow the array's storage as a Rust slice, which is sound only when the
storage is aligned for the element type, contiguous, and, for bool, holds only
the bytes 0 and 1. A numpy array promises none of these:

* A bool array can hold any byte, as ``uint8_array.view(np.bool_)`` does, and
  numpy reads every nonzero byte as True. A Rust ``bool`` must be 0 or 1, so
  such a column was misread.
* An int64 or float64 array can start at any byte of its buffer. A debug build
  aborted the interpreter on the misaligned slice, and a release build read
  through it.
* A strided, reversed or broadcast array is not contiguous, and was refused.

These tests build each layout and require the outputs numpy's own reading of
the values gives. They cover root inputs, related inputs and relation offsets;
``execute``, ``execute_f64`` and ``execute_lifetime_f64``; and both the Python
wrapper and the native extension, which callers can import directly.

They exercise the native ``axiom_rules_engine_dense`` extension and skip when
it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

from __future__ import annotations

from typing import Any, Callable

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import DenseRelationBatch, NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

# Each output reads one column, so a misread value or row shows up in it.
MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: |-
    Dense column layout fixture: reads a bool, an integer and a decimal
    column on a household and on its members.
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: flag_holds
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: flag
  - name: unflagged_payment
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if not flag:
              100
          else:
              0
  - name: doubled_units
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: units * 2
  - name: doubled_amount
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: amount * 2
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
  - name: flagged_members
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: count_where(member_of_household, member_flag)
  - name: member_units_total
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.member_units)
  - name: member_amount_total
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.member_amount)
  - name: lifetime_unflagged_payment
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(unflagged_payment)
  - name: lifetime_units
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(units)
  - name: lifetime_amount
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(amount)
  - name: lifetime_flagged_members
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(flagged_members)
  - name: lifetime_member_units
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(member_units_total)
  - name: lifetime_member_amount
    kind: derived
    entity: Household
    dtype: Decimal
    versions:
      - effective_from: 2026-01-01
        formula: sum_over_periods(member_amount_total)
"""

PERIOD = ("month", "2026-01-01", "2026-01-31")
PERIOD_OUTPUTS = [
    "flag_holds",
    "unflagged_payment",
    "doubled_units",
    "doubled_amount",
    "household_size",
    "flagged_members",
    "member_units_total",
    "member_amount_total",
]
LIFETIME_OUTPUTS = [
    "lifetime_unflagged_payment",
    "lifetime_units",
    "lifetime_amount",
    "lifetime_flagged_members",
    "lifetime_member_units",
    "lifetime_member_amount",
]
ROOT_COLUMNS = ("flag", "units", "amount")
RELATED_COLUMNS = ("member_flag", "member_units", "member_amount")


@pytest.fixture(scope="module")
def program(tmp_path_factory) -> CompiledDenseProgram:
    root = (tmp_path_factory.mktemp("dense") / "rulespec-us").resolve()
    path = root / "us/policies/tests/dense_numpy_layouts.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(MODULE_SOURCE, encoding="utf-8")
    return CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Household"
    )


@pytest.fixture(scope="module")
def relation_key(program) -> str:
    (key,) = {schema.key for schema in program.relations}
    return key


# --- Building layouts ------------------------------------------------------


def _bits(array: np.ndarray) -> np.ndarray:
    """The same elements reinterpreted as unsigned integers of their width.

    A same-width view works for any strides and copies exact bytes, so a bool
    column keeps bytes other than 0 and 1, and a float keeps its bit pattern.
    """
    return array.view(np.dtype(f"u{array.dtype.itemsize}"))


def laid_out(values: np.ndarray, *, offset: int, stride: int) -> np.ndarray:
    """``values`` in a fresh buffer, element ``i`` at byte ``offset + i * stride``.

    A negative ``stride`` stores the elements back to front. The buffer is a
    fresh ``bytearray``, so an odd ``offset`` misaligns an 8-byte element; the
    tests check each layout's numpy flags rather than assume it.
    """
    count = len(values)
    itemsize = values.dtype.itemsize
    reach = abs(stride) * max(count - 1, 0)
    buffer = bytearray(offset + reach + itemsize)
    first = offset + (reach if stride < 0 else 0)
    array = np.ndarray(
        (count,), dtype=values.dtype, buffer=buffer, offset=first, strides=(stride,)
    )
    _bits(array)[...] = _bits(values)
    return array


# Every layout but the first is one the old conversion misread or refused.
LAYOUTS: dict[str, Callable[[np.ndarray], np.ndarray]] = {
    "aligned contiguous": lambda v: laid_out(
        v, offset=0, stride=v.dtype.itemsize
    ),
    "unaligned contiguous": lambda v: laid_out(
        v, offset=1, stride=v.dtype.itemsize
    ),
    "strided": lambda v: laid_out(v, offset=0, stride=2 * v.dtype.itemsize),
    "reversed": lambda v: laid_out(v, offset=0, stride=-v.dtype.itemsize),
    "unaligned odd stride": lambda v: laid_out(
        v, offset=3, stride=2 * v.dtype.itemsize + 1
    ),
    "record field": lambda v: _record_field(v),
}


def _record_field(values: np.ndarray) -> np.ndarray:
    """The ``value`` field of a packed record array.

    Each value sits one byte into a record of ``1 + itemsize`` bytes, so the
    field is strided and, for 8-byte values, misaligned.
    """
    records = np.zeros(
        len(values), dtype=np.dtype([("tag", "u1"), ("value", values.dtype)])
    )
    field = records["value"]
    _bits(field)[...] = _bits(values)
    return field


def _bool_column(raw_bytes: list[int]) -> np.ndarray:
    return np.array(raw_bytes, dtype=np.uint8).view(np.bool_)


# --- Running ---------------------------------------------------------------


def _relations_dict(
    key: str, offsets: np.ndarray, related: dict[str, np.ndarray]
) -> dict[str, Any]:
    return {key: {"offsets": offsets, "inputs": related}}


def run(
    program: CompiledDenseProgram,
    surface: str,
    method: str,
    key: str,
    root: dict[str, np.ndarray],
    offsets: np.ndarray,
    related: dict[str, np.ndarray],
) -> dict[str, np.ndarray]:
    """One per-period execution, through the wrapper or the native extension."""
    period_kind, start, end = PERIOD
    if surface == "wrapper":
        result = getattr(program, method)(
            period_kind=period_kind,
            start=start,
            end=end,
            inputs=root,
            relations={key: DenseRelationBatch(offsets=offsets, inputs=related)},
            outputs=PERIOD_OUTPUTS,
        )
    else:
        result = getattr(program._native, method)(
            period_kind,
            start,
            end,
            root,
            _relations_dict(key, offsets, related),
            PERIOD_OUTPUTS,
        )
    return result["outputs"]


def expected_outputs(
    root: dict[str, np.ndarray], offsets: np.ndarray, related: dict[str, np.ndarray]
) -> dict[str, np.ndarray]:
    """The outputs computed in numpy from numpy's own reading of every column."""
    flag = np.array(root["flag"].tolist(), dtype=bool)
    member_flag = np.array(related["member_flag"].tolist(), dtype=bool)
    sizes = np.diff(np.array(offsets.tolist(), dtype=np.int64))
    owners = np.repeat(np.arange(len(sizes)), sizes)

    def per_household(weights: np.ndarray) -> np.ndarray:
        return np.bincount(owners, weights=weights, minlength=len(sizes))

    return {
        "flag_holds": np.where(flag, 1, -1),
        "unflagged_payment": np.where(flag, 0, 100),
        "doubled_units": np.array(root["units"].tolist(), dtype=np.int64) * 2,
        "doubled_amount": np.array(root["amount"].tolist(), dtype=np.float64) * 2,
        "household_size": sizes,
        "flagged_members": per_household(member_flag.astype(np.float64)),
        "member_units_total": per_household(
            np.array(related["member_units"].tolist(), dtype=np.float64)
        ),
        "member_amount_total": per_household(
            np.array(related["member_amount"].tolist(), dtype=np.float64)
        ),
    }


def assert_outputs(actual: dict[str, np.ndarray], expected: dict[str, np.ndarray]):
    assert sorted(actual) == sorted(expected)
    for name, values in expected.items():
        np.testing.assert_array_equal(actual[name], values, err_msg=name)


# Three households of 2, 0 and 3 members. The first and last household are
# flagged, and so are four of the five members.
OFFSETS = np.array([0, 2, 2, 5], dtype=np.int64)


def _root() -> dict[str, np.ndarray]:
    return {
        "flag": _bool_column([1, 0, 1]),
        "units": np.array([3, -7, 11], dtype=np.int64),
        "amount": np.array([1.5, -2.25, 1_000_000.125], dtype=np.float64),
    }


def _related() -> dict[str, np.ndarray]:
    return {
        "member_flag": _bool_column([1, 0, 1, 1, 1]),
        "member_units": np.array([1, 2, 3, 4, 5], dtype=np.int64),
        "member_amount": np.array([0.5, 0.25, 1.0, 2.0, 4.0], dtype=np.float64),
    }


EXPECTED = {
    "flag_holds": [1, -1, 1],
    "unflagged_payment": [0, 100, 0],
    "doubled_units": [6, -14, 22],
    "doubled_amount": [3.0, -4.5, 2_000_000.25],
    "household_size": [2, 0, 3],
    "flagged_members": [1, 0, 3],
    "member_units_total": [3, 0, 12],
    "member_amount_total": [0.75, 0.0, 7.0],
}

METHODS = ["execute", "execute_f64"]
SURFACES = ["wrapper", "native"]


def test_the_oracle_matches_the_hand_computed_outputs() -> None:
    # The premise of every comparison below: computing the outputs in numpy
    # from numpy's reading of the columns gives the hand-derived values.
    computed = expected_outputs(_root(), OFFSETS, _related())
    assert_outputs(computed, {k: np.array(v) for k, v in EXPECTED.items()})


# --- Bool bytes ------------------------------------------------------------


@pytest.mark.parametrize("surface", SURFACES)
@pytest.mark.parametrize("method", METHODS)
def test_a_nonzero_bool_byte_is_true(program, relation_key, surface, method):
    # Bytes 2, 128 and 255 are True to numpy, as 1 is. The premise:
    root_flag = _bool_column([2, 0, 255])
    member_flag = _bool_column([128, 0, 2, 1, 255])
    assert root_flag.tolist() == [True, False, True]
    assert member_flag.tolist() == [True, False, True, True, True]
    assert bool(root_flag[0]) and bool(member_flag[0])

    root = _root() | {"flag": root_flag}
    related = _related() | {"member_flag": member_flag}
    outputs = run(program, surface, method, relation_key, root, OFFSETS, related)
    assert_outputs(outputs, {k: np.array(v) for k, v in EXPECTED.items()})


@pytest.mark.parametrize("method", METHODS)
@pytest.mark.parametrize("byte", [0, 1, 2, 127, 128, 254, 255])
def test_every_bool_byte_reads_as_numpy_reads_it(
    program, relation_key, method, byte
) -> None:
    # Every household and member flag carries the same raw byte.
    root = _root() | {"flag": _bool_column([byte, byte, byte])}
    related = _related() | {"member_flag": _bool_column([byte] * 5)}
    truth = bool(np.array([byte], dtype=np.uint8).view(np.bool_)[0])
    assert truth == (byte != 0)
    outputs = run(program, "native", method, relation_key, root, OFFSETS, related)
    np.testing.assert_array_equal(outputs["flag_holds"], [1 if truth else -1] * 3)
    np.testing.assert_array_equal(
        outputs["unflagged_payment"], [0 if truth else 100] * 3
    )
    np.testing.assert_array_equal(
        outputs["flagged_members"], [2, 0, 3] if truth else [0, 0, 0]
    )


# --- Layouts ---------------------------------------------------------------


@pytest.mark.parametrize("surface", SURFACES)
@pytest.mark.parametrize("method", METHODS)
@pytest.mark.parametrize("layout", list(LAYOUTS))
@pytest.mark.parametrize("target", ["root", "related", "offsets"])
def test_every_layout_reads_its_values(
    program, relation_key, surface, method, layout, target
) -> None:
    originals = {
        "root": _root(),
        "related": _related(),
        "offsets": {"offsets": OFFSETS},
    }[target]
    shaped = {name: LAYOUTS[layout](values) for name, values in originals.items()}
    # The premise: each laid-out column holds the same values, in the layout
    # the case names.
    for name, column in shaped.items():
        assert column.dtype == originals[name].dtype
        assert column.tolist() == originals[name].tolist()
        flags = column.flags
        unaligned_if_wide = column.dtype.itemsize == 1 or not flags.aligned
        if layout == "aligned contiguous":
            assert flags.c_contiguous and flags.aligned
        elif layout == "unaligned contiguous":
            assert flags.c_contiguous and unaligned_if_wide
        else:
            assert not flags.c_contiguous
            if layout in ("unaligned odd stride", "record field"):
                assert unaligned_if_wide
    root = shaped if target == "root" else _root()
    related = shaped if target == "related" else _related()
    offsets = shaped["offsets"] if target == "offsets" else OFFSETS

    outputs = run(program, surface, method, relation_key, root, offsets, related)
    assert_outputs(outputs, {k: np.array(v) for k, v in EXPECTED.items()})


@pytest.mark.parametrize("method", METHODS)
def test_a_broadcast_column_reads_one_value_per_row(
    program, relation_key, method
) -> None:
    # Zero strides: every row reads the one stored value.
    root = {
        "flag": np.broadcast_to(_bool_column([2]), (3,)),
        "units": np.broadcast_to(np.int64(4), (3,)),
        "amount": np.broadcast_to(np.float64(0.5), (3,)),
    }
    related = {
        "member_flag": np.broadcast_to(_bool_column([255]), (5,)),
        "member_units": np.broadcast_to(np.int64(2), (5,)),
        "member_amount": np.broadcast_to(np.float64(0.25), (5,)),
    }
    for column in [*root.values(), *related.values()]:
        assert column.strides == (0,)
    outputs = run(program, "native", method, relation_key, root, OFFSETS, related)
    assert_outputs(outputs, expected_outputs(root, OFFSETS, related))
    assert_outputs(
        outputs,
        {
            "flag_holds": np.array([1, 1, 1]),
            "unflagged_payment": np.array([0, 0, 0]),
            "doubled_units": np.array([8, 8, 8]),
            "doubled_amount": np.array([1.0, 1.0, 1.0]),
            "household_size": np.array([2, 0, 3]),
            "flagged_members": np.array([2, 0, 3]),
            "member_units_total": np.array([4, 0, 6]),
            "member_amount_total": np.array([0.5, 0.0, 0.75]),
        },
    )
    # A zero-row batch: offsets [0] as a broadcast, with empty columns.
    offsets = np.broadcast_to(np.int64(0), (1,))
    empty_root = {name: values[:0] for name, values in root.items()}
    empty_related = {name: values[:0] for name, values in related.items()}
    result = getattr(program._native, method)(
        *PERIOD,
        empty_root,
        _relations_dict(relation_key, offsets, empty_related),
        PERIOD_OUTPUTS,
    )
    assert result["row_count"] == 0


@pytest.mark.parametrize("method", METHODS)
def test_an_unaligned_empty_column_reads_as_no_rows(
    program, relation_key, method
) -> None:
    # An empty array still has a data pointer, and at an odd offset it is
    # misaligned; a zero-length slice over it is as invalid as a long one.
    def empty_at_odd_offset(dtype: str) -> np.ndarray:
        return np.ndarray((0,), dtype=dtype, buffer=bytearray(16), offset=1)

    root = {name: empty_at_odd_offset(v.dtype) for name, v in _root().items()}
    related = {name: empty_at_odd_offset(v.dtype) for name, v in _related().items()}
    offsets = np.ndarray((1,), dtype=np.int64, buffer=bytearray(16), offset=1)
    offsets[0] = 0
    assert not offsets.flags.aligned
    result = getattr(program._native, method)(
        *PERIOD,
        root,
        _relations_dict(relation_key, offsets, related),
        PERIOD_OUTPUTS,
    )
    assert result["row_count"] == 0
    for name in PERIOD_OUTPUTS:
        assert len(result["outputs"][name]) == 0


@pytest.mark.parametrize("method", METHODS)
@pytest.mark.parametrize("layout", list(LAYOUTS))
def test_a_byte_swapped_column_reads_its_values(
    program, relation_key, method, layout
) -> None:
    # A non-native byte order is not the engine's int64 or float64, so these
    # columns convert value by value through numpy's own scalars, in any
    # layout. (A bool has no byte order.)
    def swapped(values: np.ndarray) -> np.ndarray:
        column = LAYOUTS[layout](values.astype(values.dtype.newbyteorder()))
        assert column.tolist() == values.tolist()
        return column

    root = {name: swapped(values) for name, values in _root().items()}
    related = {name: swapped(values) for name, values in _related().items()}
    assert not root["units"].dtype.isnative and not root["amount"].dtype.isnative
    outputs = run(
        program, "native", method, relation_key, root, swapped(OFFSETS), related
    )
    assert_outputs(outputs, {k: np.array(v) for k, v in EXPECTED.items()})


@pytest.mark.parametrize("surface", SURFACES)
@pytest.mark.parametrize("layout", list(LAYOUTS))
def test_lifetime_batches_read_every_layout(
    program, relation_key, surface, layout
) -> None:
    # Two months. Each household and each member is flagged in exactly one
    # of them, in February by bytes other than 1.
    shape = LAYOUTS[layout]
    periods = [
        ("month", "2026-01-01", "2026-01-31"),
        ("month", "2026-02-01", "2026-02-28"),
    ]
    batches = []
    for root_flag, member_flag in (
        ([1, 0, 0], [1, 0, 0, 0, 0]),
        ([0, 2, 255], [0, 128, 2, 3, 4]),
    ):
        root = {
            name: shape(values)
            for name, values in (_root() | {"flag": _bool_column(root_flag)}).items()
        }
        related = {
            name: shape(values)
            for name, values in (
                _related() | {"member_flag": _bool_column(member_flag)}
            ).items()
        }
        batches.append((root, shape(OFFSETS), related))

    if surface == "wrapper":
        result = program.execute_lifetime_f64(
            periods=periods,
            batches=[
                (root, {relation_key: DenseRelationBatch(offsets=o, inputs=rel)})
                for root, o, rel in batches
            ],
            outputs=LIFETIME_OUTPUTS,
        )
    else:
        result = program._native.execute_lifetime_f64(
            periods,
            [
                (root, _relations_dict(relation_key, o, rel))
                for root, o, rel in batches
            ],
            LIFETIME_OUTPUTS,
        )
    outputs = result["outputs"]
    # Household 1 is flagged in January, households 2 and 3 in February:
    # each is unflagged, and paid 100, in the other month.
    np.testing.assert_array_equal(outputs["lifetime_unflagged_payment"], [100] * 3)
    np.testing.assert_array_equal(outputs["lifetime_units"], [6, -14, 22])
    np.testing.assert_array_equal(
        outputs["lifetime_amount"], [3.0, -4.5, 2_000_000.25]
    )
    # Member 1 is flagged in January, members 2 to 5 in February.
    np.testing.assert_array_equal(outputs["lifetime_flagged_members"], [2, 0, 3])
    np.testing.assert_array_equal(outputs["lifetime_member_units"], [6, 0, 24])
    np.testing.assert_array_equal(outputs["lifetime_member_amount"], [1.5, 0.0, 14.0])


# --- Property: layout never changes an output ------------------------------


def _random_layout(rng: np.random.Generator, values: np.ndarray) -> np.ndarray:
    """``values`` at a random byte offset and stride, either direction."""
    itemsize = values.dtype.itemsize
    stride = itemsize * int(rng.integers(1, 4)) + int(rng.integers(0, 8))
    if rng.random() < 0.3:
        stride = -stride
    column = laid_out(values, offset=int(rng.integers(0, 8)), stride=stride)
    if rng.random() < 0.2:
        column.flags.writeable = False
    return column


def test_outputs_do_not_depend_on_layout_on_random_batches(
    program, relation_key
) -> None:
    # Property: for every batch drawn, the outputs from columns in random
    # layouts, with random bool bytes, equal both the outputs from contiguous
    # canonical copies of the same values (differential) and the numpy oracle.
    # Seeded, so a failure reproduces.
    rng = np.random.default_rng(20261014)
    for _ in range(150):
        households = int(rng.integers(0, 7))
        sizes = rng.integers(0, 4, size=households)
        members = int(sizes.sum())
        offsets = np.concatenate([[0], np.cumsum(sizes)]).astype(np.int64)
        root = {
            "flag": rng.integers(0, 256, size=households, dtype=np.uint8).view(
                np.bool_
            ),
            "units": rng.integers(-1_000, 1_000, size=households, dtype=np.int64),
            "amount": rng.integers(-8_000, 8_000, size=households) / 8.0,
        }
        related = {
            "member_flag": rng.integers(0, 256, size=members, dtype=np.uint8).view(
                np.bool_
            ),
            "member_units": rng.integers(-1_000, 1_000, size=members, dtype=np.int64),
            "member_amount": rng.integers(-8_000, 8_000, size=members) / 8.0,
        }
        canonical_root = {
            name: np.array(values.tolist(), dtype=values.dtype)
            for name, values in root.items()
        }
        canonical_related = {
            name: np.array(values.tolist(), dtype=values.dtype)
            for name, values in related.items()
        }
        shaped_root = {name: _random_layout(rng, v) for name, v in root.items()}
        shaped_related = {name: _random_layout(rng, v) for name, v in related.items()}
        shaped_offsets = _random_layout(rng, offsets)
        oracle = expected_outputs(root, offsets, related)
        for method in METHODS:
            reference = run(
                program,
                "native",
                method,
                relation_key,
                canonical_root,
                offsets,
                canonical_related,
            )
            shaped = run(
                program,
                "native",
                method,
                relation_key,
                shaped_root,
                shaped_offsets,
                shaped_related,
            )
            assert_outputs(reference, oracle)
            assert_outputs(shaped, oracle)
