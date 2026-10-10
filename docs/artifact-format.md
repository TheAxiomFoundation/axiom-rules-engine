# Compiled artifact format 3

Status: design note for axiom-rules-engine#155, riding the format bump in #125.
This file becomes the format reference when the change lands. Every statement
about current behavior cites the code it was read from, at `main` 5d63e8f3.
Revised after an independent review of the first draft; the review's twelve
findings are answered in "Review record" at the end.

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

Execution does not read any of it. `program` without `interface`, and
`metadata`, are what format 2 emitted for the same source.

## Facts the design rests on

- **Inputs are not declared.** A formula identifier that names no parameter or
  rule lowers to an `input` node (`docs/rulespec-format.md`, "Identifier
  resolution"). RuleSpec has no field for an input's dtype, entity, default or
  domain.
- **An input is read by `(slot name, entity id)`.** `Engine::lookup_input`
  (`src/engine.rs`) keys on the bare slot and the id being evaluated. The
  record's `entity` label is not part of the key, so two instances of
  different kinds that share an id string share their inputs.
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
- **An entity-free rule can be queried on any id.** A query evaluates its
  outputs on its `entity_id`, and a `Scalar` rule gives that id no kind
  (`query_entity_kinds`, `src/api.rs`).
- **`metadata.input_catalog` lists names no expression mentions.** A
  parameter's `indexed_by` label enters the catalog whether or not an
  expression reads it (`Program::input_catalog`, `src/model.rs`); 99 such
  slots appear in those artifacts.
- **`input_or_else` is the only declared default**, and the formula language
  cannot produce it; none appears in those artifacts.
- **A derived relation also reads tuples supplied under its own name.**
  `related_entity_ids_at_level` unions the filtered source members with
  `direct_related_ids(relation, ...)`.
- **Nothing constrains how many tuples an id appears in.** A relation schema
  holds a name, an arity, slot kinds and a derivation (`RelationSchema`,
  `src/model.rs`); binding checks tuple length and slot kinds
  (`RelationRecordSpec::to_model_for_program`, `src/spec.rs`).
- **Type demands are static and local.** Arithmetic operands go through
  `as_decimal`, parameter indices and date offsets through `as_index` (which
  accepts a tagged integer or a decimal with zero fraction), date positions
  through `as_date`, and `compare_scalar_values` accepts bool with bool and
  text with text for `==` and `!=` only, date with date, or two numbers. Any
  other combination is a `TypeMismatch` when the node is reached. A plain
  `input` node returns its value unchecked, and `Engine::scalar_rule` caches a
  rule's value without checking it against the rule's declared dtype.
- **Stage-3 unit aggregation compares whole artifacts.** It digests the
  serialization of the source artifact it loaded (`source_artifact_digest`)
  and compares the stored phase-two artifact, as a JSON value, with one it
  regenerates on the running engine (`validate_compiled_artifact`,
  `src/unit_derivation/aggregation.rs`). The second comparison already
  includes `engine_version`; `tests/fast_path_metadata.rs` restamps that field
  so its fixture survives a crate version cut.
- **The format-2 loader normalizes.** It collapses compatible duplicate
  parameters, fills an absent input catalog, and drops a null `extends`
  (`check_format_version`, `validate_raw_artifact_contract`, `src/compile.rs`).

## Top-level shape

```jsonc
{
  "artifact_format_version": 3,        // wire format
  "engine_version": "0.2.2",           // semantic version of the compiling engine
  "engine_build": {                    // build identity
    "commit": "5d63e8f3...40 hex",     //   or null when the build cannot know
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

| Identity | Field | At load |
|---|---|---|
| Wire format | `artifact_format_version` | Exact match, or the named legacy loader |
| Semantic version | `engine_version` (required in format 3) | Recorded |
| Build identity | `engine_build` (required in format 3) | Recorded |
| Source closure | `source_closure` (omitted when compiled from IR) | Shape only |

Only the wire format gates loading. `engine_version` names the engine that
compiled the artifact. It is a producer identity: two engines with the same
version are not promised to evaluate alike, and a different version is not a
reason to refuse. #125 asks for the identities to stop sharing one string; it
does not ask for a separate evaluation-semantics number, and this change does
not add one.

`engine_build` comes from a new `build.rs` that uses only `std` and never
fails a build:

- `commit` is `AXIOM_RULES_ENGINE_BUILD_COMMIT` when set to 40 hex digits;
  else `git rev-parse HEAD` when the git top level is the crate directory (a
  vendored copy inside another repository reports null, not that repository's
  commit); else the `sha1` in `.cargo_vcs_info.json` for a published crate;
  else null.
- `dirty` is true when `src/`, `build.rs`, `Cargo.toml` or `Cargo.lock` differ
  from that commit; from `.cargo_vcs_info.json`, its `dirty` flag when
  present; with the environment override,
  `AXIOM_RULES_ENGINE_BUILD_DIRTY=true|false` or null; null whenever `commit`
  is null.
- It reruns on `rerun-if-env-changed` for both variables and
  `rerun-if-changed` for `src`, `build.rs`, `Cargo.toml`, `Cargo.lock`,
  `.cargo_vcs_info.json`, and the resolved git paths of `HEAD`, the ref it
  points to and `packed-refs` (`git rev-parse --git-path`, so a linked
  worktree resolves correctly).
- `build.rs` joins the `include` list in `Cargo.toml`, or a published crate
  would not build.

The same source compiled by two builds of one commit gives the same bytes.
Two commits give artifacts that differ in `engine_build` only when nothing
else changed.

`source_closure.modules` lists every module text the loader read to lower the
program, the root included, sorted by target, each with the SHA-256 of the
exact text. An originless root (an in-memory string, or a composition file)
has `target: null` and sorts first. `digest` is the SHA-256 of
`axiom-rules-engine/source-closure/v1\n` followed by one `<target>\t<sha256>\n`
line per module (an empty target for the originless root). A program compiled
from a `ProgramSpec` has no RuleSpec source, so `source_closure` is omitted.
The closure identifies RuleSpec module text and nothing else: a
`--corpus-provisions` index changes `source_url` fields and is not in it.

The crate version does not move in this change; versions are cut in their own
commits. The next cut should be 0.3.0.

## `program.interface`

```jsonc
"interface": {
  "entities": [
    { "kind": "Household", "id_template": "Household:{index}",
      "instances": { "source": "supplied" } },
    { "kind": "Person", "id_template": "Person:{index}",
      "instances": { "source": "supplied" } },
    { "kind": "SnapUnit", "id_template": "Household:{index}",
      "instances": { "source": "alias", "of": "Household",
                     "via": ["us:...#relation.snap_unit"] } }
  ],
  "inputs": [
    { "name": "household_size",
      "legal_id": "us:statutes/7/2017/a#input.household_size",
      "entities": ["Household"],
      "dtype": "integer", "dtype_basis": "usage",
      "default": null,
      "values": [1, 2, 3, 4, 5, 6, 7, 8], "values_basis": "parameter_index" }
  ],
  "relations": [
    { "name": "us:statutes/7/2012/j#relation.member_of_household",
      "arity": 2, "slot_entities": ["Person", "Household"],
      "lookup_slots": [1],
      "accepts_supplied_tuples": true,
      "provenance": { "kind": "supplied" } },
    { "name": "us:...#relation.snap_unit",
      "arity": 2, "slot_entities": ["Person", "Household"],
      "lookup_slots": [1],
      "accepts_supplied_tuples": true,
      "provenance": { "kind": "derived",
                      "rule": "us:...#relation.snap_unit",
                      "source_relation": "us:statutes/7/2012/j#relation.member_of_household",
                      "reads": { "inputs": [], "rules": ["us:...#snap_member_eligible"],
                                 "relations": ["us:statutes/7/2012/j#relation.member_of_household"] } } }
  ]
}
```

Set-valued lists are sorted: entities by kind, inputs by name, relations by
name, and `entities`, `via`, `reads.*` lexicographically; `lookup_slots` and
`values` numerically. `slot_entities` is positional and keeps tuple order.
Every object has exactly the keys shown for its shape: an unknown key is
refused, and so is a missing one, including a nullable key, which is written
as an explicit `null`.

The block describes the program under the reference (explain) evaluator, for
queries and datasets that are well-kinded: each rule queried on an id of its
own entity's kind, and tuples bound to the relation's slot kinds (the strict
binding default). Fast mode evaluates the same ids. The dense compiler
refuses a program in which a rule depends directly on a rule of another named
entity (`DenseCompileError::CrossEntityDependency`), so it never reads a slot
somewhere this block does not list.

### Entities

One entry per entity kind the program can evaluate on: each derived rule's
`entity`, each kind in a relation's effective slot kinds
(`relation_typing::effective_slot_entities`), and each filtered entity (a
derived relation's `entity`).

- `instances.source: "supplied"`: the caller supplies the instances, and the
  count is the number supplied. The engine has no other count source: nothing
  in a program ties the number of `Person` ids to an input such as
  `household_size`.
- `instances.source: "alias"`: a filtered entity. Its ids are the ids of `of`
  (`relation_typing::filtered_entity_kinds`), and `via` names the derived
  relations that filter to it. `of` is null when the program does not
  establish the kind, which happens only when nothing executes those
  relations (`relation_typing::unresolved_filtered_entities`).
- `instances.source: "unscoped"`: the entity-free scope, `Scalar`. It is
  listed whenever an input lists it.

`id_template` is the kind verbatim, a colon, and `{index}` counting from 1:
`TaxUnit:{index}`. An alias repeats the template of the kind it aliases; an
alias with a null `of` and `Scalar` have a null template. Entity ids are
opaque to the engine and any string binds. The template is the spelling a
host mints when the caller gives none. Using the kind unchanged keeps the
mapping from kinds to ids one-to-one with no normalization rule to disagree
about, and keeps ids of different kinds distinct, which inputs need because
they are keyed by slot and id alone.

The engine declares no cardinality for any kind or relation.

### Inputs

A **read site** of a slot is any `input` or `input_or_else` node naming it, or
a `sum_related` whose summed value is that input, in any rule's base
expression or versions or in any derived relation's predicate. These are the
occurrences `Program::input_catalog` collects, so the slots listed here are
the catalog's slots minus the `indexed_by` labels no expression mentions.
Read sites are potential reads: an unselected branch, a `no_match` pattern
and an `over_periods` operand count, although an evaluation may never reach
them. `legal_id` is the catalog's `canonical_request_name`; the catalog keeps
the full `request_names` set.

**`entities`** lists every entity kind whose ids a read site of the slot can
be evaluated on. It follows evaluation flow, not the reading rule's label,
and it is a superset of the kinds an evaluation actually reads the slot on:

- `K(R)`, the kinds a rule `R` can be evaluated on, is the least set such
  that: `R`'s own scope is in it (its entity as an id kind, so `SnapUnit`
  contributes `Household`; `Scalar` for an entity-free rule, because any query
  may evaluate it directly); and every reference to `R` adds the kinds of the
  id that reference evaluates on.
- A reference or a read site in a rule body evaluates on `K` of the enclosing
  rule. Inside an aggregate's `where` clause or summed value it evaluates on
  the related slot's kind. Inside a derived relation's predicate a read site
  evaluates on the related slot's kind, and a referenced rule on the current
  slot's kind when its entity is the derivation's declared current kind,
  otherwise on the related slot's kind. These are the context rules
  `relation_typing::Checker` applies.

Where every rule is referenced only by rules of its own entity, `entities` is
the reading rule's entity. Where it is not, the list says so: the slots a
`Person` rule reads list `TaxUnit` too once a `TaxUnit` rule references that
rule directly, because that is the id the engine will look them up on.

**`dtype`** is the narrowest type such that every read site rejects every
value outside it, or null when the read sites do not establish one.
`dtype_basis` is `"usage"` when `dtype` is set and null otherwise;
`"declared"` is reserved for the day RuleSpec declares input types. Each read
site demands one of:

| Demand | Read site |
|---|---|
| `bool` | operand of an `eq` or `ne` comparison whose other operand is a boolean literal |
| `text` | operand of an `eq` or `ne` comparison whose other operand is a text literal |
| `date` | date operand of `date_add_*`; operand of `days_between`; operand of a comparison whose other operand is a date literal, `period_start`, `period_end` or a `date_add_*` node |
| `integer` | index of a `parameter_lookup`; offset operand of `date_add_*` |
| `number` | operand of `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil` or `floor`; summed value of `sum_related`; operand of a comparison whose other operand is an integer or decimal literal or an `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil`, `floor`, `count_related`, `sum_related` or `days_between` node |
| none | every other position: the rule's value or an `if` branch; a comparison with an input, a rule, a parameter lookup or an `if`; an ordered comparison with a boolean or text literal; a `no_match` subject or pattern; anything inside an `over_periods` reduction |

The slot's `dtype` is null if any read site demands none. Otherwise it is
`bool`, `text` or `date` when every site demands that; `integer` when every
site demands `integer`; `decimal` when every site demands `integer` or
`number` and at least one demands `number`; and null for any other mixture.
`decimal` means a number, tagged integer or decimal. `integer` means a whole
number, tagged integer or a decimal with zero fraction. The claim runs one
way: a value outside the dtype cannot be read successfully anywhere. A value
inside it can still fail, for instance a fractional value at a table index of
a `decimal` slot. A rule's declared dtype is never evidence, because no
evaluator checks a rule's value against it.

**`default`** is the literal when every read site of the slot is an
`input_or_else` with the same default, else null. Null says only that there
is no uniform default.

**`values`** is a closed domain or null. It is set when every read site of
the slot is the index of a `parameter_lookup`; it is then the sorted union,
over those parameters and all their versions, of the table keys, and
`values_basis` is `"parameter_index"`. No whole number outside it can be read
successfully (`lookup_parameter` returns `MissingParameterValue`); a value in
it can still miss in a period whose table version lacks the key. Values
compare as numbers, so a decimal `1.0` is the key `1`. An equality
comparison does not close a domain, and neither does a mixed use.

### Relations

One entry per `program.relations` entry, with its name, arity and effective
slot kinds. `slot_entities` is null only for a relation that declares none,
which relation typing allows only when nothing executes it.

`lookup_slots` is the set of slots that aggregates and membership tests in
the program key on, with a derived relation's traversal of its source
counted against the source. It is empty when no expression uses the
relation. It records direction of traversal and nothing about cardinality.

`accepts_supplied_tuples` is true for every relation today: the evaluator
reads tuples supplied under a relation's own name whether or not it is
derived.

`provenance.kind` says what the declaration makes the relation: `supplied`
for a data relation, `derived` for a `derived_relation`. `derived` carries
`rule` (the id of the rule that derives it, today the relation's own id),
`source_relation`, and `reads`: the input slots, rule ids and relations its
predicate mentions directly. Nothing here is specific to unit derivation. The
refusal of supplied tuples for a derived relation, ratified in
`docs/unit-derivation.md`, is not part of this change; when it lands,
`accepts_supplied_tuples` is where it shows.

## In Rust

`ProgramSpec` does not change, so a raw `ExecutionRequest.program` and the
`execution-request.v1` schema do not change either. `CompiledProgramArtifact`
gains `interface: Option<ProgramInterface>`, `engine_build` and
`source_closure` beside `program`. Its serialization goes through a wire
struct that repeats `ProgramSpec`'s fields in order and appends `interface`,
so the block sits at `program.interface` on the wire. The wire struct does
not use `#[serde(flatten)]`: flattening buffers values, and the buffered form
cannot read the integer keys of a parameter table. A test pins that the wire
struct without `interface` serializes byte for byte as `ProgramSpec` does.

## Loading

`CompiledProgramArtifact::from_json_*` reads format 3 only:

- no `artifact_format_version`: rejected, as today;
- a lower version: "compiled artefact `<path>` is older than this engine: it
  is artifact format 2 (written by engine 0.2.2) and this engine (0.2.2,
  build 5d63e8f) reads format 3", followed by the two ways forward: recompile,
  or run it unchanged through the format-2 compatibility loader;
- a higher version: "this engine (...) is older than compiled artefact
  `<path>` (format 4, written by engine ...); upgrade the engine";
- format 3: top-level keys are a closed set; `engine_version`, `engine_build`,
  `program.interface` and `metadata.input_catalog` are required;
  `program.extends` is refused outright; and `interface` must equal what this
  engine derives from the embedded program. The existing metadata and
  relation-typing checks are unchanged.

The format-2 dialect tolerances move out of the default loader into
`from_legacy_format_2_json_str` / `_file`. That loader accepts format 2 only
and refuses a format-2 payload that carries `interface`, `engine_build` or
`source_closure`. What it returns is still format 2:
`artifact_format_version` is 2 and `interface` is `None`. The engine never
derives an interface for it, so nothing downstream can mistake it for a
format-3 artifact. Three separate statements hold for its bytes:

- an artifact this code line compiled, in either format, round-trips byte for
  byte through its loader;
- for any format-2 payload main accepts, the legacy loader's result
  serializes exactly as main's loader result does, so every stage-3 digest of
  a loaded source artifact is what it was;
- original wire bytes are not preserved where the dialect is normalized
  (duplicate parameters, an absent catalog, a null `extends`), as today.

There is no 2-to-3 upgrade command: a format-3 artifact states who compiled it
and from what, and only a compile knows that.

Callers opt in by name:

- CLI: `run-compiled --accept-artifact-format 2`, and the two unit-aggregation
  commands, with a stderr notice. `migrate artifact` reads and writes format
  2 as before.
- Rust: the `from_legacy_format_2_*` functions.
- Python: `accept_artifact_format=2` on the client and the dense loader; the
  `CompiledProgram` model is format 3 and `LegacyCompiledProgramV2` is the old
  one.
- wasm: `execute_legacy_format_2`.

`capabilities` reports `artifact_format_version: 3`,
`legacy_artifact_formats: [2]`, `engine_build`, and adds `artifact_interface`
to `capabilities`.

### Stage-3 unit aggregation

A stage-3 artifact embeds a source artifact and a phase-two artifact. Both
load through the default loader, and through the legacy loader only when the
caller opts in, in which case both may be format 2. The phase-two check
compares executable content: the regenerated artifact's `program` and
`metadata` must equal the stored one's, and the stored one must pass its own
loader. Its `engine_version`, `engine_build` and format are the stored
artifact's own and are not compared with the running engine's, which also
ends the existing dependence on the crate version. The source digest is
unchanged: it is the digest of the loaded source artifact's serialization.

## The interface derivation is part of format 3

The loader compares the stored interface with its own derivation, so a later
engine that derived a different interface for the same program would refuse
artifacts already published, which is the `fast_path` problem of 2026-10-04.
For format 3 the derivation above is fixed. Changing what any program's
interface contains is a new format version, whose engine reads format 3
through a compatibility loader that keeps this derivation. That cost is why
the derivation claims little: structural facts, and usage rules that
transcribe what the evaluators enforce. Golden fixtures pin it.

## Invariants and tests

1. **Executable content is unchanged.** For every program, the format-3
   artifact's `program` without `interface`, and its `metadata`, equal the
   format-2 artifact main compiles. Tested against format-2 fixtures written
   by the unmodified engine at 5d63e8f3 for every in-repo RuleSpec fixture,
   and over the corpus by comparing each module with main's artifact.
2. **Round trip.** The three byte statements under "Loading": canonical
   artifacts round-trip (property test and fixtures); the legacy loader
   serializes as main's loader does (fixtures written by main, including one
   in each normalized dialect); normalization is idempotent.
3. **The interface agrees with the program.** Every interface the compiler
   emits passes the loader's re-derivation; the input slots are the catalog's
   slots minus unmentioned `indexed_by` labels, with the same canonical
   names; every relation entry matches its `program.relations` entry; every
   kind an input or relation names is in the roster. Property test, and every
   corpus program that compiles. This shows self-consistency; 6 and 7 test
   the derivation against the evaluator.
4. **Format 2 is never read as format 3.** The default loader refuses every
   format-2 fixture and names the artifact as the older side; the legacy
   loader returns format 2 with no interface and refuses format 3; a format-2
   payload restamped 3 and a format-3 payload restamped 2 are both refused; a
   format-4 stamp names the engine as the older side.
5. **Tamper checks.** For every key of every interface object: changing its
   value, removing it (nullable keys included) and adding an unknown key are
   each refused at load.
6. **`dtype` and `values` are sound.** For a generated program and a slot
   with a dtype, give every other input a well-typed value. If evaluating a
   rule without the slot fails for want of that slot, evaluating it with a
   value outside the dtype fails with a type error; if it succeeds without
   the slot, it gives the same result with that value. The same property with
   a whole number outside `values` and a missing-parameter-value error.
7. **`entities` is sound.** For a generated multi-entity program with
   well-kinded queries and tuples, every missing-input error an evaluation
   can raise names an id whose kind the slot lists, where an entity-free
   query's id has kind `Scalar`. The probe is the evaluator's own error, not
   the derivation.
8. **Execution parity.** In each of explain, fast and dense, the format-2
   artifact through the legacy loader and the format-3 artifact of the same
   source give identical results.
9. **Stage-3 survives a rebuild.** A stage-3 artifact whose embedded
   artifacts carry another `engine_build` and `engine_version` loads; one
   whose phase-two `program` or `metadata` differs is refused; the artifact
   the previous engine wrote loads through the legacy path with its source
   digest unchanged.

Corpus impact is reported as: modules compiled by main, modules compiled by
the branch (the two sets must be equal), modules whose artifact gains the
block, failures by cause, how many slots list more than one entity, and how
many have a dtype, a default and a closed domain.

## Surfaces touched

`src/compile.rs`, new `src/interface.rs`, new `src/digest.rs` (the SHA-256
routine moves out of `unit_derivation`), new `build.rs`, `Cargo.toml`
(`include`), `src/rulespec.rs` (module hashes into the lowering outcome),
`src/schema.rs` (format 3 generated; `compiled-artifact.v2.schema.json`
frozen as an archive and added to the crate `include` list), `src/main.rs`,
`src/migrate.rs`, `src/unit_derivation` (embedded artifacts and the
phase-two comparison), `python/`, `python-ext/`, `wasm/`, the three RuleSpec
docs, `README.md` and `DECISIONS.md`.

Not touched: `src/spec.rs`'s `ProgramSpec`, the evaluators, `PROGRESS.md`,
workflow pins, CODEOWNERS, `release/v0.1`, tags.

## Deferred

- Declared input dtype, default and domain. They need RuleSpec fields and the
  encoder to emit them; `dtype_basis: "declared"` is reserved.
- Cardinality of relations. Nothing declares it today.
- A compile diagnostic for a rule referenced directly from a rule of another
  entity. The interface reports the consequence; whether those references are
  encoding defects is a corpus question.
- Refusing supplied tuples for a derived relation.
- The corpus-provisions index in the source closure.

## Review record

An independent review (GPT-6.1 Sol, 2026-10-10) requested changes on the first
draft. Each finding and what the note does about it:

1. A build stamp would break stage-3 loading, which compares whole
   artifacts. Fixed: the phase-two check compares `program` and `metadata`
   ("Stage-3 unit aggregation", invariant 9).
2. The inferred dtype ignored read sites with no evidence (`a = x + 1`,
   `b = x`). Fixed: any site that demands nothing makes the dtype null, bool
   and text evidence is limited to `==` and `!=`, the claim is one-way, and
   `integer` is stated as a whole number.
3. `K(R)` dropped the direct query of an entity-free rule once another rule
   referenced it. Fixed: an entity-free rule always contributes `Scalar`.
4. An unused derived relation over an untyped source has no alias target.
   Fixed: `of` is nullable.
5. `lookup_slots` cannot establish a "many side". Fixed: the claim is
   removed; the engine declares no cardinality.
6. A null `default` does not make a missing value an error. Fixed: null means
   no uniform default.
7. Byte identity for the legacy loader was over-stated. Fixed: three separate
   statements, tested separately.
8. `derived` hid that supplied tuples are still read. Fixed:
   `accepts_supplied_tuples`.
9. The build script's reruns omitted the environment override. Fixed, with
   `.cargo_vcs_info.json` and the crate `include` list.
10. Sorting every inner list would reorder `slot_entities`. Fixed: it is
    positional.
11. Adding the field to `ProgramSpec` changed the raw-request schema. Fixed:
    `ProgramSpec` is untouched; the artifact serializes through a wire
    struct.
12. `snake_case` was unspecified and could collide. Fixed: the template uses
    the kind verbatim.

The review also asked that read sites be defined as potential reads (done,
matching the catalog), that parity be compared within each mode (invariant
8), and that the reading of #125's "semantic version" be made explicit (under
"The four identities"). Its suggestion to stop freezing the derivation is not
taken: the loader's comparison needs one derivation per format, so the note
keeps the freeze and narrows what is derived.
