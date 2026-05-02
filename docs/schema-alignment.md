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

- Explicit rule kind: `parameter`, `derived`, `relation`, and eventually
  `derived_relation`.
- Explicit relation arity and, in a follow-up, slot names/orientation.
- Multi-source provenance and source-document anchors.
- Graph-level metadata such as `sets` and `amends` in sidecar documents rather
  than overloaded expressions.

## Current Gaps

The Rust loader now compiles RuleSpec directly as the external format. Remaining
schema/runtime gaps are explicit:

- `derived_relation` is rejected until relation outputs are modelled in
  `ProgramSpec`.
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
- Rejection of `derived_relation` until relation outputs are modelled.
- Rejection of ambiguous YAML with `rules:` but no RuleSpec discriminator.
