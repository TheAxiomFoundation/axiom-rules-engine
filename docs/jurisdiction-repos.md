# Jurisdiction Repositories

Canonical rule content belongs in jurisdiction repositories. The engine repo is
runtime and schema infrastructure only; checked-in policy content belongs in
`rules-*` repositories.

## Repository Layout

Each repository represents one jurisdiction. Every jurisdiction should use the
same top-level taxonomy:

```text
us/
  statutes/
  regulations/
  policies/
  sources/

us-tn/
  statutes/
  regulations/
  policies/
  sources/
```

State repositories use `statutes/` for state statutes. Federal authorities stay
in `us/statutes/...` or `us/regulations/...` and are referenced by absolute
cross-repo paths.

Executable RuleSpec modules can compose those authorities with top-level
`imports`. Use canonical imports for cross-repo dependencies and relative
imports for files in the same repository:

```yaml
format: rulespec/v1
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
  - us-co:regulations/10-ccr-2506-1/4.207.3
rules: []
```

Import targets follow the same path identity scheme as rule IDs, without the
optional `#rule_name` suffix. The runtime resolves `us:` to `rules-us`,
`us-tn:` to `rules-us-tn`, etc., using sibling checkouts and
`AXIOM_RULE_REPO_ROOTS`.

Rule files are named by the legal or policy unit they encode. Companion tests use
the same stem:

```text
us/
  statutes/7/2014/e/6/A.yaml
  statutes/7/2014/e/6/A.test.yaml
  regulations/7-cfr/273/9/d/6.yaml
  regulations/7-cfr/273/9/d/6.test.yaml
  policies/irs/pub/501.yaml
  policies/irs/pub/501.test.yaml
```

## Path Identity

The file path is the canonical ID. Do not duplicate it in an `id:` field by
default.

```text
us:statutes/7/2014/e/6/A
us-tn:policies/dhs/snap/manual/23/L
```

These IDs derive from:

```text
<repo>:<relative path without extension>
```

For source registry files under `sources/`, the `sources/` prefix is removed:

```text
us-tn/sources/policies/dhs/snap/manual/23/L.yaml
```

has source identity:

```text
us-tn:policies/dhs/snap/manual/23/L
```

Use `aliases:` only for external citations or other non-canonical identifiers
that must remain resolvable.

## Source Registry

Git stores source metadata and expected hashes. R2 stores the actual artifacts.
`sources/` mirrors the root rule tree:

```text
us-tn/
  policies/dhs/snap/manual/23/L.yaml
  policies/dhs/snap/manual/23/L.test.yaml
  sources/policies/dhs/snap/manual/23/L.yaml
```

Default source registry shape:

```yaml
publisher: Tennessee DHS
canonical_url: https://...
retrieved_at: 2026-04-25T00:00:00Z
hashes:
  raw_sha256: ...
  text_sha256: ...
```

Do not include `id:` or `storage:` by default. Identity and storage paths are
derived from the repository and filepath.

## Deterministic R2 Paths

The default R2 path is:

```text
r2://axiom-sources/<repo>/<relative source identity>/<artifact>
```

Example:

```text
Git:
us-tn/sources/policies/dhs/snap/manual/23/L.yaml

R2:
r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/raw
r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/text
```

R2 may store actual hashes in object metadata for fast validation. Git still
stores expected hashes so a reviewed rule can prove which exact source artifacts
it was reviewed against. Validation derives the R2 path, reads object metadata
or bytes, and compares actual hashes to the Git-declared expected hashes.

Validate registry files with:

```bash
PYTHONPATH=python python3 -m axiom_rules.cli check-sources /path/to/us-tn --verbose
```

The validator rejects duplicated `id:` fields, top-level `storage:` fields,
missing expected hashes, non-taxonomy source paths, and non-absolute graph-edge
targets. By default it derives source IDs from `<repo>:<sources-relative-path>`
and R2 paths from `r2://axiom-sources/<repo>/<source-path>/<artifact>`.

To verify live R2 objects as well:

```bash
AXIOM_R2_ACCOUNT_ID=...
AXIOM_R2_ACCESS_KEY_ID=...
AXIOM_R2_SECRET_ACCESS_KEY=...
PYTHONPATH=python python3 -m axiom_rules.cli check-sources /path/to/us-tn --verify-r2
```

`AXIOM_R2_ENDPOINT_URL` can be used instead of `AXIOM_R2_ACCOUNT_ID`. Live
verification checks that each derived R2 object exists, streams its bytes, and
compares the actual SHA-256 with the Git-declared expected hash.

## Artifact Overrides

Use explicit artifact metadata only for exceptions:

```yaml
publisher: Tennessee DHS
canonical_url: https://...
retrieved_at: 2026-04-25T00:00:00Z
artifacts:
  raw:
    path: manual.pdf
    sha256: ...
    media_type: application/pdf
  text:
    path: manual.txt
    sha256: ...
    media_type: text/plain
```

Exceptions include multiple source files for one unit, nonstandard filenames,
page ranges, historical snapshots, alternate official URLs, or manually curated
OCR text corrections.

## Upstream Relationships

State policy files can point to upstream federal authorities through graph-level
metadata such as `sets`, `implements`, `extends`, or `authority`. Those edges
should point to absolute canonical paths, for example:

```text
us-tn:policies/dhs/snap/manual/23/L
sets
us:statutes/7/2014/e/6/A
```

These graph edges are source/provenance metadata. They are not duplicated inside
the executable RuleSpec formula unless the engine needs them for calculation.
