# Verification hunt: library scope

Scope: `src/lua_std/`, `src/patterns/`, `src/gmatch_schema.rs`, `src/numeral.rs`,
`src/crmath/` (kernels, `tests/{golden,oracle,fma}.rs`, `tables/`),
`tests/fixtures/crmath_golden.txt`, `scripts/crmath_tables.py`, and the
integration suites `stdlib_argument_errors`, `stdlib_divergences`,
`stdlib_numeric_format`, `string_bytes`, `string_format`, `string_metatable`,
`string_metatable_dispatch`, `gsub_errors`, `number_format_g14`,
`number_rendering`. I followed questions out of scope where the answer lived
elsewhere (`stdlib_cost_golden.rs`, `save_state.rs`, `src/vm/rng.rs`,
`src/vm/lua_val.rs`, `src/vm/eval.rs`, `tests/test_conventions.rs`,
`brokkr.toml`, `clippy.toml`, `docs/determinism.md`,
`reference/deterministic-math.md`).

I made no edits and ran no builds or tests, so every cost figure below comes
from reading the code. Where I could not measure something, I say so.

Two notes on scope:

- `tests/stdlib_optional_args.rs` does not exist, and git history has never
  had it. The optional-argument coverage lives in
  `stdlib_divergences::optional_math_nil_arguments_use_their_defaults` (which
  covers `math.atan` and `math.log` only) and in `vm_aux.rs`'s helper unit
  test. Finding 4.3 covers what this leaves out.
- `basic.rs`, `math.rs`, `string_format.rs`, `table.rs`, `lua_std.rs` and
  `gmatch_schema.rs` have no in-tree test modules. In `lua_std/`, only
  `string.rs` has one.

---

## 1. Tests that prove nothing

### 1.1 `number_rendering::concat_number_rendering_matches_tostring_across_f64_classes` compares `number_to_string` with itself

`tostring`, host `State::concat` (through `concat_values`), `OP_CONCAT` and
`Val::to_bytes_with_heap` all call `vm::lua_val::number_to_string`. The test
walks 16k bit patterns and asserts that `concat` equals `tostring`. Both sides
come from the same function, so the test cannot fail unless someone adds a
second rendering path. Even then it would miss a fast path in `OP_CONCAT`,
because the test calls the host `State::concat`, not Lua `..`. Nothing
independent checks the values: number_to_string could be wrong on all 16k
inputs and this test would stay green.

There is an independent second implementation in the tree.
`string_format::format_general` is a separately written `%g` engine
(`format_scientific` plus `format_fixed`), while `number_to_string` uses
`format_g14` plus an i64 fast path. They share only Rust's core float
formatting.

- Fix: assert `tostring(x) == string.format("%.14g", x)` over the same
  16k-class sweep plus a few thousand random bit patterns. Run it through
  Lua `..` as well as through the host API.
- Enforced by: the test itself.

### 1.2 `patterns::tests::plain_classifier_tracks_pattern_grammar_magic_bytes` restates its constant

The test loops over the literal `b"^$*+?.([%-)"`, which is byte-for-byte the
production `PATTERN_MAGIC_BYTES`, and asserts that each byte is magic. Both
sides are the same list. The test never consults the pattern grammar its name
mentions, and it does not check that the other 245 bytes are plain.

- Fix: for every byte `b` in `0..=255`, assert that `is_plain(&[b])` holds
  exactly when `LuaPattern::new(&[b])` matches the subject `[b]` as a
  one-byte literal at `0..1`. Do the same for `is_plain_for_find` against
  `string.find`'s plain path. This ties the classifier to the matcher.
- Enforced by: the test.

### 1.3 `patterns::luapat::tests::escape_class_table_matches_the_per_byte_classifier` uses a near-copy as its oracle

`match_class`, the test oracle, and `table_match_class`, which builds the
production table, are the same match statement over the same
`is_ascii_*` predicates, and both call `numeral::is_lua_whitespace`. The test
does check the mapping from `escape_class` letters to table indices. It cannot
catch a wrong class definition, which is the replay-visible part: `%p`,
`%c`, `%g` membership. The only independent reference is C-locale Lua.

- Fix: add one `examples/` script that prints, for each of the 22 class
  letters, the set of bytes in `0..=255` that match. Run it unmarked so the
  diff gate compares it against lua5.2 and lua5.4. `diff_test.sh` already
  exports `LC_ALL=C`, so the references agree with dellingr's fixed ASCII
  classes. Delete the copy oracle, or keep it only for the index mapping.
- Enforced by: the diff gate. It needs reference Lua installed, which the
  gate already requires.

### 1.4 `patterns::errors::tests::structured_details_keep_existing_display_text` checks one side twice

`PatternError`'s `Display` is defined as `PatternErrorDetail::from(self)`'s
`Display`. Its two assertions, `error.to_string() == expected` and
`detail.to_string() == expected`, therefore test the same code twice. The
literals do pin the text, which has some value, but the second assertion adds
nothing.

- Enforced by: no rule needed. Drop one assertion.

### 1.5 `golden::pinned_regression_inputs_survive_golden_regeneration`: the first half is a tautology

It calls `add_pinned_regression_inputs(f, &mut inputs)` from
`PINNED_REGRESSIONS`, then asserts that `inputs` contains the pair it just read
from `PINNED_REGRESSIONS`. The second half, a row lookup in `GOLDEN`, is real.

### 1.6 `golden_results_on_every_fma_path` passes on a truncated fixture

It asserts only that `cases > 0`. If the fixture loses every row for some
function, through a bad regeneration or a merge, the test stays green. It also
never checks the fixture's `# Worst-case sample from CORE-MATH commit ...`
header against `CORE_MATH_COMMIT`.

- Fix: count rows per `Func` and require a floor for each, or pin
  `(rows per function, digest of the file body)` the same way `CORPUS_PINS`
  pins the worst-case corpora. Assert that the header names
  `CORE_MATH_COMMIT`.
- Enforced by: the test. No checkout is needed.

### 1.7 `math_random_is_deterministic_per_seed` only checks same-process repetition

`draw(42) == draw(42)` inside one process cannot fail for an in-crate
SplitMix64. `draw(42) != draw(43)` is close to certain. The replay contract
needs the Lua-visible draw sequence to stay stable across builds and hosts, and
nothing pins it. `vm/rng.rs::splitmix64_stream_is_pinned` pins `next_u64` and
`random_range_i64(1, 6)`. It does not pin `random_f64`, whose scale comes from
`numeral::MAX_EXACT_INTEGER`, a constant defined somewhere else entirely, and
it does not pin the mapping from Lua arguments to draws in `math.random`.

- Fix: a golden test that seeds a State, runs
  `math.random()`, `math.random(n)` and `math.random(m, n)` from Lua, and
  compares the printed sequence with literal expected strings.
- Enforced by: the test.

### 1.8 `examples/math.lua` prints results nothing compares

The file is `-- DIFF:` marked because of number formatting and
`math.random`, so the diff gate skips both references. `run_examples.rs`
checks only exit status, stderr, and lines ending in `: false`. Every
`math.sin(...) =`, `math.random() =` and similar line is printed and then
compared against nothing, so a wrong value passes. The same holds for the other
ten DIFF-marked examples. Their output is run-only.

- Fix, one of:
  - Split the deterministic, reference-comparable part into an unmarked
    file.
  - Give DIFF files a checked-in expected-output fixture that
    `run_examples.rs` compares byte for byte.
- Enforced by: `run_examples.rs`. A rule that every DIFF file must have an
  `.expected` sibling is easy to check mechanically.

### 1.9 Weak "is an error" assertions

Each of these accepts any error at all, including one from a typo in the test
script:

- `gsub_function_error_is_propagated` and
  `gsub_table_metamethod_error_is_propagated` assert only `is_err()`. They
  should match `ScriptError` with the payload `"boom"` or
  `"metamethod error"`.
- `string_call_without_metamethod_is_an_error` asserts only that the message
  contains `"string"`.
- In `find_subslice_matches_the_naive_scan_at_every_budget`, the arm
  `(Err(_), Err(_)) => {}` does not check that both errors are
  `BudgetExceeded`.
- `stdlib_integer_arguments_reject_fractional_numbers_consistently` matches a
  message substring, but never checks the argument number or the function
  name it should report.

### 1.10 Test names that claim more than the body checks

- `argument_details_are_structured_without_changing_text` checks
  `detail()` only, never the text.
- `every_math_function_has_its_flat_charge`, in `stdlib_cost_golden.rs`,
  checks a hand-written list. It matches `MATH_FUNCTIONS` today, but a new
  math function would go uncovered without any failure. See 2.6.

### 1.11 Setup steps that may silently do nothing

- `string_metatable::cached_handler_is_not_reused_after_it_is_collected`
  runs `for i = 1, 40 do local unused = {upper = i} end` to reuse the
  collected handler's slot. Nothing asserts that a slot was actually reused.
  With slotmap versioned keys, the stale key may never collide, in which case
  the test cannot fail whether or not the bug is present.
- `removing_and_readding_index_after_a_warm_hit` adds 40 keys to "reshape"
  the metatable. Nothing asserts that the hash slot of `__index` moved.
- Fix: a `#[doc(hidden)]` heap probe that exposes slot or index identity,
  asserted before and after the setup. Alternatively, a unit test in
  `vm/` that constructs the collision directly.
- Enforced by: the test, once the probe exists.

### 1.12 `runtime_result_limit_accepts_255_and_rejects_256` asserts `MAX_RESULTS == u8::MAX`

That is a fact about two constants. It belongs as a `const _: () = assert!`
beside `MAX_RESULTS`, not in a runtime test in a stdlib file.

---

## 2. Guards and claims that have stopped holding, or that nothing enforces

### 2.1 `brokkr test golden` overwrites `tests/fixtures/crmath_golden.txt`

`brokkr test` always passes `--include-ignored` and filters by substring. The
regenerator `crmath::tests::golden::regenerate_crmath_golden` is guarded only
by `#[ignore]`, which this ignores. The module doc admits the problem.
Consequences:

- `brokkr test golden`, which someone would type to run `cost_golden` or
  `save_golden`, also rewrites the crmath fixture. `research/core-math` is
  present on this host, so the rewrite really happens here.
- `brokkr test crmath` runs the regenerator, all 11 worst-case sweeps and
  `random_against_oracle`, in the dev profile by default. The sweeps cover
  several million inputs and are documented "run in release".
- `brokkr test sweep`, `brokkr test pin`, `brokkr test tables` and
  `brokkr test oracle` each pull in some of the long-running or
  Python-spawning ignored tests.

The `#[ignore]` guard is keyed on a name filter that the standard tool always
defeats. The fixture rewrite has identical bytes (regeneration is
deterministic), but the new mtime makes cargo rebuild the lib test target
through `include_str!`.

- Fix: move regeneration out of the test harness, as `save_golden`
  already does with `examples/regenerate_save_golden.rs`. Because the oracle
  is crate-internal test code, that means one of:
  - a `crmath-regen` cargo feature that compiles the regenerator only when
    requested, or
  - a regenerator that writes `crmath_golden.txt.new` and never rewrites the
    fixture in place.

  The sweeps should get the same treatment: a feature or `cfg`, not
  `#[ignore]`.
- Enforced by: structure. A test that is not compiled cannot be
  name-matched. `test_conventions.rs` could also reject `fs::write` of a
  `tests/fixtures` path anywhere under `src/` (see 2.2).

### 2.2 `test_conventions.rs` scans only `tests/`

The convention "test file mutations must use the scratch helper" and the
process-spawn markers apply only to `tests/**/*.rs`. In-crate unit tests under
`src/` are outside the scan. `src/crmath/tests/golden.rs` does the following,
all of which the convention forbids or would forbid in `tests/`:

- `std::fs::write(GOLDEN_PATH, ...)`, writing into the source tree;
- `std::process::Command::new("python3")`;
- reads of a gitignored sibling checkout.

The guard is keyed on a directory name, so it is silent about everything else.

- Fix: scan every `#[cfg(test)]` module under `src/` as well, or make the
  markers a `[[textlint]]` with `test_code = "only"` over `src/**`.
- Enforced by: the existing convention test or textlint, widened.

### 2.3 `scripts/crmath_tables.py --check` never runs in the standard gate

`reference/deterministic-math.md` says the tables "are not edited by hand"
and tells the reader to run `--check`. The only caller is the ignored
`core_math_tables_match_pin`, which needs both `python3` and
`research/core-math` at the pinned commit. Nothing in `brokkr check`
notices a hand edit to `src/crmath/tables/*.rs`, unless the edit happens to
change a golden row.

The upstream inputs are about 15 C source files, all MIT, and the checkout is
only needed for those files. Fix, one of:

- Vendor the table-bearing upstream files, unmodified, under
  `reference/core-math/` or `src/crmath/upstream/`, and make
  `crmath_tables.py --check` a `[[script_check]]`. It would then run on every
  check with no network and no checkout. The checkout stays necessary only
  for the worst-case sweeps.
- If vendoring is unwanted, add a `[[script_check]]` that hashes each
  generated table file against a manifest that `--generate` writes, as
  brokkr's workload pins do. A hand edit then forces a manifest bump.

Enforced by: a script check. Either variant is mechanical.

### 2.4 The CORE-MATH commit is duplicated in five places, and nothing cross-checks them

The sha `0ce0cbc1...` appears in:

- `scripts/crmath_tables.py` (`CORE_MATH_COMMIT`)
- `src/crmath/tests/golden.rs` (`CORE_MATH_COMMIT`)
- `src/crmath/LICENSE-CORE-MATH`
- `reference/deterministic-math.md`
- the golden fixture header

Only the golden.rs constant is enforced, and only by the ignored corpus pin.

- Fix: one non-ignored unit test that uses `include_str!` on the script, the
  licence and the fixture, and asserts that each contains `CORE_MATH_COMMIT`.
  Reword the reference doc to point at the constant instead of repeating the
  sha.
- Enforced by: the test.

### 2.5 "Registration order determines table iteration order and snapshot baselines" (`math.rs` header)

`pairs(math)`, `pairs(string)`, `pairs(table)` and the order of `_G` on a
fresh State are replay-visible (`AGENTS.md`: "`pairs` order ... feed game
replays"). No test pins them. The `save_state.rs` order tests look only at
user-added keys (`first`, `second`, `future`). Reordering `MATH_FUNCTIONS`
or the string registration list would pass every test. I did not establish
whether it would change `save_golden_current.bin`. Its program does not
mention any library table.

- Fix: a golden of `for k in pairs(lib)` key order for each stock library
  on a fresh State, in both feature shapes.
- Enforced by: the test.

### 2.6 Hand-maintained lists that fail open when the library grows

- `stdlib_cost_golden::every_math_function_has_its_flat_charge`. Should
  enumerate `pairs(math)` and fail on any function it has no arity for.
- The math `consume_cost(1)` comment says "every math function charges a
  flat 1". That holds today. The enumerating test above is the enforcement.

### 2.7 `docs/determinism.md`: "A singleton integer range ... returns its only value without advancing the RNG stream"

The behaviour comes from `random_range_i64`'s `end <= start` early return.
The doc calls it "part of the replay-visible draw sequence". No test calls
`math.random(1)` or `math.random(m, m)` and checks the following draw.

- Enforced by: a test. Fold it into the 1.7 golden.

### 2.8 `gmatch_schema` values are a persisted format with no golden

The module guarantees only that the runtime and the verifier share the
literals. Renaming `"pos"` or `"string.gmatch.iter"` passes every
round-trip test in `save_state.rs`, because save and load use the same build.
It would still break every snapshot saved by an older build. The save golden
program has no gmatch closure.

- Fix: add a live gmatch iterator, both the empty and the nonempty variant,
  to `tests/common/save_golden_program.rs` so the golden pins these names.
- Enforced by: the existing save golden.

### 2.9 `clippy.toml`'s libm ban is a name list, and the f32 side has gaps

The f64 list is thorough. The f32 list lacks `sinh` through `atanh`,
`exp2`, `exp_m1`, `log2`, `log`, `ln_1p`, `cbrt`, `hypot` and `sin_cos`.
Any libm-backed method that std stabilizes later, such as
`gamma`/`ln_gamma`/`erf`/`erfc` (unstable now), is not on the list, and the
ban will not notice it. The claim in `reference/deterministic-math.md` is
"nothing in the library calls the platform libm", and a name list cannot
enforce that claim.

- Fix: a `[[script_check]]` on the release artifact that lists undefined
  dynamic and static libm symbols
  (`nm --undefined-only` on the rlib or binary, or `objdump -d` for
  `call.*\b(sin|cos|tan|exp|log|pow|cbrt|hypot|...)`), allowing only
  `fmod` (see 5.4). This also catches LLVM lowering an intrinsic to a
  libcall, and dependencies doing so.
- Enforced by: the script check. It is mechanical, but needs the toolchain's
  `nm` or `objdump`, which `scripts/frame_sizes.py` already assumes.

### 2.10 Code comments cite tracking IDs whose definitions are gone

These comments cite IDs with no surviving definition anywhere in the repo:

- `stdlib_divergences.rs`: `VM-041`, `STD-014`, `STD-015`
- `stdlib_numeric_format.rs`: `STD-004`, `STD-007`, `STD-009`
- `number_format_g14.rs`: `STD-005`
- `string_format.rs`: `C30`

They pointed at a transient `notes/` document. `AGENTS.md` requires that a
code comment carry its own context.

- Enforced by: a `[[textlint]]` banning `\b(STD|VM)-\d{3}\b` and
  `\bC\d{2}\b:` in `src/**` and `tests/**`.

### 2.11 Stale claim in `concat_values` (`src/vm/eval.rs`)

The comment says "`1e308`, which Rust renders as 309 bytes". Numbers now
render through `number_to_string` (`%.14g`), so `1e308` is `"1e+308"`, six
bytes. The point about exact lengths still stands. The example describes the
old renderer. Reword without the figure.

### 2.12 "Verified against Lua 5.2/5.4" expectations held in Rust literals

Many unit and integration tests say their expected values were "measured
against both Lua 5.2 and 5.4":

- `bracket_ranges_follow_reference_branch_order`
- `accept_empty_repeats_do_not_consume_match_depth`
- `string_library_coerces_number_subjects_and_patterns`
- the `*_match_lua_54` tests in `string_format.rs`
- every row of `number_format_g14`

Those are one-time captures. The repo has a machine that keeps such captures
honest, the diff gate, but these tests are outside it.

- Fix: move each script-level expectation that does not depend on a
  deliberate divergence into an unmarked `examples/` file, and keep the Rust
  tests only for host-API and structured-error assertions.
- Enforced by: the diff gate.

### 2.13 Supported-host claims nothing exercises or refuses

- "The same [golden] file must pass on every target" is only ever run on the
  hosts that run `brokkr check`. There is no CI. The aarch64 hardware
  instance (`fma_dispatch` built on `mul_add`, under
  `#[allow(clippy::disallowed_methods)]`) and each kernel's aarch64 `cfg`
  dispatch arm (11 copies) are not even compiled or clippy-checked on an x86
  host.
  - Fix: a `brokkr.toml` check shape that runs
    `cargo clippy --target aarch64-unknown-linux-gnu` (needs only
    `rustup target add`, no runner). If a qemu-user runner is available, also
    run the golden test under it.
- "x87-only targets are not supported" is documented, but the code does not
  refuse to build for them.
  - Fix:
    `#[cfg(all(target_arch = "x86", not(target_feature = "sse2")))] compile_error!(...)`
    in `crmath/mod.rs`.
  - Enforced by: the compiler.
- The golden test runs only in the dev profile. The release profile (fat LTO,
  `codegen-units = 1`, `#[target_feature]` inlining) is what ships, and it
  never runs the crmath tests: the release `[[check]]`s select
  `native_stack` only. Rust does not contract or reassociate, so the risk
  is a codegen bug, not semantics. Still, the property "release reproduces
  the golden file" goes unchecked.
  - Fix: run the golden test in a release check shape. Because golden is a lib
    unit test, this needs either brokkr support for selecting the lib target in
    a shape, or the crate split in 3.1.

### 2.14 `[gremlins] exclude = ["tests/string_bytes.rs"]` excludes a whole file

The file needs non-ASCII for U+26A0 (the warning sign) in Lua source and
U+FFFD in a Rust literal. Because the exclusion covers the whole file, any real gremlin
(em-dash, smart quote, zero-width) added there later passes the scan.

- Fix: write the bytes as escapes (`"\226\154\160"` in Lua,
  `"\u{fffd}a"` in Rust) and delete the exclusion.
- Enforced by: the gremlins gate, with an empty exclude list. A convention
  check could reject non-empty `[gremlins] exclude`.

---

## 3. The suite as a system

### 3.1 crmath, patterns and numeral tests run twice per `brokkr check` and rebuild with every VM edit

The crmath tests have no `feature =` cfg (only `target_arch` and
`target_feature`). The same holds for `patterns/` and `numeral.rs`. Yet the
lib unit-test binary is built and run under both the `default` and
`snapshot` shapes. So every check pays twice for:

- `golden_results_on_every_fma_path`: 13,386 rows times 3 FMA modes;
- `fma::random_operands`, `ties_and_near_ties`,
  `subnormal_and_overflow_boundaries` and `special_values`: roughly 300k
  `check` calls, each doing a libbeef infinite-precision multiply, add and
  round;
- `oracle_known_values`.

There is no `[profile.dev.package.libbeef]` override, so libbeef runs at
opt-level 0. I did not measure the cost. It is the largest pure-CPU unit-test
block in my scope.

Because crmath is a module of the one crate, any edit anywhere in the VM:

- recompiles about 570 KB of generated tables (about 5,000 lines of
  `hf("...")`, each const-evaluated) in every build shape, including both
  clippy passes;
- relinks the lib test binary that carries the oracle.

Fix:

- Make `src/crmath/` a workspace crate (`dellingr-crmath`) with its own
  tests, tables and libbeef dev-dependency. Its compile is then cached across
  VM edits, its tests run once (it has no features), it gets a clean target
  for a release check (2.13), and the "crmath depends on nothing in the VM"
  boundary becomes a dependency rule.
- Independently, add `[profile.dev.package.libbeef] opt-level = 3`, a
  one-line, measurable win.
- Optionally, have the generator emit `f64::from_bits(0x...)` with the hex
  spelling in a comment. The generator would check exact representability
  once at generation time, so the compiler need not re-check it on every
  build.

Enforced by: crate boundaries are a structural rule. The profile override is
config.

### 3.2 About a dozen integration binaries for one library

`Cargo.toml` has no `autotests = false`. Each of the eleven suites in scope,
plus `stdlib_cost_golden` and `string_cost`, is its own test binary that
links all of dellingr. That means one link per shape per profile, and a
focused `brokkr test X` builds every binary in the package. Each file also
re-implements the same compile-load-call helper: there are about 30
`fn run` / `run_bytes` / `expect_error` / `assert_runtime_error`
definitions across `tests/`, and `gsub_errors.rs` inlines the sequence
around 20 more times. No suite in scope uses `tests/common`.

- Fix: one `tests/library/main.rs` with a module per current file and a
  shared `support` module (`run`, `run_bytes`, `expect_lib_error`, and a
  capturing-callbacks State, see 4.1).
- Process-wide state that would then be shared, checked:
  - `lua_std/string.rs`'s `GMATCH_WRAPPER` `OnceLock`. Order-safe:
    `is_gmatch_wrapper` before initialization correctly answers false.
  - crmath's `FORCED_FMA` and `KERNEL_BACKENDS`. Both are thread-local, and
    libtest runs each test on its own thread.
  - The panic hook in 3.3. That is the one hazard.
- Enforced by: `autotests = false` plus explicit `[[test]]` entries make
  a stray new top-level file a build error.

### 3.3 `golden::check_inputs` swaps the process-wide panic hook without a guard

It calls `take_hook`, installs a silent hook, runs `check_inputs_quietly`,
then calls `set_hook(previous)`. Two problems:

- If `check_inputs_quietly` panics (its `h.join().expect(...)` can), the
  silent hook is never restored. Every later test in that process then fails
  with no panic message.
- Under a plain `cargo test -- --include-ignored`, which uses parallel
  threads, two sweeps can interleave take and set. One of them then restores
  the silent hook permanently. `brokkr test` serializes with
  `--test-threads=1`, which hides the race.

The hook exists only to keep oracle panics from flooding the output, and
`catch_unwind` already isolates them.

- Fix: restore the hook in a drop guard, or install it once per process
  behind a `Once`. Better still, make `certify` return `Err` instead of
  panicking and remove the hook.
- `with_forced_fma` has the same shape (restore only on the success path).
  It is harmless because the state is thread-local and the thread dies with
  the panic. A drop guard costs nothing.

### 3.4 `string_bytes::gmatch_refused_push_preserves_the_next_match` never runs

It is `#[cfg(feature = "fault-injection")]`. No `[[check]]` enables that
feature, and the documented on-demand command
(`brokkr check --features fault-injection,snapshot -- --test error_boundary`)
selects only the `error_boundary` binary. It is the only fault-injection test
outside `error_boundary.rs`, so the only way to run it is an undocumented
whole-suite run with that feature.

- Fix: move it into `error_boundary.rs`, or change the documented command
  to run every binary.
- Enforced by: a convention check that `feature = "fault-injection"`
  appears only in files the documented command reaches.

### 3.5 Default FMA mode duplicates a forced mode

`fma_modes()` returns `Default`, `Software` and, when available,
`Hardware`. On an FMA-capable x86 host, `Default` takes the same path as
`Hardware`. On a host without FMA, it takes the same path as `Software`.
The golden test therefore evaluates every row three times when two would do.
That is a third of its cost, paid twice per check (3.1).

`kernel_dispatch_honors_forced_fma` skips `Default` altogether. Nothing
asserts that production selection picks hardware when it is available. A
broken `is_x86_feature_detected` path would silently run every kernel in
software: a performance loss, invisible to every test.

- Fix: drop `Default` from the golden loop, and assert in the dispatch
  test that `Default` records the hardware backend if and only if
  `hardware_fma` is `Some`.
- Enforced by: the test.

### 3.6 Identical-bytes rewrites

- `crmath_tables.py --generate` calls `write_bytes` on every table file
  unconditionally. Identical output still bumps mtimes, which rebuilds the
  whole crate in every profile.
- `regenerate_crmath_golden` rewrites the fixture unconditionally (2.1).
- Fix: write only if the bytes differ.
- Enforced by: no rule. It is a one-line change in each writer, and a
  `--generate` run followed by `git status` shows whether it is right.

### 3.7 Cap tests allocate at full production scale

`MAX_STRING_BYTES` (16 MiB) and `MAX_STACK_SIZE` (1,000,000 slots) are
consts with no per-State injection. So:

- `print_bounds_the_raw_joined_output_by_the_cap` builds 8 MiB strings twice
  plus a 16 MiB join;
- `lossy_host_conversion_is_size_checked` builds about 5.6 MiB of `0xff`,
  then another 5.6 MiB;
- the `string_metatable_dispatch` stack-cap tests `set_top(999_999)`.

All of this runs in the dev profile, twice per check. The limits are part of
the determinism contract, so per-State limits would need care. A
`#[doc(hidden)]` `State` builder that lowers the caps for tests, refused by
snapshot save, would make these tests cheap. Deciding whether that is worth a
new surface is a design choice, not a defect.

---

## 4. What the tests reach that they should have been handed

### 4.1 `State::new()` prints to the real stdout, past libtest's capture

`DefaultCallbacks::on_print` writes through `std::io::stdout().lock()`.
libtest captures only the `print!`/`eprint!` machinery, so a test that
runs Lua `print` with `State::new()` leaks into the runner's output.
`gsub_errors::gsub_string_replacement_inside_dynamic_argument` prints two
`[PASS] ...` lines on every run.

That test is also misfiled. It is a register-allocation regression (20
locals plus a dynamic argument) living in a gsub error suite.

- Fix: a shared test constructor that installs a recording `HostCallbacks`.
  The rule would be that no test outside the CLI suites uses
  `DefaultCallbacks`.
- Enforced by: `test_conventions.rs` could reject `State::new()` in
  files that call Lua `print`, but that is crude. A clearer rule is to make
  the shared support module the only place that constructs a State.

### 4.2 `research/core-math` and `python3` are reached directly

Every sweep, the regenerator, the corpus pin and the table check resolve
`CARGO_MANIFEST_DIR/research/core-math`. That is a gitignored sibling
checkout, which the tests assume exists at a fixed layout. They fail loudly
when it is missing, which is the right behaviour. `core_math_tables_match_pin`
also spawns `python3` from `PATH`.

What makes this worse is 2.1: the default test tool runs these tests on a
substring match. Vendoring the table sources (2.3) removes the Python and
checkout dependency from the table check. The worst-case corpus is several
million lines and should stay external. Its readers are already pinned by
count and digest, which is good.

### 4.3 Optional-argument behaviour is claimed per function and tested for two

`lua_std.rs` says: "An optional argument that is absent or nil takes its
default ... they are not shared code". Since each function reads its own
optionals, the claim needs coverage per function. Today it is tested for:

- `math.atan` and `math.log` (`stdlib_divergences`);
- `error(msg, nil)`;
- the `vm_aux` helper.

`string.sub` `j`, `find` `init`/`plain`, `match` and `gmatch`
`init`, `gsub` `n`, `concat` `sep`/`i`/`j`, `unpack` `i`/`j`,
`table.remove` `pos` and `tonumber` `base` have no explicit-nil test.

- Fix: a table-driven test with one row per (function, optional parameter),
  asserting `f(required..., nil) == f(required...)` in results and in cost.
  Better structurally: declare each library function's signature
  (required and optional parameters) in one table that both registration and
  the test read. A new optional parameter then cannot ship without a row.
- Enforced by: the test, or the declarative table.

### 4.4 Test-only shortcuts in production modules

These are `cfg(test)` and so unreachable from production. All are fine:

- `LuaPattern::matches_bytes`;
- `crmath::fma`;
- `hardware_fma`;
- the `tests::forced_fma()` consult inside `kernel_hardware_available`.

One consequence: the test build's kernel entry runs an extra thread-local
read that production does not. The golden test therefore exercises a slightly
different dispatch prologue from the shipped one. 2.13's release run would
cover the shipped one.

---

## 5. Where the tests assume they run, and what they touch

### 5.1 The floating-point environment is assumed, never detected

The library assumes round-to-nearest and no flush-to-zero or
denormals-are-zero:

- `reference/deterministic-math.md` names it the supported environment;
- `crmath`'s correctness depends on it;
- so do `numeral`'s subnormal parsing and every arithmetic opcode.

A game engine embedding dellingr is exactly the kind of host that sets
FTZ/DAZ in MXCSR, or changes the rounding mode, on its threads for speed. In
that case every subnormal-touching result, and possibly kernel outputs, would
differ between hosts with no error and no diagnostic: a silent replay break.
The tests run in the default environment, so they cannot see this.

- Fix: a cheap probe at `State` creation and at each `call` entry. For
  example, `black_box(f64::MIN_POSITIVE) / black_box(2.0) != 0.0` detects
  FTZ, and a subnormal-times-one probe detects DAZ. Return an error, or
  panic in debug.
- Test it with a unit test that sets MXCSR FTZ on x86 and expects the
  refusal.
- Enforced by: the probe plus that test.

### 5.2 Writes outside owned locations

- `regenerate_crmath_golden` writes `tests/fixtures/crmath_golden.txt`
  in place, from inside the test harness (2.1, 2.2).
- `crmath_tables.py --generate` writes `src/crmath/tables/*.rs` in place.
  That is intended and owned, but see 3.6.
- Neither writes into `target/`, `.brokkr/` or another tool's directory.
  No finding there.
- Output should go to a `.new` sibling, for the golden file, that a human
  promotes, or behind an explicit feature (2.1).

### 5.3 Host CPU features silently change coverage

On an x86 host without FMA, or any host other than x86 or aarch64,
`fma_modes()` drops `Hardware`, and `fma::check` skips the hardware
comparison. The tests pass with a smaller scope and report nothing. A run on
an old CI box would read as full coverage.

- Fix: print the modes exercised once (as the
  `golden_results_on_every_fma_path` failure message already does), or
  require an explicit acknowledgement such as a skipped-test marker in the
  output.
- Enforced by: not really. This is information, not a gate.

### 5.4 `%` and `math.fmod` lower to the platform's `fmod`

`docs/determinism.md` states this correctly. IEEE `fmod` is exact, so
correct libms agree. There is still no golden of remainder edge cases (signed
zeros, subnormal divisors, huge quotients) to show it on a given host. The
symbol check in 2.9 would at least record that `fmod` is the one permitted
libm import.

### 5.5 Locale and filesystem

- The pattern classes and `numeral` whitespace are fixed ASCII, by
  deliberate design. `diff_test.sh` pins `LC_ALL=C` for the references.
  That is correct, and no finding.
- `crmath_tables.py`'s mismatch path calls `destination.read_text()` with
  no encoding, so it uses the locale's default. The tables are ASCII, so this
  is harmless today. `encoding="utf-8"` would make it explicit.

---

## Lateral findings

- **Three hex-float parsers, none checked against another.** They are
  `crmath::hf` (const; rejects inexact literals), `numeral::parse_hex`
  (runtime; rounds) and `oracle::parse_hex_float` (test; exact through
  libbeef, then rounded). `numeral::parse_hex` is tested only on about 30
  hand-picked cases. A cheap, strong addition is a randomized differential
  test in the lib test binary, comparing `parse_lua_numeral` against
  `parse_hex_float` over random mantissas and exponents, including long
  digit strings and subnormal boundaries.
- **`%a` round-trip is tested for six values.**
  `quoted_floats_round_trip_through_the_parser` re-parses `%q` output for
  six numbers. A unit test running `format_hex_float` then
  `parse_lua_numeral` over the same 16k bit-class sweep
  `number_rendering.rs` uses, plus all precisions `0..=13`, would cover
  the formatter's rounding branch.
- **11 copies of dispatch plumbing.** Each kernel repeats the same
  `cfg`-gated dispatch function, about 20 lines with
  `record_kernel_backend` and the `unsafe` hardware call, around its
  `kernel!` instantiations. One `dispatch!` macro would remove the 11 copies
  that 2.13 says are partly never compiled.
- **Misfiled tests.** Each of these sits in a stdlib or string file but
  tests something else:
  - in `string_format.rs`:
    `script_error_has_source_line_and_state_remains_reusable`, plus
    `tostring_metamethod_must_return_a_string_like_value`, which also
    duplicates a case already in
    `tostring_and_print_reject_non_string_metamethod_results`;
  - in `stdlib_argument_errors.rs`: `runtime_result_limit_*`,
    `runtime_argument_limit_*`, `host_argument_checks_*` and
    `nested_native_dispatch_*`;
  - the gsub register-allocation test in 4.1.

  Merging the suites (3.2) is the moment to file them where they belong.
- **Duplicate `tostring(-0)` coverage.**
  `number_rendering::tostring_preserves_lua_negative_zero_spelling`
  duplicates the `(-0.0, "-0")` row of `number_format_g14`. The two files
  cover one function and could be one module.
