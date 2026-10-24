# Verification hunt: runtime

Scope: the VM's in-tree tests (`src/vm/tests.rs`, `src/vm/object.rs`,
`src/vm/object/collector.rs`, the test modules in `src/vm/*.rs` and
`src/vm.rs`) and the runtime integration suites (`anchor`, `gc_policy`,
`gc_upvalues`, `global_env_proxy`, `global_write_cache`,
`host_table_api_atomicity`, `library_function`, `open_libs_registry`,
`plain_field_miss`, `restricted_env_open_libs`, `rust_fn_result_count`,
`rustfn_identity`, `table_constructors`, `table_index_regressions`,
`table_insert_index`, `table_iteration`, `table_metamethods`,
`table_resolution`, `table_sort_metamethods`, `tostring_identity`).

Nothing was run; every finding is from reading. Where a finding says a test
"cannot fail", the reasoning is given so it can be checked; two of them
(1.3, 1.4) should be confirmed by deliberately breaking the code under test
once before the test is rewritten.

Not ranked. Each finding ends with whether the fixed version can be held
mechanically.

---

## 1. Tests that prove nothing

### 1.1 `rustfn_identity.rs::reassigning_a_rust_function_key_does_not_append_a_duplicate` cannot fail

The test counts `pairs` entries where `key == print and value == "second"`.
A stale duplicate entry would carry the value `"first"` and be filtered out,
so the count is 1 whether or not the duplicate exists. The unit test
`rust_fn_key_reassignment_updates_in_place` in `src/vm/table.rs` says exactly
this in its own comment ("From Lua, a stale duplicate is invisible: a `pairs`
scan filtering on the updated value simply skips it, so the script-level test
passes either way") and does the storage-level check instead. The integration
test is therefore pure false coverage beside a real test.

Fix: count every entry whose key is `print`, regardless of value (that *is*
observable from Lua: a duplicate key shows up twice in `pairs`), or delete the
integration test and keep the unit test. `rust_functions_work_as_inline_and_promoted_table_keys`
in the same file has the same blindness (it reads `t[print]`, which finds
whichever entry the lookup hits first).

Enforceable: no general rule; a test is the fix.

### 1.2 `gc_upvalues.rs::unreferenced_closure_is_collected` passes for the wrong reason

It asserts `heap_size()` after `gc_collect()` is smaller than before. The
chunk's own closure becomes garbage when the chunk returns, so the first
collection frees at least that one object even if `big_table` and `fn` leaked.
The assertion cannot detect the leak it is named for.

Fix: measure against a baseline taken after a collection that already
reclaimed the chunk closure (the pattern `anchor.rs::anchor_fresh_table`
uses), and assert the exact delta (one table plus one closure), or anchor the
table weakly through a `%p`/identity probe. The same "strictly less than"
shape in `anchor.rs` (`dropped_anchor_is_collected_at_the_next_collection`,
`anchor_dropped_on_another_thread_is_released`,
`release_then_gc_collects_anchored_object`) is sound today only because
`anchor_fresh_table` pre-collects; asserting `before - 1` would make that
explicit instead of incidental.

Enforceable: no.

### 1.3 `gc_upvalues.rs::temporary_callable_table_survives_gc_during_call_metamethod_lookup` probably never collects

`force_next_gc()` sets a manual threshold of 1, so the *next allocation*
collects. After it, the script executes `return callable`, the `__call`
resolution, and the handler `return self.value`. None of those allocates
(metamethod names are interned at construction, the handler closure and its
runtime already exist, frames are pooled), and `maybe_collect` only runs from
`State::allocator()`. If that reading is right, no collection happens while
the callable is a temporary, and the test is vacuous. Confirm by reverting
whatever root it was written for; then make the handler allocate before it
reads `self` (`local _ = {}`), which is what
`executing_returned_closure_roots_closed_upvalues_during_auto_gc` does with
`trigger_gc = {}`.

Enforceable: yes, structurally, with 4.2 below (a collection counter lets the
test assert that a collection happened inside the window).

### 1.4 `anchor.rs::dropped_anchor_slot_is_recycled_without_collecting` does not observe recycling

The test's comment says each drop is reclaimed by the next anchor creation;
the assertion is `anchor_count() == 0`. `anchor_count` counts a drop at once
(see `anchor_count_tracks_inserts_releases_and_drops`: "A drop counts at
once, before the State reclaims the slot"), so the assertion holds whether or
not slots are ever recycled. Recycling is actually tested by the unit test
`vm/anchor.rs::insert_recycles_dropped_slots`, which inspects
`registry.slots.len()`. The integration test is a duplicate that reads as
coverage of a property it cannot see.

Fix: delete it, or move the assertion it wants into the unit test.

Enforceable: no.

### 1.5 `anchor.rs::determinism_two_states_same_sequence_same_output` cannot fail on determinism

Both runs are the same code in the same process, and the compared output is
three arithmetic results (`6,105,9`) that no anchor-slot behaviour can change.
If anchors broke, a dozen other tests fail first; if slot reuse became
nondeterministic, this test still passes. It is named for a property it does
not measure.

Fix: delete, or compare something slot-dependent (the `Debug` of the
anchors after reuse, minus the process-global `state_id`, which the
`Anchor` docs already call nondeterministic), and pin the literal expected
value rather than comparing two runs of the same code.

Enforceable: no.

### 1.6 `src/vm/tests.rs::the_cap_does_not_charge_cost` tests a path that structurally cannot charge

`push_nil` goes through `check_stack_slot(&self)`, which takes `&self` and has
no access to the cost meter. The assertion `cost_used() == before` cannot
fail. The claim it is meant to protect (AGENTS.md: "enforcing it charges no
cost") matters on paths that *can* charge: frame admission from `OP_CALL`,
open-results windows, library preflights inside dispatch, where a refused
check sits next to batched opcode cost.

Fix: drive the cap from Lua (a call whose admission is refused) and compare
`cost_used` to the same script cut off just before the call; or delete the
test and rely on the `&self` signature, which is the real enforcement and is
worth a comment saying so.

Enforceable: yes; the `&self` receiver on every stack check is already a
type-level guarantee for the host push family. Keeping all `check_stack_*`
functions `&self` is checkable by review only, unless they move into a type
that holds no `CostState`.

### 1.7 `src/vm/tests.rs::gc_host_controlled` opens with a tautology

`assert!(state.heap_size() >= initial_objects + initial_strings)`:
`heap_size()` is `GcHeap::allocation_count()`, which is defined as
`objects.len().saturating_add(strings.len())`. Both sides come from the same
two fields. The rest of the test is real.

Fix: delete the line, or assert `==` and call it what it is (a definition
check).

Enforceable: no.

### 1.8 `src/vm/tests.rs::gc_threshold_control` has a dead second half

After `gc_set_threshold(1)` and a script that allocates, the test ends in two
comments ("Now GC should be needed ... threshold may have been adjusted") and
no assertion. A manual threshold is preserved across collections
(`GcThreshold::manual`, and `gc_policy.rs` checks it), so the comment's
hedge is also stale: the outcome is knowable and could be asserted.

Fix: assert `gc_threshold() == 1` after the run, or cut the half.

Enforceable: no.

### 1.9 `gc_policy.rs` never exercises `minimum_threshold`

Both policy tests compute `survivors * growth_factor` and compare it with
`.max(minimum_threshold)`. A `State::with_gc_policy` State has run
`open_libs`, so `survivors` is in the hundreds and the product dwarfs the
minimum (7 and 11). If production ignored `minimum_threshold` entirely, both
tests still pass. The minimum is part of the documented contract
(docs/limits.md "Garbage collection policy") and of replay timing.

Fix: add a case on `State::raw()` (or with a minimum larger than
`survivors * growth`) where the minimum wins. Also,
`custom_gc_policy_is_used_before_and_after_state_construction` names "before
construction" (docs/limits.md: "The initial threshold is in force while
`open_libs` initializes the standard library") but only reads the threshold
after construction; whether `open_libs` ran under it is not observable
without 4.2.

Enforceable: no.

### 1.10 Path-parity tests do not prove the two paths differ

`table_metamethods.rs::metamethod_path_charges_like_the_raw_path` compares a
`__tostring` table ("keeps the fast path") with an `__index` table (forces the
metamethod path). `table_sort_metamethods.rs::proxy_sort_charges_like_raw_sort`
and `table_with_unrelated_metatable_keeps_raw_sort` do the same for sort.
Which path a table takes is not observable from the public API. If path
selection changed so that any metatable routes through the metamethod path
(or none does), both scripts take the same path and the parity assertion
passes trivially. The tests name a configuration they cannot confirm they run
under.

Fix: make each path evidence itself. For the metamethod variant, put a hole
in the sequence read by the operation so the `__index` handler runs and
counts; for the fast variant, give the same hole and assert no handler ran.
Or move the parity tests to a unit test that reads a `cfg(test)` path
counter, the way `pattern_compilations` instruments the pattern cache.

Enforceable: no general rule.

### 1.11 `global_write_cache.rs` names cache behaviour it cannot observe

`warm_writer_inserts_populates_then_hits_and_closures_share_the_site`,
`append_and_g_proxy_writes_do_not_displace_a_warmed_global`,
`builtin_rebind_does_not_disturb_a_warmed_writer` assert only final global
values. With the global write cache disabled, every one passes. They are
correctness-under-caching tests, which is worth having, but the names promise
"populates", "hits" and "shares the site", none of which is checked. The unit
tests in `src/vm/global_env.rs` and `src/vm.rs::runtime_tests` show the
in-crate way to inspect cache cells (`read_cached`, `cache_cells`).

Fix: rename to what they check, and add the hit/share assertions as unit
tests that can read `RuntimeCaches`.

Enforceable: no.

### 1.12 `src/vm/tests.rs::restricted_env_counts_a_repeated_whitelist_name_once` is vacuous without debug assertions

The test calls `gc_collect()` and asserts nothing; its comment says
"Collection checks the tombstone count in debug builds". Under `brokkr test
... --release` (which AGENTS.md documents for other suites) it passes
unconditionally. `src/vm/object.rs` and `src/vm/table.rs` gate their
debug-assert-only tests with `#[cfg(debug_assertions)]`; this one does not.
Today the gate only runs lib unit tests in dev, so the gap is in focused
release runs, not in `brokkr check`.

Fix: `#[cfg(debug_assertions)]` on the test, or (better) assert the tombstone
count directly through the `cfg(test)` `GlobalEnv::len`.

Enforceable: partly. A test-conventions check could require that a test whose
body contains no `assert`/`expect_err`/`should_panic` carries
`cfg(debug_assertions)`; crude but cheap.

### 1.13 `vm_test07`, `vm_test10` assert only absence

Both hand-built bytecode tests assert `global_env.get("a").is_none()`. If
`set_global` were broken or the chunk returned early, they pass. They have no
positive control (a second global written on the other branch, or after the
loop). Low value, cheap to fix. The `vm_test01`..`vm_test11` names say nothing
about what each covers.

Enforceable: no.

### 1.14 `table_resolution.rs::budget_exhaustion_mid_move_keeps_the_paid_prefix` is looser than its doc

The doc comment says the move "copies exactly the elements it paid for, one
unit each". The assertions are `0 < copied < 100` and `copied <= used`. A
regression that copies one element per two units, or charges before copying
the last paid element, passes.

Fix: compute the exact expected prefix from the budget and the known cost of
the call itself (the same cost model `cost_golden` pins) and assert equality.

Enforceable: no.

### 1.15 The window model borrows production's key classification

`src/vm/table.rs::Model::array_key` calls the production `array_index`, so the
model and the table agree on which keys are array keys by construction. The
`key_rule_tests::numeric_key_boundaries` test pins `array_index` separately,
so this is not a hole today, only a coupling worth knowing: if
`numeric_key_boundaries` is ever relaxed, the model test silently stops
covering key classification.

Enforceable: no.

### 1.16 Duplicated guard: `frame.rs::reentry_accounting_sources_cover_the_intended_dispatch_opcodes`

This unit test checks that a list of Lua snippets compiles to the opcodes
`tests/reentrant_opcode_accounting.rs` means to exercise. The snippets are a
hand copy, and they have already drifted: the integration cases prefix every
source with `local prior = left + right\n`, use `indexed:method()` where the
guard uses `receiver:method()`, and name the length case `LENGTH` where the
guard says `LEN`. The guard checks its own copy, so a change to the real
cases that stops emitting the opcode is not caught.

Fix: put the guard in `reentrant_opcode_accounting.rs` and run it over
`CASES` themselves, using the public `Program::disassemble` (or a public
opcode iterator) instead of `crate::compiler::compile`. One list, one check.

Enforceable: yes, by construction once there is one list.

---

## 2. Guards and claims that have stopped holding

### 2.1 "Proxy" vocabulary for `_G` is false since 41ad774

`_G` is now the real global table (commit 41ad774; AGENTS.md, `global_env.rs`).
Still saying "proxy":

- `tests/global_env_proxy.rs` (file name; its module doc is already updated);
- `src/vm/tests.rs::open_libs_with_values_on_the_stack_targets_its_own_g_table`:
  assertion message `"_G keeps its proxy metatable"`;
- `tests/global_write_cache.rs::append_and_g_proxy_writes_do_not_displace_a_warmed_global`
  and the `proxy_marker` global in `builtin_rebind_does_not_disturb_a_warmed_writer`;
- `tests/plain_field_miss.rs::global_proxy_does_not_expose_table_library_names`;
- `src/snapshot_format.rs` header comment ("the `_G` proxy's environment
  delta") - outside scope, same stale claim, in a file that documents a
  replay-visible format.

False today. Enforceable: a textlint rule banning `proxy` near `_G` in
`src/` and `tests/` is cheap and would have caught all of these.

### 2.2 Tracker ids nobody can resolve

`tests/host_table_api_atomicity.rs` cites `VM-006`, `VM-007`, `VM-025` in its
module doc; `src/vm/table_ops.rs` and `tests/stdlib_divergences.rs` cite
others. No file in the repository defines them (not `notes/`, `docs/`,
`reference/` or `CHANGELOG.md`). They are references into a document that no
longer exists, in durable code, which the docs layout forbids even when the
target does exist in `notes/`.

Fix: replace each id with the property it names (the module doc already
states the properties; the ids can simply go).

Enforceable: yes, a textlint rule on `\bVM-\d{3}\b` in `src/**` and
`tests/**`.

### 2.3 The GC root set and the in-flight inventory are hand lists

AGENTS.md: "`vm::mark_gc_roots` is the *single source of truth* for what's
reachable." `mark_gc_roots` reads State fields one by one; nothing makes a
new State field holding `Val`s show up there. The same holds for the
`in_flight!` macro ("New fields must be classified here"), which feeds
`debug_assert_at_rest`, `validate_quiescent` and `reset_in_flight`. A field
added and forgotten is caught only if some test collects or saves at the
right moment.

`src/vm/caches.rs::drop_dead_entries` shows this repository already knows the
fix: it destructures every struct without `..` "so a new cache family, cell
or handle field fails to compile until its verdict is decided".

Fix: write `mark_gc_roots` (and one classification function feeding the
in-flight macro) as an exhaustive `let State { stack, global_env, ...,
rng: _, user_data: _ } = state;`, each ignored field with a one-line reason.
Then both claims hold by compilation rather than by care.

Checkable today, not false today as far as I can see. Enforceable: yes,
structurally, by the destructuring.

### 2.4 The test-side cache enumerations are not exhaustive

`RuntimeCaches::named_handles` (`#[cfg(test)]`, `caches.rs`) iterates
`self.global_lookup`, `self.field_lookup`, ... by field access, and
`runtime_tests::cache_cells` plus the family list in
`collection_clears_cache_entries_naming_freed_objects` repeat that list. The
production sweep destructures exhaustively; the test oracle does not. A new
cache family would be cleared by production (compile-forced) but not reported
by either test enumeration, so the "no entry names a freed value" check would
silently not cover it.

Fix: destructure in `named_handles` the same way, and derive the test's
families from it.

Enforceable: yes, by the same destructuring.

### 2.5 The `drain_mark_worklist` assertion has no test

AGENTS.md: "A `debug_assert!` in `drain_mark_worklist` enforces this" (root
through `GcHeap::mark`, never a bare worklist push). `object.rs` has
`should_panic` tests for "GC marked a vacant object key" and "... string
key", but none for "GC worklist entry was not marked reachable". If the
assertion were weakened or deleted, nothing would notice, and the doc would
still say it is enforced.

Fix: a `#[cfg(debug_assertions)] #[should_panic(expected = "pushed directly")]`
test that pushes a live allocation onto the worklist without `mark`.

Checkable, true today. Enforceable: yes, by that test.

### 2.6 docs/limits.md restates the GC defaults with nothing tying them to code

"The default automatic policy is an initial threshold of 20 allocations, a
minimum threshold of 20, and a growth factor of 2." These are
`limits/tuning.rs` constants. The only test touching them is
`gc_collect_preserves_disabled_auto_gc`, which asserts `>= 20`. The same doc
also says collection timing affects `%p` ids and snapshot contents, so a
silent change here is replay-visible. The repository already has the
mechanism for this: `frame.rs::documented_opcode_charges_match_static_units`
reads `docs/budget.md` with `include_str!` and checks every row.

Fix: the same shape of test over the "Garbage collection policy" paragraph,
or reword the doc to name the constants instead of their values.

True today. Enforceable: yes, by a docs-parity test.

### 2.7 "Every RustFn call path shares that check" covers four paths

`rust_fn_result_count.rs` module doc. Covered: direct host call, Lua call,
generic-for iterator, nested host call. Not covered: a RustFn as a
metamethod handler (`__index`, `__newindex`, `__call`, `__len`,
`__tostring`), as a `table.sort` comparator or `gsub` replacement, through
`call_anchor`. `reference/native-calls.toml` is the inventory of exactly
those paths.

Fix: drive the over-reporting function through every inventoried entry that
can reach a RustFn, the same way `tests/native_stack.rs` walks the inventory.

Enforceable: yes, if the scenario list is generated from (or checked
against) `reference/native-calls.toml`, as `scripts/native_calls.py` already
does for `native_stack.rs`.

### 2.8 `test_conventions.rs` fails open (lateral, outside scope)

The file-mutation guard is a substring search for `fs::write(`,
`fs::remove_dir(` and so on. It misses `fs::remove_dir_all(` (the marker
requires `(` right after `remove_dir`), `fs::rename(`, `fs::copy(`,
`File::options(`, `fs::hard_link(`, and any imported-name call
(`use std::fs::write; write(...)`). It scans `tests/` only, not
`#[cfg(test)]` code in `src/`. Its scratch-helper check is
`scratch_source.contains("CARGO_TARGET_TMPDIR")`, which a comment satisfies.
`tests/common/scratch.rs::write_boundary_transcript` writes to
`.scratch/error-boundary/` with `fs::write`, which is allowed only because
the guard exempts the whole file.

Fix: a clippy `disallowed-methods` entry for the `std::fs` mutators with a
scoped allow in `scratch.rs`; clippy resolves paths, so imports and aliases
cannot dodge it, and it covers `src/` test modules too.

Enforceable: yes, by clippy.

---

## 3. The test suite as a system

### 3.1 Seventy-eight integration test binaries

`tests/` has 78 top-level `.rs` files, each its own crate, each linking
dellingr. `brokkr check` builds them twice (default and snapshot sweeps), and
clippy checks them twice. Seven (`open_libs_registry.rs`, `save_state*.rs`,
`save_golden.rs`, `ipairs_save_state.rs`) are `#![cfg(feature = "snapshot")]`
and so compile and link to empty binaries in the default sweep. A focused
`brokkr test NAME` starts every one of those binaries with a filter. I did not
measure link time; at a typical few hundred milliseconds per dev link of a
crate this size, the layout costs tens of seconds per sweep before any test
runs, paid on every check.

The twenty files in this scope are all plain public-API tests with no
process-wide state, no subprocesses and no special stack needs (the one
native-depth test already runs on its own thread via `common/stack.rs`).
They can share one binary today.

Fix: `autotests = false`, one `tests/runtime/main.rs` with these files as
modules, and a small number of other grouped crates; keep separate only what
needs its own process (`panic_poison` for its panic hook, `native_stack`
which the release sweeps select by name, the CLI and corpus suites that spawn
processes). See 3.2 for the one hazard a merge introduces.

Enforceable: yes; a test in `test_conventions.rs` can require every
`tests/*.rs` to be listed under a `[[test]]` target (with discovery off, an
unlisted file is never compiled, which is otherwise invisible; see 3.4).

### 3.2 A racing, process-wide panic hook in the lib unit-test binary (lateral)

`src/crmath/tests/golden.rs::check_inputs` does `take_hook`, `set_hook(no-op)`,
work, `set_hook(previous)`. It runs in the same binary as every test in this
scope's `src/vm/` modules, concurrently with them. Two consequences:

- while it runs, any other test's panic message (a failing `assert_eq!` in
  `src/vm/tests.rs`, say) is swallowed, so a failure reports with no message;
- two concurrent `check_inputs` calls race: A takes the default and installs
  the no-op, B takes A's no-op and installs another no-op, A restores the
  default, B restores the no-op. From then on every panic in the binary is
  silent for the rest of the run.

`tests/panic_poison.rs` shows the right shape: install once (`Once`), filter
on the payload, forward everything else.

Fix: one hook installed once that suppresses only payloads from the oracle
(or a thread-local "quiet" flag the hook consults), or have the oracle return
`Result` instead of panicking.

Enforceable: yes, a clippy `disallowed-methods` entry for
`std::panic::set_hook`/`take_hook` with a scoped allow in the one sanctioned
installer.

### 3.3 Unit tests under a feature no gate enables

`src/vm/fault_injection.rs` (two tests) and the `registers.rs` test module
(`cfg(all(test, feature = "fault-injection"))`) are compiled only with the
`fault-injection` feature, which no `[[check]]` shape enables. They are never
compiled, linted or run by `brokkr check`. Neither is the rest of the
fault-injection code in `src/vm/` (including
`debug_assert_fault_injection_disarmed` on the snapshot path), so a refactor
can break it silently until someone runs the on-demand sweep.
`hotpath`/`hotpath-alloc` are in the same position (AGENTS.md acknowledges
it: "the plain gate does not compile that feature").

Fix: a `[[check]]` shape with `features = ["fault-injection", "snapshot"]`
restricted to a cheap selection (the lib unit tests plus `error_boundary`,
whose expensive sweep is `#[ignore]`), in the way the release native-stack
shapes restrict to `native_stack`. Whether brokkr's `tests = [...]` can name
the lib target I could not tell from `brokkr.toml`. A clippy-only pass under
`--features hotpath` would cover the annotation bitrot for the price of one
check.

Enforceable: yes, by the gate.

### 3.4 Discovery and "never compiled" files

Integration test discovery is on today, so every `tests/*.rs` compiles. But
`tests/common/*.rs` are pulled in with `#[path = "common/stack.rs"] mod
stack;` per file, and `stack.rs` carries `#[allow(dead_code)]` on every
function so that files using one helper compile. A helper nobody uses any
more stays, compiled and lint-exempt. If 3.1 is done, the per-file `#[path]`
includes and the blanket `allow(dead_code)` can go, and dead helpers become
lint errors again.

Enforceable: yes, by the lint, once the includes stop being per-crate.

### 3.5 Every cap test fills the real stack

`MAX_STACK_SIZE` is a constant with no per-State override, so every test of
behaviour "at the cap" (`fill_to_one_below_cap` and its users in
`src/vm/tests.rs`; `host_table_api_atomicity.rs`'s
`get_table_preserves_the_key_*`, `raw_reads_replace_the_key_even_at_the_stack_cap`,
looped four times in one test) materialises a million-slot stack per State.
The same holds for `MAX_CALL_DEPTH` (`get_table_preserves_the_key_at_the_lua_call_depth_limit`
recurses to the real limit), `MAX_NATIVE_CALL_DEPTH`,
`MAX_METAMETHOD_DEPTH` and `TABLE_SORT_MAX_LEN`. Individually milliseconds;
together, and run twice per check, a recurring cost plus a memory peak when
libtest runs a dozen of them in parallel. See 4.1 for the structural fix.

Enforceable: n/a (cost, not correctness).

### 3.6 Snapshot-only coverage of a default-build path

The table growth-limit tests (`sequence_copy_refuses_an_unallocatable_snapshot_gap`,
`snapshot_gap_growth_refuses_append_and_both_window_extensions`,
`snapshot_length_limit_refuses_append_insert_and_migration`,
`host_insert_growth_failure_preserves_operands`,
`snapshot_growth_failure_reaches_the_callback_as_an_ordinary_error`) reach
`MAX_ARRAY_LEN` through `Table::rebuild_from_snapshot`, which exists only
under the `snapshot` feature. The growth-failure path itself is not
snapshot-specific (`MAX_ARRAY_LEN` and `TableError::GrowthFailed` are in the
default build), so in the default sweep, the shipped consumer build, it has
no test.

Fix: a `#[cfg(test)]` constructor that sets a table's logical length and
window directly, used by both sweeps; keep one snapshot-specific test for the
load path.

Enforceable: no general rule.

---

## 4. What the tests reach that they should have been handed

### 4.1 Limits with no injection point

All runtime limits are crate constants (`limits::MAX_STACK_SIZE`,
`MAX_CALL_DEPTH`, `MAX_NATIVE_CALL_DEPTH`, `MAX_METAMETHOD_DEPTH`,
`TABLE_SORT_MAX_LEN`, `MAX_ARRAY_LEN`). Every test of a boundary contract
(operands preserved, state reusable, error kind) must drive the real value,
which is how 3.5 and 3.6 arise, and the snapshot backdoor in 3.6 is the
workaround.

Fix: a `Limits` value held by `State`, defaulting to the constants, settable
only through a `#[doc(hidden)]` or test-feature constructor. Contract tests
use small limits; the native-stack envelope (`tests/native_stack.rs`) and
one test per limit keep the real values. The trade-off to state up front:
limits are part of replay behaviour, so if a host could ever set them they
would have to be saved and checked on load; keeping the setter test-only
avoids that.

Enforceable: yes, by type (production code reads `self.limits`, and a clippy
`disallowed` path or a textlint on direct `limits::MAX_*` reads in `src/vm/`
keeps it that way).

### 4.2 No way to observe that a collection happened

Several tests exist to prove "a collection at point X does not free Y"
(`gc_upvalues.rs`, `table_metamethods.rs::sequence_values_stay_rooted_across_allocating_handlers`,
`table_sort_metamethods.rs::proxy_sort_roots_scratch_values_*`,
`src/vm/tests.rs::auto_gc_marks_a_deep_lua_table_chain_iteratively`). They
arrange collections with `gc_set_threshold(1)` or a `force_gc` RustFn, but
nothing lets them assert that a collection ran inside the window that
matters. 1.3 is the result: a test whose window contains no allocation. The
deep-chain test likewise relies on automatic collection happening at some
depth large enough to matter, without checking.

Fix: a collection counter on `GcHeap` exposed as `State::gc_cycles()` (cheap,
not replay-visible, useful to embedders as a diagnostic too). Then "a
collection happened between A and B" is one assertion.

Enforceable: yes, as a test pattern once the counter exists.

### 4.3 Test thread stack size comes from the environment in the lib binary

`tests/common/stack.rs` pins integration-test stacks explicitly "so
`RUST_MIN_STACK` cannot loosen it". The lib unit tests that guard against
native recursion do not: `src/vm/tests.rs::auto_gc_marks_a_deep_lua_table_chain_iteratively`
and `object.rs::deep_table_chain_marks_iteratively_and_reuses_the_empty_worklist`
run on libtest's default thread, whose size `RUST_MIN_STACK` sets. With a
large `RUST_MIN_STACK` in the developer's environment, a recursive marker
could pass both. The parser has its own unit-test stack constant (per
`stack.rs`'s comment), so the lib binary has the pattern available.

Fix: run both on an explicitly sized thread (the lib-side equivalent of
`on_limit_test_stack`).

Enforceable: partly; a test-conventions check can require that tests named
`*iterative*`/`*deep*` use the helper. Crude.

### 4.4 Test-only switches reachable from production builds

`State::arm_fault_injection` and `finish_fault_injection` are `pub` and
`#[doc(hidden)]` under the `fault-injection` feature. AGENTS.md says the
feature is "never a production ... configuration"; nothing enforces it. Any
crate in a host's dependency graph that enables the feature turns refusal
bookkeeping on in every stack and depth preflight of the shipped VM.

Fix: `#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!(...)`, if the documented sweeps are all dev builds (they are,
per AGENTS.md); or rename the feature to something that announces itself
(`__test-fault-injection`).

Enforceable: yes, by `compile_error!`.

### 4.5 `#[cfg(test)]` fields change the State under test

`State::pattern_compilations` is a `#[cfg(test)]` field, incremented inside
production `cached_pattern_at`. The lib unit tests therefore run against a
different `State` layout than the shipped one. Harmless for correctness
tests; worth knowing for anything layout- or size-sensitive (the native-stack
probes are integration tests, so they see the real layout). Same for the
`#[cfg(test)]` accessors on `GcHeap`, `UpvaluePool`, `GlobalEnv`, `Val`, all
harmless.

Enforceable: n/a.

### 4.6 Process-wide state in production code (noted for the merge in 3.1)

`vm/anchor.rs::NEXT_STATE_ID` (documented, not replay-visible) and
`lua_std/string.rs::GMATCH_WRAPPER` (a process-wide `OnceLock` bytecode
shared by every State, first caller compiles it). Neither breaks tests if
binaries are merged. I found no environment-variable writes, global loggers
or other singletons in the runtime tests.

---

## 5. Where the tests assume they run

- 4.3 (stack size from `RUST_MIN_STACK`) is the only host assumption I found
  in this scope's tests. On a host with a large `RUST_MIN_STACK`, the two
  iterative-marking tests silently stop guarding anything.
- `common/stack.rs` distinguishes the repository's two profiles by
  `cfg!(debug_assertions)` and says so; a custom profile with
  `debug-assertions = true` and optimisation gets the 4 MiB dev envelope
  while running optimised code. Stated, not detected.
- No in-scope test writes files, spawns processes, reads the clock, sleeps,
  or reads the environment.

---

## Lateral findings

- **Function-pointer identity is profile-dependent.** `Val::RustFn` equality
  and table-key hashing use `std::ptr::fn_addr_eq` (`lua_val.rs`), and
  `save_state.rs` and `vm.rs` compare `as usize` addresses. The standard
  library documents fn-pointer comparison as unreliable: distinct functions
  with identical bodies may be merged (AGENTS.md already notes that release
  builds fold identical functions and change saved function ids), and one
  function may have several addresses. So `f == g` for two host RustFns with
  identical bodies, and whether they collide as table keys, can differ
  between dev and release builds of the same host: a script-visible,
  build-dependent result. `lua_val.rs::different_rust_functions_compare_unequal`
  uses two functions with different bodies, so it cannot see this. Not
  in the replay path if replays always use one binary, but worth a line in
  docs/determinism.md, and a test with two identical-bodied functions run
  under `--release` would show the current behaviour.
- **`gc_upvalues.rs` holds compiler tests.** Its module doc says it tests GC
  of closed upvalues; it also holds codegen tests for where upvalues close
  (`block_upvalue_closes_before_slot_reuse`, `numeric_for_closes_*`,
  `break_closes_*`), tombstone rooting and literal reclamation. Grouping by
  subject would make a focused run of either subject find all of it.
- **`fcomm2`/`fleet:group()` mentions** in `src/vm/tests.rs` and
  `gc_upvalues.rs` describe a host this repository cannot see (AGENTS.md
  notes the origin). The tests are fine; the comments are claims nobody can
  check.
- **Boolean-collapsed script results.** Many tests return `cond and 1 or 0`
  or a single boolean from a long chain of checks, so a failure says only
  `0 != 1`. The `error("what broke")` pattern some tests already use
  (`global_env_proxy.rs`, `table_metamethods.rs`) reports which check failed
  at no extra cost.
- **`frame.rs::documented_opcode_charges_match_static_units`** ends with
  `documented_rows == classes.len() + 1` with no comment on what the extra
  row is. If that row is removed or a second unexplained one added, the
  failure will be hard to read.
