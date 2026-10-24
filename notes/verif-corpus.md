# Verification: corpus and harness

Findings from the verification hunt about everything that runs the
`examples/` and `bench/` corpora: the integration crate's `corpus` subject
(`run_examples.rs`, `bench_scripts.rs`, `disassembly.rs` under
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

## VC-003 - What `run_examples` accepts as a pass, and the DIFF/SKIP-DIFF files nothing else checks

Reported by: check-machinery, corpus-harness, library.

An example passes when it exits 0, writes no stderr and prints no line ending
in `: false`; a `_bench` file must also print a line ending in `: true`. Under
that rule a condition that evaluates to `nil` prints `name: nil` and passes,
and a non-bench example that prints nothing passes.

A `-- DIFF:` or `-- SKIP-DIFF:` marker removes the reference comparison for the
whole file. The corpus test then checks of a DIFF file only that its output
matches neither reference, which any single divergent line satisfies, so for
these files the `: false` rule is all that remains. One divergence removes
every other assertion in the file from differential coverage. Several marked files print in shapes that rule never sees:

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
- `examples/stress_tables.lua`, `stress_rustfn.lua`, `stress_gc_stack.lua`
  (SKIP-DIFF): progress banners and an unconditional `PASS`. Smoke tests by
  design, but nothing records that.
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

## VC-011 - Timeouts, the 20-second cap and hard-coded concurrency do not nest

Reported by: check-machinery, corpus-harness.

`run_all_examples` (`tests/integration/corpus/run_examples.rs`) is still one
test running the entire corpus: each example through dellingr and, unless
SKIP-DIFF, both reference Luas, on `min(8, available_parallelism, files)`
in-test workers. brokkr's per-test cap is a hard 20 seconds (`brokkr man
hygiene`), so the test carries one `CORPUS_DEADLINE` of 15s, const-asserted
5s below a restated `BROKKR_PER_TEST_CAP`, and on expiry names every example
unfinished (and in which process) or not started. That gives attribution, but
the whole corpus still has to fit inside one test's cap on every host: on a
host with few cores, or under heavy load, the fixed deadline is reached and
the gate fails, and one slow example costs the verdicts of every example not
yet started. Measured on plantasjen (2026-10-24, uncommitted tree on
9746eb7): 4.96s in the default sweep and 4.82s in the snapshot sweep of
`brokkr check`; under `brokkr test`, 11.5s in the hotpath-instrumented
`instrumentation-faults` shape (comparison compiled out), which `brokkr check`
filters it out of, so that margin is only exercised by focused runs.

Enforceable: the deadline/cap assertion is a convention, since brokkr cannot
export its cap.

Decision (2026-10-24), remaining step: split the test into one `#[test]` per
example, generated by a macro from a checked-in list of example paths
(`check_example(path, deadline)` already takes one path). The list becomes
the single enumeration: every consumer of today's `example_corpus()` (the
compiler unit test, `cache_differential`, `corpus_roundtrip`,
`cost_check_interval`, `bench_scripts`, `run_all_examples`) reads it, and a
`[[script_check]]` keeps it equal to the `.lua` files on disk. Each example
then has a name, a focused `brokkr test` run and brokkr's own 20s cap, which
replaces the corpus deadline and the in-test worker pool. Open cost: brokkr
runs a sweep's tests serially unless the sweep sets `test_threads`, so the
kernels lose the in-test worker pool.

## VC-019 - Tests that spawn tools rather than the product

Reported by: check-machinery, corpus-harness.

The `test-processes-through-support` textlint rules in `brokkr.toml` keep
every process a Rust test starts in `tests/integration/support/process.rs`
(the CLI and the reference Luas), except two `python3` spawns that carry a
`process-spawn-exception:` marker: `tests/common/libm_symbols.rs` runs
`scripts/libm_symbols.py` (which runs `nm`) on a linked CLI binary, and
`src/crmath/tests/golden.rs` runs `scripts/crmath_tables.py --check-pins` (and
`--check` under `crmath-sweeps`). Undecided: keep them as test-time spawns,
port them to Rust, or move them to script checks (the libm one needs the
binary of the sweep that built it, which is why it is a test). When decided,
drop the marker and the rule's exception with them.

## VC-020 - The corpus walk is a filesystem walk, so untracked files join the gate

Reported by: corpus-harness.

The gate enumerates the corpus with `tests/common/examples.rs`, a filesystem
walk (`sorted_lua_files` in `lib.sh` walks it the same way, for the manual
`diff_test.sh` only). An untracked `.lua` dropped into `examples/` (a scratch
repro, say) joins every gate on that host and no other.

Fix: walk `git ls-files examples` (adds a git dependency), or treat untracked
files in `examples/` as a test failure with a clear message.

Enforceable: yes.

Decision (2026-10-24): resolved by VC-011's second step. The checked-in list
replaces `example_corpus()` for every consumer, so no Rust code walks
`examples/`; a `[[textlint]]` forbids `read_dir` on the examples tree and the
`example_corpus` name. The list's `[[script_check]]` compares it with the
`.lua` files on disk, so a stray untracked file fails the check until it is
listed or removed. No git dependency.
