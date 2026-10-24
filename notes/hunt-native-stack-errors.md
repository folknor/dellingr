# Verification hunt: native-stack-errors

Scope: `tests/native_stack.rs`, `reference/native-calls.md`,
`reference/native-calls.toml`, `scripts/native_calls.py` (as the guard the
scope rides on), `tests/error_boundary.rs`, `src/vm/fault_injection.rs`, the
depth / frame / isolation tests (`vm_depth_regressions.rs`,
`frame_reservation.rs`, `panic_poison.rs`, `host_frame_isolation.rs`,
`caller_stack_isolation.rs`, `host_call_preflight.rs`, `vm_fault_context.rs`,
`vm_misc_limits.rs`, `index_depth_routes.rs`, `newindex_depth_routes.rs`) and
the error suites (`error_handling.rs`, `error_construction.rs`,
`host_error_reporting.rs`, `diagnostic_texts.rs`, `hygiene_errors.rs`,
`metamethod_errors.rs`, `rustfn_error.rs`, `source_attribution.rs`), plus
`tests/common/stack.rs` and `tests/common/scratch.rs`, which they include.

Nothing was built or run; every finding below is from reading. Where a cost
is given it is an estimate from the code's shape, not a measurement, and says
so.

Findings are grouped by the five questions, not ranked. Each ends with
whether the fixed version can be held mechanically.

---

## 1. Tests that prove nothing

### 1.1 The budget sweep passes if budgets are never enforced

`tests/error_boundary.rs`, `error_boundary_budget_sweep`. For each case it
runs every budget from 0 to the case's baseline cost. `run()` asserts that an
error, if any, is `BudgetExceeded`, and that a success returns the right value.
Nothing asserts that any budget actually fails. If `set_cost_budget` became a
no-op, every run would succeed with the right value and the sweep would pass:
it would still exercise the success path, but the test names (and AGENTS.md
describes) a budget-refusal sweep.

Fix: assert at least that budget 0 fails, and better, that the outcome is
monotone: there is a threshold below which every budget fails with
`BudgetExceeded` and at or above which every budget succeeds (the signed
`remaining` rule in `cost_meter.rs` defines exactly where). Enforceable: yes,
the assertion is the enforcement.

### 1.2 `a_failed_lookup_returns_every_unit_it_took` never checks the lookups failed

`tests/index_depth_routes.rs`. `try_index` returns whether `get_table`
failed, but the probe expression
`(function() try_index(r) try_index(r) return field_site(two) end)()`
discards both results. If the lookup through `function_behind_table` (which
needs two units at depth `MAX - 1`) started succeeding, or failed before
taking any unit, the test would still pass. The sibling in
`newindex_depth_routes.rs` (`a_failed_store_returns_every_unit_it_took`) does
check: `if not (a and b) then error('the store succeeded') end`. The same
test then calls `call_near_cap` twice with `let _ =`, "whatever the outcome",
so that half proves only that nothing broke afterwards.

Fix: mirror the newindex check. Enforceable: only by the assertion itself.

### 1.3 Tests whose assertions any outcome satisfies

- `metamethod_errors.rs::newindex_function_handler`: `expect_ok` only. The
  handler doubles the value and the test reads nothing back (an unused
  `local log = {}` sits in it), so a store that bypassed `__newindex` and
  wrote raw would pass.
- `metamethod_errors.rs::set_field_cache_value_update_takes_fast_path`:
  names a fast path it cannot observe. It asserts only that 100 increments
  give 100.
- `host_error_reporting.rs::host_call_through_chain_into_failing_lua_function_notifies_once`
  discards the error (`let _error`), so any failure with one notification
  passes.
- `error_handling.rs` message tests with disjunctions almost any message
  meets: `error_msg_unexpected_token_includes_context`
  (`contains("expected") || contains("near")`), `error_msg_missing_end_keyword`
  (`<eof>` or `expected`), `error_msg_call_depth_shows_overflow`
  (`"call stack overflow"` or `"depth"`), `error_msg_budget_exceeded_shows_amounts`
  (`"budget"` and `"10"`). `diagnostic_texts.rs` already pins full texts for
  the same families; these are weaker duplicates that read as coverage.
- `metamethod_errors.rs::function_type_errors_name_functions` checks
  `error.to_string().contains("function")`, and `Display` includes the
  traceback, whose frames can say "in function". Check `error.kind()`'s text.
- `native_stack.rs::deepest_nested_functions_parse_on_the_parse_test_stack`
  accepts any `SyntaxError` for the first rejected depth; it should pin the
  depth variant, or a future source change that fails for another reason
  still passes.

Fix: pin the observed value or variant in each. Enforceable: no rule catches
a weak assertion; deleting the `error_msg_*` duplicates in favour of
`diagnostic_texts.rs` removes the class from this file.

### 1.4 "Deterministic" tests that compare a run with itself

Both sides come from the same process, binary and host, so they cannot catch
the cross-host or cross-build nondeterminism that replays care about:

- `error_handling.rs::table_sort_inconsistent_comparator_terminates_deterministically`
  (`assert_eq!(run_number(code), run_number(code))`);
- `error_handling.rs::table_move_cost_is_deterministic_across_fresh_states`
  (`assert_eq!(run(), run())`);
- the `math.random` half of `table_insert_move_and_random_validate_before_mutation`
  (two States, seed 99, compared to each other; the test name says nothing
  about the RNG).

They do catch per-State variation within a process (address-keyed hashing,
say), which is worth something, but the name promises more. Fix: pin the
golden value (`table_move_budget_partial_mutation_is_deterministic` already
does both). Enforceable: no.

### 1.5 The native-stack byte probe swallows its own check for the raising scenario

`native_stack.rs::native_bytes_per_level`: `finish()` raises
`error('uneven gaps')` before `report`, and the outer loop ignores any error
when `scenario.raises`. For `error_tostring`, `finish` runs inside the
`__tostring` handler, so an uneven gap produces no report row and no failure.
Fix: for raising scenarios, require the error to be the expected
`ScriptError` with rendering `r` (as `check` does), so the probe's own error
fails it. Enforceable: by that match.

### 1.6 The convention test checks a word, not the restriction

`tests/test_conventions.rs` requires that every file mutation in `tests/`
lives in `common/scratch.rs`, and that `scratch.rs` *contains* the strings
`CARGO_TARGET_TMPDIR` and `.starts_with(scratch_root)`. `scratch.rs` itself
has `write_boundary_transcript`, which writes to
`$CARGO_MANIFEST_DIR/.scratch/error-boundary/transcript.txt` with
`fs::create_dir_all` and `fs::write`, bypassing `write_scratch_file`'s root
check. The convention test cannot notice, because the bypass is in the one
file it exempts and the strings it greps for are still present. See 5.4 for
where the write should go.

Fix: require that the only mutating calls in `scratch.rs` are inside
`write_scratch_file` and `ScratchDirectory` (a function-level scan, or move
the mutation primitives behind one private function and grep for their count).
Enforceable: yes, in `test_conventions.rs`.

### 1.7 `error_boundary_transcript` is evidence nothing compares

`error_boundary.rs::error_boundary_transcript` is `#[ignore]`d, cfg'd to
debug and no features, and writes a transcript for "before/after comparison"
to a fixed path, which the next run overwrites. Its doc comment in
`scratch.rs` calls the location "spec-mandated"; no spec in the repository
mentions it (only AGENTS.md mentions `error_boundary`). The comparison is
manual and the A side is lost when the B side runs.

Fix: commit the transcript as a golden fixture (`tests/fixtures/`), assert
equality in the ordinary sweep, and give it a regenerate path like
`regenerate_save_golden`. The transcript is deterministic by construction
(costs, typed errors, rendered values), and it is replay-relevant: a change
to error text, cost or survivors would then be called out by a failing test
rather than by someone remembering to diff. Enforceable: yes, that is the
point of making it a golden.

---

## 2. Guards and claims that have stopped holding

### 2.1 FALSE TODAY: inventoried re-entering functions carry `#[hotpath::measure]`

AGENTS.md: "Don't add `#[hotpath::measure]` to a function that stays live
across a native re-entry (`call_unguarded`, ..., and the
metamethod/iterator/library helpers between the dispatch loop and their
`call_unguarded`)". The set of such functions is exactly the `[[function]]`
list in `reference/native-calls.toml`. Three of them carry the attribute:

- `State::instr_tfor_call` (`src/vm/eval_control.rs`), the generic-for
  iterator helper;
- `State::table_sort` (`src/vm/table_ops.rs`), live across every comparator
  call;
- `State::to_string_with_meta` (`src/vm/table_ops.rs`), live across
  `__tostring`.

The envelope is not affected in the plain build (the macro expands to
nothing), and instrumented builds already get 8 MiB in `tests/common/stack.rs`.
But the rule is false today, the timing guards in instrumented builds sit on
every iterator, sort-comparator and host-tostring native level, and the
reported times for those three functions include the nested Lua they call
(their percentages in a `--hotpath` run are inclusive of arbitrary script
work).

Fix: remove the three attributes, and have `scripts/native_calls.py` check
that no inventoried function is preceded by `#[hotpath::measure]`: it already
finds every `fn` item and its span, so this is a few lines. Enforceable: yes,
in the existing `native-call-inventory` script check.

### 2.2 FALSE TODAY: two copies of the parse-test stack size disagree under `hotpath-alloc`

`src/compiler/parser.rs` sets `DEEP_PARSE_TEST_STACK_BYTES` to 8 MiB under
`cfg(feature = "hotpath")` only. `tests/common/stack.rs` sets
`PARSE_TEST_STACK_BYTES` (and `LIMIT_TEST_STACK_BYTES`) to 8 MiB under
`hotpath` *or* `hotpath-alloc`. In `Cargo.toml`,
`hotpath-alloc = ["hotpath/hotpath", "hotpath/hotpath-alloc"]` enables the
dependency's features, not this crate's `hotpath` feature, so under
`--features hotpath-alloc` the measure guards are live (they key on the
dependency's feature) while `cfg(feature = "hotpath")` is false. The parser
unit tests then parse at the syntax budget on 2 MiB with timing guards on
every parser frame, which is what the 8 MiB branch exists to prevent.

The same gap, outside this scope: `tests/diff_test.rs` compiles its corpus
partitions out under `cfg(not(feature = "hotpath"))`, so under `hotpath-alloc`
the corpus is compiled in, contrary to AGENTS.md ("The corpus partitions are
compiled out under the `hotpath` feature").

Fix: make the local feature imply itself, `hotpath-alloc = ["hotpath",
"hotpath/hotpath-alloc"]`, so `cfg(feature = "hotpath")` means "instrumented"
everywhere and the `any(hotpath, hotpath-alloc)` spellings can collapse. Then
delete one of the two stack constants: the unit tests can use a constant the
crate exports under `cfg(test)` or a `#[doc(hidden)]` const both read.
Enforceable: the feature implication is structural (Cargo enforces it); one
constant instead of two removes the drift.

### 2.3 The scenario cross-check reads commented-out entries

`scripts/native_calls.py::scan_test_lists` locates the `SCENARIOS`,
`CHAINS` and `LEAVES` arrays on sanitized code but then extracts names from
the *raw* text slice (`body = text[m.end():end]`) with
`\bname:\s*"([\w-]+)"`. A scenario commented out with `//` in
`tests/native_stack.rs` still counts as present, so the inventory check stays
`ok` while the test no longer runs that path. It also cannot see a scenario
that is listed but skipped (a filter in the loop, `#[ignore]` on
`every_limit_at_once_fits_the_limit_test_stack`, a cfg on the test).

Fix, smallest: blank comments (but not strings) before extracting. Fix,
structural: move the cross-check into the test. `native_stack.rs` can
`include_str!("../reference/native-calls.toml")`, parse scenario names, and
assert they equal `SCENARIOS`' names at run time; a check that lives in the
running test cannot be satisfied by a test that does not run. Enforceable:
yes either way.

### 2.4 The inventory's scenario paths are claims nothing checks at run time

`reference/native-calls.toml` gives every scenario an ordered edge path, and
the script checks that the path is syntactically connected and that every
edge is covered by some path. Nothing checks that the Lua in a scenario
actually drives the edges its path names. Examples where the claim rests on
prose about the implementation:

- `sort_comparator_right` claims `table_sort_sift_down->table_sort_less:right`
  because "the heapify pass compares the left child, then the right" for
  `{3, 1, 2}`; `sort_comparator_sortdown` claims the sort-down edge because the
  third comparison is "the first of the sort-down pass". A change in heap
  order keeps both scenarios green while the right-child and sort-down edges
  go uncovered.
- `index_method` claims `run_frame->reg_get_field:self` rather than
  `:field`; nothing distinguishes them at run time.
- `tfor_ipairs` relies on "ipairs over a table with a metatable leaves the
  dispatch loop's fast path"; if the fast path learned to read through
  `__index`, the scenario would stop exercising `base_ipairs_iter`.

The witnesses in `native_stack.rs` prove that recursion happened through
*some* native path at each level, not which.

Fix: turn the marker into something a build can observe. A
`native_call!("edge-id")` macro that expands to nothing normally and, under a
test-only cfg (say `--cfg dellingr_edge_trace`, so frames in every shipped
configuration are unchanged, which is why markers are comments today),
records the id in a per-State log. `native_stack.rs` under that cfg asserts
each scenario's recorded edge set contains its toml path. The marker syntax
the script and textlint already parse stays one token per call site.
Enforceable: yes, and it would make the toml's coverage column true by
construction instead of by reading.

### 2.5 "A scenario's path is one native level" is false for two scenarios

`reference/native-calls.md` ("A scenario's path is one native level: it
starts at an edge out of `call_unguarded` and ends with an edge into it").
`index_rust` and `newindex_rust` take two levels per cycle (`native: 2` in
the test; their toml paths pass through `call_unguarded` in the middle via
`@rust_handler`). The measurement section further down says so. The script
does not cross-check a scenario's `native` count against the number of
`->call_unguarded` edges in its path.

Fix: reword, and have the script read `native:` from each `Scenario` (it
already parses those blocks) and compare it to the count of
`->call_unguarded` edges. Enforceable: yes, in the script.

### 2.6 The envelope's gmatch-compile peak is measured once, on one path

`docs/limits.md`, Envelope: "`string.gmatch` creating its iterator in the
last native level (the first call in a process also compiles the fixed
wrapper chunk there)". `GMATCH_WRAPPER` in `src/lua_std/string.rs` is a
process-global `OnceLock`. In `every_limit_at_once_fits_the_limit_test_stack`
the first gmatch call in the process is the `index_field / index / gmatch`
run, so the compile-at-depth peak is measured for exactly that scenario and
chain, not for every path as the test's doc and limits.md say each
deepest-point item is. `index_field` is one of the cheaper cycles
(`gsub_pattern_table` is roughly twice its bytes per cycle in the recorded
tables). The coverage also depends on test order: if any other test in the
binary (or, after a merge, in a shared binary) calls `string.gmatch` first,
the peak is silently never measured.

Fix: compile the wrapper eagerly when the string library opens (the cost is
one tiny compile per process, already paid on first use) and drop the
"first call compiles" clause from the envelope; or make the cache per State
so every gmatch leaf run pays it. Either removes a process-global from the
envelope argument. Enforceable: the eager init is structural; a unit test
can assert `GMATCH_WRAPPER` is set after `State::new()`.

### 2.7 limits.md places at-cap tests under `test_limited.sh`; they are not

`docs/limits.md`, Value stack: "Tests that push to `MAX_STACK_SIZE` pay that
per State, inside the fixed virtual-memory cap `scripts/test_limited.sh`
applies". Only `tests/run_examples.rs` runs things under `test_limited.sh`
(the example scripts). The Rust tests that push to the cap
(`frame_reservation.rs`, `index_depth_routes.rs`, `host_error_reporting.rs`,
`host_call_preflight.rs`, `vm_depth_regressions.rs`, `error_boundary.rs`) run
as ordinary `cargo test` processes, and nothing in `brokkr.toml` wraps them.
False today.

Fix: reword to say the cost is paid per State with no cap applied, or apply a
cap (an `RLIMIT_AS` set by a test-crate helper before the State is built,
which libc is already a dev-dependency for). Enforceable: only the second.

### 2.8 The fault-injection code is never compiled by the gate

AGENTS.md says "No standing `[[check]]` shape enables it", which is true and
is the problem: `src/vm/fault_injection.rs`, the six `cfg(feature =
"fault-injection")` arms in `eval.rs`, `stack.rs` and `metamethod.rs`, the
debug assertion in `vm.rs::validate_quiescent`, the unit tests in
`fault_injection.rs` and `src/vm/registers.rs`, and the *non-ignored*
`string_bytes.rs::gmatch_refused_push_preserves_the_next_match` are neither
clippy-checked nor compiled by `brokkr check`. A rename in any preflight
breaks the feature silently until someone runs the on-demand command. The
fault sweep is the only test of "every eligible refusal leaves the operands,
notifications and quiescence right", which is the error-cleanup contract
AGENTS.md calls load-bearing.

Fix: a `[[check]]` shape `fault-injection` with `features =
["fault-injection", "snapshot"]`, `tests = ["error_boundary",
"string_bytes"]` and `include_ignored` for it. The sweep is small (each CASES
entry has tens of eligible events, each run a few hundred instructions), so
its cost is one more dev build of the library plus two test binaries; not
measured. If that is too much per check, at minimum a clippy-only lane so the
code compiles. Enforceable: yes, in `brokkr.toml`.

### 2.9 The poisoning contract is a hand list

`panic_poison.rs` doc: "every later public entry point refuses with
`ErrorKind::StatePoisoned`". `assert_refuses_everything` lists about twenty
entry points by hand; the source enforces it with a `self.check_poisoned()?`
line at the top of each `pub fn` (over thirty sites across `vm.rs`,
`vm/stack.rs`, `vm_aux.rs`, `vm/anchor.rs`, `vm/table_ops.rs`). A new `pub fn`
that forgets the line passes everything.

Fix, script: the same lexical machinery as `native_calls.py` can list every
`pub fn` in an `impl State` returning `Result` and require that its body
starts with `check_poisoned()?` or delegates to `guarded` / `with_...`
helpers, with an allowlist for the infallible accessors that must stay safe
on a poisoned State (`is_poisoned`, `cost_used`, `gc_collect`,
`release_anchor`...). Fix, structural: route the host API through one checked
accessor (`fn live(&mut self) -> Result<&mut Live>`, where the methods live on
`Live`), which makes the unchecked spelling unrepresentable. Enforceable:
yes, either way; the second needs no script.

### 2.10 Fault-event sites are strings, and eligibility is computed twice

`inject_fault(site: &'static str, argument, eligible)` records
`(site, argument, fired)` and drops `eligible`. Consumers re-derive it:
`error_boundary_fault_sweep` filters with
`*site != "native_depth" || *argument > 0`, a copy of the rule written in
`eval.rs` (`self.native_call_depth > 0`); `string_bytes.rs` instead counts all
events and skips targets that fired nothing. Two writers of one rule, keyed on
a string. Renaming the site string makes the filter count ineligible events;
the sweep then panics on `position(...).unwrap()` rather than silently
passing, so it fails loudly, but only in a configuration no gate runs (2.8).

Fix: an `Event { site: FaultSite, argument: usize, eligible: bool, fired:
bool }` with `FaultSite` an enum, returned by `finish_fault_injection`.
Consumers filter on `eligible`. Enforceable: by the type.

### 2.11 Measurements in durable docs omit the variable that matters most

`reference/native-calls.md` and `docs/limits.md` record bytes per cycle and
frame sizes with commit and host, as the project rule asks, but not the rustc
version, and the repository pins no toolchain (no `rust-toolchain.toml`).
Frame sizes are a compiler property first. The envelope test's verdict
likewise depends on whatever rustc the host has: a toolchain bump can flip
`release-native-stack` red or green with no source change, and "this
repository's release build settings" is not a reproducible configuration
without it. Separately, `reference/native-calls.md` says "All 57 scenarios
were rerun"; the inventory has 55 scenarios (57 matches the probe's rows,
which include the two `chain_` rows). And the file has become a measurement
log (four dated sections, several superseded, rows for functions that no
longer exist) inside a `reference/` document that must be true; a reader
cannot tell which table is current.

Fix: pin the toolchain (`rust-toolchain.toml`) so the envelope is stated for a
named compiler, record `rustc -V` with every measurement, and move the
history to `reference/performance.md` (the folder table's stated home for
"measured numbers over time"), leaving native-calls.md with the guard and one
current table. Enforceable: the pin is structural; the rest is editorial.

### 2.12 Smaller claims

- `vm_fault_context.rs` defers to the unit test
  `internal_fault_keeps_innermost_instruction` by name; it exists today in
  `src/vm/tests.rs`, but a rename leaves the comment pointing at nothing.
  Not enforceable short of a grep script; low value.
- `error_handling.rs::table_sort_charges_before_mutating` cites "(L18)" and
  `rustfn_error.rs` cites "(L6)" twice: ids from an audit list that is not in
  the repository. A durable comment must carry its own context; drop the ids.
- `vm_misc_limits.rs` asserts `TooManyResults { limit: 255 }` and
  `error_boundary.rs`'s NATURAL corpus uses `unpack({}, 1, 255)`: literals
  that should be `MAX_RESULTS` / `MAX_ARGUMENTS`, so a limit change fails the
  test for the wrong reason or stops testing the boundary.
- `limits.md`, Envelope, "the deepest point any one of: a pattern match one
  call short of `MAXCCALLS` ..., or its rejection at `MAXCCALLS`". The accept
  leaf runs all three recursion shapes (optional, greedy through
  `max_expand`, minimal through `min_expand`); the `pattern_reject` leaf
  rejects only through the optional shape, whose per-level frame is the
  smallest of the three (no expansion helper frame). Add greedy and minimal
  rejection leaves, or say which shape the rejection peak is measured on.
- `limits.md`: "Parsing keeps the stricter promise: parsing at the syntax
  budget fits a 2 MiB thread in both settings." The worst parse shapes are
  driven by the parser's unit tests, which run only in the dev sweeps; in
  release the only parse-at-budget test is `native_stack.rs`'s single
  nested-`local function` shape. The release half of the promise is checked
  for one shape.

---

## 3. The test suite as a system

### 3.1 Process-global state the suite currently relies on not sharing

Each `tests/*.rs` file is its own binary today, so these are sound now and
stop being sound the day files are merged (which 3.4 argues for):

- `GMATCH_WRAPPER` (`src/lua_std/string.rs`): see 2.6. Coverage of an
  envelope peak depends on which test calls gmatch first in the process.
- `panic_poison.rs::quiet_deliberate_panics` installs a process-wide panic
  hook the first time any test calls it, permanently. It forwards everything
  but the exact deliberate message, so merging would mostly be safe, but the
  hook replaces whatever hook a later test or harness sets up and cannot be
  reset.
- `panic_poison.rs::AFTER_SWALLOW` is a `static AtomicUsize` read by one test.
  Safe only because exactly one test touches it and `brokkr test -N` repeats
  never reach the increment when the code is correct. It belongs in the
  State's `user_data` like the other harnesses in scope already do
  (`native_stack.rs`'s `Harness`, `host_error_reporting.rs`'s `Harness`).
- `NEXT_STATE_ID` (`src/vm/anchor.rs`) makes State creation order visible
  through `Anchor`'s `Debug`/`Hash`; no test in scope observes it, but a test
  that formats an anchor would become order-dependent in a merged binary.

Enforceable: the panic-hook and static-counter patterns can be banned in
`tests/` by `test_conventions.rs` (grep for `set_hook` and `static .*Atomic`
outside `common/`). The library globals need the structural fixes in 2.6.

### 3.2 `every_limit_at_once_fits_the_limit_test_stack` is one serial test of 990 runs

55 scenarios x (2 held chains x 8 leaves + 2 terminal chains) = 990 runs in
one `#[test]`, on one thread, in each of four sweeps. Consequences:

- The chunk is recompiled for every run (`check` calls `Program::compile`),
  and each chunk contains `MAX_CHUNK_NESTING - 1` nested functions plus every
  chain and leaf. It depends only on the scenario; 55 compiles would do, and
  with the step selected by a global like the chain and leaf, one.
- The first failing combination panics and hides the rest.
- A native stack overflow aborts the process, and the abort message names the
  thread, which is `every_limit_at_once` for all 990 runs: the one failure
  this test exists to catch is the one it cannot attribute to a scenario,
  chain or leaf.
- libtest cannot parallelise it.

Fix: one `#[test]` per scenario (a macro over `SCENARIOS`, or a build-time
generated list), each on a thread named after its scenario, iterating chains
and leaves inside; compile once per test. Cost not measured; the redundant
compiles are 935 of 990, and dev compile of a 199-deep chunk is not free.
Enforceable: by structure.

### 3.3 At-cap probes refill a million slots per attempt

`frame_reservation.rs::run_at_cap` and `index_depth_routes.rs::call_near_cap`
find the highest admitting stack height by starting at `MAX_STACK_SIZE - 2`
and lowering by one, and every attempt does `set_top(fill)` from 0 (one
million `Val::Nil` pushes, one at a time, in `State::set_top`) and then
`set_top(0)`. The number of attempts is the probe's frame size plus
`FRAME_HEADROOM`. `frame_reservation.rs` does this for 18 bodies plus three
more; `index_depth_routes.rs` calls `call_near_cap` about a dozen times. In
the dev profile that is on the order of hundreds of million-slot fills per
sweep, run in both dev sweeps; not measured, likely seconds.
`host_error_reporting.rs` (`execution_stack_preflights_notify_once`, four
cases, and `attempt_result_padding`, three modes) and
`host_call_preflight.rs` fill the same million through the public `push_nil`.

Fix: fill once and lower the height by popping one slot per failed attempt
(a failed host `call` leaves the filler as it was, which these tests already
assert), or compute the exact height from the probe's `max_stack`, which
`Program::disassemble` already prints. Lateral: `set_top`'s growth arm could
`resize` after its preflight instead of a push loop. Enforceable: no; it is a
fixture shape.

### 3.4 Eighty test binaries, and every focused run builds the release envelope

`tests/` holds about eighty top-level files, each its own integration-test
binary linking the library. Per sweep, every one is linked; brokkr's focused
runner (`src/test_cmd/plan.rs`) prebuilds every sweep's targets and lists
every harness before running the match. Because `brokkr.toml`'s two release
sweeps pin `profile = "release"` (fat LTO, one codegen unit), a focused
`brokkr test <anything>` after a source change also rebuilds the library and
`native_stack` twice in release, only to find nothing matched there unless
the name is in `native_stack.rs`. AGENTS.md confirms the focused runner runs
all four sweeps (`brokkr test native_bytes_per_level ... runs all four
configured sweeps`). Not measured; two fat-LTO builds dominate an
edit-test loop.

Fix: put the release envelope sweeps in a separate lane that `brokkr check`
runs and `brokkr test` does not by default (or have the focused runner skip
sweeps whose `tests = [...]` list cannot contain the name), and consolidate
the integration tests into a handful of binaries grouped by subject (error
reporting, limits, isolation, library), after the process-global fixes in
3.1 and 2.6. Enforceable: the lane split is `brokkr.toml` structure; the
consolidation can be held by a `test_conventions.rs` rule that `tests/` has
only the agreed top-level files.

### 3.5 `error_handling.rs` and `metamethod_errors.rs` are grab-bags

About half of `error_handling.rs` ("Tests for proper error handling (things
that used to panic)") is numeric `for`, `ipairs`, `table.insert` rotation,
`table.move`, `tonumber`, call-syntax sugar and constructor parsing; about
half of `metamethod_errors.rs` ("invalid metamethod handlers produce errors")
is inline-cache revalidation. A focused run by subject misses them, and
coverage of the cache lives under an error-handling name. Fix: move them to
the files that own their subject (there are `table_*`, `stdlib_*` and
`plain_field_miss.rs` neighbours already). Enforceable: no.

### 3.6 What is clean

None of the suites in scope sleeps, reads the clock, reads the environment,
spawns a process or invokes the build tool; the only binary path is
`env!("CARGO_BIN_EXE_dellingr")` in `tests/common/mod.rs`, guarded by
`test_conventions.rs`. Every limit-stack thread is joined. Harness state is
carried in the State's `user_data` rather than statics, except 3.1's
`AFTER_SWALLOW`.

---

## 4. What the tests reach that they should have been handed

### 4.1 Test-only API reachable by any host through feature unification

`fault-injection` is an ordinary Cargo feature, and `arm_fault_injection` /
`finish_fault_injection` are `pub` (`#[doc(hidden)]`). Any crate in an
embedder's graph that enables `dellingr/fault-injection` turns them on in the
game's VM, along with a branch on every stack and depth preflight. AGENTS.md
says it "is test infrastructure, never a production ... configuration", but
nothing stops it. Fix: gate it on a rustc cfg (`--cfg
dellingr_fault_injection`, set by a brokkr sweep's RUSTFLAGS) instead of a
feature, so dependency features cannot reach it; or keep the feature and
add a `compile_error!` unless `cfg(test)` or a second explicit opt-in cfg is
present. Enforceable: yes, structurally.

### 4.2 Production values with no injection point that tests must wait out

`MAX_STACK_SIZE` (one million values, about 16 MB), `MAX_NATIVE_CALL_DEPTH`,
`MAX_METAMETHOD_DEPTH` and `MAXCCALLS` are `pub const`, which is right for a
replay contract, but it means every boundary test pays the full size (3.3,
3.2). The answer here is fixture shape (fill once, compute heights), not a
test-only override: an overridable cap is a replay hazard and a test-only
path production can reach (4.1). Recorded so the trade-off is explicit.

### 4.3 The GMATCH wrapper is process state production logic reaches directly

See 2.6. `is_gmatch_wrapper` also answers from the process-global (`false`
until something compiled it), so error attribution through the shim is
correct only because the shim cannot run before it is compiled. Eager
initialisation removes both the ambient dependency and the ordering argument.

---

## 5. Where the tests assume they run, and what they touch

### 5.1 The envelope test runs on hosts the envelope does not describe

`docs/limits.md`: "How `tests/native_stack.rs` validates it, on x86-64
Linux". The test is not gated by `target_arch` or `target_os`. On another
host (aarch64, macOS, a 64 KiB-page kernel that rounds thread stacks up) it
either passes without the doc making any claim, or overflows and aborts the
whole test binary with a message that cannot name the run (3.2). The thread
sizes also include whatever static TLS the C library carves out of a thread's
allocation, which differs between the test binary and an embedder's game
thread. Fix: gate the assertion to the stated target and report "envelope not
stated for this target" elsewhere, or widen the statement after measuring.
Enforceable: by `cfg`.

### 5.2 `cfg(debug_assertions)` stands in for "the repository's dev profile"

`tests/common/stack.rs` chooses 4 MiB or 2 MiB by `cfg!(debug_assertions)`
and says so. A release profile with debug assertions on, or a dev profile
with `opt-level = 3`, picks the wrong size silently. Neither exists in this
repository today, so the guard holds; noted because it is keyed on a proxy
and reports nothing when the proxy stops matching. A `build.rs`-free way to
make it exact is to have the sweeps export the profile name to the tests via
`[[check]]` `env` and assert it agrees with `debug_assertions`.

### 5.3 The release envelope is validated for a profile no embedder ships

`[profile.release]` here is fat LTO with one codegen unit. Cargo profiles of
a dependency come from the embedding workspace, so the game builds dellingr
with its own profile, typically Cargo's default release (thin-local LTO, 16
codegen units), whose inlining and therefore frame sizes differ. limits.md
says other configurations are the embedder's to validate, which is honest,
but the repository's own release envelope then certifies an artifact nobody
links. Fix: run the release envelope sweep under Cargo's default release
settings too (a custom profile `release-default` inheriting from `release`
with the defaults restored), or state the envelope for that profile instead.
Enforceable: yes, as a `[[check]]` shape.

### 5.4 The boundary transcript writes into `diff_test.sh`'s directory

`write_boundary_transcript` writes `.scratch/error-boundary/transcript.txt`
under the manifest directory. `.scratch/` is the scratch area of
`scripts/diff_test.sh` (and `scripts/bench.sh`, which prunes
`.scratch/bench-results/`); it is ignored, so the write is invisible to
review, and nothing owns or cleans the subdirectory. The stated reason for
the exception ("must outlive the test process") does not hold:
`CARGO_TARGET_TMPDIR` also persists across runs until `cargo clean`. Best fix
is 1.7 (a committed golden, written only by an explicit regenerate example
into `tests/fixtures/`, as `regenerate_save_golden` does). Short of that,
`CARGO_TARGET_TMPDIR/error-boundary/` through `write_scratch_file`.
Enforceable: by 1.6's tightened convention test.

### 5.5 Nothing in scope writes into another tool's state

Apart from 5.4, every write in the scoped tests goes through
`ScratchDirectory` / `write_scratch_file` under `CARGO_TARGET_TMPDIR` with
pid- and counter-unique names, and nothing touches `.brokkr/`, `target/`
directly, or the home directory.

---

## Lateral

- `scripts/native_calls.py` has no tests of its own, unlike
  `notes_sections.py` (`test_notes_sections.py` runs as a script check). Its
  lexer (raw strings, char literals versus lifetimes, nested block comments,
  closure callbacks spelled exactly `|state|`) is what every marker and
  coverage claim rests on, and a lexer regression that hides a new call site
  passes silently, while one that hides an existing site fails loudly. A
  fixture-based `test_native_calls.py` in the same `script_check` pattern
  would hold it.
- `native_calls.py::scan_test_lists` re-sanitizes the whole test file once
  per list (three times); harmless, noted only because the script is on every
  check.
- `error_boundary.rs` sets up the `large` global only when the case source
  contains the substring `large`. It fails loudly when wrong, so not a guard
  problem, but it is setup keyed on a name.
- `host_call_preflight.rs` and `rustfn_error.rs` count notifications with
  `Arc<Mutex<usize>>` where an `AtomicUsize` (as `host_error_reporting.rs`'s
  `ErrorCounter` uses) would do; three copies of one counter callback across
  files could be one helper in `tests/common/`. Cosmetic.
