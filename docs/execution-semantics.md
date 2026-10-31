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
- **Aggregations are lazy per related entity.** `count`, `sum`, `min`/`max`
  and `only` over a relation visit the distinct related entity ids in sorted
  order. For each one, a derived relation's membership predicate is evaluated
  first, then the `where` clause, then (for `sum`, `min` and `max`) the
  member's value. A stage is evaluated only for entities that passed the
  previous one, and the first failing member fails the aggregation. `only`
  evaluates the `where` clause for every member and then reads the value of
  the one member it selected, so it reads no value when it selected none or
  several.
- **`min`, `max` and `only` over a relation never default.** `min`/`max`
  compare numbers numerically and dates chronologically; a Boolean or text
  member value is a type error, as is a date among numbers. With no selected
  member they fail with `EmptyRelatedAggregate`. `only` fails with
  `MissingRelation` when no member is selected and `AmbiguousRelation`
  (with the count) when several are, as `only_related` does. Each error names the relation, the
  entity and the period. `if len(r) > 0: min(r.f) else: ...` and
  `if len(r) == 1: only(r.f) else: ...` are total, because conditionals are
  lazy.
- **Derived rules are values.** A rule referenced from a formula is evaluated
  when evaluation reaches the reference, and not otherwise, for the entity the
  reference is evaluated for: in a rule body, the entity that rule is being
  evaluated for; in an aggregation's `where` clause or value, each
  related entity, whatever entity the referenced rule declares (a `Household`
  rule in a `where` clause over household members is evaluated for each
  member); and inside a derived relation's own predicate, the current entity
  when the rule's declared entity is the relation's current slot entity, and
  the related entity otherwise. A read through a term of that predicate
  overrides this choice: `anchor.rule` is evaluated for the current entity and
  `candidate.rule` for the related one, and `anchor.input` and
  `candidate.input` read the input of that entity where a bare input reads
  the related one's. The referenced rule's body is then evaluated for that
  entity, with no relation context. Its declared output rounding
  applies before any dependent reads the value, including when dense inlines
  the rule into an aggregation or predicate.
- **Predicates resolve parameters as rule bodies do.** Table parameters,
  scalar parameters and entity-free rules with literal values read the
  module's parameter values. A bare Boolean parameter can act as a judgment;
  a non-Boolean parameter used as a judgment fails when reached. A
  `parameter` record with an entity or a computed formula lowers to a rule;
  predicates read it with the routing and qualified-read kind checks of
  other rules. Parameters have no entity id, so a qualified parameter read
  is refused; read it unqualified instead (`relation_role_entity_mismatch`).
- **A value keeps the kind its expression computes.** A rule's declared
  `dtype` is reported beside its value but never converts it: `count` yields
  an integer even in a rule declared `decimal`, and `sum`, arithmetic,
  `max`/`min` (scalar or over numbers of related entities) and `ceil`/`floor`
  yield decimals even in a rule declared `integer`. `min`/`max` over dates
  yield a date, and `only` yields its member's value with its own kind. An
  `if` yields the kind of the branch each row takes. Output rounding applies
  to decimal values only. Equal decimals that differ in scale (`2.5` and
  `2.50`) or in the sign of zero are one value; a response writes every
  decimal normalized, and `min`/`max` over related entities keep the larger
  scale, then the non-negative zero, so no evaluator's member order shows in
  the value it computes.
- **`relation_member` needs a derived relation.** It tests whether the current
  and related entity of the derived relation whose predicate is being
  evaluated appear together, in the slots it names, in the named relation.
  That predicate is the only place that supplies them. Anywhere else, including an aggregation's `where` clause
  and a rule the predicate references, `relation_member` is a type error of
  the evaluation that reaches it. The same holds for a read through a term,
  which compilation and relation typing already refuse outside a predicate
  (`relation_role_outside_scope`); an evaluator that meets one fails the
  evaluation that reaches it.
- **Relation atoms fix slot order.** `R(t1, ..., tn)` in a derived relation's
  predicate tests its term ids in `R`'s declared slots. A data atom requires
  one supplied tuple matching every id whose interval contains the period.
  A binary derived atom resolves members for the term in its current slot
  and tests the term in its related slot. Computing another anchor's members
  can add substantial work in nested predicates
  ([relation-roles.md](relation-roles.md) §5; #211). `t1 == t2` and `t1 != t2`
  compare ids of the same kind and always determine a result. Compilation
  refuses these constructs outside the predicate's scope, including
  aggregation `where` clauses (`relation_role_outside_scope`).
- **Supplied derived tuples depend on the program's constructs.** Programs
  using qualified reads, relation atoms, term comparisons, `exists` or path
  sources refuse tuples
  supplied under any derived relation's name
  (`supplied_tuples_for_derived_relation`), and their members resolver ignores
  those tuples even through `Engine::new`. Programs using none retain the
  union of supplied tuples and filtered source members; a parameter read in a
  predicate, which raw `ProgramSpec` could always express, is not one of these
  constructs. An atom, `exists` or path source also refuses lenient request binding
  (`lenient_binding_with_relation_roles`).
- **`exists` evaluates a bounded disjunction.** In a derived relation's
  predicate, `exists(R(anchor, p), body)` binds `p` to each distinct domain id
  in sorted order. A data generator takes ids from tuples matching every
  other argument and containing the whole period; it reads the tuple index
  at the lowest-numbered bound slot, or all that relation's tuples for a
  unary generator with no bound slot. A derived generator resolves members
  for its current-slot term and can bind only its related slot. The body
  holds for some id or the quantifier does not hold, including for an empty
  domain. Evaluation stops at the first body that holds; a failure propagates
  in domain order as from `or`. `not exists(G, not body)` expresses "for all"
  and holds over an empty domain. Variables nest lexically and carry their
  generator slot's kind. Unqualified reads retain anchor/candidate routing;
  a variable is read through its term explicitly. Rules and aggregation
  clauses or values have no variable bindings
  (`relation_role_outside_scope`).
- **Path sources join before testing membership.** Starting with the anchor
  id, each binary step replaces the current ids with those reached from its
  `from_slot` to its `to_slot`. A data step includes only tuples whose
  interval contains the whole period. A derived step resolves forward
  members for each current id. Each frontier is distinct and sorted; only
  the final frontier becomes candidates, so intermediate ids are dropped.
  The outer predicate then runs once per candidate in sorted order, as for
  a single source. An empty frontier gives no members. A failure at any
  reached id fails the anchor's members, even for candidates that id does
  not reach. Kind checks require
  the first step to start at the anchor's kind, every junction to match, and
  the final step to end at the candidate's kind
  (`tests/relation_paths_surface.rs`, `tests/relation_paths.rs`).
- **An error fails the (query, output) it occurs in.** Division by zero, a
  missing input, a missing parameter cell, a non-integral parameter key and a
  type error are all errors of the evaluation that reaches them. Nothing that
  evaluation does not reach can fail it.
- **Once a request passes validation and dataset binding, it fails if and
  only if any of its (query, output) evaluations fails,** and the reported
  error is the first failure in query order, then output order. Validation
  (the program's dependency graph, assessment dates, input spells) and strict
  relation binding can refuse a request before any evaluation runs.

The consequence callers rely on: a guard such as

```yaml
formula: |
  if household_size == 0: 0
  else: income / household_size
```

is total. So are `household_size > 0 and income / household_size < limit`, an
input that is read only on one branch, and a derived rule that is referenced
only from a branch no row selects.

### Relation predicate compilation

Terms carry their relation slot's kind; a bound variable carries its
generator slot's kind. Compilation checks a qualified rule read against that
kind and checks each atom argument against its declared relation slot.
Input reads remain untyped. A term can appear in a qualified read, relation
atom or equality/inequality comparison, but using a term as a value is
refused (`relation_term_used_as_value`). A bound variable used outside its
`exists` body is refused (`relation_variable_out_of_scope`). A new variable
cannot hide a visible rule, parameter or relation, or rebind an enclosing
variable (`relation_variable_name_conflict`).

RuleSpec checks declaration names across the import-merged module closure
before lowering any formula. It refuses a parameter, rule or relation named
`anchor`, `candidate`, `period_start`, `period_end` or a formula keyword
(`reserved_declaration_name`), and a name declared as two of a parameter,
rule and relation (`declaration_name_clash`). A `parameter` record counts as
the kind it lowers to. Outside predicates, undeclared `anchor` and
`candidate` retain their ordinary input reads. A bare `period_start` or
`period_end` reads the built-in date in scalar position and refuses in
judgment position.

These contracts cover parameter predicates in
`tests/relation_roles.rs`, declaration and import clashes in
`tests/declaration_names.rs`, atom kinds in `tests/relation_atoms.rs`, and
variable scope and names in `tests/relation_exists_lowering.rs`.

### Guarded household fixture

The synthetic 7 CFR 273.1 fixture computes the people within two symmetric
ties of the applicant. When no co-resident has an exception flag and no tie
leaves that set, the set is the exact household. Otherwise it reads supplied
`snap_household_unit` ids and includes co-residents with the applicant's id;
a missing id fails naming that input. This never silently returns a wrong
household under section 8's two preconditions: a complete residence roster
and correct supplied ids wherever read. The guard depends on the applicant,
so in a chain a–b–c–d, b's household needs no ids while a's does. The test
`applicants_a_and_b_on_a_three_tie_chain` in `tests/snap_household.rs` checks
both cases; `exception_for_older_roommate` checks the exception branch.
The separate `snap_participating_member` relation filters (b)(7) ineligible
members from composition without changing `snap_household_member`.

### Pins

A rule pin (`CompiledExecutionRequest.pins`) rewrites the pinned rule to
`if 0 == 0 then <literal> else <original formula>`. Under the reference
semantics the original formula is never evaluated, so a pinned rule never needs
the inputs its original formula reads. This holds in every mode.

For reads through terms, explain records evaluated dependencies at the term's
id. A skipped rule read through `anchor` or `candidate` is also recorded at
that id. A skipped read through an enclosing `exists` variable is recorded
at its bound id when that enclosing quantifier is being evaluated. A wholly
skipped `exists` does not enumerate its domain: it records the body's
anchor/candidate dependencies once and omits reads through its own variable,
which has no bound id. Inside a running outer quantifier, a skipped nested
`exists` still records reads through the outer variable. When an evaluated
`exists` stops early, it records the body's skipped dependencies for each
remaining domain id with that variable bound to that id. An evaluated
`exists` with an empty domain evaluates no body and records no body
dependencies. Thus expansion as a disjunction preserves values and failures,
while the wholly skipped and empty
domain cases have distinct trace rules.

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

A compiled artifact's `metadata.fast_path` is a static, advisory scan of the
constructs fast declines or cannot answer (period bounds, date arithmetic,
over-periods reductions), listed per rule. Execution never reads it, so it
cannot change a result. It scans every branch, so a listed construct that only
dead branches reach still runs on the fast path. It does not look inside
relation aggregations (`count`, `sum`, `min`/`max`, `only`), which fast
evaluates row by row on the explain interpreter; one that yields a date sends
the request to explain. Date values are not listed either (date literals, date inputs
and date-valued parameters), although fast declines them when a live row
reaches them.

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
   `relation_member` in an aggregation's `where` clause, in any rule such a
   clause or an aggregated value reads, or in a related entity's rule that a
   predicate reads, fails the rows that reach it, as in explain. One in a
   rule dense evaluates on the root row (a root entity's rule outside related
   expressions, or a current entity's rule that a predicate reads) makes the
   dense compiler reject the program, even where explain would never reach
   it.

Dense reduces `min`/`max` over each row's related rows, walking them as it
walks a `sum`, and gathers the one related row an `only` selects; both report
explain's errors for the rows that reach them. Because dense rows carry no
entity ids, `EmptyRelatedAggregate` names the root entity kind where explain
names the entity id, as a missing root input does, and `MissingRelation` and
`AmbiguousRelation` name the row index, as `only_related` does. Each names
the relation as the formula does, a derived relation included, although its
batch is its base relation's. An aggregation nested inside a
related expression (a related rule that itself aggregates) makes the dense
compiler reject the program, whichever aggregate it is.

A derived relation's path source or predicate using a qualified read
(`anchor.name`, `candidate.name`), relation atom, term comparison or `exists`
makes the dense compiler
reject any root entity whose rules reach that relation with
`DenseCompileError::Unsupported`, naming the construct and #218; a root that
reaches no such relation compiles. Explain and fast evaluate it. Parameter
reads in a predicate compile in dense as in a rule body.

Dense evaluates the rules a `where` clause, an aggregated value or a related rule's
body reads on the related rows, for the related entity as explain does. Only a
derived relation's own predicate reads a rule of the relation's current slot
entity on the root row. In that predicate, a rule of an entity that is neither
slot entity of a relation that declares them (other than an entity-free
`Scalar` rule) makes the dense compiler reject the program; explain evaluates
it for the related entity.

A rule before its commencement date fails the rows that reach it, as the
explain path reports `MissingDerivedFormulaVersion` for that rule; a rule no
live row reaches cannot fail the batch. This also applies to a derived value
or judgment inlined into a related aggregate or a derived relation's filter.
Dense supports a single unbounded formula version for those dependencies;
multiple versions or an `effective_to` bound make compilation fail with
`DenseCompileError::Unsupported`. For `min`, `max` and `only`, a refusal of
the value the aggregate reads or of its `where` clause names the function and
the relation. A refusal from the relation itself, such as a derived relation's
filter, names only the construct that cannot be compiled: every reader of the
relation shares that filter, so its refusal reads the same whichever reader
reaches it first. Relation batches are positional, so
when several related rows of one entity fail, dense reports the first in batch
order where explain reports the first in sorted id order.

Lifetime execution (`execute_lifetime*`) has no explain counterpart. It follows
the same laziness: a reduction, a period-invariant input check and a top-N
count see only the rows that reach them, and a row stops at its first failing
period. Inside a reduction, each reached rule's commencement is checked against
that reduction period's start. The `n` argument of `sum_top_n_over_periods` is
also evaluated and checked in every period before its invariance check. In the
lifetime context, requested outputs and derived rules referenced outside a
reduction are checked against the reference period's start: the chronologically
last supplied period. A row that reaches a
rule before its commencement fails with `MissingDerivedFormulaVersion`, naming
the rule and the applicable period start, just as per-period dense and explain
do for that period. Untaken branches do not fail. Lifetime uses the same dense
compiler as per-period execution: derived rules with an `effective_to` bound or
multiple formula versions are refused with `DenseCompileError::Unsupported`.

In an output that reduces over periods, a relation aggregation outside
the reduction has no period to read and fails with `LifetimeAmbiguousLeaf`,
which names it.

The dense compiler also declines one shape by depth. An aggregation inlines
the rules its `where` clause and value read, and a current-entity rule
read there inlines the same way, so the compiled expression nests as deep as
the chain of rules it inlines. Past `dense::MAX_INLINE_DEPTH` (512) levels the
compiler returns `DenseCompileError::Unsupported` and the generic API
evaluates the program. The bound depends on the program alone. It is a
compile-time decline, like the other unsupported shapes, not an evaluation
error. Inlined levels do not defer, so the bound, not the deferral
threshold, caps the stack they use: a chain of rules at the bound compiles
and evaluates on a 1 MiB thread in a release build but not on 512 KiB, and a
debug build needs far more.

## Deep programs

A program's rules can reference each other in arbitrarily long chains, and
every mode answers them. Explain, fast and dense evaluate a rule by recursing
into the rules its formula references, and the dense compiler compiles one the
same way. Each counts its nesting levels. At a reference to a rule it has not
evaluated yet, once that count reaches a threshold, it does not recurse.
Instead it returns to a driver loop and names the rule. The driver evaluates
that rule from level zero, then retries the evaluation it interrupted, which
now finds the rule cached. A long chain of rules therefore runs in segments,
each on a stack no deeper than the threshold plus one rule's own expression
nesting, whatever the chain's length: a 20,000-rule chain answers on a 1 MiB
thread in every mode (`src/depth.rs`). Fast's relation aggregations, which
run on the explain engine fast embeds, and dense's lifetime reductions, which
drive each period's executor, nest one bounded drive inside another and can
use twice that. Expressions dense inlines and derived-relation resolution
have separate bounds (see above and below).

Typing and binding a chain of derived relations is linear in its length and
never recurses once per link. A derived relation that declares no slot kinds
has its source's, so RuleSpec lowering (`relation_direction`), typing
(`relation_typing::check_program`, which compile, artifact load and
`execute_request` run) and dataset binding each resolve every relation's kinds
once per program, through one walker (`relation_typing::follow_chains`). A
chain is followed only until it meets a relation already resolved. Filters of
filters resolve the same way, and the used relations whose derivations typing
checks form a worklist. A 50,000-link chain types and binds well within a
second on a 2 MiB thread (`tests/relation_typing_chains.rs`), where looking
each relation's kinds up down its own chain overflowed the stack.

Deferral changes no result for an acyclic program, which every checked program
is. Evaluating a rule for an entity and period is deterministic, so a retry
reaches the same references in the same order. Every value, value kind, error
and explain trace is what uninterrupted recursion would produce. Only work
changes: a retry walks again, over cached values, what the interrupted
evaluation did before the deferral. The threshold is 128 levels in release
builds, so an evaluation that never nests that deep never retries, and 8 in
debug builds, whose frames are about thirty times larger; deferring early also
exercises deferral throughout the debug test suite.

Derived-relation resolution has its own native nesting limit
(`src/engine.rs`, `RELATION_NESTING_LIMIT`; #211). A predicate can ask for the
members of another derived relation. Once resolution reaches the limit, an
unresolved request stops the current resolution and names the members it
needs. The driver resolves those members, caches the result and its trace
records, then retries the stopped resolution. That retry reads completed work
from the caches and resumes its predicate at the candidate that stopped it.
A loop walks source chains, so their length does not consume this native
nesting budget. The limit is 32 in release builds and 8 in debug
builds, whose larger frames need an earlier stop: a counted-predicate chain
needed 2048 KiB at depth 32, but 576 KiB at depth 8 on aarch64-apple-darwin
(rustc 1.94.1, measured to 16 KiB). This limit is independent of the
rule-deferral threshold, and compiled artifact bytes do not depend on it.

For every acyclic program, a relation nesting limit of at least one preserves
the values, errors and explain traces of unlimited native resolution. The
`engine::relation_resolution_tests` property tests
`suspended_resolution_matches_native_resolution` and
`iterative_resolution_matches_recursive_semantics_on_small_dags` compare
limited resolution with native resolution and an independent recursive
oracle. `tests/relation_chains.rs` checks a long counted-predicate chain in
`a_chain_through_count_related_predicates_is_linear_not_exponential`, and
`count_related_chain_stays_within_a_bounded_stack` pins its native stack budget.
`tests/relation_atoms.rs` checks values, traces and rule-deferral depths across
a long atom chain in
`long_atom_dependency_chains_preserve_values_traces_and_suspend_depths`.

An `exists` keeps its sorted domain, the next id to evaluate and the outcome
accumulated so far across a suspension. Its progress key includes the node's
address, the anchor id, the candidate id, every enclosing variable's id in
order from innermost to outermost, the period, the parent rule evaluation and
the enclosing member resolution that owns its trace effects. This separates, for example,
`required_combination(p, candidate)` resolved for several values of `p` in
one parent rule evaluation. A retry resumes the interrupted body at the same
domain id; bodies before it retain their completed work.

A path's members resolution saves the step index, sorted current frontier,
the next id in that frontier, ids reached so far and captured trace effects.
A derived step can stop while resolving an intermediate id's members. The
retry reuses completed lookups, resumes that id, and advances to the next
step only after the frontier is complete. Once candidates are generated,
the ordinary members predicate loop saves its own progress. Both relation
nesting limits and rule deferral preserve results and traces
(`engine::relation_resolution_tests::suspended_resolution_matches_native_resolution`).

What a retry walks again is bounded by the program, not the data. Explain
records the progress of every loop over data that a deferral can interrupt:
a `count` or `sum` over related entities, an `exists` domain, and the resolution
of a relation's members, which generates a path's candidates and tests a derived
relation's predicate on each candidate. The
retry of the same rule evaluation resumes an interrupted loop at the member
that deferred, and reuses a loop that had finished, so a household whose
members each defer is not walked again for every member. A relation's own
tuples are read once, when its members are resolved. Fast evaluates each
row's relation aggregation as a drive of its own on the explain engine, so
the same holds within that row; when fast itself retries a column, it
evaluates the row's aggregation again, once per retry. Dense inlines the
rules an aggregation reads, so their members never defer. The rest is the
rules still open on the interrupted path and the operands they had evaluated.
So work grows linearly with a chain's length, and a rule with `k` operands
that each defer walks its earlier operands `k` times.
`tests/deferral_transparency.rs` measures work in nodes visited, counting
each expression level, each member a `count` or `sum` walks and each tuple
read, which does not depend on the machine: a 20,000-rule chain costs four
times a 5,000-rule one in every mode, and 1,000 members that each defer, or
a derived relation whose predicate defers for each of 256 members, cost 1.6
times what recursion costs (each bounded there at three times, whatever the
member count).

Rule deferral happens only at rule references, so one rule's own expression
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
Which rule of the cycle the error names can depend on the threshold.

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
