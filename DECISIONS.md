# Decisions

Short decision log for architecture choices. Publicly and internally, this is
the Axiom Rules Engine; the Rust crate and executable are `axiom-rules-engine`. One
entry per decision, most recent first.

## 2026-07-05 — Lifetime execution surface: over-periods reductions and their semantics

**Decision.** A `derived` formula may reduce a value across an entity's own
period axis with one of four builtins — `sum_over_periods(x)`,
`max_over_periods(x)`, `count_over_periods(x)`, and
`sum_top_n_over_periods(x, n)` — which lower to a single additive expression
node (`ScalarExpr::OverPeriods { kind, value, n }`, serde-tagged, no artifact
format-version bump). They evaluate only under a new lifetime execution surface
(`DenseCompiledProgram::execute_lifetime` / `execute_lifetime_f64`), which takes
one input batch per supplied period, all describing the same entity rows in the
same positional order. The per-period execution paths reject every reduction
node (`EvalError::OverPeriodsOutsideLifetime`). The following semantics are
settled:

- **Reference period = the chronologically-last supplied period.** The entry
  points require the supplied periods to be **strictly ascending by start
  date** and error otherwise (`EvalError::LifetimePeriodsNotAscending`, naming
  the offending adjacent pair) rather than silently sorting — so the caller's
  period/batch pairing stays authoritative and a bend point / COLA parameter
  cannot resolve at the wrong year. Every period-specific scalar combined with a
  reduction (a `ParameterLookup`, and the `n` of `sum_top_n_over_periods`)
  resolves at that reference period.

- **Referential transparency (period-invariant input binding + inlined-body
  derived).** Inside any reduction, the inner expression is evaluated once per
  period with the ordinary per-period executor. Outside every reduction: a
  `ParameterLookup` resolves at the reference period; a bare **input** is bound
  only when it is provably identical across all supplied periods for each row
  (`eval_period_invariant_input`), else `EvalError::LifetimePeriodVaryingInput`
  names the input and the first divergence — this legalizes exactly the
  per-person-constant inputs (year attained 21 / 62) that a real
  computation-year count bottoms out in, and nothing else; and a **derived**
  reference means its body inlined and evaluated in this same lifetime context,
  so a derived denotes exactly its definition whether referenced inside or
  outside a reduction. A parameter-bearing derived reused in both contexts with
  a period-varying input inside it therefore errors loudly instead of silently
  computing two different values.

- **`count_over_periods` counts nonzero.** It evaluates its argument per period
  (the same leaf rules as the other reductions) and counts, per row, the
  periods whose value is nonzero (a `Bool` inner value counts `true`). It is not
  a bare supplied-period count; a text/date inner value has no zero and is
  rejected.

- **Strict `n` contract for `sum_top_n_over_periods`.** After truncation toward
  zero to an exact `i64`, `n` must satisfy `1 <= n <= the supplied period
  count`. A non-finite or out-of-range `n` (`try_to_i64_trunc` -> `None`), an
  over-length `n`, and an `n` that is not period-invariant all raise typed
  errors (`OverPeriodsTopNOutOfRange`, `OverPeriodsTopNPeriodVarying`) — no
  clamp to `i64::MAX`, no silent pin to the reference period, no zero-padding
  past the period count. Parameter-sourced and input-sourced `n` are held to the
  identical contract (both evaluated under every period's executor and required
  invariant). Summing more slots than periods only pads with zeros — an
  arithmetic no-op — so an over-length count is treated as a data error, not
  absorbed.

**Why.**

- 42 USC 415(b)'s AIME is the motivating shape: sum the top `n` of a worker's
  indexed annual earnings, where `n` (the benefit-computation-year count) is
  itself derived from per-person-constant inputs. That needs a period axis the
  per-period executor does not have, an `n` reached through a derived chain
  outside every reduction, and a floor-divided quotient — all of which the
  surface above supports end to end.
- The reductions must be *referentially consistent*: a named derived reused
  inside and outside a reduction, or a `count`/`n` whose value silently depends
  on evaluation position, would compute wrong money without any signal. Each
  ambiguous case is resolved by a single documented rule and a loud typed error
  when the inputs violate it, rather than a clamp or a positional default.

**Consequences.**

- **Additive; no artifact format-version bump.** The `over_periods` variant is
  additive on `ScalarExprSpec`; existing compiled artifacts and per-period
  execution are unchanged. Reductions are rejected in every non-lifetime path
  (per-period dense, sparse, bulk) with a clear error.
- **Lifetime outputs must reduce.** `execute_lifetime[_f64]` only supports
  outputs whose formula contains at least one over-periods reduction
  (`LifetimeOutputWithoutReduction`); period-specific outputs still use the
  per-period entry points.
- **Directly-nested reductions are a compile error**
  (`DenseCompileError::NestedOverPeriods`); a reduction reached only through a
  derived reference still fails safely at runtime.
- **PyO3 exposure is f64-only for now** (`execute_lifetime_f64`), matching the
  existing `execute_f64` parity; the exact Decimal `execute_lifetime` is staged
  for a follow-up before a consumer migrates an exact per-period workflow.

## 2026-07-03 — Currency rules opt into output rounding; `minor_units` becomes live

**Decision.** A `derived` rule may declare `rounding: <mode>` (one of
`half_up`, `half_even`, `floor`, `ceil`). When the declaration is present AND
the rule's `unit` is a `Currency { minor_units }`, the engine rounds the rule's
output to `minor_units` decimal places under the declared mode. The mode rides
on the RuleSpec rule, lowers onto `DerivedSpec.rounding`, and is resolved at
`to_program` / compile time into a `model::Rounding { mode, minor_units }` on
the model `Derived` (the unit's `minor_units` is read once at compile time, not
at evaluation). Rounding is applied to a rule's output at the single per-rule
memoization point in **every** execution path — the explain scalar interpreter
(`engine.rs`), the bulk fast columnar path (`bulk.rs`), and the dense columnar
path (`dense.rs`) — before the value is cached, so a rule that references a
rounded rule observes the rounded figure. Half-up on `Decimal` is
`MidpointAwayFromZero`; the three paths are byte-identical on the same value
(cross-path tests cover negatives and exact `.5` midpoints). The dense `f64`
mode rounds best-effort in `f64`, consistent with its documented
throughput-not-exactness role.

**Why.**

- `UnitKind::Currency { minor_units }` was declared but never applied
  (architecture review 2026-06-09 §3.4, P1 rec 6): every currency output
  carried arbitrary `Decimal` precision, which de-synchronised population
  comparisons and could not express statutory rounding (SNAP allotments to
  whole dollars; many benefit and tax figures round per their own rule).
- Rounding must be *declared*, not inferred from the unit: `minor_units`
  describes a currency's fractional digits, not a mandate to round, and most
  intermediate currency values are intentionally unrounded. Making rounding a
  per-rule opt-in lets encoders round exactly the outputs the statute rounds.
- Correct-by-construction across paths matters more than any single path:
  the same declaration rounds identically in explain, fast, and dense, so a
  program cannot round in one execution mode and not another.

**Consequences.**

- **Opt-in; no silent artifact change.** A rule with no `rounding:` behaves
  exactly as before — no path rounds it — so every existing compiled artifact
  and rule keeps its current numeric behavior. `minor_units` had no runtime
  effect before this change, so nothing shipped changes value without an
  explicit `rounding:` declaration.
- **Compile-time validation.** `rounding:` on a rule whose unit is not a
  currency, on a rule with no unit, or on a non-`derived` rule is a compile
  error (`CompiledProgramArtifact::compile` calls `ProgramSpec::validate_rounding`;
  `to_program` re-checks while resolving, so execution is guarded too).
- **Explicit units override built-in defaults.** The formula layer seeds
  common currency defaults (USD/GBP/EUR at `minor_units: 2`). A module that
  declares its own unit of the same name now overrides that default instead of
  being dropped, so `USD { minor_units: 0 }` yields whole-dollar rounding. This
  only affects `minor_units` (previously unread), so it changes no prior
  numeric result; it is what "modules may override their own units" always
  intended.
- **Trace shows the rounding step.** Explain-mode scalar trace nodes carry the
  applied `rounding` mode and, when rounding moved the value, the
  `pre_rounding_value`, so auditable law can show pre-value → mode → rounded.
- **Serde/schema additivity.** `DerivedSpec` gains an optional `rounding`
  field (a closed `half_up|half_even|floor|ceil` enum); absent it serializes
  as before. The derived artifact schema regenerates from `schemars`; the
  hand-written module schema adds the `rounding` property. No
  `ARTIFACT_FORMAT_VERSION` bump: the field is additive and ignored by older
  engines, which simply do not round.
- Out of scope, tracked elsewhere: teaching the encoder to *emit* `rounding:`
  from statutory text is an `axiom-encode` prompt/gate follow-up; this change
  only makes the engine honor the declaration.

## 2026-07-02 — The engine publishes the authoritative JSON Schemas for its serialized surface

**Decision.** The engine is the single source of truth for the shape of the
formats it exchanges, published as checked-in JSON Schemas under `schemas/`:
`rulespec-module.v1` (the RuleSpec module/authoring format), `rulespec-test.v1`
(the companion `*.test.yaml` case format), and `compiled-artifact.v1`
(`CompiledProgramArtifact`, embedding the `ProgramSpec` IR). A non-default
`schema` feature adds a `schema` module and an `emit-schemas --out <dir>` CLI
subcommand; a golden-file test regenerates the schemas in memory and fails on
any drift from the checked-in copies. The artifact schema is **derived** from
the serde types with `schemars`; the module and test schemas are **hand-written**
because their acceptance is wider than a derive can express.

**Why.**

- Python consumers (`axiom-encode`, `axiom-corpus`, `axiom-compose`) each
  re-implement the RuleSpec shape and citation parsing by hand, and drift from
  the engine silently. A published schema derived from — and tested against —
  the actual deserializer is a shared contract those consumers can validate
  and codegen from instead.
- The schema must mirror **serde acceptance**, not a tidied ideal: a file that
  deserializes must validate and vice versa. That forces hand-authoring where
  `RuleKind` accepts any string (unknown kinds defer to a lowering error, so
  the schema keeps `kind` an open string, not a closed enum), where
  `versions[].values` reads bare integer-keyed scalars through a custom
  deserializer, where `deserialize_optional_string_like` coerces
  string/bool/number, where `serde_yaml` accepts explicit `null` for defaulted
  `Vec`/`Option` fields, and where `EncodingProvenance` alone denies unknown
  fields. Each divergence from a naive derive is commented at its definition.

**Consequences.**

- `schema` is off by default so pure-runtime consumers (the PyO3 extension,
  wasm, finbot, microsim) do not compile `schemars`; the wasm32
  `--no-default-features` check is unaffected. CI runs the schema tests with
  `--features schema`.
- Fidelity is enforced by tests, not asserted: `schema_fidelity` checks the
  module schema agrees with the deserializer in both directions on adversarial
  inputs (unknown kind accepted, unknown provenance field rejected, bad
  enum/sha256 rejected); `schema_conformance` validates every module and
  companion test under a `rulespec-us` checkout (3,017 modules and 3,010 tests
  validate, 0 failures at introduction) against a ratchet, self-skipping when
  no checkout is present. Program-spec files under `programs/**` are a
  different format and are excluded.
- The schemas describe the **deserialization** layer only. A document can
  validate and still fail lowering for semantic reasons (unknown kind,
  top-level `relations:`, missing `effective_from`); that boundary is
  documented and asserted.
- Out of scope, tracked elsewhere: Python codegen from these schemas; the
  provision-record schema in `axiom-corpus`; the encoding-manifest schema in
  `axiom-encode`.

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

**Decision.** The RuleSpec `module:` block carries inert provenance. When
present, `source_verification` is an exact mapping with one required singular
`corpus_citation_path` and optional `source_sha256` (the SHA-256 hex digest of
the exact corpus provision text the module was encoded from),
`encoding_provenance` (optional `encoder`, `model`, `run_id`, `reviewed_by`
strings; unknown subfields rejected), and `validation` (a list of
`{oracle, status, last_run}` records with `status` one of
`matches` / `mismatches` / `pending`). The engine validates shape at load —
a malformed sha fails with an error naming the module file — carries the
merged root module's metadata on `ProgramSpec.module`, and passes it
through compiled-artifact JSON. Load and artifact boundaries validate it;
the metadata does not alter formula semantics or execution results.

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

- The module block remains optional. A declared `source_verification` must
  identify exactly one canonical corpus provision; source lists are composed
  upstream before they reach the engine.
- Artifacts gain an optional `program.module` pass-through. No
  `ARTIFACT_FORMAT_VERSION` bump: the field is additive, ignored by older
  engines, and evaluation semantics are unchanged.
- Malformed `source_sha256` values (not 64 hex characters), unknown
  `encoding_provenance` subfields, and unknown `validation[].status`
  values are rejected at load instead of passing silently.
- Unknown `source_verification` subfields and plural
  `corpus_citation_paths` at any source/proof depth are rejected.
- axiom-encode owns stamping the blocks at encode time and the staleness
  checker that compares pinned hashes against the current corpus.

## 2026-07-11 — Filesystem module loading uses explicit canonical country roots only

**Decision.** Every filesystem compile/load caller supplies a non-empty
`CanonicalRuleSpecRoots`. CLI callers pass required repeatable
`--rulespec-root` arguments; Rust and PyO3 callers pass the roots directly.
Each root is an absolute, real, unaliased checkout named exactly
`rulespec-<two-letter-country>`, containing direct matching jurisdiction
directories. Environment, cwd, ancestor, sibling, suffixed-worktree, and
legacy standalone discovery are removed rather than retained as compatibility
paths.

The filesystem recognizes five jurisdiction content roots: `legislation/`,
`policies/`, `programs/`, `regulations/`, and `statutes/`. Only the other four
are atomic `rulespec/v1` module roots. `programs/` belongs to `axiom-compose`
and cannot be a module target. `.yml`, root-level content roots,
wrong-country jurisdictions, duplicate roots or countries, symlinks, aliases,
special paths, and empty checkouts fail closed.

**Why.** Ambient discovery made the compiled closure depend on unrelated
filesystem state and allowed false-green builds against stale sibling
checkouts. The former resolver also admitted relative imports and top-level
`extends`. A single validated capability object makes
the root module and its whole closure obey one deterministic policy.

**Consequences.** File API signatures intentionally break: callers must pass
`CanonicalRuleSpecRoots`. The removed `AXIOM_RULESPEC_REPO_ROOTS`,
`AXIOM_RULESPEC_REPO_ROOTS_EXCLUSIVE`, and `--exclusive-rulespec-roots`
surfaces have no compatibility aliases. Pure `ModuleSource` hosts remain
filesystem-free, while their canonical targets now also require an atomic
content root. Ephemeral `axiom-compose` output uses the separate
`compile-composed` / `from_composed_rulespec_file` surface: exact external
`module.kind: composition` input, originless synthesized rules, and canonical
atomic dependencies resolved only through the same explicit roots.
Atomic imports are exact absolute canonical targets with at most one validated
symbol fragment. Relative, extension-bearing, quoted, whitespace, redundant,
or dot-segment spellings are rejected. `extends` and the legacy `schema:`
discriminator are removed rather than aliased.

Compiled artifacts publish an exact input-owner catalog. Atomic owners expose
only `<module>#input.<slot>`; originless synthesized owners expose the bare
slot. The canonical request name is the bare owner when present, otherwise the
lexicographically first atomic owner, while every actual owner remains
accepted for the shared runtime slot. This is a deterministic runtime-input
catalog, not a source manifest. The loader recomputes this catalog and the
other compiler-derived metadata and rejects inconsistencies with the embedded
program; this is a derived-metadata consistency check, not tamper detection.
Source-tamper evidence lives in the signed-corpus-release and supervisor chain.
Source-level tamper evidence (full source manifest with SHA-256, race-safe
reads) is tracked as a follow-up issue.

## 2026-06-10 — Module resolution is a host concern behind `ModuleSource`

**Decision.** The core engine is a pure function over (modules, dataset):
lowering, compilation, and execution never touch a filesystem, environment
variable, or wall clock. Finding module text for a canonical target
(`us:statutes/7/2015/e`) goes behind the `source::ModuleSource` trait
(`load(target) -> Result<Option<String>, SourceError>`);
`load_rulespec_with_source` and `CompiledProgramArtifact::from_rulespec_with_source`
are the pure entry points. Exact absolute imports are validated and their
optional symbol fragments removed in the core (`resolve_import_target`). The
filesystem implementation is `FsModuleSource`, behind a default-on `fs`
feature; the CLI binary requires it. wasm32-unknown-unknown is a
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

**Consequences (filesystem behavior superseded by the 2026-07-11 decision).**

- The default `fs` feature still owns filesystem APIs and the CLI; their root
  contract and signatures are now the explicit hard-cut surface above.
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
(currently 1) and `engine_version` at compile time. Before launch, version 1 is
the sole artifact contract: loading rejects missing, older, or newer format
versions. The Python wrapper bounds every
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

- Missing or mismatched artifact versions fail closed; there is no unstamped
  compatibility surface before launch.
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
must be explicit RuleSpec with exact `format: rulespec/v1`. The former
`schema:` discriminator is rejected.

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

**Superseded on 2026-07-11.** Canonical country monorepos now use the five-root
taxonomy, while legal source artifacts live in immutable named `axiom-corpus`
releases rather than RuleSpec `sources/` trees.

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

**Superseded on 2026-07-11.** The current loader accepts only explicit
canonical country roots and exact absolute imports; it does not resolve
top-level `extends` or arbitrary mounted layouts.

**Decision.** Production encodings live in the jurisdiction repo they belong to.
The engine repo keeps RuleSpec YAML only as parser/execution fixtures under
`tests/fixtures/rulespec/`. Canonical jurisdiction repositories use `statutes/`,
`regulation/`, `policy/`, and `sources/` paths.

The historical engine resolved dependency paths from arbitrary mounted
layouts; that behavior has been deleted.

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

## 2026-06-14 — Derived `sets` lower into delegated formula hooks

**Decision.** `source_relation.type: sets` also lowers same-kind
derived-to-derived bindings. The upstream target remains the executable hook
with its federal name and public id; the downstream value derived rule supplies
the hook's semantics when both sides are present in the merged program.

**Why.**

- Some delegations are not raw parameter settings. Federal law can define the
  formula surface and state-option category, while state law defines the
  conditional selection logic that fills that surface.
- SNAP utility allowances are the motivating case: federal rules define where
  state standard utility allowances enter the shelter-cost calculation, while
  state rules define the local HCSUA/LUA/individual allowance logic and amounts.
- Keeping the upstream hook addressable avoids duplicating federal formulas in
  every state module.

**Consequences.**

- Upstream formula hooks should be `kind: derived` placeholders with matching
  entity, dtype, unit, and period.
- Downstream state modules can bind local derived implementations to those
  hooks with `source_relation.type: sets` and `source_relation.value`.
- Lowering rejects cross-kind bindings and incompatible derived hooks instead
  of silently copying formulas across unlike concepts.
