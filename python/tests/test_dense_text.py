"""Dense text outputs share their values, as the engine does.

The engine fills a text column with one shared value per distinct text, not
a copy per row: a literal broadcast to every row, or a parameter text looked
up by many rows, is one allocation. The extension keeps that sharing: each
shared text becomes one Python string, referenced by every row that holds
it. Texts the caller supplied row by row come back with the same values.
"""

from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

MONTH = {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"}

# Longer than a page, so a copy per row would be visible in memory.
LONG = "household-label-" * 4096

TEXT_MODULE = f"""\
format: rulespec/v1
rules:
  - name: label_copy
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: label
  - name: long_label
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: |-
          "{LONG}"
"""


def _compile(tmp_path: Path) -> CompiledDenseProgram:
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/text.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(TEXT_MODULE, encoding="utf-8")
    return CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Person")


@pytest.mark.parametrize("method", ["execute", "execute_f64"])
def test_a_broadcast_text_is_one_python_string(tmp_path, method) -> None:
    program = _compile(tmp_path)
    labels = np.array(["a", "b", "a"] * 400)
    result = getattr(program, method)(
        **MONTH, inputs={"label": labels}, outputs=["long_label", "label_copy"]
    )
    long_label = result["outputs"]["long_label"]
    assert long_label == [LONG] * len(labels)
    # Every row references the one string the engine shared among them.
    assert len({id(value) for value in long_label}) == 1
    assert result["outputs"]["label_copy"] == labels.tolist()


def test_supplied_texts_round_trip(tmp_path) -> None:
    program = _compile(tmp_path)
    labels = ["", "a", "é", "a\nb", LONG]
    result = program.execute(**MONTH, inputs={"label": labels}, outputs=["label_copy"])
    assert result["outputs"]["label_copy"] == labels
