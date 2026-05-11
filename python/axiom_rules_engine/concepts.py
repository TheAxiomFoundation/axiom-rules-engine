"""Concept discovery for repo-backed RuleSpec modules."""

from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable

import yaml

TAXONOMY_ROOTS = ("statutes", "regulations", "policies")
CONCEPT_ID_RE = re.compile(
    r"^(?P<prefix>[a-z][a-z0-9_-]*):(?P<path>[A-Za-z0-9_.~/-]+)"
    r"(?:#(?P<fragment>[A-Za-z0-9_.-]+))?$"
)
IDENTIFIER_RE = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")
FORMULA_KEYWORDS = {
    "and",
    "as",
    "case",
    "else",
    "false",
    "if",
    "in",
    "is",
    "let",
    "match",
    "max",
    "min",
    "not",
    "or",
    "round",
    "sum",
    "then",
    "true",
}


@dataclass(frozen=True)
class ConceptRecord:
    """One public concept ID discovered from a RuleSpec repo."""

    concept_id: str
    label: str
    kind: str
    status: str
    source_file: str
    citation: str | None = None
    aliases: tuple[str, ...] = ()
    effective_periods: tuple[str, ...] = ()
    entity: str | None = None
    dtype: str | None = None
    unit: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Return a JSON-serializable concept record."""
        payload = asdict(self)
        return {key: value for key, value in payload.items() if value not in (None, (), [])}


@dataclass(frozen=True)
class ConceptValidation:
    """Validation result for one concept ID."""

    concept_id: str
    valid: bool
    errors: tuple[dict[str, str], ...] = ()
    concept: ConceptRecord | None = None
    suggestions: tuple[ConceptRecord, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        """Return a JSON-serializable validation payload."""
        payload: dict[str, Any] = {
            "concept_id": self.concept_id,
            "valid": self.valid,
            "errors": list(self.errors),
        }
        if self.concept is not None:
            payload["concept"] = self.concept.to_dict()
        if self.suggestions:
            payload["suggestions"] = [
                suggestion.to_dict() for suggestion in self.suggestions
            ]
        return payload


def discover_concepts(
    roots: Iterable[str | Path],
) -> list[ConceptRecord]:
    """Discover public concept IDs from RuleSpec files under repo roots."""
    concepts: dict[str, ConceptRecord] = {}
    for root in roots:
        root_path = Path(root)
        for path in _discover_rulespec_files(root_path):
            for concept in _concepts_from_rulespec_file(root_path, path):
                concepts.setdefault(concept.concept_id, concept)
    return sorted(concepts.values(), key=lambda concept: concept.concept_id)


def search_concepts(
    roots: Iterable[str | Path],
    query: str,
    *,
    limit: int = 20,
) -> list[ConceptRecord]:
    """Search concepts by ID, label, citation, aliases, and source file."""
    terms = [term.casefold() for term in query.split() if term.strip()]
    if not terms:
        return []
    matches: list[tuple[int, ConceptRecord]] = []
    for concept in discover_concepts(roots):
        haystack = _search_text(concept)
        if all(term in haystack for term in terms):
            matches.append((_search_score(concept, terms), concept))
    return [
        concept
        for _, concept in sorted(
            matches,
            key=lambda item: (-item[0], item[1].concept_id),
        )[:limit]
    ]


def list_concepts(
    roots: Iterable[str | Path],
    *,
    namespace: str | None = None,
    limit: int | None = None,
) -> list[ConceptRecord]:
    """List discovered concepts, optionally filtered by concept namespace."""
    concepts = discover_concepts(roots)
    if namespace:
        concepts = [
            concept
            for concept in concepts
            if concept.concept_id == namespace
            or concept.concept_id.startswith(f"{namespace}/")
            or concept.concept_id.startswith(f"{namespace}#")
        ]
    if limit is not None:
        return concepts[:limit]
    return concepts


def show_concept(
    roots: Iterable[str | Path],
    concept_id: str,
) -> ConceptRecord | None:
    """Return a concept record by exact concept ID."""
    return {concept.concept_id: concept for concept in discover_concepts(roots)}.get(
        concept_id
    )


def validate_concept_id(
    roots: Iterable[str | Path],
    concept_id: str,
) -> ConceptValidation:
    """Validate concept ID syntax and existence in configured RuleSpec repos."""
    parsed = CONCEPT_ID_RE.match(concept_id)
    if parsed is None:
        return ConceptValidation(
            concept_id=concept_id,
            valid=False,
            errors=(
                {
                    "code": "malformed_concept_id",
                    "message": "Concept ID must look like `us:statutes/26/62#term`.",
                },
            ),
        )

    concepts = discover_concepts(roots)
    by_id = {concept.concept_id: concept for concept in concepts}
    if concept_id in by_id:
        return ConceptValidation(
            concept_id=concept_id,
            valid=True,
            concept=by_id[concept_id],
        )

    base_id = f"{parsed.group('prefix')}:{parsed.group('path').strip('/')}"
    if parsed.group("fragment") is not None and base_id not in by_id:
        suggestions = tuple(_same_base_or_nearby(concepts, base_id))
        return ConceptValidation(
            concept_id=concept_id,
            valid=False,
            errors=(
                {
                    "code": "missing_provision",
                    "message": f"Concept provision `{base_id}` was not found.",
                },
            ),
            suggestions=suggestions,
        )
    if parsed.group("fragment") is not None:
        suggestions = tuple(
            concept for concept in concepts if concept.concept_id.startswith(f"{base_id}#")
        )
        return ConceptValidation(
            concept_id=concept_id,
            valid=False,
            errors=(
                {
                    "code": "missing_fragment",
                    "message": f"Provision `{base_id}` exists, but fragment was not found.",
                },
            ),
            suggestions=suggestions[:10],
        )

    suggestions = tuple(_same_base_or_nearby(concepts, base_id))
    return ConceptValidation(
        concept_id=concept_id,
        valid=False,
        errors=(
            {
                "code": "missing_concept",
                "message": f"Concept `{concept_id}` was not found.",
            },
        ),
        suggestions=suggestions,
    )


def concepts_to_json(concepts: Iterable[ConceptRecord]) -> str:
    """Serialize concept records as stable pretty JSON."""
    return json.dumps(
        [concept.to_dict() for concept in concepts],
        indent=2,
        sort_keys=True,
    )


def _discover_rulespec_files(root: Path) -> list[Path]:
    if not root.exists():
        return []
    files: list[Path] = []
    for taxonomy_root in TAXONOMY_ROOTS:
        base = root / taxonomy_root
        if not base.exists():
            continue
        files.extend(path for path in base.rglob("*.yaml") if _is_rulespec_file(path))
        files.extend(path for path in base.rglob("*.yml") if _is_rulespec_file(path))
    return sorted(files)


def _is_rulespec_file(path: Path) -> bool:
    return path.is_file() and not path.name.endswith(".test.yaml")


def _concepts_from_rulespec_file(root: Path, path: Path) -> list[ConceptRecord]:
    document = _load_yaml_mapping(path)
    if document is None or not _has_rulespec_discriminator(document):
        return []

    base_id = _base_concept_id(root, path)
    source_file = path.as_posix()
    module = document.get("module") if isinstance(document.get("module"), dict) else {}
    summary = str(module.get("summary") or "")
    citation_path = _corpus_citation_path(module)
    module_concept = ConceptRecord(
        concept_id=base_id,
        label=_module_label(base_id, summary),
        kind="module",
        status="encoded",
        source_file=source_file,
        citation=_citation_from_path(citation_path) or _citation_from_base_id(base_id),
        aliases=_aliases(base_id, summary),
    )
    concepts = [module_concept]

    rules = document.get("rules")
    rule_names: set[str] = set()
    if isinstance(rules, list):
        for rule in rules:
            if not isinstance(rule, dict) or not isinstance(rule.get("name"), str):
                continue
            rule_names.add(rule["name"])
            concepts.append(_rule_concept(base_id, source_file, module_concept.citation, rule))

    for input_name in sorted(_infer_input_names(rules, rule_names)):
        concepts.append(
            ConceptRecord(
                concept_id=f"{base_id}#input.{input_name}",
                label=_humanize(input_name),
                kind="input",
                status="inferred",
                source_file=source_file,
                citation=module_concept.citation,
                aliases=(input_name,),
            )
        )

    return concepts


def _rule_concept(
    base_id: str,
    source_file: str,
    module_citation: str | None,
    rule: dict[str, Any],
) -> ConceptRecord:
    kind = str(rule.get("kind") or "rule")
    fragment = rule["name"]
    if kind == "data_relation":
        fragment = f"relation.{fragment}"
    elif kind == "source_relation":
        fragment = f"source_relation.{fragment}"
    versions = rule.get("versions")
    effective_periods = ()
    if isinstance(versions, list):
        effective_periods = tuple(
            str(version["effective_from"])
            for version in versions
            if isinstance(version, dict) and version.get("effective_from") is not None
        )
    return ConceptRecord(
        concept_id=f"{base_id}#{fragment}",
        label=_humanize(rule["name"]),
        kind=kind,
        status="encoded",
        source_file=source_file,
        citation=str(rule.get("source") or module_citation or ""),
        aliases=(rule["name"],),
        effective_periods=effective_periods,
        entity=_optional_str(rule.get("entity")),
        dtype=_optional_str(rule.get("dtype")),
        unit=_optional_str(rule.get("unit")),
    )


def _infer_input_names(
    rules: Any,
    rule_names: set[str],
) -> set[str]:
    if not isinstance(rules, list):
        return set()
    inputs: set[str] = set()
    for rule in rules:
        if not isinstance(rule, dict):
            continue
        versions = rule.get("versions")
        if not isinstance(versions, list):
            continue
        for version in versions:
            if not isinstance(version, dict) or not isinstance(version.get("formula"), str):
                continue
            for identifier in IDENTIFIER_RE.findall(version["formula"]):
                if (
                    identifier not in rule_names
                    and identifier not in FORMULA_KEYWORDS
                ):
                    inputs.add(identifier)
    return inputs


def _load_yaml_mapping(path: Path) -> dict[str, Any] | None:
    try:
        payload = yaml.safe_load(path.read_text())
    except yaml.YAMLError:
        return None
    return payload if isinstance(payload, dict) else None


def _has_rulespec_discriminator(document: dict[str, Any]) -> bool:
    return document.get("format") == "rulespec/v1" or str(
        document.get("schema") or ""
    ).startswith("axiom.rules")


def _base_concept_id(root: Path, path: Path) -> str:
    root_path = root.resolve()
    prefix = _repo_prefix(root_path)
    relative = path.resolve().relative_to(root_path).with_suffix("")
    return f"{prefix}:{relative.as_posix()}"


def _repo_prefix(root: Path) -> str:
    name = root.name
    return name.removeprefix("rulespec-") if name.startswith("rulespec-") else name


def _corpus_citation_path(module: dict[str, Any]) -> str | None:
    verification = module.get("source_verification")
    if isinstance(verification, dict) and isinstance(
        verification.get("corpus_citation_path"), str
    ):
        return verification["corpus_citation_path"]
    return None


def _citation_from_path(path: str | None) -> str | None:
    if path is None:
        return None
    parts = path.split("/")
    if len(parts) >= 3 and parts[0] == "us" and parts[1] == "statute":
        return f"{parts[2]} USC {'/'.join(parts[3:])}"
    return path


def _citation_from_base_id(concept_id: str) -> str | None:
    base, _, _ = concept_id.partition("#")
    prefix, separator, path = base.partition(":")
    if separator and prefix == "us" and path.startswith("statutes/"):
        parts = path.split("/")
        if len(parts) >= 3:
            return f"{parts[1]} USC {'/'.join(parts[2:])}"
    return None


def _module_label(base_id: str, summary: str) -> str:
    if summary:
        first_line = summary.strip().splitlines()[0].strip()
        if first_line:
            return first_line
    return base_id


def _aliases(concept_id: str, summary: str) -> tuple[str, ...]:
    parts = [concept_id.split("#", 1)[0].split(":", 1)[-1].replace("/", " ")]
    if summary:
        parts.append(summary.strip().splitlines()[0].strip())
    return tuple(part for part in parts if part)


def _search_text(concept: ConceptRecord) -> str:
    values = [
        concept.concept_id,
        concept.label,
        concept.kind,
        concept.status,
        concept.source_file,
        concept.citation or "",
        " ".join(concept.aliases),
    ]
    return " ".join(values).casefold()


def _search_score(concept: ConceptRecord, terms: list[str]) -> int:
    label = concept.label.casefold()
    concept_id = concept.concept_id.casefold()
    score = 0
    if "#" in concept.concept_id:
        score += 10
    if concept.kind != "module":
        score += 5
    if all(term in label for term in terms):
        score += 5
    if all(term in concept_id.replace("_", " ") for term in terms):
        score += 3
    return score


def _same_base_or_nearby(
    concepts: list[ConceptRecord],
    base_id: str,
) -> list[ConceptRecord]:
    namespace = base_id.rsplit("/", 1)[0] if "/" in base_id else base_id
    return [
        concept
        for concept in concepts
        if concept.concept_id == base_id
        or concept.concept_id.startswith(f"{base_id}#")
        or concept.concept_id.startswith(f"{namespace}/")
    ][:10]


def _humanize(value: str) -> str:
    return " ".join(value.replace("_", " ").replace("-", " ").split())


def _optional_str(value: Any) -> str | None:
    return str(value) if value is not None else None
