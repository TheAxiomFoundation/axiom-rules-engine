"""RuleSpec module loader."""
from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path

from .models import Program

ROOT = Path(__file__).resolve().parents[2]

DEFAULT_TIMEOUT_SECONDS = 600.0


def _compile_program(
    path: Path,
    rulespec_roots: tuple[Path, ...],
    command: str,
    binary_path: str | Path | None = None,
    timeout: float | None = DEFAULT_TIMEOUT_SECONDS,
) -> Program:
    binary = (
        Path(binary_path)
        if binary_path is not None
        else ROOT / "target" / "debug" / "axiom-rules-engine"
    )
    with tempfile.TemporaryDirectory(prefix="axiom-rules-engine-program-") as temp_dir:
        artifact_path = Path(temp_dir) / "program.compiled.json"
        command = [
            str(binary),
            command,
            "--program",
            str(path),
            "--output",
            str(artifact_path),
        ]
        for root in rulespec_roots:
            command.extend(["--rulespec-root", str(root)])
        process = subprocess.run(
            command,
            text=True,
            capture_output=True,
            check=False,
            timeout=timeout,
        )
        if process.returncode != 0:
            stderr = process.stderr.strip() or "Axiom Rules Engine compile failed"
            raise RuntimeError(stderr)
        artifact = json.loads(artifact_path.read_text())
        return Program.model_validate(artifact["program"])


def load_program(
    path: str | Path,
    *,
    rulespec_roots: tuple[str | Path, ...],
    binary_path: str | Path | None = None,
    timeout: float | None = DEFAULT_TIMEOUT_SECONDS,
) -> Program:
    """Load a RuleSpec module from RuleSpec YAML.

    ``rulespec_roots`` is the non-empty explicit set of canonical country
    checkouts passed to the engine. ``timeout`` bounds the compile subprocess
    in seconds; ``None`` disables the bound. On expiry
    ``subprocess.TimeoutExpired`` is raised.
    """
    path = Path(path)
    roots = tuple(Path(root) for root in rulespec_roots)
    if not roots:
        raise ValueError("at least one explicit rulespec-<country> root is required")
    return _compile_program(
        path, roots, "compile", binary_path=binary_path, timeout=timeout
    )


def load_composed_program(
    path: str | Path,
    *,
    rulespec_roots: tuple[str | Path, ...],
    binary_path: str | Path | None = None,
    timeout: float | None = DEFAULT_TIMEOUT_SECONDS,
) -> Program:
    """Compile exact ``axiom-compose`` output through ``compile-composed``."""
    roots = tuple(Path(root) for root in rulespec_roots)
    if not roots:
        raise ValueError("at least one explicit rulespec-<country> root is required")
    return _compile_program(
        Path(path),
        roots,
        "compile-composed",
        binary_path=binary_path,
        timeout=timeout,
    )
