# Relation roles, atoms and bounded joins

Design for #214, step 3 of the unit-derivation plan. Decision d1265 (2026-10-10,
recorded on #134 and in `unit-derivation.md` section 15): a legal unit is a
derived relation from an anchor to persons, written by extending
`derived_relation`. The series is complete; section 9 records what landed.

## 1. Purpose

A unit's membership test needs five things that a derived-relation predicate
could not do before this series:

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

This section was checked against 621e0f24. Since then #249 (bfebac66) has
made chain typing, direction and binding linear. Its property tests check the
new walks against the old ones, so the behaviour below is unchanged.

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
that no corpus predicate uses them. No declaration may take `anchor` or
`candidate` as its name ([rulespec.md](rulespec.md#declaration-names)), so a
term never hides one.

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

Unqualified reads keep today's routing. Five changes affect existing programs:
- A bare parameter name in a predicate, or tested as a judgment in a rule
  body, used to read an input of that name and now reads the parameter (3.3).
  That held when nothing else had the name. A parameter that shared a rule's
  name read the rule, and in a predicate one that shared a relation's name
  tested membership; such a program is now refused at declaration (the
  declaration-name change below).
- A bare `period_start` or `period_end` tested as a judgment used to resolve
  as any bare name did: to a rule of that name, else (in a predicate) a
  relation, else an input. It is now refused. Both are dates. Scalar position
  reads the built-in before anything else of the same name, and judgment
  position now does the same.
- A bare `anchor` or `candidate` in a derived relation's predicate was an
  ordinary name and is now refused (3.1). Read through `.`, a relation of
  either name used to mean the relation: `sum(anchor.age)` aggregated over
  it and is now refused, and `min(anchor.age)` took its minimum and now
  reads the anchor's own `age`. Such a relation can no longer be declared
  (next change), so no program that compiles keeps either old meaning.
- A declaration that a bare name cannot read is refused before lowering
  (`docs/rulespec.md`, "Declaration names"). That covers a parameter, rule
  or relation named `period_start`, `period_end`, `anchor` or `candidate`,
  and a relation named after a formula keyword such as `true`
  (`reserved_declaration_name`). It also covers a name declared as two of a
  parameter, a rule and a relation (`declaration_name_clash`). Such a
  program used to compile, and one bare name reached only one of its
  declarations. On 2026-10-27 no corpus module
  declares a reserved name, and one declares a clash (rulespec-us#1953); main
  already refuses that module for another reason.
- A `parameter` record with an entity or a computed formula lowers to a
  rule. A predicate used to read its bare name, and `t.name`, as an input of
  that name. It now reads the rule, as a rule body does (3.3). On 2026-10-27
  the corpus has 299 such records and no predicate that reads one.

### 3.3 Parameters in predicates

Predicate lowering receives the same set of parameter names that rule
lowering receives for the module. That set is:
- the table parameters;
- the scalar parameters;
- the entity-free rules with literal values, which lowering turns into
  parameters.

A predicate may then use `p[index]` and a bare scalar parameter. The survey in
section 2 found no predicate that reads an input sharing a parameter's name,
so no existing program changes meaning. Dense already compiles a parameter
lookup inside a predicate, so this needs no dense refusal.

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

**Supplied tuples.** Decision d1265 keeps entry 5 of the ratified contract:
a supplied tuple for a relation the program derives is refused
(`unit-derivation.md` section 15). Today the engine instead adds such tuples
to the members. In any program that uses a qualified read (3.2), an atom or
a term comparison (3.5), both of these change:
- Request binding refuses tuples supplied under the name of any derived
  relation (`supplied_tuples_for_derived_relation`).
- The members resolver never adds a derived relation's own tuples. This also
  covers Rust callers of `Engine::new`, which skips binding checks.

Section 2's survey found no program that uses these constructs, so no existing
program changes behaviour or bytes. Programs that use none of them keep
today's union. Parameters in predicates (3.3) do not opt in: a raw
`ProgramSpec` predicate could always look up a parameter, and such a program
must keep today's union.

**Lenient binding.** Lenient binding keeps tuples whose kinds do not match
their slots, which a lookup then misses. Request binding refuses lenient
binding for a program that uses an atom, `exists` or a path source
(`lenient_binding_with_relation_roles`). #215 refuses it under its option for
the same reason.

**Cycles.** An atom naming the relation it appears in, or any relation that
depends on it, closes a cycle. The existing cycle check refuses it, since
atoms add edges to its graph, until #231 allows positive recursion.

A bare relation name keeps today's meaning. Where its slots are fixed, `R` and
the atom over `(anchor, candidate)` in those slots agree (section 7).

A call whose name is a relation is an atom. A relation whose name is also a
built-in function name (`len`, `sum`, `max` and the others) cannot be used as
an atom (`relation_name_shadows_builtin`). A call to that name still calls the
built-in, as it did before atoms; passing it a term, which only an atom
takes, is refused.

### 3.5 Term comparison

`t1 == t2` and `t1 != t2`, where both sides are terms of the same kind,
compare ids. Different kinds refuse with `relation_term_kind_mismatch`; a
term compared with a value refuses with `relation_term_compared_with_value`.
Other comparison operators refuse with `relation_term_comparison_operator`.
The result is always determined, because ids are given, not derived. It
expresses "a member other than his or her parent" in 7 CFR
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
- **Failures.** A path resolves every id it reaches before it tests a
  candidate, so a failure at any reached id fails the anchor's members. An
  `exists` over the same steps stops at its first success and can miss that
  id.
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
  completeness need naming the relation and the id whose tuple list was read:
  the atom's first argument (slot 0). Completeness is declared per relation,
  so any need naming the relation is met by one declaration. The id only tells
  the caller where the gap showed up.
- **Atom on a derived relation.** The membership of `id(t_r)` for `id(t_c)`
  (#215 section 5.7). `in` holds, `out` does not hold, unknown is unknown
  with its needs, and failed fails.

**`exists`.**

- **Value.** The strong Kleene `or` of #215 section 5.4 over the listed ids
  `x` of the domain, of `membership(x) and body(x)`. When the domain is open,
  the `or` has one more item: unknown, with the domain's open need. For a base
  relation, that need names the relation and the id whose tuple list was read,
  which is the generator's lowest-numbered bound argument. For a derived
  relation, it is that relation's own open need, which #215 section 5.7 says
  names a base relation. So `exists`:
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
open when any step it reads is open:
- for a base step, the need names that relation and the id the step was read
  from;
- for a derived step, the need is that step's own open need, which names a
  base relation.

Each of these is the strong Kleene evaluation of a finite `and`/`or` formula
over determined ids, which #215 section 7 shows sound.

**Order of landing.**

- #215's plan makes every relation read refuse under the option until its
  third PR gives relations open-world semantics.
- A predicate runs only when a relation is read. So under the option, every
  construct in this note refuses by name until that PR lands.
- Two steps, each done by whichever of its two PRs lands second:
  - Each new expression kind gets code-level handling under the option, for
    now a named refusal. This is the later of #251 (#215's first PR) and the
    PR in this series that adds the kind.
  - The refusals lift and this section's semantics take effect. This is the
    later of #215's third PR and this series.

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
  - **Skipped path** (`record_skipped_*`).
    - A skipped read of `anchor` or `candidate` is recorded at that term's id.
    - A skipped read of a variable bound by an enclosing `exists` that is being
      evaluated is recorded at the id the variable is bound to.
    - A skipped read of a variable bound by an `exists` that is not itself
      evaluated is not recorded. No id is bound, and finding one would mean
      evaluating a relation on the skip path.
    - A wholly skipped `exists` records its body's `anchor` and `candidate`
      reads once, whatever its domain holds. The written-out disjunction over
      an empty domain would record nothing, so invariant 2 covers values and
      needs, not traces.
  - **Early stop.** When `exists` stops early, each remaining id of its domain
    gets the skip records that the body's reads at that id would produce. This
    matches the trace of the written-out disjunction.
- **Cost.** An atom on a derived relation computes that relation's members for
  another anchor. The engine keeps each anchor's members for its lifetime
  (`Engine::drive` leaves `derived_members` in place), so one engine resolves a
  relation's anchor once per period. Every atom still copies those members into
  a fresh sorted vector before its binary search. A household predicate with an
  `exists` over n co-residents and a derived atom inside runs n² atoms over n
  members. That copies about n³ ids per anchor, plus one members resolution per
  co-resident, inside the #240/#245 bounds. The engine evaluates an `exists`
  whose result depends only on the anchor, such as section 8's guard, again for
  every candidate, because its resume key includes the candidate (3.6). A path
  source copies and sorts each current id's related ids, then sorts what the
  step reached. A step that reaches r ids, repeats included, takes about
  r log r time, and a derived step adds one members resolution per current id.
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
- **Supplied tuples.** In a program that uses these constructs, request
  binding refuses tuples supplied under a derived relation's name, and the
  members resolver never adds them (3.4).

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
- `relation_typing.rs`: `effective_slot_entities`, the `EffectiveSlotEntities`
  table and `follow_chains` (#249), and the derivation checks.
- `relation_direction.rs`: `kinds_of` and slot resolution.
- `migrate.rs`.
- The fast-path metadata walk in `compile.rs`, which is written into
  artifacts.
- Dense's derivation walk (`derivation_of` and its callers).
- `unit_derivation/interface.rs`, while that module exists (#232 deletes it).
- `python/axiom_rules_engine`: the rule-shape check in `concepts.py` and the
  provenance union in `models.py`.

**Input catalog.** `collect_input_slots_from_judgment_expr` collects inputs
read through a term (3.2).

**Trace.** As in section 5.

**Capabilities.** `relation_roles` lists the completed series: terms and
qualified reads, parameters in predicates, relation atoms, term comparisons,
bounded `exists` and path sources.

**Interface block (#155, PR #250).** A derived relation's provenance there is
`{kind: derived, rule, source_relation, reads: {inputs, rules, relations}}`.
Its walkers in `src/interface.rs` match exhaustively, with no catch-all arm,
so each new variant is a compile error there until it is handled:
- `collect_judgment_relations` / `collect_scalar_relations` build
  `reads.relations`. Atoms and `exists` domains add their relations, as bare
  membership tests do.
- `Collector::judgment` / `Collector::scalar` build `reads.inputs` and
  `reads.rules`, and each input's `entities` and `dtype`.
  - `Routing` carries current-versus-related flow. A read through `anchor`
    routes as current, and a read through `candidate` as related.
  - Atoms' bound slots go through `Collector::lookup`, as membership tests do.
    A derived atom keys on its current slot only, which is the slot the
    engine resolves members for.
- `derive_interface` looks up `source_relation` at the current slot. A path
  source needs a lookup per step, and a `source_path` arm in
  `RelationProvenance`, whose hand-written `Deserialize` checks exact keys per
  kind.
- Predicates that read parameters (3.3) add them to `reads`, if #155 does not
  already list parameters.

The loader re-derives the interface and refuses a mismatch, so the derivation
is frozen for every program a format-3 engine can already load. The new
constructs change only the interfaces of programs that older engines refuse
anyway, so they need no format bump. These changes ride the PRs that add the
constructs.

**Artifact.**

- Every addition is an enum variant, or a path source that omits
  `source_relation`. The expression enums have no catch-all variant, and
  `source_relation` is required. So an engine without these additions refuses
  an artifact that uses them when it loads it.
- A test checks that a path artifact never also writes `source_relation`.
  `RelationDerivationSpec` does not deny unknown fields, so writing both would
  let an older engine silently ignore the path.
- A program that uses none of the additions and makes none of the five
  changes in 3.2 compiles to the same bytes as before.
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
   fixed, it equals the atom over `(anchor, candidate)` in those slots. Under
   #215 the two agree in value and in the relations their needs name; the ids
   in those needs may differ (section 4).
4. **Qualified reads agree with routing.** Where today's routing reads the
   same id, an unqualified read equals the qualified one.
5. **Paths agree with `exists`.** When neither side fails, a path source's
   members equal the members computed with nested `exists` over its steps. The
   harness supplies a candidate source that covers every id, which the
   language cannot express. A path can fail where `exists` succeeds (3.7).
6. **Monotone in tuples.** Take a predicate that reads relations only through
   atoms and `exists` domains on base relations, each under an even number of
   `not`s. Adding tuples to those relations never removes a member, though
   evaluation may then fail: a new domain id or candidate can read a missing
   input. Nothing is claimed for a predicate that:
   - reads a derived relation;
   - reads a rule that aggregates;
   - compares a count;
   - uses a bare membership test.

   #253 defines polarity through derived relations for recursion.
7. **Deferral transparency.** Results and traces are identical at every
   suspend depth (`depth::with_suspend_depth`; compare
   `tests/deferral_transparency.rs` and, for paths,
   `tests/support/relation_paths.rs`). The resumable `exists` site is the
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

`tests/fixtures/snap_household.rulespec.yaml` holds this synthetic engine
fixture, which is not a RuleSpec module. The pilot module in rulespec-us#1428
comes from the encoder. The fixture shows the constructs on the pilot's scope:
- the meal group under (a)(3);
- the required combinations of (b)(1)(i), (b)(1)(ii) and (b)(1)(iii);
- the (b)(7) participation filter, kept apart from composition;
- the (b)(2)–(b)(6) and (b)(7)(vi) exceptions, as inputs with no default.

It must never return a wrong household silently, whether too small or too
large. The engine trusts what the caller supplies, so a wrong age or a false
exception flag gives a wrong household. With every supplied fact true, the
promise holds under three preconditions, which the generated cases meet:
- **One residence for each co-resident.** Everyone who lives in the residence,
  evaluated or not, has exactly one `member_of_household` record, and that
  record covers the whole evaluated period. A person with none, or with records
  that each cover only part of the period, reaches no one and no one reaches
  them. Their own household comes back empty, and a co-resident's comes back
  short. So twelve monthly records do not serve a year query. A person recorded
  in two residences bridges them. `lives_with` stops at each applicant's own
  co-residents, so an applicant on one side misses people on the other, even
  with correct ids. The fixture does not check any of this.
- **Complete pair facts.** A spouse, parent or parental-control tuple that is
  missing, or that covers only part of the period, reads as false, so the
  people it would tie stay apart with no failure. An input fails when missing,
  but a relation tuple does not. #215's declared
  completeness would close this gap.
- **Correct supplied ids,** where ids are read. The engine trusts them. Ids
  that split two tied people give a household that is too small. A check the
  pilot can add: a derived relation of ties between people with different
  ids, evaluated only when ids are supplied, with a rule that flags any such
  tie when no exception applies.

**The household is a closure.** (a)(3) puts a meal group together. (b)(1)
requires certain people to be "included in the same household", whether or
not they eat together. Together these make the household the set of
co-residents connected to the applicant through ties. A tie is either a shared
meal group or a (b)(1) combination.

Every tie is symmetric:
- the spouse and parent tests run in both directions;
- a shared meal group is an equality.

So, when no exception applies, the closures split a residence into disjoint
households, one for each connected group of ties.

The chain can be any length, so this note cannot compute every household.
#231's recursion can. Until then the fixture computes the closure exactly in
the common case, and in every other case it reads household ids that the
caller supplies:

- **Exact.** The fixture computes everyone within two ties of the applicant.
  If nobody in that set has a tie to someone outside it, the set is closed
  under ties and is the household.
- **Supplied.** Otherwise the household is read from
  `snap_household_unit`, an id per person with no default. Two people are in
  one household exactly when their ids are equal. Ids are needed in two
  cases:
  - a tie leads out of the two-tie set, so the chain may be longer;
  - some co-resident may fall under an exception. An exception can make a
    household larger (a boarder included at the provider's request) or
    smaller (a separate household under (b)(2)). Its ordering against (b)(1)
    is one of the contested readings left to tier B.

  In two-valued mode, a missing id fails evaluation and names the input.
  Under #215 it is unknown.

Comparing ids works for any number of applicants in a residence, because each
closure is one household.

```yaml
# Data relations. Each is a supplied pair fact with declared kinds.
- {name: member_of_household, kind: data_relation,
   data_relation: {arity: 2, arguments: [Person, Household]}}
- {name: residence_size, kind: derived, entity: Household, dtype: Integer,   # gives the
   versions: [{effective_from: 2026-01-01, formula: len(member_of_household)}]} # Household kind a rule
- {name: spouse_of, kind: data_relation,
   data_relation: {arity: 2, arguments: [Person, Person]}}
- {name: parent_of, kind: data_relation,                 # natural, adoptive or step
   data_relation: {arity: 2, arguments: [Person, Person]}}
- {name: under_parental_control_of, kind: data_relation, # (child, controller), (b)(1)(iii)
   data_relation: {arity: 2, arguments: [Person, Person]}}

- name: lives_with                    # section 3.7; includes the person themself
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

- name: snap_tie                      # (a)(3), or a (b)(1)(i)-(iii) combination
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate.meal_group == anchor.meal_group
        or spouse_of(anchor, candidate) or spouse_of(candidate, anchor)
        or (parent_of(anchor, candidate) and candidate.age < 22)
        or (parent_of(candidate, anchor) and anchor.age < 22)
        or (under_parental_control_of(candidate, anchor)
            and not parent_of(anchor, candidate) and candidate.age < 18)
        or (under_parental_control_of(anchor, candidate)
            and not parent_of(candidate, anchor) and anchor.age < 18)

- name: snap_within_two_ties
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        candidate == anchor
        or snap_tie(anchor, candidate)
        or exists(snap_tie(anchor, p), snap_tie(p, candidate))

- name: snap_exception_in_residence   # an exception may apply to some co-resident
  kind: derived
  entity: Person
  dtype: Judgment
  versions:
    - effective_from: 2026-01-01
      formula: count_where(lives_with, snap_household_exception_may_apply) > 0

- name: snap_household_member         # composition, anchored on the applicant
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1, source_relation: lives_with}
  versions:
    - effective_from: 2026-01-01
      formula: >
        (snap_within_two_ties(anchor, candidate)
         and not anchor.snap_exception_in_residence
         and not exists(snap_within_two_ties(anchor, p),
                        exists(snap_tie(p, r), not snap_within_two_ties(anchor, r))))
        or ((anchor.snap_exception_in_residence
             or exists(snap_within_two_ties(anchor, p),
                       exists(snap_tie(p, r), not snap_within_two_ties(anchor, r))))
            and candidate.snap_household_unit == anchor.snap_household_unit)

- name: snap_participating_member     # (b)(7); composition stays as it is
  kind: derived_relation
  derived_relation: {arity: 2, slot_entities: [Person, Person],
                     current_slot: 0, related_slot: 1,
                     source_relation: snap_household_member}
  versions:
    - effective_from: 2026-01-01
      formula: not candidate.snap_ineligible_member_under_b7
```

**Inputs.** All of these have no default.
- `meal_group`, a group-valued fact, and `age`. Purchases that form no single
  group (a buys with b and b with c, but a does not buy with c) have no true
  `meal_group`. That is a (c) case: set the exception input and supply ids.
  The fixture still reads `meal_group` and fails if it is missing, so supply
  any value there. With the flag set, the ids alone decide the household.
- `snap_household_exception_may_apply`. The person may be described by
  (b)(2) (60 or older and unable to buy and prepare meals because of a
  disability), (b)(3) or (b)(4) (a board arrangement, including a person
  who pays less than reasonable compensation and whom the regulation does not
  call a boarder, or a foster placement), (b)(5) or (b)(6) (a roomer or live-in attendant),
  (b)(7)(vi) (a resident of an institution, or one of its listed exceptions),
  or (c) (a situation (a) and (b) do not clearly address, which State policy
  decides).
- `snap_household_unit`. The caller's household id, read only in the cases
  above.
- `under_parental_control_of` carries the (b)(1)(iii) determination itself:
  the caller supplies a tuple only when the dependence test holds and State
  law does not treat the child as an adult.
- (b)(1)(iii) excludes a foster child. `snap_tie` does not test for one,
  because a foster placement is a (b)(4) case. It sets
  `snap_household_exception_may_apply`, so that household comes from the
  supplied ids.
- `snap_ineligible_member_under_b7`. The person is barred by (b)(7) "as a
  member of any household".

**Encoding notes.**
- `residence_size` exists only so that the `Household` kind has a rule.
  Without one, compiling warns `UnknownRelationArgumentEntity`, and under
  strict relation-entity mode that warning is an error.
- Pair atoms come before the value reads they guard. A co-resident's age is
  read only for a parent-child or parental-control pair, and the unit id only
  once the supplied case applies.
- `lives_with` comes from the roster, so every evaluated person needs a
  `member_of_household` record. A person living alone, (a)(1), is a residence
  of one. The generated cases supply one full-year record per person.
- The guard's two `exists` depend only on the anchor, but are evaluated again
  for each candidate (section 5, cost). #231 removes the guard and the ids
  that long chains read. The exception cases still read supplied ids until
  an encoding decides those paragraphs.

**Hand-worked cases.** The fixture's tests check memberships against these
households, each worked out by hand from the regulation's text:

| Household | Expected result |
|---|---|
| A person living alone, (a)(1) | One member, the person |
| A married couple who buy food separately | One household, by (b)(1)(i) |
| Roommates who buy food separately | Each alone |
| Applicant, their child under 22, and the child's other parent, who is not the applicant's spouse and buys food separately | All three are one household. (b)(1)(ii) ties the child to each parent; two ties. |
| Applicant married to B; B buys and prepares food with roommate X; the applicant buys separately | All three are one household: a required tie, then a meal-group tie |
| A grandparent applicant with their grandchild, who is 15, under the grandparent's parental control, and recorded with their own meal group | One household, by (b)(1)(iii) |
| Applicant, their child C under 22, C's other parent P, and P's child D under 22 from another relationship; everyone buys separately | All four are one household, by three ties. The guard sees a tie leaving the two-tie set and reads the supplied ids. Missing ids fail evaluation; ids that put all four together give all four. |
| Two separate groups in one residence, each a chain of three ties, with the applicants at the ends of their chains | Each applicant gets exactly their own group from the supplied ids |
| A chain a–b–c–d of three ties, with applicants a and b | b is within two ties of everyone and the set is closed, so b's household is computed exactly and no ids are read. a's household comes from the ids. The two agree when the ids are correct (third precondition above). |
| The applicant shares a meal group with a 64-year-old roommate who may qualify for (b)(2) | The exception input is true, so the household is read from the supplied ids, with no reading of (b)(1) against (b)(2) built in |
| The applicant shares a meal group with R, a student ineligible under (b)(7)(i); R's spouse S buys food separately | All three are composition members. R is not a participating member; S is. |

## 9. Landed series

The series delivers these constructs and contracts in five steps:

1. **Terms, qualified reads and parameters (#268, #269, #273).** Added the
   model, parsing, lowering, reserved and duplicate declaration checks, rule
   kind checks, term bindings, dependency walks, input catalog and trace
   routing, with named dense refusals and the `docs/rulespec.md` contract.
2. **Atoms and term comparison (#276, #279).** Added relation edges, kind
   checks, the built-in name and supplied-tuple refusals, and differential
   tests against bare membership tests. #279 bounded native relation
   nesting while preserving values and traces.
3. **Bounded `exists` (#280, #281).** Added variables and scope, resumable
   quantification, named refusals, deferral transparency and expansion tests.
   #281 checked exact values and unit-local unknowns for phase-two reads.
4. **Path sources (#282).** Added the artifact form, candidate generation,
   dependency walks and differential tests against `exists`. Typing,
   direction, migration, validation and binding follow every path step;
   explain resumes path progress and dense refuses path sources by name.
5. **Fixture and docs (#283).** Added section 8's synthetic 273.1 fixture, its eleven
   hand-worked cases and independent household properties, the
   `relation_roles` capability, `docs/execution-semantics.md` and the
   `DECISIONS.md` entry.

The series extends artifact format 3 and its checked interface block (#250);
it needs no additional format bump.

**Next steps.**

1. Under the three-valued option, follow the two steps in section 4. The
   later of #251 and each PR here adds the named refusals. The later of
   #215's third PR and this series lifts them.
2. The encoder (TheAxiomFoundation/axiom-encode#1715) learns the constructs.
3. The encoder encodes the pilot (TheAxiomFoundation/rulespec-us#1428) in
   section 8's guarded form, which supplied ids complete where the chain or
   an exception leaves the computed case.
4. Recursion (#253, then #231) later computes long chains exactly and
   retires the guard. The exceptions keep their supplied ids until an encoding
   decides those paragraphs, because a (b)(2) separation can split a meal tie.

**Dependencies.**

- This series depends on neither #155 nor #215 for its two-valued form.
- It extends artifact format 3 and its interface block, which #250 landed.
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
- **The supplied-tuple refusal is scoped to programs that use these
  constructs.** Such programs get d1265's refusal for every derived relation.
  Existing programs keep their behaviour.
- **Unqualified reads do not change.** There are five exceptions. A bare
  parameter name in a predicate, or tested as a judgment in a rule body, now
  reads the parameter (3.3). A bare period built-in tested as a judgment is
  now refused (3.2). A bare `anchor` or `candidate` in a derived relation's
  predicate is now refused (3.1). A declaration that a bare name cannot read,
  because a built-in, a term, a keyword or another declaration takes the
  name, is now refused (3.2). A predicate reads a `parameter` record that
  lowers to a rule as that rule, not as an input (3.2).
