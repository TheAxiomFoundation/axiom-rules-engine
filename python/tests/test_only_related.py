"""Actual native dense wrapper: Bool projection and fail-closed cardinality."""
from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram, DenseRelationBatch


@pytest.fixture
def program(tmp_path):
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/only_related.yaml"
    path.parent.mkdir(parents=True)
    fixture = Path(__file__).resolve().parents[2] / "tests/fixtures/only_related.yaml"
    path.write_bytes(fixture.read_bytes())
    return CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Determination")


def test_actual_dense_boolean_and_cardinality(program):
    period = dict(period_kind="month", start="2026-01-01", end="2026-01-31")
    key = program.relations[0].key
    for execute in (program.execute, program.execute_f64):
        result = execute(**period, inputs={}, relations={key: DenseRelationBatch(
            offsets=np.array([0, 1, 2], dtype=np.int64),
            inputs={"decision": np.array([True, False])},
        )})
        np.testing.assert_array_equal(result["outputs"]["projected"], [True, False])
        assert result["outputs"]["projected"].dtype == np.bool_
        for count, message in [(0, "missing related entity"), (2, "ambiguous relation")]:
            with pytest.raises(Exception, match=message):
                execute(**period, inputs={}, relations={key: DenseRelationBatch(
                    offsets=np.array([0, count], dtype=np.int64),
                    inputs={"decision": np.array([False] * count, dtype=np.bool_)},
                )})
        with pytest.raises(Exception, match="only_related requires a Boolean input"):
            execute(**period, inputs={}, relations={key: DenseRelationBatch(
                offsets=np.array([0, 1], dtype=np.int64),
                inputs={"decision": np.array([1], dtype=np.int64)},
            )})
