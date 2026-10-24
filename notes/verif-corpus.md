# Verification: corpus and harness

Findings from the verification hunt about everything that runs the
`examples/` and `bench/` corpora: the integration crate's `corpus` subject
(`run_examples.rs`, `diff_test.rs`, `bench_scripts.rs`, `disassembly.rs` under
`tests/integration/corpus/`), `scripts/diff_test.sh`, `examples/hotpath.rs`,
the `cli` subject, `tests/integration/support/` and `tests/common/`, and the
Lua files themselves. Several entries would be removed
by the in-process gate and per-example test rewrites in rewrites.md; they stay
filed here until then. Each entry names the scopes whose hunters reported it.

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

## VC-001 - 36 of the 39 kernel examples print a constant, so the diff gate compares nothing on them

Reported by: corpus-harness.

Every `.lua` under an `examples/` subdirectory except `tables/sort.lua`,
`tables/queue.lua` and `tables/metamethod_sequence.lua` ends with an
unconditional `print("<name>: true")` and contains no `error(` call and no
conditional output. `_bench()` returns a value (a checksum, in most files) and
the footer discards it. So:

- `run_all_examples`' `: true` requirement for `_bench` files is satisfied by
  the literal string; it proves only that the footer was reached.
- `differential_examples` compares dellingr with lua5.2/lua5.4 on these files,
  but the output is the same constant line whatever the kernel computed. If
  `fib(20)` returned 0 or the arithmetic kernel produced NaN, all three runs
  would print `arithmetic: true` and the gate would pass.
- These are the files `diff_test.rs` deals into six of its seven partitions
  because they are slow ("about a second each in the unoptimized test
  profile"), so most of the differential gate's wall time is spent on files
  whose compared output cannot differ.
- `examples/numerics/arithmetic.lua` prints `arithmetic: true` while every
  other kernel prints `category/name: true`; bench/ has a footer contract test,
  examples/ does not.

Fix: make the footer print the result, e.g.
`print("numerics/arithmetic: " .. tostring(_bench()))`, or fold the 30 results
into a checksum. Floats render the same in 5.2 and dellingr, so a 5.4-only
`.0` difference falls under the existing either-reference rule.

Enforceable: yes. Add to `bench_scripts.rs` (or a new examples-footer test) a
rule that an examples/ `_bench` file's success line is not a string literal (it
must concatenate a value), mirroring the bench/ footer contract.

## VC-003 - What `run_examples` accepts as a pass, and the DIFF/SKIP-DIFF files nothing else checks

Reported by: check-machinery, corpus-harness, library.

An example passes when it exits 0, writes no stderr and prints no line ending
in `: false`; a `_bench` file must also print a line ending in `: true`. Under
that rule a condition that evaluates to `nil` prints `name: nil` and passes,
and a non-bench example that prints nothing passes.

A `-- DIFF:` or `-- SKIP-DIFF:` marker removes the reference comparison for the
whole file (11 files: 5 SKIP-DIFF, 6 DIFF). The diff gate then checks only that
a DIFF file exits 0 (the `has_diff` branch), which repeats `run_examples` with
a weaker condition, so for these files the `: false` rule is all that remains.
One divergence removes every other assertion in the file from differential
coverage. Several marked files print in shapes that rule never sees:

- `examples/math.lua` (DIFF, number formatting and RNG): prints `label =\t<value>`
  via `print(a, b)`. No line can end in `: false`; every `math.sin(...)` and
  `math.random()` value is printed and compared against nothing.
- `examples/edge_cases.lua` (DIFF, because 0/0 renders `nan` where C Lua prints
  `-nan` on x86-64): about 70 checks as
  `print("10 < 9 = " .. tostring(10 < 9))  -- false`, expected values only in
  comments, `= false` is not `: false`, and the closing banner is
  unconditional. One divergent line disables comparison of the other ~70.
- `examples/upvalue_stress.lua` (SKIP-DIFF): ~30 value prints with expected
  values in comments, banner unconditional; nothing in it looks
  dellingr-specific.
- `examples/stress_tables.lua`, `stress_rustfn.lua`, `stress_gc_stack.lua`,
  `examples/benchmark.lua` (SKIP-DIFF): progress banners and an unconditional
  `PASS`. Smoke tests by design, but nothing records that.
- `feature_test.lua` is excluded for a global `unpack`,
  `feature_test_extended.lua` for semicolons.

Fixes, in the hunters' order of preference: split the divergent statement into
its own small DIFF file and drop the marker from the rest (`edge_cases.lua`:
move the `0/0` line out; `math.lua`: move the random and float-rendering lines
out); drop markers that are not needed (VC-004); convert value prints to the
`name: true/false` convention or `error()` on mismatch as `feature_test.lua`
does; or give DIFF files a checked-in `.expected` fixture compared byte for
byte (the expected-output rewrite in rewrites.md). Short of that,
`run_examples` could require every non-blank stdout line of a `name: value`
example to end in `: true`.

Enforceable: yes. In `run_examples.rs`, require every DIFF/SKIP-DIFF file to
print at least one line matching `: (true|false)$` or call `error(`, unless it
is on an explicit smoke-test allowlist; or require every DIFF file to have an
`.expected` sibling.

## VC-007 - The fingerprint cannot see a kernel's scale, and the composite's "same proportions" claim is false

Reported by: corpus-harness.

`cost_fingerprint` captures op-class counts, instruction counts and nesting.
Loop bounds are literals, so changing `for i = 1, 50000` to `for i = 1, 5000`
inside a kernel leaves the fingerprint unchanged. `brokkr.toml` says wrapper
repeat counts are "re-checked by eye"; kernel bounds are not covered either.

`examples/composite/benchmark.lua`'s header says "Same kernels in the same
proportions" as `bench/benchmark.lua`. Every section is scaled by 1/100 except
section 3: `fib(19)` makes 13,529 calls against `fib(28)`'s 1,028,457, a ratio
of 76, not 100. Recursive-call share in the hotpath file is about 1.3x what it
is in the verdict file, so per-function percentages from `--hotpath` on
`benchmark` do not carry over for the call path, the path most recent
optimization work targets.

Fix: compare `Program::disassemble()` text per function (literals included)
for the ordinary pairs, whose kernel bodies are literally identical; keep the
composite as the documented exception and fix its header, or pick a scale where
the claim holds for every section.

Enforceable: yes, a disassembly-equality assertion in `bench_scripts.rs`.

## VC-011 - Timeouts, the 20-second cap and hard-coded concurrency do not nest

Reported by: check-machinery, corpus-harness.

`run_examples.rs` gives each example `EXAMPLE_TIMEOUT_MS = 60_000`, and AGENTS.md
repeats "with a fixed 60-second timeout". brokkr's per-test cap is a hard 20
seconds and stops the whole run on a breach (`brokkr man hygiene`), and
`run_all_examples` is one test running the entire corpus. So a hung example is
killed by brokkr's watchdog at 20s, never by the runner at 60s; the runner's
diagnostic (which example, its output) is never produced, and brokkr reports
`run_all_examples` as the suspect. The whole corpus, all workers, must finish
inside 20s, and nothing in the repository states that budget.

`differential_examples` runs seven `diff_test.sh` partitions concurrently inside
one test sharing the same 20s, with `DEFAULT_DIFF_TIMEOUT=5s` per process from
`lib.sh`. `run_examples` uses `min(8, available_parallelism, files)` workers;
`diff_test.rs` always runs `KERNEL_PARTITIONS + 1 = 7` pipelines regardless of
core count, each running dellingr and two references serially. On plantasjen
(12 physical cores) both have headroom. On a 4-core host the seven partitions
oversubscribe, the debug kernels (about a second each unloaded) approach the 5s
timeout, and the result is `TIMEOUT` failures or a cap breach that read as
regressions or hangs. The partition rule (subdirectory means kernel, top level
means fast) is a naming heuristic: `tables/metamethod_sequence.lua` is a fast
correctness file classified as a kernel; `examples/benchmark.lua` is a heavy
top-level file (SKIP-DIFF, so it happens not to matter).

The per-test cap (20s), per-example timeout (60s), per-process diff timeout
(5s), poll interval (25ms) and partition count (6) are independent constants in
three files and two languages; their real relationship is written nowhere.

Fix: one test per example (the per-example rewrite in rewrites.md), each with
its own cap and name; short of that, derive per-example timeouts from one
constant below 20s, derive the partition count from `available_parallelism`,
and partition by measured or declared weight. Reword AGENTS.md to state the
actual bound.

Enforceable: a constant shared by both tests and asserted below a named
`BROKKR_PER_TEST_CAP_SECS` documents the coupling; brokkr cannot export its cap,
so that is a convention, not a proof.

## VC-012 - The 30-call standalone footer is paid by every check and every verdict launch

Reported by: corpus-harness.

Every examples/ kernel ends with `for i = 1, 30 do _bench() end`, "enough for
hyperfine resolution", serving `scripts/bench.sh`'s ratio table. The test suite
pays it every run: each of the 36 kernels runs 30 times in a debug build, in
`run_all_examples` and again in `differential_examples` (plus 60 more reference
executions), in both dev sweeps. At "about a second each" (diff_test.rs's own
estimate) that is on the order of 150 CPU-seconds per check, repeating kernels
whose output is a constant (VC-001). The whole test phase of the latest check
took 55.2s wall across four sweeps.

The verdict harness pays it too: `examples/hotpath.rs` runs the top level during
setup, so a `bench/` file's footer runs 30 seconds-scale calls before the cold
call. In the forced `method` run at 7375bb4 on plantasjen, `elapsed_ms=3900.867`
against `cold_call_us=75213` plus 20 warm calls at `warm_avg_us=75811.782`
(about 1.59s): roughly 2.3s, about 59% of the launch, is untimed footer.
`--bench 3` spends about 7s per workload on it.

Fix: make the footer count an input. Chunks receive script arguments as `...`
in both dellingr (if the CLI forwards them; today it rejects extra positionals)
and reference Lua, so `local n = tonumber((...)) or 1` with `bench.sh` passing
30 keeps hyperfine resolution where wanted and drops the footer to one call in
tests and zero in the harness (the harness can set a global before setup).

Enforceable: `bench_scripts.rs` already pins the bench/ footer text; extend it
to the examples/ footers.

## VC-013 - `run_examples` polls the child every 25ms

Reported by: check-machinery, corpus-harness.

`run_example` polls `try_wait` every 25ms, adding a mean 12.5ms (up to 25ms) per
example and spinning a thread. A waiter thread with a channel and
`recv_timeout`, or a blocking wait with a deadline, removes the sleep. The
timeout itself is a real external-process deadline and cannot be virtualised.

Enforceable: no; small, listed because the brief asks for every sleep that
stands in for an event.

## VC-015 - `.scratch/` is written by several tools, leaks, and has no owner

Reported by: check-machinery, corpus-harness, native-stack-errors.

| Writer | Location | Problem | Should use |
|---|---|---|---|
| `scripts/diff_test.sh` via `differential_examples` | `.scratch/dellingr-diff-*` (three `mktemp` files per invocation, top level) | removed by an `EXIT` trap that brokkr's SIGKILL never runs; 18 leaked files now (six sets from 2026-10-21 and 22), one of them 70,691,826 bytes; bypasses the scratch convention because the write is in a shell child | no files (the in-process gate rewrite), or `--scratch <dir>` with `diff_test.rs` passing a `ScratchDirectory` under `CARGO_TARGET_TMPDIR`, or `.scratch/diff-test/` cleaned at start |
| `scripts/bench.sh` | `.scratch/bench-results/<run>/` | fine: owns its subdir and prunes by pattern (GNU `find -printf`) | as is |
| `error_boundary_transcript` via `scratch::write_boundary_transcript` | `.scratch/error-boundary/transcript.txt` | fixed path in a dir with many owners, last writer wins, nothing owns or cleans it; breaks the scratch helper's own rule; runs on focused `brokkr test` (bugs.md) | a `brokkr run` tool with an output path argument, or a committed golden (the transcript entry in verif-assertions.md), or `CARGO_TARGET_TMPDIR/error-boundary/` through `write_scratch_file` |

`.scratch/` is the developer's scratch area (patches, perf data, notes; about
200 entries today) with several tool owners (`research/bench_luars.sh` now also keeps its JSON
under `.scratch/bench-luars/`), and nothing says which
subtree belongs to whom; a cleanup by one tool can destroy another's state. The
native-stack hunter notes the transcript's stated reason for the exception
("must outlive the test process") does not hold: `CARGO_TARGET_TMPDIR` also
persists across runs until `cargo clean`.

Enforceable: a textlint rule over `scripts/**` and `tests/**` requiring
`.scratch/<owner>/` and never bare `.scratch/` names; deleting the shell layer
is the real enforcement for the diff gate.

## VC-016 - `CARGO_TARGET_TMPDIR` lives in a target directory shared by every project

Reported by: check-machinery, corpus-harness.

`CARGO_TARGET_TMPDIR` is `/media/folk/Banan/cargo/tmp`, inside a target
directory shared by every project on the host (the `target` symlink points at
`/media/folk/Banan/cargo/`, whose `tmp/` holds other projects' fixtures).
`scratch.rs` handles collisions (`dellingr-cli-` prefix, pid, counter,
`AlreadyExists` retry), so this is sound, but isolation is the prefix alone,
another project's `cargo clean` wipes it, and a test killed by brokkr's SIGKILL
at the 20s cap leaks its directory into shared space with no reaper.

Fix: keep the location, state that the prefix is the isolation, and reap stale
`dellingr-cli-*` dirs (pid no longer alive) in `ScratchDirectory::new`.

Enforceable: n/a.

## VC-018 - The gates depend on host tools resolved from `PATH`, unversioned

Reported by: check-machinery, corpus-harness.

- `lua5.2` and `lua5.4` under exactly those names (Debian naming). Which patch
  release, and which distro compat flags (`LUA_COMPAT_ALL` changes the globals
  5.2 has), decides which reference "matches"; a patch release that changes
  `%g` rendering or an error message changes the gate's verdict with no
  repository change. `research/lua-5.2` and `research/lua-5.4` are local
  checkouts nothing builds from.
- `bash`, GNU `timeout`, `grep`, `diff`, `sed`, `sort`, `find`, `mktemp`.
- `python3` for `generated-large-source` and `native-call-inventory` (3.11+ for
  `tomllib`).

Fix: the in-process expected-output gate (rewrites.md) removes the reference
Lua and coreutils from the standard check and confines reference versions to the
refresh script, which records `lua5.x -v` beside the outputs.

Enforceable: yes, with recorded versions checked by the refresh tool.

## VC-019 - Tests of the shell tool rather than the product

Reported by: check-machinery, corpus-harness.

`malformed_timeout_is_rejected_once_before_other_checks` and
`shell_and_rust_example_corpora_match` test `diff_test.sh`'s argument parser and
its `find | sort` walk; they exist because the gate is split between a Rust
walker and a shell walker that must agree. Every `diff_test.sh` run from
`corpus::diff_test` now passes `--bin` (the timeout probe passes `/dev/null`),
so none can fall through to a build, but nothing requires the next one to.

Fix: the in-process gate rewrite deletes the need for both tests.

Enforceable: a textlint rule can require `--bin` on every `diff_test.sh`
invocation in `tests/`.

## VC-020 - The corpus walk is a filesystem walk, so untracked files join the gate

Reported by: corpus-harness.

The corpus is enumerated by `tests/common/examples.rs` and by
`sorted_lua_files` in `lib.sh`, both over the filesystem. An untracked `.lua`
dropped into `examples/` (a scratch repro, say) joins every gate on that host
and no other.

Fix: walk `git ls-files examples` (adds a git dependency), or treat untracked
files in `examples/` as a test failure with a clear message.

Enforceable: yes.

## VC-023 - Workload headers do not classify themselves consistently

Reported by: check-machinery, corpus-harness.

AGENTS.md now points at the script headers for the probe classification
instead of listing probes, but the headers are not uniform: some open with
`-- Probe:`, `calls/many_literals` with "Historical probe", and
`globals/write` with a stale `-- Hypothesis:` (its stale header is tracked in
notes/todo.md, since rewording a pinned file cuts its history).

Fix: a test requiring every registered `hotpath_file` to open with one of a
fixed set of classification prefixes (`-- Probe:`, `-- Historical probe:`,
`-- Workload:`), applied when the pinned headers are next re-hashed.

Enforceable: yes, by that test.
