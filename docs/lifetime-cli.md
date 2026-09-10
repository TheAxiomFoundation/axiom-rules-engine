# Lifetime execution over supplied periods

`axiom-rules-engine run-lifetime --artifact compiled.json` reads one JSON request
from standard input and runs the compiled artifact through
[`DenseCompiledProgram::execute_lifetime`](../src/dense.rs), using Decimal
arithmetic. The command and [Rust API](../src/lifetime_api.rs) share validation.
The existing `run-compiled` command keeps its scalar request format.

The request uses the following shape. This example describes two invented
observations; it requires an artifact whose public input and output IDs match.

```json
{
  "schema": "axiom-rules-engine/lifetime-request/v1",
  "entity": "Worker",
  "periods": [
    {"period_kind": "tax_year", "start": "2001-01-01", "end": "2001-12-31"},
    {"period_kind": "tax_year", "start": "2002-01-01", "end": "2002-12-31"}
  ],
  "batches": [
    {"row_count": 1, "entity_ids": ["worker-001"], "inputs": {
      "us:statutes/99/1#input.amount": {"kind": "decimal", "values": ["0.1"]}
    }},
    {"row_count": 1, "entity_ids": ["worker-001"], "inputs": {
      "us:statutes/99/1#input.amount": {"kind": "decimal", "values": ["0.2"]}
    }}
  ],
  "outputs": ["us:statutes/99/1#history_total"],
  "output_period": {"period_kind": "tax_year", "start": "2002-01-01", "end": "2002-12-31"}
}
```

`arithmetic` may be omitted or set to `"decimal"`. Input columns have `kind` and
`values`: `decimal` takes quoted strings exactly representable by Rust Decimal;
`integer` takes signed 64-bit integers; `bool`, `text`, and `date` take booleans,
strings, and ISO date strings. Nulls, floating-point numbers, duplicate JSON
keys, and unknown fields are refused. Declared optional input defaults still
use the engine's existing `InputOrElse` semantics; missing required inputs fail.

Every batch must contain the same unique, nonempty string `entity_ids` in the
same order. Each input column length equals `row_count`. These IDs declare row
alignment; the engine does not authenticate identity or infer links. Zero rows
remain zero rows. Input names must appear in the artifact's
[input catalog](../src/model.rs) and belong to the selected dense entity;
computed output or parameter IDs cannot substitute for input IDs. Input and
output references must be full durable public IDs, even for originless rules;
bare names are refused by this new interface. Two supplied names resolving to
one input or output are rejected.

Periods must be nonempty, ascending, non-overlapping, and of the same kind.
Allowed kinds are `month`, `benefit_week`, `tax_year`, and `custom`; only `custom`
has a required nonempty `name`. Their start/end dates are explicit. Gaps are
permitted and are never filled. `output_period` must equal the final supplied
period. The existing lifetime evaluator uses that period for outer parameter
lookups and evaluates reduction expressions in each supplied period. Its
existing invariance and top-N checks still apply.

The dense plan's commencement check now runs for **every supplied period**, as
it does for scalar execution. A history predating any compiled formula's
commencement fails. This restriction does not introduce separate determination
date semantics or make pre-commencement histories executable under later law.
Versioned or end-bounded derived formulas unsupported by the dense compiler
remain unsupported. Every requested output must contain an over-periods
reduction. Relation contexts are explicitly unsupported by JSON v1.

Success writes one `axiom-rules-engine/lifetime-response/v1` JSON object. It
contains `engine_version`, integer `artifact_format_version`, `arithmetic`,
`entity`, `row_count`, `entity_ids`, `periods`, `reference_period`,
`output_period`, and an `outputs` map. Both reference and output periods equal
the final supplied period. Dates are serialized in their canonical date form.
Each output entry contains `id` (the retained full public ID), internal `name`, declared
`dtype`, `unit` (string or null), and `column: {kind, values}`. Decimal results
are normalized quoted strings, with no binary-float conversion. Judgment values
are `holds`, `not_holds`, or `undetermined`; other column kinds match the input
vocabulary. The metadata describes the admitted rule and the column preserves
the executor's value type. Integer results of a rule declared Decimal are
widened exactly to Decimal strings. Other declared/evaluated type mismatches
fail; Decimal results are never truncated into integers.

Handled validation and execution errors return exit status 1, no result on
standard output, and a JSON
diagnostic on standard error: `schema` is
`axiom-rules-engine/lifetime-error/v1`, `category` is `invalid_request`,
`unsupported`, `artifact`, or `evaluation`, `field` is a string or null, and
`message` is bounded. The Rust error retains the underlying typed error as its
source. These are execution diagnostics, not legal validation verdicts. The
existing numeric executor can panic on arithmetic overflow even when each
input is representable. Such an unexpected process failure can have non-JSON
standard error. Hosts must treat every nonzero exit or invalid response as
failure; no partial result or substitute arithmetic is returned. This change
does not alter those numeric kernels.

The transport limits a request to 16 MiB, an artifact to 64 MiB, a history to
512 periods, a batch to 100,000 rows and 256 input columns, requested outputs to
256, and individual supplied strings to 4,096 bytes. Supplied row-ID and input
cells together are limited to 2,000,000; output cells have the same separate
limit. These bounds do not replace a host's execution timeout or memory limit
for a complex artifact. Standard compiled-artifact admission remains in force.

`emit-schemas` publishes request and response schemas alongside the existing
schemas. The companion test schema adds a strict `lifetime` case: top-level
`name`, optional `description`, `period`, `output`, and `lifetime`; the nested
object contains `entity`, `periods`, `batches`, and optional `arithmetic`.
Expected outputs may be scalars for one row or arrays in row order. Decimal
expectations must be quoted. Runtime binding and the fixture harness enforce
cross-field equality and declared output types. Scalar companion cases keep
their previous schema. No encoded statutory module or fixture is supplied by
this transport change.
