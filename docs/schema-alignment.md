# Schema alignment

The current direction is:

1. RuleSpec YAML/JSON is the canonical authoring, interchange, and
   jurisdiction-repo source format.
2. `ProgramSpec` is the Rust engine IR and compiled-artifact input.
3. Formula strings are fields inside RuleSpec, parsed by an internal engine
   module and normalised into `ProgramSpec`.

Format-3 compiled artifacts carry a deterministic runtime-input catalog and
`program.interface`, an entity roster with input and relation descriptors.
Loading checks both against the embedded program. `engine_build` identifies
the producer, while `source_closure` hashes the RuleSpec module texts read.
These are consistency and provenance records; source authenticity lives in
the signed-corpus-release and supervisor chain.

No external RuleSpec module adapter layer is part of the design. The repository
is still pre-adoption, so Git history is the migration path for old experiments.
The active code and docs should describe the architecture we would choose from a
clean start.

## Alignment points

RuleSpec should retain the core engine semantics:

- Temporal versions on each rule.
- Typed scalar and judgment outputs.
- Relation facts separate from scalar outputs.
- Effective-dated parameters.
- Provenance fields that can carry source citations and source URLs.
- Formula strings for compact expressions such as `if`, `match`, arithmetic,
  date operations, and relation aggregations.

RuleSpec should make machine-authored structure explicit:

- Explicit rule kind: `parameter`, `derived`, `data_relation`,
  `derived_relation`, and `source_relation`.
- Explicit data-relation arity and entity kinds for each tuple slot, in order.
- One canonical corpus-provision join key per source/proof node; multi-source
  composition happens upstream.
- Legal/provenance graph edges such as `restates`, `sets`, `implements`, and
  `amends` as `source_relation` records. Same-kind `sets` edges with
  `source_relation.value` lower into runtime as delegated parameter bindings
  or derived formula hooks; other source relations remain provenance metadata.

## Artifact schemas and loader checks

`compiled-artifact.v3.schema.json` is generated from the artifact wire types,
including `program.interface`. Format 3 closes the top-level key set and
requires `engine_version`, `engine_build`, `program.interface` and
`metadata.input_catalog`. `source_closure` is an object when present and is
omitted for raw IR compilation. Each interface and identity object rejects
unknown fields and requires every field in its shape, including nullable
ones. The artifact wire program repeats `ProgramSpec`'s fields in order and
appends `interface`; `ProgramSpec` itself carries `input_declarations` last,
omitted when empty. `execution-request.v1.schema.json` also admits that
additive election field.

`compiled-artifact.v2.schema.json` and `compiled-artifact.v1.schema.json` are
frozen checked-in archives. The named format-2 compatibility loader retains
its historical normalizations and relation-typing check; the default loader
accepts format 3 only. No loader executes format 1. A schema for stored data
does not opt a runtime into its historical format.

The current artifact schema adds the default loader's shape restrictions to
the shared serde wire types, whose optional fields also serve the legacy
loader. It also reflects the decimal deserializer's integer acceptance and
requires non-empty scalar `min`/`max` operands, which program conversion
enforces. Semantic checks remain in the loader:

- Interface and metadata must equal the derivations of the embedded program.
- Source-closure modules must be sorted, unique, include the root and have
  the advertised aggregate digest. The schema's target pattern is an
  approximation; the loader also rejects dot segments and final segments
  ending in `.yaml`, `.yml` or `.test`.
- Election declarations must be sorted and unique, refer to accepted input
  request names, agree for shared slots and have no defaulted read sites.
  The module schema checks local election shape; lowering checks that each
  declaring module actually reads the input.
- Dependency, effective-date, provenance, rounding and relation-typing
  validation also apply to the program.

Schema validity alone does not establish these properties. See
[the artifact format reference](artifact-format.md) for the derivation and
the format-2 opt-in on each tool.

## Current gaps

The Rust loader now compiles RuleSpec directly as the external format. Remaining
schema/runtime gaps are explicit:

- `derived_relation` lowers into `ProgramSpec` and executes in explain mode,
  bulk fast mode, and the generic dense compiler for related-input predicates,
  current-entity predicates, membership tests of the relation's own sources
  read with their derivation's slots, and composed derived-relation source
  chains. The dense compiler rejects a predicate with any other membership
  test.
- `source_relation` records are validated as provenance metadata.
  Parameter-to-parameter and derived-to-derived `sets` records now lower into
  runtime bindings when both the upstream target and downstream value concepts
  are present in the merged program; remaining relation types are still metadata
  for validators, amendments, traces, and upstream-first checks.
- Downstream jurisdiction repos still need their own migrations to replace
  SNAP approximations with filtered-entity RuleSpec.
- Formula strings currently support the implemented scalar/judgment expression
  subset, not arbitrary legal operators.
- Aggregate orientation comes from declared slot kinds. A relation whose
  slots all hold the evaluating kind requires a derived relation with
  explicit `current_slot` and `related_slot`; ambiguous direction is refused.
- Executable outputs and trace nodes carry the same singular canonical corpus
  join key; plural provenance fields are intentionally rejected.

## Tests

The Rust tests cover:

- RuleSpec compilation for a SNAP-like formula set with parameters, `match`,
  nested `if`, relation aggregation, and provenance.
- RuleSpec compilation for a housing-style judgment with date arithmetic,
  relation counts, derived judgment references, and `not`.
- Acceptance of non-executable `source_relation` records and rejection of legacy
  `reiteration` and top-level RuleSpec `relations:`.
- Lowering and explain-mode execution of `derived_relation`.
- Rejection of ambiguous YAML with `rules:` but no RuleSpec discriminator.
- Schema golden files, fidelity checks and opt-in corpus conformance.
- Format-3 round trips, interface agreement and tamper refusals; preserved
  format-2 normalized output; and execution parity within each mode.
