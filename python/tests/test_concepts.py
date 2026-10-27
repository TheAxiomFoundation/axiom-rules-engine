from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml

from axiom_rules_engine.cli import main
from axiom_rules_engine.concepts import (
    discover_concepts,
    search_concepts,
    show_concept,
    validate_concept_id,
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


def write_formula_module(root: Path, formulas: dict[str, str]) -> None:
    rules: list[dict[str, object]] = [
        {
            "name": "member_of_household",
            "kind": "data_relation",
            "data_relation": {"arity": 2},
        }
    ]
    for name, formula in formulas.items():
        rules.append(
            {
                "name": name,
                "kind": "derived",
                "entity": "Household",
                "dtype": "Money",
                "period": "Month",
                "versions": [{"effective_from": "2026-01-01", "formula": formula}],
            }
        )
    write_rulespec(
        root,
        "us/statutes/7/2014.yaml",
        yaml.safe_dump({"format": "rulespec/v1", "rules": rules}, sort_keys=False),
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


@pytest.mark.parametrize(("formula", "inputs"), FUNCTION_CALLS)
def test_no_function_name_is_inferred_as_an_input(
    tmp_path: Path, formula: str, inputs: set[str]
) -> None:
    root = canonical_root(tmp_path)
    write_formula_module(root, {"called": formula})

    assert inferred_input_names(root) == inputs


# The lexer skips whitespace, `#` comments and `"""` blocks between tokens, so
# the parser still reads `ceil` followed by any of them and `(` as a call.
@pytest.mark.parametrize(
    "separator",
    ["", " ", "\t", "\n  ", "\r\n", "\x0c", " # ...\n  ", ' """...""" '],
)
def test_call_is_seen_past_what_the_lexer_skips(
    tmp_path: Path, separator: str
) -> None:
    root = canonical_root(tmp_path)
    write_formula_module(root, {"rounded": f"ceil{separator}(wages)"})

    assert inferred_input_names(root) == {"wages"}


def test_function_name_read_as_a_value_is_an_input(tmp_path: Path) -> None:
    root = canonical_root(tmp_path)
    # Only a following `(` makes a call: the parser reads `floor` and `sum`
    # here as inputs, and a `(` inside a comment or an unclosed `"""` block
    # is no token.
    write_formula_module(
        root,
        {
            "value_named_like_functions": "ceil(floor) + sum",
            "comment_parenthesis": "max # (\n+ 1",
            "unclosed_block": 'min """ (',
        },
    )

    assert inferred_input_names(root) == {"floor", "sum", "max", "min"}


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
