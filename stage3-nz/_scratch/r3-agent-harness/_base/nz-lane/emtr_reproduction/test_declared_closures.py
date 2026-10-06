"""Mutants proving declared eligibility closures are mandatory inputs."""

import importlib.util
import json
import sys
from pathlib import Path

import pytest


HERE = Path(__file__).resolve().parent


def _harness():
    spec = importlib.util.spec_from_file_location("nz_emtr_reproduction", HERE / "run.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_missing_declared_closures_fail(tmp_path):
    harness = _harness()
    with pytest.raises(harness.HarnessError, match="eligibility closures"):
        harness.load_eligibility_closures(tmp_path / "absent.json")


def test_incomplete_declared_closures_fail(tmp_path):
    harness = _harness()
    mutant = json.loads((HERE / "eligibility-closures.json").read_text())
    del mutant["choices"]["work_tests"][
        "iwtc_allowed_paye_income_payment_when_wages_positive"
    ]
    path = tmp_path / "incomplete.json"
    path.write_text(json.dumps(mutant))
    with pytest.raises(harness.HarnessError, match="work_tests"):
        harness.load_eligibility_closures(path)
