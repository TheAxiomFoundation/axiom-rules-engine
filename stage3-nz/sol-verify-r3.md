# R3 independent verification audit of the stage-3 cure round

Audit target: `feat/unit-derivation-stage3-nz` at
`d32cf980bdd5a3b7a7e582604dffa90687784b69`, with reviewed base
`fdfcabc` and implementation commit
`d8d45a817b3700ed22ec49a9fcf1024aa6ed2198`.

## Overall verdict: NOT-CLEAR

| Finding | Verdict | Result |
| --- | --- | --- |
| 1. Family-scope reduction uniqueness | **NOT-RESOLVED** | The named rejection and mutant suite pass, but input provenance is still author-asserted and forgeable. A relabelled post-abatement child field compiles and produces a determined doubled family amount. The committed plan also is not the finding's byte-exact plan. |
| 2. Unknown/Conflict preservation | **NOT-RESOLVED** | The five named probes pass and the complete family membership is preserved, but a new mixed Conflict-care/Unknown-age probe returns top-level `families.status = determined`, contrary to the requested family-level Indeterminate result. |
| 3. Migration completeness | **RESOLVED** | Every person/child-to-family reduction not on the disclosed residual list crosses the single `aggregate_family` boundary. |
| 4. Harness binding | **RESOLVED** | Commit and binary digest pins are correct. Dirty-tree and unbound-binary guard paths both refuse, with the messages recorded below. |
| 5. Registry path | **RESOLVED** | The plan uses the normal validated registry/artifact path; no runtime sidecar plan path is reachable. |
| 6. Citation accuracy | **NOT-RESOLVED** | Several aliases name the wrong MC 7, MC 9, or MC 10 provision even though the mechanics and citation-string test pass. |
| 7. Test-suite claims | **RESOLVED** | All claimed suite totals, the byte-identical flag-off artifact, harness mutants, and the fresh end-to-end parity result reproduced. |

The merge is blocked by findings 1, 2, and 6.

## Audit basis and scope

I read `sol-review-q.md`, then `sol-stage3-fixes.md`, then reviewed
`git diff fdfcabc..d32cf980`. `git diff --check` was clean. I treated the
original finding language as the acceptance criterion where the cure report
narrowed or paraphrased it.

For the ops comparison I reconstructed the files from the read-only repository
with `git -C /Users/maxghenis/TheAxiomFoundation/ops show
bcf631b5:<path>` into `stage3-nz/_scratch/`, then applied the committed patch to
that copy. I made no ops-repository changes, push, or PR.

Audit-process exception: creating a detached validation worktree at `d8d45a81`
caused Git to materialize the repository-tracked file
`stage3-nz/_scratch/r3-root/engine-d8d45/PROGRESS.md`. This was a breach of the
instruction not to create a `PROGRESS.md`. Once discovered, I did not inspect,
edit, or delete that file, and left it in place rather than perform another
prohibited operation.

## 1. Family-scope reduction uniqueness — NOT-RESOLVED

### Checks that passed

The exact named tests passed one test each:

```text
unit_derivation::tests::reviewer_best_start_plan_is_rejected_by_named_family_scope_guard
unit_derivation::tests::every_stage3_structural_guard_kills_its_isolated_plan_mutant
```

The rejection test requires the named error and its fields, not merely an
arbitrary parse or validation failure:

```text
DuplicateFamilyScopeReduction {
  key: "untyped-family-scope",
  first: "safe_best_start_total",
  second: "best_start_total"
}
```

The mutant test changed one structural property at a time and rejected all 11
variants: selectors, aliases, input scopes, reduction keys, input kinds, care
references, relation direction, relation roles, projection references,
predicate references, and citations.

After extracting the original finding's fenced fixtures verbatim, the committed
request is byte-identical to the finding request:

```text
f2226a900f98e9951bcdcbdf83e26cc03dc1cac885b6ddc7e24e12baaf6dd1cd
```

The committed plan is not byte-identical to the finding's plan:

```text
finding plan   39011e1c1f512d98c969b951ff91420d4996e18ccdea6770762611d51b49867d
committed plan 2940a8e784b9654c38516a2e12751baf4d2d799ec6d2b5f3d53d9b81fba43139
```

The difference is the removal of these two original comments:

```text
# Even the sole nominally-safe reducer accepts a post-abatement child field
# and a caller-supplied zero as its "family once" field.
```

That comment-only difference would not itself revive the exploit, but it means
the requested exact-fixture confirmation is false.

### Live accepted-plan reproduction

The deeper type-system property is not enforced. `AggregationInput` stores
`name`, `scope`, `kind`, and `reduction_key` as plan-author declarations
(`src/unit_derivation/aggregation.rs:90-96`). The validator checks those tags
(`:1198-1213`), but the executor then fetches the request scalar solely by its
declared name (`:2855-2871`). Nothing binds `ChildGrossAmount` to a certified
upstream computation.

I mutated the valid NZ plan by globally renaming
`best_start_before_care_and_abatement` to
`best_start_after_per_child_abatement`, while retaining child scope,
`child_gross_amount`, and reduction key `best_start`. The request supplied
`3041` for each of two children and family adjustment `0`.

Both compile and execution returned exit 0:

```text
plan_digest: sha256:372f53c2eff1740fc4ff7e2f9ceb5776df31ac88791381cfc02cef088fe1e0db
members: [child-0, child-1, partner, primary]
best_start_total: { status: determined, value: "6082" }
gross:            { status: determined, value: "6082" }
```

This is an accepted plan document routing author-labelled post-abatement values
into a family aggregate. The single operation and one-consumer-per-key rules
therefore establish nominal uniqueness only; they do not establish provenance
or semantic uniqueness.

### Required cure

- Bind the child-gross operand to non-forgeable upstream computation provenance,
  rather than trusting a plan-authored kind tag and request field name.
- Add the relabelled post-abatement plan above as a negative regression.
- Commit the original finding plan byte-for-byte if the fixture is claimed to
  be exact.

## 2. Unknown/Conflict preservation — NOT-RESOLVED

### Named probes and stage-2 compatibility

All five requested probes passed individually:

```text
lifted_derived_relation_never_drops_unknown_or_conflict
unknown_and_conflict_survive_derived_relation_gates_for_counts_and_sums
indeterminate_relation_knowledge_cannot_produce_a_smaller_determined_family
omitted_relation_is_unknown_but_evidenced_complete_empty_is_known_empty
unknown_status_survives_materialization_and_indeterminates_phase_two_count
```

Source inspection confirms that `RelationMember` now collects unresolved
relation reasons, derived-relation traversal visits both its source and
predicate, and Unknown/Conflict candidates remain visible to direct and gated
counts and sums.

Flag-off behavior remained identical:

```text
cargo test --locked: 285 passed, 0 failed
parent artifact SHA-256:  355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2
current artifact SHA-256: 355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2
cmp: byte-identical
```

The changed stage-2 module remains gated behind `unit-derivation`.

### Additional mixed-evidence probe

I authored and reran
`stage3-nz/_scratch/r3-agent-knowledge/mixed_evidence_probe.sh`
(SHA-256
`615dc02c348111186330a01091dd655e0df8b30281e321c9db6ec30be0ea8ec3`).
It combines evidence states not paired by the five probes:

- `child-0.best_start_claimant_care_fraction`: Conflict between `1` and
  `0.5`;
- `child-1.age_years`: Unknown.

The member topology remains complete, and the affected values correctly carry
their reasons:

```text
members: [child-0, child-1, partner, primary]
care_projection: Indeterminate (both care-conflict reasons)
best_start_total: Indeterminate (both care-conflict reasons and Unknown age)
dependent_child_count: Indeterminate (Unknown age)
youngest_child_age: Indeterminate (Unknown age)
```

However, the family envelope is still:

```text
families_status: determined
```

The probe therefore exits 1 with:

```text
FAIL: mixed Conflict care + Unknown age returned top-level
families.status=determined; expected indeterminate
```

This no longer produces a smaller Determined family, but it does not meet the
brief's explicit requirement that the mixed family result itself be
Indeterminate.

### Required cure

Propagate unresolved scalar/age evidence to a family-level Knowledge status
while retaining the complete member topology (or add an equivalent explicit
family-level status), and commit the mixed probe as a regression.

## 3. Migration completeness — RESOLVED

I reconstructed `nz-lane/emtr_reproduction/run.py` at ops commit `bcf631b5`
and applied `stage3-nz/ops-harness.patch` to the scratch copy.

```text
base run.py SHA-256:    9aa0fc64af8dca4a8f7574e98923fe0022561679027c2ed5325bf381e9c6ab27
patched run.py SHA-256: 04edce52c75fa39c8ecf28399b944d3e8114864e9ff84d872bfd698f8793121a
```

Their corresponding Git blob IDs are
`d5f5d724ab4954e4f4a91592edc9339db245bb9b` and
`1fcff159e35d85eca9f7c70656ce1d89fdd28ba4`.

There is one `ModelEvaluator.aggregate_family` request-marshalling boundary.
All operational reads of `scenario.partnered`, `scenario.children`, and
`scenario.gross_wage2` occur inside it. Reads elsewhere are limited to pinned
profile validation, display/reporting, and serialization.

Every family discovery/classification/reduction from the original harness now
crosses that boundary, including partner presence, child count/youngest/age
bands, ten adult sums, zero-income benefit, family broadcast, discrete and
continuous Best Start, child-shape/JSS/AS predicates, IWTC care, and PTR wages.
The observed call sites cover shape caching, Best Start projection/reduction,
zero-income benefit, wages, adult benefit/tax/net/hour aggregation, IETC, and
the final Best Start result.

The remaining host-side list is exactly the cure report's list:

- construction of pinned Treasury scenarios and evidence-bearing requests;
- evaluation of each statute program for the proper Person, Child, or Family
  entity;
- ordering calls where one program's already aggregated output feeds another;
- downstream cross-program arithmetic (WFF, Net Income, the `$1` EMTR forward
  difference, RR/PTR, and diagnostics);
- the continuous-WFF diagnostic over already family-level values; and
- disclosed Treasury raw-source conventions, benefit gross-up, and
  Accommodation Supplement diagnostic mechanics.

None performs member discovery, person/child classification, or a
person/child-to-family reduction. The 14 harness mutants passed, and three
independently injected forbidden-read mutants were each detected.

## 4. Harness binding — RESOLVED

The regenerated patch has SHA-256
`9a98f3def4f4a734d69ca9563ed3bb09ab90a3a2b36c7ef68ea87cd39a0d6bab`
and pins:

```text
EXPECTED_ENGINE_SHA = d8d45a817b3700ed22ec49a9fcf1024aa6ed2198
EXPECTED_ENGINE_BINARY_SHA256 = c5a3d3d8b1fdb8480bef7cffc6dd85658ca1a48cf7ca70da968b7a46c1e8a430
```

An independent clean release build at `d8d45a81` reproduced that binary digest.
The positive binding check returned the exact commit and digest with an empty
`git status --porcelain`.

The two required negative guard paths refused as follows.

Dirty working tree:

```text
engine checkout has tracked or untracked modifications: /Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/r3-agent-harness/guard-probes/dirty-engine
```

Its backing status was ` M target/release/axiom-rules-engine`.

Binary outside the pinned checkout:

```text
engine binary must be the release binary under the pinned engine checkout: expected /Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/r3-agent-harness/guard-probes/mismatch-engine/target/release/axiom-rules-engine, found /Users/maxghenis/TheAxiomFoundation/_worktrees/engine-stage3-nz/stage3-nz/_scratch/r3-agent-harness/guard-probes/outside/axiom-rules-engine
```

I also exercised the content guard independently:

```text
engine binary SHA-256 mismatch: expected 0000000000000000000000000000000000000000000000000000000000000000, found bdb66e892d64be546c733836e37f400f3aef88afb9e184284804ea19f555c69b
```

In the patched copy, `verify_inputs` invokes `verify_engine_binding`; the
provenance output records the expected commit, digest, and cleanliness scope.

## 5. Registry path — RESOLVED

The aggregation document follows the same validated registry path as the
production artifacts:

1. Authoring input is compiled through `register_aggregation_source`, which
   validates the plan and emits its plan, constitution, and phase artifacts.
2. A compiled artifact is loaded through `register_aggregation_json`; its
   digests and compiled phase artifact are recomputed/validated, and the
   embedded v2 program goes through ordinary
   `CompiledProgramArtifact::from_json_str` loading.
3. Runtime registry execution revalidates, binds the dataset, materializes
   phase two with Knowledge, and executes through the typed entity registry.

The direct compile/execute helpers are crate-private and exposed only to tests.
There is no alternate `parse_aggregation_plan` runtime route. A sidecar attempt
was refused:

```text
run-unit-aggregation --plan ...
unknown run-unit-aggregation argument '--plan'
```

Targeted registry, CLI, digest-tamper, and schema-fixture tests passed. I found
no reachable production sidecar load path.

## 6. Citation accuracy — NOT-RESOLVED

I checked the resolved fixture citations and mechanics against the current
official legislation text for [MC 7](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518486.html),
[MC 9](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518490.html),
[MC 10](https://www.legislation.govt.nz/act/public/2007/0097/latest/DLM1518492.html),
and [MG 2](https://www.legislation.govt.nz/act/public/2007/0097/latest/LMS63785.html).

### MC 9

MC 9(1)(a) is financial independence; MC 9(1)(b) is school/tertiary
attendance; MC 9(2)-(3) govern the Commissioner-determined period. These rules
apply specifically at age 18.

The three direct inputs at
`tests/fixtures/unit_derivation/nz_income_explorer_family.yaml:96-113` cite
those provisions correctly. The `&mc9` alias, however, names only MC 9(1)(a)
and is wrongly reused for:

- the composite `age_18_conditions` (`:163-168`), which also consumes (1)(b)
  and (2)-(3);
- `child_count_age_14_18` (`:199-202`), though MC 9 covers age 18, not the
  whole 14-18 band, and (1)(a) alone does not establish even the age-18 case;
- `mc9-commissioner-period` (`:286-288`), whose authority is MC 9(2)-(3), not
  (1)(a).

These are citations to the wrong provision.

### MC 10

MC 10(3) establishes the IWTC principal-caregiver qualification and one-third
exclusive-care threshold. MC 10(4) says that, when subsection (3) applies, the
MD 10(3) calculation applies only for periods in which that caregiver has
exclusive care.

The `&mc10` alias at fixture lines 80-85 names only MC 10(3). It is accurate
for `in_work_tax_credit_principal_caregiver`, but is wrongly reused for:

- `in_work_tax_credit_child_exclusive_care_fraction` (`:80-85`);
- the child agreement (`:250-255`); and
- `mc10-iwtc-care` (`:295-297`), which itself calls the fraction an MC 10(3)
  fraction.

The fraction/period mechanic requires MC 10(4) together with the applicable
MD 10 calculation provision.

The broader caller-evidenced dependent-child and adult-role classification
citations do not attribute their entire legal tests to MC 9 or MC 10 and did
not reveal an additional narrow-provision mismatch.

### MC 7

MC 7 applies only when both partners independently meet MC 3-MC 5 for
different child sets; MC 7(2) then combines the children and, except for IWTC,
requires Commissioner selection. The generic partner relation's cautious
"used by MC 7" wording is supportable. `partner_presence` is not: it merely
tests whether a caller-supplied holder is incident to a partner pair and does
not represent the MC 7(1)(a)-(c) applicability conditions. The patched harness
also supplies its purported MC 7 holder for unpartnered families. MC 7 does
not accurately support that generic partner-presence mechanic.

### MG 2

MG 2(5) requires proportional reduction for time a child spends in another
qualifying person's exclusive care. The fixture's claimant-care input,
projection, and limitation cite MG 2(5) accurately. Execution multiplies each
eligible child's gross amount by the care share exactly once before the family
adjustment. The gross amount, under-three child count, and pinned full-period
day-shape uses of the broader MG 1/MG 2 citation are consistent with the
amount, age, and day mechanics, with the full-period scenario limitation made
explicit. I found no MG 2 citation defect.

The existing legal test passes, but it tests the literal committed strings
(including the incorrect MC 10(3) fraction string), not whether each string
names the provision supporting the mechanic.

### Required cure

- Split the MC 9 aliases and cite each composite mechanic to all applicable
  subsections; give ages 14-17 their actual authority and use full MC 9
  conditions only for age 18.
- Keep principal-caregiver qualification at MC 10(3), but cite exclusive-care
  period/fraction mechanics to MC 10(4) and the applicable MD 10 calculation.
- Either model MC 7 applicability and selection as explicit Knowledge-valued
  facts or stop presenting generic partner presence as MC 7 entitlement.
- Regenerate the compiled fixture, expected output, and bound harness artifacts
  after correcting the source plan.

## 7. Test-suite claims — RESOLVED

Fresh local reruns produced the claimed totals:

```text
cargo test --locked                              285 passed; 0 failed
cargo test --locked --features unit-derivation   328 passed; 0 failed
cargo test --locked --all-features               344 passed; 0 failed
python3 -m pytest -q test_declared_closures.py     14 passed; 0 failed
```

The last command ran in the reconstructed, patched harness copy.

For the flag-off control, independently compiled current and reviewed-parent
release binaries produced byte-identical real NZ artifacts:

```text
SHA-256 355659bf30cef49c440d5fa24014f1bf6c9a17964befb164880196950462f3b2
artifact format: 2
derived relations: 176
```

Finally, I reran parity end-to-end from the patched harness copy using explicit
engine, RuleSpec, Treasury, composition, plan, closure, and generator paths.
The harness itself performed two deterministic passes. The result reproduced:

```text
agreement cells:       1,454 / 1,976
outside-cent cells:      522
disposition IDs:         522, identical to the prior set
comparison.csv SHA-256: ccaa4dcb61b112587b47afb0e1892f670df354670fcd35f4d801edc621dd4bf2
aggregation artifact:   dc1d368844e071c17649cd463568eca5e1d82eacf822694cce0ac45f1882fc41
```

The fresh CSV is byte-identical to both prior recorded parity runs. An
independent set comparison found no added or missing disposition ID. Recorded
provenance also matches the pinned implementation commit and release-binary
digest, with the engine checkout reported clean.

## Exact remaining work before merge

1. Replace author-asserted `ChildGrossAmount` provenance with a binding the
   plan author cannot relabel, reject the post-abatement reproduction, and use
   the finding's byte-exact plan fixture.
2. Make mixed Conflict care plus Unknown age yield an Indeterminate family-level
   result without losing members, and add that exact regression.
3. Correct the MC 7, MC 9, and MC 10 citations and modelling described above,
   then regenerate and rebind every derived fixture/harness artifact.
4. Rerun the full flag-off, feature-on, all-features, 14-mutant, and parity
   matrix after those changes.
