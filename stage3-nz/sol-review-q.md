# Adversarial review R2 — stage-3 NZ engine-side aggregation

Target: `fdfcabc1c15613e11bb68866a7adc7c57a794349` on parent
`74881519c829106e4e696d77b38e10ba61c881d8`.

Overall verdict: **DO-NOT-MERGE**.

Only S-A holds. The pinned numerical parity is reproducible after correcting a
stale harness SHA in the scratch copy, but the committed proof is not runnable
against its own target. More importantly, the grammar can express the exact
6,082 per-child Best Start defect, Unknown is still dropped by a derived
relation gate, material legal inputs are absent or hardcoded, several family
reductions remain in Python, and the aggregation sidecar bypasses the common
production artifact/materialization path.

| Surface | Verdict | Short reason |
|---|---|---|
| S-A flag off | **HOLDS** | The real pinned NZ composition compiles byte-identically to the parent artifact. |
| S-B parity | **BREAK** | Numbers and 522 identities reproduce after an audit-only pin correction; the committed patch pins the wrong engine SHA and accepts dirty, unbound binaries. |
| S-C migration completeness | **BREAK** | Continuous Best Start, child-shape logic, and PTR family wages remain host reductions; two advertised age bands are dead outputs. |
| S-D Best Start inexpressibility | **BREAK** | An accepted valid plan emits canonical `8082 -> 6082` fields three different ways. |
| S-E Indeterminate | **BREAK** | Direct sum/count propagate, but a derived-relation gate drops Unknown; the new wrapper also converts omitted relations to known-empty. |
| S-F Tier-B/legal inputs | **BREAK** | Completeness, role direction, partner anchor, age-18 conditions, and care are missing, cosmetic, or hardcoded. |
| S-G containment | **BREAK** | v2/lowering are unchanged, but aggregation is a raw YAML/JSON sidecar outside the production artifact/materialization registry path. |
| S-H test honesty | **BREAK** | 284/309 are real; only two tests are new and neither covers the adversarial semantics. A help-inventory test is demonstrably false-positive. |

## Review basis

I read the whole `git show fdfcabc` diff, the worker brief and both worker
reports, the stage-2 S1-S8 review, and the checked-in ratification matrix in
[`docs/unit-derivation.md`](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/docs/unit-derivation.md:723).
That matrix records Tier A+D as ratified and Tier B as deferred, and expressly
requires Unknown/Conflict to survive derived-relation filters
([§8](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/docs/unit-derivation.md:543)).

The ops harness was reconstructed from `bcf631b5` under
`stage3-nz/_scratch/review-b/`, excluding `PROGRESS.md`, and the committed
patch was applied there. The ops checkout and every `PROGRESS.md` remained
untouched.

## S-A — HOLDS: feature-off real-composition output identity

I built parent and target without `unit-derivation` into isolated target
directories, then compiled the pinned real
`nz-lane/emtr_reproduction/composition.yaml` with both binaries and the same
pinned RuleSpec checkout.

```text
parent artifact SHA-256  355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2
target artifact SHA-256  355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2
cmp exit                  0
artifact format           2
derived outputs           176
```

The target feature-off suite also reproduced **284 passed, 0 failed, 0
ignored**. The two release executable files did not have identical hashes
(manifest/dependency wiring changed), but the requested observable artifact
from a real composition is byte-identical.

## S-B — BREAK: parity holds only after repairing the committed proof

The committed patch changes `EXPECTED_ENGINE_SHA` to the parent SHA
`7488151`, not target `fdfcabc`
([patch line 21](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/ops-harness.patch:21)).
Running the applied patch unchanged against the review target exits 2:

```text
error: engine SHA mismatch: expected 74881519c829106e4e696d77b38e10ba61c881d8,
found fdfcabc1c15613e11bb68866a7adc7c57a794349
```

For the numerical audit only, I changed that one constant in the scratch copy
to the full target SHA. Two independent end-to-end invocations then succeeded;
each invocation itself performs and byte-compares two fresh artifact builds.

```text
amount/control cells                 1,976
agree to cent                        1,454
outside cent                           522
class B / C / A / D              520 / 2 / 0 / 0
primary comparison rows              2,080
ordinary engine evaluations            883
unit-aggregation evaluations         1,411
```

The regenerated and pinned `comparison.csv` files both hash to
`ccaa4dcb61b112587b47afb0e1892f670df354670fcd35f4d801edc621dd4bf2`;
`cmp` returns 0. I independently constructed the outside-cent disposition set
as
`(scenario_id, weekly_wage, column, classification, reason_code, reason_title)`.
Both sets contain 522 unique identities and `diff` is empty. The two external
runs also produce byte-identical `comparison.csv`; their `comparison.json`
diff is only the deliberately different output-directory path.

That confirms the number, but not the committed certificate. The patch also
removes the engine dirty-check
([lines 64-72](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/ops-harness.patch:64))
and never compares `engine_binary_sha256` to an expected value: it merely
records the hash. Thus a checkout at the expected HEAD can run arbitrary dirty
source/binary content and still produce a nominally pinned certificate.

Required fix:

1. Regenerate the patch with `EXPECTED_ENGINE_SHA=fdfcabc1c156...`.
2. Restore rejection of tracked engine changes now that the work is committed.
3. Either reproducibly build the release binary from that clean checkout in
   the harness or compare it to a reviewed expected SHA-256.
4. Rerun twice and commit the exact disposition-set diff and artifact hashes.

## S-C — BREAK: the migration is partial

I compared the full original and applied `run.py` aggregation inventories.

| Original operation | Actual patched status |
|---|---|
| Family connectivity from partner/children | Connectivity moved, but Python still synthesizes every Person and relationship tuple from `scenario.partnered` and `scenario.children`. |
| Partner presence | Moved and consumed. |
| Total child count / youngest age | Moved and consumed. |
| Best Start age 0-2 band | Moved and consumed. |
| Counts age 0-13 / 14-18 | Engine computes them, but `run.py` never reads either name and the new test asserts neither. |
| Ten adult sums (wages, benefits, tax, net wage, hours, IETC) | Genuinely moved. |
| Zero-income benefit sum | Moved. |
| Family income child broadcast | Moved, though Python still evaluates each child program and marshals the results back. |
| Certified `BestStart_Total` family-once reduction | Moved. |
| Continuous Best Start family reduction | **Still Python.** |
| Eldest/subsequent child care units and child-shape gates | **Still Python.** |
| PTR gross family wage | **Still Python.** |
| Downstream net-income/program composition and EMTR recomposition | Still host-side, but outside the narrow person-to-family target. |

Concrete residuals:

- `continuous_total = max(0, child gross total - family abatement)` remains at
  [patched `run.py`:1956](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/run.py:1956).
  It feeds the Best Start continuous/EMTR residual evidence, so it is not inert
  diagnostics.
- Python still derives `eldest = child_count != 0` and
  `subsequent = max(0, child_count - 1)` at
  [`run.py`:1835](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/run.py:1835),
  plus `has_children`, `two_or_more`, entitlement-day, and sole-parent gates.
- The operational JSS branch still uses Python `youngest >= 14` at
  [`run.py`:1638](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/run.py:1638),
  rather than either advertised engine age-band count.
- PTR still forms gross family wages as
  `Decimal(wage) + scenario.gross_wage2` at
  [`run.py`:2588](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/run.py:2588).

This contradicts [`OUT.md`](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/OUT.md:20)
and generated report text claiming that Python performs none of the listed
reductions or family/child-shape calculations.

Required fix:

1. Add and consume an engine continuous Best Start reduction.
2. Carry the already-derived weekly family wage through the sweep and use it
   for PTR.
3. Derive eldest/subsequent care units and the actual JSS/AS/WFF age/shape
   predicates in the plan; remove the Python reductions.
4. Consume and mutation-test the 0-13/14-18 outputs, or delete them and narrow
   the certificate claim.
5. Add a static harness test forbidding `scenario.partnered`,
   `scenario.children`, and `scenario.gross_wage2` outside the single
   relationship/request-marshalling boundary.

## S-D — BREAK: the 6,082 defect is valid grammar

Reproduction:

```bash
target/release/axiom-rules-engine run-unit-aggregation \
  --plan stage3-nz/_scratch/agent-de/per_child_expressible.yaml \
  --enable-experimental-unit-derivation \
  < stage3-nz/_scratch/agent-de/per_child_request.json
```

The plan passes parser, validator, and executor with exit 0 and emits:

```text
best_start_total_before_abatement          = 8082
best_start_total                           = 6082
safe_best_start_gross                      = 8082
safe_best_start_total                      = 7082
unsafe_best_start_total                    = 6082
unsafe_post_abatement_via_family_once      = 6082
```

The valid plan expresses the old defect three ways:

1. A generic `all_members` scalar sum consumes child post-abatement outputs.
2. The nominal family-once reducer consumes gross 4,041 child values but an
   unconstrained caller-provided family value of 2,000, producing the canonical
   `8,082 -> 6,082` fields that the patched harness reads.
3. The same reducer consumes already-post-abated 3,041 child values and a zero
   once-value.

Inputs and selectors are arbitrary strings
([types](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:62));
generic sums accept any named value
([executor](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:515));
and the reducer trusts arbitrary child-carried scalars
([reducer](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:613)).
The validator protects only exact string names
([validation](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:259)),
which aliases bypass.

The committed regression test does genuinely call `expect_err`, but only after
replacing the sole enum token with an unknown YAML spelling
([test](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/tests.rs:1254)).
It proves a syntax blacklist, not dataflow inexpressibility.

Required fix:

1. Make reduction operands typed and scope/provenance checked: child gross
   credit and a genuinely Family-scoped abatement.
2. Never carry the family-once amount as an unconstrained per-child scalar.
3. Prevent generic aggregation from consuming post-abatement child outputs by
   typed provenance, not reserved names.
4. Commit the adversarial plan as a negative test; validation must reject all
   three 6,082 routes. Until then, narrow the claim to “the committed fixture
   chooses the safe operation.”

## S-E — BREAK: the stage-2 Unknown bug survives a derived gate

The probe at `stage3-nz/_scratch/agent-de/probe/` gives a known family `{a,b}`;
`b` has Unknown participating status and amount 20, while `a` has amount 10.

```text
direct_count = Indeterminate { reasons: {"bool:status_b"} }
direct_sum   = Indeterminate { reasons: {"bool:status_b"} }
gated_count  = Determined(Integer(1))
gated_sum    = Determined(Decimal(10))
```

Direct count and sum now hold. Gate-style aggregation does not: the reason
collector treats `JudgmentExpr::RelationMember` as a no-op
([`interface.rs`:261](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/interface.rs:261))
and follows only a derived relation's source, not its predicate
([`interface.rs`:295](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/interface.rs:295)).
Ordinary evaluation then keeps only `.is_holds()` candidates
([`engine.rs`:1105](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/engine.rs:1105)),
silently dropping `b`.

The new wrapper has a second Knowledge failure. `relations` defaults to an
empty map, while execution fabricates completeness for every plan relation
([`aggregation.rs`:424](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:424)).
Omitting partner and child relation families returned:

```text
partner_present = false
child_count     = 0
total_amount    = 30
```

Missing numeric scalar or child age instead aborts `InvalidPlan`; the request
and result types have no Knowledge representation.

Required fix:

1. Lift derived-relation predicates and materialize unresolved candidates with
   their reasons; reason collection must traverse `RelationMember` and the
   whole predicate.
2. Add Unknown and Conflict tests for direct and derived-filter counts/sums.
3. Require explicit per-relation fact evidence and completeness evidence in
   `AggregationRequest`; omitted completeness must be Unknown.
4. Represent scalars, ages, relations, broadcasts, and outputs as
   Knowledge/`CompleteReduction`; missing evidence must not become zero, false,
   or an untyped validation error.

## S-F — BREAK: deferred legal choices are not explicit cited inputs

Choice-by-choice result:

- **Family membership: partial only.** Relation names and plan-level citations
  exist, but facts carry no caller evidence/completeness. The engine manufactures
  both and therefore turns omission into Known absence.
- **Partner definition: fails.** The plan cites MB 2/MC 2, but the implementation
  hardcodes “a pair incident on `primary_person`”
  ([`aggregation.rs`:509](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:509));
  that anchor rule is neither a plan input nor cited. [MB 2](https://www.legislation.govt.nz/act/public/2007/0097/612.0/DLM1518457.html)
  is an income-period adjustment, while [MC 7](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518486.html)
  is the spouse/partner entitlement provision.
- **Direction/roles: fails.** `symmetric` is parsed but never used. All
  membership edges test both tuple orientations
  ([`aggregation.rs`:395](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:395)),
  while child identity always assumes tuple slot 1
  ([`aggregation.rs`:483](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:483)).
  Reversing `dependent_child_of` joined the same family but classified the
  primary adult as the child.
- **Age bands: mixed.** Best Start 0-2 is explicit and cited. The 14-18 count
  includes every age-18 child, but [MC 9](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518490.html)
  also requires financial dependence and school/tertiary attendance; neither input exists. The actual JSS age-14 branch
  remains host Python, and both advertised 0-13/14-18 outputs are dead.
- **Full/principal care: absent.** The plan grammar has no care role/fraction.
  Python reads uncited fixed `iwtc_child_exclusive_care_fraction=1` and
  `best_start_child_care_fraction=1` from
  [`eligibility-closures.json`](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/eligibility-closures.json:16)
  and injects the Best Start fraction in
  [`run.py`:1920](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/review-b/nz-lane/emtr_reproduction/run.py:1920).
  [MG 2(5)](https://www.legislation.govt.nz/act/public/2007/0097/latest/LMS63785.html)
  makes exclusive-care time result-affecting.
- **Adult selection: underdefined.** `selector: adults` is nominally explicit,
  but the engine silently defines “adult” as every member not appearing as a
  child
  ([`aggregation.rs`:501](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/unit_derivation/aggregation.rs:501)).
  That complement rule is not itself cited or configurable.

Required fix:

1. Replace the raw relation map with required relation-family inputs carrying
   facts, fact evidence, and explicit completeness evidence.
2. Add cited role semantics: tuple direction, caregiver/child roles, adult
   selection, and the partner-presence anchor. Implement `symmetric` or remove
   it.
3. Obtain legal review for the partner authorities; do not use MB 2 as the
   partner-definition citation.
4. Add per-child principal/exclusive-care facts or fractions cited to the
   applicable MC/MG provisions, consume them in membership/counts/Best Start,
   and remove host `care_fraction=1` defaults.
5. Add financial-dependence and education inputs for age 18, and encode the
   actual JSS youngest-age predicate with the applicable Social Security Act
   authority.
6. Add mutation tests for every direction, age, partner, completeness, and care
   input.

## S-G — BREAK: v2 is stable, but the aggregation path is not contained

Subclaims that hold:

- `schemas/compiled-artifact.v2.schema.json`, `src/schema.rs`, lowering/model
  files, and RuleSpec parsing files have identical Git blobs between parent and
  target.
- `cargo build` with defaults succeeds.
- `cargo test --features schema`: **299 passed**.
- `cargo test --all-features`: **324 passed**.
- Cargo changes are only optional `num-bigint` feature wiring and the matching
  root-package lock entry. Default `cargo tree` omits `num-bigint`.
- The membership constitution itself calls the repaired stage-2 `compile` and
  `derive_units`, so there is no exact recurrence of the old derived-ID
  collision within that narrow step.

The required common path does not hold. The CLI reads raw YAML with
`parse_aggregation_plan`, deserializes raw JSON, and calls the sidecar directly
([`main.rs`:306](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/main.rs:306)).
It never loads `CompiledProgramArtifact` or calls
`materialize_phase_two_dataset`; scalar sums/counts/broadcasts/reductions occur
after unit derivation over raw `BTreeMap`s. The fixture test constructs the raw
request directly. The sidecar schema is not an emitted/golden schema, and no
canonical digest covers its aggregation operations or citations.

There is also a new identity/provenance defect. Execution compiles one
`EdgeRule` for every request roster pair. Adding one unrelated roster person
changed the ID of the unchanged connected family:

```text
base family ID            Family:sha256:dc60eb11...e9742fc
with unrelated person     Family:sha256:1c0a47e0...fc91fa
members in both           [possible-partner-or-child, primary]
trace root in both         sha256:a592215a...6fcf6ee
```

Request data therefore changes the constitution semantics digest/Family ID,
while the reported trace root fails to distinguish those different requests
and results. Parity ignores unit IDs, so the pinned number cannot detect this.

Required fix:

1. Introduce a compiled, canonically digested experimental aggregation-plan
   object covering all operations, types, scopes, and citations.
2. Route its complete entity/fact universe through the typed binder,
   materialization barrier, and Knowledge-aware relation indexes used by the
   production path.
3. Compile plan semantics independently of a request roster; unrelated data
   must not churn an unchanged family's ID. Bind the request/facts into the
   trace root.
4. Add CLI-level schema, collision, identity-stability, trace-binding, and
   malformed/Unknown input tests.
5. Remove 15 trailing-whitespace errors in `ops-harness.patch`; currently
   `git diff --check 7488151 fdfcabc` fails.

## S-H — BREAK: honest counts, inadequate assertions

Reproduced exactly:

```text
cargo test                              284 passed, 0 failed, 0 ignored
cargo test --features unit-derivation   309 passed, 0 failed, 0 ignored
```

The +25 is misleading as stage-3 coverage: enabling the feature exposes the
whole 25-test unit-derivation module. This commit adds only two tests.

The weakest three relevant tests are:

1. `nz_best_start_host_defect_reproduces_but_plan_has_no_per_child_abatement_operator`:
   independent decimal arithmetic plus rejection of one unknown enum spelling.
   It does not execute any adversarial valid grammar; the accepted 6,082 plan
   disproves its name.
2. `nz_family_fixture_moves_person_child_aggregation_behind_the_barrier`: one
   all-known partnered happy path. It asserts only three of five counts and four
   of twelve family scalars; no age boundaries, unpartnered/multi-family cases,
   Unknown/Conflict, direction, inconsistent abatement, exact members/result
   metadata, CLI JSON path, or meaningful half-even midpoint exists. It also
   leaves both advertised age-band counts unasserted.
3. Existing `top_usage_lists_every_dispatched_command`: its hardcoded list
   omits `run-unit-aggregation`, exactly like `TOP_USAGE`, so it passes while
   violating its documented exhaustive invariant
   ([`main.rs`:90](/Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/src/main.rs:90)).
   The feature-on binary dispatches the command at line 51, but top-level help
   does not list it.

Required fix:

1. Add `run-unit-aggregation` to feature-on top help and derive/test the command
   inventory from the dispatcher rather than a second incomplete hardcoded
   list.
2. Add a CLI integration test for runtime-gate failure and exact fixture JSON.
3. Table-test all outputs and ages 0/2/13/14/18; unpartnered and multiple
   families; reversed roles; missing/Unknown/Conflict; inconsistent abatements;
   decimal half-even boundaries; and trace/identity binding.
4. Commit the S-D and S-E probes as regression tests.

## Merge-blocking fix order

1. **Restore the ratified algebra first:** fix derived-predicate Knowledge
   lifting and make completeness/Unknown representable throughout the new
   aggregation request and result.
2. **Contain and bind the sidecar:** compile/digest the full typed plan, use the
   production registry/materialization path, stabilize IDs, and bind traces to
   request facts.
3. **Make Best Start safety structural:** Family-scoped abatement, typed child
   gross operands, no arbitrary post-abatement scalar aliases; reject the
   accepted 6,082 fixture.
4. **Finish the migration:** move continuous Best Start, child-shape/JSS gates,
   and PTR wages out of Python; consume or remove dead age bands.
5. **Resolve legal inputs explicitly:** cited relationship roles,
   completeness, partner anchor, care fractions, and age-18 conditions; no
   host defaults.
6. **Repair the proof:** pin `fdfcabc`, require a clean checkout, bind or rebuild
   the binary, rerun parity/determinism, and retain the exact 522-ID diff.
7. **Expand honest tests and hygiene:** adversarial CLI/semantic coverage,
   discoverable help, and a clean `git diff --check`.

Until all seven are complete and re-reviewed, the parity number demonstrates
only that this particular sidecar-plus-Python pipeline can reproduce the
pinned CSV. It does not demonstrate engine-native aggregation under the
ratified algebra or make the Best Start defect structurally inexpressible.
