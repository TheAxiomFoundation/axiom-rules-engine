"""Lifetime (cross-period reduction) surface: ``execute_lifetime_f64``.

The acceptance test from issue #67: compile a minimal inline rulespec module
whose ``aime`` rule is

    aime = floor(sum_top_n_over_periods(indexed_earnings, 35) / 420)

feed a synthetic worker with a 40-year history through
``execute_lifetime_f64``, and assert the AIME equals the hand-derived value.

These tests exercise the native ``axiom_rules_engine_dense`` extension and skip
when it is not built (CI type-checks the extension crate but does not build the
wheel). Build locally with::

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

# 42 USC 415(b): AIME is the sum of a worker's highest-35 years of indexed
# earnings divided by 420 (35 years x 12 months), rounded down. The computation
# years count whether or not the worker had earnings, so sum_top_n zero-pads
# missing years — here there are 40 years, so the lowest 5 are dropped instead.
AIME_MODULE = """\
format: rulespec/v1
module:
  summary: |-
    Synthetic 42 USC 415(b) AIME: highest-35-years selection over a worker's own
    earnings history. Acceptance fixture for cross-period reductions (#67).
rules:
  - name: computation_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1960-01-01'
        formula: '35'
  - name: aime_divisor
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1960-01-01'
        formula: '420'
  - name: aime
    kind: derived
    entity: Worker
    dtype: Money
    unit: USD
    period: Year
    source: 42 USC 415(b)
    versions:
      - effective_from: '1960-01-01'
        formula: |-
          floor(sum_top_n_over_periods(indexed_earnings, computation_years) / aime_divisor)
"""


@pytest.fixture(scope="module")
def program(tmp_path_factory) -> CompiledDenseProgram:
    path = tmp_path_factory.mktemp("lifetime") / "aime.yaml"
    path.write_text(AIME_MODULE, encoding="utf-8")
    return CompiledDenseProgram.from_file(path, entity="Worker")


def _aime_worker() -> tuple[list[tuple[str, str, str]], list[dict[str, np.ndarray]]]:
    """A synthetic 40-year worker with a clean, hand-derivable AIME.

    Year k (k = 1..40) earns 12_000 + 1_000 * (k - 1):
        12_000, 13_000, ..., 51_000.
    The highest 35 of 40 drop the five smallest (12_000..16_000), keeping the
    arithmetic series 17_000, 18_000, ..., 51_000 (35 terms):
        sum(top 35) = 35 * (17_000 + 51_000) / 2 = 35 * 34_000 = 1_190_000
        AIME        = floor(1_190_000 / 420) = floor(2833.33...) = 2833
    """
    periods = [
        ("calendar_year", f"{1980 + k}-01-01", f"{1980 + k}-12-31") for k in range(1, 41)
    ]
    batches = [
        {"indexed_earnings": np.array([12_000.0 + 1_000.0 * (k - 1)])} for k in range(1, 41)
    ]
    return periods, batches


class TestAimeAcceptance:
    EXPECTED_AIME = 2833.0  # hand-derived in _aime_worker()

    def test_forty_year_worker_aime(self, program) -> None:
        periods, batches = _aime_worker()
        result = program.execute_lifetime_f64(
            periods=periods, batches=batches, outputs=["aime"]
        )
        assert result["row_count"] == 1
        np.testing.assert_allclose(result["outputs"]["aime"], [self.EXPECTED_AIME])

    def test_two_workers_align_by_row(self, program) -> None:
        # Two workers side by side. Worker 0 is the reference 40-year worker;
        # worker 1 earns a flat 20_000 every year, so its top 35 = 35 * 20_000 =
        # 700_000 and AIME = floor(700_000 / 420) = floor(1666.66...) = 1666.
        periods = [
            ("calendar_year", f"{1980 + k}-01-01", f"{1980 + k}-12-31") for k in range(1, 41)
        ]
        batches = [
            {"indexed_earnings": np.array([12_000.0 + 1_000.0 * (k - 1), 20_000.0])}
            for k in range(1, 41)
        ]
        result = program.execute_lifetime_f64(
            periods=periods, batches=batches, outputs=["aime"]
        )
        assert result["row_count"] == 2
        np.testing.assert_allclose(result["outputs"]["aime"], [2833.0, 1666.0])


class TestLifetimeErrors:
    def test_period_and_batch_count_mismatch_raises(self, program) -> None:
        periods = [("calendar_year", "1990-01-01", "1990-12-31")]
        batches: list[dict[str, np.ndarray]] = []
        with pytest.raises(ValueError, match="one batch per period"):
            program.execute_lifetime_f64(periods=periods, batches=batches, outputs=["aime"])

    def test_row_count_mismatch_raises(self, program) -> None:
        periods = [
            ("calendar_year", "1990-01-01", "1990-12-31"),
            ("calendar_year", "1991-01-01", "1991-12-31"),
        ]
        batches = [
            {"indexed_earnings": np.array([10_000.0, 20_000.0])},
            {"indexed_earnings": np.array([10_000.0])},
        ]
        with pytest.raises(RuntimeError, match="same entity row count"):
            program.execute_lifetime_f64(periods=periods, batches=batches, outputs=["aime"])
