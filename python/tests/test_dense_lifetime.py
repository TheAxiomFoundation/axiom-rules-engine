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

from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)


def _write_rulespec(base: Path, name: str, source: str) -> tuple[Path, Path]:
    root = base / "rulespec-us"
    path = root / "us/policies/tests" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    root = root.resolve()
    path = path.resolve()
    path.write_text(source, encoding="utf-8")
    return path, root

# 42 USC 415(b): AIME is the sum of a worker's highest-35 years of indexed
# earnings divided by 420 (35 years x 12 months), rounded down. Under the strict
# n contract n must be within 1..=the supplied period count; here 40 years are
# supplied and the top 35 drop the lowest 5.
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
    path, root = _write_rulespec(
        tmp_path_factory.mktemp("lifetime"), "aime.yaml", AIME_MODULE
    )
    return CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Worker"
    )


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


# A benefit-computation-year count derived from per-person-constant inputs (the
# years a worker attains 21 and 62), used both as the person-varying `n` of
# sum_top_n_over_periods and as an outer divisor — the exact shape of 42 USC
# 415(b). The count sits outside every reduction and is reached only through a
# derived chain; lifetime execution binds the per-person-constant inputs it
# bottoms out in because they are supplied identically in every period.
DERIVED_N_MODULE = """\
format: rulespec/v1
module:
  summary: |-
    Shape-mirror of 42 USC 415(b): a benefit-computation-year count derived from
    per-person-constant inputs feeds sum_top_n_over_periods as a person-varying n
    and divides the total to an AIME.
rules:
  - name: dropout_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '5'
  - name: minimum_computation_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '2'
  - name: months_per_year
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1979-01-01'
        formula: '12'
  - name: elapsed_years
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          year_attained_62 - max(1950, year_attained_21)
  - name: computation_year_count
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          max(minimum_computation_years, elapsed_years - dropout_years)
  - name: earnings_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          sum_top_n_over_periods(indexed_earnings, computation_year_count)
  - name: aime
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          floor(earnings_total / (months_per_year * computation_year_count))
"""


@pytest.fixture(scope="module")
def derived_n_program(tmp_path_factory) -> CompiledDenseProgram:
    path, root = _write_rulespec(
        tmp_path_factory.mktemp("derived_n"), "aime.yaml", DERIVED_N_MODULE
    )
    return CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Worker"
    )


class TestDerivedPersonVaryingN:
    """Two workers with DIFFERENT derived `n` (36 vs 31) in one batch, mirroring
    the scratchpad acceptance fixture and rulespec-us #541's 415(b) shape.

    Worker A: year21=1985, year62=2026 -> elapsed 41, count max(2, 41-5) = 36.
        Earnings 96_000 x36 then 12_000, 6_000, 0, 0, 0 (41 periods).
        top-36 sum = 36 * 96_000 = 3_456_000.
        aime = floor(3_456_000 / (12*36 = 432)) = floor(8000.0) = 8000.
    Worker B: year21=1990, year62=2026 -> elapsed 36, count max(2, 36-5) = 31.
        Earnings flat 62_000 (41 periods). top-31 sum = 31 * 62_000 = 1_922_000.
        aime = floor(1_922_000 / (12*31 = 372)) = floor(5166.666...) = 5166.
            (Statutory floor per 42 USC 415(b)(2)(A) / 20 CFR 404.211(d); the
             quotient rounds DOWN, so 5166 — not the round-half-up 5167, which no
             integer months divisor can even produce for this total.)
    """

    def test_two_workers_with_different_derived_n(self, derived_n_program) -> None:
        n_periods = 41
        periods = [
            ("calendar_year", f"{1985 + k}-01-01", f"{1985 + k}-12-31")
            for k in range(n_periods)
        ]
        a_earn = [96_000.0] * 36 + [12_000.0, 6_000.0, 0.0, 0.0, 0.0]
        b_earn = [62_000.0] * n_periods
        batches = [
            {
                "indexed_earnings": np.array([a_earn[k], b_earn[k]]),
                "year_attained_21": np.array([1985.0, 1990.0]),
                "year_attained_62": np.array([2026.0, 2026.0]),
            }
            for k in range(n_periods)
        ]
        result = derived_n_program.execute_lifetime_f64(
            periods=periods,
            batches=batches,
            outputs=["earnings_total", "aime"],
        )
        assert result["row_count"] == 2
        np.testing.assert_allclose(
            result["outputs"]["earnings_total"], [3_456_000.0, 1_922_000.0]
        )
        np.testing.assert_allclose(result["outputs"]["aime"], [8_000.0, 5_166.0])

    def test_period_varying_n_raises_under_strict_contract(
        self, derived_n_program
    ) -> None:
        # year_attained_62 feeds computation_year_count, which is the `n` of
        # sum_top_n_over_periods. When that input varies across periods the
        # derived n varies too, so the strict n contract catches it first with a
        # precise "n is not period-invariant" error (36 vs 37) rather than
        # silently pinning n to the reference period. Parameter- and
        # input-sourced n are held to the identical invariance requirement.
        periods = [
            ("calendar_year", "1985-01-01", "1985-12-31"),
            ("calendar_year", "1986-01-01", "1986-12-31"),
        ]
        batches = [
            {
                "indexed_earnings": np.array([50_000.0]),
                "year_attained_21": np.array([1985.0]),
                "year_attained_62": np.array([2026.0]),
            },
            {
                "indexed_earnings": np.array([50_000.0]),
                "year_attained_21": np.array([1985.0]),
                "year_attained_62": np.array([2027.0]),  # varies -> n varies
            },
        ]
        with pytest.raises(RuntimeError, match="n is not period-invariant"):
            derived_n_program.execute_lifetime_f64(
                periods=periods, batches=batches, outputs=["aime"]
            )


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

    def test_descending_periods_raise(self, program) -> None:
        # Supplied periods must be strictly ascending by start date.
        periods = [
            ("calendar_year", "1991-01-01", "1991-12-31"),
            ("calendar_year", "1990-01-01", "1990-12-31"),
        ]
        batches = [
            {"indexed_earnings": np.array([10_000.0])},
            {"indexed_earnings": np.array([20_000.0])},
        ]
        with pytest.raises(RuntimeError, match="strictly ascending"):
            program.execute_lifetime_f64(periods=periods, batches=batches, outputs=["aime"])


def _single_rule_module(name: str, dtype: str, formula: str) -> str:
    return f"""\
format: rulespec/v1
rules:
  - name: {name}
    kind: derived
    entity: Worker
    dtype: {dtype}
    period: Year
    versions:
      - effective_from: '1979-01-01'
        formula: |-
          {formula}
"""


class TestReductionSemantics:
    """Post-review semantics exercised through the exposed f64 lifetime path."""

    def test_count_over_periods_counts_nonzero_per_row(self, tmp_path) -> None:
        # count_over_periods counts, per row, the periods with a nonzero value.
        module = _single_rule_module("years", "Integer", "count_over_periods(earnings)")
        path, root = _write_rulespec(tmp_path, "count.yaml", module)
        program = CompiledDenseProgram.from_file(
            path, rulespec_roots=[root], entity="Worker"
        )
        periods = [
            ("calendar_year", f"{2001 + k}-01-01", f"{2001 + k}-12-31")
            for k in range(4)
        ]
        # Row 0: [0, 20, 0, 40] -> 2 nonzero. Row 1: [9, 0, 7, 6] -> 3 nonzero.
        batches = [
            {"earnings": np.array([0.0, 9.0])},
            {"earnings": np.array([20.0, 0.0])},
            {"earnings": np.array([0.0, 7.0])},
            {"earnings": np.array([40.0, 6.0])},
        ]
        result = program.execute_lifetime_f64(
            periods=periods, batches=batches, outputs=["years"]
        )
        np.testing.assert_allclose(result["outputs"]["years"], [2.0, 3.0])

    def test_sum_top_n_n_exceeds_period_count_raises(self, tmp_path) -> None:
        # Strict n contract in the exposed f64 path: n=45 with 41 periods is out
        # of range (the four extra slots would only pad zeros).
        module = _single_rule_module(
            "top", "Money", "sum_top_n_over_periods(earnings, 45)"
        )
        path, root = _write_rulespec(tmp_path, "topn.yaml", module)
        program = CompiledDenseProgram.from_file(
            path, rulespec_roots=[root], entity="Worker"
        )
        periods = [
            ("calendar_year", f"{1985 + k}-01-01", f"{1985 + k}-12-31")
            for k in range(41)
        ]
        batches = [{"earnings": np.array([1_000.0])} for _ in range(41)]
        with pytest.raises(RuntimeError, match="1 <= n <="):
            program.execute_lifetime_f64(
                periods=periods, batches=batches, outputs=["top"]
            )

    def test_unknown_over_periods_reduction_fails_to_parse(self, tmp_path) -> None:
        # An unknown *_over_periods name fails at parse/compile time.
        module = _single_rule_module("bad", "Money", "avg_over_periods(earnings)")
        path, root = _write_rulespec(tmp_path, "bad.yaml", module)
        with pytest.raises((ValueError, RuntimeError), match="avg_over_periods"):
            CompiledDenseProgram.from_file(
                path, rulespec_roots=[root], entity="Worker"
            )
