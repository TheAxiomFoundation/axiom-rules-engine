# Execution semantics

The engine has three evaluators for the same compiled program:

- **explain** (`src/engine.rs`): a recursive interpreter that evaluates one
  (entity, period, output) at a time and records a trace;
- **fast** (`src/bulk.rs`): a columnar evaluator over every query row of a
  request, used when `ExecutionRequest.mode` is `fast`;
- **dense** (`src/dense.rs`): a columnar evaluator over caller-supplied typed
  columns, reached through the Rust `DenseCompiledProgram` API and the PyO3
  extension.

This document fixes what each of them must compute. Explain is the reference.
Fast and dense are optimisations of it; a divergence is a bug in the optimised
path unless this document names it as a representation limit.

## Reference semantics

A request is a list of queries; each query names an entity, a period and a list
of outputs. For each query in order, and each requested output in order, the
output's formula is evaluated for that entity and period as follows.

- **Conditionals are lazy.** `if c then a else b` evaluates `c` first. When `c`
  holds, only `a` is evaluated; when `c` does not hold or is undetermined, only
  `b` is evaluated. `match` lowers to nested `if` and inherits this.
- **Boolean operators short-circuit.** `and(x1, ..., xn)` evaluates its items
  left to right and stops at the first `not_holds`; it is `undetermined` if an
  evaluated item was undetermined and none failed, else `holds`. `or` stops at
  the first `holds` and is symmetric.
- **Every other node evaluates all of its operands, left to right,** with one
  exception: division evaluates its divisor first, fails on a zero divisor, and
  only then evaluates the dividend.
- **Aggregations are lazy per related entity.** `count`/`sum` over a relation
  visit the distinct related entity ids in sorted order. For each one, a derived
  relation's membership predicate is evaluated first, then the `where` clause,
  then (for `sum`) the summed value. A stage is evaluated only for entities that
  passed the previous one.
- **Derived rules are values.** A rule referenced from a formula is evaluated
  for the referencing entity (or, inside a relation predicate, the entity its
  declared entity kind selects) when evaluation reaches the reference, and not
  otherwise.
- **An error fails the (query, output) it occurs in.** Division by zero, a
  missing input, a missing parameter cell, a non-integral parameter key and a
  type error are all errors of the evaluation that reaches them. Nothing that
  evaluation does not reach can fail it.
- **A request fails if and only if any of its (query, output) evaluations
  fails,** and the reported error is the first failure in query order, then
  output order.

The consequence callers rely on: a guard such as

```yaml
formula: |
  if household_size == 0: 0
  else: income / household_size
```

is total. So are `household_size > 0 and income / household_size < limit`, an
input that is read only on one branch, and a derived rule that is referenced
only from a branch no row selects.

### Pins

A rule pin (`CompiledExecutionRequest.pins`) rewrites the pinned rule to
`if 0 == 0 then <literal> else <original formula>`. Under the reference
semantics the original formula is never evaluated, so a pinned rule never needs
the inputs its original formula reads. This holds in every mode.

## Fast mode contract

For every request, fast returns exactly what explain returns, except that
`trace` is empty and `metadata` reports `actual_mode: fast`. This covers every
output value, including its kind: a rule whose selected branch yields an
integer returns `{"kind": "integer"}` in both modes, even when another row's
branch yields a decimal. It also covers every error: fast fails exactly when
explain fails, with the error explain reports.

Fast may decline a request it does not implement (a construct such as date
arithmetic, queries with different periods, or a parameter or unknown output).
It then runs explain and records the reason in `metadata.fallback_reason`. A
fallback is never an error, and a construct that only a dead branch contains
never forces one. Fast never reports an evaluation error of its own: when a
live row fails, fast hands the request to explain, which reports its first
error, so a failing request's error is explain's by construction.

## Dense contract

A dense batch is a set of rows, each standing for one entity, with typed input
columns and positional relation batches. For every row and every requested
output, the dense column holds the value explain would return for a query of
that row, and a dense call fails exactly when explain would fail for some row.
When several rows fail, the error is the first failing row's, then the first
failing output's in the requested order.

Three representation limits apply. They are properties of typed columns, not
of evaluation order. A column's dtype never depends on which rows are live:
dense still types a branch no row selects, and a parameter lookup's dtype is
that of the selected table, not of the keys a batch happens to look up.

1. **One dtype per column.** An integer and a decimal (or `f64`) branch combine
   into the executor's numeric dtype, so dense returns `1` as a decimal where
   explain returns the integer `1`; values are equal. If the live rows of one
   `if` select branches whose dtypes cannot combine (for example, text on some
   rows and a number on others), dense rejects the batch with
   `dense if() branches must have the same dtype`. A branch that no row selects
   never constrains the dtype.
2. **`f64` mode is inexact.** `execute_f64` and `execute_lifetime_f64` trade
   exactness for throughput; they are not for published amounts.
3. **Absent columns are missing for every row.** A root or related input
   column the caller does not supply is a missing input on every row. It is an
   error only if a live row reads it, exactly as a missing input record is in
   explain.

A rule before its commencement date fails the rows that reach it, as the
explain path reports `MissingDerivedFormulaVersion` for that rule; a rule no
live row reaches cannot fail the batch. Relation batches are positional, so
when several related rows of one entity fail, dense reports the first in batch
order where explain reports the first in sorted id order.

Lifetime execution (`execute_lifetime*`) has no explain counterpart. It follows
the same laziness: a reduction, a period-invariant input check and a top-N
count see only the rows that reach them, and a row stops at its first failing
period.

## How the columnar evaluators implement this

Fast and dense evaluate one expression node for many rows at once, so they
carry the reference control flow as data:

- **Active-row masks.** Every node is evaluated under a mask of the rows whose
  reference evaluation reaches it. An `if` splits its mask by the condition;
  `and`/`or` shrink the mask as rows are decided; an aggregation masks its
  related rows by the previous stage. A node under an empty mask does no
  per-row work and cannot fail.
- **Per-row errors.** An error on a live row is recorded against that row
  instead of aborting the batch, and the row leaves the mask for the rest of
  the enclosing expression, exactly as the reference evaluation of that row
  stops. Dense then reports the first recorded error in row order, then output
  order; fast hands any request with a recorded error to explain.
- **Incremental derived caches.** A derived rule's column is computed for the
  rows that have asked for it so far and extended when a later reference asks
  for more rows. A rule reached only through dead branches is never computed
  for those rows, and its errors never surface.
- **Relation aggregations** in fast run row by row on the explain
  interpreter itself, so related-id resolution, derived-relation filtering and
  `where` laziness match by construction. Dense masks related rows by stage:
  derived-relation filters, then the `where` clause, then the summed value.
- **Declining is structural.** Fast declines (and falls back to explain) only
  when a live row reaches a construct fast does not implement.

`tests/execution_mode_parity.rs` checks this contract differentially: it
generates programs with dead branches, zero divisors, missing inputs, mixed
integer/decimal branches, pins and relations, runs them through explain, fast
and dense, and shrinks any disagreement to a minimal counterexample. It also
checks that a batch equals its concatenated single-query runs and that
permuting the queries permutes the results. `tests/lazy_branches.rs` holds
named cases for the `rulespec-us` idioms that failed before masking.
