# Compiled node annotations

Issue #115 requires certification metadata to be computed by the compiler, not
reconstructed by consumers. The current compiler input is missing three facts
needed to do that honestly:

- Axiom Compose does not carry its typed `outputs` into emitted RuleSpec.
- Unknown formula names all lower to identical runtime input slots, so the
  engine cannot distinguish observations, cross-program values, and missing
  computations.
- Transformation pattern names are erased before the engine sees a composed
  RuleSpec.

The engine must not parse prose summaries or infer these facts from names. This
change therefore defines an additive, fail-closed contract.

## Authoring contract

A RuleSpec composition may carry:

```yaml
outputs:
  - benefit
input_states:
  observed_income: exogenous
  upstream_program_amount: policy_derived
  not_yet_encoded_intermediate: pending
relation_states:
  household_membership: exogenous
  upstream_case_links: policy_derived
  not_yet_encoded_links: pending
```

`outputs` is the exact set of derived roots for structural reachability.
`input_states` must classify every runtime input slot exactly once whenever
`outputs` is present. `relation_states` does the same for runtime-supplied data
relations; the compiler never assumes that relation data is exogenous. Unknown
outputs, duplicate outputs, duplicate executable nodes, missing
classifications, and classifications for unknown slots or relations are
compile errors.

Legacy RuleSpec without this contract still compiles and legacy v2 artifacts
still load, but they do not gain a node catalog. Absence means “not declared,”
not “all reachable” and not “all inputs are exogenous.” Certification consumers
must require the catalog rather than interpreting absence as success.

## Compiled contract

When the authoring contract is complete, `metadata.nodes` contains one stable,
deterministically ordered entry for every executable node:

- implicit scalar input slot;
- parameter table;
- derived rule;
- data relation;
- derived relation.

Each entry carries:

- `id` and local `name`;
- `kind`;
- `state: input | derived | pending`;
- `input_kind: exogenous | policy_derived` only for `input` state;
- `reachable`, computed by a backwards structural traversal from `outputs`;
- `provenance: provision_backed | synthesized | unverified`.
- `corpus_citation_path` when and only when provenance is
  `provision_backed`.

Parameters are `derived` state because the runtime computes their value from a
compiled, effective-dated table rather than accepting them as runtime facts.
Data relations take their state and input kind from `relation_states`; derived
relations are `derived`.
`unverified` is necessary for legacy/uncited atomic declarations and implicit
input slots: neither “provision-backed” nor “synthesized” is truthful for them.

Reachability unions dependencies from every effective-dated version and visits
derived references, inputs, parameter lookups and their indices, relation
membership/aggregates, derived-relation sources and predicates, condition
branches, and period reductions.

## Collision-safe provenance

Backing follows the exact declaration and its loading surface:

- a composition-root declaration is `synthesized`, even when the composition
  module itself cites a provision;
- an atomic declaration with a module-level corpus citation is
  `provision_backed`;
- an uncited atomic declaration is `unverified`.

Canonical IDs, citation fields after lowering, source prose, transformation
names, and `_core` naming are never used to infer backing. Metadata propagation
is restricted to the declaration's executable kind so an imported parameter
cannot stamp its ID, citation, or backing onto a same-named synthesized derived
rule. A computed entity-free parameter is the supported exception to the
declared-kind mapping: because formula lowering makes it an actual derived
Scalar node, identity and provenance follow that actual node only when no exact
derived declaration owns the same name.

Every `provision_backed` entry is grounded by a canonical
`corpus_citation_path`. For parameter and derived nodes that path must equal the
path already carried by the executable node; relations retain the path in the
provenance sidecar because `RelationSpec` has no citation field. A raw
`ProgramSpec` therefore cannot assert provision backing with a bare enum.
