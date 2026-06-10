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

`schema: axiom.rules.*` is also accepted as a discriminator. YAML with a
top-level `rules:` key and no discriminator is rejected, because RuleSpec module files
must identify their schema explicitly.

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
- `source_relation`: non-executable legal/provenance edges. It must declare
  `source_relation.type` and `source_relation.target` and is ignored during
  lowering into `ProgramSpec`.

`kind` describes the record's schema and lowering behavior. Source/legal
semantics live in `source_relation.type`, not in `kind`. The supported
source-relation types are `defines`, `delegates`, `implements`, `sets`,
`amends`, `restates`, and `cites`. Executable `parameter` and `derived` rules
implicitly define their own durable outputs, so explicit `defines` records are
only needed for span-level provenance that is not otherwise represented by an
executable rule.

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
before the current file is lowered. Relative imports resolve from the current
file. Canonical imports use jurisdiction repo paths:

```yaml
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
  - us-co:regulations/10-ccr-2506-1/4.207.3
```

The canonical form is `<jurisdiction>:<relative path without extension>`.
A jurisdiction prefix resolves against either layout, with the same durable
ids in both:

- **Country monorepo** — a repo named `rulespec-<country>` holding one
  directory per jurisdiction: `us:` → `rulespec-us/us/…`, `us-co:` →
  `rulespec-us/us-co/…`.
- **Legacy standalone repo** — `us-co:` → a sibling checkout named
  `rulespec-us-co` with content at its root. Legacy candidates are tried
  first, so existing sibling checkouts keep their precedence.

The loader searches sibling checkouts and any roots listed in
`AXIOM_RULESPEC_REPO_ROOTS` (each entry may be a jurisdiction repo, a
country monorepo, or a directory containing either).

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

Known hard gaps:

- Formula strings are parsed by the internal `crate::formula` parser and
  normalised into `ProgramSpec`.
- Current formula-string gaps include latest-only derived temporal formulas,
  inferred relation slot orientation, and incomplete optimized support for
  complex cross-scope derived-relation predicates. These should be closed in
  RuleSpec and `ProgramSpec`, not by adding another source format.

## Why This Instead Of Direct `ProgramSpec` YAML

Direct `ProgramSpec` YAML is useful as an engine IR/debug format, but it is not
the right authoring target. RuleSpec keeps metadata and provenance structured
while leaving formulas concise enough for generation and review. The Axiom app
should provide the human-readable visualisation layer; raw source readability is
secondary to schema validity, provenance fidelity, and avoiding silent lossy
translation.

Canonical jurisdiction repos use the filepath as the rule ID. Source artifacts
are tracked in parallel `sources/` registry files, with expected hashes stored in
Git and R2 object paths derived from repo + path. See
[`jurisdiction-repos.md`](jurisdiction-repos.md).
