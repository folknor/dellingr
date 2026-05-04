# dellingr

An embeddable, pure-Rust Lua VM with precise per-opcode instruction-cost
accounting. No FFI, no system Lua dependency.

Originally extracted from
[fcomm2](https://github.com/folknor/fcomm2) — a fleet-combat sim where
Lua scripts control ships under a strict cost budget — and based on
[cjneidhart/lua-in-rust](https://github.com/cjneidhart/lua-in-rust),
heavily modified.

## Status

The public API (`State`, `HostCallbacks`, `RustFunc`, cost analysis) is
**not yet stable**. Consume via git dependency until v0.1 lands on
crates.io.

```toml
[dependencies]
dellingr = { git = "https://github.com/folknor/dellingr", branch = "main" }
```

## What it provides

- `State` — VM instance with cost-bounded execution.
- `HostCallbacks` trait — embedders redirect `print`, hook errors, etc.
- `RustFunc` — expose Rust functions to Lua scripts.
- `analyze_cost` — static worst-case instruction count for a script.
- Per-state user-data attachment for hanging embedder context off the VM.

## What it deliberately omits

`assert`, `pcall`, `coroutine`, `io`, `os`, `debug`. Designed for
sandboxed embedding, not as a general-purpose Lua replacement.

## Standalone CLI

The crate also ships a `dellingr` binary for running `.lua` files:

```sh
cargo run --release -- path/to/script.lua
cargo run --release -- --analyze path/to/script.lua
cargo run --release -- --limit 100000 path/to/script.lua
```

## License

MIT — see [LICENSE](./LICENSE).
