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
- `brokkr test <NAME>` - focused single-test runner. With no per-sweep profile override, this repo's `[test] debug = true` selects the dev profile by default; explicit profile flags take precedence. Always passes `--include-ignored --nocapture`; the test thread count comes from the selected profile, one when unset. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--timeout <SECS>` - raise brokkr's per-test cap for the resolved tests (the only exception to it), each run as its own process.
  - `--timings` - after the run, list every test that ran by wall time.
  - `--debug`, `--release` - force the dev or release profile. Use `--debug` for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time dominates wall time and the optimization level doesn't change the behavior under test.

`dellingr` is a single-crate repo, so `-p` is normally unnecessary; pass `-p dellingr` if a brokkr invocation requires an explicit package.

Running scripts (the binary, not the test runner):

```sh
brokkr run --release -- path/to/script.lua             # run a script
brokkr run --release -- --analyze path/to/script.lua   # static cost analysis, no execution
brokkr run --release -- --limit 100000 path/to/script.lua  # run with a cost budget
brokkr run --quiet -- script.lua                      # forwards raw to cargo run
```

Differential testing against reference Lua is part of `brokkr check`: the
example corpus tests compare every example not marked `-- SKIP-DIFF:` with
`lua5.2` and `lua5.4` (see Tests). The scripts below are manual tools outside
the gate, and each owns its own requirements:

```sh
./scripts/diff_test.sh        # diff vs Lua 5.2 / 5.4; prints "ok", or one FAIL/TIMEOUT line per failing script
./scripts/test_limited.sh <cmd>  # run <cmd> under a virtual-memory cap (for stress tests)
```

`diff_test.sh` compares chosen files or directories (default: the listed
corpus, read from `tests/common/examples.rs` through
`scripts/example_list.py --paths`) by the same marker rules as the corpus
tests, except that it compares stdout and stderr together and ignores the
references' exit status. It needs bash, python3 for the default list and,
unless `--timeout 0`, GNU `timeout`.
It exits 1 at the first example not marked `-- SKIP-DIFF:` if
either reference Lua is missing and names the executable. It requires dellingr
to exit zero even for `-- DIFF: <reason>` files. DIFF files still run both
references after dellingr succeeds, and fail as a stale marker when dellingr's
output matches either one. Both `-- DIFF:` and
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

`scripts/check_tools.py`, the first pre-clippy `brokkr check` script check
after the `.scratch/` wipe, verifies every external tool the gate needs and
nothing else: python3 3.11 or newer, `lua5.2` and `lua5.4` at pinned releases
with pinned compatibility globals, `git` and `nm`. Bumping a host's reference
Lua means re-checking the corpus and updating the pins there.

The scripts read no environment variables of their own. The hotpath crate
consumes `HOTPATH_OUTPUT_PATH`, set by brokkr, and the harness reads
`BROKKR_MARKER_FIFO`, also set by brokkr. Script constants live in
`scripts/lib.sh`; hyperfine uses a fixed warmup of 2 runs.

`scripts/native_calls.py` is the native-stack guard: it checks
`reference/native-calls.toml`, the inventory of every function that can call
back into Lua and every scenario `tests/native_stack.rs` runs, against `src/`
and the test, and prints `ok` (a `brokkr check` script check). Every call to
an inventoried function is wrapped in `native_call!("<edge-id>", call)`, a
macro that expands to the bare call unless the debug-only `native-call-trace`
feature records the edge; the `native-call-marker` textlint rule also
enforces the wrapper, and `python3 scripts/native_calls.py sites` lists the
call sites. The curated `native-call-coverage` check shape runs
`tests/native_stack.rs`'s `native_call_edge_coverage` under that feature, so
each run's first descent witness must have its scenario's inventoried path
live in order; separately, the whole-run union covers every non-excluded edge.
`scripts/test_native_calls.py` holds fixture tests for its lexer and call
model and runs as a separate `brokkr check` script check.
reference/native-calls.md explains the inventory, the direct-call convention
the lexical check relies on, and how to add a path.
`scripts/poison_contract.py`, built on the same lexer, checks that every
public fallible `State` method starts with the entry check that refuses a
poisoned State and an unsupported floating-point environment, and
that every infallible one is on its reviewed allowlist;
`scripts/test_poison_contract.py` holds its fixture tests. Both are `brokkr
check` script checks.

`scripts/notes_drop.py` and `scripts/notes_move.py` are maintainer helpers for
the `notes/` working documents, run directly when needed; no gate invokes them.
They share `scripts/notes_sections.py`, which finds `## ` section headings
while skipping fenced code blocks; `scripts/test_notes_sections.py` tests that
handling and runs as a `brokkr check` script check.
`scripts/frame_sizes.py <binary> <symbol substring>...` prints native frame
sizes (prologue `sub rsp` plus pushes) from a built binary's disassembly, with
`--calls` for each function's call targets (inlining decisions) and `--body`
for an address-blanked body dump; `scripts/asm_diff.py` diffs two such dumps
(`--slots` lists the `%rsp` slots each build uses). They need `objdump` and
are how a `tests/native_stack.rs` probe delta gets attributed to a function.
`scripts/perf_hot_lines.py <annotate-output> [min-percent] [context]` filters
saved `perf annotate --stdio` output down to its hot instructions, which is how
a `Frame::run` listing of several hundred KB becomes readable.

Shell scripts live in `scripts/` and share `scripts/lib.sh`, which builds bin
and example targets through `brokkr build` and captures the immutable artifact
path it prints, so no script runs cargo or hard-codes a `target/` path
(`research/bench_luars.sh` builds dellingr the same way; only its external
lua-rs comparator is expected at `research/lua-rs/target/release/lua-rs`). Each script anchors itself to the repo
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
- **Entropy crates are refused at the dependency boundary.** Unseeded RNG breaks determinism. The VM RNG is `state.rng`, an in-crate `VmRng` (SplitMix64) seeded via `set_rng_seed` (default seed 0). `brokkr.toml`'s `no-host-entropy` dependency rule fails the gate if `rand`, `rand_core` or `getrandom` enters dellingr's dependency graph; there are no Clippy bans for them, because a path into an absent crate cannot resolve and such a ban would never fire.
- **`f64`/`f32` transcendental methods, `powi` and `mul_add` are banned.** The platform libm rounds differently across hosts. Use `crate::crmath` (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `ln`, `log10`, `pow`; inside a kernel, the fused multiply-add of its selected backend, never `mul_add`); `src/crmath/` ports CORE-MATH, its tables are generated by `scripts/crmath_tables.py`, and `reference/deterministic-math.md` is the contract. The lints cannot see dependencies or LLVM libcalls, so `scripts/libm_symbols.py` also rejects platform libm transcendental symbols in a linked binary (`scripts/test_libm_symbols.py`, a `brokkr check` script check, tests its parser). `brokkr check` gates the dev-profile CLI through `corpus::libm_symbols` and the fat-LTO release CLI through the dedicated `libm_symbols` target in both release-native-stack sweeps. Each test uses its sweep's `CARGO_BIN_EXE_dellingr`. This covers linked CLI code, not library code discarded by the linker.
- **Host state is banned:** `std::env::args`/`var` and kin, `temp_dir`, `current_dir`, `home_dir`, `std::process::id`, `std::thread::current`/`available_parallelism`, pointer `addr`/`expose_provenance`, `Instant`/`SystemTime` and their `now`, `RandomState` and `DefaultHasher`. Host entry points and measurement harnesses use scoped allows with reasons; production library code gets none. Pointer-to-integer casts still require review; method bans do not cover casts.
- **Every ban is proven to fire.** `fixtures/lint-policy/` is a standalone crate with one source file per `clippy.toml` entry, each calling or naming its banned path; brokkr's `determinism-policy` fixture lint runs Clippy over it (both with `clippy.toml` found by walking up and with `CLIPPY_CONF_DIR` set) and checks the lints each file raises against `expect.toml`. `fixtures/lint-policy/check_policy.py`, a script check, keeps `expect.toml` in step with `clippy.toml` and refuses nested Clippy configuration files that would shadow the policy. A new ban needs its fixture file and manifest entry.
- `dbg_macro`, `todo!`, `await_holding_lock`, `await_holding_refcell_ref` are denied.

Determinism is a product requirement, not a style preference - game replays depend on it.

## Architecture

Compilation pipeline: source bytes to `compiler::compile` (the parser builds each function's syntax tree, code generation turns it into register bytecode when the function ends, then `finalize`) to bytecode `Chunk` to executed by `State` in `vm::eval`. `Program::compile` is the one public entry to it.

**`src/instr.rs`** - fixed-width 64-bit register bytecode (`[opcode:8][A:16][B:16][C:16][D:8]`, or `[opcode:8][A:16][B:16][sJ:24]` for jumps). Registers and literal indices are 16-bit; count operands use `OPEN_COUNT` (`0xFFFF`) for an open (`Dynamic`/`All`) count, which `ArgCount`/`RetCount` round-trip through `to_operand`/`from_operand`. D carries inline-cache slots, the method-call mark and template entries. Constant operands are separate opcodes (`ADD_RR`, `ADD_RK`, `ADD_KR`, ...), not an RK bit. The `Builtin` enum gives well-known globals (`print`, `pairs`, `math`, etc.) fast array-indexed access through the global environment's builtin mirror instead of a hash lookup.

**`src/compiler/`** - `lexer.rs`, `parser.rs` plus `parser/` (builds the tree, with folding, upvalue resolution, literal pooling and every limit check at the parse events that always had them), `ast.rs` (the tree; anything the parser iterates is a flat list, so walking it never goes deeper than the parser's charged syntax depth), `codegen.rs` (Lua-style register allocation: locals in the low registers, temporaries at `freereg`; it copies a captured local before a later sibling that may run code, and gives multiple assignment fresh value temporaries, so stores see operands as evaluated), `verify.rs` (the admission verifier shared with snapshot load: register ranges against `max_stack`, pool indices, cache-slot ownership, jump targets and the open-results protocol), `token.rs`. Produces a `Chunk` with `code`, `max_stack`, literal pools, `nested` chunks for nested functions, `upvalues: Vec<UpvalueDesc>` (`Local(slot)` / `Upvalue(idx)`), and `line_info` mapping bytecode index to source line for stack traces.

**`src/vm.rs`** plus `src/vm/` is the runtime. `State` owns:

- `global_env: GlobalEnv` (`vm/global_env.rs`) - owns the global table, an ordinary heap `Table` that is `_G` (a raw State has one too, unbound), with a slot policy that keeps a cleared global's slot and, until `seal_globals`, every tombstone. Global lookup caches hold (table version, hash slot or absence by slot count); the builtin names are interned at construction and read through a mirror validated by the heap's mutation epoch for that table (`GcHeap::watched_epoch`), so builtins cost no slot-map lookup. `with_restricted_env` swaps the table's contents, never its identity. Under `snapshot` it also keeps the pin list sealing records; snapshot save and restore of the globals go through it
- `stack: Vec<Val>` - single shared stack for both Lua and Rust frames; `in_flight.stack_bottom` is the current frame's base. A Lua frame owns `[base, base + max_stack)` from admission on, and admission also checks `FRAME_HEADROOM` free slots above it; the stack top sits at the frame top between instructions except inside an open-results window (a call asking for all results, or `...`, immediately followed by the call, return or set-list that consumes them). Instructions work on their registers in place: the table and length instructions run an outlined resolver that calls nothing (`State::resolve_index`, `store_field`, `resolve_length`), and only a function handler's call window, which the headroom always fits, goes above the frame top
- `heap: GcHeap` (mark-and-sweep, in `vm/object.rs`); strings are interned
- `upvalue_pool` + `in_flight.open_upvalues` (sorted by stack index for efficient close-on-return)
- `cost: CostState` (`cost_meter.rs`) - a `used: u64` counter plus an optional budget (`limit`, `remaining: i64`); no budget means count-only. The signed `remaining` lets the operation that pushes you over budget complete, then the next costed op fails
- `in_flight: InFlight` (`vm/inventory.rs`) - every execution-lifetime field, back at its initial value whenever no call is in progress: the frame base, the activation records, varargs, open upvalues, transient roots, pending cache identities, the call and metamethod depths and the active Rust function
- `in_flight.call_stack: Vec<CallInfo>` - one activation record per active Lua call, innermost last, across every dispatch-loop invocation: tracebacks, `print` locations and the bytecode-cache sweep read it; `vararg_stack` is one shared arena of extra arguments, each record owning the suffix from its `vararg_start` while innermost and releasing it on pop (its live length is a GC root)
- `callbacks: Box<dyn HostCallbacks>` and a typed `user_data` slot
- `rng: VmRng` (in-crate SplitMix64) seeded via `set_rng_seed`
- `rust_fns: RustFnTable` (`vm/rust_fn.rs`) - the append-only Rust function table. A `Val::RustFn` is an index into it, never a code address: the standard library's catalog (`lua_std::CATALOG`) fills fixed positions at the front, and each `add_rust_fn` / `register_rust_fn` call appends a new entry with its own identity (the host holds a State-bound `RustFnHandle`). Under `snapshot` it maps stable save ids to entries, one id per entry. No function-pointer comparison decides identity, registration or a fast path

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

**GC roots**: `State::for_each_gc_root` (`vm/inventory.rs`) is the *single source of truth* for what's reachable: it destructures `State` without a rest pattern, so a new field does not compile until it is visited as a `GcRoot` or bound to `_` with a reason, and `GcRoot` is matched exhaustively by the collector (`State::mark_roots`) and by the snapshot (`save_state::save_coverage`). Its sibling `State::rest_state` classifies every field's lifetime the same way: execution-lifetime state lives in the `InFlight` sub-struct (`state.in_flight`), host-held state (the value stack, suspended environments) may be non-empty between calls but not for a snapshot, and everything else is persistent. Any allocator that may trigger GC must use this same root set. Closed upvalues are reached transitively via the closures that hold them, not as a separate root set - there's an explicit comment about this (`vm.rs` / `vm/object.rs`). Root an object with `GcHeap::mark`, never by pushing onto the worklist directly: `mark` sets the object's colour *and* queues it, whereas a bare push traces the object's children while leaving the object itself Unmarked, so sweep frees it and later reads dangle. A `debug_assert!` in `drain_mark_worklist` enforces this - the mistake is otherwise invisible until something actually collects.

**Stack indexing**: Rust callbacks (`RustFunc = fn(&mut State) -> Result<u8>`) use **1-based** indexing; Lua bytecode internally uses 0-based. `vm_aux.rs` and the lua_std modules show the 1-based pattern.

**Standard library**: `src/lua_std/{basic,math,string,string_format,table}.rs`, opened by `lua_std::open_libs(state)` from `State::with_callbacks` (`string_format.rs` is the Lua 5.4 `string.format` engine, registered by `string.rs`). Every `open_*` function pushes builtin functions onto the stack and uses `set_global` / `set_table_raw` to install them. `_G` is the global table itself; `open_base` gives it a protected metatable (`__metatable = false`, nothing else), because bare global access is raw and must never disagree with `_G.x`.

**Cost model**: opcodes charge in `vm/eval.rs`'s dispatch; `analyze_cost` (`src/lib.rs`) walks bytecode statically and produces a `ScopeCost` tree (own + nested totals). `docs/budget.md` flags that structural ops like `while true do end` are intentionally free.

**Patterns**: Lua-pattern matching is an in-crate implementation in `src/patterns/`, not a dependency. `luapat.rs` compiles a pattern once into an item program plus interned 256-bit byte classes (`LuaPattern::new`), then matches over `&[u8]` with absolute subject offsets (`matches_bytes_from(subject, init)` - callers pass the *whole* subject with a start offset, never a re-slice, so `%f` keeps its left context). Recursion depth is passed by value and bounded at `MAXCCALLS = 200` active invocations, matching reference exactly; only capture start/end, `max_expand`/`min_expand` backtracking and the greedy `?` branch recurse, while every other continuation is a loop iteration (reference's `goto init`). Validation is lazy, as in reference Lua: `LuaPattern::new` is infallible; the first malformed point compiles to an `Item::Fail` that `matches_bytes_from` raises only when a match reaches it, and an unclosed capture is returned as `LuaCapture::Unfinished` for the library function that reads it to reject. That placement is contract, asserted by tests. String operations use a fast path for plain byte patterns and otherwise use the Lua-pattern matcher. `string.gsub` dispatches replacements by type and supports `%0..%9` capture substitutions.

## Tests

- `tests/integration/` (`main.rs`) is one crate holding every suite that can share a process, as modules grouped by subject: `cli`, `compiler`, `corpus`, `cost`, `failures`, `host`, `library`, `runtime`, and `snapshot`, which compiles only under the `snapshot` feature. A focused run names a subject or a file by module path (`brokkr test snapshot::`, `brokkr test gc_upvalues::`); the filter is a substring, so no module may end with a subject's name (the library's unit tests still match by subject: `compiler::` also runs the compiler's unit tests). Its `support` module holds the CLI binary path, CLI spawning (scripts go in on stdin), the shared compile-load-call helpers (`support::run`, `eval`, `run_error`, `eval_number`, `cost_of`, ...) and `Recorder`, a `HostCallbacks` that records prints and error notifications. `tests/native_stack.rs` is kept apart because an overflow at the envelope's edge aborts its process. The other named exception is `tests/libm_symbols.rs`, which checks each release sweep's own CLI without compiling the full integration suite. `tests/common/` holds only files another target also includes by path. `corpus::test_conventions` keeps that shape: no other top-level target, every integration source reached by `mod` declarations from `main.rs` (a `cfg` only on its allowlist), no dead-code allowances or file-level `cfg`s, and a `// process-global:` comment on every `static`, panic-hook, environment or working-directory use; `scripts/test_tree_anchor.py`, a `brokkr check` script check, confirms from outside the crate that those tests still compile and run. The suites share one process, so a new one must not leave state another test can observe.
- `tests/common/examples.rs` is the example corpus's only enumeration: one `name: "examples/<path>.lua",` entry per Lua file under `examples/` (the stress examples and the generated `parse/large_source.lua` included), in byte order of the path, inside a single `examples! { ... }` invocation. The name is the path below `examples/` without `.lua`, directories joined by `_` (`examples/calls/global.lua` is `calls_global`). Each includer defines its own `examples!` macro and then `include!`s the file: `support::examples::EXAMPLES` and the library's `example_list::EXAMPLES` (for `compiler_examples_verify_and_disassemble` and the inline-cache differential) expand it to a slice of paths, which `cost::cost_check_interval` and `corpus::bench_scripts` also iterate, and `corpus::run_examples` and `snapshot::corpus_roundtrip` expand it to one test per example. `scripts/example_list.py`, the pre-clippy `example-list` script check, keeps the list equal to the `.lua` files on disk, so a new example or a stray untracked file fails the check until it is listed or removed; `python3 scripts/example_list.py --write` regenerates it (a path component outside `[a-z0-9_]`, a name colliding with another, a leading digit, `_` alone, a Rust keyword, and a symlink or a `.lua` entry that is not a regular file under `examples/` are refused), and `--paths` prints it for `scripts/diff_test.sh`; `scripts/test_example_list.py` holds its fixture tests and runs as a `brokkr check` script check. No Rust code walks `examples/`: the `example-corpus-only-from-the-list` and `no-examples-directory-literal` textlint rules refuse the old walker's name, a `read_dir` naming the tree and a string literal naming the directory itself.
- `tests/integration/corpus/run_examples.rs` is the example corpus gate: one test per listed example in its `standalone` module (`brokkr test standalone::calls_global` runs one). Each runs its example once with the binary cargo built for the test run (`support` owns the binary path; no test may reference `target/` or spawn cargo). On Unix, each example runs directly with the `TEST_LIMIT_KB` address-space cap from `scripts/lib.sh`, applied by `support::process::limited_command`; `scripts/test_limited.sh` remains available as a manual wrapper. An example fails on nonzero exit, unexpected stderr, more than 1 MiB of output, if any stdout line ends with `: false`, or if a file whose main chunk assigns a global `_bench` (read from its compiled bytecode, so a `local function _bench` does not count) prints no line ending in `: true`. Examples that print test results follow the convention `print("test name: " .. tostring(condition))`. Adding an example that prints a line ending in `: false` will fail this test.
  - Every example not marked `-- SKIP-DIFF: <reason>` is then run by `lua5.2` and `lua5.4`, spawned from Rust by `support::process::reference_command` (from the manifest directory, under `LC_ALL=C` and `-E`, with the address-space cap and, on Unix, `RLIMIT_NPROC` 0 so a reference cannot start a process; a run as uid 0 is refused), and compared in memory. A reference matches when it exits 0, writes no stderr and prints exactly dellingr's stdout. An unmarked example must match at least one. Mark an intentional divergence with a `-- DIFF: <reason>` comment: dellingr must still pass, and its output must match neither reference (a match fails as a stale marker). Every marker needs a nonempty reason, which the `diff-marker-needs-reason` textlint rule also reports before anything compiles; a file may not carry both kinds. A reference that cannot be judged (it fails to start, is killed by a signal, overflows its output or is cut by the deadline) fails the example whichever marker it carries. The comparison is compiled out under the `hotpath` feature, which `hotpath-alloc` also enables; the pass rules still apply there.
  - Each example's processes share a 15-second deadline from the start of its test, asserted below a restated copy of brokkr's 20-second per-test cap (brokkr does not export it): no process starts after it, and a process still running at it is killed and named. A test that passes brokkr's cap stops the whole `brokkr check` run, every other example's verdict with it, so the deadline keeps a hung process this example's own failure. It bounds child execution and output capture, not the in-process compile; `brokkr test --timeout` raises brokkr's cap but not this deadline. The CLI's snapshot-shape probe (`support::dellingr_bin`) has a deadline of its own, and its verdict is kept, so concurrent tests do not each wait on a hung probe.
  - `brokkr check` runs the standalone tests in a lane of their own per feature shape: the `default` and `snapshot` sweeps skip `corpus::run_examples::standalone::`, and their siblings `default-corpus` and `snapshot-corpus`, of the same build, run only those tests with `parallel = {}` (brokkr's shared-process parallel lane, as many in flight as one last-level cache domain has cores). The rest of the integration crate, million-slot at-cap tests included, stays serial. All four entries and the two doc twins pin `feature_unification = "workspace"`, which brokkr would otherwise apply to the parallel siblings alone, so clippy and rustdoc still lint each shape once. `standalone_tests_run_in_one_parallel_lane_per_shape` holds `brokkr.toml` to that partition, with the selector taken from the module's own path. `brokkr test` drops the filters and dedupes each sibling onto its main sweep, so a focused run executes once per shape, the instrumentation shapes included.
  - Every process a Rust test starts is constructed in `tests/integration/support/process.rs`; the `test-processes-through-support` textlint rules refuse `Command` construction and imports anywhere else under `src/` and `tests/`, except two `python3` spawns (`tests/common/libm_symbols.rs`, `src/crmath/tests/golden.rs`) carrying a `process-spawn-exception:` marker.
- `tests/native_stack.rs` backs docs/limits.md's "Envelope": each path in `reference/native-calls.toml` recursed to `MAX_NATIVE_CALL_DEPTH` under every metamethod chain kind and deepest-point leaf, on `tests/common/stack.rs`'s profile-dependent thread size. `brokkr.toml` runs it in dev (the default and snapshot sweeps) and release (`release-native-stack` and `release-native-stack-snapshot`). A new call path into Lua needs a scenario there; reference/native-calls.md has the steps.
- `tests/integration/corpus/bench_scripts.rs` checks every `xxh128` and `hotpath_xxh128` pin in `brokkr.toml` against the registered file bytes, the bench/ footer contract, that each bench/ file runs (its setup without the footer, then `_bench` with `local K` patched to 1, or its local `kernel()` once when it has no `K`, or else one full `_bench` call, each phase under a cost budget, loaded under the harness's bare-path chunk name), and that each `brokkr.toml` workload's `file` and `hotpath_file` give their `kernel` and shared helper functions matching bytecode and literal pools as well as `analyze_cost` fingerprints, which guards the same-kernel pairing described under Hotpath benchmarks. It also requires every examples/ file that defines a global `_bench` to print `tostring(_bench())` in its success line, so the corpus tests' reference comparison checks a computed value rather than a constant.
- Unit tests live alongside their modules (e.g. `src/vm/object.rs` has GC tests).
- Tests never write files (a `[[textlint]]` in `brokkr.toml` enforces it): CLI tests hand Lua source to the binary on stdin, and tests whose subject is a path are unit tests of `parse_args` in `src/main.rs`. `.scratch/` is the developer's gitignored area, wiped at the start of every `brokkr check`.
- To intentionally refresh `tests/fixtures/save_golden_current.bin` and its
  generated `.txt` companion after a reviewed save-output change, run
  `brokkr run --features snapshot regenerate_save_golden`. Never
  regenerate the archived `save_released_v*.bin` fixtures
  (docs/save-state.md). Any profile writes the same payload (saved Rust
  functions are function-table ids, not code addresses), and the
  `release-save-golden` check shape holds the release build to the fixture.

The non-default `fault-injection` feature is test infrastructure, never a
production or native-stack-probe configuration; `src/lib.rs` refuses to
compile it without debug assertions. It adds a single-refusal
countdown and event log, armed after setup, to stack and depth preflights.
`error_boundary_fault_sweep` checks every eligible event of each fail-fast
corpus execution, including event identity, surviving operands, notification
count, State reuse and snapshot quiescence. Recovery execution is covered
separately by focused caught-and-continue tests. The curated
`instrumentation-faults` `[[check]]` shape in `brokkr.toml` enables it together
with `snapshot`, `hotpath`, `hotpath-alloc` and `crmath-sweeps`: it compiles
and lints every target under those features and runs only the refusal sweep,
the gmatch recovery test and the library's fault-injection unit tests.
For every `curated = true` shape, `corpus::test_conventions` fails if an
`only` entry matches no test in the shape's targets, or if a test gated on a
shape feature that a selected test is gated on (here `fault-injection`) is not
selected.

The non-default `disable-inline-caches` feature is debug-only test
infrastructure in the same sense (`src/lib.rs` refuses it without debug
assertions). It makes every inline-cache and builtin-mirror read miss; the
library's unit-test build alone can turn hits back on per thread, so
`vm::cache_differential::corpus_inline_cache_differential` runs each example
with hits on and off in one build and compares prints, cost and errors. The
curated `inline-cache-differential` shape is the only one that enables it and
runs only that test: other cache tests would fail with hits off.

The non-default `cost-check-interval` feature is debug-only test
infrastructure too (`src/lib.rs` refuses it without debug assertions). With it
off, the dispatch loop's flush test is the plain comparison against
`tuning::COST_CHECK_INTERVAL`; with it on, the loop reads the interval from a
thread-local control (the production value unless overridden) and records
each accepted opcode charge. The doc-hidden `with_cost_check_interval` runs a
closure under a given interval and returns those charge sites, so
`cost::cost_check_interval::corpus_cost_check_interval_differential` runs the
example corpus, the boundary cases and the natural failures at interval 1 and
at the production interval over a sweep of budgets in one build and compares
prints, cost, errors and charge sites. The curated
`cost-check-interval-differential` shape is the only one that enables it and
runs only that test.

The budget and natural-failure sweeps also run in ordinary check shapes.
`brokkr test error_boundary --release` exercises them without debug checks;
`brokkr test native_bytes_per_level` (also with `--release`) prints the
on-demand recurring-stack probe in default and snapshot configurations.

The error-boundary transcript is compared with
`tests/fixtures/error_boundary_transcript.txt` in the ordinary sweep. After
reviewing an intentional change, regenerate it with
`brokkr run --debug regenerate_error_boundary -- tests/fixtures/error_boundary_transcript.txt`.
The tool requires an output path, so separate paths can also retain A/B evidence.
No test writes this fixture.

Math fixture regeneration is also an explicit tool:
`brokkr run --release regenerate_crmath_golden -- tests/fixtures/crmath_golden.txt`.
It requires the pinned `research/core-math` checkout and an output path. The
non-default `crmath-sweeps` feature opts into the long oracle sweeps and the
checkout-dependent corpus and table checks; it gates only the library's own
test code, so it changes nothing for a consumer. The default and snapshot
check shapes omit it, and the curated `instrumentation-faults` shape enables
it only to compile and lint that code, filtering the tests out. Run those
checks deliberately with
`brokkr test crmath::tests::golden --release --features crmath-sweeps`.

Every `regenerate_*` example (these two and `regenerate_save_golden`) writes
through the shared `examples/tools/output.rs`, which leaves an unchanged file
untouched and reports `wrote` or `unchanged`. The Python generators
(`examples/parse/generate.py`, `scripts/crmath_tables.py` and the notes
helpers) share the same policy through `scripts/write_if_changed.py`.

## Hotpath benchmarks

`examples/hotpath.rs` is a single Rust harness, and the binary brokkr invokes for `--bench` / `--hotpath` / `--alloc` runs. It takes one required positional arg, a script path resolved from the caller's working directory (`bench/arithmetic.lua`, `examples/fields/same_obj_read.lua`), and exits nonzero if the script does not define a global `_bench()` function. `--iterations N` overrides the warm-call count (default from `DEFAULT_ITERATIONS` in the harness source). When `BROKKR_MARKER_FIFO` names a writable path, the harness also emits brokkr sidecar markers (`PARSE`/`SETUP`/`COLD`/`WARM` START/END pairs, plus batched `WARM_BLOCK` sub-spans bounded by `MAX_WARM_BLOCKS` in the harness source) and `@name=value` counters; without the env var those emits are no-ops. A configured FIFO that cannot be opened or written fails the run rather than recording a partial marker stream. Probe and correctness bench scripts live in subdirectories of `examples/` alongside correctness tests:

```
examples/hotpath.rs        # harness (parse / cold call / warm calls)
examples/numerics/         # arithmetic, constants, math
examples/calls/            # global, local, method, vararg, fixedarg, many_literals, ...
examples/fields/           # same_obj_read, same_obj_write, polymorphic, ...
examples/globals/          # write
examples/iter/             # pairs, ipairs
examples/tables/           # table operation examples
examples/alloc/            # allocation and GC examples
examples/strings/          # mixed, patterns, literal_find
examples/parse/            # large_source (+ generate.py, see below)
examples/composite/        # benchmark (the seven-section mixed suite, whose
                           # kernel bench/benchmark.lua repeats K times)
```

Benches come in two flavours, and the distinction matters when reading the
README table. *Workload* benches are meant to look like plausible script
code; diagnostic probes are written to isolate one path. Every
registered `hotpath_file` opens with `-- Probe:`, `-- Historical probe:` or
`-- Workload:` (`bench_scripts.rs` checks this). Each probe's header describes
the measured path and comparison or phase. The registered
workload set lives in `brokkr.toml`; `ls examples/<category>/` lists the files
in a category. Probe ratios against reference Lua are worst cases by
construction, not representative numbers.
When a probed candidate ships, the script can stay as a regression tracker;
its header marks the historical status, and its `bench/` port's `K` is
recalibrated back to the seconds scale so the verdict wall can still resolve a
regression.

`examples/parse/large_source.lua` is generated - regenerate with `python3
examples/parse/generate.py`, do not edit it by hand. `brokkr check` verifies the
checked-in output with `python3 examples/parse/generate.py --check`. It exists because
parse and codegen costs that scale with source size cannot surface in the other
benches, which are 15-80 lines. The parser's `MAX_NESTED` limit in `src/limits.rs`
bounds how many top-level definitions the generator can emit; size comes from
making each function longer instead.

Each bench script is also a standalone-runnable test: top-level setup + a `_bench()` function + an outer loop that calls `_bench()` enough times for hyperfine resolution and prints `<name>: <computed result>: true` (or a computed success condition). This lets one file serve three masters: the hotpath harness (calls `_bench` directly for parse/cold/warm phasing), `tests/integration/corpus/run_examples.rs` (executes the standalone runner, asserts no `: false`), and `scripts/bench.sh` (hyperfine timings vs reference Lua 5.2/5.4/5.5 + LuaJIT). The default `bench.sh` script list comes from the `hotpath_file` entries in `brokkr.toml`; per-script hyperfine JSON and the matching script list land under `.scratch/bench-results/`, which the next `brokkr check` wipes, so copy out anything worth keeping.

`bench/` holds the brokkr *verdict* workloads: seconds-scale ports of the curated examples/ benches - same kernels, wrapped in a repeat loop calibrated so one `_bench()` call is ~100ms, footer fixed at 30 calls, which lands the standalone run at ~3s. The harness warm phase scales with `DEFAULT_ITERATIONS` in `examples/hotpath.rs`. A short launch cannot resolve the deltas the optimization backlog targets; a seconds-scale one can. Ordinary `[dellingr.workloads.*]` registrations pair two files: `file`/`xxh128` (the bench/ port, for wall-clock verdict runs) and `hotpath_file`/`hotpath_xxh128` (the examples/ counterpart, for instrumented profiling, which only works at the ms scale). `large_source` is a parse/codegen probe and an exception: both modes use the generated examples/ source, and its timing is a parse/codegen diagnostic rather than a calibrated runtime verdict. `queue` cannot be baselined with `--bench --commit` before 2bcc71e (the commit that bounded a table's array storage): on older VMs every `pairs` start walks the queue's whole push history, so the seconds-scale `bench/queue.lua` runs quadratically for hours. Compare across that commit with `--hotpath`/`--alloc` on the ms-scale `examples/tables/queue.lua`, or with a smaller one-off variant. Editing a pinned file requires re-hashing its pin, which is the deliberate signal that older stored results no longer compare. For execution workloads, the paired files must keep the same kernel in the same proportions, or per-function distributions measured on the ms file stop describing the seconds-scale walls. `bench/` is intentionally outside every examples/ glob - `run_examples.rs`, `diff_test.sh` and `bench.sh` do not see it - so the fast test/diff/ratio surfaces and the slow verdict surface stay independent.

The harness measures four phases on one State and emits KV pairs to stderr: `parse_us`, `cold_call_us`, `warm_avg_us` (averaged over the configured warm iteration count; fractional, three decimals), plus the dellingr-specific `cold_cost`, `warm_avg_cost` (integer, truncated), `warm_total_cost` (exact, over all warm iterations) and the derived `cost_per_us` (from the exact total). The script is compiled with its path, as given, as the chunk name, so error locations read `bench/arithmetic.lua:12:` with no `@` prefix; `brokkr dellingr` passes the registered root-relative path from the project root, so chunk names and the stored `target=` carry no checkout path. Wall time is the benchmarking metric; cost is a deterministic property of the workload and rides along as a fingerprint (a cost change means the script or compiler changed, not the VM's speed). `setup_*` and `final_*` heap/object counts bracket GC pressure. Note the harness runs the script's top level during setup, so the standalone footer's calls execute there - the "cold" call is only cold with respect to what the footer has not already warmed.

Internal functions carry `#[cfg_attr(feature = "hotpath", hotpath::measure)]` measurement sites across `vm/table.rs`, `vm/table_ops.rs`, `vm/eval*.rs`, `vm/object.rs` and the compiler front end. The `hotpath` profiler is an optional dependency enabled only by this crate's `hotpath` feature (which `hotpath-alloc` implies), and brokkr.toml's `hotpath-must-be-optional` dependency rule keeps it optional, so default builds neither expand the attribute nor link the profiler. Always write a new site in that `cfg_attr` form; it is cheap to add one when a new candidate needs a measurement point, and worth doing at the same time as the bench that will exercise it. Recursive parser helpers (function, statement, expression and constructor parsing) stay unmeasured: each measured call holds an allocation guard for its whole duration, so under `hotpath-alloc` the guards accumulate with syntax depth and can exhaust the profiler's nested guard storage. The whole-parse measurements (`compiler::compile`, `parser::parse`, `Parser::parse_all`) and the lexer's remain. **Don't add a measurement to a function that stays live across a native re-entry** (`call_unguarded`, `begin_call`, `begin_callable_call`, `run_invocation`, `Frame::run`, and the metamethod/iterator/library helpers between the dispatch loop and their `call_unguarded`): every native level carries those frames, and the limit tests drive `limits::MAX_NATIVE_CALL_DEPTH` levels on the envelope's thread (4 MiB in dev, 2 MiB in release; docs/limits.md "Envelope"). Instrumented builds already get a larger test stack (`tests/common/stack.rs`), but the margin is not unlimited. `scripts/native_calls.py` (the `brokkr check` native-call inventory check) rejects the attribute on every function in that set. A function *called from* the dispatch loop whose frame is popped before any re-entry is fine. The release frames of the live-across-re-entry functions are also sensitive to live-range shape, not only to what is inlined: a value returned for the caller to consume after a drop, or a `match` scrutinee `Result` whose arm makes the re-entering call, costs a spill slot or an unshared result slot there, and deleting a redundant line in something inlined into `Frame::run` can re-pack its spill slots one alignment step larger. Measure with `brokkr test native_bytes_per_level` (dev and `--release`, default and `snapshot`) and attribute with `scripts/frame_sizes.py` before and after.

The `instrumentation-faults` sweep in `brokkr check` compiles and lints the `hotpath` and `hotpath-alloc` configurations, so a bad annotation fails the gate.

To add a new target: write `examples/{category}/{name}.lua` defining `_bench()` and a standalone runner footer whose success line starts `print("{category}/{name}: " .. tostring(_bench()) .. ` (`bench_scripts.rs` checks that prefix), then run `python3 scripts/example_list.py --write` to add it to `tests/common/examples.rs` (the `example-list` script check fails until it is listed). That list entry is the only Rust-side change: it gives the example its corpus gate test (`corpus::run_examples::standalone::{category}_{name}`), its snapshot round trip and its place in every corpus-wide test. The default `scripts/bench.sh` list and brokkr's `--hotpath`/`--alloc` reach it only once it is the `hotpath_file` of a `[dellingr.workloads.*]` registration in `brokkr.toml`, and a registered `hotpath_file` must open with one of the classification headers above.

Function-level profiles and allocation counts come from `brokkr dellingr --lua
<workload> --hotpath` and `--alloc` (see "'brokkr' benchmarking" in CLAUDE.md);
don't invoke the `cargo run --example hotpath` command manually.

`python3 scripts/ab_pairs.py <commit-a> <commit-b> <rounds> <workload>...`
runs `brokkr dellingr --lua <workload> --bench --commit <commit>` serially,
alternating A/B and B/A order by round for each workload. It stores the matched
cells in `.brokkr/results.db`, which `brokkr results -n <cells>` can read.

`research/bench_luars.sh` is a one-off comparison harness: same bench
scripts and same hyperfine pass, but against [lua-rs](https://github.com/ianm199/lua-rs)
(the only comparable pure-Rust Lua VM) anchored to C Lua 5.4 and the
LuaJIT floor. It expects a `lua-rs` release binary built from a clone in
`research/lua-rs/` (the script header names the binary path it reads); `research/` is
an area for local upstream checkouts, which remain ignored. The comparison
script lives at `research/bench_luars.sh`. The README's `lua-rs` column comes
from this script - its footnote pins the captured date and upstream commit, so
refresh both together.

## Project conventions worth knowing

- Examples in `examples/` are part of the test surface - don't add throwaway scripts there. brokkr verdict workloads live in `bench/` (see above).
- The CLI prints `Cost used: N` to stderr after each run (and `--analyze` prints its caveat to stderr); `--quiet` suppresses the cost line. `scripts/diff_test.sh` and `tests/integration/corpus/run_examples.rs` pass `--quiet`, and `run_examples.rs` fails on any stderr from a passing example.
- The crate was lifted out of a game project (originally extracted from `fcomm2`); some doc comments still mention `FleetCallbacks` etc. as illustrative examples.
- `target/` is a symlink to a shared cargo cache.
- `.scratch/` is gitignored throwaway space, and every `brokkr check` deletes it wholesale before anything else runs (the `wipe-scratch` script check). Use it for intermediate files, logs, patches, perf data and agent reports, but anything there is gone after the next check: `scripts/bench.sh` results included. Nothing may rely on it surviving a check, and nothing that must be kept goes there.
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
