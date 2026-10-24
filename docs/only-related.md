# Exactly one related Boolean input

`only_related(relation, input)` returns the Boolean input of the single related
entity active for the query period. It does not establish eligibility, membership,
or completeness of supplied relation facts. Callers must supply the relation
appropriate to the particular determination being queried.

```yaml
inputs:
  - name: decision
    entity: Subject
    dtype: Boolean
rules:
  - name: determination_subject
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Determination, Subject]
  - name: subject_decision
    kind: derived
    entity: Determination
    dtype: Boolean
    period: Month
    versions:
      - effective_from: '2026-01-01'
        formula: only_related(determination_subject, decision)
```

RuleSpec lowering requires exactly one Boolean input declaration in the citing
module, owned by the relation's related-slot entity. An unrelated import's
same-named declaration is not authority for this input. Direction follows the
existing declared-entity resolution; ambiguous same-kind direction is refused.
Derived values and non-Boolean projections are not supported.

The serialized scalar expression has a new `only_related` tag, a relation name,
distinct binary `current_slot` and `related_slot` coordinates, and an input-valued
`value` reference. Old parsers reject the new function or tag. This is an additive
expression, following the existing expression-level schema policy; archived
artifact schemas remain unchanged. Raw `ProgramSpec` does not carry RuleSpec input
declarations, so it cannot prove input ownership or declared dtype. Relation
coordinate/kind validation still applies and the returned value is strictly
checked to be Boolean at evaluation.

Evaluation is lazy: an unreached projection does not require relation facts.
For a reached projection:

- No distinct active target produces `MissingRelation`.
- More than one distinct active target produces `AmbiguousRelation`, even when
  the targets happen to have equal values.
- One target reads its input normally. Missing or uncovered input periods and
  conflicting input facts retain the existing input errors; non-Boolean values
  produce `TypeMismatch`.

Existing sparse relation traversal deduplicates identical target IDs, applies
relation predicates, and requires coverage of the query period. Dense relation
batches already represent related rows rather than ID-labelled edges; each row
is a distinct target, and equal values are not deduplicated. Dense execution
applies cardinality after relation filtering and only reads the selected input
for reached rows with exactly one target. Decimal and f64 execution use the same
Boolean path. Existing operators retain their behavior.

This expression does not validate a whole supplied collection or infer an absent
relationship as false. A policy using it must independently specify the meaning
of its relation and decision context; it must not assume that different persons
in one household necessarily share a decision.

The dense compiler retains its existing restrictions on nested relation
aggregations. A projection nested inside another relation expression is refused
rather than approximated. In lifetime execution a standalone relation projection
has no single relation period and is refused; use an explicit supported
per-period reduction when that is the intended calculation. These restrictions
are not evidence that a missing relationship is false.
