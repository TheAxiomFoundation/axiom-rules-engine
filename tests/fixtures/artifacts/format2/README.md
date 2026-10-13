# Frozen format-2 artifacts

These files stand for format-2 artifacts already published. They were written
by the unmodified library at `main` 5d63e8f3 (crate 0.2.2, artifact format 2)
on 2026-10-10, before format 3. Never regenerate them with a newer engine: a
regenerated file would test the engine against itself.

## `compiled/`

Real compiler output. Each file is what
`CompiledProgramArtifact::from_rulespec_str` followed by `write_json_file`
wrote at 5d63e8f3 for the RuleSpec fixture its name spells, with `/` written
as `__` and `.yaml` replaced by `.compiled.json`
(`tests__fixtures__rulespec__other__flat_tax__rules.compiled.json` is
`tests/fixtures/rulespec/other/flat_tax/rules.yaml`). They cover every
in-repo RuleSpec fixture that compiles; the twenty-first,
`tests/fixtures/artifacts/relation_typing/inconsistent_filter_contests_slot.rulespec.yaml`,
is ill-typed on purpose and has no artifact.

At 5d63e8f3, `CompiledProgramArtifact::from_json_str` followed by
`serde_json::to_string_pretty` reproduced each of these files byte for byte,
so each is also the expected output of the format-2 compatibility loader.

## `legacy-reload/`

What the 5d63e8f3 loader returned for format-2 payloads in older emission
shapes.

- `inputs/dialect-*.json` are CONSTRUCTED payloads, not compiler output. Each
  is `compiled/tests__fixtures__rulespec__other__flat_tax__rules.compiled.json`
  edited to reproduce a shape older engines wrote:
  - `dialect-null-extends.json`: `"extends": null` in `program` (the v0.1
    maintenance line serializes it unconditionally);
  - `dialect-absent-catalog.json`: no `metadata.input_catalog` (engines that
    predate the catalog);
  - `dialect-duplicate-parameter.json`: a compatible duplicate parameter
    declaration (published us-tn-snap carries them);
  - `dialect-v0.1-maintenance.json`: all three at once;
  - `dialect-absent-engine-version.json`: no `engine_version`.
- `reloaded/<input>.reloaded.json` is real output: what
  `CompiledProgramArtifact::from_json_str` followed by
  `serde_json::to_string_pretty` returned for that input at 5d63e8f3.
- `reloaded/inconsistent_filter_contests_slot.v0.2.2-6031295.json.error.txt`
  is the error text the 5d63e8f3 loader returned for
  `tests/fixtures/artifacts/relation_typing/inconsistent_filter_contests_slot.v0.2.2-6031295.json`.

The 5d63e8f3 loader also reloaded
`tests/fixtures/artifacts/related_derived_sums.pre-lazy-fast-path.compiled.json`
byte for byte. `tests/artifact_format.rs` checks that the format-2
compatibility loader returns exactly these results.
