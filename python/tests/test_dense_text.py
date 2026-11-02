"""Dense text columns through the native extension.

The engine shares a text value among the rows that hold it rather than
copying it per row. Python sees no difference: a text column goes in as a
string array or list and comes back as a list of ``str``, value for value.

These tests exercise the native ``axiom_rules_engine_dense`` extension and skip
when it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
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

TEXT_MODULE = """\
format: rulespec/v1
rules:
  - name: label_copy
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: label
  - name: greeting
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: '"café \\"quoted\\""'
  - name: chosen_label
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: |-
          if label == "a": greeting
          else: label
"""


def _compile(tmp_path: Path) -> CompiledDenseProgram:
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/text.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(TEXT_MODULE, encoding="utf-8")
    return CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Person")


@pytest.mark.parametrize("as_list", [False, True])
@pytest.mark.parametrize("mode", ["execute", "execute_f64"])
def test_text_round_trips_value_for_value(tmp_path, as_list, mode) -> None:
    program = _compile(tmp_path)
    long = "x" * 100_000
    labels = ["a", long, "", "a", long, "é\n\"\\"]
    column = labels if as_list else np.array(labels)
    result = getattr(program, mode)(**MONTH, inputs={"label": column})
    outputs = result["outputs"]
    assert result["row_count"] == len(labels)
    assert outputs["label_copy"] == labels
    assert all(type(value) is str for value in outputs["label_copy"])
    greeting = 'café "quoted"'
    assert outputs["greeting"] == [greeting] * len(labels)
    assert outputs["chosen_label"] == [
        greeting if label == "a" else label for label in labels
    ]
