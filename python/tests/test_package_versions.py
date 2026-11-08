"""The engine, the dense extension and the Python wrapper carry one version.

A release attaches the dense extension's and the wrapper's wheels beside the
CLI archives (.github/workflows/build-python-wheels.yml), and each wheel's file
name and installed metadata carry its package's version. A cut that bumps the
engine in Cargo.toml bumps python-ext/Cargo.toml (with python-ext/Cargo.lock)
and python/pyproject.toml with it, so a deployment's installed versions name
the release its wheels came from.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def _version(path: Path, table: str) -> str:
    with path.open("rb") as file:
        return tomllib.load(file)[table]["version"]


def test_extension_and_wrapper_carry_the_engine_version() -> None:
    engine = _version(REPO / "Cargo.toml", "package")
    dense = _version(REPO / "python-ext" / "Cargo.toml", "package")
    wrapper = _version(REPO / "python" / "pyproject.toml", "project")

    assert {"python-ext/Cargo.toml": dense, "python/pyproject.toml": wrapper} == {
        "python-ext/Cargo.toml": engine,
        "python/pyproject.toml": engine,
    }
