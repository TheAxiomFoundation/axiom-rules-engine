"""Derived relations that share a base relation's batch through Python.

Every link of a derived-relation chain is keyed to the chain's base relation,
so ``relations`` lists several schemas with one key, each naming the related
inputs its own predicate reads. The caller supplies one batch per key, and
that batch must reach every schema with all of its columns. ``build_batch``
used to keep only the last schema's columns for a key, so a composed filter
failed with a missing input the caller had supplied.

These tests exercise the native ``axiom_rules_engine_dense`` extension and
skip when it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import DenseRelationBatch, NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

# `snap_unit` keeps members with an SSN (it reads `has_ssn`); `adult_snap_unit`
# keeps the adults among those (it reads `age`). Both are keyed to
# `member_of_household`, slots (1, 0).
MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: |-
    Two derived relations over one data relation, each reading a different
    related input: SNAP-unit members with an SSN, and the adults among them.
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

PERIOD = dict(period_kind="month", start="2026-01-01", end="2026-01-31")


@pytest.fixture(scope="module")
def program(tmp_path_factory) -> CompiledDenseProgram:
    root = (tmp_path_factory.mktemp("dense") / "rulespec-us").resolve()
    path = root / "us/policies/tests/shared_relation_key.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(MODULE_SOURCE, encoding="utf-8")
    return CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="AdultSnapUnit"
    )


def _batch(
    program: CompiledDenseProgram, sizes: list[int], columns: dict[str, np.ndarray]
) -> dict[str, DenseRelationBatch]:
    (key,) = {schema.key for schema in program.relations}
    offsets = np.concatenate([[0], np.cumsum(sizes)]).astype(np.int64)
    return {key: DenseRelationBatch(offsets=offsets, inputs=columns)}


def test_every_schema_sharing_a_key_reads_its_own_columns(program) -> None:
    # The premise: two schemas share one key and read different columns.
    schemas = program.relations
    assert len({schema.key for schema in schemas}) == 1
    assert sorted(schema.related_inputs for schema in schemas) == [
        ("age",),
        ("has_ssn",),
    ]

    # h1: p1 (SSN, 30) counts; p2 (SSN, 12) is a child; p3 (no SSN, 40) is
    # outside the SNAP unit. h2: both members count. h3 has no members.
    relations = _batch(
        program,
        [3, 2, 0],
        {
            "has_ssn": np.array([True, True, False, True, True]),
            "age": np.array([30, 12, 40, 18, 70], dtype=np.int64),
        },
    )
    for execute in (program.execute, program.execute_f64):
        result = execute(**PERIOD, inputs={}, relations=relations)
        np.testing.assert_array_equal(
            result["outputs"]["adult_snap_unit_size"], [1, 2, 0]
        )


def test_a_column_no_schema_supplies_is_still_missing(program) -> None:
    # Without `age`, the adult filter fails for the rows that reach it: the
    # union of the schemas' columns does not stand in for an absent one.
    relations = _batch(
        program, [2], {"has_ssn": np.array([True, False])}
    )
    with pytest.raises(RuntimeError, match="missing input `age`"):
        program.execute(**PERIOD, inputs={}, relations=relations)

    # A household whose members all lack an SSN never reaches the adult
    # filter, so the missing `age` column fails no row.
    result = program.execute(
        **PERIOD,
        inputs={},
        relations=_batch(program, [2], {"has_ssn": np.array([False, False])}),
    )
    np.testing.assert_array_equal(result["outputs"]["adult_snap_unit_size"], [0])


@pytest.mark.parametrize("execute_name", ["execute", "execute_f64"])
@pytest.mark.parametrize(
    "related_count", [65536, int(np.iinfo(np.uint32).max), int(np.iinfo(np.int64).max)]
)
def test_malformed_related_count_returns_column_length_error(
    program, execute_name, related_count
) -> None:
    # Offsets declare one household, but its one supplied member cannot fill
    # the declared related row count. Reject the column before allocating
    # owners for that count, including the largest offset Python accepts.
    (key,) = {schema.key for schema in program.relations}
    relations = {
        key: DenseRelationBatch(
            offsets=np.array([0, related_count], dtype=np.int64),
            inputs={
                "has_ssn": np.array([True]),
                "age": np.array([30], dtype=np.int64),
            },
        )
    }
    first_schema = program.relations[0]
    first_input = first_schema.related_inputs[0]
    expected = (
        f"type mismatch: dense relation input `{first_input}` for "
        f"`{first_schema.name}` has length 1 but related row count is "
        f"{related_count}"
    )
    with pytest.raises(RuntimeError) as error:
        getattr(program, execute_name)(**PERIOD, inputs={}, relations=relations)
    assert str(error.value) == expected


@pytest.mark.parametrize("execute_name", ["execute", "execute_f64"])
def test_omitted_related_columns_reject_unbounded_count(program, execute_name) -> None:
    # Missing columns do not establish a related row count. Even the largest
    # offset Python accepts must fail before allocating owners for that count.
    (key,) = {schema.key for schema in program.relations}
    relations = {
        key: DenseRelationBatch(
            offsets=np.array([0, np.iinfo(np.int64).max], dtype=np.int64),
            inputs={},
        )
    }
    with pytest.raises(
        RuntimeError, match="type mismatch: dense relation.*related row count"
    ):
        getattr(program, execute_name)(**PERIOD, inputs={}, relations=relations)


def test_counts_match_the_rules_on_random_households(program) -> None:
    # Property: for every batch, a household's adult SNAP unit is its members
    # with an SSN who are at least 18. Seeded, so a failure reproduces.
    rng = np.random.default_rng(20260927)
    for _ in range(25):
        households = int(rng.integers(1, 12))
        sizes = rng.integers(0, 6, size=households)
        members = int(sizes.sum())
        has_ssn = rng.random(members) < 0.7
        age = rng.integers(0, 90, size=members).astype(np.int64)
        relations = _batch(
            program, sizes.tolist(), {"has_ssn": has_ssn, "age": age}
        )
        owners = np.repeat(np.arange(households), sizes)
        expected = np.bincount(
            owners[has_ssn & (age >= 18)], minlength=households
        )
        for execute in (program.execute, program.execute_f64):
            result = execute(**PERIOD, inputs={}, relations=relations)
            np.testing.assert_array_equal(
                result["outputs"]["adult_snap_unit_size"], expected
            )
