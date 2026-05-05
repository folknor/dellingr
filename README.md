# dellingr

An embeddable, deterministic, pure-Rust Lua VM with precise per-opcode instruction-cost accounting. No FFI, no system Lua dependency.

It's slower than reference Lua, but not dramatically so: roughly 2-4x behind lua5.4 / 5.5 on most workloads, ~2x on `pairs` iteration, and ~8x on string-heavy code. LuaJIT is its own world - 18-30x faster than us on tight loops, much less on alloc/string-heavy code. dellingr is fast enough for continuous bounded execution of a few kilobytes of Lua code to let a game run at several thousand FPS.

Run `./bench.sh` to reproduce on your own host. Sample run on AMD Ryzen 9 5900X / Linux 7.0 / commit `bd24832`:

| bench                    | dellingr | vs lua5.5 | vs lua5.4 | vs lua5.2 | vs luajit |
|--------------------------|---------:|----------:|----------:|----------:|----------:|
| `numerics/arithmetic`    |    104ms |     3.96x |     3.87x |     2.85x |    23.33x |
| `iter/pairs`             |     97ms |     2.30x |     2.18x |     2.11x |    18.18x |
| `tables/fill`            |    107ms |     4.23x |     3.51x |     2.63x |     7.23x |
| `strings/mixed`          |     99ms |     7.79x |     8.56x |     5.65x |     8.45x |
| `fields/same_obj_read`   |    113ms |     3.99x |     4.35x |     2.50x |    30.34x |
| `benchmark` (multi)      |    173ms |     4.18x |     4.05x |     2.80x |    23.74x |

Built with LLMs. See [LLM.md](LLM.md).

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

Some of them might be added later, but behind a feature gate if so. The lack of the features above make the VM much more suitable for embedded use. Especially in games where the Lua scripting might be exposed to users. In those cases, these 3 string methods - for example - could be used to work around restrictions the game wants to put on the user.

## Budget

There's a few gotchas with the current instruction-cost accounting. For example, `while true do end` is free, which means that a users Lua script could run forever. This is a known trade-off made in the full light of day - the main consumer of dellingr does not want to penalise the user (that is, subtract from their per-gametick budget) for structural semantics. Users should be encouraged to write more code, not less.

## Status

The public API (`State`, `HostCallbacks`, `RustFunc`, cost analysis) is
**not yet stable**. Consume via git dependency until v0.1 lands on
crates.io.

```toml
[dependencies]
dellingr = { git = "https://github.com/folknor/dellingr", branch = "main" }
```

## What it provides

- `State` - VM instance with cost-bounded execution.
- `HostCallbacks` trait - embedders redirect `print`, hook errors, etc.
- `RustFunc` - expose Rust functions to Lua scripts.
- `analyze_cost` - static worst-case instruction count for a script.
- Per-state user-data attachment for hanging embedder context off the VM.

## Standalone CLI

The crate also ships a `dellingr` binary for running `.lua` files:

```sh
cargo run --release -- path/to/script.lua
cargo run --release -- --analyze path/to/script.lua
cargo run --release -- --limit 100000 path/to/script.lua
```

## Acknowledgements

Initially based on [cjneidhart/lua-in-rust](https://github.com/cjneidhart/lua-in-rust).

## License

MIT - see [LICENSE](./LICENSE).
