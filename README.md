# dellingr

<a href="https://crates.io/crates/dellingr"><img src="https://img.shields.io/crates/v/dellingr" alt="crates.io"></a>
<a href="https://docs.rs/dellingr"><img src="https://img.shields.io/docsrs/dellingr" alt="docs.rs"></a>
<img src="https://img.shields.io/badge/rust-1.99+-orange?logo=rust" alt="MSRV 1.99">
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>

An embeddable, deterministic, pure-Rust Lua VM with precise per-opcode instruction-cost accounting. No FFI, no system Lua dependency.

Built with LLMs. See [LLM.md](LLM.md).

## Performance

It's slower than reference Lua, but not dramatically so: roughly 1.4-3.3x behind lua5.2 on representative workloads. LuaJIT is obviously in its own league. dellingr is fast enough for continuous bounded execution of a few kilobytes of Lua code to let a game run at several thousand FPS.

Run `./scripts/bench.sh` to reproduce on your own host. Sample run on AMD Ryzen 9 5900X / Linux 7.0:

| bench                    | dellingr |  lua-rs | vs lua5.5 | vs lua5.4 | vs lua5.2 | vs luajit |
|--------------------------|---------:|--------:|----------:|----------:|----------:|----------:|
| `benchmark`              |    139ms |   104ms |     3.42x |     3.35x |     2.29x |    19.65x |
| `numerics/arithmetic`    |     92ms |    60ms |     3.63x |     3.53x |     2.59x |    21.30x |
| `numerics/constants` *   |     64ms |       - |     6.50x |     6.48x |     5.64x |    23.15x |
| `iter/pairs`             |     61ms |   169ms |     1.49x |     1.40x |     1.38x |    11.41x |
| `tables/fill`            |    102ms |    64ms |     4.10x |     3.46x |     2.62x |     7.07x |
| `strings/mixed`          |     34ms |    65ms |     2.77x |     3.02x |     1.97x |     2.95x |
| `strings/patterns`       |     60ms |    22ms |     3.48x |     3.45x |     3.34x |     5.27x |
| `strings/literal_find`   |     21ms |       - |     3.31x |     3.41x |     2.42x |     6.23x |
| `fields/same_obj_read` * |    169ms |    61ms |     6.01x |     7.16x |     3.81x |    46.27x |
| `globals/write` *        |     32ms |       - |     1.62x |     2.04x |     1.10x |    27.12x |
| `calls/many_literals` *  |     57ms |       - |     4.40x |     4.39x |     2.70x |    61.03x |
| `alloc/closure`          |     81ms |   126ms |     2.20x |     2.21x |     1.83x |     2.80x |
| `alloc/record_tables`    |    266ms |       - |     3.47x |     3.23x |     2.11x |    91.94x |
| `alloc/gc_churn` *       |     20ms |       - |     1.31x |     1.26x |     1.10x |     1.77x |
| `parse/large_source` *   |      8ms |       - |     2.28x |     2.30x |     2.04x |     3.19x |

<sub>Rows marked `*` are diagnostic probes, not representative workloads. Each one deliberately isolates a single path that was unoptimized when the probe was written, so its ratio tracks one specific optimization target rather than what typical scripts see. Several of those paths have since been optimized, which is why some probes now sit among the cheaper rows. `calls/many_literals` calls a function carrying 32 string constants that returns from its first branch, concentrating its runtime in per-call literal handling. `parse/large_source` is a parse and codegen probe on generated source; its timing is a parse/codegen diagnostic rather than an execution benchmark.</sub>

<sub>The `lua-rs` column was captured 2026-06-02 against [ianm199/lua-rs](https://github.com/ianm199/lua-rs) at commit [`98bd6bd`](https://github.com/ianm199/lua-rs/commit/98bd6bd); rows added since carry no `lua-rs` figure. The `benchmark` row in every column timed `examples/benchmark.lua`, since retired; its successor `examples/composite/benchmark.lua` is a different file and has not been captured here. Every column other than `lua-rs` was captured 2026-07-27 at commit `abf7c7b` on host `plantasjen`.</sub>

Note that recorded results always track the latest git head and may not match the released version.

## Won't implement

Designed for sandboxed embedding, not as a general-purpose Lua replacement. These Lua features are intentionally excluded:

- Integer division (`//`), bitwise operators
- Coroutines - no yield/resume; each callback runs to completion or budget
- IO/OS libraries - sandboxed environment, no filesystem or system access
- Debug library - no introspection of VM internals
- `pcall`/`xpcall`, `assert` - no error recovery; errors kill the callback
- `goto`/labels - simplifies VM, prevents obfuscated control flow
- `string.rep`, `string.byte`, `string.char`
- Arithmetic/comparison/concat metamethods
- The `__pairs`, `__mode`, `__gc`, `__close` and `__name` fields are ignored:
  no pairs hook, weak tables, finalizers, close handlers or custom type names
- Dynamic code and module loading (`load`, `loadstring`, `dofile`, `require`,
  and the `package` library); script-controlled collection (`collectgarbage`)
- `math.randomseed` (the host seeds the stream with `State::set_rng_seed`)
- `_ENV`: every variable use of the name is a syntax error - reading it,
  assigning it, and declaring it as a local, parameter, loop variable or
  function name. Field names such as `t._ENV` are unaffected.
- Lua 5.4 `\u{XXXX}` string escapes and `<const>` / `<close>` local attributes
- Integer-specific `math` members (`math.type`, `math.tointeger`, `math.ult`,
  `math.maxinteger`, `math.mininteger`), and the Lua 5.2-only
  `math.pow`, `math.ldexp`, `math.frexp`, `math.cosh`, `math.sinh`,
  `math.tanh`, `math.log10`
- `table.maxn`
- Implicit string-to-number coercion in arithmetic and numeric control expressions
- Long strings (`[[...]]`, `[=[...]=]`)

Keeping these features out makes the VM more suitable for embedded use, especially in games that expose scripting to users. Their absence helps hosts enforce the constraints they choose for those scripts.

## Source limits

The parser bounds syntax nesting (`limits::MAX_SYNTAX_DEPTH`) and raises `chunk has too many syntax levels` past that, the way reference Lua does with `LUAI_MAXCCALLS`. Without the bound, deeply nested source exhausts the native stack, and a Rust stack overflow aborts the host process rather than returning a catchable error.

One divergence worth knowing: a nested parenthesis costs two levels because each level descends through both the expression and unary parsers. Table constructors in a field and `if` bodies also cost two; call and index nesting costs four, while unary operators and `do`, loop or function bodies cost one. Flat `elseif` chains and expression suffix chains are parsed iteratively and do not consume the budget at all; only what they contain does. Real code does not come close to the limit.

Per-function ceilings (`limits::MAX_LOCALS`, `MAX_UPVALUES` and `MAX_LITERALS` per string or number pool) are reported as syntax errors rather than silently truncated. Table constructors and `t.f = v` assignment sites have no source-count limit: array entries are written in batches, constructors beyond the inline-template limit use ordinary table construction, and field assignments beyond their cache-slot capacity use the uncached path.

## Runtime limits

Call depth is capped at `limits::MAX_CALL_DEPTH` frames, held on the heap: a Lua function calling a Lua function costs no native stack. Calls made from Rust - host calls, metamethod handlers, iterator functions, `table.sort` comparators, `string.gsub` replacement functions - do run on the native stack, and `limits::MAX_NATIVE_CALL_DEPTH` caps how many are active at once, as reference Lua's `LUAI_MAXCCALLS` does; see `docs/limits.md`.

**Thread stack size.** Every recursion limit at once fits a 2 MiB thread (the Rust default for spawned threads) in a release build, and a 4 MiB thread in a debug build. If a debug build of your host runs scripts on threads it spawns itself, give them 4 MiB (`std::thread::Builder::new().stack_size(4 << 20)`); otherwise a deeply recursive script can overflow the native stack and abort the process instead of failing with a limit error. Release builds and the main thread (usually 8 MiB) need nothing. The shared Lua/Rust value stack is capped at `limits::MAX_STACK_SIZE` values, and that cap is enforced on every operation that can grow the stack - bytecode operand pushes, frame setup, metamethod dispatch, and the host-facing `push_*` / `new_table` / `get_global` methods alike. Exceeding it produces a catchable `StackOverflow` error rather than an abort, and a rejected operation leaves the stack exactly as it was.

This is a bound on the value stack, **not** a total host-memory quota. It stops a runaway script or a looping host callback from growing the stack without limit; it does not stop a host from allocating memory by other means, any more than the `limits::MAX_STRING_BYTES` string cap does. Enforcing it charges nothing against the instruction-cost budget.

## Budget

There's a few gotchas with the current instruction-cost accounting. For example, `while true do end` is free, which means that a users Lua script could run forever. This is a known trade-off made in the full light of day - the main consumer of dellingr does not want to penalise the user (that is, subtract from their per-gametick budget) for structural semantics. Users should be encouraged to write more code, not less.

Data-dependent native work in the string, pattern, and table libraries is generally charged for bytes examined or emitted, table elements visited, and matcher primitives. `table.insert`, `table.remove`, and `table.pack` instead cost a flat 1 regardless of elements shifted or stored; global `unpack` and `table.unpack` are free regardless of elements visited. Structural semantics remain free by design. A helper extraction, an intermediate name, or another legible refactor should not make a script more expensive, because the budget is a game-design instrument rather than a CPU meter. Per-function charges are listed in [docs/budget.md](docs/budget.md).

This cost model changed in 0.4.0. Embedders must re-measure their tick budgets: the increase depends on string lengths, output expansion, pattern matches and backtracking, and table contents.

## Compatibility divergences

Numeric `for` loops with a zero step skip their body in both directions. Lua 5.2 skips an ascending range but loops forever for a descending one; Lua 5.4 raises an error. dellingr deliberately skips the loop to avoid the unbounded execution case.

`math.log(x, 2)` follows Lua 5.2's general-base calculation. This differs in the last bit from Lua 5.4 for some inputs (for example, `math.log(3, 2)`).

The full list is in [docs/compatibility.md](docs/compatibility.md).

## Determinism

Execution is deterministic: the same script and inputs produce the same result
and the same cost on a given build. `math.random` draws from an in-crate seeded
RNG (`set_rng_seed`), so it is bit-stable across platforms. `math`'s
transcendental functions (`sin`, `cos`, `exp`, `log`, ...) and the `^` operator
never call the platform libm: they are in-crate ports of the correctly rounded
CORE-MATH implementations and give the same bits on every supported host, as do
basic arithmetic, comparisons and `sqrt`. See
[docs/determinism.md](docs/determinism.md) for the exact guarantee and
[reference/deterministic-math.md](reference/deterministic-math.md) for how it
is built.

## Save state

The optional `snapshot` feature adds `State::save_state()` and
`State::load_state(...)` for game saves, serialized through a small in-crate
deterministic binary codec (no `serde`/`bincode`). This is a data snapshot of a
quiescent VM: globals, reachable tables/closures/upvalues/strings, the
deterministic RNG stream, cost counters, and the state-local identities behind
`tostring`/`%p` are persisted. Changes you make to the standard library tables
themselves - `math.myconst = 42`, `string.trim = f`, or deleting a stock entry -
are persisted as a delta against the pristine environment and replayed on load,
so extending a library table does not silently lose data. It is not a
continuation capture; no paused call stack, program counter, coroutine, anchor,
callback, or host user-data handle survives a save/load. Anchors created inside
the load setup closure do survive it. Hosts recreate
callbacks and register the same named Rust functions during load setup - a
reachable `RustFunc` must be registered with a stable id (e.g.
`set_global_named_rust_fn`) or the save fails fast. The module doc on
`src/vm/save_state.rs` covers the format and design; details are in
[docs/save-state.md](docs/save-state.md).

Save files are user-editable input. Loading restores the saved cost counters,
so hosts that load user-editable saves must call `set_cost_budget` after a
successful load. Saves containing a Lua string larger than `limits::MAX_STRING_BYTES`
are rejected on load, including ones written by an earlier build using the same snapshot
format version. A format version other than `FORMAT_VERSION` is rejected while
reading the header, before payload decoding. Save authentication is required
if tampering with future RNG outcomes matters.

## Status

The public API is pre-1.0 and not yet stable. Breaking changes may land at any point.
The declared MSRV is Rust 1.99; release validation should include a real 1.99
toolchain check.

```toml
[dependencies]
dellingr = "0.5"
```

## Library usage/VM

- `Program` (`Send + Sync + Clone`) - compiled bytecode handle.
  `Program::compile(source, name)` is the one way to compile Lua: source
  and chunk name are bytes, kept exactly. Compile once, load into many
  states with `state.load(&program)`.
- `State` (`Send`) - VM instance with cost-bounded execution. `State::new`
  opens the standard library, `State::raw` starts with empty globals; both
  have `_with_callbacks` variants. Movable across threads; pair with
  `Mutex<State>` if you need to share one.
- `Anchor` (`Send + Sync + 'static`) - owned handle that retains a Lua value
  (function, table, etc.) across host calls without using globals. Dropping
  it releases the value. State-scoped; cross-state misuse surfaces as an
  error, not silent corruption.
- `HostCallbacks` trait (`: Send`) - embedders redirect `print` (the
  script's exact bytes), hook errors, etc.
- `RustFunc` - expose Rust functions to Lua scripts.
- Typed errors - `ErrorKind::ScriptError` carries the value a script passed
  to `error()` (`ErrorValue`: nil, boolean, number, exact string bytes, or a
  table/function's type and rendering); library, syntax, argument and limit
  failures have their own kinds, and `ErrorKind::HostError` carries a
  `RustFunc`'s own boxed error back to the host.
- `program.analyze_cost()` - static cost
  estimate for a script: sums each costed opcode once across the main
  chunk and every nested function body (counted once each, whether or not
  invoked; loops/branches counted once). `CostAnalysis::caveat()` returns the
  standard explanation of the estimate's limits for display in a host.
- Per-state user-data (`Send + 'static`) for hanging embedder context off
  the VM.

## Standalone CLI

The crate also ships a `dellingr` binary for running `.lua` files:

```sh
cargo run --release -- path/to/script.lua
cargo run --release -- --analyze path/to/script.lua
cargo run --release -- --limit 100000 path/to/script.lua
cargo run --release -- --quiet path/to/script.lua
```

## Acknowledgements

Initially based on [cjneidhart/lua-in-rust](https://github.com/cjneidhart/lua-in-rust).

`src/patterns` initially ripped from [mwerezak/lua-patterns](https://github.com/mwerezak/lua-patterns), which itself is a fork of [stevedonovan/lua-patterns](https://github.com/stevedonovan/lua-patterns).

## License

MIT - see [LICENSE](./LICENSE).
