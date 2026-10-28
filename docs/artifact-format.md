# Compiled artifact format 3

This is the compiled-artifact reference for axiom-rules-engine. The generated
[format-3 schema](../schemas/compiled-artifact.v3.schema.json) describes its
wire shape; compilation and loading also enforce the consistency checks below.

## Format-3 contract

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

Execution does not read these additions. For source without election
declarations, `program` without `interface`, and `metadata`, are what format 2
emits for the same source. Elections add inert `program.input_declarations`
content, described below.

## Evaluation and compilation facts

- **Most inputs are inferred from expressions.** A formula identifier that
  names no parameter or rule lowers to an `input` node
  ([Identifier resolution](rulespec-format.md#identifier-resolution)). The
  interface derives type demands, entity flow, defaults and domains from read
  sites. The `inputs:` block supplies elections; the `only_related` and
  `input_for_year` constructs also validate declarations there.
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
- **That difference occurs in published RuleSpec.** Main at 5d63e8f3
  compiles 4,342 of the 4,878
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
- **`input_or_else` supplies an input default.** The formula language cannot
  produce it; raw `ProgramSpec` expressions can. No such read appears in the
  corpus measurement above.
- **Supplied derived tuples depend on the program's constructs.** Programs
  using qualified reads, relation atoms or term comparisons refuse them at
  request binding (`supplied_tuples_for_derived_relation`), and their members
  resolver ignores them through `Engine::new` too. Other programs union the
  filtered source members with `direct_related_ids(relation, ...)`, including
  programs whose predicates read parameters, which raw `ProgramSpec` programs
  could always express.
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
- **Stage-3 unit aggregation checks executable content.** It digests the
  serialization of the source artifact it loaded (`source_artifact_digest`)
  and compares the stored phase-two artifact's `program` and `metadata` with
  a regenerated artifact's. Both embedded artifacts must pass their own
  loaders (`validate_compiled_artifact`,
  `src/unit_derivation/aggregation.rs`). Producer versions and builds may
  differ from the running engine's.
- **The format-2 loader normalizes.** It collapses compatible duplicate
  parameters, fills an absent input catalog, and drops a null `extends`
  (`from_format_2_source`, `check_format_2`, `validate_raw_artifact_contract`,
  `src/compile.rs`).

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

`engine_build` comes from `build.rs`, which uses only `std` and never
fails a build:

- `commit` is `AXIOM_RULES_ENGINE_BUILD_COMMIT` when set to 40 hex digits;
  else `git rev-parse HEAD` when the git top level is the crate directory (a
  vendored copy inside another repository has no git identity); else the
  `sha1` in `.cargo_vcs_info.json` for a published crate;
  else null.
- `dirty` is true when `src/`, `build.rs`, `Cargo.toml` or `Cargo.lock` differ
  from that commit; from `.cargo_vcs_info.json`, its `dirty` flag when
  present; with the environment override,
  `AXIOM_RULES_ENGINE_BUILD_DIRTY=true|false` or null; null whenever `commit`
  is null.
- It reruns on `rerun-if-env-changed` for both variables and
  `rerun-if-changed` for existing `src`, `build.rs`, `Cargo.toml`, `Cargo.lock`,
  `.cargo_vcs_info.json`, and the resolved git paths of `HEAD`, the ref it
  points to and `packed-refs` (`git rev-parse --git-path`, so a linked
  worktree resolves correctly).
- `build.rs` is in the crate's `Cargo.toml` `include` list.

Compilation is byte-deterministic for a fixed build: the same source, the
same compile options and an engine with the same `engine_build` give the same
bytes. Builds whose `engine_build` differs (another commit, a dirty tree, an
override) give artifacts that differ in that field.

`source_closure.modules` lists every module text the loader read to lower the
program, the root included, sorted by target, each with the SHA-256 of the
exact text. An originless root (an in-memory string, or a composition file)
has `target: null` and sorts first. `digest` is the SHA-256 of
`axiom-rules-engine/source-closure/v1\n` followed by one `<target>\t<sha256>\n`
line per module (an empty target for the originless root). A program compiled
from a `ProgramSpec` has no RuleSpec source, so `source_closure` is omitted.
The closure identifies RuleSpec module text and nothing else: a
`--corpus-provisions` index changes `source_url` fields and is not in it.

The crate version and artifact format advance independently.

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
      "values": [1, 2, 3, 4, 5, 6, 7, 8], "values_basis": "parameter_index",
      "election": null },
    { "name": "provider_requests_boarder_inclusion",
      "legal_id": "us:regulations/7/273/1#input.provider_requests_boarder_inclusion",
      "entities": ["Person"],
      "dtype": "bool", "dtype_basis": "usage",
      "default": null, "values": null, "values_basis": null,
      "election": { "actor": "Household",
                    "provision": "us/regulation/7/273/1/b/3" } }
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
(`relation_typing::effective_slot_entities`), and each entity a derived
relation filters to.

- `instances.source: "supplied"`: the caller supplies the instances, and the
  count is the number supplied. The engine has no other count source: nothing
  in a program ties the number of `Person` ids to an input such as
  `household_size`.
- `instances.source: "alias"`: a filtered entity whose ids are the ids of
  another kind, `of` (`relation_typing::filtered_entity_names` and
  `filtered_entity_kinds`); `via` names the derived relations that filter to
  it. `of` is null when the program does not establish the kind, which
  happens only when nothing executes those relations
  (`relation_typing::unresolved_filtered_entities`). A filter of a kind to
  itself (households filtered to households) is a physical kind and stays
  `supplied`, as relation typing treats it.
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

A **read site** of a slot is any `input`, `input_or_else`, `input_for_year`
or `role_input` node naming it, or a `sum_related`, `only_related`,
`extreme_related` or `single_related` whose value is that input, in any rule's
base expression or versions or in any derived relation's predicate. These are the
occurrences `Program::input_catalog` collects, so the slots listed here are
the catalog's slots minus the `indexed_by` labels no expression mentions.
Read sites are potential reads: an unselected branch, a `no_match` pattern
and an `over_periods` operand count, although an evaluation may never reach
them. `legal_id` is the catalog's `canonical_request_name`; the catalog keeps
the full `request_names` set.
An `input_for_year` read uses its selected-year contract's runtime `slot`;
`legal_id` remains the slot's canonical catalog request name.

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
  otherwise on the related slot's kind. A read through a term
  (`role_input`, `role_derived`) evaluates on the term's id whatever the
  rule's entity: `anchor` on the current slot's kind, `candidate` on the
  related slot's kind. These are the context rules
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
| `bool` | value of `only_related`; operand of an `eq` or `ne` comparison whose other operand is a boolean literal or `only_related` |
| `text` | operand of an `eq` or `ne` comparison whose other operand is a text literal |
| `date` | date operand of `date_add_*`; operand of `days_between`; operand of a comparison whose other operand is a date literal, `period_start`, `period_end` or a `date_add_*` node |
| `integer` | index of a `parameter_lookup`; offset operand of `date_add_*` |
| `number` | operand of `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil` or `floor`; summed value of `sum_related`; operand of a comparison whose other operand is an integer or decimal literal or an `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil`, `floor`, `count_related`, `sum_related` or `days_between` node |
| none | every other position: the rule's value or an `if` branch; a comparison with an input or a rule (read directly or through a term), a parameter lookup or an `if`; an ordered comparison with a boolean or text literal; a `no_match` subject or pattern; anything inside an `over_periods` reduction |

The `over_periods` suppression covers every nested expression in its
operands; parameter-index domains and defaults remain structurally derived.

`input_for_year` keeps the demand of its expression position. The input
value of `single_related` also keeps the enclosing position's demand, because
the aggregate returns the one selected member's value. The input value of
`extreme_related` keeps that demand too, except that an `integer` demand
becomes `number`: the aggregate compares every selected member's value but
returns only the winner, so `min` of `1.0` and `1.5` meets a table index
although `1.5` does not. Neither infers a parameter-index domain through the
aggregate. This deliberately makes no additional claim from a selected-year
input's declared dtype or an extremum's own runtime restrictions.

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
successfully: the lookup fails with a missing parameter value, or with a
type error for a number no table key can hold. A value in it can still miss
in a period whose table version lacks the key. Values
compare as numbers, so a decimal `1.0` is the key `1`. An equality
comparison does not close a domain, and neither does a mixed use.

**`election`** says the input is a choice the law gives a named actor:
`actor` is the entity kind that makes it and `provision` the corpus citation
path of the provision that gives it. It is null for a fact. The engine
evaluates the value it is given; choosing among values belongs to the
simulation layer, which may branch over an election and must not optimize
over a fact. The engine never defaults an election, so a slot with an
election and any `input_or_else` read site is a compile error, and its
`default` is always null. A slot is an election when a module that reads it
declares it one (`program.input_declarations`, below); two declarations for
one slot that disagree on actor or provision are a compile error. Like the
rest of the block, `election` is derived from the program alone, so the
loader's re-derivation checks it.

#### Declaring an election in RuleSpec

A module's top-level `inputs:` block declares its inputs, as a list of
entries with `name` or as a mapping from name to entry. Every entry key
except `election` is inert for this derivation: the
corpus carries `entity`, `dtype`, `period`, `description` and `unit` there,
with dtypes such as `Count` that are not engine vocabulary, and a block with
no `election` contributes no election declarations. Programs using
`only_related` or `input_for_year` separately validate their input
declarations ([RuleSpec](rulespec.md#input-declarations-and-elections)).
`election` is
strict: `{actor, provision}` and no other key, `actor` an UpperCamelCase kind
label (it need not be a kind the program evaluates on), `provision` a
canonical corpus citation path. The entry must have a name, and the name
must be an input slot that a rule or derived-relation predicate of the same
module reads; an imported module is checked against its own rules. Each
failure is a load error naming the module and the entry.

```yaml
inputs:
  - name: provider_requests_boarder_inclusion
    entity: Person
    dtype: Boolean
    period: Month
    election:
      actor: Household
      provision: us/regulation/7/273/1/b/3
```

Lowering carries the declarations in `ProgramSpec.input_declarations`, its
last field, omitted when empty: a list sorted by `input`, each
`{input, election}`, where `input` is the declaring module's request name,
`<module>#input.<slot>`, or the bare slot for an originless composition
root. The interface derivation refuses declarations that are not in that
form: unsorted or repeated, naming an input the program does not read, or
malformed.
An unused parameter `indexed_by` label is insufficient: a declaration must
name an expression read even when the label appears in the input catalog.

### Relations

One entry per `program.relations` entry, with its name, arity and effective
slot kinds. `slot_entities` is null only for a relation that declares none,
which relation typing allows only when nothing executes it.

`lookup_slots` is the set of slots that aggregates, membership tests and atoms
in the program key on, with a derived relation's traversal of its source
counted against the source. It is empty when no expression uses the relation.
It records direction of traversal and nothing about cardinality.

An atom on a data relation records a lookup for each bound argument slot. An
atom on a derived relation records its current slot, as a membership test
does: it resolves that term's members and tests the related term against
them. A term comparison adds no lookup, input read or relation read.

`accepts_supplied_tuples` is false for every derived relation in a program
using qualified reads, relation atoms or term comparisons; otherwise it is
true. This is a program-wide restriction, including derived relations whose
own predicates use none of those constructs. A parameter read in a predicate
does not count: raw `ProgramSpec` predicates could always look up parameters.

`provenance.kind` says what the declaration makes the relation: `supplied`
for a data relation, `derived` for a `derived_relation`. `derived` carries
`rule` (the id of the rule that derives it, today the relation's own id),
`source_relation`, and `reads`: the input slots, rule ids and relations its
predicate mentions directly. An atom adds its named relation to
`reads.relations`, as a bare membership test does; a term comparison adds
nothing. Nothing here is specific to unit derivation.

The judgment wire forms are
`{kind: relation_atom, relation: R, terms: [anchor, candidate]}` (with terms in
slot order) and `{kind: same_entity, left: anchor, right: candidate, negated:
false}`; `negated: true` encodes `!=`. They add variants to format 3 without
a version bump: older engines refuse the unknown expression kinds. Loading
re-derives the complete interface and refuses a mismatch. The supplied-tuple
restriction also covers programs using only qualified reads (#268), so
their derived relations now report `accepts_supplied_tuples: false`. Earlier
format-3 artifacts from programs using only qualified reads must be
recompiled: the loader now expects that derived-relation interface field to be
false. Programs using none of these constructs, including programs whose
predicates only read parameters, derive the same interface as before.

## In Rust

`ProgramSpec` carries `input_declarations` last and omits it when empty.
A raw `ExecutionRequest.program` may carry it, and the
`execution-request.v1` schema includes it. This is deliberate and additive:
an election is program content, compiled from the module source, and the
loader derives the interface from the program alone. Execution never reads
the field. A program without elections serializes as before.
`CompiledProgramArtifact` carries `interface: Option<ProgramInterface>`,
`engine_build` and `source_closure` beside `program`. Its serialization goes
through a wire struct that repeats `ProgramSpec`'s fields in order and
appends `interface`,
so the block sits at `program.interface` on the wire. The wire struct does
not use `#[serde(flatten)]`: flattening buffers values, and the buffered form
cannot read the integer keys of a parameter table. A test pins that the wire
struct without `interface` serializes byte for byte as `ProgramSpec` does.
The artifact's `JsonSchema` delegates to the wire struct, so the generated
schema describes the block at `program.interface`; a test checks that path
in the emitted schema.

## Loading

`CompiledProgramArtifact::from_json_*` reads format 3 only:

- no `artifact_format_version`: rejected;
- a lower version: "compiled artefact `<path>` is older than this engine: it
  is artifact format 2 (written by engine 0.2.2) and this engine (0.2.2,
  build 5d63e8f) reads format 3", followed by the two ways forward: recompile,
  or run it unchanged through the format-2 compatibility loader;
- a higher version: "this engine (...) is older than compiled artefact
  `<path>` (format 4, written by engine ...); upgrade the engine";
- format 3: top-level keys are a closed set; `engine_version`, `engine_build`,
  `program.interface` and `metadata.input_catalog` are required;
  `program.extends` is refused outright; and `interface` must equal what this
  engine derives from the embedded program. Metadata must equal this
  engine's current analysis; relation typing is checked as at compile.

The format-2 dialect tolerances are confined to
`from_legacy_format_2_json_str` / `_file`. That loader accepts format 2 only
and refuses a format-2 payload that carries `program.interface`,
`program.input_declarations`, `engine_build` or `source_closure`.
What it returns is still format 2:
`artifact_format_version` is 2 and `interface` is `None`. The engine never
derives an interface for it, so nothing downstream can mistake it for a
format-3 artifact. Three separate statements hold for its bytes:

- an artifact this code line compiled, in either format, round-trips byte for
  byte through its loader;
- for any format-2 payload main accepts, the legacy loader's result
  serializes exactly as main's loader result does, so every stage-3 digest of
  a loaded source artifact is what it was;
- original wire bytes are not preserved where the dialect is normalized
  (duplicate parameters, an absent catalog, a null `extends`).

The compatibility loader retains the format-2 tolerances: an absent
`engine_version`, compatible duplicate parameters, an absent input catalog
filled after normalization, null `extends`, and metadata from a retired
fast-path rule set. It preserves accepted retired metadata and emits a
`stale_fast_path_metadata` diagnostic. Relation typing remains mandatory,
with the existing migration pointer for an unsafe format-2 program.

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
For format 3 the derivation above is fixed for every program a format-3
engine can already load. Changing one of those programs' interfaces requires
a new format version, whose engine reads format 3 through a compatibility
loader that keeps this derivation. A later construct that older engines
refuse to deserialize may extend the derivation and `RelationProvenance`
for programs using that construct without a format bump: no published
artifact changes. Examples include a new expression variant or a derived
relation without `source_relation`, as #214 plans. The derivation therefore
claims structural facts and usage rules that transcribe what evaluators
enforce. Golden fixtures pin it.

## Invariants and tests

1. **Executable content is unchanged.** For source without elections, the
   format-3 artifact's `program` without `interface`, and its `metadata`, equal the
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
   kind an input or relation names is in the roster. Elections agree with
   `program.input_declarations`. Property test, and every
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
   with a dtype, give every other input a well-typed value and evaluate each
   rule twice: without the slot, and with a value outside the dtype. The
   second evaluation either fails with a type error or returns exactly what
   the first returned, and where the first failed for want of the slot the
   second fails with a type error. So an out-of-type value is never read
   successfully, whether the site has a default or not, and an unreached
   site changes nothing. The same property holds for a whole number outside
   `values`, where the failure is a missing parameter value or a type
   error.
7. **`entities` is sound.** For a generated multi-entity program with
   well-kinded queries and tuples, every missing-input error the explain
   evaluator raises names an id whose kind, by the generator's own
   assignment of ids to kinds, the slot lists; an entity-free query's id has
   kind `Scalar`. The probe is the evaluator's error, not the derivation.
   The generator reads slots through plain `input` nodes so every read can
   surface as that error, and supplies the reported slot and re-runs until
   the evaluation succeeds.
8. **Execution parity.** In each of explain, fast and dense, the format-2
   artifact through the legacy loader and the format-3 artifact of the same
   source give identical results.
9. **Stage-3 survives a rebuild.** A stage-3 artifact whose embedded
   artifacts carry another `engine_build` and `engine_version` loads; one
   whose phase-two `program` or `metadata` differs is refused; the artifact
   the previous engine wrote loads through the legacy path with its source
   digest unchanged.
10. **Elections are declared, never defaulted.** Both forms of the `inputs:`
    block reach the interface and compile to the same artifact; any block
    in which no entry carries `election` compiles to the artifact the module
    compiles to without it, when no declaration-sensitive primitive is used
    (property test); each refusal names its module
    and entry; an imported module's elections are checked against its own
    rules; disagreeing declarations of one slot, an election read with
    `input_or_else`, and tampering with an `election` or with
    `program.input_declarations` are refused; explain and fast give
    byte-identical responses with and without the declarations; and the
    module schema refuses exactly the election shapes the loader refuses.

Corpus acceptance compares the complete sets of modules that compile and
each module's executable content with a baseline. Its tally includes module
and artifact counts, failures by cause, slots listing several entities or
`Scalar`, dtype counts, defaults, closed domains, entity instance sources,
relation provenance, and elections by actor.

## Deferred

- Declared input dtype, default and domain. They need RuleSpec fields and the
  encoder to emit them; `dtype_basis: "declared"` is reserved.
- Cardinality of relations. Nothing declares it today.
- A compile diagnostic for a rule referenced directly from a rule of another
  entity. The interface reports the consequence; whether those references are
  encoding defects is a corpus question.
- The corpus-provisions index in the source closure.
