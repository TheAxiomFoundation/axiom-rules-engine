"""Run after `CARGO_TARGET_DIR="$PWD/target" CARGO_PROFILE_DEV_DEBUG=0 cargo build --lib`."""

import os
from pathlib import Path
import subprocess


root = Path(__file__).resolve().parents[1]
deps = root / "target/debug/deps"
binary = root / "target/issue202_repro"
command = [
    "rustc", "--edition=2024", str(root / "review/issue202_repro.rs"),
    "-C", "debuginfo=0", "-L", f"dependency={deps}", "-o", str(binary),
]
for name in ("axiom_rules_engine", "serde_json", "rust_decimal"):
    libraries = list(deps.glob(f"lib{name}-*.rlib"))
    if not libraries:
        raise SystemExit(f"Build this checkout's library first: missing {name}")
    library = max(libraries, key=lambda path: path.stat().st_mtime_ns)
    command.extend(("--extern", f"{name}={library}"))
environment = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"), CARGO_PROFILE_DEV_DEBUG="0")
subprocess.run(command, cwd=root, env=environment, check=True)
raise SystemExit(subprocess.run([str(binary)], cwd=root, env=environment).returncode)
