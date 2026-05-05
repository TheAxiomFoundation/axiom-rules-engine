from __future__ import annotations

import json
from pathlib import Path

from axiom_rules.cli import main
from axiom_rules.concepts import (
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
    kind: input
    entity: TaxUnit
    dtype: Money
    period: Year
    unit: USD
    versions:
      - effective_from: '2023-01-01'
        formula: wages + interest_income
"""


def test_discover_concepts_from_rulespec_file(tmp_path: Path) -> None:
    root = tmp_path / "rules-us"
    write_rulespec(root, "statutes/26/62.yaml", rulespec_body())

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


def test_search_show_and_validate_concept(tmp_path: Path) -> None:
    root = tmp_path / "rules-us"
    write_rulespec(root, "statutes/26/62.yaml", rulespec_body())

    matches = search_concepts([root], "adjusted gross", limit=5)
    concept = show_concept([root], "us:statutes/26/62#adjusted_gross_income")
    valid = validate_concept_id([root], "us:statutes/26/62#adjusted_gross_income")

    assert matches[0].concept_id == "us:statutes/26/62#adjusted_gross_income"
    assert concept is not None
    assert concept.label == "adjusted gross income"
    assert valid.valid
    assert valid.concept is not None


def test_validate_reports_missing_fragment_and_suggestions(tmp_path: Path) -> None:
    root = tmp_path / "rules-us"
    write_rulespec(root, "statutes/26/62.yaml", rulespec_body())

    result = validate_concept_id([root], "us:statutes/26/62#taxable_income")

    assert not result.valid
    assert result.errors[0]["code"] == "missing_fragment"
    assert {
        suggestion.concept_id for suggestion in result.suggestions
    } >= {"us:statutes/26/62#adjusted_gross_income"}


def test_cli_concepts_search_json(tmp_path: Path, capsys) -> None:
    root = tmp_path / "rules-us"
    write_rulespec(root, "statutes/26/62.yaml", rulespec_body())

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
    root = tmp_path / "rules-us"
    write_rulespec(root, "statutes/26/62.yaml", rulespec_body())

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
