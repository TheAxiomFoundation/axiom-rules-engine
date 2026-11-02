"""Dense programs compiled for the outputs a caller requests (#266).

``CompiledDenseProgram.from_file`` compiles every rule of the root entity that
a module's import closure holds, so one rule dense cannot compile refuses the
program, even a rule no wanted output reaches. With ``outputs=`` it compiles
only those rules and the rules they reach; a rule they reach still refuses it.

The modules are synthetic. They have the shape the issue reports for
26 USC 32 in ``rulespec-us``: the credit imports a module that also holds an
unrelated ``TaxUnit`` rule reading a ``Person`` rule directly. The rule names
echo the issue's; the formulas are placeholders.

These tests exercise the native ``axiom_rules_engine_dense`` extension and skip
when it is not built. Build locally with::

    maturin develop --release --manifest-path python-ext/Cargo.toml

The artifact test also runs the engine binary (``target/debug/axiom-rules-engine``)
and skips without it, except in CI.
"""

from __future__ import annotations

import os
from pathlib import Path

import numpy as np
import pytest

from axiom_rules_engine import AxiomRulesEngine, CompiledDenseProgram
from axiom_rules_engine.dense import NativeCompiledDenseProgram

pytestmark = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / "target" / "debug" / "axiom-rules-engine"
needs_binary = pytest.mark.skipif(
    not BINARY.exists() and not os.environ.get("CI"),
    reason="the engine binary is not built (cargo build)",
)

YEAR = {"period_kind": "tax_year", "start": "2026-01-01", "end": "2027-01-01"}

CREDIT = """\
format: rulespec/v1
imports:
  - us:policies/fixture/deduction
rules:
  - name: earned_income_credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income_for_credit * 0.1
"""

DEDUCTION = """\
format: rulespec/v1
imports:
  - us:policies/fixture/exclusion
rules:
  - name: earned_income_for_credit
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: wages + self_employment_income
  - name: senior_deduction_modified_adjusted_gross_income
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: adjusted_gross_income + section_911_amount_excluded_from_gross_income
"""

EXCLUSION = """\
format: rulespec/v1
rules:
  - name: section_911_amount_excluded_from_gross_income
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: foreign_earned_income
"""

COMPOSITION = """\
format: rulespec/v1
module:
  kind: composition
imports:
  - us:policies/fixture/credit
rules:
  - name: credit_with_bonus
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: earned_income_credit + bonus
"""

CROSSES = "`section_911_amount_excluded_from_gross_income` from `senior_deduction_modified_adjusted_gross_income` crosses into `Person`"


@pytest.fixture(scope="module")
def corpus(tmp_path_factory) -> tuple[Path, Path]:
    """A ``rulespec-us`` root holding the three modules, and the credit."""
    root = (tmp_path_factory.mktemp("requested") / "rulespec-us").resolve()
    for name, text in [
        ("credit", CREDIT),
        ("deduction", DEDUCTION),
        ("exclusion", EXCLUSION),
    ]:
        path = root / "us/policies/fixture" / f"{name}.yaml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root, root / "us/policies/fixture/credit.yaml"


def _credit_values(program: CompiledDenseProgram) -> list[float]:
    result = program.execute(
        **YEAR,
        inputs={
            "wages": np.array([1000.0, 0.0, 250.5]),
            "self_employment_income": np.array([200.0, 0.0, 49.5]),
        },
        outputs=["earned_income_credit"],
    )
    return [float(value) for value in result["outputs"]["earned_income_credit"]]


def test_the_whole_root_refuses_the_credit_for_a_rule_it_does_not_reach(corpus) -> None:
    root, credit = corpus
    with pytest.raises(ValueError) as refused:
        CompiledDenseProgram.from_file(credit, rulespec_roots=[root], entity="TaxUnit")
    message = str(refused.value)
    assert CROSSES in message
    assert "by design" in message
    assert (
        "or compile only outputs that do not reach "
        "`senior_deduction_modified_adjusted_gross_income`"
    ) in message


def test_requested_outputs_compile_the_credit(corpus) -> None:
    root, credit = corpus
    program = CompiledDenseProgram.from_file(
        credit,
        rulespec_roots=[root],
        entity="TaxUnit",
        outputs=["earned_income_credit"],
    )
    assert program.root_entity == "TaxUnit"
    assert sorted(program.output_names) == [
        "earned_income_credit",
        "earned_income_for_credit",
    ]
    assert sorted(program.root_inputs) == ["self_employment_income", "wages"]
    assert program.relations == []
    # The metadata still covers every rule the module brings in.
    assert {item.name for item in program.derived_metadata} == {
        "earned_income_credit",
        "earned_income_for_credit",
        "senior_deduction_modified_adjusted_gross_income",
        "section_911_amount_excluded_from_gross_income",
    }
    assert _credit_values(program) == [120.0, 0.0, 30.0]

    # With no entity named, the outputs name it; any iterable of names works.
    inferred = CompiledDenseProgram.from_file(
        credit,
        rulespec_roots=[root],
        outputs=(name for name in ["earned_income_credit"]),
    )
    assert inferred.root_entity == "TaxUnit"
    assert inferred.output_names == program.output_names
    assert _credit_values(inferred) == _credit_values(program)


def test_a_requested_output_that_reaches_the_crossing_rule_still_refuses(corpus) -> None:
    root, credit = corpus
    with pytest.raises(ValueError, match="crosses into `Person`") as refused:
        CompiledDenseProgram.from_file(
            credit,
            rulespec_roots=[root],
            entity="TaxUnit",
            outputs=["senior_deduction_modified_adjusted_gross_income"],
        )
    assert CROSSES in str(refused.value)


def test_requested_outputs_are_checked(corpus) -> None:
    root, credit = corpus

    def compile_(outputs, entity="TaxUnit"):
        return CompiledDenseProgram.from_file(
            credit, rulespec_roots=[root], entity=entity, outputs=outputs
        )

    with pytest.raises(TypeError, match="not a single string"):
        compile_("earned_income_credit")
    with pytest.raises(ValueError, match="at least one requested output"):
        compile_([])
    with pytest.raises(
        ValueError, match="could not find requested output `no_such_rule`"
    ):
        compile_(["no_such_rule"])
    with pytest.raises(ValueError, match="so request `earned_income_credit`"):
        compile_(["us:policies/fixture/credit#earned_income_credit"])
    with pytest.raises(ValueError, match="is a `Person` rule"):
        compile_(["section_911_amount_excluded_from_gross_income"])
    with pytest.raises(ValueError, match="belong to entities `Person` and `TaxUnit`"):
        compile_(
            ["earned_income_credit", "section_911_amount_excluded_from_gross_income"],
            entity=None,
        )


def test_a_composition_compiles_for_its_requested_outputs(corpus, tmp_path) -> None:
    root, _ = corpus
    composition = (tmp_path / "composition.yaml").resolve()
    composition.write_text(COMPOSITION, encoding="utf-8")
    with pytest.raises(ValueError, match="crosses into `Person`"):
        CompiledDenseProgram.from_composed_file(
            composition, rulespec_roots=[root], entity="TaxUnit"
        )
    program = CompiledDenseProgram.from_composed_file(
        composition,
        rulespec_roots=[root],
        entity="TaxUnit",
        outputs=["credit_with_bonus"],
    )
    assert sorted(program.output_names) == [
        "credit_with_bonus",
        "earned_income_credit",
        "earned_income_for_credit",
    ]
    assert sorted(program.root_inputs) == ["bonus", "self_employment_income", "wages"]


@needs_binary
def test_an_artifact_file_loads_for_its_requested_outputs(corpus, tmp_path) -> None:
    root, credit = corpus
    artifact = tmp_path / "credit.json"
    AxiomRulesEngine(binary_path=BINARY).compile(
        program_path=credit, rulespec_roots=(root,), output_path=artifact
    )
    with pytest.raises(ValueError, match="crosses into `Person`"):
        CompiledDenseProgram.from_artifact_file(artifact, entity="TaxUnit")
    program = CompiledDenseProgram.from_artifact_file(
        artifact, entity="TaxUnit", outputs=["earned_income_credit"]
    )
    assert program.artifact_format_version == 3
    assert sorted(program.output_names) == [
        "earned_income_credit",
        "earned_income_for_credit",
    ]
    assert _credit_values(program) == [120.0, 0.0, 30.0]
