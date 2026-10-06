# RuleSpec format reference

This is the expression-grammar reference for RuleSpec authoring: what a rule
record contains, what the formula language accepts, and what the lowered,
`kind:`-tagged expression vocabulary looks like to downstream consumers.
[rulespec.md](rulespec.md) is the companion for operational semantics — rule
kinds in depth, versioning and effective ranges, currency rounding, source
pinning, and provenance. The JSON Schemas in [../schemas/](../schemas/)
(regenerated with the `emit-schemas` CLI subcommand) stay authoritative for
exact field shapes.

Every `yaml` block in this document is a complete module that CI lowers
through the real loader (`tests/format_reference_examples.rs`), so the
examples cannot drift from the grammar they document.

## Module shape

A module is one YAML document: a `format` marker, an optional `module` block
(proof requirements, source verification, summary — see
[rulespec.md](rulespec.md)), and a `rules` list.

```yaml
format: rulespec/v1
rules:
  - name: snap_maximum_allotment
    kind: parameter
    dtype: Money
    unit: USD
    source: 7 USC 2017(a)
    versions:
      - effective_from: '2026-10-01'
        formula: "994"
  - name: snap_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: '2026-10-01'
        formula: max(0, snap_maximum_allotment - net_income * 0.3)
```

Rule records share these fields:

- `name` — the identifier other formulas use to reference the rule.
- `kind` — `parameter`, `derived`, `data_relation`, `derived_relation`, or
  `source_relation`. [rulespec.md](rulespec.md) documents each kind's
  semantics and required sub-blocks.
- `entity` — the entity kind a `derived` rule is computed for (for example
  `Person`, `Household`, `TaxUnit`).
- `dtype` — see the data-type table below.
- `period`, `unit`, `source`, `source_url` — optional descriptive fields.
- `metadata` — tolerated and ignored by the engine loader; encoding tooling
  and validation gates consume it (proof atoms — see
  [rulespec.md](rulespec.md) on source pinning).
- `versions` — dated entries. Each version has `effective_from`, an optional
  inclusive `effective_to`, and either a `formula` (expression source) or,
  for indexed parameters, a `values` table.

## Data types

`dtype` accepts these names (case variants as listed):

| Declared | Lowers to |
|---|---|
| `Judgment`, `judgment` | judgment (three-valued: holds / not-holds / undetermined) |
| `Boolean`, `Bool`, `boolean`, `bool` | boolean |
| `Integer`, `integer`, `int` | integer |
| `Money`, `money`, `Rate`, `rate`, `Decimal`, `decimal`, `float` | decimal |
| `Text`, `text`, `String`, `string` | text |
| `Date`, `date` | date |

Anything else — including a missing `dtype` — silently lowers to decimal, so
spell the names exactly.

A rule with `dtype: Judgment` lowers its formula in judgment position; every
other dtype lowers in scalar position. The two positions accept different
constructs, listed below.

## Identifier resolution

A bare identifier in scalar position resolves in this order: the reserved
names `period_start` and `period_end` (the evaluation period's bounds), then
a declared parameter (lowered as a zero-index table lookup), then a declared
derived rule, and otherwise a free input slot supplied by the caller at
execution time. In judgment position the order is: a declared judgment
derived rule, and otherwise a free Boolean input compared against `true`.
Relation-predicate membership resolution applies only when lowering
`derived_relation` predicate formulas, not ordinary rules.

Undeclared names are not an error at lowering time — a typo becomes an input
the engine then reports as missing. Compile queries can surface the
resulting input catalog; `metadata.input_catalog` on compiled artifacts
lists every owner.

## Indexed parameters

A `parameter` rule with `indexed_by` and versioned `values` encodes a source
table. Formulas address cells with `table_name[index_expr]`.

```yaml
format: rulespec/v1
rules:
  - name: eitc_phase_in_rates
    kind: parameter
    dtype: Rate
    indexed_by: qualifying_child_count
    source: 26 USC 32(b)(1)
    versions:
      - effective_from: '2026-01-01'
        values:
          0: 0.0765
          1: 0.34
          2: 0.40
          3: 0.45
  - name: eitc_phase_in_rate
    kind: derived
    entity: TaxUnit
    dtype: Rate
    period: Year
    versions:
      - effective_from: '2026-01-01'
        formula: eitc_phase_in_rates[eitc_capped_child_count]
```

## Operators

Binary and unary operators, loosest-binding first (the parser's precedence
chain; parenthesize whenever a formula's grouping matters to a reader):

| Level | Operators | Position |
|---|---|---|
| 1 (loosest) | `or` | judgment |
| 2 | `and` | judgment |
| 3 | `<` `<=` `>` `>=` `==` `!=` | comparison: scalar operands, judgment result |
| 4 | `+` `-` | scalar |
| 5 | `*` `/` | scalar |
| 6 (tightest) | unary `-`, `not` | scalar / judgment |

Literals: integers (`3`), decimals (`0.3`), booleans (`true` / `false`),
and double- or single-quoted strings. `#` starts a comment that runs to the
end of the line. Date literals are not part of the expression grammar —
dates come from inputs, parameters, the reserved `period_start` /
`period_end` identifiers, or the version envelope.

```yaml
format: rulespec/v1
rules:
  - name: household_is_eligible
    kind: derived
    entity: Household
    dtype: Judgment
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: |-
          (net_income <= income_limit or has_categorical_eligibility)
          and not is_disqualified
          and household_size > 0
```

## Conditionals

Scalar formulas branch with `if cond: value` / `elif cond: value` /
`else: value`. The condition lowers in judgment position; the branches lower
in scalar position.

```yaml
format: rulespec/v1
rules:
  - name: standard_utility_allowance
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: '2026-01-01'
        formula: |-
          if has_heating_or_cooling_cost: full_sua_amount
          elif has_two_utility_costs: limited_sua_amount
          else: 0
```

## Match

`match subject:` selects by equality against literal patterns, one
`pattern => result` arm per line. A final `_ => result` arm is the explicit
fallback; matches without `_` are a compatibility-mode warning and a
strict-mode error, so always write the fallback arm.

```yaml
format: rulespec/v1
rules:
  - name: eitc_phase_out_rate
    kind: derived
    entity: TaxUnit
    dtype: Rate
    period: Year
    versions:
      - effective_from: '2026-01-01'
        formula: |-
          match eitc_capped_child_count:
              0 => 0.0765
              1 => 0.1598
              2 => 0.2106
              3 => 0.2106
              _ => 0.2106
```

## Functions: scalar position

| Function | Arity | Meaning |
|---|---|---|
| `max(a, b, ...)` | any | largest argument |
| `min(a, b, ...)` | any | smallest argument |
| `ceil(x)` | 1 | round up to an integer |
| `floor(x)` | 1 | round down to an integer |
| `days_between(from, to)` | 2 | day count between two dates |
| `date_add_days(date, days)` | 2 | shift a date by a day count |
| `len(relation)` | 1 | count of related entities |
| `sum(relation.field)` | 1 | sum a field over related entities |
| `count_where(relation, predicate_field)` | 2 | count related entities whose Boolean `predicate_field` holds |
| `sum_where(relation, field, predicate_field)` | 3 | sum `field` over related entities whose `predicate_field` holds |
| `sum_over_periods(x)` | 1 | sum `x` across the entity's own periods |
| `max_over_periods(x)` | 1 | maximum of `x` across periods |
| `count_over_periods(x)` | 1 | period count of `x` |
| `sum_top_n_over_periods(x, n)` | 2 | sum the `n` largest per-period values |

The `_where` predicate names a Boolean input or a derived judgment computed
on the related entity. The lowerer enforces the fixed arities (`ceil`,
`floor`, `days_between`, `date_add_days`, `count_where`, `sum_where`, and
the reductions); `max` and `min` accept any argument list unchecked,
including an empty one. The `*_over_periods` reductions are meaningful only
under the lifetime execution surface; per-period execution paths reject
them, and an unrecognized `*_over_periods` name fails lowering with an error
that lists the supported reductions. Any other unknown function name fails
lowering.

```yaml
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: len(member_of_household)
  - name: countable_earned_income
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: '2026-01-01'
        formula: sum_where(member_of_household, earned_income, is_countable_member)
```

## Judgment position

A `dtype: Judgment` formula composes:

- comparisons between scalar expressions;
- `and`, `or`, `not`;
- references to other judgment-dtype derived rules;
- bare Boolean facts (an undeclared name lowers to an input compared against
  `true`);
- inside `derived_relation` predicate formulas only: relation-predicate
  names, which lower to membership tests;
- `exactly_one(a, b, ...)` — two or more judgment-position arguments; holds
  when exactly one holds. Serialises as a first-class `exactly_one` node
  (one n-input gate in artifacts and graphs); evaluation lowers it to the
  or-of-and-of-nots expansion, keeping outcomes, short-circuit reads, and
  missing-input faults identical to the hand-written form — see the
  mutual-exclusivity section of [rulespec.md](rulespec.md).

Scalar constructs (`if`, `match`, arithmetic, the function table above) are
rejected in judgment position, and judgment constructs are rejected in scalar
position — the two grammars meet only at comparisons.

## The lowered expression vocabulary

Lowering a module produces a program whose expressions are `kind:`-tagged
objects (snake_case). This is the vocabulary downstream consumers — compiled
artifacts, the graph viewer, wasm hosts — actually traverse; the generated
JSON Schemas give exact field shapes.

Scalar kinds: `literal`, `input`, `input_or_else`, `derived`,
`parameter_lookup`, `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil`,
`floor`, `period_start`, `period_end`, `date_add_days`, `days_between`,
`count_related`, `sum_related`, `if`, `over_periods`.

Judgment kinds: `comparison` (with `op` one of `lt`, `lte`, `gt`, `gte`,
`eq`, `ne`), `derived`, `relation_member`, `and`, `or`, `not`,
`exactly_one`.

Literal values carry their own `kind`: `bool`, `integer`, `decimal`, `text`,
or `date`.

Formula surface and lowered form do not map one-to-one: `match` lowers into
nested `if` comparisons, unary minus lowers into `0 - x`, chained `and`/`or`
operators nest as binary pairs while some sugar lowers n-ary, and Boolean
facts lower into `== true` comparisons. Consumers should target this
vocabulary, not the formula text.
