# Schema Alignment

The current direction is:

1. RuleSpec YAML/JSON is the canonical authoring, interchange, and
   jurisdiction-repo source format.
2. `ProgramSpec` is the Rust engine IR and compiled-artifact input.
3. Formula strings are fields inside RuleSpec, parsed by an internal engine
   module and normalised into `ProgramSpec`.

No external RuleSpec module adapter layer is part of the design. The repository
is still pre-adoption, so Git history is the migration path for old experiments.
The active code and docs should describe the architecture we would choose from a
clean start.

## Alignment Points

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
- Explicit data-relation arity and, in a follow-up, slot names/orientation.
- Multi-source provenance and source-document anchors.
- Legal/provenance graph edges such as `restates`, `sets`, `implements`, and
  `amends` as `source_relation` records that do not lower directly into the
  executable runtime.

## Current Gaps

The Rust loader now compiles RuleSpec directly as the external format. Remaining
schema/runtime gaps are explicit:

- `derived_relation` lowers into `ProgramSpec` and executes in explain mode,
  bulk fast mode, and the generic dense compiler for related-input predicates,
  current/root predicates, and composed derived-relation source chains.
- `source_relation` records are validated as provenance metadata and ignored
  during runtime lowering; the harness/compiler should consume them when
  resolving imports, amendments, and upstream-first checks.
- Downstream jurisdiction repos still need their own migrations to replace
  SNAP approximations with filtered-entity RuleSpec.
- Formula strings currently support the implemented scalar/judgment expression
  subset, not arbitrary legal operators.
- Relation slot orientation is still inferred in some expression forms and
  should become explicit before larger-scale jurisdiction ingestion.
- Multi-source provenance needs first-class arrays on executable outputs and
  trace nodes.

## Tests In This Pass

The Rust tests cover:

- RuleSpec compilation for a SNAP-like formula set with parameters, `match`,
  nested `if`, relation aggregation, and provenance.
- RuleSpec compilation for a housing-style judgment with date arithmetic,
  relation counts, derived judgment references, and `not`.
- Acceptance of non-executable `source_relation` records and rejection of legacy
  `reiteration` and top-level RuleSpec `relations:`.
- Lowering and explain-mode execution of `derived_relation`.
- Rejection of ambiguous YAML with `rules:` but no RuleSpec discriminator.
