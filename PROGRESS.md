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

## Next
1. python-ext/src/lib.rs + python/dense.py: execute_lifetime_f64 (parallel to execute_f64).
2. Rust unit tests (tests/lifetime.rs): each builtin (top-n ties, n>period_count, n as expr, negatives),
   alignment/validation errors, per-period-context rejection, count, max, sum, f64==decimal.
3. Python acceptance test (test_dense_lifetime.py): AIME 40-yr worker, hand-derived expected in comment.
4. Build wheel locally (maturin) to run Python tests; final fmt/clippy/test/schema/python-ext; draft PR.

## Concerns
- Reference-period choice for non-reduction scalars (parameters/derived) inside the outer formula:
  chose LAST supplied period. Documented; flag for review. For the AIME acceptance formula the outer
  formula only combines the reduction with a literal (420), so this choice is not exercised there.
- Lifetime execution intentionally rejects period-ambiguous bare leaves outside reductions rather than
  silently picking a period — the safe faithful reading of the brief.
