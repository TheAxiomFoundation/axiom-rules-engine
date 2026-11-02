# Frozen format-2 artifacts

These files stand for format-2 artifacts already published. They were written
by the unmodified library at `main` 5d63e8f3 (crate 0.2.2, artifact format 2)
on 2026-10-10, before format 3. The same steps run against `main` at 621e0f24
(2026-10-13), the last `main` commit before format 3, wrote every file here
byte for byte the same, so each is what `main` wrote at both commits. `main`
at 841b9b65 (2026-10-26), still format 2, again wrote all twenty byte for
byte, and wrote the two fixtures `main` added after 621e0f24:
`tests/fixtures/only_related.yaml` (#261) and
`tests/fixtures/selected_year_input.yaml` (#263). Never
regenerate them with a newer engine: a regenerated file would test the engine
against itself.

## `compiled/`

Real compiler output. Each file is what
`CompiledProgramArtifact::from_rulespec_str` followed by `write_json_file`
wrote at 5d63e8f3 for the RuleSpec fixture its name spells, with `/` written
as `__` and `.yaml` replaced by `.compiled.json`
(`tests__fixtures__rulespec__other__flat_tax__rules.compiled.json` is
`tests/fixtures/rulespec/other/flat_tax/rules.yaml`). They cover every
in-repo RuleSpec fixture that compiles; the twenty-third,
`tests/fixtures/artifacts/relation_typing/inconsistent_filter_contests_slot.rulespec.yaml`,
is ill-typed on purpose and has no artifact.

At both commits, `CompiledProgramArtifact::from_json_str` followed by
`serde_json::to_string_pretty` reproduced each of these files byte for byte,
so each is also the expected output of the format-2 compatibility loader.

## `legacy-reload/`

What the loader at 5d63e8f3, and again at 621e0f24, returned for format-2
payloads in older emission shapes.

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
  `serde_json::to_string_pretty` returned for that input at 5d63e8f3 and at
  621e0f24.
- `reloaded/inconsistent_filter_contests_slot.v0.2.2-6031295.json.error.txt`
  is the error text the loader returned at both commits for
  `tests/fixtures/artifacts/relation_typing/inconsistent_filter_contests_slot.v0.2.2-6031295.json`.

The loader at both commits also reloaded
`tests/fixtures/artifacts/related_derived_sums.pre-lazy-fast-path.compiled.json`
byte for byte. `tests/artifact_format.rs` checks that the format-2
compatibility loader returns exactly these results.
