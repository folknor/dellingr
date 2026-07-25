# What this is

`dellingr` is an embeddable, deterministic, pure-Rust Lua VM with per-opcode instruction-cost accounting. It is the script host for a game project; the README's "Won't implement" section is load-bearing - `pcall`/`xpcall`, coroutines, `io`/`os`/`debug`, `goto`, integer division, bitwise ops, long strings, `string.rep`/`byte`/`char`, and arithmetic/comparison/concat metamethods are deliberately absent. Don't add them without asking. Errors kill the callback by design.

The crate ships both a library (`State`, `HostCallbacks`, `RustFunc`, `analyze_cost`, `ArgCount`, `RetCount`) and a thin CLI binary (`src/main.rs`). The public API is pre-1.0 and not stable.

## Commands

Use `brokkr` (not raw `cargo`) for check/test. It runs a gremlins scan (banned Unicode), then clippy, then tests. Clippy denies warnings project-wide, so a clippy failure short-circuits before tests run. By default, output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope)
- `brokkr check --all` - show every diagnostic, no cap, no scope filter
- `brokkr check --fix-gremlins` - rewrite banned Unicode in tracked files (em/en dash to `-`, smart quotes to straight, NBSP to space, zero-width/bidi deleted) before checking
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test <NAME>` - release-mode focused single-test runner. Always passes `--release --include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug` - build and run the test in dev profile instead of release. Use this for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time dominates wall time and the optimization level doesn't change the behavior under test.

`dellingr` is a single-crate repo, so `-p` is normally unnecessary; pass `-p dellingr` if a brokkr invocation requires an explicit package.

Running scripts (the binary, not the test runner):

```sh
cargo run --release -- path/to/script.lua             # run a script
cargo run --release -- --analyze path/to/script.lua   # static cost analysis, no execution
cargo run --release -- --limit 100000 path/to/script.lua  # run with a cost budget
./run.sh script.lua                                    # cargo run --quiet wrapper
```

Differential testing against reference Lua:

```sh
./diff_test.sh                # diff vs lua5.2 / lua5.4 (must be on PATH); prints "ok" on success or "FAIL: <path>" per failing script
./test_limited.sh <cmd>       # run <cmd> with a 2GB virtual-memory cap (for stress tests)
```

Debug-print feature flags: `--features debug_parser`, `debug_vm`, `debug_gc`.

MSRV is `1.92`. Edition 2024.

## Lint gate (don't disable, fix the code)

`src/lib.rs` denies a long list of clippy lints; `clippy.toml` adds disallowed methods/types. The non-obvious ones:

- **`unwrap_used` is denied outside `#[cfg(test)]`.** Use `?`, `expect("reason that explains why this can't fail")`, or proper error handling. The string in `expect` should explain the invariant, not just describe the call.
- **`Result::ok()` is banned.** It silently discards errors. Use `?` or handle the error.
- **`HashMap` / `HashSet` are banned.** Iteration order is non-deterministic. Use `IndexMap` (already a dep) or `BTreeMap` / `BTreeSet`.
- **`rand::rng` / `rand::thread_rng` are banned.** Unseeded RNG breaks determinism. The VM RNG is `state.rng`, an in-crate `VmRng` (SplitMix64) seeded via `set_rng_seed` (default seed 0); the `rand` crate is no longer a dependency. The clippy ban stays as a guard against re-introducing unseeded entropy; `allow-invalid = true` keeps it from tripping now that the path is gone.
- `dbg_macro`, `todo!`, `await_holding_lock`, `await_holding_refcell_ref` are denied.

Determinism is a product requirement, not a style preference - game replays depend on it.

## Architecture

Compilation pipeline: source to `compiler::parse_str` to bytecode `Chunk` to executed by `State` in `vm::eval`. There is no separate IR.

**`src/instr.rs`** - fixed-width 32-bit bytecode (`[opcode:8][A:8][B:8][C:8]` or `[opcode:8][A:8][sBx:16]`). `ArgCount` and `RetCount` use `255` as a sentinel for `Dynamic`/`All`; the encoding round-trips through `to_u8`/`from_u8`. The `Builtin` enum gives well-known globals (`print`, `pairs`, `math`, etc.) fast array-indexed access in `State.builtins` instead of a hash lookup.

**`src/compiler/`** - `lexer.rs`, `parser.rs` (large; the parser is also the codegen), `exp_desc.rs`, `token.rs`. Produces a `Chunk` with `code`, literal pools, `nested` chunks for nested functions, `upvalues: Vec<UpvalueDesc>` (`Local(slot)` / `Upvalue(idx)`), and `line_info` mapping bytecode index to source line for stack traces.

**`src/vm.rs`** plus `src/vm/` is the runtime. `State` owns:

- `globals: IndexMap<String, Val>` (deterministic iter for GC marking and `restrict_globals`)
- `builtins: [Val; Builtin::COUNT]` - fast path for well-known globals
- `stack: Vec<Val>` - single shared stack for both Lua and Rust frames; `stack_bottom` is the current frame's base
- `heap: GcHeap` (mark-and-sweep, in `vm/object.rs`); strings are interned
- `upvalue_pool` + `open_upvalues` (sorted by stack index for efficient close-on-return)
- `cost_remaining: i64`, `cost_used: u64`, `cost_budget: i64` - i64 lets the operation that pushes you over budget complete, then the next costed op fails
- `call_stack: Vec<CallInfo>` - for stack traces
- `callbacks: Box<dyn HostCallbacks>` and a typed `user_data` slot
- `rng: VmRng` (in-crate SplitMix64) seeded via `set_rng_seed`

`MAX_CALL_DEPTH = 1000`, `MAX_STACK_SIZE = 1_000_000`.

**GC roots**: `vm::mark_gc_roots` is the *single source of truth* for what's reachable. Any allocator that may trigger GC must use this same root set. Closed upvalues are reached transitively via the closures that hold them, not as a separate root set - there's an explicit comment about this (`vm.rs` / `vm/object.rs`). Root an object with `GcHeap::mark`, never by pushing onto the worklist directly: `mark` sets the object's colour *and* queues it, whereas a bare push traces the object's children while leaving the object itself Unmarked, so sweep frees it and later reads dangle. A `debug_assert!` in `drain_mark_worklist` enforces this - the mistake is otherwise invisible until something actually collects.

**Stack indexing**: Rust callbacks (`RustFunc = fn(&mut State) -> Result<u8>`) use **1-based** indexing; Lua bytecode internally uses 0-based. `vm_aux.rs` and the lua_std modules show the 1-based pattern.

**Standard library**: `src/lua_std/{basic,math,string,string_format,table}.rs`, opened by `lua_std::open_libs(state)` from `State::with_callbacks` (`string_format.rs` is the Lua 5.4 `string.format` engine, registered by `string.rs`). Every `open_*` function pushes builtin functions onto the stack and uses `set_global` / `set_table_raw` to install them. `_G` exists but is wired through a metatable that proxies to `state.globals` (see `basic.rs`); it is not a real table.

**Cost model**: opcodes charge in `vm/eval.rs`'s dispatch; `analyze_cost` (`src/lib.rs`) walks bytecode statically and produces a `ScopeCost` tree (own + nested totals). The README's "Budget" section flags that structural ops like `while true do end` are intentionally free.

**Patterns**: Lua-pattern matching is an in-crate implementation in `src/patterns/`, not a dependency. `luapat.rs` compiles a pattern once into an item program plus interned 256-bit byte classes (`from_bytes_try`), then matches over `&[u8]` with absolute subject offsets (`matches_bytes_from(subject, init)` - callers pass the *whole* subject with a start offset, never a re-slice, so `%f` keeps its left context). Recursion depth is passed by value and bounded at `MAXCCALLS = 200` active invocations, matching reference exactly; only capture start/end, `max_expand`/`min_expand` backtracking and the greedy `?` branch recurse, while every other continuation is a loop iteration (reference's `goto init`). Compile-time errors surface from `from_bytes_try` and match-time ones from `matches_bytes_from`; that split is contract, asserted by tests. `string.rs` has helpers `is_plain_lua_pattern` (fast path) and `gsub_replacement` (string / table / function replacements with Lua's `%0..%9` capture syntax).

## Tests

- `tests/run_examples.rs` - runs every `examples/*.lua` via `cargo run` and fails on nonzero exit OR if the output contains the substring `: false`. Examples that print test results follow the convention `print("test name: " .. tostring(condition))`. Adding a new `examples/foo.lua` that prints `: false` will break this test.
- `tests/error_handling.rs`, `gc_upvalues.rs`, `gsub_errors.rs`, `metamethod_errors.rs`, `rustfn_error.rs` - focused integration suites.
- `tests/diff_test.rs` is a Rust harness; `diff_test.sh` is the differential shell script that compares output against `lua5.2` and `lua5.4`. Mark intentional divergences with a `-- DIFF: <reason>` comment in the example. `benchmark.lua`, `stress_*.lua`, and `upvalue_stress.lua` are skipped by the diff script.
- Unit tests live alongside their modules (e.g. `src/vm/object.rs` has GC tests).

## Hotpath benchmarks

`examples/hotpath.rs` is a single Rust harness. It takes one positional arg (a target path like `fields/same_obj_read`) and loads `examples/{target}.lua`, which must define a global `_bench()` function. Bench scripts live in subdirectories of `examples/` alongside correctness tests:

```
examples/hotpath.rs        # harness (parse / cold call / warm calls)
examples/numerics/         # arithmetic
examples/calls/            # global, local, method, vararg, fixedarg, ...
examples/fields/           # same_obj_read, same_obj_write, polymorphic, ...
examples/iter/             # pairs, ipairs
examples/tables/           # fill, mixed, numeric_index
examples/alloc/            # closure, short_tables
examples/strings/          # mixed
```

Each bench script is also a standalone-runnable test: top-level setup + a `_bench()` function + an outer loop that calls `_bench()` enough times for hyperfine resolution and prints `<name>: true`. This lets one file serve three masters: the hotpath harness (calls `_bench` directly for parse/cold/warm phasing), `tests/run_examples.rs` (executes the standalone runner, asserts no `: false`), and `bench.sh` (hyperfine timings vs reference Lua 5.2/5.4/5.5 + LuaJIT).

The harness measures four phases on one State and emits KV pairs to stderr: `parse_us`, `cold_call_us`, `warm_avg_us` (averaged over 20 iterations), plus the dellingr-specific `cost_used` per phase and the derived `cost_per_us`. Cost is deterministic across hosts; wall time is the noisy metric. `setup_*` and `final_*` heap/object counts bracket GC pressure.

Two internal hot paths carry `#[hotpath::measure]`: `compiler::parse_str_named` (one full parse) and `vm::State::gc_collect` (one mark+sweep). Both are non-recursive entry points. The annotation is a no-op when the `hotpath` cargo feature is off. **Don't add `#[hotpath::measure]` to `eval_closure` or any function that recurses through the bytecode dispatch loop**: each level adds enough stack-frame bloat to abort the `call_depth_exceeded_error` test (which intentionally recurses to `MAX_CALL_DEPTH = 1000`).

To add a new target: write `examples/{category}/{name}.lua` defining `_bench()` and a standalone runner footer. No Rust changes, no manifest updates.

```sh
./hotbench.sh fields/same_obj_read           # KVs from harness + hyperfine wall time
./hotbench.sh tables/fill --runs 20          # extra args pass through to hyperfine
FEATURES=hotpath-alloc ./hotbench.sh tables/fill   # alloc tracking on the KV side
```

`hotbench.sh` builds both the regular `dellingr` binary and the
hotpath example, prints the harness's KV breakdown, and runs hyperfine
on the script via the regular binary (the hotpath stats table at exit
otherwise dominates wall time). Don't invoke the `cargo run --example
hotpath` command manually - run it via the script so the timing path
stays consistent.

`research/bench_luars.sh` is a one-off comparison harness: same bench
scripts and same hyperfine pass, but against [lua-rs](https://github.com/ianm199/lua-rs)
(the only comparable pure-Rust Lua VM) anchored to C Lua 5.4 and the
LuaJIT floor. It expects a `lua-rs` release binary built from a clone in
`research/lua-rs/` (build command in the script header); `research/` is
an untracked external checkout, not part of the crate. The README's
`lua-rs` column comes from this script - its footnote pins the captured
date and upstream commit, so refresh both together.

## Project conventions worth knowing

- Examples in `examples/` are part of the test surface - don't add throwaway scripts there. Use `hotpath/` for bench scripts (see above).
- The CLI prints `Cost used: N` after each run; `diff_test.sh` filters this line out before comparing.
- The crate was lifted out of a game project (originally extracted from `fcomm2`); some doc comments still mention `FleetCallbacks` etc. as illustrative examples.
- `target/` is a symlink to a shared cargo cache.
- `OPTIMIZATIONS.md` is a working backlog of forward-looking optimization ideas (rejected, deferred, hypothetical). Items get deleted as they ship or stop being worth tracking. Not a discrepancy doc.
- `TODO.md` is the matching backlog for non-perf forward-looking ideas (features, refactors, ergonomic gaps). Same conventions: working list, items deleted as they land.
