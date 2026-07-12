from __future__ import annotations

import importlib

import pytest

from axiom_rules_engine.cli import main


def test_deleted_source_registry_cannot_be_imported() -> None:
    with pytest.raises(ModuleNotFoundError):
        importlib.import_module("axiom_rules_engine.source_registry")


def test_cli_rejects_removed_check_sources_command() -> None:
    with pytest.raises(SystemExit) as error:
        main(["check-sources"])

    assert error.value.code == 2
