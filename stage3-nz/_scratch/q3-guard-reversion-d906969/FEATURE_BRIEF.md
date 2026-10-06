# Feature brief: cross-period reduction (issue #67)

Implement the capability tracked in
https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/67 :
reductions over a person's own period axis, so 42 USC 415(b) AIME
(highest-35-years selection) computes end-to-end from a RuleSpec
encoding.

## Design (decided — do not relitigate, flag concerns in PROGRESS.md)

1. **Lifetime execution surface, per-period evaluation reused.**
   Add to `CompiledDenseProgram`:
   `execute_lifetime` / `execute_lifetime_f64(periods, batches,
   outputs)` where `periods: &[Period]` and `batches` is one
   `DenseBatchSpec` per period with IDENTICAL entity row order and
   count (v1 constraint — validate and error clearly if row counts
   differ; alignment by position). Internally: for each output whose
   formula contains an over-periods reduction, evaluate the
   reduction's inner expression per period with the existing
   `DenseExecutor`, stack the per-period vectors, apply the
   reduction row-wise, then evaluate the remainder of the formula in
   a context where the reduction call yields that per-entity vector.
   Non-reduction outputs error under lifetime execution only if they
   are requested and reference no reduction (direct users should use
   the existing per-period entry points) — keep it simple: lifetime
   execution only supports outputs whose formulas contain at least
   one over-periods reduction; error otherwise.
2. **Builtins** (formula.rs), valid only in lifetime context —
   error with a clear message if compiled for per-period execution:
   - `sum_over_periods(x)`
   - `max_over_periods(x)`
   - `count_over_periods(x)` (count of periods supplied)
   - `sum_top_n_over_periods(x, n)` — sum of the n largest
     per-period values of `x` for the entity; if fewer than n
     periods are supplied, missing periods contribute zero (this
     mirrors 415(b): computation years count whether or not the
     worker had earnings). `n` may be any scalar expression
     (typically a parameter); truncate to integer, error if < 1.
3. **PyO3 surface**: expose `execute_lifetime_f64` on the Python
   binding exactly parallel to the existing `execute_f64` (see the
   python/ package for how execute_f64 is exported and tested).
4. **No RuleSpec schema changes.** The declarative
   `over: periods` schema form is a separate future conversation;
   this feature is expression-level only.

## Tests

- Rust: unit tests for each builtin (top-n correctness incl. ties,
  n greater than period count, n as expression, negative values;
  alignment validation errors; per-period-context rejection of the
  builtins).
- Python: an integration test that compiles a MINIMAL inline
  rulespec module with rules
  `aime = floor(sum_top_n_over_periods(indexed_earnings, 35) / 420)`
  (shape it to whatever the module/loader needs), feeds a synthetic
  worker with a 40-year history through `execute_lifetime_f64`, and
  asserts the AIME equals the hand-derived value written out in a
  comment. This is the acceptance test from issue #67.

## Working rules

- Worktree: this directory, branch `cross-period-reduction`.
- Read the repo's CLAUDE.md/AGENTS.md and CI workflow first; match
  existing code style; run `cargo fmt`, `cargo clippy`, `cargo test`
  and the python-ext test path CI runs, before every commit.
- Maintain PROGRESS.md (state / done / next / concerns) from the
  start; commit and push after every coherent step
  (`git push -u origin cross-period-reduction` on first push).
- Finish with a DRAFT pull request titled
  "Cross-period reductions: lifetime execution surface (#67)"
  referencing issue #67, summarizing semantics and limitations
  (positional alignment, lifetime-only builtins). Do not merge.
