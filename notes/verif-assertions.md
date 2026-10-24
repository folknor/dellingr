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
or that the integration crate's `support` module (which now has a recording
`Recorder` callbacks type) is the only place a State is constructed. A grep for
`State::new()` in files whose Lua calls `print` is crude.

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

## VA-008 - The reentrant-opcode coverage guard checks its own copy of the sources

Reported by: runtime, compiler-cost.

`src/vm/frame.rs::reentry_accounting_sources_cover_the_intended_dispatch_opcodes`
exists to prove that `tests/integration/cost/reentrant_opcode_accounting.rs`'s sources exercise
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
(`global_table.rs`, `table_metamethods.rs`) reports which check failed at no
extra cost.

Enforceable: no.

# Errors and boundaries

## VA-010 - The budget sweep passes if budgets are never enforced

Reported by: native-stack-errors.

`failures::error_boundary::error_boundary_budget_sweep` runs every budget from 0 to
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
loudly, in the `instrumentation-faults` sweep.

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

Enforceable: no.

# Runtime

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

## VA-029 - The cache-collection test's family list is still hand-maintained

Reported by: runtime.

`RuntimeCaches::named_handles` and the `runtime_tests::cache_cells` oracle now
destructure exhaustively, so a new cache family must be enumerated there. But
`collection_clears_cache_entries_naming_freed_objects` (`src/vm.rs`) still
checks a fixed list of families. Deriving the list from the warmed entries
was tried and rejected: a family whose warmup stopped filling any cell would
then drop out silently. A new family is therefore enumerated by the oracle but
not required by the test.

Fix: keep the fixed list and also assert that it equals the set of families
`cache_cells` can produce (derived from the exhaustive destructuring, not
from what the warmup happened to fill), so adding a family fails the test until
the list and the warmup cover it.

Enforceable: yes, by that assertion.

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

## VA-051 - Strings and env baselines sit outside the save inventory's checks

Reported by: the agent that made State's root and in-flight inventories
exhaustive (`src/vm/inventory.rs`).

The weak tables are now an exhaustive `WeakTable` inventory. What remains:

- `save_walker_encodes_exactly_what_serialized_roots_reach` compares objects
  only, and only for the root kinds its fixture fills. Strings (pins, builtin
  and event names, chunk literals) are outside it.
- That env baselines can be rebuilt on load is a stated invariant of
  `capture_env_tokens`, not something the code checks.

Fix: extend the identity test to the string pool; check the env-baseline
invariant on load or in a test.

Enforceable: by those tests.

# Compiler and cost

## VA-035 - `runtime_cache_population_and_hits_charge_nothing` runs cold against cold

Reported by: compiler-cost.

`bytecode_caches.rs`. The doc says the second execution "re-parses (a fresh
Bytecode identity) while the State already carries warm entries for identical
code". Runtime entries are keyed by `Arc::as_ptr(bytecode)`
(`State::resolve_bytecode_runtime`), so a re-parse gets a brand-new cold runtime;
nothing is warm except interned strings. The only warm hit is inside the first run
(`f(b)` after `f(a)`).

Fix: also `load` the same `Program` a second time, which reuses the runtime and
hits warm caches. The corpus-wide version is the cache-disable differential in
verif-goldens.md.

Enforceable: no.

## VA-036 - `decimal_and_hex_literals_use_the_shared_numeral_parser` is a wiring test

Reported by: compiler-cost.

The test (`src/compiler/parser/tests.rs`) compares the parser's pooled literal
with `crate::numeral::parse_lua_numeral`, the function the lexer calls: a wiring
test, not numeral coverage. Rename it to say it checks wiring, or give it
independent expected values.

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

## VA-052 - Leftovers from the cost-fixture and oracle changes

Reported by: the resolution loop's reviewer.

- `src/crmath/tests/golden.rs::pinned_regression_inputs_survive_golden_regeneration`
  no longer checks regeneration (that half was tautological and was removed);
  only the golden-row lookup remains, so the name promises more than the body.
- `tests/integration/cost/cost_golden.rs::table_library_native_charges_are_one_or_zero`
  still hard-codes the seven table-library costs that the cost fixture's
  `table_*` rows now pin: a second, unversioned copy.

Fix: rename the crmath test to what it checks; make the table-library test
read the fixture rows (or delete it in favour of them).

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
