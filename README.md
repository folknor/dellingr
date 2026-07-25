# dellingr

<a href="https://crates.io/crates/dellingr"><img src="https://img.shields.io/crates/v/dellingr" alt="crates.io"></a>
<a href="https://docs.rs/dellingr"><img src="https://img.shields.io/docsrs/dellingr" alt="docs.rs"></a>
<img src="https://img.shields.io/badge/rust-1.92+-orange?logo=rust" alt="MSRV 1.92">
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>

An embeddable, deterministic, pure-Rust Lua VM with precise per-opcode instruction-cost accounting. No FFI, no system Lua dependency.

Built with LLMs. See [LLM.md](LLM.md).

## Performance

It's slower than reference Lua, but not dramatically so: roughly 1.6-3x behind lua5.2 on most workloads. LuaJIT is obviously in its own league. dellingr is fast enough for continuous bounded execution of a few kilobytes of Lua code to let a game run at several thousand FPS.

Run `./bench.sh` to reproduce on your own host. Sample run on AMD Ryzen 9 5900X / Linux 7.0:

| bench                    | dellingr |  lua-rs | vs lua5.5 | vs lua5.4 | vs lua5.2 | vs luajit |
|--------------------------|---------:|--------:|----------:|----------:|----------:|----------:|
| `benchmark`              |    168ms |   104ms |     4.00x |     3.97x |     2.74x |    23.35x |
| `numerics/arithmetic`    |    101ms |    60ms |     3.93x |     3.99x |     2.80x |    23.17x |
| `iter/pairs`             |     88ms |   169ms |     2.14x |     2.02x |     1.97x |    16.32x |
| `tables/fill`            |     99ms |    64ms |     3.90x |     3.28x |     2.43x |     6.72x |
| `strings/mixed`          |     38ms |    65ms |     2.93x |     3.19x |     2.09x |     3.12x |
| `strings/patterns`       |     30ms |    22ms |     1.70x |     1.67x |     1.62x |     2.55x |
| `fields/same_obj_read`   |    115ms |    61ms |     3.96x |     4.33x |     2.51x |    30.52x |
| `alloc/closure`          |     77ms |   126ms |     2.05x |     1.99x |     1.67x |     2.55x |

<sub>The `lua-rs` column was captured 2026-06-02 against [ianm199/lua-rs](https://github.com/ianm199/lua-rs) at commit [`98bd6bd`](https://github.com/ianm199/lua-rs/commit/98bd6bd).</sub>

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
- Long strings (`[[...]]`, `[=[...]=]`)

Some of them might be added later (probably most of them behind a feature gate, if so). The lack of the features above make the VM much more suitable for embedded use. Especially in games where the Lua scripting might be exposed to users. In those cases, these 3 string methods - for example - could be used to work around restrictions the game wants to put on the user.

## Source limits

The parser bounds syntax nesting at 200 levels and raises `chunk has too many syntax levels` past that, the way reference Lua does with `LUAI_MAXCCALLS`. Without the bound, deeply nested source exhausts the native stack, and a Rust stack overflow aborts the host process rather than returning a catchable error.

One divergence worth knowing: a nested parenthesis costs two levels rather than one, because each level descends through both the expression and unary parsers. Parentheses therefore nest about 99 deep here against reference Lua's ~200. Every other construct - statement blocks, `elseif` chains, table constructors, unary operator runs, field-access chains - gets the full 200. Real code does not come close to either figure.

Per-function ceilings, all reported as syntax errors rather than silently truncated: 255 locals and 255 upvalues, and 65536 distinct string literals and number literals. Table constructors and `t.f = v` assignment sites are effectively unbounded - array entries are written in batches, and assignment sites past the 255th simply stop being inline-cached rather than being rejected.

## Budget

There's a few gotchas with the current instruction-cost accounting. For example, `while true do end` is free, which means that a users Lua script could run forever. This is a known trade-off made in the full light of day - the main consumer of dellingr does not want to penalise the user (that is, subtract from their per-gametick budget) for structural semantics. Users should be encouraged to write more code, not less.

## Determinism

Execution is deterministic: the same script and inputs produce the same result
and the same cost on a given build. `math.random` draws from an in-crate seeded
RNG (`set_rng_seed`), so it is bit-stable across platforms. The one caveat is
`math`'s transcendental functions (`sin`, `cos`, `exp`, `log`, `pow`, `sqrt`,
...), which delegate to the platform `f64`/libm implementation: results are
deterministic per build+platform but the last ULP may differ across
architectures or libm versions. Replays that must be bit-identical across
heterogeneous hosts should avoid depending on exact transcendental results;
arithmetic, comparisons, and `math.random` are bit-stable everywhere.

## Save state

The optional `snapshot` feature adds `State::save_state()` and
`State::load_state(...)` for game saves, serialized through a small in-crate
deterministic binary codec (no `serde`/`bincode`). This is a data snapshot of a
quiescent VM: globals, reachable tables/closures/upvalues/strings, the
deterministic RNG stream, cost counters, and the object identities behind
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
`src/vm/save_state.rs` covers the format and design.

Save files are user-editable input. Loading restores the saved cost counters,
so hosts that load user-editable saves must call `set_cost_budget` after a
successful load. Saves containing a Lua string larger than 16 MiB are rejected
on load, including ones written by an earlier build using the same snapshot
format version - an older format version is rejected before that, as an
unsupported version. Save authentication is required if tampering with future
RNG outcomes matters.

## Status

The public API is pre-1.0 and not yet stable. Breaking changes may land at any point.

```toml
[dependencies]
dellingr = "0.2"
```

## Library usage/VM

- `Engine` (`Send + Sync`) - factory for compiling source into `Program`s
  and creating `State`s. One `Engine` per app, shared via `Arc`.
- `Program` (`Send + Sync + Clone`) - compiled bytecode handle. Compile
  once on the engine, load into many states with `state.load(&program)`.
- `State` (`Send`) - VM instance with cost-bounded execution. Movable
  across threads; pair with `Mutex<State>` if you need to share one.
- `Anchor` (`Copy + Send`) - retain a Lua value (function, table, etc.)
  across host calls without using globals. State-scoped; cross-state
  misuse and use-after-release surface as errors, not silent corruption.
- `HostCallbacks` trait (`: Send`) - embedders redirect `print`, hook
  errors, etc.
- `RustFunc` - expose Rust functions to Lua scripts.
- `analyze_cost` (or `engine.analyze_cost(&program)`) - static cost
  estimate for a script: sums each costed opcode once across the main
  chunk and every nested function body (counted once each, whether or not
  invoked; loops/branches counted once), so it is neither a runtime lower
  nor upper bound.
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
