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
(source verification, interpretations, summary — see
[rulespec.md](rulespec.md)), an optional `inputs` block (elections and the
declarations required by `only_related` and `input_for_year`; see
[Input declarations and elections](rulespec.md#input-declarations-and-elections)),
and a `rules` list.

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

## Interpretations

`module.interpretations` records each reading an encoding adopts when its
source is ambiguous, contradicts itself, or is read other than literally.
Encoders and reviewers require an entry for a conflict between provisions,
including a conflict between operative text and a summary table; a departure
from literal text, such as supplying a missing multiplier or restoring a
dropped qualifier; and any question put to the publisher, whatever reading
is encoded while awaiting the reply.

The block is descriptive and inert: it never changes lowered output names,
compiled rules, or execution results. Provenance hashes that cover the whole
source artifact do change when entries are added: the experimental aggregation
response's `plan_digest` and `trace_root`, for example, while every computed
value stays the same. The root module's entries survive lowering
on `program.module` and round-trip through compiled-artifact JSON. The list
defaults to empty and is omitted when empty.

This complete synthetic example illustrates a ruled reading for the invented
Lantern Supply Scheme in the fictional Lantern jurisdiction. Its excerpts,
organisation, and record are invented for this example.

```yaml
format: rulespec/v1
module:
  interpretations:
    - id: weekly-review
      question: >-
        Does the Lantern Supply Scheme require a review at each weekly boundary
        or at each daily boundary?
      texts:
        - corpus_citation_path: zz-fictional-lantern/manual/lantern-supply-scheme/review-schedule
          excerpt: "Review supplies at the start of each period."
      reading: Review supplies at each weekly boundary.
      alternatives:
        - id: daily-review
          reading: Review supplies at each daily boundary.
          effect: Reviews would occur more often.
      basis: >-
        The synthetic guide's review calendar describes a weekly review,
        so its use of "period" is read as a week.
      status: ruled
      ruling:
        by: Fictional Lantern Scheme Office
        date: '2024-10-10'
        record: https://example.org/fictional-lantern/weekly-review
      affects:
        - lantern_weekly_review_required
rules:
  - name: lantern_weekly_review_required
    kind: parameter
    dtype: Boolean
    versions:
      - effective_from: '2024-04-01'
        formula: "true"
```

Each entry has these fields. Every mapping in an entry rejects unknown keys.

| Field | Shape and meaning |
|---|---|
| `id` | Required string, unique within the module; matches `^[a-z0-9]+(-[a-z0-9]+)*$`. |
| `question` | Required non-blank string; one question in plain words. |
| `texts` | Required non-empty list of mappings with `corpus_citation_path` and non-blank `excerpt`; the provisions in tension, verbatim. |
| `reading` | Required non-blank string describing the adopted reading. |
| `alternatives` | Required non-empty list of mappings with required string `id`, non-blank `reading`, and optional string `effect` describing what changes. Each alternative id matches `^[a-z0-9]+(-[a-z0-9]+)*$` and is unique within its interpretation. |
| `basis` | Required non-blank string explaining why this reading was adopted. |
| `status` | Required string: `open`, `ruled`, `confirmed`, or `contradicted`. Objects and YAML enum tags are rejected. |
| `ruling` | Status-dependent mapping with required `by`, `date`, and `record`. |
| `confirmation` | Status-dependent mapping with required `by`, `date`, and non-blank `excerpt`, plus optional `record`; quotes the publisher's confirming or contradicting words. |
| `affects` | Required non-empty list of names defined by `rules[].name` in this document. |

The engine requires or forbids the status-dependent mappings as follows:

| Status | `ruling` | `confirmation` |
|---|---|---|
| `open` | Forbidden | Forbidden |
| `ruled` | Required | Forbidden |
| `confirmed` | Required | Required |
| `contradicted` | Required | Required |

At load time, the engine checks all required fields and types, both id patterns
and their respective uniqueness scopes, non-empty lists, and the non-blank
strings listed above.
`ruling.by` and `confirmation.by` must also be non-blank. Each
`texts[].corpus_citation_path` must pass the same canonical corpus citation
path check as source verification. Dates must have exact `YYYY-MM-DD` shape
and identify real calendar dates. `ruling.record`, and `confirmation.record`
when present, must start with `https://`.

`affects` resolves against this document's rule names before imports merge or
runtime names are assigned. An imported name or a name listed only in
`deferred_outputs` is insufficient. An error reports all unresolved names
for the entry together. Compiled-artifact loading checks entry structure;
the document-local resolution check occurs while loading RuleSpec, where the
original document's rules are available.

Public records name organisations in `ruling.by` and `confirmation.by`, never
individuals. `confirmation.by` names the publisher. A ruling links a public
PR, issue, or comment; omit people’s names, decision-queue ids, session ids,
and local paths. These are authoring and review requirements: the engine
checks non-blank organisation fields and the HTTPS prefix, rather than
identifying organisations or checking public access.

The engine does not retrieve or match corpus text, so these are requirements
on the encoder: it must verify each excerpt verbatim against corpus text at
its citation, must not count interpretation excerpts toward its
complete-source coverage gate, and must treat `contradicted` as a blocking
finding requiring repair of the adopted reading. Model-generated entries may be `open` only: a human
ruling or publisher reply supplies `ruling` or `confirmation`.

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

Format-3 compiled artifacts also publish `program.interface.inputs` for
slots expressions actually read. Each entry uses its catalog canonical
request name as `legal_id`, lists the entity kinds whose ids can read it,
and reports conservative usage-derived type/default/domain information plus
an election, when declared. An unused `indexed_by` label remains in the
catalog and is absent from the interface. The complete derivation and
format-2 opt-in are in [the artifact format reference](artifact-format.md).

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
and double- or single-quoted strings. Inside a string, `\n`, `\r`, and `\t`
stand for a newline, carriage return, and tab; a backslash before any other
character, including `\\`, `\"`, `\'`, or a non-ASCII character such as
`\é`, stands for that character. `#` starts a comment that runs to the
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
strict-mode error, so always write the fallback arm. When a match without `_`
does compile, a subject that equals none of its patterns is an evaluation
error naming the rule, the subject and its value, in every execution mode; it
never takes the last arm's value. It fails only an evaluation that reaches the
match: a row whose conditions select another branch, or that reads the
matching rule only from such a branch, is unaffected.

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
| `max(a, b, ...)` | 1 or more | largest argument; `max()` is a compile error |
| `min(a, b, ...)` | 1 or more | smallest argument; `min()` is a compile error |
| `min(relation.field)` | 1 | least `field` over related entities; fails when there is none |
| `max(relation.field)` | 1 | greatest `field` over related entities; fails when there is none |
| `ceil(x)` | 1 | round up to an integer |
| `floor(x)` | 1 | round down to an integer |
| `days_between(from, to)` | 2 | day count between two dates |
| `date_add_days(date, days)` | 2 | shift a date by a day count |
| `date_add_months(date, months)` | 2 | shift by an integer number of calendar months; clamp a missing target day to month end |
| `date_add_years(date, years)` | 2 | shift by calendar years using the same month-end clamping |
| `len(relation)` | 1 | count of related entities |
| `sum(relation.field)` | 1 | sum a field over related entities |
| `count_where(relation, predicate_field)` | 2 | count related entities whose Boolean `predicate_field` holds |
| `sum_where(relation, field, predicate_field)` | 3 | sum `field` over related entities whose `predicate_field` holds |
| `min_where(relation, field, predicate_field)` | 3 | least `field` over related entities whose `predicate_field` holds; fails when there is none |
| `max_where(relation, field, predicate_field)` | 3 | greatest `field` over related entities whose `predicate_field` holds; fails when there is none |
| `only(relation.field)` | 1 | `field` of the one related entity; fails when there are none or several |
| `sum_over_periods(x)` | 1 | sum `x` across the entity's own periods |
| `max_over_periods(x)` | 1 | maximum of `x` across periods |
| `count_over_periods(x)` | 1 | period count of `x` |
| `sum_top_n_over_periods(x, n)` | 2 | sum the `n` largest per-period values |

The `_where` predicate names a Boolean input or a derived judgment computed
on the related entity. The lowerer enforces the fixed arities (`ceil`,
`floor`, `days_between`, `date_add_days`, `date_add_months`, `date_add_years`,
`count_where`, `sum_where`, `min_where`, `max_where`, `only`, and the
reductions); `max` and `min` take one or more arguments. They aggregate over
related entities only when their single argument is a relation field access
(`min(member_of_family.age)`); every other form is the scalar `min`/`max`.
The related aggregates never default: an empty `min`/`max` and an `only` over
zero or several entities fail the evaluation, so a rule whose law gives the
empty case a value guards it (`if len(member_of_family) > 0:
min(member_of_family.age) else: ...`). See [RuleSpec](rulespec.md#minimum-maximum-and-exactly-one-over-related-entities). The `*_over_periods` reductions are meaningful only
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
  - name: youngest_member_age
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: |-
          if len(member_of_household) > 0: min(member_of_household.age)
          else: 0
  - name: household_rent_of_member
    kind: derived
    entity: Person
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: '2026-01-01'
        formula: only(member_of_household.rent)
```

Relation entity typing is mandatory. Every relation an aggregate or
membership test executes must have one entity kind per tuple slot, in tuple
order: a `data_relation` declares them with `arguments`, and a
`derived_relation` inherits its source's. Compiling a program that executes an untyped relation
fails with `untyped_relation`, because the engine cannot check dataset tuples
against slots it knows nothing about, and a tuple stored in the other
orientation would aggregate nothing.

For a two-slot relation, aggregates use the slot whose kind is the enclosing
rule's `entity` as the current entity and the other slot as the related
entity. Both `[Person, Household]` and `[Household, Person]` work when dataset
tuples follow the declared order. Compilation fails when the direction cannot
be read from the kinds:

- when no slot holds the rule's entity (`relation_current_slot_entity_mismatch`);
- when a rule evaluated on the related ids has another entity
  (`relation_related_slot_entity_mismatch`);
- when every slot holds the rule's entity, as in `[Person, Person]`
  (`AmbiguousRelationDirection`). Wrap such a relation in a
  `derived_relation` whose explicit `current_slot` and `related_slot` say which
  side the rule aggregates from.

An aggregate over a `derived_relation` uses the derivation's `current_slot` and
`related_slot`. A derived relation takes its source's slot kinds; if it
declares `slot_entities`, they must match the source's.

A relation name a module aggregates without declaring it (a composition
root, or a module relying on an import) is that module's own relation,
separate from any same-named declaration in its imports, and it has no
declared kinds: executing it is an `untyped_relation` error whose message
names those declarations. Declare the relation, typed, in the module that
aggregates it, or aggregate the imported relation through the module that
declares it (see axiom-rules-engine#226 on aliasing).

The explicit format-2 compatibility loader still enforces relation typing.
An old compiled artifact that executes an untyped relation fails with a
message naming
`axiom-rules-engine migrate artifact`, which types the artifact as it executes
(see [`docs/rulespec.md`](rulespec.md#relation-entity-typing)). Recompiling from
typed source is the durable fix.

Wire dataset binding rejects known tuple entity-kind mismatches by default.
For example, a relation declared `[TaxUnit, Person]` expects the tax unit id
first and the person id second after RuleSpec compilation. Input records must
also label those ids `TaxUnit` and `Person`; `Entity` is not a wildcard. A
request can explicitly set `"relation_binding": "lenient"` during migration,
but leniency does not reorder the tuple and may still produce a zero count.
See [dataset binding in RuleSpec](rulespec.md#semantics) for the CLI override,
response metadata, and limits for unknown ids and older compiled artifacts.

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

Scalar kinds: `literal`, `input`, `input_for_year`, `derived`,
`parameter_lookup`, `add`, `sub`, `mul`, `div`, `max`, `min`, `ceil`,
`floor`, `period_start`, `period_end`, `date_add_days`, `date_add_months`,
`date_add_years`, `days_between`, `only_related`, `single_related`,
`count_related`, `sum_related`, `extreme_related`, `if`, `no_match`,
`over_periods`.

Judgment kinds: `comparison` (with `op` one of `lt`, `lte`, `gt`, `gte`,
`eq`, `ne`), `derived`, `relation_member`, `and`, `or`, `not`,
`exactly_one`.

The wire schemas (execution requests, compiled artifacts) also admit
`input_or_else`, an input read with a fallback value, for hand-built
ProgramSpec programs. No RuleSpec construct lowers to it, and RuleSpec
refuses rule-level defaults.

Literal values carry their own `kind`: `bool`, `integer`, `decimal`, `text`,
or `date`.

Formula surface and lowered form do not map one-to-one: `match` lowers into
nested `if` comparisons (a match without `_` ends in a `no_match` node carrying
the subject and every pattern, which evaluates to an error), unary minus lowers into `0 - x`, chained `and`/`or`
operators nest as binary pairs while some sugar lowers n-ary, and Boolean
facts lower into `== true` comparisons. Consumers should target this
vocabulary, not the formula text. Dense execution accepts a `no_match` only as
the `else` of an `if`, where the lowering puts it, and refuses a program with
one anywhere else.

Calendar shifts accept negative and zero offsets. For example, adding one
month to 2025-01-31 yields 2025-02-28; adding one year to 2024-02-29 yields
2025-02-28. Shifts outside the supported date range return an evaluation
error. These are date operations, not jurisdictional age or deadline rules:
encode statutory inclusion, exclusion and expiry semantics explicitly.
As with date_add_days, generic dense and explain execution support calendar
shifts; the specialized bulk path reports them as unsupported.

### Reading a declared annual input for a selected year

`input_for_year(income, 2025)` reads the current entity's `income` fact covering
January 1 through December 31, 2025, even when the enclosing query is monthly.
The initial form accepts only an integer year literal from 1 through 9999. Rule
versions must express the source's applicable reference year; the engine does
not infer a jurisdiction's reference-year policy or accept a caller override.

The primitive must have one explicit declaration in the citing module, a
matching entity, `period: Year`, a supported scalar dtype, and no default. A
`Money` input also requires a declared currency unit. Imported declarations
sharing a runtime slot, foreign primitives, derived values and undeclared inputs
are rejected. The compiled `selected_year_inputs` contract retains the input's
canonical identity, runtime slot, entity, dtype, period and unit. Structural IR
validation is not proof of source authorship or source certification.

The lookup changes only the input's period. It preserves the current entity and
the query period used for relation membership, rule versions and other inputs.
A related entity's annual input can be read through a typed derived rule over
that entity. Direct annual reads in relation predicates are not supported.

Missing facts, wrong value types and ambiguous equal-precedence facts fail;
zero and false remain actual values. Decimal values retain their precision and
are not automatically rounded to currency minor units. Existing latest-start
covering-interval precedence applies. The trace records the selected input,
entity, annual interval and value separately from the enclosing query.

Explain executes the lookup. Fast falls back to Explain with the original
historical dataset when execution reaches this expression; an inactive branch
does not force a fallback. Dense and lifetime execution refuse the expression
because those representations do not retain the required historical axis.
Older engines reject the new expression rather than silently treating it as a
current-period input. Hosts must deploy matching compiler/runtime artifacts and
transport historical facts explicitly before using this feature.
