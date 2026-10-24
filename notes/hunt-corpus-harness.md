# Verification hunt: corpus-harness

Scope: `tests/common/` (cli, examples, scratch, stack, save_golden_program),
`tests/run_examples.rs`, `tests/diff_test.rs`, `tests/bench_scripts.rs`,
`tests/cli_arguments.rs`, `tests/cli_limits.rs`, `tests/cli_output.rs`,
`tests/disassemble_corpus.rs`, `examples/hotpath.rs`,
`examples/regenerate_save_golden.rs`, and the Lua corpora under `examples/`
and `bench/` as test inputs. I followed threads into `scripts/diff_test.sh`,
`scripts/lib.sh`, `scripts/test_limited.sh`, `brokkr.toml`, `Cargo.toml`,
`src/compiler.rs` (the corpus unit test), `src/host.rs`, `tests/save_golden.rs`,
`tests/test_conventions.rs`, `tests/error_boundary.rs`, the brokkr manual, the
latest brokkr check/measure logs and the host's `~/.cargo/config.toml`.

Evidence sources quoted below: the `brokkr check` run log
`.brokkr/check-logs/check-001792803436286.log` (plantasjen, 2026-10-23; the log
does not record the commit), its accounting plan/journal, and the forced
measure log `.brokkr/check-logs/measure-001792803544537.log` (plantasjen,
commit 7375bb4, dirty tree).

Findings are grouped by the five questions, not ranked. Each ends with what
could hold the fix mechanically.

---

## 1. Tests that prove nothing

### 1.1 36 of the 39 kernel examples check nothing, and the diff gate compares a constant

Every `.lua` under an `examples/` subdirectory except `tables/sort.lua`,
`tables/queue.lua` and `tables/metamethod_sequence.lua` ends with an
unconditional `print("<name>: true")` and contains no `error(` call and no
conditional output. `_bench()` returns a value (a checksum, in most files) and
the footer discards it. Consequences:

- `run_all_examples`' `: true` requirement for `_bench` files is satisfied by
  the literal string; it proves only "the footer was reached".
- `differential_examples` compares dellingr's output with lua5.2/lua5.4 on these
  files, but the output is the same constant line regardless of what the
  kernel computed. If `fib(20)` returned 0, or the arithmetic kernel produced
  NaN, all three runs would print `arithmetic: true` and the gate would pass.
- These are the files `diff_test.rs` deals into six of its seven partitions
  because they are slow ("about a second each in the unoptimized test
  profile"). Most of the differential gate's wall time is spent on files whose
  compared output cannot differ.

Fix: make the footer print the result, for example
`print("numerics/arithmetic: " .. tostring(_bench()))` after the loop, or fold
the 30 results into a checksum and print it. Then the diff gate compares real
values against two reference implementations, which is exactly the
differential coverage the file names suggest. Floats render the same in 5.2 and
dellingr, so a 5.4-only `.0` difference falls under the existing either-reference
rule.

Enforceable: yes. Add to `bench_scripts.rs` (or a new examples-footer test) a
rule that an examples/ `_bench` file's success line is not a string literal
(it must concatenate a value), mirroring the bench/ footer contract that test
already enforces. A stricter version: run each kernel twice under the test
with a perturbed VM is not feasible, but the literal-string check is a
one-regex rule.

### 1.2 DIFF/SKIP-DIFF files that have no assertions of their own are checked by nothing

A `-- DIFF:` or `-- SKIP-DIFF:` marker removes the reference comparison for the
whole file. For such files the only remaining check is `run_examples`' rule
that no stdout line ends in `: false`. Several marked files print their results
in a shape that rule never sees:

- `examples/math.lua` (DIFF): prints `label =\t<value>` via `print(a, b)`.
  No line can end in `: false`. The file asserts nothing; it proves "math
  functions do not crash".
- `examples/edge_cases.lua` (DIFF, because 0/0 renders as `nan` where C Lua
  prints `-nan` on x86-64): about 70 checks written as
  `print("10 < 9 = " .. tostring(10 < 9))  -- false`. Expected values live
  only in comments. `= false` is not `: false`, and the closing
  `=== All edge case tests passed! ===` prints unconditionally. One divergent
  line (section 3) disables comparison of the other ~70.
- `examples/upvalue_stress.lua` (SKIP-DIFF): ~30 value prints with expected
  values in comments (`-- 55`, `-- 11111`, ...), closing banner unconditional.
  Nothing in it looks like a dellingr divergence; upvalue semantics are
  standard Lua. The marker seems to exist because the file is called a "stress"
  test, and it removes the only check its output could have had.
- `examples/stress_tables.lua`, `examples/stress_rustfn.lua`,
  `examples/stress_gc_stack.lua`, `examples/benchmark.lua` (SKIP-DIFF): print
  progress banners and `PASS` unconditionally. They are smoke tests by
  design; that is fine, but nothing records that they are smoke tests.

Fixes, in order of preference:

1. Split the divergent statement into its own small DIFF file and drop the
   marker from the rest (`edge_cases.lua`: move the `0/0` line out;
   `math.lua`: move the `math.random` and float-rendering lines out).
2. Drop markers that are not needed (see 2.3 for a checker).
3. Convert value prints to the `name: true/false` convention, or to `error()`
   on mismatch as `feature_test.lua` does.

Enforceable: yes. In `run_examples.rs`, require every DIFF/SKIP-DIFF file to
print at least one line matching `: (true|false)$` or to call `error(`, unless
it is listed in an explicit smoke-test allowlist in the test. That turns
"checked by nothing" into a reviewed decision.

### 1.3 `run_feature_test_extended_in_process` cannot see the output it produces, and writes it past libtest

The test runs `feature_test_extended.lua` on `State::new()`, whose
`DefaultCallbacks::on_print` writes through `std::io::stdout().lock()`
directly (`src/host.rs`). That bypasses libtest's output capture, so ~100
`[PASS] ...` lines go straight onto the test binary's stdout, which under brokkr
is the libtest JSON event stream (`--format json`). The test only checks that
`call` returned `Ok`; it relies on the script's `error()` on failure, so it is
not vacuous, but it is strictly weaker than the CLI run of the same file in
`run_all_examples`, and it finds the file by walking the entire corpus to look
up one fixed path.

Fix: use a capturing `HostCallbacks`, assert the captured lines contain no
`[FAIL]` and at least N `[PASS]`, and read the file by its path directly.

Enforceable: partly. A `test_conventions.rs` rule that integration tests do not
call `State::new()` (or `DefaultCallbacks`) when the program they run calls
`print` is not checkable in general; a narrower rule "tests that load a corpus
file must use a capturing callback" is a grep. Alternatively, give
`DefaultCallbacks` a `cfg(test)`-free sibling that writes via `print!`, which
libtest does capture; that is a type-level fix.

### 1.4 The bench-pair fingerprint test is vacuous for `large_source`

`registered_workload_pairs_keep_kernel_cost_fingerprints_aligned` compares
`file` against `hotpath_file`. For `[dellingr.workloads.large_source]` both are
`examples/parse/large_source.lua`, so the test compares a file with itself and
cannot fail. It reads as coverage for every registration.

Fix: skip registrations whose two paths are equal, explicitly and with an
assertion that only the documented parse probe may do so.

Enforceable: yes, in the same test.

### 1.5 The fingerprint cannot see the kernel's scale, and the composite's "same proportions" claim is false

`cost_fingerprint` captures op-class counts, instruction counts and nesting.
Loop bounds are literals, so changing `for i = 1, 50000` to `for i = 1, 5000`
inside a kernel leaves the fingerprint unchanged. brokkr.toml says wrapper
repeat counts are "re-checked by eye"; the kernel's own bounds are not covered
either.

The concrete false claim: `examples/composite/benchmark.lua`'s header says
"Same kernels in the same proportions" as `bench/benchmark.lua`. Every section
is scaled by 1/100 except section 3: `fib(19)` makes 13,529 calls against
`fib(28)`'s 1,028,457, a ratio of 76, not 100. Recursive-call share in the
hotpath file is about 1.3x what it is in the verdict file, so per-function
percentages from `--hotpath` on `benchmark` do not carry over for the call
path, which is the path most of the recent optimization work targets.

Fix: compare `Program::disassemble()` text per function (literals included)
for the ordinary pairs, where the kernel bodies are literally identical; keep
the composite as the documented exception and fix its header (or pick a scale
where the claim is true for every section).

Enforceable: yes, a disassembly-equality assertion in `bench_scripts.rs`.

### 1.6 `disassemble_corpus` checks one thing that cannot happen and one that barely constrains

`every_example_disassembles_into_well_formed_rows` asserts no line contains
`<out of range>`. Its own doc comment says why that holds: finalized bytecode is
verified, so every operand is in range by construction. The other assertion
(each line is empty, starts with `function main` or starts with two spaces)
admits almost any output. The string `"<out of range>"` is also duplicated
from `src/disassemble.rs` rather than shared, so a rename there turns the check
into a silent no-op.

The same corpus is compiled for the same purpose by
`compiler::...::compiler_examples_pass_bytecode_verification` in
`src/compiler.rs`, which asserts much more (verifier pass plus coverage of open
call/return/setlist/vararg shapes).

Fix: fold the disassembly pass into the compiler's corpus test (one walk, one
compile per file), and export the annotation text as a `pub(crate) const` if
the check stays.

Enforceable: the shared constant makes the string check rename-proof.

### 1.7 `hotpath.rs`'s unit tests never run

`examples/hotpath.rs` has a `#[cfg(test)] mod tests` with five tests. The
`[[example]] name = "hotpath"` entry has no `test = true`, examples default to
`test = false`, and brokkr scopes every sweep to `cargo test --tests`. The
accounting plan of the latest check (`plan.json`, 2119 expected executions)
contains `local_count_label_distinguishes_parameters` but not
`hotpath_requires_a_script_path`. These tests have never been part of a check.
Whether clippy's `--all-targets` even type-checks them depends on whether it
builds examples in test mode, which I did not confirm; if it does not, they
may not compile.

Fix: `test = true` on the `hotpath` example, or move `parse_args_from`,
`run_metrics` and `write_kvs` into a module a real test target includes.

Enforceable: yes. A `test_conventions.rs` (or `brokkr.toml` manifest-gate)
rule: any `[[example]]` whose source contains `#[cfg(test)]` must set
`test = true`. Cheap to write as a script check over `Cargo.toml` plus a grep.

### 1.8 The save golden pins a property of the dev profile that production does not share

`tests/save_golden.rs` documents that the fixture "must come from the profile
the tests run under": release builds merge identical functions (`unpack` and
`table.unpack`), and the save then writes the smallest id for both. So the
pinned bytes are the bytes a debug build saves; a shipped release build saves
different bytes for the same program. The test pins walker order, which is
profile-independent, through a fixture that is not. Consequences:

- `brokkr test save_golden --release` fails on correct code.
- `examples/regenerate_save_golden.rs` does not check its profile; run with
  `--release`, it silently writes a fixture every dev sweep then rejects.
- The production save format is never pinned at all.

Fix: build the golden state so its function set has no identical-code
candidates (a restricted environment containing only the functions the program
uses, or normalise function ids before comparing), so the fixture is the same
in every profile; then run it in a release sweep too.

Enforceable: once profile-independent, add `save_golden` to a release
`[[check]]` (the release native-stack sweeps already exist) so the property is
held by the build. Until then, the regenerator can refuse to run unless
`cfg!(debug_assertions)`, which is a proxy (see 5.4) but better than nothing.

### 1.9 `diff_test.rs` and `run_examples.rs` assert things about the shell tool, not the product

`malformed_timeout_is_rejected_once_before_other_checks` and
`shell_and_rust_example_corpora_match` test `scripts/diff_test.sh`'s argument
parser and its `find | sort` walk. They exist because the gate is split between
a Rust walker and a shell walker that must agree. `diff_markers_have_nonempty_reasons`
re-implements a rule `diff_test.sh` already enforces on every file it runs
(`grep -Eq '^-- (DIFF|SKIP-DIFF):[[:space:]]*$'`), with slightly different
whitespace semantics (`str::trim` is Unicode, `[[:space:]]` under `LC_ALL=C` is
ASCII). All three run twice per check. See 3.1 for the structural fix that
deletes the need for all three.

### 1.10 Smaller "proves less than it says" items

- `tests/cli_arguments.rs::analyze_rejects_limit_and_sends_its_warning_to_stderr`
  and `tests/cli_output.rs::analyze_warning_uses_the_library_caveat` both assert
  the analyze warning equals `ANALYSIS_WARNING_PREFIX + CostAnalysis::caveat()`.
  One of them is redundant.
- `examples/demo_stack_trace.lua.txt` is renamed out of the corpus (it errors
  on purpose), referenced by nothing in src, tests, docs, reference, scripts or
  notes, and run by nothing. Either delete it or turn it into an
  expected-stderr test of stack-trace rendering.
- `examples/benchmark.lua` (SKIP-DIFF, prints sums, no checks) is the original
  of `bench/benchmark.lua` and `examples/composite/benchmark.lua`. It is likely
  the heaviest top-level example in the debug build (1M-iteration loop plus
  `fib(28)`), it proves only "does not crash", and its header still says
  "Run with external timing: time lua examples/benchmark.lua", which the
  project's benchmarking rules forbid. `bench/benchmark.lua`'s header cites it
  as its source. Candidate for deletion; the header citation would go with it.
- `bench/*.lua` files are compiled and loaded by `bench_scripts.rs` but never
  executed by any test. A wrapper that errors at runtime (bad helper name in
  `_bench`, wrong `K` type) is found only when someone benchmarks. The paired
  examples/ kernel covers the kernel body, not the wrapper.

---

## 2. Guards and claims that have stopped holding

### 2.1 The run_examples 60-second timeout cannot fire under brokkr; the whole corpus has 20 seconds

`run_examples.rs` gives each example `EXAMPLE_TIMEOUT_MS = 60_000` and AGENTS.md
repeats "with a fixed 60-second timeout". brokkr's per-test cap is a hard 20
seconds per test (`brokkr man hygiene`), and `run_all_examples` is one test
that runs the entire corpus. So:

- A single hung example is killed by brokkr's watchdog at 20s, not by the
  runner at 60s. The runner's diagnostic (which example, its stdout/stderr) is
  never produced; brokkr reports `run_all_examples` as the suspect.
- The whole corpus, all workers, must finish inside 20s. That is the real
  budget, and nothing in the repository states it.

The same holds for `differential_examples`: the per-process `DEFAULT_DIFF_TIMEOUT=5s`
in `lib.sh` operates inside one 20s test containing seven partitions.

Fix: per-example timeouts well under the cap (for example 5s, matching the
diff gate), or one test per example (see 3.2) so each gets its own 20s and its
own name. Reword AGENTS.md to state the actual bound.

Enforceable: a `run_examples.rs` const assertion that the per-example timeout
is below a named `BROKKR_PER_TEST_CAP_SECS` constant documents the coupling;
brokkr cannot export its cap, so this is a convention, not a proof.

### 2.2 `feature = "hotpath"` guards silently miss `--features hotpath-alloc`

`Cargo.toml` defines `hotpath-alloc = ["hotpath/hotpath", "hotpath/hotpath-alloc"]`.
That enables the dependency's features, not this crate's own `hotpath`
feature. Instrumentation is on under `hotpath-alloc`, but
`cfg(feature = "hotpath")` is false. Name-keyed guards that only test
`"hotpath"` therefore fail open under `hotpath-alloc`:

- `tests/diff_test.rs` compiles its corpus tests out only for `"hotpath"`, for
  the stated reason that instrumentation pushes the corpus past the time
  budget. Under `hotpath-alloc` they compile in and run instrumented.
- `src/compiler/parser.rs`'s unit-test stack constant switches only on
  `"hotpath"`, so instrumented `hotpath-alloc` parser tests run on the
  uninstrumented 2 MiB stack. `tests/common/stack.rs` says its
  `PARSE_TEST_STACK_BYTES` matches "as `src/compiler/parser.rs`'s unit-test
  constant does"; that parity claim is false for `hotpath-alloc`.
- `tests/common/mod.rs`'s `cfg_attr(feature = "hotpath", allow(dead_code))`.

`tests/common/stack.rs` and `tests/error_boundary.rs` spell both features and
are correct, so the repository already knows; the guards just disagree.

Fix: `hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]`, so the local
feature is implied, then every cfg tests `"hotpath"` only.

Enforceable: yes. After the fix, a textlint rule banning
`feature = "hotpath-alloc"` inside `cfg(` anywhere except the allocator
installs (`src/main.rs`, `examples/hotpath.rs`) holds it.

### 2.3 Nothing notices a DIFF marker that is no longer needed

A DIFF marker skips both reference runs. If dellingr starts matching a
reference (a divergence fixed, or a reference upgraded), the marker keeps
suppressing the comparison forever. Candidate stale markers today:

- `examples/tables/metamethod_sequence.lua`: the stated reason is that *Lua 5.2*
  reads raw elements and has no `table.move`. The gate passes on a match with
  *either* reference, and every behaviour the file asserts (`__len`/`__index`
  in unpack and concat, `__newindex` order in insert/remove, move through
  metamethods, explicit-bounds unpack skipping `__len`) is what Lua 5.4's
  `ltablib.c` does. The marker looks unnecessary. I did not run lua5.4 to
  confirm.
- `examples/upvalue_stress.lua`, `stress_tables.lua`, `stress_rustfn.lua`
  (SKIP-DIFF): nothing in them is dellingr-specific.

Fix: have the gate run the references for DIFF files too, and fail with
"stale DIFF marker" when dellingr matches either. SKIP-DIFF stays for files
that cannot run under the references.

Enforceable: yes, that is the check itself.

### 2.4 The scratch-path convention is a substring scan that misses common spellings

`tests/test_conventions.rs` flags file mutations outside `common/scratch.rs`
by substring. Spellings it does not see: `fs::remove_dir_all(` (contains
`remove_dir_all(`, not `remove_dir(`), `fs::rename(`, `fs::copy(`,
`fs::hard_link(`, `File::options()`, a `use std::fs::write;` followed by a bare
`write(`, and any file write performed by a spawned process (the diff gate's
`mktemp` into `.scratch/`, see 5.1). Its final assertion checks that
`scratch.rs` *contains the strings* `CARGO_TARGET_TMPDIR` and
`.starts_with(scratch_root)`; a comment containing both would satisfy it. It
also scans only `tests/`, so a unit test in `src/` is not covered, and the
"no test may reference `target/` or spawn cargo" rule AGENTS.md states is not
checked at all (no scan for `Command::new("cargo")` or `"target/"`).

Fix: replace the substring list with clippy's `disallowed_methods` for the
integration-test targets (clippy.toml applies package-wide; a
`#![allow(clippy::disallowed_methods)]` inside `scratch.rs` is the one
exemption), listing `std::fs::write`, `remove_dir_all`, `rename`, `copy`,
`File::create`, `OpenOptions::new` and `std::process::Command::new` (with
scoped allows in `cli.rs`). That moves the rule from text to resolved paths.

Enforceable: yes, that is the point of the change. Caveat: the same
disallowed list would hit `src/` and `examples/`; scoped allows there are the
cost.

### 2.5 brokkr.toml workload parsing in `bench_scripts.rs` fails open on valid TOML

`workload_pairs` is a hand-written line scanner. A registration written as an
inline table (`[dellingr.workloads]` then `foo = { file = "...", ... }`) or with
dotted keys is skipped silently; the only floor is "at least one registration".
`scripts/lib.sh` parses the same file with Python `tomllib`, and brokkr with
its own parser, so three readers can disagree.

Fix: a `toml` dev-dependency (or `brokkr` exposing the resolved registry as
JSON) and a real parse; assert the set of names equals what brokkr resolves.

Enforceable: yes, by construction.

### 2.6 Pins, orphans and generated-file claims that nothing checks at check time

- No test verifies that each `xxh128`/`hotpath_xxh128` pin matches its file.
  brokkr refuses on drift, but only when someone benchmarks that workload. A
  pure-Rust `xxhash-rust` dev-dependency would let `bench_scripts.rs` check
  every pin on every check, so an edit without re-registration is caught by
  the commit that made it.
- No test asserts that every `bench/*.lua` is registered (every one is today).
- AGENTS.md: "the parser caps a chunk at 255 nested functions" restates
  `MAX_NESTED` (`u8::MAX as usize`); `generate.py` reads the constant, the prose
  does not. True today; reword to name the constant.

Enforceable: all three are a few lines in `bench_scripts.rs`.

### 2.7 Harness path claims that are false under brokkr

AGENTS.md says the harness compiles the script "with its path, as given, as
the chunk name, so error locations read `bench/arithmetic.lua:12:`". The
brokkr manual says brokkr passes `/abs/path/to/workload.lua`, and the measure
log confirms it: `target=/home/folk/Programs/dellingr/bench/method.lua`. So
under the documented caller, error locations and the stored `target=` KV carry
the developer's absolute checkout path, and the `target` column in
`results.db` differs between checkouts and hosts for the same workload.
`hotpath_keeps_the_callers_script_path` (never run, 1.7) tests only the
relative case. Worth checking separately: whether the chunk name is interned
in the GC heap, in which case `setup_heap_bytes`/`final_heap_bytes` (stored
sidecar counters) also vary with checkout path length.

Fix: the harness should take the workload name or strip the project root and
use a root-relative chunk name; or brokkr should pass the registered relative
path. Either way the claim and the behaviour should agree.

Enforceable: a harness unit test (once 1.7 makes them run) that an absolute
path under the manifest dir yields a root-relative chunk name.

### 2.8 "The only environment input" claims

`scripts/lib.sh` and AGENTS.md say `BROKKR_MARKER_FIFO` is "the only
environment input in the repository" and that "the scripts read no environment
variables". Not true as stated: the harness also consumes `HOTPATH_OUTPUT_PATH`
(through the `hotpath` crate; brokkr sets it), every script resolves `bash`,
`lua5.2`, `lua5.4`, `timeout`, `python3` and `cargo` through `PATH`, and
`cargo` itself reads `~/.cargo/config.toml` (see 5.3). Reword to "reads no
environment variables of its own" and list what is resolved through `PATH`.

---

## 3. The test suite as a system

### 3.1 The differential gate is a Rust test driving a bash pipeline; move it into Rust and stop calling references on every run

Today one `cargo test` process spawns seven `bash diff_test.sh` processes, each
of which, per file, runs `grep` three times, then `timeout dellingr`, then
`timeout lua5.2` and `timeout lua5.4`, writes all three outputs to `mktemp`
files in `.scratch/`, and runs `diff -q`/`diff -u`. The split creates the need
for `shell_and_rust_example_corpora_match`, `malformed_timeout...` and the
duplicated marker rule (1.9), the `.scratch/` leak (5.1), the `bash`/coreutils/
`grep`/`diff`/`find`/`mktemp` host dependencies, and the opaque failure shape
(one test, seven partitions, a stderr blob).

Two structural options, which compose:

1. **Native gate.** Spawn dellingr and the references from Rust with
   `Stdio::piped()`, a deadline loop (the code already exists in
   `run_examples.rs`), and compare bytes in memory. No files, no bash, no
   shell-side walker, failures reported per example. `diff_test.sh` becomes a
   manual tool or is deleted, and with it three tests.
2. **Checked-in expected output.** Reference outputs depend only on the file's
   bytes and the reference version. Generate `examples/**/<name>.expected`
   once with a script that runs both references (recording which one matched
   and a hash of the source in a header), check them in, and have the test
   compare dellingr's output to the file. Then:
   - `brokkr check` needs no lua5.2/lua5.4 on the host (today the gate fails
     on any host without them, by design);
   - DIFF files carry dellingr's own expected output instead of escaping
     comparison entirely (1.2), and the divergence is reviewable in a diff;
   - the reference runs, currently four per file per check (two sweeps), drop
     to zero;
   - a stale `.expected` (source hash mismatch) is a check failure, and the
     on-demand regeneration script is where reference drift is detected.

Enforceable: the source-hash header makes a stale expected file a test
failure; nothing to remember.

### 3.2 Corpus tests are single tests, which defeats every per-test mechanism brokkr has

`run_all_examples` and `differential_examples` each run the whole corpus as
one libtest test, and both re-implement a worker pool because "brokkr runs
test binaries with one test thread". Costs: no focused run of one example
(`brokkr test calls_method` matches nothing), no per-example 20s cap or
attribution, a hang anywhere is reported against the whole corpus, and the
in-test pools hard-code their concurrency (3.4).

Fix: generate one `#[test]` per example from a `build.rs` that walks
`examples/` and emits `include!`-able test functions (with
`cargo:rerun-if-changed=examples`). Each example then has a name, its own cap
and attribution, and can be run alone; concurrency becomes brokkr's job
(`test_threads` or `parallel = { budget }` on a sweep). The same generated
list can feed the compiler-verification and disassembly passes.

Enforceable: structural; the walker becomes the only enumeration.

### 3.3 Four corpus walks, two implementations, three compiles per file

The corpus is enumerated by `tests/common/examples.rs` (used by
`run_examples`, `diff_test`, `disassemble_corpus`, and `include!`d into
`src/compiler.rs`'s unit test) and by `sorted_lua_files` in `lib.sh`. Each
sweep compiles every example in `compiler_examples_pass_bytecode_verification`,
again in `disassemble_corpus`, and executes it in `run_all_examples` and
`differential_examples`. The compiles are cheap; the executions are not, and
see 3.5 for what they spend.

The walk is a filesystem walk, not a tracked-file walk: an untracked `.lua`
dropped into `examples/` (a scratch repro, say) joins every gate on that host
and no other. Either walk `git ls-files examples` (adds a git dependency) or
treat untracked files in `examples/` as a test failure with a clear message.

### 3.4 Concurrency hard-coded against the host

- `run_examples.rs`: `min(8, available_parallelism, files)`.
- `diff_test.rs`: always `KERNEL_PARTITIONS + 1 = 7` concurrent pipelines,
  independent of core count, each running dellingr and two references
  serially, inside a 5s-per-process and 20s-per-test budget.

On plantasjen (12 physical cores) both have headroom. On a 4-core host the
seven partitions oversubscribe and the debug kernels (about a second each
unloaded) approach the 5s per-process timeout, producing `TIMEOUT` failures
that are host load, not regressions. The partition rule "subdirectory means
kernel, top level means fast" is also a naming heuristic:
`tables/metamethod_sequence.lua` is a fast correctness file classified as a
kernel, and `examples/benchmark.lua` is a heavy top-level file (SKIP-DIFF, so
it happens not to matter today).

Fix: 3.2 removes the in-test pools. Short of that, derive the partition count
from `available_parallelism` and partition by a measured or declared weight,
not by directory depth.

### 3.5 The 30-call standalone footer is paid by every check and every bench launch

Every examples/ kernel ends with `for i = 1, 30 do _bench() end`, "enough for
hyperfine resolution", which serves `scripts/bench.sh`'s reference-ratio
table. The test suite pays it on every run: each of the 36 kernels runs its
kernel 30 times in a debug build, in `run_all_examples` and again in
`differential_examples` (plus 60 more reference executions), in both the
default and snapshot sweeps. At "about a second each" (diff_test.rs's own
estimate) that is on the order of 150 CPU-seconds per check spent repeating
kernels whose output is a constant (1.1). The whole test phase of the latest
check took 55.2s wall across four sweeps (plantasjen, log above).

The verdict harness pays it too: `examples/hotpath.rs` runs the top level
during setup, so a `bench/` file's footer runs 30 seconds-scale calls before
the cold call. In the forced `method` run at 7375bb4 on plantasjen,
`elapsed_ms=3900.867` against `cold_call_us=75213` plus 20 warm calls at
`warm_avg_us=75811.782` (about 1.59s), so roughly 2.3s, about 59% of the
launch, is untimed footer. `--bench 3` spends about 7s per workload on it.

Fix: make the footer count an input. Lua chunks receive script arguments as
`...` in both dellingr (if the CLI forwards them; today it rejects extra
positionals) and reference Lua, so `local n = tonumber((...)) or 1` with
`bench.sh` passing 30 keeps hyperfine resolution where it is wanted and drops
the footer to one call in tests and zero in the harness (the harness can set a
global before setup instead, since it controls the State).

Enforceable: `bench_scripts.rs` already pins the bench/ footer text; extend it
to the examples/ footers.

### 3.6 The feature-shape claim of the corpus gates rests on a shared output path

`CARGO_BIN_EXE_dellingr` is the uplifted path `target/debug/dellingr`, which
the default and snapshot sweeps both write. The check log shows brokkr noticing
and rebuilding before each sweep ("rebuilt before listing -
/media/folk/Banan/cargo/debug/dellingr has other content"). `diff_test.rs`'s
doc ("the corpus is checked against the CLI build of that shape") therefore
holds only because brokkr runs sweeps serially and re-uplifts between them,
and because nothing else writes that path mid-run. A parallel sweep layout, a
`brokkr run --debug` between prepare and run, or an editor-driven build would
make the snapshot sweep's corpus tests run the default binary (or vice versa)
and pass.

Fix: have the CLI report its feature shape (`dellingr --build-info`, or a line
in `--help`), and have `run_examples`/`diff_test` assert it matches
`cfg!(feature = "snapshot")` before running anything. One spawn, and the
assertion turns a silent mismatch into a failure.

Enforceable: yes, that assertion is the enforcement.

### 3.7 `brokkr test` always passes `--include-ignored`, so "on-demand" tests run on any matching focused name

`error_boundary_transcript` in `tests/error_boundary.rs` is
`#[ignore = "on-demand transcript ..."]` and writes
`.scratch/error-boundary/transcript.txt` through
`scratch::write_boundary_transcript`. `brokkr test error_boundary` (dev
profile, the repository default) matches it and runs it, because brokkr test
always adds `--include-ignored`, so the documented focused command writes into
the source tree as a side effect. AGENTS.md's `--release` form avoids it only
because the test is compiled out without `debug_assertions`.
`write_boundary_transcript`'s comment calls the location "spec-mandated"; the
spec is not in `reference/` or `docs/`, so a durable comment is citing a
transient document.

Fix: make the transcript an `examples/`-style tool (`brokkr run ...`) rather
than an ignored test, and write it under `CARGO_TARGET_TMPDIR` or a path given
on the command line.

### 3.8 Roughly 80 integration-test binaries, each linking the crate, with blanket dead-code allows on shared helpers

Each `tests/*.rs` is its own binary; `tests/common/cli.rs` is `#[path]`
included per binary with `#[allow(dead_code)]` on the items (and on the whole
module in `run_examples.rs`), so an unused helper is never reported. Each
sweep links every binary. Merging into one `tests/it/main.rs` binary with
modules would cut link work and make dead helpers visible, at the cost of
sharing process state between tests. In my scope, the only process-global
state is `scratch.rs`'s `NEXT_SCRATCH_ID` (safe to share: ids are unique per
pid and counter) and `hotpath.rs`'s FIFO `OnceLock` (harness only). The
envelope tests elsewhere depend on thread stack sizes, not process state, so
the merge looks feasible; an audit of the other ~75 files would be needed.

Enforceable: once merged, `dead_code` works without allows; a
`test_conventions.rs` rule "no `#[allow(dead_code)]` under `tests/`" holds it.

### 3.9 Captured output is unbounded

`run_examples.rs` reads each child's stdout/stderr fully into memory;
`diff_test.sh` redirects to files. A runaway print loop fills both until a
timeout fires. `.scratch/dellingr-diff-our.kc5D3C` is 70,691,826 bytes, left
by a killed run on 2026-10-21. Cap captured output (read with a limit and fail
with "output exceeded N bytes"), which also turns a runaway print loop into a
fast, named failure.

---

## 4. What the tests reach that they should have been handed

### 4.1 Production values with no injection point

- `DefaultCallbacks::on_print` writes to the process stdout with no seam;
  every test that uses `State::new()` and runs a printing script writes past
  libtest (1.3). `State::new()` is the convenient constructor, so it is the one
  tests reach for.
- The per-test cap (20s), the per-example timeout (60s), the per-process diff
  timeout (5s), the poll interval (25ms) and the partition count (6) are
  independent constants in three files and two languages, and their real
  relationship (all must nest under 20s per test) is written nowhere.
- `TEST_LIMIT_KB` lives only in `lib.sh`, so `run_examples` must go through
  `bash scripts/test_limited.sh` to apply it. `libc` is already a
  dev-dependency: `Command::pre_exec` with `setrlimit(RLIMIT_AS, ...)` applies
  the same cap without spawning bash and sourcing `lib.sh` once per example.
  The constant then lives in Rust and `test_limited.sh` can read it from there
  or be retired.

### 4.2 Wall-clock polling in `run_examples.rs`

`run_example` polls `try_wait` every 25ms. Each example pays up to 25ms of
latency after exit. Small per example, recurring per check. A waiter thread
per child (or `wait-timeout` style blocking wait with a deadline) removes the
sleep entirely. The timeout itself is a real external-process deadline and
cannot be virtualised, which is fine.

### 4.3 Working directory and absolute paths

- `cli::run` runs the CLI with `current_dir(".")`, i.e. whatever the harness
  cwd is (cargo and brokkr both use the package root, so this works by
  convention); `limited_command` pins `CARGO_MANIFEST_DIR`. Pick one.
- `disassemble_corpus` uses the absolute path as the chunk name; the
  compiler's corpus test uses `None`; `run_feature_test_extended_in_process`
  uses a manifest-relative name. Harmless today (nothing compares the names),
  but three conventions for one corpus.

### 4.4 Shared mutable state in the test machinery

- `.scratch/` is shared by the diff gate (mktemp files), the ignored transcript
  test (fixed path, last writer wins) and the developer's own working files
  (patches, perf data, notes; about 200 entries today). See 5.1.
- `CARGO_TARGET_TMPDIR` is `/media/folk/Banan/cargo/tmp`, inside a target
  directory shared by every project on this host (the `target` symlink points
  at `/media/folk/Banan/cargo/`, whose `tmp/` holds other projects' fixtures).
  `scratch.rs` handles collisions (`dellingr-cli-` prefix, pid, counter,
  `AlreadyExists` retry), so this is sound, but a killed test (brokkr's
  SIGKILL at the 20s cap) leaks its directory into another tool's shared space
  with no reaper.

---

## 5. Where the tests assume they run, and what they assume they may touch

### 5.1 The diff gate writes into the source tree's `.scratch/` and leaks there

`diff_test.sh` creates three `mktemp` files per invocation in `$ROOT/.scratch/`
and removes them in an `EXIT` trap. brokkr's timeout kill is SIGKILL, which runs
no trap. `.scratch/` currently holds 18 leaked `dellingr-diff-*` files from
2026-10-21 and 2026-10-22, one of them 70 MB. `.scratch/` is the developer's
scratch area, not a test-owned directory; the test convention
(`test_conventions.rs`: "test file mutations must use the scratch helper ...
under CARGO_TARGET_TMPDIR") is bypassed because the write happens in a shell
child the scan never sees.

Fix: 3.1's native gate removes the files entirely. If the shell gate stays,
add `--scratch <dir>` and have `diff_test.rs` pass a `ScratchDirectory`
(under `CARGO_TARGET_TMPDIR`, owned by cargo's tmp convention and reaped with
`cargo clean`).

Enforceable: a `test_conventions.rs` rule that `Command::new("bash")` call
sites pass a scratch argument is brittle; deleting the shell layer is the real
enforcement.

### 5.2 Host tools the gates require

- `run_examples`: `bash` on PATH and a working `ulimit -v` (fails as exit 2 for
  every example where the hard limit is below 2 GiB, in some containers, and on
  macOS). Loud, not silent, but the failure is one message per example.
- `differential_examples`: `bash`, GNU `timeout`, `grep`, `diff`, `find`,
  `sort`, `mktemp`, and `lua5.2`/`lua5.4` under exactly those names (Debian
  naming). The reference patch versions are whatever the distro ships;
  a patch release that changes `%g` rendering or an error message changes the
  gate's verdict with no repository change. `edge_cases.lua`'s DIFF reason
  ("C Lua prints `-nan` for 0/0 on x86-64") is architecture-dependent: on
  aarch64 the reference prints `nan` and the marker would be unnecessary
  there.
- `generated-large-source` and `native-call-inventory` script checks: Python 3
  (and 3.11+ for `lib.sh`'s `tomllib`).

3.1's expected-output files remove the Lua and coreutils dependencies from the
standard check and confine reference versions to the regeneration script,
which can record them.

### 5.3 The host's global cargo config shapes what the tests measure

`~/.cargo/config.toml` on plantasjen sets `-Ctarget-cpu=native`, links with
`wild` via `clang`, sets `[profile.dev] debug = "line-tables-only"`, and
installs `brokkr-rustc-guard` as the rustc wrapper. None of this is in the
repository. Consequences in and around my scope:

- `tests/common/stack.rs` states the envelope as "4 MiB with this repository's
  dev settings and 2 MiB ... with its release settings". The frames that
  envelope bounds are measured with `-Ctarget-cpu=native` (wider vector
  registers and different spill decisions) and with this host's dev debuginfo
  level. On a host or CI without those flags, frame sizes differ, and nothing
  in the repository pins the flags under which the envelope was verified.
- `src/crmath/` picks the FMA kernel when `target_feature = "fma"` is on or
  detected at runtime. On this host every corpus run (`math.lua`,
  `numerics/math.lua`, the composite's `math.sin`) exercises only the FMA
  path. I did not check whether the crmath unit tests force each backend; if
  they do not, the software path is untested here.
- Hotpath/bench walls are taken with native CPU tuning. That is a valid choice,
  but it is a host property the results rows do not record unless brokkr's
  env fingerprint includes rustflags.

Fix: put the flags the envelope depends on in a repository `.cargo/config.toml`
(or `[profile]` settings) so they travel with the code, or state in
docs/limits.md that the envelope was verified under plantasjen's global
rustflags and add a sweep that runs `native_stack` without them.

### 5.4 `cfg!(debug_assertions)` stands in for the profile

`stack.rs` chooses 4 MiB vs 2 MiB on `debug_assertions` and says so. A
`[profile.release] debug-assertions = true` (or the reverse in dev) silently
selects the other envelope, and the release tests then run against the looser
4 MiB. brokkr's manual warns that this heuristic is unreliable for the same
reason. A `build.rs` can export cargo's `PROFILE` (`debug`/`release`) as a
`cfg` and the constant can key on that; custom profiles would need a rule, but
the two repository profiles would be named rather than inferred.

### 5.5 Writes outside owned locations, summarised

| Writer | Location | Owner | Should be |
|---|---|---|---|
| `diff_test.sh` via `differential_examples` | `.scratch/dellingr-diff-*` | developer scratch | in-memory, or a `ScratchDirectory` under `CARGO_TARGET_TMPDIR` |
| `error_boundary_transcript` (ignored, runs on focused `brokkr test`) | `.scratch/error-boundary/transcript.txt` | developer scratch | a `brokkr run` tool with an output path argument |
| `scratch.rs` killed-test leftovers | `CARGO_TARGET_TMPDIR` on a target dir shared by every project | cargo, shared | same place, plus a reap of stale `dellingr-cli-*` dirs at `ScratchDirectory::new` (pid no longer alive) |
| `regenerate_save_golden` | `tests/fixtures/` | repository (intended) | unchanged, but refuse the wrong profile (1.8) |
| `generate.py` (manual) | `examples/parse/large_source.lua` | repository (intended) | write only when bytes differ, so the mtime does not churn brokkr's mtime-cached hash |

---

## Lateral findings

- `examples/numerics/arithmetic.lua` prints `arithmetic: true` while every
  other examples/ kernel prints its `category/name: true`. bench/ has a footer
  contract test; examples/ does not.
- `run_examples.rs` detects `_bench` files with `function _bench(` or
  `local function _bench(` at line start. A `_bench = function()` spelling
  escapes the success-line rule; `local function _bench` would pass the rule
  while the harness (which needs a global) rejects the file.
- `src/compiler.rs`'s corpus test reads examples with `read_to_string`, so a
  non-UTF-8 example (legal Lua source) would panic there while
  `disassemble_corpus` (which uses `fs::read`) would accept it.
- `feature_test.lua` still carries "SKIP: long strings not implemented yet"
  comments; long strings are in the README's won't-implement list, so "yet" is
  misleading.
- `bench_scripts.rs` compiles bench files with an `@`-prefixed chunk name while
  the harness uses the bare path; harmless, but the test does not load the file
  the way the harness does.
