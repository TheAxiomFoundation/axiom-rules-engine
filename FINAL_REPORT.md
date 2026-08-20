# PR #136 maintenance rebase report

Date: 2026-08-20

Branch: `codex/node-state-annotations-115`

Base: `origin/main` at `2c0e1ed`

## Outcome

The node-state annotation producer is rebased onto current engine main,
integrated with the intervening relation-slot and query-output semantics, and
fully validated. The branch is 0 commits behind main and remains unmerged for
engine-side review.

`docs/node-annotations.md` now states that certified-node serving in
`axiom-api/docs/certified-serving.md` and `certify_nodes` in `axiom-oracles`
issue #427 consume compiled-node `provenance` as the `provision_rooted`
criterion.
This PR is their missing producer. Artifacts must be **REBUILT after this PR
merges** for the field to exist; consumers must fail closed for older artifacts
without it.

## Conflict summary

Fourteen branch commits were replayed over 42 intervening main commits. The
rebase required 7 textual conflict-hunk resolutions across 4 replayed commits:

- `src/rulespec.rs`: 4 hunks;
- `src/compile.rs`: 2 hunks;
- `schemas/compiled-artifact.v2.schema.json`: 1 hunk.

The resolutions retain main's lowering options and diagnostics, relation
argument/slot validation, compatible duplicate-parameter normalization,
relation source-path diagnostics, `extends: null` compatibility, and compiled
input-catalog loading. They also retain the branch's fail-closed provenance and
declaration-origin binding.

Post-rebase semantic integration added reachability traversal for main's
`ExactlyOne` judgment, current relation `slot_entities`, derived-graph
validation ordering, non-indexed parameter output roots, indexed-output
rejection, execution-query name/ID resolution, and safe provenance
deduplication after compatible parameter normalization. The stage-3 result
golden was rebuilt because annotations intentionally change its plan and trace
digests. A Python native fixture was updated to declare its canonical output ID
under the same execution-query contract.

## Validation

| Lane | Result |
| --- | --- |
| `cargo test --all-features` | 369 passed; 0 failed; 0 ignored |
| Schema test binaries | 16 passed; 0 failed (8 golden, 6 fidelity, 2 conformance) |
| Canonical RuleSpec corpus | 4,336 modules + 4,336 companion tests passed; 0 failed; 8,672 files total |
| Python 3.14 native suite | 85 passed; 0 failed after release PyO3 rebuild |
| Native extension import | Passed |
| PyO3 `cargo check` | Passed |
| PyO3 `cargo build` | Passed |
| WASM no-default-features check | Passed |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |

The real corpus lane used the `rulespec-us` canonical-layout revision
`a0a0a3d428914fe6eee794adc4675192443c3fe7`. Live `rulespec-us` `origin/main`
still contains legacy path components that the engine's canonical-root hard
cut rejects before schema validation, so it is not an admissible exact root.
No corpus content or schema ratchet was changed.

No toolchain, CI, CODEOWNERS, or dependency-pin files were changed.

## Publication

The validated rebased history and this report are committed locally, but
publication is blocked by the environment:

- three force-with-lease HTTPS pushes failed before authentication with
  `Could not resolve host: github.com`;
- explicit-IP HTTPS probes cannot leave the sandbox;
- the connected GitHub API still reads PR #136, but both an unreferenced tree
  write and a tiny blob write were cancelled before mutation.

A final read confirms the remote head remains the expected pre-rebase
`db801cc245dffd83cafbddbd3f08846b1344e2a6`; PR #136 is open, draft, and
unmerged. Its description was not changed while the remote still serves the
old code. Once GitHub writes are restored, push with an exact lease on that SHA
and apply this report's rebase and validation summary to the PR description.
Do not merge; engine-side review remains required.
