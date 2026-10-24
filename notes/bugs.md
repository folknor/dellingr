# Bugs

Defects found during the verification hunt over the whole tree (scopes:
check-machinery, corpus-harness, native-stack-errors, snapshot, compiler-cost,
runtime, library). Each entry names the scopes whose hunters reported it.

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

## BUG-001 - Three re-entering functions carry `#[hotpath::measure]` against the stated rule

Reported by: check-machinery, native-stack-errors.

AGENTS.md: "Don't add `#[hotpath::measure]` to a function that stays live
across a native re-entry (`call_unguarded`, ..., and the
metamethod/iterator/library helpers between the dispatch loop and their
`call_unguarded`)." The set of such functions is exactly the `[[function]]`
list in `reference/native-calls.toml`. Three of them, all in the textlint
alternation, carry the attribute:

- `State::instr_tfor_call` (`src/vm/eval_control.rs`), the generic-for
  iterator call;
- `State::table_sort` (`src/vm/table_ops.rs`), live across every comparator
  call;
- `State::to_string_with_meta` (`src/vm/table_ops.rs`), live across
  `__tostring` through `tostring_bytes`.

The plain build is unaffected (the macro expands to nothing), and
instrumented builds get 8 MiB in `tests/common/stack.rs`; no gate runs
`native_stack` under `hotpath`. But the rule is false today, the timing guards
in instrumented builds sit on every iterator, sort-comparator and
host-tostring native level, and the reported times for those three functions
include the nested Lua they call, so their percentages in a `--hotpath` run
are inclusive of arbitrary script work.

Fix: remove the three attributes. `scripts/native_calls.py` already computes
the exact set of functions that stay live across re-entry and finds every
`fn` item and its span; have it report any inventoried function, any
boundary, and `run_invocation`/`Frame::run` whose attribute block contains
`hotpath::measure`.

Enforceable: yes, in the existing `native-call-inventory` script check, at no
extra cost.

## BUG-002 - `hotpath-alloc` does not enable this crate's `hotpath` feature, so `cfg(feature = "hotpath")` guards fail open

Reported by: corpus-harness, native-stack-errors, compiler-cost.

`Cargo.toml` defines `hotpath-alloc = ["hotpath/hotpath",
"hotpath/hotpath-alloc"]`. That enables the dependency's features, not this
crate's own `hotpath` feature. Under `--features hotpath-alloc` the measure
guards are live (they key on the dependency's feature) while
`cfg(feature = "hotpath")` is false. Guards spelled with `"hotpath"` alone
therefore fail open under `hotpath-alloc`:

- `tests/diff_test.rs` compiles its corpus partitions out only for
  `"hotpath"`, for the stated reason that instrumentation pushes the corpus
  past the time budget. Under `hotpath-alloc` they compile in and run
  instrumented, contrary to AGENTS.md ("The corpus partitions are compiled out
  under the `hotpath` feature").
- `src/compiler/parser.rs` sets `DEEP_PARSE_TEST_STACK_BYTES` to 8 MiB under
  `cfg(feature = "hotpath")` only, so instrumented `hotpath-alloc` parser unit
  tests parse at the syntax budget on 2 MiB with timing guards on every parser
  frame, which is what the 8 MiB branch exists to prevent.
  `tests/common/stack.rs` sets `PARSE_TEST_STACK_BYTES` and
  `LIMIT_TEST_STACK_BYTES` to 8 MiB under `hotpath` *or* `hotpath-alloc`, and
  says its constant matches "as `src/compiler/parser.rs`'s unit-test constant
  does"; that parity claim is false for `hotpath-alloc`.
- `tests/common/mod.rs`'s `cfg_attr(feature = "hotpath", allow(dead_code))`.

`tests/common/stack.rs` and `tests/error_boundary.rs` spell both features and
are correct, so the repository already knows; the guards disagree.

Fix: `hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]`, so
`cfg(feature = "hotpath")` means "instrumented" everywhere and the
`any(hotpath, hotpath-alloc)` spellings collapse. Then keep the parse-test
stack size in one place both the unit tests and the integration tests read (a
`#[doc(hidden)] pub` constant in `limits`, a `cfg(test)` export, or a
`tests/common/stack.rs` that `src/` includes the way it already includes
`tests/common/examples.rs`).

Enforceable: the feature implication is structural (Cargo enforces it). A
textlint rule banning `feature = "hotpath-alloc"` inside `cfg(` anywhere except
the allocator installs (`src/main.rs`, `examples/hotpath.rs`) keeps it so. One
constant instead of two removes the drift.

## BUG-003 - `crmath/tests/golden.rs` swaps the process-wide panic hook without a guard

Reported by: runtime, library.

`src/crmath/tests/golden.rs::check_inputs` does `take_hook`, installs a no-op
hook, runs `check_inputs_quietly`, then `set_hook(previous)`. It runs in the
lib unit-test binary, concurrently with every other unit test there.

- While it runs, any other test's panic message (a failing `assert_eq!` in
  `src/vm/tests.rs`, say) is swallowed, so a failure reports with no message.
- If `check_inputs_quietly` panics (its `h.join().expect(...)` can), the
  silent hook is never restored, and every later test in the process fails
  with no panic message.
- Two concurrent `check_inputs` calls race: A takes the default and installs
  the no-op, B takes A's no-op and installs another, A restores the default,
  B restores the no-op. From then on every panic in the binary is silent.
  Under a plain `cargo test -- --include-ignored`, which uses parallel
  threads, this can happen; `brokkr test` serializes with `--test-threads=1`,
  which hides the race.

The hook exists only to keep oracle panics from flooding the output, and
`catch_unwind` already isolates them. `with_forced_fma` has the same shape
(restore only on the success path); it is harmless because the state is
thread-local and the thread dies with the panic, but a drop guard costs
nothing. `tests/panic_poison.rs` shows the right shape: install once behind a
`Once`, filter on the payload, forward everything else.

Fix: restore in a drop guard, or install one filtering hook once per process
(or a thread-local "quiet" flag the hook consults). Better still, make
`certify` return `Err` instead of panicking and remove the hook.

Enforceable: yes, a clippy `disallowed-methods` entry for
`std::panic::set_hook`/`take_hook` with a scoped allow in the one sanctioned
installer.

## BUG-004 - A focused `brokkr test` runs ignored tests that rewrite a tracked fixture or write into `.scratch/`

Reported by: check-machinery, corpus-harness, compiler-cost, library.

`brokkr test` always passes `--include-ignored` and matches by substring.
Two ignored tests have side effects that the `#[ignore]` was the only guard
against:

- `src/crmath/tests/golden.rs::regenerate_crmath_golden` is
  `#[ignore = "rewrites tests/fixtures/crmath_golden.txt"]` and calls
  `std::fs::write` on the tracked fixture it is meant to verify. Any focused
  run whose name matches it rewrites the fixture: `brokkr test golden` (a
  natural way to run `cost_golden`, `save_golden`,
  `golden_results_on_every_fma_path` and `stdlib_cost_golden` together),
  `regenerate`, `crmath`. `research/core-math` is present on plantasjen, so
  the rewrite really happens there. The bytes are identical (regeneration is
  deterministic), but the new mtime makes cargo rebuild the lib test target
  through `include_str!`. The module doc admits the problem.
- The same substrings pull in the long ignored crmath sweeps: `brokkr test
  crmath` runs the regenerator, all 11 worst-case sweeps and
  `random_against_oracle` (several million inputs, documented "run in
  release") in the dev profile by default, which hits the 20s per-test cap and
  stops the run. `brokkr test sweep`, `pin`, `tables` and `oracle` each pull in
  some of the long-running or Python-spawning ignored tests.
- `tests/error_boundary.rs::error_boundary_transcript` is
  `#[ignore = "on-demand transcript ..."]` and writes
  `.scratch/error-boundary/transcript.txt` through
  `scratch::write_boundary_transcript`. The documented focused command
  `brokkr test error_boundary` (dev profile, the repository default) matches
  and runs it, so it writes into the source tree as a side effect. AGENTS.md's
  `--release` form avoids it only because the test is compiled out without
  `debug_assertions`.

The save golden already uses the safe pattern: a `regenerate_save_golden`
example.

Fix: move regeneration and on-demand writers out of the test harness. Because
the crmath oracle is crate-internal test code, that means a `crmath-regen`
cargo feature that compiles the regenerator only when requested, or a
regenerator that writes `crmath_golden.txt.new` and never rewrites the fixture
in place. Give the long sweeps a feature or `cfg` instead of `#[ignore]`, and
names that share no substrings with the fast tests. Make the transcript a
`brokkr run` tool that takes an output path (see also the transcript entries
in verif-corpus.md and verif-assertions.md).

Enforceable: a test that is not compiled cannot be name-matched. A textlint
rule with `test_code = "only"` banning `fs::write`/`File::create` into the
manifest dir in test code would have caught it.

## BUG-005 - `scripts/notes_move.py` loses moved sections when the destination is also a source

Reported by: check-machinery.

`scripts/notes_move.py` rewrites every source file it was given, including
ones it took nothing from, and normalises their trailing newlines. If `dest`
is also one of the sources, the second write overwrites the first and loses
the moved sections. `scripts/test_notes_sections.py` covers only the fence
handling; it does not cover `main()` or this data loss. Separately,
`notes_drop.py` with no arguments dies with an `IndexError` traceback rather
than a usage message.

Fix: refuse `dest in sources`; write a file only when its bytes changed (see
the generators entry in verif-gates.md); give `notes_drop.py` an argument
check; extend the script test to cover `main()`.

Enforceable: yes, by the extended script test.

## BUG-006 - `native_calls.py` counts commented-out scenarios as present

Reported by: check-machinery, native-stack-errors.

`scripts/native_calls.py::scan_test_lists` locates the `SCENARIOS`, `CHAINS`
and `LEAVES` list bodies in the sanitized code, but then runs
`re.findall(r'\bname:\s*"..."')` over the *raw* text slice
(`body = text[m.end():end]`). A scenario commented out with `//` in
`tests/native_stack.rs` still counts as present, so the inventory check stays
`ok` while the test no longer runs that path. Any `name: "..."` field nested
inside an entry (an inline struct, say) would also be read as a scenario. It
also cannot see a scenario that is listed but skipped (a filter in the loop,
`#[ignore]` on `every_limit_at_once_fits_the_limit_test_stack`, a cfg on the
test).

Fix, smallest: extract names from the sanitized code (blank comments but not
strings). Fix, structural: move the cross-check into the test.
`native_stack.rs` can `include_str!("../reference/native-calls.toml")`, parse
the scenario names and assert they equal `SCENARIOS`' names at run time; a
check that lives in the running test cannot be satisfied by a test that does
not run.

Enforceable: yes, either way.

## BUG-007 - Save bytes differ between dev and release builds of one version, while the docs and the golden say otherwise

Reported by: corpus-harness, snapshot, compiler-cost.

`tests/save_golden.rs` documents that the fixture "must come from the profile
the tests run under": a release build (`lto = "fat"`) can fold
`base_unpack_values` and `table_unpack_values` (two byte-identical wrappers
around `unpack_values` in `src/lua_std/basic.rs` and `src/lua_std/table.rs`),
and the save then writes the lexicographically smallest id (`table.unpack`)
for the global `unpack`. The golden program loads the standard library, so
every builtin global is in the save as an `Fn` token and folding reaches the
bytes. Consequences:

- `docs/save-state.md` "Byte stability" says "Two saves of the same logical
  state are byte-identical with the same crate version", and the module doc of
  `src/vm/save_state.rs` repeats the crate-version wording. The same docs
  file's "Rust functions" section says "snapshot bytes are only guaranteed
  identical across the same build". They contradict each other, and the
  crate-version wording is false. The module doc's "because the diagnostic
  crate-version string is part of the blob" also inverts the logic: the string
  makes saves differ across versions, it does not make them identical within
  one.
- The pinned bytes are a debug build's; the bytes a shipped release game
  writes are pinned by nothing.
- Nothing in the test detects its profile. Run under release, it fails with
  "the walker reordered the object graph and every existing save file is now
  unreadable", which is the wrong diagnosis (a pure traversal reorder does not
  make old saves unreadable either; ids are dense indices). `brokkr test
  save_golden --release` fails on correct code. The snapshot hunter adds that
  `brokkr test save_traversal_order` should also run in the
  `release-native-stack-snapshot` sweep (brokkr drops each sweep's test
  filter), so a focused run may fail in one of its two sweeps; whether LLVM
  merges the two wrappers there needs one run to confirm.
- `examples/regenerate_save_golden.rs` does not check its profile; run with
  `--release`, it silently writes a fixture every dev sweep then rejects.
- The failure message offers only "format or traversal change"; a
  `COST_MODEL_VERSION` bump also changes bytes and fails it.

Fix, in order of preference given by the hunters: remove address identity
from Rust functions (the registry-handle rewrite in rewrites.md), after which
the golden is profile-independent and can run in a release sweep; failing
that, build the golden State with `State::raw()` plus only the functions the
program needs so no foldable pair is reachable, or normalise function ids
before comparing; at minimum mark the test
`#[cfg_attr(not(debug_assertions), ignore = "...")]` and have the regenerator
refuse to run unless `cfg!(debug_assertions)`. Correct the docs and the
failure message either way.

Enforceable: once profile-independent, add `save_golden` to a release
`[[check]]`. The registry-handle rewrite makes the hazard unrepresentable.

## BUG-008 - An unterminated string's error position depends on the newline convention

Reported by: compiler-cost.

`lexer_error_positions_stay_pinned_across_newlines` in `src/compiler/lexer.rs`
calls its own expectation a "Pre-existing quirk, preserved": an unterminated
short string ending in `\r` or `\n` reports line 2 column 1, while one ending
in `\r\n` or `\n\r` reports line 1 column 4. The reported position depends on
the newline convention of the author's editor, which by AGENTS.md's own rule
is a script-author defect (a misleading message), not a reference-Lua
difference to preserve. The test freezes it.

Fix: make the error position independent of the newline flavour and update
the pin.

Enforceable: the same test holds the fixed version.
