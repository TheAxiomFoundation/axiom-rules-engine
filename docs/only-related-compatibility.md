# Unique Boolean relation projection: compatibility branch

This maintenance release extends engine commit
`6e709eb1ca7ea686263293d932c759d9dee48a4a` with
`only_related(relation, input)`. It is distinct from the full implementation
merged as `50215d97ce9b59467d4738b2e872d5253b6e2847`.

The operation requires one distinct active related entity and a Boolean input
owned by that relation's declared related slot. Zero targets produce
`MissingRelation`; multiple targets produce `AmbiguousRelation`, even if their
values agree. Duplicate identical edges retain the existing deduplication.
Missing facts, wrong value types, and wrong reached fact owners refuse. Lookup
uses the existing period containment and latest-covering-start rules.

RuleSpec lowering requires a module-local Boolean input declaration, typed
binary relation, distinct slots, and unambiguous direction from the citing
owner. Imported declarations cannot supply a missing declaration in another
module. Raw ProgramSpec has no RuleSpec input declarations: it validates relation
coordinates and owners, and still checks the reached value and fact owner at
runtime. The new expression tag is rejected by older schemas/readers.

## Explicit backend contract

This branch implements the operation in the scalar Explain evaluator. A program
containing the operation in any version or derived-relation predicate is
ineligible for Fast. Capability detection occurs before Fast input normalization,
BulkEvaluator construction, or warmup. The existing API Unsupported branch then
runs Explain with the original dataset and reports `requested_mode: fast`,
`actual_mode: explain`, and a fallback reason. No evaluation error triggers a
retry or a fallback. Direct dense compilation refuses with Unsupported.

This preserves lazy scalar branches: an unselected operation does not demand a
relation or a fact. It does not port the later engine's masked bulk/dense
implementation. Lifetime relation projection remains unsupported. Programs
without the new expression retain the original backend and validation paths.

The operation does not validate household membership, infer links, admit source
ownership, or establish any policy's determination context. Callers supply
explicit links. No policy, signed manifest, waiver, or production selector is
changed by this engine package.

## Publication prerequisites

The proposed maintenance target is
`codex/issue233-runtime-compatibility-base` at the exact base commit above. CI
adds this target to push and pull-request branch filters while retaining all
existing jobs, pins, and main triggers. Publication requires independent review
and current green checks on the official maintenance PR. Default main is not
rewritten.

Production selection additionally requires unchanged complete catalog results
for the retained capture (including original blocked results), runtime golden
parity, and actual generated-policy compatibility. Focused operation tests do
not establish those conditions.

The existing repository ruleset targets the default branch, so it does not
implicitly protect the proposed maintenance target. The reviewed publication
procedure requires all current applicable checks on the exact maintenance PR head,
including the six existing CI jobs and applicable Release/wasm build checks
(with their existing publish skips),
final independent review, and verification of the base and resulting merge tree
before a normal PR merge. An unprotected branch is not permission to omit those
checks. No ruleset or branch mutation is part of this local preparation.

The existing release and wasm-npm workflows remain unchanged. This work does not
authorize tags, package publication, or bypassing any applicable check.
