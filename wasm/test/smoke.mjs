// Node smoke test for the wasm bindings: the same federal+state SNAP module
// pair as tests/module_source.rs, run compile -> execute through the
// `wasm-pack build --target nodejs` output in wasm/pkg-node, asserting the
// same 268 allotment.
//
// Build first:  wasm-pack build wasm --target nodejs --out-dir pkg-node
// Then run:     node wasm/test/smoke.mjs

import assert from "node:assert/strict";
import { createRequire } from "node:module";

// The nodejs target emits CommonJS; load it through createRequire so this
// file can stay an ES module.
const require = createRequire(import.meta.url);
const engine = require("../pkg-node/axiom_rules_engine_wasm.js");

const FEDERAL_TARGET = "us:policies/usda/snap/fy-2026-cola/maximum-allotments";
const STATE_TARGET = "us-co:policies/cdhs/snap/fy-2026-benefit";
const OUTPUT_ID = `${STATE_TARGET}#snap_regular_month_allotment`;

const FEDERAL_MODULE = `
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
`;

const STATE_MODULE = `
format: rulespec/v1
imports:
  - ${FEDERAL_TARGET}
module:
  source_verification:
    corpus_citation_path: us-co/guidance/cdhs/snap/fy-2026-benefit
    upstream_source_check:
      status: official_parameter_source
      checked_paths:
        - us/statute/7/2017/a
      rationale: The state guidance supplies the current implementation parameter.
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
`;

// Provenance exports.
const engineVersion = engine.engine_version();
const artifactFormatVersion = engine.artifact_format_version();
console.log(`engine_version: ${engineVersion}`);
console.log(`artifact_format_version: ${artifactFormatVersion}`);
assert.match(engineVersion, /^\d+\.\d+\.\d+$/);
assert.equal(typeof artifactFormatVersion, "number");

// compile: {canonical_target: yaml_text} map -> CompiledProgramArtifact JSON.
const modulesJson = JSON.stringify({
  [FEDERAL_TARGET]: FEDERAL_MODULE,
  [STATE_TARGET]: STATE_MODULE,
});
const artifactJson = engine.compile(modulesJson, STATE_TARGET);
const artifact = JSON.parse(artifactJson);
assert.equal(artifact.artifact_format_version, artifactFormatVersion);
assert.equal(artifact.engine_version, engineVersion);
assert.ok(
  artifact.program.derived.some((derived) => derived.id === OUTPUT_ID),
  `compiled program exposes ${OUTPUT_ID}`,
);
assert.deepEqual(artifact.program.module.source_verification, {
  corpus_citation_path: "us-co/guidance/cdhs/snap/fy-2026-benefit",
  upstream_source_check: {
    status: "official_parameter_source",
    checked_paths: ["us/statute/7/2017/a"],
    rationale: "The state guidance supplies the current implementation parameter.",
  },
});

// execute: CompiledExecutionRequest JSON -> ExecutionResponse JSON, exactly
// the CLI's request/response shapes.
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
    {
      entity_id: "household-1",
      period: { period_kind: "month", ...interval },
      outputs: [OUTPUT_ID],
    },
  ],
};

const response = JSON.parse(engine.execute(artifactJson, JSON.stringify(request)));
assert.equal(response.metadata.requested_mode, "explain");
assert.equal(response.metadata.actual_mode, "explain");

const output = response.results[0].outputs[OUTPUT_ID];
assert.ok(output, `response carries ${OUTPUT_ID}`);
assert.equal(output.kind, "scalar");
assert.equal(output.name, "snap_regular_month_allotment");
assert.equal(output.value.kind, "decimal");
assert.equal(output.value.value, "268");
console.log(`${OUTPUT_ID} = ${output.value.value} (expected 268)`);

// Errors cross the boundary as thrown JS Errors with the core's messages.
assert.throws(
  () => engine.compile("{}", "us:policies/never-written"),
  /never-written/,
  "missing root module is reported",
);
assert.throws(
  () => engine.execute(artifactJson, "not json"),
  /CompiledExecutionRequest/,
  "malformed request JSON is reported",
);
const v1Artifact = JSON.stringify({ ...artifact, artifact_format_version: 1 });
assert.throws(
  () => engine.execute(v1Artifact, JSON.stringify(request)),
  /requires exact version/,
  "v1 artifacts are rejected by the v2 engine",
);

// A formula nested past the bound is an error with the core's message, not a
// trap. Before the bound, each of these nested 1,000 levels deep overflowed
// the wasm stack, and after a trap later calls on the same instance could
// fail too.
const DEEP_TARGET = "us:policies/test/deep";
const deepModules = (formula) =>
  JSON.stringify({
    [DEEP_TARGET]: `
format: rulespec/v1
rules:
  - name: deep
    kind: derived
    entity: Household
    dtype: Integer
    period: Month
    versions:
      - effective_from: 2026-01-01
        formula: "${formula}"
`,
  });
const nested = (levels) => [
  `${"(".repeat(levels)}1${")".repeat(levels)}`,
  `${"-".repeat(levels)}1`,
  `1${" + 1".repeat(levels)}`,
];
let bound;
for (const formula of nested(100_000)) {
  assert.throws(
    () => engine.compile(deepModules(formula), DEEP_TARGET),
    (error) => {
      bound = Number(/formula nests more than (\d+) levels deep/.exec(error.message)?.[1]);
      return error instanceof Error && bound > 0;
    },
    "a formula nested past the bound is reported",
  );
}
// At the bound each of those compiles on the wasm stack, and the instance
// still answers after the refusals above.
for (const formula of nested(bound - 1)) {
  const deep = JSON.parse(engine.compile(deepModules(formula), DEEP_TARGET));
  assert.ok(deep.program.derived.some((derived) => derived.name === "deep"));
}
assert.equal(engine.compile(modulesJson, STATE_TARGET), artifactJson);
console.log(`formulas nest at most ${bound} levels`);

console.log("smoke test passed");
