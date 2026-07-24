#!/usr/bin/env node
/**
 * Assembles the publishable npm package for the wasm engine:
 * `@axiom-foundation/rules-engine-wasm`, both wasm-pack targets in one
 * package — `web` (browsers/bundlers, ESM with a default init) and `node`
 * (CommonJS, loads the binary from disk). Consumers:
 *
 *   import init, { compile, execute } from "@axiom-foundation/rules-engine-wasm";
 *   const engine = require("@axiom-foundation/rules-engine-wasm/node");
 *
 * Usage (from wasm/):  node npm/build-npm.mjs [version]
 * Output:              wasm/npm-dist/  (publish with: npm publish npm-dist/)
 *
 * No dependencies; needs wasm-pack and a Rust toolchain on PATH.
 */

import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const wasmDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(wasmDir, "npm-dist");

const cargoToml = readFileSync(join(wasmDir, "Cargo.toml"), "utf8");
const crateVersion = /\nversion = "([^"]+)"/.exec(cargoToml)?.[1];
const version = process.argv[2] ?? crateVersion;
if (!version) throw new Error("No version: pass one or set it in Cargo.toml");

for (const [target, outDir] of [
  ["web", "pkg-npm-web"],
  ["nodejs", "pkg-npm-node"],
]) {
  console.log(`wasm-pack build --target ${target}…`);
  execFileSync(
    "wasm-pack",
    ["build", "--release", "--target", target, "--out-dir", outDir, "--no-pack"],
    { cwd: wasmDir, stdio: "inherit" },
  );
}

rmSync(dist, { recursive: true, force: true });
mkdirSync(join(dist, "web"), { recursive: true });
mkdirSync(join(dist, "node"), { recursive: true });

const KEEP = /\.(js|d\.ts|wasm)$/;
for (const [src, dest] of [
  ["pkg-npm-web", "web"],
  ["pkg-npm-node", "node"],
]) {
  cpSync(join(wasmDir, src), join(dist, dest), {
    recursive: true,
    filter: (path) => !path.includes(".") || KEEP.test(path) || path.endsWith(src),
  });
}
cpSync(join(wasmDir, "LICENSE"), join(dist, "LICENSE"));
cpSync(join(wasmDir, "npm", "README.md"), join(dist, "README.md"));

const glue = "axiom_rules_engine_wasm";
writeFileSync(
  join(dist, "package.json"),
  JSON.stringify(
    {
      name: "@axiom-foundation/rules-engine-wasm",
      version,
      description:
        "The Axiom rules engine compiled to WebAssembly: compile and execute RuleSpec law encodings in the browser or Node, no server involved.",
      license: "Apache-2.0",
      repository: {
        type: "git",
        url: "git+https://github.com/TheAxiomFoundation/axiom-rules-engine.git",
        directory: "wasm",
      },
      homepage: "https://github.com/TheAxiomFoundation/axiom-rules-engine/tree/main/wasm",
      keywords: ["rulespec", "rules-engine", "wasm", "law", "benefits", "tax"],
      main: `./node/${glue}.js`,
      module: `./web/${glue}.js`,
      types: `./web/${glue}.d.ts`,
      exports: {
        ".": {
          types: `./web/${glue}.d.ts`,
          node: `./node/${glue}.js`,
          default: `./web/${glue}.js`,
        },
        "./node": {
          types: `./node/${glue}.d.ts`,
          default: `./node/${glue}.js`,
        },
        "./web/*": "./web/*",
      },
      files: ["web", "node", "README.md", "LICENSE"],
      sideEffects: false,
    },
    null,
    2,
  ),
);

// Scoped module-type markers: the web build is ESM, the node build is
// CommonJS — a root "type" would misdeclare one of them.
writeFileSync(join(dist, "web", "package.json"), JSON.stringify({ type: "module" }, null, 2));
writeFileSync(join(dist, "node", "package.json"), JSON.stringify({ type: "commonjs" }, null, 2));

console.log(`assembled ${dist} as @axiom-foundation/rules-engine-wasm@${version}`);
