# PR #207 delta re-review

**Verdict: APPROVE** — reviewed `8f6e2a617106c7f69be3ba690584851261eff4a6` against `5f9045d`, with `5e37d59` used to distinguish the merged #204 changes from this PR.

No actionable defects found. Both prior findings are resolved.

## P2: first-error ordering is fixed

The unchanged `review/issue202_repro.rs` now exits successfully. On incomes `[Decimal::MAX, 1, 0]` with `where income > 0 OR relation_member(member)`, both explain and dense report:

```text
arithmetic overflow: addition result is outside the representable decimal range
```

- `src/dense.rs:3105` removes expression-error members from the live set. A predicate/value error at member k therefore prevents addition at k, including when adding its raw input would overflow.
- `src/dense.rs:3116` merges earlier member errors before the next live addition; `stopped` preserves each owner's first error. An earlier overflow cannot be overwritten by a later predicate/value error.
- `src/dense.rs:3128` drains remaining errors, including roots with no live members. Root-keyed errors and `first_row_error` (`src/dense.rs:2242`) preserve requested root order across different failure kinds.
- `src/dense.rs:3131` merges these errors into the existing relation-filter errors, retaining the latter's precedence, as explain constructs relation membership before summing.

Executed additional probes: same-member predicate/value errors before potential overflow; positive overflow and negative underflow; errors before/after accumulation; empty and all-error roots; 90 ordered pairs of roots with different failure positions/types; filter errors before aggregate errors. No divergence found.

## P3: documentation now matches the implementation

`docs/execution-semantics.md:137`–`142` now distinguishes runtime errors in related predicates/rule bodies from compile-time rejection in root/current rules. This matches the context-free related node at `src/dense.rs:1549` and rejections at `src/dense.rs:1466` and `1939`.

Executed root/current-rule probes with membership hidden after an always-true `or`: explain succeeds, while dense declines compilation, exactly as the revised wording states. The original direct-root probe still reports the documented compile-time rejection.

## Remaining delta

- The new ancestor-slot test (`tests/dense_relation_member.rs:1034`) passes for mixed-direction source chains and rejects the off-chain base-slot test. The compiler comment correctly distinguishes a sufficient acceptance condition from all memberships explain could prove true.
- `record_dense` (`tests/execution_mode_parity.rs:3474`) excludes declines/refusals from execution counters, counts a refusal only with a matching explain key, and requires both runtime messages to have the outside-derived-relation suffix for that error counter. The effect-based decisive-membership counter is inherited from #204 and used per case. Normal parity comparison still rejects divergent outcomes.
- The 20% decline cap (`tests/execution_mode_parity.rs:3634`) matches the stated policy. FULL/RELATIONS generator changes visible against `5f9045d` come from merged #204; this PR preserves their strategies relative to `5e37d59`.
- No other runtime changes beyond the reviewed sum loop occur in the requested source delta; remaining changes are documentation/comments and tests.

## Executed validation

All builds used this isolated checkout's own `target` and `CARGO_PROFILE_DEV_DEBUG=0`.

- `cargo build --lib`, then the unchanged `python3 review/run_issue202_repro.py`: **passed**.
- `cargo test --test dense_relation_member --test execution_mode_parity`: **8 named tests + 6 parity properties passed** (5,500 generated cases, including 3,000 dense cases; default seed).
- `python3 review/run_issue207_ordering.py`: **5 reviewer probes passed**, including **468** exhaustive sequence/program combinations and **90** ordered root pairs. Both retained runners use Cargo's artifact manifest to select compatible library/dependency builds; the original runner copy was hardened after a mixed-artifact probe-build failure. The original reproduction source is unchanged.
- `git diff --check`: passed. No production source, submitted tests, or documentation was edited.

## Workspace and artifacts

The original checkout and existing review files were preserved. Its shared Git metadata prevented fetch/checkout/staging, so the re-review used `review/recheck-207`, an isolated checkout entirely inside the assigned workspace. Fetch from origin succeeded there and both HEAD and FETCH_HEAD resolved to `8f6e2a6` before review. Build artifacts were copied, not shared, and rebuilt for this checkout.

The review report, original reproduction copies, and additional probe sources/runner are retained on the local `fix/dense-relation-member-202` branch of that isolated checkout. Nothing was pushed.
