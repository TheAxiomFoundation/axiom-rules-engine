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
  `b` is evaluated. `match` lowers to nested `if` and inherits this. A
  `match` without `_` ends its chain in `no_match`, which fails the row with
  the subject's value when no arm covers it; a row whose conditions never
  reach the `match` never reaches that error.
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
  when evaluation reaches the reference, and not otherwise, for the entity the
  reference is evaluated for: in a rule body, the entity that rule is being
  evaluated for; in a `count`/`sum` `where` clause or summed value, each
  related entity, whatever entity the referenced rule declares (a `Household`
  rule in a `where` clause over household members is evaluated for each
  member); and inside a derived relation's own predicate, the current entity
  when the rule's declared entity is the relation's current slot entity, and
  the related entity otherwise. The referenced rule's body is then evaluated
  for that entity, with no relation context. Its declared output rounding
  applies before any dependent reads the value, including when dense inlines
  the rule into an aggregation or predicate.
- **A value keeps the kind its expression computes.** A rule's declared
  `dtype` is reported beside its value but never converts it: `count` yields
  an integer even in a rule declared `decimal`, and `sum`, arithmetic,
  `max`/`min` and `ceil`/`floor` yield decimals even in a rule declared
  `integer`. An `if` yields the kind of the branch each row takes. Output
  rounding applies to decimal values only.
- **`relation_member` needs a derived relation.** It tests whether the current
  and related entity of the derived relation whose predicate is being
  evaluated appear together, in the slots it names, in the named relation.
  That predicate is the only place that supplies them. Anywhere else, including a `count`/`sum` `where` clause
  and a rule the predicate references, `relation_member` is a type error of
  the evaluation that reaches it.
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

Four representation limits apply. They are properties of typed columns and
positional relation batches, not of evaluation order. A column's dtype never
depends on which rows are live: dense still types a branch no row selects, and
a parameter lookup's dtype is that of the selected table, not of the keys a
batch happens to look up.

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
4. **Relation batches carry base relations only.** A derived relation is its
   base relation's batch filtered by its predicate, and related rows carry no
   entity ids. Tuples a dataset supplies under a derived relation's own name,
   which explain adds to the filtered ones, have no place in a batch. Inside a
   derived relation's predicate, dense accepts a `relation_member` only when
   it tests the relation's source, or a source further up its chain, with the
   slots the chain reads it with: such a test holds for every filtered tuple.
   The dense compiler rejects a predicate with any other membership test. A
   `relation_member` in a `count`/`sum` `where` clause, in any rule such a
   clause or a summed value reads, or in a related entity's rule that a
   predicate reads, fails the rows that reach it, as in explain. One in a
   rule dense evaluates on the root row (a root entity's rule outside related
   expressions, or a current entity's rule that a predicate reads) makes the
   dense compiler reject the program, even where explain would never reach
   it.

Dense evaluates the rules a `where` clause, a summed value or a related rule's
body reads on the related rows, for the related entity as explain does. Only a
derived relation's own predicate reads a rule of the relation's current slot
entity on the root row. In that predicate, a rule of an entity that is neither
slot entity of a relation that declares them (other than an entity-free
`Scalar` rule) makes the dense compiler reject the program; explain evaluates
it for the related entity.

A rule before its commencement date fails the rows that reach it, as the
explain path reports `MissingDerivedFormulaVersion` for that rule; a rule no
live row reaches cannot fail the batch. Relation batches are positional, so
when several related rows of one entity fail, dense reports the first in batch
order where explain reports the first in sorted id order.

Lifetime execution (`execute_lifetime*`) has no explain counterpart. It follows
the same laziness: a reduction, a period-invariant input check and a top-N
count see only the rows that reach them, and a row stops at its first failing
period.

The dense compiler also declines one shape by depth. A `count`/`sum` inlines
the rules its `where` clause and summed value read, and a current-entity rule
read there inlines the same way, so the compiled expression nests as deep as
the chain of rules it inlines. Past `dense::MAX_INLINE_DEPTH` (512) levels the
compiler returns `DenseCompileError::Unsupported` and the generic API
evaluates the program. The bound depends on the program alone. It is a
compile-time decline, like the other unsupported shapes, not an evaluation
error.

## Deep programs

A program's rules can reference each other in arbitrarily long chains, and
every mode answers them. Explain, fast and dense evaluate a rule by recursing
into the rules its formula references, and the dense compiler compiles one the
same way. Each counts its nesting levels. At a reference to a rule it has not
evaluated yet, once that count reaches a threshold, it does not recurse.
Instead it returns to a driver loop and names the rule. The driver evaluates
that rule from level zero, then retries the evaluation it interrupted, which
now finds the rule cached. A long chain therefore runs in segments, each on a
stack no deeper than the threshold plus one rule's own expression nesting,
whatever the chain's length and the host's stack (`src/depth.rs`).

Deferral changes no result. Evaluating a rule for an entity and period is
deterministic, so a retry reaches the same references in the same order. Every
value, value kind, error and explain trace is what uninterrupted recursion
would produce. Only work changes: a retry walks again, over cached values,
what the interrupted evaluation did before the deferral. The threshold is 128
levels in release builds, so an evaluation that never nests that deep never
retries, and 8 in debug builds, whose frames are about thirty times larger;
deferring early also exercises deferral throughout the debug test suite.

What a retry walks again is bounded by the program, not the data. A `count`
or `sum` over related entities that a member's deferral interrupts resumes
at that member, so a household whose members each defer is not walked again
for every member (explain does this; fast evaluates these aggregations with
explain, and dense inlines the rules they read, so their members never
defer). The rest is the rules
still open on the interrupted path and the operands they had evaluated. So
work grows linearly with a chain's length, and a rule with `k` operands that
each defer walks its earlier operands `k` times.
`tests/deferral_transparency.rs` measures work in nodes visited, which does
not depend on the machine: a 20,000-rule chain costs four times a 5,000-rule
one in every mode, and 1,000 members that each defer cost 1.6 times what
recursion costs.

Deferral happens only at rule references, so one rule's own expression
nesting is still recursed through. Inlined dense expressions have no rule
references, which is why dense bounds them (see above). Dense computes a rule
it has never computed even when no row reaches it (to fix the rule's dtype),
so it defers that computation too: a deep chain behind an untaken branch, or
in an empty batch, runs in segments like any other.

A rule that is deferred while an evaluation it transitively started still
waits for it depends on itself. The drivers report that as
`EvalError::DependencyCycle` (or the dense compiler's cyclic-dependency error)
instead of deferring forever. Compiled artifacts and `execute_request` refuse
cyclic programs before evaluating, so only a hand-built `Program` can reach it.

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
  derived-relation filters, then the `where` clause, then the summed value,
  and evaluates the rules each stage reads on the related rows, except the
  current-entity rules a derived relation's own predicate reads, which it
  evaluates on the root row and projects to its related rows.
- **Declining is structural.** Fast declines (and falls back to explain) only
  when a live row reaches a construct fast does not implement.

`tests/evaluation_depth.rs` checks deep programs in every mode, including
chains around explicit thresholds, a 20,000-rule chain on a 1 MiB thread, and
the requests that reproduced #206.
`tests/execution_mode_parity.rs` checks this contract differentially: it
generates programs with dead branches, zero divisors, missing inputs, mixed
integer/decimal branches, pins and relations, runs them through explain, fast
and dense, and shrinks any disagreement to a minimal counterexample. It also
checks that a batch equals its concatenated single-query runs and that
permuting the queries permutes the results, and that every response, trace
and error is identical with deferral at every rule reference and with none.
`tests/lazy_branches.rs` holds
named cases for the `rulespec-us` idioms that failed before masking.
