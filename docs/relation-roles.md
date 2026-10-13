# Relation roles, atoms and bounded joins

Design for #214, step 3 of the unit-derivation plan. Decision d1265 (2026-10-10,
recorded on #134 and in `unit-derivation.md` section 15): a legal unit is a
derived relation from an anchor to persons, written by extending
`derived_relation`. Not built yet.

## 1. Purpose

A unit's membership test needs five things that a derived-relation predicate
cannot do on main:

- read a fact of either end of the pair by name, including when both ends are
  the same kind (a `[Person, Person]` relation);
- test a pair fact between any two people in scope, not only the predicate's
  own pair;
- quantify over a finite set ("a household member other than his or her
  parent");
- reach candidates through a join (an applicant reaches the people they live
  with through the residence they share);
- read a parameter (a State's age threshold under 42 CFR 435.603(f)(3)).

This note specifies six constructs that supply them:

- terms with qualified reads (`anchor.age`);
- parameters in predicates;
- relation atoms (`spouse_of(anchor, candidate)`);
- term comparison (`p != anchor`);
- bounded `exists`;
- path sources.

It also says how each behaves under the three-valued option of #215, and in
what order they land.

Out of scope:

- recursion, so chains of unbounded length (#231);
- pair facts that carry values;
- traversing a derived relation backwards;
- a `kind: unit` shorthand;
- dense execution, which refuses these constructs by name (#218).

## 2. What main does today (621e0f24)

**Predicates.**

- **Evaluation.** To find a derived relation's members for an id, the engine
  first resolves its source's members (the candidates). It then evaluates the
  predicate once per candidate, with `entity_id` set to the candidate and a
  `RelationEvalContext { current_id, related_id, current_entity,
  related_entity }` (`related_entity_ids_at_level`, `src/engine.rs`). It adds
  any tuples supplied under the derived relation's own name to the members.
- **Routing.** A bare input always reads the candidate. A derived rule reads
  the current id if its entity is the current slot's kind, and the candidate
  otherwise (`RelationEvalContext::entity_id_for`). Take a `[Person, Person]`
  relation:
  - with declared slot kinds, every Person rule reads the anchor and every
    input reads the candidate;
  - with no declared slot kinds, everything reads the candidate.

  Either way, no predicate can read an input of the anchor. Nor can it read a
  rule of the candidate when both ends are the same kind.
- **Membership tests.** A bare relation name in a predicate tests the pair
  (current, related). Formula lowering gives it the legacy slots (1, 0)
  (`infer_slots`, `src/formula.rs`). Direction resolution
  (`src/relation_direction.rs`) then changes them:
  - for a derived relation, it uses the derivation's own slots;
  - for a typed relation, it uses the slot kinds;
  - when the predicate's two kinds are equal and every slot of the relation
    has that kind, it refuses the test as ambiguous;
  - when the kinds are unknown, it keeps (1, 0) silently.

  Direction resolution never stamps kinds onto a same-kind relation.
- **Scope.** The context does not reach a `count`/`sum` `where` clause, a
  summed value, or the body of any rule.
- **Parameters.** None. `lower_judgment_formula` builds its lowering context
  with no parameters, so `p[i]` fails ("not a parameter table") and a bare
  parameter name reads an input.
- **Candidates.** A derived relation's candidates come from one source
  relation, or from a chain of derived relations that each filters its
  parent. There is no join.

**Inputs.**

- **Implicit and untyped.** Any name that is not a rule or a parameter lowers
  to an input read. The input catalog (`Program::input_catalog`,
  `src/model.rs`) is built from where names are read.
- **Declarations ignored.** RuleSpec `inputs:` blocks carry no meaning, apart
  from the `election` annotation #155 adds.
- **No defaults.** The model has an `InputOrElse` node, but formula lowering
  never produces one.

**Pair data.** The only datum keyed by a pair is a relation tuple
(`RelationRecord { name, tuple, interval }`), which carries no value. The
interpreter indexes tuples by `(relation, slot, id)` for every slot
(`Engine::new`), so a lookup from any slot is already indexed.

**Syntax.**

- `.` lowers only as the argument of `sum(relation.field)`, and inside
  `len(relation.field)`, where the field is ignored. Anywhere else, lowering
  fails ("bare field access in scalar position not supported").
- A call lowers only for the built-in function names.
- A bare `anchor`, `candidate` or `exists` in a predicate reads an input of
  that name.

**The corpus.** On 2026-10-13, all 42 `rulespec-*` repositories at
`origin/main` held two derived relations:

- `calfresh_mce_canonical_member_of_household`, with formula `true`;
- `medicaid_household_member_of_applicant`, whose formula is a tautology.

Neither reads a bare `anchor`, `candidate` or `exists`, or a name that is also
a parameter.

**The cost of these gaps.** Because a predicate cannot read both ends,
`us/policies/medicaid/magi_household_income_pipeline.yaml` in rulespec-us
copies facts onto one synthetic row per (applicant, candidate) pair: 27
`household_row_*` inputs and 11 `household_row_*` rules. Ten of the inputs
are facts about the pair, such as `candidate_is_the_applicant`,
`lives_with_applicant` and `is_applicants_spouse`. All ten are Boolean.

## 3. Constructs

### 3.1 Terms

Inside a derived relation's predicate, a **term** names an entity id:

- `anchor`: the id the relation is read from (its current slot);
- `candidate`: the id being tested (its related slot);
- a variable bound by `exists` (3.6).

Every term has a kind. `anchor` and `candidate` take the derivation's
`slot_entities` at its current and related slots; a variable takes the kind of
the slot it binds. The constructs below check kinds, so a derived relation
whose predicate uses any of them must declare `slot_entities`
(`relation_roles_need_slot_kinds`). Stamped kinds do not help a same-kind
relation, because direction resolution never stamps one.

`anchor` and `candidate` are reserved inside derived-relation predicates. So
is every variable name bound anywhere in the predicate. A reserved name
written bare, as if it were a value or an input, refuses
(`relation_term_used_as_value`). A term may appear only:

- before `.` in a qualified read;
- as an atom's argument;
- in a term comparison.

Outside predicates, these names stay ordinary identifiers. Section 2 shows
that no corpus predicate uses them.

### 3.2 Qualified reads

`t.name`, where `t` is a term and `name` is a derived rule or an input:

- **Rule.** The rule is evaluated at `t`'s id. Its entity must be `t`'s kind
  (`relation_role_entity_mismatch`).
- **Input.** The input is read at `t`'s id. Inputs have no declared kind
  (section 2), so there is no kind check. A misspelt input compiles and fails
  at evaluation as a missing input, the same as a bare input today. Once
  RuleSpec `inputs:` declarations carry an entity with meaning, this read can
  check it. That is not part of this work.

A qualified read of a relation name refuses; use an atom. An input read only
through `t.name` enters the input catalog as a bare read does, under the
owning relation's request name.

A qualified read names a value at an id. It is not a scope for an expression
(there is no `anchor.(a + b)`), so rule values stay cached by
`(rule, id, period)` as today.

A qualified read may appear anywhere in a predicate that a value may:
arithmetic, comparisons, parameter indices, `if`, and inside an `exists` body.
It may not appear inside a `count`/`sum` `where` clause or summed value. Those
have no relation context today and keep none (`relation_role_outside_scope`).

Unqualified reads keep today's routing, so every existing program compiles to
the same artifact.

### 3.3 Parameters in predicates

Predicate lowering receives the module's parameter names, as rule lowering
does. A predicate may then use `p[index]` and a bare scalar parameter. The
survey in section 2 found no predicate that reads an input sharing a
parameter's name, so no existing program changes meaning.

### 3.4 Relation atoms

`R(t1, ..., tn)`, where `R` names a declared relation of arity `n` and each
`ti` is a term. It is a judgment that holds when the tuple
`(id(t1), ..., id(tn))` holds in the period.

- **Data relation.** A supplied tuple with exactly these ids whose interval
  contains the period.
- **Derived relation.** Arity 2 only. With current slot `c` and related slot
  `r`, it holds when `id(t_r)` is a member of `R` for `id(t_c)`.

Arguments are written in `R`'s slot order. That fixes the direction for a
same-kind relation, which a bare membership test cannot do. Each `ti` must
have `R`'s slot kind for slot `i`, and `R` must have declared slot kinds
(`relation_atom_needs_typed_relation`, `relation_atom_kind_mismatch`).

**Supplied tuples.** A request that supplies tuples under the name of a
derived relation which the program reads through an atom, an `exists` domain
or a path step is refused at binding (`supplied_tuples_for_derived_relation`).
Decision d1265 keeps this refusal (`unit-derivation.md` section 15). Bare
membership tests and aggregates keep today's behaviour, which adds such tuples
to the members, so existing programs do not change.

**Cycles.** An atom naming the relation it appears in, or any relation that
depends on it, closes a cycle. The existing cycle check refuses it, since
atoms add edges to its graph, until #231 allows positive recursion.

A bare relation name keeps today's meaning. Where its slots are fixed, `R` and
the atom over `(anchor, candidate)` in those slots agree, on data that
supplies no tuples under a derived relation's name (section 7).

A call whose name is a relation is an atom. A relation whose name is also a
built-in function name (`len`, `sum`, `max` and the others) cannot be used as
an atom (`relation_name_shadows_builtin`).

### 3.5 Term comparison

`t1 == t2` and `t1 != t2`, where both sides are terms of the same kind,
compare ids. The result is always determined, because ids are given, not
derived. It expresses "a member other than his or her parent" in 7 CFR
273.1(b)(1)(iii), and "anyone but the applicant" in a quantified set.

### 3.6 Bounded `exists`

`exists(G, body)`. `G` is an atom with exactly one argument that is a new
variable name; every other argument is a term in scope. The variable ranges
over the ids in that slot of `G`'s tuples that match the other arguments in
the period: distinct, in sorted order. `body` is a judgment in which the
variable is in scope.

- For a data relation, the variable may be in any slot.
- For a derived relation, the variable must be in its related slot, so the
  domain is the relation's members for the id in its current slot. A variable
  in the current slot would traverse the relation backwards, and refuses
  (`inverse_derived_traversal_unsupported`).
- A variable name is a lowercase identifier. It may not be:
  - `anchor` or `candidate`;
  - the name of a rule, parameter or relation the module can see;
  - a variable already bound by an enclosing `exists`.

  Any of these refuses (`relation_variable_name_conflict`).
- Section 3.1 reserves every bound name in the whole predicate. So a variable
  used outside its body refuses (`relation_variable_out_of_scope`) instead of
  reading an input of that name.
- `exists` nests. Its nodes count toward any bound on formula or expression
  size or depth (#240, #245) as other nodes do.

"For all" is `not exists(G, not body)`.

Every domain is the set of ids found in tuples or members, so it is finite by
construction. There is no separate safety analysis.

**Semantics** (today's two values). `exists` holds when `body` holds for some
id in the domain, and does not hold for an empty domain. It is evaluated as
`or` over the domain in sorted order: it stops at the first id for which the
body holds, and a failure propagates as it does from `or`. It is therefore
equal to the disjunction obtained by writing out `body` once per id of the
domain, in sorted order.

**Resume.** `exists` is a resumable site, like a fold. Its saved progress is
keyed by the node's address, the anchor id, the candidate id, every bound
variable's id, the period and the parent evaluation. One rule evaluation can
resolve the same relation for several anchors, for example
`required_combination(p, candidate)` inside an `exists` over `p`, so a key
without all of these could return one anchor's result for another.

### 3.7 Path sources

A derived relation may take its candidates from a path instead of one
relation:

```yaml
- name: lives_with
  kind: derived_relation
  derived_relation:
    arity: 2
    slot_entities: [Person, Person]
    current_slot: 0
    related_slot: 1
    source_path:
      - {relation: member_of_household, from_slot: 0, to_slot: 1}
      - {relation: member_of_household, from_slot: 1, to_slot: 0}
  versions:
    - effective_from: 2026-01-01
      formula: true
```

- **Steps.** Each step reads a binary relation, data or derived, from one slot
  to the other. Each step's `to` kind is the next step's `from` kind. The
  first `from` kind is the anchor's, and the last `to` kind is the
  candidate's.
- **Derived steps.** A derived step is read forward only: `from_slot` is its
  current slot and `to_slot` its related slot.
- **Candidates.** The candidates for an anchor are the ids the path reaches
  from it: distinct, in sorted order. Intermediate ids are not kept. With each
  atom written in its relation's slot order, candidate `c` is reached from
  `a` exactly when `exists(member_of_household(a, h),
  member_of_household(c, h))` holds. The general form has one `exists` per
  intermediate id. A differential test checks this equivalence (section 7).
- **Membership.** A candidate's membership is the `and` of its reachability
  and the predicate, as for a single source.
- **Exclusive.** `source_relation` and `source_path` are exclusive, and one is
  required.
- **Required declarations.** A path source requires `arity: 2`,
  `slot_entities`, `current_slot` and `related_slot`, with the two slots
  different. The legacy (1, 0) default does not apply.
- **Refused fields.** A path source refuses `entity` and `member_relation`.
  The filtered-entity alias they create takes its runtime id from a single
  source's current id.
- **Supplied tuples.** Refused under 3.4.

Why: datasets supply a residence roster such as
`member_of_household(Person, Household)`. A relation anchored on an applicant
reaches the applicant's co-residents through the household. Without a path
source, the caller would have to supply every pair of co-residents.

### 3.8 Elections

An election is an input the law lets a named actor choose, such as separate
household status under 7 CFR 273.1(b)(2). Its declaration (`election: {actor,
provision}`, no default) and its place in the artifact interface block belong
to #155. The engine evaluates one set of supplied elections. Choosing the best
set belongs to the simulation layer (#134, 2026-10-10).

Some elections are about a pair. Under (b)(3)(ii), for example, the household
providing board requests that a particular boarder be included. Such an
election is a binary relation, not a slot input. Whether #155's declaration
covers relations is open on that work; nothing here depends on the answer.

## 4. Under the three-valued option (#215)

Under #215's `missing_facts: "indeterminate"`, each construct reduces to forms
#215 already defines.

**Reads and comparisons.**

- **Qualified read.** An input read or a rule evaluation at the term's id. It
  originates unknown exactly as an unqualified read does (#215 section 5.3).
- **Term comparison.** Always holds or does not hold.

**Atoms.**

- **Atom on a base relation.** A listed tuple holds. An absent tuple does not
  hold if the relation is declared complete. Otherwise it is unknown, with a
  completeness need naming the relation and the id of the atom's
  lowest-numbered argument.
- **Atom on a derived relation.** The membership of `id(t_r)` for `id(t_c)`
  (#215 section 5.7). `in` holds, `out` does not hold, unknown is unknown
  with its needs, and failed fails.

**`exists`.**

- **Value.** The strong Kleene `or` of #215 section 5.4 over the listed ids
  `x` of the domain, of `membership(x) and body(x)`. When the domain's
  relation is open, the `or` has one more item: unknown, with a completeness
  need naming the relation and the id of the generator's lowest-numbered bound
  argument. So `exists`:
  - holds if some id with membership `in` makes the body hold;
  - does not hold if the domain is not open and every listed id is `out` or
    makes the body not hold;
  - is unknown otherwise.
- **Failures.** Each item of that `or` is a catching position. This series
  adds it to the exhaustive list in #215 section 5.9.
- **Resume.** The saved progress (3.6) carries the needs and held failures
  gathered so far, as #215 section 5.10 requires of every saved loop state.

**Path source.** A candidate's reachability is the `or`, over the paths that
reach it, of the `and` of each step's membership. Its membership is the `and`
of that reachability with the predicate (#215 section 5.7). The relation is
open when any step relation it reads is open. The need names that relation and
the id the step was read from.

Each of these is the strong Kleene evaluation of a finite `and`/`or` formula
over determined ids, which #215 section 7 shows sound.

**Order of landing.**

- #215's plan makes every relation read refuse under the option until its
  third PR gives relations open-world semantics. The same holds here: under
  the option, atoms, `exists` and path sources refuse by name until that PR
  lands.
- Qualified reads and term comparisons are not relation reads. They follow
  #215's first PR (#251).
- Whichever of this series and the matching #215 PR lands second lifts the
  new expression kinds.

## 5. Execution paths

- **Explain.** As specified above.
- **Fast.** Bulk evaluates relation aggregations row by row on the explain
  interpreter, so derived-relation predicates run there with the same
  semantics. The new constructs occur only inside predicates, which bulk never
  evaluates directly.
- **Dense.** Refuses, by name, a program whose reachable predicates use a
  qualified read, an atom, a term comparison, `exists` or a path source:
  `DenseCompileError::Unsupported`, naming the construct and #218.
- **Trace.** A qualified read records its dependency at the term's id.
  - **Evaluated path.** Recorded as it is evaluated.
  - **Skipped path** (`record_skipped_*`). A skipped read of `anchor` or
    `candidate` is recorded at that term's id. Nothing is recorded for reads of
    a variable inside an `exists` that is not evaluated, because no id is
    bound; finding one would mean evaluating a relation on the skip path.
  - **Early stop.** When `exists` stops early, each remaining id of its domain
    gets the skip records that the body's reads at that id would produce. This
    matches the trace of the written-out disjunction.
- **Cost.** An atom on a derived relation computes that relation's members for
  another anchor. Members are not cached between drives
  (`Engine::drive` clears them), so a household predicate with an `exists`
  over co-residents and a derived atom inside costs about O(n³) for n
  co-residents per anchor, inside the #240/#245 bounds. #211's member cache
  reduces this.
- **Failure spread.** In two-valued mode, one co-resident's missing input fails
  every membership that reads it. Encodings should put pair atoms before the
  value reads they guard, because `and` stops at the first `not_holds`.

## 6. Engine changes

**Model.**

- `Term { Anchor, Candidate, Var(index) }`, with a variable's index counted
  from the innermost enclosing `exists`.
- New variants:
  - `ScalarExpr::RoleInput { term, name }`;
  - `ScalarExpr::RoleDerived { term, name }`;
  - `JudgmentExpr::RoleDerived { term, name }`;
  - `JudgmentExpr::RelationAtom { relation, terms }`;
  - `JudgmentExpr::SameEntity { left, right, negated }`;
  - `JudgmentExpr::Exists { relation, terms, variable_slot, body }`.
- `RelationDerivation` gains a path source. `source_relation` becomes optional
  in the artifact and is omitted exactly when a path is present.

**Formula.**

- Parse and lower `t.name` on a term, calls whose name is a relation,
  `exists(atom, body)`, and comparisons between two terms.
- The lowering context carries the declared relation names, the parameter
  names (3.3), the reserved names (3.1), and the variables in scope.

**Engine.**

- **Bindings.** `RelationEvalContext` becomes a binding environment: the
  anchor's and candidate's ids and kinds, as now, plus a borrowed chain of
  frames for bound variables. Unqualified routing reads only the anchor and
  candidate, as now.
- **`exists`.** A new resumable site, keyed as in 3.6.
- **Paths.** Path candidates are generated inside the existing members site.
- **Supplied tuples.** Request binding refuses them as in 3.4.

**Compile.**

- **Checks.** Kind checks and the refusals named in section 3.
- **Edges and order.** Every dependency walk and order walk gains edges:
  - for rules read through a term: rule edges, as `collect_judgment_dependencies`
    gives today for unqualified reads;
  - for atoms, `exists` domains and path steps: relation edges.

  A missed rule edge would let a cycle through a relation reach the stack
  overflow `reject_relation_routed_cycles` exists to prevent, so a refusal test
  covers it.

**Every walk that reads relation names or `source_relation`.** Each must
handle atoms, domains and paths:

- RuleSpec alias rewriting and used-relation collection (`src/rulespec.rs`).
  Otherwise an imported relation used only in an atom is pruned.
- `relation_typing.rs`: `effective_slot_entities` and the derivation checks.
- `relation_direction.rs`: `kinds_of` and slot resolution.
- `migrate.rs`.
- The fast-path metadata walk in `compile.rs`, which is written into
  artifacts.
- Dense's derivation walk (`derivation_of` and its callers).
- `unit_derivation/interface.rs`, while that module exists (#232 deletes it).

**Input catalog.** `collect_input_slots_from_judgment_expr` collects inputs
read through a term (3.2).

**Trace.** As in section 5.

**Capabilities.** `relation_roles`, listed once the series is complete.

**Interface block (#155).** A derived relation's provenance there is
`{kind: derived, rule, source_relation, reads: {inputs, rules, relations}}`.

- Atoms and `exists` domains add their relations to `reads.relations`, as bare
  membership tests do.
- A path source adds a `source_path` form beside `source_relation`.

These changes ride the PRs that add the constructs.

**Artifact.**

- Every addition is an enum variant, or a path source that omits
  `source_relation`. The expression enums have no catch-all variant, and
  `source_relation` is required. So an engine without these additions refuses
  an artifact that uses them when it loads it.
- A test checks that a path artifact never also writes `source_relation`.
  `RelationDerivationSpec` does not deny unknown fields, so writing both would
  let an older engine silently ignore the path.
- A program that uses none of the additions compiles to the same bytes as
  before.
- If #155's format 3 (#250) merges first, this work adds to format 3 and
  needs no separate version bump.

## 7. Invariants

Each PR states the invariants it touches and adds property tests (proptest)
and differential tests for them.

1. **Order.** Permuting input records, relation tuples or roster order changes
   no output and no trace.
2. **`exists` expands.** `exists(G, body)` equals the disjunction of `body`
   over its sorted domain. The language has no literal id, so the test builds
   the expansion at the model level, with test-only id terms. Under the
   three-valued option the expansion includes the extra unknown item for an
   open domain.
3. **Atoms agree with bare tests.** Where a bare membership test's slots are
   fixed, it equals the atom over `(anchor, candidate)` in those slots. This
   holds on data that supplies no tuples under a derived relation's name.
4. **Qualified reads agree with routing.** Where today's routing reads the
   same id, an unqualified read equals the qualified one.
5. **Paths agree with `exists`.** A path source's members equal the members
   computed with nested `exists` over its steps. The harness supplies a
   candidate source that covers every id, which the language cannot express.
6. **Monotone in tuples.** Take a predicate that reads relations only through
   atoms and `exists` domains on base relations, each under an even number of
   `not`s. Adding tuples to those relations never removes a member. Nothing is
   claimed for a predicate that:
   - reads a derived relation;
   - reads a rule that aggregates;
   - compares a count;
   - uses a bare membership test.

   #253 defines polarity through derived relations for recursion.
7. **Deferral transparency.** Results and traces are identical at every
   suspend depth (`depth::with_suspend_depth`; compare
   `tests/deferral_transparency.rs`). The resumable `exists` site is the
   riskiest code in the series.
8. **Determinism.** Two runs produce identical output and trace bytes.
9. **Modes agree.** Explain and fast agree on every test program, and dense
   refuses by name.
10. **Byte identity.** Every engine fixture, and every module of rulespec-us,
    rulespec-nz, rulespec-uk and rulespec-ca at pinned commits, compiles to
    the same artifact bytes before and after each PR.
11. **Kinds.** No compiled rule read, atom or domain meets a slot or entity of
    a kind other than its term's. Input reads are untyped (3.2). This is
    checked by refusal tests, plus a property over generated programs.
12. **Termination.** Every domain is finite, and recursion stays refused.

With #215:

- when every input is supplied and every base relation is declared complete,
  nothing is unknown;
- supplying a missing fact or declaring a relation complete never changes a
  determined result.

## 8. Worked example: 7 CFR 273.1, common case

This is a synthetic engine fixture, not a RuleSpec module. The pilot module in
rulespec-us#1428 comes from the encoder. The fixture shows the constructs on
the pilot's scope:

- the meal group under (a)(3);
- the required combinations of (b)(1)(i) and (b)(1)(ii);
- the (b)(7) participation filter, kept apart from composition.

**The household is a closure.** (a)(3) puts a meal group together.
(b)(1)(i) and (b)(1)(ii) require certain people to be "included in the same
household", whether or not they eat together. Together these make the
household the set of co-residents connected to the applicant through ties. A
tie is either a shared meal group or a required combination.

The chain can be any length, so this note cannot compute the household exactly
for every case. #231's recursion can. Until then, the fixture computes chains
of up to two ties exactly. When the household might reach further, it reads an
input with no default: in two-valued mode a missing input fails evaluation
loudly, and under #215 it is unknown. It never returns a smaller household
silently.

```yaml
- name: required_combination          # 273.1(b)(1)(i)-(ii), between two people
  kind: derived_relation
  derived_relation:
    arity: 2
    slot_entities: [Person, Person]
    current_slot: 0
    related_slot: 1
    source_relation: lives_with       # section 3.7
  versions:
    - effective_from: 2026-01-01
      formula: >
        spouse_of(anchor, candidate) or spouse_of(candidate, anchor)
        or (parent_of(anchor, candidate) and candidate.age < 22)
        or (parent_of(candidate, anchor) and anchor.age < 22)

- name: snap_tie                      # (a)(3) meal group, or a (b)(1) combination
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate.meal_group == anchor.meal_group
        or required_combination(anchor, candidate)

- name: snap_within_two_ties          # exact for chains of up to two ties
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate == anchor
        or snap_tie(anchor, candidate)
        or exists(snap_tie(anchor, p), snap_tie(p, candidate))

- name: snap_household_member         # composition, anchored on the applicant
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        snap_within_two_ties(anchor, candidate)
        or (exists(snap_within_two_ties(anchor, p),
                   exists(snap_tie(p, r), not snap_within_two_ties(anchor, r)))
            and candidate.snap_member_through_longer_chain)

- name: snap_participating_member     # (b)(7): composition stays as it is
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1,
                     source_relation: snap_household_member}
  versions:
    - effective_from: 2026-01-01
      formula: not candidate.snap_ineligible_member_under_b7
```

**The guard.** The second clause of `snap_household_member` asks whether
anyone within two ties has a tie to someone outside that set. If nobody does,
the set is closed under ties and is the exact household. Then `and` stops
before the input, and no input is read. If someone does, every candidate not
already admitted reads `snap_member_through_longer_chain`, which has no
default. That input is keyed by the candidate alone, so it is exact only when
one applicant per residence is evaluated. #231 removes the guard and the input.

**Inputs.**

- `parent_of` stands for a natural, adoptive or step-parent.
- `meal_group` is the group a person customarily buys food and prepares meals
  with.
- `snap_ineligible_member_under_b7` (no default) says the person is one of
  those (b)(7) bars "as a member of any household".

The (b)(2)–(b)(6) and (b)(7)(vi) exceptions follow the guard pattern: a person
who may fall under one reads an input with no default until those paragraphs
are encoded. The pair atoms come before the value reads they guard, so a
co-resident's missing age is read only for a parent-child pair.

**Hand-worked cases.** The fixture's tests check memberships against these
households, each worked out by hand from the regulation's text:

| Household | Expected result |
|---|---|
| A married couple who buy food separately | One household, by (b)(1)(i) |
| Roommates who buy food separately | Each alone |
| Applicant, their child under 22, and the child's other parent, who is not the applicant's spouse and buys food separately | All three are one household. (b)(1)(ii) ties the child to each parent; two ties. |
| Applicant married to B; B buys and prepares food with roommate X; the applicant buys separately | All three are one household. A required tie, then a meal-group tie. |
| Applicant, their child C under 22, C's other parent P, and P's child D under 22 from another relationship; everyone buys separately | All four are one household, by three ties. In two-valued mode, evaluating this household fails, naming the missing input, until the input is supplied for D. |
| The applicant shares a meal group with R, a student ineligible under (b)(7)(i); R's spouse S buys food separately | All three are composition members. R is not a participating member; S is. |

## 9. Plan

Five pull requests, in order. Each has:

- a test that fails before it;
- the full suite green with and without the `unit-derivation` feature;
- the byte-identity check of invariant 10;
- its invariants in the PR body, with their tests.

Each is sized for one build lane.

1. **Terms, qualified reads and parameters.** The model, parsing, lowering,
   reserved names, the kind check for rules, the binding environment, rule
   edges in every walk, the input catalog, trace routing, the dense refusal,
   and the `docs/rulespec.md` section.
2. **Atoms and term comparison.** Relation edges, kind checks, the built-in
   name refusal, the supplied-tuple refusal, and the differential test against
   bare membership tests.
3. **Bounded `exists`.** Variables and scope, the resumable site and its key,
   the refusals, deferral transparency, and the expansion test.
4. **Path sources.** The artifact form, candidate generation, every walk in
   section 6, and the differential test against `exists`. This lands after
   #211 and #249, or with an explicit merge plan for them, because those PRs
   rewrite the single-source walks (`kinds_of`, `effective_slot_entities`,
   dense's `derivation_of`) that path sources turn into graphs.
5. **Fixture and docs.** The 273.1 fixture and its hand-worked cases, the
   `relation_roles` capability, `docs/execution-semantics.md`, and a
   `DECISIONS.md` entry.

**Next steps.**

1. With #215's third PR, lift atoms, `exists` and path sources under the
   three-valued option. Whichever of #251 and this series lands second lifts
   the new expression kinds.
2. Recursion (#253, then #231) makes the household exact.
3. The encoder (TheAxiomFoundation/axiom-encode#1715) learns the constructs.
4. The pilot (TheAxiomFoundation/rulespec-us#1428) is encoded. It should
   follow #231, so that its households are exact rather than guarded.

**Dependencies.**

- This series depends on neither #155 nor #215 for its two-valued form.
- It adds to artifact format 3 if #250 merges first.
- #253 is stacked on this note, and builds on section 7's limits on
  monotonicity.

## 10. Choices made here that the issue does not dictate

- **Atoms are written `R(a, b)`, not `holds(R, a, b)`.** The form reads like
  the regulation ("spouse of"), fixes slot order, and is the form #231 needs.
  For 42 CFR 435.603(f)(2), the household of the taxpayer who claims the
  applicant, the predicate would read
  `exists(claims_as_dependent(t, anchor), medicaid_household(t, candidate))`.
- **`exists` takes a generator atom, not a free variable.** Every domain is
  finite by construction.
- **Pair facts are Boolean relations.** The ten pair facts the Medicaid
  pipeline copies are all Boolean, so the first two encodings need no valued
  pair facts. A synthetic pair entity covers the case until one does.
- **Derived relations are traversed forward only.** Backward traversal needs
  every possible anchor's members, and nothing needs it yet.
- **Qualified reads name a value, not an expression scope,** so rule caching
  keeps its key.
- **Inputs stay untyped.** A kind check for inputs waits for RuleSpec input
  declarations to carry meaning.
- **Atoms refuse supplied tuples for derived relations; bare tests do not.**
  The new constructs keep d1265's refusal, and existing programs keep their
  behaviour.
- **Unqualified reads do not change,** so no existing artifact changes.
