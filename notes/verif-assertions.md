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
- `string_charges.rs::count_only_costs_are_deterministic_across_runs`: runs one gsub
  source twice in two fresh States; there is no process-wide state on the gsub
  path.

Fix: pin the golden value (`table_move_budget_partial_mutation_is_deterministic`
already does both), compare something slot-dependent for the anchor case (the
anchors' `Debug` after reuse, minus the process-global `state_id`), or delete in
favour of the corpus-wide cost golden in verif-goldens.md.

Enforceable: no.

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
- `gsub_errors.rs::gsub_string_replacement_inside_dynamic_argument` is a
  register-allocation regression (20 locals plus a dynamic argument) filed in a
  gsub error suite.
- `string_format.rs` holds `script_error_has_source_line_and_state_remains_reusable`
  and `tostring_metamethod_must_return_a_string_like_value` (which also duplicates
  a case in `tostring_and_print_reject_non_string_metamethod_results`);
  `stdlib_argument_errors.rs` holds `runtime_result_limit_*`,
  `runtime_argument_limit_*`, `host_argument_checks_*` and `nested_native_dispatch_*`.
- `vm_test01`..`vm_test11` (`src/vm/tests.rs`) and `test_lexer01`..`test_lexer12`
  (`lexer.rs`) say nothing about what each covers.

Fix: rename and relocate. The integration suites are now modules of one crate
grouped by subject (`tests/integration/<subject>/`), so moving a test is a cut
and paste; the merge moved whole files only.

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
  kind.
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
case in the integration crate.

Enforceable: no.

## VA-009 - Boolean-collapsed script results

Reported by: runtime.

Many tests return `cond and 1 or 0` or one boolean from a long chain of checks, so
a failure says only `0 != 1`. The `error("what broke")` pattern some tests use
(`global_table.rs`, `table_metamethods.rs`) reports which check failed at no
extra cost.

Enforceable: no.

# Errors and boundaries

## VA-016 - Smaller items in the error suites

Reported by: native-stack-errors.

- `vm_fault_context.rs` defers to the unit test
  `internal_fault_keeps_innermost_instruction` by name; it exists today in
  `src/vm/tests.rs`, but a rename leaves the comment pointing at nothing. Low
  value; not enforceable short of a grep.
- `error_boundary.rs` sets up the `large` global only when the case source
  contains the substring `large`. It fails loudly when wrong; setup keyed on a
  name.

Enforceable: no.

# Runtime

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

# Library

# Snapshot
