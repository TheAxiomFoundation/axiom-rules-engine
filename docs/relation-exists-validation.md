# Bounded `exists` validation (#214, PR 3/5)

Draft PR: [#280](https://github.com/TheAxiomFoundation/axiom-rules-engine/pull/280).
Design: [relation-roles.md](relation-roles.md), principally §3.6.

The assigned base is `bb6bcff95`. Its required history was confirmed to contain
#268, #269, #250, #273, #276 and #279. Work stays on the assigned branch and
workspace; no RuleSpec module repository is changed.

The checkout's Git metadata points outside the writable workspace. Commits
therefore use isolated metadata at `target/lane-git`, with read-only object
alternates, and are pushed to `relation-roles/3-exists`. The caller's index,
refs and checkout are not written.

| Commit | Coherent step |
| --- | --- |
| `bf37ec13f` | Model and lower bounded `exists` with lexical scope and kind refusals. |
| `c74c685b7` | Evaluate bounded `exists` with resumable bindings and precise skipped traces. |
| `9669124a2` | Prove expansion, resume isolation and relation invariants with independent oracles. |
| `800e60de2` | Test nested variable input reads and bare-name scope reservations. |
| This report's commit | Document syntax, semantics, format-3 additions and the validation evidence. |

## Invariants and independent checks

The property tests use `proptest`. In this table, `E` means
`engine::relation_resolution_tests::exists_expands_and_defers_with_observed_paths`
and `M` means
`engine::relation_resolution_tests::exists_positive_base_domains_are_monotone_with_observed_paths`.
The expansion checks 256 cases with fixed ChaCha seed `[71; 32]`; monotonicity
checks 256 with `[83; 32]`. Their counters are incremented where the engine
computes a domain, evaluates a bound body, resumes that body, or finishes the
early-stop skip work. They count cases that actually execute each path.

| Invariant from §7 | Exact tests | Independent oracle and observed path evidence |
| --- | --- | --- |
| 1. Order | `E`; `relation_exists::exists_values_order_determinism_and_modes_have_an_independent_oracle` | Domains are computed separately from raw records using a `BTreeSet`, then expanded into an ordinary disjunction. Reversed inputs, relation tuples and roster records preserve the result and trace. The shared expansion coverage below applies. |
| 2. `exists` expands | `E` | Data and derived generators, flat and nested bodies, and missing inputs are compared with the separately constructed sorted disjunction. Observed `[resumed, early stop, empty, nonempty, nested] = [77, 17, 125, 148, 59]`, against floors `[20, 2, 20, 100, 20]`. Failures: 68, floor 10. Independent flat trace comparisons: 131, floor 100. |
| 6. Monotone in tuples | `M`; `relation_exists::positive_base_exists_are_monotone_with_exact_memberships` | Direct Boolean membership/reachability over the generated records supplies exact before/after values, as well as the monotonicity order. Observed `[early stop, empty, nonempty, nested] = [149, 57, 253, 110]`, floors `[20, 20, 150, 50]`. The positive fragment allows base atoms and domains under an even number of `not`s; it excludes derived relation reads, aggregating rule reads, count comparisons and bare membership tests. Counting the selected result observes membership rather than adding a count comparison to the predicate. |
| 7. Deferral transparency | `E`; `engine::relation_resolution_tests::suspended_resolution_matches_native_resolution`; `engine::relation_resolution_tests::exists_resume_key_separates_every_binding_period_node_and_parent` | Expansion gives the independent value/failure oracle. Flat expansion traces and explicit expected nested skip lists check trace semantics. `E` sweeps rule suspend depths 1–12 and relation nesting limits 1–2, which cover every stopping point in its generated programs. The resume-key matrix sweeps depths 1–8 and both relation limits; limit 1 must complete a real stopped resolution and resume a bound `exists`, while limit 2 must stop none in that matrix. Observed resumed cases: 77, floor 20. |
| 8. Determinism | `E`; `relation_exists::exists_values_order_determinism_and_modes_have_an_independent_oracle` | Repeated runs compare values/failures with the independent expansion and compare serialized traces with the first run; the shared expansion coverage applies. |
| 9. Modes agree | `E`; `relation_exists::exists_values_order_determinism_and_modes_have_an_independent_oracle`; `relation_exists::exists_opts_into_strict_binding_and_dense_names_its_refusal`; `relation_exists_model::dense_refuses_exists_by_name_and_issue` | Explain and fast each compare with the independent expansion or direct Boolean oracle. Successful fast checks require an actual bulk `Executed` response, not fallback: 188 cases, floor 100. Dense must return `DenseCompileError::Unsupported` naming `exists` and #218. |
| 11. Kinds | `relation_exists_model::variable_kind_comes_from_its_generator_slot`; `relation_exists_model::variable_scope_is_exactly_the_enclosing_frame_count`; `relation_exists_model::compiled_domains_accept_exactly_matching_bound_slot_kinds`; lowering refusal tests | Expected acceptance is computed from generated slot kinds and the lexical frame count, independently of the compiler. Fixed-seed floors are: rule-read kinds 20 accepted/20 refused/16 nested; scope 12 accepted/30 refused/8 nested; domain slot kinds 40 accepted/80 refused/20 nested/24 derived. Body rule reads and generator-bound slots are checked separately. |
| 12. Termination | `relation_exists::unary_data_generator_has_a_finite_unbound_domain`; `relation_exists_model::exists_refuses_generator_and_body_cycles`; `relation_exists_model::generated_exists_generator_and_body_cycles_remain_refused` | Every runtime domain comes from a finite supplied tuple set or finite resolved member set. Generated graph cycles are known independently from their construction and must refuse: 128 fixed-seed cases, with floors 100 refused/60 containing generator edges/60 containing body-atom edges. |

The expansion's literal-id term is `Term::Id`, guarded by `#[cfg(test)]` in
the model. It has no `TermSpec`, schema, artifact or RuleSpec variant.
`TermSpec::from_model(Term::Id(_))` panics explicitly; the expected-panic test
`spec::literal_id_term_tests::literal_ids_have_no_spec_or_artifact_representation`
pins that boundary. `relation_exists_model::variable_artifact_form_keeps_legacy_terms_and_round_trips`
checks the supported string and `{var: index}` forms.

## Exhaustive-match and walk inventory

Every new arm handles either the generator relation, the body with its new
binding, or the named refusal appropriate to that execution path.

| Location | Handling |
| --- | --- |
| `api::format_judgment_expression` | Render an `exists` expression and its term arguments for the executed-expression projection. |
| `bulk::outside_derived_relation`, `bulk::judgment_node` | Give a named scope failure if reached directly; relation predicates continue to run on the explain interpreter. |
| `compile::collect_fast_blockers_from_judgment_expr` | Walk the body for advisory metadata. |
| `compile::collect_judgment_dependencies`, `compile::collect_relation_members_from_judgment` | Add the generator's relation edge and walk body rule/relation dependencies, including combined-graph ordering and cycle detection. |
| `compile/evaluation_order_reference.rs::collect_judgment_dependencies` | `unreachable!` for `Exists`, as for atoms: this frozen oracle orders legacy programs only. |
| `dense::{compile_judgment_node, compile_related_predicate_node, compile_current_judgment_expr_node}` | All three compilation paths refuse `exists` by name with `Unsupported` and #218. |
| `engine::RelationEvalContext::id_of`, role dispatch and judgment-node dispatch | Resolve `Var(index)` through borrowed frames; route `Exists` through the existing `relation_role_judgment` call site into its non-inlined evaluator. |
| Engine skip-reference collection | Walk the body without enumerating a skipped domain; remove the unbound inner variable and decrement enclosing indices. Early stops supply an actual frame for each remaining domain id. |
| Formula lowering and scope walks | Lower the call, reserve bound spellings across the predicate, maintain the active lexical frame stack, check atom arguments and reject scoped constructs in aggregations. |
| `interface::Routing::term_flow`, `Collector::judgment`, relation collection | Track lexical variable kinds; add the generator to `reads.relations`; record data bound slots or the derived current slot; walk body reads. |
| Migration's judgment walk | Recurse through the body while preserving the generator. |
| Model construct detection, term labels and input catalog | `Exists` opts into relation-role and atom binding restrictions; the catalog collects body reads. `Term::as_str` gives `Var` the stable label `variable` for diagnostics and descriptions, and gives test-only `Id` its literal id. |
| Relation-direction judgment walk | Preserve the explicit generator relation/term order, include its target in the declared-kind treatment and walk the body. |
| Relation typing | Judgment relation collection, `term_kind`, `role_construct`, `role_atom`, role-read collection and derived-rule collection check scope, generator marker/arity/direction, bound-slot kinds, body rule kinds and dependencies. |
| RuleSpec walks | `rewrite_relation_alias_in_judgment`, `collect_judgment_relation_names`, `rewrite_judgment_relation_references`, `judgment_uses_imported_derived` rewrite aliases and canonical references, retain generator/body relations and inspect imported body reads. |
| Spec conversions | Convert `Var(index)` and `Exists` between spec/artifact and model representations, preserving the body's new binding. Literal test ids cannot convert to a serialized term. |
| Unit-derivation preflight | Inspect the generator relation and body for unresolved relation/reduction reasons, without assuming predicate bindings in the structural scan. |
| `execution_mode_parity` helpers | `walk_judgment` and `retired_sums_in_judgment` descend into the body; `fmt_judgment` uses `unreachable!` because its legacy generator produces no `Exists` nodes. This test suite was not run, as requested. |

## Mechanism-sentence audit

The new mechanism statements in `rulespec.md`, `execution-semantics.md` and
`artifact-format.md` were checked against final code, with these named cases:

| Documented mechanism | Case that checks it |
| --- | --- |
| Nested lexical indices in generators, reads, atoms and comparisons | `relation_exists_lowering::nested_variables_lower_to_innermost_indices_in_reads_atoms_and_comparisons`; `relation_exists::variable_input_reads_use_the_bound_id_in_nested_frames` |
| Whole-predicate name reservation, sibling scopes and named generator/name refusals | `relation_exists_lowering::variables_are_reserved_before_after_and_beside_the_body`; `generators_require_one_new_lowercase_variable_and_an_atom`; `a_new_variable_cannot_hide_a_visible_rule_parameter_or_relation` |
| Builtin `exists` keeps its meaning beside a relation with that name; atom/generator calls of that relation refuse | `relation_exists_lowering::exists_is_a_builtin_even_if_a_relation_has_that_name` |
| Generator-slot variable kinds, wrong-kind body reads and forward-only binary derived generators | `relation_exists_lowering::generator_kinds_are_inherited_by_variables_and_checked_in_the_body`; `untyped_generators_and_inverse_derived_traversals_refuse_by_name`; `relation_exists_model::derived_generator_of_arity_other_than_two_refuses_by_name` |
| Rule/aggregation scope exclusion | `relation_exists_lowering::exists_has_no_scope_in_rule_bodies_or_aggregation_clauses_or_values` |
| Sorted distinct domains, every bound argument, whole-period matching, failure order and unary domains | `relation_exists::data_domains_check_every_bound_slot_period_and_deduplicate`; `sorted_domains_propagate_first_failure_and_stop_before_a_later_failure`; `unary_data_generator_has_a_finite_unbound_domain` |
| `not exists(G, not body)` including empty domains | `relation_exists::exists_and_for_all_cover_empty_domains_and_both_generator_directions`; `relation_exists_lowering::forall_is_not_exists_over_a_negated_body` |
| Skipped reads use evaluated outer bindings; unevaluated bindings produce no record | `engine::relation_resolution_tests::exists_skipped_reads_use_evaluated_outer_bindings`; `exists_unevaluated_inner_bindings_produce_no_skip_records`; `exists_skipped_nested_frames_preserve_enclosing_variable_indices` |
| Wholly skipped anchor/candidate reads occur once; early stops record each remaining sorted distinct id | `engine::relation_resolution_tests::exists_wholly_skipped_records_anchor_and_candidate_once`; `exists_early_stop_records_each_remaining_sorted_distinct_id` |
| Resume identity includes node, anchor, candidate, every outer id, period, parent and trace owner | `engine::relation_resolution_tests::exists_resume_key_separates_every_binding_period_node_and_parent`; `E` checks continuation against expansion across all stopping limits. |
| Binding opt-in, named dense refusal and parameter-only legacy behavior | `relation_exists::exists_opts_into_strict_binding_and_dense_names_its_refusal`; `relation_atoms::parameter_lookups_in_predicates_keep_the_union_of_supplied_tuples` |
| Interface relation edges, bound/current lookup slots, lexical input kinds and tamper refusal | `relation_exists_interface::generator_records_bound_slots_and_body_reads_follow_lexical_kinds`; `derived_generator_records_only_its_current_slot_in_either_direction`; `unary_generator_records_no_bound_lookup_slot` |
| Format-3 indices and marker validation; literal test ids stay unserializable | `relation_exists_model::variable_artifact_form_keeps_legacy_terms_and_round_trips`; `raw_variable_indices_and_generator_marker_are_checked`; `spec::literal_id_term_tests::literal_ids_have_no_spec_or_artifact_representation` |
| Generator/body dependency edges, imported aliases and refused cycles | `relation_exists_model::exists_adds_generator_and_body_edges_to_evaluation_order`; `imported_relations_used_only_by_generator_and_body_survive_and_are_rewritten`; `exists_refuses_generator_and_body_cycles`; `generated_exists_generator_and_body_cycles_remain_refused` |

The audit distinguishes an evaluated empty domain, which produces no body
dependencies, from a wholly skipped `exists`, which records its body's
anchor/candidate dependencies without looking up a domain. It also names the
zero-variable cases precisely: repeated existing terms or an enclosing variable
give the rebinding conflict; `R(anchor, candidate)` gives the variable-count
refusal. Every emitted `exists` refusal code is documented by name.

## Stack measurements

Measured on Apple aarch64 with rustc 1.94.1, against an isolated worktree at
`bb6bcff95377`. Each default-thread `relation_chains` test was bisected in
16 KiB steps with `RUST_MIN_STACK`, then refined to 1 KiB; the preceding
1 KiB request failed with stack overflow and the listed request passed on both
revisions.

| `relation_chains` test | Main minimum passing KiB | Branch minimum passing KiB |
| --- | ---: | ---: |
| `a_chain_closed_into_a_cycle_is_refused_without_recursing` | 33 | 33 |
| `a_chain_testing_its_base_relation_at_every_link_is_linear` | 113 | 113 |
| `a_chain_through_count_related_predicates_is_linear_not_exponential` | 561 | 561 |
| `a_chain_through_relation_member_predicates_is_linear_not_exponential` | 257 | 257 |
| `a_compiled_chain_loads_and_runs_in_linear_time` | 113 | 113 |
| `a_dense_chain_compiles_and_runs_in_linear_time` | 97 | 97 |
| `a_queried_typed_chain_evaluates_in_linear_time` | 113 | 113 |
| `an_unqueried_chain_binds_and_validates_in_linear_time` | 49 | 49 |
| `counting_every_link_of_a_chain_resolves_each_link_once` | 145 | 145 |

The fixed-stack guards also pass without enlargement:
`count_related_chain_stays_within_a_bounded_stack` at 1 MiB and
`long_atom_dependency_chains_preserve_values_traces_and_suspend_depths` at
512 KiB. `Exists` shares the existing role-dispatch call site in
`judgment_node` and uses a non-inlined evaluator. Borrowing the context's
kind strings keeps `RelationEvalContext` at 56 bytes, below main's 64 bytes.

The [complete passing/failing bounds](validation/bounded-exists-stack.json) are retained.
Trial commands and bounds were recorded as they ran in
`target/exists-validation/stack-trials.jsonl`, `main-stack.json`,
`branch-stack.json` and `stack-comparison.json`. The 1 KiB refinement is
recorded in `stack-trials-fine.jsonl`, `main-stack-fine.json`,
`branch-stack-fine.json` and `stack-comparison-fine.json`. The first main bisection
batch reached the 600-second cap after saving completed trials; its remaining
cases were resumed from those checkpoints. No per-case bound was widened.

## Mutants

Each patch below was applied individually, compiled and run under the 2 GiB,
600-second cap, then restored. Every recorded kill is Rust test exit 101 with
an assertion failure; compilation failures do not count. The final unmutated
suites run afterward.

Reproduce a row with `git apply <patch>`, then the listed `cargo test` selection
and `-- --test-threads=4 --nocapture`; follow the host reservation, niceness,
job and cap requirements. The exact patches are retained in the repository; all nine also pass
`git apply --check` against the final tree. Two patches use less context
to avoid trailing whitespace in stored diff files; their added/deleted lines
are identical to those actually run.

| Exact patch | Mutation | Test selection after `cargo test` | Observed failure |
| --- | --- | --- | --- |
| [resume-key](validation/bounded-exists-mutants/mutant-resume-key.patch) | Keep only the innermost outer variable id in the resume key. | `--lib exists_resume_key_separates_every_binding_period_node_and_parent` | At relation limit 1 / suspend depth 1, changing the other outer binding incorrectly reused `Holds` instead of `NotHolds`. |
| [sort](validation/bounded-exists-mutants/mutant-sort.patch) | Delete domain sorting. | `--test relation_exists sorted_domains_propagate_first_failure_and_stop_before_a_later_failure` | The later id `z` failed for missing `points` instead of the earlier successful id yielding `Holds`. |
| [dedup](validation/bounded-exists-mutants/mutant-dedup.patch) | Delete domain deduplication. | `--lib exists_early_stop_records_each_remaining_sorted_distinct_id` | Trace contained an extra skipped `eligible:a` after that id had already been evaluated. |
| [scope](validation/bounded-exists-mutants/mutant-scope.patch) | Disable whole-predicate variable reservation. | `--test relation_exists_lowering variables_are_reserved_before_after_and_beside_the_body` | `p and exists(relative_of(anchor, p), true)` compiled with `p` in the input catalog instead of refusing. |
| [late-stop](validation/bounded-exists-mutants/mutant-late-stop.patch) | Evaluate one additional body after a successful id. | `--test relation_exists sorted_domains_propagate_first_failure_and_stop_before_a_later_failure` | Missing `points` at `z` propagated despite the prior success. |
| [nested path](validation/bounded-exists-mutants/mutant-nested-path-dark.patch) | Generate only flat expansion cases. | `--lib exists_expands_and_defers_with_observed_paths` | `nested frame: 0 cases, floor 20`. Values alone still agreed with the independent expansion. |
| [monotone path](validation/bounded-exists-mutants/mutant-monotone-path-dark.patch) | Generate only flat monotonicity cases. | `--lib exists_positive_base_domains_are_monotone_with_observed_paths` | `monotone nested: 0 cases, floor 50`. Exact membership checks still agreed. |
| [kind path](validation/bounded-exists-mutants/mutant-kind-path-dark.patch) | Generate only wrong-kind first bound slots. | `--test relation_exists_model compiled_domains_accept_exactly_matching_bound_slot_kinds` | `only 0 matching domains compiled`, below floor 40; every generated mismatch still refused. |
| [cycle path](validation/bounded-exists-mutants/mutant-cycle-path-dark.patch) | Reduce the generated cycle cases to zero. | `--test relation_exists_model generated_exists_generator_and_body_cycles_remain_refused` | `only 0 generated cycles refused`, below floor 100. |

Commands, timestamps, exact output paths and exits were checkpointed at
execution time in `target/exists-validation/checkpoints.jsonl` and
`mutants.jsonl`. The scope patch was run twice; the second run used the bare
input spelling first, proving acceptance of a leaked input rather than merely
a changed refusal code.

## Verification

Every Cargo command followed a successful host-reservation check and used
`CARGO_BUILD_JOBS=4` and `nice -n 11`; test runs used
`-- --test-threads=4`. Property, mutation and stack stress runs used the
2 GiB / 600-second cap. Commands ran in foreground process groups, with
exact commands, output paths and exits recorded at generation time in
`target/exists-validation/checkpoints.jsonl`.

The sandbox rejects the priority adjustment (`nice: setpriority: Operation
not permitted`); the mandated prefix was present on every command, but
lowered host priority could not be enforced. Jobs and test-thread caps were
enforced.

The default and `--features "schema unit-derivation"` integration commands
selected all eleven files below. Their actual result tails follow:

| Test file | Default result tail | Schema + unit-derivation result tail |
| --- | --- | --- |
| `artifact_interface` | `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s` | `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s` |
| `deferral_transparency` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 27.00s` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 16.76s` |
| `relation_atom_interface` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s` |
| `relation_atom_order` | `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.59s` | `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.99s` |
| `relation_atoms` | `test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.59s` | `test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.82s` |
| `relation_chains` | `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 37.60s` | `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 43.39s` |
| `relation_exists` | `test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.38s` | `test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.35s` |
| `relation_exists_interface` | `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s` | `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s` |
| `relation_exists_lowering` | `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s` | `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s` |
| `relation_exists_model` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.34s` | `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.52s` |
| `relation_roles` | `test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.93s` | `test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.10s` |

`engine-final` (exit 0, 251.07 seconds):

```text
python3 /Users/maxghenis/reviews/disk-sustainable-2026-10-09/tools/run_capped.py --rss-gb 2 --timeout 600 -- nice -n 11 cargo test --lib engine::relation_resolution_tests -- --test-threads=4 --nocapture
COVERAGE stopped per limit: [0, 892, 364, 174, 74, 32, 0] nested 2797
test engine::relation_resolution_tests::suspended_resolution_matches_native_resolution ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out; finished in 188.70s

```

`engine-features-final` (exit 0, 440.95 seconds):

```text
python3 /Users/maxghenis/reviews/disk-sustainable-2026-10-09/tools/run_capped.py --rss-gb 2 --timeout 600 -- nice -n 11 cargo test --features 'schema unit-derivation' --lib engine::relation_resolution_tests -- --test-threads=4 --nocapture
COVERAGE stopped per limit: [0, 892, 364, 174, 74, 32, 0] nested 2797
test engine::relation_resolution_tests::suspended_resolution_matches_native_resolution ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 115 filtered out; finished in 233.10s

```

`fmt-doc-label` (exit 0, 4.15 seconds):

```text
nice -n 11 cargo fmt --all -- --check
nice: setpriority: Operation not permitted
```

`clippy` (exit 0, 236.84 seconds):

```text
nice -n 11 cargo clippy --lib --no-deps -- -A clippy::all
    Checking serde_json v1.0.149
    Checking rust_decimal v1.41.0
    Checking chrono v0.4.44
    Checking serde_yaml v0.9.34+deprecated
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 47s
```

`schema-golden` (exit 0, 153.2 seconds):

```text
python3 /Users/maxghenis/reviews/disk-sustainable-2026-10-09/tools/run_capped.py --rss-gb 2 --timeout 600 -- nice -n 11 cargo test --features schema --test schema_golden -- --test-threads=4
test v3_artifact_schema_and_the_archives_reject_each_others_generation ... ok
test artifact_schema_rejects_wrong_version_and_invalid_provenance ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.67s

```

`schema-unit-golden` (exit 0, 21.96 seconds):

```text
python3 /Users/maxghenis/reviews/disk-sustainable-2026-10-09/tools/run_capped.py --rss-gb 2 --timeout 600 -- nice -n 11 cargo test --features 'schema unit-derivation' --test schema_golden -- --test-threads=4
test artifact_schema_rejects_wrong_version_and_invalid_provenance ... ok
test v3_artifact_schema_and_the_archives_reject_each_others_generation ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.05s

```

`python-ext-check-local-cache` (exit 0, 76.14 seconds):

```text
nice -n 11 env CARGO_HOME=/Users/maxghenis/.subfleet/worktrees/20261030-152242-ud-pr3-exists/target/exists-cargo-home cargo check --manifest-path python-ext/Cargo.toml
   Compiling pyo3-macros v0.27.2
    Checking rustc-hash v2.1.2
    Checking chrono v0.4.44
    Checking serde_yaml v0.9.34+deprecated
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 13s
```

`schema-generation` (exit 0, 173.4 seconds):

```text
nice -n 11 cargo run --features 'schema unit-derivation' -- emit-schemas --out schemas
wrote schemas/execution-response.v1.schema.json
wrote schemas/experimental-unit-aggregation-plan.stage3.schema.json
wrote schemas/compiled-unit-aggregation.stage3.schema.json
wrote schemas/unit-aggregation-request.stage3.schema.json
wrote schemas/unit-aggregation-result.stage3.schema.json
```

The only source edit after the semantic regression runs corrects the
`Term::as_str` documentation comment; the final formatting and Python-extension
checks include it.

Additional targeted unit tests used the already-built default test binary:

| Test | Actual tail |
| --- | --- |
| `builtin-names` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 86 filtered out; finished in 0.02s` |
| `literal-id-no-wire` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 86 filtered out; finished in 0.01s` |

The initial Python-extension check stopped before compilation because the shared
Cargo cache is read-only and lacked a required crate. The successful retry
uses `CARGO_HOME=target/exists-cargo-home`: 67 existing locked crate archives
and 104 exact sparse-index entries were copied by package name from
`python-ext/Cargo.lock`, and missing crates were downloaded into that writable
workspace cache. No lockfile or dependency version changed. The schema-only
run also emits an existing `unused_mut` warning at `src/schema.rs:69`;
that source is unchanged.

The original all-engine property invocation reached the 600-second cap.
The test helper was then changed to convert each generated model and dataset
once, while retaining a fresh engine for every depth/limit. The 3,000 cases,
seeds, broad/deep shape weights, independent recursive oracle and all stopping
limits were retained. The final default and feature-enabled groups above
complete under the cap. The successful default stopping sweep observed
`[0, 892, 364, 174, 74, 32, 0]` actual suspended cases at limits 0–6
(index 0 is unused), with 2,797 evaluated nested cases; each stopping limit
1–5 has floor 10, and limit 6 must stop none.


## Process cleanup

After the last build and stack trial, the required
`pgrep -fl 'cargo|rustc|pytest|maturin|axiom-rules-engine'` returned 3 because
this sandbox cannot reach `sysmond`. The libproc fallback enumerated
1,355 processes and checked matching executables' working directories.
Its one match was a different lane's pytest probe (PID 24415, cwd
`/Users/maxghenis/.subfleet/worktrees/20261030-162705-pr284-review-r1`),
which was left alone. There were no owned matching processes to stop.
The exact result was recorded at generation time in
`target/exists-validation/process-audit.json`.

## Scope and deferred checks

#240, #245 and #251 are open and absent from the assigned baseline. There is
no formula/expression bound to extend and therefore no at-bound test;
`relation_exists_lowering::nested_exists_traverses_each_expression_level`
checks 24 lexical levels. The #215 option-specific refusal is skipped because
its prerequisite #251 has not landed.

As requested, driver-owned verification excludes byte identity, corpus
interface acceptance, the Python suite and `execution_mode_parity`. The
Python extension's Rust `cargo check` remains part of lane verification.
Path sources (§3.7) and the 273.1 fixture remain for later PRs. No
`DECISIONS.md`, `PROGRESS.md`, workflow, pin, CODEOWNERS, toolchain file,
release branch or release tag is changed.
