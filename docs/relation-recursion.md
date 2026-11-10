# Recursive derived relations

Design for #231, step 4 of the unit-derivation plan (decision d1265,
2026-10-10, recorded on #134). Builds on the constructs in
`relation-roles.md` (#214). Not built yet.

## 1. Purpose

Some units are closures. Under 7 CFR 273.1(b)(1), anyone in a required
combination with a household member is a member: a member's spouse, a
member's child under 22, that child's other parent, and so on, to any length.
No fixed number of `exists` steps covers every household.

This note lets a derived relation's predicate refer to the relation itself,
or to other derived relations that refer back to it, at the same anchor or at
another one. The meaning is the least fixed point, computed over the finite
set of ids the data supplies.

## 2. What main does today (0e0cd3d3)

`evaluation_order` (`src/compile.rs`) refuses cycles in two places:

- `validate_relation_derivation_graph` runs right after the check for
  duplicate rule names. It builds a graph of derived relations only. A
  relation's edges go to its source and to every relation its predicate
  reads through `collect_relation_members_from_judgment`. Those are the bare
  membership tests and the relations a `count_related` or `sum_related`
  aggregates, including those inside their `where` clauses. Any cycle is
  refused with `CyclicRelationDependency`, whether or not a rule reads it.
- `reject_relation_routed_cycles` builds one graph of rules and derived
  relations. A rule has edges to the rules it reads and to the relations its
  expressions read through the same collector. A derived relation has edges
  to its predicate's rules, to the relations the collector finds in its
  predicate, and to its source. It refuses with `CyclicDependency`, naming
  every rule on or downstream of a cycle.

Neither collector records the relation of an `only_related` (#261), so that
relation adds no edge to either graph.

Evaluation:

- Members are resolved on demand. Their progress is kept per evaluation step
  in `Engine::members` and cleared at the end of each drive (`Engine::drive`,
  `src/engine.rs`).
- #211 (0c22bea5) keeps each derived relation's members per (relation,
  entity, period) in `Engine::derived_members`, for the interpreter's
  lifetime. An entry holds a resolution's failure as well as its members,
  and the trace records the resolution made, which are replayed on every rule
  that reads it. Resolutions nested past `RELATION_NESTING_LIMIT` (32) levels
  suspend, and an explicit stack resolves what they need. A resolution that
  needs members waiting on itself fails with "derived relation `R` depends on
  its own members", for any cycle the compile checks miss.
- Rule values are cached by (rule, id, period) for the life of the
  interpreter.

## 3. Which recursion is allowed

This work replaces the two graphs of section 2 with one. Its nodes are rules
and derived relations. A rule has an edge to everything it reads. A derived
relation has an edge to its source, to each step of its path, and to every
relation and rule its predicate reads: atoms, `exists` domains, membership
tests, aggregates and rule references. The edges include those #214 adds and
the relation of every aggregate, `only_related` included.

A **recursive group** is a strongly connected component of this graph that
contains a cycle. A group is accepted only when all of these hold:

1. **Only derived relations.** No rule is on a cycle. A rule reads a relation
   only through an aggregate (`len`, `sum`, `count_where`, `sum_where`,
   `only_related`, and the `min`/`max`/`only` of #232). An aggregate is not
   monotone in membership in general: `len(R) == 1` is the counterexample on
   #231, which has no fixed point.
2. **Positive references.** Inside the group, a predicate may refer to a group
   relation only:
   - as an atom, `R(t1, t2)`;
   - as the domain of an `exists`, `exists(R(t, x), body)`;
   - as its source relation or a path step.

   Each such reference must be in a positive position. Polarity is decided on
   the predicate as it is lowered to the model, not as it is written:
   - the predicate itself is positive;
   - an item of `and` or `or`, an `exists` domain and an `exists` body keep
     the polarity of the expression they are in;
   - `not` flips it.

   Every other position is **mixed**, and a group reference there is refused.
   This includes:
   - an item of `exactly_one`. It lowers to an `or` of `and`s in which every
     item also appears under `not` (`JudgmentExprSpec::ExactlyOne`,
     `src/spec.rs`), so `exactly_one(R(anchor, candidate), candidate ==
     anchor)` reads `R` both ways;
   - an `if` condition, and a comparison operand;
   - the `where` clause or value of any aggregate;
   - anything a later construct adds, until this rule gives it a polarity.
3. **Bare membership tests stay outside.** A bare relation name (today's
   `relation_member`) may not name a group relation. Use the atom.

Refusals:
- A cycle through a rule keeps today's `CyclicDependency`.
- A cycle of derived relations that breaks condition 2 or 3 is refused with
  `RecursiveRelationNotMonotone`. Today, such a program that uses only
  constructs main has gets `CyclicRelationDependency`. The diagnostic names
  the relation, the path from the reference back to it, the condition it
  breaks, and the construct that made the position mixed or negative (for
  example "inside `exactly_one`").

These conditions make each predicate monotone in the group's memberships.
Checking each predicate's own references is enough. Every relation on a cycle
is in the group, and no rule is. A path from one group relation back to
another is therefore a chain of references that each predicate makes directly,
and each of those references is positive. A predicate may also read a derived
relation outside the group. That relation does not depend on the group, so it
is fixed while the group is computed, whatever negations its own predicate
holds. (Invariant 6 of `relation-roles.md` makes no claim about such reads;
this argument needs none.)

Relations outside the group may be read in any way, negated or aggregated.
That is stratification: a non-monotone use of a recursive relation belongs to
a later relation or rule, which reads the group's final memberships.

## 4. Meaning

For one period, the members of each group relation, for every anchor, are the
least sets that satisfy every predicate in the group. Candidates come from
sources and paths as in #214. Every id comes from the data, so the sets are
finite.

A reference at another anchor is allowed. In `exists(claims_as_dependent(t,
anchor), medicaid_household(t, candidate))`, the relation is read at anchor
`t`. The fixed point is over (relation, anchor) pairs together, so two
anchors that refer to each other converge like any other cycle. No runtime
cycle error is needed. This differs from #231's first text, which proposed
one.

### Stages

The result is defined by stages over **every** (relation, anchor) pair of the
group in the period, whatever is read and in whatever order. Section 5 says
how evaluation computes only the pairs it needs and gets the same answer.

- **Stage 0.** Every pair has no members.
- **Stage k.** For every pair, the predicate of every candidate that is not a
  stage k − 1 member is evaluated. Every group relation is read as it stood
  at stage k − 1, and so are the candidates a group source or path supplies.
  The stage's admissions are applied together at its end.
- **End.** The first stage that admits nothing anywhere is the last. Its sets
  are the **final sets**.

Each stage reads only the previous stage's sets, so the stages are a function
of the program and the data. They do not depend on the order of records,
tuples, the roster, evaluations within a stage, or which pair is read first.
Every stage but the last admits a member, and there are finitely many (pair,
candidate) combinations, so the stages end. In practice they take the length
of the longest chain.

A member's **stage** is the one that admitted it.

### Two-valued (today's mode)

- A candidate is admitted in a stage when its predicate holds.
- An evaluation that fails admits nothing in that stage. The candidate is
  evaluated again in a later stage if anything it read has changed.
- **Final check.** Every candidate of every pair, member or not, is evaluated
  against the final sets.
- **Reach.** A pair's **reach** is the pair itself and every pair whose
  members a final-check evaluation of a candidate in the reach reads. That
  is, the pairs its final evaluations read, directly or through other pairs.
- **Result.** If a final-check evaluation of a candidate of a pair in the
  reach of P fails, the read of the group at P fails. It fails with the first
  such failure in (relation, anchor, candidate id) order. Otherwise the read
  at P returns P's final set.

The reach depends only on the final sets, so the result does not depend on
which pair is read first. A failure fails the reads of exactly the pairs that
depend on it. Take a source of all pairs over ids A, Z and B, `Link(A, Z)`,
`A.bad` true and `Z.bad` false, and the predicate `candidate == anchor or
exists(Link(anchor, p), R(p, candidate)) or (anchor.bad and 1 / 0 > 0)`.
- The final-check evaluation of candidate B at A fails, so the read at A
  fails.
- Z's final-check evaluations read only Z, so Z's reach is Z alone. The read
  at Z returns `{Z}`, whether or not A was read first.

A failure can disappear as the sets grow. Take `anchor.seed or
exists(Link(anchor, p), R(p, candidate)) or 1 / 0 > 0`, with `A.seed` false,
`Z.seed` true and `Link(A, Z)`. At stage 1, A's evaluation fails, because `R`
at `Z` is still empty, and Z is admitted. At stage 2, A's `exists` holds and
A is admitted. The final check passes.

A failure can also appear as the sets grow. In `(R(anchor, candidate) and 1 /
0 > 0) or candidate == anchor`, the anchor is admitted at stage 1, while
`R(anchor, anchor)` is still false. Against the final sets, the `and` reaches
`1 / 0` and the check fails. That is the failure evaluation would give if the
final sets had been supplied as tuples.

**When the read passes, the result is the least fixed point.** Short-circuit
evaluation of a positive predicate that holds against some sets reads, along
the path that made it hold, atoms that stay true as the sets grow and values
that do not depend on the group. So against larger sets it still holds, unless
it fails. Hence, over the reach of P, when no final-check evaluation in it
fails:
- every member holds against the final sets, and every non-member does not,
  because its last evaluation read them: the final sets are a fixed point
  there;
- for any memberships that satisfy every predicate without a failure, each
  stage's admissions are among them, by induction on the stages: the result
  is the least such set.

### Three-valued (with #215)

Memberships are `in`, unknown (with needs), failed or `out`, and a relation is
open or not for an anchor (#215 section 5.7). Judgments follow #215 section
5.4: `and`/`or` are strong Kleene and catch failures in their items, and a
decisive item decides them whatever its siblings do.

Inside a group the stages read sets, not memberships. A failed candidate is
not admitted, so the group's own predicates read it as not holding. Reads
from outside the group see the result below, where a failed membership fails
an atom (`relation-roles.md` section 4). This keeps the stages monotone. A
failure read as a fourth value inside the group would not be.

**Two passes.** The lower pass is computed first, then the upper pass. Both
use the stages.

- **Lower.**
  - Candidates come from a group source's lower sets, and a path's ids from
    its group steps' lower sets.
  - A group atom holds for a lower member and does not hold otherwise. A
    group `exists` ranges over the lower members, with no extra item.
  - A candidate is admitted when its predicate holds.
  - An evaluation that fails admits nothing.
- **Upper.**
  - Candidates come from a group source's upper sets, and a path's ids from
    its group steps' upper sets. These are the **listed** candidates.
  - Reachability is the strong Kleene evaluation of `relation-roles.md`
    section 4 ("Path source"). A candidate whose reachability is unknown adds
    the reachability's needs to its own.
  - A group atom `R(q, x)` reads:
    - holds, if x is a lower member at q;
    - unknown, if x is an upper member that is not a lower member;
    - for any other id, listed or not: unknown with the open need if R is
      open at q, and does not hold if it is not.
  - A group `exists` follows `relation-roles.md` section 4, including the
    extra unknown item for an open domain.
  - A candidate is admitted when its predicate holds or is unknown.
  - An evaluation that fails admits nothing.
- **Openness.** A group relation is open at an anchor when its source is open
  there, or when a step of its path is open at any id the upper pass passes
  through. A source or step that is itself a group relation raises the same
  question one level down. Openness only grows, as the upper sets do, so it is
  computed with them as a least fixed point. Its need is that of the open
  base relation behind it, as #215 section 5.7 requires.

The lower sets are final before the upper stages start. So during the upper
stages an atom only moves from not holding to unknown: when its id joins the
upper set, or when its anchor becomes open. Under #215's rules that move can
turn a result that does not hold or fails into unknown, and it never turns a
result that holds or is unknown into one that is not. So the upper stages
reach the least set closed under upper admission.

At an open anchor, a listed candidate that is not an upper member reads as
unknown, not as not holding. Not holding would be sound against the final
sets. But an id can be unlisted at one stage, reading unknown, and listed at
the next, reading not holding. The stages would then not be monotone, and the
result could depend on when an id was listed. Section 8 lists the precision
this costs.

**Result.** A candidate's **upper evaluation** is its evaluation against the
final upper sets. A candidate is **certain** when the lower pass takes it:
its reachability holds over the lower sets.

- A lower member is `in`.
- A listed candidate whose upper evaluation does not hold is `out`.
- A certain candidate whose upper evaluation fails is failed if that
  evaluation read no unknown group atom and no open group domain. Its failure
  is the one the evaluation gives, the least under #215 section 5.4, rule 3.
- Every other listed candidate is unknown.
- An unlisted id is `out` if the relation is not open at the anchor, and
  unknown with the open need if it is.

**Needs** are computed once the passes end, so they do not depend on the
order of anything. An unknown candidate's needs are the least sets such that
they contain:
- what its upper evaluation met: missing cells, open relations, and its
  reachability needs;
- the needs of every upper member that is not a lower member whose atom that
  evaluation read as unknown.

This is a finite union system, so its least solution exists and is reached by
iteration. Every unknown traces back to a missing fact. With source `{(a, a),
(a, b)}`, `a.seed` missing, `b.seed` false, a complete `Edge` holding `(a,
b)`, and the predicate `candidate.seed or exists(R(anchor, p), Edge(p,
candidate))`:
- a is unknown with the need `a.seed`;
- so is b, through its read of `R(a, a)`.

**Why it is sound.** #215 section 7 shows that for any completion D′ of the
data D (#215 section 5.2): if an expression holds, does not hold or fails on
D, it does the same on D′. That holds for any fixed reading of the group
atoms, and also when D′ reads an atom that D reads as unknown in any way. A
positive predicate gives two more facts, by induction on `and`, `or` and
`exists`. An `exists` is an `or` over every id `x` of `member(x) and body(x)`.
Group atoms appear only positively, and `not`, comparisons, conditionals and
aggregates contain none.
- If it holds, it still holds when group atoms move from not holding to
  holding.
- If it does not hold, it still does not hold when group atoms move the other
  way.

The argument has two parts.

- **Exact on total data.** When nothing is missing or open, nothing reads as
  unknown. The upper stages then admit exactly what holds against the lower
  sets, which are already closed under that, so the upper sets equal the
  lower sets. Lower stages admit what holds, and that only grows with the
  sets, so the lower sets are the least fixed point. So `in` is the least
  fixed point. A non-member is `out` when its predicate does not hold against
  it, and failed when its predicate fails. This is the option's two-valued
  meaning, with failures caught as #215 section 5.4 catches them.
- **Determined results stay determined.** Let D′ be any completion of D. It
  may resolve missing cells, declare relations complete, and add tuples,
  including tuples with new ids, to relations open in D.
  - *Lower sets grow.* By induction on the stages: an admission in D holds
    against D's sets of the stage before. So in D′ it holds against those
    sets, and against D′'s, which contain them. Its reachability does the
    same.
  - *Upper sets shrink.* Consider D's upper sets, together with the ids D′
    adds at anchors open in D. Read in D′, each atom over them is one D reads
    the same way, or one D reads as unknown. A listed candidate of D outside
    D's upper sets has an upper evaluation that does not hold or fails in D,
    so it does the same in D′. So that set is closed under D′'s upper
    admission. D′'s upper stages reach the least such set, so D′'s upper sets
    are inside it.
  - *So:*
    - `in` stays `in`, because the lower sets grow.
    - `out` stays `out`: D′'s upper reading reads each atom as D does, or
      fixes one D reads as unknown, so an upper evaluation that does not hold
      in D does not hold in D′.
    - Failed stays failed: the failing evaluation read only atoms that D′
      reads the same way. Lower members hold in D′, and an id that is not an
      upper member at a closed anchor stays one. So it fails in D′, and the
      candidate stays certain.

Together: every membership D determines is the same on every completion,
total or not, which is #215 section 7's claim for memberships. The result can
be conservative, for example unknown where every completion gives `out`, as
#215 section 12 allows.

**Agreement with today's mode.** Suppose a dataset has nothing missing or
open, and today's mode passes the read at P.
- Every admission today's mode makes holds when failures are caught too, so
  the option's sets contain today's, by induction on the stages.
- Today's final sets make no evaluation fail over P's reach. So there they
  are also a fixed point when failures are caught, and the option's sets,
  the least such fixed point, are inside them.

So the sets, and the memberships, are the same, as #215's invariant 6
requires.

## 5. Evaluation

- **Demand.** Reading a group relation at an anchor computes the stages of
  the pairs reached from it: by atoms and `exists` domains at other anchors,
  and by sources and path steps. The computation runs round by round, and
  round k computes stage k for every reached pair.
  - **Catch-up.** A pair first reached in round k first computes its stages 1
    to k − 1, each reading the others as of the stage before, as the
    definition requires. Pairs it reaches in turn catch up the same way, to
    the stage they are needed at. So every reached pair has exactly the
    stages section 4 defines, whatever was read first.
  - **Stamps.** Each member is stamped with its stage. A pair's set as of
    stage j is its members stamped j or earlier, so catching up reads any
    stage of any pair without computing it again.
  - **Passes.** Under the option, the lower pass runs this way to its end,
    then the upper pass, with openness, runs the same way.
  - **Changes.** A round changes something when it admits a member or
    reaches a new pair. The rounds stop after the first round that changes
    nothing. Then every reached pair's next stage reads only reached pairs,
    none of which changed, so the sets are final.
- **Dependencies.** Every read of a group relation registers a dependency on
  the (relation, anchor) it read. That covers atoms, `exists` domains, sources
  and path steps, and it includes a read that found nothing. A relation whose
  source is a group relation that starts empty evaluates no predicate in
  round 1, but its source read is still registered. When a pair's members
  grow (under the option: its lower or upper set, or its openness), the next
  round evaluates every candidate that read it. Every pair that takes
  candidates from it gets its new candidates.
- **Semi-naive.** A round evaluates only new candidates and those whose reads
  changed in the previous round. This gives the same result as evaluating
  every non-member, because an evaluation whose reads did not change returns
  what it returned before.
- **Caching.** A computation publishes its results only when its rounds have
  ended and its final check has run: every reached pair at once.
  Intermediate sets are never visible outside the computation.
  - Each published pair keeps, in #211's memo, its members with their stamps
    and its result: its final set, or the failure its reach gives it. A later
    computation that reaches a published pair reads its stages from the
    stamps, and its final evaluations and reach from the memo. So a pair
    published first gives a later reader the same stages as one reached in
    the same computation.
  - The trace of each member's admission is replayed to every rule that reads
    it, as #211 does.
  - No rule is in a group (section 3, condition 1). So every rule a predicate
    reads has a value that does not depend on an intermediate set. Rule values
    stay cached by (rule, id, period) and are never invalidated.
  - There is no second cache. #211's runtime refusal of a resolution that
    waits on itself stays for programs with no accepted group.
- **Deferral.** A predicate can defer (`Interrupt::Defer`) or suspend past
  #211's nesting limit. The working state is kept per (group, period): sets
  with stamps, openness, the current round, its queue and admissions so far,
  and the dependencies. The round resumes at the candidate that stopped it. A
  round's admissions are applied only at its end, so resuming changes
  nothing.
- **Trace.** A member's trace is the evaluation that admitted it, at its
  stage, against the sets as of the stage before. So the stage, the witness
  and the group atoms it cites as holding are the same in every evaluation
  order and every order of demand. Each cited atom was admitted at an earlier
  stage, so every justification is well founded. A justification taken from
  the final sets could be circular: in `R = T`, `T = R or base`, each would
  cite the other. The number of stages is recorded on the group. The final
  check adds nothing to the trace unless it fails. Under the option, an
  unknown or failed candidate's trace is its upper evaluation. #219 adds
  the citing provision to each admission.
- **Fast.** Bulk runs relation reads on the explain interpreter, so it
  inherits this.
- **Dense.** Refuses a program with a recursive group, by name (#218).

## 6. Invariants

Each PR states these and adds property and differential tests.

1. **Least fixed point.** When the read passes, every member's predicate
   holds against the result, every non-member's does not, and the result
   equals a brute-force least fixed point over all pairs on small rosters
   (differential). The cases include a relation whose source is another group
   relation that starts empty.
2. **Termination.** In two-valued mode, rounds never exceed the reached
   (pair, candidate) combinations plus the reached pairs plus one (property,
   with a counter).
3. **Order.** None of these changes any result, failure or trace:
   - permuting records, tuples or roster order;
   - the order of evaluations within a round;
   - the order of demands: which anchor is read first, and whether a pair
     was published by an earlier computation.

   The cases include the failure examples of section 4 and the three
   demand-order cases on #253's second review:
   - a failure in a pair that another pair's reach does not contain;
   - a member whose admission stage would otherwise depend on which pair was
     read first;
   - a pair reached during a round, which must not end the rounds early.
4. **Unrolling.** A recursive relation equals its unrolling to depth `k` with
   `exists`, for every roster whose longest chain is at most `k`
   (differential).
5. **Monotone in tuples.** When the read passes before and after, adding
   tuples to a relation that appears only positively never removes a member.
6. **Refusals.** These are refused at compile time:
   - the oscillating example from #231;
   - every negated, aggregated or conditional group reference;
   - a group reference inside `exactly_one`, including
     `exactly_one(R(anchor, candidate), candidate == anchor)`.

   A cycle through a rule keeps `CyclicDependency`.
7. **Sandwich.** Under the three-valued option, on small rosters with up to
   10 unknown Boolean facts and open relations:
   - enumerate every total completion: each missing fact, and each tuple an
     open relation could gain over the existing ids and up to two new ones;
   - evaluate each under the option, where nothing is missing or open, so
     every membership is `in`, `out` or failed;
   - check that `in`, `out` and failed in D are the same in every completion,
     and that an unlisted `out` id is a member in none;
   - check the same for completions that supply only some of the missing
     facts, evaluated under the option: what D determines stays determined;
   - check that every unknown has needs, each naming a missing cell or an open
     base relation.

   Report how often a membership is unknown although every completion agrees.
   This is a measured conservatism, not a failure. The check needs no code
   from `src/unit_derivation`, which #232 deletes.
8. **Well-founded traces.** Every group atom a member's trace cites as
   holding was admitted at an earlier stage.
9. **Mode agreement.** On data with nothing missing or open, where today's
   mode passes the read, the option gives the same memberships (#215
   invariant 6).
10. **Byte identity.** Programs with no recursive group compile to the same
    bytes, and evaluate to the same results.

## 7. Worked examples

Synthetic engine fixtures. The pilot modules come from the encoder.

**7 CFR 273.1(b)(1) chains.** The household as a closure, replacing the
two-tie set and the chain guard of `relation-roles.md` section 8:

```yaml
- name: snap_tie_closure              # recursive
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate == anchor
        or exists(snap_tie_closure(anchor, p), snap_tie(p, candidate))

- name: snap_household_member
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        (not anchor.snap_exception_in_residence
         and snap_tie_closure(anchor, candidate))
        or (anchor.snap_exception_in_residence
            and candidate.snap_household_unit == anchor.snap_household_unit)
```

`snap_tie`, `lives_with` and `snap_exception_in_residence` are as in
`relation-roles.md` section 8. `snap_tie` already covers (b)(1)(i)–(iii),
including "a household member other than his or her parent".

`snap_tie_closure` says a person is in the closure when they are the
applicant, or are tied to someone in it. Its only reference to itself is an
`exists` domain under no `not`, so it is positive. `snap_household_member` is
not recursive. It reads the closure through an atom, and its `not` applies to
a rule outside the group.

The chain guard is gone. The exception fallback stays until the (b)(2)–(b)(6)
and (b)(7)(vi) exceptions are encoded, because each of them needs a tier B
reading of its order against (b)(1).

The tests cover the cases `relation-roles.md` sends to its guard. In one, the
applicant lives with their child, who is under 22. The child's other parent
is also in the home, with that parent's own child under 22 from another
relationship. Each person buys food separately. All four are one household,
by three ties, now with no supplied ids.

**42 CFR 435.603(f)(2).** "The household is the household of the taxpayer
claiming such individual". The taxpayer's own household is set by (f)(1),
with two adjustments:
- (f)(5) sends a person the taxpayer cannot reasonably establish as a tax
  dependent to the (f)(3) rules;
- (f)(4) puts a spouse the taxpayer lives with in the household.

None of (f)(1), (f)(3), (f)(4) and (f)(5) refers to another person's
household. So if the encoding keeps the taxpayer's household, with those
adjustments, as its own relation, the (f)(2) reference reaches a different
relation and needs only #214. If the encoding uses one relation for the whole
household, the reference is to itself, and this note makes it compile. The
tests cover the second form. They include data in which two people each
expect to be claimed by the other, so the reads at other anchors form a
cycle. The test checks that the fixed point terminates and is least.

## 8. Known conservatism and choices

- Under the option, a membership can be unknown where every completion agrees
  (section 4, measured by invariant 7). Three sources:
  - a candidate that every completion makes `out` or failed;
  - a failure whose evaluation read an unknown group atom, even where every
    completion fails it;
  - at an open anchor, a listed candidate that is not an upper member reads
    as unknown to the group's predicates, which keeps the stages monotone.
- In two-valued mode, a failure fails the read at every pair whose reach
  contains it, even where non-recursive evaluation would fail only the rows
  that reach the failing candidate.
- Inside a group, a failed candidate reads as not holding, so the stages stay
  monotone. Outside the group it fails an atom, as for any derived relation.
- No aggregate over a group relation inside the group, even where the use is
  monotone (`len(R) >= 1`). Such a use can be written as an `exists`.

## 9. Plan

Three pull requests after #214's series. Each has a failing-first test, the
full suite green with and without the `unit-derivation` feature, and the
invariants above in its body.

1. **Compile.** The single graph, its strongly connected components, the
   conditions of section 3 with polarity on the lowered predicate, and the
   diagnostics. This replaces both checks of section 2 for accepted groups,
   and records the relation of every aggregate, `only_related` included.
   Groups compile but refuse at evaluation by name
   (`recursion_not_yet_evaluated`). Includes invariant 6.
2. **Two-valued evaluation.** Stages computed by rounds with catch-up and
   stamps, dependency registration for every kind of read, the final check
   and reach, publication with stamps into #211's memo, the per-group working
   state across deferral and suspension, and the trace. Includes the 273.1
   and 435.603(f)(2) fixtures and invariants 1–5, 8 and 10.
3. **Three-valued evaluation,** with #215's third PR: the lower and upper
   passes, openness, the result, needs, and the sandwich check (invariants 7
   and 9).
