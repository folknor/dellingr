# dellingr

<p align="center">
  <a href="https://crates.io/crates/dellingr"><img src="https://img.shields.io/crates/v/dellingr" alt="crates.io"></a>
  <a href="https://docs.rs/dellingr"><img src="https://img.shields.io/docsrs/dellingr" alt="docs.rs"></a>
  <img src="https://img.shields.io/badge/rust-1.92+-orange?logo=rust" alt="MSRV 1.92">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

An embeddable, deterministic, pure-Rust Lua VM with precise per-opcode instruction-cost accounting. No FFI, no system Lua dependency.

It's slower than reference Lua, but not dramatically so: roughly 2-4x behind lua5.4 / 5.5 on most workloads and ~2x on `pairs` iteration / pattern matching. LuaJIT is its own world - 18-30x faster than us on tight loops where its tracing JIT shines, much less on alloc/string/pattern work where we get within 3-4x. dellingr is fast enough for continuous bounded execution of a few kilobytes of Lua code to let a game run at several thousand FPS.

Run `./bench.sh` to reproduce on your own host. Sample run on AMD Ryzen 9 5900X / Linux 7.0:

| bench                    | dellingr | vs lua5.5 | vs lua5.4 | vs lua5.2 | vs luajit |
|--------------------------|---------:|----------:|----------:|----------:|----------:|
| `numerics/arithmetic`    |    102ms |     3.89x |     3.91x |     2.77x |    22.92x |
| `iter/pairs`             |     91ms |     2.10x |     1.96x |     1.92x |    16.35x |
| `strings/patterns`       |     30ms |     1.69x |     1.65x |     1.61x |     2.53x |
| `tables/fill`            |    108ms |     4.13x |     3.42x |     2.63x |     7.22x |
| `strings/mixed`          |     43ms |     3.32x |     3.65x |     2.40x |     3.57x |
| `fields/same_obj_read`   |    120ms |     4.08x |     4.42x |     2.58x |    31.34x |
| `benchmark` (multi)      |    172ms |     4.07x |     3.97x |     2.75x |    23.44x |

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

v0.1.0 is on crates.io. The public API (`State`, `HostCallbacks`,
`RustFunc`, cost analysis) is **pre-1.0 and not yet stable** -
breaking changes may land in any 0.x bump until 1.0.

```toml
[dependencies]
dellingr = "0.1"
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
