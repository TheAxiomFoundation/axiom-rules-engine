# Universal Credit — entitledto.co.uk comparison test pack

Axiom Rules Engine RuleSpec module: `universal_credit` (rates effective 2025-04-07).
Assessment period: 2025-05-01 to 2025-05-31 (one calendar month).

For each scenario below: the Axiom Rules Engine inputs, the Axiom Rules Engine outputs, and a block of
questions-to-answers to enter into entitledto.co.uk. Record the entitledto
monthly UC figure in the "Entitledto UC" row and flag any gap >£1.

All amounts £/month unless otherwise noted. Adults assumed aged 30 unless
stated. Rent is assumed private rented, no service charges, council tax
separate. All earnings are assumed from PAYE employment (no self-employment,
no pension contributions, no childcare costs).

---

## Scenario 1 — Single adult, no other income

**Household.** Single, age 30, no children, no housing costs, no earnings,
no unearned income, no capital.

**Axiom Rules Engine inputs.**
```
is_couple: false
has_housing_costs: false
eligible_housing_costs: 0
earned_income_monthly: 0
unearned_income_monthly: 0
capital_total: 0
adults: [{age_25_or_over: true, has_lcwra: false, is_carer: false}]
children: []
```

**Axiom Rules Engine outputs.**
- standard_allowance: **£400.14**
- max_uc: £400.14
- **uc_award: £400.14**

**Entitledto UC:** **£424.90 / month** (2026/27 rates)
- Breakdown: Standard allowance £424.90, no other elements, no adjustments.
- Δ vs Axiom Rules Engine: +£24.76. This is exactly the 2025/26 → 2026/27 uprating of the
  single-25+ standard allowance (£400.14 × 1.06186 ≈ £424.90, i.e. ~6.19%).
- **Conclusion:** structurally identical — entirely explained by the rate year
  differential. Zero Axiom Rules Engine bug surfaced here.

---

## Scenario 2 — Single parent, two children, part-time earnings

**Household.** Single parent, age 30, two children (ages 8 and 5 — both born
after 6 April 2017 for entitledto purposes; neither disabled). No housing
costs (e.g. lives rent-free with family). PAYE earnings £800/month net.
No capital, no unearned income.

**Axiom Rules Engine inputs.**
```
is_couple: false
has_housing_costs: false
eligible_housing_costs: 0
earned_income_monthly: 800
unearned_income_monthly: 0
capital_total: 0
adults: [{age_25_or_over: true, has_lcwra: false, is_carer: false}]
children:
  - {qualifies_for_child_element: true, disability_level: none}
  - {qualifies_for_child_element: true, disability_level: none}
```

**Axiom Rules Engine outputs.**
- standard_allowance: £400.14
- child_element_total: £678.00  _(2 × £339.00 — see note below)_
- max_uc: £1,078.14
- work allowance (no housing, responsible for a child): £684
- earnings above allowance: £800 − £684 = £116
- earnings deduction: 55% × £116 = £63.80
- **uc_award: £1,014.34**

**Entitledto UC:** **£983.28 / month** (2026/27 rates)
- Breakdown:
  - Standard allowance £424.90
  - Child element total £607.88 (2 × £303.94 — both children at the
    post-6-April-2017 rate of £303.94/mo)
  - Max UC before adjustments £1,032.78
  - Earned-income deduction £49.50 → implies work allowance £710.00 and
    earnings above = £90 → 55% × £90 = £49.50
  - Unearned deduction £0.00
  - UC award £983.28
- Δ vs Axiom Rules Engine: −£31.06 (entitledto lower). Decomposition:
  - Standard allowance uprating: +£24.76
  - Child element: entitledto uses the correct £303.94/child (post-Apr-2017
    rate, 2026/27), Axiom Rules Engine uses £339 for every child → Axiom Rules Engine over-states child
    element by (339 − 303.94) × 2 = £70.12
  - Work allowance uprating: entitledto £710 vs Axiom Rules Engine £684 → earnings deduction
    lower by (116 − 90) × 55% = £14.30 in entitledto's favour
  - Net: +24.76 − 70.12 + 14.30 = **−£31.06** ✓ reconciles exactly
- **Conclusion:** confirms Axiom Rules Engine bug #1 in the testpack footer — Axiom Rules Engine applies
  the £339 "first child (pre-April-2017)" rate to every qualifying child.
  For two post-April-2017 children this over-states the child element by
  ~£70/month. Fix: use £339 only for the eldest qualifying child if DOB is
  before 6 April 2017; otherwise £303.94 (2026/27) or £292.81 (2025/26).

---

## Scenario 3 — Couple with two children, private rent, one earner

**Household.** Couple, both 30, two children (ages 7 and 3, both post-April
2017, neither disabled). Private rent £800/month (assume eligible in full,
no non-dep deductions). One partner earns £1,500/month PAYE; other not
working. No capital, no unearned income.

**Axiom Rules Engine inputs.**
```
is_couple: true
has_housing_costs: true
eligible_housing_costs: 800
non_dep_deductions_total: 0
earned_income_monthly: 1500
unearned_income_monthly: 0
capital_total: 0
adults:
  - {age_25_or_over: true, has_lcwra: false, is_carer: false}
  - {age_25_or_over: true, has_lcwra: false, is_carer: false}
children:
  - {qualifies_for_child_element: true, disability_level: none}
  - {qualifies_for_child_element: true, disability_level: none}
```

**Axiom Rules Engine outputs.**
- standard_allowance: £628.10
- child_element_total: £678.00
- housing_element: £800.00
- max_uc: £2,106.10
- work allowance (with housing, responsible for a child): £411
- earnings above allowance: £1,500 − £411 = £1,089
- earnings deduction: 55% × £1,089 = £598.95
- **uc_award: £1,507.15**

**Entitledto UC:** _____________

---

## Scenario 4 — Couple, one LCWRA, one carer, no earnings

**Household.** Couple, both 30, no children, no housing costs. One partner
has LCWRA (Limited Capability for Work-Related Activity); the other is a
regular carer for a disabled adult receiving qualifying disability benefits.
No income, no capital.

**Axiom Rules Engine inputs.**
```
is_couple: true
has_housing_costs: false
eligible_housing_costs: 0
earned_income_monthly: 0
unearned_income_monthly: 0
capital_total: 0
adults:
  - {age_25_or_over: true, has_lcwra: true,  is_carer: false}
  - {age_25_or_over: true, has_lcwra: false, is_carer: true}
children: []
```

**Axiom Rules Engine outputs.**
- standard_allowance: £628.10
- lcwra_element: £423.27
- carer_element: £201.68
- max_uc: £1,253.05
- **uc_award: £1,253.05**

**Entitledto UC:** _____________

_Note: entitledto will require you to specify the LCWRA person is in receipt
of UC LCWRA (normally confirmed via Work Capability Assessment), and the
carer partner must be getting Carer's Allowance or meet the "regular and
substantial" caring test. Use those specifics when entering on entitledto._

---

## Scenario 5 — Single adult with capital and small pension

**Household.** Single, age 30, no children, no housing costs, no earnings.
£200/month unearned income (e.g. from a small personal pension). £8,500 of
capital in a savings account.

**Axiom Rules Engine inputs.**
```
is_couple: false
has_housing_costs: false
eligible_housing_costs: 0
earned_income_monthly: 0
unearned_income_monthly: 200
capital_total: 8500
adults: [{age_25_or_over: true, has_lcwra: false, is_carer: false}]
children: []
```

**Axiom Rules Engine outputs.**
- standard_allowance: £400.14
- max_uc: £400.14
- tariff income: (£8,500 − £6,000) / £250 = 10 complete bands × £4.35 = **£43.50**
- unearned income deduction: £200.00 (£-for-£)
- **uc_award: £400.14 − £43.50 − £200.00 = £156.64**

**Entitledto UC:** **£181.40 / month** (2026/27 rates)
- Breakdown: Standard allowance £424.90, tariff income deduction £43.50,
  pension deduction £200.00 → £181.40.
- Δ vs Axiom Rules Engine: +£24.76. Again exactly the 2025/26 → 2026/27 standard-allowance
  uprating (+£24.76). Tariff income formula, £6,000 floor, £250 band, and
  £-for-£ unearned-income deduction all match exactly.
- **Conclusion:** structurally identical. The capital tariff income and
  pension handling in Axiom Rules Engine agree with entitledto to the penny.

---

## Scenario 6 — Couple, three children (one disabled), LCWRA, high rent

**Household.** Couple, both 30. Three children — two qualify for the child
element (e.g. born before 6 April 2017 or otherwise within two-child limit),
one excluded by the two-child limit. The middle child has a higher-rate
disability (e.g. DLA highest-rate care or CP at enhanced daily living). One
parent has LCWRA. Private rent £1,200/month. No earnings, no capital.

**Axiom Rules Engine inputs.**
```
is_couple: true
has_housing_costs: true
eligible_housing_costs: 1200
non_dep_deductions_total: 0
earned_income_monthly: 0
unearned_income_monthly: 0
capital_total: 0
adults:
  - {age_25_or_over: true, has_lcwra: true,  is_carer: false}
  - {age_25_or_over: true, has_lcwra: false, is_carer: false}
children:
  - {qualifies_for_child_element: true,  disability_level: none}
  - {qualifies_for_child_element: true,  disability_level: higher}
  - {qualifies_for_child_element: false, disability_level: none}
```

**Axiom Rules Engine outputs.**
- standard_allowance: £628.10
- child_element_total: £678.00  _(2 × £339)_
- disabled_child_element_total (internal): £495.87 (higher rate, 1 child)
- lcwra_element: £423.27
- housing_element: £1,200.00
- max_uc: £3,425.24
- **uc_award: £3,425.24**

**Entitledto UC:** _____________

_Note on disabled-child addition: the third child is excluded by the
two-child limit but under UC rules still gets a disabled-child addition if
eligible. This scenario has the disabled child as the second (qualifying)
one, so that edge case doesn't bite. Worth checking a variant where the
disabled child is the one excluded by the two-child limit._

---

## Things I already noticed in Axiom Rules Engine that are worth flagging on entitledto

1. **First-child child element is £339 regardless of child order or birth
   date.** Real UC rule: the £339 "higher" rate only applies to a first
   child born before 6 April 2017; all other qualifying children get
   £292.81. Axiom Rules Engine applies £339 to every qualifying child, so it likely
   over-states child element by ~£46/child/month for post-April-2017
   children. Entitledto should use the correct blended rate once DOBs are
   entered — expect the biggest divergences in scenarios 2, 3, and 6.

2. **`disabled_child_element_total` is not in the declared outputs** even
   though it's computed internally and included in `max_uc`. That's a
   schema/doc nit, not an arithmetic bug. Confirm by summing the output
   components against `max_uc` — any gap is the disabled-child piece.

3. **Not in scope for Axiom Rules Engine yet:** childcare costs element, benefit cap,
   transitional protection, two-child limit exceptions (multiple births,
   kinship care, non-consensual conception), sanctions. If entitledto
   applies any of these (especially the benefit cap in scenario 6), the
   answers will diverge and it isn't an Axiom Rules Engine bug.

4. **LCWRA + standard allowance interaction for under-25s:** not exercised
   here (all adults are 25+). Worth a separate case if you want coverage.

---

## How to enter on entitledto.co.uk

Go to https://www.entitledto.co.uk/benefits-calculator/ and pick "Start the
calculator." For each scenario above:

- "Where do you live?" — England (doesn't affect UC).
- Date of birth — pick one making the adult 30 today (so 17 April 1996).
- Relationship — single / couple per scenario.
- Children — set DOBs to match the intended ages. For the two-child-limit
  excluded child in scenario 6, set DOB post-6-April-2017 and note
  entitledto will apply the limit automatically.
- Housing — "private rent" with the monthly figure given; no service
  charges; assume LHA covers the full amount (entitledto may cap at LHA —
  if it does, use a lower rent and re-run Axiom Rules Engine with the capped value).
- Work — "employed" with the monthly gross equal to the stated "net"
  (entitledto asks gross; if there's a gap >£10 it's because Axiom Rules Engine's input is
  post-tax/NI whereas entitledto runs its own tax & NI calc — harmonise by
  picking a gross where entitledto's net matches).
- Savings — enter the capital figure.
- Other income — enter any unearned income, tagged appropriately (e.g.
  "private pension" for scenario 5).

Once the results page loads, record the "Universal Credit — monthly"
figure in the "Entitledto UC" row of each scenario above.

---

## Quick comparison table (fill in after running entitledto)

| # | Scenario                           | Axiom Rules Engine UC (2025/26) | Entitledto UC (2026/27) | Δ (£)   | Notes |
|---|------------------------------------|------------------|-------------------------|---------|-------|
| 1 | Single 25+, no income              | 400.14           | 424.90                  | +24.76  | Pure SA uprating (+6.19%). Clean. |
| 2 | Single parent, 2 kids, £800 earn   | 1,014.34         | 983.28                  | −31.06  | Reconciles exactly: SA +24.76, child −70.12 (Axiom Rules Engine £339 bug), work-allowance uprating +14.30. |
| 3 | Couple, 2 kids, £800 rent, £1500e  | 1,507.15         | _not run_               |         | Would replicate bug #1 (both post-Apr-2017 kids at £339 in Axiom Rules Engine). |
| 4 | Couple, LCWRA + carer              | 1,253.05         | _not run_               |         |       |
| 5 | Single, £8.5k capital, £200 pension| 156.64           | 181.40                  | +24.76  | Pure SA uprating. Tariff income + pension handling match exactly. |
| 6 | Couple, 3 kids (1 higher), LCWRA   | 3,425.24         | _not run_               |         |       |

---

## Validation summary (scenarios 1, 2, 5 run on 2026-04-17)

Three scenarios were driven through entitledto.co.uk's public calculator via
browser automation on 2026-04-17. Entitledto defaulted to its own 2026/27
rates; Axiom Rules Engine was computed with 2025/26 rates. Every gap reconciles to a
combination of (a) the ~6.19% standard-allowance uprating and (b) one known
Axiom Rules Engine bug; nothing is unexplained.

**Identified Axiom Rules Engine bug (confirmed).** Scenario 2 exposes the issue already
flagged in the "Things I already noticed" section above: Axiom Rules Engine applies the
£339 first-child-(pre-April-2017) rate to every qualifying child. The
correct rule is £339 only for the first child if DOB is before 6 April 2017,
otherwise £292.81 (2025/26) / £303.94 (2026/27) per child. For a household
with two post-April-2017 children this over-states the child element by
~£70/month. Scenarios 3 and 6 would show the same over-statement; scenarios
1, 4, 5 have no children so they don't exercise the code path.

**What's been confirmed correct.** Single-adult standard allowance,
tariff-income formula (£4.35 per £250 complete band above £6,000), £-for-£
unearned-income (pension) deduction, earnings taper (55%), work allowance
with responsibility for a child (no housing). All of these reproduce
entitledto to the penny once the uprating differential is netted out.

**Known non-coverage.** Scenarios 3 (housing + earnings), 4 (LCWRA + carer),
and 6 (three kids, disabled child, LCWRA, high rent) were not run in this
pass. Scenario 4 is the most useful next target because it exercises
LCWRA + carer element stacking and neither has been cross-checked.

**Next actions recommended.**
1. Fix the child element rule to key off DOB (or a `born_before_april_2017`
   input flag), not apply £339 blanket. The internal doc note in this file
   about £339 vs £292.81 matches the MoneyHelper published rates.
2. Run scenario 4 for LCWRA + carer stacking confirmation.
3. Run scenario 6 for the disabled-child addition and two-child limit
   interaction (the test pack also suggests a variant where the excluded
   child is the disabled one — still worth doing).
