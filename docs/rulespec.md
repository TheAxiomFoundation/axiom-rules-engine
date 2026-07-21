# RuleSpec

RuleSpec is the canonical authoring and interchange schema for Axiom Rules
Engine rules.
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
  `data_relation.arity`. Dataset relation records use durable ids such as
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
```

Derived relations are rule-defined views over data relations or other derived
relations. The source relation supplies candidate tuples; the formula decides
which candidate tuples remain in the filtered relation.

```yaml
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2

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

Derived relations execute in explain mode, bulk fast mode, and the generic dense
compiler for predicates that can be evaluated from related inputs, related
judgment rules, and current/root entity judgment or scalar rules. A
`source_relation` may also point at another `derived_relation`; the runtime
applies the parent filter before the child filter. Optimized execution paths may
still reject membership predicates that aggregate another relation from inside a
current/root predicate.

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

Callers must supply at least one absolute, real, unaliased country checkout.
The CLI uses required repeatable `--rulespec-root` arguments; Rust callers pass
a validated `CanonicalRuleSpecRoots` to `load_rulespec_file`,
`CompiledProgramArtifact::from_rulespec_file`, or `FsModuleSource`. There is no
environment, cwd, ancestor, sibling-checkout, suffixed-worktree, or standalone
jurisdiction-repository fallback.

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
not an atomic module. Compile that output with the separate
`compile-composed` command. Its input must be an absolute, real, unaliased
`.yaml` outside every RuleSpec checkout with exact
`module.kind: composition`; it still requires one or more explicit
`--rulespec-root` arguments. Only fragmentless canonical atomic imports are
allowed, and synthesized root rules remain originless. The removed top-level
`extends` directive is rejected on both surfaces. The ordinary `compile` command
rejects the ephemeral file, while `compile-composed` rejects atomic modules and
declarative ProgramSpecs.

Compiled artifacts make the input boundary explicit in
`metadata.input_catalog`. Each runtime slot has a deterministic
`canonical_request_name` plus the complete sorted `request_names` set. A bare
name is published only when an originless synthesized rule actually owns that
slot, and it is then canonical. Otherwise the lexicographically first exact
atomic `<module>#input.<slot>` owner is canonical. Multiple owner names are
equivalent inputs to one slot; arbitrary prefixes and `<target>#<slot>` aliases
are not accepted. This is a deterministic runtime-input catalog, not a source
manifest. Artifact loading also recomputes compiler-derived metadata and checks
it for consistency with the embedded program; source-tamper evidence belongs to
the signed-corpus-release and supervisor chain.

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
relation names. Dataset inputs use `#input.<local symbol>` when supplying an
input slot from the current rule, or the upstream rule id when supplying an
imported derived/parameter value:

```yaml
input:
  us:statutes/7/2017/a#input.household_size: 1
  us:statutes/7/2014/e/6/A#snap_net_income: 100
  us:statutes/7/2012/j#relation.member_of_household:
    - us:statutes/7/2012/j#input.snap_member_is_elderly_or_disabled: false
output:
  us:statutes/7/2017/a#snap_regular_month_allotment: 268
```

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

## Why This Instead Of Direct `ProgramSpec` YAML

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
