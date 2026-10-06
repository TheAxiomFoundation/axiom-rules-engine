# PROGRESS — Lane B execution semantics and numeric fidelity audit

Audit date: 2026-07-27

Release line under test: `v0.1.1` at `e3e2da83222463d9b68b0681c00820e9d412c011`

Comparison line: `origin/main` in sibling read-only worktree `../laneC`

Report: `../../laneB-execution-numerics.md`

## State

Source mapping and differential reproduction are in progress. The report now
contains two confirmed S1s on both public lines: the published Flemish jobbonus
returns EUR 650 in Explain and EUR 0 in Fast for identical input, and dense f64
mis-rounds a published Bristol half-penny case from £1.01 to £1.00. There is
also a release-only S2 stage risk for undeclared GHS handling.

The audit will reproduce and classify execution-mode divergences, assess PR #101,
and test numeric fidelity and the issue set #69, #70, #72, #73, #74, #80, and
#100 on both public code lines. Engine defects will not be fixed in this lane.

## Done

- Resolved the staged worktree paths: the brief's `laneB` and `laneC` directories
  are present under `lanes/`.
- Confirmed this worktree is detached at tag `v0.1.1`, commit
  `e3e2da83222463d9b68b0681c00820e9d412c011`.
- Read the GitNexus PR-review, debugging, and impact-analysis skill instructions.
- Replaced the unrelated pre-existing issue-#67 progress file with this audit
  ledger.
- The staged worktree's Git index resolves into the prohibited, write-blocked
  shared clone under `~/TheAxiomFoundation/`; the first normal commit therefore
  failed before writing an index lock. Audit checkpoints use an isolated Git
  directory stored inside this assigned worktree and include audit artifacts
  only.
- Confirmed from `tests/execution.rs:99-113` on both lines that
  `fast_mode_matches_explain_mode_on_batch` sends `ExecutionMode::Fast` in both
  the variable named `explain` and the variable named `fast`.
- Read issue bodies #69, #70, #72, #73, #74, #80, #100, and #101.
- Located PR #101 at `refs/review/pr101` (`6669d82`): it is test-only, changing
  `tests/execution.rs` and adding `tests/execution_mode_parity.rs`.
- Read PR #101's entire 725-line differential test and its change to the named
  parity test.
- Mapped lazy Explain versus eager Fast/dense evaluation, input-interval
  selection, parameter-index conversion, and heterogeneous output warmup in
  the source.
- Ran `cargo test --offline --quiet --test rounding` on both `v0.1.1` and
  `main`: 14 passed, 0 failed on each line. This covers rounding modes, minor
  units, composition, trace visibility, and Decimal/f64 dense paths.
- Created the requested report at `../../laneB-execution-numerics.md` and
  checkpointed the confirmed vacuous-test finding and sound rounding result.
- Built minimal overlapping-input fixtures and reproduced Explain = 20 versus
  Fast = 10 on both lines.
- Compiled the real published
  `rulespec-be/be-vlg/regulations/employment/jobbonus.yaml` on both lines. With
  identical overlapping wage records, the published annual full-time amount is
  EUR 650 in Explain and EUR 0 in Fast. Both executions return success and Fast
  reports `actual_mode: fast`. Confirmed S1, both, STAGE RISK.
- Read the exact selection mechanisms: Explain sorts covering records by
  descending interval start (`engine.rs:153-163`); Fast overwrites cells in
  DatasetSpec order (`bulk.rs:263-271`).
- Compiled the published Ghana COVID levy on both lines. On `v0.1.1`, Explain
  errors `unit GHS was not declared` while Fast completes with GHS 10; `main`
  seeds GHS and does not have that unit gap. Confirmed S2, release-only, STAGE
  RISK.
- Added the S1 and S2 findings, full execution transcripts, live-content
  excerpts, mechanisms, impacts, and fix sketches to the report immediately.
- Persisted `audit-fixtures/bristol-live-rounding.rs`, which compiles the exact
  published Bristol council-tax RuleSpec and runs one £1.005 input through
  Explain, Fast, Decimal dense, and f64 dense.
- Ran that fixture against both line-specific libraries: Explain/Fast/Decimal
  dense each return £1.01; dense f64 returns £1.00. The live rule explicitly
  requires half-penny-or-more to round up. Confirmed S1, both, STAGE RISK, and
  checkpointed it immediately in the report.
- Read the f64 path: Decimal converts through `to_f64`; HalfUp multiplies by
  `10^minor_units` and calls binary-float `round` (`dense.rs:189-221`). The two
  public `dense.rs` files are byte-identical.

## Next

1. Inventory offline issue bodies, PR #101 refs/diff, repository architecture,
   tests, and GitNexus index state.
2. Reproduce each claimed evaluator divergence with minimal programs on
   `v0.1.1` and `main`, recording whether money or eligibility moves.
3. Search published RuleSpec repositories for reachability.
4. Audit YAML-number conversion, rounding/minor units, missing/Undetermined
   propagation, related-entity aggregation, overflow, dtype invariance, lifetime
   materialization, Judgment coverage, and OverPeriods serialization coverage.
5. Checkpoint every confirmed result into the report and this ledger, committing
   each coherent audit step.
