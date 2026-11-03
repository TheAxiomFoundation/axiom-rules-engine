"""Dense vectorized surface: metadata, Decimal + f64 execution.

These tests exercise the native ``axiom_rules_engine_dense`` extension and
skip when it is not built (CI type-checks the extension crate but does not
build the wheel). Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml
"""

import tempfile
from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import CompiledDenseProgram, DerivedMetadata
from axiom_rules_engine.concepts import discover_concepts
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


# Derived rules on two entities, joined the way the other dense fixtures join
# them: a rule on Person, and two rules on Household that read the household's
# members through `member_of_household`. A dense program compiles one entity's
# rules; `derived_metadata` must still describe all three.
TWO_ENTITY_MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: |-
    Dense-surface test fixture: derived rules on a person and on the
    household the person belongs to.
units:
  - name: EUR
    kind: currency
    minor_units: 2
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: dense_test_net_income
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: EUR
    source: dense-surface two-entity fixture
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          dense_test_gross_income - dense_test_deductions
  - name: dense_test_household_net_income
    kind: derived
    entity: Household
    dtype: Money
    period: Year
    unit: EUR
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          sum(member_of_household.dense_test_net_income)
  - name: dense_test_household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: '2025-01-01'
        formula: len(member_of_household)
"""

TWO_ENTITY_RULES = {
    "dense_test_net_income": "Person",
    "dense_test_household_net_income": "Household",
    "dense_test_household_size": "Household",
}


@pytest.fixture(scope="module")
def two_entity_module(tmp_path_factory) -> tuple[Path, Path]:
    """The canonical root holding ``TWO_ENTITY_MODULE_SOURCE``, and its path."""
    root = (tmp_path_factory.mktemp("two_entity") / "rulespec-us").resolve()
    path = root / "us/policies/tests/dense_two_entity.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(TWO_ENTITY_MODULE_SOURCE, encoding="utf-8")
    return root, path


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


# Generated modules for the property test: two membership relations, then
# derived rules on Person, Household and TaxUnit. Each rule's formula depends
# on its entity and dtype; Household and TaxUnit rules read their members.
GENERATED_MODULE_HEADER = """\
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, TaxUnit]
"""

GENERATED_FORMULAS = {
    ("Person", "Integer"): "person_count + 1",
    ("Person", "Money"): "person_amount * 2",
    ("Household", "Integer"): "len(member_of_household)",
    ("Household", "Money"): "sum(member_of_household.person_amount)",
    ("TaxUnit", "Integer"): "len(member_of_tax_unit)",
    ("TaxUnit", "Money"): "sum(member_of_tax_unit.person_amount)",
}


def _generated_rules(
    draws: list[tuple[str, bool, str | None, str | None]],
) -> list[DerivedMetadata]:
    """The metadata of one generated rule per ``(entity, is_money, period,
    source)`` draw: a Money rule is in USD, any other rule is an Integer."""
    return [
        DerivedMetadata(
            name=f"{entity.lower()}_rule_{index}",
            id=f"us:policies/tests/generated#{entity.lower()}_rule_{index}",
            entity=entity,
            dtype="decimal" if is_money else "integer",
            unit="USD" if is_money else None,
            period=period,
            source=source,
        )
        for index, (entity, is_money, period, source) in enumerate(draws)
    ]


def _generated_module_source(rules: list[DerivedMetadata]) -> str:
    """RuleSpec text that declares ``rules`` after the two relations."""
    lines = GENERATED_MODULE_HEADER.splitlines()
    for rule in rules:
        dtype = "Money" if rule.dtype == "decimal" else "Integer"
        lines += [
            f"  - name: {rule.name}",
            "    kind: derived",
            f"    entity: {rule.entity}",
            f"    dtype: {dtype}",
        ]
        lines += [
            f"    {field}: {value}"
            for field, value in (
                ("unit", rule.unit),
                ("period", rule.period),
                ("source", rule.source),
            )
            if value is not None
        ]
        lines += [
            "    versions:",
            "      - effective_from: '2025-01-01'",
            f"        formula: {GENERATED_FORMULAS[rule.entity, dtype]}",
        ]
    return "\n".join(lines) + "\n"


def _by_name(items: list[DerivedMetadata]) -> list[DerivedMetadata]:
    return sorted(items, key=lambda item: item.name)


def _derived_concepts(root: Path) -> list[tuple[str, str | None, str | None]]:
    """Each derived rule's id, entity and unit, as concept discovery reads
    them from the RuleSpec YAML in Python, without the compiler."""
    return sorted(
        (concept.concept_id, concept.entity, concept.unit)
        for concept in discover_concepts([root])
        if concept.kind == "derived"
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

    @pytest.mark.parametrize("entity", ["Person", "Household"])
    def test_lists_rules_on_every_entity_not_just_the_compiled_one(
        self, two_entity_module, entity
    ) -> None:
        root, path = two_entity_module
        compiled = CompiledDenseProgram.from_file(
            path, rulespec_roots=[root], entity=entity
        )
        # The premise: the dense program compiles only `entity`'s rules.
        assert compiled.root_entity == entity
        assert set(compiled.output_names) == {
            name for name, owner in TWO_ENTITY_RULES.items() if owner == entity
        }
        # Its metadata still lists every derived rule, each on its own entity.
        assert sorted(
            (item.name, item.entity) for item in compiled.derived_metadata
        ) == sorted(TWO_ENTITY_RULES.items())

    def test_is_the_same_whichever_entity_compiles(self, two_entity_module) -> None:
        # A caller may cache the metadata of the first program it compiles for
        # a module, whichever entity that program compiles.
        root, path = two_entity_module
        person, household = (
            CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity=entity)
            for entity in ("Person", "Household")
        )
        assert person.derived_metadata == household.derived_metadata

    @pytest.mark.parametrize("entity", ["Person", "Household"])
    def test_agrees_with_concept_discovery(self, two_entity_module, entity) -> None:
        # Differential: the extension reads the compiled artifact, concept
        # discovery reads the YAML. Both must name the same derived rules, on
        # the same entities, in the same units.
        root, path = two_entity_module
        compiled = CompiledDenseProgram.from_file(
            path, rulespec_roots=[root], entity=entity
        )
        discovered = _derived_concepts(root)
        assert len(discovered) == len(TWO_ENTITY_RULES)
        assert (
            sorted(
                (item.id, item.entity, item.unit)
                for item in compiled.derived_metadata
            )
            == discovered
        )

    def test_covers_every_entity_property(self) -> None:
        """Over generated modules with derived rules on up to three entities,
        each entity's program reports every rule's metadata, and concept
        discovery names the same rules (skipped when Hypothesis is not
        installed)."""
        hypothesis = pytest.importorskip("hypothesis")
        strategies = pytest.importorskip("hypothesis.strategies")

        draw = strategies.tuples(
            strategies.sampled_from(["Person", "Household", "TaxUnit"]),
            strategies.booleans(),
            strategies.sampled_from([None, "Year", "Month"]),
            strategies.sampled_from([None, "generated rule source"]),
        )

        @hypothesis.settings(max_examples=100, deadline=None, database=None)
        @hypothesis.given(draws=strategies.lists(draw, min_size=1, max_size=6))
        def check(draws: list[tuple[str, bool, str | None, str | None]]) -> None:
            rules = _generated_rules(draws)
            with tempfile.TemporaryDirectory() as directory:
                root = (Path(directory) / "rulespec-us").resolve()
                path = root / "us/policies/tests/generated.yaml"
                path.parent.mkdir(parents=True)
                path.write_text(_generated_module_source(rules), encoding="utf-8")
                for entity in sorted({rule.entity for rule in rules}):
                    compiled = CompiledDenseProgram.from_file(
                        path, rulespec_roots=[root], entity=entity
                    )
                    assert compiled.root_entity == entity
                    assert _by_name(compiled.derived_metadata) == _by_name(rules)
                assert _derived_concepts(root) == sorted(
                    (rule.id, rule.entity, rule.unit) for rule in rules
                )

        check()


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


OVERFLOW_MODULE_SOURCE = """\
format: rulespec/v1
module:
  summary: |-
    Dense-surface test fixture: doubling an amount, to exercise a result
    beyond the exact Decimal range.
  source_verification:
    corpus_citation_path: us/guidance/tests/dense-overflow
    upstream_source_check:
      status: official_parameter_source
      checked_paths:
        - us/statute/tests/dense-overflow
      rationale: The synthetic guidance supplies the test formula.
rules:
  - name: dense_test_doubled
    kind: derived
    entity: Person
    dtype: Decimal
    period: Year
    versions:
      - effective_from: '2025-01-01'
        formula: |-
          dense_test_amount * 2
"""


def test_decimal_overflow_is_a_runtime_error_not_a_panic(tmp_path) -> None:
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/dense_overflow.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(OVERFLOW_MODULE_SOURCE, encoding="utf-8")
    compiled = CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Person"
    )
    execution = dict(
        period_kind="calendar_year",
        start="2025-01-01",
        end="2025-12-31",
        inputs={"dense_test_amount": np.array([1.0, 7.0e28])},
    )
    # 1.4e29 is beyond Decimal's range (about 7.9e28). Exact execution reports
    # that as an ordinary error; it used to raise pyo3's PanicException, a
    # BaseException that `except Exception` does not catch.
    with pytest.raises(
        RuntimeError, match="arithmetic overflow: multiplication result"
    ):
        compiled.execute(**execution)
    # f64 execution follows IEEE 754 and answers.
    result = compiled.execute_f64(**execution)
    np.testing.assert_allclose(
        result["outputs"]["dense_test_doubled"], [2.0, 1.4e29]
    )
