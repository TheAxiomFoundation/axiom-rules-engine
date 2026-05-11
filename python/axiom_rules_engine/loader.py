"""RuleSpec module loader."""
from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path

import yaml

from .models import Program

ROOT = Path(__file__).resolve().parents[2]


def _looks_like_rulespec(spec: dict) -> bool:
    return spec.get("format") == "rulespec/v1" or str(spec.get("schema", "")).startswith(
        "axiom.rules"
    )


def _compile_program(path: Path, binary_path: str | Path | None = None) -> Program:
    binary = (
        Path(binary_path)
        if binary_path is not None
        else ROOT / "target" / "debug" / "axiom-rules-engine"
    )
    with tempfile.TemporaryDirectory(prefix="axiom-rules-engine-program-") as temp_dir:
        artifact_path = Path(temp_dir) / "program.compiled.json"
        process = subprocess.run(
            [
                str(binary),
                "compile",
                "--program",
                str(path),
                "--output",
                str(artifact_path),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        if process.returncode != 0:
            stderr = process.stderr.strip() or "Axiom Rules Engine compile failed"
            raise RuntimeError(stderr)
        artifact = json.loads(artifact_path.read_text())
        return Program.model_validate(artifact["program"])


def load_program(path: str | Path, *, binary_path: str | Path | None = None) -> Program:
    """Load a RuleSpec module from RuleSpec YAML."""
    path = Path(path)
    spec: dict = yaml.safe_load(path.read_text()) or {}
    if not _looks_like_rulespec(spec):
        raise ValueError(
            f"{path} is not RuleSpec YAML; expected format: rulespec/v1 "
            "or schema: axiom.rules.*"
        )
    return _compile_program(path, binary_path=binary_path)
