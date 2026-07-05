# PROGRESS — cross-period reduction (issue #67)

Branch: `cross-period-reduction` (worktree axiom-engine-67, from origin/main).

## State
Design finalized after full read of dense.rs, formula.rs, spec.rs, model.rs,
engine.rs, bulk.rs, api.rs, rulespec.rs, python-ext/src/lib.rs, python/dense.py,
CI workflow, and tests/dense.rs. Starting implementation.

## Design (from brief, settled) + resolved mechanics

### New expression node (threaded end-to-end)
- model `ScalarExpr::OverPeriods { kind: OverPeriodsKind, value: Box<ScalarExpr>, n: Option<Box<ScalarExpr>> }`
  where `OverPeriodsKind ∈ { Sum, Max, Count, SumTopN }`.
- spec `ScalarExprSpec::OverPeriods { kind, value, n }` (serde-tagged, round-trips; additive → no artifact version bump).
- dense `CompiledScalarExpr::OverPeriods { kind, value: Box<..>, n: Option<Box<..>> }`.
- formula.rs `lower_to_scalar` parses the four builtin `Call`s:
  - `sum_over_periods(x)` → OverPeriods{Sum, x, None}
  - `max_over_periods(x)` → OverPeriods{Max, x, None}
  - `count_over_periods(x)` → OverPeriods{Count, x, None}  (x still lowered so alignment/period count is well-defined; count ignores values)
  - `sum_top_n_over_periods(x, n)` → OverPeriods{SumTopN, x, Some(n)}
  arg-count errors mirror existing builtins.

### Per-period-context rejection (lifetime-only)
- Per-period dense executor `eval_scalar_expr`: OverPeriods → EvalError::OverPeriodsOutsideLifetime.
- dense `compile_related_scalar` / `compile_current_scalar_expr`: reject (can't nest a cross-period
  reduction inside a related/where expr) via DenseCompileError::Unsupported.
- Sparse `engine.rs` eval_scalar_expr + bulk.rs (both columnar and related scalar paths) + api.rs
  dep-collector + model.rs input-slot collector: handle new variant (sparse/bulk error clearly;
  collectors recurse into value/n so inputs referenced inside a reduction are still discovered).

### Lifetime execution surface (dense.rs)
- `execute_lifetime(periods: &[Period], batches: Vec<DenseBatchSpec>, outputs) -> DenseExecutionResult` (Decimal)
- `execute_lifetime_f64(...)` (f64), parallel to execute/execute_f64.
- Validation:
  - periods.len() == batches.len(), non-empty.
  - every batch binds identically (bind_batch each) and all have the SAME row_count (v1 positional
    alignment; error clearly on mismatch — new EvalError::LifetimeRowCountMismatch).
  - each requested output's compiled formula must contain ≥1 OverPeriods node; else
    EvalError::LifetimeOutputWithoutReduction (brief: lifetime exec only supports reduction outputs).
- Evaluation model (LifetimeExecutor):
  - Build one per-period bound batch + a way to make a per-period DenseExecutor<N>.
  - Outer formula evaluated ONCE (row-wise), recursively:
    * OverPeriods{kind,value,n}: evaluate `value` per period with that period's DenseExecutor
      → P columns of length R; transpose to per-row Vec<N> of length P; reduce row-wise:
        Sum = Σ; Max = max (error if P==0 — guaranteed ≥1 by non-empty periods); Count = P (ignores value);
        SumTopN = sort desc, take top n, zero-pad when P<n (missing periods contribute 0 — mirrors 415(b)).
        n: evaluate `n` at reference period (last), per-row; truncate toward zero to i64; error if < 1
        (EvalError::TypeMismatch with a clear message). n is per-row but typically constant.
    * Literal / Add / Sub / Mul / Div / Max / Min / Ceil / Floor / If(condition over reductions): recurse row-wise.
    * ParameterLookup / Derived(scalar): evaluate at reference period (last period) — parameters like a
      bend point are indexed to the eligibility/last year; Derived recurses through lifetime evaluator so a
      derived that itself reduces works.
    * Period-ambiguous leaves OUTSIDE any reduction (bare Input, InputOrElse, PeriodStart/End, CountRelated,
      SumRelated): error EvalError::LifetimeAmbiguousLeaf — tell the caller these must appear inside an
      over-periods reduction. (Faithful to "reused per-period evaluation"; keeps semantics unambiguous.)
  - Reference period = last period supplied (documented). Rationale: over-periods reductions are the
    lifetime axis; scalars combined with the reduction (bend points, divisor) are resolved at the
    determination period, which is the most recent supplied period.

### PyO3
- `execute_lifetime_f64(periods, batches, outputs=None)` on CompiledDenseProgramHandle, parallel to
  execute_f64. `periods`: list of (period_kind, start, end); `batches`: list of the same input/relation
  dict shape build_batch already accepts. Python wrapper method `execute_lifetime_f64` in dense.py.

### No RuleSpec schema changes (expression-level only). Additive serde on ScalarExprSpec.

## Done
- Full exploration; PROGRESS.md seeded.
- model.rs: OverPeriodsKind (+ as_call_name) + ScalarExpr::OverPeriods + input-slot collector arm.
- spec.rs: ScalarExprSpec::OverPeriods + OverPeriodsKindSpec + to_model.
- formula.rs: parse sum/max/count/sum_top_n_over_periods in lower_to_scalar; promote_ints descends into value.
- engine.rs: EvalError variants (OverPeriodsOutsideLifetime, Lifetime*, OverPeriodsTopNInvalid) + sparse reject arm.
- bulk.rs (both paths) + api.rs collector: new arms.
- compile.rs: fast-blocker (adds blocker), dependency collector, relation-member collector arms.
- rulespec.rs: 4 traversal arms (alias rewrite, relation-name collect, relation-ref rewrite, imported-derived check).
- dense.rs: CompiledScalarExpr::OverPeriods + compile arm + per-period reject + related/current reject +
  DenseNum::to_i64_trunc + LifetimeExecutor + execute_lifetime[_f64] + derived_reduces_over_periods gate.
- schemas/compiled-artifact.v1.schema.json regenerated (additive over_periods variant; no format-version bump).
- GREEN: cargo build, fmt, cargo test (default + schema feature), cargo check python-ext.
- clippy: baseline origin/main already emits 36 lib warnings; my additions introduce ZERO new warnings
  (every warning location is in pre-existing code). Not touching the 36 pre-existing ones (scope).

- python-ext/src/lib.rs: execute_lifetime_f64 PyO3 method + shared execution_to_pydict +
  split_lifetime_batch; python/dense.py: execute_lifetime_f64 wrapper.
- tests/lifetime.rs: 22 Rust tests — AIME acceptance (Decimal + f64, =2833), each builtin, top-n
  (ties, n>period_count zero-pad, n as param expr, non-integer n truncation, negatives), multi-row
  alignment, outer-formula-with-parameter, all validation/error paths, per-period rejection, Decimal exactness.
- python/tests/test_dense_lifetime.py: 4 tests — AIME acceptance (=2833), two-worker row alignment,
  period/batch-count and row-count mismatch errors. Built wheel via maturin (py3.14 venv), 36/36 python tests pass.

## Acceptance test AIME value
AIME = 2833. Synthetic 40-year worker, indexed earnings year k = 12_000 + 1_000*(k-1) (12_000..51_000).
Top 35 of 40 drops the lowest 5; sum(top 35) = 35*(17_000+51_000)/2 = 1_190_000;
floor(1_190_000 / 420) = floor(2833.33..) = 2833.

## Independent review + fixes (applied)
An independent read-only review found NO correctness bugs (verified AIME math, Max empty-slice safety,
SumTopN zero-pad/ties/negatives, rejection completeness, transitive gate + cycle protection, serde
round-trip, PyO3 layer). Three polish items applied:
- LifetimeAmbiguousLeaf now carries a String (removed the Box::leak &'static str helper).
- count_over_periods short-circuits before building the per-period matrix (a period count must not fail
  because the inner body is unevaluable).
- Directly-nested reductions (e.g. sum_over_periods(max_over_periods(x)), or a reduction in the `n` arg)
  are now rejected at DENSE COMPILE TIME (DenseCompileError::NestedOverPeriods) with a precise message,
  instead of surfacing an opaque per-period runtime error. Reduction-via-Derived still errors safely at
  runtime (documented; transitive check needs the whole program, not one expr).
Added 2 Rust tests for the nested-reduction rejection (24 lifetime tests total).

## Final gate (all GREEN)
- cargo fmt --check clean; cargo build; cargo test (default) all pass; cargo test --features schema all pass;
  cargo check python-ext; cargo check wasm32-unknown-unknown --no-default-features.
- clippy: 36 lib warnings, identical to origin/main baseline — ZERO introduced by this change; no warnings
  in tests/lifetime.rs or python-ext.
- 24 Rust lifetime tests + 36 Python tests pass against the maturin-built extension.

## DRAFT PR
https://github.com/TheAxiomFoundation/axiom-rules-engine/pull/68
(draft, base main, head cross-period-reduction, title "Cross-period reductions: lifetime execution surface (#67)").
Do NOT merge.

## Concerns
- Reference-period choice for non-reduction scalars (parameters/derived) inside the outer formula:
  chose LAST supplied period. Documented; flag for review. For the AIME acceptance formula the outer
  formula only combines the reduction with a literal (420), so this choice is not exercised there.
- (Superseded for INPUTS by the amendment below.) Lifetime execution previously rejected ALL
  period-ambiguous bare leaves outside reductions. Bare inputs are now bound when provably invariant;
  the reference-period-for-parameters convention above is unchanged.

## Amendment — runtime-verified period-invariant binding for inputs (this session)

### Why
42 USC 415(b)'s benefit-computation-year count is a derived scalar built from per-person-constant
inputs (year the worker attains 21 / 62) and must feed `sum_top_n_over_periods` as its `n`. Under the
original design that `n` — a bare input reached through a derived chain, sitting OUTSIDE every reduction
— hit the blanket `LifetimeAmbiguousLeaf` rejection, so real 415(b) logic could not run end to end.
Empirically confirmed failing (scratchpad/check_derived_n_lifetime.py errored on `year_attained_62`).

### What changed (dense.rs lifetime paths only; no schema change; per-period execute/execute_f64 unchanged)
- `LifetimeExecutor::eval_scalar` `Input` / `InputOrElse` arms no longer error. They call the new
  `eval_period_invariant_input(expr, name)`, which evaluates the SAME bare-input node under every
  period's `DenseExecutor` (one column per period, positionally aligned) and checks PER ROW that the
  value is identical across ALL supplied periods.
  - All rows invariant → bind period 0's column (dtype preserved); the caller consumes it exactly like
    any other column, so a value in scalar OR in the `n` position, reached directly OR through a derived
    chain (Sub/Max/… compose row-wise), all work.
  - Any row varies → new `EvalError::LifetimePeriodVaryingInput`, naming the input and the first two
    differing period labels (`start..end`) and values. Truly period-varying inputs still fail loudly.
- New free helpers in dense.rs: `first_differing_row` (exact per-row equality; numeric variants compared
  by Decimal-promoted value so `1985` == `1985.0`; non-numeric mismatch or non-finite → treated as
  differing = safe error), `period_label`, `dense_value_label`.
- New `EvalError::LifetimePeriodVaryingInput` variant in engine.rs. `LifetimeAmbiguousLeaf` still used
  for the genuinely period-specific leaves (PeriodStart/End, Date*, Count/SumRelated).
- Parameters keep their existing last-supplied-period behavior; the amendment touches inputs only.

### Tests
- tests/lifetime.rs (+4, 28 total): (a) per-person-constant input binds outside a reduction;
  (b) per-row-varying but per-period-constant input binds per row (rows 100 vs 500);
  (c) period-VARYING input errors, message names the input; (d) the derived-n-from-constant-inputs
  chain end to end (two workers, n = 36 and 31; earnings_total [3_456_000, 1_922_000]; aime [8000, 5166]).
- python/tests/test_dense_lifetime.py (+2): two-worker derived-n case (mirrors the scratchpad script) and
  a period-varying-input error case naming the input.

### Acceptance-script arithmetic note (IMPORTANT — flagged for review)
scratchpad/check_derived_n_lifetime.py as delivered expected `aime[1] = 5167` for worker B, with a
comment `floor(5167.20)`. That is wrong: worker B has total 1_922_000, n = 31, so
`aime = floor(1_922_000 / (12*31 = 372)) = floor(5166.666…) = 5166`. 5167 is the round-HALF-UP value,
not the `floor` the module (and 42 USC 415(b)(2)(A) / 20 CFR 404.211(d), which round DOWN) use — and no
integer months divisor can even produce 5167 for this total (floor jumps 5180 at d=371 to 5166 at d=372,
skipping 5167). Independently re-derived. The engine is statutorily correct at 5166; the fixture's one
expected constant was corrected 5167 → 5166 (worker A's 8000 is unaffected: floor and round agree there).
The binding mechanism the script exercises — two workers with DIFFERENT derived n binding correctly — is
proven by `earnings_total = [3_456_000, 1_922_000]`, which matches exactly.

### Final gate (all GREEN, this amendment)
- cargo fmt --check clean; cargo build; cargo test (default) 0 failed; cargo test --features schema
  0 failed (schema golden-file guard passes — no artifact-schema change); cargo check python-ext;
  cargo check wasm32-unknown-unknown --no-default-features.
- clippy --all-targets: actual lint findings BYTE-IDENTICAL to the pre-amendment branch tip (37 finding
  lines; 35 lib + 35 lib-test as before) — ZERO new warnings; none reference the new code.
- 28 Rust lifetime tests + 38 Python tests pass against the maturin-rebuilt extension; the corrected
  scratchpad acceptance script passes (earnings_total [3456000, 1922000], aime [8000, 5166]).
