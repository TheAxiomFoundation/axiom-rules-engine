# Three-valued values

Status: design, approved after four rounds of independent review. Tracking
issue: [#215](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/215).
Section 14 records what has landed as the work proceeds.

## 1. Purpose

A request can opt in to three-valued evaluation. Under it, a fact the dataset
does not supply makes the affected result **indeterminate**, and the result
names the fact. It never becomes a smaller count, a smaller sum or a denial.

Requests that do not opt in are evaluated exactly as today, byte for byte.

This is the general form of #215 and the fix for the #118 class. It contains
nothing specific to unit derivation, recursion or pair facts.

## 2. What main does today

Checked against main at 5d63e8f3.

- Nothing originates `JudgmentOutcome::Undetermined`. It only propagates
  through `and`, `or` and `not` (`src/engine.rs` 1455–1523, `src/bulk.rs`
  1160–1220, `src/dense.rs` 3020–3063).
- A missing input is `EvalError::MissingInput` (`engine.rs` 1685) unless the
  expression is `input_or_else`, which returns its declared default
  (`engine.rs` 1128–1133; fast does the same in its own columns, `bulk.rs`
  1031–1033). RuleSpec formulas lower to plain `input` (`src/formula.rs`
  1412); an `input_or_else` comes only from a program or artifact built some
  other way.
- An input is resolved per **cell**: (slot, entity, period). Among the records
  whose interval contains the period, the one with the latest start wins
  (`engine.rs` 1679–1706). Two such records with the same start and different
  values are `AmbiguousInput`.
- `and` and `or` short-circuit left to right, so `missing and false` is an
  error and `false and missing` is `not_holds`. The same holds for any
  failing item: `(1 / 0 > 0) and false` is an error and `false and (1 / 0 >
  0)` is `not_holds`. The first pair was run on a release build of main.
- Every consumer that selects or filters on a judgment treats undetermined as
  not-holds: `if` (`engine.rs` 1344–1346, `bulk.rs` 924, `dense.rs` 3457 and
  4323), `where` clauses (`engine.rs` 1625–1627), the derived-relation filter
  (`engine.rs` 1883) and dense's boolean related predicates (`dense.rs` 3694,
  4796). `docs/execution-semantics.md` states it as the reference rule.
- `ScalarValue` has no unknown variant (`src/model.rs` 183–189) and
  `OutputValue::Scalar` cannot say "indeterminate" (`src/api.rs` 133–141).
- Every relation is closed-world: an absent tuple is false (`engine.rs`
  1920–1938). A dataset cannot say whether a relation's tuples are complete.
- Explain unions tuples supplied under a derived relation's own name with the
  members its predicate selects (`engine.rs` 1907–1911).
- An aggregation resolves all of a relation's members first, testing a derived
  relation's predicate on every candidate in sorted order (`engine.rs`
  1562–1568, 1862–1901), and only then walks them, evaluating the `where`
  clause and the value per member (`engine.rs` 1574–1599).
- Fast never reports an evaluation error. A live row that fails, or that
  reaches a construct fast does not implement, sends the request to explain
  (`bulk.rs` 333–358). Fast runs every relation aggregation row by row on an
  embedded explain interpreter (`bulk.rs` 1108–1125).
- Dense sums related rows in batch order (`dense.rs` 3653), explain in sorted
  id order, and `execute_f64` computes in floating point (`dense.rs`
  733–743).
- The request and dataset types do not deny unknown fields (`api.rs` 16–48,
  `src/spec.rs` 384–390), so an engine that predates a new field ignores it,
  whatever the field holds.

## 3. Scope

In scope: origination of unknown from missing inputs and from relations not
declared complete; strong-Kleene judgments; indeterminate scalars and
aggregates; three-valued relation membership; an indeterminate output form;
agreement of explain and fast; a named refusal from dense.

Out of scope:

- `kind: unit`, recursion, endpoint roles and pair facts.
- Conflicting input records. They stay `AmbiguousInput`.
- Exact determination by case analysis over the unknown facts (section 12).
- Value intervals. A scalar is a value or indeterminate; `0 * unknown` and
  `max(0, unknown)` are indeterminate.
- Completeness scoped to one entity's list. The declaration is a list of
  objects so that a scope can be added later without a format change.
- Unifying relation aliases at compile time (the other half of #118; #226).
- Three-valued evaluation in dense. Dense refuses the mode.

## 4. Opting in

Two declarations, both carried by the request.

**The switch.** `missing_facts` on `ExecutionRequest` and
`CompiledExecutionRequest`:

- absent, or `"error"`: today's behaviour. A missing input fails the
  evaluation that reads it and an absent tuple is false.
- `"indeterminate"`: the semantics in section 5.
- any other value: the request is refused. The key is reserved from now on.

**Completeness.** `relation_completeness` on the dataset:

```json
"relation_completeness": [{"relation": "member_of_household"}]
```

Each entry says every tuple of that relation is supplied, for every period the
request queries. `relation` resolves like a relation record's `name`.

Under `"indeterminate"`, a base relation is **complete** when an entry names
it and **open** otherwise. Without the switch the field is not read at all: it
is carried as raw JSON and decoded only under `"indeterminate"`, so a request
that does not opt in gets today's response whatever this field holds, a
malformed value included.

Under `"indeterminate"` the request is refused, by name, when:

- an entry names a relation the program does not declare, or a derived
  relation (its completeness follows from its source);
- a tuple is supplied under a derived relation's own name;
- a tuple's length is not its relation's arity;
- `relation_binding` is `lenient`. Lenient binding keeps tuples in the wrong
  orientation, which lookups then miss; together with a completeness
  declaration that would be a silent shrink;
- a tuple's slot holds an entity whose known kind is not the slot's declared
  kind (the check strict binding makes today).

These checks live in one validation function that takes the dataset and the
kind evidence: the input records, and (entity id, entity kind) pairs for what
will be queried. It runs once, on the dataset as supplied:

- request binding calls it with the request's queries as evidence, as strict
  binding does today;
- the engine's public opt-in constructor calls it with the pairs its caller
  passes, so a Rust caller that builds a `DataSet` directly gets the same
  check on the same evidence.

The interpreters that the request path builds afterwards, including the one
fast embeds over its single-period copy of the dataset, are constructed
without re-validating. Validating a reduced copy would be a different check:
dropping another period's records can change the kind evidence for an id.

**Program checks.** Under `"indeterminate"` the program is also checked, for
defects that today surface only if evaluation reaches them and that no
dataset can change. A request is checked for all of its outputs before either
execution path evaluates anything; a direct call on the engine is checked for
the output it asks for. For every rule, parameter and relation the output can
reach by reference, using the formula each rule has for the query period:

- every referenced rule, parameter and relation exists, and every unit a rule
  declares exists;
- a scalar reference names a rule that is scalar for the period, and a
  judgment reference one that is a judgment;
- relation slots are within the relation's arity;
- `relation_member` appears only in a derived relation's own predicate
  expression. The relation context does not carry into a rule that predicate
  references, or into a `count`/`sum` `where` clause or summed value, as in
  evaluation today, so a `relation_member` there is a defect;
- `max` and `min` have an operand, and there is no over-periods reduction.

The walk follows what evaluation can execute. It does not enter the patterns
of a `no_match`, which are only labels for its error message and are never
evaluated: `if true then 7 else no_match(0, [max()])` is not refused.

A rule with no formula for the period is not a defect: it fails if evaluation
reaches it, as today.

All of the above are refusals made before evaluation starts. They are not
evaluation failures, and nothing in section 5 can catch one.

## 5. Semantics under `"indeterminate"`

Explain is the reference, as in `docs/execution-semantics.md`. This section
extends that document's "Reference semantics". Everything it says still holds
unless restated here.

### 5.1 Values and needs

A judgment is `holds`, `not_holds` or **unknown**. A scalar is a value or
**indeterminate**. Unknown and indeterminate each carry a non-empty set of
**needs**. A need is one of:

- an input cell: slot name, entity id, period;
- a base relation's completeness: relation name, and the entity whose list
  was read.

The set is ordered and deduplicated, so it does not depend on evaluation or
dataset order.

An evaluation can also **fail**, with an evaluation error, as today. Section
5.9 says where a failure is caught.

Needs are the facts the evaluation reached and could not read. They are not a
transitive closure: the facts behind an `if` whose condition is unknown are
not listed until the condition is known.

### 5.2 Facts and completions

A dataset **resolves** a cell when some record covers it; the resolved value
is the latest-starting covering record's (section 2). A cell is **missing**
when no record covers it. The facts a dataset supplies are its resolved cells,
the tuples of its relations, and its completeness declarations. A record that
a later-starting record overrides for some period is not a fact for that
period.

A **completion** of a dataset D is a dataset D* such that:

- every cell D resolves, D* resolves to the same value;
- every tuple of D is a tuple of D*, and D* adds tuples only to open
  relations;
- D* declares complete every relation D does;
- D* is a dataset the engine accepts for the request under the mode: it
  passes every check of section 4 and has no ambiguous input.

D is then D* **with facts withdrawn**. Deleting a record is a withdrawal only
when every cell that stays resolved keeps its value. Deleting the newer of two
overlapping spells is not: it changes the facts for the periods the older
spell then covers.

A completion is **total** for a request when evaluating the request on it
under `"indeterminate"` reads no missing cell and no open relation.

The **result** of a dataset, unless said otherwise, is its result under
`"indeterminate"`. Section 6 relates it to the result under `"error"`.

### 5.3 Inputs

`input(x)` and `input_or_else(x, d)` for a missing cell are both
indeterminate, with the need for that cell. A declared default applies only
without the switch. The caller who opts in is saying that absence means
nothing; a default that turned absence into `false` would be a denial by
absence.

### 5.4 Judgments

- A comparison evaluates both operands. If either is indeterminate it is
  unknown, with the union of their needs.
- `not` of unknown is unknown, with the same needs.
- `and` evaluates its items left to right and stops at the first `not_holds`,
  as today. It does not stop at an unknown item, and it does not stop at an
  item that fails. When it has evaluated its items, its result is the first
  that applies:
  1. `not_holds`, if an item is `not_holds`;
  2. unknown, with the union of needs, if an item is unknown;
  3. a failure, if an item failed: of the failed items' errors, the least
     under a total order that does not depend on position: first the error's
     message, then its variant, then each of its fields in a fixed order;
  4. `holds`.

  `or` is the mirror image, with `holds` decisive.

So `and`/`or` are strong Kleene, and a decisive item decides them whatever
its siblings do. `unknown and false`, `false and unknown`,
`(1 / 0 > 0) and false` and `false and (1 / 0 > 0)` are all `not_holds`.
Nothing about the result depends on operand order: not the value, not the
needs, and not which failure is reported, because rule 3 picks by the error's
content and not by its position. The order compares every field, not only the
message, because two errors can render the same message and still differ: a
`no_match` error gains the name of its rule as it leaves that rule, and only
an error whose rule field is still empty gains it. Comparing the stored fields
makes the choice the same in every order before that happens.

### 5.5 Scalars

Every operator other than `if` evaluates its operands and performs its
conversions and arithmetic in exactly the reference order, with one change: it
does not stop at an indeterminate operand. Once an operand is indeterminate,
the operator still evaluates each remaining operand and applies each remaining
type conversion, failing if they fail, but performs no further arithmetic. The
result is then indeterminate, with the union of needs.

Arithmetic on the known operands before the first indeterminate one happens as
today, so an overflow there fails before later operands are evaluated:
`Decimal::MAX + 1 + 1/0` is an overflow, as on main, with or without a missing
operand after it.

Division evaluates its divisor first. A known zero divisor fails before the
dividend is read. With an indeterminate divisor the dividend is still
evaluated.

A parameter lookup with an indeterminate key is indeterminate. `no_match` with
an indeterminate subject is indeterminate; its patterns are diagnostics and
are never evaluated.

Under the mode, a decimal written into an error message is normalized, as
response values already are. Two records with the same start whose decimals
are equal at different scales (`1.0`, `1.00`) are both admitted today, and
the one read depends on record order; outputs hide that, and normalized
messages hide it too. Without the switch, messages are unchanged.

### 5.6 Conditionals

`if c then a else b` with `c` unknown is indeterminate, with `c`'s needs.
Neither branch is evaluated. It never takes `else`. `match` lowers to `if` and
inherits this: an indeterminate subject never reaches `no_match`.

### 5.7 Relations and membership

The members of a relation for an entity are the distinct ids in the related
slot of its tuples for that entity, as today. Under `"indeterminate"` a
relation, for an entity, has a finite list of **listed** ids, each with a
**membership** that is `in`, unknown (with needs) or failed (with the
failure), and it is **open** (with a need) or not.

- A base relation lists the supplied tuples' ids, all `in`. It is open iff it
  is not declared complete; the need names the relation and the entity.
- A derived relation takes its candidates from its source's listed ids. Its
  predicate is evaluated for every candidate, in sorted id order, before
  anything aggregates over it. A candidate's membership is the `and` of 5.4
  applied to its membership in the source and the predicate's outcome: it is
  dropped from the list if the predicate does not hold; otherwise it is
  unknown if either is unknown; otherwise failed if either failed; otherwise
  `in`. The relation is open iff its source is open, with the source's need.
  So the need always names a base relation, which is the only kind a caller
  can declare complete.
- An id that is not listed is `out` if the relation is not open. If it is
  open, the id's membership is unknown, with the completeness need. The
  predicate is not evaluated for an unlisted id.
- `relation_member` holds for an `in` id, does not hold for an `out` id, is
  unknown for an unknown one, and fails with a failed id's failure.

In every completion, an `in` id is a member and an `out` id is not.

### 5.8 Aggregates

`count`/`sum` first resolve the memberships (5.7), then walk the listed ids in
sorted order.

For each listed id, its **selection** is the `and` of 5.4 applied to its
membership and its `where` clause (absent: holds). The `where` clause is
evaluated for every listed id. Its **contribution** follows from the
selection:

| Selection | Count | Sum |
|---|---|---|
| `not_holds` | nothing | nothing; the value is not evaluated |
| `holds` | one | the value: a number is added; indeterminate is **uncertain**; a failure fails the aggregate |
| unknown | uncertain | the value is evaluated, catching a failure. Known zero: nothing. Anything else: uncertain |
| failed | fails the aggregate | the value is evaluated, catching a failure. Known zero: nothing. A non-zero number or a failure: fails the aggregate. Indeterminate: uncertain |

The result is the first that applies:

1. a failure, if a contribution fails the aggregate: the first such failure;
2. indeterminate, if a contribution is uncertain or the relation is open,
   with the needs of every uncertain contribution and, if open, the
   completeness need;
3. the count or the sum.

That is the case analysis behind "indeterminate unless the lower and upper
bounds agree". A sum's member whose value is zero adds zero whether or not it
is selected, so it never makes the sum uncertain.

The accumulator follows the same discipline as 5.5. Contributions are added in
sorted id order with checked arithmetic, as today, until the first listed id
whose contribution is not exact, and not at all when the relation is open:
from that point no completion is bound to add the same numbers in the same
order, so an overflow there is not certain. The walk still evaluates every
remaining listed id.

Every aggregate over an open relation is indeterminate or fails, because its
membership has no upper bound. This is conservative for a sum whose value is
zero for every possible member.

### 5.9 Failures

A failure is an evaluation error, raised as today. Under `"indeterminate"` it
is **caught** at exactly these positions:

- each item of an `and`/`or` (5.4);
- a derived relation's predicate, for each candidate (5.7);
- a `where` clause, for each listed id (5.8);
- a sum's value, for a listed id whose selection is unknown or failed (5.8).

At each, the failure becomes one of the four outcomes that the rule there
combines. If the combination yields a failure, it is raised again from that
position. Everywhere else a failure propagates as today: through every scalar
operator, comparison, `not`, `if` and rule reference. An operand's failure in a
scalar operator propagates even when an earlier operand was indeterminate.

Catching is local. A rule's body is evaluated the same way wherever it is
referenced from, so the result cached for a rule is intrinsic to the rule,
entity and period. A rule that fails caches no value, but its failure is
remembered for the rest of the drive, as 5.10 already does for a deferred
rule, and raised at each reference; each reference catches it, or not, where
it sits.

A deferral is not a failure and is never caught. Neither is a refusal: the
checks of section 4 run before evaluation, and the temporary refusals of an
unfinished construct (section 10) propagate through every catching position.
So none of the structural defects section 4 lists can reach a catching
position. Other failures can, including ones no data can change, such as
`1 / 0` written into a formula; they are caught like any other.

### 5.10 Rules, caching and deferral

A rule evaluation that ends unknown or indeterminate is cached with its needs,
like a value. A reference to it returns the cached result.

Deferral (`src/depth.rs`) changes no result under either mode. Two things keep
that true:

- The loop state a deferral saves (`FoldResume`, `MembersResume`) carries the
  memberships, the uncertain contributions, their needs, and any failure held
  so far.
- When a deferred rule's own evaluation fails, the driver cannot tell whether
  the reference to it sits at a catching position. Under `"indeterminate"` it
  records the failure against the deferred request itself (the rule, entity,
  period, and whether a scalar or a judgment was asked for), for the rest of
  the drive, and retries the interrupted evaluation. The retry reaches the
  reference again, finds the record instead of deferring, and fails there,
  where 5.9 applies. Without the switch the driver returns the failure at
  once, as today.

**Trace.** A reference records its dependency edge when the referenced
evaluation returns a value or unknown, not before it starts. Without the
switch this is unobservable: an evaluation that fails emits no trace, and a
parent's edges keep their order because a parent records nothing while a
callee runs. Under `"indeterminate"`, a rule whose failure was caught has no
node and no edge points to it; the referencing node lists it as a dependency
that was not evaluated, with the reason `failed`.

## 6. Invariants

Results are under `"indeterminate"` unless said otherwise. D is a dataset the
mode admits, and "completion" means a completion of D (5.2), total or not.

1. **Soundness.** If D's result is a value `v`, every completion's result is
   `v`.
2. **Failures are certain.** If D's evaluation fails, every completion's
   evaluation fails.
3. **No silent shrink.** If D′ is a completion of D and D′'s result is a value
   `v`, D's result is `v` or indeterminate. It is never another value and
   never a failure. In particular it is never a smaller count, a smaller sum
   or `not_holds` in place of `holds`.
4. **Monotone refinement.** A value stays that value as missing facts are
   supplied. (This is invariant 1 for completions that are not total.)
5. **Opt-out identity.** With `missing_facts` absent or `"error"`, every
   response is byte-identical to the response from the engine before this
   work, whatever else the request carries.
6. **Agreement with today's results.** Take a request both modes admit, on a
   dataset where the evaluation under `"indeterminate"` reads no missing cell
   and no open relation. If `"error"` returns a response, `"indeterminate"`
   returns the same outputs and the same trace. If `"error"` fails,
   `"indeterminate"` fails, possibly with a different error, or returns
   values where 5.4, 5.7 or 5.8 absorbs the failure.
7. **Needs are real.** Every need names a cell D does not resolve or a base
   relation D does not declare complete, and an indeterminate result has at
   least one. So supplying every listed need either decides the result or
   exposes needs that were not listed, and repeating this terminates.
8. **Order invariance.** Permuting the items of an `and`/`or`, or permuting
   the input and relation records, changes nothing in the result: not a
   value, not indeterminacy, not a need, and not the failure reported.
9. **Path agreement.** Explain and fast return the same values,
   indeterminacy, needs and failures. Dense refuses the mode by name.
10. **Deferral transparency.** Every result, need, failure and trace is the
    same at every deferral threshold.

### How these relate to the invariants on #215

Soundness, no silent shrink, monotone refinement and order invariance hold as
the issue states them, with "the completed result" read as the result of the
completed dataset under the same mode. Four things make that possible and are
worth the owner's attention.

- **A decisive item absorbs a failing sibling, in either order.** Today
  `false and x` is `not_holds` when `x` fails, and `x and false` is an error.
  Under the mode both are `not_holds`. Without this, soundness could only say
  "or an error": inputs are untyped, so every missing input has completions
  in which the operand that reads it fails, and `unknown and false` would be
  `not_holds` while such a completion was an error. The same rule is what
  makes `and`/`or` independent of operand order without exception.
- **So the mode can return a value where today's engine fails**, on a complete
  dataset (invariant 6). It never returns a different value from today's, and
  where today's engine succeeds the response is the same. The cases are:
  - a decisive `and`/`or` item beside a failing one, in either order;
  - a listed id whose predicate or `where` clause fails but whose `where`
    clause or predicate does not hold: it contributes nothing;
  - in a sum, a listed id whose selection fails but whose value is zero: it
    contributes nothing;
  - a derived relation one of whose candidates has a failing predicate:
    today resolving the relation fails for every use, and under the mode
    only uses that depend on that candidate fail.

  When both engines fail, the error reported can differ, because 5.4 picks
  among failed items by text and today's engine reports the leftmost. The
  alternative, keeping today's failures exactly under the mode, would need
  soundness weakened to "the value, or an error" and order invariance
  limited to items that do not fail.
- **A program defect that no data can change is refused up front** (section
  4), where today it is reported only if evaluation reaches it. Without this
  a defect behind a decisive operand would disappear.
- **"Removing a fact" means withdrawing it** (5.2). Deleting the newer of two
  overlapping spells reveals the older value. That is a change of fact under
  the input-precedence rule, which this work does not alter.

## 7. Why it is sound

Programs are acyclic (`compile::validate_dependency_graph`), so there is one
stratum per rule and derived relation in dependency order. Give each
expression four possible outcomes under the mode: a value, unknown, a failure,
and, for a membership, `in`/`out`/unknown/failed. The argument is one
structural induction, in dependency order, of a single claim. For a completion
D* of D:

> If an expression's outcome on D is a value, its outcome on D* is that value.
> If it is a failure on D, it is a failure on D*. If it is unknown on D, it
> may be anything on D*.

Invariants 1, 2 and 4 are this claim at the root. Invariant 3 follows: D′ is a
completion of D with value `v`, so D cannot have another value (1) or a
failure (2).

The cases:

- **The input leaf.** A resolved cell has the same value in D*, by the
  definition of a completion. A missing cell is unknown.
- **Scalar operators and comparisons.** A value needs every operand to be a
  value and the arithmetic to succeed; both are the same on D*. A failure
  comes from an operand that failed, or from a conversion or arithmetic step
  on known operands. On D* the earlier operands are the same values or, where
  they were indeterminate, anything; either an earlier one now fails or the
  same step fails.
- **`and`.** `not_holds` needs an item that is `not_holds`; it still is, and
  rule 1 of 5.4 applies whatever the others became. `holds` needs every item
  to hold; they still do. A failure needs no `not_holds` item and no unknown
  item, so every item held or failed; each is the same kind on D*, so the
  result is a failure.
- **Negation.** `not relation_member(R)` holds only for an `out` id and fails
  to hold only for an `in` id. `not` maps unknown to unknown; no rule reads
  "not known to be a member" as "not a member".
- **Membership.** A base id is `in` or `out` only through a supplied tuple or
  a completeness declaration, both of which D* keeps. A derived candidate's
  membership is an `and`, covered above.
- **Aggregates.** A value needs no uncertain contribution and a relation that
  is not open, so no id can be listed on D* that was not listed on D. Each
  listed id on D contributes exactly or nothing. One that contributes a
  number has a membership, a `where` clause and a value that are all values,
  so it contributes the same number on D*. One that contributes nothing
  either has a selection that does not hold, which is the same on D*, or is
  a sum's zero-valued member whose selection was unknown or failed; on D* it
  may be selected, not selected or dropped from the list, and it adds zero or
  nothing. So the accumulator adds the same non-zero numbers in the same
  order, and adding a zero changes neither the total nor whether a step
  overflows. A failure of the aggregate comes from a contribution whose
  selection and value are not unknown, which is the same on D*, or from the
  accumulator before the first inexact contribution of a relation that is not
  open, where the same argument applies.
- **Non-monotone uses of an aggregate.** `len(R) == 1` is decided only when
  the count is a value. There is no polarity to track.

**Evaluating twice is not the method.** For `(p and not q) or (q and not p)`
with `p` and `q` unknown, unknown-as-false and unknown-as-true both give
`not_holds`, and both mixed valuations give `holds`. The rules of 5.4 give
unknown.

**Invariant 6.** Under the mode, evaluation follows today's order exactly
until it meets something missing, something open, or a failure. A dataset on
which `"error"` succeeds and the mode reads nothing missing or open has none
of the three, so the two evaluations are the same step for step.

## 8. Execution paths

**Explain** implements section 5.

**Fast** stays a determinate-only evaluator. It carries the mode and the
completeness declarations: into its embedded interpreter, into the
single-period dataset it evaluates, and into its own input columns. Fast
declines, and the request goes to explain, when:

- a live row reads a missing cell in fast's own columns, with or without a
  declared default;
- an embedded aggregation returns indeterminate or fails;
- a live row fails for any other reason (as today).

An embedded aggregation may meet unknowns and failures and absorb them; what
it returns is then the value explain computes at that node. So if fast
completes, every node it evaluated has explain's value and no live row failed,
and the two agree by the existing contract. `metadata.actual_mode` and
`fallback_reason` report which ran.

**Dense** refuses the mode. It gains entry points that take options
(`execute_with_options` and the like); under `"indeterminate"` each returns a
named error (`EvalError::IndeterminateUnsupported`) before evaluating
anything. The existing entry points are unchanged and mean `"error"`. Dense
cannot support the mode without work this series does not do: its batches have
no entity ids, so it cannot name a need or sum in explain's order, and its
`f64` path is inexact by contract.

## 9. Wire format and versioning

Requests: `missing_facts` (section 4), omitted from serialization when absent.
Dataset: `relation_completeness`, omitted when absent.

Responses, only under `"indeterminate"`:

- `metadata.missing_facts: "indeterminate"`.
- A scalar output that is indeterminate is
  `{"kind": "indeterminate", "name", "id", "dtype", "unit", "needs": [...]}`.
- A judgment output keeps `"kind": "judgment"`; with
  `"outcome": "undetermined"` it also carries a non-empty `"needs"`.
- Trace nodes gain the same two forms, with the fields a scalar or judgment
  node has today except `value`, `rounding` and `pre_rounding_value`.
  `not_evaluated_dependencies` gains two reasons: `condition_undetermined`,
  for both branches of an `if` whose condition is unknown, and `failed`
  (5.10).

A need is `{"kind": "input", "name", "request_names", "entity_id", "period"}`
or `{"kind": "relation_completeness", "relation", "entity_id"}`.
`request_names` are the names the program's input catalog accepts for the
slot.

Versioning:

- **The mode is a named value of an enum.** A later semantics (for example
  exact case analysis) is a new value, never a change in what
  `"indeterminate"` means.
- **A new schema family.** `execution-request.v2`,
  `compiled-execution-request.v2` and `execution-response.v2` are generated
  from the types and describe the new fields and forms. The three v1 schemas
  are frozen at their current bytes. Every request that does not opt in, and
  its response, validates against v1; a test checks that over generated
  requests. The v1 request schemas allow unknown properties, so they do not
  reject an opt-in request; an indeterminate output is what v1 cannot
  describe.
- **The response echoes the mode.** An engine that predates the field ignores
  it and returns closed-world results with no echo. A caller that opts in
  must check `metadata.missing_facts == "indeterminate"`; its absence means
  the engine did not honour the request. The Python client makes that check
  itself and raises when it fails.
- `capabilities` lists `missing_facts_indeterminate`.
- **The compiled artifact format does not change.** The mode is a property of
  a request, and no artifact field depends on it.

## 10. Plan

Five pull requests. Each has a test that fails before it, the full suite
green with and without the `unit-derivation` feature, and a corpus impact
number (section 11).

The request field arrives in the fourth. Until then the mode is an engine
option (`EvaluationOptions`, hidden from the docs and marked incomplete),
exercised by tests through the Rust API. Every intermediate state is sound:
under the option, a construct that a given PR has not yet lifted refuses with
a named error. In particular every relation read refuses until the PR that
gives it open-world semantics. Such a refusal is not an evaluation failure:
no catching position absorbs it, so the evaluation that reaches it is
refused. Nothing ever coerces unknown to not-holds, zero or an absent member.

1. **Origination and judgments.** `MissingFacts`, needs, the lifted result
   type, the engine option and its checked constructor, the program checks of
   section 4, origination at input reads, scalar operators (5.5),
   comparisons, `and`/`or`/`not` (5.4), caching of unknown rules, the
   deferred-failure record, trace edges recorded on return. `if` on an
   unknown condition refuses. Every relation read refuses.
2. **Scalars and aggregates.** `if`; base-relation completeness on `DataSet`
   and the completeness need; the dataset validation of section 4 that
   applies to base relations (arity, kinds, completeness names), with the
   unchecked internal constructor;
   `count`/`sum` over base relations (5.8); the fold's deferral state. Derived
   relations and `relation_member` still refuse.
3. **Relations.** Memberships as one payload type that the resolver returns
   and aggregates consume; derived membership; `relation_member`; refusal of
   tuples under a derived relation's name. Nothing refuses any more.
4. **Outputs and schema.** The request field, the dataset field, binding,
   output and trace forms, the metadata echo, `capabilities`, the v2 schema
   family, the Python models and the client's echo check, fast's mode and
   declines, `docs/execution-semantics.md` (including a correction of its
   aggregation staging to match section 2), a `DECISIONS.md` entry.
5. **Dense.** The options entry points and the named refusal, through PyO3
   and the Python wrapper.

Internally a result is a value or unknown-with-needs, carried as a value
through the interpreter (`Determination<T>`). Interrupts stay reserved for
failures, refusals and deferral; a refusal is an interrupt of its own, so no
catching position can mistake it for a failure. The same code path serves
both modes, which is what makes invariant 6 hold by construction.

**Overlap with #211.** That PR memoizes a relation's members per (relation,
entity, period) and replays their trace effects, and it predates main's
deferral driver. PR 3 here changes what the resolver returns (memberships in
place of a list of ids) and how aggregates consume it, and leaves the
resolver's traversal and keying alone. Whichever lands second still has real
work to reconcile the two; this series only keeps that work to the payload.

## 11. Tests

**Oracle.** A small evaluator written in the test crate, over the test
generator's own expression type. It shares no code with the engine, and the
generator's expressions are lowered separately to engine programs. It has two
parts.

- *The reference.* It implements section 5 in full, for any dataset: values,
  unknown with needs, failures, memberships, the contribution table, the
  accumulator rule. The engine under the mode must return exactly what it
  returns: the same value, the same need set, or a failure with the same
  message. This is the conformance test. It is what catches an
  implementation that is safe but says "indeterminate" where section 5 gives
  a value.
- *Today's rules*, for datasets with nothing missing, checked against the
  engine without the switch. That tests the oracle's reading of the existing
  semantics.

The invariants of section 6 are then checked on the reference itself as well
as on the engine. For a dataset with missing facts the test enumerates every
total completion within a stated finite universe: each missing cell over a
small domain per dtype that includes the literals the program compares
against, zero, and one value of the wrong type; each open relation over every
subset of a small set of extra candidate ids. A failure of soundness there is
a defect in section 5, not in the code.

**Properties** (proptest, fixed seed, shrinking), one per invariant in section
6: soundness and failures-are-certain against the oracle; no silent shrink by
withdrawing facts from a complete dataset, including withdrawals that would
disturb another period's cell and so are not withdrawals; monotone refinement
and needs-are-real by supplying facts; invariant 6 on complete datasets,
including trace; order invariance by permuting items and records; deferral
transparency at thresholds `usize::MAX`, 1, 2 and the default. Datasets
include overlapping spells and queries over overlapping periods. Sessions
reuse one engine across several queries and outputs, so shared caches are
exercised.

**Path properties** extend `tests/execution_mode_parity.rs` and its generator
with the mode on: explain against fast, comparing values, needs, and the
failure's variant and message. Trace closure (every dependency key resolves)
is asserted on every explain response.

**Non-vacuity guards.** Minimum shares of cases that are determinate with
something missing; indeterminate; decided by absorbing an unknown item;
decided by absorbing a failing item; failing in a rule that was deferred from
a catching position; answered by fast without fallback.

**Differential, feature-gated.** With `unit-derivation` on, the `in` and
unknown members of a single derived relation over a complete source equal the
stage-2 `lift_derived_relation` on random candidate sets.

**Table cases.** One named case per cell of the contribution table in 5.8,
for count and for sum, and per operand position of each scalar operator with
an indeterminate and with a failing operand.

**Named cases.** The #118 reproducer shape (tuples under an alias no rule
reads: indeterminate, naming the relation the rule reads); one member's fact
missing (count indeterminate, the other members' rules determined);
`missing and false` and `(1 / 0 > 0) and false`, both ways; two failing
items in both orders, reporting the same failure, including two errors with
the same message that differ in a stored field; equal-start records with
decimals `1.0` and `1.00`, in both record orders, giving the same message; a
wrong-kind reference beside a right-kind reference to the same rule, refused
by the program checks; an undeclared unit behind `or true`, refused;
`if true then 7 else no_match(0, [max()])`, not refused; `(R and Q) or not R`; an `if` that would have taken `else`; a complete empty relation (count 0)
against an open empty one (indeterminate); an open sum of `MAX`, `MAX` that
must not overflow, and a sum with a possible cancelling member between two
large ones; `input_or_else` on a missing cell in explain and in fast; a
reversed tuple for an entity with no input records; a short tuple; an id with
input records of two kinds in different periods, admitted alike by explain
and by fast; a request
without the switch carrying `"relation_completeness": 7`, byte-identical to
main; an old-engine response without the echo, rejected by the Python client;
dense refusing the mode.

**Unchecked internals.** The failure record's key (5.10) can only be told
apart from a coarser key by a wrong-kind reference, which the program checks
refuse. A unit test inside the engine therefore builds an opted-in engine
without the program checks and shows that a wrong-kind reference that defers
and fails does not fail a right-kind reference to the same rule.

**Opt-out identity.** The existing suite passes unchanged. Default requests
and responses serialize without the new keys and validate against the frozen
v1 schemas.

**Corpus impact** is measured per PR with a harness outside this repository:
a shim stands in for the engine binary under the encoder's companion-test
runner, sends each invocation to a base binary and a head binary, and compares
exit code, stdout and stderr bytes, in explain and in fast. It reports
companion files run, engine invocations, compile and load failures on each
side, and differing responses, with pinned corpus and base commits. The
expected number of differing responses is 0, over a non-zero number of
invocations.

## 12. Known conservatism

Bounds can report indeterminate where every completion agrees.

- `(R and Q) or not R` with `R` unknown and `Q` holding. Every completion
  holds; the result is unknown. Rewriting the formula as `Q or not R` decides
  it. The design review on #134 reads the boarder rule in 7 CFR
  273.1(b)(3)(ii) as having this shape.
- `0 * x`, `max(0, x)` with `x ≤ 0` in every completion, a sum whose uncertain
  members cancel, a sum over an open relation whose value is always zero.
- Repeated unknowns: `x == x`.

Exact case analysis over a small set of unknown yes/no facts is a later
refinement and would be a new mode value. Decision d1265 (2026-10-10, on #134)
amended the unit-derivation contract's tier A entry 1 to this standard: sound,
conservative bounds, with exact case analysis possibly added later for small
cases.

## 13. Choices made here that the issue does not dictate

1. **A decisive item absorbs a failing sibling under the mode** (5.4, 5.8,
   section 6). This is the choice that lets the issue's invariants hold as
   written. Its cost is invariant 6: the mode can answer where today's engine
   fails.
2. **The request field lands with the outputs** (PR 4), not with origination
   (PR 1). Until the response can carry needs, a request-level opt-in could
   only turn every indeterminate output into an error.
3. **A declared default does not apply under the mode** (5.3). Keeping it
   would let a withdrawn fact become a denial.
4. **Fast is determinate-only and dense refuses.** Fast's agreement comes from
   declining at the first missing fact or failure a live row reaches.
   Carrying validity masks through both columnar evaluators is the
   alternative; the feasibility map sizes it at 750–1,500 lines for no result
   a caller can observe except speed on indeterminate rows.
5. **`"indeterminate"` requires strict relation binding.**
6. **A v2 schema family**, with v1 frozen, in addition to the echo.
7. **Completeness is per relation only.** Scoping it to one entity's list is
   left for later.

## 14. Status

| Step | State |
|---|---|
| Design note | Approved by the fourth independent review (2026-10-10), after rejections in rounds 1–3. Reviews and responses are kept outside the repository. |
| PR 1, origination and judgments | In review: the explain interpreter (`src/engine.rs`) and the model types (`src/model.rs`), behind the hidden engine option; tested by `tests/three_valued.rs` |
| PR 2, scalars and aggregates | Not started |
| PR 3, relations | Not started |
| PR 4, outputs and schema | Not started |
| PR 5, dense | Not started |
