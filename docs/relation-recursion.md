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

## 2. What main does today (621e0f24)

- `reject_relation_routed_cycles` (`src/compile.rs`) builds one graph of
  rules and derived relations. It has edges from rules to what they read, and
  from each derived relation's predicate rules, membership tests and source.
  It refuses with `CyclicDependency`, naming every rule on or downstream of a
  cycle. A cycle that no rule reads is never evaluated.
- Members are resolved on demand. Their progress is kept per evaluation step
  in `Engine::members` and cleared at the end of each drive (`Engine::drive`,
  `src/engine.rs`). Nothing caches membership per (relation, anchor, period)
  across drives (#211 proposes that cache).
- Rule values are cached by (rule, id, period) for the life of the
  interpreter.

## 3. Which recursion is allowed

A **recursive group** is a strongly connected component of the graph above
that contains a cycle. The edges also include those #214 adds for atoms,
`exists` domains and path steps. A group is accepted only when all of these
hold:

1. **Only derived relations.** No rule is on a cycle. A rule reads a relation
   only through an aggregate (`len`, `sum`, `count_where`, `sum_where`, and the
   `min`/`max`/`only` of #232). An aggregate is not monotone in membership in
   general: `len(R) == 1` is the counterexample on #231, which has no fixed
   point. So a rule on a cycle is refused, as today.
2. **Positive references.** Inside the group, a predicate may refer to a group
   relation only:
   - as an atom, `R(t1, t2)`;
   - as the domain of an `exists`, `exists(R(t, x), body)`;
   - as its source relation or a path step.

   Each such reference must sit under an even number of `not`s. `and`, `or`
   and an `exists` body keep the count. An `if` condition, a comparison
   operand, a `count`/`sum` `where` clause and a summed value may not contain
   one.
3. **Bare membership tests stay outside.** A bare relation name (today's
   `relation_member`) may not name a group relation. Use the atom.

A refused group gets `RecursiveRelationNotMonotone`. The diagnostic names the
relation and the path from the reference back to it, and says which condition
the reference breaks.

These conditions make each predicate monotone in the group's memberships.
Checking each predicate's own references is enough. Every relation on a cycle
is in the group, and no rule is. A path from one group relation back to
another is therefore a chain of references that each predicate makes directly,
and each of those references is positive. A predicate may also read a derived
relation outside the group. That relation does not depend on the group, so it
is fixed while the group is computed, whatever negations its own predicate
holds. (Invariant 6 of `relation-roles.md` makes no claim about such reads;
this argument needs none.)

Adding members never removes one, so the least fixed point exists
(Knaster–Tarski), and iteration from the empty relation reaches it.

Relations outside the group are fixed while the group is computed. They may
be read in any way, negated or aggregated. That is stratification: a
non-monotone use of a recursive relation belongs to a later relation or rule,
which reads the group's final memberships.

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

### Two-valued (today's mode)

Iteration: start every reached (relation, anchor) pair at no members. Then
re-evaluate the predicate for each candidate, reading group relations from
the current sets, until no set changes. Sets only grow, so this terminates
after at most (number of reached pairs × number of candidates) rounds. In
practice it takes the length of the longest chain.

A failure while evaluating a predicate fails the evaluation, as today.

### Three-valued (with #215)

Two least fixed points are computed:
- the **lower** sets, which read group atoms from the lower sets and admit a
  candidate only when its predicate holds;
- the **upper** sets, which read group atoms from the upper sets and admit a
  candidate when its predicate holds or is unknown.

Outside the group, facts are read with #215's semantics in both. A candidate
in the lower set is `in`, one in the upper set but not the lower is unknown,
and one outside the upper set is `out`. The unknown's needs are the union of
the needs met while computing the upper set for that candidate.

Every completion of the missing facts has a least fixed point between the two
sets, because every predicate is monotone in the group and Kleene evaluation
is monotone in the facts. So `in` and `out` are true in every completion. The
result can be conservative (unknown where every completion agrees), as #215
section 12 allows.

A predicate failure inside a group, under the three-valued option, fails that
group's evaluation for the anchor that reached it. Carrying a per-member
failed state through a fixed point is left for later. This conservatism is
listed in section 8.

## 5. Evaluation

- **Demand.** Reading a group relation at an anchor starts a fixed-point
  computation for the whole group at the period. Its working set is the
  (relation, anchor) pairs reached from that read: by atoms and domains at
  other anchors, and by sources and paths.
- **Worklist.** Semi-naive: after a set grows, only the pairs that read it are
  re-evaluated. A pair's dependents are recorded the first time its predicate
  reads the pair.
- **Caching.** No rule is in a group (section 3, condition 1). So every rule a
  predicate reads has a value that does not depend on an intermediate set.
  Rule values stay cached by (rule, id, period) and are never invalidated. The
  final sets are cached per (relation, anchor, period) for the rest of the
  drive. That cache is the one #211 adds; this work keys group results the
  same way.
- **Deferral.** A predicate can defer (`Interrupt::Defer`) when evaluation is
  deep. The working state (sets, worklist, dependents) is kept per group and
  period, and resumes after the deferred rule is computed. Re-running an
  iteration from saved sets is safe because the sets only grow.
- **Trace.** Each member's trace records the predicate evaluation that
  admitted it in the final round, and the group atoms it read. The number of
  rounds is recorded on the group. #219 adds the citing provision to each
  admission.
- **Fast.** Bulk runs relation reads on the explain interpreter, so it
  inherits this.
- **Dense.** Refuses a program with a recursive group, by name (#218).

## 6. Invariants

Each PR states these and adds property and differential tests.

1. **Least fixed point.** The members satisfy every predicate. Removing any
   one member breaks a predicate or the minimality check against a
   brute-force least fixed point on small rosters (differential).
2. **Termination.** Rounds never exceed reached pairs × candidates (property,
   with a counter).
3. **Order.** Permuting records, tuples, roster order or worklist order
   changes no result and no trace.
4. **Unrolling.** A recursive relation equals its unrolling to depth `k` with
   `exists`, for every roster whose longest chain is at most `k`
   (differential).
5. **Monotone in tuples.** Adding tuples to a relation that appears only
   positively never removes a member.
6. **Refusals.** The oscillating example from #231 and every negated,
   aggregated or conditional group reference is refused at compile time.
7. **Sandwich.** Under the three-valued option, on small rosters with up to 10
   unknown Boolean facts:
   - enumerate every completion;
   - evaluate each in two-valued mode;
   - check that `in` members are members in every completion and `out`
     members in none.

   Report how often a membership is unknown although every completion agrees.
   This is a measured conservatism, not a failure. The check needs no code
   from `src/unit_derivation`, which #232 deletes.
8. **Byte identity.** Programs with no recursive group compile to the same
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
claiming such individual". This is a reference at another anchor. If the
encoding keeps the (f)(1) household as its own relation, the reference reaches
a different relation and needs only #214. If the encoding uses one relation
for the whole household, the reference is to itself, and this note makes it
compile. The tests cover the second form. They include data in which two
people each expect to be claimed by the other, so the reads at other anchors
form a cycle; the test checks that the fixed point terminates and is least.

## 8. Known conservatism

- Unknown where every completion agrees (section 4, measured by invariant 7).
- A predicate failure under the three-valued option fails the group for that
  anchor rather than marking one member failed.
- No aggregate over a group relation inside the group, even where the use is
  monotone (`len(R) >= 1`). Such a use can be written as an `exists`.

## 9. Plan

Three pull requests after #214's series. Each has a failing-first test, the
full suite green with and without the `unit-derivation` feature, and the
invariants above in its body.

1. **Compile.** Strongly connected components, the conditions of section 3,
   and the diagnostics. Groups compile but refuse at evaluation by name
   (`recursion_not_yet_evaluated`). Includes invariant 6.
2. **Two-valued evaluation.** The demand-driven, semi-naive fixed point, the
   per-group working state across deferral, the result cache, and the trace.
   Includes the 273.1 and 435.603(f)(2) fixtures and invariants 1–5 and 8.
3. **Three-valued evaluation,** with #215's third PR: lower and upper sets
   and the sandwich check (invariant 7).

The cache in PR 2 overlaps #211, which memoizes members per (relation,
entity, period). Whichever lands later reconciles with the other.
