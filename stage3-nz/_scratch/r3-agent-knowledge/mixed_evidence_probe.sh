#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
engine="$repo_root/target/debug/axiom-rules-engine"
plan="$repo_root/tests/fixtures/unit_derivation/nz_income_explorer_family.yaml"
request="$repo_root/tests/fixtures/unit_derivation/nz_income_explorer_request.json"
artifact="$repo_root/stage3-nz/_scratch/r3-agent-knowledge/nz-aggregation-artifact.json"

"$engine" compile-unit-aggregation --plan "$plan" --output "$artifact" >/dev/null

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
      --artifact "$artifact" \
      --enable-experimental-unit-derivation
)

printf '%s\n' "$result" | jq '{
  families_status: .families.status,
  members: (.families.value[0].members // null),
  care_projection: (.families.value[0].children[0].scalars.best_start_claimant_care_fraction // null),
  best_start_total: (.families.value[0].scalars.best_start_total // null),
  dependent_child_count: (.families.value[0].counts.dependent_child_count // null),
  youngest_child_age: (.families.value[0].counts.youngest_child_age // null)
}'

status=$(printf '%s\n' "$result" | jq -r '.families.status')
if [ "$status" != "indeterminate" ]; then
  echo "FAIL: mixed Conflict care + Unknown age returned top-level families.status=$status; expected indeterminate" >&2
  exit 1
fi

echo "PASS: mixed evidence cannot produce a Determined family"
