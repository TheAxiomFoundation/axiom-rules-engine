# axiom-rules-engine-wasm

Browser/wasm bindings for the Axiom Rules Engine: compile and execute RuleSpec
programs entirely in the browser. Household data never leaves the page — zero
PII round-trip.

This is a sibling crate (like `python-ext/`) depending on the core with
`default-features = false`, so the engine inside the wasm binary has no
filesystem, environment, or clock access. Module text is supplied through an
in-memory `ModuleSource`, the seam introduced in `src/source.rs`.

## The JSON boundary

Everything crosses the wasm boundary as JSON strings, reusing the exact serde
types the CLI already speaks. No types are redefined on the JS side, so the
wasm build and the CLI cannot drift apart:

| Export | In | Out |
| --- | --- | --- |
| `compile(modules_json, root_target)` | `{canonical_target: yaml_text}` map + root target | `CompiledProgramArtifact` JSON (same format the CLI's `compile` writes) |
| `execute(artifact_json, request_json)` | artifact JSON + `CompiledExecutionRequest` JSON (`mode`, `dataset`, `queries`) | `ExecutionResponse` JSON (same format the CLI's `execute` prints) |
| `engine_version()` | — | core crate version string, for provenance display |
| `artifact_format_version()` | — | exact artifact format this engine writes/accepts |

Canonical targets are the `<jurisdiction>:<path>` form
(`us:policies/usda/snap/fy-2026-cola/maximum-allotments`). Relative imports
inside modules resolve against the importer's canonical target, exactly as on
a filesystem checkout, so durable ids are identical across hosts: an artifact
compiled in the browser matches one compiled by the CLI from the same modules.

Errors (unresolved imports, cycles, bad payloads, evaluation failures) are
thrown as JS `Error`s carrying the core's error messages.

## Building

Requires [wasm-pack](https://github.com/drager/wasm-pack)
(`brew install wasm-pack` or `cargo install wasm-pack`). Two targets, from the
repo root:

```sh
# Browser (ES module + .wasm, for bundlers or direct <script type="module">):
wasm-pack build wasm --target web --out-dir pkg-web

# Node (CommonJS, used by the smoke test and CI):
wasm-pack build wasm --target nodejs --out-dir pkg-node
```

Both land inside `wasm/` and are gitignored. The release profile is tuned for
payload size (`opt-level = "s"`, LTO); the `.wasm` is ~1.2 MB raw, ~380 KiB
gzipped.

## Testing

`wasm/test/smoke.mjs` runs the same federal+state SNAP module pair as
`tests/module_source.rs` through `compile` → `execute` against the nodejs
build and asserts the same 268 allotment — no browser needed:

```sh
wasm-pack build wasm --target nodejs --out-dir pkg-node
node wasm/test/smoke.mjs
```

CI runs this in the `wasm-pkg` job alongside the existing `wasm` core-check
job (which only `cargo check`s the core for wasm32).

## Browser usage sketch

```js
import init, {
  compile,
  execute,
  engine_version,
  artifact_format_version,
} from "./pkg-web/axiom_rules_engine_wasm.js";

await init(); // fetches and instantiates the .wasm once

// 1. Ship RuleSpec module text to the page however you like (static bundle,
//    fetch from a rules registry, …) — it contains no user data.
const modules = {
  "us:policies/usda/snap/fy-2026-cola/maximum-allotments": federalYaml,
  "us-co:policies/cdhs/snap/fy-2026-benefit": stateYaml,
};

// 2. Compile once; cache the artifact JSON (localStorage, IndexedDB, …).
const artifact = compile(
  JSON.stringify(modules),
  "us-co:policies/cdhs/snap/fy-2026-benefit",
);

// 3. Execute locally. The dataset holds the household's answers — it is
//    built and consumed on-device and never sent anywhere.
const response = JSON.parse(
  execute(
    artifact,
    JSON.stringify({
      mode: "explain",
      dataset: {
        inputs: [
          {
            name: "us:policies/usda/snap/fy-2026-cola/maximum-allotments#input.household_size",
            entity: "Household",
            entity_id: "household-1",
            interval: { start: "2026-01-01", end: "2026-01-31" },
            value: { kind: "integer", value: 1 },
          },
          {
            name: "us-co:policies/cdhs/snap/fy-2026-benefit#input.net_income",
            entity: "Household",
            entity_id: "household-1",
            interval: { start: "2026-01-01", end: "2026-01-31" },
            value: { kind: "decimal", value: "100" },
          },
        ],
        relations: [],
      },
      queries: [
        {
          entity_id: "household-1",
          period: { period_kind: "month", start: "2026-01-01", end: "2026-01-31" },
          outputs: ["us-co:policies/cdhs/snap/fy-2026-benefit#snap_regular_month_allotment"],
        },
      ],
    }),
  ),
);

const output =
  response.results[0].outputs[
    "us-co:policies/cdhs/snap/fy-2026-benefit#snap_regular_month_allotment"
  ];
// → { kind: "scalar", value: { kind: "decimal", value: "268" }, … }
// response.results[0].trace carries the full explain trace for rendering.

// Provenance for the UI footer:
console.log(`engine ${engine_version()}, artifact format v${artifact_format_version()}`);
```

`compile` and `execute` are synchronous CPU work; for large programs or
datasets, call them from a Web Worker to keep the UI thread free.
