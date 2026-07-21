# Bitemporal semantics: valid time and assessment time

The engine currently has one time axis. Parameter and derived rules carry
`versions[]` with `effective_from` and optional inclusive `effective_to`, and
the query's `period` selects whichever version is live for the period being
calculated. That single axis answers one
question — "what does the law say about this benefit period?" — while silently
assuming a second answer: that the assessor knows every enactment that will
ever touch that period.

Real eligibility systems cannot make that assumption. A determination is made
*at a moment in time*, under the law *as known at that moment*. Retroactive
amendments, corrections, appeals, and retro-certification all separate the
period a determination covers from the date the determination is made. This
document defines the two axes, reserves the schema and query surface for the
second one, and states exactly what is and is not implemented today.

## The two axes

- **Valid time** — the benefit period the law governs. This is the existing
  axis: the query's `period`, matched against each version's effective range.
  It answers: *which version of the rule was in force for the period being
  calculated?*
- **Decision/assessment time** — when the determination is made. This is the
  new axis: the query's `assessment_date`, matched against each version's
  enactment/knowledge date. It answers: *which enactments were visible to the
  assessor when the determination was made?*

| | Valid time | Assessment time |
|---|---|---|
| Query field | `period` | `assessment_date` |
| Version field | `effective_from` / `effective_to` | `enacted_on` / `known_from` |
| Question | law in force during the period | law as known at determination |

Today the engine conflates the two: selecting a version by
`effective_from <= period.start` implicitly evaluates every query as an
omniscient, end-of-history assessment. For a single point-in-time snapshot of
the law that is correct. The moment an encoding contains a retroactive
enactment — two versions claiming the same valid time, enacted at different
moments — the single axis cannot say which one a March determination should
have used.

## Why eligibility systems need both

**Amendments with retroactive effective dates.** The American Rescue Plan Act
(Pub. L. 117-2) was enacted on 2021-03-11 and excluded up to $10,200 of
unemployment compensation from federal taxable income *for tax year 2020* —
an effective date more than fourteen months before enactment. A tax-year-2020
determination made on 2021-02-15 (an early filer) correctly applied pre-ARPA
law; the same period determined on 2021-04-15 correctly applied the
exclusion. Same `period`, different `assessment_date`, both determinations
right when made. Encoded bitemporally, the new version carries
`effective_from: 2020-01-01` and `enacted_on: 2021-03-11`.

**Retroactive COLA corrections.** SNAP maximum allotments adjust each October
1 (7 U.S.C. 2012(u), 2017(a)); FNS publishes the tables in a summer COLA
memo. Suppose FNS issues a December correction revising an allotment cell,
retroactive to October 1. The encoding then holds two versions with the same
`effective_from: <fy>-10-01` and different enactment dates. A November
determination used the original cell; a January re-determination of that same
November month uses the corrected one. Today the engine cannot even represent
this case faithfully: among versions sharing an `effective_from`, selection
falls through to document order (`max_by_key` keeps the last maximum), which
is an accident of file layout rather than a statement about enactment. The
assessment axis turns that tie-break into a principled rule: the
latest-enacted version *visible at the assessment date* wins.

**SNAP retro-certification.** Initial-month benefits run from the date of
application (7 CFR 273.10(a)(1)(ii)), and certification may lawfully complete
up to 30 days later (7 CFR 273.2(g)). An April certification of a household
that applied March 30 computes the March allotment: valid time is March,
assessment time is April. Restored benefits reach further — agency errors and
fair-hearing reversals are corrected with up to twelve months of restoration
(7 CFR 273.17, 273.15), so a determination made in June routinely computes a
benefit month from the previous year, under that month's rules, including any
enactments that retroactively reached it in the meantime.

**Appeals and audits.** Reproducing what a caseworker *should have decided*
on a given date requires pinning both axes: the benefit period under review
and the assessment date of the original decision. Quality-control reviews
(7 CFR 275) ask exactly this question. Without assessment time, an encoding
that has since absorbed a retroactive amendment can no longer reproduce the
original, correct-when-made determination.

## How RuleSpec grows

Versions gain two optional dates alongside `effective_from`:

```yaml
rules:
  - name: unemployment_compensation_exclusion_cap
    kind: parameter
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2020-01-01
        enacted_on: 2021-03-11
        formula: "10200"
```

- `enacted_on` — when the legal act creating this version was enacted or
  promulgated. This is both provenance and the default visibility date for
  assessment-time selection.
- `known_from` — when the version became visible to the assessing system, if
  that differs from enactment. State manuals routinely publish federal changes
  weeks after the federal notice; `known_from` lets an encoding model the
  administrative lag without misstating the enactment date. When present it
  overrides `enacted_on` for selection only.

A version's **visibility date** is `known_from`, else `enacted_on`, else
negative infinity. Both fields apply to `parameter` and `derived` versions
alike. They are RuleSpec authoring fields first; `ProgramSpec` and the
compiled artifact grow matching fields only when the engine starts honoring
them.

## Query semantics

`assessment_date` is an optional field on `ExecutionQuery`, available on both
the direct and the compiled request paths, with one value per query (it
scopes the whole determination, not individual outputs). The two fields
divide the work:

- `assessment_date` selects which enactments are **visible**: versions whose
  visibility date is on or before the assessment date.
- `period` selects which visible version **applies**: the latest
  `effective_from` at or before `period.start`, exactly as today.

Normatively, once implemented:

1. visible = versions where `visibility_date <= assessment_date`
2. applicable = visible where `effective_from <= period.start` and
   (`effective_to` is absent or `period.start <= effective_to`)
3. select the maximum by (`effective_from`, then visibility date, then
   document order)

An omitted `assessment_date` means every version is visible — the law as
currently encoded, which is today's behavior unchanged and the right default
for policy analysis and microsimulation.

`assessment_date` must be on or after `period.start`. Assessing a period
before it begins is a projection, not a determination — prospective
certifications are modeled as assessments made when each benefit month is
issued — and projection semantics (with their own questions, such as
forecasting indexed parameters) are deliberately out of scope. The constraint
is a validation, so relaxing it later is backward compatible.

Each `QueryResult` echoes the query's `assessment_date`, so a stored result
remains self-describing about the assessment it was computed under.

**Implemented today:** the field is parsed, validated against `period.start`,
and echoed (`src/api.rs`; mirrored in
`python/axiom_rules_engine/models.py`). It has **no effect on evaluation**.
Version selection still considers every version
(`src/engine.rs`, `src/bulk.rs`, `src/dense.rs` all select by
`effective_from` only). The point of reserving the field now is that requests,
stored results, and calling code can start carrying assessment dates before
any encoding depends on the semantics.

## Migration story

Absent fields mean "known since forever": a version without `enacted_on` or
`known_from` has a visibility date of negative infinity, is visible at every
assessment date, and the selection rule above reduces exactly to today's
`effective_from` rule — with or without an `assessment_date` on the query.
Every existing encoding, request, artifact, and response is therefore
unaffected:

- Requests: `assessment_date` is optional and omitted from the wire when
  unset, so requests that do not use it are byte-identical to before.
- Responses: the echo is likewise omitted when unset.
- Encodings: `enacted_on`/`known_from` are optional; existing rule files are
  valid and mean what they always meant.
- Compiled artifacts: today's engines ignore unknown fields, so an artifact
  carrying visibility dates would *silently evaluate omnisciently* on an old
  engine. When the engine starts honoring visibility dates,
  `ARTIFACT_FORMAT_VERSION` must bump so older engines reject such artifacts
  loudly (see the 2026-06-09 decision in `DECISIONS.md`).

One known implementation consequence for later: the evaluation cache keys on
(derived, entity, period). Honoring assessment time adds a dimension —
results may differ across assessment dates for the same period — so the cache
key (or the engine instance scope) must grow with it.

## Out of scope now

- **Honoring `assessment_date` in version selection.** The field is reserved,
  validated, and echoed only. Selection semantics land together with
  `enacted_on`/`known_from` support in the loader, IR, and all three
  execution paths.
- **A retro recalculation engine.** Detecting that a retroactive enactment
  changes past determinations, finding affected cases, and re-running them is
  workflow built on top of the engine. The engine stays a pure function of
  (rules, data, period, assessment date).
- **A cross-period corrections ledger.** Computing underpayment/overpayment
  deltas between an original and a corrected determination, claims
  establishment, and recoupment (7 CFR 273.18) are downstream products of
  running the same period at two assessment dates — not engine features.
- **Knowledge time for data.** `assessment_date` governs visibility of *law*.
  What evidence the assessor had is already the caller's responsibility: the
  dataset is caller-supplied, so callers reconstruct the evidence as-of the
  assessment themselves. `InputRecord` does not grow a `known_from`.
- **Projection.** `assessment_date < period.start` stays an error.
- **Per-output assessment overrides.** One assessment date per query.
