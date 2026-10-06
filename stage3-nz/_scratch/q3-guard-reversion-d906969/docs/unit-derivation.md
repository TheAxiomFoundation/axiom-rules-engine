# Unit derivation: semantics RFC

**Status: RFC v2 — semantics only, no implementation.** This document is stage 1
of [#134](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/134):
the contract a `kind: unit` rule must satisfy before any code exists. Nothing in
it changes the release line. Supplied membership remains the default and
continues to work; derivation, when it eventually exists, is opt-in per program
and off by default. The concrete case worked throughout is 7 CFR 273.1 (SNAP
household composition), from the eCFR text as of 2026-07-01, with
7 CFR 273.11(c) read for the excluded-member treatment it prescribes.

v2 incorporates a cross-model audit (gpt-5.6-sol, 2026-07-29, findings S1–S23)
adjudicated finding-by-finding; the substantive corrections are the attachment
and independent-participation-bar operations, per-person/per-projection
determination, dependency-closure locality, scoped relation-family
completeness, world-relative pipeline evaluation with simultaneous cuts, and a
full ratification matrix replacing the earlier four-decision list.

Related: [#118](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/118)
(relation absence silently became a $0 denial),
[#83](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/83)
(unknown identifiers silently become inputs),
[#125](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/125)
(artifact format identities), and the 2026-07-27 from-scratch design documents,
whose semantics this contract is written to survive.

## 1. The problem

RuleSpec can encode what a household gets, but not what a household is.
Entities and their membership relations arrive as dataset inputs. The law that
constitutes them — 7 CFR 273.1's purchase-and-prepare rules for SNAP, filing
units for tax, benefit units elsewhere — cannot be encoded, so every consumer
computes unit membership outside the engine, in uncited, unversioned code. The
single most contested determination in benefits administration happens outside
the audited, traced machinery.

What exists today is predicate-*filtered* membership: a `derived_relation` is a
filtered view over a source relation's candidate tuples (`docs/rulespec.md`),
where the corpus convention is a supplied all-member relation
(`member_of_household`) filtered into an eligible view (`snap_unit`). What does
not exist is *partition*: given N persons and person-level facts, produce the
Household entities and their membership tuples, where the number of units is
not pre-given. There is no rule kind whose output is a set of new entity
instances, and the evaluator builds its relation index once over supplied
records — a fixed population.

Issue #118 was, at bottom, a membership-supply failure: composition was data
the caller had to get right rather than law the engine could derive and cite.
This RFC defines the semantics under which the engine could derive it.

## 2. Vocabulary

- **Universe** — the persons among whom units are formed, given by a roster
  relation (for SNAP: the co-residents of a dwelling) plus person-level and
  person-pair facts.
- **Constitution rules** — the rules that produce units, in seven operation
  kinds: base grouping, combination (merge) edges, separations (cuts),
  attachments, independent-participation bars, status tags, and discretion
  hooks.
- **Base grouping facts** — the assertions the general definition runs on (for
  SNAP: purchase-and-prepare assertions; exact statutory phrasing in § 3).
- **Combination (merge) edge** — statutorily forced co-membership regardless
  of base facts (spouses; under-22 children with a parent).
- **Separation (cut)** — a statutorily licensed removal of a person-set from
  its group into its own unit (the elderly-disabled exception).
- **Attachment** — a statutorily directed addition of a person or group to
  another candidate unit (a below-threshold board payer joins the providing
  household; a requested boarder joins at the provider's election).
- **Independent-participation bar** — a rule that a person or group may not
  form a participating unit of their own (reasonable-paying boarders, foster
  placements), distinct from exclusion within a unit.
- **Status tag** — a per-person determination that changes participation, not
  composition (the § 273.1(b)(7) exclusions).
- **Election / request** — a fact recording that a permissive provision was
  invoked, carrying a declared *actor* (for board and foster attachment, the
  actor is the providing household — a candidate unit, § 4.3).
- **Candidate unit** — a unit produced by the composition stages before
  attachment/classification rules that reference candidate units run.
- **Projection** — a named view of membership. This contract distinguishes at
  minimum `unit_constituent` (composition/association — who the unit is
  built from, including excluded members where authority requires) and
  `participating_member` (who participates for eligibility and allotment).
  A bare aggregation never reads an implicit default projection.
- **Derived unit** — an entity instance the engine materializes, with a
  content-derived identity and cited membership tuples per person per
  projection.

## 3. The statutory case: 7 CFR 273.1, decomposed

Every operative paragraph of § 273.1 maps onto the seven operation kinds.
Quotations are verbatim; shortened forms are labeled as paraphrase.

| Provision | Content | Operation kind |
|---|---|---|
| (a)(1)–(3) | "[a]n individual living alone"; an individual "customarily purchasing food and preparing meals for home consumption separate and apart from others"; "[a] group of individuals who live together and customarily purchase food and prepare meals together for home consumption" | base grouping over purchase-and-prepare assertions. (a)(1) applies only to a person living alone; (a)(2) requires affirmative separate-and-apart evidence — absence of an edge proves neither (§ 11) |
| (b)(1)(i)–(iii) | spouses; "[a] person under 22 years of age who is living with his or her natural or adoptive parent(s) or step-parent(s)"; "[a] child (other than a foster child) under 18 years of age who lives with and is under the parental control of a household member other than his or her parent", with a dependency presumption ("financially or otherwise dependent") and a State-law-adulthood override — all "must be considered as customarily purchasing food and preparing meals with the others, even if they do not do so", "unless otherwise specified" | combination edges. "(other than a foster child)" is a *trigger condition* of (b)(1)(iii), not a defeat of an assembled edge (§ 4.4). (b)(1)(ii) must cover natural, adoptive, and step-parents. (b)(1)(iii)'s reference to a "household member" controller needs a non-circular reading (candidate-relative or residence-based) fixed in the ratification matrix |
| (b)(2) | an "otherwise eligible member of a household who is 60 years of age or older and is unable to purchase and prepare meals because" of a qualifying permanent disability "may be considered, together with his or her spouse (if living there), a separate household", "Notwithstanding the provisions of paragraph (a) of this section", barred "when the income of the others with whom the elderly disabled individual resides (excluding the income of the elderly and disabled individual and his or her spouse) exceeds 165 percent of the poverty line" | guarded, elective separation. Guard elements: otherwise-eligible (stratum question open, § 10), age, the *causal* disability condition, spouse carry-along, the co-resident income measure and period (open), and the poverty-line size key (open). All opens sit in the ratification matrix |
| (b)(3) | commercial-boarding-house residents ineligible; other payers of "a reasonable amount" are boarders, "not eligible to participate in the Program independently of the household providing the board", who "may participate, along with a spouse or children living with them, as members of the household providing the boarder services, only at the request of the household providing the boarder services"; "[a]n individual paying less than a reasonable amount for board must not be considered a boarder but must be considered, along with a spouse or children living with him or her, as a member of the household providing the board"; thresholds: "equals or exceeds the maximum SNAP allotment" for more than two meals a day, "equals or exceeds two-thirds of the maximum SNAP allotment" for two or fewer, each "for the appropriate size of the boarder household" | classification over a `board_arrangement` candidate (payer individual/group, provider candidate, payment, meals per day, period, spouse/child carry-along, evidence) → independent-participation bar (reasonable payers), request-guarded attachment (actor = providing household), and *mandatory* attachment for the below-threshold payer (textually an "individual"). The threshold's size key (payer group vs payer plus spouse/children) is an open legal reading in the matrix |
| (b)(4) | governmental foster-care placements "must be considered to be boarders", cannot participate independently, attach "only at the request of the household providing the foster care" | forced classification → independent-participation bar + request-guarded attachment |
| (b)(5)–(b)(6) | roomers and live-in attendants "may participate as separate households"; "[p]ersons described in paragraph (b)(1) of this section must not be considered" roomers or live-in attendants | permissive separations; the (b)(1) clause is a *classification prohibition* (the classification is unavailable for such persons), not a boundary-cut geometry test (§ 4.4) |
| (b)(7)(i)–(xii) | listed persons "are not eligible to participate as separate households or as a member of any household": aliens/students (§§ 273.4, 273.5), SSN failure (§ 273.6), work-requirement, IPV, institution residents (majority-meals test, "over 50 percent of three meals daily") with five exception classes that "can participate in the Program and must be treated as separate households from the others with whom they reside, subject to the mandatory household combination requirements of paragraph (b)(1), unless otherwise stated" — treatment centers include "the children but not the spouses"; shelters for battered women cover "[i]ndividual women or women with their children" — plus felony-drug, fleeing-felon, child-support, ABAWD (§ 273.24), and, "[a]t State agency option", comparable disqualifications under § 273.11(k) | status tags on the participation projection — composition is unchanged, and § 273.1(d)(2) itself still calls such a person a "household member (including excluded members)". The institution exceptions are separations subject to (b)(1); their children/spouse clauses are carry-along membership rules whose exact operation is an open reading. (b)(7)(viii) is not unconditional: it carries a State/authority/effective-period *policy guard* recording the exercised option |
| (c) | "the State agency may apply its own policy for determining when an individual is a separate household or a member of another household if the policy is applied fairly, equitably and consistently throughout the State" | delegated discretion: an explicit hook naming *who* decides. An engine-wide default is not a State policy (§ 11) |
| (d) | head of household designation | a determination *within* a derived unit; out of scope (§ 12) |
| (e) | strikers | unit-scoped eligibility law reading membership; phase-2 law, not constitution (§ 12) |

Two textual observations recur below — stated now with their honest strength:

1. **Mandatory combinations survive the separations that address them.** The
   institution-resident exceptions are separate households "subject to the
   mandatory household combination requirements of paragraph (b)(1)", and
   (b)(5)/(b)(6) prohibit classifying (b)(1) persons as roomers or attendants.
   Whether (b)(2) — whose notwithstanding clause names paragraph (a) only —
   also yields to a crossing (b)(1) combination is *not* settled by this text:
   (b)(1)'s own hook is "unless otherwise specified", which does not require a
   numbered cross-reference. That interaction is a legal-interpretation
   decision in the ratification matrix, not an engine default.
2. **Declared precedence is an engine discipline, not a statutory canon.**
   This contract requires every override to be *written down* with its
   citation so conflicts are compile-visible (§ 4.4). That is an authoring
   safety rule. It does not assert that the regulation only ever defeats
   (b)(1) by express reference — (b)(3)'s request-gated attachment, for
   example, can conflict with (b)(1) without naming it, and each such
   interaction needs a cited resolution.

On thresholds inside constitution rules: the (b)(2) income test reads the
income of the others with whom the individual *resides* — the co-resident
roster, not the derived unit — and the boarder thresholds read a maximum
allotment parameter keyed by a *candidate* group's size. Both are
implementable below the unit stratum *provided* the open readings above
(otherwise-eligible; the two size keys; the income measure) resolve to
person-scoped or candidate-scoped terms. If any resolves to a term that
depends on the derived household, that provision requires an explicitly
declared two-pass/fixpoint semantics or is excluded from the stratified pilot
(§ 10). This is a conditional implementability statement, not a proof.

## 4. The derivation algebra

The construct is **not** a transitive closure. It is a staged pipeline of
seven operation kinds whose precedence is declared by citation, evaluated
world-relative (§ 5), composed as follows.

### 4.1 Inputs

- A roster relation over persons (for SNAP: co-residence in a dwelling),
  with an explicit completeness assertion (§ 5.1). Roster slices for one
  (program, relation, segment) must be person-disjoint, or declare an
  explicit scope that enters the unit-identity preimage with a defined
  cross-scope conflict rule.
- Person facts, person-pair facts, and election/request facts, each with
  provenance and with the scoped completeness semantics of § 5.1.
- Parameters, temporally selected under § 10's segmentation rule.

### 4.2 Pipeline (per admissible world, § 5.2)

1. **Edge assembly.** Evaluate every edge trigger over the roster:
   - *base edges* from purchase-and-prepare assertions ((a)(2)–(3)),
     symmetric;
   - *combination edges* from (b)(1)-class provisions, where a trigger's own
     negative conditions (the foster-child carve-out) simply prevent the edge.
   Then apply *edge-level defeaters* — provisions declared to remove another
   provision's edges. The result is the world's **frozen edge set**. No later
   stage modifies it.
2. **Classification.** Evaluate classification provisions over pre-unit
   candidates (e.g. the `board_arrangement` candidate; institution residence
   via the majority-meals test), honoring classification prohibitions
   ("must not be considered" clauses) as guard-level unavailability. Guards
   use only person-scoped facts, person-scoped derived rules, candidate-scoped
   terms, parameters, and elections (§ 10).
3. **Separations.** Evaluate every separation guard, yielding candidate cut
   sets S₁…Sₖ with citations. Coalesce identical sets (union citations and
   evidence). Apply declared cut-versus-cut precedence. The surviving active
   sets **must be pairwise disjoint**: if the compiler cannot prove
   disjointness, it must emit a runtime overlap check, and a co-active
   overlap without a governing precedence declaration is a
   `constitution_overlap_conflict` — the affected persons' membership is
   Indeterminate, never silently repaired.
4. **Blocking — simultaneous, against the frozen edge set.** A cut Sᵢ is
   *blocked* if a combination edge in the frozen set crosses its boundary
   (one endpoint in Sᵢ, one outside) and Sᵢ's provision does not carry a
   declared, cited override of that edge's provision. All cuts' block tests
   are evaluated against the same frozen edge set; no cut removes an endpoint
   before another cut's test. Blocked cuts do not apply; each block is a
   cited determination in the trace.
5. **Partition of the residue.** Let V be the roster and A the union of
   unblocked active cut sets. The candidate units are the sets in A plus the
   connected components of the frozen edge graph induced on V \ A. Removal
   can split a component; a residual unit's existence is cited to the
   separation's operation of law plus the residual grouping rule — not to
   (a)(2), whose factual predicate ("separate and apart") the residue may
   contradict (§ 11).
6. **Attachments and bars.** Apply independent-participation bars and
   attachments over candidate units: mandatory attachments (the
   below-threshold board payer, with spouse/children carry-along) move their
   person-set into the providing candidate unit; request-guarded attachments
   move it only on the provider candidate unit's recorded request (§ 4.3);
   barred groups that are not attached remain composed as units that cannot
   participate, with the bar cited. Attachment target identification (the
   provider candidate) is part of the `board_arrangement` candidate, fixed
   before units are minted.
7. **Status tagging.** Apply (b)(7)-class exclusions per person, producing
   per-projection roles: `unit_constituent` membership is unchanged;
   `participating_member` excludes the tagged person with the tagging
   provision's citation. Downstream treatment of excluded members —
   § 273.11(c)(1)'s count-in-entirety class and § 273.11(c)(2)'s pro-rata
   class (which includes the SSN and ABAWD cases), and § 273.11(c)(1)(ii)'s
   exclusion from household size — is ordinary phase-2 law reading the
   projections.
8. **Emission.** Materialize each unit whose membership is Determined (§ 5.2)
   as an entity instance with a content-derived identity (§ 4.5), and one
   membership tuple per (unit, person, projection) carrying role and the
   citation set that put the person there. Tuples are emitted under the
   compile-resolved canonical relation id (§ 8).

### 4.3 Elections and requests carry actors

Every permissive or request provision declares its legally authorized actor,
its evidence form, and its missing-evidence policy:

- (b)(3) and (b)(4): the actor is **the household providing** board or foster
  care — a candidate unit, identified through the pre-unit provider candidate.
  This is why attachment runs after candidate-unit formation (§ 4.2 step 6).
- (b)(2), (b)(5), (b)(6): the text names no actor ("may be considered", "may
  participate"). The actor and the default when no election fact is supplied
  must be established by cited administrative authority per provision. Absent
  such cited authority, a missing election fact is **Unknown** — it enters
  the admissible worlds like any other guard fact — not a silent
  "not exercised". Where authority does establish a default, its application
  is an explicit, trace-recorded default event naming the missing fact.
- An explicitly supplied Unknown or Conflict election/request record is an
  ordinary unresolved fact, never coerced by any default.

### 4.4 Declared precedence, not positional precedence

The compiler distinguishes three things that v1 of this document conflated:

- **Trigger conditions** — negative clauses inside a provision's own trigger
  ("other than a foster child"): no edge or classification arises.
- **Edge-level defeaters** — a provision declared to remove another
  provision's edges before the edge set freezes.
- **Cut-local overrides** — a separation's declared, cited permission to
  cross a specific provision's edges in its block test.
- **Classification prohibitions** — "must not be considered X" clauses,
  encoded as unavailability of the classification for the described persons,
  not as boundary geometry.

Every defeater, override, and prohibition names the provision it addresses,
quoting its statutory hook. The compiler builds the defeat/override graph
from those declarations; it must be acyclic, and two operations that can
disagree about a person with no declared order are a compile error — no
document order, no last-wins, consistent with the engine's
ambiguity-is-an-error direction (#79, #82, #132). Where the statute itself
does not settle a precedence (anchor 1's (b)(1)/(b)(2) question), the
declaration must cite a reviewed authoritative interpretation; until one
exists the interaction is an unresolved legal conflict and affected
determinations are Indeterminate with that reason — not a default block and
not a default cut.

### 4.5 Determinism

Same facts → same result, exactly:

- **Order invariance.** Derivation consumes fact sets after the § 5.4
  normalization; permuting dataset records cannot change the result.
- **Confluence, with its real preconditions.** Cuts commute because block
  tests run simultaneously against one frozen edge set, active cut sets are
  pairwise disjoint, subtraction is simultaneous, and guards are
  stratum-checked to be unit-independent (§ 10). Stratification alone does
  not buy confluence; the frozen-set and disjointness rules above are
  load-bearing.
- **Content-derived unit identity.** A derived unit's id is a digest over a
  normative preimage — illustratively
  `SHA-256(domain_tag ‖ constitution_semantics_digest ‖ canonical_relation_id ‖
  roster_scope ‖ segment ‖ len-prefixed sorted canonical member ids)` — with
  the algorithm, domain separation, length encoding, id canonicalization,
  segment encoding, and the exact definition of `constitution_semantics_digest`
  (which edits churn ids, which do not) fixed normatively with test vectors
  at stage 2. Insertion order does not exist. Identity across periods is out
  of scope (§ 12).
- **Idempotence, stated honestly.** Re-deriving over the same facts yields
  the same units, tuples, and determinations. Byte-for-byte artifact
  idempotence additionally requires canonical ordering and deduplication of
  tuples, citations, evidence sets, edges, blocked-cut records, and trace
  DAG nodes, which the stage-2 format must define before claiming it.

## 5. Unknown semantics: which missing facts poison which determinations

The rule #118 established for relations applies with more force here: absence
must never silently become a smaller household or a denial. This section
defines what becomes indeterminate — and what does not — precisely.

### 5.1 Completeness is scoped, and the roster is only the first scope

Deriving membership needs two different kinds of completeness:

- **Roster completeness.** A derivation consumes its roster relation only
  under an explicit completeness assertion ("every person residing at this
  dwelling in the period appears here"). Without it, every determination in
  that roster is Indeterminate with reason `roster_not_asserted_complete`,
  because an unlisted co-resident could be a mandatory member of any unit.
  This is not a new burden: a caller supplying `member_of_household` tuples
  today is asserting completeness of the very conclusion. Derivation moves
  the assertion one level down, onto observable facts.
- **Relation-family completeness.** Roster completeness says who exists; it
  does not say what an absent tuple means. For every relation family a
  constitution reads (purchase-and-prepare, spousal status, parentage,
  parental control, board arrangements, elections/requests, status facts),
  absence inside an evidence-carrying complete slice is Known-false;
  absence outside any complete slice is Unknown. Three roommates with only
  the affirmative assertion P&P(a,b) do **not** yield a Determined
  {a,b}|{c}: the unasserted a–c and b–c possibilities (and spousal/parentage
  possibilities, and applicable guards) must be Known-false, decisively
  settled by other facts, or covered by complete slices before the invariance
  criterion below can close. Blanket global completeness is *not* required —
  a determination stands whenever unresolved facts cannot change it — but
  fixtures intended to produce fully Determined output must supply enough
  complete slices and Known facts to eliminate every result-changing
  Unknown, and may not infer those facts from supplied membership (§ 13,
  comparison protocol).

### 5.2 Admissible worlds and the invariance criterion

The unresolved input set U for a roster and period is the finite set of input
keys reachable from the checked constitution plan whose normalized state
(§ 5.4) is Unknown or Conflict. An **admissible world** is a total valuation
of U that:

- preserves every Known fact;
- ranges each Conflict key over its distinct observed candidates;
- ranges each Unknown key over its declared typed domain, with infinite
  domains (age, income) quotiented into the finite observational classes
  induced by the truth vectors of the plan's edge, guard, member, status,
  and attachment formulas — the compiler must derive this finite quotient or
  reject the constitution as uncheckable;
- satisfies declared joint integrity constraints (age/date-of-birth
  consistency, symmetric spousal assertions, and the like).

If the admissible set is empty, the result is `inconsistent_inputs` for the
affected scope — never Determined, and not vacuously invariant.

Determination is **per person and per projection**:

> For person p, `membership(p)` is Determined iff p's constituent block —
> the member set of the unit containing p, or None — is identical in every
> admissible world. `role(p, projection)` is a separate determination with
> the same quantifier. A derived unit is minted only for a block whose every
> constituent has that same invariant block; unstable alternatives mint no
> unit ids and are reported as person-anchored `IndeterminateMembership`
> records carrying the specific unresolved facts (and, where useful, witness
> worlds).

Never a guessed partition; never a silently smaller household. The reported
unresolved-fact set is the elicitation product: which question, if answered,
would determine this person's household. A person's membership can be
Determined while a role on it is Indeterminate — the #118 case done right is
exactly that shape: composition {C, D} Determined, D's participating-member
role Indeterminate with the § 273.6 SSN fact named via § 273.1(b)(7)(iv),
allotment Indeterminate downstream — rather than a confident $0.

Conflicts poison through the same quantifier, ranging over their candidates:
a Conflict between ages 20 and 21 does not disturb an under-22 edge, because
every candidate yields the same truth vector. Conflict is still reported
distinctly (§ 5.4), because its remedy differs.

### 5.3 Locality: influence sets, not graph components

An Unknown fact's blast radius is its **dependency closure**, computed
conservatively: seed with the persons named by the unresolved key and every
subject or member-candidate of each constitution or status operation whose
guard, member expression, aggregate, election, parameter lookup, or derived
person-scoped input depends on that key; close under every edge possible in
any admissible world and every potentially active separation, attachment, or
classification set. Persons outside the closure are unaffected and their
determinations stand.

Plain connected-component locality — v1's rule — is valid **only** for
edge-only Unknowns with fixed vertices and no guard, cut, role, parameter, or
classification dependency. It fails beyond that: an unknown *income* of a
co-resident who shares no edge with anyone still toggles a (b)(2) cut and
changes three other people's units, because the 165-percent test aggregates
over co-residents, not over the graph. There is no "most-connective"
resolution in general (two values of one categorical fact can activate two
different cuts, producing incomparable partitions), so no single extremal
world defines the bound; the closure above does.

Within that discipline, partial output is the point: Determined persons and
units of a roster proceed while Indeterminate ones wait for facts — never
all-or-nothing per roster, never a guess.

Implementation note (non-normative): for Unknowns that are pure independent
edge presence/absence with no non-edge dependencies, let E− be edges present
in every admissible world and E+ edges present in at least one; equal
components in E− and E+ certify Determined. This two-evaluation test is a
*sufficiency certificate*; it is complete only when both extremes are
themselves jointly admissible (independent Boolean keys). Correlated edges
break completeness: with admissible worlds {a–b, b–c} and {a–c, b–c} only,
every world yields {a,b,c}, yet E− separates a — the certificate must then
fall back to the world quantifier rather than report Indeterminate.

### 5.4 Normalization and conflicts

Constitution facts are normalized once, before either phase, into Knowledge
states: zero covering records → Unknown; one → Known; several byte-identical
with the same evidence → deduplicated Known; several distinct → Conflict
unless the input declaration names an explicit, cited precedence policy.
Dataset order is never a policy.

This is a **new normative requirement on the stage-2 binder and on both
target designs** — it is not current engine behavior, and this document does
not claim otherwise. Today a missing scalar input is an error
(`EvalError::MissingInput`), tied distinct covering values are
`AmbiguousInput`, and covering-record selection follows the deterministic
input-precedence rule of #131. Those semantics remain v2 semantics; the
Knowledge normalization here binds the constitution path and the (#125-riding)
format that carries it.

## 6. Cited membership: the trace

"Household size = 3" must be an auditable conclusion. The derivation trace
contains, per determination:

- the roster and every completeness assertion consumed (evidence-carrying);
- every frozen edge that contributed, with its provision and facts;
- every classification and its candidate (e.g. the board arrangement with
  payment, meals per day, and the parameter cells selected for the
  threshold);
- every separation applied — guard evaluation, election/request evidence
  with its actor, citation — and every separation *blocked*, with the
  blocking edge's citation: overrides that fired and overrides that were
  denied are both conclusions;
- every attachment and independent-participation bar, with the provider
  candidate and request evidence;
- per member, per projection: the provision(s) that placed them and their
  role citations;
- for Indeterminate determinations: the person-anchored unresolved facts,
  each tied to the provision whose answer it would change.

Canonical trace bytes (ordering, deduplication, proof selection over
redundant edge support) are a stage-2 format definition; the *content* above
is the contract.

### Worked example

Dwelling roster (asserted complete, with complete P&P, spousal, and parentage
slices): A (age 62, disability meeting (b)(2)'s causal test, otherwise
eligible), B (A's spouse), C (A and B's 21-year-old child), D (roommate),
E (D's 8-year-old child). P&P: {A,B,C} together; D with E; all cross-pairs
Known-false via the complete slice. Co-resident income of C, D, E is below
the (b)(2) threshold under the measure the matrix fixes. A separation
election for (b)(2) is supplied with cited authority for A as actor.

- Frozen edges: base A–B, A–C, B–C, D–E; combination A–B ((b)(1)(i)), A–C
  and B–C ((b)(1)(ii): under-22 child with parents), D–E ((b)(1)(ii)).
- Separation S₁ = {A, B} per (b)(2): guard holds; election recorded.
- **The open precedence decides the result, visibly.** Combination edges
  A–C and B–C cross S₁'s boundary. Under the reading that (b)(2) does not
  override (b)(1), S₁ is blocked (cited to (b)(1)(ii)) and the units are
  {A,B,C} and {D,E}. Under the reading that (b)(2) "otherwise specifie[s]",
  S₁ applies and the units are {A,B}, {C}, {D,E} — where {C}'s existence is
  cited to (b)(2)'s operation of law plus the residual grouping rule, *not*
  to (a)(2), since C's known together-facts with A and B contradict
  "separate and apart". The constitution module must declare one reading
  with cited authority (§ 4.4); with none, A, B, and C are Indeterminate
  with the unresolved legal conflict named. D and E are Determined either
  way — the question's influence set does not reach them.
- Membership tuple sketch (shape illustrative; format decided with #125's
  bump):

```yaml
person: person:E
projection: unit_constituent
unit: household:sha256:9f31…            # § 4.5 preimage
role: member
established_by:
  - provision: us:regulations/7-cfr/273/1#a-3
    via: base_edge {D, E}
  - provision: us:regulations/7-cfr/273/1#b-1-ii
    via: combination_edge {D, E}
derivation: trace_node …
```

Now delete the P&P fact between D and E (leaving it outside any complete
slice): the (b)(1)(ii) combination edge alone still connects them — Determined,
with the combination citation only. That is #134's requirement 1 in one
example: mandatory inclusion overrides missing base facts, citedly. Delete
instead E's parentage fact: the base edge alone connects them — Determined via
(a)(3). Delete both: D's and E's membership is Indeterminate naming both
facts; A, B, C are untouched. Make D's SSN-cooperation fact Unknown instead:
membership stays Determined, D's `participating_member` role is Indeterminate
((b)(7)(iv) via § 273.6), and phase-2 counts over the participation
projection go Indeterminate with that reason.

## 7. Artifact provenance: derived vs supplied

A consumer must be able to tell whether composition came from law or from the
caller. Requirements on the compiled artifact and the result — all of them
**new format capabilities** riding the #125 bump, none of them descriptions
of the current wire format:

- **Input contract.** Each relation is marked `provenance: supplied` or
  `provenance: derived {rule, citations}`. A derived relation leaves the
  required-inputs contract, replaced by the facts its derivation consumes
  (roster with completeness, P&P assertions, ages, elections, …): the
  artifact's question to the caller becomes the facts of § 273.1.
  `axiom inspect` surfaces this.
- **Result marking.** Emitted units and tuples carry derived provenance with
  the constitution rule id and trace root; supplied tuples continue to pass
  through and are *newly* marked supplied (current records carry no
  provenance or role payload — the marking is part of the bump).
- **Typed payloads.** Membership tuples carry projection and role; bare
  relation aggregations must name the projection they read (§ 2).
- **Versioning.** Derivation changes the semantic version (a new rule kind
  with defined evaluation) and the wire format (contract, payload, and
  provenance fields) together, under #125's separation of identities. The
  stage-2 prototype names a distinct experimental artifact/semantics version;
  a flag-built artifact is rejected by publication tooling and never
  serialized as v2.

Supplied-and-derived conflict for the same relation is a bind error by
default, with an explicit comparison mode that binds the supplied side into a
non-evaluable shadow channel (§ 13, comparison protocol). Silent
supplied-wins or derived-wins are both rejected.

## 8. Interaction with existing machinery

- **Canonical relation identity.** Lowering already emits canonical
  origin-qualified relation ids, rewrites aliases to them, and binds public
  dataset names through checked resolution before indexing; the emitter must
  *receive and emit the compile-resolved relation id* — never reconstruct a
  public string — and comparison data must pass the same checked binder into
  the shadow channel. Raw engine/dataset construction that bypasses binding
  is non-authoritative. With that boundary, the #118 unread-alias class
  cannot arise for derived membership; a conformance case must prove emitter,
  downstream filters, comparison binder, and phase-2 reads share one resolved
  identity.
- **Entity-instance registry — a new invariant.** Compile-time namespaces are
  checked today (`insert_unique`, #132), but nothing checks *runtime entity
  instances*; the current dataset is untyped records with no instance
  registry, so a derived/supplied id collision has nothing to reject it.
  Stage 2 must add a typed registry — supplied instances bind first, unit
  emission performs a checked insert of (entity type, id, provenance), and
  any collision fails before phase-2 indexes build. The compile-time ratchet
  is the doctrine precedent, not the mechanism.
- **`derived_relation` composes only if lifted.** Filtered views over derived
  membership are well-defined *downstream* of materialization — but the
  filter must lift over Knowledge: Holds keeps, DoesNotHold drops,
  Unknown/Conflict makes the candidate's membership in the view unresolved,
  and complete-list reductions (count, sum) over unresolved candidates are
  Indeterminate with reasons. The current v2 filter pushes a candidate only
  on `is_holds()`, so an Undetermined predicate silently drops it and a
  count returns a confident smaller number — precisely the unknown-as-absence
  behavior this contract forbids. That behavior stays quarantined in v2
  semantics; the constitution path requires the lifted semantics. Direct
  records supplied under a derived relation's own name — accepted and
  unioned by the current engine — are likewise excluded from the
  constitution path outside the shadow channel.

## 9. Authoring surface (illustrative only)

Non-normative sketch; the surface is settled at stage 2:

```yaml
concepts:
  snap_household:
    kind: unit                          # name negotiable per #134
    unit:
      universe:
        roster: dwelling_resident       # completeness assertion required
      base:
        edges: purchases_and_prepares_with          # (a)(2)-(3), symmetric
      combine:
        - id: spouses                               # (b)(1)(i)
          when: spouse_of
        - id: under_22_with_parent                  # (b)(1)(ii)
          when: (parent_of or step_parent_of or adoptive_parent_of)
                and age < 22
        - id: child_under_control                   # (b)(1)(iii)
          when: age < 18 and not foster_child
                and parental_control_by_non_parent  # non-circularity: matrix
      separate:
        - id: elderly_disabled                      # (b)(2)
          members: [self, spouse_of]
          guard: otherwise_eligible and age >= 60
                 and unable_to_purchase_and_prepare_due_to_qualifying_disability
                 and coresident_income <= 1.65 * poverty_guideline(size_key)
          election: {actor: matrix_open, record: elects_separate_household}
          overrides: []                             # (b)(1) interaction: matrix
      classify:
        - id: board_arrangement                     # (b)(3)-(b)(4)
          candidate: {payers, provider_candidate, payment, meals_per_day}
          prohibitions: []
          outcomes:
            reasonable_payment:                     # >= max_allotment(size_key)
              bar_independent_participation: true   # or 2/3 branch
              attach: {to: provider_candidate, on_request_of: provider_candidate,
                       carry: [spouse_of, children]}
            below_reasonable_individual:
              attach: {to: provider_candidate, mandatory: true,
                       carry: [spouse_of, children]}
        - id: roomer                                # (b)(5)
          separation: permitted
          prohibited_for: b1_described_persons      # classification prohibition
      status:
        - id: ssn_noncooperation                    # (b)(7)(iv) → § 273.6
          projection: participating_member
          role: excluded
        - id: comparable_disqualification           # (b)(7)(viii)
          projection: participating_member
          role: excluded
          policy_guard: {authority: state_agency, exercised_option: required}
      emits:
        entity: household
        relations:
          unit_constituent: member_of_household     # matches corpus convention
          participating_member: snap_unit_members   # filtered view analogue
```

Every `when`/`guard` is an ordinary cited judgment; every block carries proof
atoms like any other rule. Terms the statute leaves open (`size_key`,
`otherwise_eligible`'s stratum, actors and defaults, the (b)(1)(iii)
"household member" reading) are matrix entries, resolved with citations —
never engine defaults.

## 10. Two-phase evaluation

Constitution rules run before unit-scoped rules. Precisely:

- **The invariant, stated algebraically.** Let Deps\*(c) be the transitive
  dependency closure of constitution node c, computed after alias resolution
  and lowering. For every constitution node: Deps\*(c) contains no
  unit-scoped rule output, no materialized unit relation, and no stratum-2
  node. Stratum-2 nodes may depend on materialized constitution outputs; the
  reverse is forbidden, and the check runs on the complete closure — a
  person-scoped label does not exempt a rule whose dependencies reach a unit
  term. A violation is a compile error naming the path.
- Stratification is **necessary** for the guard independence that § 4.5's
  confluence uses; it is not sufficient — the frozen-edge-set, simultaneous
  blocking, and disjointness rules carry the rest.
- **Candidate-scoped terms are stratum-1 terms.** The boarder thresholds and
  any poverty-guideline lookup key by *candidate* attributes and parameters —
  apparent circularity that stratification resolves, conditional on the § 3
  open readings resolving person- or candidate-scoped. A term that genuinely
  requires the derived unit forces a declared two-pass/fixpoint semantics
  (the re-review's simultaneity escape) or exclusion from the stratified
  pilot; no silent widening.
- **Segmentation before selection.** Derivation is per evaluation segment:
  facts and parameters must cover the segment under one selection, or the
  result is `RequiresSegmentation`. (The current engine selects parameter
  cells at `period.start`; the target designs require whole-period coverage —
  the constitution path takes the strict rule from day one, since a
  mid-period membership or parameter change otherwise changes the partition
  silently.)
- **Materialization barrier.** Between phases the engine materializes derived
  units and tuples (through the § 8 registry and canonical-id boundary);
  phase 2 evaluates over the enlarged universe exactly as over a supplied
  one. Derivation events are ordinary trace nodes; phase-2 values point to
  membership tuples whose traces reach back through the derivation.
- **Forward compatibility.** In the from-scratch designs this is a stratified
  schedule with a materialization step: sol's columnar machine gains a
  variable-cardinality structural operator, two checked schedules, and
  row/index rebuild at the barrier — its closed-world compiler,
  RelationSlice completeness, normalize-once, and Knowledge/Determination
  layers carry the § 5 semantics directly; the Claude design generalizes its
  derivation-as-value shape to structural partition results and extends
  checked inserts to runtime entities. No corrected clause requires a second
  semantic evaluator or an uncited preprocessing path outside the engine.
- **Supplied-membership programs are unaffected.** No constitution rules,
  empty stratum, evaluation exactly as today.

## 11. What derivation is not

- **Not a transitive closure.** Base grouping uses connected components
  within the staged pipeline, but the construct is components + declared
  merges + guarded simultaneous cuts + attachments + bars + projections.
  Reducing it to any single closure loses the override algebra that is most
  of the law.
- **Not an exerciser of discretion.** Where the statute delegates —
  (c)'s unregulated situations — the deciding actor is the State agency, and
  an engine-wide canonicalization is not a State policy merely because its
  trace cites (c). Pairwise-chain closure (A–B, B–C, no A–C assertion) is
  permitted only when the fact contract supplies a true group-valued
  assertion, or a named, effective State (c) policy module — with its own
  citation and period — adopts closure for that State. An affirmative
  non-mutual triangle (A–C known *not* together) is never closed into one
  unit: (a)(3)'s "together" is contradicted, and the case is (c) discretion.
  Absent an applicable policy module, the determination is Indeterminate
  with reason `discretion_not_encoded(273.1(c))`; a checked program either
  contains the policy or that node — the runtime never searches for an
  optional module, and missing *policy encoding* is distinct from missing
  *exercise evidence*. Discretion is neither Unknown nor a hard-coded
  interpretation.
- **Not entity resolution.** Derivation partitions an asserted roster of
  distinct persons. Whether two records are the same person is upstream data
  work, out of scope.

## 12. Non-goals

- **Cross-period unit continuity** — identity is per segment (§ 4.5);
  certification-period continuity is real and deferred.
- **Head of household ((d))** — a within-unit designation needing elections
  and agency fallbacks; representable later as unit-scoped law. Its (d)(2)
  "household member (including excluded members)" language is already load-
  bearing here as evidence for the projection model.
- **Strikers ((e))** — unit-scoped eligibility law; its "assuming the strike
  did not occur" clause is the hypothetical-evaluation gap tracked by the
  re-review, orthogonal to units.
- **Pilot scope vs semantic scope.** The operation vocabulary above covers
  (b)(3)–(b)(6) — classification candidates, bars, attachments, request
  actors — but their full decomposition (thresholds' size keys, provider
  candidates, carry-along nuances, the institution exceptions' children/
  spouse clauses) requires legal review and is validated in stage 2. The
  earlier draft's claim that these semantics were "complete now" is
  withdrawn; what is fixed now is the operation kinds and their composition
  rules, so that no (b)(3)–(b)(6) provision requires a *new* operation kind.
  Fixtures touching staged-out provisions are predeclared out of scope in
  the comparison (§ 13).
- **No release-line change.** Supplied membership remains the default
  indefinitely; nothing here blocks current consumers.

## 13. Ratification matrix

The earlier draft listed four decisions and called the rest settled; the
cross-model audit showed that undercounts. The matrix below is the complete
set of result-affecting choices, tiered by who resolves them. Tier A is
ratified now and freezes the contract; tier B entries are resolved during
stage 2 with cited legal authority, each with conformance tests for the
chosen branch; tier C entries are per-jurisdiction parameters encoded with
citations, never engine defaults; tier D are interface decisions ratified
with tier A. No legal reading may be selected after comparison results are
observed.

**Tier A — core semantics (ratify to freeze the contract):**

1. **Determination shape.** Per-person, per-projection invariance over
   admissible worlds; person-anchored indeterminacy; units minted only for
   invariant blocks (§ 5.2). Alternatives (per-unit determination, guessed
   partitions) rejected above.
2. **Cut discipline.** Frozen edge set, simultaneous blocking, coalesced
   identical sets, pairwise-disjointness with `constitution_overlap_conflict`
   on undeclared overlap (§§ 4.2, 4.4–4.5).
3. **Elections carry actors; defaults require cited authority; absent
   authority, absence is Unknown** (§ 4.3). The earlier "statutory default
   to not-exercised" blanket is withdrawn.
4. **Projections.** `unit_constituent` and `participating_member` as
   distinct, with (b)(7) implemented on participation and § 273.11(c)
   consuming the split; the pilot maps derived `unit_constituent` to the
   existing supplied `member_of_household` relation (the corpus's all-member
   convention) and participation to the filtered-view analogue. A bare
   aggregation names its projection.
5. **Supplied ∧ derived = bind error; comparison only via the shadow
   channel** (§ 7; protocol below).
6. **Scoped completeness semantics** for roster and every constitution
   relation family (§ 5.1).

**Tier B — legal interpretation (stage 2, cited authority, both branches
testable):**

7. (b)(1)/(b)(2) precedence — does the elderly-disabled separation cross a
   mandatory combination? (§ 3 anchor 1; worked example shows both.)
8. (b)(2) guard completion — otherwise-eligible's meaning and stratum; the
   causal disability test's evidence; the co-resident income measure and
   period; the poverty-line size key.
9. Boarder decomposition — the threshold size key; provider-candidate
   construction; joint-payer treatment; carry-along scope; request evidence.
10. Institution exceptions — each class's separation membership, the
    treatment-center children/spouses and shelter children clauses, and
    their (b)(1) subjection.
11. (b)(1)(iii) — the dependency presumption, State-law-adult override, and
    the non-circular reading of "household member" for the controller.
12. Permissive-provision actors and defaults for (b)(2), (b)(5), (b)(6).

**Tier C — jurisdiction/program parameters (encoded, cited, per State):**

13. (c) policies, including any pairwise-closure policy for non-mutual
    chains (engine ships no default; § 11).
14. (b)(7)(viii) comparable-disqualification option — State, authority,
    effective period, exercised option.
15. State-law adulthood definitions feeding (b)(1)(iii).

**Tier D — interface and migration (ratify with tier A):**

16. **Comparison protocol.** Supplied membership binds through the checked
    binder into a non-evaluable shadow channel; only derived tuples reach
    phase-2 indexes. Equality is partition-normalized — multisets of sorted
    canonical constituent blocks per projection, unit instance ids ignored —
    compared separately for constituent and participation projections.
    Fixtures either carry enough independent facts and completeness to
    determine the intended comparison or predeclare an expected
    Indeterminate; rosters are never inferred from supplied membership. A
    statutory expected-behavior ledger is frozen *before* results are
    observed (supplied composition can be wrong — old≠oracle, per the
    migration doctrine both designs adopt); every discrepancy and every
    Indeterminate/Conflict is classified against it. Out-of-pilot provisions
    are predeclared. The prototype serializes only under the named
    experimental version.
17. **Identity and canonicalization** — the § 4.5 preimage and the trace/
    tuple canonical ordering, fixed with test vectors before any byte-level
    idempotence claim.
18. **Registry and binder obligations** — the entity-instance registry, the
    emitter's compile-resolved-id boundary, Knowledge normalization at the
    constitution binder, and the lifted `derived_relation` semantics (§ 8).

Ratifying tier A and D fixes the contract; stage 2 of #134 (off-by-default
prototype, SNAP household pilot in rulespec-us, shadow-channel comparison on
fixtures satisfying entry 16) then proceeds, resolving tier B with legal
review as it encodes and parameterizing tier C per jurisdiction.
