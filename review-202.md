# Independent review of issue #202

**Verdict: REQUEST CHANGES**

Reviewed only `59014e0..5f9045dd06e72c459120224ac30cc241e9edbf08`.

## Findings

### P2 — A later membership error hides an earlier sum overflow

Location: [src/dense.rs:3154](src/dense.rs#L3154), the new error-producing node, interacting with [src/dense.rs:3101](src/dense.rs#L3101) and [src/dense.rs:3107](src/dense.rs#L3107).

**Executed and minimized.** One household `h1` has three `member(household, person)` tuples, with the related people ordered `p1`, `p2`, `p3`. Their decimal `income` values are:

| Person | Income |
| --- | ---: |
| p1 | 79228162514264337593543950335 (`Decimal::MAX`) |
| p2 | 1 |
| p3 | 0 |

The only output is a decimal Household rule:

```text
n = sum_related(member, current_slot=0, related_slot=1,
                value=input(income),
                where=(input(income) > 0 OR relation_member(member, 0, 1)))
```

All records cover January 2026. The dense batch has `row_count=1`, the `member/0/1` relation's offsets `[0, 3]`, and the same income column in sorted person-ID order. This uses only representable base-relation data.

Observed results through `execute_request` and `CompiledProgramArtifact::compile` → `DenseCompiledProgram::from_artifact` → `execute`:

```text
Explain / expected:
arithmetic overflow: addition result is outside the representable decimal range

Dense / actual:
type mismatch: relation predicate `member` can only be evaluated inside a derived relation
```

Explain adds the first two incomes and stops on overflow at [src/engine.rs:1080](src/engine.rs#L1080). It never evaluates the third person's membership predicate. Dense evaluates the predicates first, lifts the third person's error to the household at line 3101, and then skips every addition for that household at line 3107. The new node therefore exposes an error explain never reaches.

**Regression attribution also executed:** in an isolated source copy under this checkout's `target`, restoring only `dense.rs` from `59014e0` makes the unchanged reproduction pass: both modes return the addition-overflow error. The reviewed source was never modified for this comparison.

Suggested fix: merge predicate/value errors and checked accumulation in related-row order. For each root, stop at its earliest member error or addition overflow. Preserve relation-filter errors as an earlier stage, since explain finishes constructing relation membership before starting the sum. Merely moving `lift_errors` after all additions would reverse the bug for an earlier predicate error and later overflow. Add this example as an error-order regression.

### P3 — Documentation promises runtime behavior for unsupported root predicates

Location: [docs/execution-semantics.md:136](docs/execution-semantics.md#L136).

The new sentence says a `relation_member` “anywhere else fails the rows that reach it.” **Executed:** declaring `member` and the Household judgment `is_member = relation_member(member, 0, 1)`, then requesting `is_member`, gives:

```text
Explain: type mismatch: relation predicate `member` can only be evaluated inside a derived relation
Dense compilation: Unsupported("relation predicate `member`")
```

Root and current-entity compilation still reject this expression at [src/dense.rs:1464](src/dense.rs#L1464) and [src/dense.rs:1937](src/dense.rs#L1937). This is an inaccurate new documentation claim, not a newly introduced root-evaluation defect.

Suggested fix: limit the statement to count/sum related predicates and inlined related-entity rule bodies, and state that root/current-entity membership remains unsupported.

## Checked and found correct

- Membership scope is introduced only for a derived relation's own predicate and cleared for inlined rule bodies. Direct `if`, lowered `match`, scalar operands, and parameter indices preserve explain's context. Root/current compiler paths conservatively decline membership rather than leaking scope.
- Source/ancestor membership with the matching derivation-edge slots holds for candidates representable by dense batches. Parent filters use their own scope. The documented exclusion of asserted tuples under derived-relation names is material and correct.
- Boolean short-circuit masks and ordered merging of ordinary predicate/value errors preserve the first related-row error. Relation-filter errors precede aggregate predicates/values, matching explain. The finding concerns overflow during the subsequent reduction.
- All six named tests contain assertions supporting their central claims. Their error fixtures do not cover an earlier accumulation overflow competing with a later membership error.
- Artifact refusals cannot match successful explain results. Dense declines return before success/error coverage counters. EVAL_ORDER, FULL, and RELATIONS retain their original generator strategy and random draws; added boxing delegates to the same strategy.

## Reproduction

The complete input and both API paths are in [review/issue202_repro.rs](review/issue202_repro.rs). From this checkout, run:

```sh
CARGO_TARGET_DIR="$PWD/target" CARGO_PROFILE_DEV_DEBUG=0 cargo build --lib
python3 review/run_issue202_repro.py
```

The root documentation probe passes, then the sum parity assertion fails on `5f9045d`. The standalone probe stays outside the integration-test suite.

## Validation and workspace

- The minimized overflow probe fails with the exact observed outputs above; the root documentation probe confirms compile-time rejection. Both were also executed through `cargo test --test review_issue202_probes review_ -- --nocapture` (one expected failure, one pass); that temporary integration-test file was removed after retaining the standalone reproduction.
- `CARGO_TARGET_DIR="$PWD/target" CARGO_PROFILE_DEV_DEBUG=0 cargo test --test dense_relation_member`: **6 passed**.
- Full `execution_mode_parity` debug test binary: **6 properties passed**, 5,500 generated cases total, zero reported divergences. The test binary was compiled directly with `rustc --test` against this checkout's Cargo-built library/dependencies while the standard Cargo build was occupied compiling test-only dependencies; no release build was run.
- Dense's 3,000 cases included 60 membership errors, 224 successful membership-filter cases, 291 membership declines, and 29 artifact refusals. The targeted overflow example still fails despite this passing random run.
- The unchanged standalone reproduction fails against `5f9045d` (exit 101) and passes against the isolated parent-dense comparison (exit 0).
- All compilation uses this checkout's `target` directory and `CARGO_PROFILE_DEV_DEBUG=0`. No author-worktree build or mutation was performed.
- Git branch creation and staging failed because shared Git metadata is outside the writable sandbox (`index.lock: Operation not permitted`). The checkout started detached at the requested commit; report changes could not be committed.
