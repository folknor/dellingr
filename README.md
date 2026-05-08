# dellingr

<a href="https://crates.io/crates/dellingr"><img src="https://img.shields.io/crates/v/dellingr" alt="crates.io"></a>
<a href="https://docs.rs/dellingr"><img src="https://img.shields.io/docsrs/dellingr" alt="docs.rs"></a>
<img src="https://img.shields.io/badge/rust-1.92+-orange?logo=rust" alt="MSRV 1.92">
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>

An embeddable, deterministic, pure-Rust Lua VM with precise per-opcode instruction-cost accounting. No FFI, no system Lua dependency.

Built with LLMs. See [LLM.md](LLM.md).

## Performance

It's slower than reference Lua, but not dramatically so: roughly 2-4x behind lua5.4 / 5.5 on most workloads and ~2x on `pairs` iteration / pattern matching. LuaJIT is its own world - 18-30x faster than us on tight loops where its tracing JIT shines, much less on alloc/string/pattern work where we get within 3-4x. dellingr is fast enough for continuous bounded execution of a few kilobytes of Lua code to let a game run at several thousand FPS.

Run `./bench.sh` to reproduce on your own host. Sample run on AMD Ryzen 9 5900X / Linux 7.0:

| bench                    | dellingr | vs lua5.5 | vs lua5.4 | vs lua5.2 | vs luajit |
|--------------------------|---------:|----------:|----------:|----------:|----------:|
| `numerics/arithmetic`    |    101ms |     3.93x |     3.99x |     2.80x |    23.17x |
| `iter/pairs`             |     88ms |     2.14x |     2.02x |     1.97x |    16.32x |
| `strings/patterns`       |     30ms |     1.70x |     1.67x |     1.62x |     2.55x |
| `tables/fill`            |     99ms |     3.90x |     3.28x |     2.43x |     6.72x |
| `strings/mixed`          |     38ms |     2.93x |     3.19x |     2.09x |     3.12x |
| `fields/same_obj_read`   |    115ms |     3.96x |     4.33x |     2.51x |    30.52x |
| `alloc/closure`          |     77ms |     2.05x |     1.99x |     1.67x |     2.55x |
| `benchmark` (multi)      |    168ms |     4.00x |     3.97x |     2.74x |    23.35x |

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

## Budget

There's a few gotchas with the current instruction-cost accounting. For example, `while true do end` is free, which means that a users Lua script could run forever. This is a known trade-off made in the full light of day - the main consumer of dellingr does not want to penalise the user (that is, subtract from their per-gametick budget) for structural semantics. Users should be encouraged to write more code, not less.

## Status

The public API is **pre-1.0 and not yet stable** -
breaking changes may land in any 0.x bump until 1.0.

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
- `analyze_cost` (or `engine.analyze_cost(&program)`) - static worst-case
  instruction count for a script.
- Per-state user-data (`Send + 'static`) for hanging embedder context off
  the VM.

## Standalone CLI

The crate also ships a `dellingr` binary for running `.lua` files:

```sh
cargo run --release -- path/to/script.lua
cargo run --release -- --analyze path/to/script.lua
cargo run --release -- --limit 100000 path/to/script.lua
```

## Acknowledgements

Initially based on [cjneidhart/lua-in-rust](https://github.com/cjneidhart/lua-in-rust).

`src/patterns` initially ripped from [mwerezak/lua-patterns](https://github.com/mwerezak/lua-patterns), which itself is a fork of [stevedonovan/lua-patterns](https://github.com/stevedonovan/lua-patterns).

## License

MIT - see [LICENSE](./LICENSE).
