"""A formula string may escape a multibyte character.

The RuleSpec lexer once decoded only the first UTF-8 byte of an escaped
character and then panicked on its next slice of the source. Through the
native extension that surfaced as ``pyo3_runtime.PanicException``, a
``BaseException`` that ``except Exception`` does not catch. Compile errors
must instead arrive as ``ValueError``.
"""

import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

LABEL_FORMULA = '"caf\\é"'

MODULE_SOURCE = f"""\
format: rulespec/v1
rules:
  - name: escape_test_label
    kind: parameter
    dtype: Text
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          {LABEL_FORMULA}
  - name: escape_test_tax
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          escape_test_income * 0.1
"""


def _compile(tmp_path, source: str) -> CompiledDenseProgram:
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/escape.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(source, encoding="utf-8")
    return CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Person")


def test_multibyte_escape_compiles(tmp_path) -> None:
    compiled = _compile(tmp_path, MODULE_SOURCE)
    assert "escape_test_tax" in compiled.output_names


def test_unterminated_multibyte_escape_is_a_value_error(tmp_path) -> None:
    unterminated = MODULE_SOURCE.replace(LABEL_FORMULA, LABEL_FORMULA[:-1])
    with pytest.raises(ValueError, match="unterminated string"):
        _compile(tmp_path, unterminated)
