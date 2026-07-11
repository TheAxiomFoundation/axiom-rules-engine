from __future__ import annotations

import subprocess
from pathlib import Path

from .models import (
    CompiledExecutionRequest,
    CompiledProgram,
    Dataset,
    ExecutionMode,
    ExecutionQuery,
    ExecutionRequest,
    ExecutionResponse,
    Program,
)


DEFAULT_TIMEOUT_SECONDS = 600.0


class AxiomRulesEngine:
    def __init__(
        self,
        binary_path: str | Path = "target/debug/axiom-rules-engine",
        *,
        timeout: float | None = DEFAULT_TIMEOUT_SECONDS,
    ) -> None:
        """Wrap the Rust engine binary.

        ``timeout`` bounds every engine subprocess in seconds so a
        pathological program cannot hang the caller; ``None`` disables the
        bound. On expiry the engine process is killed and
        ``subprocess.TimeoutExpired`` is raised.
        """
        self.binary_path = Path(binary_path)
        self.timeout = timeout

    def execute(self, request: ExecutionRequest) -> ExecutionResponse:
        process = subprocess.run(
            [str(self.binary_path)],
            input=request.model_dump_json(exclude_none=True),
            text=True,
            capture_output=True,
            check=False,
            timeout=self.timeout,
        )
        if process.returncode != 0:
            stderr = process.stderr.strip() or "Axiom Rules Engine executable failed"
            raise RuntimeError(stderr)
        return ExecutionResponse.model_validate_json(process.stdout)

    def execute_compiled(
        self, *, artifact_path: str | Path, request: CompiledExecutionRequest
    ) -> ExecutionResponse:
        process = subprocess.run(
            [
                str(self.binary_path),
                "run-compiled",
                "--artifact",
                str(Path(artifact_path)),
            ],
            input=request.model_dump_json(exclude_none=True),
            text=True,
            capture_output=True,
            check=False,
            timeout=self.timeout,
        )
        if process.returncode != 0:
            stderr = process.stderr.strip() or "Axiom Rules Engine executable failed"
            raise RuntimeError(stderr)
        return ExecutionResponse.model_validate_json(process.stdout)

    def compile(
        self,
        *,
        program_path: str | Path,
        rulespec_roots: tuple[str | Path, ...],
        output_path: str | Path,
    ) -> CompiledProgram:
        return self._compile_file(
            command="compile",
            program_path=program_path,
            rulespec_roots=rulespec_roots,
            output_path=output_path,
        )

    def compile_composed(
        self,
        *,
        program_path: str | Path,
        rulespec_roots: tuple[str | Path, ...],
        output_path: str | Path,
    ) -> CompiledProgram:
        return self._compile_file(
            command="compile-composed",
            program_path=program_path,
            rulespec_roots=rulespec_roots,
            output_path=output_path,
        )

    def _compile_file(
        self,
        *,
        command: str,
        program_path: str | Path,
        rulespec_roots: tuple[str | Path, ...],
        output_path: str | Path,
    ) -> CompiledProgram:
        if not rulespec_roots:
            raise ValueError("at least one explicit rulespec-<country> root is required")
        argv = [
            str(self.binary_path),
            command,
            "--program",
            str(Path(program_path)),
            "--output",
            str(Path(output_path)),
        ]
        for root in rulespec_roots:
            argv.extend(["--rulespec-root", str(Path(root))])
        process = subprocess.run(
            argv,
            text=True,
            capture_output=True,
            check=False,
            timeout=self.timeout,
        )
        if process.returncode != 0:
            stderr = process.stderr.strip() or "Axiom Rules Engine compile failed"
            raise RuntimeError(stderr)
        return CompiledProgram.model_validate_json(Path(output_path).read_text())

    def run(
        self,
        *,
        mode: ExecutionMode,
        program: Program,
        dataset: Dataset,
        queries: list[ExecutionQuery],
    ) -> ExecutionResponse:
        return self.execute(
            ExecutionRequest(
                mode=mode,
                program=program,
                dataset=dataset,
                queries=queries,
            )
        )

    def run_compiled(
        self,
        *,
        mode: ExecutionMode,
        artifact_path: str | Path,
        dataset: Dataset,
        queries: list[ExecutionQuery],
    ) -> ExecutionResponse:
        return self.execute_compiled(
            artifact_path=artifact_path,
            request=CompiledExecutionRequest(
                mode=mode,
                dataset=dataset,
                queries=queries,
            ),
        )
