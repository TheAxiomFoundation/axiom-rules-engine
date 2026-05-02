from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Sequence

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
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
