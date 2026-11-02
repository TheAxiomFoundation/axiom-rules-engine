from __future__ import annotations

import json
import os
import random
import re
from pathlib import Path

import pytest
import yaml

from axiom_rules_engine import AxiomRulesEngine, CompiledDenseProgram
from axiom_rules_engine.cli import main
from axiom_rules_engine.concepts import (
    _infer_input_names,
    _parse_formula,
    discover_concepts,
    search_concepts,
    show_concept,
    validate_concept_id,
)
from axiom_rules_engine.dense import NativeCompiledDenseProgram

BINARY = Path(__file__).resolve().parents[2] / "target" / "debug" / "axiom-rules-engine"
needs_binary = pytest.mark.skipif(
    not BINARY.exists() and not os.environ.get("CI"),
    reason="the engine binary is not built (cargo build)",
)
needs_dense = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)


def write_rulespec(root: Path, relative: str, body: str) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body.strip() + "\n")
    return path


def canonical_root(tmp_path: Path) -> Path:
    root = tmp_path / "rulespec-us"
    root.mkdir()
    return root.resolve()


def rulespec_body() -> str:
    return """
format: rulespec/v1
module:
  summary: Internal Revenue Code section 62 defines adjusted gross income.
  source_verification:
    corpus_citation_path: us/statute/26/62
rules:
  - name: member_of_tax_unit
    kind: data_relation
    data_relation:
      arity: 2
  - name: section_62_cites_section_63
    kind: source_relation
    source: 26 USC 62
    source_relation:
      type: cites
      target: us:statutes/26/63
  - name: adjusted_gross_income
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    source: 26 USC 62
    versions:
      - effective_from: '2023-01-01'
        formula: gross_income - above_the_line_deductions
  - name: gross_income
    kind: derived
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: '2023-01-01'
        formula: wages + interest_income
"""


def test_discover_concepts_from_rulespec_file(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    concepts = discover_concepts([root])
    by_id = {concept.concept_id: concept for concept in concepts}

    assert "us:statutes/26/62" in by_id
    assert by_id["us:statutes/26/62#adjusted_gross_income"].kind == "derived"
    assert by_id["us:statutes/26/62#adjusted_gross_income"].citation == "26 USC 62"
    assert by_id["us:statutes/26/62#relation.member_of_tax_unit"].kind == "data_relation"
    assert (
        by_id["us:statutes/26/62#source_relation.section_62_cites_section_63"].kind
        == "source_relation"
    )
    assert by_id["us:statutes/26/62#input.wages"].kind == "input"
    assert by_id["us:statutes/26/62#input.wages"].status == "inferred"


def test_predicate_terms_are_not_inferred_inputs(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(
        root,
        "us/statutes/26/62.yaml",
        """
format: rulespec/v1
module:
  summary: Internal Revenue Code section 62 defines adjusted gross income.
  source_verification:
    corpus_citation_path: us/statutes/26/62
rules:
  - name: relative_of
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Person]
  - name: older_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: '2026-01-01'
        formula: candidate.age > anchor.age
  - name: anchor_plus_one
    kind: derived
    entity: Person
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: anchor + 1
""",
    )

    inputs = {
        concept.concept_id
        for concept in discover_concepts([root])
        if concept.kind == "input"
    }

    # Outside a predicate `anchor` is an ordinary identifier, so it still
    # infers an input there.
    assert inputs == {
        "us:statutes/26/62#input.age",
        "us:statutes/26/62#input.anchor",
    }


PATH_RELATION = """
format: rulespec/v1
module:
  summary: Internal Revenue Code section 62 defines adjusted gross income.
  source_verification:
    corpus_citation_path: us/statutes/26/62
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: lives_with
    kind: derived_relation
    derived_relation:
      arity: 2
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
SOURCE
    versions:
      - effective_from: '2026-01-01'
        formula: 'true'
"""


def test_a_path_source_is_a_derived_relations_source(tmp_path: Path) -> None:
    root = tmp_path / "rulespec-us"
    path = """      source_path:
        - {relation: member_of_household, from_slot: 0, to_slot: 1}
        - {relation: member_of_household, from_slot: 1, to_slot: 0}"""
    write_rulespec(root, "us/statutes/26/62.yaml", PATH_RELATION.replace("SOURCE", path))

    assert "us:statutes/26/62#lives_with" in {
        concept.concept_id for concept in discover_concepts([root])
    }


def test_a_derived_relation_needs_a_source(tmp_path: Path) -> None:
    root = tmp_path / "rulespec-us"
    write_rulespec(
        root,
        "us/statutes/26/62.yaml",
        PATH_RELATION.replace("SOURCE\n", ""),
    )

    with pytest.raises(ValueError, match="source_relation or source_path"):
        discover_concepts([root])


def write_formula_module(
    root: Path,
    formulas: dict[str, str],
    *,
    dtype: str = "Money",
    inputs: list[dict[str, str]] | None = None,
    imports: list[str] | None = None,
    rules: list[dict[str, object]] | None = None,
) -> Path:
    document: dict[str, object] = {"format": "rulespec/v1"}
    if imports:
        document["imports"] = imports
    if inputs:
        document["inputs"] = inputs
    module_rules: list[dict[str, object]] = [
        {
            "name": "member_of_household",
            "kind": "data_relation",
            "data_relation": {"arity": 2, "arguments": ["Person", "Household"]},
        },
        *(rules or []),
    ]
    for name, formula in formulas.items():
        module_rules.append(
            {
                "name": name,
                "kind": "derived",
                "entity": "Household",
                "dtype": dtype,
                "period": "Month",
                "versions": [{"effective_from": "2026-01-01", "formula": formula}],
            }
        )
    document["rules"] = module_rules
    return write_rulespec(
        root,
        "us/statutes/7/2014.yaml",
        yaml.safe_dump(document, sort_keys=False),
    )


def inferred_input_names(root: Path) -> set[str]:
    prefix = "us:statutes/7/2014#input."
    return {
        concept.concept_id.removeprefix(prefix)
        for concept in discover_concepts([root])
        if concept.kind == "input"
    }


def test_function_calls_are_not_inferred_inputs(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_formula_module(
        root,
        {"has_several_members": "len(member_of_household) >= 2 and income < limit"},
        dtype="Judgment",
    )

    assert inferred_input_names(root) == {"income", "limit"}


# Each function the formula lowering accepts (`lower_to_scalar` and
# `lower_to_judgment` in src/formula.rs), called as documented there, with the
# inputs the call reads.
FUNCTION_CALLS = [
    ("max(wages, 0)", {"wages"}),
    ("min(wages, cap)", {"wages", "cap"}),
    ("max(member_of_household.age)", {"age"}),
    ("min(member_of_household.age)", {"age"}),
    ("ceil(wages)", {"wages"}),
    ("floor(wages)", {"wages"}),
    ("days_between(birth_date, assessment_date)", {"birth_date", "assessment_date"}),
    ("date_add_days(birth_date, 1)", {"birth_date"}),
    ("date_add_months(birth_date, 1)", {"birth_date"}),
    ("date_add_years(birth_date, 18)", {"birth_date"}),
    ("sum_over_periods(wages)", {"wages"}),
    ("max_over_periods(wages)", {"wages"}),
    ("count_over_periods(is_adult)", {"is_adult"}),
    ("sum_top_n_over_periods(wages, 35)", {"wages"}),
    ("len(member_of_household)", set()),
    ("sum(member_of_household.wages)", {"wages"}),
    ("only(member_of_household.age)", {"age"}),
    ("input_for_year(wages, 2024)", {"wages"}),
    ("only_related(member_of_household, is_adult)", {"is_adult"}),
    ("count_where(member_of_household, is_adult)", {"is_adult"}),
    ("sum_where(member_of_household, wages, is_adult)", {"wages", "is_adult"}),
    ("min_where(member_of_household, age, is_adult)", {"age", "is_adult"}),
    ("max_where(member_of_household, age, is_adult)", {"age", "is_adult"}),
    ("exactly_one(is_adult, is_student)", {"is_adult", "is_student"}),
]
# The parser calls a name wrapped in grouping parentheses too.
GROUPED_CALLS = [
    ("(ceil)(wages)", {"wages"}),
    ("(max)(wages, 0)", {"wages"}),
    ("((len))(member_of_household)", set()),
    ("max((floor)(wages), 0)", {"wages"}),
]
# The lexer skips whitespace, `#` comments and `"""` blocks between tokens, so
# the parser still reads `ceil` followed by any of them and `(` as a call.
SEPARATORS = [
    "",
    " ",
    "\t",
    "\n  ",
    "\r\n",
    "\x0c",
    " # rounds up\n  ",
    ' """Rounds up.""" ',
]
# Only a call makes a name a function: the parser reads `floor`, `sum` and a
# grouped `max` here as inputs, and a `(` inside a comment or an unclosed
# `"""` block is no token.
BARE_FUNCTION_NAMES = [
    ("ceil(floor) + sum", {"floor", "sum"}),
    ("(max) + 1", {"max"}),
    ("max # (\n+ 1", {"max"}),
    ('min """ (', {"min"}),
]
# String literals, comments and `"""` blocks name nothing.
TEXT_CASES = [
    ('"max"', set()),
    ('"Aged (65+)"', set()),
    ("wages # then floor(wages)", {"wages"}),
    ('"""Rounded down, as floor(x) would."""\nfloor(wages)', {"wages"}),
]
# The words `keyword()` in src/formula.rs lexes as keywords are never names,
# and no other word is a keyword: `then`, `in` and the like are inputs.
KEYWORD_CASES = [
    ("if is_adult: 1 elif is_student: 2 else: 3", {"is_adult", "is_student"}),
    ("if True: wages else: 0", {"wages"}),
    ("if is_adult and False: 0 else: wages", {"is_adult", "wages"}),
    ("ceil(then)", {"then"}),
    ("max(as, case) + min(in, is) + let", {"as", "case", "in", "is", "let"}),
]
# The keywords that begin declarations are refused where a formula reads a
# name (`parse_primary`).
DECLARATION_KEYWORDS = [
    (f"wages + {keyword}", {"wages"}) for keyword in ("amend", "entity", "from", "to")
]
# `_ =>` is the match wildcard (`parse_match`). `_` read anywhere else, and a
# name pattern, are inputs.
MATCH_CASES = [
    ("match filing_status: 1 => wages _ => 0", {"filing_status", "wages"}),
    ("match filing_status:\n  1 => wages\n  _=>0", {"filing_status", "wages"}),
    ("match filing_status: 1 => _ _ => 0", {"_", "filing_status"}),
    (
        "match filing_status: single => wages _ => 0",
        {"filing_status", "single", "wages"},
    ),
    ("_ >= 1", {"_"}),
]
# `lower_to_scalar` reads `period_start` and `period_end` as the period's
# dates, and `lower_to_judgment` refuses them. An argument read by name and a
# field read through a relation are still inputs of those names.
PERIOD_CASES = [
    ("days_between(period_start, period_end)", set()),
    ("days_between(period_start, birth_date)", {"birth_date"}),
    ("match birth_date: period_end => 1 _ => 0", {"birth_date"}),
    ("not period_end", set()),
    ("count_where(member_of_household, period_start)", {"period_start"}),
    ("count_where(member_of_household, (period_end))", {"period_end"}),
    (
        "sum_where(member_of_household, period_end, period_start)",
        {"period_end", "period_start"},
    ),
    ("sum(member_of_household.period_start)", {"period_start"}),
]
# `len(r.f)` counts `r` and reads no field. A name indexed with `[` is a
# parameter table, here one another module declares, and a relation argument
# is a relation even where no rule declares it.
POSITION_CASES = [
    ("len(member_of_household.age)", set()),
    ("len((member_of_household).age)", set()),
    ("len((member_of_household.age))", set()),
    ("rate_table[household_size] * 2", {"household_size"}),
    ("(rate_table)[household_size]", {"household_size"}),
    ("count_where(household_members, is_adult)", {"is_adult"}),
    ("sum(household_members.wages)", {"wages"}),
]
# The lexer reads a path such as `a/b` as one name, and the lowering reads it
# as the input of that name. With whitespace around `/`, or a digit after it,
# `/` divides.
PATH_CASES = [
    ("household/size", {"household/size"}),
    ("household/size/adults + wages", {"household/size/adults", "wages"}),
    ("household/_size", {"household/_size"}),
    ("wages / months", {"months", "wages"}),
    ("wages /months", {"months", "wages"}),
    ("wages/ months", {"months", "wages"}),
    ("wages/12", {"wages"}),
]
# `lower_to_scalar` lowers only the first argument of `len` and of `sum`, and
# reads no field through `len`'s: nothing reads the arguments after it.
UNREAD_ARGUMENT_CASES = [
    ("len(member_of_household, wages)", set()),
    ("len(member_of_household.age, wages)", set()),
    ("len(member_of_household, member_of_household.age)", set()),
    ("sum(member_of_household.wages, extra)", {"wages"}),
    ("match len(member_of_household, z): _ => 1", set()),
]
# A `match` with only a wildcard arm lowers to that arm, so nothing reads its
# subject. With any other arm, the comparison reads it.
WILDCARD_ONLY_CASES = [
    ("match x: _ => 1", set()),
    ("match x + y: _ => z", {"z"}),
    ("match x: 1 => (match y: _ => 2) _ => 3", {"x"}),
    ("(match a/b: _ => wages) + 1", {"wages"}),
    ("max(match x: _ => 1, wages)", {"wages"}),
    ("match count_where(member_of_household, is_adult): _ => wages", {"wages"}),
]
# What a declaration's name reads ("Declaration names" in docs/rulespec.md).
DECLARED_NAME_CASES = [
    # Bare, a parameter, a table or a rule reads itself.
    ("local_rate + 1", set()),
    ("local_table + 1", set()),
    ("local_table[local_rate]", set()),
    ("local_table[wages]", {"wages"}),
    ("person_pay + 1", set()),
    # Read by name, only a derived rule shadows the input of its name.
    ("sum(member_of_household.person_pay)", set()),
    ("count_where(member_of_household, person_is_adult)", set()),
    ("count_where(member_of_household, is_eligible)", {"is_eligible"}),
    ("sum(member_of_household.local_rate)", {"local_rate"}),
    ("sum(member_of_household.local_table)", {"local_table"}),
    ("max(member_of_household.local_rate)", {"local_rate"}),
    ("only(member_of_household.local_rate)", {"local_rate"}),
    ("sum_where(member_of_household, local_rate, is_eligible)", {"local_rate", "is_eligible"}),
    ("min_where(member_of_household, local_rate, is_eligible)", {"local_rate", "is_eligible"}),
    # A rule with no entity and only literal values is a parameter, a grouped
    # literal included. `-1` computes, so that rule is a derived rule.
    ("count_where(member_of_household, literal_rule)", {"literal_rule"}),
    ("count_where(member_of_household, grouped_literal_rule)", {"grouped_literal_rule"}),
    ("count_where(member_of_household, yaml_number_rule)", {"yaml_number_rule"}),
    ("count_where(member_of_household, negative_rule)", set()),
    ("literal_rule + grouped_literal_rule + yaml_number_rule + negative_rule", set()),
    # Outside a predicate a relation's name reads an input, and a source
    # relation's reads one everywhere.
    ("member_of_household + 1", {"member_of_household"}),
    ("member_of_household", {"member_of_household"}),
    ("cites_section_63 + 1", {"cites_section_63"}),
]
# A number to `serde_yaml`, though PyYAML leaves these as text.
YAML_NUMBER_CASES = [
    ("1e5", set()),
    ("1E5", set()),
    ("1e-5", set()),
    ("1e+5", set()),
    ("1.5e3", set()),
    ("-1e5", set()),
    (".5e3", set()),
    ("5.e3", set()),
]
# Whitespace around a formula is no part of it, of any kind (`str::trim`).
TRIM_CASES = [
    ("len(member_of_household) ", set()),
    (" wages", {"wages"}),
]
# A rule of each other kind in the module. None reads an input, so the compiled
# program's inputs come from the formula under test alone.
LOCAL_TABLE_LABEL = "local_table_index"
DECLARED_RULES: list[dict[str, object]] = [
    {
        "name": name,
        "kind": kind,
        "dtype": dtype,
        **({"entity": entity, "period": "Month"} if entity else {}),
        "versions": [{"effective_from": "2026-01-01", "formula": formula}],
    }
    for name, kind, dtype, entity, formula in [
        ("local_rate", "parameter", "Integer", None, "35"),
        ("is_eligible", "parameter", "Judgment", None, "true"),
        ("yaml_number_rule", "parameter", "Money", None, 0.5),
        ("literal_rule", "derived", "Money", None, "1"),
        ("grouped_literal_rule", "derived", "Money", None, "(1)"),
        ("negative_rule", "derived", "Money", None, "-1"),
        ("person_pay", "derived", "Money", "Person", "3"),
        ("person_is_adult", "derived", "Judgment", "Person", "true"),
    ]
] + [
    {
        "name": "local_table",
        "kind": "parameter",
        "dtype": "Money",
        "indexed_by": LOCAL_TABLE_LABEL,
        "versions": [{"effective_from": "2026-01-01", "values": {1: 10, 2: 20}}],
    },
    {
        "name": "cites_section_63",
        "kind": "source_relation",
        "source": "7 USC 2014",
        "source_relation": {"type": "cites", "target": "us:statutes/7/2013"},
    },
]
DECLARED_NAME_FORMULAS = {formula for formula, _ in DECLARED_NAME_CASES}
# What some formulas need to compile: a result dtype other than Money, an
# input declaration, or an imported module.
CALL_DTYPES = {
    "date_add_days(birth_date, 1)": "Date",
    "date_add_months(birth_date, 1)": "Date",
    "date_add_years(birth_date, 18)": "Date",
    "only_related(member_of_household, is_adult)": "Boolean",
    "exactly_one(is_adult, is_student)": "Judgment",
    '"max"': "Text",
    '"Aged (65+)"': "Text",
    "_ >= 1": "Judgment",
    "days_between(period_start, period_end)": "Integer",
    "days_between(period_start, birth_date)": "Integer",
    "match birth_date: period_end => 1 _ => 0": "Integer",
    "not period_end": "Judgment",
    "count_where(member_of_household, period_start)": "Integer",
    "count_where(member_of_household, (period_end))": "Integer",
    "len(member_of_household.age)": "Integer",
    "len((member_of_household).age)": "Integer",
    "len((member_of_household.age))": "Integer",
    "count_where(household_members, is_adult)": "Integer",
    "member_of_household": "Judgment",
}
RATE_TABLE_MODULE = "us:statutes/7/2013"
CALL_IMPORTS = {
    "rate_table[household_size] * 2": [RATE_TABLE_MODULE],
    "(rate_table)[household_size]": [RATE_TABLE_MODULE],
}
CALL_INPUTS = {
    "input_for_year(wages, 2024)": [
        {
            "name": "wages",
            "entity": "Household",
            "dtype": "Money",
            "unit": "USD",
            "period": "Year",
        }
    ],
    "only_related(member_of_household, is_adult)": [
        {"name": "is_adult", "entity": "Person", "dtype": "Boolean"}
    ],
}


def write_call_module(root: Path, formula: str) -> Path:
    imports = CALL_IMPORTS.get(formula)
    if imports:
        write_rate_table_module(root)
    return write_formula_module(
        root,
        {"called": formula},
        dtype=CALL_DTYPES.get(formula, "Money"),
        inputs=CALL_INPUTS.get(formula),
        imports=imports,
        rules=DECLARED_RULES if formula in DECLARED_NAME_FORMULAS else None,
    )


def write_rate_table_module(root: Path) -> Path:
    document = {
        "format": "rulespec/v1",
        "rules": [
            {
                "name": "rate_table",
                "kind": "parameter",
                "dtype": "Money",
                "indexed_by": "household_size",
                "versions": [
                    {"effective_from": "2026-01-01", "values": {1: 10, 2: 20}}
                ],
            }
        ],
    }
    return write_rulespec(
        root,
        "us/statutes/7/2013.yaml",
        yaml.safe_dump(document, sort_keys=False),
    )


@pytest.mark.parametrize(("formula", "inputs"), FUNCTION_CALLS + GROUPED_CALLS)
def test_no_function_name_is_inferred_as_an_input(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    write_call_module(root, formula)

    assert inferred_input_names(root) == inputs


@pytest.mark.parametrize("separator", SEPARATORS)
def test_call_is_seen_past_what_the_lexer_skips(
    tmp_path: Path, separator: str
) -> None:
    root = canonical_root(tmp_path)
    write_call_module(root, f"ceil{separator}(wages)")

    assert inferred_input_names(root) == {"wages"}


@pytest.mark.parametrize(("formula", "inputs"), BARE_FUNCTION_NAMES)
def test_function_name_read_as_a_value_is_an_input(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    write_call_module(root, formula)

    assert inferred_input_names(root) == inputs


@pytest.mark.parametrize(("formula", "inputs"), TEXT_CASES)
def test_words_in_strings_and_comments_name_no_input(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    write_call_module(root, formula)

    assert inferred_input_names(root) == inputs


@pytest.mark.parametrize(
    ("formula", "inputs"),
    KEYWORD_CASES
    + DECLARATION_KEYWORDS
    + MATCH_CASES
    + PERIOD_CASES
    + POSITION_CASES
    + PATH_CASES
    + UNREAD_ARGUMENT_CASES
    + WILDCARD_ONLY_CASES
    + DECLARED_NAME_CASES
    + YAML_NUMBER_CASES
    + TRIM_CASES,
)
def test_a_name_is_an_input_only_where_the_lowering_reads_one(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    write_call_module(root, formula)

    assert inferred_input_names(root) == inputs


PATH_RULES = {"household/size": "wages", "called": "household/size + 1"}


def test_a_path_that_names_a_rule_reads_the_rule(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_formula_module(root, PATH_RULES)

    assert inferred_input_names(root) == {"wages"}


# A name starts where the lexer's previous token ends: a date is ten bytes, and
# a number keeps its underscores.
@pytest.mark.parametrize(
    ("formula", "inputs"),
    [("2026-01-01_x", {"_x"}), ("1_000_x", {"x"}), ("1.5_x", {"x"})],
)
def test_names_start_where_the_lexer_starts_them(
    formula: str, inputs: set[str]
) -> None:
    rules = [{"name": "r", "kind": "derived", "versions": [{"formula": formula}]}]

    assert _infer_input_names(rules) == inputs


class SearchCountingFormula(str):
    """A formula that counts the characters its `find` calls search."""

    searched = 0

    def find(self, sub, start=0, end=None):  # type: ignore[override]
        index = super().find(sub, start, len(self) if end is None else end)
        stop = len(self) if index == -1 else index + len(sub)
        self.searched += stop - start
        return index


@pytest.mark.parametrize(
    "text",
    [
        "x" + " # x" * 2_000,
        'x """' + " x #" * 2_000 + '"""',
        "x # x\n" * 2_000,
    ],
    ids=["comment", "block", "comment lines"],
)
def test_inference_searches_each_comment_once(text: str) -> None:
    # Each name inside a comment once searched the rest of the comment again,
    # which made a long comment take quadratic time.
    formula = SearchCountingFormula(text)
    rules = [{"name": "long", "kind": "derived", "versions": [{"formula": formula}]}]

    assert _infer_input_names(rules) == {"x"}
    assert formula.searched <= len(formula)


# The fixture groups above, checked against the inputs the compiled program
# reads. A fixture the compiler refuses is valid up to the refusal named here,
# which is where its inputs stop being checkable.
COMPILE_REFUSALS = {
    "input_for_year(wages, 2024)": (
        "dense compilation does not yet support selected-year input"
    ),
    'min """ (': "block is never closed",
    **{
        formula: re.escape(
            f"unexpected token in expression: {keyword.capitalize()} ({keyword})"
        )
        for (formula, _), keyword in zip(
            DECLARATION_KEYWORDS, ("amend", "entity", "from", "to"), strict=True
        )
    },
    "not period_end": "`period_end` is a date, so it cannot be tested as a judgment",
    "count_where(household_members, is_adult)": "typing is mandatory",
    "sum(household_members.wages)": "typing is mandatory",
    # The dense compiler covers less than the artifact, which checks these two.
    "person_pay + 1": "dense compilation only supports dependencies within the same root",
    "count_where(member_of_household, negative_rule)": (
        "dense compilation does not yet support where-clause predicates"
    ),
}
COMPILED_CASES = [
    (formula, inputs, COMPILE_REFUSALS.get(formula))
    for formula, inputs in [
        *FUNCTION_CALLS,
        *GROUPED_CALLS,
        *((f"ceil{separator}(wages)", {"wages"}) for separator in SEPARATORS),
        *BARE_FUNCTION_NAMES,
        *TEXT_CASES,
        *KEYWORD_CASES,
        *DECLARATION_KEYWORDS,
        *MATCH_CASES,
        *PERIOD_CASES,
        *POSITION_CASES,
        *PATH_CASES,
        *UNREAD_ARGUMENT_CASES,
        *WILDCARD_ONLY_CASES,
        *DECLARED_NAME_CASES,
        *YAML_NUMBER_CASES,
        *TRIM_CASES,
    ]
]


def dense_inputs(program: CompiledDenseProgram) -> set[str]:
    return set(program.root_inputs) | {
        name for relation in program.relations for name in relation.related_inputs
    }


def artifact_inputs(root: Path, path: Path, output: Path) -> set[str]:
    """The inputs the compiled artifact lists for the whole module, whichever
    rule or predicate reads them. It also lists a table's `indexed_by` label,
    read or not."""
    compiled = AxiomRulesEngine(binary_path=BINARY).compile(
        program_path=path, rulespec_roots=(root,), output_path=output
    )
    return {entry.slot for entry in compiled.metadata.input_catalog} - {LOCAL_TABLE_LABEL}


@needs_dense
@pytest.mark.parametrize(("formula", "inputs", "refusal"), COMPILED_CASES)
def test_inferred_inputs_are_the_compiled_program_inputs(
    tmp_path: Path, formula: str, inputs: set[str], refusal: str | None
) -> None:
    root = canonical_root(tmp_path)
    path = write_call_module(root, formula)
    assert inferred_input_names(root) == inputs

    if refusal is not None:
        with pytest.raises(ValueError, match=refusal):
            CompiledDenseProgram.from_file(
                path, rulespec_roots=[root], entity="Household"
            )
        return
    program = CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Household"
    )
    assert dense_inputs(program) == inputs
    # The engine accepts each input under the concept ID discovery gives it.
    assert set(program.input_request_names) - {LOCAL_TABLE_LABEL} == inputs
    assert all(
        f"us:statutes/7/2014#input.{name}" in program.input_request_names[name]
        for name in inputs
    )


@needs_binary
@pytest.mark.parametrize(
    ("formula", "inputs"),
    [
        (formula, inputs)
        for formula, inputs, refusal in COMPILED_CASES
        if refusal is None or refusal.startswith("dense compilation")
    ],
)
def test_inferred_inputs_are_the_compiled_artifact_inputs(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    path = write_call_module(root, formula)

    assert artifact_inputs(root, path, tmp_path / "artifact.json") == inputs


def write_predicate_module(root: Path, predicate: str) -> Path:
    document = {
        "format": "rulespec/v1",
        "rules": [
            {
                "name": "member_of_household",
                "kind": "data_relation",
                "data_relation": {"arity": 2, "arguments": ["Person", "Household"]},
            },
            {
                "name": "relative_of",
                "kind": "data_relation",
                "data_relation": {"arity": 2, "arguments": ["Person", "Person"]},
            },
            *DECLARED_RULES,
            {
                "name": "kin",
                "kind": "derived_relation",
                "derived_relation": {
                    "arity": 2,
                    "source_relation": "relative_of",
                    "slot_entities": ["Person", "Person"],
                    "current_slot": 0,
                    "related_slot": 1,
                },
                "versions": [{"effective_from": "2026-01-01", "formula": predicate}],
            },
        ],
    }
    return write_rulespec(
        root,
        "us/statutes/7/2014.yaml",
        yaml.safe_dump(document, sort_keys=False),
    )


# What a derived relation's predicate reads (`lower_judgment_formula`).
PREDICATE_CASES = [
    # `anchor`, `candidate` and what an `exists` binds are terms: a read through
    # one reads the name after it, and a term alone reads nothing.
    ("candidate.age > anchor.age", {"age"}),
    ("anchor == candidate", set()),
    ("relative_of(anchor, candidate)", set()),
    ("not relative_of(candidate, anchor)", set()),
    ("exists(relative_of(anchor, p), p.age >= 65)", {"age"}),
    ("exists(relative_of(anchor, p), p != candidate)", set()),
    ("exists(relative_of(p, anchor), p.age > local_rate)", {"age"}),
    ("exists(relative_of(anchor, p), exists(relative_of(p, q), q.age > p.age))", {"age"}),
    ("exists(relative_of(anchor, p), p.age > 1) or other", {"age", "other"}),
    # An `exists` nothing lowers still binds no input.
    ("len(member_of_household, exists(relative_of(anchor, p), true)) > 0 and free", {"free"}),
    ("max(candidate.age) > 1", {"age"}),
    ("candidate.person_is_adult", set()),
    ("exists(relative_of(anchor, p), p.person_is_adult)", set()),
    # A relation's name reads the relation only in the judgment position.
    ("member_of_household", set()),
    ("(if member_of_household: 1 else: 0) > 0", set()),
    ("exactly_one(member_of_household, is_kin)", {"is_kin"}),
    ("member_of_household == true", {"member_of_household"}),
    ("cites_section_63", {"cites_section_63"}),
    ("cites_section_63 > 1", {"cites_section_63"}),
    # Other names read as they do in a rule.
    ("is_eligible", set()),
    ("candidate.age > 1 and shared", {"age", "shared"}),
    ("true", set()),
    ("'text' == x", {"x"}),
    ("period_start == period_end", set()),
    ("days_between(period_start, birth_date) > 0", {"birth_date"}),
    ("local_table[wages] > 0", {"wages"}),
    ("(match x: _ => 1) > 0", set()),
    ("(match x: 1 => y _ => 1) > 0", {"x", "y"}),
    ("sum(member_of_household.age) > 0", {"age"}),
    ("sum(member_of_household.local_rate) > 0", {"local_rate"}),
    ("min(member_of_household.age) > 0", {"age"}),
    ("only(member_of_household.age) > 0", {"age"}),
    ("len(member_of_household, wages) > 0", set()),
    ("len(member_of_household) > 0 and person_is_adult", set()),
    ("count_where(member_of_household, is_adult) > 0", {"is_adult"}),
    ("count_where(member_of_household, person_is_adult) > 0", set()),
    ("count_where(member_of_household, is_eligible) > 0", {"is_eligible"}),
    ("sum_where(member_of_household, local_rate, is_eligible) > 0", {"local_rate", "is_eligible"}),
    # The writer's checks on a rule's formula do not cover a predicate.
    ('x """ y', {"x"}),
]


@needs_binary
@pytest.mark.parametrize(("predicate", "inputs"), PREDICATE_CASES)
def test_inferred_predicate_inputs_are_the_compiled_artifact_inputs(
    tmp_path: Path, predicate: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    path = write_predicate_module(root, predicate)

    assert inferred_input_names(root) == inputs
    assert artifact_inputs(root, path, tmp_path / "artifact.json") == inputs


RANDOM_NAMES = [
    "wages", "age", "then", "in", "_", "period_start", "period_end",
    "household/size", "single", "max", "floor", "sum", "len",
    "local_rate", "is_eligible", "local_table", "literal_rule", "negative_rule",
    "person_pay", "person_is_adult", "member_of_household", "cites_section_63",
]  # fmt: skip
RANDOM_FIELDS = [
    "age", "wages", "period_start", "_", "then", "local_rate", "local_table",
    "person_pay", "person_is_adult", "member_of_household", "cites_section_63",
]  # fmt: skip


def random_formula(rng: random.Random, depth: int) -> str:
    """A formula over the shapes the cases above pin, nested at random."""

    def name() -> str:
        text = rng.choice(RANDOM_NAMES)
        return f"({text})" if rng.random() < 0.15 else text

    def operand() -> str:
        roll = rng.random()
        if depth == 0 or roll < 0.4:
            return name()
        if roll < 0.5:
            return rng.choice(["1", "2.5", '"a"', "True"])
        return random_formula(rng, depth - 1)

    relation = rng.choice(["member_of_household", "(member_of_household)"])
    shapes = [
        lambda: f"{operand()}{rng.choice(['+', '-', '*', '/', ' / '])}{operand()}",
        lambda: f"{rng.choice(['ceil', 'floor'])}({operand()})",
        lambda: f"{rng.choice(['max', 'min'])}({operand()}, {operand()})",
        lambda: f"days_between({operand()}, {operand()})",
        lambda: f"len({relation}{rng.choice(['', '.' + rng.choice(RANDOM_FIELDS)])})",
        lambda: f"len({relation}, {operand()})",
        lambda: f"sum({relation}.{rng.choice(RANDOM_FIELDS)}, {operand()})",
        lambda: f"local_table[{operand()}]",
        lambda: f"count_where({relation}, {name()})",
        lambda: f"sum_where({relation}, {name()}, {name()})",
        lambda: f"{rng.choice(['sum', 'max', 'only'])}({relation}.{rng.choice(RANDOM_FIELDS)})",
        lambda: (
            f"(match {name()}: {rng.choice(['1', 'single', 'period_end'])} => "
            f"{operand()}{rng.choice(['', f' _ => {operand()}'])})"
        ),
        lambda: f"(match {operand()}: _ => {operand()})",
        lambda: f"(if {operand()}: {operand()} elif {operand()}: {operand()} else: {operand()})",
        lambda: f"{operand()} {rng.choice(['>=', '==', 'and', 'or not'])} {operand()}",
    ]
    return rng.choice(shapes)()


@needs_dense
@pytest.mark.parametrize("seed", range(4))
def test_random_formulas_infer_the_compiled_program_inputs(
    tmp_path: Path, seed: int
) -> None:
    # For every formula the compiler accepts, under the first result dtype it
    # accepts, inference names exactly the program's inputs.
    rng = random.Random(seed)
    root = canonical_root(tmp_path)
    compiled_formulas = 0
    for _ in range(50):
        formula = random_formula(rng, rng.randint(1, 2))
        for dtype in ("Money", "Judgment", "Integer", "Date"):
            path = write_formula_module(
                root, {"called": formula}, dtype=dtype, rules=DECLARED_RULES
            )
            try:
                program = CompiledDenseProgram.from_file(
                    path, rulespec_roots=[root], entity="Household"
                )
            except ValueError:
                continue
            compiled_formulas += 1
            compiled = dense_inputs(program)
            assert inferred_input_names(root) == compiled, formula
            assert set(program.input_request_names) - {LOCAL_TABLE_LABEL} == compiled, formula
            assert all(
                f"us:statutes/7/2014#input.{name}" in program.input_request_names[name]
                for name in compiled
            ), formula
            break
    assert compiled_formulas >= 25


def random_predicate(
    rng: random.Random, depth: int, terms: tuple[str, ...] = ("anchor", "candidate")
) -> str:
    """A predicate over terms, atoms and `exists`, with scalar formulas in it."""

    def term() -> str:
        return rng.choice(terms)

    def read() -> str:
        return f"{term()}.{rng.choice(RANDOM_FIELDS)}"

    def inner(scope: tuple[str, ...] = terms) -> str:
        return random_predicate(rng, depth - 1, scope) if depth else read()

    variable = f"v{len(terms)}"
    shapes = [
        read,
        lambda: rng.choice(RANDOM_NAMES),
        lambda: f"{term()} {rng.choice(['==', '!='])} {term()}",
        lambda: f"relative_of({term()}, {term()})",
        lambda: f"{read()} {rng.choice(['>=', '=='])} {random_formula(rng, 0)}",
        lambda: f"{random_formula(rng, 1)} {rng.choice(['>=', '=='])} 1",
        lambda: f"{inner()} {rng.choice(['and', 'or'])} {inner()}",
        lambda: f"not ({inner()})",
        lambda: f"exactly_one({inner()}, {inner()})",
        lambda: f"exists(relative_of({term()}, {variable}), {inner((*terms, variable))})",
        lambda: f"exists(relative_of({variable}, {term()}), {inner((*terms, variable))})",
    ]
    return rng.choice(shapes)()


@needs_binary
@pytest.mark.parametrize("seed", range(4))
def test_random_predicates_infer_the_compiled_artifact_inputs(
    tmp_path: Path, seed: int
) -> None:
    # The dense compiler refuses most predicates, so the artifact is the check.
    rng = random.Random(seed)
    root = canonical_root(tmp_path)
    compiled_predicates = 0
    for _ in range(100):
        predicate = random_predicate(rng, rng.randint(1, 3))
        path = write_predicate_module(root, predicate)
        try:
            compiled = artifact_inputs(root, path, tmp_path / "artifact.json")
        except RuntimeError:
            continue
        compiled_predicates += 1
        assert inferred_input_names(root) == compiled, predicate
    assert compiled_predicates >= 25


# What mutation adds to a formula: every kind of token the lexer yields, and
# some it refuses.
RANDOM_TOKENS = [
    "wages", "a/b", "_", "1", "2.5", '"s"', "True", "2026-01-01", "if", "elif",
    "else", "match", "and", "or", "not", "from", "to", "entity", "amend", "(",
    ")", "[", "]", ",", ".", ":", "=", "=>", "->", "+", "-", "*", "/", "<",
    "<=", "==", "!=", ">", ">=", "$", "!", "'open",
]  # fmt: skip


def mutated(rng: random.Random, formula: str) -> str:
    tokens = re.findall(r"(?:[^\s()\[\],:.]|(?<=\d)\.(?=\d))+|[()\[\],:.]", formula)
    for _ in range(rng.choice((1, 1, 2))):
        index = rng.randrange(len(tokens) + 1)
        roll = rng.random()
        if roll < 0.4 and index < len(tokens):
            del tokens[index]
        elif roll < 0.7 and index < len(tokens):
            tokens[index] = rng.choice(RANDOM_TOKENS)
        else:
            tokens.insert(index, rng.choice(RANDOM_TOKENS))
    return " ".join(tokens) or "1"


def engine_parses(root: Path, path: Path) -> bool:
    try:
        CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")
    except ValueError as error:
        return not re.search("formula parse error|has no formula version", str(error))
    return True


@needs_dense
@pytest.mark.parametrize("seed", range(4))
def test_the_parser_accepts_what_the_engine_parses(tmp_path: Path, seed: int) -> None:
    rng = random.Random(seed)
    root = canonical_root(tmp_path)
    accepted = refused = 0
    for _ in range(500):
        formula = mutated(rng, random_formula(rng, rng.randint(0, 2)))
        path = write_formula_module(root, {"called": formula})
        parses = engine_parses(root, path)
        assert (_parse_formula(formula).expr is not None) == parses, formula
        accepted += parses
        refused += not parses
    assert accepted >= 15
    assert refused >= 100


PARSER_EDGES = [
    # Numbers
    "1_000", "1__0", "1_", "1_.5", "1._5", "1.5_", "1.", "1.x", "x + 1.", "1.5.2", "0x10",
    ".5", "x + .5", "007", "1_000.5_5", "1 .5", "1. 5", "9223372036854775807",
    # Numbers `serde_yaml` reads from a bare scalar
    "1e5", "1E5", "1e-5", "1e+5", "1.5e3", "-1e5", ".5e3", "5.e3", "x + 1e5",
    # Dates
    "2026-01-01", "x == 2026-01-01", "2026-1-1", "2026-01-011", "12026-01-01",
    "2026-01-0", "2026-01", "2026_01-01-01",
    # Names and paths
    "a/b", "a/b/c", "a/1", "a/ b", "a /b", "a//b", "a/b/", "/a", "_", "__", "_1", "a1",
    "1a", "a.b", "a.b.c", "a/b.c", "a.b/c", "a.1", "a._", "a._b", "é", "x é", "aé",
    # Strings
    '"s"', "'s'", '"s', "'s", '"', "'", '"a\\"b"', "'a\\'b'", '"a\\', '""', "''",
    '"a" "b"', '"a\nb"', '"""', '"""x', '"""x"""', 'x """y"""', '"""y""" x',
    '"""x""" + """y"""', '""""', '"""""', '""""""', '"""""""',
    # Comments
    "x # c", "# c", "# c\nx", "x # c\n+ y", "x #", "#", 'x # "unclosed', 'x # """',
    # Whitespace
    "x\t+\ty", "x\n+\ny", "x\r\n+ y", "x\x0c+ y", "x\x0b+ y", "x\xa0+ y", "\xa0", " ",
    " ", "\n", "x + y",
    # Keywords in odd places
    "if", "else", "elif", "match", "and", "or", "not", "from", "to", "entity", "amend",
    "true", "True", "TRUE", "false", "False", "not not x", "not -x", "- not x", "--x",
    "x and", "and x", "x or or y", "x and not y", "x.if", "x.true", "x.not", "if.x",
    "true.x", "(true).x", "1 . x", "(1).x", '"s".x',
    # Operators
    "x = 1", "x == 1", "x != 1", "x ! 1", "!x", "x -> y", "x => y", "x <= y", "x < = y",
    "x >= y", "x > = y", "x < y < z", "x == y == z", "(x < y) < z", "x + - y", "x - - y",
    "x * - y", "x / / y", "x // y", "x / y", "x/y", "x /y", "x/ y", "x/1", "x/ 1", "x / 1",
    "x%y", "x^y", "x & y", "x | y", "x ~ y", "x ? y : z", "x;", "x; y", "{x}", "@x", "x$",
    # Grouping, calls and indexes
    "()", "(x", "x)", "(x))", "((x)", "[x]", "x[", "x[]", "x[1", "x[1]", "x[1][2]",
    "x[1].y", "x.y[1]", "x(", "x()", "x(1", "x(1,", "x(1,)", "x(,)", "x(,1)", "x(1,,2)",
    "x(y)(z)", "(x)(y)", "((x))(y)", "x.y(z)", "x[1](z)", "f(x).y", "f(x)[1]", "(x)[1]",
    "(x).y", "(x + y).z", "(x + y)[1]", "(x + y)(z)", "x y", "x 1", "1 x", "1 2", '"s" x',
    "x,", ",x", "x, y",
    # Let
    "x = y = 1", "a/b = 1", "_ = 1", "(x = 1)", "if a: x = 1 else: 2", "f(x = 1)", "1 = 2",
    "if = 1", "x == = 1",
    # Conditionals
    "if a: 1", "if a: 1 else 2", "if a 1 else: 2", "if a: 1 else: 2", "if a: 1 elif b: 2",
    "if a: 1 elif b: 2 else: 3", "if a: 1 else: 2 else: 3", "if a: 1 elif: 2 else: 3",
    "elif a: 1", "else: 1", "if: 1 else: 2", "if a: else: 2", "if a: 1 else:",
    "if if a: 1 else: 2: 3 else: 4", "1 + if a: 2 else: 3", "(if a: 2 else: 3) + 1",
    "if a: 2 else: 3 + 1", "not if a: 1 else: 2", "-if a: 1 else: 2",
    "f(if a: 1 else: 2)", "x[if a: 1 else: 2]", "if a: match b: _ => 1 else: 2",
    "if a: 1 else: match b: _ => 2",
    # Match
    "match", "match x", "match x:", "match x: _ => 1", "match x: 1 => 2",
    "match x: 1 => 2 _ => 3", "match x: _ => 1 _ => 2", "match x: _ => 1 1 => 2",
    "match x: a => 1", "match x: a/b => 1", "match x: true => 1", "match x: True => 1",
    "match x: 'a' => 1", "match x: 1.5 => 1", "match x: -1 => 1", "match x: (1) => 1",
    "match x: 1 + 1 => 1", "match x: a.b => 1", "match x: 1 2", "match x: 1 =>",
    "match x: => 1", "match : _ => 1", "match x _ => 1", "match x: _ -> 1",
    "match x, y: _ => 1", "match x: 1 => 2, 3 => 4", "match x: if => 1",
    "match if a: 1 else: 2: _ => 3", "match (if a: 1 else: 2): _ => 3",
    "match match x: _ => 1: _ => 2", "match (match x: _ => 1): _ => 2",
    "(match x: _ => 1) + 1", "match x: _ => 1 + 1", "match x: _ => match y: _ => 1",
    "match x: 1 => match y: 2 => 3 _ => 4", "1 + match x: _ => 1", "not match x: _ => 1",
    "f(match x: _ => 1)", "x[match y: _ => 1]", "match x: 1 => 2 y", "match x: 1 => 2 3",
    "match x: _ => 1 y", "match x: 2026-01-01 => 1", "match x: and => 1",
    # Declarations, which a formula refuses
    "x from 2030-01-01: y", "x\nother:\n  y", "from 2030-01-01: y", "entity Person",
    "amend x", "x status: foo",
]  # fmt: skip


@needs_dense
@pytest.mark.parametrize("formula", PARSER_EDGES)
def test_the_parser_agrees_with_the_engine_at_the_edges(tmp_path: Path, formula: str) -> None:
    root = canonical_root(tmp_path)
    path = write_formula_module(root, {"called": formula})

    assert (_parse_formula(formula).expr is not None) == engine_parses(root, path)


# The engine refuses these when it converts the number, which the parser leaves
# to it. They name no input either way.
@needs_dense
@pytest.mark.parametrize(
    ("formula", "refusal"),
    [
        ("9223372036854775808", "number too large"),
        ("99999999999999999999999999999.5", "Invalid decimal"),
    ],
)
def test_only_the_engine_refuses_a_number_too_large(
    tmp_path: Path, formula: str, refusal: str
) -> None:
    root = canonical_root(tmp_path)
    path = write_formula_module(root, {"called": formula})

    assert _parse_formula(formula).expr is not None
    assert inferred_input_names(root) == set()
    with pytest.raises(ValueError, match=refusal):
        CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")


@needs_binary
@pytest.mark.parametrize(
    ("scalar", "inputs"),
    [("yes", {"yes"}), ("no", {"no"}), ("on", {"on"}), ("off", {"off"})],
)
def test_pyyaml_reads_these_words_as_booleans_where_the_engine_reads_names(
    tmp_path: Path, scalar: str, inputs: set[str]
) -> None:
    # Intended, for now: discovery sees only what PyYAML loaded, so it can't tell
    # a bare `yes` from `true`. No corpus formula is one of these words.
    root = canonical_root(tmp_path)
    path = write_rulespec(
        root,
        "us/statutes/7/2014.yaml",
        f"""
format: rulespec/v1
rules:
  - name: called
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: {scalar}
""",
    )

    assert inferred_input_names(root) == set()
    assert artifact_inputs(root, path, tmp_path / "artifact.json") == inputs


# A formula the parser refuses has no reads to follow, and no program to check
# them against: every undeclared name it does not call counts.
@pytest.mark.parametrize(
    ("formula", "inputs", "refusal"),
    [
        ("x = 1", {"x"}, "let-bindings are not supported"),
        ("f(x)(y)", {"x", "y"}, "can only call named functions"),
        ("max(x) y", {"x", "y"}, "unexpected trailing token"),
        ("x.from", {"x"}, "expected Ident, got From"),
        ("match x: a/b => 1", {"a/b", "x"}, "unexpected trailing token"),
        ("match x: _ => 1 2 => 3", {"_", "x"}, "wildcard arm must be final"),
        ("a < b < c", {"a", "b", "c"}, "unexpected trailing token"),
        ("1 + if x: 2 else: 3", {"x"}, "unexpected token in expression"),
        ("x + 'open", {"x"}, "unterminated string"),
        ("x $", {"x"}, "unexpected char"),
        ("x y", {"x", "y"}, "unexpected trailing token"),
    ],
)
def test_a_refused_formula_counts_every_undeclared_name_it_does_not_call(
    tmp_path: Path, formula: str, inputs: set[str], refusal: str
) -> None:
    root = canonical_root(tmp_path)
    path = write_formula_module(root, {"called": formula})

    assert inferred_input_names(root) == inputs
    if NativeCompiledDenseProgram is not None:
        with pytest.raises(ValueError, match=refusal):
            CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")


@pytest.mark.parametrize(
    ("formula", "inputs"),
    [
        (
            f"max({', '.join(f'x{index}' for index in range(4_000))})",
            {f"x{index}" for index in range(4_000)},
        ),
        ("if a: 1 " + "elif a: 1 " * 4_000 + "else: 1", {"a"}),
        ("a" + " + a" * 4_000, {"a"}),
        # The parser loops over this chain and the reads recurse down it, so the
        # stack ends in the reads: every name counts, as in a refused formula.
        ("a" + ".b" * 4_000, {"a", "b"}),
        ("(" * 4_000 + "a" + ")" * 4_000, {"a"}),
        ("if a: 1 else: " * 4_000 + "1", {"a"}),
        ("-" * 4_000 + "a", {"a"}),
    ],
    ids=[
        "wide call",
        "elif chain",
        "operator chain",
        "field chain",
        "parentheses",
        "else if",
        "negation",
    ],
)
def test_long_and_deep_formulas_keep_their_names(formula: str, inputs: set[str]) -> None:
    rules = [{"name": "r", "kind": "derived", "versions": [{"formula": formula}]}]

    assert _infer_input_names(rules) == inputs


def test_a_formula_nested_past_the_recursion_limit_counts_every_undeclared_name() -> None:
    # The parser gives up where Python's stack ends, not where the engine's does,
    # so this counts `s` where a shallower formula counts nothing.
    def inputs(depth: int) -> set[str]:
        formula = "(" * depth + "len(s)" + ")" * depth
        return _infer_input_names(
            [{"name": "r", "kind": "derived", "versions": [{"formula": formula}]}]
        )

    assert inputs(50) == set()
    assert inputs(4_000) == {"s"}


@needs_dense
def test_a_table_label_is_a_request_name_discovery_does_not_name(tmp_path: Path) -> None:
    # `input_catalog` accepts a table's `indexed_by` label as a request name even
    # with no formula reading it. Discovery names the reads, so it omits this one.
    root = canonical_root(tmp_path)
    write_rate_table_module(root)
    path = write_formula_module(
        root, {"called": "rate_table[2] * 2"}, imports=[RATE_TABLE_MODULE]
    )

    program = CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")

    assert inferred_input_names(root) == set()
    assert dense_inputs(program) == set()
    assert program.input_request_names == {
        "household_size": (f"{RATE_TABLE_MODULE}#input.household_size",)
    }


def test_data_relation_and_table_formulas_read_nothing() -> None:
    # Lowering reads neither: a data relation has no body and a table lowers to
    # its values.
    rules = [
        {"name": "held", "kind": "data_relation", "versions": [{"formula": "a"}]},
        {
            "name": "table",
            "kind": "parameter",
            "versions": [{"values": {1: 1}}, {"formula": "b"}],
        },
    ]

    assert _infer_input_names(rules) == set()


def test_top_level_formula_reads_like_a_version() -> None:
    # `effective_versions` in src/rulespec.rs: a rule with no versions has one.
    rules = [
        {"name": "rate", "kind": "parameter", "formula": 0.5},
        {"name": "total", "kind": "derived", "formula": "rate * wages"},
    ]

    assert _infer_input_names(rules) == {"wages"}


@needs_dense
def test_an_import_may_name_one_rule_of_its_module(tmp_path: Path) -> None:
    # `resolve_import_target` in src/rulespec.rs takes one `#fragment` with no `/`.
    root = canonical_root(tmp_path)
    write_rate_table_module(root)
    formulas = {"called": "rate_table[2] * 2"}

    path = write_formula_module(root, formulas, imports=[f"{RATE_TABLE_MODULE}#rate_table"])
    program = CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")
    assert inferred_input_names(root) == set()
    assert dense_inputs(program) == set()

    path = write_formula_module(root, formulas, imports=[f"{RATE_TABLE_MODULE}#a/b"])
    with pytest.raises(ValueError, match="import"):
        CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Household")
    with pytest.raises(ValueError, match="exact absolute canonical atomic targets"):
        discover_concepts([root])


def test_every_discovered_concept_id_validates(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_formula_module(root, {"household/rate": "household/size * 2"})

    ids = {concept.concept_id for concept in discover_concepts([root])}

    assert {
        "us:statutes/7/2014#household/rate",
        "us:statutes/7/2014#input.household/size",
    } <= ids
    for concept_id in ids:
        assert validate_concept_id([root], concept_id).valid, concept_id


@needs_dense
def test_a_path_that_names_a_rule_is_compiled_as_the_rule(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    path = write_formula_module(root, PATH_RULES)

    program = CompiledDenseProgram.from_file(
        path, rulespec_roots=[root], entity="Household"
    )

    assert "household/size" in program.output_names
    assert set(program.root_inputs) == {"wages"}


def test_search_show_and_validate_concept(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    matches = search_concepts([root], "adjusted gross", limit=5)
    concept = show_concept([root], "us:statutes/26/62#adjusted_gross_income")
    valid = validate_concept_id([root], "us:statutes/26/62#adjusted_gross_income")

    assert matches[0].concept_id == "us:statutes/26/62#adjusted_gross_income"
    assert concept is not None
    assert concept.label == "adjusted gross income"
    assert valid.valid
    assert valid.concept is not None


def test_validate_reports_missing_fragment_and_suggestions(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    result = validate_concept_id([root], "us:statutes/26/62#taxable_income")

    assert not result.valid
    assert result.errors[0]["code"] == "missing_fragment"
    assert {
        suggestion.concept_id for suggestion in result.suggestions
    } >= {"us:statutes/26/62#adjusted_gross_income"}


@pytest.mark.parametrize(
    "concept_id",
    [
        "us:/statutes/26/62#adjusted_gross_income",
        "us:statutes//26/62#adjusted_gross_income",
        "us:statutes/../62#adjusted_gross_income",
        "us:programs/snap#benefit",
        "us:statutes/26/62.yaml#adjusted_gross_income",
    ],
)
def test_validate_rejects_noncanonical_concept_ids(
    tmp_path: Path, concept_id: str
) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    result = validate_concept_id([root], concept_id)

    assert not result.valid
    assert result.errors[0]["code"] == "malformed_concept_id"


def test_cli_concepts_search_json(tmp_path: Path, capsys) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    rc = main(
        [
            "concepts",
            "search",
            "adjusted gross",
            "--root",
            str(root),
            "--json",
        ]
    )
    captured = capsys.readouterr()
    payload = json.loads(captured.out)

    assert rc == 0
    assert payload[0]["concept_id"] == "us:statutes/26/62#adjusted_gross_income"


def test_cli_concepts_validate_json_failure(tmp_path: Path, capsys) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/statutes/26/62.yaml", rulespec_body())

    rc = main(
        [
            "concepts",
            "validate",
            "us:statutes/26/63#taxable_income",
            "--root",
            str(root),
            "--json",
        ]
    )
    captured = capsys.readouterr()
    payload = json.loads(captured.out)

    assert rc == 1
    assert payload["valid"] is False
    assert payload["errors"][0]["code"] == "missing_provision"


def test_concepts_use_four_atomic_roots_and_exclude_programs(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/legislation/act.yaml", rulespec_body())
    write_rulespec(root, "us/programs/not-atomic.yaml", rulespec_body())

    ids = {concept.concept_id for concept in discover_concepts([root])}
    assert "us:legislation/act" in ids
    assert all(":programs/" not in concept_id for concept_id in ids)


def test_concepts_reject_yml_wrong_country_and_missing_explicit_root(
    tmp_path: Path,
) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/policies/legacy.yml", rulespec_body())
    with pytest.raises(ValueError, match="exact .yaml"):
        discover_concepts([root])

    wrong_root = (tmp_path / "wrong" / "rulespec-us")
    write_rulespec(wrong_root, "uk/policies/rule.yaml", rulespec_body())
    wrong_root = wrong_root.resolve()
    with pytest.raises(ValueError, match="does not match country"):
        discover_concepts([wrong_root])

    with pytest.raises(SystemExit) as error:
        main(["concepts", "search", "adjusted gross", "--json"])
    assert error.value.code == 2


@pytest.mark.parametrize(
    ("body", "message"),
    [
        ("format: [", "invalid RuleSpec YAML"),
        ("[]", "root must be a mapping"),
        ("schema: axiom.rules.module.v1\nrules: []", "requires exact format"),
        (
            "format: rulespec/v1\nschema: axiom.rules.module.v1\nrules: []",
            "schema discriminator was removed",
        ),
        ("format: wrong/v1\nrules: []", "requires exact format"),
        ("format: rulespec/v1\nrules: nope", "rules must be a list"),
        (
            "format: rulespec/v1\nrules:\n  - kind: parameter\n    formula: '1'\n    effective_from: 2020-01-01",
            "requires a nonempty name",
        ),
        (
            "format: rulespec/v1\nrules:\n  - name: p\n    kind: unknown",
            "missing or unsupported kind",
        ),
        (
            "format: rulespec/v1\nrules:\n  - name: p\n    kind: parameter",
            "has no formula",
        ),
        (
            "format: rulespec/v1\nmodule:\n  kind: composition\nrules: []",
            "must not declare module.kind",
        ),
        (
            "format: rulespec/v1\nmodule:\n  id: us:policies/other\nrules: []",
            "module.id was removed",
        ),
        (
            "format: rulespec/v1\nextends: us:policies/base\nrules: []",
            "extends was removed",
        ),
        (
            "format: rulespec/v1\nimports: [../base]\nrules: []",
            "exact absolute canonical atomic targets",
        ),
        (
            "format: rulespec/v1\nimports: [us:policies/base.YAML]\nrules: []",
            "exact absolute canonical atomic targets",
        ),
        (
            "format: rulespec/v1\nimports: ['us:policies/base#rate/year']\nrules: []",
            "exact absolute canonical atomic targets",
        ),
        (
            "format: rulespec/v1\nimports: ['us:policies/base#a#b']\nrules: []",
            "exact absolute canonical atomic targets",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_paths: [us/statute/26/62]\nrules: []",
            "unknown fields",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/statute/26/62\n    upstream_source_check: {}\nrules: []",
            "must contain exactly status, checked_paths, and rationale",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/guidance/treasury/rate\n    upstream_source_check:\n      status: official_parameter_source\n      checked_paths: [42]\n      rationale: checked\nrules: []",
            "checked_paths must be a list of strings",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/guidance/treasury/rate\n    upstream_source_check:\n      status: official_parameter_source\n      checked_paths: [us/statute/26/62]\n      rationale: checked\n      note: typo\nrules: []",
            "must contain exactly status, checked_paths, and rationale",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: us/statute\nrules: []",
            "requires one canonical corpus_citation_path",
        ),
        (
            "format: rulespec/v1\nrules:\n  - name: p\n    metadata:\n      proof:\n        source:\n          corpus_citation_paths: [us/statute/26/62]",
            "plural corpus_citation_paths",
        ),
        (
            "format: rulespec/v1\nrules:\n  - name: p\n    metadata:\n      proof:\n        source:\n          corpus_citation_path: us/statute/26/62\n          source_sha256: bad",
            "source_sha256 must be 64 hexadecimal characters",
        ),
        (
            "format: rulespec/v1\nmodule:\n  source_verification:\n    corpus_citation_path: 'us/statute/26/62 /a'\nrules: []",
            "canonical corpus_citation_path",
        ),
    ],
)
def test_concepts_fail_closed_on_invalid_atomic_modules(
    tmp_path: Path, body: str, message: str
) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/policies/invalid.yaml", body)
    with pytest.raises(ValueError, match=message):
        discover_concepts([root])


@pytest.mark.parametrize("name", ["double.yaml.yaml", "legacy.YAML", "legacy.Yml"])
def test_concepts_reject_ambiguous_or_case_variant_yaml_names(
    tmp_path: Path, name: str
) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, f"us/policies/{name}", rulespec_body())
    with pytest.raises(ValueError, match="double extension|exact .yaml"):
        discover_concepts([root])


def test_concepts_reject_noncanonical_empty_directories(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    (root / "us/policies/bad directory").mkdir(parents=True)
    with pytest.raises(ValueError, match="non-canonical RuleSpec path component"):
        discover_concepts([root])


def test_concepts_ignore_companion_test_yaml(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(root, "us/policies/base.yaml", rulespec_body())
    write_rulespec(root, "us/policies/base.test.yaml", "format: [")
    assert discover_concepts([root])


def test_concepts_accept_multi_segment_corpus_jurisdictions(tmp_path: Path) -> None:
    root = tmp_path / "rulespec-uk"
    write_rulespec(
        root,
        "uk-kingston-upon-thames/policies/ctr/2026.yaml",
        """
format: rulespec/v1
module:
  source_verification:
    corpus_citation_path: uk-kingston-upon-thames/regulation/ctr/2026
rules: []
""",
    )
    concepts = discover_concepts([root.resolve()])
    assert concepts[0].concept_id == "uk-kingston-upon-thames:policies/ctr/2026"


def test_concepts_accept_typed_upstream_source_check(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    write_rulespec(
        root,
        "us/policies/treasury/rate.yaml",
        """
format: rulespec/v1
module:
  source_verification:
    corpus_citation_path: us/guidance/treasury/rate
    upstream_source_check:
      status: official_parameter_source
      checked_paths:
        - us/statute/26/62
      rationale: The official guidance supplies the current annual parameter.
rules: []
""",
    )
    concepts = discover_concepts([root])
    assert concepts[0].concept_id == "us:policies/treasury/rate"


def test_concepts_reject_case_aliased_root_on_case_insensitive_filesystems(
    tmp_path: Path,
) -> None:
    actual = tmp_path / "RULESPEC-US"
    write_rulespec(actual, "us/policies/base.yaml", rulespec_body())
    alias = tmp_path / "rulespec-us"
    if not alias.exists():
        pytest.skip("filesystem is case-sensitive")
    with pytest.raises(
        ValueError,
        match="case-aliased content root|component casing is aliased",
    ):
        discover_concepts([alias])


def test_concepts_reject_case_variant_content_root(tmp_path: Path) -> None:
    root = tmp_path / "rulespec-us"
    write_rulespec(root, "us/Policies/base.yaml", rulespec_body())
    with pytest.raises(
        ValueError,
        match="case-aliased content root|component casing is aliased",
    ):
        discover_concepts([root.resolve()])
