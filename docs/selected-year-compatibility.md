# Selected annual inputs on the compatibility runtime

This extends compatibility base `88c7fd9e3c0976c9a700608e8f9b93650dc48b92`
with the selected-year contract reviewed on main in
`6fefef2162177c390d7dbce99a544db807f3b8fa`. It does not replace the
compatibility runtime with main's unrelated evaluator changes.

A typed derived rule may read `input_for_year(income, 2025)` from its own
module's explicit primitive declaration. The year is an integer literal from
1 through 9999; dated source versions own the reference-year mapping. The
primitive requires the same owner entity, Year period, supported scalar type,
no default, and an unambiguous runtime slot. Money requires a declared currency
unit. Metadata is retained through compilation; raw IR validation is structural
and does not authenticate source authorship.

Evaluation keeps the current entity and current query/relation context while
looking up the inclusive selected calendar year. Covering/latest-start lookup,
conflicts and lazy execution retain existing semantics. Winning tied records
must all have the declared owner. No missing-year fallback, coercion, entity
creation or currency rounding is introduced. Executed trace nodes report the
selected interval, input, entity and value.

Unlike main's masked evaluator, this branch detects the feature before Fast
input normalization or warmup and explicitly selects Explain with the original
dataset. Detection covers all versions. A dead annual-read branch may therefore
report Fast-to-Explain fallback, while Explain still does not demand its unused
facts. Evaluation failures never trigger retries. Programs without this feature
or the existing `only_related` feature retain their previous Fast behavior.
Direct dense and lifetime compilation refuse this operation.

The existing `only_related` contract is preserved, including mixed programs that
use both operators. No new global primitive-input validation is imposed.

Before selecting this build in the API, require matching compiler/runtime
identity, all current maintenance-PR checks and independent review, unchanged
complete retained catalog outcomes (including blocked outcomes), runtime golden
parity and actual generated-policy compatibility. The API's annual transport
also needs explicit source-bound validation, certification dependencies and
isolation from current-period defaults and mirroring. Focused tests alone are
not deployment, source admission or entitlement-readiness evidence.
