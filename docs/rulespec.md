# RuleSpec

RuleSpec is the canonical authoring and interchange schema for Axiom Rules
Engine rules.
Authoring tools should emit RuleSpec YAML/JSON from Axiom source documents; the
Rust engine normalises it into `ProgramSpec` before compilation. `ProgramSpec` is
the runtime IR, not a programme file format.

## Shape

Every RuleSpec YAML file must declare an explicit discriminator:

```yaml
format: rulespec/v1
module:
  title: Texas SNAP overlay
relations:
  - name: member_of_household
    arity: 2
rules:
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
top-level `rules:` key and no discriminator is rejected, because programme files
must identify their schema explicitly.

## Semantics

Supported rule kinds in the current Rust loader:

- `parameter`: no entity-scoped output; literal versions lower to indexed scalar
  parameters through the existing bridge.
- `parameter` rules with `indexed_by` and versioned `values` encode source
  tables/scales as addressable parameter cells. `indexed_by` is required for
  every `values` table. Formulas reference them with `table_name[index_expr]`.
- `derived`: entity-scoped scalar or judgment outputs.
- `relation`: explicit relation declarations with `arity`.
- `reiteration`: a non-executable coverage marker for a provision that restates
  another authority. It must declare `reiterates.target` and is ignored during
  lowering into `ProgramSpec`.

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

Example reiterative provision:

```yaml
rules:
  - name: co_snap_maximum_allotment_reiterates_usda_fy_2026
    kind: reiteration
    source: 10 CCR 2506-1 section 4.207.3(D)
    source_url: https://www.sos.state.co.us/...
    reiterates:
      target: us:policies/usda/snap/fy-2026-cola#snap_maximum_allotment
      authority: federal
      relationship: restates
    verification:
      values:
        snap_maximum_allotment_table:
          1: 298
          2: 546
```

Use `reiteration` when the local text should be represented for coverage and
auditability, but computation and reformable values belong to the target rule.
If the local provision changes the target rule's legal effect, encode it as a
real rule or amendment instead.

Known hard gaps:

- `derived_relation` is represented in the schema direction but intentionally
  rejected until relation outputs are modelled in `ProgramSpec`.
- Formula strings are parsed by the internal `crate::formula` parser and
  normalised into `ProgramSpec`.
- Current formula-string gaps include latest-only derived temporal formulas,
  inferred relation slot orientation, and no relation-output rules. These should
  be closed in RuleSpec and `ProgramSpec`, not by adding another source format.

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
