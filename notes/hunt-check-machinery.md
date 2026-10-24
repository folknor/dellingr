# Verification hunt: check-machinery

Scope: `brokkr.toml`, `Cargo.toml` (`[lints]`, `[package]`, profiles,
features), `clippy.toml`, `.gitignore`, `.review.toml`, `scripts/`,
`research/bench_luars.sh`, `examples/parse/generate.py`,
`tests/test_conventions.rs`, plus whatever those lean on (`tests/diff_test.rs`,
`tests/run_examples.rs`, `tests/bench_scripts.rs`, `tests/common/`,
`tests/native_stack.rs`, `reference/native-calls.*`, `docs/limits.md`, the
brokkr manual). Evidence from this tree at 06959fb on this host, including
the brokkr run logs under `.brokkr/check-logs/`.

The findings are grouped by the five questions in the brief. They are not
ranked. Each one says whether its fixed version can be held mechanically.

Nothing in scope turned up a test file that the build never compiles: every
`tests/*.rs` is an autotest, `tests/common/*` is pulled in by `#[path]`, and
every file under `src/` is reached by a `mod` declaration (`snapshot_format.rs`
through `#[path]` in `src/instr.rs`). No test writes environment variables or
installs a global logger.

---

## 1. Tests that prove nothing

### 1.1 `large_source` fingerprint pair compares a file with itself

`tests/bench_scripts.rs::registered_workload_pairs_keep_kernel_cost_fingerprints_aligned`
loads `file` and `hotpath_file` for every workload and asserts that their cost
fingerprints are equal. For `[dellingr.workloads.large_source]` both keys name
`examples/parse/large_source.lua`, so both sides of the assertion come from the
same bytes and it cannot fail. It reads as one more covered pair.

Fix: make the exception explicit. The test should skip a pair whose two paths
are equal only when the workload is on a named allowlist (today just
`large_source`), and fail for any other equal pair.

Enforceable: yes, in the same test.

### 1.2 The final assertion in `test_conventions.rs` checks for strings, and the helper it blesses breaks its own rule

The last block of `cli_paths_and_test_file_mutations_stay_in_common_helpers`
asserts that `tests/common/scratch.rs` *contains* the text
`CARGO_TARGET_TMPDIR` and `.starts_with(scratch_root)`. A comment carrying those
two strings would satisfy it. The guard it means to prove is that test file
mutations stay under Cargo's temporary directory. That is already false for the
file it exempts: `scratch.rs::write_boundary_transcript` does
`fs::create_dir_all` and `fs::write` into
`$CARGO_MANIFEST_DIR/.scratch/error-boundary/`. Its doc comment admits this,
but the test's failure message ("test file mutations must be restricted to
Cargo's temporary test directory") states otherwise.

Fix: test the behaviour. Call `write_scratch_file` with a path outside the
root and assert `PermissionDenied`. Move the transcript writer out of the
module whose contract it breaks, or give it its own named exemption.

Enforceable: yes, as a unit test of the guard instead of a grep.

### 1.3 The mutation markers in `test_conventions.rs` both miss and over-reach

The markers it misses are `fs::remove_dir_all(`, `fs::rename(`, `fs::copy(`,
`File::options(`, `fs::hard_link(`, `fs::set_permissions(`,
`std::os::unix::fs::symlink(` and an imported bare `write(` (after
`use std::fs::write`). Any of these in `tests/` passes. `fs::remove_dir(` does
not match `fs::remove_dir_all(`.

Its scan stops at `tests/`. `src/crmath/tests/golden.rs` (a `#[cfg(test)]`
module) does `std::fs::write(GOLDEN_PATH, ...)` into the tracked
`tests/fixtures/crmath_golden.txt` and spawns `python3`. It sits outside the
convention's reach. The AGENTS.md rule "no test may reference `target/` or
spawn cargo" has no check at all.

It also over-reaches. `write_all(` is banned everywhere outside `scratch.rs`,
including writes into a child's stdin pipe. `tests/cli_arguments.rs::run_with_stdin`
therefore writes its stdin bytes to a scratch *file* and opens that file as
stdin, rather than piping them in memory. The lexical rule caused the very
filesystem writes it was meant to prevent.

Fix: move this to brokkr `[[textlint]]` rules with `test_code = "only"` over
`src/**/*.rs` and `tests/**/*.rs` (brokkr's classifier finds test code inside
`src/`), with `region = "code"`. Use narrower patterns: real filesystem
mutators, plus `Command::new("cargo")` and `"target/` in `region = "string"`.
Textlint runs pre-clippy, costs no compile and fails fast, where this test is
compiled and run once per sweep.

Enforceable: yes, as textlint rules with `must_scan` sentinels.

### 1.4 What `run_examples.rs` accepts as a pass

An example passes when it exits 0, writes no stderr and prints no line ending
in `: false`. A `_bench` file must also print some line ending in `: true`.
Under that rule:

- A condition that evaluates to `nil` prints `name: nil` and passes.
- A non-bench example that prints nothing passes.
- For the 11 marked files (5 `SKIP-DIFF`, 6 `DIFF`), nothing else ever checks
  their output. The differential gate checks only that a `DIFF` file exits 0
  (`scripts/diff_test.sh`, the `has_diff` branch), which repeats
  `run_examples` with a weaker condition.

The marker works per file, so one divergence removes every other assertion in
that file from differential coverage. `examples/math.lua` is excluded for
number formatting and RNG, `feature_test.lua` for a global `unpack`,
`feature_test_extended.lua` for semicolons, and `edge_cases.lua` for `-nan`.

Fix: see 3.6. Committed expected output with line-level divergence notes
would keep the rest of each file under comparison. Until then, `run_examples`
could require every non-blank stdout line of a `name: value` example to end in
`: true`.

Enforceable: yes, in the runner.

### 1.5 `bench/` scripts are compiled, never run

`bench_scripts_parse_and_follow_the_standalone_footer_contract` compiles and
`load`s each `bench/*.lua` but never calls into it. `run_examples`,
`diff_test` and `bench.sh` deliberately do not see `bench/`. A verdict file
that raises at runtime (a misspelled global, a helper that drifted from its
`examples/` twin outside the fingerprinted scopes) passes the gate. It is
found only when someone runs `brokkr dellingr --lua <W> --bench`, which may be
weeks later and on someone else's change.

Fix: in the test, call each file's `kernel()` once (or `_bench` with the
repeat count patched to 1) on a budget-limited State.

Enforceable: yes, as a test.

### 1.6 The malformed-timeout test can become a nested release build

`tests/diff_test.rs::malformed_timeout_is_rejected_once_before_other_checks`
runs `diff_test.sh --timeout "5 s"` without `--bin`. If the regex validation
ever regressed, the script would go on to
`dellingr_bin_built release` (`cargo build --release`, fat LTO,
`codegen-units = 1`) inside a test under brokkr's lock and 20s cap. The
failure would show up as a per-test timeout and a stray cargo, not as a clean
assertion.

Fix: pass `--bin` with `common::dellingr_bin()`, or with a path that does not
exist. Validation still runs first.

Enforceable: yes. A textlint rule can require `--bin` on every
`diff_test.sh` invocation in `tests/`.

### 1.7 `native_calls.py` counts commented-out scenarios, and edge coverage is only declared

`scan_test_lists` finds the `const SCENARIOS/CHAINS/LEAVES` list bodies in the
sanitized code. It then runs `re.findall(r'\bname:\s*"..."')` over the *raw*
text (`body = text[m.end():end]`). A scenario commented out in
`tests/native_stack.rs` still counts as present, so the inventory check stays
green while the test no longer runs the path. Any `name: "..."` field nested
inside an entry (say an inline struct) would also be read as a scenario.

More structurally, an edge counts as "covered" when the inventory says so: it
lies on a declared scenario path, or it names a chain or leaf that exists by
name. Nothing observes that `every_limit_at_once_fits_the_limit_test_stack`
actually traverses the marked call site. `reference/native-calls.md` presents
the script as keeping the inventory, markers and test "in agreement". That
agreement is between declarations, not executions.

Fix: read the names from the sanitized code. For real coverage, record edge
ids at runtime. A `cfg(any(test, feature = "fault-injection"))` counter keyed
by edge id at each marked site, which `native_stack.rs` asserts is non-zero for
every non-excluded edge, would turn the declarative claim into an observed one.

Enforceable: the first part yes (script). The second needs a test-only hook
in `src/`, then a test.

---

## 2. Guards and claims that have stopped holding

### 2.1 `clippy.toml` entries that seal nothing, and no fixture proving any of them

- `allow-invalid = true` on `rand::rng`, `rand::thread_rng` and the four
  `foldhash::{fast,quality}::{RandomState,SeedableRandomState}` entries. With
  `rand` not a dependency, the first two cannot resolve and seal nothing. A
  re-introduced `rand` would come in through `rand::random`,
  `SeedableRng::from_os_rng`/`from_entropy` or `getrandom`, none of which is
  listed. Whether the foldhash paths resolve depends on foldhash's features,
  which an embedding host can change through unification. `allow-invalid`
  hides both outcomes.
- brokkr has a `[[fixture_lint]]` phase built for exactly this case ("Those
  lints fail open: a configured path that no longer resolves ... seals
  nothing", `brokkr man conventions fixture_lint`). dellingr does not use it
  (the check log says `fixture-lint: not applicable`). No entry in
  `clippy.toml` is proven to fire, including the determinism-critical
  `f64::sin` family, `Instant::now` and `HashMap`.
- Host state that is not banned: `std::env::current_dir`, `temp_dir`,
  `home_dir`, `std::process::id`, `std::thread::current().id()`,
  `std::thread::available_parallelism`, and pointer-address values
  (`<*const T>::addr`, `expose_provenance`). I found no use of these in `src/`
  today, so this is a missing guard, not a live bug.
- `Result::unwrap_or` and `unwrap_or_default` are banned as error-discarding,
  but `Result::unwrap_or_else(|_| ..)`, `map_or` and `map_or_else` discard the
  error the same way and are allowed.
- `[lints.clippy]` sets `disallowed_methods = "deny"` but not
  `disallowed_types`, which defaults to warn. Inside brokkr every diagnostic
  fails, so the gate is unaffected. A plain `cargo build`, rust-analyzer, or a
  consumer building this crate sees a banned `HashMap` as a warning and a
  banned `sin` as an error.

Fix: add a `[[fixture_lint]]` crate that calls every banned path, with an
`expect.toml`. Drop `allow-invalid` wherever the fixture proves the path
resolves. Add the missing host-state paths. Make `disallowed_types = "deny"`.

Enforceable: yes. This is what `fixture_lint` exists for.

### 2.2 `native_calls.py` skips files by name

`production_sources` drops any `src/` file whose path matches
`(^|/)(tests?|[a-z_]*_tests)(\.rs|/)`, without checking that its `mod`
declaration is `#[cfg(test)]`. Today every such file is
(`vm/tests.rs`, `compiler/parser/tests.rs`, `line_attribution_tests.rs`,
`save_state/integrity_tests.rs`, `crmath/tests/`). A production module that
happened to be named `test.rs` or `foo_tests.rs` would be silently unscanned,
and its calls into Lua would need no markers.

Fix: for every file the name rule skips, find the `mod` item that declares it
and require a `cfg(test)` or `cfg(all(test, ..))` attribute on it, or report
it.

Enforceable: yes, in the script.

### 2.3 Calls through pointers and trait objects are a reviewer-only rule

`reference/native-calls.md`: "A new indirect call that can reach Lua needs this
guard extended before it lands." Nothing detects a new indirect path. Storing
`State::call_unguarded` in a variable, passing an inventoried function as a
`fn` value, or a new `dyn` trait whose implementation re-enters: none of these
is a lexical call, so neither the script nor the textlint rule sees it.

Fix (partial): a textlint rule flagging any inventoried name used as a value,
meaning followed by neither `(` nor `::<`, with the same generated alternation.
`native_calls.py` can verify that rule's pattern as it already does for
`native-call-marker`. Trait-object paths stay a review item.

Enforceable: partly.

### 2.4 The `#[hotpath::measure]` placement rule is already broken, and the inventory could enforce it

AGENTS.md: "Don't add `#[hotpath::measure]` to a function that stays live
across a native re-entry (`call_unguarded`, ..., and the
metamethod/iterator/library helpers between the dispatch loop and their
`call_unguarded`)." Three functions in the native-call inventory, all in the
textlint alternation, carry the attribute:

- `instr_tfor_call` (`src/vm/eval_control.rs`), the generic-for iterator call.
- `table_sort` (`src/vm/table_ops.rs`), which calls comparators.
- `to_string_with_meta` (`src/vm/table_ops.rs`), which reaches `__tostring`
  through `tostring_bytes`.

The impact is limited to instrumented builds. `docs/limits.md` puts them
outside the envelope, and no gate runs `native_stack` under `hotpath` (see
3.9). Still, the rule exists, is stated as a rule, and is false today.
`native_calls.py` already computes the exact set of functions that stay live
across re-entry.

Fix: in `native_calls.py`, report any inventoried function, any boundary, and
`run_invocation`/`Frame::run` whose attribute block contains
`hotpath::measure`.

Enforceable: yes, in the script, at no extra cost.

### 2.5 `bench_scripts.rs` reads `brokkr.toml` with a hand-written parser

`workload_pairs` recognises a workload only from a line that is exactly
`[dellingr.workloads.NAME]` and a `key = "value"` pair. Each of these is valid
TOML and silently drops the workload from the fingerprint check:

- a trailing comment on the header,
- spaces inside the brackets,
- a quoted name,
- dotted keys under `[dellingr]`,
- a single-quoted literal string.

The only failure the test catches is an empty registry. The repository also
has two TOML readers for this one file: `tomllib` in `scripts/lib.sh` and
`native_calls.py`, and this one in Rust.

The test also does not check that every `bench/*.lua` is registered. Today
24 bench files map onto 25 workloads (`large_source` has none), but an orphan
verdict file would pass.

Fix: add a TOML dev-dependency and parse properly. Assert that the set of
`file` values equals `bench/*.lua` plus the named exceptions.

Enforceable: yes, as a test.

### 2.6 Workload pins are checked only when someone benches

brokkr verifies `xxh128`/`hotpath_xxh128` at `brokkr dellingr` preflight
(`brokkr man dellingr`). Nothing in `brokkr check` does. An edit to
`bench/*.lua` or to a pinned `examples/` file that forgets to re-pin passes
the gate, and the next person to bench gets the refusal. The re-registration
is meant to be "the deliberate signal". Today it is a deferred one.

A second effect: the pin covers comments. `examples/globals/write.lua`'s
header describes `globals_version`, an uncached `SET_GLOBAL`, UTF-8
validation and a fresh `String` per write. `globals_version` no longer exists
in `src/`, and `tests/global_write_cache.rs` tests a global write cache. The
comment is stale. Correcting it re-pins the workload and so cuts its history,
which is exactly why such comments stay wrong.

Fix: verify the pins in the gate (an `xxhash-rust` dev-dependency in
`bench_scripts.rs`). Consider pinning a semantic fingerprint instead (the
`analyze_cost` tree the test already computes, or the bytecode listing) so
that comment edits do not cut history. That is a brokkr-side question.

Enforceable: the pin check yes. The pin's definition is a brokkr design call.

### 2.7 `Cargo.toml` `exclude` versus what the packaged tests read

The comment says `tests/` and `examples/` stay in the package because they
carry the corpus that `run_examples.rs` and the diff gate execute. Yet the
package excludes `scripts/`, which holds `diff_test.sh` (driven by
`tests/diff_test.rs`) and `test_limited.sh` (driven by
`tests/common/cli.rs::limited_command`, used by `run_examples.rs`). It also
excludes `bench/` and `brokkr.toml`, both read by `tests/bench_scripts.rs`.
`cargo test` on the published crate therefore fails in at least three
binaries. `cargo package` verifies only that the crate builds, so nothing
notices.

Fix: either publish a coherent test surface (an `include` allowlist naming
what the tests read) or none. Excluding `tests/` and the `.lua` corpus keeps
`examples/hotpath.rs` and `regenerate_save_golden.rs`, which are all the
declared `[[example]]` targets need.

Enforceable: yes. A background `script_check` can run `cargo package --list`
and assert that every path the tests open is in it, or that `tests/` is not.

### 2.8 The CORE-MATH pin is duplicated, and the tables' "do not edit by hand" is unenforced

`CORE_MATH_COMMIT` is written twice: in `scripts/crmath_tables.py` and in
`src/crmath/tests/golden.rs`. Nothing checks that the two agree. No file
records where `research/core-math` comes from: there is no clone URL, only
`research/*/` in `.gitignore`. `crmath_tables.py --check` runs only via the
ignored test `core_math_tables_match_pin`, which needs that local checkout. So
the gate never checks the `src/crmath/tables/*.rs` header "Generated by
`scripts/crmath_tables.py`; do not edit by hand".

Fix: single-source the pin, with the URL, in a small TOML that both read.
Record each generated table file's digest in that TOML and check the digests
in a pre-clippy `script_check`. That needs no checkout and catches hand
edits. Regeneration still needs the checkout.

Enforceable: yes.

### 2.9 The `[gremlins] exclude` entry is a file, but the manual describes directories

`brokkr.toml` has `exclude = ["tests/string_bytes.rs"]`. The manual: "Entries
are project-root-relative directories matched by path prefix". Whether a file
path matches is behaviour the manual does not promise. The file's non-ASCII
content is six lines: a literal U+26A0 in Lua source and a `"\u{FFFD}a"` expectation.
The whole-file exemption would also hide any real gremlin added to that file
later.

Fix: write the Lua as `"\xE2\x9A\xA0"` and the Rust as `"\u{FFFD}a"`, then
drop the exclusion. `brokkr check --audit` reports an exclusion that excuses
nothing. I did not run it.

Enforceable: yes, by deleting the exemption.

### 2.10 Lists in AGENTS.md and CLAUDE.md that the tree generates, and other claims that have drifted

- **Example directories.** The comment for `examples/alloc/` (`closure,
  short_tables, record_tables, gc_churn`) omits `list_literals`, a registered
  workload. The comment for `examples/tables/` omits `metamethod_sequence`.
- **Probe list.** AGENTS.md names `numerics/constants`, `globals/write`,
  `calls/many_literals`, `calls/loop_beside_closure`, `alloc/gc_churn` and
  `parse/large_source` as the probes, and says each "carries a header comment
  naming the candidate". The file headers disagree in both directions:
  - `calls/method_chain`, `calls/method_polymorphic`, `fields/same_obj_read`
    and `fields/same_obj_write` all open with `-- Probe:` and are not listed.
  - `globals/write` opens with `-- Hypothesis:`, and that header is stale
    (2.6).
  - `calls/many_literals` calls itself a "Historical probe".
- **Target paths.** "no script hard-codes a `target/` path" is false for
  `research/bench_luars.sh` (`DELLINGR="target/release/dellingr"`).
- **The 60-second timeout.** AGENTS.md says `run_examples` runs each example
  "with a fixed 60-second timeout". Under brokkr that timeout can never fire
  (3.5).

Fix: replace the lists with a pointer to their source of truth (`brokkr.toml`
for workloads, `ls examples/`). If the probe classification is wanted, make
the header the source: a test can require every `hotpath_file` to open with
either `-- Probe:` or `-- Workload:` and print the list.

Enforceable: the header rule yes, as a test. The prose cannot be enforced, so
stop enumerating.

### 2.11 The benchmarking tools the docs forbid and the docs prescribe

The CLAUDE.md benchmarking rules: "Never hyperfine, `scripts/bench.sh`,
`scripts/hotbench.sh`". AGENTS.md documents both scripts as the tools, and
`brokkr man dellingr` says comparison against reference Lua is "not part of
this surface - `scripts/bench.sh` in the dellingr repo owns that". The README
ratio table and the `lua-rs` column (via `research/bench_luars.sh`) can
therefore only be produced by a tool the rules forbid.

`hotbench.sh` is fully superseded by `brokkr dellingr --lua W --hotpath`. Its
hyperfine half times the ms-scale `hotpath_file`, which the same rules say is
below launch noise. `scripts/diff_gate.sh` is invoked by nothing, and
`brokkr test differential_examples --debug` covers its job.

Fix: delete `hotbench.sh` and `diff_gate.sh`. Decide whether vs-Lua ratios are
a measurement the project still publishes. If they are, the rule needs an
explicit carve-out, and the tool needs the same provenance discipline
(hostname, commit) as `brokkr results`.

Enforceable: partly. A deleted script cannot drift.

### 2.12 `stack.rs` tells the profiles apart with `cfg!(debug_assertions)`

`tests/common/stack.rs` picks 4 MiB or 2 MiB using `cfg!(debug_assertions)`
and calls that "not a definition of unoptimized". The brokkr manual calls the
heuristic unreliable under profile overrides. Nothing ties it to
`Cargo.toml`. A later `[profile.release] debug-assertions = true` (a common
hardening move) would make the release sweeps test the 4 MiB envelope and
still report "release".

Fix: assert the profile mapping. The release sweeps can export a marker
through `[[check]] env`, and the test can refuse when the marker and
`cfg!(debug_assertions)` disagree. Alternatively, `BROKKR_TEST_BIN_DIR` ends
in `/release` for those sweeps and the test can compare against that.

Enforceable: yes.

### 2.13 The envelope is stated for x86-64 Linux, and the test does not say where it runs

`docs/limits.md` says how `tests/native_stack.rs` validates the envelope "on
x86-64 Linux". The test runs on any target and passes or fails without saying
that the result is outside the validated scope.

Fix: on other targets, print the arch and fail or skip.

Enforceable: yes, minor.

---

## 3. The test suite as a system

### 3.1 `brokkr test <NAME>` builds the whole suite in release twice

Evidence: `.brokkr/check-logs/test-001792800558168.log`
(`brokkr test a_site_holding_two_classes`). It shows four sweeps. For
`release-native-stack` and `release-native-stack-snapshot` it built
`--release -p dellingr --tests`, all 80 harnesses rather than only
`native_stack`, in 54.4s and 1m03s. It then ran the 250ms test four times.
`brokkr test` drops the entry-level `tests = ["native_stack"]` selection and
honours the entry's `profile = "release"` over `[test] debug = true`. Every
focused run on a cold release cache pays about two minutes of fat-LTO,
`codegen-units = 1` builds for a test that has nothing to do with native
stack.

Fix: put the release sweeps in a profile `brokkr test` does not walk. Set
`[test] default_profile` to a profile referencing only `default` and
`snapshot`. Then either a `gate_profile` (lanes including the release
entries) for `brokkr check --gate`, or a `release-native` profile run
explicitly. Whether plain `brokkr check` should still include the release
sweeps is a policy call. If it should, a lanes profile can keep them there
while `brokkr test` stays dev-only. As a separate question, whether the
release native-stack check needs fat LTO and `codegen-units = 1` at all is
answered by `docs/limits.md`: it states the envelope for "this repository's
release build settings", so yes, but only because those are the settings it
names.

Enforceable: yes, in config.

### 3.2 The `default` and `snapshot` sweeps share one `CARGO_BIN_EXE_dellingr` path

Both check logs show, for every sweep on every run: "prepare: sweep 'default':
rebuilt before listing - /media/folk/Banan/cargo/debug/dellingr has other
content", and the same for `snapshot`. Each feature shape's
`cargo test --no-run` re-uplifts `target/debug/dellingr`, so the binary that
`CARGO_BIN_EXE_dellingr` names belongs to whichever shape built last.

`tests/diff_test.rs`'s claim that "the corpus is checked against the CLI build
of that shape" holds only because brokkr rebuilds right before each sweep.
Under plain `cargo test`, or two runs in parallel, a `snapshot` test can drive
the default CLI. The same happens under `release` between the two release
sweeps (`.../release/dellingr has other content`).

Fix: give the `snapshot` sweeps their own target dir
(`[[check]] env = { CARGO_TARGET_DIR = ... }`; the manual forbids it only
alongside `rustflags`). The uplifted paths then stop colliding, and the
re-uplift on every run disappears. This costs disk.

Enforceable: yes, in config.

### 3.3 80 test binaries, each linking the crate, in four shapes

`tests/` holds about 80 integration crates. Each links `dellingr` in each
sweep, which is the dominant per-change cost of the test phase's prepare.
That prepare took 37.2s after a code change, against 5.8s warm. A focused
`brokkr test` lists all 80 harnesses per sweep. Separate crates are also why
`tests/common/*` is pulled in by `#[path]` with `#[allow(dead_code)]` on
helpers (`cli.rs`, `stack.rs`, `scratch.rs::write_boundary_transcript`). Those
allows also hide a helper that has become dead in every crate.

On process-global state, a merge is safe today. The only process-global test
state is `panic_poison.rs`'s `Once`-guarded panic hook, which silences only
its own message, and `scratch.rs`'s atomic counter.

Fix: merge into one integration harness (`tests/integration/main.rs` with
`mod` per current file), or a few grouped by need. Keep a process-level
separation only where a test genuinely needs its own process.

Enforceable: the safety of the merge is. A textlint rule over `tests/**` can
ban `set_var`/`remove_var`/`set_hook` outside one named fixture module, so a
later test cannot quietly make the merged binary order-dependent.

### 3.4 Source-level checks that compile and run as integration tests, twice per check

The following inspect files only, yet they are compiled and run once per
sweep: `test_conventions.rs`,
`diff_test.rs::diff_markers_have_nonempty_reasons`,
`diff_test.rs::shell_and_rust_example_corpora_match`, and
`diff_test.rs::malformed_timeout_is_rejected...`. They also run under the
release sweeps whenever those widen (3.1). They fail late, after clippy and a
full build.

Fix: textlint rules, or one pre-clippy `script_check`, for the marker-reason
rule and the conventions. The corpus-equivalence test exists only because two
walkers exist (3.6).

Enforceable: yes.

### 3.5 Whole-corpus tests under a 20-second per-test cap

brokkr gives each test 20s and stops the whole run on a breach
(`brokkr man hygiene`). Two consequences follow.

`run_all_examples` runs all 82 examples inside one test, yet sets
`EXAMPLE_TIMEOUT_MS = 60_000` per example. That timeout can never fire under
brokkr. A hang is reported as a per-test timeout of `run_all_examples`, with
the example lost from the message that the 60s path was written to produce.

`differential_examples` runs seven `diff_test.sh` partitions concurrently
inside one test, and they share the same 20s. The workers scale with
`available_parallelism().min(8)`, while the seven partitions are fixed. On a
host with fewer or slower cores, either test crosses the cap and the run
stops. That is a host-dependent refusal that reads as a hang.

When brokkr kills `differential_examples`, the SIGKILL skips `diff_test.sh`'s
`trap ... EXIT`. `.scratch/` holds 18 leaked files right now: six sets of
`dellingr-diff-{our,lua52,lua54}.*`.

Fix: one test per example would give exact attribution and a per-example cap.
brokkr's `harness = "nextest"` sweep runs process-per-test concurrently, so
the hand-built parallelism inside a single test (worker pools, seven bash
partitions) is no longer needed. Short of that, derive the per-example
timeouts from a constant below 20s, and stop writing temp files (3.6).

Enforceable: a constant shared by both tests and asserted below the cap is a
test.

### 3.6 Each sweep runs the corpus three times and recomputes reference output

Per sweep:

- `run_examples` executes all 82 examples under `bash test_limited.sh`.
- `differential_examples` executes them again under `bash diff_test.sh`, with
  roughly ten processes per example: `grep` three times, `timeout`, dellingr,
  `lua5.2`, `lua5.4`, and `diff` once or twice.

That is 71 unmarked files times two references, in both dev sweeps: about 284
reference-Lua runs per `brokkr check`, producing the same bytes every time,
because reference output does not depend on dellingr's build.

Fix (the larger rewrite): commit expected output per example (for example
`examples/**/*.lua.out`, with a line-level way to record a deliberate
divergence and its reason). Generate it with an explicit refresh script that
runs `lua5.2`/`lua5.4` and records their versions. Then:

- the gate compares dellingr's output in Rust, in-process, in the same pass
  that applies `run_examples`' rules, so the corpus runs once per sweep;
- `lua5.2`/`lua5.4`, bash, GNU `timeout`, `grep`, `diff`, `sed` and
  `mktemp` stop being gate host requirements;
- the shell/Rust corpus-equivalence test, the temp files and their leak, and
  `diff_test.sh`'s gate role all disappear;
- reference drift becomes an on-demand audit (an ignored test, or the refresh
  script in check mode) instead of an ambient dependency of every run.

Enforceable: yes. The refresh script's check mode, and a test that every
example has expected output.

### 3.7 `brokkr test` with `--include-ignored` runs a fixture regenerator

`brokkr test` always passes `--include-ignored` and matches by substring.
`src/crmath/tests/golden.rs::regenerate_crmath_golden` is
`#[ignore = "rewrites tests/fixtures/crmath_golden.txt"]` and writes the
tracked fixture. Any focused run whose name matches it rewrites the golden it
is meant to verify: `brokkr test golden`, `regenerate`, `crmath`.
`golden_results_on_every_fma_path` and the `save_golden` tests share the
substring, so `brokkr test golden` is a natural thing to type. The same
substring pulls in the long `core_math_*` sweeps and `random_against_oracle`
("long-running"), which hit the 20s cap and stop the run.

The save golden already uses the safe pattern, a `regenerate_save_golden`
example.

Fix: turn the crmath regenerator into an example or bin as well. Give the
long sweeps names that share no substrings with the fast tests.

Enforceable: yes. A textlint rule with `test_code = "only"` banning
`fs::write`/`File::create` into the manifest dir in test code would have
caught it.

### 3.8 Doctests never run, and no rustdoc phase

`[test] doctests` is unset, which brokkr defaults to `false`. The check log
shows every sweep scoped to `--tests`. There are five doc examples: the crate
docs in `src/lib.rs` and `src/host.rs`, `Program`'s in `src/lib.rs`, and two
in `src/vm.rs` (`with_callbacks`, `set_user_data`). They are the
consumer-facing API surface, and none is compiled. There is no `[rustdoc]`
table either, so broken intra-doc links go unnoticed.

Fix: `doctests = true` (dev sweeps only), or a `doc_only` entry, plus a
`[rustdoc]` table.

Enforceable: yes, in config.

### 3.9 Three features no gate compiles

`hotpath`, `hotpath-alloc` and `fault-injection` appear in no `[[check]]`.
Several things go uncompiled as a result:

- the `cfg(feature = "hotpath")` branches in `tests/common/stack.rs` and
  `tests/diff_test.rs`,
- every `#[hotpath::measure]` expansion,
- the fault-injection hooks.

AGENTS.md asks a human to remember `brokkr check --features hotpath`. A bad
annotation surfaces on the next `--hotpath` run. A fault-injection
regression surfaces on the next on-demand sweep.

Fix: a `background` `script_check` that runs
`cargo clippy --all-targets --features hotpath,hotpath-alloc,fault-injection`
into its own target dir. The manual endorses this for "one that drives its own
cargo". The alternative is a `curated` `[[check]]` entry with those features
and a narrow `only` filter.

Enforceable: yes, in config.

### 3.10 The convention phase costs about 9 seconds per check, and the script grows superlinearly

Both check logs report "gremlins (219 files), textlint (1 rule, 79 files),
script-check (2 checks), publish cycle: ok in 9.1s". That compares with 406ms
for warm clippy and 1m10s for the whole warm check. I did not measure how the
9.1s splits. `native_calls.py` is the main suspect, because of these shapes:

- `stray_excluded` calls `textlint_excludes()`, which re-reads and re-parses
  `brokkr.toml` for each of about 100 marked sites, and again for each stray
  candidate.
- `attach_markers` does `s.code.split("\n")` once per comment, which is
  quadratic in file size.
- `textlint_strays` re-splits the file and sums line lengths for each
  matching line, also quadratic. The alternation includes `call`, `run`,
  `format` and `get_table`, so a large file has many matching lines.
- `closure()` rescans every call token in `src/` for each reached key.
- `enclosing()` and `in_test()` are linear scans per call.

`src/vm/eval.rs`, `frame.rs` and `table_ops.rs` grow with the VM, so this cost
rises faster than the tree. Separately, `generated-large-source` runs at
`stage = "post-test"`, although it is a pure source comparison. It is skipped
whenever any test fails, and it reports last.

Fix: index once per file (line starts, function spans sorted for bisection,
call tokens grouped by name, parsed config passed in). Move the generated-file
check to `pre-clippy`.

Enforceable: a script check's wall time is visible in the run log. No ceiling
other than brokkr's 5-minute `script_check` limit applies.

### 3.11 Generators rewrite identical bytes

These rewrite files whose content has not changed:

- `scripts/crmath_tables.py --generate` calls `destination.write_bytes(output)`
  for every table file unconditionally. A no-op regeneration touches eleven
  `src/` files, so cargo recompiles the crate and every test binary in every
  shape.
- `examples/parse/generate.py` always rewrites `large_source.lua`.
- `scripts/notes_move.py` rewrites every source file it was given, including
  ones it took nothing from. It also normalises their trailing newlines. If
  `dest` is also one of the sources, the second write overwrites the first and
  loses the moved sections.

Fix: write only when the bytes differ, using one shared helper. In
`notes_move.py`, refuse `dest in sources`.

Enforceable: partly. A textlint rule over `scripts/**` and `examples/**/*.py`
can ban `.write_text(`/`.write_bytes(`/`open(..., "w")` outside the helper.

---

## 4. What the tests reach that they should be handed

### 4.1 The CLI binary

`tests/common/mod.rs` hands every test `CARGO_BIN_EXE_dellingr`, and
`test_conventions.rs` keeps that path in one place. This is the right shape.
Its weak point is 3.2: the path is not specific to the feature shape.

The scripts that build for themselves are `scripts/lib.sh::cargo_artifact`
(used by `diff_test.sh` without `--bin`, `diff_gate.sh`, `bench.sh` and
`hotbench.sh`). They run raw `cargo build`, which is foreign to brokkr's host
model. brokkr's stray reaper kills cargo processes without a brokkr owner at
any locked command, and its rustc guard refuses compiles while a hold exists.
So a `scripts/bench.sh` run started next to any `brokkr check` dies, or
blocks it. `research/bench_luars.sh` does not build at all. It runs whatever
`target/release/dellingr` holds, and per 3.2 that is the binary last uplifted
by any release build. It may come from another commit, or be the `snapshot`
feature build from `release-native-stack-snapshot`.

Fix: scripts should take the binary as a required argument. The standalone
build path belongs to brokkr. No `brokkr` subcommand prints an artifact path
today, so that is a brokkr feature request.

Enforceable: a textlint rule banning `cargo ` invocations in `scripts/**` and
`research/*.sh`.

### 4.2 Reference Lua, Python, bash and coreutils from `PATH`

Gate dependencies are resolved from the ambient `PATH` with no version
detection:

- `lua5.2` and `lua5.4`. Which patch release, and which distro compat flags
  (`LUA_COMPAT_ALL` changes the globals 5.2 has), decides which reference
  "matches". `examples/edge_cases.lua` is already `DIFF`-marked because C
  Lua's `-nan` output depends on the host. `research/lua-5.2` and
  `research/lua-5.4` are local checkouts that nothing builds from.
- `python3`. `native_calls.py` imports `tomllib` without a version check, so on
  Python older than 3.11 it raises an ImportError traceback.
  `scripts/lib.sh::workload_hotpath_files` checks the version properly.
- `bash`, GNU `timeout`, `grep`, `diff`, `sed`, `sort`, `find` and `mktemp`.

Fix: 3.6 removes the reference Lua and coreutils from the gate. Until then,
the refresh tool should record `lua5.x -v` beside the outputs. Give
`native_calls.py` the same version guard as `lib.sh`.

Enforceable: yes, with recorded versions checked by the refresh tool.

### 4.3 Memory cap through a bash wrapper

`run_examples` runs every example as
`bash scripts/test_limited.sh <bin> ...` to apply `ulimit -v`. That is one
extra bash per example per sweep, and on hosts where `ulimit -v` is
unsupported (macOS) it is a refusal with exit 2. `libc` is already a
dev-dependency.

Fix: apply `setrlimit(RLIMIT_AS)` in a `pre_exec` hook on the `Command`.
`test_limited.sh` remains for manual use.

Enforceable: n/a.

### 4.4 Poll-and-sleep in `run_examples`

`run_example` polls `try_wait` every 25ms. That adds a mean 12.5ms per example
and spins a thread. A waiter thread with a channel and `recv_timeout`, or
`wait` on the child from the reader side, would remove the poll.

Enforceable: n/a. This is small; it is listed because the brief asks for every
sleep that stands in for an event.

### 4.5 Production values with no injection point

`EXAMPLE_TIMEOUT_MS`, `EXAMPLE_WORKERS`, `KERNEL_PARTITIONS` and
`TEST_LIMIT_KB` are fixed constants, which is fine for test infrastructure.
The problem is that they are tuned against an environment they do not
detect: core count, and the 20s cap (3.5).

### 4.6 Determinism across hosts is never exercised by the gate

Replays must be byte-for-byte reproducible "on every host", and goldens exist
for it:

- `tests/cost_golden.rs`
- `tests/fixtures/save_golden_current.bin`
- `tests/fixtures/crmath_golden.txt`
- `tests/fixtures/opcode_encoding.txt`

Every sweep runs them on one host and one target. There is no CI directory in
the tree. `examples/hotpath.rs` uses `std::os::unix` and `libc::O_NONBLOCK`,
and `cargo test` builds examples, so the suite cannot even build on Windows.

brokkr supports `[[check]] target = "x86_64-unknown-linux-musl"`, which runs
natively on this host (`brokkr man sweeps`). That would at least run the
goldens against a different libc and allocator. Running `i686-unknown-linux-gnu`
natively would exercise 32-bit `usize`, if multilib is present. Neither covers
aarch64.

Enforceable: yes, as a `[[check]]` entry with a narrow `only` filter for the
golden and determinism tests.

---

## 5. Where they assume they run, and what they touch

### 5.1 Writes into shared or ignored locations

| Writer | Location | Problem | Should use |
|---|---|---|---|
| `scripts/diff_test.sh` | `.scratch/dellingr-diff-*` (top level) | mixed in with ad hoc files; leaks on SIGKILL (18 files now) | no files (3.6), or `.scratch/diff-test/` cleaned at start |
| `scripts/bench.sh` | `.scratch/bench-results/<run>/` | fine, owns its subdir and prunes by pattern | as is |
| `tests/common/scratch.rs::write_boundary_transcript` | `.scratch/error-boundary/transcript.txt` | fixed path in a dir with many owners; breaks the helper's own rule (1.2) | keep the subdir but move the writer out of `scratch.rs` |
| `research/bench_luars.sh` | `.bench-luars.json` at the repo root | not in `.gitignore`, so an interrupted run leaves an untracked file | `.scratch/bench-luars/` |
| `src/crmath/tests/golden.rs::regenerate_crmath_golden` | tracked `tests/fixtures/` | reachable by `brokkr test` substring (3.7) | an example, like `regenerate_save_golden` |
| `tests/common/scratch.rs` | `CARGO_TARGET_TMPDIR` = `/media/folk/Banan/cargo/tmp` | Cargo's sanctioned spot, but this target dir is shared by other projects (their test dirs are there now), so isolation is the `dellingr-cli-<name>-<pid>-<n>` prefix alone, and another project's `cargo clean` wipes it | acceptable; state that the prefix is the isolation |

`.scratch/` is one ignored directory with at least four tool owners plus
hand-made files. Nothing says which subtree belongs to whom. A cleanup by one
tool can destroy another's state.

Enforceable: a textlint rule over `scripts/**` and `tests/**` that requires
`.scratch/<owner>/` and never bare `.scratch/` names.

### 5.2 Host configuration taken for granted

- **Core count.** See 3.5. On a small host this is a refusal, not a slow pass.
- **Arch.** `scripts/frame_sizes.py` parses AT&T x86-64 prologues
  (`sub $0x..,%rsp`). On another arch, or when no symbol matches, it prints
  nothing and exits 0, so a probe comparison reads as "no frames".
- **GNU tools.** `bench.sh`'s prune uses GNU `find -printf`.
- **Absolute paths.** brokkr passes the workload as an absolute path
  (`brokkr man dellingr`). AGENTS.md says the harness compiles "its path, as
  given" and illustrates with `bench/arithmetic.lua:12:`. Under brokkr the
  chunk name, and so every error location, is a host-absolute path.

---

## Lateral findings

- **`examples/globals/write.lua`'s header is stale and pinned** (2.6). It
  describes `globals_version` and an uncached global write that the tree no
  longer has.
- **`hotpath` is a non-optional dependency of an embeddable deterministic
  library.** `Cargo.lock` lists `tiny_http`, `quanta`, `time`, `serde_json`,
  `crossbeam-channel` and `hdrhistogram` among its possible dependencies. I
  did not verify which of them compile with the feature off. The comment in
  `Cargo.toml` says it "costs only compile time", but nothing checks that the
  consumer's dependency graph stays free of a timing or HTTP crate. A
  `cargo tree -e normal` script check against an allowlist would hold it.
- **No unsafe policy lint.** There is `unsafe impl Sync for RuntimeCaches`
  (`src/vm/caches.rs`) and about 14 `unsafe` calls into the hardware FMA
  kernels in `src/crmath/`. No `unsafe_code` level and no
  `clippy::undocumented_unsafe_blocks` is set, so a new `unsafe` needs neither
  a justification nor a review flag.
- **`notes_move.py` and `notes_drop.py`.** `test_notes_sections.py` covers
  only the fence handling. It does not cover `main()`, the dest-equals-source
  data loss (3.11), or `notes_drop.py`'s missing argument check (an
  `IndexError` traceback with no arguments).
- **`scripts/ab_pairs.py` is undocumented.** AGENTS.md describes every other
  helper script but not this one, which implements the CLAUDE.md "interleave
  matched A/B cells" rule.
- **Two lists of reference interpreters.** `scripts/lib.sh` has
  `REFERENCE_LUAS` (5.2, 5.4) and `scripts/bench.sh` its own (5.5, 5.4, 5.2,
  luajit).
- **`research/bench_luars.sh` has several smaller faults.** It does not anchor
  to the repo root. It hard-codes `--warmup 2` instead of
  `HYPERFINE_WARMUP`. Its header says it "Mirrors ../bench.sh's jq
  extraction", but `bench.sh` lives in `scripts/`. It tells the reader to
  `cargo build --release`, which the project's rules ban.
- **`.review.toml` is unexplained.** It holds only `[_audit] id = "40c9"`, has
  no comment naming its consumer, and is excluded from the package. If the
  `review` tool owns it, a one-line comment saying so would let a reader know
  whether it can be edited.
- **The textlint alternation includes very common names** (`call`, `run`,
  `format`, `main`, `get_table`, `set_table`). Any new method with one of those
  names on an unrelated type becomes a "stray" that `native_calls.py` asks to
  exclude by file. That pushes toward excluding whole files, which is what
  `src/compiler/**` already is. Receiver-aware matching (already modelled in
  the script) would let the script, not the regex, decide.
