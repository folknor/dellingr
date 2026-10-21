# Hygiene hunt: tooling, benchmarks and test harnesses

Scope: scripts/**, examples/hotpath.rs, examples/parse/generate.py, brokkr.toml,
bench/, tests/common/, tests/run_examples.rs, tests/diff_test.rs, tests/cli_*.rs,
.gitignore, .review.toml. I followed questions into src/ and Cargo.toml where
they led. These are findings only. The list is unranked, as the hunt asked.

Existing enforcement I read first: the Cargo.toml `[lints]`, clippy.toml's
host-state bans (the harness opts out with a crate-level allow), the src/lib.rs
cfg lints, and `brokkr check` (gremlins, clippy, tests in two feature sweeps,
then two `[[script_check]]` entries). None of it reaches shell or Python. The
only check on tooling text today is `generate.py --check`. So most of what
follows can be enforced only by a test, or by moving the logic into Rust where
the lints apply.

---

## A. Bug-class findings (flagged first because they bite now)

### A1. `brokkr test` can silently rewrite the save golden fixture
`tests/save_golden.rs` has an `#[ignore]`d test,
`deliberately_rewrite_snapshot_fixture_file`, that overwrites
`tests/fixtures/save_golden_current.bin`. AGENTS.md documents that
`brokkr test <NAME>` *always* passes `--include-ignored` and that `<NAME>` is a
substring filter. Any filter that matches this test name runs the regeneration
helper: `save_golden`, `snapshot`, `fixture`, `rewrite`, `deliberately`, and so
on. Within the same sweep, the golden comparison then checks against freshly
written bytes. The format-drift guard defeats itself, and the only trace is a
dirty `.bin` in `git status`. The ignore attribute was the only safety, and the
tool removes it.
Fix: take regeneration out of the test harness. Use an env-gated helper that
refuses without an explicit variable (for example `DELLINGR_REGENERATE_GOLDEN=1`
checked at the top of the test, with a panic otherwise), or a small `examples/`
or `xtask` binary.
Enforcement: a test that walks `tests/` and refuses any `#[ignore]` test that
writes under `tests/fixtures`. Simpler: a rule in brokkr.toml or AGENTS that no
test may write into the source tree, backed by a text check for `fs::write`
outside `CARGO_TARGET_TMPDIR` in `tests/`.

### A2. Benchmark timing includes FIFO writes and backpressure sleeps
In `examples/hotpath.rs`, the warm phase emits `WARM_BLOCK_START`/`WARM_BLOCK_END`
*inside* the `warm_start..warm_total_us` window. Each emit is a syscall on the
FIFO. On `WouldBlock`, `write_fifo_all` sleeps 1ms in a loop, and that sleep is
charged to `warm_avg_us`, the verdict metric. With at most 32 blocks the
syscall cost is small. A stalled reader turns it into whole milliseconds of
fake regression. The retry loop is also unbounded: a reader that keeps the FIFO
open but stops draining hangs the benchmark forever.
Fix: time each block separately and sum only the block bodies, or buffer markers
and flush them outside the timed region. Give the retry loop a deadline after
which the run fails.
Enforcement: structural only. Keep a single timed closure that contains no emit
calls.

### A3. `DELLINGR` env leaks into benchmarks and mislabels them
`scripts/lib.sh::dellingr_bin` returns `$DELLINGR` whenever it is set. That is
the hook diff_test.sh needs, but `bench.sh` and `hotbench.sh` call the same
function. A shell that still has `DELLINGR` exported (for example a debug binary
used for diff runs) makes `bench.sh` time that binary. Its header still prints
`commit: $(git rev-parse HEAD)` and a dirty count, so the output claims a
provenance it does not have. In `hotbench.sh` the two halves diverge: the
harness is always rebuilt at HEAD, while the hyperfine half uses whatever
`DELLINGR` names. This combines an ambient dependency (section 6) with a
resolution rule that has two consumers who want different things.
Fix: give lib.sh two functions, `dellingr_bin_build` (always builds) and
`dellingr_bin_or_override`, and have only diff_test.sh use the override. If an
override is kept for benchmarks, have bench.sh print the binary path and refuse
when it is not the one it just built.
Enforcement: a `bash -n`-level test cannot catch this. A Rust test that runs
`bench.sh --dry-run` with `DELLINGR` set and asserts on the output could.

### A4. bench/ files are never executed by anything except a verdict run
`bench/` is deliberately outside every examples/ glob (AGENTS.md). As a result,
no test, diff run or check parses these files, runs them, or checks their
`bench/<name>: true` line. A bench/ port that stops compiling after a
language-surface change (for example the 0.5.0 newline-paren rule) is found only
when someone runs `brokkr dellingr --lua X --bench`, minutes in. The pairing
invariant ("same kernel, same proportions") is also explicitly unchecked
(brokkr.toml comment).
Fix: add a cheap test that parses every `bench/*.lua` (`parse_str` only, no
execution) and asserts that each defines `_bench`. A static cost-fingerprint
comparison is also possible: `analyze_cost` on the `_bench` scope of the pair.
That would at least prove the *kernel* (the body inside the repeat loop) is the
same opcode mix.
Enforcement: a test.

### A5. Diff timeout is validated at use, once per file
`DELLINGR_DIFF_TIMEOUT` is never validated up front. With GNU `timeout`, a
malformed value (`5 s`, `5sec`) makes every invocation exit 125. That is
reported as `FAIL: <name> (dellingr exited 125)` for every file in the corpus,
which blames dellingr. With the Python fallback, each file prints
`invalid DELLINGR_DIFF_TIMEOUT` and then fails. Either way the operator sees N
false failures instead of one refusal at startup.
Fix: parse the value once before the loop (the Python parser already exists) and
exit 2 on garbage.
Enforcement: a test that runs diff_test.sh with a bad value and expects a single
refusal line.

### A6. Stale or contradictory comments in the harness
- `examples/hotpath.rs` refers to "hotpath 0.15 ... as of 0.16", but Cargo.toml
  pins `hotpath = "0.28.6"`. Following the global documentation rule, reword it
  without version numbers ("hotpath's CountingAllocator must be declared by the
  consumer").
- The `use std::process::exit` comment says "the only caller is brokkr". The
  module doc two screens up and AGENTS.md both say `scripts/hotbench.sh` also
  calls it.
- The module doc says `--iterations N, default 20`, which duplicates
  `DEFAULT_ITERATIONS`. AGENTS.md avoids this by citing the constant. The doc
  comment should do the same.
- `generate.py` says `parser/func.rs stores the count in u8`. The limit is
  `limits::MAX_NESTED` in `src/limits.rs` (func.rs only reads it). See B6.

---

## B. One value, one owner

### B1. Reference-Lua set and install hint: spelled in at least six places
The set "lua5.2 and lua5.4" appears in diff_test.sh (the tool check loop, the two
`run_lua` call sites, and the TIMEOUT labels), tests/diff_test.rs
(`lua_versions_available` spawns each with `-v`, and the assertion text carries
an `apt install` hint), diff_gate.sh's comment, the brokkr.toml comment, and
AGENTS.md. bench.sh has a *different* set (`lua5.5 lua5.4 lua5.2 luajit`), which
is legitimate, but the two lists have no shared definition or naming. The copies
have already drifted in behaviour: the Rust side checks `lua -v` exits zero,
while the shell checks `command -v`. The install hint exists only in Rust.
Fix: one `REFERENCE_LUAS` array in lib.sh, used by both shell scripts. The Rust
test should not pre-check at all, because diff_test.sh already fails loudly with
the right message (and see B2 on whether the Rust test should exist).
Enforcement: structural, by deleting the second site.

### B2. The differential gate is implemented twice and runs the corpus several times per check
`scripts/diff_gate.sh` (a `[[script_check]]` in brokkr.toml, debug build, with
timeouts) and `tests/diff_test.rs` (three buckets that shell out to the same
diff_test.sh, with `DELLINGR_SKIP_TIMEOUT=1`, the test-profile binary) are the
same gate. `brokkr check` runs the test suite once per `[[check]]` sweep
(default and snapshot), so the corpus is diffed against reference Lua at least
three times per check. Depending on how brokkr schedules script_checks, it may
be more. The two copies differ in policy:
- **Timeouts:** the script_check has a 5s per-process timeout. The Rust test
  disables timeouts entirely, so one hanging example hangs `cargo test` with no
  bound.
- **Feature build:** the Rust test also runs under `snapshot`. That is the one
  thing it adds, and it is the reason to keep exactly one of them, parameterised
  over the feature sweep.
- **Bucketing:** `KERNEL_DIRS_A`/`_B` is tuned to "the gate's per-test time
  budget". That budget is a tunable owned by brokkr, in another repository, and
  nothing here names its value. A new kernel directory silently lands in the
  "top-level" bucket. The bucket name is wrong for it, and the time-budget
  reasoning silently stops holding.
Fix: pick one. My recommendation is to delete `tests/diff_test.rs`, and have
diff_gate.sh take the feature set from brokkr if snapshot coverage matters.
Otherwise, delete the script_check and give the Rust test a timeout.
Enforcement: structural.

### B3. Five walkers of the examples corpus with subtly different rules
- `tests/run_examples.rs::collect_lua_files`: relative `examples`, follows
  symlinked dirs through `is_dir`.
- `tests/diff_test.rs::collect_examples::visit`: relative `examples`, follows
  symlinks.
- `src/compiler.rs` test `collect_lua_sources`: `CARGO_MANIFEST_DIR/examples`,
  with a `> 20` vacuity floor.
- `scripts/diff_test.sh::append_lua_files`: `find -type f`, which does *not*
  follow symlinks, with `LC_ALL=C` sort.
- `scripts/bench.sh`: reads brokkr.toml instead (see B4).

The Rust walkers anchor two different ways: cwd-relative, which depends on cargo
setting cwd to the manifest dir, versus `CARGO_MANIFEST_DIR`. Their vacuity
guards differ too (`!is_empty` versus `> 20`). The sort orders also differ: Rust
`PathBuf` component order versus C-locale byte order on the whole string. Today
these choices agree only because the tree has no symlinks and no names where the
orders diverge.
Fix: one `tests/common::example_corpus()` that returns the sorted list anchored
at `CARGO_MANIFEST_DIR`, used by every Rust site. The compiler unit test cannot
reach `tests/common`, so it needs a `#[cfg(test)]` helper in src/ or a move to an
integration test. The shell walker remains a necessary second copy for as long
as diff_test.sh exists, with a test that the two lists match.
Enforcement: the parity test.

### B4. bench.sh hand-parses brokkr.toml
bench.sh treats `brokkr.toml` as line-oriented `key = "value"`. It breaks on
single-quoted strings, inline `# comments`, or a `hotpath_file` key in another
table. That is a second TOML parser for a file whose owner (brokkr) has the real
one. Python 3.11+ ships `tomllib`. Better: ask brokkr for the workload list, if
it has or gains a `brokkr dellingr --list` command.
Enforcement: use `tomllib`. The dependency on Python is already there.

### B5. Three naming schemes for the same benchmarks
brokkr addresses workloads by registry name (`--lua arithmetic`). hotbench.sh
takes `category/name` and builds `examples/${TARGET}.lua`. bench.sh takes file
paths. AGENTS.md's prose has to teach all three, and nothing maps a brokkr
workload name to a hotbench argument.
Fix: make hotbench.sh accept a workload name and resolve `hotpath_file` from
brokkr.toml, which also enforces AGENTS.md's "never instrument a bench/ file"
rule by construction. The current `examples/` prefix enforces it only by
accident.

### B6. generate.py restates `limits::MAX_NESTED` and the 200-local limit
`MAX_NESTED_FUNCTIONS = (1 << 8) - 1` duplicates `src/limits.rs::MAX_NESTED`.
The docstring's "200-local-per-function limit" refers to reference Lua.
dellingr's `MAX_LOCALS` is 255. Neither copy is checked against the source.
This duplication is forced: Python cannot import Rust. What keeps the copies in
step today is nothing, except that `run_examples` would fail on a generated file
that exceeds the limit, and that only catches an increase in FUNCTION_COUNT, not
a decrease in the limit below 200. That is still caught, because the example
would fail to parse.
Fix: acceptable as is if the comment cites `limits.rs`, or have `--check`
grep the constant from `src/limits.rs`.

### B7. Footer call count, `: true` sentinel and `_bench` name are per-file conventions
Every bench/ and examples/ kernel spells `for i = 1, 30 do _bench() end` and
`print("<name>: true")` by hand. The harness hard-codes `_bench`. run_examples
hard-codes `": false"`. AGENTS.md says "footer fixed at 30 calls". Each file is
a separate site for that rule. Nothing checks that a bench/ file's footer is 30
(it matters: the harness runs the footer during SETUP, so a different count
shifts SETUP, not WARM), or that a file prints anything at all. **A kernel that
prints nothing passes run_examples.**
Fix: in a test, assert per kernel that the footer line matches and that stdout
contains exactly one `: true` line. Keep `_bench` as a named const shared by the
harness and that test.

### B8. Two copies of `ScratchScript`; `tests/common` bypassed by half its intended users
`tests/cli_arguments.rs` and `tests/cli_limits.rs` each define
`ScratchScript` (with near-identical Drop logic). Both call
`env!("CARGO_BIN_EXE_dellingr")` directly, although `tests/common::dellingr_bin`
exists precisely to own that ("Tests must use this"). Only `run_examples.rs` and
`diff_test.rs` use common.
Fix: move `ScratchScript`/`ScratchDirectory` and `run` into `tests/common`.
Enforcement: a text check that `CARGO_BIN_EXE_` appears only in
`tests/common/mod.rs`.

### B9. Exit-code vocabulary
The CLI is always 1 (documented in main.rs). The harness uses 1 and 2. The
scripts use 1 and 2 for usage, 124 for timeout, and 125 for a bad timeout. The
Python fallback maps signals to `128 - returncode`. Each set is documented
locally, which is fine. The one interaction that matters: diff_test.sh treats
dellingr status 124 as "TIMEOUT". That holds only because the CLI never exits
124. This is an unstated cross-file assumption. A comment in diff_test.sh citing
main.rs's exit policy would make it explicit.

---

## C. Values nobody can find, change, or trust

### C1. No inventory of tooling knobs
These are the tunables for this scope. Nothing lists them together:
- **Env vars:** `DELLINGR`, `DELLINGR_SKIP_BUILD`, `DELLINGR_DIFF_TIMEOUT`,
  `DELLINGR_SKIP_TIMEOUT`, `DELLINGR_TEST_LIMIT_KB`, `FEATURES` (hotbench), and
  `BROKKR_MARKER_FIFO`.
- **Constants:** `DEFAULT_ITERATIONS`, `MAX_WARM_BLOCKS`, the hyperfine
  `--warmup 2` (spelled in both bench.sh and hotbench.sh), the 5s diff timeout,
  the 2 GiB vm cap, the bucket directory lists, `FUNCTION_COUNT`, and the
  30-call footer.

AGENTS.md covers most of the env vars but omits `DELLINGR_SKIP_BUILD` (which
diff_test.sh documents). That is the documented set already diverging from the
real one.
Fix: make lib.sh the one place shell knobs are resolved, with a header comment
listing them. The `--warmup 2` belongs there as `HYPERFINE_WARMUP`.

### C2. `DELLINGR_SKIP_TIMEOUT` is a "legacy" switch with a live caller
diff_test.sh calls it "legacy" and offers `DELLINGR_DIFF_TIMEOUT=0` as the
replacement, yet `tests/diff_test.rs` is its active user and AGENTS.md documents
it as current. The two spellings mean almost the same thing, but not exactly:
SKIP also skips the timeout-tool presence check. That is two knobs with one
meaning, plus a subtle extra difference.
Fix: delete SKIP_TIMEOUT. Make `DIFF_TIMEOUT=0` skip the tool check, and move
the Rust caller over (or delete the Rust caller, per B2).

### C3. `FEATURES` is an unprefixed, generic env name
In hotbench.sh, an ambient `FEATURES` from CI or another tool silently changes
which harness is built. `${FEATURES:-hotpath}` also makes "no features"
unrepresentable: an empty value becomes `hotpath`. Use
`DELLINGR_HOTPATH_FEATURES` or a `--alloc` flag.

### C4. The diff and example runners have no injectable bound
run_examples has no per-example timeout and no memory cap. Worker count is
`available_parallelism()`, so the host's core count decides how many stress
examples run at once. The test binary runs concurrently with the diff buckets,
so the box is oversubscribed. `test_limited.sh` exists "for stress tests", but
nothing invokes it, including run_examples, which *does* run the stress
examples. Either the cap is not needed (and test_limited.sh is dead, see E2), or
the policy it encodes is not applied where the stress examples actually run.

---

## D. Channels, errors and output

### D1. bench.sh swallows hyperfine's failure reason
`hyperfine ... >/dev/null 2>&1`. On failure the table shows `FAIL`, and the
reason (which Lua failed, its stderr) is gone. No JSON is written on failure.
Fix: send stderr to `$results_dir/$run_index.err` and print that path in the
end summary.

### D2. Diff failures name no detail
diff_test.sh's `FAIL: <name>` gives no hint which reference was closer, and no
diff. The temp outputs are deleted by the EXIT trap. The terse format is
deliberate (the brokkr last-line match), but the actionable detail could go to
stderr or a kept `.scratch/diff-fail/<name>.{ours,52,54}` without disturbing
stdout.

### D3. `-- DIFF:` files still run both references, and their failures count
For `has_diff=1` files the script runs lua5.2 and lua5.4 and only then skips
the comparison. A reference *timeout* on a DIFF file (a dellingr-only construct
that loops in reference Lua) fails the gate, although the file is declared
divergent. Reference non-zero exits are ignored everywhere, so the policy is
inconsistent across failure kinds. The grammar is inconsistent too:
`SKIP-DIFF` requires a reason (`'^-- SKIP-DIFF: .'`), but `DIFF` does not
(`'^-- DIFF:'`).
Fix: skip the reference runs for DIFF files, and require a reason for both
markers.
Enforcement: a test that greps the corpus for empty-reason markers.

### D4. bench.sh calls python3 before checking for it
The default script list is computed with `python3 -c` *before* the tool loop
that reports `missing tool: python3`. Without python3, the result is "no
benchmark scripts found" plus a shell error, not the clean message.

### D5. Harness counters saturate silently
`counter_value` maps overflow to `i64::MAX`, and `warm_avg_cost` truncates by
integer division. Both are harmless at today's scales. Saturation should at
least be a refusal: a counter at `i64::MAX` is a lie in the results DB.

---

## E. Code that is no longer load-bearing

### E1. `scripts/notes_drop.py`
It has no caller in the repo and is not mentioned in AGENTS.md, CLAUDE.md,
README, CHANGELOG or notes/fix-loop.md. It edits `## <ID> - ` sections of
findings documents, the tool behind the "final drain of the findings documents"
in 7996ccf. What tells me it is dead: there are zero references, and the
documents it drained no longer exist. The hunt reports now landing in notes/
may make it relevant again. Decide based on whether the new hunt reports adopt
its `## ABC-123 - ` heading format. If kept, document it in AGENTS.md's scripts
section.

### E2. `scripts/test_limited.sh`
Its only reference is the AGENTS.md command list. No script, test or brokkr
entry invokes it, and run_examples runs the stress examples without it (C4).
`ulimit -v` is also a poor cap on modern allocators, which reserve large virtual
ranges. It is dead unless someone runs it by hand. Either wire it into the
stress path or delete it.

### E3. `.gitignore` entries for things not in this project's workflow
`overnight.sh`, `overnight-logs` and `.plans` (plans/ is retired per AGENTS.md)
match nothing any tracked tool writes. Unless a local workflow still produces
them, they are fossils. They are also harmful in a quiet way: they hide such
files from `git status` if they ever reappear.

### E4. `research/` is ignored, yet README numbers depend on a script in it
AGENTS.md describes `research/bench_luars.sh` as the source of the README's
`lua-rs` column. `research` is gitignored wholesale, and `git ls-files research`
is empty. The published numbers therefore come from a script that is not in
version control and exists on one host. Fix: track `research/bench_luars.sh` (or
move it to `scripts/`) and ignore only `research/lua-rs/`.

### E5. diff_test.sh's portable-timeout fallback chain
`timeout` -> `gtimeout` -> a 20-line embedded Python reimplementation of GNU
duration parsing. That third implementation is load-bearing only on a host with
no coreutils, and the project already requires GNU-ish tools (`find`,
`mktemp`, `bash`). If macOS support is not a goal, the Python path is dead
weight that has to be kept in step with `timeout(1)` syntax by hand.

---

## F. Host assumptions and where tools write

### F1. Writes
- diff_test.sh writes `mktemp` files under `.scratch/`, cleaned by an EXIT
  trap. SIGKILL leaks them. That is acceptable: `.scratch/` is the documented
  project scratch.
- bench.sh writes `.scratch/bench-results/<run_id>/` on every run and **never
  prunes it**. Growth is unbounded and nothing reads old runs. This conflicts
  with the brokkr results DB as the owner of benchmark history.
  Fix: either keep only the last N runs or treat `.scratch/bench-results/` as
  disposable and say so.
- cli tests write into `CARGO_TARGET_TMPDIR`. That is cargo's sanctioned
  per-test scratch, and the right location even though `target/` is a symlink
  to a shared cache. Names are `dellingr-cli-<name>-<pid>`. `ScratchDirectory`
  uses `create_dir`, so a directory leaked by a killed run with a recycled pid
  makes the test fail with "should be created". Fix: remove the directory first,
  or add a random or monotonic suffix.
- A1 is the one write into the source tree (`tests/fixtures/`), which should
  never be reachable from a normal test invocation.

### F2. Host assumptions
- **Reference Lua and tools on PATH under Debian names** (`lua5.2`, `lua5.4`,
  `lua5.5`, `luajit`, hyperfine, jq, python3, xxhsum). On distros that ship
  `lua52` or `lua-5.4`, the result is a loud refusal, which is acceptable. A
  `LUA52=`/`LUA54=` override in lib.sh would make it detectable rather than
  assumed.
- **Core count:** run_examples takes it from `available_parallelism` (C4). That
  is OK as detection, but there is no override for constrained CI.
- **`ulimit -v`** semantics in test_limited.sh are Linux-specific (E2).
- **`./scripts/diff_test.sh` exec bit:** tests/diff_test.rs runs the script
  directly, while brokkr.toml runs `bash scripts/diff_gate.sh`. A checkout that
  loses mode bits (a Windows-side or archive extraction) breaks only the Rust
  path. Use one invocation style. `bash <script>` is the robust one.
- **cwd:** the Rust walkers rely on cargo's cwd = manifest dir (B3). The scripts
  anchor themselves, which is good. `test_limited.sh` deliberately does not, and
  that is documented.
- **The hotpath marker protocol is unix-only.** The harness refuses on other
  platforms when the FIFO is set, which is a clean refusal.

---

## G. Smaller smells noticed along the way

- **brokkr.toml `[test] debug = true` versus AGENTS.md:** AGENTS.md says
  `brokkr test` "always passes `--release`" and offers `--debug` as opt-in. One
  of the two is stale, or the key means something else. Clarify which.
- **The brokkr.toml comment** says diff_gate prints `FAIL: <path>`. The actual
  lines are `FAIL: <name>` (examples-relative), `FAIL: <name> (dellingr exited
  N)` and `TIMEOUT: ...`. Reword it so the comment does not enumerate formats.
- **large_source** registers the same ms-scale file for `--bench`. The exception
  is documented, but the stated rationale for bench/ ("seconds-scale so wall
  deltas resolve above launch noise") then does not hold for its verdict walls.
  Only `parse_us` is meaningful there, and nothing in the results flow marks
  that.
- **`run_feature_test_extended_in_process`** hard-codes one example path that
  the corpus walker already covers. It is a sixth site that knows the examples
  layout.
- **The harness exits on error paths without dropping the hotpath guard.** That
  is intended (the stats table would be partial), but it means an `--alloc` run
  that fails late reports nothing about where it was.
