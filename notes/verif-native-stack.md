# Verification: native stack and limits

Findings from the verification hunt about the native-stack envelope and the
limit tests: `tests/native_stack.rs`, `reference/native-calls.md` and
`reference/native-calls.toml`, `tests/common/stack.rs`, the at-cap and
iterative-walker tests, and the claims in `docs/limits.md`. Each entry names the
scopes whose hunters reported it.

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

## VN-001 - The inventory's edge coverage is declared, not observed

Reported by: check-machinery, native-stack-errors.

`reference/native-calls.toml` gives every scenario an ordered edge path, and
`native_calls.py` checks that each path is syntactically connected and every
edge is covered by some path. An edge counts as "covered" when the inventory
says so; nothing observes that `every_limit_at_once_fits_the_limit_test_stack`
actually traverses the marked call site. `reference/native-calls.md` presents
the script as keeping the inventory, markers and test "in agreement"; that
agreement is between declarations, not executions. Examples where the claim
rests on prose about the implementation:

- `sort_comparator_right` claims `table_sort_sift_down->table_sort_less:right`
  because "the heapify pass compares the left child, then the right" for
  `{3, 1, 2}`; `sort_comparator_sortdown` claims the sort-down edge because the
  third comparison is "the first of the sort-down pass". A change in heap order
  keeps both green while those edges go uncovered.
- `index_method` claims `run_frame->reg_get_field:self` rather than `:field`;
  nothing distinguishes them at run time.
- `tfor_ipairs` relies on "ipairs over a table with a metatable leaves the
  dispatch loop's fast path"; if the fast path learned to read through
  `__index`, the scenario would stop exercising `base_ipairs_iter`.

The witnesses in `native_stack.rs` prove recursion happened through *some*
native path at each level, not which.

Fix: make the marker observable. A `native_call!("edge-id")` macro that expands
to nothing normally and, under a test-only cfg (`--cfg dellingr_edge_trace`, so
frames in every shipped configuration are unchanged, which is why markers are
comments today), records the id in a per-State log (native-stack-errors); or a
`cfg(any(test, feature = "fault-injection"))` counter keyed by edge id
(check-machinery). `native_stack.rs` under that cfg asserts each scenario's
recorded edge set contains its toml path, or that every non-excluded edge was hit.
The marker stays one token per call site, as the script and textlint parse it.

Enforceable: yes; it would make the toml's coverage column true by
construction.

## VN-002 - "A scenario's path is one native level" is false for two scenarios

Reported by: native-stack-errors.

`reference/native-calls.md`: "A scenario's path is one native level: it starts
at an edge out of `call_unguarded` and ends with an edge into it."
`index_rust` and `newindex_rust` take two levels per cycle (`native: 2` in the
test); their toml paths pass through `call_unguarded` in the middle via
`@rust_handler`. The measurement section further down says so. The script does
not cross-check a scenario's `native` count against its path.

Fix: reword, and have the script read `native:` from each `Scenario` (it already
parses those blocks) and compare it to the count of `->call_unguarded` edges.

Enforceable: yes, in the script.

## VN-003 - The gmatch wrapper's compile-at-depth peak is measured once, by whichever test runs first

Reported by: native-stack-errors, runtime, compiler-cost.

`docs/limits.md`, Envelope: "`string.gmatch` creating its iterator in the last
native level (the first call in a process also compiles the fixed wrapper chunk
there)". `GMATCH_WRAPPER` in `src/lua_std/string.rs` is a process-global
`OnceLock`. In `every_limit_at_once_fits_the_limit_test_stack` the first gmatch
call in the process is the `index_field / index / gmatch` run, so the
compile-at-depth peak is measured for exactly that scenario and chain, not for
every path as the test's doc and limits.md say each deepest-point item is.
`index_field` is one of the cheaper cycles (`gsub_pattern_table` is roughly twice
its bytes per cycle in the recorded tables). Coverage also depends on test order
and scheduling: if any other test in the binary (or, after a merge, in a shared
binary) calls `string.gmatch` first, the peak is silently never measured.
`is_gmatch_wrapper` answers from the process-global (`false` until something
compiled it), so error attribution through the shim is correct only because the
shim cannot run before it is compiled.

Fix: compile the wrapper eagerly when the string library opens (or from a
`LazyLock` forced in `State::new`; the cost is one tiny compile per process,
already paid on first use) and drop the "first call compiles" clause from the
envelope; or make the cache per State so every gmatch leaf run pays it. Either
removes a process-global from the envelope argument and the ordering argument.

Enforceable: the eager init is structural; a unit test can assert
`GMATCH_WRAPPER` is set after `State::new()`.

## VN-004 - The envelope is stated for x86-64 Linux, and the test runs anywhere

Reported by: check-machinery, native-stack-errors.

`docs/limits.md` says how `tests/native_stack.rs` validates the envelope "on
x86-64 Linux". The test is not gated by `target_arch` or `target_os`. On another
host (aarch64, macOS, a 64 KiB-page kernel that rounds thread stacks up) it
either passes without the doc making any claim, or overflows and aborts the whole
binary with a message that cannot name the run (VN-008). Thread sizes also
include whatever static TLS the C library carves out of a thread's allocation,
which differs between the test binary and an embedder's game thread.

Fix: gate the assertion to the stated target and report "envelope not stated for
this target" elsewhere (print the arch and fail or skip), or widen the statement
after measuring.

Enforceable: by `cfg`.

## VN-005 - `cfg!(debug_assertions)` stands in for the build profile

Reported by: check-machinery, corpus-harness, native-stack-errors, runtime.

`tests/common/stack.rs` picks 4 MiB or 2 MiB by `cfg!(debug_assertions)` and
says it is "not a definition of unoptimized". The brokkr manual calls the
heuristic unreliable under profile overrides, and nothing ties it to
`Cargo.toml`. A `[profile.release] debug-assertions = true` (a common hardening
move), or a dev profile with `opt-level = 3`, or a custom profile with debug
assertions and optimisation, silently selects the other envelope: the release
sweeps would test the looser 4 MiB envelope and still report "release". Neither
exists in the repository today, so the guard holds; it is keyed on a proxy and
reports nothing when the proxy stops matching.

Fix options: have the release sweeps export a marker through `[[check]] env` and
refuse when it disagrees with `cfg!(debug_assertions)`; or compare against
`BROKKR_TEST_BIN_DIR`, which ends in `/release` for those sweeps; or a
`build.rs` that exports cargo's `PROFILE` as a `cfg` so the two repository
profiles are named rather than inferred.

Enforceable: yes.

## VN-006 - The release envelope is validated for a profile no embedder ships

Reported by: native-stack-errors.

`[profile.release]` here is fat LTO with one codegen unit. Cargo profiles of a
dependency come from the embedding workspace, so the game builds dellingr with
its own profile, typically Cargo's default release (thin-local LTO, 16 codegen
units), whose inlining and frame sizes differ. `docs/limits.md` says other
configurations are the embedder's to validate, which is honest, but the
repository's own release envelope certifies an artifact nobody links.

Fix: run the release envelope sweep under Cargo's default release settings too
(a custom profile `release-default` inheriting from `release` with the defaults
restored), or state the envelope for that profile instead.

Enforceable: yes, as a `[[check]]` shape.

## VN-007 - The envelope depends on build inputs the repository neither pins nor records

Reported by: native-stack-errors, corpus-harness.

- Toolchain: `reference/native-calls.md` and `docs/limits.md` record bytes per
  cycle and frame sizes with commit and host, but not the rustc version, and the
  repository pins no toolchain (no `rust-toolchain.toml`). Frame sizes are a
  compiler property first; a toolchain bump can flip `release-native-stack`
  red or green with no source change, and "this repository's release build
  settings" is not reproducible without it.
- Host rustflags: `~/.cargo/config.toml` on plantasjen sets
  `-Ctarget-cpu=native` (wider vector registers, different spill decisions),
  links with `wild` via `clang`, sets `[profile.dev] debug =
  "line-tables-only"`, and installs `brokkr-rustc-guard` as the rustc wrapper.
  None is in the repository. `tests/common/stack.rs` states the envelope as "4
  MiB with this repository's dev settings and 2 MiB ... with its release
  settings", but the frames it bounds are measured under these host flags; on a
  host or CI without them frame sizes differ. Native tuning also means the
  crmath FMA path is the only one corpus runs exercise on this host (see the FMA
  mode entries in verif-assertions.md).
- `reference/native-calls.md` says "All 57 scenarios were rerun"; the inventory
  has 55 scenarios (57 matches the probe's rows, which include two `chain_`
  rows). The file has become a measurement log (four dated sections, several
  superseded, rows for functions that no longer exist) inside a `reference/`
  document that must be true, and a reader cannot tell which table is current.

Fix: pin the toolchain (`rust-toolchain.toml`) so the envelope is stated for a
named compiler, and record `rustc -V` with every measurement; put the flags the
envelope depends on in a repository `.cargo/config.toml` (or `[profile]`
settings) so they travel with the code, or state in `docs/limits.md` that the
envelope was verified under plantasjen's global rustflags and add a sweep without
them; move the measurement history to `reference/performance.md` (the folder
table's home for measured numbers over time), leaving native-calls.md with the
guard and one current table; drop the scenario count.

Enforceable: the pin is structural; the rest is editorial.

## VN-008 - `every_limit_at_once_fits_the_limit_test_stack` is one serial test of 990 runs

Reported by: native-stack-errors.

55 scenarios x (2 held chains x 8 leaves + 2 terminal chains) = 990 runs in one
`#[test]`, on one thread, in each of four sweeps.

- The chunk is recompiled for every run (`check` calls `Program::compile`), and
  each contains `MAX_CHUNK_NESTING - 1` nested functions plus every chain and
  leaf. It depends only on the scenario: 55 compiles would do (one, with the step
  selected by a global like the chain and leaf). 935 of 990 compiles are
  redundant; not measured, and a dev compile of a 199-deep chunk is not free.
- The first failing combination panics and hides the rest.
- A native stack overflow aborts the process, and the abort message names the
  thread, which is `every_limit_at_once` for all 990 runs: the one failure the
  test exists to catch is the one it cannot attribute to a scenario, chain or
  leaf.
- libtest cannot parallelise it.

Fix: one `#[test]` per scenario (a macro over `SCENARIOS` or a build-time list),
each on a thread named after its scenario, iterating chains and leaves inside;
compile once per test.

Enforceable: by structure.

## VN-009 - At-cap probes refill a million slots per attempt

Reported by: native-stack-errors, runtime, library.

`frame_reservation.rs::run_at_cap` and `index_depth_routes.rs::call_near_cap`
find the highest admitting stack height by starting at `MAX_STACK_SIZE - 2` and
lowering by one; every attempt does `set_top(fill)` from 0 (one million
`Val::Nil` pushes, one at a time, in `State::set_top`) then `set_top(0)`. The
attempt count is the probe's frame size plus `FRAME_HEADROOM`.
`frame_reservation.rs` does this for 18 bodies plus three more;
`index_depth_routes.rs` calls `call_near_cap` about a dozen times.
`host_error_reporting.rs` (`execution_stack_preflights_notify_once`, four cases;
`attempt_result_padding`, three modes) and `host_call_preflight.rs` fill the same
million through public `push_nil`. In `src/vm/tests.rs`, `fill_to_one_below_cap`
and its users, `host_table_api_atomicity.rs`'s `get_table_preserves_the_key_*`
and `raw_reads_replace_the_key_even_at_the_stack_cap` (looped four times in one
test), the `string_metatable_dispatch` stack-cap tests (`set_top(999_999)`), and
`get_table_preserves_the_key_at_the_lua_call_depth_limit` (recursing to the real
`MAX_CALL_DEPTH`) do the same at full scale. Estimated on the order of hundreds of
million-slot fills per sweep in dev, both dev sweeps, likely seconds, plus a
memory peak when libtest runs a dozen in parallel; not measured.

Fix: fill once and lower the height by popping one slot per failed attempt (a
failed host `call` leaves the filler as it was, which these tests already assert),
or compute the exact height from the probe's `max_stack`, which
`Program::disassemble` prints. Lateral: `set_top`'s growth arm could `resize`
after its preflight instead of a push loop. The test-only `Limits` override in
rewrites.md is the alternative the runtime and library hunters prefer; the
native-stack hunter argues against it.

Enforceable: no; it is a fixture shape.

## VN-010 - The byte probe swallows its own check for the raising scenario

Reported by: native-stack-errors.

`native_stack.rs::native_bytes_per_level`: `finish()` raises `error('uneven
gaps')` before `report`, and the outer loop ignores any error when
`scenario.raises`. For `error_tostring`, `finish` runs inside the `__tostring`
handler, so an uneven gap produces no report row and no failure.

Fix: for raising scenarios, require the error to be the expected `ScriptError`
with rendering `r` (as `check` does), so the probe's own error fails it.

Enforceable: by that match.

## VN-011 - `docs/limits.md` places at-cap tests under `test_limited.sh`; they are not

Reported by: native-stack-errors.

`docs/limits.md`, Value stack: "Tests that push to `MAX_STACK_SIZE` pay that per
State, inside the fixed virtual-memory cap `scripts/test_limited.sh` applies".
Only `tests/run_examples.rs` runs anything under `test_limited.sh` (the example
scripts). The Rust tests that push to the cap (`frame_reservation.rs`,
`index_depth_routes.rs`, `host_error_reporting.rs`, `host_call_preflight.rs`,
`vm_depth_regressions.rs`, `error_boundary.rs`) run as ordinary `cargo test`
processes, and nothing in `brokkr.toml` wraps them. False today.

Fix: reword to say the cost is paid per State with no cap applied, or apply a cap
(an `RLIMIT_AS` set by a test-crate helper before the State is built; `libc` is
already a dev-dependency).

Enforceable: only the second.

## VN-012 - Envelope and parse-depth claims checked for fewer shapes than stated

Reported by: native-stack-errors, compiler-cost.

- `docs/limits.md`, Envelope: "a pattern match one call short of `MAXCCALLS`
  ..., or its rejection at `MAXCCALLS`". The accept leaf runs all three recursion
  shapes (optional, greedy through `max_expand`, minimal through `min_expand`);
  the `pattern_reject` leaf rejects only through the optional shape, whose
  per-level frame is the smallest (no expansion helper frame). Add greedy and
  minimal rejection leaves, or say which shape the rejection peak is measured on.
- `docs/limits.md`: "parsing at the syntax budget fits a 2 MiB thread in both
  settings." The worst parse shapes are driven by the parser's unit tests, which
  run only in the dev sweeps. In release the only parse-at-budget test is
  `native_stack.rs`'s nested-`local function` shape
  (`deepest_nested_functions_parse_on_the_parse_test_stack`); the call chain,
  constructor chain, unary, `^`, parenthesis and statement shapes are dev-only.
  Release frames are usually smaller, but inlining can make one larger. Move the
  shape list into `tests/native_stack.rs` or a sibling listed in the release
  shapes.
- `docs/limits.md`: "`local x = ((...1...))` compiles up to 98 parentheses and
  `return f(f(...f()...))` up to 49 calls", and the "Units per level" table. The
  49 is asserted (computed from constants) in `deepest_nested_programs_body`; the
  98 is asserted nowhere (the nearest test uses 95 "below the limit"), and the
  per-construct unit table is asserted nowhere. `deepest_accepted_program_until`
  already finds the exact boundary per shape: a unit test can derive units per
  level and compare with the table parsed out of `docs/limits.md`, in the style of
  `documented_opcode_charges_match_static_units`.
- `deepest_nested_functions_parse_on_the_parse_test_stack` accepts any
  `SyntaxError` for the first rejected depth; pin the depth variant, or a source
  change that fails for another reason still passes.

Enforceable: yes, by the added leaves and tests.

## VN-013 - Tests of iterative algorithms run on the ambient thread stack

Reported by: runtime, snapshot, compiler-cost.

`tests/common/stack.rs` pins integration-test stacks explicitly "so
`RUST_MIN_STACK` cannot loosen it", and the parser's depth tests use
`DEEP_PARSE_TEST_STACK_BYTES` for the same reason. These do not, and run on
libtest's default thread, whose size `RUST_MIN_STACK` sets (and release frames
are smaller than dev frames):

- `src/vm/tests.rs::auto_gc_marks_a_deep_lua_table_chain_iteratively` and
  `object.rs::deep_table_chain_marks_iteratively_and_reuses_the_empty_worklist`;
- `deep_global_chain_saves_and_loads_iteratively` (a 50000-deep chain), which
  also computes `done` and never reads it;
- `syntax_depth_headroom` (`src/compiler/parser/tests.rs`), "the empirical check
  that MAX_SYNTAX_DEPTH fits the stack a debug cargo test thread gets (2MB by
  default)", which also hard-codes depth 190, paren 95 and
  `depth / (2 + SYNTAX_SUFFIX_COST)` instead of deriving them from
  `MAX_SYNTAX_DEPTH`, and is superseded by
  `nested_statement_and_expression_shapes_compile_within_the_test_stack` and
  `deepest_nested_programs_compile_within_the_test_stack` in `src/compiler.rs`,
  which measure the exact deepest program on an explicit 2 MiB thread.

With a large `RUST_MIN_STACK` in the environment, a regression to recursion
passes all of them.

Fix: run each on an explicitly sized thread (a lib-side equivalent of
`on_limit_test_stack`, e.g. 256 KiB for the save walker via
`std::thread::Builder::stack_size`), assert on `done`, and delete
`syntax_depth_headroom`. Route deep-parse tests through one helper
(`on_parse_test_stack`) so the helper is the only place a depth loop lives.

Enforceable: partly; a convention check requiring `*iterative*`/`*deep*` tests to
spawn their own thread is keyed on a name and would fail open.

## VN-014 - `frame_sizes.py` prints nothing on a non-matching arch

Reported by: check-machinery.

`scripts/frame_sizes.py` parses AT&T x86-64 prologues (`sub $0x..,%rsp`). On
another arch, or when no symbol matches, it prints nothing and exits 0, so a probe
comparison reads as "no frames".

Fix: fail when no symbol matched or the arch is not x86-64.

Enforceable: yes, in the script.
