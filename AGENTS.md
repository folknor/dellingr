# What this is

`dellingr` is an embeddable, deterministic, pure-Rust Lua VM with per-opcode instruction-cost accounting. It is the script host for a game project; the README's "Won't implement" section is load-bearing - `pcall`/`xpcall`, `assert`, coroutines, `io`/`os`/`debug`, `goto`, integer division, bitwise ops, long strings, `string.rep`/`byte`/`char`, and arithmetic/comparison/concat metamethods are deliberately absent. Don't add them without asking. Errors kill the callback by design.

Ground rules that are easy to get wrong (the `docs/` pages have the detail):

- **The cost budget is a game-design instrument, not a CPU meter** (`docs/budget.md`). Charges are design decisions: flat and free charges are often deliberate, and structural semantics are free on purpose. Host work that is out of proportion to its charge is a performance concern, not a cost-model defect.
- **Deliberate divergences from reference Lua** are listed in `docs/compatibility.md`. Check there before treating a difference from lua5.2/lua5.4 as a bug.
- **Matching reference Lua is not a goal in itself.** A difference is a defect only when it hurts determinism, the cost model, or the script author (wrong results, a misleading message, a surprising behaviour). Don't report or "fix" a difference because it differs, and don't justify a change by "matches lua5.4" alone; error wording that already says what went wrong is fine as it is.
- **Replay-visible changes must be called out**, never slipped in: `pairs` order, rendered numbers and strings, the RNG draw sequence, and snapshot contents all feed game replays.
- Lua written for tests and examples has no `assert`; use `if not cond then error(...) end`.

The crate ships both a library (`State`, `HostCallbacks`, `RustFunc`, `analyze_cost`, `ArgCount`, `RetCount`) and a thin CLI binary (`src/main.rs`). The public API is pre-1.0 and not stable.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.

### Bash rules

- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

## Commands

Use `brokkr` (not raw `cargo`) for check/test. It runs a gremlins scan (banned Unicode), then clippy, then tests. Clippy denies warnings project-wide, so a clippy failure short-circuits before tests run. Output is never capped or scoped: every diagnostic prints every time, and errors in files with unstaged changes are listed first.

- `brokkr check` - gremlins + clippy + all tests
- `brokkr check --fix-gremlins` - rewrite banned Unicode in tracked files (em/en dash to `-`, smart quotes to straight, NBSP to space, zero-width/bidi deleted) before checking
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test <NAME>` - focused single-test runner. With no per-sweep profile override, this repo's `[test] debug = true` selects the dev profile by default; explicit profile flags take precedence. Always passes `--include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug`, `--release` - force the dev or release profile. Use `--debug` for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time dominates wall time and the optimization level doesn't change the behavior under test.

`dellingr` is a single-crate repo, so `-p` is normally unnecessary; pass `-p dellingr` if a brokkr invocation requires an explicit package.

Running scripts (the binary, not the test runner):

```sh
brokkr run --release -- path/to/script.lua             # run a script
brokkr run --release -- --analyze path/to/script.lua   # static cost analysis, no execution
brokkr run --release -- --limit 100000 path/to/script.lua  # run with a cost budget
brokkr run --quiet -- script.lua                      # forwards raw to cargo run
```

Differential testing against reference Lua:

```sh
./scripts/diff_test.sh        # diff vs Lua 5.2 / 5.4; prints "ok", or one FAIL/TIMEOUT line per failing script
./scripts/test_limited.sh <cmd>  # run <cmd> under a virtual-memory cap (for stress tests)
```

`diff_test.sh` exits 1 at the first unmarked example if either reference Lua
is missing and names the executable. It requires dellingr to exit zero even
for `-- DIFF: <reason>` files. DIFF files
skip both reference runs after dellingr succeeds. Both `-- DIFF:` and
`-- SKIP-DIFF:` require a nonempty reason. For each failure it also prints
detail to stderr: the captured output of a process that failed or timed out,
or unified diffs of dellingr's output against both references.
Flags (before any paths):
`--bin <path>` uses an existing binary instead of building one;
`--timeout <time>` sets the per-process timeout in `timeout(1)` syntax
(default in `scripts/lib.sh`). A nonzero timeout requires GNU `timeout`
(coreutils) on PATH; there is no other fallback. `0` disables the timeout and
needs no timeout utility. Invalid values are rejected once at startup with exit
code 2. Its temporary output lives under the gitignored `.scratch/`. The
reference tools are fixed as `lua5.2` and `lua5.4`.
`test_limited.sh` applies the fixed `TEST_LIMIT_KB` `ulimit -v` cap from
`scripts/lib.sh`. If that limit cannot be applied, the command fails with exit
code 2.

The scripts read no environment variables. `BROKKR_MARKER_FIFO`, read by the
hotpath harness and set by brokkr, is the only environment input in the
repository. Script constants live in `scripts/lib.sh`; hyperfine uses a fixed warmup of
2 runs.

`scripts/notes_drop.py` and `scripts/notes_move.py` are maintainer helpers for
the `notes/` working documents, run directly when needed; no gate invokes them.
`scripts/frame_sizes.py <binary> <symbol substring>...` prints native frame
sizes (prologue `sub rsp` plus pushes) from a built binary's disassembly, with
`--calls` for each function's call targets (inlining decisions) and `--body`
for an address-blanked body dump; `scripts/asm_diff.py` diffs two such dumps
(`--slots` lists the `%rsp` slots each build uses). They need `objdump` and
are how a `tests/native_stack.rs` probe delta gets attributed to a function.
`scripts/perf_hot_lines.py <annotate-output> [min-percent] [context]` filters
saved `perf annotate --stdio` output down to its hot instructions, which is how
a `Frame::run` listing of several hundred KB becomes readable.

Shell scripts live in `scripts/` and share `scripts/lib.sh`, which builds a
cargo target and asks cargo for the artifact path (it needs `python3`), so no
script hard-codes a `target/` path. Each script anchors itself to the repo
root, so they can be invoked from anywhere; `test_limited.sh` is the
exception, since it is a transparent `exec` wrapper that should not move the
caller's working directory.

To inspect compiled bytecode, `brokkr run --release -- --dump-bytecode path/to/script.lua`
(prints the listing without executing; the library entry is `Program::disassemble`).
Library code never prints: `clippy::print_stdout`/`print_stderr` are denied in
`src/lib.rs` and `std::io::stdout`/`stderr` are banned in `clippy.toml`, with scoped
allows only where printing is the job (`DefaultCallbacks::on_print`, the CLI, the
hotpath harness).

MSRV is the `rust-version` in `Cargo.toml`. Edition 2024.

## Lint gate (don't disable, fix the code)

`Cargo.toml` `[lints.clippy]` (and `[lints.rust]`) defines package-wide lint levels for every target; `clippy.toml` supplies the disallowed methods and types; `src/lib.rs` additionally denies `clippy::unwrap_used` and `clippy::float_cmp` under `cfg(not(test))` and warns on `missing_docs`, for the library only. The non-obvious ones:

- **`unwrap_used` is denied in the library outside `#[cfg(test)]`.** Use `?`, `expect("reason that explains why this can't fail")`, or proper error handling. The string in `expect` should explain the invariant, not just describe the call.
- **`Result::ok()` is banned.** It silently discards errors. Use `?` or handle the error.
- **`HashMap` / `HashSet` are banned.** Iteration order is non-deterministic. Use `crate::det_hash::IndexMap` or `BTreeMap` / `BTreeSet`. The alias fixes the hasher to constant-seed foldhash. Disabling `indexmap`'s `std` feature makes a bare `indexmap::IndexMap<K, V>` fail to compile in standalone builds; an embedding host can re-enable that feature through Cargo feature unification, so always use the alias. Construct with `IndexMap::default()` or `det_hash::map_with_capacity`.
- **`rand::rng` / `rand::thread_rng` are banned.** Unseeded RNG breaks determinism. The VM RNG is `state.rng`, an in-crate `VmRng` (SplitMix64) seeded via `set_rng_seed` (default seed 0); the `rand` crate is no longer a dependency. The clippy ban stays as a guard against re-introducing unseeded entropy; `allow-invalid = true` keeps it from tripping now that the path is gone.
- **`f64`/`f32` transcendental methods, `powi` and `mul_add` are banned.** The platform libm rounds differently across hosts. Use `crate::crmath` (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `ln`, `log10`, `pow`, `fma`); `src/crmath/` ports CORE-MATH, its tables are generated by `scripts/crmath_tables.py`, and `reference/deterministic-math.md` is the contract.
- **Host state is banned:** `std::env::args`/`var` and kin, `Instant`/`SystemTime` and their `now`, `RandomState` and `DefaultHasher`. Host entry points use scoped allows as needed; library code gets none.
- `dbg_macro`, `todo!`, `await_holding_lock`, `await_holding_refcell_ref` are denied.

Determinism is a product requirement, not a style preference - game replays depend on it.

## Architecture

Compilation pipeline: source bytes to `compiler::compile` (the parser builds each function's syntax tree, code generation turns it into register bytecode when the function ends, then `finalize`) to bytecode `Chunk` to executed by `State` in `vm::eval`. `Program::compile` is the one public entry to it.

**`src/instr.rs`** - fixed-width 64-bit register bytecode (`[opcode:8][A:16][B:16][C:16][D:8]`, or `[opcode:8][A:16][B:16][sJ:24]` for jumps). Registers and literal indices are 16-bit; count operands use `OPEN_COUNT` (`0xFFFF`) for an open (`Dynamic`/`All`) count, which `ArgCount`/`RetCount` round-trip through `to_operand`/`from_operand`. D carries inline-cache slots, the method-call mark and template entries. Constant operands are separate opcodes (`ADD_RR`, `ADD_RK`, `ADD_KR`, ...), not an RK bit. The `Builtin` enum gives well-known globals (`print`, `pairs`, `math`, etc.) fast array-indexed access through the global environment's builtin mirror instead of a hash lookup.

**`src/compiler/`** - `lexer.rs`, `parser.rs` plus `parser/` (builds the tree, with folding, upvalue resolution, literal pooling and every limit check at the parse events that always had them), `ast.rs` (the tree; anything the parser iterates is a flat list, so walking it never goes deeper than the parser's charged syntax depth), `codegen.rs` (Lua-style register allocation: locals in the low registers, temporaries at `freereg`; it copies a captured local before a later sibling that may run code, and gives multiple assignment fresh value temporaries, so stores see operands as evaluated), `verify.rs` (the admission verifier shared with snapshot load: register ranges against `max_stack`, pool indices, cache-slot ownership, jump targets and the open-results protocol), `token.rs`. Produces a `Chunk` with `code`, `max_stack`, literal pools, `nested` chunks for nested functions, `upvalues: Vec<UpvalueDesc>` (`Local(slot)` / `Upvalue(idx)`), and `line_info` mapping bytecode index to source line for stack traces.

**`src/vm.rs`** plus `src/vm/` is the runtime. `State` owns:

- `global_env: GlobalEnv` (`vm/global_env.rs`) - privately owns the global bindings (an `IndexMap<String, Val>` for deterministic iteration, with nil tombstones that keep indices stable), the `[Val; Builtin::COUNT]` builtin mirror that gives well-known globals a fast path, and the version that invalidates global and method lookup caches; restricted environments, snapshot restore and GC marking all go through it
- `stack: Vec<Val>` - single shared stack for both Lua and Rust frames; `stack_bottom` is the current frame's base. A Lua frame owns `[base, base + max_stack)` from admission on; the stack top sits at the frame top between instructions except inside an open-results window (a call asking for all results, or `...`, immediately followed by the call, return or set-list that consumes them). Slow paths copy their operands above the frame top, run the stack-top helpers, and take the result back into a register
- `heap: GcHeap` (mark-and-sweep, in `vm/object.rs`); strings are interned
- `upvalue_pool` + `open_upvalues` (sorted by stack index for efficient close-on-return)
- `cost: CostState` (`cost_meter.rs`) - a `used: u64` counter plus an optional budget (`limit`, `remaining: i64`); no budget means count-only. The signed `remaining` lets the operation that pushes you over budget complete, then the next costed op fails
- `call_stack: Vec<CallInfo>` - one activation record per active Lua call, innermost last, across every dispatch-loop invocation: tracebacks, `print` locations and the bytecode-cache sweep read it; `vararg_stack` holds the varargs of the records that have any (a GC root)
- `callbacks: Box<dyn HostCallbacks>` and a typed `user_data` slot
- `rng: VmRng` (in-crate SplitMix64) seeded via `set_rng_seed`

**Calls do not recurse natively from Lua.** Each dispatch-loop invocation (`State::run_invocation`) runs over a frame stack of its own (pooled in `frame_stacks`): `OP_CALL` to a Lua function (directly or through `__call`) admits the callee, pushes its `Frame` and continues the same loop in it; `OP_RETURN` pops back to the caller. A Lua callee keeps its function value in its slot just below its frame (that roots it) and returns its results into that slot. Only calls made from Rust (`call_unguarded`: host calls, metamethod handlers, generic-for iterators, library callbacks) start a nested invocation on the native stack, and `limits::MAX_NATIVE_CALL_DEPTH` counts them. An invocation owns the activation records from its entry upwards and unwinds exactly those on error. The call path is sensitive to store-forwarding: frames and records are written in place and dropped in place, never moved whole (see the comments in `vm/eval.rs`). `limits::MAX_CALL_DEPTH` and `limits::MAX_STACK_SIZE` bound Lua calls and values; docs/limits.md "Native stack" states the native-stack envelope.

**The stack cap is a real, global invariant, not a documented aspiration.**
Every path that can grow `State.stack` is either checked (a single
`check_stack_slot`), batch-preflighted (`check_stack_space(n)` before an
unchecked append), or provably net-neutral because it pops before it pushes.
`push_unchecked` is legal *only* after a preflight or when replacing popped
values - reaching for `self.stack.push(..)` directly re-opens the hole. The
public `push_*` methods, `new_table`, `get_global` and `open_libs` are all
fallible for this reason; a rejected operation must leave the stack, the string
pool and the function registry unchanged. The cap bounds the shared Lua/Rust
value stack, not total host memory, and enforcing it charges no cost.

**Error cleanup belongs to boundaries and resource owners.** Ordinary bodies
propagate with `?`. A new resource gets one owner that releases what it took
before propagating. `call_unguarded` clears failed call operands and restores
the caller's stack bottom; `get_table_unguarded` clears handler temporaries
and restores the key only before handler entry; `unwind_invocation` owns
failed Lua frames. `call_anchor` and `tostring_bytes` use the
`call_unguarded` boundary beneath their temporaries. A new host entry point
that can return an error to continuing code must define and test its own
boundary and operand contract. Handler writes survive an error.

**GC roots**: `vm::mark_gc_roots` is the *single source of truth* for what's reachable. Any allocator that may trigger GC must use this same root set. Closed upvalues are reached transitively via the closures that hold them, not as a separate root set - there's an explicit comment about this (`vm.rs` / `vm/object.rs`). Root an object with `GcHeap::mark`, never by pushing onto the worklist directly: `mark` sets the object's colour *and* queues it, whereas a bare push traces the object's children while leaving the object itself Unmarked, so sweep frees it and later reads dangle. A `debug_assert!` in `drain_mark_worklist` enforces this - the mistake is otherwise invisible until something actually collects.

**Stack indexing**: Rust callbacks (`RustFunc = fn(&mut State) -> Result<u8>`) use **1-based** indexing; Lua bytecode internally uses 0-based. `vm_aux.rs` and the lua_std modules show the 1-based pattern.

**Standard library**: `src/lua_std/{basic,math,string,string_format,table}.rs`, opened by `lua_std::open_libs(state)` from `State::with_callbacks` (`string_format.rs` is the Lua 5.4 `string.format` engine, registered by `string.rs`). Every `open_*` function pushes builtin functions onto the stack and uses `set_global` / `set_table_raw` to install them. `_G` exists but is wired through a metatable that proxies to the global environment (see `basic.rs`); it is not a real table.

**Cost model**: opcodes charge in `vm/eval.rs`'s dispatch; `analyze_cost` (`src/lib.rs`) walks bytecode statically and produces a `ScopeCost` tree (own + nested totals). `docs/budget.md` flags that structural ops like `while true do end` are intentionally free.

**Patterns**: Lua-pattern matching is an in-crate implementation in `src/patterns/`, not a dependency. `luapat.rs` compiles a pattern once into an item program plus interned 256-bit byte classes (`LuaPattern::new`), then matches over `&[u8]` with absolute subject offsets (`matches_bytes_from(subject, init)` - callers pass the *whole* subject with a start offset, never a re-slice, so `%f` keeps its left context). Recursion depth is passed by value and bounded at `MAXCCALLS = 200` active invocations, matching reference exactly; only capture start/end, `max_expand`/`min_expand` backtracking and the greedy `?` branch recurse, while every other continuation is a loop iteration (reference's `goto init`). Validation is lazy, as in reference Lua: `LuaPattern::new` is infallible; the first malformed point compiles to an `Item::Fail` that `matches_bytes_from` raises only when a match reaches it, and an unclosed capture is returned as `LuaCapture::Unfinished` for the library function that reads it to reject. That placement is contract, asserted by tests. `string.rs` has helpers `is_plain_lua_pattern` (fast path) and `gsub_replacement` (string / table / function replacements with Lua's `%0..%9` capture syntax).

## Tests

- `tests/run_examples.rs` - runs every regular Lua file under `examples/` with the binary cargo built for the test run (`tests/common` owns the binary path and the shared, manifest-rooted example list; no test may reference `target/` or spawn cargo), in parallel worker threads. The shared list skips symlinks and orders relative paths by encoded bytes to match `find -type f` and `LC_ALL=C sort`. That includes the stress examples and the generated `parse/large_source.lua`. Each example runs under `scripts/test_limited.sh` (so its memory cap applies) with a fixed 60-second timeout. The runner uses at most eight workers, bounded by available parallelism and the number of examples. It fails on timeout, nonzero exit, unexpected stderr, if any stdout line ends with `: false`, or if a file defining `_bench()` prints no line ending in `: true`. Examples that print test results follow the convention `print("test name: " .. tostring(condition))`. Adding an example that prints a line ending in `: false` will fail this test.
- `tests/error_handling.rs`, `gc_upvalues.rs`, `gsub_errors.rs`, `metamethod_errors.rs`, `rustfn_error.rs` - focused integration suites.
- `tests/diff_test.rs` is the differential gate: it uses the same shared example list, and checks that its ordered paths match `scripts/diff_test.sh --list-examples`. It drives the shell gate over the whole corpus in partitions that run concurrently inside one test, once per `[[check]]` feature shape in `brokkr.toml` (there is no separate brokkr script check for it), and also checks malformed timeout rejection and nonempty marker reasons. The corpus partitions are compiled out under the `hotpath` feature. `scripts/diff_gate.sh` is an optional debug-build wrapper around `diff_test.sh` for direct runs; nothing in `brokkr check` invokes it. Mark intentional divergences with a `-- DIFF: <reason>` comment; dellingr must still succeed, but the references are skipped. A file carrying a `-- SKIP-DIFF: <reason>` line is not run by the differential gate. Reference Lua is required for unmarked examples; the test leaves that check, and its error message, to `diff_test.sh`.
- `tests/bench_scripts.rs` checks the bench/ footer contract and that each `brokkr.toml` workload's `file` and `hotpath_file` give their `kernel` and shared helper functions matching `analyze_cost` fingerprints, which guards the same-kernel pairing described under Hotpath benchmarks.
- Unit tests live alongside their modules (e.g. `src/vm/object.rs` has GC tests).
- To intentionally refresh `tests/fixtures/save_golden_current.bin` after a
  serializer format change, run
  `brokkr run --debug --features snapshot regenerate_save_golden`. Use the
  debug profile the tests run under: release builds fold identical Rust
  functions, which changes the saved function ids.

The non-default `fault-injection` feature is test infrastructure, never a
production or native-stack-probe configuration. It adds a single-refusal
countdown and event log, armed after setup, to stack and depth preflights.
The ignored `error_boundary_fault_sweep` checks every eligible event of each
fail-fast corpus execution, including event identity, surviving operands,
notification count, State reuse and snapshot quiescence. Recovery execution
is covered separately by focused caught-and-continue tests. No standing
`[[check]]` shape enables it. Run on demand:

```sh
brokkr check --features fault-injection,snapshot -- --test error_boundary -- --include-ignored
```

The budget and natural-failure sweeps also run in ordinary check shapes.
`brokkr test error_boundary --release` exercises them without debug checks;
`brokkr test native_bytes_per_level` (also with `--release`) prints the
on-demand recurring-stack probe in default and snapshot configurations.

## Hotpath benchmarks

`examples/hotpath.rs` is a single Rust harness, and the binary brokkr invokes for `--bench` / `--hotpath` / `--alloc` runs. It takes one required positional arg, a script path resolved from the caller's working directory (`bench/arithmetic.lua`, `examples/fields/same_obj_read.lua`), and exits nonzero if the script does not define a global `_bench()` function. `--iterations N` overrides the warm-call count (default from `DEFAULT_ITERATIONS` in the harness source). When `BROKKR_MARKER_FIFO` names a writable path, the harness also emits brokkr sidecar markers (`PARSE`/`SETUP`/`COLD`/`WARM` START/END pairs, plus batched `WARM_BLOCK` sub-spans bounded by `MAX_WARM_BLOCKS` in the harness source) and `@name=value` counters; without the env var those emits are no-ops. A configured FIFO that cannot be opened or written fails the run rather than recording a partial marker stream. Probe and correctness bench scripts live in subdirectories of `examples/` alongside correctness tests:

```
examples/hotpath.rs        # harness (parse / cold call / warm calls)
examples/numerics/         # arithmetic, constants, math
examples/calls/            # global, local, method, vararg, fixedarg, many_literals, ...
examples/fields/           # same_obj_read, same_obj_write, polymorphic, ...
examples/globals/          # write
examples/iter/             # pairs, ipairs
examples/tables/           # fill, mixed, numeric_index, queue
examples/alloc/            # closure, short_tables, record_tables, gc_churn
examples/strings/          # mixed, patterns, literal_find
examples/parse/            # large_source (+ generate.py, see below)
examples/composite/        # benchmark (the seven-section mixed suite at ~1/100
                           # of bench/benchmark.lua, written as its hotpath_file)
```

Benches come in two flavours, and the distinction matters when reading the
README table. Most are *workload* benches, meant to look like plausible script
code. A few are *diagnostic probes* written to isolate one known-unoptimized
path so that a candidate optimization has something to measure against;
`numerics/constants`, `globals/write`, `calls/many_literals`,
`calls/loop_beside_closure`, `alloc/gc_churn` and `parse/large_source` are
all probes. Their ratios against
reference Lua are worst cases by construction, not representative numbers, and
each carries a header comment naming the candidate it exists to measure and the
sibling bench to compare it against.

`examples/parse/large_source.lua` is generated - regenerate with `python3
examples/parse/generate.py`, do not edit it by hand. `brokkr check` verifies the
checked-in output with `python3 examples/parse/generate.py --check`. It exists because
parse and codegen costs that scale with source size cannot surface in the other
benches, which are 15-80 lines. Note the parser caps a chunk at 255 nested functions, which bounds how
many top-level definitions the generator can emit; size comes from making each
function longer instead.

Each bench script is also a standalone-runnable test: top-level setup + a `_bench()` function + an outer loop that calls `_bench()` enough times for hyperfine resolution and prints `<name>: true`. This lets one file serve three masters: the hotpath harness (calls `_bench` directly for parse/cold/warm phasing), `tests/run_examples.rs` (executes the standalone runner, asserts no `: false`), and `scripts/bench.sh` (hyperfine timings vs reference Lua 5.2/5.4/5.5 + LuaJIT). The default `bench.sh` script list comes from the `hotpath_file` entries in `brokkr.toml`; per-script hyperfine JSON and the matching script list remain under `.scratch/bench-results/`.

`bench/` holds the brokkr *verdict* workloads: seconds-scale ports of the curated examples/ benches - same kernels, wrapped in a repeat loop calibrated so one `_bench()` call is ~100ms, footer fixed at 30 calls, which lands the standalone run at ~3s. The harness warm phase scales with `DEFAULT_ITERATIONS` in `examples/hotpath.rs`. A short launch cannot resolve the deltas the optimization backlog targets; a seconds-scale one can. Ordinary `[dellingr.workloads.*]` registrations pair two files: `file`/`xxh128` (the bench/ port, for wall-clock verdict runs) and `hotpath_file`/`hotpath_xxh128` (the examples/ counterpart, for instrumented profiling, which only works at the ms scale). `large_source` is a parse/codegen probe and an exception: both modes use the generated examples/ source, and its timing is a parse/codegen diagnostic rather than a calibrated runtime verdict. `queue` cannot be baselined with `--bench --commit` before 2bcc71e (the commit that bounded a table's array storage): on older VMs every `pairs` start walks the queue's whole push history, so the seconds-scale `bench/queue.lua` runs quadratically for hours. Compare across that commit with `--hotpath`/`--alloc` on the ms-scale `examples/tables/queue.lua`, or with a smaller one-off variant. Editing a pinned file requires re-hashing its pin, which is the deliberate signal that older stored results no longer compare. For execution workloads, the paired files must keep the same kernel in the same proportions, or per-function distributions measured on the ms file stop describing the seconds-scale walls. `bench/` is intentionally outside every examples/ glob - `run_examples.rs`, the diff gate, and `bench.sh` do not see it - so the fast test/diff/ratio surfaces and the slow verdict surface stay independent.

The harness measures four phases on one State and emits KV pairs to stderr: `parse_us`, `cold_call_us`, `warm_avg_us` (averaged over the configured warm iteration count; fractional, three decimals), plus the dellingr-specific `cold_cost` / `warm_avg_cost` and the derived `cost_per_us`. Wall time is the benchmarking metric; cost is a deterministic property of the workload and rides along as a fingerprint (a cost change means the script or compiler changed, not the VM's speed). `setup_*` and `final_*` heap/object counts bracket GC pressure. Note the harness runs the script's top level during setup, so the standalone footer's calls execute there - the "cold" call is only cold with respect to what the footer has not already warmed.

Internal functions carry `#[hotpath::measure]` annotations across `vm/table.rs`, `vm/table_ops.rs`, `vm/eval*.rs`, `vm/object.rs` and the compiler front end. The annotation is a no-op when the `hotpath` cargo feature is off, so it costs nothing in normal builds; it is cheap to add one when a new candidate needs a measurement point, and worth doing at the same time as the bench that will exercise it. **Don't add `#[hotpath::measure]` to a function that stays live across a native re-entry** (`call_unguarded`, `begin_call`, `begin_callable_call`, `run_invocation`, `Frame::run`, and the metamethod/iterator/library helpers between the dispatch loop and their `call_unguarded`): every native level carries those frames, and the limit tests drive `limits::MAX_NATIVE_CALL_DEPTH` levels on the envelope's thread (4 MiB in dev, 2 MiB in release; docs/limits.md "Envelope"). Instrumented builds already get a larger test stack (`tests/common/stack.rs`), but the margin is not unlimited. A function *called from* the dispatch loop whose frame is popped before any re-entry is fine. The release frames of the live-across-re-entry functions are also sensitive to live-range shape, not only to what is inlined: a value returned for the caller to consume after a drop, or a `match` scrutinee `Result` whose arm makes the re-entering call, costs a spill slot or an unshared result slot there, and deleting a redundant line in something inlined into `Frame::run` can re-pack its spill slots one alignment step larger. Measure with `brokkr test native_bytes_per_level` (dev and `--release`, default and `snapshot`) and attribute with `scripts/frame_sizes.py` before and after.

When adding annotations, verify the `hotpath` configuration still builds (`brokkr check --features hotpath`); the plain gate does not compile that feature, so a bad annotation is otherwise invisible.

To add a new target: write `examples/{category}/{name}.lua` defining `_bench()` and a standalone runner footer. No Rust changes. `tests/run_examples.rs` and the diff gate pick it up automatically; `scripts/hotbench.sh`, the default `scripts/bench.sh` list and brokkr's `--hotpath`/`--alloc` reach it only once it is the `hotpath_file` of a `[dellingr.workloads.*]` registration in `brokkr.toml`.

```sh
./scripts/hotbench.sh same_obj_read           # KVs from harness + hyperfine wall time
./scripts/hotbench.sh fill --runs 20          # extra args pass through to hyperfine
./scripts/hotbench.sh --alloc fill            # alloc tracking on the KV side
```

`scripts/hotbench.sh` takes a brokkr workload name and resolves it to that
registration's `hotpath_file`. The `--alloc` flag selects `hotpath-alloc` for
the harness build; without it the build uses `hotpath`. Hyperfine uses a fixed
warmup of two runs. It builds both the regular `dellingr` binary and the
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
an area for local upstream checkouts, which remain ignored. The comparison
script lives at `research/bench_luars.sh`. The README's `lua-rs` column comes
from this script - its footnote pins the captured date and upstream commit, so
refresh both together.

## Project conventions worth knowing

- Examples in `examples/` are part of the test surface - don't add throwaway scripts there. brokkr verdict workloads live in `bench/` (see above).
- The CLI prints `Cost used: N` to stderr after each run (and `--analyze` prints its caveat to stderr); `--quiet` suppresses the cost line. `scripts/diff_test.sh` and `tests/run_examples.rs` pass `--quiet`, and `run_examples.rs` fails on any stderr from a passing example.
- The crate was lifted out of a game project (originally extracted from `fcomm2`); some doc comments still mention `FleetCallbacks` etc. as illustrative examples.
- `target/` is a symlink to a shared cargo cache.
- `notes/optimizations.md` is a working backlog of forward-looking optimization ideas (rejected, deferred, hypothetical). Items get deleted as they ship or stop being worth tracking. Not a discrepancy doc.
- `notes/todo.md` is the matching backlog for non-perf forward-looking ideas (features, refactors, ergonomic gaps). Same conventions: working list, items deleted as they land.
- `notes/work.md` holds the current fix loop's problem statement and plan; `notes/fix-loop.md` is the process that drives it.

## Document folders

The standing layout, across every project. Three live folders plus one retired,
split by durability first, subject second.

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | Durable in-repo reference for anyone working on or with the code - how the thing is built and why: `architecture.md`, `technical-implementation-spec.md`, `performance.md` (the durable record of measured numbers over time), invariants, protocol contracts | Citable from source as a source of truth. What it says must be true. |
| `docs/` | Durable in-repo documentation of how the thing is used - guides, CLI reference, the consumer-facing API surface. Sometimes exposed as a hand-edited VitePress gh-pages site | Same must-be-true rule. |
| `notes/` | Transient - work items (`todo.md`), future plans, hypotheticals, bug reports, research, analysis. Things that will die | No truth guarantee. Nothing durable cites it. |
| `plans/` | Retired | Plan documents are transient: they go in `notes/`. |

`reference/` and `docs/` are both durable and both binding. The difference is
subject, not audience: `reference/` covers how the thing is built and why - what
you need in order to change it safely - while `docs/` covers how it is used. A
developer or library consumer reads both. Where a project publishes a site,
`docs/` is what gets published; the folder means the same thing either way.
`notes/` is neither durable nor binding, which is the whole point of keeping it
separate: a document that may be wrong must not sit where a document that must
be right is expected.

The dependency direction is therefore one-way. `notes/` may cite `docs/` and
`reference/`; nothing durable may cite `notes/` - not a code comment, not
`docs/`, not `reference/`. A code comment must carry its full context, because
it outlives the note.

**Root-level convention files are exempt.** `AGENTS.md`, `CLAUDE.md`,
`README.md`, `LICENSE`, `CHANGELOG.md` and their kin are found by tooling and by
convention at the repository root, and stay there. These folders govern
documents we chose where to put, not files whose location is dictated.

In `notes/`, `docs/` and `reference/` alike, avoid citing source line numbers -
they drift fast.
