# Stage-3 NZ adversarial cures

Status: all seven findings cured; parity and feature-off identity hold.

Final branch SHA: `d32cf980bdd5a3b7a7e582604dffa90687784b69`

Certified implementation SHA: `d8d45a817b3700ed22ec49a9fcf1024aa6ed2198`

The harness patch pins the clean certified implementation commit, because the
commit containing a patch cannot contain its own Git object ID. The subsequent
certificate commit contains only the regenerated patch and proof receipt.

## Finding-by-finding cures

### 1. Best Start defect is structurally inexpressible

The old grammar admitted generic aliases and multiple family-scoped reductions,
so a plan could feed post-abatement child amounts back through a family sum or
consume the same family adjustment repeatedly.

The compiled grammar now has typed adult, child, and family inputs; a single
fixed operation, `sum_children_then_subtract_family_once`; explicit child-gross,
care-fraction, and family-adjustment operands; and one consumer per
`reduction_key`. Validation performs the family-scope uniqueness check before
new required-field checks and returns the named
`DuplicateFamilyScopeReduction` error.

The reviewer's exact formerly accepted plan and request are committed as:

- `tests/fixtures/unit_derivation/per_child_expressible.yaml`
- `tests/fixtures/unit_derivation/per_child_request.json`

`reviewer_best_start_plan_is_rejected_by_named_family_scope_guard` asserts the
exact named rejection. `every_stage3_structural_guard_kills_its_isolated_plan_mutant`
also mutates selectors, scopes, kinds, aliases, reduction keys, directions,
roles, citations, and references one at a time.

### 2. Unknown survives the derived-relation gate

Stage-2's derived-relation lifting now retains unresolved candidates and their
reasons instead of filtering them out. The aggregation boundary follows the
same semantics and fails closed: Unknown or Conflict relation knowledge returns
an Indeterminate family result naming the relation evidence. It cannot produce
a smaller Determined family.

The relevant mutation probes are:

- `lifted_derived_relation_never_drops_unknown_or_conflict`
- `unknown_and_conflict_survive_derived_relation_gates_for_counts_and_sums`
- `indeterminate_relation_knowledge_cannot_produce_a_smaller_determined_family`
- `omitted_relation_is_unknown_but_evidenced_complete_empty_is_known_empty`
- `unknown_status_survives_materialization_and_indeterminates_phase_two_count`

Missing and conflicting ages, facts, scalars, care fractions, family inputs,
and completeness evidence likewise remain Indeterminate rather than becoming
zero, false, or known-empty.

### 3. Person/child/family aggregation moved engine-side

The plan and typed materialization path now perform:

- adult wage, benefit, tax, net-wage, hours, and IETC sums;
- partner presence anchored to the explicit MC 7 entitlement holder;
- dependent-child membership, youngest age, all age-band counts, and the
  unpartnered/JSS/Accommodation Supplement shape predicates;
- eldest/subsequent-child care units and fixed entitlement-day/week scalars;
- family-income and care projections from Family to Child;
- IWTC child-care agreement;
- discrete Best Start child-gross/care aggregation followed by one
  family-scope adjustment;
- continuous Best Start family abatement; and
- weekly family wages used as the PTR denominator.

The copied harness has a single `aggregate_family` request-marshalling boundary.
Outside that boundary, Python reads the engine's family outputs.

What remains host-side, and why it is not relational aggregation:

- construction of the pinned Treasury scenarios and evidence-bearing engine
  requests;
- evaluation of each statute program for its proper Person, Child, or Family
  entity;
- ordering those program calls where one program's already aggregated output
  is another program's input;
- cross-program arithmetic such as WFF, Net Income, the $1 EMTR forward
  difference, RR/PTR, and diagnostics;
- the continuous-WFF diagnostic over already family-level values; and
- disclosed Treasury raw-source conventions, benefit gross-up, and
  Accommodation Supplement diagnostic mechanics.

None of those operations discovers members, classifies a person or child, or
reduces person/child values into a family. The copied harness tests inspect this
boundary and kill reintroduced `scenario.partnered`, `scenario.children`, or
`scenario.gross_wage2` reads in operational methods.

### 4. Harness commit, binary, plan, and cleanliness binding

`stage3-nz/ops-harness.patch` is regenerated from files obtained with
`git -C ops show bcf631b5:<path>`. It applies cleanly to a fresh copy and pins:

- engine commit `d8d45a817b3700ed22ec49a9fcf1024aa6ed2198`;
- release binary SHA-256
  `c5a3d3d8b1fdb8480bef7cffc6dd85658ca1a48cf7ca70da968b7a46c1e8a430`;
- the canonical aggregation-plan path inside that checkout; and
- the plan and compiled-artifact hashes in emitted provenance.

Preflight rejects a wrong commit, any tracked or untracked non-ignored engine
change, a binary outside the pinned checkout, a mismatched binary digest, and a
foreign aggregation plan. Each rejection has a copied-harness mutant.

### 5. Registry and production materialization path

Raw plan execution has been removed. The feature-on CLI now has two
registry-driven commands:

- `compile-unit-aggregation --plan ... --output ...`
- `run-unit-aggregation --artifact ... --enable-experimental-unit-derivation`

Compilation validates the typed plan and emits
`axiom/compiled-unit-aggregation-stage3/1` with canonical plan and constitution
digests plus an embedded compiled phase-two program. Loading recomputes both
digests, regenerates and compares the embedded phase, and passes it through the
production `CompiledProgramArtifact` loader. Runtime accepts only a registered
compiled artifact.

People, facts, Knowledge values, supplied entities, derived Families, and
relations then pass through the stage-2 entity registry and phase-two
materialization barrier. Public raw compile/execute helpers are not exported
outside tests. Artifact, plan, phase, duplicate-registration, raw-sidecar, and
unknown-field mutants are covered at unit and CLI level.

Four feature-gated published schemas cover the authoring plan, compiled
artifact, request, and result; exact CLI fixtures validate against them.

### 6. Tier-B legal inputs

The plan no longer treats deferred legal readings as engine defaults.

- Partner entitlement is cited to Income Tax Act 2007 MC 7, not MB 2. The
  caller supplies the entitlement holder. The explicit limitation says the
  engine does not choose between partners or exercise Commissioner discretion.
- An age-18 child requires three Knowledge-valued inputs: not financially
  independent under MC 9(1)(a), attending school or tertiary education under
  MC 9(1)(b), and inclusion in the Commissioner-determined period under
  MC 9(2)-(3).
- General/principal care and the separate IWTC principal-care/exclusive-care
  inputs cite MC 4 and MC 10.
- Best Start claimant care is an explicit fraction cited to MG 2(5), and is
  applied exactly once to each child gross amount before the single family
  adjustment.

The copied harness versions all three MC 9 facts and the distinct MC 10 and
MG 2 care facts in `eligibility-closures.json`. The pinned comparison contains
no age-18 child; age-18 behavior is exercised by engine mutation tests.

The citation text was checked against the local RuleSpec NZ corpus records
`2026-06-17-wff-eligibility.jsonl` (MC 7, MC 9, MC 10) and
`2026-06-17-wff-tax-credits.jsonl` (MG 2), and against the official
[MC 7](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518486.html),
[MC 9](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518490.html),
[MC 10](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518492.html),
and [MG 2](https://www.legislation.govt.nz/act/public/2007/0097/latest/LMS63785.html)
texts.

Declared limitations remain explicit for MC 7 allocation, the MC 9
Commissioner period, relationship and role classification, MC 10/MG 2 care,
the pinned full-period 365/52 mechanics, and the JSS scenario gate.

### 7. Honest adversarial tests and mutants

The new coverage includes:

- every family scalar, count, predicate, child projection, member, schema,
  digest, trace, and identity field;
- ages 0, 2, 3, 13, 14, 18, and 19, including each independent MC 9 failure
  and Unknown case;
- unpartnered, reversed-role, non-holder-role, multi-family, and unrelated
  roster cases;
- missing, Unknown, Observations, and Conflict inputs and completeness;
- inconsistent family abatements and IWTC child agreements;
- MG 2 care fractions 0, 0.5, and 1 plus out-of-range rejection;
- half-even rounding on both midpoint parities;
- stable family identity with request-bound trace changes;
- canonical relation ordering, exact-duplicate normalization, and evidence
  binding;
- compiled-artifact digest/phase tampering and registry collisions; and
- CLI runtime-gate failure, raw-plan rejection, malformed input, deterministic
  compile/run, and exact golden JSON.

The help inventory is now driven by the same command metadata as dispatch.
Feature-on help exposes both aggregation commands; feature-off help remains
byte-identical to the parent.

## Verification

Rust:

- `cargo test --locked`: 285 passed, 0 failed, 0 ignored.
- `cargo test --locked --features unit-derivation`: 328 passed, 0 failed,
  0 ignored.
- `cargo test --locked --all-features`: 344 passed, 0 failed, 0 ignored.
- Copied harness mutation suite: 14 passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

Feature-off parent comparison:

- Parent: `74881519c829106e4e696d77b38e10ba61c881d8`.
- Real compiled NZ composition SHA-256, parent and current:
  `355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2`.
- `cmp`: 0.
- Artifact format: 2; derived outputs: 176.
- Top-level help `cmp`: 0.

Full parity was run as two independent external harness invocations. Each
invocation itself performed and byte-compared two fresh compile/evaluate
passes:

- amount/control cells agreeing to cent: 1,454 / 1,976;
- outside-cent dispositions: 522 = 520 class B + 2 class C;
- class A: 0; class D: 0;
- `comparison.csv` SHA-256 in both runs and the prior review:
  `ccaa4dcb61b112587b47afb0e1892f670df354670fcd35f4d801edc621dd4bf2`;
- `comparison.csv` byte comparison: identical;
- sorted 522-disposition identity-set SHA-256 in both runs and the prior
  review: `44063af7ec7c3a4dc6a296b70efc18914762a0d88e59594610e025c4d671e14e`;
- only-in-run and only-in-prior disposition diffs: all empty;
- compiled production artifact SHA-256:
  `355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2`;
- compiled unit-aggregation artifact SHA-256:
  `dc1d368844e071c17649cd463568eca5e1d82eacf822694cce0ac45f1882fc41`.

The machine-readable receipt is `stage3-nz/parity-proof.json`.

## Commits and out-file limitation

- `d8d45a817b3700ed22ec49a9fcf1024aa6ed2198`
  — engine, registry, fixture, legal-input, and test cures.
- `d32cf980bdd5a3b7a7e582604dffa90687784b69`
  — bound harness patch and parity receipt; final branch SHA.

The requested destination
`/Users/maxghenis/TheAxiomFoundation/ops/nz-lane/_cert/sol-stage3-fixes.md`
is in the explicitly read-only ops repository and outside the writable
sandbox. This file is the completed out-file staged in the engine worktree for
handoff; no ops file was modified.
