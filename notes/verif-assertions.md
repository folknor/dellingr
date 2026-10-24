# Verification: assertions

Findings from the verification hunt about individual tests: tests that cannot
fail, that check something weaker than their name, that rely on a setup step
that may do nothing, or that sit under the wrong subject. Grouped by subsystem
after a cross-cutting section. Each entry names the scopes whose hunters
reported it.

```
1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.
```

---

# Cross-cutting

## VA-001 - Tests on `State::new()` print to the real stdout, past libtest's capture

Reported by: corpus-harness, library.

`DefaultCallbacks::on_print` (`src/host.rs`) writes through
`std::io::stdout().lock()`, which bypasses libtest's capture (it captures only
the `print!`/`eprint!` machinery). Under brokkr the test binary's stdout is the
libtest JSON event stream (`--format json`). `State::new()` is the convenient
constructor, so it is what tests reach for:

- `run_feature_test_extended_in_process` runs `feature_test_extended.lua` on
  `State::new()`, so ~100 `[PASS] ...` lines go straight onto that stream. It
  checks only that `call` returned `Ok` (relying on the script's `error()` on
  failure, so it is not vacuous), but it is strictly weaker than the CLI run of
  the same file in `run_all_examples`, and it finds the file by walking the whole
  corpus to look up one fixed path.
- `gsub_errors::gsub_string_replacement_inside_dynamic_argument` prints two
  `[PASS] ...` lines on every run. It is also misfiled: a register-allocation
  regression (20 locals plus a dynamic argument) in a gsub error suite.

Fix: a shared test constructor that installs a recording `HostCallbacks`; assert
the captured lines (no `[FAIL]`, at least N `[PASS]`) and read the file by path.
Alternatively give `DefaultCallbacks` a sibling that writes via `print!`, which
libtest captures.

Enforceable: a rule that no test outside the CLI suites uses `DefaultCallbacks`,
or that the shared support module is the only place a State is constructed (with
the binary merge in rewrites.md). A grep for `State::new()` in files whose Lua
calls `print` is crude.

## VA-002 - "Deterministic" tests that compare a run with itself

Reported by: native-stack-errors, runtime, compiler-cost.

Both sides come from the same process, binary and host, so these cannot catch the
cross-host or cross-build nondeterminism replays care about. They do catch
per-State variation within a process (address-keyed hashing, say), which is
worth something, but the names promise more.

- `error_handling.rs::table_sort_inconsistent_comparator_terminates_deterministically`
  (`assert_eq!(run_number(code), run_number(code))`);
- `error_handling.rs::table_move_cost_is_deterministic_across_fresh_states`
  (`assert_eq!(run(), run())`);
- the `math.random` half of `table_insert_move_and_random_validate_before_mutation`
  (two States, seed 99, compared to each other; the name says nothing about the
  RNG);
- `anchor.rs::determinism_two_states_same_sequence_same_output`: the compared
  output is three arithmetic results (`6,105,9`) no anchor-slot behaviour can
  change;
- `string_cost.rs::count_only_costs_are_deterministic_across_runs`: runs one gsub
  source twice in two fresh States; there is no process-wide state on the gsub
  path.

Fix: pin the golden value (`table_move_budget_partial_mutation_is_deterministic`
already does both), compare something slot-dependent for the anchor case (the
anchors' `Debug` after reuse, minus the process-global `state_id`), or delete in
favour of the corpus-wide cost golden in verif-goldens.md.

Enforceable: no.

## VA-003 - Boundary inputs hard-coded instead of derived from the public constants

Reported by: native-stack-errors, compiler-cost, library.

Each silently stops testing its boundary when the constant moves:

- `cost_check_interval.rs`: additions `[1, 2, 63, 64, 65, 127, 128, 129]`,
  "immediately before, at, and after interval boundaries", hard-code
  `COST_CHECK_INTERVAL = 64`, which is public as
  `dellingr::limits::COST_CHECK_INTERVAL` (`mid_run_budget_reset_sees_a_flushed_meter`
  in `cost_golden.rs` does use the constant).
- `cost_golden.rs` `arith_error_after_batch_flush`: "65 loop adds ... spanning
  the 64-unit batch boundary", in a const string inside the fingerprinted corpus,
  so it cannot be derived; an argument for moving boundary cases out of the corpus.
- `cost_determinism.rs` eviction test: `for i in 0..256` is
  `PATTERN_CACHE_ENTRIES` (public).
- `src/compiler/parser/tests.rs`: `too_many_upvalues_errors` (200 + 56, "limit
  255", should be `MAX_UPVALUES`, not even imported);
  `return_255_literals_uses_a_fixed_count` (255, not `MAX_EXPRESSIONS`).
- `compiler_limits.rs`: 254/253/255 argument counts, 255/256 parameters,
  `TooManyAssignmentTargets { count: 256 }`, 300-entry constructors meant to
  cross `MAX_TABLE_BATCH_SIZE`, 256 sites meant to cross `MAX_CACHE_SLOTS`.
- `call_counts.rs`: `limit: 255` in `TooManyArguments`/`TooManyResults` matchers;
  `vm_misc_limits.rs` asserts `TooManyResults { limit: 255 }`; `error_boundary.rs`'s
  NATURAL corpus uses `unpack({}, 1, 255)` (should be `MAX_RESULTS` /
  `MAX_ARGUMENTS`).
- `runtime_result_limit_accepts_255_and_rejects_256` asserts `MAX_RESULTS ==
  u8::MAX`, a fact about two constants that belongs as a `const _: () =
  assert!` beside `MAX_RESULTS`, not a runtime test in a stdlib file.

The rendered error strings ("too many arguments (limit 255)") are fine as message
goldens; the boundary inputs are not.

Enforceable: a textlint rule over `tests/**` and `src/**` test code flagging
63/64/65, 254/255/256 and 65534/65535 inside `.repeat(`, `repeat_n(`, `0..N` and
`1..=N` would catch most with few false positives; a heuristic, not a proof.

## VA-004 - Assertions that accept any error

Reported by: native-stack-errors, compiler-cost, library.

Each passes on any error, including one from a typo in the test script:

- `metamethod_errors.rs::function_type_errors_name_functions` checks
  `error.to_string().contains("function")`, and `Display` includes the
  traceback, whose frames can say "in function"; check `error.kind()`'s text.
- `host_error_reporting.rs::host_call_through_chain_into_failing_lua_function_notifies_once`
  discards the error (`let _error`).
- `compare_branches_keep_comparison_semantics` (parser tests) checks
  `state.call(..).is_err()` for `1 < 'x'`.
- `gsub_function_error_is_propagated` and
  `gsub_table_metamethod_error_is_propagated` assert only `is_err()`; match
  `ScriptError` with payload `"boom"` / `"metamethod error"`.
- `string_call_without_metamethod_is_an_error` asserts only that the message
  contains `"string"`.
- `find_subslice_matches_the_naive_scan_at_every_budget`: the arm
  `(Err(_), Err(_)) => {}` does not check both are `BudgetExceeded`.
- `stdlib_integer_arguments_reject_fractional_numbers_consistently` matches a
  message substring but never the argument number or function name.
- The verifier negatives in `src/compiler/verify.rs` `tree_tests`
  (`registers_must_lie_inside_the_frame`, all four negatives of
  `open_results_need_an_adjacent_consumer_no_jump_enters`, the aliased and
  miscounted variants of `cache_families_cap_below_the_encodable_limit`,
  `a_cache_slot_owns_one_site`) use `assert!(validate_bytecode(&x).is_err())`. A
  malformed chunk can be rejected for another defect. `a_cache_slot_owns_one_site`
  changes the second site's slot to 0 and the declared count to 1, so the
  rejection may be the count/order rule, not ownership.
  `malformed_sizes_report_the_actual_defect` shows the right shape
  (`expect_err(..).reason == ...`).

Fix: pin the kind, variant or payload in each; an `assert_rejects(&bc,
ValidationReason::X)` helper for the verifier.

Enforceable: partly; a textlint rule over `verify.rs` test code banning
`validate_bytecode(` followed by `.is_err()`.

## VA-005 - Tests cannot observe whether a collection, a cache hit or a slot reuse happened

Reported by: runtime, compiler-cost, library.

Several tests exist to prove "a collection at point X does not free Y" or "the
cache was hit/evicted", but nothing lets them assert the precondition happened:

- `gc_upvalues.rs`, `table_metamethods.rs::sequence_values_stay_rooted_across_allocating_handlers`,
  `table_sort_metamethods.rs::proxy_sort_roots_scratch_values_*` and
  `src/vm/tests.rs::auto_gc_marks_a_deep_lua_table_chain_iteratively` arrange
  collections with `gc_set_threshold(1)` or a `force_gc` RustFn; the deep-chain
  test relies on automatic collection happening at a depth large enough to matter.
  `string_cost.rs::gsub_keeps_its_pattern_across_collections_in_its_replacement`
  has no proof a collection ran during the replacement.
- `cost_determinism.rs::gmatch_resume_cost_ignores_fifo_cache_eviction` assumes
  256 distinct patterns evict the iterator's pattern;
  `gmatch_first_use_cost_ignores_a_shared_pattern_cache_hit` has no proof of a hit;
  `string_cost.rs::refusal_leaves_snapshot_state_quiescent` says "matcher refusal
  occurs after the cache hit", unverified. `string_cost.rs` says twice "The
  compile-once half of this check is a crate unit test, since the compilation
  counter is test-only instrumentation": the property is split across two
  binaries because of where the counter lives.
- `string_metatable::cached_handler_is_not_reused_after_it_is_collected` runs
  `for i = 1, 40 do local unused = {upper = i} end` to reuse the collected
  handler's slot; with slotmap versioned keys the stale key may never collide, in
  which case the test cannot fail either way.
  `removing_and_readding_index_after_a_warm_hit` adds 40 keys to "reshape" the
  metatable; nothing asserts the hash slot of `__index` moved.

The existing instrumentation is `#[cfg(test)]` only: `State::pattern_compilations`
is a `cfg(test)` field incremented inside production `cached_pattern_at`, visible
to unit tests only, and it makes the unit-test `State` a different layout from the
integration-test and production one (harmless for correctness, worth knowing for
anything layout-sensitive; the native-stack probes are integration tests and see
the real layout). The `cfg(test)` accessors on `GcHeap`, `UpvaluePool`,
`GlobalEnv`, `Val` are harmless.

Fix: a collection counter on `GcHeap` exposed as `State::gc_cycles()` (cheap, not
replay-visible, useful to embedders as a diagnostic), so "a collection happened
between A and B" is one assertion; expose cache residency and compilation counts,
and slot/index identity, through one test-infrastructure feature or
`#[doc(hidden)]` diagnostic accessor used by both test kinds and compiled out of
production; assert the precondition in each test (or construct the collision
directly in a `vm/` unit test).

Enforceable: yes, as a test pattern once the counters exist.

## VA-006 - Tests filed under the wrong subject, or named by sequence number

Reported by: native-stack-errors, runtime, compiler-cost, library.

A focused run by subject misses these, and coverage reads under a misleading name:

- About half of `error_handling.rs` ("Tests for proper error handling (things
  that used to panic)") is numeric `for`, `ipairs`, `table.insert` rotation,
  `table.move`, `tonumber`, call-syntax sugar and constructor parsing; about half
  of `metamethod_errors.rs` ("invalid metamethod handlers produce errors") is
  inline-cache revalidation.
- `gc_upvalues.rs` (module doc: GC of closed upvalues) also holds codegen tests
  for where upvalues close (`block_upvalue_closes_before_slot_reuse`,
  `numeric_for_closes_*`, `break_closes_*`), tombstone rooting and literal
  reclamation.
- `call_counts.rs` tests argument/result count limits, not call counting or cost;
  `count_encoding.rs` holds the accept side of the 255 limits whose reject side is
  in `call_counts.rs`; `host_concat_count.rs` tests `State::concat`'s stack-index
  errors; `cost_determinism.rs` is documented as "Regression coverage for budget
  accounting and saved gmatch iterator costs"; `lexer_diagnostics.rs` mostly tests
  `Error`'s `Display` prefix selection
  (`selected_caller_frame_does_not_borrow_the_innermost_column`).
- `src/compiler.rs`'s test module is `runtime_cache_tests` but holds the verifier
  corpus proof and the nesting-depth tests; `src/cost_meter.rs` holds
  `gmatch_string_checks_preserve_relative_indexes_in_nested_frames`, an
  argument-index test unrelated to the meter.
- `string_format.rs` holds `script_error_has_source_line_and_state_remains_reusable`
  and `tostring_metamethod_must_return_a_string_like_value` (which also duplicates
  a case in `tostring_and_print_reject_non_string_metamethod_results`);
  `stdlib_argument_errors.rs` holds `runtime_result_limit_*`,
  `runtime_argument_limit_*`, `host_argument_checks_*` and `nested_native_dispatch_*`.
- `vm_test01`..`vm_test11` (`src/vm/tests.rs`) and `test_lexer01`..`test_lexer12`
  (`lexer.rs`) say nothing about what each covers.

Fix: rename and relocate during the binary merge in rewrites.md.

Enforceable: no, beyond a textlint ban on `fn (vm_)?test_?[a-z]*[0-9]+\(` in
`src/**`.

## VA-007 - Duplicate coverage across unit and integration tests

Reported by: native-stack-errors, runtime, compiler-cost, library.

Each copy is cheap; together they are drift risk, and the weaker copy reads as
coverage:

- `string_literal_errors.rs`: its six malformed-escape sources are the same six as
  `literal_string_escape_errors_use_the_backslash_position` (parser tests, which
  also pin the column), and its three long-string sources the same three as
  `long_strings_return_an_error_at_the_opening_bracket` (`lexer.rs`, which pins
  line and column). It proves only that `Program::compile` surfaces the same
  kind, at the cost of one integration binary.
- `error_handling.rs` message tests with disjunctions almost any message meets
  (`error_msg_unexpected_token_includes_context`: `contains("expected") ||
  contains("near")`; `error_msg_missing_end_keyword`: `<eof>` or `expected`;
  `error_msg_call_depth_shows_overflow`: `"call stack overflow"` or `"depth"`;
  `error_msg_budget_exceeded_shows_amounts`: `"budget"` and `"10"`).
  `diagnostic_texts.rs` already pins full texts for the same families.
- `lexer_diagnostics.rs::lparen_on_a_new_line_compiles_as_a_call` only checks that
  it compiles; `a_new_line_paren_continues_the_call` (parser tests) checks it
  compiled as a call.
- The chunk-nesting boundary is proven three times
  (`chunk_nesting_limit_is_enforced_by_the_parser`, the `"function f() "` shape in
  `nested_statement_and_expression_shapes_body`,
  `deepest_nested_functions_parse_on_the_parse_test_stack`);
  `fixed_call_argument_limit_preserves_dynamic_calls` (`compiler_limits.rs`)
  overlaps `fixed_argument_limit_matches_its_error_message`;
  `uncached_slots_templates_and_wide_set_field_at_execute` overlaps
  `wide_literal_indices_and_uncached_slots_round_trip` in `save_state.rs`.
- `set_list_static_units_count_fixed_batches_only` (`instr.rs`) duplicates the two
  `set_list` assertions inside `documented_opcode_charges_match_static_units`.
- `number_rendering::tostring_preserves_lua_negative_zero_spelling` duplicates the
  `(-0.0, "-0")` row of `number_format_g14`; the two files cover one function and
  could be one module.
- `anchor.rs::dropped_anchor_slot_is_recycled_without_collecting` (see VA-019).

Fix: keep the stronger copy; delete or reduce the other to one public-API smoke
case in a merged binary.

Enforceable: no; merging binaries makes the duplication visible.

## VA-008 - The reentrant-opcode coverage guard checks its own copy of the sources

Reported by: runtime, compiler-cost.

`src/vm/frame.rs::reentry_accounting_sources_cover_the_intended_dispatch_opcodes`
exists to prove that `tests/reentrant_opcode_accounting.rs`'s sources exercise
`CALL`, `TFOR_CALL`, `LEN`, `GET_FIELD`, `GET_TABLE`, `SET_FIELD`, `SET_TABLE`
and `SELF`. Its snippets are a hand copy that has already drifted: the integration
cases prefix every source with `local prior = left + right\n`, use
`indexed:method()` where the guard uses `receiver:method()`, and name the length
case `LENGTH` where the guard says `LEN`. Edit the real case so it no longer emits
`SELF` and the guard still passes.

Fix: put the guard in `reentrant_opcode_accounting.rs` and run it over `CASES`
themselves, using the public `Program::disassemble()` text (opcode names are
printed) instead of `crate::compiler::compile`; delete the unit-test copy.

Enforceable: yes, by construction once there is one list.

## VA-009 - Boolean-collapsed script results

Reported by: runtime.

Many tests return `cond and 1 or 0` or one boolean from a long chain of checks, so
a failure says only `0 != 1`. The `error("what broke")` pattern some tests use
(`global_env_proxy.rs`, `table_metamethods.rs`) reports which check failed at no
extra cost.

Enforceable: no.

# Errors and boundaries

## VA-010 - The budget sweep passes if budgets are never enforced

Reported by: native-stack-errors.

`tests/error_boundary.rs::error_boundary_budget_sweep` runs every budget from 0 to
the case's baseline cost. `run()` asserts that an error, if any, is
`BudgetExceeded`, and that a success returns the right value. Nothing asserts any
budget fails. If `set_cost_budget` became a no-op, every run would succeed with the
right value and the sweep would pass; it would still exercise the success path, but
the test name and AGENTS.md describe a budget-refusal sweep.

Fix: assert at least that budget 0 fails; better, that the outcome is monotone:
a threshold below which every budget fails with `BudgetExceeded` and at or above
which every budget succeeds (the signed `remaining` rule in `cost_meter.rs`
defines where).

Enforceable: yes, the assertion is the enforcement.

## VA-011 - `a_failed_lookup_returns_every_unit_it_took` never checks the lookups failed

Reported by: native-stack-errors.

`tests/index_depth_routes.rs`. `try_index` returns whether `get_table` failed, but
the probe `(function() try_index(r) try_index(r) return field_site(two) end)()`
discards both results. If the lookup through `function_behind_table` (two units at
depth `MAX - 1`) started succeeding, or failed before taking any unit, the test
would still pass. The sibling `a_failed_store_returns_every_unit_it_took` in
`newindex_depth_routes.rs` checks (`if not (a and b) then error('the store
succeeded') end`). The same test then calls `call_near_cap` twice with `let _ =`,
"whatever the outcome", so that half proves only that nothing broke afterwards.

Fix: mirror the newindex check.

Enforceable: only by the assertion itself.

## VA-012 - Metamethod tests whose assertions any outcome satisfies

Reported by: native-stack-errors.

- `metamethod_errors.rs::newindex_function_handler`: `expect_ok` only. The handler
  doubles the value and the test reads nothing back (an unused `local log = {}`
  sits in it), so a store that bypassed `__newindex` and wrote raw would pass.
- `metamethod_errors.rs::set_field_cache_value_update_takes_fast_path` names a fast
  path it cannot observe; it asserts only that 100 increments give 100.

Fix: read the stored value back; observe the path (VA-005) or rename.

Enforceable: no.

## VA-013 - `error_boundary_transcript` is evidence nothing compares

Reported by: native-stack-errors.

`error_boundary.rs::error_boundary_transcript` is `#[ignore]`d, cfg'd to debug and
no features, and writes a transcript for "before/after comparison" to a fixed path,
which the next run overwrites. The comparison is manual and the A side is lost when
the B side runs. (Its location and its running under focused `brokkr test` are
filed in verif-corpus.md and bugs.md.)

Fix: commit the transcript as a golden fixture in `tests/fixtures/`, assert
equality in the ordinary sweep, and give it a regenerate path like
`regenerate_save_golden`. It is deterministic by construction (costs, typed errors,
rendered values) and replay-relevant: a change to error text, cost or survivors
would then fail a test rather than depend on someone remembering to diff.

Enforceable: yes, as a golden.

## VA-014 - The poisoning contract is a hand list

Reported by: native-stack-errors.

`panic_poison.rs` doc: "every later public entry point refuses with
`ErrorKind::StatePoisoned`". `assert_refuses_everything` lists about twenty entry
points by hand; the source enforces it with a `self.check_poisoned()?` line at the
top of each `pub fn` (over thirty sites across `vm.rs`, `vm/stack.rs`,
`vm_aux.rs`, `vm/anchor.rs`, `vm/table_ops.rs`). A new `pub fn` that forgets the
line passes everything.

Fix, script: the lexical machinery of `native_calls.py` can list every `pub fn` in
an `impl State` returning `Result` and require its body to start with
`check_poisoned()?` or delegate to `guarded`/`with_...` helpers, with an allowlist
for infallible accessors that must stay safe on a poisoned State (`is_poisoned`,
`cost_used`, `gc_collect`, `release_anchor`...). Fix, structural: route the host
API through one checked accessor (`fn live(&mut self) -> Result<&mut Live>`, with
the methods on `Live`), making the unchecked spelling unrepresentable.

Enforceable: yes, either way.

## VA-015 - Fault-event sites are strings, and eligibility is computed twice

Reported by: native-stack-errors.

`inject_fault(site: &'static str, argument, eligible)` records `(site, argument,
fired)` and drops `eligible`. Consumers re-derive it: `error_boundary_fault_sweep`
filters with `*site != "native_depth" || *argument > 0`, a copy of the rule in
`eval.rs` (`self.native_call_depth > 0`); `string_bytes.rs` counts all events and
skips targets that fired nothing. Two writers of one rule, keyed on a string.
Renaming the site string makes the filter count ineligible events; the sweep then
panics on `position(...).unwrap()` rather than silently passing, so it fails
loudly, but only in a configuration no gate runs (see verif-gates.md).

Fix: an `Event { site: FaultSite, argument: usize, eligible: bool, fired: bool }`
with `FaultSite` an enum, returned by `finish_fault_injection`; consumers filter
on `eligible`.

Enforceable: by the type.

## VA-016 - Smaller items in the error suites

Reported by: native-stack-errors.

- `vm_fault_context.rs` defers to the unit test
  `internal_fault_keeps_innermost_instruction` by name; it exists today in
  `src/vm/tests.rs`, but a rename leaves the comment pointing at nothing. Low
  value; not enforceable short of a grep.
- `error_boundary.rs` sets up the `large` global only when the case source
  contains the substring `large`. It fails loudly when wrong; setup keyed on a
  name.
- `host_call_preflight.rs` and `rustfn_error.rs` count notifications with
  `Arc<Mutex<usize>>` where an `AtomicUsize` (as `host_error_reporting.rs`'s
  `ErrorCounter` uses) would do; three copies of one counter callback could be one
  helper in `tests/common/`. Cosmetic.

Enforceable: no.

# Runtime

## VA-017 - `reassigning_a_rust_function_key_does_not_append_a_duplicate` cannot fail

Reported by: runtime.

`rustfn_identity.rs`. The test counts `pairs` entries where `key == print and
value == "second"`. A stale duplicate would carry `"first"` and be filtered out, so
the count is 1 whether or not the duplicate exists. The unit test
`rust_fn_key_reassignment_updates_in_place` in `src/vm/table.rs` says exactly this
in its own comment and does the storage-level check instead.
`rust_functions_work_as_inline_and_promoted_table_keys` in the same file has the
same blindness (it reads `t[print]`, which finds whichever entry the lookup hits
first).

Fix: count every entry whose key is `print` regardless of value (a duplicate key
shows up twice in `pairs`, observable from Lua), or delete the integration test.

Enforceable: no.

## VA-018 - GC tests that pass for the wrong reason

Reported by: runtime.

- `gc_upvalues.rs::unreferenced_closure_is_collected` asserts `heap_size()` after
  `gc_collect()` is smaller than before. The chunk's own closure becomes garbage
  when the chunk returns, so the first collection frees at least that even if
  `big_table` and `fn` leaked. Measure against a baseline taken after a collection
  that already reclaimed the chunk closure (the `anchor.rs::anchor_fresh_table`
  pattern) and assert the exact delta.
- The "strictly less than" shape in `anchor.rs`
  (`dropped_anchor_is_collected_at_the_next_collection`,
  `anchor_dropped_on_another_thread_is_released`,
  `release_then_gc_collects_anchored_object`) is sound only because
  `anchor_fresh_table` pre-collects; asserting `before - 1` makes it explicit.
- `gc_upvalues.rs::temporary_callable_table_survives_gc_during_call_metamethod_lookup`
  probably never collects: `force_next_gc()` sets a manual threshold of 1, so the
  *next allocation* collects, but after it the script runs `return callable`, the
  `__call` resolution and `return self.value`, none of which allocates (metamethod
  names are interned at construction, the handler closure exists, frames are
  pooled), and `maybe_collect` runs only from `State::allocator()`. Confirm by
  reverting the root it was written for; then make the handler allocate before it
  reads `self` (`local _ = {}`), as
  `executing_returned_closure_roots_closed_upvalues_during_auto_gc` does with
  `trigger_gc = {}`, and assert a collection happened (VA-005).

Enforceable: no, beyond the collection counter in VA-005.

## VA-019 - Anchor slot recycling is not observed by the integration test

Reported by: runtime.

`anchor.rs::dropped_anchor_slot_is_recycled_without_collecting` says each drop is
reclaimed by the next anchor creation; its assertion is `anchor_count() == 0`.
`anchor_count` counts a drop at once ("A drop counts at once, before the State
reclaims the slot"), so the assertion holds whether or not slots are recycled.
Recycling is tested by the unit test `vm/anchor.rs::insert_recycles_dropped_slots`
(`registry.slots.len()`).

Fix: delete it, or move the assertion it wants into the unit test.

Enforceable: no.

## VA-020 - `the_cap_does_not_charge_cost` tests a path that structurally cannot charge

Reported by: runtime.

`src/vm/tests.rs`. `push_nil` goes through `check_stack_slot(&self)`, which takes
`&self` and has no access to the cost meter, so `cost_used() == before` cannot
fail. The AGENTS.md claim ("enforcing it charges no cost") matters on paths that
*can* charge: frame admission from `OP_CALL`, open-results windows, library
preflights inside dispatch, where a refused check sits next to batched opcode cost.

Fix: drive the cap from Lua (a call whose admission is refused) and compare
`cost_used` to the same script cut off just before the call; or delete the test
and rely on the `&self` signature, which is the real enforcement, with a comment
saying so.

Enforceable: the `&self` receiver on every stack check is a type-level guarantee
for the host push family; keeping all `check_stack_*` `&self` is review-only
unless they move into a type that holds no `CostState`.

## VA-021 - GC control tests with a tautology and a dead half

Reported by: runtime.

- `src/vm/tests.rs::gc_host_controlled` opens with
  `assert!(state.heap_size() >= initial_objects + initial_strings)`; `heap_size()`
  is `GcHeap::allocation_count()`, defined as
  `objects.len().saturating_add(strings.len())`. Both sides come from the same two
  fields. The rest of the test is real. Delete the line or assert `==` and call it
  a definition check.
- `gc_threshold_control` ends after `gc_set_threshold(1)` and an allocating script
  in two comments ("Now GC should be needed ... threshold may have been adjusted")
  and no assertion. A manual threshold is preserved across collections
  (`GcThreshold::manual`, checked by `gc_policy.rs`), so the hedge is stale; assert
  `gc_threshold() == 1` or cut the half.

Enforceable: no.

## VA-022 - `gc_policy.rs` never exercises `minimum_threshold`

Reported by: runtime.

Both policy tests compute `survivors * growth_factor` and compare with
`.max(minimum_threshold)`. A `State::with_gc_policy` State has run `open_libs`, so
`survivors` is in the hundreds and the product dwarfs the minimum (7 and 11). If
production ignored `minimum_threshold`, both pass. The minimum is part of the
documented contract (`docs/limits.md` "Garbage collection policy") and of replay
timing. Also, `custom_gc_policy_is_used_before_and_after_state_construction` names
"before construction" (`docs/limits.md`: "The initial threshold is in force while
`open_libs` initializes the standard library") but only reads the threshold after
construction; whether `open_libs` ran under it is not observable without the
collection counter in VA-005.

Fix: a case on `State::raw()` (or with a minimum larger than `survivors * growth`)
where the minimum wins.

Enforceable: no.

## VA-023 - Path-parity tests do not prove the two paths differ

Reported by: runtime.

`table_metamethods.rs::metamethod_path_charges_like_the_raw_path` compares a
`__tostring` table ("keeps the fast path") with an `__index` table (forces the
metamethod path); `table_sort_metamethods.rs::proxy_sort_charges_like_raw_sort`
and `table_with_unrelated_metatable_keeps_raw_sort` do the same for sort. Which
path a table takes is not observable from the public API. If path selection
changed so that any metatable (or none) routes through the metamethod path, both
scripts take the same path and parity passes trivially.

Fix: make each path evidence itself (a hole in the sequence so the `__index`
handler runs and counts in one variant and is shown not to run in the other), or
move the parity tests to a unit test reading a `cfg(test)` path counter, as
`pattern_compilations` instruments the pattern cache.

Enforceable: no general rule.

## VA-024 - `global_write_cache.rs` names cache behaviour it cannot observe

Reported by: runtime.

`warm_writer_inserts_populates_then_hits_and_closures_share_the_site`,
`append_and_g_proxy_writes_do_not_displace_a_warmed_global` and
`builtin_rebind_does_not_disturb_a_warmed_writer` assert only final global values.
With the cache disabled every one passes. They are correctness-under-caching
tests, worth having, but the names promise "populates", "hits" and "shares the
site". The unit tests in `src/vm/global_env.rs` and `src/vm.rs::runtime_tests`
show the in-crate way to inspect cache cells (`read_cached`, `cache_cells`).

Fix: rename to what they check, and add hit/share assertions as unit tests that
can read `RuntimeCaches`.

Enforceable: no.

## VA-025 - `restricted_env_counts_a_repeated_whitelist_name_once` is vacuous without debug assertions

Reported by: runtime.

The test calls `gc_collect()` and asserts nothing; its comment says "Collection
checks the tombstone count in debug builds". Under `brokkr test ... --release` it
passes unconditionally. `src/vm/object.rs` and `src/vm/table.rs` gate their
debug-assert-only tests with `#[cfg(debug_assertions)]`; this one does not. The
gate runs lib unit tests only in dev, so the gap is in focused release runs.

Fix: `#[cfg(debug_assertions)]` on the test, or assert the tombstone count directly
through the `cfg(test)` `GlobalEnv::len`.

Enforceable: partly; a convention check could require a test whose body has no
`assert`/`expect_err`/`should_panic` to carry `cfg(debug_assertions)`.

## VA-026 - Smaller runtime assertion gaps

Reported by: runtime.

- `vm_test07` and `vm_test10` assert only `global_env.get("a").is_none()`; if
  `set_global` broke or the chunk returned early they pass. Add a positive control.
- `table_resolution.rs::budget_exhaustion_mid_move_keeps_the_paid_prefix`: the doc
  says the move "copies exactly the elements it paid for, one unit each"; the
  assertions are `0 < copied < 100` and `copied <= used`. Compute the exact prefix
  from the budget and the call's known cost and assert equality.
- `src/vm/table.rs::Model::array_key` calls production `array_index`, so the model
  and the table agree on key classification by construction. Not a hole while
  `key_rule_tests::numeric_key_boundaries` pins `array_index` separately; if that
  is relaxed, the model test silently stops covering classification.
- `frame.rs::documented_opcode_charges_match_static_units` ends with
  `documented_rows == classes.len() + 1` with no comment on the extra row; a
  failure will be hard to read.
- `fcomm2`/`fleet:group()` mentions in `src/vm/tests.rs` and `gc_upvalues.rs`
  describe a host this repository cannot see; the tests are fine, the comments are
  claims nobody can check.

Enforceable: no.

## VA-027 - "Proxy" vocabulary for `_G` is false since 41ad774

Reported by: runtime.

`_G` is the real global table (41ad774; AGENTS.md, `global_env.rs`). Still saying
"proxy": the file name `tests/global_env_proxy.rs` (its module doc is updated);
`src/vm/tests.rs::open_libs_with_values_on_the_stack_targets_its_own_g_table`
(message `"_G keeps its proxy metatable"`);
`global_write_cache.rs::append_and_g_proxy_writes_do_not_displace_a_warmed_global`
and the `proxy_marker` global in `builtin_rebind_does_not_disturb_a_warmed_writer`;
`plain_field_miss.rs::global_proxy_does_not_expose_table_library_names`; and
`src/snapshot_format.rs`'s header (filed with the snapshot laterals in
verif-goldens.md). False today.

Enforceable: a textlint rule banning `proxy` near `_G` in `src/` and `tests/`.

## VA-028 - The `drain_mark_worklist` assertion has no test

Reported by: runtime.

AGENTS.md: "A `debug_assert!` in `drain_mark_worklist` enforces this" (root through
`GcHeap::mark`, never a bare worklist push). `object.rs` has `should_panic` tests
for "GC marked a vacant object key" and "... string key", none for "GC worklist
entry was not marked reachable". If the assertion were weakened or deleted, nothing
would notice. True today, checkable.

Fix: a `#[cfg(debug_assertions)] #[should_panic(expected = "pushed directly")]`
test that pushes a live allocation onto the worklist without `mark`.

Enforceable: yes, by that test.

## VA-029 - The test-side cache enumerations are not exhaustive

Reported by: runtime.

`RuntimeCaches::named_handles` (`#[cfg(test)]`, `caches.rs`) iterates
`self.global_lookup`, `self.field_lookup`, ... by field access, and
`runtime_tests::cache_cells` plus the family list in
`collection_clears_cache_entries_naming_freed_objects` repeat that list. The
production sweep destructures exhaustively; the test oracle does not. A new cache
family would be cleared by production (compile-forced) but reported by neither
test enumeration, so "no entry names a freed value" would silently not cover it.

Fix: destructure in `named_handles` the same way and derive the test's families
from it.

Enforceable: yes, by the destructuring.

## VA-030 - "Every RustFn call path shares that check" covers four paths

Reported by: runtime.

`rust_fn_result_count.rs` module doc. Covered: direct host call, Lua call,
generic-for iterator, nested host call. Not covered: a RustFn as a metamethod
handler (`__index`, `__newindex`, `__call`, `__len`, `__tostring`), as a
`table.sort` comparator or `gsub` replacement, through `call_anchor`.
`reference/native-calls.toml` is the inventory of exactly those paths.

Fix: drive the over-reporting function through every inventoried entry that can
reach a RustFn, as `tests/native_stack.rs` walks the inventory.

Enforceable: yes, if the scenario list is generated from (or checked against) the
inventory, as `native_calls.py` does for `native_stack.rs`.

## VA-031 - Growth-limit coverage exists only under `snapshot`

Reported by: runtime.

`sequence_copy_refuses_an_unallocatable_snapshot_gap`,
`snapshot_gap_growth_refuses_append_and_both_window_extensions`,
`snapshot_length_limit_refuses_append_insert_and_migration`,
`host_insert_growth_failure_preserves_operands` and
`snapshot_growth_failure_reaches_the_callback_as_an_ordinary_error` reach
`MAX_ARRAY_LEN` through `Table::rebuild_from_snapshot`, which exists only under
`snapshot`. The growth-failure path is not snapshot-specific (`MAX_ARRAY_LEN` and
`TableError::GrowthFailed` are in the default build), so in the default sweep, the
shipped consumer build, it has no test.

Fix: a `#[cfg(test)]` constructor that sets a table's logical length and window
directly, used by both sweeps; keep one snapshot-specific test for the load path.
(The `Limits` rewrite in rewrites.md is the broader alternative.)

Enforceable: no general rule.

# Compiler and cost

## VA-032 - `direct_and_native_meters_share_budget_semantics` compares a function with itself

Reported by: compiler-cost.

`src/cost_meter.rs`. `consume_cost` is `check_poisoned()` then `charge_cost`, and
`charge_cost` is `self.cost_meter().consume(cost)` (`src/vm.rs`), so the asserted
agreement on acceptance, `cost_used` and `cost_remaining` cannot fail. The only
independent bit is the trailing `cost_used() == u64::MAX` saturation check in
count-only mode. `consume_units_matches_the_unit_at_a_time_loop_exactly` is the
real independent oracle.

Fix: delete the equality half, keep the saturation assertion.

Enforceable: no.

## VA-033 - Leftover load halves and a snapshot block with nothing between its two saves

Reported by: compiler-cost.

- `compiler_limits.rs` (`expect_syntax_error`,
  `set_field_sites_past_the_cache_capacity_use_the_uncached_path`) and
  `string_literal_errors.rs` (`malformed_escapes_return_syntax_errors_through_state_loading`)
  chain `.and_then(|program| State::new().load(&program))` after a compile they
  expect to fail, or compile the same source twice
  (`dellingr::Program::compile(...).and_then(load)` and `Program::compile(...)`
  "should accept ..."), which is the same function. It reads like residue of the
  migration from a `load_string`-style API; the name
  "...through_state_loading" claims a path never reached because the compile fails
  first. Collapse to one call per case and rename.
- `name_limits.rs::source_names_accept_the_limit_and_reject_one_more_byte`, under
  `cfg(feature = "snapshot")`, takes `before = state.save_state()` and immediately
  asserts `state.save_state() == before` ("rejected load leaves state savable"). The
  rejected operation (the oversized-name `Program::compile`) happened earlier and
  has no State to disturb. The file's doc ("rejected ... without disturbing the
  State") promises a property the test cannot violate. Drop the block and the
  claim.

Enforceable: a textlint rule over `tests/**` banning `State::new().load(` (a load
into a State nobody keeps) catches the first shape.

## VA-034 - `table_concat_cost_is_identical_with_or_without_a_budget` discards both outcomes

Reported by: compiler-cost.

`string_cost.rs`. Each run is `match call { Ok(_) | Err(_) => {} }` and only the
two `cost_used` values are compared. "Some cases in this list fail on an invalid
element on purpose", but nothing checks which case fails or where; if the first
case started failing at its first instruction in both modes, both report the same
cost and the test passes.

Fix: assert the outcome per case (Ok for the first, `ArgError`/`TypeError` for the
second) and pin its cost.

Enforceable: a textlint pattern banning `Ok(_) | Err(_) => {}` in tests.

## VA-035 - `runtime_cache_population_and_hits_charge_nothing` runs cold against cold

Reported by: compiler-cost.

`bytecode_runtime.rs`. The doc says the second execution "re-parses (a fresh
Bytecode identity) while the State already carries warm entries for identical
code". Runtime entries are keyed by `Arc::as_ptr(bytecode)`
(`State::resolve_bytecode_runtime`), so a re-parse gets a brand-new cold runtime;
nothing is warm except interned strings. The only warm hit is inside the first run
(`f(b)` after `f(a)`).

Fix: also `load` the same `Program` a second time, which reuses the runtime and
hits warm caches. The corpus-wide version is the cache-disable differential in
verif-goldens.md.

Enforceable: no.

## VA-036 - Single assertions weaker than their names

Reported by: compiler-cost.

- `zero_static_cost_generic_for_is_not_labeled_free` (`src/lib.rs`) asserts
  `!rendered.contains("(free)")`; no code path emits "(free)" any more. A
  tripwire, not coverage.
- `gmatch_full_scan_is_linear_in_its_subject` (`string_cost.rs`) accepts
  `long <= 3 * short` for a doubling; a quadratic term small next to the fixed
  per-iteration charges passes. Pin the exact `cost(256) - cost(128)`.
- `decimal_and_hex_literals_use_the_shared_numeral_parser` compares the parser's
  pooled literal with `crate::numeral::parse_lua_numeral`, the function the lexer
  calls: a wiring test, not numeral coverage.
- `static_analysis_counts_each_instruction_once` (`src/lib.rs`) asserts
  `instructions == 10` for a loop: a codegen golden inside a cost test that any
  codegen improvement breaks while the cost property is unchanged. Move it or
  assert only the cost fields.
- `compiler_limits.rs::long_jumps_compile_and_run`: "Far past the 16-bit jump range
  of the stack VM, inside the 24-bit one." The stack VM is gone; 40,000-instruction
  jumps are near no boundary of the 24-bit encoding, and
  `jump_patching_rejects_offsets_past_the_encoding` (`codegen.rs`) covers the real
  boundary. It costs three 40k-line compiles per sweep; delete or reword as a smoke
  test.

Enforceable: no.

## VA-037 - `COST_CHECK_INTERVAL` has no injection point, so the batching test needs a hand model

Reported by: compiler-cost.

`cost_check_interval.rs` cannot run the VM at interval 1 and 64 and compare,
because the interval is a crate-private constant; it compares against
`immediate_flush_reference`, a hand-written unit-at-a-time model. The model is
independent and correct but covers only a straight line of global-add statements;
the documented property ("the interval does not change the charged prefix or
`cost_used` at `BudgetExceeded`") is general.

Fix: a test-infrastructure feature that sets the interval to 1, and a differential
over the corpus and the error corpus at a sweep of budgets (the sweep machinery in
`error_boundary.rs` already walks budgets). The production constant stays fixed.

Enforceable: by the differential test.

## VA-038 - Tests poke private state to build preconditions

Reported by: compiler-cost.

`gmatch_string_checks_preserve_relative_indexes_in_nested_frames` sets
`state.stack_bottom = 2` directly; `literal_pool_limits_match_their_error_messages`
fills `literal_indices.strings` without the matching `chunk.string_literals` (the
number half fills both). Both construct states production cannot reach, so they
can pass against invariants the real code never relies on, or fail when a
representation changes without behaviour changing.

Fix: reach the precondition through the public path (a nested Rust call for the
stack bottom; real literals, or the 65,536-literal source the parser would see).

Enforceable: no.

## VA-039 - Three hand-maintained per-opcode operand descriptions

Reported by: compiler-cost.

`compiler/verify.rs` (which operand is a literal, a cache slot, a nested index),
`disassemble.rs`'s `write_annotation` (the same mapping again) and the dispatch
loop each know which field of which opcode means what. `opcode_rows!` already
carries layout, cost and arithmetic function; adding operand kinds there and
deriving the verifier's pool checks and the disassembly annotations from it removes
two of the three copies.

Enforceable: yes, structurally.

# Library

## VA-040 - Library tests whose oracle is the code under test

Reported by: library.

- `number_rendering::concat_number_rendering_matches_tostring_across_f64_classes`:
  `tostring`, host `State::concat` (via `concat_values`), `OP_CONCAT` and
  `Val::to_bytes_with_heap` all call `vm::lua_val::number_to_string`, so asserting
  `concat == tostring` over 16k bit patterns cannot fail unless a second rendering
  path appears, and would miss a fast path in `OP_CONCAT` since it calls the host
  API, not Lua `..`. `string_format::format_general` is an independently written
  `%g` engine (`format_scientific` plus `format_fixed`, sharing only core float
  formatting with `format_g14`). Assert `tostring(x) == string.format("%.14g", x)`
  over the same sweep plus a few thousand random bit patterns, through Lua `..` as
  well.
- `patterns::tests::plain_classifier_tracks_pattern_grammar_magic_bytes` loops over
  `b"^$*+?.([%-)"`, byte for byte the production `PATTERN_MAGIC_BYTES`, never
  consults the grammar its name mentions, and does not check the other 245 bytes.
  For every byte assert `is_plain(&[b])` holds exactly when `LuaPattern::new(&[b])`
  matches `[b]` as a one-byte literal at `0..1`; likewise `is_plain_for_find`
  against `string.find`'s plain path.
- `patterns::luapat::tests::escape_class_table_matches_the_per_byte_classifier`:
  the oracle `match_class` and `table_match_class` are the same match statement
  over the same `is_ascii_*` predicates, both calling
  `numeral::is_lua_whitespace`. It checks the letter-to-index mapping, not the
  replay-visible class membership (`%p`, `%c`, `%g`). Add an unmarked `examples/`
  script printing, for each of the 22 class letters, the matching bytes in
  `0..=255`, so the diff gate compares it with lua5.2/lua5.4 (`diff_test.sh` exports
  `LC_ALL=C`).
- `patterns::errors::tests::structured_details_keep_existing_display_text`:
  `PatternError`'s `Display` is `PatternErrorDetail::from(self)`'s, so its two
  assertions test one code path twice; drop one.
- `golden::pinned_regression_inputs_survive_golden_regeneration`: the first half
  asserts that `inputs` contains the pair it just read from `PINNED_REGRESSIONS`;
  the second half (a row lookup in `GOLDEN`) is real.

Enforceable: the tests themselves; the class-membership check by the diff gate.

## VA-041 - Names that claim more than the body

Reported by: library.

`argument_details_are_structured_without_changing_text` checks `detail()` only,
never the text.

Enforceable: no.

## VA-042 - "Verified against Lua 5.2/5.4" expectations frozen in Rust literals

Reported by: library.

`bracket_ranges_follow_reference_branch_order`,
`accept_empty_repeats_do_not_consume_match_depth`,
`string_library_coerces_number_subjects_and_patterns`, the `*_match_lua_54` tests in
`string_format.rs` and every row of `number_format_g14` say their expected values
were "measured against both Lua 5.2 and 5.4". Those are one-time captures; the diff
gate is the machine that keeps such captures honest, and these are outside it.

Fix: move each script-level expectation that does not depend on a deliberate
divergence into an unmarked `examples/` file; keep Rust tests for host-API and
structured-error assertions.

Enforceable: by the diff gate.

## VA-043 - Optional-argument behaviour is claimed per function and tested for two

Reported by: library.

`lua_std.rs`: "An optional argument that is absent or nil takes its default ...
they are not shared code". Since each function reads its own optionals, the claim
needs per-function coverage. Tested: `math.atan` and `math.log`
(`stdlib_divergences::optional_math_nil_arguments_use_their_defaults`),
`error(msg, nil)`, the `vm_aux` helper. Untested with explicit nil: `string.sub`
`j`, `find` `init`/`plain`, `match` and `gmatch` `init`, `gsub` `n`, `concat`
`sep`/`i`/`j`, `unpack` `i`/`j`, `table.remove` `pos`, `tonumber` `base`.
(`tests/stdlib_optional_args.rs`, in the hunt's brief, does not exist and never
has.)

Fix: a table-driven test, one row per (function, optional parameter), asserting
`f(required..., nil) == f(required...)` in results and cost; structurally, declare
each library function's signature in one table both registration and the test read.

Enforceable: yes, by the test or the declarative table.

## VA-044 - FMA test modes duplicate one another and change silently with the host

Reported by: library.

`fma_modes()` returns `Default`, `Software` and, when available, `Hardware`. On an
FMA-capable x86 host `Default` takes the `Hardware` path; on a host without FMA, the
`Software` path. The golden evaluates every row three times where two would do, a
third of its cost, paid twice per check. `kernel_dispatch_honors_forced_fma` skips
`Default`, so nothing asserts production selection picks hardware when available: a
broken `is_x86_feature_detected` path would silently run every kernel in software,
a performance loss invisible to every test. On an x86 host without FMA, or any host
other than x86/aarch64, `fma_modes()` drops `Hardware` and `fma::check` skips the
hardware comparison: tests pass with a smaller scope and report nothing, so an old
CI box would read as full coverage.

Fix: drop `Default` from the golden loop; assert in the dispatch test that `Default`
records the hardware backend iff `hardware_fma` is `Some`; print the modes
exercised once (as the golden failure message already does).

Enforceable: the dispatch assertion by test; the host coverage is information, not
a gate.

## VA-045 - Parsers and formatters with thin cross-checks

Reported by: library.

- Three hex-float parsers, none checked against another: `crmath::hf` (const,
  rejects inexact literals), `numeral::parse_hex` (runtime, rounds), and
  `oracle::parse_hex_float` (test, exact through libbeef then rounded).
  `numeral::parse_hex` is tested on about 30 hand-picked cases. A randomized
  differential in the lib test binary comparing `parse_lua_numeral` with
  `parse_hex_float` over random mantissas and exponents, long digit strings and
  subnormal boundaries is cheap and strong.
- `%a` round-trip is tested for six values
  (`quoted_floats_round_trip_through_the_parser`). Running `format_hex_float` then
  `parse_lua_numeral` over the 16k bit-class sweep `number_rendering.rs` uses, at
  every precision `0..=13`, would cover the formatter's rounding branch.

Enforceable: by the tests.

## VA-046 - Stale claim in `concat_values`

Reported by: library.

`src/vm/eval.rs` `concat_values`: "`1e308`, which Rust renders as 309 bytes".
Numbers render through `number_to_string` (`%.14g`), so `1e308` is `"1e+308"`, six
bytes. The point about exact lengths stands; the example describes the old
renderer. Reword without the figure.

Enforceable: no.

# Snapshot

## VA-047 - `saves_are_byte_stable` checks only that saving is pure

Reported by: snapshot.

It saves one State twice in a row and compares. That fails only if `save_state`
mutates the State, and it takes `&self`. The determinism that matters is across
processes, address-space layouts and slotmap histories: `object_ids`, `string_ids`
and `env_reverse` are `BTreeMap`s keyed by `ObjectPtr`/`StringPtr`, and
`rust_fn_ids_by_addr` is keyed by function address. Only the golden exercises
cross-process stability, and only in debug. The code comment at `finish` ("Address
keys are lookup-only") is the only guard against iterating a pointer-keyed map in
the walker, and it is unenforced.

Fix: build the same logical state twice by different routes so slotmap keys differ
(State A allocates and drops 1000 throwaway tables first, then builds the graph;
State B builds it directly; same for strings and closures) and compare bytes.

Enforceable: the test is the enforcement; no lint can express "do not iterate a
pointer-keyed map in the save walker".

## VA-048 - Snapshot tests that cannot tell their claim from a near miss

Reported by: snapshot.

- `literal_identity_strings_follow_chunk_ids_instead_of_addresses` orders chunks by
  descending `Arc` address, encodes in that order and expects literals in the same
  order. It catches an ascending-address sort; a descending-address sort also
  passes. Run it twice with the encode order flipped.
- `tombstones_are_preserved_in_snapshots` asserts `tombstoned_bytes.len() >
  clean_bytes.len()`. In case 1 the extra bytes are already supplied by the dead
  key's string `"b"` entering the string pool, whether or not slot history is
  encoded; the pairs-order half carries the weight. Decode both saves and assert
  the dead slot itself is present.
- `every_save_fixture_is_declared_and_referenced` checks "declared" only.
  `FIXTURE_NAMES` is a second spelling of `save_golden_program::GOLDEN_FIXTURE`;
  rename the constant and `FIXTURE_NAMES` still names the old file. Derive one from
  the other.
- `saved_main_chunk_keeps_zero_defined_line` round-trips `valid_saved_bytecode()`
  and asserts nothing about main chunks or line 0 beyond
  `saved_bytecode_round_trips`.

Enforceable: the tests themselves; the fixture name by derivation.

## VA-049 - `corruption_sweep_never_panics` covers one mutation of one small save

Reported by: snapshot.

It truncates at every prefix and XORs `0xff` at every offset of one save of a
four-global program. One mutation kind (no single-bit flips, length fields set to
`u32::MAX`, swapped ids). Many flips land in the `EngineVersion` string, which load
reads and discards. A flip that loads successfully is never exercised, so a forged
save that loads and then breaks an interpreter invariant on first use (the
verifier's purpose) is invisible. The comment says "never panics or OOMs"; an OOM
aborts the process and nothing bounds or measures allocation. Cost: about 2x the
save length in `load_state` calls (the similar golden program saves 2467 bytes, so
roughly 5000 loads), each successful flip building a full standard State in debug;
unmeasured (`brokkr test corruption_sweep --timings` would give it).

Fix: after each successful mutated load, call every global function, iterate every
table with `pairs`, then `save_state`; run over a corpus of saves (the corpus round
trip in verif-goldens.md); move open-ended mutation to a `cargo fuzz` target with a
grammar-aware mutator over `SavePayload`.

Enforceable: the in-check sweep is a test; a fuzz target cannot gate, but a
fixed-seed bounded mutator can.

## VA-050 - Four hand-written copies of the save header and five hand-built payloads

Reported by: snapshot.

The header (`write_magic_and_versions` plus the `CARGO_PKG_VERSION` field) is
written in production `save_state()`, `integrity_tests::encode_fixture`,
`tests::load_bytecode_with_globals` and `tests::forged_literal_save`. If the header
changes, the test copies keep producing the old shape and their failures read as
decoder rejections, not stale fixtures. `SavePayload { .. }` literals are
hand-built in five places (those three, `integrity_tests::empty_payload`, and
`verify.rs`'s `payload` and `gmatch_payload`); every new payload field touches all
five.

Fix: one `fn encode_save(payload: &SavePayload) -> Result<Vec<u8>, SaveError>` that
`save_state` and every test call, and one `SavePayload::empty(standard_env: bool)`
test constructor.

Enforceable: a textlint rule allowing `write_magic_and_versions(` only inside
`encode_save`.
