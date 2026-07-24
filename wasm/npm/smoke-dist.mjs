// Smoke test for the ASSEMBLED npm package (wasm/npm-dist): the node target
// must compile and execute the same federal+state pair test/smoke.mjs runs,
// and the web target's files must be present for bundler consumers.
// Run after build-npm.mjs:  node wasm/npm/smoke-dist.mjs

import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "npm-dist");
const require = createRequire(import.meta.url);
const engine = require(join(dist, "node", "axiom_rules_engine_wasm.js"));

const FEDERAL_TARGET = "us:policies/usda/snap/fy-2026-cola/maximum-allotments";
const STATE_TARGET = "us-co:policies/cdhs/snap/fy-2026-benefit";
const OUTPUT_ID = `${STATE_TARGET}#snap_regular_month_allotment`;

const modules = {
  [FEDERAL_TARGET]: `
format: rulespec/v1
rules:
  - name: snap_maximum_allotment_table
    kind: parameter
    dtype: Money
    unit: USD
    indexed_by: household_size
    versions:
      - effective_from: 2025-10-01
        values:
          1: 298
          2: 546
  - name: snap_maximum_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: snap_maximum_allotment_table[household_size]
`,
  [STATE_TARGET]: `
format: rulespec/v1
imports:
  - ${FEDERAL_TARGET}
rules:
  - name: snap_household_food_contribution_rate
    kind: parameter
    dtype: Rate
    versions:
      - effective_from: 2025-10-01
        formula: "0.30"
  - name: snap_regular_month_allotment
    kind: derived
    entity: Household
    dtype: Money
    period: Month
    unit: USD
    versions:
      - effective_from: 2025-10-01
        formula: floor(snap_maximum_allotment - (net_income * snap_household_food_contribution_rate))
`,
};

const artifactJson = engine.compile(JSON.stringify(modules), STATE_TARGET);
const interval = { start: "2026-01-01", end: "2026-01-31" };
const request = {
  mode: "explain",
  dataset: {
    inputs: [
      {
        name: `${FEDERAL_TARGET}#input.household_size`,
        entity: "Household",
        entity_id: "household-1",
        interval,
        value: { kind: "integer", value: 1 },
      },
      {
        name: `${STATE_TARGET}#input.net_income`,
        entity: "Household",
        entity_id: "household-1",
        interval,
        value: { kind: "decimal", value: "100" },
      },
    ],
    relations: [],
  },
  queries: [
    { entity_id: "household-1", period: { period_kind: "month", ...interval }, outputs: [OUTPUT_ID] },
  ],
};
const response = JSON.parse(engine.execute(artifactJson, JSON.stringify(request)));
const output = response.results[0].outputs[OUTPUT_ID];
assert.equal(output.value.value, "268");

// The web target ships alongside, assets intact, and the manifest exports both.
for (const file of [
  "web/axiom_rules_engine_wasm.js",
  "web/axiom_rules_engine_wasm_bg.wasm",
  "web/axiom_rules_engine_wasm.d.ts",
  "README.md",
  "LICENSE",
]) {
  assert.ok(existsSync(join(dist, file)), `missing ${file}`);
}
const manifest = JSON.parse(readFileSync(join(dist, "package.json"), "utf8"));
assert.equal(manifest.name, "@axiom-foundation/rules-engine-wasm");
assert.ok(manifest.exports["."], "missing root export");
assert.ok(manifest.exports["./node"], "missing ./node export");

console.log(
  `npm-dist smoke: 268 ✓ — ${manifest.name}@${manifest.version}, web+node targets present`,
);
