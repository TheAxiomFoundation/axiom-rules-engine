"""Concept discovery for repo-backed RuleSpec modules."""

from __future__ import annotations

import json
import os
import re
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable

import yaml

FILESYSTEM_ROOTS = (
    "legislation",
    "policies",
    "programs",
    "regulations",
    "statutes",
)
ATOMIC_ROOTS = ("legislation", "policies", "regulations", "statutes")
COUNTRY_ROOT_RE = re.compile(r"^rulespec-(?P<country>[a-z]{2})$")
JURISDICTION_RE = re.compile(r"^(?P<country>[a-z]{2})(?:-[a-z0-9]+)*$")
CONCEPT_ID_RE = re.compile(
    r"^(?P<prefix>[a-z]{2}(?:-[a-z0-9]+)*):(?P<path>[A-Za-z0-9_.~/-]+)"
    r"(?:#(?P<fragment>[A-Za-z0-9_.-]+))?$"
)
CORPUS_CITATION_PATH_RE = re.compile(
    r"^[a-z]{2,3}(?:-[a-z0-9]+)*/"
    r"[a-z][a-z0-9-]*"
    r"(?:/[A-Za-z0-9](?:[A-Za-z0-9 .:\-–]*[A-Za-z0-9.:\-–])?)+$"
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
    """Discover concepts under explicit canonical country checkouts."""
    concepts: dict[str, ConceptRecord] = {}
    for root_path, country in _canonical_country_roots(roots):
        for path in _discover_rulespec_files(root_path, country):
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
    parsed = CONCEPT_ID_RE.fullmatch(concept_id)
    if parsed is None or not _is_canonical_import(
        f"{parsed.group('prefix')}:{parsed.group('path')}"
    ):
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

    base_id = f"{parsed.group('prefix')}:{parsed.group('path')}"
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


def _canonical_country_roots(
    roots: Iterable[str | Path],
) -> list[tuple[Path, str]]:
    raw_roots = [Path(root) for root in roots]
    if not raw_roots:
        raise ValueError("at least one explicit rulespec-<country> root is required")
    validated: list[tuple[Path, str]] = []
    seen_paths: set[Path] = set()
    seen_countries: set[str] = set()
    for root in raw_roots:
        if not root.is_absolute():
            raise ValueError(f"RuleSpec root must be absolute: {root}")
        if root.is_symlink() or not root.is_dir():
            raise ValueError(f"RuleSpec root must be a real directory: {root}")
        resolved = root.resolve(strict=True)
        if str(resolved) != str(root):
            raise ValueError(
                f"RuleSpec root must be unaliased: {root}; use {resolved}"
            )
        _require_exact_component_spelling(root)
        match = COUNTRY_ROOT_RE.fullmatch(root.name)
        if match is None:
            raise ValueError(
                f"RuleSpec root must be named exactly rulespec-<country>: {root}"
            )
        country = match.group("country")
        if root in seen_paths:
            raise ValueError(f"duplicate RuleSpec root: {root}")
        for existing, _ in validated:
            if root.is_relative_to(existing) or existing.is_relative_to(root):
                raise ValueError(
                    f"overlapping RuleSpec roots are forbidden: {existing}, {root}"
                )
        if country in seen_countries:
            raise ValueError(f"duplicate RuleSpec country: {country}")
        seen_paths.add(root)
        seen_countries.add(country)
        validated.append((root, country))
    return validated


def _require_exact_component_spelling(path: Path) -> None:
    """Reject case aliases even on case-insensitive filesystems."""
    cursor = Path(path.anchor)
    for component in path.parts[1:]:
        try:
            names = {entry.name for entry in os.scandir(cursor)}
        except OSError as error:
            raise ValueError(f"cannot inspect RuleSpec root component {cursor}: {error}") from error
        if component not in names:
            raise ValueError(
                f"RuleSpec root component casing is aliased at {cursor / component}"
            )
        cursor /= component


def _discover_rulespec_files(root: Path, country: str) -> list[Path]:
    for content_root in FILESYSTEM_ROOTS:
        root_level = root / content_root
        if root_level.exists() or root_level.is_symlink():
            raise ValueError(f"root-level content is forbidden: {root_level}")

    files: list[Path] = []
    jurisdiction_count = 0
    content_root_count = 0
    for jurisdiction in sorted(root.iterdir()):
        if jurisdiction.is_symlink():
            raise ValueError(f"symlink is forbidden: {jurisdiction}")
        lowercase_name = jurisdiction.name.lower()
        if jurisdiction.name != lowercase_name and (
            lowercase_name in FILESYSTEM_ROOTS
            or JURISDICTION_RE.fullmatch(lowercase_name) is not None
        ):
            raise ValueError(f"case-aliased reserved path is forbidden: {jurisdiction}")
        if not jurisdiction.is_dir():
            continue
        match = JURISDICTION_RE.fullmatch(jurisdiction.name)
        if match is None:
            continue
        if match.group("country") != country:
            raise ValueError(
                f"jurisdiction {jurisdiction.name!r} does not match country {country!r}"
            )
        jurisdiction_count += 1
        for entry in jurisdiction.iterdir():
            lowercase_name = entry.name.lower()
            if entry.name != lowercase_name and lowercase_name in FILESYSTEM_ROOTS:
                raise ValueError(f"case-aliased content root is forbidden: {entry}")
        for content_root in FILESYSTEM_ROOTS:
            base = jurisdiction / content_root
            if base.is_symlink():
                raise ValueError(f"symlink is forbidden: {base}")
            if not base.exists():
                continue
            if not base.is_dir():
                raise ValueError(f"content root must be a directory: {base}")
            _require_exact_component_spelling(base)
            content_root_count += 1
            for path in base.rglob("*"):
                if path.is_symlink():
                    raise ValueError(f"symlink is forbidden: {path}")
                for component in path.relative_to(base).parts:
                    if any(character.isspace() for character in component) or any(
                        character in "#:\"'\\" for character in component
                    ) or re.fullmatch(r"[A-Za-z0-9_.~-]+", component) is None:
                        raise ValueError(
                            f"non-canonical RuleSpec path component {component!r}: {path}"
                        )
                if path.is_dir():
                    continue
                if not path.is_file():
                    raise ValueError(f"special path is forbidden: {path}")
                if path.suffix.lower() in {".yaml", ".yml"} and path.suffix != ".yaml":
                    raise ValueError(f"YAML files must use exact .yaml: {path}")
                if path.suffix == ".yaml" and Path(path.stem).suffix.lower() in {
                    ".yaml",
                    ".yml",
                }:
                    raise ValueError(f"ambiguous YAML-like double extension: {path}")
                if (
                    content_root in ATOMIC_ROOTS
                    and path.suffix == ".yaml"
                    and _is_rulespec_file(path)
                ):
                    files.append(path)
    if jurisdiction_count == 0 or content_root_count == 0:
        raise ValueError(
            f"empty RuleSpec root {root}: expected a matching jurisdiction "
            "with a canonical content root"
        )
    return sorted(files)


def _is_rulespec_file(path: Path) -> bool:
    return path.is_file() and not path.name.endswith(".test.yaml")


def _concepts_from_rulespec_file(root: Path, path: Path) -> list[ConceptRecord]:
    document = _load_yaml_mapping(path)
    base_id = _base_concept_id(root, path)
    _validate_atomic_document(document, base_id, path)
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


def _load_yaml_mapping(path: Path) -> dict[str, Any]:
    try:
        payload = yaml.safe_load(path.read_text())
    except (OSError, yaml.YAMLError) as error:
        raise ValueError(f"invalid RuleSpec YAML {path}: {error}") from error
    if not isinstance(payload, dict):
        raise ValueError(f"RuleSpec root must be a mapping: {path}")
    return payload


def _has_rulespec_discriminator(document: dict[str, Any]) -> bool:
    return document.get("format") == "rulespec/v1"


def _validate_atomic_document(
    document: dict[str, Any], base_id: str, path: Path
) -> None:
    if not _has_rulespec_discriminator(document):
        raise ValueError(f"RuleSpec requires exact format: rulespec/v1: {path}")
    if "extends" in document:
        raise ValueError(f"top-level extends was removed: {path}")
    if "schema" in document:
        raise ValueError(f"top-level schema discriminator was removed: {path}")
    if document.get("relations") not in (None, []):
        raise ValueError(f"top-level relations are forbidden: {path}")

    imports = document.get("imports", [])
    if imports is None:
        imports = []
    if not isinstance(imports, list) or not all(
        isinstance(target, str) and _is_canonical_import(target)
        for target in imports
    ):
        raise ValueError(f"imports must be exact absolute canonical atomic targets: {path}")

    module_value = document.get("module")
    if module_value is None:
        module: dict[str, Any] = {}
    elif isinstance(module_value, dict):
        module = module_value
    else:
        raise ValueError(f"module must be a mapping: {path}")
    if "kind" in module:
        raise ValueError(f"atomic RuleSpec modules must not declare module.kind: {path}")
    if "id" in module:
        raise ValueError(f"module.id was removed; path identity is canonical: {path}")

    verification = module.get("source_verification")
    if verification is not None:
        if not isinstance(verification, dict):
            raise ValueError(f"source_verification must be an exact mapping: {path}")
        if set(verification) - {
            "corpus_citation_path",
            "source_sha256",
            "upstream_source_check",
        }:
            raise ValueError(f"source_verification contains unknown fields: {path}")
        citation = verification.get("corpus_citation_path")
        if not isinstance(citation, str) or CORPUS_CITATION_PATH_RE.fullmatch(citation) is None:
            raise ValueError(
                f"source_verification requires one canonical corpus_citation_path: {path}"
            )
        digest = verification.get("source_sha256")
        if digest is not None and (
            not isinstance(digest, str)
            or re.fullmatch(r"[0-9A-Fa-f]{64}", digest) is None
        ):
            raise ValueError(f"source_sha256 must be 64 hexadecimal characters: {path}")
        upstream_check = verification.get("upstream_source_check")
        if upstream_check is not None:
            if not isinstance(upstream_check, dict) or set(upstream_check) != {
                "status",
                "checked_paths",
                "rationale",
            }:
                raise ValueError(
                    "upstream_source_check must contain exactly status, "
                    f"checked_paths, and rationale: {path}"
                )
            if not isinstance(upstream_check["status"], str):
                raise ValueError(f"upstream_source_check status must be a string: {path}")
            checked_paths = upstream_check["checked_paths"]
            if not isinstance(checked_paths, list) or not all(
                isinstance(checked_path, str) for checked_path in checked_paths
            ):
                raise ValueError(
                    f"upstream_source_check checked_paths must be a list of strings: {path}"
                )
            if not isinstance(upstream_check["rationale"], str):
                raise ValueError(
                    f"upstream_source_check rationale must be a string: {path}"
                )

    _validate_recursive_citation_fields(document, path)
    _validate_rule_shapes(document.get("rules"), path)


def _validate_rule_shapes(rules: Any, path: Path) -> None:
    if rules is None:
        return
    if not isinstance(rules, list):
        raise ValueError(f"rules must be a list: {path}")
    supported = {
        "parameter",
        "derived",
        "data_relation",
        "derived_relation",
        "source_relation",
    }
    for index, rule in enumerate(rules):
        if not isinstance(rule, dict):
            raise ValueError(f"rules[{index}] must be a mapping: {path}")
        if not isinstance(rule.get("name"), str) or not rule["name"]:
            raise ValueError(f"rules[{index}] requires a nonempty name: {path}")
        kind = rule.get("kind")
        if kind not in supported:
            raise ValueError(f"rules[{index}] has missing or unsupported kind: {path}")
        if kind == "data_relation":
            relation = rule.get("data_relation")
            if not isinstance(relation, dict) or not isinstance(
                relation.get("arity"), int
            ):
                raise ValueError(
                    f"data_relation rules require data_relation.arity: {path}"
                )
        elif kind == "derived_relation":
            relation = rule.get("derived_relation")
            if (
                not isinstance(relation, dict)
                or not isinstance(relation.get("arity"), int)
                or not isinstance(relation.get("source_relation"), str)
            ):
                raise ValueError(
                    "derived_relation rules require arity and source_relation: "
                    f"{path}"
                )
            if not _has_executable_formula(rule):
                raise ValueError(f"derived_relation rule has no formula: {path}")
        elif kind == "source_relation":
            relation = rule.get("source_relation")
            if (
                not isinstance(relation, dict)
                or not isinstance(relation.get("type"), str)
                or not isinstance(relation.get("target"), str)
            ):
                raise ValueError(
                    f"source_relation rules require type and target: {path}"
                )
        elif not _has_executable_formula(rule):
            raise ValueError(f"{kind} rule {rule['name']!r} has no formula: {path}")


def _has_executable_formula(rule: dict[str, Any]) -> bool:
    if "formula" in rule and (
        rule.get("effective_from") is not None or rule.get("from") is not None
    ):
        return True
    versions = rule.get("versions")
    if not isinstance(versions, list) or not versions:
        return False
    return all(
        isinstance(version, dict)
        and (
            version.get("effective_from") is not None
            or version.get("from") is not None
        )
        and (
            version.get("formula") is not None
            or (
                rule.get("kind") == "parameter"
                and isinstance(version.get("values"), dict)
                and bool(version["values"])
            )
        )
        for version in versions
    )


def _is_canonical_import(target: str) -> bool:
    match = CONCEPT_ID_RE.fullmatch(target)
    if match is None or any(character.isspace() for character in target):
        return False
    parts = match.group("path").split("/")
    terminal = parts[-1].lower()
    return (
        len(parts) >= 2
        and parts[0] in ATOMIC_ROOTS
        and not terminal.endswith((".yaml", ".yml"))
        and not terminal.endswith(".test")
        and all(part not in {"", ".", ".."} for part in parts)
    )


def _validate_recursive_citation_fields(value: Any, path: Path) -> None:
    if isinstance(value, dict):
        if "corpus_citation_paths" in value:
            raise ValueError(f"plural corpus_citation_paths was removed: {path}")
        if "corpus_citation_path" in value:
            citation = value["corpus_citation_path"]
            if (
                not isinstance(citation, str)
                or CORPUS_CITATION_PATH_RE.fullmatch(citation) is None
            ):
                raise ValueError(f"non-canonical corpus_citation_path: {path}")
        if "source_sha256" in value:
            digest = value["source_sha256"]
            if (
                not isinstance(digest, str)
                or re.fullmatch(r"[0-9A-Fa-f]{64}", digest) is None
            ):
                raise ValueError(f"source_sha256 must be 64 hexadecimal characters: {path}")
        for nested in value.values():
            _validate_recursive_citation_fields(nested, path)
    elif isinstance(value, list):
        for nested in value:
            _validate_recursive_citation_fields(nested, path)


def _base_concept_id(root: Path, path: Path) -> str:
    relative = path.relative_to(root).with_suffix("")
    jurisdiction, *module_path = relative.parts
    return f"{jurisdiction}:{Path(*module_path).as_posix()}"


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
