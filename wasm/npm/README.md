# @axiom-foundation/rules-engine-wasm

The [Axiom rules engine](https://github.com/TheAxiomFoundation/axiom-rules-engine)
compiled to WebAssembly: compile and execute RuleSpec law encodings — statutes,
regulations, and policy as versioned, citable programs — in the browser or in
Node. No server, no network: household facts never leave the process.

Both wasm-pack targets ship in this package:

- **`.` (default, ESM/browsers/bundlers)** — the `web` build; call the default
  export to instantiate before use.
- **`./node` (CommonJS)** — the `nodejs` build; loads the binary from disk,
  ready on require.

## Browser / bundler

```js
import init, { compile, execute, engine_version } from "@axiom-foundation/rules-engine-wasm";

await init(); // fetches and instantiates the .wasm (bundlers resolve the asset)

// A RuleSpec program is a map of canonical targets to YAML module text.
const artifactJson = compile(JSON.stringify(modules), rootTarget);

// …or execute a precompiled artifact you downloaded and hash-verified.
const response = JSON.parse(execute(artifactJson, JSON.stringify(request)));
```

## Node

```js
const engine = require("@axiom-foundation/rules-engine-wasm/node");
const response = JSON.parse(engine.execute(artifactJson, JSON.stringify(request)));
```

## The JSON boundary

`compile(modules_json, root_target) -> artifact_json` takes
`{"<canonical target>": "<RuleSpec YAML>", …}` and returns a compiled program
artifact carrying `engine_version`, `artifact_format_version`, and every
rule's durable legal id.

`execute(artifact_json, request_json) -> response_json` takes a
`CompiledExecutionRequest` — a dataset of typed input records and relation
tuples plus queries — and returns outputs with an explain-mode trace whose
every value cites the statute, regulation, or answer it came from.

`engine_version()` and `artifact_format_version()` report what this build
compiles and accepts.

## Reference consumers

- [axiom-playground](https://github.com/TheAxiomFoundation/axiom-playground) —
  full composed programs (Colorado SNAP, federal income tax, …) executing
  in-tab, with descriptors, screening presumptions, and a citation-trace UI.
  Its `src/lib/` shows request building and trace reconstruction against this
  boundary.
- [axiom-reg-demo](https://github.com/TheAxiomFoundation/axiom-reg-demo) — a
  UK Companies Act determination in the browser.

## Provenance

Published from CI with npm provenance attestations; the workflow, the crate,
and the engine source are one repository. Apache-2.0.
