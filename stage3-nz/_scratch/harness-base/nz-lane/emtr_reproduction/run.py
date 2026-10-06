#!/usr/bin/env python3
"""Reproduce the pinned NZ Treasury IncomeExplorer comparison with RuleSpec NZ.

This is intentionally a standard-library-only validation harness.  It compiles
the external composition with the pinned Axiom Rules Engine, evaluates the
statute-grounded modules in explain mode, aligns their periods to Treasury's
weekly output convention, and writes deterministic machine-readable artifacts.

The harness does not alter RuleSpec parameters to resemble the Treasury
forecast vintage.  The only Treasury-specific policy-flow convention retained
on the RuleSpec side is the raw IncomeExplorer IWTC branch threshold; it is
reported explicitly in the output metadata and never injected as a RuleSpec
parameter.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import subprocess
import sys
import tempfile
from collections import Counter
from dataclasses import dataclass
from decimal import Decimal, ROUND_CEILING, getcontext
from pathlib import Path
from typing import Any, Mapping, Sequence


getcontext().prec = 40

HERE = Path(__file__).resolve().parent
FOUNDATION_ROOT = HERE.parents[2]

DEFAULT_RULESPEC_ROOT = (
    FOUNDATION_ROOT / "_axiom-worktrees" / "rulespec-nz-emtr" / "rulespec-nz"
)
DEFAULT_ENGINE_ROOT = FOUNDATION_ROOT / "_worktrees" / "engine-release-clone"
DEFAULT_TREASURY_ROOT = (
    Path.home() / "_axiom-worktrees" / "nz-treasury-income-explorer"
)
DEFAULT_COMPOSITION = HERE / "composition.yaml"
DEFAULT_ELIGIBILITY_CLOSURES = HERE / "eligibility-closures.json"
DEFAULT_ORACLE_GENERATOR = HERE / "generate_treasury_emtr_snapshot_expanded.R"
DEFAULT_RSCRIPT = Path("/usr/local/bin/Rscript")
EXPANDED_ORACLE_NAME = "treasury-emtr-snapshot-expanded.json"

EXPECTED_RULESPEC_SHA = "89a7d25dc03a4d045348620283332de10b1047da"
EXPECTED_ENGINE_SHA = "d59969b53430ae2fd97eb4349d44ad23ce930d85"
EXPECTED_ORACLE_COMMIT = "741a6ca4f5d27b1dc00b43dc395e39ffc4040a4b"
EXPECTED_PARAMETER_FILE = "inst/parameters/TY27_BEFU25.yaml"
EXPECTED_PARAMETER_SHA256 = (
    "de89898c78989e057bde7d006b725fed4615ff32731ad008b96148c5a74b683c"
)
EXPECTED_MODEL_YEAR = 2027
EXPECTED_PERIOD_START = "2026-04-01"
EXPECTED_PERIOD_END = "2027-03-31"
EXPECTED_EXPANDED_GENERATED_AT = "2026-07-29"
EXPECTED_SAMPLE_WAGES = (0, 160, 250, 370, 555, 740, 1000, 1500)
ORIGINAL_SCENARIOS = (
    "single_parent_three_children_area1_rent",
    "couple_two_children_area2_mortgage",
    "couple_one_child_partner_10h_area3_rent",
    "single_no_children_area2_no_housing_costs",
)
EXPANDED_SCENARIOS = (
    "lone_parent_two_teens_jss",
    "couple_two_best_start_children_binding",
    "couple_two_children_dual_full_time",
    "single_childless_ietc_focused",
    "lone_parent_two_children_area4_high_rent_cap",
    "couple_childless_boarder_proxy",
    "large_family_four_children_age_bands",
)
EXPECTED_SCENARIOS = ORIGINAL_SCENARIOS + EXPANDED_SCENARIOS
EXPECTED_ADDITIONAL_WAGES: dict[str, tuple[int, ...]] = {
    **{scenario_id: () for scenario_id in ORIGINAL_SCENARIOS},
    "lone_parent_two_teens_jss": (),
    "couple_two_best_start_children_binding": (
        775,
        776,
        1125,
        1126,
        1476,
        1477,
    ),
    "couple_two_children_dual_full_time": (121, 122),
    "single_childless_ietc_focused": (
        688,
        689,
        692,
        693,
        1265,
        1266,
        1342,
        1343,
    ),
    "lone_parent_two_children_area4_high_rent_cap": (),
    "couple_childless_boarder_proxy": (),
    "large_family_four_children_age_bands": (),
}
EXPECTED_OUTPUT_COLUMNS = (
    "gross_wage1",
    "hours1",
    "gross_wage1_annual",
    "gross_wage2",
    "wage1_tax",
    "wage1_ACC_levy",
    "net_wage1",
    "net_wage",
    "net_benefit",
    "FTC_abated",
    "IWTC_abated",
    "MFTC",
    "IETC_abated",
    "WinterEnergy",
    "BestStart_Total",
    "AS_Amount",
    "WFF_abated",
    "Net_Income",
    "Net_Income_annual",
    "EMTR",
    "RR",
    "PTR",
)
EMTR_COMPONENT_BASE_COLUMNS = (
    "net_wage",
    "net_benefit",
    "WFF_abated",
    "MFTC",
    "IETC_abated",
    "WinterEnergy",
    "BestStart_Total",
    "AS_Amount",
)
TREASURY_EMTR_COMPONENT_COLUMNS = tuple(
    f"EMTR_{column}" for column in EMTR_COMPONENT_BASE_COLUMNS
)

# IncomeExplorer TY27_BEFU25 raw branch:
# FamilyAssistance_IWTC_IncomeThreshold_{Single,Couple} / 52.2.
# This remains a scenario-flow convention only; it is not a RuleSpec parameter.
ORACLE_IWTC_ANNUAL_THRESHOLD = Decimal("1226.7")
ORACLE_IWTC_THRESHOLD_DIVISOR = Decimal("52.2")
ORACLE_IWTC_WEEKLY_THRESHOLD = (
    ORACLE_IWTC_ANNUAL_THRESHOLD / ORACLE_IWTC_THRESHOLD_DIVISOR
)
ORACLE_ACC_WEEKLY_RATE = Decimal("0.0175")

D0 = Decimal(0)
D1 = Decimal(1)
D7 = Decimal(7)
D52 = Decimal(52)
D365 = Decimal(365)
WEEKS_IN_MODEL_YEAR = D365 / D7
ENGINE_DECIMAL_TOLERANCE = Decimal("0.000000000000000001")
ORACLE_SIX_DECIMAL_TOLERANCE = Decimal("0.0000005")

TAX_MODULE = "nz:statutes/income_tax/schedule_1/individual_income_tax"
ACC_MODULE = "nz:regulations/acc/earners_levy"
ELIGIBILITY_MODULE = "nz:statutes/income_tax/family_scheme/eligibility"
FSI_MODULE = "nz:statutes/income_tax/family_scheme/family_scheme_income"
WFF_MODULE = "nz:statutes/income_tax/family_scheme/tax_credits"
IETC_MODULE = "nz:statutes/income_tax/credits/individual_credits"
BENEFIT_MODULE = "nz:statutes/social_security/main_benefits/rates"
WEP_MODULE = "nz:statutes/social_security/winter_energy_payment/core"
AS_MODULE = "nz:statutes/social_security/accommodation_supplement/core"


class HarnessError(RuntimeError):
    """A deterministic precondition or engine-evaluation failure."""


EXPECTED_CLOSURE_KEYS = {
    "residence": {
        "wff_caregiver_disqualifying_status",
        "iwtc_person_new_zealand_resident",
        "iwtc_person_present_in_new_zealand_continuous_months",
        "iwtc_person_resident_under_yd1_on_credit_days",
        "iwtc_person_transitional_resident",
        "iwtc_person_spouse_or_partner_transitional_resident",
        "iwtc_child_new_zealand_resident_when_present",
        "iwtc_child_present_for_entitlement_period_when_present",
        "ietc_person_resident_in_new_zealand",
    },
    "care": {
        "wff_commissioner_considers_primary_day_to_day_care_when_child_present",
        "wff_care_is_temporary",
        "wff_caregiver_lives_apart_when_child_present",
        "iwtc_child_exclusive_care_fraction",
        "best_start_child_care_fraction",
    },
    "work_tests": {
        "iwtc_allowed_paye_income_payment_when_wages_positive",
        "iwtc_normally_earner_when_wages_positive",
        "iwtc_currently_meets_fifth_requirement",
        "mftc_single_full_time_hours_per_week",
        "mftc_couple_full_time_hours_per_week",
    },
    "gates": {
        "iwtc_main_benefit_disqualifies",
        "iwtc_basic_or_independent_grants_received",
        "iwtc_parent_allowance_or_childrens_pension_received",
        "ietc_main_benefit_disqualifies",
        "ietc_wff_disqualifies",
        "ietc_veterans_pension_received",
        "ietc_new_zealand_superannuation_received",
        "ietc_overseas_like_support_received",
    },
}


def load_eligibility_closures(path: Path) -> dict[str, Any]:
    """Load the complete, versioned set of stylised eligibility choices."""

    resolved = require_file(path, "eligibility closures")
    try:
        document = json.loads(resolved.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f"eligibility closures are not valid JSON: {exc}") from exc
    if document.get("schema") != "axiom.nz_incomeexplorer.eligibility_closures.v1":
        raise HarnessError("eligibility closures schema is missing or unsupported")
    if not isinstance(document.get("version"), str) or not document["version"]:
        raise HarnessError("eligibility closures require a non-empty version")
    choices = document.get("choices")
    if not isinstance(choices, dict) or set(choices) != set(EXPECTED_CLOSURE_KEYS):
        raise HarnessError(
            "eligibility closure groups changed: expected "
            f"{sorted(EXPECTED_CLOSURE_KEYS)}, found "
            f"{sorted(choices) if isinstance(choices, dict) else choices!r}"
        )
    for group, expected in EXPECTED_CLOSURE_KEYS.items():
        values = choices[group]
        if not isinstance(values, dict) or set(values) != expected:
            raise HarnessError(
                f"eligibility closure group {group!r} must declare exactly "
                f"{sorted(expected)}, found "
                f"{sorted(values) if isinstance(values, dict) else values!r}"
            )
        for name, value in values.items():
            if name.endswith("hours_per_week"):
                if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                    raise HarnessError(f"eligibility closure {group}.{name} must be a positive integer")
            elif name.endswith("continuous_months"):
                if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                    raise HarnessError(f"eligibility closure {group}.{name} must be a non-negative integer")
            elif name.endswith("care_fraction"):
                if not isinstance(value, int | float) or isinstance(value, bool) or not 0 <= value <= 1:
                    raise HarnessError(f"eligibility closure {group}.{name} must be between 0 and 1")
            elif not isinstance(value, bool):
                raise HarnessError(f"eligibility closure {group}.{name} must be boolean")
    return document


def dec(value: Any) -> Decimal:
    """Convert a JSON/Python numeric value to Decimal without binary drift."""

    if isinstance(value, Decimal):
        return value
    if value is None:
        raise HarnessError("cannot convert null to Decimal")
    return Decimal(str(value))


def decimal_text(value: Decimal) -> str:
    """Stable non-exponent decimal text with insignificant zeros removed."""

    if value.is_zero():
        return "0"
    text = format(value, "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text


def json_number(value: Decimal | None) -> float | None:
    if value is None:
        return None
    return float(value)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_checked(
    argv: Sequence[str],
    *,
    stdin: str | None = None,
    cwd: Path | None = None,
    label: str,
) -> subprocess.CompletedProcess[str]:
    process = subprocess.run(
        list(argv),
        input=stdin,
        text=True,
        capture_output=True,
        cwd=cwd,
        check=False,
    )
    if process.returncode:
        stderr = process.stderr.strip()
        stdout = process.stdout.strip()
        detail = stderr or stdout or f"exit status {process.returncode}"
        raise HarnessError(f"{label} failed: {detail}")
    return process


def git_sha(root: Path) -> str:
    return run_checked(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        label=f"git SHA check for {root}",
    ).stdout.strip()


def git_tracked_dirty(root: Path) -> bool:
    status = run_checked(
        [
            "git",
            "-C",
            str(root),
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
        label=f"git status check for {root}",
    ).stdout
    return bool(status.strip())


def require_file(path: Path, label: str) -> Path:
    resolved = path.expanduser().resolve()
    if not resolved.is_file():
        raise HarnessError(f"{label} does not exist: {resolved}")
    return resolved


def require_directory(path: Path, label: str) -> Path:
    resolved = path.expanduser().resolve()
    if not resolved.is_dir():
        raise HarnessError(f"{label} does not exist: {resolved}")
    return resolved


def output_id(module: str, rule: str) -> str:
    return f"{module}#{rule}"


@dataclass(frozen=True)
class Provenance:
    oracle_path: Path
    oracle_sha256: str
    baseline_oracle_path: Path
    baseline_oracle_sha256: str
    oracle_generator_path: Path
    oracle_generator_sha256: str
    rscript_path: Path
    rscript_version: str
    rulespec_root: Path
    rulespec_sha: str
    rulespec_tracked_dirty: bool
    engine_root: Path
    engine_sha: str
    engine_tracked_dirty: bool
    engine_binary: Path
    engine_binary_sha256: str
    composition_path: Path
    composition_sha256: str
    eligibility_closures_path: Path
    eligibility_closures_sha256: str
    eligibility_closures_version: str
    treasury_root: Path
    treasury_present: bool
    treasury_sha: str | None
    treasury_tracked_dirty: bool
    treasury_parameter_sha256: str | None

    def as_json(self) -> dict[str, Any]:
        return {
            "oracle_snapshot": {
                "path": str(self.oracle_path),
                "sha256": self.oracle_sha256,
                "generated_at": EXPECTED_EXPANDED_GENERATED_AT,
            },
            "baseline_oracle_regeneration": {
                "path": str(self.baseline_oracle_path),
                "sha256": self.baseline_oracle_sha256,
                "byte_identical": True,
            },
            "oracle_generator": {
                "path": str(self.oracle_generator_path),
                "sha256": self.oracle_generator_sha256,
                "rscript": str(self.rscript_path),
                "rscript_version": self.rscript_version,
            },
            "rulespec": {
                "root": str(self.rulespec_root),
                "git_sha": self.rulespec_sha,
                "expected_git_sha": EXPECTED_RULESPEC_SHA,
                "tracked_dirty": self.rulespec_tracked_dirty,
            },
            "engine": {
                "root": str(self.engine_root),
                "git_sha": self.engine_sha,
                "expected_git_sha": EXPECTED_ENGINE_SHA,
                "tracked_dirty": self.engine_tracked_dirty,
                "binary": str(self.engine_binary),
                "binary_sha256": self.engine_binary_sha256,
            },
            "composition": {
                "path": str(self.composition_path),
                "sha256": self.composition_sha256,
            },
            "eligibility_closures": {
                "path": str(self.eligibility_closures_path),
                "sha256": self.eligibility_closures_sha256,
                "version": self.eligibility_closures_version,
            },
            "treasury_checkout": {
                "root": str(self.treasury_root),
                "present": self.treasury_present,
                "git_sha": self.treasury_sha,
                "tracked_dirty": self.treasury_tracked_dirty,
                "parameter_file": EXPECTED_PARAMETER_FILE,
                "parameter_sha256": self.treasury_parameter_sha256,
            },
        }


@dataclass(frozen=True)
class RegeneratedOracle:
    oracle: dict[str, Any]
    expanded_text: str
    baseline_oracle_path: Path
    baseline_oracle_sha256: str
    generator_path: Path
    rscript_path: Path
    rscript_version: str


def serialized_scenario_objects(snapshot_text: str) -> tuple[str, ...]:
    """Extract each serialized scenario object without normalising its bytes."""

    marker = '"scenarios": ['
    marker_index = snapshot_text.find(marker)
    if marker_index < 0:
        raise HarnessError("oracle JSON has no scenarios array")
    cursor = marker_index + len(marker)
    decoder = json.JSONDecoder()
    result: list[str] = []
    while True:
        while (
            cursor < len(snapshot_text)
            and snapshot_text[cursor] in " \t\r\n,"
        ):
            cursor += 1
        if cursor >= len(snapshot_text):
            raise HarnessError("oracle scenarios array is unterminated")
        if snapshot_text[cursor] == "]":
            break
        _value, end = decoder.raw_decode(snapshot_text, cursor)
        result.append(snapshot_text[cursor:end])
        cursor = end
    return tuple(result)


def regenerate_treasury_oracles(
    *,
    rulespec_root: Path,
    treasury_root: Path,
    oracle_generator: Path,
    rscript: Path,
) -> RegeneratedOracle:
    baseline_oracle_path = require_file(
        rulespec_root / "data" / "oracles" / "treasury-emtr-snapshot.json",
        "pinned Treasury baseline oracle",
    )
    treasury_root = require_directory(
        treasury_root, "Treasury source checkout"
    )
    oracle_generator = require_file(
        oracle_generator, "Treasury oracle generator"
    )
    rscript = require_file(rscript, "Rscript 4.3.0")
    parameter_path = require_file(
        treasury_root / EXPECTED_PARAMETER_FILE,
        "Treasury parameter file",
    )

    treasury_sha = git_sha(treasury_root)
    if treasury_sha != EXPECTED_ORACLE_COMMIT:
        raise HarnessError(
            f"Treasury checkout SHA mismatch: expected {EXPECTED_ORACLE_COMMIT}, "
            f"found {treasury_sha}"
        )
    if git_tracked_dirty(treasury_root):
        raise HarnessError(
            "Treasury checkout has tracked modifications: "
            f"{treasury_root}"
        )
    parameter_sha = sha256_file(parameter_path)
    if parameter_sha != EXPECTED_PARAMETER_SHA256:
        raise HarnessError(
            "Treasury parameter SHA-256 mismatch: expected "
            f"{EXPECTED_PARAMETER_SHA256}, found {parameter_sha}"
        )

    version_process = run_checked(
        [str(rscript), "--version"],
        label="Rscript version check",
    )
    rscript_version = "\n".join(
        part.strip()
        for part in (version_process.stdout, version_process.stderr)
        if part.strip()
    )
    if "version 4.3.0" not in rscript_version:
        raise HarnessError(
            "Treasury regeneration requires Rscript 4.3.0, found "
            f"{rscript_version!r}"
        )

    with tempfile.TemporaryDirectory(
        prefix="axiom-emtr-oracle-regeneration-"
    ) as raw_temp:
        temp_root = Path(raw_temp)
        baseline_output = temp_root / "baseline.json"
        expanded_output = temp_root / EXPANDED_ORACLE_NAME
        common_args = (
            str(rscript),
            str(oracle_generator),
            f"--repo={treasury_root}",
            f"--parameter-file={parameter_path}",
        )
        run_checked(
            [
                *common_args,
                "--mode=baseline",
                f"--output={baseline_output}",
            ],
            label="Treasury baseline oracle regeneration",
        )
        regenerated_baseline = baseline_output.read_bytes()
        pinned_baseline = baseline_oracle_path.read_bytes()
        if regenerated_baseline != pinned_baseline:
            raise HarnessError(
                "STOP: pinned Treasury baseline did not regenerate "
                "byte-for-byte; expected SHA-256 "
                f"{hashlib.sha256(pinned_baseline).hexdigest()}, found "
                f"{hashlib.sha256(regenerated_baseline).hexdigest()}"
            )
        run_checked(
            [
                *common_args,
                "--mode=expanded",
                f"--output={expanded_output}",
            ],
            label="expanded Treasury oracle regeneration",
        )
        expanded_text = expanded_output.read_text(encoding="utf-8")

    try:
        oracle = json.loads(expanded_text)
    except json.JSONDecodeError as exc:
        raise HarnessError(
            f"expanded Treasury generator emitted invalid JSON: {exc}"
        ) from exc
    return RegeneratedOracle(
        oracle=oracle,
        expanded_text=expanded_text,
        baseline_oracle_path=baseline_oracle_path,
        baseline_oracle_sha256=sha256_file(baseline_oracle_path),
        generator_path=oracle_generator,
        rscript_path=rscript,
        rscript_version=rscript_version,
    )


def verify_inputs(
    *,
    regenerated: RegeneratedOracle,
    rulespec_root: Path,
    engine_root: Path,
    engine_binary: Path,
    treasury_root: Path,
    composition_path: Path,
    eligibility_closures_path: Path,
    eligibility_closures: Mapping[str, Any],
    oracle_generator: Path,
    rscript: Path,
    output_dir: Path,
) -> tuple[dict[str, Any], Provenance]:
    rulespec_root = require_directory(rulespec_root, "RuleSpec root")
    engine_root = require_directory(engine_root, "engine root")
    engine_binary = require_file(engine_binary, "engine binary")
    composition_path = require_file(composition_path, "composition")
    eligibility_closures_path = require_file(
        eligibility_closures_path, "eligibility closures"
    )
    expected_engine_binary = (
        engine_root / "target" / "release" / "axiom-rules-engine"
    ).resolve()
    if engine_binary != expected_engine_binary:
        raise HarnessError(
            "engine binary must be the release binary under the pinned engine "
            f"checkout: expected {expected_engine_binary}, found {engine_binary}"
        )

    if rulespec_root.name != "rulespec-nz":
        raise HarnessError(
            f"RuleSpec root basename must be rulespec-nz, got {rulespec_root.name!r}"
        )

    oracle_generator = require_file(
        oracle_generator, "Treasury oracle generator"
    )
    rscript = require_file(rscript, "Rscript 4.3.0")
    oracle = regenerated.oracle
    oracle_path = (
        output_dir.expanduser().resolve() / EXPANDED_ORACLE_NAME
    )
    baseline_text = regenerated.baseline_oracle_path.read_text(
        encoding="utf-8"
    )
    baseline_oracle = json.loads(baseline_text)

    pinned = oracle.get("oracle", {})
    checks: tuple[tuple[str, Any, Any], ...] = (
        (
            "generated_at",
            oracle.get("generated_at"),
            EXPECTED_EXPANDED_GENERATED_AT,
        ),
        ("oracle.commit", pinned.get("commit"), EXPECTED_ORACLE_COMMIT),
        (
            "oracle.parameter_file",
            pinned.get("parameter_file"),
            EXPECTED_PARAMETER_FILE,
        ),
        (
            "oracle.parameter_file_sha256",
            pinned.get("parameter_file_sha256"),
            EXPECTED_PARAMETER_SHA256,
        ),
        ("oracle.model_year", pinned.get("model_year"), EXPECTED_MODEL_YEAR),
        (
            "generator.adapter",
            oracle.get("generator", {}).get("adapter"),
            "treasury-income-explorer-emtr-snapshot-expanded",
        ),
        (
            "generator.sampled_weekly_gross_wage",
            tuple(oracle.get("generator", {}).get("sampled_weekly_gross_wage", ())),
            EXPECTED_SAMPLE_WAGES,
        ),
        (
            "generator.output_columns",
            tuple(oracle.get("generator", {}).get("output_columns", ())),
            EXPECTED_OUTPUT_COLUMNS,
        ),
        (
            "scenario ids",
            tuple(item.get("id") for item in oracle.get("scenarios", ())),
            EXPECTED_SCENARIOS,
        ),
    )
    for label, actual, expected in checks:
        if actual != expected:
            raise HarnessError(
                f"{label} mismatch: expected {expected!r}, found {actual!r}"
            )
    note = oracle.get("generator", {}).get("note")
    if note != "Treasury outputs are weekly unless the column name says annual.":
        raise HarnessError(f"unexpected generator.note: {note!r}")
    scenario_provenance = oracle.get("scenario_provenance", {})
    if tuple(scenario_provenance) != EXPECTED_SCENARIOS:
        raise HarnessError(
            "scenario provenance ids mismatch: expected "
            f"{EXPECTED_SCENARIOS!r}, found {tuple(scenario_provenance)!r}"
        )
    if oracle.get("scenarios", [])[: len(ORIGINAL_SCENARIOS)] != baseline_oracle.get(
        "scenarios", []
    ):
        raise HarnessError(
            "expanded oracle did not preserve the original four scenario "
            "objects in value and schema"
        )
    baseline_serialized_scenarios = serialized_scenario_objects(baseline_text)
    expanded_serialized_scenarios = serialized_scenario_objects(
        regenerated.expanded_text
    )
    if (
        expanded_serialized_scenarios[: len(ORIGINAL_SCENARIOS)]
        != baseline_serialized_scenarios
    ):
        raise HarnessError(
            "expanded oracle did not preserve the serialized bytes of the "
            "original four scenario objects"
        )
    for raw_scenario in oracle.get("scenarios", ()):
        scenario_id = str(raw_scenario["id"])
        scenario_metadata = scenario_provenance[scenario_id]
        displayed = tuple(
            int(value)
            for value in scenario_metadata.get(
                "displayed_weekly_gross_wage", ()
            )
        )
        if displayed != EXPECTED_SAMPLE_WAGES:
            raise HarnessError(
                f"{scenario_id} display wages changed: {displayed!r}"
            )
        additional = tuple(
            int(value)
            for value in scenario_metadata.get(
                "additional_weekly_gross_wage", ()
            )
        )
        if additional != EXPECTED_ADDITIONAL_WAGES[scenario_id]:
            raise HarnessError(
                f"{scenario_id} additional wages changed: expected "
                f"{EXPECTED_ADDITIONAL_WAGES[scenario_id]!r}, "
                f"found {additional!r}"
            )
        expected_wages = tuple(sorted(set(displayed + additional)))
        actual_wages = tuple(
            dec(item["gross_wage1"])
            for item in raw_scenario.get("sampled_outputs", ())
        )
        if actual_wages != tuple(Decimal(wage) for wage in expected_wages):
            raise HarnessError(
                f"{scenario_id} sampled wages mismatch: expected "
                f"{expected_wages!r}, found {actual_wages!r}"
            )
        component_rows = scenario_metadata.get(
            "treasury_emtr_components", ()
        )
        component_wages = tuple(
            dec(item["gross_wage1"]) for item in component_rows
        )
        if component_wages != actual_wages:
            raise HarnessError(
                f"{scenario_id} Treasury EMTR component wages mismatch"
            )
        expected_component_keys = (
            "gross_wage1",
            *TREASURY_EMTR_COMPONENT_COLUMNS,
        )
        for index, component_row in enumerate(component_rows):
            if tuple(component_row) != expected_component_keys:
                raise HarnessError(
                    f"{scenario_id} Treasury EMTR component columns changed"
                )
            component_sum = sum(
                (dec(component_row[column]) for column in
                 TREASURY_EMTR_COMPONENT_COLUMNS),
                D0,
            )
            treasury_emtr = dec(
                raw_scenario["sampled_outputs"][index]["EMTR"]
            )
            if abs(component_sum - treasury_emtr) > Decimal("0.000004"):
                raise HarnessError(
                    f"{scenario_id} Treasury EMTR components do not "
                    f"recompose at wage {decimal_text(component_wages[index])}"
                )

    rulespec_sha = git_sha(rulespec_root)
    engine_sha = git_sha(engine_root)
    if rulespec_sha != EXPECTED_RULESPEC_SHA:
        raise HarnessError(
            f"RuleSpec SHA mismatch: expected {EXPECTED_RULESPEC_SHA}, "
            f"found {rulespec_sha}"
        )
    if engine_sha != EXPECTED_ENGINE_SHA:
        raise HarnessError(
            f"engine SHA mismatch: expected {EXPECTED_ENGINE_SHA}, found {engine_sha}"
        )
    rulespec_dirty = git_tracked_dirty(rulespec_root)
    engine_dirty = git_tracked_dirty(engine_root)
    if rulespec_dirty:
        raise HarnessError(f"RuleSpec checkout has tracked modifications: {rulespec_root}")
    if engine_dirty:
        raise HarnessError(f"engine checkout has tracked modifications: {engine_root}")

    treasury_root = require_directory(treasury_root, "Treasury source checkout")
    treasury_present = True
    treasury_sha: str | None = git_sha(treasury_root)
    treasury_dirty = git_tracked_dirty(treasury_root)
    parameter_sha: str | None
    if treasury_sha != EXPECTED_ORACLE_COMMIT:
        raise HarnessError(
            f"Treasury checkout SHA mismatch: expected {EXPECTED_ORACLE_COMMIT}, "
            f"found {treasury_sha}"
        )
    if treasury_dirty:
        raise HarnessError(
            "Treasury checkout has tracked modifications: "
            f"{treasury_root}"
        )
    parameter_path = require_file(
        treasury_root / EXPECTED_PARAMETER_FILE,
        "Treasury parameter file",
    )
    parameter_sha = sha256_file(parameter_path)
    if parameter_sha != EXPECTED_PARAMETER_SHA256:
        raise HarnessError(
            "Treasury parameter SHA-256 mismatch: expected "
            f"{EXPECTED_PARAMETER_SHA256}, found {parameter_sha}"
        )

    provenance = Provenance(
        oracle_path=oracle_path,
        oracle_sha256=hashlib.sha256(
            regenerated.expanded_text.encode("utf-8")
        ).hexdigest(),
        baseline_oracle_path=regenerated.baseline_oracle_path,
        baseline_oracle_sha256=regenerated.baseline_oracle_sha256,
        oracle_generator_path=oracle_generator,
        oracle_generator_sha256=sha256_file(oracle_generator),
        rscript_path=rscript,
        rscript_version=regenerated.rscript_version,
        rulespec_root=rulespec_root,
        rulespec_sha=rulespec_sha,
        rulespec_tracked_dirty=rulespec_dirty,
        engine_root=engine_root,
        engine_sha=engine_sha,
        engine_tracked_dirty=engine_dirty,
        engine_binary=engine_binary,
        engine_binary_sha256=sha256_file(engine_binary),
        composition_path=composition_path,
        composition_sha256=sha256_file(composition_path),
        eligibility_closures_path=eligibility_closures_path,
        eligibility_closures_sha256=sha256_file(eligibility_closures_path),
        eligibility_closures_version=str(eligibility_closures["version"]),
        treasury_root=treasury_root,
        treasury_present=treasury_present,
        treasury_sha=treasury_sha,
        treasury_tracked_dirty=treasury_dirty,
        treasury_parameter_sha256=parameter_sha,
    )
    return oracle, provenance


class Engine:
    """Small JSON-CLI adapter that always evaluates in explain mode."""

    def __init__(
        self,
        *,
        binary: Path,
        artifact: Path,
        catalog: Mapping[str, str],
    ) -> None:
        self.binary = binary
        self.artifact = artifact
        self.catalog = dict(catalog)
        self.call_count = 0
        self.observed_inputs: dict[str, set[str]] = {
            slot: set() for slot in self.catalog
        }

    @classmethod
    def compile(
        cls,
        *,
        binary: Path,
        composition: Path,
        rulespec_root: Path,
        artifact: Path,
    ) -> tuple["Engine", dict[str, Any], str]:
        process = run_checked(
            [
                str(binary),
                "compile-composed",
                "--program",
                str(composition),
                "--rulespec-root",
                str(rulespec_root),
                "--output",
                str(artifact),
            ],
            label="RuleSpec composition compile",
        )
        with artifact.open(encoding="utf-8") as source:
            compiled = json.load(source)
        catalog_entries = compiled.get("metadata", {}).get("input_catalog", [])
        catalog = {
            entry["slot"]: entry["canonical_request_name"]
            for entry in catalog_entries
        }
        if not catalog:
            raise HarnessError("compiled artifact has an empty input catalog")
        engine = cls(binary=binary, artifact=artifact, catalog=catalog)
        return engine, compiled, process.stdout.strip()

    def input_record(
        self,
        *,
        slot: str,
        value: bool | int | Decimal,
        entity: str,
        entity_id: str,
    ) -> dict[str, Any]:
        try:
            name = self.catalog[slot]
        except KeyError as exc:
            raise HarnessError(f"compiled input catalog has no slot {slot!r}") from exc
        observed = (
            decimal_text(value) if isinstance(value, Decimal)
            else json.dumps(value, sort_keys=True)
        )
        self.observed_inputs[slot].add(observed)
        if isinstance(value, bool):
            scalar = {"kind": "bool", "value": value}
        elif isinstance(value, int):
            scalar = {"kind": "integer", "value": value}
        elif isinstance(value, Decimal):
            scalar = {"kind": "decimal", "value": decimal_text(value)}
        else:
            raise HarnessError(
                f"unsupported input value type for {slot}: {type(value).__name__}"
            )
        return {
            "name": name,
            "entity": entity,
            "entity_id": entity_id,
            "interval": {
                "start": EXPECTED_PERIOD_START,
                "end": EXPECTED_PERIOD_END,
            },
            "value": scalar,
        }

    def evaluate(
        self,
        *,
        entity: str,
        entity_id: str,
        inputs: Mapping[str, bool | int | Decimal],
        outputs: Sequence[str],
    ) -> tuple[dict[str, Decimal | str], dict[str, Any]]:
        request = {
            "mode": "explain",
            "dataset": {
                "inputs": [
                    self.input_record(
                        slot=slot,
                        value=value,
                        entity=entity,
                        entity_id=entity_id,
                    )
                    for slot, value in sorted(inputs.items())
                ],
                "relations": [],
            },
            "queries": [
                {
                    "entity_id": entity_id,
                    "period": {
                        "period_kind": "tax_year",
                        "start": EXPECTED_PERIOD_START,
                        "end": EXPECTED_PERIOD_END,
                    },
                    "outputs": list(outputs),
                }
            ],
        }
        process = run_checked(
            [
                str(self.binary),
                "run-compiled",
                "--artifact",
                str(self.artifact),
            ],
            stdin=json.dumps(request, sort_keys=True, separators=(",", ":")),
            label=f"engine evaluation for {entity_id}",
        )
        self.call_count += 1
        try:
            response = json.loads(process.stdout)
        except json.JSONDecodeError as exc:
            raise HarnessError(
                f"engine returned invalid JSON for {entity_id}: {process.stdout!r}"
            ) from exc
        metadata = response.get("metadata", {})
        if (
            metadata.get("requested_mode") != "explain"
            or metadata.get("actual_mode") != "explain"
        ):
            raise HarnessError(
                f"engine did not execute {entity_id} in explain mode: {metadata!r}"
            )
        results = response.get("results", [])
        if len(results) != 1:
            raise HarnessError(
                f"expected one engine result for {entity_id}, found {len(results)}"
            )
        values: dict[str, Decimal | str] = {}
        returned = results[0].get("outputs", {})
        for target in outputs:
            if target not in returned:
                raise HarnessError(f"engine omitted requested output {target}")
            item = returned[target]
            if item.get("kind") == "judgment":
                values[target] = item["outcome"]
            elif item.get("kind") == "scalar":
                values[target] = dec(item["value"]["value"])
            else:
                raise HarnessError(f"unknown engine output shape for {target}: {item!r}")
        return values, results[0].get("trace", {})


def normalize_children(raw: Any) -> list[int]:
    if isinstance(raw, list):
        return [int(value) for value in raw]
    if isinstance(raw, (int, float)):
        return [int(raw)]
    if raw in ({}, None):
        return []
    raise HarnessError(f"unsupported Children_ages encoding: {raw!r}")


TAX_OUTPUT = output_id(TAX_MODULE, "individual_income_tax_before_credits")
ACC_OUTPUT = output_id(ACC_MODULE, "acc_standard_earners_levy_including_gst")
JOBSEEKER_OUTPUT = output_id(BENEFIT_MODULE, "jobseeker_support_net_weekly_payment")
SOLE_PARENT_OUTPUT = output_id(
    BENEFIT_MODULE, "sole_parent_support_net_weekly_payment"
)
FTC_OUTPUT = output_id(WFF_MODULE, "family_tax_credit_after_abatement")
FTC_BEFORE_OUTPUT = output_id(
    WFF_MODULE, "family_tax_credit_before_abatement"
)
IWTC_BEFORE_OUTPUT = output_id(WFF_MODULE, "in_work_tax_credit_before_abatement")
IWTC_REMAINING_ABATEMENT_OUTPUT = output_id(
    WFF_MODULE, "wff_abatement_remaining_after_family_tax_credit"
)
IWTC_ENTITLEMENT_OUTPUT = output_id(
    ELIGIBILITY_MODULE, "entitled_to_in_work_tax_credit"
)
MFTC_OUTPUT = output_id(WFF_MODULE, "minimum_family_tax_credit")
IETC_OUTPUT = output_id(IETC_MODULE, "independent_earner_tax_credit")
WEP_RATE_OUTPUT = output_id(
    WEP_MODULE, "winter_energy_payment_rate_per_winter_period"
)
BEST_START_OUTPUT = output_id(WFF_MODULE, "best_start_tax_credit")
BEST_START_BEFORE_OUTPUT = output_id(
    WFF_MODULE, "best_start_tax_credit_before_abatement"
)
BEST_START_ABATEMENT_OUTPUT = output_id(
    WFF_MODULE, "best_start_credit_abatement"
)
AS_UNROUNDED_OUTPUT = output_id(
    AS_MODULE, "accommodation_supplement_weekly_amount_before_rounding"
)
AS_ROUNDED_OUTPUT = output_id(
    AS_MODULE, "accommodation_supplement_rounded_weekly_payment"
)
AS_QUALIFYING_COST_OUTPUT = output_id(
    AS_MODULE,
    "accommodation_supplement_weekly_qualifying_accommodation_costs",
)
FSI_OUTPUT = output_id(FSI_MODULE, "family_scheme_income")

COMPARISON_COLUMNS = (
    "gross_wage1",
    "hours1",
    "gross_wage1_annual",
    "gross_wage2",
    "wage1_tax",
    "wage1_ACC_levy",
    "net_wage1",
    "net_wage",
    "net_benefit",
    "FTC_abated",
    "IWTC_abated",
    "MFTC",
    "IETC_abated",
    "WinterEnergy",
    "BestStart_Total",
    "AS_Amount",
    "WFF_abated",
    "Net_Income",
    "Net_Income_annual",
    "EMTR",
)


@dataclass(frozen=True)
class Scenario:
    id: str
    description: str
    partnered: bool
    wage1_hourly: Decimal
    children: tuple[int, ...]
    gross_wage2: Decimal
    hours2: Decimal
    accommodation_costs: Decimal
    accommodation_rent: bool
    accommodation_area: int
    accommodation_boarder: bool
    weekly_board_and_lodgings_paid: Decimal
    sampled_wages: tuple[int, ...]

    @classmethod
    def from_oracle(
        cls,
        raw: Mapping[str, Any],
        provenance: Mapping[str, Any],
    ) -> "Scenario":
        inputs = raw["inputs"]
        rulespec_profile = provenance.get("rulespec_profile", {})
        profile_checks = (
            ("partnered", bool(inputs["Partnered"])),
            ("wage1_hourly", dec(inputs["wage1_hourly"])),
            (
                "children_ages",
                tuple(normalize_children(inputs["Children_ages"])),
            ),
            ("partner_weekly_wage", dec(inputs["gross_wage2"])),
            ("partner_hours", dec(inputs["hours2"])),
            ("accommodation_area", int(inputs["AS_Area"])),
        )
        for name, expected in profile_checks:
            raw_value = rulespec_profile.get(name)
            actual = (
                tuple(normalize_children(raw_value))
                if name == "children_ages"
                else bool(raw_value)
                if name == "partnered"
                else int(raw_value)
                if name == "accommodation_area"
                else dec(raw_value)
            )
            if actual != expected:
                raise HarnessError(
                    f"{raw['id']} RuleSpec provenance {name} does not "
                    "match the raw Treasury profile"
                )
        boarder = bool(rulespec_profile.get("boarder", False))
        accommodation_type = rulespec_profile.get("accommodation_type")
        if boarder:
            if accommodation_type != "board":
                raise HarnessError(
                    f"{raw['id']} boarder provenance lacks board type"
                )
        else:
            expected_type = (
                "rent" if inputs["AS_Accommodation_Rent"] else "mortgage"
            )
            if accommodation_type != expected_type:
                raise HarnessError(
                    f"{raw['id']} accommodation type does not match "
                    "Treasury rent/mortgage input"
                )
            if (
                dec(rulespec_profile.get("accommodation_cost"))
                != dec(inputs["AS_Accommodation_Costs"])
            ):
                raise HarnessError(
                    f"{raw['id']} accommodation cost does not match "
                    "Treasury input"
                )
        displayed = tuple(
            int(value)
            for value in provenance.get(
                "displayed_weekly_gross_wage",
                EXPECTED_SAMPLE_WAGES,
            )
        )
        additional = tuple(
            int(value)
            for value in provenance.get(
                "additional_weekly_gross_wage",
                (),
            )
        )
        sampled_wages = tuple(sorted(set(displayed + additional)))
        return cls(
            id=str(raw["id"]),
            description=str(raw["description"]),
            partnered=bool(inputs["Partnered"]),
            wage1_hourly=dec(inputs["wage1_hourly"]),
            children=tuple(normalize_children(inputs["Children_ages"])),
            gross_wage2=dec(inputs["gross_wage2"]),
            hours2=dec(inputs["hours2"]),
            accommodation_costs=dec(inputs["AS_Accommodation_Costs"]),
            accommodation_rent=bool(inputs["AS_Accommodation_Rent"]),
            accommodation_area=int(inputs["AS_Area"]),
            accommodation_boarder=boarder,
            weekly_board_and_lodgings_paid=dec(
                rulespec_profile.get("accommodation_cost", 0)
                if boarder
                else 0
            ),
            sampled_wages=sampled_wages,
        )


def scalar_decimal(values: Mapping[str, Decimal | str], target: str) -> Decimal:
    value = values[target]
    if not isinstance(value, Decimal):
        raise HarnessError(f"expected scalar output {target}, found {value!r}")
    return value


def selected_parameter_values(
    compiled: Mapping[str, Any],
    name: str,
) -> dict[int, Decimal]:
    matches = [
        item
        for item in compiled["program"]["parameters"]
        if item.get("name") == name
    ]
    if len(matches) != 1:
        raise HarnessError(
            f"expected one compiled parameter named {name!r}, found {len(matches)}"
        )
    eligible = [
        version
        for version in matches[0]["versions"]
        if version["effective_from"] <= EXPECTED_PERIOD_START
    ]
    if not eligible:
        raise HarnessError(
            f"parameter {name!r} has no version at {EXPECTED_PERIOD_START}"
        )
    selected = max(eligible, key=lambda item: item["effective_from"])
    result: dict[int, Decimal] = {}
    for raw_index, raw_value in selected["values"].items():
        result[int(raw_index)] = dec(raw_value["value"])
    return result


class ModelEvaluator:
    """Host-level composition required by the relation-free compiled program."""

    def __init__(
        self,
        engine: Engine,
        compiled: Mapping[str, Any],
        eligibility_closures: Mapping[str, Any],
    ) -> None:
        self.engine = engine
        self.closures = eligibility_closures["choices"]
        self._cache: dict[
            tuple[
                tuple[tuple[str, str, str], ...],
                tuple[str, ...],
            ],
            dict[str, Decimal | str],
        ] = {}
        self._gross_benefit_cache: dict[Decimal, Decimal] = {}
        self._as_cutout_cache: dict[
            tuple[bool, int, bool], Decimal
        ] = {}
        self.tax_rates = selected_parameter_values(
            compiled, "individual_income_tax_bracket_rates"
        )
        self.tax_thresholds = selected_parameter_values(
            compiled, "individual_income_tax_bracket_thresholds"
        )
        self.ftc_eldest_annual = selected_parameter_values(
            compiled, "family_tax_credit_eldest_child_annual_amount"
        )[0]
        self.wff_abatement_threshold = selected_parameter_values(
            compiled, "wff_family_credit_abatement_threshold"
        )[0]
        self.wff_abatement_rate = selected_parameter_values(
            compiled, "wff_family_credit_abatement_rate"
        )[0]
        self.best_start_abatement_threshold = selected_parameter_values(
            compiled, "best_start_abatement_threshold"
        )[0]
        self.best_start_abatement_rate = selected_parameter_values(
            compiled, "best_start_abatement_rate"
        )[0]
        self.ietc_abatement_threshold = selected_parameter_values(
            compiled, "independent_earner_tax_credit_abatement_threshold"
        )[0]
        self.ietc_abatement_rate = selected_parameter_values(
            compiled, "independent_earner_tax_credit_abatement_rate"
        )[0]
        self.ietc_minimum_income = selected_parameter_values(
            compiled, "independent_earner_tax_credit_minimum_net_income"
        )[0]
        self.ietc_full_year_amount = selected_parameter_values(
            compiled, "independent_earner_tax_credit_full_year_amount"
        )[0]
        self.benefit_income_test_lower_threshold = selected_parameter_values(
            compiled, "main_benefit_income_test_lower_weekly_threshold"
        )[0]
        self.benefit_income_test_3_rate = selected_parameter_values(
            compiled, "main_benefit_income_test_3_abatement_rate"
        )[0]
        self.jobseeker_single_with_children_rate = selected_parameter_values(
            compiled,
            "jobseeker_support_single_with_dependent_children_weekly_rate",
        )[0]
        expected_rate_indexes = set(range(1, 6))
        expected_threshold_indexes = set(range(1, 5))
        if set(self.tax_rates) != expected_rate_indexes:
            raise HarnessError(
                f"unexpected tax-rate indexes: {sorted(self.tax_rates)}"
            )
        if set(self.tax_thresholds) != expected_threshold_indexes:
            raise HarnessError(
                f"unexpected tax-threshold indexes: {sorted(self.tax_thresholds)}"
            )

    def _evaluate(
        self,
        *,
        entity: str,
        entity_id: str,
        inputs: Mapping[str, bool | int | Decimal],
        outputs: Sequence[str],
    ) -> dict[str, Decimal | str]:
        cache_inputs = tuple(
            (
                slot,
                type(value).__name__,
                decimal_text(value) if isinstance(value, Decimal) else str(value),
            )
            for slot, value in sorted(inputs.items())
        )
        key = (cache_inputs, tuple(outputs))
        if key not in self._cache:
            values, _trace = self.engine.evaluate(
                entity=entity,
                entity_id=entity_id,
                inputs=inputs,
                outputs=outputs,
            )
            self._cache[key] = values
        return dict(self._cache[key])

    def tax_from_schedule(self, annual_income: Decimal) -> Decimal:
        income = max(D0, annual_income)
        tax = D0
        lower = D0
        for bracket in range(1, 5):
            upper = self.tax_thresholds[bracket]
            tax += max(D0, min(income, upper) - lower) * self.tax_rates[bracket]
            lower = upper
        tax += max(D0, income - lower) * self.tax_rates[5]
        return tax

    def annual_tax(self, annual_income: Decimal) -> Decimal:
        values = self._evaluate(
            entity="Person",
            entity_id=f"tax:{decimal_text(annual_income)}",
            inputs={"taxable_income": annual_income},
            outputs=[TAX_OUTPUT],
        )
        engine_tax = scalar_decimal(values, TAX_OUTPUT)
        schedule_tax = self.tax_from_schedule(annual_income)
        # The engine's JSON boundary emits fewer decimal places than the
        # 40-digit host context. This verifies the same schedule far below a
        # cent while retaining the engine value for every reported result.
        if abs(engine_tax - schedule_tax) > ENGINE_DECIMAL_TOLERANCE:
            raise HarnessError(
                "compiled tax schedule and engine output disagree at "
                f"{decimal_text(annual_income)}: "
                f"{decimal_text(schedule_tax)} != {decimal_text(engine_tax)}"
            )
        return engine_tax

    def annual_acc(self, annual_earnings: Decimal) -> Decimal:
        values = self._evaluate(
            entity="Person",
            entity_id=f"acc:{decimal_text(annual_earnings)}",
            inputs={"acc_earnings_for_earners_levy": annual_earnings},
            outputs=[ACC_OUTPUT],
        )
        return scalar_decimal(values, ACC_OUTPUT)

    def gross_benefit_from_net_weekly(self, net_weekly: Decimal) -> Decimal:
        if net_weekly <= 0:
            return D0
        if net_weekly in self._gross_benefit_cache:
            return self._gross_benefit_cache[net_weekly]
        annual_net = net_weekly * WEEKS_IN_MODEL_YEAR
        low = annual_net
        high = annual_net * 2 + Decimal("1000")
        while high - self.tax_from_schedule(high) < annual_net:
            high *= 2
        for _ in range(160):
            midpoint = (low + high) / 2
            if midpoint - self.tax_from_schedule(midpoint) < annual_net:
                low = midpoint
            else:
                high = midpoint
        gross_weekly = ((low + high) / 2) / WEEKS_IN_MODEL_YEAR
        residual = (
            gross_weekly
            - self.tax_from_schedule(gross_weekly * WEEKS_IN_MODEL_YEAR)
            / WEEKS_IN_MODEL_YEAR
            - net_weekly
        )
        if abs(residual) > Decimal("0.00000000000000000001"):
            raise HarnessError(
                "tax gross-up did not converge for weekly net benefit "
                f"{decimal_text(net_weekly)}: residual {decimal_text(residual)}"
            )
        self._gross_benefit_cache[net_weekly] = gross_weekly
        return gross_weekly

    def jobseeker_payment(
        self,
        scenario: Scenario,
        weekly_family_income: Decimal,
    ) -> Decimal:
        child_count = len(scenario.children)
        youngest = min(scenario.children) if scenario.children else 0
        inputs: dict[str, bool | int | Decimal] = {
            "jobseeker_support_applicant_age": 25,
            "jobseeker_support_benefit_commenced_on_or_after_1998_07_01": False,
            "jobseeker_support_dependent_children_count": child_count,
            "jobseeker_support_living_with_parent": False,
            "jobseeker_support_partner_ineligible_due_to_sanction_or_strike": False,
            "jobseeker_support_partner_receives_main_benefit": scenario.partnered,
            "jobseeker_support_partner_receives_new_zealand_superannuation_or_veterans_pension": False,
            "jobseeker_support_single": not scenario.partnered,
            "jobseeker_support_total_income": weekly_family_income,
            "jobseeker_support_transferred_2013_no_dependent_children": False,
            "jobseeker_support_youngest_dependent_child_age": youngest,
        }
        values = self._evaluate(
            entity="Person",
            entity_id=f"{scenario.id}:jobseeker:{decimal_text(weekly_family_income)}",
            inputs=inputs,
            outputs=[JOBSEEKER_OUTPUT],
        )
        return scalar_decimal(values, JOBSEEKER_OUTPUT)

    def sole_parent_payment(
        self,
        scenario: Scenario,
        weekly_family_income: Decimal,
    ) -> Decimal:
        values = self._evaluate(
            entity="Person",
            entity_id=f"{scenario.id}:sole-parent:{decimal_text(weekly_family_income)}",
            inputs={
                "sole_parent_support_personal_earnings_used_for_childcare": D0,
                "sole_parent_support_total_income": weekly_family_income,
            },
            outputs=[SOLE_PARENT_OUTPUT],
        )
        return scalar_decimal(values, SOLE_PARENT_OUTPUT)

    def raw_iwtc_branch(self, scenario: Scenario, weekly_wages: Decimal) -> bool:
        return bool(scenario.children) and weekly_wages >= ORACLE_IWTC_WEEKLY_THRESHOLD

    def benefit_components(
        self,
        scenario: Scenario,
        weekly_wages: Decimal,
    ) -> tuple[Decimal, ...]:
        if scenario.partnered:
            per_person = self.jobseeker_payment(scenario, weekly_wages)
            scheduled = (per_person, per_person)
        elif scenario.children:
            if min(scenario.children) >= 14:
                # Treasury R/emtr.R lines 310-319 deliberately select the
                # lone-parent JSS rate with the SPS income-test scale once the
                # youngest child is 14. RuleSpec's Jobseeker branch encodes
                # that same combination.
                scheduled = (self.jobseeker_payment(scenario, weekly_wages),)
            else:
                scheduled = (self.sole_parent_payment(scenario, weekly_wages),)
        else:
            scheduled = (self.jobseeker_payment(scenario, weekly_wages),)
        if self.raw_iwtc_branch(scenario, weekly_wages):
            return tuple(D0 for _item in scheduled)
        return scheduled

    def zero_income_jobseeker_components(
        self,
        scenario: Scenario,
    ) -> tuple[Decimal, ...]:
        per_person = self.jobseeker_payment(scenario, D0)
        if scenario.partnered:
            return (per_person, per_person)
        return (per_person,)

    def family_scheme_inputs(
        self,
        annual_base_income: Decimal,
        annual_wages: Decimal,
    ) -> dict[str, bool | int | Decimal]:
        inputs: dict[str, bool | int | Decimal] = {
            "family_scheme_base_income_excluding_mb3_to_mb13_adjustments": annual_base_income,
            "family_scheme_activity_income": D0,
            "family_scheme_activity_deductions": D0,
            "family_scheme_superannuation_distribution_income": D0,
            "family_scheme_retirement_scheme_distribution_income": D0,
            "family_scheme_close_company_person_voting_interest": D0,
            "family_scheme_close_company_dependent_child_voting_interest": D0,
            "family_scheme_close_company_relevant_major_shareholders": 0,
            "family_scheme_close_company_net_income": D0,
            "family_scheme_close_company_main_income_equalisation_deposits": D0,
            "family_scheme_close_company_main_income_equalisation_refunds": D0,
            "family_scheme_close_company_dividends": D0,
            "family_scheme_trustee_net_income": D0,
            "family_scheme_trustee_main_income_equalisation_deposits": D0,
            "family_scheme_trustee_main_income_equalisation_refunds": D0,
            "family_scheme_trustee_beneficiary_income_vested_or_paid": D0,
            "family_scheme_trustee_company_voting_interest": D0,
            "family_scheme_trustee_company_net_income": D0,
            "family_scheme_trustee_company_main_income_equalisation_deposits": D0,
            "family_scheme_trustee_company_main_income_equalisation_refunds": D0,
            "family_scheme_trustee_company_dividends": D0,
            "family_scheme_trust_settlors_alive_count": 0,
            "family_scheme_employee_salary_or_wages": annual_wages,
            "family_scheme_short_term_charge_facility_value_excluding_fbt": D0,
            "family_scheme_short_term_charge_facility_value_including_fbt": D0,
            "family_scheme_employment_income_foregone_for_motor_vehicle": D0,
            "family_scheme_controlling_shareholder_voting_interest": D0,
            "family_scheme_attributed_fringe_benefits_taxable_value": D0,
            "family_scheme_attributed_fringe_benefits_fbt_liability": D0,
            "family_scheme_exempt_pension_or_annuity_amount": D0,
            "family_scheme_dependent_child_relevant_amounts": D0,
            "family_scheme_dependent_child_principal_caregivers_count": 0,
            "family_scheme_spouse_or_partner_nonresident_foreign_sourced_income": D0,
            "family_scheme_commissioner_excludes_non_settlor_trust_payment": False,
            "family_scheme_non_settlor_trust_payment": D0,
            "family_scheme_other_payments_not_excluded": D0,
            "family_scheme_income_period_days": 365,
        }
        return inputs

    def iwtc_eligibility_inputs(
        self,
        scenario: Scenario,
        weekly_wages: Decimal,
        net_benefit: Decimal,
    ) -> dict[str, bool | int | Decimal]:
        has_children = bool(scenario.children)
        earns_paye = weekly_wages > 0
        residence = self.closures["residence"]
        care = self.closures["care"]
        work_tests = self.closures["work_tests"]
        gates = self.closures["gates"]
        return {
            "wff_commissioner_considers_primary_day_to_day_care": (
                has_children
                and care[
                    "wff_commissioner_considers_primary_day_to_day_care_when_child_present"
                ]
            ),
            "wff_care_is_temporary": care["wff_care_is_temporary"],
            "wff_caregiver_is_body_of_persons": False,
            "wff_caregiver_is_spouse_or_partner_of_non_electing_transitional_resident": False,
            "wff_caregiver_is_disqualifying_residence_or_institution_operator_or_employee": residence[
                "wff_caregiver_disqualifying_status"
            ],
            "wff_caregiver_lives_apart_from_another_qualifying_person_for_child": (
                has_children and care["wff_caregiver_lives_apart_when_child_present"]
            ),
            "in_work_tax_credit_child_exclusive_care_fraction": (
                dec(care["iwtc_child_exclusive_care_fraction"])
                if has_children else D0
            ),
            "in_work_tax_credit_person_age": 25,
            "in_work_tax_credit_child_financially_dependent": has_children,
            "in_work_tax_credit_child_treated_financially_dependent_by_payments": False,
            "in_work_tax_credit_person_new_zealand_resident": residence[
                "iwtc_person_new_zealand_resident"
            ],
            "in_work_tax_credit_person_present_in_new_zealand_continuous_months": residence[
                "iwtc_person_present_in_new_zealand_continuous_months"
            ],
            "in_work_tax_credit_person_resident_under_yd1_on_credit_days": residence[
                "iwtc_person_resident_under_yd1_on_credit_days"
            ],
            "in_work_tax_credit_person_transitional_resident": residence[
                "iwtc_person_transitional_resident"
            ],
            "in_work_tax_credit_person_spouse_or_partner_transitional_resident": residence[
                "iwtc_person_spouse_or_partner_transitional_resident"
            ],
            "in_work_tax_credit_child_new_zealand_resident": (
                has_children and residence["iwtc_child_new_zealand_resident_when_present"]
            ),
            "in_work_tax_credit_child_present_in_new_zealand_for_entitlement_period": (
                has_children and residence[
                    "iwtc_child_present_for_entitlement_period_when_present"
                ]
            ),
            "in_work_tax_credit_person_or_partner_receives_main_benefit": (
                gates["iwtc_main_benefit_disqualifies"] and net_benefit > 0
            ),
            "in_work_tax_credit_person_or_partner_receives_basic_and_independent_circumstances_grants": gates[
                "iwtc_basic_or_independent_grants_received"
            ],
            "in_work_tax_credit_person_or_partner_receives_parent_allowance_or_childrens_pension": gates[
                "iwtc_parent_allowance_or_childrens_pension_received"
            ],
            "in_work_tax_credit_allowed_paye_income_payment": (
                earns_paye and work_tests[
                    "iwtc_allowed_paye_income_payment_when_wages_positive"
                ]
            ),
            "in_work_tax_credit_rd3b_or_rd3c_income": False,
            "in_work_tax_credit_business_income_from_profit_activity": False,
            "in_work_tax_credit_personal_service_rehabilitation_payment_income": False,
            "in_work_tax_credit_normally_earner": (
                earns_paye and work_tests["iwtc_normally_earner_when_wages_positive"]
            ),
            "in_work_tax_credit_earner_in_relation_to_close_company": False,
            "in_work_tax_credit_earner_major_shareholder_in_close_company": False,
            "in_work_tax_credit_close_company_derives_gross_income": False,
            "in_work_tax_credit_child_tax_credit_received_for_period_ending_2006_03_31": False,
            "in_work_tax_credit_incapacity_suffered_between_2006_01_01_and_2006_03_31": False,
            "in_work_tax_credit_weekly_compensation_paid_for_incapacity": False,
            "in_work_tax_credit_full_time_earner_income_at_incapacity": False,
            "in_work_tax_credit_would_have_been_eligible_under_legacy_formula": False,
            "in_work_tax_credit_normally_full_time_earner": False,
            "in_work_tax_credit_birth_absence_weeks": 0,
            "in_work_tax_credit_work_reduced_or_stopped_due_to_child_birth": False,
            "in_work_tax_credit_entitled_to_parental_tax_credit_for_child": False,
            "in_work_tax_credit_currently_meets_fifth_requirement": work_tests[
                "iwtc_currently_meets_fifth_requirement"
            ],
            "in_work_tax_credit_days_since_last_met_fifth_requirement": 0,
        }

    def family_credit_values(
        self,
        *,
        scenario: Scenario,
        weekly_wages: Decimal,
        weekly_gross_benefits: Decimal,
        net_benefit: Decimal,
        wage_tax_total: Decimal,
        hours_total: Decimal,
    ) -> dict[str, Decimal | str]:
        child_count = len(scenario.children)
        annual_wages = weekly_wages * WEEKS_IN_MODEL_YEAR
        annual_base_income = (
            weekly_wages + weekly_gross_benefits
        ) * WEEKS_IN_MODEL_YEAR
        inputs = self.family_scheme_inputs(annual_base_income, annual_wages)
        inputs.update(
            {
                "family_tax_credit_eldest_dependent_child_care_units": (
                    D1 if child_count else D0
                ),
                "family_tax_credit_subsequent_dependent_child_care_units": Decimal(
                    max(0, child_count - 1)
                ),
                "family_tax_credit_entitlement_days": 365 if child_count else 0,
                "wff_family_credit_abatement_days": 365 if child_count else 0,
                "in_work_tax_credit_allowed_children_count": child_count,
                "in_work_tax_credit_weekly_periods": 52 if child_count else 0,
                "child_tax_credit_for_entitlement_period": D0,
                "parental_tax_credit_for_entitlement_period": D0,
                "parental_tax_credit_additional_abatement": D0,
            }
        )
        inputs.update(
            self.iwtc_eligibility_inputs(scenario, weekly_wages, net_benefit)
        )
        work_tests = self.closures["work_tests"]
        full_time_hours = Decimal(
            work_tests[
                "mftc_couple_full_time_hours_per_week"
                if scenario.partnered
                else "mftc_single_full_time_hours_per_week"
            ]
        )
        mftc_eligible = (
            bool(child_count)
            and hours_total >= full_time_hours
            and net_benefit == 0
        )
        inputs.update(
            {
                "minimum_family_scheme_income_attributable_to_full_time_weeks": (
                    weekly_wages * D52 if mftc_eligible else D0
                ),
                "minimum_family_full_time_earner_weeks": 52 if mftc_eligible else 0,
                "minimum_family_adjusted_income_tax_liability": (
                    wage_tax_total * D52 if mftc_eligible else D0
                ),
                "minimum_family_amount_received": D0,
                "minimum_family_amount_paid": D0,
                "minimum_family_tax_credit_weekly_periods": (
                    52 if mftc_eligible else 0
                ),
            }
        )
        return self._evaluate(
            entity="Family",
            entity_id=f"{scenario.id}:family:{decimal_text(weekly_wages)}",
            inputs=inputs,
            outputs=[
                FTC_BEFORE_OUTPUT,
                FTC_OUTPUT,
                IWTC_BEFORE_OUTPUT,
                IWTC_REMAINING_ABATEMENT_OUTPUT,
                IWTC_ENTITLEMENT_OUTPUT,
                MFTC_OUTPUT,
                FSI_OUTPUT,
            ],
        )

    def best_start_total(
        self,
        *,
        scenario: Scenario,
        annual_base_income: Decimal,
        annual_wages: Decimal,
    ) -> tuple[Decimal, Decimal, Decimal]:
        eligible_children = [
            index for index, age in enumerate(scenario.children) if age in (0, 1, 2)
        ]
        total_before_abatement = D0
        per_child_total = D0
        family_abatement: Decimal | None = None
        for index in eligible_children:
            inputs = self.family_scheme_inputs(annual_base_income, annual_wages)
            inputs.update(
                {
                    "best_start_entitlement_days": 365,
                    "best_start_child_care_fraction": dec(
                        self.closures["care"]["best_start_child_care_fraction"]
                    ),
                    "best_start_abatement_days": 365,
                }
            )
            values = self._evaluate(
                entity="Child",
                entity_id=f"{scenario.id}:child:{index}",
                inputs=inputs,
                outputs=[
                    BEST_START_BEFORE_OUTPUT,
                    BEST_START_ABATEMENT_OUTPUT,
                    BEST_START_OUTPUT,
                ],
            )
            child_before = scalar_decimal(values, BEST_START_BEFORE_OUTPUT)
            child_abatement = scalar_decimal(
                values, BEST_START_ABATEMENT_OUTPUT
            )
            total_before_abatement += child_before
            per_child_total += scalar_decimal(values, BEST_START_OUTPUT)
            if family_abatement is None:
                family_abatement = child_abatement
            elif child_abatement != family_abatement:
                raise HarnessError(
                    "Best Start family abatement changed between children for "
                    f"{scenario.id}"
                )
        aggregate_total = max(
            D0,
            total_before_abatement - (family_abatement or D0),
        )
        continuous_total = max(
            D0,
            total_before_abatement
            - max(
                D0,
                annual_base_income - self.best_start_abatement_threshold,
            )
            * self.best_start_abatement_rate,
        )
        return (
            aggregate_total / WEEKS_IN_MODEL_YEAR,
            per_child_total / WEEKS_IN_MODEL_YEAR,
            continuous_total / WEEKS_IN_MODEL_YEAR,
        )

    def ietc_for_person(
        self,
        *,
        scenario: Scenario,
        person: int,
        weekly_wage: Decimal,
        weekly_benefit: Decimal,
        family_support_payable: bool,
    ) -> tuple[Decimal, Decimal]:
        primary = person == 1
        residence = self.closures["residence"]
        gates = self.closures["gates"]
        inputs: dict[str, bool | int | Decimal] = {
            "independent_earner_tax_credit_net_income": (
                weekly_wage * WEEKS_IN_MODEL_YEAR
            ),
            "independent_earner_tax_credit_period_whole_months": 12,
            "independent_earner_tax_credit_person_is_natural_person": True,
            "independent_earner_tax_credit_resident_in_new_zealand": residence[
                "ietc_person_resident_in_new_zealand"
            ],
            "independent_earner_tax_credit_receives_main_benefit": (
                gates["ietc_main_benefit_disqualifies"] and weekly_benefit > 0
            ),
            "independent_earner_tax_credit_receives_veterans_pension": gates[
                "ietc_veterans_pension_received"
            ],
            "independent_earner_tax_credit_receives_new_zealand_superannuation": gates[
                "ietc_new_zealand_superannuation_received"
            ],
            "independent_earner_tax_credit_entitled_to_or_receives_wff_tax_credit": (
                gates["ietc_wff_disqualifies"] and family_support_payable
                if primary else False
            ),
            "independent_earner_tax_credit_partner_entitled_to_and_receives_wff_tax_credit": (
                gates["ietc_wff_disqualifies"] and family_support_payable
                if not primary else False
            ),
            "independent_earner_tax_credit_receives_overseas_like_support": gates[
                "ietc_overseas_like_support_received"
            ],
            "independent_earner_tax_credit_partner_receives_overseas_like_wff_tax_credit": gates[
                "ietc_overseas_like_support_received"
            ],
        }
        values = self._evaluate(
            entity="Person",
            entity_id=(
                f"{scenario.id}:p{person}:ietc:{decimal_text(weekly_wage)}"
            ),
            inputs=inputs,
            outputs=[IETC_OUTPUT],
        )
        actual = scalar_decimal(values, IETC_OUTPUT) / WEEKS_IN_MODEL_YEAR
        annual_income = weekly_wage * WEEKS_IN_MODEL_YEAR
        status_eligible = (
            (not gates["ietc_main_benefit_disqualifies"] or weekly_benefit <= 0)
            and (not gates["ietc_wff_disqualifies"] or not family_support_payable)
        )
        continuous = D0
        if (
            status_eligible
            and annual_income >= self.ietc_minimum_income
        ):
            continuous = max(
                D0,
                self.ietc_full_year_amount
                - max(
                    D0,
                    annual_income - self.ietc_abatement_threshold,
                )
                * self.ietc_abatement_rate,
            ) / WEEKS_IN_MODEL_YEAR
        return actual, continuous

    def winter_energy_average(
        self,
        scenario: Scenario,
        net_benefit: Decimal,
    ) -> Decimal:
        if net_benefit <= 0:
            return D0
        values = self._evaluate(
            entity="Person",
            entity_id=f"{scenario.id}:winter-energy-rate",
            inputs={
                "winter_energy_payment_single": not scenario.partnered,
                "winter_energy_payment_has_dependent_children": bool(
                    scenario.children
                ),
            },
            outputs=[WEP_RATE_OUTPUT],
        )
        return scalar_decimal(values, WEP_RATE_OUTPUT) / WEEKS_IN_MODEL_YEAR

    def accommodation_cutout(
        self,
        scenario: Scenario,
        *,
        treasury_host_aligned: bool,
    ) -> Decimal:
        key = (
            scenario.partnered,
            len(scenario.children),
            treasury_host_aligned,
        )
        if key in self._as_cutout_cache:
            return self._as_cutout_cache[key]
        if not scenario.partnered and scenario.children:
            # Treasury's source-defined lone-parent AS branch is deliberately
            # not the SPS/JSS-with-children benefit schedule used for current
            # net benefit. R/emtr.R lines 324-329 instead combine the lone-
            # parent JSS base rate with the JSS (Income Test 3) 70% scale.
            raw_cutout = (
                self.benefit_income_test_lower_threshold
                + self.jobseeker_single_with_children_rate
                / self.benefit_income_test_3_rate
            )
        else:
            anchor = Decimal(500)
            at_anchor = self.jobseeker_payment(scenario, anchor)
            after_one_dollar = self.jobseeker_payment(scenario, anchor + D1)
            reduction_per_dollar = at_anchor - after_one_dollar
            if at_anchor <= 0 or reduction_per_dollar <= 0:
                raise HarnessError(
                    "could not derive Accommodation Supplement cutout from the "
                    f"Jobseeker schedule for {scenario.id}"
                )
            raw_cutout = anchor + at_anchor / reduction_per_dollar
        if treasury_host_aligned:
            # Raw IncomeExplorer imposes this annual-dollar ceiling. Regulation
            # 18 instead asks for the exact income level that extinguishes JSS.
            annual_ceiling = (
                raw_cutout * WEEKS_IN_MODEL_YEAR
            ).to_integral_value(rounding=ROUND_CEILING)
            cutout = annual_ceiling / WEEKS_IN_MODEL_YEAR
        else:
            cutout = raw_cutout
        self._as_cutout_cache[key] = cutout
        return cutout

    def accommodation_values(
        self,
        *,
        scenario: Scenario,
        weekly_wages: Decimal,
        net_benefit: Decimal,
        eldest_ftc_annual: Decimal,
        treasury_host_aligned: bool,
    ) -> tuple[Decimal, Decimal, Decimal, Decimal, Decimal]:
        zero_income_benefit = sum(
            self.zero_income_jobseeker_components(scenario), D0
        )
        base_rate = zero_income_benefit
        if scenario.children:
            # Social Security Regulations 2018 reg 17 uses the annual FTC rate
            # divided by 52. Treasury raw emtr() instead divides by 365/7; that
            # source-host convention is retained only in the labeled diagnostic.
            base_rate += eldest_ftc_annual / (
                WEEKS_IN_MODEL_YEAR if treasury_host_aligned else D52
            )
        cutout = self.accommodation_cutout(
            scenario,
            treasury_host_aligned=treasury_host_aligned,
        )
        rent = scenario.accommodation_rent
        boarder = scenario.accommodation_boarder
        area = scenario.accommodation_area
        inputs: dict[str, bool | int | Decimal] = {
            "accommodation_supplement_additional_resident_in_social_housing": False,
            "accommodation_supplement_area_let_to_nonresidents": D0,
            "accommodation_supplement_base_rate_weekly_amount": base_rate,
            "accommodation_supplement_boarder": boarder,
            "accommodation_supplement_business_use_area": D0,
            "accommodation_supplement_costs_are_homeownership": not rent,
            "accommodation_supplement_has_dependent_children": bool(
                scenario.children
            ),
            "accommodation_supplement_has_two_or_more_dependent_children": (
                len(scenario.children) >= 2
            ),
            "accommodation_supplement_in_relationship": scenario.partnered,
            "accommodation_supplement_joint_owner_with_resident": False,
            "accommodation_supplement_non_beneficiary": net_benefit == 0,
            "accommodation_supplement_non_beneficiary_income_cutout_weekly_amount": (
                cutout
            ),
            "accommodation_supplement_owner_weekly_payment_share": D0,
            "accommodation_supplement_owner_weekly_required_payments": (
                scenario.accommodation_costs
                if not rent and not boarder
                else D0
            ),
            "accommodation_supplement_owns_premises_and_not_joint_owner_with_resident": (
                not rent and not boarder
            ),
            "accommodation_supplement_relevant_weekly_income": weekly_wages,
            "accommodation_supplement_resides_in_area_1": area == 1,
            "accommodation_supplement_resides_in_area_2": area == 2,
            "accommodation_supplement_resides_in_area_3": area == 3,
            "accommodation_supplement_self_contained_area_let_to_residents": D0,
            "accommodation_supplement_social_housing_weekly_contributions_paid": D0,
            "accommodation_supplement_sole_parent": (
                not scenario.partnered and bool(scenario.children)
            ),
            "accommodation_supplement_total_premises_area": D0,
            "accommodation_supplement_weekly_board_and_lodgings_paid": (
                scenario.weekly_board_and_lodgings_paid
            ),
            "accommodation_supplement_weekly_boarder_payments_received": D0,
            "accommodation_supplement_weekly_contributions_received_from_additional_residents": D0,
            "accommodation_supplement_weekly_rent_paid": (
                scenario.accommodation_costs if rent and not boarder else D0
            ),
            "accommodation_supplement_weekly_rent_received_from_other_residents": D0,
        }
        values = self._evaluate(
            entity="Family",
            entity_id=(
                f"{scenario.id}:as:"
                f"{'treasury-host' if treasury_host_aligned else 'statutory'}:"
                f"{decimal_text(weekly_wages)}"
            ),
            inputs=inputs,
            outputs=[
                AS_UNROUNDED_OUTPUT,
                AS_ROUNDED_OUTPUT,
                AS_QUALIFYING_COST_OUTPUT,
            ],
        )
        return (
            scalar_decimal(values, AS_UNROUNDED_OUTPUT),
            scalar_decimal(values, AS_ROUNDED_OUTPUT),
            base_rate,
            cutout,
            scalar_decimal(values, AS_QUALIFYING_COST_OUTPUT),
        )

    def evaluate_state(
        self,
        scenario: Scenario,
        weekly_wage1: Decimal,
    ) -> dict[str, Decimal | str]:
        weekly_wage2 = scenario.gross_wage2
        weekly_wages = weekly_wage1 + weekly_wage2
        hours1 = (
            weekly_wage1 / scenario.wage1_hourly
            if scenario.wage1_hourly > 0
            else D0
        )
        hours_total = hours1 + scenario.hours2
        benefit_components = self.benefit_components(scenario, weekly_wages)
        net_benefit = sum(benefit_components, D0)
        gross_benefits = tuple(
            self.gross_benefit_from_net_weekly(value)
            for value in benefit_components
        )
        gross_benefit_total = sum(gross_benefits, D0)

        primary_gross_benefit = gross_benefits[0]
        tax1 = (
            self.annual_tax(
                (primary_gross_benefit + weekly_wage1) * WEEKS_IN_MODEL_YEAR
            )
            - self.annual_tax(primary_gross_benefit * WEEKS_IN_MODEL_YEAR)
        ) / WEEKS_IN_MODEL_YEAR
        acc1 = self.annual_acc(
            weekly_wage1 * WEEKS_IN_MODEL_YEAR
        ) / WEEKS_IN_MODEL_YEAR
        net_wage1 = weekly_wage1 - tax1 - acc1

        tax2 = D0
        acc2 = D0
        net_wage2 = D0
        if scenario.partnered:
            partner_gross_benefit = gross_benefits[1]
            tax2 = (
                self.annual_tax(
                    (partner_gross_benefit + weekly_wage2)
                    * WEEKS_IN_MODEL_YEAR
                )
                - self.annual_tax(partner_gross_benefit * WEEKS_IN_MODEL_YEAR)
            ) / WEEKS_IN_MODEL_YEAR
            acc2 = self.annual_acc(
                weekly_wage2 * WEEKS_IN_MODEL_YEAR
            ) / WEEKS_IN_MODEL_YEAR
            net_wage2 = weekly_wage2 - tax2 - acc2
        net_wage = net_wage1 + net_wage2

        family_values = self.family_credit_values(
            scenario=scenario,
            weekly_wages=weekly_wages,
            weekly_gross_benefits=gross_benefit_total,
            net_benefit=net_benefit,
            wage_tax_total=tax1 + tax2,
            hours_total=hours_total,
        )
        ftc_before = (
            scalar_decimal(family_values, FTC_BEFORE_OUTPUT)
            / WEEKS_IN_MODEL_YEAR
        )
        ftc = scalar_decimal(family_values, FTC_OUTPUT) / WEEKS_IN_MODEL_YEAR
        iwtc_before = scalar_decimal(family_values, IWTC_BEFORE_OUTPUT) / D52
        iwtc_remaining_abatement = (
            scalar_decimal(family_values, IWTC_REMAINING_ABATEMENT_OUTPUT)
            / WEEKS_IN_MODEL_YEAR
        )
        iwtc = max(D0, iwtc_before - iwtc_remaining_abatement)
        mftc = scalar_decimal(family_values, MFTC_OUTPUT) / D52
        wff = ftc + iwtc
        family_scheme_income = scalar_decimal(family_values, FSI_OUTPUT)
        continuous_wff = max(
            D0,
            ftc_before
            + iwtc_before
            - (
                max(
                    D0,
                    family_scheme_income - self.wff_abatement_threshold,
                )
                * self.wff_abatement_rate
                / WEEKS_IN_MODEL_YEAR
            ),
        )

        annual_wages = weekly_wages * WEEKS_IN_MODEL_YEAR
        annual_base_income = (
            weekly_wages + gross_benefit_total
        ) * WEEKS_IN_MODEL_YEAR
        (
            best_start,
            best_start_per_child,
            best_start_continuous,
        ) = self.best_start_total(
            scenario=scenario,
            annual_base_income=annual_base_income,
            annual_wages=annual_wages,
        )
        family_support_payable = (wff + mftc + best_start) > 0
        ietc1, ietc1_continuous = self.ietc_for_person(
            scenario=scenario,
            person=1,
            weekly_wage=weekly_wage1,
            weekly_benefit=benefit_components[0],
            family_support_payable=family_support_payable,
        )
        ietc2 = D0
        ietc2_continuous = D0
        if scenario.partnered:
            ietc2, ietc2_continuous = self.ietc_for_person(
                scenario=scenario,
                person=2,
                weekly_wage=weekly_wage2,
                weekly_benefit=benefit_components[1],
                family_support_payable=family_support_payable,
            )
        ietc = ietc1 + ietc2
        ietc_continuous = ietc1_continuous + ietc2_continuous
        winter_energy = self.winter_energy_average(scenario, net_benefit)
        (
            as_unrounded,
            as_rounded,
            as_base_rate,
            as_cutout,
            as_qualifying_cost,
        ) = self.accommodation_values(
            scenario=scenario,
            weekly_wages=weekly_wages,
            net_benefit=net_benefit,
            eldest_ftc_annual=self.ftc_eldest_annual,
            treasury_host_aligned=False,
        )
        (
            as_treasury_host_aligned,
            _as_treasury_host_aligned_rounded,
            as_treasury_host_base_rate,
            as_treasury_host_cutout,
            _as_treasury_host_qualifying_cost,
        ) = self.accommodation_values(
            scenario=scenario,
            weekly_wages=weekly_wages,
            net_benefit=net_benefit,
            eldest_ftc_annual=self.ftc_eldest_annual,
            treasury_host_aligned=True,
        )
        net_income = (
            net_wage
            + net_benefit
            + wff
            + mftc
            + ietc
            + winter_energy
            + best_start
            + as_unrounded
        )
        return {
            "gross_wage1": weekly_wage1,
            "hours1": hours1,
            "gross_wage1_annual": weekly_wage1 * WEEKS_IN_MODEL_YEAR,
            "gross_wage2": weekly_wage2,
            "wage1_tax": tax1,
            "wage1_ACC_levy": acc1,
            "net_wage1": net_wage1,
            "net_wage": net_wage,
            "net_benefit": net_benefit,
            "FTC_abated": ftc,
            "IWTC_abated": iwtc,
            "MFTC": mftc,
            "IETC_abated": ietc,
            "WinterEnergy": winter_energy,
            "BestStart_Total": best_start,
            "BestStart_Total_naive_per_child_abatement": (
                best_start_per_child
            ),
            "BestStart_Total_continuous_abatement": (
                best_start_continuous
            ),
            "AS_Amount": as_unrounded,
            "WFF_abated": wff,
            "Net_Income": net_income,
            "Net_Income_annual": net_income * WEEKS_IN_MODEL_YEAR,
            "AS_Amount_statutory_rounded": as_rounded,
            "AS_Amount_treasury_host_aligned": as_treasury_host_aligned,
            "AS_statutory_base_rate": as_base_rate,
            "AS_treasury_host_base_rate": as_treasury_host_base_rate,
            "AS_statutory_cutout": as_cutout,
            "AS_treasury_host_cutout": as_treasury_host_cutout,
            "AS_qualifying_accommodation_cost": as_qualifying_cost,
            "gross_benefit_taxable_weekly": gross_benefit_total,
            "wage2_tax": tax2,
            "wage2_ACC_levy": acc2,
            "net_wage2": net_wage2,
            "family_scheme_income": scalar_decimal(family_values, FSI_OUTPUT),
            "WFF_abated_continuous_abatement": continuous_wff,
            "IETC_abated_continuous_abatement": ietc_continuous,
            "iwtc_entitlement": str(
                family_values[IWTC_ENTITLEMENT_OUTPUT]
            ),
        }


def sweep_scenario(
    evaluator: ModelEvaluator,
    scenario: Scenario,
) -> dict[int, dict[str, Decimal | str | None]]:
    required_wages = set(scenario.sampled_wages)
    if 1500 in required_wages:
        required_wages.add(1499)
    required_wages.update(
        wage + 1 for wage in scenario.sampled_wages if wage < 1500
    )
    states = {
        wage: evaluator.evaluate_state(scenario, Decimal(wage))
        for wage in sorted(required_wages)
    }
    baseline = scalar_decimal(states[0], "Net_Income")
    sampled: dict[int, dict[str, Decimal | str | None]] = {}
    for wage in scenario.sampled_wages:
        state: dict[str, Decimal | str | None] = dict(states[wage])
        if wage == 1500:
            current_state = states[1499]
            following_state = states[1500]
        else:
            current_state = states[wage]
            following_state = states[wage + 1]
        current = scalar_decimal(current_state, "Net_Income")
        following = scalar_decimal(following_state, "Net_Income")
        state["EMTR"] = D1 - (following - current)
        for component in EMTR_COMPONENT_BASE_COLUMNS:
            current_component = scalar_decimal(current_state, component)
            following_component = scalar_decimal(
                following_state, component
            )
            state[f"RuleSpec_EMTR_{component}"] = (
                D1 - (following_component - current_component)
                if component == "net_wage"
                else current_component - following_component
            )
        state["RuleSpec_EMTR_AS_Amount_treasury_host_aligned"] = (
            scalar_decimal(
                current_state, "AS_Amount_treasury_host_aligned"
            )
            - scalar_decimal(
                following_state, "AS_Amount_treasury_host_aligned"
            )
        )
        state["RuleSpec_EMTR_BestStart_Total_naive_per_child_abatement"] = (
            scalar_decimal(
                current_state,
                "BestStart_Total_naive_per_child_abatement",
            )
            - scalar_decimal(
                following_state,
                "BestStart_Total_naive_per_child_abatement",
            )
        )
        recomposed_emtr = sum(
            (
                scalar_decimal(state, f"RuleSpec_EMTR_{component}")
                for component in EMTR_COMPONENT_BASE_COLUMNS
            ),
            D0,
        )
        if abs(recomposed_emtr - scalar_decimal(state, "EMTR")) > (
            ENGINE_DECIMAL_TOLERANCE * Decimal(10)
        ):
            raise HarnessError(
                f"RuleSpec EMTR components do not recompose for "
                f"{scenario.id} wage {wage}"
            )
        rulespec_acc_marginal = (
            scalar_decimal(following_state, "wage1_ACC_levy")
            - scalar_decimal(current_state, "wage1_ACC_levy")
        )
        state["EMTR_ACC_rounding_component"] = (
            rulespec_acc_marginal - ORACLE_ACC_WEEKLY_RATE
        )
        continuous_fields = {
            "WFF": (
                "WFF_abated",
                "WFF_abated_continuous_abatement",
            ),
            "BestStart": (
                "BestStart_Total",
                "BestStart_Total_continuous_abatement",
            ),
            "IETC": (
                "IETC_abated",
                "IETC_abated_continuous_abatement",
            ),
        }
        for label, (actual_field, continuous_field) in (
            continuous_fields.items()
        ):
            actual_loss = (
                scalar_decimal(current_state, actual_field)
                - scalar_decimal(following_state, actual_field)
            )
            continuous_loss = (
                scalar_decimal(current_state, continuous_field)
                - scalar_decimal(following_state, continuous_field)
            )
            state[f"EMTR_{label}_complete_dollar_component"] = (
                actual_loss - continuous_loss
            )
        state["EMTR_WFF_discrete_bound"] = (
            evaluator.wff_abatement_rate / WEEKS_IN_MODEL_YEAR
        )
        state["EMTR_BestStart_discrete_bound"] = (
            evaluator.best_start_abatement_rate / WEEKS_IN_MODEL_YEAR
        )
        state["EMTR_IETC_discrete_bound"] = (
            evaluator.ietc_abatement_rate / WEEKS_IN_MODEL_YEAR
        )
        net_income = scalar_decimal(state, "Net_Income")
        state["RR"] = baseline / net_income if net_income != 0 else None
        gross_income = Decimal(wage) + scenario.gross_wage2
        state["PTR"] = (
            D1 - (net_income - baseline) / gross_income
            if gross_income != 0
            else None
        )
        sampled[wage] = state
    return sampled


def attach_treasury_emtr_components(
    *,
    oracle: Mapping[str, Any],
    scenarios: Sequence[Scenario],
    sweeps: Mapping[
        str,
        Mapping[int, dict[str, Decimal | str | None]],
    ],
) -> None:
    provenance = oracle["scenario_provenance"]
    for scenario in scenarios:
        component_rows = provenance[scenario.id][
            "treasury_emtr_components"
        ]
        if len(component_rows) != len(scenario.sampled_wages):
            raise HarnessError(
                f"{scenario.id} Treasury EMTR component row count changed"
            )
        for index, wage in enumerate(scenario.sampled_wages):
            raw = component_rows[index]
            if dec(raw["gross_wage1"]) != Decimal(wage):
                raise HarnessError(
                    f"{scenario.id} Treasury EMTR component row {index} "
                    f"is not wage {wage}"
                )
            state = sweeps[scenario.id][wage]
            for column in TREASURY_EMTR_COMPONENT_COLUMNS:
                state[f"Treasury_{column}"] = dec(raw[column])


def validate_expanded_coverage(
    *,
    oracle: Mapping[str, Any],
    scenarios: Sequence[Scenario],
    sweeps: Mapping[
        str,
        Mapping[int, Mapping[str, Decimal | str | None]],
    ],
) -> dict[str, Any]:
    scenario_by_id = {scenario.id: scenario for scenario in scenarios}
    raw_by_id = {str(item["id"]): item for item in oracle["scenarios"]}

    expected_profiles: dict[str, tuple[Any, ...]] = {
        "lone_parent_two_teens_jss": (
            False, Decimal("18.5"), (14, 16), D0, D0, D0, True, 2,
            False, D0,
        ),
        "couple_two_best_start_children_binding": (
            True, Decimal("18.5"), (1, 2), Decimal(740), Decimal(40),
            D0, True, 2, False, D0,
        ),
        "couple_two_children_dual_full_time": (
            True, Decimal("18.5"), (6, 15), Decimal(740), Decimal(40),
            D0, True, 2, False, D0,
        ),
        "single_childless_ietc_focused": (
            False, Decimal(25), (), D0, D0, D0, True, 2, False, D0,
        ),
        "lone_parent_two_children_area4_high_rent_cap": (
            False, Decimal("18.5"), (6, 15), D0, D0, Decimal(600),
            True, 4, False, D0,
        ),
        "couple_childless_boarder_proxy": (
            True, Decimal("18.5"), (), D0, D0, Decimal(248), True, 2,
            True, Decimal(400),
        ),
        "large_family_four_children_age_bands": (
            False, Decimal("18.5"), (3, 6, 13, 16), D0, D0, D0,
            True, 1, False, D0,
        ),
    }
    for scenario_id, expected in expected_profiles.items():
        scenario = scenario_by_id[scenario_id]
        actual = (
            scenario.partnered,
            scenario.wage1_hourly,
            scenario.children,
            scenario.gross_wage2,
            scenario.hours2,
            scenario.accommodation_costs,
            scenario.accommodation_rent,
            scenario.accommodation_area,
            scenario.accommodation_boarder,
            scenario.weekly_board_and_lodgings_paid,
        )
        if actual != expected:
            raise HarnessError(
                f"expanded scenario semantics changed for {scenario_id}: "
                f"{actual!r}"
            )

    def treasury_state(scenario_id: str, wage: int) -> Mapping[str, Any]:
        scenario = scenario_by_id[scenario_id]
        index = scenario.sampled_wages.index(wage)
        return raw_by_id[scenario_id]["sampled_outputs"][index]

    best_start_id = "couple_two_best_start_children_binding"
    best_start_rows: list[dict[str, Any]] = []
    for wage in (775, 1000, 1125, 1476):
        state = sweeps[best_start_id][wage]
        row = {
            "weekly_wage": wage,
            "treasury": dec(
                treasury_state(best_start_id, wage)["BestStart_Total"]
            ),
            "rulespec_after_aggregate_once": scalar_decimal(
                state, "BestStart_Total"
            ),
            "rulespec_before_naive_per_child_abatement": scalar_decimal(
                state, "BestStart_Total_naive_per_child_abatement"
            ),
        }
        if (
            row["rulespec_after_aggregate_once"]
            < row["rulespec_before_naive_per_child_abatement"]
        ):
            raise HarnessError(
                "Best Start aggregate-once correction is not active at "
                f"wage {wage}"
            )
        best_start_rows.append(row)
    if not any(
        row["rulespec_after_aggregate_once"]
        > row["rulespec_before_naive_per_child_abatement"]
        for row in best_start_rows
    ):
        raise HarnessError(
            "Best Start aggregate-once and naive per-child results never diverge"
        )
    treasury_best_start_at_zero = dec(
        treasury_state(best_start_id, 0)["BestStart_Total"]
    )
    if not any(
        D0 < row["treasury"] < treasury_best_start_at_zero
        for row in best_start_rows
    ):
        raise HarnessError("Treasury Best Start abatement is not binding")

    dual_id = "couple_two_children_dual_full_time"
    dual_state = sweeps[dual_id][740]
    if (
        scalar_decimal(dual_state, "hours1") != Decimal(40)
        or scalar_decimal(dual_state, "wage2_tax") <= 0
        or scalar_decimal(dual_state, "wage2_ACC_levy") <= 0
    ):
        raise HarnessError(
            "dual-full-time partner tax/ACC path is not active"
        )

    ietc_id = "single_childless_ietc_focused"
    ietc_state = sweeps[ietc_id][740]
    treasury_ietc = dec(treasury_state(ietc_id, 740)["IETC_abated"])
    if (
        treasury_ietc <= 0
        or scalar_decimal(ietc_state, "IETC_abated") <= 0
        or scalar_decimal(ietc_state, "net_benefit") != 0
    ):
        raise HarnessError("focused IETC profile does not pay IETC")

    area4_id = "lone_parent_two_children_area4_high_rent_cap"
    cap_wages = tuple(
        wage
        for wage in scenario_by_id[area4_id].sampled_wages
        if (
            dec(treasury_state(area4_id, wage)["AS_Amount"])
            == Decimal(120)
            and scalar_decimal(sweeps[area4_id][wage], "AS_Amount")
            == Decimal(120)
        )
    )
    if not cap_wages:
        raise HarnessError("Area 4 high-rent scenario never binds the $120 cap")

    boarder_id = "couple_childless_boarder_proxy"
    qualifying_costs = {
        scalar_decimal(
            sweeps[boarder_id][wage],
            "AS_qualifying_accommodation_cost",
        )
        for wage in scenario_by_id[boarder_id].sampled_wages
    }
    if qualifying_costs != {Decimal(248)}:
        raise HarnessError(
            "RuleSpec boarder path did not convert $400 to $248"
        )

    large_id = "large_family_four_children_age_bands"
    large_zero = sweeps[large_id][0]
    if (
        scalar_decimal(large_zero, "WinterEnergy") <= 0
        or scalar_decimal(large_zero, "FTC_abated") <= 0
    ):
        raise HarnessError(
            "large-family FTC/Winter Energy paths are not active"
        )

    jss_id = "lone_parent_two_teens_jss"
    if scalar_decimal(sweeps[jss_id][0], "net_benefit") <= 0:
        raise HarnessError("14+ lone-parent JSS branch is not active")

    original_ietc_id = "single_no_children_area2_no_housing_costs"
    original_ietc_positive_wages = tuple(
        wage
        for wage in scenario_by_id[original_ietc_id].sampled_wages
        if dec(
            treasury_state(original_ietc_id, wage)["IETC_abated"]
        ) > 0
    )
    if not original_ietc_positive_wages:
        raise HarnessError(
            "continuity snapshot unexpectedly has no positive IETC"
        )

    return {
        "scenario_count": len(scenarios),
        "sampled_point_count": sum(
            len(scenario.sampled_wages) for scenario in scenarios
        ),
        "best_start_aggregate_fix": best_start_rows,
        "dual_full_time_partner_at_wage_740": {
            "primary_hours": scalar_decimal(dual_state, "hours1"),
            "partner_hours": scenario_by_id[dual_id].hours2,
            "partner_weekly_tax": scalar_decimal(dual_state, "wage2_tax"),
            "partner_weekly_acc": scalar_decimal(
                dual_state, "wage2_ACC_levy"
            ),
        },
        "focused_ietc_at_wage_740": {
            "treasury": treasury_ietc,
            "rulespec": scalar_decimal(ietc_state, "IETC_abated"),
        },
        "original_profile_positive_ietc_wages": (
            original_ietc_positive_wages
        ),
        "area4_cap": {
            "weekly_maximum": Decimal(120),
            "binding_wages_on_both_sides": cap_wages,
        },
        "boarder_proxy": {
            "weekly_board_and_lodgings": Decimal(400),
            "rulespec_qualifying_cost": Decimal(248),
            "treasury_supplied_rent_like_cost": Decimal(248),
            "native_treasury_boarder_input": False,
        },
        "large_family_at_wage_0": {
            "children": scenario_by_id[large_id].children,
            "rulespec_ftc": scalar_decimal(large_zero, "FTC_abated"),
            "rulespec_winter_energy": scalar_decimal(
                large_zero, "WinterEnergy"
            ),
            "treasury_winter_energy": dec(
                treasury_state(large_id, 0)["WinterEnergy"]
            ),
        },
    }


@dataclass(frozen=True)
class ComparisonRow:
    scenario_id: str
    scenario_description: str
    weekly_wage: int
    column: str
    unit: str
    treasury: Decimal
    rulespec: Decimal
    signed_delta: Decimal
    absolute_delta: Decimal
    relative_delta: Decimal | None
    classification: str
    reason_code: str


CLASSIFICATION_DETAILS: dict[str, tuple[str, str, str]] = {
    "MATCH_SNAPSHOT_PRECISION": (
        "match",
        "Match at oracle precision",
        "The absolute difference is at most half of the snapshot's six-decimal "
        "rounding unit.",
    ),
    "A_SCENARIO_ENCODING": (
        "a",
        "Scenario encoding bug",
        "A direct scenario identity (wage, hours, or annualisation) did not "
        "survive the host-to-engine mapping.",
    ),
    "B_BENEFIT_VINTAGE": (
        "b",
        "Main-benefit forecast vintage",
        "Treasury's BEFU25 TY27 rate differs from the enacted 1 April 2026 "
        "rate under Social Security Act 2018 Schedule 4 as amended by the "
        "Social Security (Rates of Benefits and Allowances) Order 2026 cl 5.",
    ),
    "B_BENEFIT_GROSSUP_TAX": (
        "b",
        "Benefit-vintage tax interaction",
        "Treasury taxes wages incrementally above its forecast-vintage grossed "
        "benefit. The enacted benefit rate changes that tax base before the "
        "same Schedule 1 tax rates are applied.",
    ),
    "B_WFF_VINTAGE": (
        "b",
        "Working for Families forecast vintage",
        "The BEFU25 FTC, IWTC, MFTC, or Best Start prescribed amount differs "
        "from the amount enacted for 2026/27 under Income Tax Act 2007 "
        "ss MD 3, MD 10, ME 1, or MG 2.",
    ),
    "B_AS_UPSTREAM_VINTAGE": (
        "b",
        "Accommodation Supplement compound difference, primary vintage",
        "A Treasury-host-aligned diagnostic establishes a BEFU25-versus-enacted "
        "benefit/FTC effect. The statutory primary also retains smaller reg "
        "17–18 host conventions: FTC divided by 52 and the exact JSS cutout.",
    ),
    "B_COMPOUND_NET_INCOME": (
        "b",
        "Compound forecast-vintage level difference",
        "At least one material component of net income is a class-(b) benefit "
        "or tax-credit vintage difference; smaller class-(c) rounding effects "
        "may also be present.",
    ),
    "B_WINTER_ENERGY_BENEFIT_GATE": (
        "b",
        "Winter Energy benefit-gate vintage interaction",
        "Winter Energy uses the same rate on both sides, but its positive-main-"
        "benefit gate changes in this interval because the BEFU25 benefit amount "
        "and the enacted 2026/27 amount extinguish at different wages.",
    ),
    "B_IETC_UPSTREAM_VINTAGE_GATE": (
        "b",
        "IETC eligibility gate moved by another instrument's vintage",
        "IETC parameters agree, but Income Tax Act 2007 s LC 13 excludes a "
        "person receiving a main benefit or WFF. A BEFU25-versus-enacted "
        "benefit, WFF, or Best Start boundary therefore changes whether IETC "
        "is present.",
    ),
    "B_EMTR_INSTRUMENT_VINTAGE": (
        "b",
        "Named-instrument vintage boundary in EMTR",
        "Treasury's own component EMTRs and RuleSpec's independently recomputed "
        "components isolate the difference to a main-benefit, WFF, MFTC, Best "
        "Start, downstream IETC/Winter Energy gate, or Accommodation Supplement "
        "boundary moved by BEFU25-versus-enacted instrument amounts.",
    ),
    "C_ACC_ANNUAL_CENTS": (
        "c",
        "ACC period/rounding convention",
        "Treasury applies an unrounded weekly 1.75% levy. RuleSpec applies the "
        "annual including-GST levy and its whole-cent rounding, then converts "
        "the result by 365/7 weeks.",
    ),
    "C_NET_WAGE_ROUNDING": (
        "c",
        "Net-wage rounding propagation",
        "The net-wage residual is the propagation of annual-cent ACC rounding "
        "through a weekly value (and, for partner wages, the same conversion).",
    ),
    "C_COMPOUND_NET_INCOME": (
        "c",
        "Compound period/rounding difference",
        "No established forecast-vintage component is active; the aggregate "
        "difference is inherited from documented statutory period or rounding "
        "conventions.",
    ),
    "C_IETC_WHOLE_DOLLARS": (
        "c",
        "IETC statutory whole-dollar convention",
        "RuleSpec retains the annual statutory whole-dollar calculation before "
        "converting by 365/7; Treasury's R flow is continuous weekly arithmetic.",
    ),
    "C_AS_STATUTORY_HOST": (
        "c",
        "Accommodation Supplement statutory host convention",
        "The statutory primary uses the reg 17 annual FTC amount divided by 52 "
        "and reg 18's exact JSS vanishing point; raw Treasury uses 365/7 and an "
        "annual-dollar-ceiled cutout.",
    ),
    "C_EMTR_DISCRETE_ROUNDING": (
        "c",
        "EMTR statutory discreteness/period convention",
        "Both sides use the same weekly $1 forward interval. Component "
        "recomposition confines the residual to annual-cent ACC rounding, "
        "complete-dollar WFF, Best Start, or IETC arithmetic, statutory "
        "Accommodation Supplement host inputs, and six-decimal Treasury "
        "component display rounding.",
    ),
    "D_UNEXPLAINED": (
        "d",
        "Unexplained",
        "The observed difference is not established by the pinned-vintage or "
        "unit/period evidence in this audit.",
    ),
}


def comparison_unit(column: str) -> str:
    if column == "hours1":
        return "hours/week"
    if column == "gross_wage1_annual" or column == "Net_Income_annual":
        return "NZD/year"
    if column == "EMTR":
        return "ratio"
    return "NZD/week"


def outside_oracle_precision(left: Decimal, right: Decimal) -> bool:
    return abs(left - right) > ORACLE_SIX_DECIMAL_TOLERANCE


def classify_discrepancy(
    *,
    column: str,
    treasury_state: Mapping[str, Any],
    rulespec_state: Mapping[str, Decimal | str | None],
    signed_delta: Decimal,
    absolute_delta: Decimal,
) -> tuple[str, str]:
    if absolute_delta <= ORACLE_SIX_DECIMAL_TOLERANCE:
        return "match", "MATCH_SNAPSHOT_PRECISION"

    if column in {
        "gross_wage1",
        "hours1",
        "gross_wage1_annual",
        "gross_wage2",
    }:
        return "a", "A_SCENARIO_ENCODING"

    treasury_benefit = dec(treasury_state["net_benefit"])
    rulespec_benefit = scalar_decimal(rulespec_state, "net_benefit")
    benefit_vintage_active = (
        treasury_benefit > 0
        or rulespec_benefit > 0
    ) and outside_oracle_precision(treasury_benefit, rulespec_benefit)

    if column == "wage1_tax":
        if benefit_vintage_active:
            return "b", "B_BENEFIT_GROSSUP_TAX"
        return "d", "D_UNEXPLAINED"
    if column == "wage1_ACC_levy":
        return "c", "C_ACC_ANNUAL_CENTS"
    if column in {"net_wage1", "net_wage"}:
        treasury_tax = dec(treasury_state["wage1_tax"])
        rulespec_tax = scalar_decimal(rulespec_state, "wage1_tax")
        if (
            benefit_vintage_active
            and outside_oracle_precision(treasury_tax, rulespec_tax)
        ):
            return "b", "B_BENEFIT_GROSSUP_TAX"
        return "c", "C_NET_WAGE_ROUNDING"
    if column == "net_benefit":
        return "b", "B_BENEFIT_VINTAGE"
    if column in {
        "FTC_abated",
        "IWTC_abated",
        "MFTC",
        "BestStart_Total",
        "WFF_abated",
    }:
        return "b", "B_WFF_VINTAGE"
    if column == "IETC_abated":
        ietc_discrete_bound = scalar_decimal(
            rulespec_state, "EMTR_IETC_discrete_bound"
        )
        if absolute_delta > (
            ietc_discrete_bound + ORACLE_SIX_DECIMAL_TOLERANCE
        ):
            return "b", "B_IETC_UPSTREAM_VINTAGE_GATE"
        return "c", "C_IETC_WHOLE_DOLLARS"
    if column == "WinterEnergy":
        if benefit_vintage_active:
            return "b", "B_WINTER_ENERGY_BENEFIT_GATE"
        return "d", "D_UNEXPLAINED"
    if column == "AS_Amount":
        treasury_as = dec(treasury_state["AS_Amount"])
        aligned_as = scalar_decimal(
            rulespec_state, "AS_Amount_treasury_host_aligned"
        )
        if outside_oracle_precision(treasury_as, aligned_as):
            return "b", "B_AS_UPSTREAM_VINTAGE"
        if outside_oracle_precision(
            aligned_as,
            scalar_decimal(rulespec_state, "AS_Amount"),
        ):
            return "c", "C_AS_STATUTORY_HOST"
        return "d", "D_UNEXPLAINED"
    if column in {"Net_Income", "Net_Income_annual"}:
        material_vintage_columns = (
            "wage1_tax",
            "net_benefit",
            "FTC_abated",
            "IWTC_abated",
            "MFTC",
            "BestStart_Total",
            "AS_Amount",
        )
        material_vintage_difference = False
        for name in material_vintage_columns:
            component_differs = outside_oracle_precision(
                dec(treasury_state[name]),
                scalar_decimal(rulespec_state, name),
            )
            if not component_differs:
                continue
            if name == "wage1_tax" and not benefit_vintage_active:
                continue
            if name == "AS_Amount" and not outside_oracle_precision(
                dec(treasury_state["AS_Amount"]),
                scalar_decimal(
                    rulespec_state,
                    "AS_Amount_treasury_host_aligned",
                ),
            ):
                continue
            material_vintage_difference = True
            break
        if material_vintage_difference:
            return "b", "B_COMPOUND_NET_INCOME"
        return "c", "C_COMPOUND_NET_INCOME"
    if column == "EMTR":
        component_tolerance = Decimal("0.000004")
        component_deltas = {
            component: (
                scalar_decimal(
                    rulespec_state,
                    f"RuleSpec_EMTR_{component}",
                )
                - scalar_decimal(
                    rulespec_state,
                    f"Treasury_EMTR_{component}",
                )
            )
            for component in EMTR_COMPONENT_BASE_COLUMNS
        }
        recomposed = sum(component_deltas.values(), D0)
        if abs(signed_delta - recomposed) > component_tolerance:
            return "d", "D_UNEXPLAINED"

        vintage_components: list[str] = []
        unexplained_components: list[str] = []

        net_wage_delta = component_deltas["net_wage"]
        acc_component = scalar_decimal(
            rulespec_state, "EMTR_ACC_rounding_component"
        )
        if abs(net_wage_delta) > component_tolerance and (
            abs(net_wage_delta - acc_component) > component_tolerance
        ):
            vintage_components.append("net_wage")

        if abs(component_deltas["net_benefit"]) > component_tolerance:
            vintage_components.append("net_benefit")

        discrete_convention_components = {
            "WFF_abated": (
                "EMTR_WFF_complete_dollar_component",
                "EMTR_WFF_discrete_bound",
            ),
            "IETC_abated": (
                "EMTR_IETC_complete_dollar_component",
                "EMTR_IETC_discrete_bound",
            ),
            "BestStart_Total": (
                "EMTR_BestStart_complete_dollar_component",
                "EMTR_BestStart_discrete_bound",
            ),
        }
        for component, (
            convention_field,
            bound_field,
        ) in discrete_convention_components.items():
            component_delta = component_deltas[component]
            if abs(component_delta) <= component_tolerance:
                continue
            convention_component = scalar_decimal(
                rulespec_state, convention_field
            )
            bound = scalar_decimal(rulespec_state, bound_field)
            if abs(convention_component) > bound + component_tolerance:
                unexplained_components.append(component)
            elif (
                abs(component_delta - convention_component)
                > component_tolerance
            ):
                vintage_components.append(component)

        if abs(component_deltas["MFTC"]) > component_tolerance:
            vintage_components.append("MFTC")
        if abs(component_deltas["WinterEnergy"]) > component_tolerance:
            vintage_components.append("WinterEnergy")

        as_delta = component_deltas["AS_Amount"]
        if abs(as_delta) > component_tolerance:
            treasury_as_component = scalar_decimal(
                rulespec_state, "Treasury_EMTR_AS_Amount"
            )
            aligned_as_delta = (
                scalar_decimal(
                    rulespec_state,
                    "RuleSpec_EMTR_AS_Amount_treasury_host_aligned",
                )
                - treasury_as_component
            )
            if abs(aligned_as_delta) > component_tolerance:
                vintage_components.append("AS_Amount")

        known_components = {
            "net_wage",
            "net_benefit",
            *discrete_convention_components,
            "MFTC",
            "WinterEnergy",
            "AS_Amount",
        }
        for component, component_delta in component_deltas.items():
            if (
                component not in known_components
                and abs(component_delta) > component_tolerance
            ):
                unexplained_components.append(component)
        if unexplained_components:
            return "d", "D_UNEXPLAINED"
        if vintage_components:
            return "b", "B_EMTR_INSTRUMENT_VINTAGE"
        return "c", "C_EMTR_DISCRETE_ROUNDING"
    return "d", "D_UNEXPLAINED"


def build_comparison_rows(
    *,
    oracle: Mapping[str, Any],
    scenarios: Sequence[Scenario],
    sweeps: Mapping[str, Mapping[int, Mapping[str, Decimal | str | None]]],
) -> list[ComparisonRow]:
    oracle_by_id = {str(item["id"]): item for item in oracle["scenarios"]}
    rows: list[ComparisonRow] = []
    for scenario in scenarios:
        raw_scenario = oracle_by_id[scenario.id]
        sampled_outputs = raw_scenario["sampled_outputs"]
        if len(sampled_outputs) != len(scenario.sampled_wages):
            raise HarnessError(
                f"{scenario.id} has {len(sampled_outputs)} oracle points"
            )
        for index, wage in enumerate(scenario.sampled_wages):
            treasury_state = sampled_outputs[index]
            if dec(treasury_state["gross_wage1"]) != Decimal(wage):
                raise HarnessError(
                    f"{scenario.id} oracle row {index} is not wage {wage}"
                )
            rulespec_state = sweeps[scenario.id][wage]
            for column in COMPARISON_COLUMNS:
                treasury_value = dec(treasury_state[column])
                rulespec_value = scalar_decimal(rulespec_state, column)
                signed_delta = rulespec_value - treasury_value
                absolute_delta = abs(signed_delta)
                if treasury_value != 0:
                    relative_delta = absolute_delta / abs(treasury_value)
                elif absolute_delta == 0:
                    relative_delta = D0
                else:
                    relative_delta = None
                classification, reason_code = classify_discrepancy(
                    column=column,
                    treasury_state=treasury_state,
                    rulespec_state=rulespec_state,
                    signed_delta=signed_delta,
                    absolute_delta=absolute_delta,
                )
                expected_class = CLASSIFICATION_DETAILS[reason_code][0]
                if classification != expected_class:
                    raise HarnessError(
                        f"classification metadata mismatch for {reason_code}"
                    )
                rows.append(
                    ComparisonRow(
                        scenario_id=scenario.id,
                        scenario_description=scenario.description,
                        weekly_wage=wage,
                        column=column,
                        unit=comparison_unit(column),
                        treasury=treasury_value,
                        rulespec=rulespec_value,
                        signed_delta=signed_delta,
                        absolute_delta=absolute_delta,
                        relative_delta=relative_delta,
                        classification=classification,
                        reason_code=reason_code,
                    )
                )
    return rows


@dataclass(frozen=True)
class VintageParameter:
    label: str
    treasury_name: str
    treasury_value: Decimal
    rulespec_name: str
    unit: str
    statute: str
    corpus_citation_path: str


VINTAGE_PARAMETERS = (
    VintageParameter(
        label="Sole Parent Support / lone-parent JSS",
        treasury_name="Benefits_SPS_Rate",
        treasury_value=Decimal("517.39"),
        rulespec_name="sole_parent_support_weekly_rate",
        unit="NZD/week",
        statute=(
            "Social Security Act 2018 Sch 4 pt 2 cl 1 (SPS) and pt 1 "
            "cl 1(e) (lone-parent JSS); Social Security (Rates of Benefits "
            "and Allowances) Order 2026 cl 5"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2018/0032/schedule/4/part/2/clause/lms118467; "
            "nz/statute/act/public/2018/0032/schedule/4/part/1/clause/lms118447"
        ),
    ),
    VintageParameter(
        label="JSS partnered with children, each adult",
        treasury_name="Benefits_JSS_Rate_CoupleParent",
        treasury_value=Decimal("332.05"),
        rulespec_name=(
            "jobseeker_support_partnered_both_main_benefits_with_children_weekly_rate"
        ),
        unit="NZD/week",
        statute=(
            "Social Security Act 2018 Sch 4 pt 1 cl 1(g)(ii); Social "
            "Security (Rates of Benefits and Allowances) Order 2026 cl 5"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2018/0032/schedule/4/part/1/clause/lms118447"
        ),
    ),
    VintageParameter(
        label="JSS single, no children",
        treasury_name="Benefits_JSS_Rate_Single",
        treasury_value=Decimal("369.60"),
        rulespec_name="jobseeker_support_single_no_children_standard_weekly_rate",
        unit="NZD/week",
        statute=(
            "Social Security Act 2018 Sch 4 pt 1 cl 1(d); Social Security "
            "(Rates of Benefits and Allowances) Order 2026 cl 5"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2018/0032/schedule/4/part/1/clause/lms118447"
        ),
    ),
    VintageParameter(
        label="Family Tax Credit, eldest child",
        treasury_name="FamilyAssistance_FTC_Rates_FirstChild",
        treasury_value=Decimal("7524"),
        rulespec_name="family_tax_credit_eldest_child_annual_amount",
        unit="NZD/year",
        statute=(
            "Income Tax Act 2007 s MD 3(4)(a); Income Tax (Tax Credit) "
            "Order 2025 cl 4"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2007/0097/section/md-3"
        ),
    ),
    VintageParameter(
        label="Family Tax Credit, subsequent child",
        treasury_name="FamilyAssistance_FTC_Rates_SubsequentChild",
        treasury_value=Decimal("6130"),
        rulespec_name="family_tax_credit_subsequent_child_annual_amount",
        unit="NZD/year",
        statute=(
            "Income Tax Act 2007 s MD 3(4)(b); Income Tax (Tax Credit) "
            "Order 2025 cl 4"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2007/0097/section/md-3"
        ),
    ),
    VintageParameter(
        label="In-Work Tax Credit, base",
        treasury_name="FamilyAssistance_IWTC_Rates_UpTo3Children",
        treasury_value=Decimal("5070"),
        rulespec_name="in_work_tax_credit_base_annual_amount",
        unit="NZD/year",
        statute=(
            "Income Tax Act 2007 s MD 10(3)(a); Taxation (Annual Rates for "
            "2025-26, Compliance Simplification, and Remedial Measures) "
            "Act 2026 ss 2, 105"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2026/0008/section/105"
        ),
    ),
    VintageParameter(
        label="Minimum Family Tax Credit prescribed amount",
        treasury_name="FamilyAssistance_MFTC_Rates_MinimumIncome",
        treasury_value=Decimal("36504"),
        rulespec_name="minimum_family_tax_credit_prescribed_amount",
        unit="NZD/year",
        statute=(
            "Income Tax Act 2007 s ME 1(3)(a); Income Tax (Tax Credit) "
            "Order 2025 cl 5"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2007/0097/section/me-1"
        ),
    ),
    VintageParameter(
        label="Best Start prescribed amount, each eligible child",
        treasury_name="FamilyAssistance_BestStart_Rates_Age0",
        treasury_value=Decimal("3838"),
        rulespec_name="best_start_tax_credit_prescribed_amount",
        unit="NZD/year",
        statute=(
            "Income Tax Act 2007 s MG 2(2)(a); Income Tax (Tax Credit) "
            "Order 2025 cl 6"
        ),
        corpus_citation_path=(
            "nz/statute/act/public/2007/0097/section/mg-2"
        ),
    ),
)


def read_top_level_yaml_decimal(path: Path, name: str) -> Decimal:
    prefix = f"{name}:"
    with path.open(encoding="utf-8") as source:
        for raw_line in source:
            if raw_line.startswith(prefix):
                value = raw_line[len(prefix) :].strip()
                try:
                    return Decimal(value)
                except Exception as exc:
                    raise HarnessError(
                        f"Treasury YAML value {name!r} is not a scalar decimal"
                    ) from exc
    raise HarnessError(f"Treasury YAML has no top-level parameter {name!r}")


def build_vintage_parameter_rows(
    compiled: Mapping[str, Any],
    provenance: Provenance,
) -> list[dict[str, Any]]:
    parameter_path = provenance.treasury_root / EXPECTED_PARAMETER_FILE
    if provenance.treasury_present:
        treasury_jss_sole_parent = read_top_level_yaml_decimal(
            parameter_path, "Benefits_JSS_Rate_SoleParent"
        )
        if treasury_jss_sole_parent != Decimal("517.39"):
            raise HarnessError(
                "pinned Treasury Benefits_JSS_Rate_SoleParent changed: "
                f"{decimal_text(treasury_jss_sole_parent)}"
            )
    rows: list[dict[str, Any]] = []
    for spec in VINTAGE_PARAMETERS:
        if provenance.treasury_present:
            observed_treasury = read_top_level_yaml_decimal(
                parameter_path, spec.treasury_name
            )
            if observed_treasury != spec.treasury_value:
                raise HarnessError(
                    f"pinned Treasury {spec.treasury_name} changed: expected "
                    f"{decimal_text(spec.treasury_value)}, found "
                    f"{decimal_text(observed_treasury)}"
                )
        rulespec_values = selected_parameter_values(
            compiled, spec.rulespec_name
        )
        if set(rulespec_values) != {0}:
            raise HarnessError(
                f"expected scalar RuleSpec parameter {spec.rulespec_name}"
            )
        rulespec_value = rulespec_values[0]
        rows.append(
            {
                "label": spec.label,
                "treasury_parameter": spec.treasury_name,
                "treasury_befu25": spec.treasury_value,
                "rulespec_parameter": spec.rulespec_name,
                "rulespec_enacted": rulespec_value,
                "signed_delta": rulespec_value - spec.treasury_value,
                "unit": spec.unit,
                "statute": spec.statute,
                "corpus_citation_path": spec.corpus_citation_path,
            }
        )
    return rows


def summary_statistics(rows: Sequence[ComparisonRow]) -> dict[str, Any]:
    class_counts = Counter(row.classification for row in rows)
    reason_counts = Counter(row.reason_code for row in rows)
    net_income_rows = [row for row in rows if row.column == "Net_Income"]
    emtr_rows = [row for row in rows if row.column == "EMTR"]
    dollar_rows = [
        row
        for row in rows
        if row.column != "EMTR"
    ]
    if not net_income_rows or len(net_income_rows) != len(emtr_rows):
        raise HarnessError(
            "comparison summary has inconsistent Net Income and EMTR rows"
        )
    dollar_outside_cent = [
        row for row in dollar_rows if row.absolute_delta >= Decimal("0.005")
    ]
    dollar_outside_cent_classes = Counter(
        row.classification for row in dollar_outside_cent
    )
    maximum_net = max(
        net_income_rows,
        key=lambda row: (
            row.absolute_delta,
            row.scenario_id,
            row.weekly_wage,
        ),
    )
    maximum_emtr = max(
        emtr_rows,
        key=lambda row: (
            row.absolute_delta,
            row.scenario_id,
            row.weekly_wage,
        ),
    )
    return {
        "primary_cells": len(rows),
        "amount_control_cells": len(dollar_rows),
        "amount_control_cells_agree_to_cent": (
            len(dollar_rows) - len(dollar_outside_cent)
        ),
        "amount_control_cells_outside_cent": len(dollar_outside_cent),
        "amount_control_cells_outside_cent_by_class": {
            key: dollar_outside_cent_classes.get(key, 0)
            for key in ("match", "a", "b", "c", "d")
        },
        # Backward-compatible machine keys retained for consumers of the
        # original audit. They include the hours/control identity column.
        "dollar_cells": len(dollar_rows),
        "dollar_cells_agree_to_cent": (
            len(dollar_rows) - len(dollar_outside_cent)
        ),
        "dollar_cells_outside_cent": len(dollar_outside_cent),
        "dollar_cells_outside_cent_by_class": {
            key: dollar_outside_cent_classes.get(key, 0)
            for key in ("match", "a", "b", "c", "d")
        },
        "exact_numeric_matches": sum(
            row.absolute_delta == 0 for row in rows
        ),
        "nonzero_deltas_within_snapshot_precision": sum(
            D0 < row.absolute_delta <= ORACLE_SIX_DECIMAL_TOLERANCE
            for row in rows
        ),
        "matches_at_six_decimal_oracle_precision": class_counts.get("match", 0),
        "differences_outside_six_decimal_envelope": (
            len(rows) - class_counts.get("match", 0)
        ),
        "classification_counts": {
            key: class_counts.get(key, 0)
            for key in ("match", "a", "b", "c", "d")
        },
        "reason_counts": dict(sorted(reason_counts.items())),
        "weekly_net_income_points_within_1_nzd": sum(
            row.absolute_delta <= D1 for row in net_income_rows
        ),
        "weekly_net_income_points": len(net_income_rows),
        "maximum_weekly_net_income_absolute_delta": maximum_net.absolute_delta,
        "maximum_weekly_net_income_delta_location": {
            "scenario_id": maximum_net.scenario_id,
            "weekly_wage": maximum_net.weekly_wage,
        },
        "maximum_emtr_absolute_delta": maximum_emtr.absolute_delta,
        "maximum_emtr_absolute_delta_percentage_points": (
            maximum_emtr.absolute_delta * Decimal(100)
        ),
        "maximum_emtr_delta_location": {
            "scenario_id": maximum_emtr.scenario_id,
            "weekly_wage": maximum_emtr.weekly_wage,
        },
        "emtr_points": len(emtr_rows),
    }


def comparison_row_json(row: ComparisonRow) -> dict[str, Any]:
    detail = CLASSIFICATION_DETAILS[row.reason_code]
    return {
        "scenario_id": row.scenario_id,
        "scenario_description": row.scenario_description,
        "weekly_wage": row.weekly_wage,
        "column": row.column,
        "unit": row.unit,
        "treasury": row.treasury,
        "rulespec": row.rulespec,
        "signed_delta_rulespec_minus_treasury": row.signed_delta,
        "absolute_delta": row.absolute_delta,
        "relative_absolute_delta": row.relative_delta,
        "classification": row.classification,
        "reason_code": row.reason_code,
        "reason_title": detail[1],
        "reason": detail[2],
    }


def json_ready(value: Any) -> Any:
    if isinstance(value, Decimal):
        return decimal_text(value)
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, Mapping):
        return {
            str(key): json_ready(item)
            for key, item in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [json_ready(item) for item in value]
    return value


def json_text(value: Any) -> str:
    return json.dumps(
        json_ready(value),
        indent=2,
        sort_keys=True,
        ensure_ascii=False,
    ) + "\n"


def comparison_csv_text(rows: Sequence[ComparisonRow]) -> str:
    target = io.StringIO(newline="")
    fieldnames = (
        "scenario_id",
        "scenario_description",
        "weekly_wage",
        "column",
        "unit",
        "treasury",
        "rulespec",
        "signed_delta_rulespec_minus_treasury",
        "absolute_delta",
        "relative_absolute_delta",
        "classification",
        "reason_code",
        "reason_title",
    )
    writer = csv.DictWriter(target, fieldnames=fieldnames, lineterminator="\n")
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                "scenario_id": row.scenario_id,
                "scenario_description": row.scenario_description,
                "weekly_wage": row.weekly_wage,
                "column": row.column,
                "unit": row.unit,
                "treasury": decimal_text(row.treasury),
                "rulespec": decimal_text(row.rulespec),
                "signed_delta_rulespec_minus_treasury": decimal_text(
                    row.signed_delta
                ),
                "absolute_delta": decimal_text(row.absolute_delta),
                "relative_absolute_delta": (
                    ""
                    if row.relative_delta is None
                    else decimal_text(row.relative_delta)
                ),
                "classification": row.classification,
                "reason_code": row.reason_code,
                "reason_title": CLASSIFICATION_DETAILS[row.reason_code][1],
            }
        )
    return target.getvalue()


def build_secondary_rate_rows(
    *,
    oracle: Mapping[str, Any],
    scenarios: Sequence[Scenario],
    sweeps: Mapping[str, Mapping[int, Mapping[str, Decimal | str | None]]],
) -> list[dict[str, Any]]:
    oracle_by_id = {str(item["id"]): item for item in oracle["scenarios"]}
    result: list[dict[str, Any]] = []
    for scenario in scenarios:
        raw = oracle_by_id[scenario.id]
        for index, wage in enumerate(scenario.sampled_wages):
            for column in ("RR", "PTR"):
                treasury_raw = raw["sampled_outputs"][index][column]
                rulespec_raw = sweeps[scenario.id][wage][column]
                treasury_value = (
                    None if treasury_raw is None else dec(treasury_raw)
                )
                if rulespec_raw is not None and not isinstance(
                    rulespec_raw, Decimal
                ):
                    raise HarnessError(
                        f"expected Decimal or null for {scenario.id} {column}"
                    )
                if treasury_value is None or rulespec_raw is None:
                    signed_delta = None
                    absolute_delta = None
                    relative_delta = None
                else:
                    signed_delta = rulespec_raw - treasury_value
                    absolute_delta = abs(signed_delta)
                    relative_delta = (
                        absolute_delta / abs(treasury_value)
                        if treasury_value != 0
                        else (D0 if absolute_delta == 0 else None)
                    )
                result.append(
                    {
                        "scenario_id": scenario.id,
                        "weekly_wage": wage,
                        "column": column,
                        "treasury": treasury_value,
                        "rulespec": rulespec_raw,
                        "signed_delta_rulespec_minus_treasury": signed_delta,
                        "absolute_delta": absolute_delta,
                        "relative_absolute_delta": relative_delta,
                    }
                )
    return result


def generic_csv_text(
    rows: Sequence[Mapping[str, Any]],
    fieldnames: Sequence[str],
) -> str:
    target = io.StringIO(newline="")
    writer = csv.DictWriter(
        target,
        fieldnames=fieldnames,
        lineterminator="\n",
        extrasaction="ignore",
    )
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                field: (
                    ""
                    if row.get(field) is None
                    else decimal_text(row[field])
                    if isinstance(row[field], Decimal)
                    else row[field]
                )
                for field in fieldnames
            }
        )
    return target.getvalue()


def build_as_diagnostic_rows(
    *,
    oracle: Mapping[str, Any],
    scenarios: Sequence[Scenario],
    sweeps: Mapping[str, Mapping[int, Mapping[str, Decimal | str | None]]],
) -> list[dict[str, Any]]:
    oracle_by_id = {str(item["id"]): item for item in oracle["scenarios"]}
    result: list[dict[str, Any]] = []
    for scenario in scenarios:
        raw = oracle_by_id[scenario.id]
        for index, wage in enumerate(scenario.sampled_wages):
            state = sweeps[scenario.id][wage]
            before_rounding = scalar_decimal(state, "AS_Amount")
            treasury_host_aligned = scalar_decimal(
                state, "AS_Amount_treasury_host_aligned"
            )
            statutory = scalar_decimal(
                state, "AS_Amount_statutory_rounded"
            )
            treasury_value = dec(
                raw["sampled_outputs"][index]["AS_Amount"]
            )
            result.append(
                {
                    "scenario_id": scenario.id,
                    "weekly_wage": wage,
                    "treasury_raw_unrounded": treasury_value,
                    "rulespec_statutory_inputs_before_rounding": before_rounding,
                    "rulespec_treasury_host_aligned_before_rounding": (
                        treasury_host_aligned
                    ),
                    "rulespec_statutory_rounded": statutory,
                    "total_signed_delta": (
                        before_rounding - treasury_value
                    ),
                    "forecast_vintage_component_under_treasury_host_convention": (
                        treasury_host_aligned - treasury_value
                    ),
                    "statutory_host_convention_component": (
                        before_rounding - treasury_host_aligned
                    ),
                    "legal_rounding_uplift": statutory - before_rounding,
                    "rulespec_statutory_base_rate": scalar_decimal(
                        state, "AS_statutory_base_rate"
                    ),
                    "rulespec_treasury_host_base_rate": scalar_decimal(
                        state, "AS_treasury_host_base_rate"
                    ),
                    "rulespec_statutory_cutout": scalar_decimal(
                        state, "AS_statutory_cutout"
                    ),
                    "rulespec_treasury_host_cutout": scalar_decimal(
                        state, "AS_treasury_host_cutout"
                    ),
                }
            )
    return result


def build_diagnostic_rows(
    *,
    scenarios: Sequence[Scenario],
    sweeps: Mapping[str, Mapping[int, Mapping[str, Decimal | str | None]]],
) -> list[dict[str, Any]]:
    fields = (
        "AS_Amount_statutory_rounded",
        "AS_Amount_treasury_host_aligned",
        "AS_statutory_base_rate",
        "AS_treasury_host_base_rate",
        "AS_statutory_cutout",
        "AS_treasury_host_cutout",
        "AS_qualifying_accommodation_cost",
        "gross_benefit_taxable_weekly",
        "wage2_tax",
        "wage2_ACC_levy",
        "net_wage2",
        "family_scheme_income",
        "iwtc_entitlement",
        "BestStart_Total_naive_per_child_abatement",
        "BestStart_Total_continuous_abatement",
        "WFF_abated_continuous_abatement",
        "IETC_abated_continuous_abatement",
        "RuleSpec_EMTR_AS_Amount_treasury_host_aligned",
        "RuleSpec_EMTR_BestStart_Total_naive_per_child_abatement",
        "EMTR_ACC_rounding_component",
        "EMTR_WFF_complete_dollar_component",
        "EMTR_BestStart_complete_dollar_component",
        "EMTR_IETC_complete_dollar_component",
        "EMTR_WFF_discrete_bound",
        "EMTR_BestStart_discrete_bound",
        "EMTR_IETC_discrete_bound",
        *(
            f"RuleSpec_EMTR_{component}"
            for component in EMTR_COMPONENT_BASE_COLUMNS
        ),
        *(
            f"Treasury_{component}"
            for component in TREASURY_EMTR_COMPONENT_COLUMNS
        ),
    )
    return [
        {
            "scenario_id": scenario.id,
            "weekly_wage": wage,
            **{
                field: sweeps[scenario.id][wage][field]
                for field in fields
            },
        }
        for scenario in scenarios
        for wage in scenario.sampled_wages
    ]


def validate_emtr_residual_patterns(
    rows: Sequence[ComparisonRow],
    sweeps: Mapping[str, Mapping[int, Mapping[str, Decimal | str | None]]],
) -> list[dict[str, Any]]:
    component_tolerance = Decimal("0.000004")
    diagnostics: list[dict[str, Any]] = []
    for row in rows:
        if row.column != "EMTR":
            continue
        state = sweeps[row.scenario_id][row.weekly_wage]
        component_deltas = {
            component: (
                scalar_decimal(state, f"RuleSpec_EMTR_{component}")
                - scalar_decimal(state, f"Treasury_EMTR_{component}")
            )
            for component in EMTR_COMPONENT_BASE_COLUMNS
        }
        component_recomposition_remainder = (
            row.signed_delta - sum(component_deltas.values(), D0)
        )
        if abs(component_recomposition_remainder) > component_tolerance:
            raise HarnessError(
                "Treasury and RuleSpec EMTR components do not recompose at "
                f"{row.scenario_id} wage {row.weekly_wage}: "
                f"{decimal_text(component_recomposition_remainder)}"
            )
        if row.classification == "d":
            raise HarnessError(
                "unexplained EMTR difference remains at "
                f"{row.scenario_id} wage {row.weekly_wage}"
            )

        as_convention_component = (
            scalar_decimal(state, "RuleSpec_EMTR_AS_Amount")
            - scalar_decimal(
                state,
                "RuleSpec_EMTR_AS_Amount_treasury_host_aligned",
            )
        )
        convention_components = {
            "ACC annual cents": scalar_decimal(
                state, "EMTR_ACC_rounding_component"
            ),
            "WFF complete dollars": scalar_decimal(
                state, "EMTR_WFF_complete_dollar_component"
            ),
            "Best Start complete dollars": scalar_decimal(
                state, "EMTR_BestStart_complete_dollar_component"
            ),
            "IETC complete dollars": scalar_decimal(
                state, "EMTR_IETC_complete_dollar_component"
            ),
            "AS statutory host": as_convention_component,
        }
        convention_recomposition_remainder = (
            row.signed_delta - sum(convention_components.values(), D0)
        )
        if (
            row.classification == "c"
            and abs(convention_recomposition_remainder)
            > component_tolerance
        ):
            raise HarnessError(
                "class-(c) EMTR difference is not accounted for by source-"
                f"defined conventions at {row.scenario_id} wage "
                f"{row.weekly_wage}: "
                f"{decimal_text(convention_recomposition_remainder)}"
            )

        material_components = tuple(
            component
            for component, value in component_deltas.items()
            if abs(value) > component_tolerance
        )
        material_conventions = tuple(
            component
            for component, value in convention_components.items()
            if abs(value) > component_tolerance
        )
        diagnostics.append(
            {
                "scenario_id": row.scenario_id,
                "weekly_wage": row.weekly_wage,
                "classification": row.classification,
                "signed_delta": row.signed_delta,
                "component_deltas": component_deltas,
                "material_components": material_components,
                "convention_components": convention_components,
                "material_conventions": material_conventions,
                "component_recomposition_remainder": (
                    component_recomposition_remainder
                ),
                "convention_recomposition_remainder": (
                    convention_recomposition_remainder
                ),
            }
        )
    expected_count = sum(row.column == "EMTR" for row in rows)
    if len(diagnostics) != expected_count:
        raise HarnessError(
            f"expected {expected_count} EMTR decomposition rows, "
            f"found {len(diagnostics)}"
        )

    grouped: dict[
        tuple[str, tuple[str, ...], tuple[str, ...]],
        list[dict[str, Any]],
    ] = {}
    for item in diagnostics:
        key = (
            item["classification"],
            item["material_components"],
            item["material_conventions"],
        )
        grouped.setdefault(key, []).append(item)
    patterns: list[dict[str, Any]] = []
    for key in sorted(grouped):
        classification, material_components, material_conventions = key
        members = grouped[key]
        patterns.append(
            {
                "classification": classification,
                "material_components": material_components,
                "material_conventions": material_conventions,
                "count": len(members),
                "maximum_absolute_signed_delta": max(
                    abs(item["signed_delta"]) for item in members
                ),
                "maximum_absolute_component_recomposition_remainder": max(
                    abs(item["component_recomposition_remainder"])
                    for item in members
                ),
                "maximum_absolute_convention_recomposition_remainder": max(
                    abs(item["convention_recomposition_remainder"])
                    for item in members
                ),
                "explanation": (
                    "named-instrument vintage boundary"
                    if classification == "b"
                    else "source-defined statutory convention"
                    if classification == "c"
                    else "match at total six-decimal precision"
                ),
                "locations": [
                    {
                        "scenario_id": item["scenario_id"],
                        "weekly_wage": item["weekly_wage"],
                    }
                    for item in members
                ],
            }
        )
    return patterns


def report_number(value: Decimal, places: int = 6) -> str:
    return format(value, f".{places}f")


def report_relative(value: Decimal | None) -> str:
    if value is None:
        return "n/a"
    return f"{report_number(value * Decimal(100), 6)}%"


def markdown_cell(value: Any) -> str:
    return str(value).replace("|", "\\|").replace("\n", " ")


def headline_text(statistics: Mapping[str, Any]) -> str:
    cent_matches = statistics["amount_control_cells_agree_to_cent"]
    amount_control_cells = statistics["amount_control_cells"]
    outside = statistics["amount_control_cells_outside_cent_by_class"]
    return (
        f"RuleSpec agrees with pinned Treasury to the cent in {cent_matches} "
        f"of {amount_control_cells} amount/control cells. Every cent-level "
        f"exception is classified: "
        f"{outside['b']} named BEFU25-versus-enacted instrument-vintage cells "
        f"and {outside['c']} documented convention cells; "
        f"{outside['a']} remaining encoding bugs and {outside['d']} unexplained."
    )


def scenario_summary_rows(
    rows: Sequence[ComparisonRow],
    scenarios: Sequence[Scenario],
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for scenario in scenarios:
        scenario_rows = [
            row for row in rows if row.scenario_id == scenario.id
        ]
        net_rows = [
            row for row in scenario_rows if row.column == "Net_Income"
        ]
        emtr_rows = [
            row for row in scenario_rows if row.column == "EMTR"
        ]
        amount_control_rows = [
            row for row in scenario_rows if row.column != "EMTR"
        ]
        classes = Counter(row.classification for row in scenario_rows)
        result.append(
            {
                "scenario_id": scenario.id,
                "points": len(net_rows),
                "amount_control_cells": len(amount_control_rows),
                "amount_control_cells_agree_to_cent": sum(
                    row.absolute_delta < Decimal("0.005")
                    for row in amount_control_rows
                ),
                "max_net_income_delta": max(
                    row.absolute_delta for row in net_rows
                ),
                "emtr_b": sum(
                    row.classification == "b" for row in emtr_rows
                ),
                "emtr_c": sum(
                    row.classification == "c" for row in emtr_rows
                ),
                "b_cells": classes.get("b", 0),
                "c_cells": classes.get("c", 0),
                "a_or_d_cells": (
                    classes.get("a", 0) + classes.get("d", 0)
                ),
            }
        )
    return result


def render_report(
    *,
    oracle: Mapping[str, Any],
    provenance: Provenance,
    compiled_artifact_sha256: str,
    compiled_counts: Mapping[str, int],
    engine_call_count: int,
    scenarios: Sequence[Scenario],
    rows: Sequence[ComparisonRow],
    statistics: Mapping[str, Any],
    baseline_statistics: Mapping[str, Any],
    vintage_rows: Sequence[Mapping[str, Any]],
    emtr_patterns: Sequence[Mapping[str, Any]],
    as_rows: Sequence[Mapping[str, Any]],
    coverage_evidence: Mapping[str, Any],
) -> str:
    """Render the living expanded-grid audit report."""

    headline = headline_text(statistics)
    observed_reasons = Counter(row.reason_code for row in rows)
    material_as_count = sum(
        abs(item["total_signed_delta"]) > ORACLE_SIX_DECIMAL_TOLERANCE
        for item in as_rows
    )
    provenance_by_id = oracle["scenario_provenance"]
    lines: list[str] = [
        "# Treasury IncomeExplorer EMTR reproduction audit",
        "",
        "## Headline",
        "",
        f"> {headline}",
        "",
        "This is a dollar-exactness audit of amount/control outputs. EMTR is "
        "retained as a diagnostic and classified component-by-component; it is "
        "not used to soften the dollar result into a percentage-point claim.",
        "",
        "The expanded matrix has "
        f"{statistics['primary_cells']} primary cells: "
        f"{len(scenarios)} scenarios at "
        f"{coverage_evidence['sampled_point_count']} scenario/wage points, "
        "with 19 amount/control columns and one EMTR column per point.",
        "",
        "## What we can honestly say",
        "",
        "> RuleSpec's enacted 2026/27 amount rules are compared pointwise with "
        "a freshly regenerated, pinned TY27 BEFU25 output from Treasury's raw "
        "`R/emtr.R#emtr` function. The comparison is conditional on explicit "
        "host-side eligibility assumptions; it is not an end-to-end "
        "IncomeExplorer UI reproduction or a legal entitlement determination.",
        "",
        f"The numerical result is: {headline}",
        "",
        "The original four scenarios remain a strict continuity slice: "
        f"{baseline_statistics['amount_control_cells_agree_to_cent']} of "
        f"{baseline_statistics['amount_control_cells']} amount/control cells "
        "agree to the cent, and all "
        f"{baseline_statistics['amount_control_cells_outside_cent']} cent-level "
        "differences remain class (b) forecast-vintage effects. The first four "
        "serialized scenario objects in the expanded oracle are byte-identical "
        "to the pinned snapshot objects.",
        "",
        "## Oracle regeneration gate",
        "",
        "Before either comparison pass, the harness runs the R generator in "
        "baseline mode against the pinned Treasury checkout. It stops unless "
        "the result is byte-identical to RuleSpec's pinned snapshot. It then "
        "runs the same generator in expanded mode; no Treasury-side value is "
        "hand-computed, extrapolated, or copied from RuleSpec.",
        "",
        "| Check | Verified result |",
        "|---|---|",
        f"| Baseline snapshot | 25,888 bytes; SHA-256 "
        f"`{provenance.baseline_oracle_sha256}`; byte-identical |",
        f"| Expanded snapshot | generated by R; SHA-256 "
        f"`{provenance.oracle_sha256}` |",
        f"| Treasury commit | `{EXPECTED_ORACLE_COMMIT}` |",
        "| Treasury tracked tree | clean; enforced before R execution |",
        f"| Parameter file | `{EXPECTED_PARAMETER_FILE}`; SHA-256 "
        f"`{EXPECTED_PARAMETER_SHA256}` |",
        f"| R runtime | `{markdown_cell(provenance.rscript_version)}` |",
        f"| Generator | `{provenance.oracle_generator_path.name}`; SHA-256 "
        f"`{provenance.oracle_generator_sha256}` |",
        "| Original four scenario objects | identical serialized bytes in "
        "baseline and expanded JSON |",
        "",
        "## Result summary",
        "",
        "| Measure | Result |",
        "|---|---:|",
        (
            "| Amount/control cells agreeing to the cent | "
            f"{statistics['amount_control_cells_agree_to_cent']} / "
            f"{statistics['amount_control_cells']} |"
        ),
        (
            "| Cent-level exceptions: forecast vintage (b) | "
            f"{statistics['amount_control_cells_outside_cent_by_class']['b']} |"
        ),
        (
            "| Cent-level exceptions: convention (c) | "
            f"{statistics['amount_control_cells_outside_cent_by_class']['c']} |"
        ),
        (
            "| Cent-level exceptions: remaining bug (a) | "
            f"{statistics['amount_control_cells_outside_cent_by_class']['a']} |"
        ),
        (
            "| Cent-level exceptions: unexplained (d) | "
            f"{statistics['amount_control_cells_outside_cent_by_class']['d']} |"
        ),
        (
            "| All primary cells matching at six-decimal oracle precision | "
            f"{statistics['matches_at_six_decimal_oracle_precision']} / "
            f"{statistics['primary_cells']} |"
        ),
        (
            "| All primary class-(b) cells | "
            f"{statistics['classification_counts']['b']} |"
        ),
        (
            "| All primary class-(c) cells | "
            f"{statistics['classification_counts']['c']} |"
        ),
        (
            "| All primary class-(a) / class-(d) cells | "
            f"{statistics['classification_counts']['a']} / "
            f"{statistics['classification_counts']['d']} |"
        ),
        "",
        "Scenario-level summary:",
        "",
        "| Scenario | Points | Amount/control to cent | Max weekly NI |Δ| | "
        "(b) cells | (c) cells | EMTR (b)/(c) | (a)+(d) |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for item in scenario_summary_rows(rows, scenarios):
        lines.append(
            f"| `{item['scenario_id']}` | {item['points']} | "
            f"{item['amount_control_cells_agree_to_cent']} / "
            f"{item['amount_control_cells']} | "
            f"${report_number(item['max_net_income_delta'], 6)} | "
            f"{item['b_cells']} | {item['c_cells']} | "
            f"{item['emtr_b']} / {item['emtr_c']} | "
            f"{item['a_or_d_cells']} |"
        )

    lines.extend(
        [
            "",
            "An amount/control cell agrees to the cent when its absolute delta "
            "is less than half a cent. A delta of at most `0.0000005` is a "
            "snapshot-precision match because Treasury outputs are rounded to "
            "six decimals. Exact signed deltas are retained in "
            "[comparison.csv](comparison.csv) and "
            "[comparison.json](comparison.json).",
            "",
            "## Expanded coverage",
            "",
            "| New scenario | What is exercised | Extra wages | Verified outcome |",
            "|---|---|---|---|",
            (
                "| `lone_parent_two_teens_jss` | Youngest child 14+: Treasury "
                "`R/emtr.R:310-319` JSS parent rate with Income Test 1 | none | "
                "Positive RuleSpec JSS at wage $0; mapping follows "
                "[SSA 2018 s 33](https://www.legislation.govt.nz/act/public/"
                "2018/0032/latest/DLM6783171.html) and Schedule 4 pt 1 cl 1(e). |"
            ),
            (
                "| `couple_two_best_start_children_binding` | Two eligible "
                "children, full-time partner, aggregate Best Start abatement | "
                "775/776, 1125/1126, 1476/1477 | Abatement binds inside the "
                "grid; aggregate-once and old naive composition diverge. |"
            ),
            (
                "| `couple_two_children_dual_full_time` | Partner wage tax/ACC "
                "and joint WFF abatement | 121/122 | At wage $740 both adults "
                "are at 40 hours; partner weekly tax and ACC are "
                f"${report_number(coverage_evidence['dual_full_time_partner_at_wage_740']['partner_weekly_tax'], 6)} "
                "and "
                f"${report_number(coverage_evidence['dual_full_time_partner_at_wage_740']['partner_weekly_acc'], 2)}. |"
            ),
            (
                "| `single_childless_ietc_focused` | Positive IETC and its "
                "benefit-release, abatement, and extinction boundaries | "
                "688/689, 692/693, 1265/1266, 1342/1343 | At wage $740, "
                f"Treasury/RuleSpec pay ${report_number(coverage_evidence['focused_ietc_at_wage_740']['treasury'], 6)} / "
                f"${report_number(coverage_evidence['focused_ietc_at_wage_740']['rulespec'], 6)} weekly. |"
            ),
            (
                "| `lone_parent_two_children_area4_high_rent_cap` | Area 4 "
                "renter and AS maximum | none | The $120 weekly maximum binds "
                "on both sides at wages "
                f"{', '.join(str(value) for value in coverage_evidence['area4_cap']['binding_wages_on_both_sides'])}. |"
            ),
            (
                "| `couple_childless_boarder_proxy` | RuleSpec boarder cost "
                "composition and downstream raw-Treasury AS arithmetic | none | "
                "$400 board becomes $248 qualifying cost in RuleSpec; Treasury "
                "is actually run with a rent-like $248 because raw `emtr()` has "
                "no boarder input. |"
            ),
            (
                "| `large_family_four_children_age_bands` | Four children "
                "aged 3, 6, 13, 16; FTC/IWTC subsequent-child scaling and "
                "dependent Winter Energy rate | none | FTC and Winter Energy "
                "are both positive at wage $0; Treasury/RuleSpec Winter Energy "
                f"is ${report_number(coverage_evidence['large_family_at_wage_0']['treasury_winter_energy'], 6)} / "
                f"${report_number(coverage_evidence['large_family_at_wage_0']['rulespec_winter_energy'], 6)} weekly. |"
            ),
            "",
            "The prompt's statement that the old grid had IETC identically zero "
            "does not match the pinned data: the original childless profile "
            "already pays IETC at wages "
            f"{', '.join(str(value) for value in coverage_evidence['original_profile_positive_ietc_wages'])}. "
            "The focused profile nevertheless closes the real gap by sampling "
            "Treasury's forecast benefit/IETC transition (688/689), RuleSpec's "
            "enacted transition (692/693), and the LC 13 abatement/extinction "
            "kinks.",
            "",
            "### Boarder limitation",
            "",
            "[Social Security Act 2018 s 65AAA]"
            "(https://www.legislation.govt.nz/act/public/2025/27/en/latest/"
            "sections/LMS1442953/) makes a boarder's accommodation cost 62% of "
            "board and lodgings for 2026/27. RuleSpec therefore receives $400 "
            "and independently emits $248. Treasury's raw signature exposes "
            "only accommodation cost, rent/mortgage, and area "
            "(`R/emtr.R:164-183`); its UI likewise has no boarder choice. Feeding "
            "$248 to Treasury tests downstream arithmetic, not Treasury's "
            "missing 62% transformation. This remains a named coverage gap.",
            "",
            "## Class-(a) defect found and fixed",
            "",
            "The binding two-child scenario exposed a host-composition defect. "
            "RuleSpec's Best Start module is child-entity based. The old harness "
            "summed each child's *post-abatement* result, thereby subtracting the "
            "same family abatement once per child. The fixed harness sums the "
            "per-child gross credits and subtracts one family abatement.",
            "",
            "[Income Tax Act 2007 MG 1]"
            "(https://www.legislation.govt.nz/act/public/2007/0097/latest/"
            "LMS63782.html), [MG 2]"
            "(https://www.legislation.govt.nz/act/public/2007/0097/latest/"
            "LMS63785.html), and [MG 3]"
            "(https://www.legislation.govt.nz/act/public/2007/0097/latest/"
            "LMS63788.html) provide the per-child base and the person's/couple's "
            "single family-income abatement. The decisive multi-child "
            "administrative cross-check is [IRD's 2026/27 IR271 table]"
            "(https://www.ird.govt.nz/-/media/project/ir/home/documents/"
            "forms-and-guides/ir200---ir299/ir271/ir271-2027.pdf): moving above "
            "the threshold reduces the one-, two-, and three-child family totals "
            "by the same amount, not once per child.",
            "",
            "| Primary wage | Treasury | Before: naive per-child abatement | "
            "After: aggregate once |",
            "|---:|---:|---:|---:|",
        ]
    )
    for item in coverage_evidence["best_start_aggregate_fix"]:
        lines.append(
            f"| {item['weekly_wage']} | "
            f"{report_number(item['treasury'], 6)} | "
            f"{report_number(item['rulespec_before_naive_per_child_abatement'], 6)} | "
            f"{report_number(item['rulespec_after_aggregate_once'], 6)} |"
        )
    lines.extend(
        [
            "",
            "This was a harness aggregation bug, not a defect in the underlying "
            "RuleSpec child-level amount formula. It is fixed in `run.py`; the "
            "isolated RuleSpec worktree remains unchanged. The final matrix has "
            f"{statistics['classification_counts']['a']} class-(a) cells.",
            "",
            "The prior four-family phase also fixed two host mappings before its "
            "final run: the source-specific lone-parent AS cutout and the "
            "statutory reg 17–18 AS delegated inputs. Those corrections remain "
            "in place.",
            "",
            "## Discrepancy classification",
            "",
            "| Class | Cells | Conclusion |",
            "|---|---:|---|",
            (
                f"| match | {statistics['classification_counts']['match']} | "
                "Exact or inside the six-decimal oracle envelope. |"
            ),
            (
                f"| (a) our bug | {statistics['classification_counts']['a']} | "
                "No encoding defect remains in the final matrix. |"
            ),
            (
                f"| (b) forecast vintage | "
                f"{statistics['classification_counts']['b']} | Named benefit, "
                "FTC, IWTC, MFTC, or Best Start amount differences and their "
                "tax/eligibility/AS boundary effects. |"
            ),
            (
                f"| (c) convention | {statistics['classification_counts']['c']} | "
                "Statutory complete-dollar, annual-cent, period, or AS host "
                "conventions, all numerically decomposed. |"
            ),
            (
                f"| (d) unexplained | {statistics['classification_counts']['d']} | "
                "No unexplained cell remains. |"
            ),
            "",
            "Observed reason codes:",
            "",
            "| Code | Class | Cells | Evidence |",
            "|---|:---:|---:|---|",
        ]
    )
    for reason_code in sorted(observed_reasons):
        category, title, explanation = CLASSIFICATION_DETAILS[reason_code]
        lines.append(
            f"| `{reason_code}` | {category} | "
            f"{observed_reasons[reason_code]} | "
            f"{markdown_cell(title)} — {markdown_cell(explanation)} |"
        )

    lines.extend(
        [
            "",
            "### Class (b): BEFU25 forecast versus enacted 2026/27 law",
            "",
            "Treasury commit `741a6ca4f5d27b1dc00b43dc395e39ffc4040a4b` "
            "pins the 12 August 2025 `TY27_BEFU25.yaml` forecast. RuleSpec selects "
            "law effective 1 April 2026. The enacted benefit evidence is the "
            "[Social Security (Rates of Benefits and Allowances) Order 2026]"
            "(https://www.legislation.govt.nz/regulation/public/2026/0036/"
            "latest/LMS1573997.html); the enacted FTC, MFTC, and Best Start "
            "amounts are in [Income Tax (Tax Credit) Order 2025 cls 4–6]"
            "(https://legislation.govt.nz/secondary-legislation/pco-drafted/"
            "2025/260/en/2025-11-17.pdf). IWTC is sourced to Income Tax Act "
            "2007 s MD 10 as amended by [the 2026 annual-rates Act s 105]"
            "(https://www.legislation.govt.nz/act/public/2026/8/en/latest/). "
            "These named "
            "vintages, not fitting tolerances, drive class (b).",
            "",
            "| Parameter | Treasury BEFU25 | RuleSpec enacted | Signed delta | "
            "Unit | Statutory source |",
            "|---|---:|---:|---:|---|---|",
        ]
    )
    for item in vintage_rows:
        lines.append(
            f"| {markdown_cell(item['label'])} | "
            f"{report_number(item['treasury_befu25'], 2)} | "
            f"{report_number(item['rulespec_enacted'], 2)} | "
            f"{report_number(item['signed_delta'], 2)} | "
            f"{item['unit']} | {markdown_cell(item['statute'])}; "
            f"`{item['corpus_citation_path']}` |"
        )
    lines.extend(
        [
            "",
            f"The {material_as_count} Accommodation Supplement amount rows "
            "outside the six-decimal envelope are independently decomposed in "
            "[as_rounding_diagnostic.csv](as_rounding_diagnostic.csv). A row is "
            "class (b) only when the Treasury-host-aligned RuleSpec diagnostic "
            "still differs; reg 17's `/52` FTC input, reg 18's exact JSS cutout, "
            "and reg 19 rounding remain separately visible conventions.",
            "",
            "### EMTR classification by source component",
            "",
            "The expanded R generator records Treasury's own EMTR decomposition "
            "for net wage, benefit, WFF, MFTC, IETC, Winter Energy, Best Start, "
            "and AS at every sampled point. RuleSpec recomputes the same eight "
            "components from fresh `$1` endpoint evaluations. Class-(c) rows "
            "must also recompose from exact no-floor counterfactuals for MD 13 "
            "WFF, MG 3 Best Start, LC 13 IETC, annual-cent ACC, and statutory AS "
            "host inputs. The harness aborts on a component remainder over "
            "`0.000004` or any class-(d) row.",
            "",
            "| Class | Material RuleSpec−Treasury components | Source conventions | "
            "Points | Max |Δ| | Max component remainder |",
            "|:---:|---|---|---:|---:|---:|",
        ]
    )
    for item in emtr_patterns:
        components = ", ".join(item["material_components"]) or "none"
        conventions = ", ".join(item["material_conventions"]) or "none"
        lines.append(
            f"| {item['classification']} | {markdown_cell(components)} | "
            f"{markdown_cell(conventions)} | {item['count']} | "
            f"{report_number(item['maximum_absolute_signed_delta'], 9)} | "
            f"{report_number(item['maximum_absolute_component_recomposition_remainder'], 9)} |"
        )
    lines.extend(
        [
            "",
            "The eight class-(b) EMTR points are boundary evidence, not a claim "
            "that percentage-point closeness is the target: Best Start/WFF "
            "extinction at 1476, 1477, and 1500; dual-earner IWTC extinction at "
            "1500; forecast/enacted childless JSS-to-IETC transitions at 688, "
            "689, and 692; and the benefit-vintage wage-tax interaction in the "
            "boarder proxy at 1000. The other non-matching EMTR rows recompose "
            "from named class-(c) conventions.",
            "",
            "### Class (c): period and statutory discreteness",
            "",
            "| Component | RuleSpec source period/arithmetic | Treasury alignment |",
            "|---|---|---|",
            "| Main benefits | Weekly | none |",
            "| Income tax, ACC, FTC, Best Start, IETC | Annual | divide by `365/7` |",
            "| IWTC base and MFTC | Annual rules expressed over weekly periods | divide by `52` |",
            "| IWTC abatement | Annual complete dollars | divide by `365/7`, subtract from `/52` base |",
            "| WFF total | FTC plus IWTC after one family abatement | add converted components |",
            "| Winter Energy | Per-winter total | divide by `365/7` for Treasury annual-average convention |",
            "| Accommodation Supplement | Weekly; reg 17 FTC `/52`, exact reg 18 cutout | compare pre-reg-19-rounding primary; emit host-aligned diagnostic |",
            "",
            "## Method",
            "",
            "1. Run the pinned Treasury R source with Rscript 4.3.0 and the "
            "SHA-pinned parameter file. Require a byte-identical regeneration "
            "of the original snapshot before generating the expanded oracle.",
            "2. Verify the Treasury, RuleSpec, and engine commits; generator, "
            "parameter, composition, and binary hashes; clean tracked Treasury, "
            "RuleSpec, and engine trees; scenario semantics; dense wage "
            "coordinates; and serialized continuity objects.",
            "3. Compile the ten-module RuleSpec composition from scratch and run "
            "all engine queries in `explain` mode.",
            "4. Map every profile to explicit person, child, and family inputs. "
            "The 14+ lone-parent path uses JSS with Income Test 1. Wage tax is "
            "incremental tax above grossed taxable benefit, matching raw "
            "`emtr()`.",
            "5. Evaluate the common wages `0, 160, 250, 370, 555, 740, 1000, "
            "1500` plus only the documented kink points: Best Start "
            "`775/776, 1125/1126, 1476/1477`; dual-earner WFF `121/122`; IETC "
            "`688/689, 692/693, 1265/1266, 1342/1343`.",
            "6. Calculate `EMTR = 1 - (NetIncome(w+1) - NetIncome(w))`. Carry "
            "the `$1,499 → $1,500` interval at the endpoint, matching "
            "`zoo::na.locf`.",
            "7. Compare without replacing any RuleSpec parameter. Deltas are "
            "always RuleSpec minus Treasury. Two complete fresh passes must "
            "produce byte-identical artifacts before anything is written.",
            "",
            "Treasury's app server instead calls `calculate_income()`, whose "
            "`WFF_or_Benefit: Max` wrapper can select between transfers. This "
            "audit deliberately calls raw `emtr()` on both the continuity and "
            "expanded grids.",
            "",
            "## Coverage gaps",
            "",
            "The expanded grid closes the named profile gaps—14+ lone-parent "
            "JSS, binding multi-child Best Start, dual full-time earnings, "
            "positive IETC and its kinks, Area 4, a high-rent cap, and a "
            "four-child family—but it still does not cover:",
            "",
            "- Treasury-native boarder treatment. Treasury has no boarder input; "
            "the $248 run is a cost-normalised proxy and cannot independently "
            "validate the statutory 62% transformation.",
            "- Full entitlement closures: residence/immigration, work "
            "availability, medical/student/strike/concurrent-benefit facts, "
            "assets, social housing, duplicate claims, and take-up.",
            "- Full WFF child eligibility: principal caregiver, residence, "
            "shared care, exact birth/due dates, parental leave, and part-year "
            "entitlement. Every listed child is assumed full-care for 365 days.",
            "- Full IWTC, MFTC, IETC, and Winter Energy legal eligibility. "
            "Omitted facts are supplied as disclosed stylised assumptions; WEP "
            "is host-gated by positive reconstructed benefit.",
            "- Relation-native person/child/family aggregation. The compiled "
            "composition has zero relations, so the auditable host harness "
            "performs aggregation, including the corrected Best Start family "
            "abatement.",
            "- The IncomeExplorer UI wrapper, population representativeness, "
            "non-integer wage kinks, or exhaustive threshold coverage. RR and "
            "PTR are emitted in `secondary_rates.csv` but remain outside the "
            "primary matrix.",
            "- NZ Super, Supported Living Payment, youth payments, FamilyBoost, "
            "IRRS/TAS, KiwiSaver, student loans/allowances, paid parental leave, "
            "and child-support pass-on.",
            "",
            "## Provenance and reproducibility",
            "",
            "| Item | Verified value |",
            "|---|---|",
            f"| Expanded oracle SHA-256 | `{provenance.oracle_sha256}` |",
            f"| Baseline oracle SHA-256 | "
            f"`{provenance.baseline_oracle_sha256}` |",
            f"| Oracle generator SHA-256 | "
            f"`{provenance.oracle_generator_sha256}` |",
            f"| Treasury commit | `{provenance.treasury_sha}` |",
            f"| Treasury tracked tree clean | "
            f"`{not provenance.treasury_tracked_dirty}` |",
            f"| Treasury parameter SHA-256 | "
            f"`{provenance.treasury_parameter_sha256}` |",
            f"| RuleSpec commit | `{provenance.rulespec_sha}` |",
            f"| Engine source commit | `{provenance.engine_sha}` |",
            f"| Engine binary SHA-256 | `{provenance.engine_binary_sha256}` |",
            f"| Composition SHA-256 | `{provenance.composition_sha256}` |",
            f"| Compiled artifact SHA-256 | `{compiled_artifact_sha256}` |",
            f"| Compiled shape | {compiled_counts['derived_outputs']} derived, "
            f"{compiled_counts['parameters']} parameters, "
            f"{compiled_counts['input_slots']} inputs, "
            f"{compiled_counts['relations']} relations |",
            f"| Engine evaluations per fresh pass | {engine_call_count} |",
            "| Tax-year interval | `2026-04-01` to `2027-03-31` |",
            "| Expanded snapshot `generated_at` | `2026-07-29` |",
            "",
            "Run from the Foundation workspace:",
            "",
            "```sh",
            "python3 ops/nz-lane/emtr_reproduction/run.py",
            "```",
            "",
            "The command performs the R regeneration gate and two independent "
            "fresh compile/evaluate passes, compares every artifact byte-for-byte, "
            "and only then writes the outputs. `SHA256SUMS` covers the report, "
            "expanded oracle, and machine-readable comparison artifacts.",
            "",
            "## Full comparison matrix",
            "",
            "Values are weekly unless the unit says annual. `|Δ|` is "
            "`|RuleSpec − Treasury|`; relative delta divides by `|Treasury|`. "
            "The tables include the denser kink points only in their relevant "
            "scenarios.",
            "",
        ]
    )

    rows_by_scenario: dict[str, list[ComparisonRow]] = {
        scenario.id: [] for scenario in scenarios
    }
    for row in rows:
        rows_by_scenario[row.scenario_id].append(row)
    for scenario in scenarios:
        scenario_provenance = provenance_by_id[scenario.id]
        tags = ", ".join(scenario_provenance["coverage_tags"])
        wages = ", ".join(str(wage) for wage in scenario.sampled_wages)
        lines.extend(
            [
                f"### `{scenario.id}`",
                "",
                scenario.description,
                "",
                f"Sampled wages: {wages}. Coverage tags: {tags}.",
                "",
                "| Wage | Metric | Unit | Treasury | RuleSpec | Abs delta | "
                "Relative | Class |",
                "|---:|---|---|---:|---:|---:|---:|---|",
            ]
        )
        for row in rows_by_scenario[scenario.id]:
            class_label = (
                "match"
                if row.classification == "match"
                else f"({row.classification}) `{row.reason_code}`"
            )
            lines.append(
                f"| {row.weekly_wage} | `{row.column}` | {row.unit} | "
                f"{report_number(row.treasury, 6)} | "
                f"{report_number(row.rulespec, 6)} | "
                f"{report_number(row.absolute_delta, 9)} | "
                f"{report_relative(row.relative_delta)} | {class_label} |"
            )
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def build_audit_artifacts(
    *,
    rulespec_root: Path,
    engine_root: Path,
    engine_binary: Path,
    treasury_root: Path,
    composition_path: Path,
    eligibility_closures_path: Path,
    oracle_generator: Path,
    rscript: Path,
    output_dir: Path,
) -> tuple[dict[str, str], dict[str, Any]]:
    eligibility_closures = load_eligibility_closures(
        eligibility_closures_path
    )
    regenerated = regenerate_treasury_oracles(
        rulespec_root=rulespec_root,
        treasury_root=treasury_root,
        oracle_generator=oracle_generator,
        rscript=rscript,
    )
    oracle, provenance = verify_inputs(
        regenerated=regenerated,
        rulespec_root=rulespec_root,
        engine_root=engine_root,
        engine_binary=engine_binary,
        treasury_root=treasury_root,
        composition_path=composition_path,
        eligibility_closures_path=eligibility_closures_path,
        eligibility_closures=eligibility_closures,
        oracle_generator=oracle_generator,
        rscript=rscript,
        output_dir=output_dir,
    )
    with tempfile.TemporaryDirectory(prefix="axiom-emtr-fresh-") as raw_temp:
        temp_root = Path(raw_temp)
        compiled_path = temp_root / "composition.json"
        engine, compiled, _compile_stdout = Engine.compile(
            binary=provenance.engine_binary,
            composition=provenance.composition_path,
            rulespec_root=provenance.rulespec_root,
            artifact=compiled_path,
        )
        compiled_artifact_sha256 = sha256_file(compiled_path)
        compiled_counts = {
            "derived_outputs": len(compiled["program"]["derived"]),
            "parameters": len(compiled["program"]["parameters"]),
            "input_slots": len(
                compiled["metadata"]["input_catalog"]
            ),
            "relations": len(compiled["program"]["relations"]),
        }
        if compiled_counts != {
            "derived_outputs": 176,
            "parameters": 129,
            "input_slots": 328,
            "relations": 0,
        }:
            raise HarnessError(
                f"unexpected composition shape: {compiled_counts!r}"
            )

        scenarios = [
            Scenario.from_oracle(
                raw,
                oracle["scenario_provenance"][str(raw["id"])],
            )
            for raw in oracle["scenarios"]
        ]
        evaluator = ModelEvaluator(engine, compiled, eligibility_closures)
        sweeps = {
            scenario.id: sweep_scenario(evaluator, scenario)
            for scenario in scenarios
        }
        attach_treasury_emtr_components(
            oracle=oracle,
            scenarios=scenarios,
            sweeps=sweeps,
        )
        coverage_evidence = validate_expanded_coverage(
            oracle=oracle,
            scenarios=scenarios,
            sweeps=sweeps,
        )
        comparison_rows = build_comparison_rows(
            oracle=oracle,
            scenarios=scenarios,
            sweeps=sweeps,
        )
        statistics = summary_statistics(comparison_rows)
        baseline_statistics = summary_statistics(
            [
                row
                for row in comparison_rows
                if row.scenario_id in ORIGINAL_SCENARIOS
            ]
        )
        baseline_continuity = (
            baseline_statistics["amount_control_cells"],
            baseline_statistics["amount_control_cells_agree_to_cent"],
            baseline_statistics[
                "amount_control_cells_outside_cent_by_class"
            ],
        )
        if baseline_continuity != (
            608,
            434,
            {"match": 0, "a": 0, "b": 174, "c": 0, "d": 0},
        ):
            raise HarnessError(
                "original four-scenario continuity result changed: "
                f"{baseline_continuity!r}"
            )
        vintage_rows = build_vintage_parameter_rows(compiled, provenance)
        emtr_patterns = validate_emtr_residual_patterns(
            comparison_rows, sweeps
        )
        secondary_rows = build_secondary_rate_rows(
            oracle=oracle,
            scenarios=scenarios,
            sweeps=sweeps,
        )
        as_rows = build_as_diagnostic_rows(
            oracle=oracle,
            scenarios=scenarios,
            sweeps=sweeps,
        )
        diagnostic_rows = build_diagnostic_rows(
            scenarios=scenarios,
            sweeps=sweeps,
        )

        scenario_json = [
            {
                "id": scenario.id,
                "description": scenario.description,
                "inputs": {
                    "partnered": scenario.partnered,
                    "wage1_hourly": scenario.wage1_hourly,
                    "children_ages": scenario.children,
                    "gross_wage2": scenario.gross_wage2,
                    "hours2": scenario.hours2,
                    "accommodation_costs": scenario.accommodation_costs,
                    "accommodation_rent": scenario.accommodation_rent,
                    "accommodation_area": scenario.accommodation_area,
                    "accommodation_boarder": scenario.accommodation_boarder,
                    "weekly_board_and_lodgings_paid": (
                        scenario.weekly_board_and_lodgings_paid
                    ),
                },
                "sampled_weekly_wages": scenario.sampled_wages,
                "provenance": oracle["scenario_provenance"][scenario.id],
            }
            for scenario in scenarios
        ]
        machine_payload = {
            "schema_version": 1,
            "headline": headline_text(statistics),
            "scope": {
                "comparison_target": (
                    "pinned Treasury raw R/emtr.R#emtr snapshot"
                ),
                "not_compared": (
                    "IncomeExplorer calculate_income UI wrapper and "
                    "WFF_or_Benefit: Max"
                ),
                "tax_year": {
                    "start": EXPECTED_PERIOD_START,
                    "end": EXPECTED_PERIOD_END,
                },
                "displayed_weekly_wages": EXPECTED_SAMPLE_WAGES,
                "sampled_weekly_wages_by_scenario": {
                    scenario.id: scenario.sampled_wages
                    for scenario in scenarios
                },
                "primary_columns": COMPARISON_COLUMNS,
                "oracle_match_tolerance": ORACLE_SIX_DECIMAL_TOLERANCE,
            },
            "provenance": provenance.as_json(),
            "compiled_program": {
                **compiled_counts,
                "artifact_sha256": compiled_artifact_sha256,
                "engine_evaluations": engine.call_count,
            },
            "exercise_input_catalog": {
                slot: {
                    "canonical_request_name": engine.catalog[slot],
                    "distinct": len(values),
                    "state": (
                        "not_supplied"
                        if not values
                        else "constant"
                        if len(values) == 1
                        else "varied"
                    ),
                    "observed_values": sorted(values),
                }
                for slot, values in sorted(engine.observed_inputs.items())
            },
            "declared_eligibility_closures": eligibility_closures,
            "methodology": {
                "emtr": (
                    "weekly $1 forward difference; the displayed $1,500 "
                    "value carries the $1,499 to $1,500 interval"
                ),
                "delta_sign": "rulespec minus treasury",
                "period_conversions": {
                    "tax_acc_ftc_best_start_ietc": "divide annual by 365/7",
                    "benefits": "already weekly; no conversion",
                    "iwtc_base_mftc": "divide annual by 52",
                    "iwtc_abatement": "divide annual by 365/7",
                    "winter_energy": (
                        "divide per-winter amount by 365/7 for snapshot "
                        "annual-average convention"
                    ),
                    "accommodation_supplement": (
                        "weekly statutory primary: reg 17 FTC /52, exact reg "
                        "18 JSS cutout, before reg 19 rounding; Treasury-host-"
                        "aligned inputs emitted only as diagnostics"
                    ),
                },
                "determinism": (
                    "CLI performs two independent fresh compile/evaluate "
                    "passes and requires byte-identical artifacts"
                ),
                "parameter_policy": (
                    "no Treasury amount is injected as a RuleSpec parameter; "
                    "the disclosed raw-flow IWTC branch threshold is retained "
                    "host-side"
                ),
            },
            "statistics": statistics,
            "baseline_continuity_statistics": baseline_statistics,
            "classification_definitions": {
                code: {
                    "classification": detail[0],
                    "title": detail[1],
                    "explanation": detail[2],
                }
                for code, detail in sorted(
                    CLASSIFICATION_DETAILS.items()
                )
            },
            "vintage_parameter_evidence": vintage_rows,
            "emtr_residual_patterns": emtr_patterns,
            "scenarios": scenario_json,
            "comparisons": [
                comparison_row_json(row)
                for row in comparison_rows
            ],
            "secondary_rates": secondary_rows,
            "accommodation_supplement_rounding_diagnostics": as_rows,
            "engine_state_diagnostics": diagnostic_rows,
            "expanded_coverage_evidence": coverage_evidence,
            "coverage_statement": (
                "Conditional stylised-family amount comparison; not full "
                "statutory entitlement evaluation or UI reproduction."
            ),
        }

        report = render_report(
            oracle=oracle,
            provenance=provenance,
            compiled_artifact_sha256=compiled_artifact_sha256,
            compiled_counts=compiled_counts,
            engine_call_count=engine.call_count,
            scenarios=scenarios,
            rows=comparison_rows,
            statistics=statistics,
            baseline_statistics=baseline_statistics,
            vintage_rows=vintage_rows,
            emtr_patterns=emtr_patterns,
            as_rows=as_rows,
            coverage_evidence=coverage_evidence,
        )
        artifacts: dict[str, str] = {
            "REPORT.md": report,
            EXPANDED_ORACLE_NAME: regenerated.expanded_text,
            "comparison.csv": comparison_csv_text(comparison_rows),
            "comparison.json": json_text(machine_payload),
            "secondary_rates.csv": generic_csv_text(
                secondary_rows,
                (
                    "scenario_id",
                    "weekly_wage",
                    "column",
                    "treasury",
                    "rulespec",
                    "signed_delta_rulespec_minus_treasury",
                    "absolute_delta",
                    "relative_absolute_delta",
                ),
            ),
            "as_rounding_diagnostic.csv": generic_csv_text(
                as_rows,
                (
                    "scenario_id",
                    "weekly_wage",
                    "treasury_raw_unrounded",
                    "rulespec_statutory_inputs_before_rounding",
                    "rulespec_treasury_host_aligned_before_rounding",
                    "rulespec_statutory_rounded",
                    "total_signed_delta",
                    "forecast_vintage_component_under_treasury_host_convention",
                    "statutory_host_convention_component",
                    "legal_rounding_uplift",
                    "rulespec_statutory_base_rate",
                    "rulespec_treasury_host_base_rate",
                    "rulespec_statutory_cutout",
                    "rulespec_treasury_host_cutout",
                ),
            ),
        }
        artifacts["SHA256SUMS"] = "".join(
            f"{hashlib.sha256(content.encode('utf-8')).hexdigest()}  {name}\n"
            for name, content in sorted(artifacts.items())
        )
        run_summary = {
            "headline": machine_payload["headline"],
            "statistics": statistics,
            "compiled_artifact_sha256": compiled_artifact_sha256,
            "engine_evaluations": engine.call_count,
            "artifact_names": tuple(sorted(artifacts)),
        }
        return artifacts, run_summary


def compare_fresh_artifacts(
    first: Mapping[str, str],
    second: Mapping[str, str],
) -> None:
    if set(first) != set(second):
        raise HarnessError(
            "fresh runs generated different artifact names: "
            f"{sorted(first)} != {sorted(second)}"
        )
    mismatches = [
        name
        for name in sorted(first)
        if first[name].encode("utf-8") != second[name].encode("utf-8")
    ]
    if mismatches:
        details = {
            name: (
                hashlib.sha256(first[name].encode("utf-8")).hexdigest(),
                hashlib.sha256(second[name].encode("utf-8")).hexdigest(),
            )
            for name in mismatches
        }
        raise HarnessError(
            f"fresh-run artifacts are not deterministic: {details!r}"
        )


def write_artifacts(output_dir: Path, artifacts: Mapping[str, str]) -> None:
    destination = output_dir.expanduser().resolve()
    destination.mkdir(parents=True, exist_ok=True)
    for name, content in sorted(artifacts.items()):
        (destination / name).write_text(content, encoding="utf-8")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Reproduce the pinned Treasury EMTR comparison with RuleSpec NZ"
        )
    )
    parser.add_argument(
        "--rulespec-root",
        type=Path,
        default=DEFAULT_RULESPEC_ROOT,
    )
    parser.add_argument(
        "--engine-root",
        type=Path,
        default=DEFAULT_ENGINE_ROOT,
    )
    parser.add_argument(
        "--engine-binary",
        type=Path,
        default=None,
        help=(
            "Defaults to <engine-root>/target/release/"
            "axiom-rules-engine"
        ),
    )
    parser.add_argument(
        "--treasury-root",
        type=Path,
        default=DEFAULT_TREASURY_ROOT,
    )
    parser.add_argument(
        "--composition",
        type=Path,
        default=DEFAULT_COMPOSITION,
    )
    parser.add_argument(
        "--eligibility-closures",
        type=Path,
        default=DEFAULT_ELIGIBILITY_CLOSURES,
    )
    parser.add_argument(
        "--oracle-generator",
        type=Path,
        default=DEFAULT_ORACLE_GENERATOR,
    )
    parser.add_argument(
        "--rscript",
        type=Path,
        default=DEFAULT_RSCRIPT,
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=HERE,
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    engine_binary = (
        args.engine_binary
        if args.engine_binary is not None
        else args.engine_root / "target" / "release" / "axiom-rules-engine"
    )
    try:
        first, summary = build_audit_artifacts(
            rulespec_root=args.rulespec_root,
            engine_root=args.engine_root,
            engine_binary=engine_binary,
            treasury_root=args.treasury_root,
            composition_path=args.composition,
            eligibility_closures_path=args.eligibility_closures,
            oracle_generator=args.oracle_generator,
            rscript=args.rscript,
            output_dir=args.output_dir,
        )
        second, second_summary = build_audit_artifacts(
            rulespec_root=args.rulespec_root,
            engine_root=args.engine_root,
            engine_binary=engine_binary,
            treasury_root=args.treasury_root,
            composition_path=args.composition,
            eligibility_closures_path=args.eligibility_closures,
            oracle_generator=args.oracle_generator,
            rscript=args.rscript,
            output_dir=args.output_dir,
        )
        compare_fresh_artifacts(first, second)
        if json_text(summary) != json_text(second_summary):
            raise HarnessError(
                "fresh-run summaries are not deterministic"
            )
        write_artifacts(args.output_dir, first)
    except HarnessError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(
        json.dumps(
            json_ready(
                {
                    **summary,
                    "deterministic_fresh_runs": 2,
                    "output_dir": str(
                        args.output_dir.expanduser().resolve()
                    ),
                }
            ),
            indent=2,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
