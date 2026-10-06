# Stage 3 NZ out-file

Status: complete. `PROGRESS.md` was not modified.

## Aggregations moved behind the engine boundary

- Explicit Person roster plus `partner_of` and `dependent_child_of` relation
  facts derive one Family unit and its member projections.
- Partner presence is derived from the Family relation graph.
- Adult values are summed to Family: weekly and annual wages, family-scheme
  base income, net and gross benefits, wage tax, net wage, hours, IETC, and
  continuous IETC.
- Child values are derived by age band: total dependent children, ages 0--2,
  0--13, and 14--18, plus youngest-child age.
- Family-scheme income is broadcast into each Child projection before the
  Child Best Start evaluation.
- Best Start is reduced as `sum(child gross credit) - family abatement once`,
  clamped at zero. The plan grammar has no per-child abatement operator.

The harness still marshals primitive Person scalars and explicit scenario
relationship facts into the request. It no longer performs any of the above
reductions or family/child-shape calculations in Python.

## Plan fixture and legal boundary

The plan is
`tests/fixtures/unit_derivation/nz_income_explorer_family.yaml`. It is limited
to the pinned IncomeExplorer comparison and records the cited Income Tax Act
2007 family/partner/child provisions on the membership and aggregation
operations. Relationship facts are explicit and complete inputs; the engine
does not infer legal relationship status from age, income, or missing facts.
Accordingly no deferred Tier-B choice is made. The 40-significant-digit
round-half-even context is also an explicit plan input because it is part of
the pinned comparison's numeric reproducibility contract, not a legal reading.

## What stage 3 added beyond the stage-2 prototype

- A declarative experimental aggregation-plan parser and executor in
  `src/unit_derivation/aggregation.rs`.
- Runtime construction of the stage-2 `ConstitutionPlan` from the explicit
  roster and relation facts, followed by the existing compiler/deriver.
- Family scalar sums, child counts/minimums, Family-to-Child broadcasts, and
  the single structurally safe family-once reducer.
- Arbitrary-precision decimal aggregation with explicit significant-digit
  precision so the engine boundary preserves the pinned 40-digit results.
- Feature-gated CLI command `run-unit-aggregation`, requiring both the Cargo
  `unit-derivation` feature and the existing explicit runtime enable switch.

The v2 artifact schema and RuleSpec lowering were not changed. With the
feature disabled the new module, dependency, and CLI branch are not compiled.
Against clean stage-2 merge `74881519c829106e4e696d77b38e10ba61c881d8`,
the default-feature help text and compiled NZ composition artifact are
byte-identical; both artifacts have SHA-256
`355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2`.

## Parity proof

The patched harness ran end to end against the pinned expanded Treasury
snapshot and performed two fresh runs:

- 1,454 / 1,976 amount/control cells agree to the cent.
- 522 amount/control exceptions are unchanged: class B 520, class C 2,
  class A 0, class D 0.
- All 2,080 primary comparison rows, including 104 EMTR rows, are
  byte-identical to `bcf631b5:nz-lane/emtr_reproduction/comparison.csv`.
- Therefore every cell value, classification, reason code/title, and ordered
  disposition coordinate is identical. The pinned format has no literal
  `disposition_id` field; its effective identity tuple
  `(scenario_id, weekly_wage, column, classification, reason_code,
  reason_title)` is unchanged.
- The patched harness reports `deterministic_fresh_runs: 2`, 883 ordinary
  engine evaluations, and 1,411 unit-aggregation evaluations.

The regenerated `comparison.json` is intentionally not byte-identical to the
old JSON because its provenance, methodology, and coverage evidence now name
the aggregation plan and engine-owned reduction. Its comparison CSV payload
is byte-identical, and each patched fresh run is byte-identical to the other.

## Tests

- Full engine, feature off: 284 passed, 0 failed, 0 ignored.
- Full engine, feature on: 309 passed, 0 failed, 0 ignored.
- Unit-derivation focused suite: 25 passed, 0 failed.
- Pinned harness declared-closure tests: 2 passed.
- The NZ fixture test verifies relation-derived Family/Child projections,
  long-decimal sum fidelity, broadcasts, and the family-once Best Start value.
- The regression test reproduces the old host result (6,082) versus the
  correct family-once result (7,082), then proves the plan parser rejects a
  `sum_per_child_after_abatement` operation.

## Ops harness patch

Apply `stage3-nz/ops-harness.patch` to ops commit `bcf631b5`. It changes only
`nz-lane/emtr_reproduction/run.py`; the pinned composition and eligibility
closures remain unchanged. The patch passes a dry-run application check,
Python compilation, the two closure tests, and the end-to-end comparison.

## Honest limits

- This is an experimental plan API beside artifact v2, not a relation-bearing
  artifact-schema extension. The compiled RuleSpec composition remains
  relation-free; the separate engine-owned aggregation plan supplies the
  relation semantics and reductions, as allowed by the stage-2 extension
  route.
- GitHub issue comment 5299498361 was unavailable from the local checkout and
  could not be fetched in this environment. The implementation follows the
  verbatim ratification supplied in the brief and the matching Tier A/D versus
  deferred Tier B boundary recorded in `docs/unit-derivation.md`.
