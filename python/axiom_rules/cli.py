from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Sequence

from .concepts import (
    concepts_to_json,
    list_concepts,
    search_concepts,
    show_concept,
    validate_concept_id,
)
from .source_registry import validate_source_registries


def check_sources(args: argparse.Namespace) -> int:
    roots = [Path(root) for root in args.roots]
    if args.repo and len(roots) != 1:
        print("--repo can only be used with one root", file=sys.stderr)
        return 2
    total_entries = 0
    total_issues = 0
    for root in roots:
        try:
            report = validate_source_registries(
                root,
                repo=args.repo,
                bucket=args.bucket,
                verify_r2=args.verify_r2,
            )
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return 2
        total_entries += len(report.entries)
        total_issues += len(report.issues)
        if report.issues:
            print(f"[FAIL] {root}")
            for issue in report.issues:
                try:
                    issue_path = issue.path.relative_to(root.resolve())
                except ValueError:
                    issue_path = issue.path
                print(f"  - {issue_path}: {issue.message}")
        elif args.verbose:
            print(f"[ok] {root}: {len(report.entries)} source registry file(s)")
            for entry in report.entries:
                print(f"  - {entry.source_id}")
                for artifact in entry.artifacts:
                    print(f"    {artifact.name}: {artifact.r2_path}")

    if total_issues:
        print(f"\nSource registry check failed with {total_issues} issue(s).")
        return 1
    print(f"\nValidated {total_entries} source registry file(s).")
    return 0


def concepts_search(args: argparse.Namespace) -> int:
    concepts = search_concepts(args.roots, args.query, limit=args.limit)
    if args.json:
        print(concepts_to_json(concepts))
        return 0
    for concept in concepts:
        print(f"{concept.concept_id}\t{concept.kind}\t{concept.label}")
    return 0


def concepts_show(args: argparse.Namespace) -> int:
    concept = show_concept(args.roots, args.concept_id)
    if concept is None:
        if args.json:
            print(
                json.dumps(
                    {
                        "concept_id": args.concept_id,
                        "error": "not_found",
                    },
                    indent=2,
                    sort_keys=True,
                )
            )
        else:
            print(f"Concept not found: {args.concept_id}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(concept.to_dict(), indent=2, sort_keys=True))
    else:
        print(f"{concept.concept_id}")
        print(f"  label: {concept.label}")
        print(f"  kind: {concept.kind}")
        print(f"  status: {concept.status}")
        print(f"  source_file: {concept.source_file}")
        if concept.citation:
            print(f"  citation: {concept.citation}")
    return 0


def concepts_validate(args: argparse.Namespace) -> int:
    result = validate_concept_id(args.roots, args.concept_id)
    if args.json:
        print(json.dumps(result.to_dict(), indent=2, sort_keys=True))
    elif result.valid:
        print(f"[ok] {args.concept_id}")
    else:
        print(f"[FAIL] {args.concept_id}")
        for error in result.errors:
            print(f"  - {error['code']}: {error['message']}")
    return 0 if result.valid else 1


def concepts_list(args: argparse.Namespace) -> int:
    concepts = list_concepts(args.roots, namespace=args.namespace, limit=args.limit)
    if args.json:
        print(concepts_to_json(concepts))
        return 0
    for concept in concepts:
        print(f"{concept.concept_id}\t{concept.kind}\t{concept.label}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="python -m axiom_rules.cli")
    subcommands = parser.add_subparsers(dest="command", required=True)

    sources = subcommands.add_parser(
        "check-sources",
        help="Validate jurisdiction-repo sources/**/*.yaml registry files.",
    )
    sources.add_argument(
        "roots",
        nargs="+",
        help="Jurisdiction repository root(s) containing a sources/ tree.",
    )
    sources.add_argument(
        "--repo",
        help="Override the repo ID used for derived source IDs. Only valid with one root.",
    )
    sources.add_argument(
        "--bucket",
        default="axiom-sources",
        help="R2 bucket name used when deriving default artifact paths.",
    )
    sources.add_argument(
        "--verbose",
        action="store_true",
        help="Print derived source IDs and R2 paths for valid entries.",
    )
    sources.add_argument(
        "--verify-r2",
        action="store_true",
        help="Fetch derived R2 objects and verify their SHA-256 hashes.",
    )
    sources.set_defaults(func=check_sources)

    concepts = subcommands.add_parser(
        "concepts",
        help="Search, show, list, and validate public Axiom concept IDs.",
    )
    concept_subcommands = concepts.add_subparsers(
        dest="concept_command",
        required=True,
    )

    def add_concept_roots(command: argparse.ArgumentParser) -> None:
        command.add_argument(
            "--root",
            dest="roots",
            action="append",
            default=None,
            help=(
                "RuleSpec jurisdiction repo root. May be repeated. "
                "Defaults to the current directory."
            ),
        )
        command.add_argument(
            "--json",
            action="store_true",
            help="Emit JSON output.",
        )

    search = concept_subcommands.add_parser(
        "search",
        help="Search concept IDs by text.",
    )
    search.add_argument("query", help="Search text.")
    search.add_argument(
        "--limit",
        type=int,
        default=20,
        help="Maximum number of concepts to return.",
    )
    add_concept_roots(search)
    search.set_defaults(func=concepts_search)

    show = concept_subcommands.add_parser(
        "show",
        help="Show a concept by exact ID.",
    )
    show.add_argument("concept_id", help="Concept ID to show.")
    add_concept_roots(show)
    show.set_defaults(func=concepts_show)

    validate = concept_subcommands.add_parser(
        "validate",
        help="Validate concept ID syntax and existence.",
    )
    validate.add_argument("concept_id", help="Concept ID to validate.")
    add_concept_roots(validate)
    validate.set_defaults(func=concepts_validate)

    concept_list = concept_subcommands.add_parser(
        "list",
        help="List concepts, optionally under a namespace.",
    )
    concept_list.add_argument(
        "--namespace",
        help="Filter by concept namespace, e.g. `us:statutes/26`.",
    )
    concept_list.add_argument(
        "--limit",
        type=int,
        default=None,
        help="Maximum number of concepts to return.",
    )
    add_concept_roots(concept_list)
    concept_list.set_defaults(func=concepts_list)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if getattr(args, "roots", None) is None:
        args.roots = ["."]
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
