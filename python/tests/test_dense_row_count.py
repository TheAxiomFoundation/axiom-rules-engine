"""The row count a Python caller states for a dense batch.

``DenseBatchSpec::row_count`` is the caller's statement of how many root rows a
batch has. Python callers used to have no way to make it: the extension took
it from the first supplied root input column, else from the first relation's
offsets, else raised "dense compilation produced neither root inputs nor
relations". So an input-free program could never run, and a program called
without its root input (or with the key misspelt) got that false message
instead of the missing input.

``execute`` and ``execute_f64`` now take ``row_count=``, and a lifetime batch
may be an ``(inputs, relations, row_count)`` tuple. A stated count must agree
with every supplied root column and with the relations' offsets; the engine
checks both, and refuses a count above ``MAX_DENSE_ROWS``. When no column,
relation or stated count gives a row count, the call raises and names the
program's root inputs. It never defaults to zero rows: a misspelt key would
then return an empty result with no error.

These tests exercise the native ``axiom_rules_engine_dense`` extension and skip
when it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

from typing import Any, Callable

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram
from axiom_rules_engine.dense import DenseRelationBatch, NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

MONTH = {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"}
YEARS = [
    ("year", "2024-01-01", "2024-12-31"),
    ("year", "2025-01-01", "2025-12-31"),
]

# No root input and no relation: nothing in a batch gives its row count.
FIVE_MODULE = """\
format: rulespec/v1
rules:
  - name: five
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: '5'
"""

DOUBLED_MODULE = """\
format: rulespec/v1
rules:
  - name: doubled
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: rent * 2
"""

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

LIFETIME_FIVE_MODULE = """\
format: rulespec/v1
rules:
  - name: total
    kind: derived
    entity: Worker
    dtype: Integer
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: sum_over_periods(5)
"""

LIFETIME_EARNINGS_MODULE = """\
format: rulespec/v1
rules:
  - name: total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: sum_over_periods(earnings)
"""

# The message build_batch used to raise for both problems.
OLD_MESSAGE = "neither root inputs nor relations"


Compile = Callable[[str, str], CompiledDenseProgram]


@pytest.fixture(scope="session")
def compile_program(tmp_path_factory) -> Compile:
    """Compile a module for an entity once per session; execution does not
    change a compiled program, so tests share it."""
    compiled: dict[tuple[str, str], CompiledDenseProgram] = {}

    def compile_(source: str, entity: str) -> CompiledDenseProgram:
        if (source, entity) not in compiled:
            root = (tmp_path_factory.mktemp("row_count") / "rulespec-us").resolve()
            path = root / "us/policies/tests/row_count.yaml"
            path.parent.mkdir(parents=True)
            path.write_text(source, encoding="utf-8")
            compiled[source, entity] = CompiledDenseProgram.from_file(
                path, rulespec_roots=[root], entity=entity
            )
        return compiled[source, entity]

    return compile_


Runner = Callable[..., dict[str, Any]]


def _stated(row_count) -> dict[str, Any]:
    # An unstated count is left out, so the default path is what runs.
    return {} if row_count is None else {"row_count": row_count}


def _wrapper(method: str) -> Runner:
    def run(program, inputs, relations=None, row_count=None):
        return getattr(program, method)(
            **MONTH, inputs=inputs, relations=relations, **_stated(row_count)
        )

    return run


def _native(method: str) -> Runner:
    # The extension's own surface, which takes the relation dicts the wrapper
    # prepares.
    def run(program, inputs, relations=None, row_count=None):
        prepared = None
        if relations is not None:
            prepared = {
                key: {
                    "offsets": np.asarray(batch.offsets),
                    "inputs": {k: np.asarray(v) for k, v in batch.inputs.items()},
                    "related_row_count": batch.related_row_count,
                }
                for key, batch in relations.items()
            }
        return getattr(program._native, method)(
            MONTH["period_kind"],
            MONTH["start"],
            MONTH["end"],
            {name: np.asarray(values) for name, values in inputs.items()},
            prepared,
            None,
            **_stated(row_count),
        )

    return run


RUNNERS = {
    "execute": _wrapper("execute"),
    "execute_f64": _wrapper("execute_f64"),
    "native.execute": _native("execute"),
    "native.execute_f64": _native("execute_f64"),
}


@pytest.fixture(params=list(RUNNERS), ids=list(RUNNERS))
def run(request) -> Runner:
    return RUNNERS[request.param]


def _only_relation_key(program: CompiledDenseProgram) -> str:
    keys = {relation.key for relation in program.relations}
    assert len(keys) == 1, keys
    return keys.pop()


# --- Problem 1: an input-free program could never run from Python. ---


@pytest.mark.parametrize("row_count", [0, 1, 3])
def test_an_input_free_program_runs_for_a_stated_row_count(
    compile_program, run, row_count
) -> None:
    program = compile_program(FIVE_MODULE, "Household")
    assert program.root_inputs == [] and program.relations == []

    result = run(program, {}, row_count=row_count)

    assert result["row_count"] == row_count
    np.testing.assert_array_equal(result["outputs"]["five"], [5] * row_count)


PARAMETER_MODULE = """\
format: rulespec/v1
rules:
  - name: computation_years
    kind: parameter
    dtype: Integer
    versions:
      - effective_from: '1960-01-01'
        formula: '35'
  - name: years
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: computation_years
"""


def test_a_parameter_only_program_runs_for_a_stated_row_count(
    compile_program, run
) -> None:
    program = compile_program(PARAMETER_MODULE, "Household")
    assert program.root_inputs == [] and program.relations == []

    result = run(program, {}, row_count=2)

    np.testing.assert_array_equal(result["outputs"]["years"], [35, 35])
    with pytest.raises(ValueError, match="reads no root inputs and no relations"):
        run(program, {})


def test_an_input_free_program_without_a_row_count_says_to_state_one(
    compile_program, run
) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(ValueError) as excinfo:
        run(program, {})

    message = str(excinfo.value)
    assert OLD_MESSAGE not in message
    assert "reads no root inputs and no relations" in message
    assert "pass row_count" in message


def test_columns_an_input_free_program_does_not_read_give_no_row_count(
    compile_program, run
) -> None:
    # A column the program never reads is ignored, so it cannot be what sizes
    # the result; the error names it so a caller sees it was not used.
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(ValueError) as excinfo:
        run(program, {"zeta": np.array([1.0, 2.0]), "alpha": np.array([3.0, 4.0])})

    message = str(excinfo.value)
    assert "pass row_count" in message
    assert "not root inputs: `alpha`, `zeta`" in message


# --- Problem 2: a missing or misspelt root input got a false message. ---


def test_a_missing_root_input_is_named(compile_program, run) -> None:
    program = compile_program(DOUBLED_MODULE, "Household")

    with pytest.raises(ValueError) as excinfo:
        run(program, {})

    message = str(excinfo.value)
    assert OLD_MESSAGE not in message
    assert "root inputs (`rent`)" in message
    assert "pass row_count" in message
    assert "not root inputs" not in message


def test_a_misspelt_root_input_is_refused_not_read_as_zero_rows(
    compile_program, run
) -> None:
    # Defaulting to zero rows would return an empty result here, with no error,
    # because no row would ever read `rent`.
    program = compile_program(DOUBLED_MODULE, "Household")

    with pytest.raises(ValueError) as excinfo:
        run(program, {"rnet": np.array([1000.0, 2000.0])})

    message = str(excinfo.value)
    assert OLD_MESSAGE not in message
    assert "root inputs (`rent`)" in message
    assert "not root inputs: `rnet`" in message


def test_with_a_stated_row_count_the_missing_input_is_reported(
    compile_program, run
) -> None:
    # Since #201 an absent column is a missing input on every row, reported
    # by the first row that reads it.
    program = compile_program(DOUBLED_MODULE, "Household")

    with pytest.raises(RuntimeError, match="missing input `rent`"):
        run(program, {}, row_count=2)


def test_a_stated_zero_rows_reads_no_input(compile_program, run) -> None:
    # Zero rows is allowed when the caller says so: no row reads `rent`.
    program = compile_program(DOUBLED_MODULE, "Household")

    result = run(program, {}, row_count=0)

    assert result["row_count"] == 0
    assert len(result["outputs"]["doubled"]) == 0


# --- A stated count must agree with every supplied column and offsets. ---


def test_a_stated_row_count_that_agrees_changes_nothing(compile_program, run) -> None:
    program = compile_program(DOUBLED_MODULE, "Household")
    rent = np.array([100.0, 250.5, 0.0])

    inferred = run(program, {"rent": rent})
    stated = run(program, {"rent": rent}, row_count=3)

    assert inferred["row_count"] == stated["row_count"] == 3
    np.testing.assert_array_equal(
        inferred["outputs"]["doubled"], stated["outputs"]["doubled"]
    )
    np.testing.assert_array_equal(stated["outputs"]["doubled"], rent * 2)


@pytest.mark.parametrize("row_count", [0, 2, 4])
def test_a_stated_row_count_that_disagrees_with_a_column_is_refused(
    compile_program, run, row_count
) -> None:
    program = compile_program(DOUBLED_MODULE, "Household")

    with pytest.raises(
        RuntimeError,
        match=f"dense root input `rent` has length 3 but row_count is {row_count}",
    ):
        run(program, {"rent": np.array([1.0, 2.0, 3.0])}, row_count=row_count)


def test_a_stated_row_count_must_agree_with_the_offsets(compile_program, run) -> None:
    program = compile_program(HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    relations = {
        key: DenseRelationBatch(
            offsets=np.array([0, 2, 3]), inputs={}, related_row_count=3
        )
    }

    result = run(program, {}, relations, row_count=2)
    np.testing.assert_array_equal(result["outputs"]["household_size"], [2, 1])

    with pytest.raises(
        RuntimeError, match=r"offsets must have length 4 \(row_count \+ 1\) but has 3"
    ):
        run(program, {}, relations, row_count=3)


def test_a_relation_still_gives_the_row_count_when_none_is_stated(
    compile_program, run
) -> None:
    program = compile_program(HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)
    relations = {
        key: DenseRelationBatch(
            offsets=np.array([0, 2, 3]), inputs={}, related_row_count=3
        )
    }

    result = run(program, {}, relations)

    assert result["row_count"] == 2


def test_a_relation_program_needs_its_relation_batch_even_with_a_count(
    compile_program, run
) -> None:
    program = compile_program(HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)

    for row_count in (None, 2):
        with pytest.raises(ValueError, match=f"missing dense relation batch `{key}`"):
            run(program, {}, None, row_count=row_count)


# --- The stated count itself. ---


@pytest.mark.parametrize("row_count", [-1, -(2**63), -(2**70), -(2**200)])
def test_a_negative_row_count_is_refused(compile_program, run, row_count) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(ValueError, match="row_count must be non-negative"):
        run(program, {}, row_count=row_count)


@pytest.mark.parametrize(
    "row_count", [2.0, "2", True, False, np.True_, np.float64(2.0), [2]]
)
def test_a_row_count_that_is_not_an_integer_is_refused(
    compile_program, run, row_count
) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(TypeError, match="row_count must be an integer, not"):
        run(program, {}, row_count=row_count)


@pytest.mark.parametrize(
    "row_count", [np.int64(2), np.uint32(2), np.int8(2), np.array(2)]
)
def test_a_numpy_integer_row_count_is_accepted(compile_program, run, row_count) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    assert run(program, {}, row_count=row_count)["row_count"] == 2


@pytest.mark.parametrize("row_count", [2**60, 2**63 - 1, 2**63, 2**64 - 1])
def test_the_engine_bound_refuses_a_huge_row_count(
    compile_program, run, row_count
) -> None:
    # MAX_DENSE_ROWS (#255) is the arbiter: every count that fits a usize
    # reaches the engine, which refuses one above the bound before allocating.
    # A pyo3 PanicException is a BaseException, so it would fail this test.
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(RuntimeError, match=f"row_count {row_count} exceeds"):
        run(program, {}, row_count=row_count)


@pytest.mark.parametrize("row_count", [2**64, 2**70, 2**200])
def test_a_row_count_past_usize_overflows(compile_program, run, row_count) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(OverflowError, match="row_count"):
        run(program, {}, row_count=row_count)


def test_a_stated_related_row_count_is_read_the_same_way(compile_program, run) -> None:
    # `related_row_count` shares the extraction: a bool is refused, and a
    # count past i64::MAX reaches the engine rather than overflowing here.
    program = compile_program(HOUSEHOLD_SIZE_MODULE, "Household")
    key = _only_relation_key(program)

    def relations(related_row_count):
        return {
            key: DenseRelationBatch(
                offsets=np.array([0, 2, 3]),
                inputs={},
                related_row_count=related_row_count,
            )
        }

    with pytest.raises(
        TypeError, match="related_row_count must be an integer, not bool"
    ):
        run(program, {}, relations(True))
    with pytest.raises(
        ValueError, match="related_row_count must be non-negative, got -1"
    ):
        run(program, {}, relations(-1))
    with pytest.raises(
        RuntimeError, match=f"offsets end at 3 but related_row_count is {2**63}"
    ):
        run(program, {}, relations(2**63))
    with pytest.raises(OverflowError, match="related_row_count"):
        run(program, {}, relations(2**64))


def test_the_native_row_count_is_keyword_only(compile_program) -> None:
    program = compile_program(FIVE_MODULE, "Household")

    with pytest.raises(TypeError, match="positional argument"):
        program._native.execute(
            MONTH["period_kind"], MONTH["start"], MONTH["end"], {}, None, None, 3
        )


# --- Lifetime: each batch may state its own row count. ---


def _lifetime_runners() -> dict[str, Callable[..., dict[str, Any]]]:
    def wrapper(program, batches):
        return program.execute_lifetime_f64(
            periods=YEARS, batches=batches, outputs=["total"]
        )

    def native(program, batches):
        return program._native.execute_lifetime_f64(YEARS, batches, ["total"])

    return {"wrapper": wrapper, "native": native}


LIFETIME_RUNNERS = _lifetime_runners()


@pytest.fixture(params=list(LIFETIME_RUNNERS), ids=list(LIFETIME_RUNNERS))
def run_lifetime(request) -> Callable[..., dict[str, Any]]:
    return LIFETIME_RUNNERS[request.param]


def test_an_input_free_lifetime_program_runs_for_stated_row_counts(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_FIVE_MODULE, "Worker")

    result = run_lifetime(program, [({}, None, 3), [{}, None, 3]])

    assert result["row_count"] == 3
    np.testing.assert_array_equal(result["outputs"]["total"], [10.0, 10.0, 10.0])


def test_a_lifetime_batch_without_a_row_count_is_named(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_FIVE_MODULE, "Worker")

    for batches, index in (([{}, {}], 0), ([({}, None, 2), ({}, None)], 1)):
        with pytest.raises(ValueError) as excinfo:
            run_lifetime(program, batches)
        message = str(excinfo.value)
        assert OLD_MESSAGE not in message
        assert f"lifetime batch {index} has" in message
        assert "(inputs, relations, row_count)" in message


def test_a_lifetime_batch_missing_its_root_input_names_it(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_EARNINGS_MODULE, "Worker")

    with pytest.raises(ValueError) as excinfo:
        run_lifetime(
            program, [{"earnings": np.array([1.0])}, {"earning": np.array([2.0])}]
        )

    message = str(excinfo.value)
    assert "lifetime batch 1 has" in message
    assert "root inputs (`earnings`)" in message
    assert "not root inputs: `earning`" in message


def test_lifetime_stated_row_counts_must_agree_across_batches(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_FIVE_MODULE, "Worker")

    with pytest.raises(RuntimeError, match="period 1 has 3 rows but period 0 has 2"):
        run_lifetime(program, [({}, None, 2), ({}, None, 3)])


def test_a_lifetime_stated_row_count_must_agree_with_its_columns(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_EARNINGS_MODULE, "Worker")
    earnings = np.array([10.0, 20.0])

    result = run_lifetime(
        program, [({"earnings": earnings}, None, 2), {"earnings": earnings}]
    )
    np.testing.assert_array_equal(result["outputs"]["total"], [20.0, 40.0])

    with pytest.raises(
        RuntimeError,
        match="dense root input `earnings` has length 2 but row_count is 3",
    ):
        run_lifetime(
            program, [({"earnings": earnings}, None, 3), {"earnings": earnings}]
        )


def test_a_lifetime_stated_row_count_is_checked_like_execute(
    compile_program, run_lifetime
) -> None:
    program = compile_program(LIFETIME_FIVE_MODULE, "Worker")

    with pytest.raises(ValueError, match="row_count must be non-negative"):
        run_lifetime(program, [({}, None, -1), ({}, None, -1)])
    with pytest.raises(
        TypeError, match="lifetime batch 0 row_count must be an integer, not float"
    ):
        run_lifetime(program, [({}, None, 2.0), ({}, None, 2.0)])
    with pytest.raises(RuntimeError, match=f"row_count {2**62} exceeds"):
        run_lifetime(program, [({}, None, 2**62), ({}, None, 2**62)])


@pytest.mark.parametrize("batch", [(), ({}, None, 2, None)])
def test_a_lifetime_batch_of_the_wrong_shape_is_refused(
    compile_program, run_lifetime, batch
) -> None:
    program = compile_program(LIFETIME_FIVE_MODULE, "Worker")

    with pytest.raises(
        ValueError, match=r"\(inputs\[, relations\[, row_count\]\]\) tuple"
    ):
        run_lifetime(program, [batch, batch])


# --- Properties, checked exhaustively over small batches. ---


@pytest.mark.parametrize("column_rows", [None, 0, 1, 2, 3])
@pytest.mark.parametrize("stated", [None, 0, 1, 2, 3, 4])
@pytest.mark.parametrize("extra", [False, True])
def test_every_result_row_count_is_one_the_caller_gave(
    compile_program, run, column_rows, stated, extra
) -> None:
    """For `doubled = rent * 2`, over every small combination of a `rent`
    column (or none), a stated count (or none), and an unread extra column:

    - the call succeeds exactly when something gives the row count and every
      such source agrees;
    - a successful call has that row count, never a default, and its output
      equals `2 * rent` row for row (differential against numpy);
    - a stated count that agrees with the column gives the same result as no
      stated count;
    - with no `rent` column, the only rows that can succeed are zero stated
      rows, and any other stated count fails on the missing input.
    """
    program = compile_program(DOUBLED_MODULE, "Household")
    inputs: dict[str, np.ndarray] = {}
    rent = None
    if column_rows is not None:
        rent = np.arange(column_rows, dtype=float) * 7.5
        inputs["rent"] = rent
    if extra:
        inputs["unread"] = np.ones(5)

    sources = [count for count in (stated, column_rows) if count is not None]
    if not sources:
        with pytest.raises(ValueError, match="pass row_count"):
            run(program, inputs, row_count=stated)
        return
    if len(set(sources)) > 1:
        with pytest.raises(RuntimeError, match="dense root input `rent` has length"):
            run(program, inputs, row_count=stated)
        return
    expected_rows = sources[0]
    if rent is None and expected_rows > 0:
        with pytest.raises(RuntimeError, match="missing input `rent`"):
            run(program, inputs, row_count=stated)
        return

    result = run(program, inputs, row_count=stated)

    assert result["row_count"] == expected_rows
    expected = rent * 2 if rent is not None else np.zeros(0)
    np.testing.assert_array_equal(result["outputs"]["doubled"], expected)
    if stated is not None and rent is not None:
        unstated = run(program, inputs)
        np.testing.assert_array_equal(unstated["outputs"]["doubled"], expected)


@pytest.mark.parametrize(
    "row_count",
    [
        -(2**200),
        -(2**70),
        -(2**64),
        -(2**63),
        -1,
        0,
        1,
        2,
        63,
        2**60,
        2**63 - 1,
        2**63,
        2**64 - 1,
        2**64,
        2**70,
        2**200,
    ],
)
def test_every_integer_row_count_is_accepted_or_refused_by_class(
    compile_program, run, row_count
) -> None:
    """The partition of integer row counts, by class representative: negative
    counts raise ValueError; counts past a 64-bit usize raise OverflowError;
    counts above the engine bound raise the engine's RuntimeError; and the
    rest run with exactly that many rows. Counts from a few thousand up to
    the bound are left out: the engine accepts them and allocates one value
    per row, so one larger than memory aborts the process (the per-row
    executor buffers #255 leaves infallible)."""
    program = compile_program(FIVE_MODULE, "Household")
    _check_row_count_class(program, run, row_count)


def _check_row_count_class(
    program: CompiledDenseProgram, run: Runner, row_count: int
) -> None:
    if row_count < 0:
        with pytest.raises(ValueError, match="row_count must be non-negative"):
            run(program, {}, row_count=row_count)
    elif row_count >= 2**64:
        with pytest.raises(OverflowError, match="row_count"):
            run(program, {}, row_count=row_count)
    elif row_count >= 2**60:
        with pytest.raises(RuntimeError, match=f"row_count {row_count} exceeds"):
            run(program, {}, row_count=row_count)
    else:
        result = run(program, {}, row_count=row_count)
        assert result["row_count"] == row_count
        np.testing.assert_array_equal(result["outputs"]["five"], [5] * row_count)


def test_integer_row_counts_property(compile_program) -> None:
    """The same partition over Hypothesis-generated integers (skipped when
    Hypothesis is not installed)."""
    hypothesis = pytest.importorskip("hypothesis")
    strategies = pytest.importorskip("hypothesis.strategies")
    program = compile_program(FIVE_MODULE, "Household")

    counts = strategies.one_of(
        strategies.integers(max_value=-1),
        strategies.integers(min_value=0, max_value=64),
        strategies.integers(min_value=2**60, max_value=2**64 - 1),
        strategies.integers(min_value=2**64),
    )

    @hypothesis.settings(max_examples=300, deadline=None, database=None)
    @hypothesis.given(
        row_count=counts, runner=strategies.sampled_from(list(RUNNERS.values()))
    )
    def check(row_count: int, runner: Runner) -> None:
        _check_row_count_class(program, runner, row_count)

    check()
