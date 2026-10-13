# Relation roles, atoms and bounded joins

Design for #214, step 3 of the unit-derivation plan. Decision d1265 (2026-10-10,
recorded on #134 and in `unit-derivation.md` section 15): a legal unit is a
derived relation from an anchor to persons, written by extending
`derived_relation`. Not built yet.

## 1. Purpose

A unit's membership test needs four things that a derived-relation predicate
cannot do on main:

- read a fact of either end of the pair by name, including when both ends are
  the same kind (a `[Person, Person]` relation);
- test a pair fact between any two people in scope, not only the predicate's
  own pair;
- quantify over a finite set ("a household member other than his or her
  parent");
- reach candidates through a join (an applicant reaches the people they live
  with through the residence they share).

This note specifies five constructs that supply them: terms with qualified
reads (`anchor.age`), relation atoms (`spouse_of(anchor, candidate)`), term
equality (`p != anchor`), bounded `exists`, and path sources. It also says how
each behaves under the three-valued option of #215, and in what order they
land.

Out of scope: recursion (#231), pair facts that carry values, traversing a
derived relation backwards, a `kind: unit` shorthand, and dense execution
(dense refuses these constructs by name; #218).

## 2. What main does today (621e0f24)

- **Predicate evaluation.** To find the members of a derived relation for an
  id, the engine resolves its source's members (the candidates) and evaluates
  the predicate once per candidate, with `entity_id` set to the candidate and
  a `RelationEvalContext { current_id, related_id, current_entity,
  related_entity }` (`related_entity_ids_at_level`, `src/engine.rs`).
- **Routing.** A bare input always reads the candidate. A derived rule reads
  the current id if its entity is the current slot's kind, else the candidate
  (`RelationEvalContext::entity_id_for`). In a `[Person, Person]` relation
  with declared slot kinds, every Person rule therefore reads the anchor and
  every input reads the candidate; with no declared kinds, everything reads
  the candidate. No predicate can read an input of the anchor, or a rule of
  the candidate when both are the same kind.
- **Membership tests.** A bare relation name in a predicate tests the pair
  (current, related). Formula lowering gives it the legacy slots (1, 0)
  (`infer_slots`, `src/formula.rs`); direction resolution then uses the
  derivation's own slots for a derived relation, or the slot kinds for a
  typed one, and refuses a same-kind typed relation as ambiguous
  (`src/relation_direction.rs`).
- **Scope.** The context does not reach a `count`/`sum` `where` clause, a
  summed value, or the body of any rule.
- **Candidates.** A derived relation's candidates come from one source
  relation (or a chain of derived relations, each filtering its parent). There
  is no join.
- **Pair data.** The only datum keyed by a pair is a relation tuple
  (`RelationRecord { name, tuple, interval }`), which carries no value. The
  interpreter indexes tuples by `(relation, slot, id)` for every slot
  (`Engine::new`), so a lookup from any slot is already indexed.
- **Syntax.** `.` parses as field access but lowers only as the argument of
  `sum(relation.field)`; anywhere else lowering fails ("bare field access in
  scalar position not supported"). A call lowers only for the built-in
  function names; any other name fails lowering. So `anchor.age`,
  `spouse_of(anchor, candidate)` and `exists(...)` fail to compile today, and
  no compiled program contains them. On 2026-10-13 no `rulespec-*` corpus at
  `origin/main` had an `anchor.` or `candidate.` read, an `exists(` call, or a
  rule, input or relation named `anchor`, `candidate` or `exists`.

The cost of these gaps shows in rulespec-us. Because a predicate cannot read
both ends, `us/policies/medicaid/magi_household_income_pipeline.yaml` copies
facts onto one synthetic row per (applicant, candidate) pair: 27
`household_row_*` inputs and 11 `household_row_*` rules. Ten of the inputs are
facts about the pair (`candidate_is_the_applicant`, `lives_with_applicant`,
`is_applicants_spouse`, and so on). All ten are Boolean.

## 3. Constructs

### 3.1 Terms

Inside a derived relation's predicate, a **term** names an entity id:

- `anchor`: the id the relation is read from (its current slot);
- `candidate`: the id being tested (its related slot);
- a variable bound by `exists` (3.5).

Every term has a kind: `anchor` and `candidate` take the derivation's slot
kinds, and a variable takes the kind of the slot it binds. The constructs below
check kinds, so a derived relation whose predicate uses any of them must have
known slot kinds (declared `arguments` or `slot_entities`, or kinds stamped by
direction resolution). Otherwise compilation refuses with
`relation_roles_need_slot_kinds`.

`anchor` and `candidate` are reserved inside derived-relation predicates only.
Elsewhere they stay ordinary identifiers.

A term is not a value. It can appear only as the left side of a qualified
read, as an atom's argument, or in a term comparison (3.4). Anything else
refuses with `relation_term_used_as_value`.

### 3.2 Qualified reads

`t.name`, where `t` is a term and `name` is an input or a derived rule:

- **Input.** The input declared on `t`'s kind, read at `t`'s id. A declared
  default applies exactly as for an unqualified read.
- **Rule.** The rule evaluated at `t`'s id. Its entity must be `t`'s kind.

It refuses when `name` is not an input or rule (`relation_role_unknown_name`),
when its entity is not `t`'s kind (`relation_role_entity_mismatch`), and when
`name` is a relation (use an atom).

A qualified read names a value at an id. It is not a scope for an expression
(there is no `anchor.(a + b)`), so rule values stay cached by
`(rule, id, period)` as today, and the cache does not grow a dimension.

A qualified read may appear anywhere in a predicate that a value may:
arithmetic, comparisons, parameter indices, `if`, and inside an `exists` body.
It may not appear inside a `count`/`sum` `where` clause or summed value, which
have no relation context today and keep none
(`relation_role_outside_scope`).

Unqualified reads keep today's routing, so every existing program compiles to
the same artifact. A reader cannot tell from an unqualified read in a
same-kind predicate which end it reads; a later lint can suggest the qualified
form. That lint is not part of this work.

### 3.3 Relation atoms

`R(t1, ..., tn)`, where `R` names a declared relation of arity `n` and each
`ti` is a term. It is a judgment that holds when the tuple
`(id(t1), ..., id(tn))` holds in the period.

- **Data relation.** A supplied tuple with exactly these ids whose interval
  contains the period.
- **Derived relation.** Arity 2 only. With current slot `c` and related slot
  `r`, it holds when `id(t_r)` is a member of `R` for `id(t_c)`.

Arguments are written in `R`'s slot order. That fixes the direction for a
same-kind relation, which a bare membership test cannot do. Each `ti` must
have `R`'s slot kind for slot `i`, and `R` must be typed
(`relation_atom_needs_typed_relation`, `relation_atom_kind_mismatch`).

An atom naming the relation it appears in, or any relation that depends on
it, closes a cycle. The existing cycle check refuses it (atoms add edges to its
graph) until #231 allows positive, monotone recursion.

A bare relation name keeps today's meaning. Where its slots are fixed, `R` and
the atom over `(anchor, candidate)` in those slots agree; a differential test
checks it (section 7).

A call whose name is a relation is an atom. A relation whose name is also a
built-in function name (`len`, `sum`, `max`, and the rest) cannot be used as
an atom (`relation_name_shadows_builtin`).

### 3.4 Term comparison

`t1 == t2` and `t1 != t2`, where both sides are terms of the same kind,
compare ids. The result is always determined, because ids are given, not
derived. It expresses "a member other than his or her parent" in 7 CFR
273.1(b)(1)(iii) and "anyone but the applicant" in a quantified set.

### 3.5 Bounded `exists`

`exists(G, body)`. `G` is an atom with exactly one argument that is a new
variable name; every other argument is a term in scope. The variable ranges
over the ids in that slot of `G`'s tuples that match the other arguments in
the period: distinct, in sorted order. `body` is a judgment in which the
variable is in scope.

- For a data relation, the variable may be in any slot.
- For a derived relation, the variable must be in its related slot, so the
  domain is the relation's members for the id in its current slot. A variable
  in the current slot would traverse the relation backwards and refuses
  (`inverse_derived_traversal_unsupported`).
- A variable name is a lowercase identifier. It may not be `anchor` or
  `candidate`, the name of a rule, input, parameter or relation the module can
  see, or a variable already bound by an enclosing `exists`
  (`relation_variable_name_conflict`). Using a variable outside its body is an
  unknown name.
- `exists` nests. Its nodes count toward any bound on formula or expression
  size or depth (#240, #245) as other nodes do.

"For all" is `not exists(G, not body)`.

Every domain is the set of ids found in tuples or members, so it is finite by
construction. There is no separate safety analysis.

**Semantics** (today's two values): `exists` holds when `body` holds for some
id in the domain and does not hold for an empty domain. It is evaluated as
`or` over the domain in sorted order: it stops at the first id for which the
body holds, and a failure propagates as it does from `or`. It is therefore
equal to the disjunction obtained by writing out `body` once per id of the
domain, in sorted order.

### 3.6 Path sources

A derived relation may take its candidates from a path instead of one
relation:

```yaml
- name: lives_with
  kind: derived_relation
  derived_relation:
    arguments: [Person, Person]
    current_slot: 0
    related_slot: 1
    source_path:
      - {relation: member_of_household, from_slot: 0, to_slot: 1}
      - {relation: member_of_household, from_slot: 1, to_slot: 0}
  versions:
    - effective_from: 2026-01-01
      formula: true
```

- Each step reads a binary relation, data or derived, from one slot to the
  other. Each step's `to` kind is the next step's `from` kind; the first
  `from` kind is the anchor's and the last `to` kind is the candidate's.
- A derived step is read forward only: `from_slot` is its current slot and
  `to_slot` its related slot.
- The candidates for an anchor are the ids the path reaches from it: distinct,
  in sorted order. Intermediate ids are not kept. Candidate `c` is reached from
  `a` exactly when `exists(R1(a, h), R2(h, c))` holds, with each atom written in
  its step's slots; a differential test checks this (section 7).
- `source_relation` and `source_path` are exclusive, and one is required. A
  path source requires explicit `arguments`, `current_slot` and
  `related_slot`; the legacy (1, 0) default does not apply.

Why: datasets supply a residence roster such as
`member_of_household(Person, Household)`. A relation anchored on an applicant
reaches the applicant's co-residents through the household. Without a path
source, the caller would have to supply every pair of co-residents.

### 3.7 Elections

An election is an input the law lets a named actor choose, such as separate
household status under 7 CFR 273.1(b)(2). Its declaration (`election: {actor,
provision}`, no default) and its place in the artifact interface block belong
to #155. The engine evaluates one set of supplied elections. Choosing the best
set belongs to the simulation layer (#134, 2026-10-10).

Some elections are about a pair: under (b)(3)(ii) the household providing
board requests that a particular boarder be included. Such an election is a
binary relation, not a slot input. Whether #155's declaration covers
relations is open on that work; nothing here depends on the answer.

## 4. Under the three-valued option (#215)

Under #215's `missing_facts: "indeterminate"`, each construct is defined by
reduction to forms #215 already defines:

- **Qualified read.** An input read or a rule evaluation at the term's id. It
  originates unknown exactly as an unqualified read does (#215 section 5.3).
- **Term comparison.** Always holds or does not hold.
- **Atom on a base relation.** A listed tuple holds. An absent tuple does not
  hold if the relation is declared complete, and is unknown otherwise, with
  the completeness need naming the relation.
- **Atom on a derived relation.** The membership of `id(t_r)` for
  `id(t_c)` (#215 section 5.7): `in` holds, `out` does not hold, unknown is
  unknown with its needs, failed fails.
- **`exists`.** The strong Kleene `or` of #215 section 5.4 over the listed
  ids `x` of the domain, of `membership(x) and body(x)`, with one more unknown
  item, carrying the completeness need, when the domain's relation is open. So
  it holds if some id with membership `in` makes the body hold; it does not
  hold if the domain is not open and every listed id is `out` or makes the
  body not hold; otherwise it is unknown.
- **Path source.** A candidate's membership is the `or` over the paths that
  reach it of the `and` of each step's membership. The derived relation is
  open when any step relation it reads is open, with that relation's
  completeness need.

Each is the strong Kleene evaluation of a finite `and`/`or` formula over
determined ids, which #215 section 7 shows sound.

#215's plan makes every relation read refuse under the option until its third
PR gives relations open-world semantics. The same holds here: under the
option, atoms, `exists` and path sources refuse by name until that PR lands.
Qualified reads and term comparisons are not relation reads, and follow #215's
first PR. Whichever of this series and #215's third PR lands second lifts the
refusals.

## 5. Execution paths

- **Explain.** As specified above.
- **Fast.** Bulk evaluates relation aggregations row by row on the explain
  interpreter, so derived-relation predicates run there with the same
  semantics. The new constructs occur only inside predicates, which bulk never
  evaluates directly.
- **Dense.** Refuses, by name, a program whose reachable predicates use a
  qualified read, an atom, a term comparison, `exists` or a path source:
  `DenseCompileError::Unsupported` naming the construct and #218.

## 6. Engine changes

- **Model.** `Term { Anchor, Candidate, Var(index) }`, with a variable's
  index counted from the innermost enclosing `exists`. New variants:
  `ScalarExpr::RoleInput { term, name }`, `ScalarExpr::RoleDerived { term,
  name }`, `JudgmentExpr::RoleDerived { term, name }`,
  `JudgmentExpr::RelationAtom { relation, terms }`,
  `JudgmentExpr::SameEntity { left, right, negated }`,
  `JudgmentExpr::Exists { relation, terms, variable_slot, body }`.
  `RelationDerivation` gains a path source; `source_relation` becomes
  optional in the artifact and is omitted only when a path is present.
- **Formula.** Parse and lower `t.name` on a term, calls whose name is a
  relation, `exists(atom, body)`, and comparisons between two terms. The
  lowering context carries the declared relation names and the variables in
  scope.
- **Engine.** `RelationEvalContext` becomes a binding environment: the
  anchor's and candidate's ids and kinds as now, plus a borrowed chain of
  frames for bound variables. Unqualified routing reads only the anchor and
  candidate, as now. `exists` is a new resumable site, keyed by its address
  and every bound id, with the same resume discipline as a fold, so a
  deferral resumes where it stopped. Path candidates are generated inside the
  existing members site.
- **Compile.** Kind checks and the refusals named in section 3; cycle edges
  from atoms, `exists` domains and path steps.
- **Trace.** A qualified read records its dependency at the term's id, on the
  evaluated path and on the skipped path (`record_skipped_*`).
- **Capabilities.** `relation_roles`, listed once the series is complete.
- **Artifact.** Every addition is an enum variant or a path source that omits
  `source_relation`. An engine without these additions refuses such an
  artifact when it loads it (unknown variant, or the missing required field);
  none is an optional field an older engine would silently skip. A program
  that uses none of them compiles to the same bytes as before. If #155's
  format 3 merges first, this work adds to format 3 and needs no separate
  version bump.

## 7. Invariants

Each PR states the invariants it touches and adds property tests (proptest)
and differential tests for them:

1. **Order.** Permuting input records, relation tuples or roster order changes
   no output and no trace.
2. **`exists` expands.** `exists(G, body)` equals the disjunction of `body`
   over its sorted domain, in both modes (differential).
3. **Atoms agree with bare tests.** Where a bare membership test's slots are
   fixed, it equals the atom over `(anchor, candidate)` in those slots
   (differential).
4. **Qualified reads agree with routing.** Where today's routing reads the
   same id, an unqualified read equals the qualified one (differential).
5. **Paths agree with `exists`.** A path source's members equal the members
   computed with `exists` over its steps (differential).
6. **Monotone in tuples.** If every atom and `exists` in a predicate sits under
   an even number of negations, adding tuples never removes a member. Under an
   odd number, removal is intended, and the tests label it.
7. **Determinism.** Two runs produce identical output and trace bytes.
8. **Paths agree with each other.** Explain and fast agree on every test
   program; dense refuses by name.
9. **Byte identity.** Every engine fixture, and every module of rulespec-us,
   rulespec-nz, rulespec-uk and rulespec-ca at pinned commits, compiles to the
   same artifact bytes before and after each PR.
10. **Kinds.** No compiled read, atom or domain meets a slot or entity of a
    kind other than its term's (refusal tests, plus a property over generated
    programs).
11. **Termination.** Every domain is finite, and recursion stays refused.

With #215: when every input is supplied and every base relation is declared
complete, nothing is unknown; supplying a missing fact or declaring a relation
complete never changes a determined result.

## 8. Worked example: 7 CFR 273.1, common case

A synthetic engine fixture, not a RuleSpec module. The pilot module in
rulespec-us#1428 comes from the encoder. The fixture shows that the constructs
cover the pilot's scope: the meal group under (a)(3), the required
combinations of (b)(1)(i) and (b)(1)(ii), and a two-step chain of them.

```yaml
- name: required_combination          # 273.1(b)(1)(i)-(ii), between two people
  kind: derived_relation
  derived_relation:
    arguments: [Person, Person]
    current_slot: 0
    related_slot: 1
    source_relation: lives_with       # section 3.6
  versions:
    - effective_from: 2026-01-01
      formula: >
        spouse_of(anchor, candidate) or spouse_of(candidate, anchor)
        or (candidate.age < 22 and parent_of(anchor, candidate))
        or (anchor.age < 22 and parent_of(candidate, anchor))

- name: snap_household_member         # anchored on the applicant
  kind: derived_relation
  derived_relation:
    arguments: [Person, Person]
    current_slot: 0
    related_slot: 1
    source_relation: lives_with
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate == anchor
        or candidate.meal_group == anchor.meal_group
        or required_combination(anchor, candidate)
        or exists(lives_with(anchor, p),
                  (p.meal_group == anchor.meal_group
                    or required_combination(anchor, p))
                  and required_combination(p, candidate))
```

`parent_of` stands for a natural, adoptive or step-parent, and `meal_group`
for the group a person customarily buys food and prepares meals with. Both are
inputs the fixture supplies.

The last clause matters in a common arrangement. An applicant lives with
their child, who is under 22, and the child's other parent, who is not the
applicant's spouse and buys food separately. (b)(1)(ii) puts the child in the
applicant's household and also in the other parent's, so all three are one
household. Only the two-step clause finds the other parent.

A chain of three or more steps needs #231. The pilot must report one as
unknown, not drop it. One way to do that with these constructs is an open
binary relation of chain ties, whose absent tuples are unknown until a caller
supplies them or declares the relation complete. The pilot brief on
rulespec-us#1428 decides how it does this.

The fixture's tests check memberships for small households against values
worked out by hand from the regulation's text, including the case above, a
married couple who buy food separately, and roommates who buy food
separately.

## 9. Plan

Five pull requests, in order. Each has a test that fails before it, the full
suite green with and without the `unit-derivation` feature, the byte-identity
check of invariant 9, and its invariants in the PR body with their tests. Each
is sized for one build lane.

1. **Terms and qualified reads.** Model, parsing, lowering, kind checks, the
   binding environment, trace routing, the dense refusal, and the
   `docs/rulespec.md` section.
2. **Atoms and term comparison.** Cycle edges, kind checks, the built-in name
   refusal, and the differential test against bare membership tests.
3. **Bounded `exists`.** Variables and scope, the resumable site, the
   refusals, and the expansion test.
4. **Path sources.** The artifact form, candidate generation, and the
   differential test against `exists`.
5. **Fixture and docs.** The 273.1 fixture, the `relation_roles` capability,
   `docs/execution-semantics.md`, and a `DECISIONS.md` entry.

Then, with #215's third PR, lift atoms, `exists` and path sources under the
three-valued option. After that the encoder (TheAxiomFoundation/axiom-encode#1715)
learns the constructs, and the pilot (TheAxiomFoundation/rulespec-us#1428) is
encoded.

This series depends on neither #155 nor #215 for its two-valued form. It
touches the same code as #211 and #249, which rework derived-relation typing
and traversal; whichever lands later rebases.

## 10. Choices made here that the issue does not dictate

- **Atoms are written `R(a, b)`, not `holds(R, a, b)`.** The form reads like
  the regulation ("spouse of"), fixes slot order, and is the form #231 needs.
  For 42 CFR 435.603(f)(2), the household of the taxpayer who claims the
  applicant, the predicate would read
  `exists(claims_as_dependent(t, anchor), medicaid_household(t, candidate))`.
  Only the cycle refusal has to change for it to run.
- **`exists` takes a generator atom, not a free variable.** Every domain is
  finite by construction.
- **Pair facts are Boolean relations.** The ten pair facts the Medicaid
  pipeline copies are all Boolean, so the first two encodings need no valued
  pair facts. A synthetic pair entity covers the case until one does.
- **Derived relations are traversed forward only.** Backward traversal needs
  every possible anchor's members; nothing needs it yet.
- **Qualified reads name a value, not an expression scope,** so rule caching
  keeps its key.
- **Unqualified reads do not change,** so no existing artifact changes.
