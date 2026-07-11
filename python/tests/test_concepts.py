from __future__ import annotations

import json
from pathlib import Path

import pytest

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
            "unknown fields",
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
