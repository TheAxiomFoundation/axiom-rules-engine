# RuleSpec

RuleSpec is the canonical authoring and interchange schema for Axiom Rules
Engine rules. This document covers operational semantics;
[rulespec-format.md](rulespec-format.md) is the expression-grammar reference.
Authoring tools should emit RuleSpec YAML/JSON from Axiom source documents; the
Rust engine normalises it into `ProgramSpec` before compilation. `ProgramSpec` is
the runtime IR, not a RuleSpec module file format.

## Shape

Every RuleSpec YAML file must declare an explicit discriminator:

```yaml
format: rulespec/v1
module:
  title: Texas SNAP overlay
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: medical_deduction
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    sources:
      - citation: 7 CFR 273.9(d)(3)(x)
        url: https://www.ecfr.gov/current/title-7/section-273.9
    versions:
      - effective_from: 2025-10-01
        formula: |
          if has_elderly_or_disabled_member:
              if total_medical_expenses > snap_medical_deduction_threshold:
                  snap_state_sme_flat_amount
              else: 0
          else: 0
```

The removed `schema:` discriminator is rejected even when `format` is also
present. YAML with a top-level `rules:` key and no exact format is rejected.

## Semantics

Supported rule kinds in the current Rust loader:

- `parameter`: no entity-scoped output; literal versions lower to indexed scalar
  parameters through the existing bridge.
- `parameter` rules with `indexed_by` and versioned `values` encode source
  tables/scales as addressable parameter cells. `indexed_by` is required for
  every `values` table. Formulas reference them with `table_name[index_expr]`.
- `derived`: entity-scoped scalar or judgment outputs.
- `data_relation`: executable runtime predicate declarations with
  `data_relation.arity` and a `data_relation.arguments` list declaring one
  entity kind per tuple slot, in tuple order. The list length must equal the
  arity; that mismatch is always a compile error. The list is required for any
  relation a rule executes (see [Relation entity typing](#relation-entity-typing));
  a relation no rule executes may omit it. Usable entity-kind labels are ASCII
  UpperCamelCase/alphanumeric. A shape-failing declaration warns and is treated
  as undeclared (`slot_entities` remains empty). A well-shaped kind absent from
  the import-merged program closure also warns, but remains carried verbatim
  because published modules legitimately declare relation-only kinds. Dataset
  relation records use durable ids such as
  `us:statutes/7/2012/j#relation.member_of_household`.
- `derived_relation`: executable runtime predicates computed by filtering a
  source relation with a judgment formula. This supports filtered membership
  sets such as SNAP units, MAGI households, and qualifying-child sets.
- `source_relation`: legal/provenance edges. It must declare
  `source_relation.type` and `source_relation.target`. Most source relations
  are non-executable metadata; same-kind `sets` records with
  `source_relation.value` lower into delegated parameter bindings or derived
  formula hooks.

`kind` describes the record's schema and lowering behavior. Source/legal
semantics live in `source_relation.type`, not in `kind`. The supported
source-relation types are `defines`, `delegates`, `implements`, `sets`,
`amends`, `restates`, and `cites`. Executable `parameter` and `derived` rules
implicitly define their own durable outputs, so explicit `defines` records are
only needed for span-level provenance that is not otherwise represented by an
executable rule.

Parameter and derived versions may declare an inclusive `effective_to` in
addition to `effective_from`. Runtime selection uses the query period's start
date and chooses the latest version whose complete range contains that date.
After a bounded version expires, it does not remain as a fallback: a gap before
the next version produces the ordinary missing-parameter or missing-derived-
version error. An `effective_to` before its `effective_from` is rejected when
the program is compiled or a compiled artifact is loaded.

Explain and bulk-fast execution honor bounded derived versions directly. The
standalone `DenseCompiledProgram` compiler continues to reject every versioned
derived formula, including a single bounded version; callers using that surface
must use generic execution until dated-derived dense compilation is added.

RuleSpec files must not use top-level `relations:`. Runtime predicates are
normal rule records:

```yaml
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
```

### Relation entity typing

Entity ids are opaque strings, and an aggregate reads a relation by looking
up `(relation, current slot, id)`. Without declared slot kinds, a tuple stored
in the other orientation matches nothing and the aggregate silently returns
zero. Relation entity typing is therefore mandatory:

- **Compile.** Every relation that a `count_related`, `sum_related`, or
  membership node executes, directly or as a derived relation's source, must
  have one entity kind per slot: declared with `data_relation.arguments`
  (lowered to `program.relations[].slot_entities`), or, for a derived
  relation, inherited from its source. The slot a node keys on must hold the
  evaluating entity (a filtered entity, a derived relation's `entity` such as
  `SnapUnit`, counts as its source's current kind), rules evaluated on the
  related ids must have the related slot's kind, and rules a derived
  relation's predicate reads must run on an id of their own entity. A
  derived relation's kinds must match its source's, a chain of derived
  relations must keep one direction, and derived relations filtering to the
  same entity must agree on its kind. Violations are compile errors listing
  each relation and citing rule: `untyped_relation`, `unknown_relation`,
  `relation_slot_entity_count`, `relation_slot_out_of_range`,
  `relation_current_slot_entity_mismatch`,
  `relation_related_slot_entity_mismatch`,
  `relation_membership_slot_entity_mismatch`,
  `relation_predicate_entity_mismatch`, `derived_relation_source_slot_conflict`,
  `derived_relation_slots_diverge`, `filtered_entity_kind_conflict`, and
  `relation_slot_kind_is_filtered_entity` (a slot declares a filtered entity
  such as `SnapUnit` rather than the kind of the ids it is queried with).
  Reads through a predicate's terms add `relation_roles_need_slot_kinds`,
  `relation_role_entity_mismatch`, and `relation_role_outside_scope` (see
  the terms of a derived relation's predicate, below).
- **Load and request.** The same check runs when a compiled artifact loads
  (`from_json_*`, `run-compiled`, the Python and wasm bindings), when a request
  carries a raw `ProgramSpec`, and when the dense compiler builds from a
  program or artifact. `Engine::new` evaluates a `Program` exactly as given;
  Rust callers that build one themselves must call
  `relation_typing::check_program` first.
- **Bind.** Dataset tuples are checked against the declared kinds. An id's
  kind comes from input records that carry both `entity_id` and `entity`, and
  from queries: a query evaluates its output rules on its `entity_id`, so a
  queried household is a `Household` even with no household-level inputs. A
  filtered entity's evidence (a `SnapUnit` query) counts as its source kind.
  Ids with no evidence, or evidence of more than one kind, are skipped. A
  known mismatch is an error by default and a warning under lenient binding
  (see [Binding policy](#binding-policy)). A tuple whose length differs from the
  relation's arity is always an error.

Declared-kind labels must be UpperCamelCase entity kinds. A label that fails
that shape, or names a kind no rule in the import-merged closure uses, is a
warning (`invalid_relation_argument_entity_shape`,
`unknown_relation_argument_entity`) that `strict_relation_entities` promotes to
an error:

```rust
CompiledProgramArtifact::from_rulespec_str_with_options(
    source,
    CompileOptions {
        strict_relation_entities: true,
        ..CompileOptions::default()
    },
)
```

A shape-failing declaration leaves the relation untyped, so executing it is
still an `untyped_relation` error.

#### Binding policy

Dataset binding is strict by default: a known mismatch between a tuple slot's expected entity kind
and the supplied entity id's kind is an error before execution. The error names
the relation, slot, entity id, and expected and supplied kinds so callers can
correct the tuple and input entity labels.

Expected tuple positions are the relation's declared slot kinds (typing makes
them agree with every executed use). Kind evidence is described under **Bind**
above: input records and queries. Ids with no evidence, or ids used with more
than one kind, remain unknown and are skipped. `Entity` is an ordinary supplied
kind, not a wildcard for `Person`, `TaxUnit`, or another expected kind.

Both `ExecutionRequest` and `CompiledExecutionRequest` accept a top-level
`"relation_binding": "strict"` or `"relation_binding": "lenient"` field; omitting
it selects `strict`. This applies to `api::execute_request`,
`api::execute_compiled_request`, CLI execution, and wasm `execute`. Successful
responses echo the selected policy in `metadata.relation_binding`.
The [self-contained request](../schemas/execution-request.v1.schema.json),
[compiled request](../schemas/compiled-execution-request.v1.schema.json), and
[response](../schemas/execution-response.v1.schema.json) schemas describe this
wire contract.

New served responses always echo the binding policy. When reading a historical
response without `metadata.relation_binding`, the policy remains unknown; the
reader must not infer that strict validation occurred. Lower-level bulk execution
that bypasses dataset binding likewise omits the policy.

Callers migrating an existing dataset can explicitly select lenient binding:

```sh
axiom-rules-engine run --relation-binding lenient < request.json
axiom-rules-engine run-compiled --artifact compiled.json --relation-binding lenient < request.json
```

The CLI flag overrides the request field. Requests supplied directly on stdin
without a subcommand can select the same policy through the JSON field.
The CLI emits a stderr notice for lenient binding and retains
`warning[relation_slot_entity_mismatch]` diagnostics. It does not reorder tuples
or correct their kinds: a reversed relation can still return a zero count or
credit. Correcting the dataset lets callers return to the strict default.

Rust callers using `DatasetSpec::to_dataset_for_program` also get strict
binding. An explicit `DatasetBindingOptions::lenient()` passed to
`to_dataset_for_program_with_options` opts out. Lower-level callers that
construct a `DataSet` or `Engine` directly bypass wire-dataset validation. The
Python extension's dense execution methods take arrays rather than a wire
`DatasetSpec`, so this request policy does not apply to those methods.

When upgrading or recompiling RuleSpec, ensure tuple order follows declared
arguments and that input records carry the correct kinds. An old compiled
artifact that executes relations without `program.relations[].slot_entities`
no longer loads; see the next section.

#### Migrating compiled artifacts

Artifacts compiled before typing became mandatory keep format version 2.
This engine loads format 2 only through the explicit compatibility path
(`run-compiled --accept-artifact-format 2`, Rust's
`from_legacy_format_2_*`, or Python's `accept_artifact_format=2`). Executed
relations must still pass the typing check. One that executes an untyped
relation, or reads a typed one against its declared kinds, fails with a
message naming the migration. Recompiling from typed source is the durable fix; for an
artifact whose source cannot be recompiled yet:

```bash
axiom-rules-engine migrate artifact --artifact old.json
axiom-rules-engine migrate artifact --artifact old.json --output typed.json \
  --relation-entities 'us:statutes/7/2012/j#relation.member_of_household=Person,Household'
```

Without `--output` the command reports what it would change and writes
nothing. It stamps each untyped relation with the kinds execution
establishes, and only those:

- the slot an aggregate keys on holds ids of the evaluating rule's entity;
- the other slot holds ids of the entity of the rules its `where` clause and
  summed value read, when they agree;
- an aggregate over a derived relation is evidence about its data source only
  when every link of the chain keeps the same slots;
- a filtered entity counts as its source's kind only when every derivation
  defining that name is consistent with its source chain and they agree.

Declarations never vouch for a kind, and derived-relation predicates and
membership tests give no evidence. A slot this leaves open (for example
`len(relation)` with no related rule, or a rule whose entity is a filter over
an untyped source) needs `--relation-entities <relation>=<Kind>,<Kind>` in
tuple order; the relation may be named by its full id or a unique short name.
The command never moves an aggregate's slots, so datasets that bound
correctly before still bind, and datasets in the other orientation now get
`relation_slot_entity_mismatch` at binding (an error by default, a warning
under lenient binding) instead of aggregating nothing silently. An override
that contradicts what execution establishes is refused. A typed declaration
that contradicts it (artifacts compiled between the declaration carry and
declared-order resolution) is refused unless an override in executed order
retypes it; to change an orientation, recompile. The rewritten
artifact's bytes differ, so republish any sha256 pins. `axiom-rules-engine
capabilities` lists `relation_entity_typing` for engines that enforce this.
The migration command continues to write format 2; recompilation from
RuleSpec produces format 3 with its checked interface and producer identities.

Derived relations are rule-defined views over data relations or other derived
relations. The source relation supplies candidate tuples; the formula decides
which candidate tuples remain in the filtered relation.
Local source and predicate references resolve within the declaring module,
including when that module is imported. Inputs used only by a relation predicate
use the declaring module’s canonical `#input.<slot>` reference; an importing
module cannot supply them under its own identity.

```yaml
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]

  - name: snap_member_eligible
    kind: derived
    entity: Person
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: has_ssn and not student_ineligible

  - name: snap_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: SnapUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: snap_member_eligible

  - name: snap_unit_size
    kind: derived
    entity: SnapUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
```

In this example, `member_of_household` is supplied by the dataset. `snap_unit`
is computed at runtime by keeping only household members whose
`snap_member_eligible` judgment holds. Rules scoped to `entity: SnapUnit` use
the `member_relation` alias (`members` above) to aggregate over the filtered
members. The runtime id for the filtered entity is the source relation's current
entity id, so a SNAP unit backed by `household-1` is queried with
`entity_id: household-1`.

Derived relations execute in explain mode, bulk fast mode (which evaluates each
row's relation aggregations on the explain interpreter), and the generic dense
compiler for predicates that can be evaluated from related inputs, related
judgment rules, judgment or scalar rules of the relation's current slot entity,
entity-free `Scalar` rules, and membership of the relation's own source
(`member_of_household` in `snap_unit` above). A `source_relation` may also
point at another `derived_relation`; the runtime applies the parent filter
before the child filter, and a predicate may then
also test membership of a source further up that chain. Each such test must
read the relation with the slots its derivation reads it with, as a bare
relation name in a formula does unless the derivation sets `current_slot` or
`related_slot`. The dense compiler rejects a predicate with any other
membership test, because a dense batch carries no tuples for it, rejects a
predicate that reads a rule of an entity that is neither of the relation's
declared slot entities (explain evaluates such a rule for the related record),
and may still reject membership predicates that aggregate another relation from
inside a current-entity rule.

Inside a derived-relation predicate, a referenced scalar rule uses its declared
entity to select the current or related record, just as a referenced judgment
does. For example, a related record's key can be compared with the current
group's key without copying the group value onto each record. This scope is
preserved through arithmetic, parameter indices, date expressions and scalar
conditionals, including dependency traces. Bare input references still read the
related record; use an entity-scoped scalar rule for a current-record value.
Conditional scalar membership operands evaluate in every mode.

Only a derived relation's own predicate selects a record this way. A
`count`/`sum` `where` clause, a summed value and the body of every rule are
evaluated with no relation context, so each rule they reference is evaluated
for the related record, whatever entity it declares: a `Household` rule in a
`where` clause over household members is evaluated for each member, not for
the household, and a `Person` rule in a `where` clause over a
person-to-person relation is evaluated for each related person. See
[execution-semantics.md](execution-semantics.md).

A predicate can also name the record it reads. Inside a derived relation's
predicate, two **terms** name entity ids: `anchor`, the id the relation is read
from (its current slot), and `candidate`, the id being tested (its related
slot). `t.name` reads a value at a term's id:

- `t.rule` evaluates a derived rule at `t`'s id. The rule's entity must be the
  kind of `t`'s slot, where a filtered entity such as `SnapUnit` counts as its
  source's current kind (`relation_role_entity_mismatch`). An entity-free rule
  reads no id; read it unqualified.
- `t.input` reads an input at `t`'s id. Inputs declare no kind, so this read is
  not kind-checked: a misspelt input compiles and fails at evaluation as a
  missing input, as a bare read does. An input read only through a term enters
  the input catalog as a bare read does.

A predicate reads the module's parameters as a rule body does: parameter
tables, scalar parameters, and entity-free rules with literal values, which
lowering turns into parameters. A `parameter` record with an entity or a
computed formula lowers to a rule, and a predicate now reads it as a rule
too, bare or through a term; it used to read an input of that name.

```yaml
  - name: older_relative_of
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: relative_of
      slot_entities: [Person, Person]
      current_slot: 0
      related_slot: 1
    versions:
      - effective_from: 2026-01-01
        formula: candidate.age >= anchor.age + minimum_gap
```

Here `minimum_gap` is a scalar parameter. Both slots of `relative_of` hold
`Person` ids, so a bare `age` reads the candidate's and a bare `Person` rule
the anchor's; the terms reach either side. A term's kind is the
derivation's `slot_entities` at the term's slot. A derivation that omits
them takes them from a source whose two slots hold different kinds, but not
from a source whose slots share a kind such as `[Person, Person]`, so such a
derivation must declare them or a read through a term is refused
(`relation_roles_need_slot_kinds`).

Inside a predicate, `anchor` and `candidate` are reserved. A term written bare
(as a value, a judgment, a parameter index, an aggregated relation, or after
another term's `.`) is refused with `relation_term_used_as_value`. A read
through a term may not appear in an aggregation's `where` clause or
aggregated value (`count`, `sum`, `min`, `max`, `only` and their `_where`
forms, and `only_related`), which have no relation context
(`relation_role_outside_scope`). Because `anchor` and `candidate` name terms,
not relations, `min(candidate.age)` and `max(candidate.age)` in a predicate
are the scalar minimum and maximum of one qualified read. Reading a
parameter, `period_start` or `period_end` through a term is refused with
`relation_role_entity_mismatch`; reading a relation through a term is refused:
use a relation atom. Outside predicates the two names are ordinary
identifiers: `anchor + 1` in a rule body reads an input named
`anchor`, and `anchor.age` there is a field access, which RuleSpec rejects.
No declaration may take either name ([Declaration names](#declaration-names)).
A compiled program that carries a read through a term anywhere but a derived
relation's predicate fails relation typing with `relation_role_outside_scope`.

Unqualified reads keep the routing above. A bare parameter name now reads the
parameter in two places where it used to read an input of that name: anywhere
in a predicate ([relation-roles.md](relation-roles.md) §3.3), and where a
formula tests it as a judgment (`enabled`, `not enabled`, `enabled and ...`),
in a rule body as in a predicate. That held when nothing else had the name: a
parameter that shared a rule's name used to read the rule there, and in a
predicate one that shared a relation's name tested membership. Such a program
is now refused at declaration. A bare `period_start` or `period_end` tested
as a judgment, which used to read a rule, a relation or an input of that
name, is refused. A judgment resolves a bare name in the order a scalar does,
and a scalar reads the built-in date first, so neither name is a judgment. In
a predicate, a relation named `anchor` or `candidate` used to be read through
`.`: `sum(anchor.age)` aggregated over it and is now refused, and
`min(anchor.age)` took its minimum and now reads the anchor's own `age`. Such
a relation can no longer be declared, so no program that compiles keeps
either old meaning. A predicate that reads a `parameter` record with an
entity or a computed formula used to read an input of that name and now
reads the rule. No declaration may take a reserved name, and no name may be
declared as two of a parameter, a rule and a relation
([Declaration names](#declaration-names)).
A program with none of these, and with no term, compiles to the
same artifact as before terms existed. Explain and bulk fast mode evaluate reads through terms, and a trace
records a rule read through a term at the term's id. The dense compiler
rejects a root entity whose rules reach a derived relation whose predicate
uses a qualified read, relation atom or term comparison, naming the construct
and #218; a root that reaches no such relation compiles. Dense does compile
parameter reads in a predicate.

A relation atom, `R(t1, ..., tn)`, tests exactly those term ids in the declared
relation's slot order. For example, `relative_of(candidate, anchor)` tests the
reverse pair without the ambiguity of a bare same-kind membership test.
A data atom holds when a supplied tuple matches every id and covers the
evaluation period. A derived atom requires arity 2
(`derived_relation_atom_arity_unsupported`) and tests whether its
related-slot term belongs to the relation's members for its current-slot term.
Each atom requires declared slot kinds (`relation_atom_needs_typed_relation`),
the same number of terms as slots (`relation_atom_arity_mismatch`), a term in
scope as every argument (`relation_atom_argument_not_term`), and a matching
kind at each slot (`relation_atom_kind_mismatch`). An atom is a judgment, so
`relative_of(candidate, anchor) > 0` is refused with
`relation_atom_used_as_value`. A relation whose
name is also a built-in function, such as `len`, cannot be used as an atom
(`relation_name_shadows_builtin`): `len(relative_of)` still calls the built-in,
and `len(anchor, candidate)` is refused. A bare relation name keeps its
meaning.

Two terms of the same kind can compare ids with `==` or `!=`:
`candidate != anchor` excludes the anchor from its own candidates. Different
kinds refuse with `relation_term_kind_mismatch`, a term compared with a
value refuses with `relation_term_compared_with_value`, and any other
operator, such as `anchor < candidate`, refuses with
`relation_term_comparison_operator`. An atom or term
comparison belongs only in a derived relation's predicate; rules and
aggregation `where` clauses refuse it with `relation_role_outside_scope`.

A program using qualified reads, atoms or term comparisons refuses supplied
tuples under any derived relation's name
(`supplied_tuples_for_derived_relation`). Its members resolver also ignores
such tuples for direct Rust callers of `Engine::new`. Programs using none of
these constructs retain the union of supplied and derived members, including
programs whose predicates only read parameters. Request
binding also refuses `relation_binding: lenient` when the program uses an
atom (`lenient_binding_with_relation_roles`). See
[relation-roles.md](relation-roles.md) for the design and derived-atom cost.

Every mode evaluates conditionals, `and`/`or` and aggregations lazily per row:
a branch, operand or related member that a row's evaluation does not reach
cannot fail that row or the batch. See
[execution-semantics.md](execution-semantics.md).

Top-level `imports` merge other RuleSpec files into the compiled RuleSpec module
before the current file is lowered. Every import is an exact absolute canonical
target, optionally followed by one `#rule_fragment`:

```yaml
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
  - us-co:regulations/10-ccr-2506-1/4.207.3
```

The canonical form is `<jurisdiction>:<atomic-root>/<relative path without
extension>`. Filesystem loading admits one layout only: an exact country
checkout named `rulespec-<country>` with direct matching jurisdiction
directories. For example, `us:` resolves below `rulespec-us/us/` and `us-co:`
below `rulespec-us/us-co/`.

For current `main`, filesystem callers must supply at least one absolute, real,
unaliased country checkout. The CLI built from `main` uses required repeatable
`--rulespec-root` arguments; Rust callers pass a validated
`CanonicalRuleSpecRoots` to `load_rulespec_file`,
`CompiledProgramArtifact::from_rulespec_file`, or `FsModuleSource`. These
current-`main` interfaces have no environment, cwd, ancestor, sibling-checkout,
suffixed-worktree, or standalone jurisdiction-repository fallback.

The released v0.1.1 CLI uses a different contract. Its `compile` command accepts
`--program` and `--output` only and resolves canonical cross-repo imports
without a root flag. See the [release instructions](install.md).

Configured roots must remain trusted and quiescent throughout validation and
compilation. This is a deterministic authority boundary, not a sandbox against
concurrent pathname replacement or hard-link mutation; CI satisfies the
assumption with fresh independent Git checkouts and integrity gates.

The five filesystem content roots are `legislation/`, `policies/`, `programs/`,
`regulations/`, and `statutes/`. Only four are atomic RuleSpec roots:
`legislation/`, `policies/`, `regulations/`, and `statutes/`. `programs/`
contains declarative ProgramSpecs for `axiom-compose`; the engine rejects it as
an atomic module target. Module files use `.yaml` only; companion
`*.test.yaml` files are validation cases and never module targets. Root-level content,
wrong-country jurisdictions, duplicate roots or countries, aliases, symlinks,
special paths, `.yml`/case-variant/double extensions, relative imports, and
reserved or whitespace path components fail closed before compilation.

`axiom-compose` emits an ephemeral import-bearing `rulespec/v1` composition,
not an atomic module. The CLI built from current `main` compiles that output
with the separate `compile-composed` command. Its input must be an absolute,
real, unaliased `.yaml` outside every RuleSpec checkout with exact
`module.kind: composition`; it still requires one or more explicit
`--rulespec-root` arguments. Only fragmentless canonical module imports are
allowed; those targets may declare `module.kind: composition` and
are recursively merged only on the composed-program surface. Their rules keep
their canonical module origins and source citations, while synthesized root
rules remain originless. The removed top-level `extends` directive is rejected
on both surfaces. On current `main`, the ordinary `compile` command rejects
composition modules at any depth, while `compile-composed` rejects atomic entry
modules and declarative ProgramSpecs. Release v0.1.1 does not provide
`compile-composed`.

Compiled artifacts make the input boundary explicit in
`metadata.input_catalog`. Each runtime slot has a deterministic
`canonical_request_name` plus the complete sorted `request_names` set. A bare
name is published only when an originless synthesized rule actually owns that
slot, and it is then canonical. Otherwise the lexicographically first exact
atomic `<module>#input.<slot>` owner is canonical. Multiple owner names are
equivalent inputs to one slot; arbitrary prefixes and `<target>#<slot>` aliases
are not accepted. This is a deterministic runtime-input catalog, not a source
manifest. Format-3 artifacts additionally carry `program.interface`, whose
input entries use these canonical names and describe entity flow, conservative
type demands, uniform defaults, closed parameter-index domains and elections.
Its entity and relation entries describe instance sources, tuple slot kinds,
traversal and provenance. Loading re-derives the interface and metadata from
the embedded program and checks consistency. `engine_build` identifies the
producer build, and `source_closure` hashes the RuleSpec texts read, including
imports. Source authenticity remains in the signed-corpus-release and
supervisor chain. See [the artifact format reference](artifact-format.md).

Filesystem search is one host strategy, not part of the core. A host can
instead supply module text directly through the `source::ModuleSource` trait
and the `load_rulespec_with_source` /
`CompiledProgramArtifact::from_rulespec_with_source` entry points — for
example a browser (wasm) bundle, a server holding modules in memory, or a
registry client. Imports remain exact absolute canonical targets; the host only
answers "what is the YAML text for `us:statutes/7/2015/e`?", and durable ids
come out identical to the filesystem layout. Canonical targets supplied to any
host still require one of the four atomic roots. The filesystem behavior above
is packaged as `FsModuleSource` behind the default-on `fs` cargo feature.

Executable rules loaded from jurisdiction repos receive a durable id of
`<canonical file target>#<rule name>`, for example
`us:statutes/7/2017/a#snap_regular_month_allotment`. Formula strings may still
reference rules by local symbol inside the compiled RuleSpec module, but public
access must use durable legal ids whenever one exists.

Rule names are public concept fragments, not just display labels. See
[`concept-naming.md`](concept-naming.md) for the naming contract.

Execution requests for repo-backed RuleSpec reject bare output, input, and
relation names. Dataset inputs must use a request name from the compiled
program's `metadata.input_catalog`, such as the owning module's
`#input.<local symbol>`. An imported rule's inputs use that upstream module's
catalog names. For example, a companion case can supply primitive inputs and
related-entity facts:

```yaml
input:
  us:statutes/7/2017/a#input.household_size: 1
  us:statutes/7/2012/j#relation.member_of_household:
    - us:statutes/7/2012/j#input.snap_member_is_elderly_or_disabled: false
```

Dataset records naming a derived rule or parameter are rejected during
program-aware binding in both explain and fast modes, including records for
unqueried rules. The same refusal applies to bare rule names unless the name
is also an actual input slot exposed by the catalog. Unknown input names and
unknown `#input.` references are rejected too.

To override a scalar derived rule in a `run-compiled` request, use `pins` with
the rule's local name and a typed value, alongside the request's `mode`,
`dataset`, and `queries` fields:

```json
{
  "pins": [
    {"rule": "snap_net_income", "value": {"kind": "decimal", "value": "100"}}
  ]
}
```

Pins accept scalar derived rules, not judgments or parameters. Parameters
are law values defined in the program; change the program parameter to model
a different law. Companion test `input` maps contain dataset facts, not
implicit pins. Callers of the lower-level `DatasetSpec::to_dataset` or direct
`DataSet` constructors must use program-aware binding to obtain these checks.

Responses are keyed by durable ids and include the local `name` inside each
output value only as display metadata alongside the id.

Example structured scale:

```yaml
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: max_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment_table[household_size]
```

Source-stated tables should use this shape instead of derived `match` formulas
with embedded numeric cells. That keeps reforms path-addressable at the cell or
selector level.

Example restating provision:

```yaml
rules:
  - name: co_snap_maximum_allotment_restates_usda_fy_2026
    kind: source_relation
    source: 10 CCR 2506-1 section 4.207.3(D)
    source_url: https://www.sos.state.co.us/...
    source_relation:
      type: restates
      target: us:policies/usda/snap/fy-2026-cola#snap_maximum_allotment
      authority: federal
    verification:
      values:
        snap_maximum_allotment_table:
          1: 298
          2: 546
```

Use `source_relation.type: restates` when the local text should be represented
for coverage and auditability, but computation and reformable values belong to
the target rule. Restatement records cannot contain executable formulas,
versions, or table values that lower into runtime. If the local provision
changes the target rule's legal effect, encode the executable local rule and add
a `source_relation.type: sets`, `implements`, or `amends` edge to the upstream
delegation or target.

For delegated parameter settings, the upstream source should expose the
reformable slot as a `kind: parameter`. A downstream source relation whose
`source_relation.type` is `sets` binds the local parameter values into the
upstream slot during RuleSpec lowering when its `target` points to that
upstream parameter and its `value` points to a local parameter. This lets
federal formulas encode the legal structure once while state modules supply the
delegated standards.

For delegated formula hooks, the upstream source should expose the hook as a
`kind: derived` placeholder with the same entity, dtype, unit, and period that
the upstream formula consumes. A downstream `sets` relation can bind a local
derived rule into that hook. Lowering copies the local derived semantics into
the upstream hook while preserving the upstream name, public id, and source
metadata. This is appropriate when federal law defines where a state option
enters a formula, but state law defines the option's selection logic.

Example delegated parameter setting:

```yaml
rules:
  - name: co_snap_heating_cooling_sua_fy_2026
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    source: Colorado SNAP manual FY 2026 SUA table
    versions:
      - effective_from: 2025-10-01
        values:
          1: 475
          2: 475

  - name: co_snap_heating_cooling_sua_sets_federal_slot
    kind: source_relation
    source: Colorado SNAP manual FY 2026 SUA table
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#state_utility_allowance_amount
      authority: state
      value: us-co:policy/cdhs/snap/fy-2026#co_snap_heating_cooling_sua_fy_2026
      basis:
        delegation: us:regulations/7-cfr/273/9#state_utility_allowance_delegation
```

Example delegated formula hook:

```yaml
rules:
  - name: snap_standard_utility_allowance_state_option
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    source: 7 CFR 273.9(d)(6)(iii)
    versions:
      - effective_from: 2025-10-01
        formula: "0"

  - name: state_snap_standard_utility_allowance
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    source: State SNAP utility allowance rule
    versions:
      - effective_from: 2025-10-01
        formula: if household_has_qualifying_heating_or_cooling_cost: state_sua_amount else: 0

  - name: state_snap_standard_utility_allowance_sets_federal_hook
    kind: source_relation
    source_relation:
      type: sets
      target: us:regulations/7-cfr/273/9#snap_standard_utility_allowance_state_option
      value: us-state:policies/snap/utility-allowance#state_snap_standard_utility_allowance
      basis:
        delegation: us:regulations/7-cfr/273/9#snap_state_standard_utility_allowance_delegation
```

### Mutual exclusivity: `exactly_one`

Judgment formulas accept `exactly_one(a, b, ...)` (two or more arguments,
each any judgment-position expression: a fact reference, a comparison, a
negation). It holds when exactly one argument holds. It serialises as a
first-class `exactly_one` node in specs and compiled artifacts, so graph
consumers see one n-input gate instead of the ~n² expansion. Evaluation
lowers the node to the flat Or-of-And-of-Nots expansion — branch *i*
asserts argument *i* and negates the rest — so outcomes, short-circuit
reads, and missing-input faults match hand-written expansions on every
input. Trace text renders the flat lowering, which is shallower than the
binary nesting hand-chained `and`/`or` produce. Prefer it wherever a rule validates a choose-one set, such as filing
statuses:

```yaml
versions:
  - effective_from: '2026-01-01'
    formula: exactly_one(filing_status_single, filing_status_married_separate, filing_status_joint, filing_status_head_of_household)
```

replaces the four-branch expansion whose leaf-reference count grows with the
square of the option count.

### Minimum, maximum and exactly-one over related entities

Beside `len(relation)`, `sum(relation.field)`, `count_where(relation,
predicate)` and `sum_where(relation, field, predicate)`, scalar formulas take
three more aggregates over the entities a relation relates to the rule's
entity:

| Function | Value |
|---|---|
| `min(relation.field)` / `max(relation.field)` | the least / greatest `field` over the related entities |
| `min_where(relation, field, predicate)` / `max_where(relation, field, predicate)` | the same over the related entities whose `predicate` holds |
| `only(relation.field)` | `field` of the one related entity |

`min` and `max` aggregate only when their single argument is a relation field
access; every other form (`min(a, b)`, `max(x)`, `min(members.age, 3)`) keeps
its scalar meaning. `field` is an input or a derived scalar rule of the related
entity, and a `_where` predicate is a Boolean input or a derived judgment on
it, exactly as for `sum` and `sum_where`. The members are resolved as `sum`
resolves them: the distinct related ids, a derived relation's filter, then the
`where` clause, evaluated for each member. The relation's direction comes from
its declared slot kinds, as for every aggregate.

- `min` and `max` compare numbers numerically and yield a decimal, as `sum`
  does, and compare dates chronologically and yield a date. A Boolean or text
  value is a type error.
- `only` yields the member's value with its own kind (integer, decimal, text,
  Boolean or date).
- None of them invents a value. `min` or `max` with no selected member fails
  with `EmptyRelatedAggregate`; `only` fails with `MissingRelation` when there
  is no member and with `AmbiguousRelation`, which reports the count,
  when there are several. Each error names the relation, the entity and the
  period. A rule whose law gives the empty case a value says so with a guard;
  conditionals are lazy, so the guarded form never fails:

```yaml
rules:
  - name: member_of_family
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Family]
  - name: youngest_child_age
    kind: derived
    entity: Family
    dtype: Integer
    period: Year
    versions:
      - effective_from: '2026-04-01'
        formula: |-
          if len(member_of_family) > 0: min(member_of_family.age)
          else: 0
  - name: family_income_of_child
    kind: derived
    entity: Person
    dtype: Money
    period: Year
    versions:
      - effective_from: '2026-04-01'
        formula: only(member_of_family.family_income)
```

Here `youngest_child_age` reads each member's `age` from the family; a family
with no member takes the `else` branch, which the encoder writes from the law
(0 here only for the example). `family_income_of_child` reads from the person
side, so it runs for each person: a person in exactly one family reads that
family's `family_income`, and a person in none or in two fails rather than
reading 0.

They serialize as the expression kinds `extreme_related` (with `extreme:
min | max`) and `single_related`. `only(r.f)` is not
[`only_related(r, f)`](only-related.md), which projects a Boolean input only
and serializes as its own kind; where both apply they give the same value and
the same errors. An engine whose `capabilities` lists
`related_min_max_only` loads them; an earlier engine refuses an artifact that
uses them on load, and a program that uses none of them compiles to the same
bytes as before. Explain, fast and dense evaluate them; see
[execution semantics](execution-semantics.md) for the evaluation order and
each mode's contract.

## Declaration names

Each name declared in a module's import-merged closure names one
declaration, and wherever a formula may read that kind of declaration bare, a
bare read of the name reaches it. The compiler refuses two kinds of declaration that would
break this, before it lowers any formula, in the default and the strict mode
alike.

**A name declared as two things** (`declaration_name_clash`). A bare name
reads, in a fixed order, a parameter, then a rule, then (in a predicate's
judgment position) a relation, then an input. A name declared as two of a
parameter, a rule and a relation would read only the first. The other would
still compile (a hidden rule could still be requested by name), but no bare
read would reach it. Each declaration counts as lowering reads it:

- a parameter table, or a `parameter` or `derived` record with no entity and
  only literal versions, is a parameter;
- any other `parameter` or `derived` record is a rule, so a `parameter` with
  an entity clashes with a scalar parameter, not with a rule;
- a `data_relation` or `derived_relation` is a relation.

The error names both declarations' modules. Two declarations that read the
same way are left to the existing checks: compatible duplicate parameters
merge with a diagnostic (an error in strict mode), and a rule declared twice
is an error.

**A reserved name** (`reserved_declaration_name`). A bare `period_start` or
`period_end` means the start or end date of the period being computed: a
scalar read returns the date, and a judgment read is refused. In a derived
relation's predicate, a bare `anchor` or `candidate` names a term. The lexer
reads a formula keyword as a literal (`true`, `True`, `false`, `False`) or as
syntax (`and`, `or`, `not`, `if`, `elif`, `else`, `match`, `entity`, `amend`,
`from`, `to`). No parameter, rule or relation may take any of these names. A
parameter or rule could never take a keyword, because its name is written
into the formula source, so the keyword check adds relations.

Refusing a reserved name costs the routes that do not read it bare. Before
the refusal, a declaration named `period_start` was still reached by a table
index (`period_start[0]`), a related read (`sum(member_of.period_start)`), a
`where` clause or summed field (`count_where(member_of, period_start)`,
`sum_where(member_of, period_start, adult)`), an aggregation over a relation
of that name (`sum(period_start.age)`, `len(period_start)`), and a request for
the rule by name. Meanwhile every bare scalar read of it silently returned
the date, and a bare judgment read was refused. A
declaration named `anchor` or `candidate` was reached by every route outside
predicates; inside one, a bare or qualified read was refused and
`min(anchor.age)` read the term's own `age`. Imports merge whole modules, so
a declaration's name reaches every importer's predicates. Renaming the
declaration keeps every route, with the same lowering.

Five things are not checked:

- `source_relation` records: formulas never read their names, which are
  cited by `#` id;
- a relation whose name is not a formula identifier or path, such as
  `member-of`: no formula can write it, so it has no bare read to lose.
  Such a relation can still be a derived relation's `source_relation`. A
  parameter's or rule's name must already be one identifier or path;
- a derived relation's `member_relation` alias (`members` in the SNAP
  example above): in rules of its filtered entity it names that derived
  relation. It is a scoped name, not a declaration;
- undeclared names, including the inputs an `inputs:` block describes. Its
  entries are not declarations in the sense of this section: inputs are
  inferred from reads, and an entry does not change which declaration or
  input a name reads. Outside predicates `anchor + 1` reads an input named
  `anchor`, as above, with or without an entry. An entry whose `election`
  names an input that no rule or derived-relation predicate of its module
  reads is refused
  ([Input declarations and elections](#input-declarations-and-elections)).
  A declared name can also name an input in a position that does not read
  that declaration, and this is not refused. A bare relation name outside a
  predicate's judgment position, as in a rule body or in
  `shared == true`, reads an input of that name. A parameter's name in a
  related read (`sum(member_of.x)`), in `count_where` or in `only_related`
  also reads an input of that name. An election on such an input is
  accepted;
- `ProgramSpec` JSON: each of its reads names its kind (`parameter_lookup`,
  `derived`, `input`, `relation_member`, `period_start`), so no name chooses
  between declarations.

## Currency rounding

A `derived` rule whose `unit` is a currency may declare an output-rounding
mode. When declared, the engine rounds the rule's output to the unit's
`minor_units` (the currency's fractional-digit count) under that mode, in every
execution path — the explain interpreter, the bulk fast path, and the dense
columnar path produce the identical rounded value. Rounding is applied to the
rule's output before it is cached, so a rule that references a rounded rule sees
the rounded figure (statutory rounding composes, as in SNAP where a rounded net
income feeds the allotment).

```yaml
units:
  - name: USD
    kind: currency
    minor_units: 0        # whole dollars (SNAP allotments); use 2 for cents
rules:
  - name: snap_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    rounding: half_up     # half_up | half_even | floor | ceil
    effective_from: 2025-10-01
    formula: max(0, snap_max_allotment - net_income * 0.3)
```

Modes: `half_up` rounds a `.5` midpoint away from zero (the benefit/tax
default); `half_even` is banker's rounding; `floor` rounds toward negative
infinity; `ceil` toward positive infinity. An explicitly declared unit overrides
the engine's built-in currency defaults, so declaring `USD { minor_units: 0 }`
gives whole-dollar rounding even though the default USD is two-decimal.

Rounding is **opt-in**: a rule with no `rounding:` behaves exactly as before
(no rounding). Declaring `rounding:` on a rule whose unit is not a currency, on
a rule with no unit, or on a non-`derived` rule is a compile error. In
explain-mode traces, a rounded node reports the applied `rounding` mode and,
when rounding changed the value, the `pre_rounding_value`, so the rounding step
is auditable.

## Input declarations and elections

A module's top-level `inputs:` block describes the inputs its rules read, as
a list of entries with `name` or as a mapping from name to entry. The engine
ignores descriptive keys for ordinary inferred inputs. A block without an
`election` contributes no election declarations. A module that uses
`only_related` or `input_for_year` also needs the block to declare those
inputs and satisfy their separate checks; see
[only-related.md](only-related.md) and
[rulespec-format.md](rulespec-format.md).

`election` marks an input as a choice the law gives a named actor, rather
than a fact:

```yaml
inputs:
  - name: is_boarder
    entity: Person
    dtype: Boolean
    period: Month
  - name: provider_requests_boarder_inclusion
    entity: Person
    dtype: Boolean
    period: Month
    election:
      actor: Household
      provision: us/regulation/7/273/1/b/3
```

`actor` is the entity kind that makes the choice, an UpperCamelCase label
that need not be a kind the program evaluates on. `provision` is the corpus
citation path of the provision that gives it. `election` takes exactly those
two keys. The entry must have a name, and the name must be an input that a
rule or derived-relation predicate of the same module reads. The engine
evaluates the value it is given and never defaults an election, so reading
an election with `input_or_else` is a compile error. Two modules that read
one input slot may both declare it an election only with the same actor and
provision. The compiled artifact reports each input's election, or null for
a fact, in `program.interface` ([artifact-format.md](artifact-format.md)), so
a simulator knows which inputs it may branch over; it must not optimize over
a fact.

Lowering carries elections in `ProgramSpec.input_declarations`, a list of
`{input, election}` entries sorted by the declaring module's input request
name. The field is last and omitted when empty; raw `ProgramSpec` programs
may supply it too. Compilation refuses unsorted or repeated declarations,
unknown request names, malformed election metadata, disagreeing declarations
of one slot, and any defaulted read of an election. The loader checks the
same declarations when it re-derives `program.interface`.

## Source pinning and provenance

The `module:` block can carry inert metadata that grounds the module in source
text and records how the encoding was produced and checked. The block itself
is optional. When `source_verification` is present it is an exact mapping with
one required singular `corpus_citation_path`, an optional `source_sha256`, and
an optional typed `upstream_source_check`; unknown fields and the removed
plural spelling are rejected recursively.
The loader and artifact boundary validate this metadata, but it never changes
formula semantics or execution results. The lowered `ProgramSpec` keeps the
(merged) root module's metadata on `program.module`, and compiled artifacts pass
it through their JSON, so tooling can read it from a loaded module or artifact
without re-parsing the YAML.

`module.interpretations` carries structured readings of ambiguous,
conflicting, or nonliteral source text. It is also preserved on
`program.module` and in compiled artifacts without affecting output names,
compilation, or results. The [format reference](rulespec-format.md#interpretations)
documents its shape, status-dependent fields, load-time validation, and
encoder responsibilities for verbatim corpus excerpts.

```yaml
module:
  source_verification:
    corpus_citation_path: us/guidance/agency/annual-parameter
    source_sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
    upstream_source_check:
      status: official_parameter_source
      checked_paths:
        - us/statute/7/2017/a
      rationale: The cited guidance supplies the annually determined parameter.
  encoding_provenance:
    encoder: axiom-encode/0.2.645
    model: claude-fable-5
    run_id: run-2026-06-10-001
    reviewed_by: human-reviewer
  validation:
    - oracle: policyengine-us
      status: matches
      last_run: 2026-06-09
```

- `source_verification.corpus_citation_path` is one non-empty canonical
  provision identity in the pinned Axiom corpus release. Proof/source nodes
  use the same singular field; plural source lists are composed before they
  reach the engine.
- Optional `source_verification.source_sha256` pins the SHA-256 hex digest of the
  exact corpus provision text the module was encoded from. When the source
  republishes (for example an eCFR update), recomputing the hash gives a
  mechanical "this module is stale" signal; `axiom-encode
  check-source-staleness` does exactly that. The value must be 64
  hexadecimal characters — anything else is rejected at load with an error
  naming the module file.
- Optional `source_verification.upstream_source_check` preserves an encoder-
  validated higher-authority audit with required `status` (string),
  `checked_paths` (list of strings), and `rationale` (string). The nested
  mapping is exact, so misspelled or additional keys are rejected. The engine
  validates structure only; axiom-encode owns allowed statuses, authority
  classification, and rationale-quality policy.
- No other `source_verification` subfields are allowed.
- `encoding_provenance` records the encoding tool (`encoder`, for example
  `axiom-encode/0.2.645`), `model`, `run_id`, and human `reviewed_by` — all
  optional strings. Unknown subfields are rejected so typos cannot pass for
  provenance.
- `validation` lists oracle-validation results: `oracle` (string), `status`
  (one of `matches`, `mismatches`, `pending`), and optional `last_run`
  (ISO date). Unknown statuses are rejected.

Known hard gaps:

- Formula strings are parsed by the internal `crate::formula` parser and
  normalised into `ProgramSpec`.
- Current formula-string gaps include inferred relation slot orientation and
  incomplete optimized support for complex cross-scope derived-relation
  predicates. These should be closed in RuleSpec and `ProgramSpec`, not by
  adding another source format.

## Why RuleSpec is the external format

`ProgramSpec` is the engine's typed IR inside compiled artifacts, not a
filesystem authoring format or alternate loader surface. RuleSpec keeps
metadata and provenance structured while leaving formulas concise enough for
generation and review. The Axiom app should provide the human-readable
visualisation layer; raw source readability is secondary to schema validity,
provenance fidelity, and avoiding silent lossy translation.

Canonical jurisdiction repos use the filepath as the rule ID. Source artifacts
live in immutable named `axiom-corpus` releases, joined through the singular
`corpus_citation_path`; RuleSpec checkouts do not maintain parallel `sources/`
registries. See [`jurisdiction-repos.md`](jurisdiction-repos.md).
