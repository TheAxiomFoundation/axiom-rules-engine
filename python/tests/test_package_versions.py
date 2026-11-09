"""The engine, the dense extension and the Python wrapper carry one version.

A release attaches the dense extension's and the wrapper's wheels beside the
CLI archives (.github/workflows/build-python-wheels.yml), and each wheel's file
name and installed metadata carry its package's version. A cut that bumps the
engine in Cargo.toml bumps python-ext/Cargo.toml (with python-ext/Cargo.lock)
and python/pyproject.toml with it, so a deployment's installed versions name
the release its wheels came from.
"""

from __future__ import annotations

import importlib.metadata
import re
import tomllib
from pathlib import Path

import pytest

from axiom_rules_engine.dense import NativeCompiledDenseProgram

REPO = Path(__file__).resolve().parents[2]

needs_extension = pytest.mark.skipif(
    NativeCompiledDenseProgram is None,
    reason="axiom_rules_engine_dense extension is not built",
)


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


@needs_extension
def test_dense_module_reports_the_versions_it_was_built_with() -> None:
    import axiom_rules_engine_dense as dense

    engine = _version(REPO / "Cargo.toml", "package")
    assert {
        "__version__": dense.__version__,
        "ENGINE_VERSION": dense.ENGINE_VERSION,
        "installed metadata": importlib.metadata.version("axiom-rules-engine-dense"),
    } == {
        "__version__": _version(REPO / "python-ext" / "Cargo.toml", "package"),
        "ENGINE_VERSION": engine,
        "installed metadata": engine,
    }


@needs_extension
def test_dense_module_reports_its_engine_build() -> None:
    import axiom_rules_engine_dense as dense

    build = dense.engine_build()
    assert set(build) == {"commit", "dirty"}
    if build["commit"] is None:
        assert build["dirty"] is None
    else:
        assert re.fullmatch(r"[0-9a-f]{40}", build["commit"])
        assert build["dirty"] in (True, False, None)
    # Each call is a fresh dict, so a caller cannot change what the next sees.
    build["commit"] = "changed"
    assert dense.engine_build()["commit"] != "changed"
