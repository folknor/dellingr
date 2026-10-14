# Hygiene hunt: tooling

Scope: `src/main.rs`, `examples/hotpath.rs`, `scripts/`, `brokkr.toml`,
`examples/parse/generate.py`, `.review.toml`, `clippy.toml`, `.gitignore`,
the features/profile/lints parts of `Cargo.toml`, and the shelling-out tests
(`tests/run_examples.rs`, `tests/diff_test.rs`, `tests/cli_limits.rs`,
`tests/save_golden.rs`).

Each finding ends with **Enforce:** saying whether the fixed version can be
held mechanically.

---

## A. Which binary gets tested (correctness bugs, not only hygiene)

### A1. `run_examples.rs` prefers whatever `target/release/dellingr` is on disk
`run_example` checks for `target/release/dellingr` relative to cwd first, then
falls back to `CARGO_BIN_EXE_dellingr`, then to `cargo run`. The release binary
is not a build product of this test run. `brokkr.toml` sets `[test] debug =
true`, so the gate compiles a debug binary and then runs every example against
a release binary from whenever someone last did a release build (diff_test.sh,
bench.sh, hotbench.sh or `brokkr run --release`). A VM change that breaks an
example passes the gate as long as an older, correct release binary is still
there. And it fails the other way too: an old broken release binary fails a
fixed tree. `target/` is a symlink into a shared cargo cache (AGENTS.md), so
the stale binary can come from a different checkout or commit entirely. The
`cargo run` fallback can never be reached, because cargo always sets
`CARGO_BIN_EXE_dellingr` for integration tests of a crate with a bin target.
So it is dead code, and it shells out to raw cargo.
Fix: use `env!("CARGO_BIN_EXE_dellingr")` and nothing else.
**Enforce:** yes. A text rule or test that bans `"target"` path literals under
`tests/`, or a single shared `tests/common/mod.rs::dellingr_bin()` that every
harness must call.

### A2. `diff_test.rs` builds a release binary with nested raw cargo, then trusts a hard-coded path
`ensure_release_binary` runs `cargo build --release` from inside the test
process. That is a fat-LTO build, codegen-units=1, in the middle of
`brokkr check`. It inherits `CARGO_TARGET_DIR` if one is set. It then calls
`scripts/diff_test.sh`, which reads `./target/release/dellingr` unless
`DELLINGR` is set. If `CARGO_TARGET_DIR` points somewhere else, the build lands
in one place and the script runs a binary from another, which may be stale or
missing (and missing reports "FAIL: build"). The test could simply pass
`DELLINGR=env!("CARGO_BIN_EXE_dellingr")`, as `diff_gate.sh` does with its own
debug binary. That removes the nested build, the hard-coded path and the
`OnceLock` together.
**Enforce:** same shared-binary helper as A1. Also a rule that no `tests/*.rs`
spawns `cargo`.

### A3. Three different "where is the binary" rules for the same artifact
- `diff_test.sh`: `${DELLINGR:-./target/release/dellingr}`
- `bench.sh`: `${DELLINGR:-target/release/dellingr}`, a different spelling
  with no `./`, put into a hyperfine command string unquoted
- `hotbench.sh`: `target/release/dellingr` and
  `target/release/examples/hotpath`, with no override at all
- `diff_gate.sh`: `./target/debug/dellingr`
- `run_examples.rs`: `target/release/dellingr{EXE_SUFFIX}`

None of them asks cargo where it put the binary. `cargo build
--message-format=json` reports the executable path, and `cargo metadata`
reports `target_directory`. A host with `CARGO_TARGET_DIR` or `build.target-dir`
set breaks all of them in the same way. That host could be brokkr's own
`--commit` worktree (CLAUDE.md says it has its own CARGO_TARGET_DIR), or CI.
Fix: one `scripts/lib.sh` with `dellingr_bin <profile>` that builds and echoes
the path cargo reports. The `DELLINGR` env override should resolve in that one
place.
**Enforce:** partly. A grep rule banning `target/` literals in `scripts/` and
`tests/`.

---

## B. The differential comparison has more than one owner

### B1. The skip list is spelled three times, and the copies already differ
- `diff_test.sh`: `case benchmark.lua|stress_*.lua|upvalue_stress.lua`. In a
  bash `case`, `*` also matches `/`, so `stress_x/foo.lua` would be skipped
  too.
- `diff_test.rs::should_skip`: `stress_` prefix plus `!rel.contains('/')`,
  so top-level only. The Rust filter then passes the list to the script, which
  filters again.
- AGENTS.md restates the rule in prose.

`run_examples.rs` has no skip list, which is intended. It runs the stress
files, but nothing says so.
Fix: one owner. Either a marker comment in the file (`-- SKIP-DIFF: reason`,
in the same style as `-- DIFF:`) or a single list file that both read.
**Enforce:** yes, if the marker lives in the `.lua` file. The script then
becomes the only reader.

### B2. Bucket boundaries duplicated, and buckets drop directories
Both `run_examples.rs` and `diff_test.rs` split root files at the literal
`"feature_test_extended.lua"`.
- `diff_test.rs` buckets cover alloc, calls, fields, iter, strings, tables and
  numerics. They do not cover `composite/`, `globals/` or `parse/`. So
  `cargo test` alone never diff-tests `composite/benchmark.lua`,
  `globals/write.lua` or `parse/large_source.lua`. Only brokkr's `diff-lua`
  script_check covers them.
- `run_examples.rs` lists subdirectory files by hand with `example_file_test!`.
  The hand-written list is already missing `alloc/gc_churn.lua`,
  `calls/many_literals.lua`, `composite/benchmark.lua`, `fields/miss.lua`,
  `globals/write.lua`, `numerics/constants.lua` and `parse/large_source.lua`.
  None of these is executed by `run_examples.rs`. They are exactly the probes
  and verdict counterparts. AGENTS.md ("runs every `examples/*.lua`") and
  `generate.py`'s docstring ("tests/run_examples.rs executes every
  examples/*.lua") are both false today.

Fix: generate the per-file tests from the directory (build.rs, or one test that
walks and reports each file), and drop the hand list.
**Enforce:** yes. Add a test that walks `examples/` and asserts that every
`.lua` belongs to exactly one bucket, or remove buckets altogether.

### B3. Missing reference Lua: three different policies
- `diff_gate.sh` (and its comment) says a missing reference Lua is a broken
  environment and the gate fails loudly.
- `diff_test.rs` probes `lua5.2 -v` and `lua5.4 -v`. If either is missing it
  prints "Skipping", returns, and the test passes green.
- `diff_test.sh` never checks. Each run captures "command not found" as the
  reference output, so every non-DIFF file is reported FAIL. Every file tagged
  `-- DIFF:` passes silently.

Fix: one preflight in `diff_test.sh` (`command -v`, as `bench.sh` already does
for its tools) that refuses with a named message. The Rust test should fail,
not skip.
**Enforce:** yes. The script preflight is the single implementation, and the
Rust wrapper inherits it.

### B4. `-- DIFF:` exempts the whole file from everything
A tagged file passes whatever happens, including a dellingr crash, a timeout
or empty output. `diff_test.sh` also ignores dellingr's exit status entirely
(it merges stdout and stderr and only diffs). The tag is meant to mark "may
differ from reference". It should not turn off "must succeed".
`feature_test.lua` and `feature_test_extended.lua` are tagged, so the diff
script would not catch a regression in either one. `run_examples.rs` still
catches it via `: false` and the exit code, as long as A1 does not mask it.
**Enforce:** yes. Require a zero exit even for DIFF files, inside the script.

### B5. The 5 s timeout is a knob with no name
`timeout 5s` is hard-coded in `diff_test.sh`. The only control is
`DELLINGR_SKIP_TIMEOUT=1`, which the Rust test sets, so the two entry points
run under different time limits. A debug-build gate (`diff_gate.sh`) gets the
same 5 s as release, and a slow host turns a timeout into a FAIL with no
"timed out" text. Exit 124 is not distinguished.
Fix: `DELLINGR_DIFF_TIMEOUT`, defaulted once, and report timeouts as such.
**Enforce:** no mechanical rule. It is a single site once named.

### B6. `diff_test.sh` writes to `/tmp`
`mktemp "${TMPDIR:-/tmp}/..."` falls back to `/tmp`, which the project's rules
forbid. `.gitignore` still carries `.diff_test_*.out`. That is the residue of
an earlier in-repo scratch location that nothing writes any more (dead ignore
rule). Fix: put the scratch files under the cargo target tmp dir, or a
project-owned `.scratch/` (not `target/`, which belongs to cargo and is
shared). Delete the stale ignore line.
**Enforce:** a grep rule banning `/tmp` in `scripts/`.

### B7. GNU-only `realpath --relative-to` and absolute shebangs
`diff_test.sh` and `diff_gate.sh` hard-code `#!/usr/bin/bash`, while
`bench.sh` and `hotbench.sh` use `#!/usr/bin/env bash`. `test_limited.sh`
also uses `/usr/bin/bash`. On macOS or BSD (the hotpath harness has a macOS
branch, so another unix is anticipated) `realpath --relative-to` does not
exist, and `/usr/bin/bash` is absent or bash 3, where `globstar` is
unsupported. Failure mode: refusal at the first line, or a silently empty
`examples/**` glob under bash 3, which `nullglob` turns into "ok". **Silent
pass.**
**Enforce:** a shebang lint (`grep -L '^#!/usr/bin/env bash'`). Add a guard
that the script list is non-empty.

---

## C. The `Cost used:` line and CLI output contract

### C1. AGENTS.md describes a mechanism that no longer exists
AGENTS.md says `scripts/diff_test.sh` filters out the `Cost used:` line. It
does not. It passes `--quiet`. `run_examples.rs` also passes `--quiet`. The
format string itself lives only in `main.rs`, and `cli_limits.rs` asserts it
as a substring. `bench.sh` and `hotbench.sh` run without `--quiet`, which
matters little for timing but is a different shape.
**Enforce:** a CLI test pinning the exact line and its stream already half
exists. Fix the doc sentence.

### C2. `Cost used:` goes to stdout, mixed with the script's output
The cost summary is printed on stdout after the script's own `print`s, even
when the script errored (it is printed before the error check). Any consumer
that wants script output has to know to pass `--quiet`. The `--analyze`
caveat warning also goes to stdout, as part of the "result". Diagnostics
belong on stderr, or behind an explicit `--cost` flag. There is no output
channel abstraction in the CLI: every message is an ad-hoc
`println!`/`eprintln!` with prefixes "Error:", "Parse error:",
"Error reading ...:" and "Unknown option:" (no prefix). Help goes to stdout.
The short usage goes to stderr and is a second, differently worded copy of
the usage text (`Usage: dellingr [OPTIONS] <file.lua>` vs
`Usage: dellingr [--limit N] [--analyze] [--quiet] <file.lua>`).
**Enforce:** a CLI integration test per stream. Generate the usage from one
table of options, or use clap.

### C3. Exit codes are spelled per site and differ between tools
`main.rs`: `exit(1)` at seven sites, for usage error, IO error, parse error
and runtime error alike. `exit(0)` for help. `hotpath.rs`: `exit(2)` for
usage and `exit(1)` for everything else. `hotbench.sh`: 2 for usage.
`diff_test.sh`: 1. So the CLI's usage error is 1 and the harness's is 2, and a
caller of the CLI cannot tell "your script failed" from "you invoked me
wrong" or "budget exhausted". The game host cares about the budget case.
Fix: one `ExitCode` enum per binary (`main` returning `ExitCode`), with
distinct codes for usage, IO, compile, runtime and budget.
**Enforce:** yes, by type. Ban `std::process::exit` in `src/main.rs` and
`examples/` via `clippy.toml` `disallowed-methods`, and return `ExitCode`.

### C4. CLI argument handling aborts or silently resolves operator mistakes
- `std::env::args()` panics on a non-UTF-8 argument (abort on operator input).
  Use `args_os`.
- With several positionals, the last one wins silently (`dellingr a.lua
  b.lua` runs `b.lua`).
- A lone `-` (stdin convention) is reported as "Unknown option".
- A script path starting with `-` cannot be passed (there is no `--`
  terminator).
- `--limit` is accepted in `--analyze` mode and silently ignored.
**Enforce:** a parser library or a table-driven parser with tests.

### C5. The CLI declares a counting global allocator that nothing reads
`main.rs` installs `hotpath::CountingAllocator` under `hotpath-alloc`, but the
CLI never builds a `HotpathGuard`, so nothing reports allocation stats. The
comment is a near-copy of the one in `hotpath.rs`, and the two have already
diverged: "hotpath through 0.14 installed..." vs "hotpath 0.15 installed...".
Dead in the CLI. Delete it, or say why it is load-bearing.
**Enforce:** no lint. Deleting it removes the duplicate.

---

## D. The bench harness (`examples/hotpath.rs`)

### D1. Silent success on a script without `_bench`
If `_bench` is nil, the harness prints partial KVs and **returns exit 0**.
brokkr's `--bench` then records a run with only parse and setup. A typo'd
global or a script that predates the convention (`examples/benchmark.lua`)
benchmarks nothing and passes. A refusal is owed.
**Enforce:** a test that runs the harness on a script without `_bench` and
expects a nonzero exit.

### D2. Silent default workload
With no positional argument, the harness runs `numerics/arithmetic`
(`DEFAULT_TARGET`). A brokkr misconfiguration that drops the argument measures
the wrong workload, and the KV output says so only in `target=`. That is a
convenience default inside a measurement tool. Make the argument required.

### D3. Legacy target resolution is bound to the build checkout's absolute path
A bare name resolves against compile-time `env!("CARGO_MANIFEST_DIR")`. The
binary is permanently tied to the directory it was built in. A harness built
in brokkr's `--commit` worktree resolves bare names into that worktree's
`examples/`, not into the current tree, contradicting CLAUDE.md's "loads the
workload file from the CURRENT tree". This is fine today only because brokkr
passes paths. A path, by contrast, resolves against cwd. That makes two
resolution rules in one argument, picked by whether the string ends in
`.lua`. AGENTS.md calls the bare form "legacy". The only remaining user is
`hotbench.sh`, and it passes a path. So the bare-name path looks dead.
**Enforce:** delete it. Then there is nothing to enforce.

### D4. FIFO failures are invisible
`BROKKR_MARKER_FIFO` is read once (good), but if it is set and the open fails
(wrong path, no reader with O_NONBLOCK gives ENXIO), the harness silently
emits nothing. Every `write_all` error is `drop`ped, so a full pipe loses
markers, and a dropped `*_END` marker corrupts the span pairing brokkr builds.
The env var is set by brokkr exactly when brokkr expects markers, so a failure
to open is a misconfiguration owed a stderr warning or exit. The protocol
(`<us> NAME`, `<us> @k=v`) is defined in the harness and parsed in brokkr,
a second owner in another repo with nothing keeping them in step.
**Enforce:** a shared protocol crate, or a brokkr-side schema test against
captured harness output.

### D5. Hand-rolled `O_NONBLOCK` constants
`0x800` for linux and `0x0004` for macos. Any other unix target is a compile
error (the const is undefined in the `#[cfg(unix)]` block). `libc::O_NONBLOCK`
or `rustix` owns this value.
**Enforce:** yes, with a dependency instead of a literal.

### D6. KV output is two hand-maintained lists
The KV `eprintln!` block is written twice: once for the no-`_bench` early
return and once at the end. The `emit_counter` names restate a subset of the
same keys again. Key names are the contract with brokkr and `hotbench.sh`
readers, and nothing pins them. `warm_avg_us` is integer microseconds
(truncating), so `cost_per_us` is computed against a truncated denominator
and is 0 when the average is under 1 us.
Fix: one `Vec<(&str, Value)>` built once and emitted to both channels.
**Enforce:** a test parsing harness stderr for the expected key set.

### D7. Iteration count 20 and the calibration it implies have several owners
`DEFAULT_ITERATIONS = 20` in the harness. AGENTS.md and CLAUDE.md both restate
"default 20" and "~2s warm phase". The `bench/` files are calibrated
(~100 ms per call, 30-call footer) against that number. `MAX_WARM_BLOCKS = 32`
is restated as "up to 32" in AGENTS.md. Change the constant and four documents
go stale without a signal. Per the house documentation rule, reword the docs
to refer to the constant ("the harness default").
**Enforce:** no. Doc wording only.

### D8. `.unwrap()` on `get_global` at file scope
`#![allow(clippy::unwrap_used)]` covers the whole harness. `get_global` is
fallible precisely because of the stack cap. A failure panics with no subject
named, whereas the neighbouring calls name the script path.
**Enforce:** remove the file-level allow, so clippy enforces it.

---

## E. Benchmark tooling: duplicated workload lists and stale doc claims

### E1. Three workload registries
`bench.sh`'s default script list, the `[dellingr.workloads.*]` entries in
`brokkr.toml`, and AGENTS.md's directory table. `bench.sh` uses
`examples/benchmark.lua` (the legacy no-`_bench` script), while brokkr uses
`examples/composite/benchmark.lua` as the hotpath counterpart. `bench.sh`'s
list matches brokkr's hotpath files plus the different benchmark entry. Adding
a workload means editing two files with nothing checking them against each
other. The README table comes from `bench.sh`, so the README and brokkr can
diverge on which kernel "benchmark" means.
**Enforce:** `bench.sh` could read the `hotpath_file` keys from `brokkr.toml`
(one owner). That still needs a TOML reader in bash. Otherwise a test that
diffs the two lists.

### E2. `bench.sh` hard-codes counts and tool names in several places
"5 lua versions" in the dry-run message is really 4 references plus dellingr.
The reference list `lua5.5 lua5.4 lua5.2 luajit` is spelled four times: in the
tool preflight, in the hyperfine `-n` arguments, in the jq selectors and in
the header row. `$DELLINGR $script` is word-split inside the command string.
Fix: one array that drives all four.
**Enforce:** structural (single array). No lint.

### E3. Contradictory instructions about running the harness
The `hotpath.rs` module doc says "Run standalone: cargo run --release
--example hotpath ...". AGENTS.md says not to invoke it manually and to use
`hotbench.sh`, and raw `cargo` is banned in this project. Also, `hotbench.sh`
runs `target/release/examples/hotpath` built with `--features hotpath`, while
brokkr builds the same example target name into the same shared target
directory. Whichever ran last leaves its feature set in place. A plain
`--bench` cell is rebuilt by brokkr, but a human running the binary directly
gets whichever feature shape is on disk.
**Enforce:** no. Fix the doc text.

### E4. Pinned digests are mechanically enforced. The kernel-pairing claim is not
brokkr refuses on hash drift, which is good. The real invariant ("same kernel
in the same proportions" between `bench/X` and `examples/.../X`) rests on
review alone. A cheap partial check: a test asserting that both files of each
pair define `_bench`, and that their `cost` per `_bench` call is in a fixed
ratio (cost is deterministic). That would turn the pairing claim into a fact.
**Enforce:** yes, as a test that reads `brokkr.toml`.

### E5. `large_source` pin and the generator
`brokkr.toml` pins `examples/parse/large_source.lua` twice (same hash in both
slots). Nothing checks that the committed file equals the `generate.py`
output, so a hand edit (forbidden by its header) is caught only by the hash
pin. That also fires on a legitimate regeneration. A test that runs the
generator logic and diffs it, or a brokkr script_check, would hold it.
`generate.py` writes next to itself (owned location, fine). Its docstring
repeats the false "run_examples executes every examples/*.lua" claim (B2), and
it restates the parser's 255 nested-function limit as prose. The real owner is
a constant in the parser. FUNCTION_COUNT = 200 is guarded only by the comment.
**Enforce:** the diff test above. The 255 is not enforceable from Python;
`compiler_limits.rs` owns the limit on the Rust side.

---

## F. Configuration and lint files

### F1. `clippy.toml` reasons are stale
The `rand::rng`/`thread_rng` reasons say "Use State.rng (seeded StdRng)".
StdRng is gone. It is now `VmRng` (SplitMix64) and `rand` is no longer a
dependency (AGENTS.md). Reword it to "use `State.rng` (seeded via
`set_rng_seed`)". The bans themselves are `allow-invalid`, so they guard
nothing until `rand` comes back. The intent is a tripwire, and that is
acceptable. But the determinism hazard that actually remains reachable is not
banned: `std::time::Instant`/`SystemTime` in library code, `std::env::var` in
library code, and `std::collections::hash_map::RandomState`/`DefaultHasher`
(HashMap's own types are banned, but not the hasher used directly).
**Enforce:** yes. Add those to `disallowed-methods`/`disallowed-types`, and
allow them per site in `main.rs`/`examples/`.

### F2. Two lint sources
`Cargo.toml` `[lints.clippy]` and the deny list in `src/lib.rs` (per AGENTS.md)
both carry denials. `await_holding_lock`, `await_holding_refcell_ref`,
`dbg_macro` and `todo` appear in `Cargo.toml`, and AGENTS.md says `lib.rs` has
them too. Two owners for the same rule set. `[lints]` covers every target,
including the bin, examples and tests, whereas `lib.rs` attributes cover only
the library. That is probably why both exist, but nothing documents which list
is authoritative. Consolidate into `[lints]` and keep only crate-specific
`cfg_attr`s in `lib.rs`.
**Enforce:** a text check that `lib.rs` has no `deny` already present in
`[lints]`.

### F3. `hotpath` is a non-optional dependency of the library
`hotpath = "0.28.6"` is unconditional, and the features merely toggle
`hotpath/hotpath`. Every downstream consumer (the game) compiles the hotpath
crate and its dependency tree to get no-op macros. That is a build-time and
supply-chain cost for an internal profiling tool. Make it `optional = true`
with a cfg_attr shim, or accept it and say why.
**Enforce:** `cargo tree -e normal` check in the gate.

### F4. Feature shapes the gate never compiles
`brokkr.toml` sweeps `default` and `snapshot` only. `debug_parser`,
`debug_vm`, `debug_gc`, `hotpath` and `hotpath-alloc` are never built by
`brokkr check`. AGENTS.md admits this for hotpath and makes it a manual step.
The debug_* features guard four `cfg` sites (`parser.rs`, `frame.rs`, and two
in `object.rs`) that can rot unseen. Either add `[[check]]` entries, or delete
the debug_* features if nobody uses them (nothing in scripts, CI or docs other
than one AGENTS.md line exercises them, which suggests they are dead).
**Enforce:** yes. Add `[[check]]` entries (a clippy-only pass would do, if
brokkr supports it).

### F5. `.gitignore` entries for tools that do not exist here
`.plans` (the `plans/` folder is retired), `overnight.sh`, `overnight-logs`
and `.diff_test_*.out` (see B6) have no producer in the tree. Ignore rules for
vanished tools hide whatever later lands under those names. `research` is
documented (the lua-rs checkout). `.agents`, `.codex` and `.claude` are
ignored wholesale, so `.claude/settings.local.json` changes (the allowlist
CLAUDE.md tells agents to read) are invisible to review. That may be intended.
**Enforce:** no.

### F6. `.review.toml`
It contains only an `[_audit] id`. Nothing in the tree reads it (it is
presumably the external `review` tool's state). It is listed in `Cargo.toml`
`exclude`. That is fine, but it is undocumented tool state in the repo root.

---

## G. Fixtures and test I/O

### G1. Orphaned fixture `tests/fixtures/save_golden.bin`
It is tracked, but nothing references it: `save_golden.rs` reads
`save_golden_current.bin` and `save_golden_legacy_v6.bin`, and a grep finds no
other reader. Evidence it is dead: no path literal in any source, script or
config. It is probably the pre-rename name of one of the two.
**Enforce:** a test that every file in `tests/fixtures/` is named by some
test.

### G2. Regeneration helper writes into the source tree via an `#[ignore]` test, and its instruction uses raw cargo
The regeneration command is spelled twice: in the module doc and in the
`expect` message. Both use raw `cargo test`. The project's runner is `brokkr
test regenerate` (which passes `--include-ignored`), so the documented command
contradicts the house rule. Note also that `brokkr test regenerate` with
`--include-ignored` matches by substring, so any test whose name contains
"regenerate" also rewrites a fixture on a casual run. The fixture name is a
hard-coded `tests/fixtures/` join, which is fine (owned location).
**Enforce:** give the helper an unusual name, and say "brokkr test <name>".

### G3. `cli_limits.rs` scratch files
They go to `CARGO_TARGET_TMPDIR`, which is cargo-managed and correct. Cleanup
is a bare `fs::remove_file` after the asserts in two tests, but in
`signed_limits...` it comes after an assert inside the loop, so a failure
leaks the file. Minor. The pid suffix avoids collisions. A drop guard would
make the cleanup structural.

### G4. `run_examples.rs`'s "`: false` anywhere" convention is enforced on stdout only
The check is `line.ends_with(": false")` on stdout. stderr is not checked, so
the assertion convention is only partly enforced. AGENTS.md says "contains
the substring", and the code says "ends with". They have diverged. The code's
version is better: a line like "x: falsey" would otherwise fail.
**Enforce:** already a test. Fix the doc.

---

## H. Lateral

- `run_examples.rs::run_feature_test_extended_in_process` unwraps the
  load/call result with no message. The failure output will not name the
  script error clearly. Minor.
- `diff_test.sh` accepts a match against *either* lua5.2 or lua5.4 for the
  whole file. A file that matches 5.2 for half its lines and 5.4 for the other
  half fails, which is correct. But the either-or rule means a regression
  toward 5.2 behaviour in a 5.4-intended feature is invisible. Per-file pinning
  (`-- REF: 5.4`) would sharpen it.
- `bench.sh` records host, commit and dirty count in its header (good,
  matching the "commit hash and hostname" rule), but hyperfine's JSON is
  deleted afterwards, so nothing durable remains unless the operator copies
  stdout.
- `test_limited.sh`'s 2 GiB `ulimit -v` is a constant with no override, and
  nothing in the tree invokes it (AGENTS.md only). `ulimit -v` also counts
  reserved address space, so it can refuse under allocators that reserve
  large arenas (it would interact with the hotpath queue). It is possibly
  dead. It was superseded in practice by the stack cap described in AGENTS.md.
