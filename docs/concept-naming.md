# Concept Naming

RuleSpec concept names are the public fragments in durable legal IDs such as
`us:statutes/7/2017/a#snap_regular_month_allotment`. They are not just display
labels, so they should be stable, precise, and boring.

## Principles

Use lowercase `snake_case`. Names should be readable as domain terms, not as
implementation notes. Prefer `snap_net_income_limit_100_percent_fpl_48_states_dc`
over `net_limit`, `fy_2026_net_income`, or `co_snap_table_d`.

Put legal identity in the path and concept identity in the fragment. The path
already says jurisdiction, source family, title, section, and often annual
source document. Do not repeat those details in executable rule names unless
they distinguish the legal concept itself.

Name the upstream legal concept, not the downstream document that repeats it.
If Colorado restates a USDA standard, the executable rule should stay in the
USDA file and keep the USDA concept name. The Colorado file should add a
`source_relation`, not a copied executable formula.

Include legal dimensions that distinguish concepts. Threshold basis,
geography, unit, population, program, and percentage-of-FPL are concept
dimensions when the source has multiple standards. For example,
`snap_gross_income_limit_130_percent_fpl_48_states_dc` and
`snap_gross_income_limit_165_percent_fpl_48_states_dc` are distinct concepts.

Do not put numeric parameter values in names. Values belong in `versions`,
`values`, source text, and tests. Use `snap_elderly_or_disabled_household` or
`snap_standard_deduction` rather than names that embed dollar amounts.

Do not use friendly aliases as public references. Public refs must be full
RuleSpec IDs with a path and fragment. Formula-local symbols may be short inside
a compiled module, but app/API surfaces should expose durable IDs.

## Executable Rules

Executable `parameter` and `derived` names should follow this pattern:

```text
<program>_<concept>[_<basis>][_<population_or_geography>]
```

Examples:

```yaml
name: snap_regular_month_allotment
name: snap_gross_income_limit_130_percent_fpl_48_states_dc
name: snap_household_has_elderly_or_disabled_member
name: co_snap_expanded_categorical_gross_income_limit
```

Use a jurisdiction prefix only when the executable concept is genuinely
jurisdiction-specific. Do not add `co_` to a rule that is merely copied from
USDA or federal law.

Avoid source names and dates in executable concept fragments when the file path
or effective date already carries that information. For example, prefer:

```yaml
us:policies/usda/snap/fy-2026-cola/deductions#snap_standard_deduction
```

over:

```yaml
us:policies/usda/snap/fy-2026-cola/deductions#snap_standard_deduction_fy_2026_usda
```

## Data Relations

`data_relation` names should describe runtime predicates, not legal source
edges:

```yaml
name: member_of_household
kind: data_relation
data_relation:
  arity: 2
```

Public dataset refs use the `#relation.` prefix:

```text
us:statutes/7/2012/j#relation.member_of_household
```

Use relation-like names such as `member_of_household`,
`member_of_tax_unit`, or `child_of_benefit_unit`. Avoid source/provenance verbs
such as `restates`, `implements`, or `amends` for data relations.

## Source Relations

`source_relation` names are record IDs for legal/provenance edges. They should
be just explicit enough to distinguish multiple edges in one file. Do not make
them carry information already supplied by the repo path, provision path,
relation type, target, or effective source document. Display surfaces should
primarily show `source_relation.type`, source span, and target, not the raw
record name.

Recommended pattern:

```text
<relation_type>_<local_topic>
```

Examples:

```yaml
name: restates_net_income_limit_100_percent_fpl
kind: source_relation
source_relation:
  type: restates
  target: us:policies/usda/snap/fy-2026-cola/income-eligibility-standards#snap_net_income_limit_100_percent_fpl_48_states_dc

name: sets_heating_cooling_sua
kind: source_relation
source_relation:
  type: sets
  target: us:regulations/7-cfr/273/9#state_utility_allowance_amount
```

For `restates`, the local topic should be at least as specific as the upstream
target. If the target is `snap_net_income_limit_100_percent_fpl_48_states_dc`,
do not name the edge only `restates_net_income_limit`. But also avoid repeating
jurisdiction, target authority, or annual source metadata:

```yaml
# Good.
name: restates_net_income_limit_100_percent_fpl

# Too much duplication; this is already in path and target.
name: co_snap_net_income_limit_100_percent_fpl_restates_usda_fy_2026
```

## Tables And Selectors

Source-stated scale tables should use separate addressable names for the table,
additional-member increment, and computed selector when the source has that
structure:

```yaml
name: snap_net_income_limit_100_percent_fpl_48_states_dc_table
name: snap_net_income_limit_100_percent_fpl_48_states_dc_additional_member
name: snap_net_income_limit_100_percent_fpl_48_states_dc
```

This makes parametric reforms possible at the table cell or increment level
without changing the formula concept.

## Bad Names

Avoid these patterns:

```yaml
name: D
name: table_1
name: usda_elderly_or_disabled_60
name: co_snap_standard_deduction
name: snap_max_allotment_298
name: fy2026_values
```

Each of these either lacks legal meaning, embeds a parameter value, duplicates a
restated upstream concept downstream, or hides the actual concept behind source
layout.
