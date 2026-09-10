# Whole calendar-year units to months

`calendar_years_to_months(years)` converts a signed quantity of whole Gregorian
calendar years to an exact Integer quantity of calendar months. This is the
same calendar relationship used by `date_add_years`, exposed without requiring
a synthetic date or a caller-supplied conversion factor. It does not select
eligible years or establish any legal calculation.

The single argument must evaluate to an Integer or exactly integral Decimal
within the signed 64-bit range. The conversion uses checked multiplication and
returns an Integer. Zero and negative quantities are valid; source-defined
count constraints are separate. Fractional Decimals, Float columns, Boolean,
Text, Date, out-of-range inputs and multiplication overflow produce evaluation
errors. No truncation, saturation, rounding or inferred minimum is applied.

The conversion works in explain/scalar execution, generic dense execution and
lifetime execution, including nested derived expressions and related/current
entity expressions. Its lifetime argument follows the ordinary expression
rules: period-varying bare inputs remain ambiguous, and reductions define
their own period axis. The operation adds no period selection or date lookup.

Use Decimal execution for arithmetic-derived counts. Dense f64 arithmetic and
Decimal literals become Float columns, which this operation deliberately
refuses even if the result looks integral: prior floating arithmetic may have
already lost precision. Integer columns or literals that reach the operation
unchanged retain their exact Integer semantics. The separate bulk fast
executor explicitly reports this operation as unsupported.

For a history whose observations each represent one complete calendar year,
a selected-year sum can use the same count for its month denominator:

```text
sum_top_n_over_periods(annual_amount, selected_year_count)
    / calendar_years_to_months(selected_year_count)
```

Every complete calendar year contributes the same month count, so this
denominator is independent of which years are selected or how earnings ties
are ordered. Existing top-N bounds and period-invariance checks still apply.
An arithmetic-derived integral count is accepted under Decimal execution;
a fractional count is refused by the conversion, regardless of top-N's
separate existing numeric count behavior.

The caller/source must establish that the selected observations represent
complete calendar years and that selection itself is correct. A `tax_year`
label alone does not prove complete-year coverage. The operation does not
count covered months in partial years, fill history gaps, apply month-specific
exclusions, infer missing records or support other calendars. If selected
periods contribute different month counts, their denominator requires summing
those counts over the same selected indices; converting the number of periods
is insufficient.

The compiled expression remains an explicit `calendar_years_to_months` node,
so explanations and serialized artifacts retain the operation's meaning.
Artifact format v2 and lifetime request/response v1 remain unchanged; consumers
must use an engine revision supporting the added expression variant.
