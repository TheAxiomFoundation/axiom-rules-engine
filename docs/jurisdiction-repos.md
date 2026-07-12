# Jurisdiction Repositories

Canonical rule content belongs in jurisdiction repositories. The engine repo is
runtime and schema infrastructure only; checked-in policy content belongs in
`rulespec-*` repositories.

## Repository Layout

Each repository represents one country and is named exactly
`rulespec-<country>`. Direct jurisdiction directories use the same five-root
filesystem taxonomy:

```text
us/
  legislation/
  policies/
  programs/
  regulations/
  statutes/

us-tn/
  legislation/
  policies/
  programs/
  regulations/
  statutes/
```

The four atomic `rulespec/v1` roots are `legislation/`, `policies/`,
`regulations/`, and `statutes/`. `programs/` contains declarative ProgramSpecs
for `axiom-compose` and must never be loaded as an atomic RuleSpec module.

State repositories use `statutes/` for state statutes. Federal authorities stay
in `us/statutes/...` or `us/regulations/...` and are referenced by absolute
cross-repo paths.

Executable RuleSpec modules compose authorities with top-level `imports`.
Every import is an exact absolute canonical target:

```yaml
format: rulespec/v1
imports:
  - us:policies/usda/snap/fy-2026-cola/maximum-allotments
  - us-co:regulations/10-ccr-2506-1/4.207.3
rules: []
```

Import targets follow the same path identity scheme as rule IDs, without the
optional `#rule_name` suffix. Supply the exact country checkout explicitly:

```bash
axiom-rules-engine compile \
  --program /srv/rulespec-us/us-tn/policies/example.yaml \
  --rulespec-root /srv/rulespec-us \
  --output /tmp/example.compiled.json
```

The runtime resolves `us:` to `/srv/rulespec-us/us/` and `us-tn:` to
`/srv/rulespec-us/us-tn/`. It never discovers or prefers standalone,
suffixed-worktree, sibling, ancestor, cwd, or environment-provided roots.

Rule files are named by the legal or policy unit they encode. Companion tests use
the same stem and are never importable module targets:

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

The file path is the canonical module ID. `module.id` has been removed and is
rejected; the configured path or `ModuleSource` target is the sole identity.

```text
us:statutes/7/2014/e/6/A
us-tn:policies/dhs/snap/manual/23/L
```

These IDs derive from:

```text
<jurisdiction>:<atomic-root>/<relative path without extension>
```

External citation aliases belong in the pinned `axiom-corpus` release or other
graph metadata, not in a second module-identity field.

## Corpus Provenance

Legal source artifacts and validation belong to `axiom-corpus`, not to a
parallel `sources/` tree in RuleSpec checkouts. Each module optionally carries
an exact `module.source_verification` mapping with one required singular
`corpus_citation_path` and an optional `source_sha256`. RuleSpec repositories
pin an immutable named corpus release in `.axiom/toolchain.toml`; the corpus
release owns source acquisition, hashes, aliases, and publication validation.

## Upstream Relationships

State policy files can point to upstream federal authorities through graph-level
metadata such as `sets`, `implements`, or `authority`. Those edges
should point to absolute canonical paths, for example:

```text
us-tn:policies/dhs/snap/manual/23/L
sets
us:statutes/7/2014/e/6/A
```

These graph edges are source/provenance metadata. They are not duplicated inside
the executable RuleSpec formula unless the engine needs them for calculation.
