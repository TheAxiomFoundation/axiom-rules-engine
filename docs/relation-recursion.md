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

## 2. What main does today (97dee100)

Two compile checks refuse cycles:

- `validate_relation_derivation_graph` (`src/compile.rs`, the first step of
  `evaluation_order`) builds a graph of derived relations only. A relation's edges go to its source and to every relation
  its predicate tests with a bare membership test. Any cycle is refused with
  `CyclicRelationDependency`, whether or not a rule reads it.
- `reject_relation_routed_cycles` (`src/compile.rs`) builds one graph of
  rules and derived relations. It has edges from rules to what they read, and
  from each derived relation to its predicate's rules, membership tests and
  source. It refuses with `CyclicDependency`, naming every rule on or
  downstream of a cycle.

Evaluation:

- Members are resolved on demand. Their progress is kept per evaluation step
  in `Engine::members` and cleared at the end of each drive (`Engine::drive`,
  `src/engine.rs`). Nothing caches membership across drives.
- #211 (open) adds a memo per (relation, entity, period) on the engine, kept
  for the interpreter's lifetime. It holds a resolution's failure as well as
  its members, and replays the trace a resolution recorded to every rule that
  reads it. Resolutions nested past 32 levels suspend and resume.
- Rule values are cached by (rule, id, period) for the life of the
  interpreter.

## 3. Which recursion is allowed

This work replaces the two graphs of section 2 with one. Its nodes are rules
and derived relations. A rule has an edge to everything it reads. A derived
relation has an edge to its source, to each step of its path, and to every
relation and rule its predicate reads: atoms, `exists` domains, membership
tests and rule references. The edges include those #214 adds.

A **recursive group** is a strongly connected component of this graph that
contains a cycle. A group is accepted only when all of these hold:

1. **Only derived relations.** No rule is on a cycle. A rule reads a relation
   only through an aggregate (`len`, `sum`, `count_where`, `sum_where`, and the
   `min`/`max`/`only` of #232). An aggregate is not monotone in membership in
   general: `len(R) == 1` is the counterexample on #231, which has no fixed
   point.
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
  constructs main has gets `CyclicRelationDependency`. The diagnostic names the relation, the path from
  the reference back to it, the condition it breaks, and the construct that
  made the position mixed or negative (for example "inside `exactly_one`").

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

### Rounds

The result is defined by rounds, not by the order of any evaluation.

- **Round 0.** Every reached (relation, anchor) pair has no members.
- **Round k.** For every reached pair, the predicate of every candidate that
  is not yet a member is evaluated. Every group relation is read as it stood
  at the end of round k − 1, and so are the candidates a group source or path
  supplies. The admissions of round k are applied together at its end.
- **End.** The rounds stop after the first round that changes nothing.

Each round reads only the previous round's sets. So the result is a function
of the program and the data. It does not depend on the order of records,
tuples, the roster, or the evaluations within a round.

Every round but the last admits a member. There are finitely many (pair,
candidate) combinations, so the rounds terminate. In practice they take the
length of the longest chain.

### Two-valued (today's mode)

- A candidate is admitted in a round when its predicate holds.
- An evaluation that fails admits nothing in that round. The candidate is
  evaluated again in a later round if anything it read has changed.
- **Final check.** After the last round, every candidate of every reached
  pair, member or not, is evaluated against the final sets. If any of those
  evaluations fails, reading the group fails, with the failure of the first
  failing candidate in (relation, anchor, candidate id) order. Otherwise the
  final sets are the result. An implementation evaluates again only the
  candidates whose reads changed after their last evaluation; for the rest,
  that evaluation stands.

A failure can disappear as the sets grow. Take `anchor.seed or
exists(Link(anchor, p), R(p, candidate)) or 1 / 0 > 0`, with `A.seed` false,
`Z.seed` true and `Link(A, Z)`. In round 1, A's evaluation fails, because `R`
at `Z` is still empty, and Z is admitted. In round 2, A's `exists` holds and
A is admitted. The final check passes. No evaluation order can change this.

A failure can also appear as the sets grow. In `(R(anchor, candidate) and 1 /
0 > 0) or candidate == anchor`, the anchor is admitted in round 1, while
`R(anchor, anchor)` is still false. Against the final sets, the `and` reaches
`1 / 0` and the check fails. That is the failure evaluation would give if the
final sets had been supplied as tuples.

**When the check passes, the result is the least fixed point.** Short-circuit
evaluation of a positive predicate that holds against some sets reads, along
the path that made it hold, atoms that stay true as the sets grow and values
that do not depend on the group. So against larger sets it still holds, unless
it fails. Hence:
- every member holds against the final sets, and every non-member does not,
  because its last evaluation read them: the final sets are a fixed point;
- for any memberships that satisfy every predicate without a failure, each
  round's admissions are among them, by induction on the rounds: the result is
  the least such set.

### Three-valued (with #215)

Memberships are `in`, unknown (with needs), failed or `out`, and a relation is
open or not for an anchor (#215 section 5.7).

**Openness.** A group relation is open at an anchor when something it takes
candidates from is open: its source at that anchor, or a step of its path at
an id the path passes through. A source or step that is itself a group
relation raises the same question one level down. So openness is a least fixed
point too. It starts closed for every reached pair and is computed in the same
rounds. Its need is that of the open base relation behind it, as #215 section
5.7 requires.

An id that a group relation does not list at an open anchor is unknown, with
that need. This includes ids the data does not mention at all: a completion
may add a tuple with a new id to an open relation.

**Lower and upper sets.** Computed in rounds, lower first.
- The **lower** sets read a group atom as holding for a lower member and not
  holding otherwise. They admit a candidate when its predicate holds.
- The **upper** sets read a group atom with three values:
  - it holds for a lower member;
  - it is unknown for an upper member that is not a lower member, with that
    member's needs as they stood at the end of the previous round;
  - for any other id, it does not hold if the relation is not open at that
    anchor. Otherwise it is unknown with the open need.

  An `exists` over a group relation follows `relation-roles.md` section 4,
  including the extra unknown item for an open domain. The upper sets admit a
  candidate when its predicate holds or is unknown.
- An upper member is evaluated again when the needs of something it read have
  grown, to update its own needs. Needs only grow and are finite, so the
  rounds still end.

**Result.**
- A lower member is `in`.
- An upper member that is not a lower member is unknown. Its needs are those
  its last upper evaluation met: missing cells, open relations, and the needs
  of every unknown group membership it read. So every unknown traces back to a
  missing fact. With source `{(a, a), (a, b)}`, `a.seed` missing, `b.seed`
  false, a complete `Edge` holding `(a, b)`, and the predicate `candidate.seed
  or exists(R(anchor, p), Edge(p, candidate))`, a is unknown with the need
  `a.seed`, and so is b, through its read of `R(a, a)`.
- A listed candidate outside the upper set is `out`.
- An unlisted id is `out` if the relation is not open at the anchor, and
  unknown with the open need if it is.

**Failures.** #215 section 5.9 catches a failure at a derived relation's
predicate, for each candidate, and makes the membership failed. Inside a
group, a failed membership would be a fourth value for other predicates to
read through the rounds. That is left for later: under the option, a failure
in either final check fails the read of the group at that anchor, as in
two-valued mode. Section 8 lists this.

**Why it is sound.** Let D* be any completion of the data D (#215 section
5.2). It may add tuples, including tuples with new ids, to open relations. Let
R* be its two-valued result, and suppose its check passes.
- **Lower ⊆ R\*.** By induction on the lower rounds. A lower admission holds,
  under Kleene evaluation in D, against lower sets that R* contains.
  Kleene evaluation holds in every completion, and a positive predicate keeps
  holding as the group grows. So the candidate holds against R*, and R* is a
  fixed point, so it is in R*.
- **R\* ⊆ upper, or unknown in D.** By induction on R*'s rounds. A member
  admitted in D* holds against an earlier set of R*. In D, each group atom it
  read holds or is unknown, by the induction. Kleene evaluation in D is no
  more determined than evaluation in D*. So the predicate holds or is unknown
  in D, and the candidate is an upper member. The exception is a candidate D
  does not list. It came from a tuple D* added to an open relation, which
  makes the group relation open at that anchor in D, so D reports it unknown.

So `in` is a member and `out` a non-member in every completion. The result
can be conservative (unknown where every completion agrees), as #215 section
12 allows.

## 5. Evaluation

- **Demand.** Reading a group relation at an anchor starts the computation
  for the whole group at that period. Its working set is the (relation,
  anchor) pairs reached from that read: by atoms and `exists` domains at other
  anchors, and by sources and path steps. The working set can grow during the
  rounds, as new members lead to new anchors.
- **Dependencies.** Every read of a group relation registers a dependency on
  the (relation, anchor) it read. That covers atoms, `exists` domains, sources
  and path steps, and it includes a read that found nothing. A relation whose
  source is a group relation that starts empty evaluates no predicate in
  round 1, but its source read is still registered. When a pair's members
  grow, or under the option its needs or openness, the next round evaluates
  every candidate that read it. Every pair that takes candidates from it gets
  its new candidates.
- **Semi-naive.** A round evaluates only new candidates and those whose reads
  changed in the previous round. This gives the same result as evaluating
  every non-member, because an evaluation whose reads did not change returns
  what it returned before.
- **Caching.** A group publishes its results only when it has converged and
  passed its check, all its (relation, anchor) pairs at once. Intermediate sets
  are never visible outside the computation.
  - The published result, members or the failure, is kept per (relation,
    anchor, period) for the life of the interpreter, as #211's memo keeps
    them.
  - The trace of each member's admission is replayed to every rule that reads
    it, as #211 does.
  - No rule is in a group (section 3, condition 1). So every rule a predicate
    reads has a value that does not depend on an intermediate set. Rule values
    stay cached by (rule, id, period) and are never invalidated.
  - Whichever of #211 and PR 2 lands second builds on the other's memo, so
    there is never a second cache.
- **Deferral.** A predicate can defer (`Interrupt::Defer`), or, once #211 has
  landed, suspend past its nesting limit. The working state is kept per
  (group, period): sets, needs, openness, the current round, its queue and
  admissions so far, and the dependencies. The round resumes at the candidate
  that stopped it. A round's admissions are applied only at its end, so
  resuming changes nothing.
- **Trace.** A member's trace is the evaluation that admitted it. It records
  the round, and the group atoms the evaluation read, each admitted in an
  earlier round. So every member's justification is well founded, and it is
  the same for every evaluation order. A justification taken from the final
  sets could be circular: in `R = T`, `T = R or base`, each would cite the
  other. The number of rounds is recorded on the group. The final check adds
  nothing to the trace unless it fails. Under the option, an unknown member's
  trace is its last upper evaluation. #219 adds the citing provision to each
  admission.
- **Fast.** Bulk runs relation reads on the explain interpreter, so it
  inherits this.
- **Dense.** Refuses a program with a recursive group, by name (#218).

## 6. Invariants

Each PR states these and adds property and differential tests.

1. **Least fixed point.** When the check passes, every member's predicate
   holds against the result, every non-member's does not, and the result
   equals a brute-force least fixed point on small rosters (differential).
   The cases include a relation whose source is another group relation that
   starts empty.
2. **Termination.** Rounds never exceed the reached (pair, candidate)
   combinations plus one, in two-valued mode (property, with a counter).
3. **Order.** Permuting records, tuples, roster order, or the order of
   evaluations within a round changes no result, no failure and no trace. The
   cases include the two failure examples in section 4.
4. **Unrolling.** A recursive relation equals its unrolling to depth `k` with
   `exists`, for every roster whose longest chain is at most `k`
   (differential).
5. **Monotone in tuples.** When the check passes before and after, adding
   tuples to a relation that appears only positively never removes a member.
6. **Refusals.** These are refused at compile time:
   - the oscillating example from #231;
   - every negated, aggregated or conditional group reference;
   - a group reference inside `exactly_one`, including
     `exactly_one(R(anchor, candidate), candidate == anchor)`.

   A cycle through a rule keeps `CyclicDependency`.
7. **Sandwich.** Under the three-valued option, on small rosters with up to
   10 unknown Boolean facts and open relations:
   - enumerate every completion: each missing fact, and each tuple an open
     relation could gain over the existing ids and up to two new ones;
   - evaluate each in two-valued mode;
   - check that `in` members are members in every completion, and `out`
     members and unlisted `out` ids in none;
   - check that every unknown has needs, each naming a missing cell or an open
     base relation.

   Report how often a membership is unknown although every completion agrees.
   This is a measured conservatism, not a failure. The check needs no code
   from `src/unit_derivation`, which #232 deletes.
8. **Well-founded traces.** Every group atom a member's trace cites as
   holding was admitted in an earlier round.
9. **Byte identity.** Programs with no recursive group compile to the same
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

## 8. Known conservatism

- Unknown where every completion agrees (section 4, measured by invariant 7).
- A failure in any reached pair fails the read of the group, even where
  non-recursive evaluation would fail only the rows that reach it.
- Under the three-valued option, a predicate failure fails the read of the
  group rather than marking one member failed.
- No aggregate over a group relation inside the group, even where the use is
  monotone (`len(R) >= 1`). Such a use can be written as an `exists`.

## 9. Plan

Three pull requests after #214's series. Each has a failing-first test, the
full suite green with and without the `unit-derivation` feature, and the
invariants above in its body.

1. **Compile.** The single graph, its strongly connected components, the
   conditions of section 3 with polarity on the lowered predicate, and the
   diagnostics. This replaces both checks of section 2 for accepted groups. It
   coordinates with #211, which rewrites the relation cycle search as an
   explicit-stack walk; whichever lands second adapts. Groups compile but
   refuse at evaluation by name (`recursion_not_yet_evaluated`). Includes
   invariant 6.
2. **Two-valued evaluation.** Rounds, dependency registration for every kind
   of read, the final check, publication into #211's memo (or that memo with
   #211's lifetime, if #211 has not landed), the per-group working state
   across deferral and suspension, and the trace. Includes the 273.1 and
   435.603(f)(2) fixtures and invariants 1–5, 8 and 9.
3. **Three-valued evaluation,** with #215's third PR: openness, lower and
   upper sets, needs, and the sandwich check (invariant 7).
