# Axiom Rules Engine

Prototype Rust implementation of the Axiom Rules Engine around a
temporal-relational core.

This prototype is intentionally opinionated. It resets the repository around one
claim:

> the current rules surface handles dated formulas well, but arbitrary law needs a
> runtime that treats time, relations, legal judgments, and typed values as
> first-class.

The prototype therefore does four things:

1. keeps a unified `derived + dtype + unit` surface for outputs
2. treats `Judgment` as a dtype with legal inference semantics, not as a plain
   boolean
3. keeps `relation` separate because relations are tuple sets over time, not
   scalar values
4. exposes a compiled executable interface that can be wrapped from Python
5. proves the direction with a SNAP law fixture and conformance tests rather
   than product-specific engine code

The Rust crate and executable are named `axiom-rules`. The detailed plan and
justification live in
[`docs/prototype-plan.md`](docs/prototype-plan.md).
Canonical jurisdiction-repo layout and source-registry conventions live in
[`docs/jurisdiction-repos.md`](docs/jurisdiction-repos.md).

## What is implemented

- user-defined units
- typed derived outputs
- temporal periods and intervals
- relation facts
- scalar expressions
- judgment expressions with `holds`, `not_holds`, and `undetermined`
- serialisable program and dataset documents
- explicit `explain` and `fast` execution modes over the same semantics
- a general `axiom-rules compile` command that turns RuleSpec YAML programmes
  into reusable compiled artefacts
- a generic dense compiled executor for an acyclic scalar/judgment subset, proven on multiple programmes
- a CLI that reads a JSON execution request from stdin and writes JSON results
- a Python wrapper over the compiled executable using Pydantic models
- a SNAP DSL fixture for the 48 states and DC monthly rules:
  - household size from relations
  - earned and unearned income aggregation across household members
  - standard deduction lookup by household size
  - gross and net income tests as judgments
  - maximum allotment lookup by household size
  - monthly allotment calculation

## Why this is a prototype

The engine is intentionally small. It parses canonical RuleSpec YAML fixtures,
but it does not yet emit Arrow or implement the full fixed-point legal runtime.
It is a working spike for the semantic centre of gravity.

## RuleSpec direction

The canonical authoring target is RuleSpec YAML/JSON: structured rule metadata
with concise formula strings. Authoring tools should emit RuleSpec; the Axiom app
should render human-facing rule graphs and traces; the Rust engine normalises
RuleSpec into `ProgramSpec` before compilation. Direct `ProgramSpec` YAML is an
internal engine IR, not an accepted programme file format.

Formula strings are fields inside RuleSpec. The parser for those strings is an
internal implementation detail, not a separate source format.

RuleSpec files must declare `format: rulespec/v1` or a schema starting with
`axiom.rules`. YAML with a top-level `rules:` key but no discriminator is
rejected to avoid silently compiling the wrong shape. See
[`docs/rulespec.md`](docs/rulespec.md).

Production rule content belongs in jurisdiction repositories where the filepath
is the canonical ID. Source registries mirror the rule tree under `sources/`,
store expected hashes in Git, and derive R2 artifact paths deterministically.

To validate a jurisdiction repo's source registry files:

```bash
PYTHONPATH=python python3 -m axiom_rules.cli check-sources /path/to/us-tn
```

Add `--verify-r2` with `AXIOM_R2_ACCOUNT_ID`, `AXIOM_R2_ACCESS_KEY_ID`, and
`AXIOM_R2_SECRET_ACCESS_KEY` set to verify object existence and SHA-256 hashes
against R2.

## Python wrapper

The thin Python wrapper lives under `python/axiom_rules/`. It exposes `Program`,
`Dataset`, and `AxiomRulesEngine`, uses Pydantic models for the request and
response envelope, and shells out to the compiled `axiom-rules` binary for the reference
and generic compiled-artefact flows.

There is also now a separate generic dense Python binding,
`CompiledDenseProgram`, which loads a programme directly into the generic
dense compiler path and executes it in-process over NumPy arrays.

Requests now choose a mode explicitly:

- `explain`: always run the reference executor
- `fast`: try the bulk compiled path first, then fall back to explain mode if the
  requested programme uses a feature the fast path does not support yet

To compile a programme into a reusable artefact:

```bash
cargo run -- compile --program programmes/other/snap/rules.yaml --output /tmp/snap.compiled.json
```

To execute that compiled artefact:

```bash
cargo run -- run-compiled --artifact /tmp/snap.compiled.json < request.json
```

The compiled artefact is currently an analysed execution package:

- resolved `ProgramSpec`
- dependency-ordered derived outputs
- durable RuleSpec output ids in the form `<canonical file target>#<rule name>`
- fast-path eligibility metadata

That means the generic compile pipeline is now real and reusable. The repo no
longer contains a SNAP-specific Rust execution kernel; the remaining fast proof is
the generic dense compiler path below.

To run the SNAP demo end to end from Python:

```bash
python3 python/examples/run_snap_cases.py
```

To run every maintained Python case runner, use the packaged check command:

```bash
PYTHONPATH=python python3 -m axiom_rules.cli check-examples
```

To run the reg 15 child-benefit responsibility demo (explain mode), and then its
fast-mode benchmark over a million children:

```bash
python3 python/examples/run_child_benefit_cases.py
python3 python/examples/run_child_benefit_benchmark.py --children 1000000
```

To run the UK income tax 2025-26 demo (23 HMRC-validated cases covering rUK
and Scottish rate ladders, savings and dividend income with the personal
savings allowance / starting rate for savings / dividend allowance, blind
person's allowance, marriage allowance, EIS relief, Gift Aid band extension,
and HICBC), and then its fast-mode benchmark over a million taxpayers:

```bash
python3 python/examples/run_uk_income_tax_cases.py
python3 python/examples/run_uk_income_tax_cases.py --trace   # with ITA / ITTOIA / FA citations on every derived output
python3 python/examples/run_uk_income_tax_benchmark.py --taxpayers 1000000
```

The benchmark reports around half a million taxpayers per second through the
in-process dense runtime — the programme is organised around the ITA 2007
s.23 seven-step skeleton, compiles to 80 derived outputs, and splits income
into NSND / savings / dividend channels with the correct band stacking.

To run the Universal Credit demo (eight cases rendered in explain mode with
the legislation citation trace), and then its fast-mode benchmark over a
million synthetic benefit units:

```bash
python3 python/examples/run_universal_credit_cases.py
python3 python/examples/run_universal_credit_benchmark.py --benefit-units 1000000
```

The UC benchmark reports around 1.2 million benefit units per second in fast
mode; every derived output in explain mode is annotated with its UC Regs 2013
citation so the trace is readable as a legal explanation of the award.

To run the Housing Act 1988 s.21 notice-validity cases — the first non-tax-benefit
demonstrator in the repo, where the top-level output is a statutory conclusion
(`section_21_notice_valid`: holds / not_holds) rather than an amount:

```bash
python3 python/examples/run_section_21_cases.py
```

The programme composes gates drawn from HA 1988 s.21(4B), HA 2004 s.213–215
(deposit protection + prescribed information within 30 days), the Deregulation
Act 2015 ss.33/38/39 (EPC, gas safety, How-to-Rent guide, retaliatory-eviction
bar), and HA 2004 Parts 2/3 (licensing). Eleven cases exercise each failure mode
independently, including the `count_related` window that implements the
retaliatory bar as an absence condition. No arithmetic on the question asked —
this is the shape of law that a formula-only surface has no natural way to
express.

To install the in-process dense Python binding into your virtualenv:

```bash
maturin develop --release --manifest-path python-ext/Cargo.toml
```

To run the fast Python benchmark over random SNAP households:

```bash
python3 python/examples/run_snap_benchmark.py
```

That path now reports:

- compile time for the generic dense programme
- generation time in Python
- execution time through the in-process generic dense runtime

To compare against the slower CLI boundary explicitly:

```bash
python3 python/examples/run_snap_benchmark.py --engine cli
```

The CLI path is still useful as a correctness boundary, but it is no longer the
performance benchmark that matters.

## Generic dense proof

There is now also a generic dense compiled path in Rust for a substantial subset:

- scalar and judgment derived outputs (bool, integer, decimal, text, date)
- acyclic dependencies
- parameter lookups (effective-dated, integer-indexed)
- `if` / and / or / not
- `count_related` with optional `where` predicate
- `sum_related` over related inputs, with optional `where` predicate
- `ceil`, `floor`, `date_add_days`
- source citations and durable ids on derived outputs, surfaced as a
  tree-shaped trace in explain mode with every intermediate value and
  regulation reference

It is exercised on multiple programmes:

- `programmes/other/flat_tax/rules.yaml`
- `programmes/other/family_allowance/rules.yaml`
- `programmes/other/snap/rules.yaml`
- `programmes/uksi/1987/1967/regulation/15/rules.yaml` (SI 1987/1967 reg 15: child benefit responsibility with an absence condition, encoded as `count_related(cb_receipt) == 0`)
- `programmes/ukpga/2007/3/rules.yaml` (UK income tax 2025-26: full ITA 2007 s.23 seven-step calculation — income split across NSND / savings / dividend channels, personal allowance with £100k taper and BPA and marriage-allowance transfers, starting rate for savings and PSA, dividend allowance, rUK and Scottish NSND rate ladders, Gift Aid / pension band extensions, marriage / EIS / SEIS / VCT reducers, HICBC, Gift Aid recovery; 80 derived outputs, every one cited to ITA / ITTOIA / ITEPA / FA)
- `programmes/ssi/2021/249/regulation/71/rules.yaml` (SSI 2021/249 reg 71: Scottish CTR notional capital, uses a filtered `sum_related` with a where-clause)
- `programmes/uksi/2013/376/rules.yaml` (UC Regs 2013 core monthly calculation: standard allowance, child element with two-child limit, disabled child addition, LCWRA, carer, housing net of non-dep deductions, capital tariff, unearned and earned income taper with work allowance, capital disentitlement — every derived output cites the underlying regulation)

The dense path is exercised from Python via `CompiledDenseProgram` — the
`python/examples/run_*_benchmark.py` scripts are the honest measure of
in-process throughput over the real consumer entry point, no subprocess or
JSON overhead.

## SNAP examples

The prototype SNAP law lives in [`programmes/other/snap/rules.yaml`](programmes/other/snap/rules.yaml).
The executable test cases live in [`programmes/other/snap/cases.yaml`](programmes/other/snap/cases.yaml).
Companion rule tests use `rules.test.yaml`; the CI runners currently exercise
the richer `cases.yaml` fixtures.

## Running tests

```bash
cargo test
```
