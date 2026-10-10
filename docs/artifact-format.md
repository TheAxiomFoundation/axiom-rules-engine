# Compiled artifact format 3

Status: design note for axiom-rules-engine#155, riding the format bump in #125.
This file becomes the format reference when the change lands. Every statement
about current behavior cites the code it was read from, at `main` 5d63e8f3.

## What changes

Format 3 adds three things to a compiled artifact and changes how artifacts
are admitted:

1. `program.interface`: the entity roster, the input slots and the relations,
   derived from the embedded program at compile time and re-derived and
   compared at load.
2. Two identities format 2 does not carry: `engine_build` (which code compiled
   it) and `source_closure` (which RuleSpec text it was compiled from).
3. An exact wire-format gate. The default loader reads format 3 only and says
   which side is older. Format 2 loads only through a separately named
   compatibility loader and stays format 2 in memory.

Execution does not read any of it. `program` minus `interface`, and
`metadata`, are what format 2 emitted for the same source.

## Facts the design rests on

- **Inputs are not declared.** A formula identifier that names no parameter or
  rule lowers to an `input` node (`docs/rulespec-format.md`, "Identifier
  resolution"). RuleSpec has no field for an input's dtype, entity, default or
  domain.
- **An input is read by `(slot name, entity id)`.** `Engine::lookup_input`
  (`src/engine.rs`) keys on the bare slot and the id being evaluated. The
  record's `entity` label is not part of the key.
- **A rule runs on the id of whatever evaluates it.** `ScalarExpr::Derived`
  evaluates the referenced rule on the current `entity_id`, unless a derived
  relation's predicate routes it by entity (`RelationEvalContext::entity_id_for`).
  A `where` clause and a summed value run on the related id
  (`fold_member`). So a rule's declared `entity` does not fix the ids its
  inputs are read on.
- **That difference is real in the corpus.** Main compiles 4,342 of the 4,878
  atomic modules in rulespec-us at 16a667719. In those artifacts, counting
  base and version expressions, 168 rule references in 52 modules go directly
  from a rule of one named kind to a rule of another (a `TaxUnit` rule to a
  `Person` rule in `us/statutes/26/151`, `Person` rules to `StateAgency` rules
  in `us/regulations/42-cfr/435/601/d/1`), and 172 go to an entity-free
  (`Scalar`) rule. Entity-free rules read nine input slots; where a
  `Household` rule references one (`us-al/policies/dhr/poe/.../802`), the slot
  is read on the household's id. Sixty-one slots are read directly by rules
  of two kinds.
- **`metadata.input_catalog` lists names nothing reads.** A parameter's
  `indexed_by` label enters the catalog whether or not an expression reads it
  (`Program::input_catalog`, `src/model.rs`); 99 such slots appear in those
  artifacts.
- **`input_or_else` is the only declared default**, and the formula language
  cannot produce it; none appears in those artifacts.
- **A derived relation also reads tuples supplied under its own name.**
  `related_entity_ids_at_level` unions the filtered source members with
  `direct_related_ids(relation, ...)`.
- **Type demands are static.** Arithmetic operands go through `as_decimal`,
  parameter indices and date offsets through `as_index`, date positions
  through `as_date`, and `compare_scalar_values` accepts only bool/bool,
  text/text, date/date or two numbers. Any other combination is a
  `TypeMismatch` when the node is reached.
- **Unit aggregation digests the serialization of the artifact it loads**
  (`source_artifact_digest`), so a loader may not rewrite a format-2 artifact
  (DECISIONS.md, 2026-10-04).

## Top-level shape

```jsonc
{
  "artifact_format_version": 3,        // wire format
  "engine_version": "0.2.2",           // semantic version of the compiling engine
  "engine_build": {                    // build identity
    "commit": "5d63e8f3...40 hex" ,    //   or null when the build cannot know
    "dirty": false                     //   or null
  },
  "source_closure": {                  // source-closure identity; omitted when
    "root": "us:statutes/26/32",       //   there is no RuleSpec source. null root
                                       //   for an originless root
    "modules": [ { "target": "us:statutes/26/32", "sha256": "..." } ],
    "digest": "sha256:..."
  },
  "program": { "...": "as format 2", "interface": { } },
  "metadata": { "evaluation_order": [], "fast_path": {}, "input_catalog": [] }
}
```

### The four identities (#125)

| Identity | Field | Checked at load |
|---|---|---|
| Wire format | `artifact_format_version` | Exact match, or the named legacy loader |
| Semantic version | `engine_version` (required in format 3) | Recorded, not gated |
| Build identity | `engine_build` | Recorded, not gated |
| Source closure | `source_closure` (omitted when compiled from IR) | Shape only |

`engine_build.commit` comes from `build.rs`: the `AXIOM_RULES_ENGINE_BUILD_COMMIT`
environment variable, else `git rev-parse HEAD` when the git top level is the
crate directory (a vendored copy inside another repository reports null, not
that repository's commit), else `.cargo_vcs_info.json` for a published crate,
else null. `dirty` is true when `src/`, `build.rs`, `Cargo.toml` or
`Cargo.lock` differ from that commit. The build script reruns when those
paths, `HEAD`, the ref it points to, or `packed-refs` change.

`source_closure.modules` lists every module text the loader read to lower the
program, the root included, sorted by target, each with the SHA-256 of the
exact text. An originless root (an in-memory string, or a composition file)
has `target: null`. `digest` is the SHA-256 of
`axiom-rules-engine/source-closure/v1\n` followed by one `<target>\t<sha256>\n`
line per module. A program compiled from a `ProgramSpec` has no RuleSpec
source, so `source_closure` is omitted. The closure covers RuleSpec module text
only: a `--corpus-provisions` index used to resolve `source_url` is not in it.

The crate version does not move in this change; versions are cut in their own
commits. The next cut should be 0.3.0.

## `program.interface`

```jsonc
"interface": {
  "entities": [
    { "kind": "Household", "id_template": "household:{index}",
      "instances": { "source": "supplied" } },
    { "kind": "Person", "id_template": "person:{index}",
      "instances": { "source": "supplied" } },
    { "kind": "SnapUnit", "id_template": "household:{index}",
      "instances": { "source": "alias", "of": "Household",
                     "via": ["us:...#relation.snap_unit"] } }
  ],
  "inputs": [
    { "name": "household_size",
      "legal_id": "us:statutes/7/2017/a#input.household_size",
      "entities": ["Household"],
      "dtype": "integer", "dtype_basis": "usage",
      "default": null,
      "values": null, "values_basis": null }
  ],
  "relations": [
    { "name": "us:statutes/7/2012/j#relation.member_of_household",
      "arity": 2, "slot_entities": ["Person", "Household"],
      "lookup_slots": [1],
      "provenance": { "kind": "supplied" } },
    { "name": "us:...#relation.snap_unit",
      "arity": 2, "slot_entities": ["Person", "Household"],
      "lookup_slots": [1],
      "provenance": { "kind": "derived",
                      "rule": "us:...#relation.snap_unit",
                      "source_relation": "us:statutes/7/2012/j#relation.member_of_household",
                      "reads": { "inputs": [], "rules": ["us:...#snap_member_eligible"],
                                 "relations": ["us:statutes/7/2012/j#relation.member_of_household"] } } }
  ]
}
```

Every list is sorted (entities by kind, inputs by name, relations by name,
inner lists lexicographically) and every struct rejects unknown fields.

### Entities

One entry per entity kind the program evaluates on: each derived rule's
`entity`, each kind in a relation's effective slot kinds
(`relation_typing::effective_slot_entities`), and each filtered entity (a
derived relation's `entity`).

- `instances.source: "supplied"`: the caller supplies the instances. The count
  is the number supplied. The engine has no other count source: nothing in a
  program ties the number of `Person` ids to an input such as
  `household_size`.
- `instances.source: "alias"`: a filtered entity. Its ids are the ids of `of`
  (`relation_typing::filtered_entity_kinds`), one per instance of `of`. `via`
  names the derived relations that filter to it.
- `instances.source: "unscoped"`: the entity-free scope, `Scalar`. It is listed
  only when an input lists it (below).

`id_template` is `<snake_case(kind)>:{index}`, `{index}` counting from 1; an
alias repeats the template of the kind it aliases; `Scalar` has none. Entity
ids are opaque to the engine and any unique string binds. The template is the
spelling a host mints when the caller gives none, so two hosts name the same
instance the same way.

### Inputs

One entry per slot that at least one expression reads. Each such slot is in
`metadata.input_catalog`; catalog slots absent here are exactly the
`indexed_by` labels nothing reads. `legal_id` is the catalog's
`canonical_request_name`; the catalog keeps the full `request_names` set.

**`entities`** is the set of entity kinds whose ids a read site of the slot is
evaluated on, by evaluation flow, not by the reading rule's label:

- `K(R)`, the kinds a rule `R` is evaluated on, is the least set such that:
  `R`'s own entity is in it (as its id kind, so `SnapUnit` contributes
  `Household`) unless `R` is `Scalar`; and every reference to `R` adds the
  kinds of the id that reference evaluates on.
- A reference or an input read in a rule body evaluates on `K` of the
  enclosing rule. Inside an aggregate's `where` clause or summed value it
  evaluates on the related slot's kind. Inside a derived relation's predicate
  a bare input evaluates on the related slot's kind, and a referenced rule on
  the current slot's kind when its entity is the derivation's declared current
  kind, otherwise on the related slot's kind. These are the rules
  `relation_typing::Checker` already applies.
- A `Scalar` rule that no entity-bearing rule reaches has `K = {Scalar}`.

Where every rule is referenced only by rules of its own entity, `entities` is
the reading rule's entity. Where it is not, the list says so: the slots a
`Person` rule reads list `TaxUnit` too once a `TaxUnit` rule references that
rule directly, because that is the id the engine will look them up on.

**`dtype`** is the type every read site demands, or null. `dtype_basis` is
`"usage"` when `dtype` is set and null otherwise; `"declared"` is reserved in
the schema for the day RuleSpec declares input types. Evidence, per read site:

| Evidence | Read site |
|---|---|
| `bool` | compared with a boolean literal |
| `text` | compared with a text literal |
| `date` | date operand of `date_add_*` or `days_between`; compared with a date literal, `period_start`, `period_end` or a `date_add_*` result |
| `integer` | index of a parameter lookup; offset operand of `date_add_*` |
| `decimal` | operand of `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil`, `floor`; summed value of `sum_related`; compared with an integer or decimal literal or with the result of arithmetic, `count_related`, `sum_related` or `days_between` |

A site with none of these gives no evidence (an input compared with another
input, with a rule or with a parameter, or returned as a rule's value). The
slot's `dtype` is the single evidence kind when all sites with evidence agree;
`integer` when the evidence mixes `integer` and `decimal`; null when there is
none or it conflicts. `decimal` means a number: the engine accepts an integer
wherever it accepts a decimal. A rule's declared dtype is not evidence:
`Engine::scalar_rule` caches a rule's value without checking it against the
declaration, so a comparison with a rule constrains nothing the engine
enforces.

**`default`** is the literal when every read site of the slot is an
`input_or_else` with the same default, else null. Null means a missing value
is an evaluation error at any read site reached.

**`values`** is a closed domain or null. It is set only when every read site
of the slot is the direct index of a lookup on an indexed parameter; it is
then the sorted union of those parameters' keys over all versions, and
`values_basis` is `"parameter_index"`. Outside that set no read site can
succeed (`lookup_parameter` returns `MissingParameterValue`). An equality
comparison does not close a domain, and neither does a mixed use (a table
index that is also compared or computed with), because a value outside the
table can still evaluate.

### Relations

One entry per `program.relations` entry, same name, arity and effective slot
kinds. `slot_entities` is null only for a relation that declares none, which
relation typing allows only when nothing executes it.

`lookup_slots` is the sorted set of slots that executed aggregates and
membership tests key on, with a derived relation's traversal of its source
counted against the source. It is empty exactly when nothing executes the
relation. A kind that appears only outside a relation's lookup slots is the
many side of that relation.

`provenance.kind` is `supplied` for a data relation and `derived` for a
`derived_relation`. `derived` carries `rule` (the id of the rule that derives
it, today the relation's own id), `source_relation`, and `reads`: the input
slots, rule ids and relations its predicate reads directly. Nothing here is
specific to unit derivation. Today a derived relation also unions tuples
supplied under its own name; the interface records where its tuples come from
by rule, and the refusal of supplied tuples for a derived relation (ratified
in `docs/unit-derivation.md`) is not part of this change.

## Loading

`CompiledProgramArtifact::from_json_*` reads format 3 only:

- no `artifact_format_version`: rejected, as today;
- a lower version: "compiled artefact `<path>` is older than this engine: it
  is artifact format 2 (written by engine 0.2.2) and this engine (0.2.2,
  build 5d63e8f) reads format 3", followed by the two ways forward: recompile,
  or run it unchanged through the format-2 compatibility loader;
- a higher version: "this engine (...) is older than compiled artefact
  `<path>` (format 4, written by engine ...); upgrade the engine";
- format 3: top-level keys are a closed set, `engine_version`, `engine_build`,
  `program.interface` and `metadata.input_catalog` are required,
  `program.extends` is refused outright, and `interface` must equal
  what this engine derives from the embedded program. The existing metadata
  and relation-typing checks are unchanged.

The format-2 dialect tolerances (a null `extends`, an absent input catalog
filled from the program, an absent `engine_version`) move out of the default
loader into `from_legacy_format_2_json_str` / `_file`. That loader accepts
format 2 only, refuses a format-2 artifact that carries `interface` or the
new identity fields, and returns an artifact that is still format 2:
`artifact_format_version` is 2, `program.interface` is `None`, and it
reserializes to the bytes it would have before this change. The engine never
derives an interface for it, so nothing downstream can mistake it for a
format-3 artifact. There is no 2-to-3 upgrade command: a format-3 artifact
states who compiled it and from what, and only a compile knows that.

Callers opt in by name:

- CLI: `run-compiled --accept-artifact-format 2` (and the two unit-aggregation
  commands), with a stderr notice. `migrate artifact` reads and writes format
  2 as before.
- Rust: the `from_legacy_format_2_*` functions.
- Python: `accept_artifact_format=2` on the client and the dense loader; the
  `CompiledProgram` model is format 3 and `LegacyCompiledProgramV2` is the old
  one.
- wasm: `execute_legacy_format_2`.

`capabilities` reports `artifact_format_version: 3`,
`legacy_artifact_formats: [2]`, `engine_build`, and adds `artifact_interface`
to `capabilities`.

A raw `ExecutionRequest.program` may carry `interface`; execution ignores it,
as it ignores `module`. `compile` always overwrites it.

## The interface derivation is part of format 3

The loader compares the stored interface with its own derivation, so a later
engine that derived a different interface for the same program would refuse
artifacts already published, which is the `fast_path` problem of 2026-10-04.
For format 3 the derivation above is frozen: changing what any program's
interface contains is a new format version. Golden fixtures pin it.

## Invariants and tests

1. **Executable content is unchanged.** For every program, the format-3
   artifact with `program.interface` removed has the same `program` and
   `metadata` as the format-2 artifact main compiles. Tested against format-2
   fixtures written by the unmodified engine at 5d63e8f3 for every in-repo
   RuleSpec fixture, and over the corpus by compiling each module with both
   engines.
2. **Round trip.** Serialize, load, serialize is byte-identical, for format 3
   and, through the legacy loader, for format 2. Property test over generated
   programs plus the fixtures.
3. **The interface agrees with the program.** Every interface the compiler
   emits passes the loader's re-derivation; every input slot is in the
   catalog with the same canonical name; every relation entry matches its
   `program.relations` entry; every `entities` kind is in the roster.
   Property test, and every corpus program that compiles.
4. **Format 2 is never read as format 3.** The default loader refuses every
   format-2 fixture; the legacy loader returns format 2 with no interface and
   refuses format 3; a format-2 payload restamped 3, and a format-3 payload
   restamped 2, are both refused.
5. **Tamper checks.** Changing, adding or removing any interface entry, or any
   field of one, is refused at load.
6. **`dtype` is sound.** For a generated program and a slot with an inferred
   dtype, supplying a value of another type fails with a type error at a
   reached read site, and supplying the inferred type does not fail on type.
7. **`entities` is sound.** For a generated multi-entity program, evaluating
   any rule on an id of its own entity reads each slot only on ids whose kind
   the slot lists.
8. **Execution parity.** Explain, fast and dense results are identical for a
   format-2 artifact through the legacy loader and the format-3 artifact of
   the same source.

Corpus impact is reported as: modules compiled by main, modules compiled by
the branch (the two sets must be equal), modules whose artifact gains the
block, failures by cause, and how many slots list more than one entity.

## Surfaces touched

`src/compile.rs`, new `src/interface.rs`, new `src/digest.rs` (the SHA-256
routine moves out of `unit_derivation`), new `build.rs`, `src/rulespec.rs`
(module hashes into the lowering outcome), `src/spec.rs`
(`ProgramSpec.interface`, last field, omitted when `None`), `src/schema.rs`
(format 3 generated; `compiled-artifact.v2.schema.json` frozen as an archive
and added to the crate `include` list), `src/main.rs`, `src/migrate.rs`,
`src/unit_derivation` (embedded artifacts), `python/`, `python-ext/`, `wasm/`,
the three RuleSpec docs, `README.md` and `DECISIONS.md`.

Not touched: `PROGRESS.md`, workflow pins, CODEOWNERS, `release/v0.1`, tags.

## Deferred

- Declared input dtype, default and domain. They need RuleSpec fields and the
  encoder to emit them; `dtype_basis: "declared"` is reserved.
- A compile diagnostic for a rule referenced directly from a rule of another
  entity. The interface reports the consequence; whether those references are
  encoding defects is a corpus question.
- Refusing supplied tuples for a derived relation.
- The corpus-provisions index in the source closure.
