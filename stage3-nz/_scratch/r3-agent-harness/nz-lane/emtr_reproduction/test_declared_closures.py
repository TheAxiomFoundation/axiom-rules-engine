"""Mutants proving declared eligibility closures are mandatory inputs."""

import ast
import importlib.util
import inspect
import json
import subprocess
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


def test_missing_separate_mc10_principal_caregiver_choice_fails(tmp_path):
    harness = _harness()
    mutant = json.loads((HERE / "eligibility-closures.json").read_text())
    del mutant["choices"]["care"][
        "iwtc_child_is_principal_caregiver_when_present"
    ]
    path = tmp_path / "missing-mc10-care.json"
    path.write_text(json.dumps(mutant))
    with pytest.raises(harness.HarnessError, match="care"):
        harness.load_eligibility_closures(path)


def test_missing_mc9_age_18_choice_fails(tmp_path):
    harness = _harness()
    mutant = json.loads((HERE / "eligibility-closures.json").read_text())
    del mutant["choices"]["age_18"][
        "attending_school_or_tertiary_at_age_18"
    ]
    path = tmp_path / "missing-mc9-age-18.json"
    path.write_text(json.dumps(mutant))
    with pytest.raises(harness.HarnessError, match="age_18"):
        harness.load_eligibility_closures(path)


def test_aggregation_plan_must_be_the_pinned_checkout_fixture(tmp_path):
    harness = _harness()
    root = tmp_path / "engine"
    canonical = (
        root
        / "tests"
        / "fixtures"
        / "unit_derivation"
        / "nz_income_explorer_family.yaml"
    )
    canonical.parent.mkdir(parents=True)
    canonical.write_text("canonical")
    foreign = tmp_path / "foreign.yaml"
    foreign.write_text("mutant")
    assert harness.verify_aggregation_plan_binding(root, canonical) == canonical
    with pytest.raises(harness.HarnessError, match="canonical plan"):
        harness.verify_aggregation_plan_binding(root, foreign)


def test_engine_preflight_rejects_dirty_checkout(tmp_path, monkeypatch):
    harness = _harness()
    root = tmp_path / "engine"
    binary = root / "target" / "release" / "axiom-rules-engine"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"reviewed-binary")
    monkeypatch.setattr(harness, "git_sha", lambda _root: "reviewed-commit")
    monkeypatch.setattr(harness, "git_dirty", lambda _root: True)
    monkeypatch.setattr(harness, "EXPECTED_ENGINE_SHA", "reviewed-commit")
    with pytest.raises(harness.HarnessError, match="tracked or untracked"):
        harness.verify_engine_binding(root, binary)


def test_engine_preflight_rejects_wrong_commit(tmp_path, monkeypatch):
    harness = _harness()
    root = tmp_path / "engine"
    binary = root / "target" / "release" / "axiom-rules-engine"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"reviewed-binary")
    monkeypatch.setattr(harness, "git_sha", lambda _root: "wrong-parent")
    monkeypatch.setattr(harness, "EXPECTED_ENGINE_SHA", "implementation-commit")
    with pytest.raises(harness.HarnessError, match="engine SHA mismatch"):
        harness.verify_engine_binding(root, binary)


def test_engine_preflight_rejects_unbound_binary(tmp_path, monkeypatch):
    harness = _harness()
    root = tmp_path / "engine"
    binary = root / "target" / "release" / "axiom-rules-engine"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"mutant-binary")
    monkeypatch.setattr(harness, "git_sha", lambda _root: "reviewed-commit")
    monkeypatch.setattr(harness, "git_dirty", lambda _root: False)
    monkeypatch.setattr(harness, "EXPECTED_ENGINE_SHA", "reviewed-commit")
    monkeypatch.setattr(harness, "EXPECTED_ENGINE_BINARY_SHA256", "0" * 64)
    with pytest.raises(harness.HarnessError, match="binary SHA-256 mismatch"):
        harness.verify_engine_binding(root, binary)


def test_runtime_uses_compiled_registered_artifact_not_raw_plan(tmp_path, monkeypatch):
    harness = _harness()
    artifact = tmp_path / "aggregation.json"
    artifact.write_text("{}")
    engine = harness.Engine(
        binary=tmp_path / "axiom-rules-engine",
        artifact=tmp_path / "composition.json",
        aggregation_artifact=artifact,
        catalog={"slot": "canonical"},
    )
    observed = []

    family = {
        "id": "Family:test",
        "members": ["primary"],
        "partner_present": {"status": "determined", "value": False},
        "scalars": {},
        "counts": {},
        "predicates": {},
        "children": [],
    }

    def fake_run(argv, **_kwargs):
        observed.append(argv)
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout=json.dumps(
                {
                    "schema": "axiom/unit-aggregation-plan-stage3/1",
                    "families": {
                        "status": "determined",
                        "value": [family],
                    },
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(harness, "run_checked", fake_run)
    assert engine.aggregate({"request": "known"})["id"] == "Family:test"
    assert "--artifact" in observed[0]
    assert "--plan" not in observed[0]


def test_compile_boundary_materializes_registered_aggregation_artifact(
    tmp_path, monkeypatch
):
    harness = _harness()
    composition_artifact = tmp_path / "composition.json"
    aggregation_artifact = tmp_path / "aggregation.json"
    commands = []

    def fake_run(argv, **_kwargs):
        commands.append(argv)
        if "compile-composed" in argv:
            composition_artifact.write_text(
                json.dumps(
                    {
                        "metadata": {
                            "input_catalog": [
                                {
                                    "slot": "taxable_income",
                                    "canonical_request_name": "canonical-taxable-income",
                                }
                            ]
                        }
                    }
                )
            )
        elif "compile-unit-aggregation" in argv:
            aggregation_artifact.write_text(
                json.dumps(
                    {
                        "format": "axiom/compiled-unit-aggregation-stage3/1",
                        "plan_digest": "sha256:registered",
                        "phase_two_artifact": {"format_version": 2},
                    }
                )
            )
        return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")

    monkeypatch.setattr(harness, "run_checked", fake_run)
    engine, _compiled, _stdout = harness.Engine.compile(
        binary=tmp_path / "axiom-rules-engine",
        composition=tmp_path / "composition.yaml",
        rulespec_root=tmp_path / "rulespec-nz",
        artifact=composition_artifact,
        aggregation_plan=tmp_path / "aggregation.yaml",
        aggregation_artifact=aggregation_artifact,
    )
    assert engine.aggregation_artifact == aggregation_artifact
    assert any("compile-unit-aggregation" in command for command in commands)


def test_indeterminate_aggregation_result_never_becomes_a_default():
    harness = _harness()
    with pytest.raises(harness.HarnessError, match="indeterminate"):
        harness.determined(
            {"status": "indeterminate", "reasons": ["child:age:unknown"]},
            "child_count",
        )


def test_operational_code_reads_scenario_relationship_shape_only_at_boundary():
    harness = _harness()
    tree = ast.parse(inspect.getsource(harness.ModelEvaluator))
    evaluator = tree.body[0]
    assert isinstance(evaluator, ast.ClassDef)
    forbidden = (
        "scenario.partnered",
        "scenario.children",
        "scenario.gross_wage2",
    )
    for method in evaluator.body:
        if not isinstance(method, ast.FunctionDef) or method.name == "aggregate_family":
            continue
        source = ast.unparse(method)
        assert all(term not in source for term in forbidden), method.name


def test_engine_outputs_are_consumed_for_every_migrated_reduction():
    harness = _harness()
    family_credit = inspect.getsource(harness.ModelEvaluator.family_credit_values)
    benefit = inspect.getsource(harness.ModelEvaluator.benefit_components)
    best_start = inspect.getsource(harness.ModelEvaluator.best_start_total)
    sweep = inspect.getsource(harness.sweep_scenario)
    state = inspect.getsource(harness.ModelEvaluator.evaluate_state)
    coverage = inspect.getsource(harness.validate_expanded_coverage)
    assert "family_tax_credit_subsequent_dependent_child_care_units" in family_credit
    assert "max(0, child_count - 1)" not in family_credit
    assert "jss_sole_parent_youngest_age_at_least_14" in benefit
    assert "youngest >= 14" not in benefit
    assert "best_start_total_continuous_abatement" in best_start
    assert "family_scheme_income - self.best_start_abatement_threshold" not in best_start
    assert 'scalar_decimal(state, "weekly_family_wages")' in sweep
    assert "scenario.gross_wage2" not in sweep
    for output in ("child_count_age_0_13", "child_count_age_14_18"):
        assert output in state
        assert output in coverage


def test_request_boundary_supplies_roles_evidence_legal_facts_and_observations():
    harness = _harness()

    class RecordingEngine:
        def __init__(self):
            self.request = None

        def aggregate(self, request):
            self.request = request
            return {"engine": "boundary-only"}

    engine = RecordingEngine()
    evaluator = object.__new__(harness.ModelEvaluator)
    evaluator.engine = engine
    evaluator.best_start_abatement_threshold = harness.Decimal("79000")
    evaluator.best_start_abatement_rate = harness.Decimal("0.21")
    evaluator.closures = {
        "care": {
            "best_start_child_care_fraction": 0.5,
            "iwtc_child_exclusive_care_fraction": 0.75,
            "iwtc_child_is_principal_caregiver_when_present": True,
            "wff_commissioner_considers_primary_day_to_day_care_when_child_present": True,
        },
        "age_18": {
            "not_financially_independent_at_age_18": False,
            "attending_school_or_tertiary_at_age_18": True,
            "commissioner_period_contains_segment_at_age_18": False,
        },
    }
    scenario = harness.Scenario(
        id="boundary",
        description="request boundary",
        partnered=True,
        wage1_hourly=harness.Decimal("25"),
        children=(1, 18),
        gross_wage2=harness.Decimal("740"),
        hours2=harness.Decimal("20"),
        accommodation_costs=harness.Decimal("0"),
        accommodation_rent=True,
        accommodation_area=1,
        accommodation_boarder=False,
        weekly_board_and_lodgings_paid=harness.Decimal("0"),
        sampled_wages=(0,),
    )
    evaluator.aggregate_family(
        scenario,
        child_scalars={
            0: {"best_start_family_abatement": harness.Decimal("1000")},
            1: {"best_start_family_abatement": harness.Decimal("1001")},
        },
        family_scheme_income=harness.Decimal("90480"),
    )
    request = engine.request
    assert request is not None
    assert "named_people" not in request
    assert "family_scalars" not in request
    assert request["roster_completeness"]["id"]
    assert request["segment_completeness"]["id"]
    assert [person["role"] for person in request["persons"]] == [
        "adult",
        "adult",
        "child",
        "child",
    ]
    partner = request["persons"][1]
    assert partner["scalars"]["weekly_wage"]["value"] == "740"
    for child in request["persons"][2:]:
        assert child["scalars"][
            "best_start_claimant_care_fraction"
        ]["value"] == "0.5"
        assert child["scalars"][
            "in_work_tax_credit_child_exclusive_care_fraction"
        ]["value"] == "0.75"
        assert child["facts"][
            "principal_care_for_family_scheme"
        ]["value"] is True
        assert child["facts"][
            "in_work_tax_credit_principal_caregiver"
        ]["value"] is True
        assert {
            "not_financially_independent_at_age_18",
            "attending_school_or_tertiary_at_age_18",
            "commissioner_period_contains_segment_at_age_18",
        } <= set(child["facts"])
        assert child["facts"][
            "not_financially_independent_at_age_18"
        ]["value"] is False
        assert child["facts"][
            "attending_school_or_tertiary_at_age_18"
        ]["value"] is True
        assert child["facts"][
            "commissioner_period_contains_segment_at_age_18"
        ]["value"] is False
    assert all(relation["completeness"]["id"] for relation in request["relations"])
    assert all(
        fact["knowledge"]["evidence"]["id"]
        for relation in request["relations"]
        for fact in relation["facts"]
    )
    family_input = request["family_inputs"][0]
    assert family_input["anchor_person"] == "boundary:person:1"
    assert family_input["named_people"][
        "mc7_entitlement_holder"
    ]["value"] == "boundary:person:1"
    adjustment = family_input["scalars"]["best_start_family_abatement"]
    assert adjustment["status"] == "observations"
    assert [item["value"] for item in adjustment["observations"]] == [
        "1000",
        "1001",
    ]
