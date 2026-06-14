# Decisions

Short decision log for architecture choices. Publicly and internally, this is
the Axiom Rules Engine; the Rust crate and executable are `axiom-rules-engine`. One
entry per decision, most recent first.

## 2026-06-10 — Browser execution is a first-class target; the wasm boundary is the CLI's JSON

**Decision.** `wasm/` hosts a sibling crate (the `python-ext/` convention),
`axiom-rules-engine-wasm`: a cdylib over wasm-bindgen exposing exactly four
functions — `compile(modules_json, root_target)`,
`execute(artifact_json, request_json)`, `engine_version()`, and
`artifact_format_version()`. The boundary is JSON strings in both directions,
reusing the CLI's serde types unchanged: `compile` takes a
`{canonical_target: yaml_text}` map (served to an in-memory `ModuleSource`)
and returns a `CompiledProgramArtifact`; `execute` takes a
`CompiledExecutionRequest` and returns an `ExecutionResponse`. The crate
depends on the core with `default-features = false`. wasm-pack builds two
targets: `--target web` (browser ESM) into `wasm/pkg-web/` and
`--target nodejs` into `wasm/pkg-node/`, which a Node smoke test
(`wasm/test/smoke.mjs`) runs in CI's `wasm-pkg` job — no browser required.

**Why.**

- Browser execution keeps household PII on-device with zero round-trip.
  `ModuleSource` made the core pure over (modules, dataset); this crate is
  the host layer that actually ships that purity to a page.
- JSON strings, not structured `JsValue` marshalling (serde-wasm-bindgen),
  keep one canonical wire format: any payload that works against the CLI
  works unchanged against the wasm build, artifacts cache and transfer as
  plain text, and the JS side needs no generated type definitions to drift.
- `engine_version()` / `artifact_format_version()` give UIs provenance to
  display alongside any result they render, matching the fields stamped
  into artifacts.

**Consequences.**

- The core gains a one-line `ENGINE_VERSION` const so bindings can report
  the core crate's version; nothing else in the core changes.
- Determinations computed in the browser are byte-identical to CLI output
  for the same artifact and request; the smoke test pins this with the
  fixture pair from `tests/module_source.rs`.
- JS-side ergonomics (typed wrappers, Web Worker hosting, module fetching)
  live with consuming apps, not in this crate; the exported surface stays
  four functions.
## 2026-06-10 — Source pinning, encoding provenance, and validation status are module content

**Decision.** The RuleSpec `module:` block grows three optional, inert
fields: `source_verification.source_sha256` (the SHA-256 hex digest of the
exact corpus provision text the module was encoded from),
`encoding_provenance` (optional `encoder`, `model`, `run_id`, `reviewed_by`
strings; unknown subfields rejected), and `validation` (a list of
`{oracle, status, last_run}` records with `status` one of
`matches` / `mismatches` / `pending`). The engine validates shape at load —
a malformed sha fails with an error naming the module file — carries the
merged root module's metadata on `ProgramSpec.module`, and passes it
through compiled-artifact JSON. Nothing in lowering, compilation, or
execution reads any of it.

**Why.**

- Modules ground to legal text through
  `module.source_verification.corpus_citation_path`, but not to a content
  hash of the exact text version encoded. When eCFR republishes a section
  there is no mechanical "this module is stale" signal; pinning
  `source_sha256` makes staleness a hash comparison
  (`axiom-encode check-source-staleness`).
- Encoding provenance (tool, model, run id, reviewer) lives in a
  side-channel telemetry DB today. Provenance should travel with the
  content it describes so review and audit need only the module file.
- Oracle-validation status lives nowhere in content; consumers cannot tell
  a validated module from a pending one without external systems.

**Consequences.**

- Every field is optional; absent means exactly today's behavior. Existing
  modules and fixtures are untouched, and artifacts without module
  metadata serialize byte-identically to before.
- Artifacts gain an optional `program.module` pass-through. No
  `ARTIFACT_FORMAT_VERSION` bump: the field is additive, ignored by older
  engines, and evaluation semantics are unchanged.
- Malformed `source_sha256` values (not 64 hex characters), unknown
  `encoding_provenance` subfields, and unknown `validation[].status`
  values are rejected at load instead of passing silently.
- Unmodeled `source_verification` subfields (for example
  `corpus_citation_paths`) are preserved verbatim for tooling.
- axiom-encode owns stamping the blocks at encode time and the staleness
  checker that compares pinned hashes against the current corpus.

## 2026-06-10 — Module resolution is a host concern behind `ModuleSource`

**Decision.** The core engine is a pure function over (modules, dataset):
lowering, compilation, and execution never touch a filesystem, environment
variable, or wall clock. Finding module text for a canonical target
(`us:statutes/7/2015/e`) goes behind the `source::ModuleSource` trait
(`load(target) -> Result<Option<String>, SourceError>`);
`load_rulespec_with_source` and `CompiledProgramArtifact::from_rulespec_with_source`
are the pure entry points. Resolving relative imports to canonical targets is
pure string logic on the importer's canonical target and stays in the core
(`resolve_import_target`). The existing filesystem resolution
(sibling checkouts, country monorepos, `AXIOM_RULESPEC_REPO_ROOTS`) becomes
`FsModuleSource` plus the unchanged `*_file` APIs, all behind a default-on
`fs` feature; the CLI binary requires it. wasm32-unknown-unknown is a
supported check target: CI runs
`cargo check --target wasm32-unknown-unknown --no-default-features`.

**Why.**

- Running benefit calculations in the browser keeps household PII on-device —
  zero round-trip. That requires the core to compile for wasm32 with no
  filesystem assumptions.
- Servers with modules in memory, registry clients, and test harnesses all
  want to supply module text directly instead of staging checkouts on disk.
- Splitting "resolve an import to a canonical target" (pure, in core) from
  "find and read the text" (host) keeps durable ids identical across hosts:
  an in-memory host and a checkout produce byte-identical programs.

**Consequences.**

- Default builds are unchanged: `fs` is on, the CLI, `load_rulespec_file`,
  `from_rulespec_file`, and artifact file I/O behave exactly as before.
- With `--no-default-features` the crate has no `std::fs` / `std::env` usage
  and no clock reads; chrono is pinned `default-features = false` (no `clock`
  feature) so wall-clock reads cannot creep into core paths unnoticed.
- Hosts own availability semantics: `Ok(None)` means "no such module"
  (reported with importer context), `Err(SourceError)` means the host failed.
- Module identity, import cycles, and deduplication key on canonical targets
  rather than canonicalized paths in the source-driven loader.

## 2026-06-10 — Reserve the assessment-time axis before content depends on it

**Decision.** The engine is bitemporal by design: valid time (the benefit
period the law governs — the existing `period` / `effective_from` axis) and
decision/assessment time (when the determination is made) are separate axes.
`ExecutionQuery` reserves an optional `assessment_date` on both the direct and
compiled request paths, mirrored in the Python models and echoed on each
`QueryResult`. For now it is parsed and validated only — it must be on or
after `period.start` — and has no effect on evaluation. RuleSpec versions will
grow optional `enacted_on` / `known_from` alongside `effective_from`;
`assessment_date` will select which enactments are visible while `period`
keeps selecting which visible version applies. `docs/bitemporal.md` is the
full design.

**Why.**

- Retroactive amendments (ARPA's 2021-03-11 enactment effective for tax year
  2020), retroactive COLA corrections, SNAP retro-certification and restored
  benefits (7 CFR 273.10, 273.17), and appeals all separate "law in force
  during the period" from "law as known at assessment". One axis cannot
  represent a correct-when-made determination once an encoding absorbs a
  retroactive enactment.
- Among versions sharing an `effective_from`, selection currently falls
  through to document order — an accident of file layout. The assessment axis
  turns that into a principled rule: the latest-enacted visible version wins.
- Reserving the query field now lets requests, stored results, and callers
  carry assessment dates before any encoding depends on the semantics, instead
  of retrofitting the wire format later.

**Consequences.**

- Absent fields mean "known since forever": versions without
  `enacted_on`/`known_from` are visible at every assessment date, so selection
  reduces exactly to today's `effective_from` rule. Existing encodings,
  requests, artifacts, and responses are unchanged; unset fields are omitted
  from the wire.
- Queries with `assessment_date` before the period start are rejected;
  assessing a period before it begins is projection, which stays out of scope.
- When the engine starts honoring visibility dates, `ARTIFACT_FORMAT_VERSION`
  must bump (per the 2026-06-09 decision) and the evaluation cache key gains
  an assessment dimension.
- Explicitly out of scope: retro recalculation workflow, cross-period
  corrections/claims ledgers, and knowledge time for input data.

## 2026-06-09 — Compiled artifacts carry a format version and are bounded subprocesses

**Decision.** `CompiledProgramArtifact` stamps `artifact_format_version`
(currently 1) and `engine_version` at compile time. Loading rejects artifacts
whose format version is newer than the engine supports; artifacts without the
field (version 0, pre-stamping) still load. The Python wrapper bounds every
engine subprocess with a configurable timeout (default 600 s, `None` to
disable).

**Why.**

- Artifacts are durable and ship to consumers (finbot, microsim, demos).
  Without a version field, an engine reading an artifact from a different
  generation fails late or silently miscalculates; with one, mismatches fail
  loudly at load.
- A pathological or hung engine process previously blocked Python callers
  forever; web apps and batch microsim runs sit on that path.

**Consequences.**

- New artifacts include two extra JSON fields; legacy artifacts deserialize
  with `artifact_format_version: 0` and `engine_version: null`, so nothing
  shipped breaks.
- Future IR-breaking changes must bump `ARTIFACT_FORMAT_VERSION` so older
  engines reject newer artifacts instead of guessing.
- The `compile` CLI summary now reports both versions.
- Callers that legitimately run longer than 600 s must pass an explicit
  `timeout` (or `None`).

## 2026-05-20 — Filtered entities lower as derived runtime relations

**Decision.** RuleSpec models filtered entity membership with
`kind: derived_relation`: a runtime relation derived from a source relation and
a judgment predicate. A derived relation may declare an entity name and a member
relation alias so rules can be scoped to the filtered view, for example
`entity: SnapUnit` with `formula: len(members)`.

**Why.**

- Legal constructs such as SNAP units, MAGI households, and qualifying-child
  sets are filtered membership sets, not household-level booleans.
- The existing runtime already understands relation aggregation. Extending
  relations preserves that model and avoids inventing a second collection
  mechanism.
- A filtered entity instance is keyed by the source relation's current entity
  id, which keeps execution compatible with existing query shapes.

**Consequences.**

- `len`, `sum`, `count_where`, and `sum_where` operate over filtered
  membership in explain mode, bulk fast mode, and dense mode for supported
  predicate shapes.
- The compiler rejects derived-relation cycles.
- Membership predicates can combine related entity rules with current/root
  entity rules, and a derived relation can use another derived relation as its
  source.
- Filtered entity ids are not separately materialized; a filtered entity such as
  `SnapUnit` is keyed by the source/current entity id, for example
  `household-1`.
- Jurisdiction repos must migrate their own SNAP approximations separately; this
  engine change only provides the runtime feature.

## 2026-05-04 — Runtime predicates and source relations are separate RuleSpec kinds

**Decision.** RuleSpec has separate record kinds for executable data predicates
and legal/provenance graph edges:

- `data_relation` declares runtime predicates such as `member_of_household`.
- `source_relation` declares non-executable source edges such as `restates`,
  `implements`, `sets`, `amends`, and `cites`.

`kind: reiteration`, `kind: relation`, and top-level RuleSpec `relations:` are
not accepted. Restatements are represented as `kind: source_relation` with
`source_relation.type: restates`.

**Why.**

- A runtime predicate and a legal authority edge have different schemas and
  lowering behavior.
- Keeping provenance out of `ProgramSpec` gives the runtime a clean executable
  program while still letting the harness verify upstream-first encoding,
  delegated settings, amendments, and restatements.
- `restates` is one member of a broader source-relation taxonomy; it should not
  be hard-coded as its own rule kind.

**Consequences.**

- `parameter`, `derived`, and `data_relation` lower into runtime.
- `source_relation` is validated but ignored during runtime lowering.
- Public relation dataset references remain durable ids of the form
  `<file>#relation.<local predicate>`.

## 2026-04-25 — RuleSpec is the only external rule format

**Decision.** The canonical authoring and interchange surface is RuleSpec
YAML/JSON: structured rule metadata with concise formula strings. Authoring
tools write RuleSpec, the Axiom app visualises RuleSpec and compiled traces, and
the Rust engine normalises RuleSpec into `ProgramSpec` before compilation.

`ProgramSpec` is the engine IR, not the author schema. It remains useful inside
compiled artifacts and tests, but rule files accepted by the compile path
must be explicit RuleSpec (`format: rulespec/v1` or `schema: axiom.rules.*`).

**Why.**

- Machine authors need an unambiguous, schema-valid target more than a
  hand-written DSL.
- The Axiom app can provide human visualisers for rule graphs, provenance, and traces,
  so raw source readability is secondary to faithful generation and validation.
- A structured schema can represent provenance, source-document anchors,
  jurisdiction/repo ownership, temporal versions, rule kind, relation
  orientation, and future hard gaps without overloading expression syntax.
- Concise formula strings keep common calculations compact while the surrounding
  YAML/JSON keeps metadata machine-checkable.

**Consequences.**

- `axiom-rules-engine compile` accepts RuleSpec YAML only.
- Ambiguous YAML with a top-level `rules:` key and no discriminator is rejected.
- The formula parser is an internal implementation module for RuleSpec formula
  fields, not a separate rule format.
- Old experiments should be recovered from Git history, not preserved in active
  code.

## 2026-04-25 — Jurisdiction repo paths are canonical IDs

**Decision.** Production rule content lives in jurisdiction repositories using
the same top-level taxonomy in every repo:

- `statutes/`
- `regulation/`
- `policy/`
- `sources/`

The canonical rule ID is the filepath, not an `id:` field:

- `us:statutes/7/2014/e/6/A`
- `us-tn:policy/dhs/snap/manual/23/L`

Rule files use the legal-unit stem, with companion tests beside them:

- `statutes/7/2014/e/6/A.yaml`
- `statutes/7/2014/e/6/A.test.yaml`

`sources/` mirrors the root rule tree and stores source-registry metadata. The
registry path also defines identity; remove the `sources/` prefix when deriving
the source ID. R2 object paths are deterministic from repo + relative source
path, so source registry files do not include `storage:` or `id:` by default.
They do include expected hashes in Git.

**Why.**

- Filepaths are already the reviewable, mergeable namespace.
- Explicit IDs and storage paths create drift risk when they repeat the path.
- Git needs expected hashes to prove which exact source artifacts a rule was
  reviewed against; R2 metadata only tells us what is stored now.
- Mirroring `sources/` to `statutes/`, `regulation/`, and `policy/` gives simple
  path-addressable joins between source material and executable rules.

**Consequences.**

- Source registry files default to metadata and hashes:
  `publisher`, `canonical_url`, `retrieved_at`, and `hashes`.
- Explicit `artifacts:` metadata is reserved for exceptions such as multiple
  files, nonstandard artifact names, page ranges, historical snapshots,
  alternate official URLs, or curated OCR text corrections.
- Jurisdiction repos should use legal-unit paths like
  `policy/dhs/snap/manual/23/L.yaml`.
- See `docs/jurisdiction-repos.md` for the concrete layout.

## 2026-04-19 — Rule content lives in jurisdiction repos

**Decision.** Production encodings live in the jurisdiction repo they belong to.
The engine repo keeps RuleSpec YAML only as parser/execution fixtures under
`tests/fixtures/rulespec/`. Canonical jurisdiction repositories use `statutes/`,
`regulation/`, `policy/`, and `sources/` paths.

The engine resolves `extends:` and RuleSpec imports by filesystem path; any
mounted layout works.

**Why.**

- Keeps the engine repo focused on runtime and schema, not content.
- Per-jurisdiction repos have their own release cadence, reviewers, and license
  boundaries.

**Consequences.**

- `axiom-rules-engine` has no checked-in production policy content.
- Engine tests can keep a small set of RuleSpec fixtures for parser, compiler,
  and execution coverage.

## 2026-04-19 — `sets` and `amends` are graph-level metadata

**Decision.** State-delegation (`sets`) and regulation-amends-statute
(`amends`) edges stay in source/provenance graph metadata, not inside executable
RuleSpec formulas. The engine reads merged RuleSpec / `ProgramSpec`; graph-level
facts are consumed by validators, the Axiom app, and trace renderers.

**Why.**

- Overloading executable rules with graph metadata makes them harder to diff and
  harder to review.
- Multi-source citations on a derived output are an engine feature, but they are
  not the same thing as graph-level `sets` / `amends` edges.

**Consequences.**

- No engine execution change is required for `sets` / `amends`.
- A follow-up can teach explain traces to pull graph metadata for rendering.
- The `source` / `source_url` fields on derived outputs should become arrays to
  support multi-document provenance.

## 2026-06-14 — Parameter `sets` lower into executable bindings

**Decision.** `source_relation.type: sets` remains graph/provenance metadata,
but parameter-to-parameter `sets` records also lower into executable parameter
bindings when both sides are present in the merged RuleSpec program. The
upstream delegated slot stays addressable as the federal parameter; the
downstream `source_relation.value` parameter supplies that slot's versions.

**Why.**

- Federal formulas should encode the legal structure once, with state modules
  setting delegated standards rather than duplicating federal eligibility
  formulas.
- Keeping the upstream parameter name as the runtime slot preserves stable
  formula references and public IDs.
- The `sets` record still carries the legal edge needed for audit and trace
  rendering.

**Consequences.**

- Upstream sources that delegate a reformable setting should expose that setting
  as a `kind: parameter` slot.
- Downstream state modules can import the upstream formula, define the local
  value parameter, and bind it with `source_relation.type: sets`,
  `source_relation.target`, `source_relation.value`, and
  `source_relation.basis.delegation`.
- Non-parameter source relations, amendments, restatements, and source graph
  edges without a local `value` remain metadata-only.
