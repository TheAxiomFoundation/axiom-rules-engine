#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
engine="$repo_root/target/debug/axiom-rules-engine"
plan="$repo_root/tests/fixtures/unit_derivation/nz_income_explorer_family.yaml"
request="$repo_root/tests/fixtures/unit_derivation/nz_income_explorer_request.json"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/axiom-stage3-mixed-evidence.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
scratch=$(CDPATH= cd -- "$scratch" && pwd -P)

rulespec_root="$scratch/rulespec-nz"
source_program="$rulespec_root/nz/statutes/income_tax/family_scheme/tax_credits.yaml"
mkdir -p "$(dirname -- "$source_program")"
cp "$repo_root/tests/fixtures/unit_derivation/nz_best_start_gross.rulespec.yaml" "$source_program"

"$engine" compile \
  --program "$source_program" \
  --rulespec-root "$rulespec_root" \
  --output "$scratch/source.json" >/dev/null
"$engine" compile-unit-aggregation \
  --plan "$plan" \
  --source-artifact "$scratch/source.json" \
  --output "$scratch/aggregation.json" >/dev/null

result=$(
  jq '
    (.persons[] | select(.id == "child-0") |
      .scalars.best_start_claimant_care_fraction) = {
        "status": "conflict",
        "observations": [
          {"value": "1", "evidence": {"id": "mixed:care:full"}},
          {"value": "0.5", "evidence": {"id": "mixed:care:half"}}
        ]
      }
    | (.persons[] | select(.id == "child-1") | .age_years) = {
        "status": "unknown",
        "evidence": {"id": "mixed:age:unknown"}
      }
  ' "$request" |
    "$engine" run-unit-aggregation \
      --artifact "$scratch/aggregation.json" \
      --enable-experimental-unit-derivation
)

printf '%s\n' "$result" | jq -e '
  .families.status == "indeterminate"
  and .families.value[0].members == ["child-0", "child-1", "partner", "primary"]
  and [.families.value[0].children[].person] == ["child-0", "child-1"]
  and (.families.value[0].children[0].scalars.best_start_claimant_care_fraction.status == "indeterminate")
  and (.families.value[0].scalars.best_start_total.status == "indeterminate")
  and (.families.value[0].counts.dependent_child_count.status == "indeterminate")
  and (.families.value[0].counts.youngest_child_age.status == "indeterminate")
' >/dev/null

printf '%s\n' "$result" | jq '{
  families_status: .families.status,
  members: .families.value[0].members,
  children: [.families.value[0].children[].person],
  best_start_total: .families.value[0].scalars.best_start_total.status,
  dependent_child_count: .families.value[0].counts.dependent_child_count.status,
  youngest_child_age: .families.value[0].counts.youngest_child_age.status
}'
