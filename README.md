# Axiom Rules Engine

Rust runtime and Python bindings for executing Axiom RuleSpec.

`axiom-rules-engine` is engine infrastructure only. Production policy content belongs
in exact country checkouts such as `rulespec-us`, with direct jurisdiction directories
such as `us/` and `us-co/`; the file path supplies the legal ID. This repo keeps
RuleSpec YAML only as test fixtures under `tests/fixtures/rulespec/`.

## What Is Implemented

- typed scalar and judgment outputs
- temporal periods and intervals
- relation facts over time
- RuleSpec import resolution across jurisdiction repos
- durable repo-backed output IDs, e.g.
  `us:statutes/7/2017/a#snap_regular_month_allotment`
- `explain` execution with traces
- `fast` execution through the generic dense path when supported
- opt-in currency output rounding per rule (`rounding: half_up|half_even|floor|ceil`),
  applied identically across the explain, fast, and dense paths — see
  [`docs/rulespec.md`](docs/rulespec.md#currency-rounding)
- compiled artifacts for repeated execution
- Python request/response models and subprocess wrapper

Direct `ProgramSpec` YAML is an internal engine IR. Rule authors should use
RuleSpec in jurisdiction repos.

## RuleSpec

RuleSpec files must declare exact `format: rulespec/v1`. The removed `schema:`
discriminator and YAML with a top-level `rules:` key but no exact format are
rejected.

Rule names are public concept fragments. Use
[`docs/concept-naming.md`](docs/concept-naming.md) when adding or reviewing
RuleSpec names.

Canonical imports use jurisdiction repo paths:

```yaml
format: rulespec/v1
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
  - us-co:regulations/10-ccr-2506-1/4.207.3
rules: []
```

For repo-backed RuleSpec files, public execution requests must use durable
legal IDs for queried outputs and dataset input/relation names. Bare local names
remain local formula symbols only.

## Commands

Compile a RuleSpec file:

```bash
cargo run -- compile \
  --program /path/to/rulespec-us/us/policies/usda/snap/fy-2026-cola/maximum-allotments.yaml \
  --rulespec-root /path/to/rulespec-us \
  --output /tmp/snap.compiled.json
```

`--rulespec-root` is required and repeatable. Each value must be an absolute,
real, unaliased checkout named exactly `rulespec-<two-letter-country>`. The
engine never discovers roots from environment variables, cwd, ancestors, or
sibling checkouts.
Configured checkouts must remain trusted and quiescent for the compile. The
path validator provides deterministic authority selection, not a sandbox
against concurrent filesystem replacement or hard-link mutation.

Compile an ephemeral RuleSpec composition emitted by `axiom-compose`:

```bash
cargo run -- compile-composed \
  --program /tmp/snap-composition.yaml \
  --rulespec-root /path/to/rulespec-us \
  --output /tmp/snap.compiled.json
```

`compile-composed` requires exact `format: rulespec/v1` and
`module.kind: composition`, keeps the composition's synthesized root rules
originless, and permits only canonical atomic imports resolved through the
explicit roots. Atomic files, declarative ProgramSpecs, relative dependencies,
and composition files inside a RuleSpec checkout are rejected.

Every compiled artifact publishes `metadata.input_catalog`. Each entry records
the internal runtime `slot`, one deterministic `canonical_request_name`, and all
accepted `request_names`. An originless synthesized owner makes the bare slot
canonical; otherwise the lexicographically first exact
`<module>#input.<slot>` owner is canonical. All listed names feed the same slot.
Invented module prefixes and `<target>#<slot>` aliases are rejected. Artifact
loading recomputes the catalog, evaluation order, and fast-path metadata as a
derived-metadata consistency check against the embedded program. This does not
provide source-tamper evidence; that belongs to the signed-corpus-release and
supervisor chain.

Run a compiled artifact:

```bash
cargo run -- run-compiled --artifact /tmp/snap.compiled.json < request.json
```

`request.json` must key atomic inputs by one of the exact owners published in
`metadata.input_catalog`. Synthesized composition inputs use the catalog's bare
name. Queried atomic outputs and relations still use their legal RuleSpec IDs:

```json
{
  "mode": "explain",
  "dataset": {
    "inputs": [
      {
        "name": "us:statutes/7/2017/a#input.household_size",
        "entity": "Household",
        "entity_id": "household:1",
        "interval": { "start": "2026-01-01", "end": "2026-02-01" },
        "value": { "kind": "integer", "value": 1 }
      },
      {
        "name": "us:statutes/7/2014/e/6/A#input.snap_net_income",
        "entity": "Household",
        "entity_id": "household:1",
        "interval": { "start": "2026-01-01", "end": "2026-02-01" },
        "value": { "kind": "decimal", "value": "100" }
      }
    ],
    "relations": [
      {
        "name": "us:statutes/7/2012/j#relation.member_of_household",
        "tuple": ["household:1", "person:1"],
        "interval": { "start": "2026-01-01", "end": "2026-02-01" }
      }
    ]
  },
  "queries": [
    {
      "entity_id": "household:1",
      "period": {
        "period_kind": "month",
        "start": "2026-01-01",
        "end": "2026-02-01"
      },
      "outputs": ["us:statutes/7/2017/a#snap_regular_month_allotment"]
    }
  ]
}
```

Search and validate public concept IDs discovered from jurisdiction RuleSpec
repos:

```bash
axiom concepts search "adjusted gross income" --root /path/to/rulespec-us --json
axiom concepts show us:statutes/26/1401#self_employment_tax --root /path/to/rulespec-us --json
axiom concepts validate us:statutes/26/62#adjusted_gross_income --root /path/to/rulespec-us --json
axiom concepts list --namespace us:statutes/26 --root /path/to/rulespec-us --json
```

The concept index is static and repo-backed. It includes module IDs, rule output
IDs, data-relation IDs, source-relation IDs, and inferred `#input.*` leaves from
RuleSpec formulas. This lets the Axiom app, validators, and encoding tools
validate source-to-legal-concept alignment without importing the runtime.

## JSON Schemas

The `schemas/` directory holds the authoritative JSON Schemas (draft-07) for the
formats the engine exchanges:

- `rulespec-module.v1.schema.json` — the RuleSpec module/authoring format
  (`format: rulespec/v1`).
- `rulespec-test.v1.schema.json` — the companion `*.test.yaml` case format.
- `compiled-artifact.v1.schema.json` — `CompiledProgramArtifact` (the compiled
  program, embedding the `ProgramSpec` IR).

These are the single source of truth for consumers that would otherwise
re-implement the shape by hand. They mirror the engine's serde **deserialization**
acceptance: a document that deserializes validates, and vice versa. A document
can still validate and fail lowering for semantic reasons (unknown rule `kind`,
top-level `relations:`, missing `effective_from`).

Schema generation lives behind the non-default `schema` feature, so pure-runtime
consumers do not compile `schemars`. Regenerate the checked-in files with:

```bash
cargo run --features schema -- emit-schemas --out schemas
```

A golden-file test (`cargo test --features schema`) fails if the checked-in
schemas drift from the types. `schema_conformance` validates every module and
companion test under the explicit `AXIOM_RULESPEC_US_ROOT` checkout and
self-skips when none is configured.

## Python Package

The Python wrapper lives under `python/axiom_rules_engine/`. It exposes
`Program`, `Dataset`, `AxiomRulesEngine`, atomic and composed dense execution
bindings, and concept discovery helpers. It shells out to the compiled
`axiom-rules-engine` binary for reference and compiled-artifact flows.

## Tests

```bash
cargo test
cargo test --features schema
python -m pytest -q python/tests
```

The Rust tests cover parsing, lowering, execution, dense compilation, traces,
and RuleSpec import/ID behavior using fixtures under `tests/fixtures/rulespec/`.
The `--features schema` run adds the JSON Schema golden-file, fidelity, and
`rulespec-us` conformance tests.
