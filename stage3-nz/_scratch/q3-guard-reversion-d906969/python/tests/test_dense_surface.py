"""Dense vectorized surface: metadata, Decimal + f64 execution.

These tests exercise the native ``axiom_rules_engine_dense`` extension and
skip when it is not built (CI type-checks the extension crate but does not
build the wheel). Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: |-
    Dense-surface test fixture: a two-bracket flat tax with a boolean
    exemption predicate, exercising metadata, Decimal and f64 execution.
  source_verification:
    corpus_citation_path: us/guidance/tests/dense-flat-tax
    upstream_source_check:
      status: official_parameter_source
      checked_paths:
        - us/statute/tests/dense-flat-tax
      rationale: The synthetic guidance supplies the current test parameters.
units:
  - name: EUR
    kind: currency
    minor_units: 2
rules:
  - name: dense_test_bracket_threshold
    kind: parameter
    dtype: Money
    unit: EUR
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          10000
  - name: dense_test_low_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          0.10
  - name: dense_test_high_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          0.25
  - name: dense_test_tax
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: EUR
    source: dense-surface test fixture
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          if dense_test_is_exempt:
              0
          else:
              if dense_test_income <= dense_test_bracket_threshold:
                  dense_test_income * dense_test_low_rate
              else:
                  dense_test_bracket_threshold * dense_test_low_rate + (dense_test_income - dense_test_bracket_threshold) * dense_test_high_rate
"""


@pytest.fixture(scope="module")
def program(tmp_path_factory) -> CompiledDenseProgram:
    root = tmp_path_factory.mktemp("dense") / "rulespec-us"
    path = root / "us/policies/tests/dense_flat_tax.yaml"
    path.parent.mkdir(parents=True)
    root = root.resolve()
    path = path.resolve()
    path.write_text(MODULE_SOURCE, encoding="utf-8")
    return CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Person"
    )


def test_dense_composed_file_uses_explicit_composition_surface(tmp_path) -> None:
    root = (tmp_path / "rulespec-us").resolve()
    atomic = root / "us/policies/tests/dense_flat_tax.yaml"
    atomic.parent.mkdir(parents=True)
    atomic.write_text(MODULE_SOURCE, encoding="utf-8")
    composition = (tmp_path / "dense-composition.yaml").resolve()
    composition.write_text(
        """\
format: rulespec/v1
module:
  kind: composition
imports:
  - us:policies/tests/dense_flat_tax
rules:
  - name: dense_composed_tax
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: EUR
    versions:
      - effective_from: '2025-01-01'
        formula: dense_test_tax + adjustment
""",
        encoding="utf-8",
    )

    compiled = CompiledDenseProgram.from_composed_file(
        composition,
        rulespec_roots=[root],
        entity="Person",
    )
    assert "dense_test_tax" in compiled.output_names
    assert "dense_composed_tax" in compiled.output_names
    assert any(item.name == "dense_test_tax" for item in compiled.derived_metadata)
    assert compiled.input_catalog["adjustment"] == "adjustment"
    assert (
        compiled.input_catalog["dense_test_income"]
        == "us:policies/tests/dense_flat_tax#input.dense_test_income"
    )
    assert compiled.input_request_names["dense_test_income"] == (
        "us:policies/tests/dense_flat_tax#input.dense_test_income",
    )


class TestDerivedMetadata:
    def test_reports_entity_dtype_period_unit_source(self, program) -> None:
        by_name = {item.name: item for item in program.derived_metadata}
        tax = by_name["dense_test_tax"]
        assert tax.entity == "Person"
        assert tax.dtype == "decimal"
        assert tax.period == "Year"
        assert tax.unit == "EUR"
        assert tax.source == "dense-surface test fixture"
        assert tax.id == "us:policies/tests/dense_flat_tax#dense_test_tax"


class TestExecutionModes:
    def _inputs(self, n: int = 3) -> dict:
        return {
            "dense_test_income": np.array([5_000.0, 10_000.0, 20_000.0][:n]),
            "dense_test_is_exempt": np.array([False, False, False][:n]),
        }

    EXPECTED = [500.0, 1_000.0, 3_500.0]

    def test_decimal_execution(self, program) -> None:
        result = program.execute(
            period_kind="calendar_year",
            start="2025-01-01",
            end="2025-12-31",
            inputs=self._inputs(),
        )
        assert result["row_count"] == 3
        np.testing.assert_allclose(
            result["outputs"]["dense_test_tax"], self.EXPECTED
        )

    def test_f64_execution_matches_decimal(self, program) -> None:
        result = program.execute_f64(
            period_kind="calendar_year",
            start="2025-01-01",
            end="2025-12-31",
            inputs=self._inputs(),
        )
        np.testing.assert_allclose(
            result["outputs"]["dense_test_tax"], self.EXPECTED
        )

    def test_exemption_predicate_takes_bool_columns(self, program) -> None:
        inputs = self._inputs()
        inputs["dense_test_is_exempt"] = np.array([True, False, True])
        result = program.execute(
            period_kind="calendar_year",
            start="2025-01-01",
            end="2025-12-31",
            inputs=inputs,
        )
        np.testing.assert_allclose(
            result["outputs"]["dense_test_tax"], [0.0, 1_000.0, 0.0]
        )
