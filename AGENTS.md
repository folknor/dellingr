# What this is

`dellingr` is an embeddable, deterministic, pure-Rust Lua VM with per-opcode instruction-cost accounting. It is the script host for a game project; the README's "Won't implement" section is load-bearing - `pcall`/`xpcall`, `assert`, coroutines, `io`/`os`/`debug`, `goto`, integer division, bitwise ops, long strings, `string.rep`/`byte`/`char`, and arithmetic/comparison/concat metamethods are deliberately absent. Don't add them without asking. Errors kill the callback by design.

Ground rules that are easy to get wrong (the `docs/` pages have the detail):

- **The cost budget is a game-design instrument, not a CPU meter** (`docs/budget.md`). Charges are design decisions: flat and free charges are often deliberate, and structural semantics are free on purpose. Host work that is out of proportion to its charge is a performance concern, not a cost-model defect.
- **Deliberate divergences from reference Lua** are listed in `docs/compatibility.md`. Check there before treating a difference from lua5.2/lua5.4 as a bug.
- **Matching reference Lua is not a goal in itself.** A difference is a defect only when it hurts determinism, the cost model, or the script author (wrong results, a misleading message, a surprising behaviour). Don't report or "fix" a difference because it differs, and don't justify a change by "matches lua5.4" alone; error wording that already says what went wrong is fine as it is.
- **Replay-visible changes must be called out**, never slipped in: `pairs` order, rendered numbers and strings, the RNG draw sequence, and snapshot contents all feed game replays.
- **Determinism is a product requirement**, not a style preference - game replays depend on it.
- Lua written for tests and examples has no `assert`; use `if not cond then error(...) end`.

The crate ships a library (`Program`, `State`, `HostCallbacks`, `RustFunc`, `Program::analyze_cost`, `ArgCount`, `RetCount`) and a thin CLI binary (`src/main.rs`). The public API is pre-1.0 and not stable. MSRV is the `rust-version` in `Cargo.toml`; edition 2024.

## Rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.
- Never disable a lint; fix the code. The `clippy.toml` bans name their replacements. An `expect` string explains the invariant, not the call.

## Commands

`brokkr` runs a gremlins scan (banned Unicode), then clippy (warnings denied, so a failure short-circuits), then tests. `brokkr --help` and `brokkr test --help` cover the flags.

```sh
brokkr check                       # gremlins + clippy + every check shape and test
brokkr check --fix-gremlins        # rewrite banned Unicode in tracked files first
brokkr test <NAME>                 # focused run: case-sensitive substring, unit and integration
brokkr run --release -- script.lua                 # run a script
brokkr run --release -- --analyze script.lua       # static cost analysis, no execution
brokkr run --release -- --limit 100000 script.lua  # run with a cost budget
brokkr run --release -- --dump-bytecode script.lua # bytecode listing (Program::disassemble)
```

The CLI prints `Cost used: N` to stderr after each run; `--quiet` suppresses it.

`scripts/` holds the manual tools and the `brokkr check` script checks; each script's header says what it does. Shell scripts share `scripts/lib.sh`, which builds through `brokkr build`, so no script runs cargo or hard-codes a dellingr `target/` path. The ones worth knowing by name: `diff_test.sh` (manual diff against lua5.2/lua5.4), `bench.sh` (the README comparison table, run on request), `ab_pairs.py` (interleaved `--bench --commit` A/B cells), `frame_sizes.py`/`asm_diff.py` (native frame sizes from disassembly), `perf_hot_lines.py` (filters `perf annotate` output).

## Architecture

Pipeline: source bytes -> `compiler::compile` (the parser builds each function's tree, `codegen.rs` emits register bytecode, `verify.rs` admits it; the same verifier admits snapshot loads) -> `Chunk` -> executed by `State` in `vm::eval`. `Program::compile` is the one public entry.

- **`src/instr.rs`**: fixed-width 64-bit register instructions. Constant operands are separate opcodes (`ADD_RK`, ...), not an RK bit. `Builtin` gives well-known globals array-indexed access through a mirror instead of a hash lookup.
- **`src/vm.rs` + `src/vm/`**: the runtime. `State` owns the global environment (`_G` is an ordinary heap table, `vm/global_env.rs`), one shared value stack for Lua and Rust frames, the mark-and-sweep heap with interned strings (`vm/object.rs`), the cost meter (`cost_meter.rs`; the operation that crosses the budget completes, the next costed op fails), the execution-lifetime state in `in_flight` (`vm/inventory.rs`), host callbacks and user data, the seeded RNG, and the append-only Rust function table (`vm/rust_fn.rs`: a `Val::RustFn` is an index, never a code address).
- **`src/lua_std/`**: the standard library, opened by `open_libs` from `State::with_callbacks`. **`src/patterns/`**: the in-crate Lua-pattern matcher (callers pass the whole subject plus a start offset, never a re-slice, so `%f` keeps its left context).
- **Cost**: opcodes charge in `vm/eval.rs`'s dispatch; `analyze_cost` walks bytecode statically into a `ScopeCost` tree.

Invariants that are easy to break:

- **Calls do not recurse natively from Lua.** Lua-to-Lua calls push a frame onto the current dispatch loop's own frame stack. Only calls made from Rust (`call_unguarded`: host calls, metamethod handlers, iterators, library callbacks) start a nested invocation, counted by `limits::MAX_NATIVE_CALL_DEPTH`. Frames and records are written and dropped in place, never moved whole (store-forwarding; see `vm/eval.rs`). A new call path into Lua needs an entry in `reference/native-calls.toml` and a scenario in `tests/native_stack.rs` (`reference/native-calls.md` has the steps).
- **The stack cap is global.** Every path that grows `State.stack` is checked (`check_stack_slot`), preflighted (`check_stack_space(n)` before `push_unchecked`), or pops before it pushes. Never `self.stack.push(..)` directly. A rejected operation leaves the stack, string pool and function registry unchanged.
- **Error cleanup belongs to boundaries and resource owners.** Ordinary bodies propagate with `?`; `call_unguarded`, `get_table_unguarded` and `unwind_invocation` own the cleanup. A new host entry point that returns errors to continuing code defines and tests its own boundary.
- **GC roots have one source of truth**: `State::for_each_gc_root` destructures `State` without a rest pattern, so a new field does not compile until it is classified (`State::rest_state` does the same for lifetimes). Root an object with `GcHeap::mark`, never by pushing onto the worklist: a bare push traces the children but leaves the object unmarked, so sweep frees it.
- **Rust callbacks index the stack 1-based**; bytecode is 0-based.

## Tests

- `tests/integration/` is one crate of subject modules (`cli`, `compiler`, `corpus`, `cost`, `failures`, `host`, `library`, `runtime`, `snapshot`); `support` holds the shared helpers. Focus with `brokkr test snapshot::` or a file's module path. `tests/native_stack.rs` and `tests/libm_symbols.rs` are the only other targets; `corpus::test_conventions` enforces that shape. Unit tests live beside their modules.
- **Tests never write files** and spawn processes only through `tests/integration/support/process.rs` (textlint rules enforce both).
- **The example corpus** (`examples/`, listed in `tests/common/examples.rs`; regenerate the list with `python3 scripts/example_list.py --write`) is a gate: `corpus::run_examples` runs each example and compares it with lua5.2 and lua5.4. An example fails on a stdout line ending in `: false`, on stderr other than the cost line, or on a cost that differs from `tests/fixtures/corpus_cost.txt`. An intentional divergence carries `-- DIFF: <reason>` plus `-- EXPECT:` lines spelling its exact output; `run_examples.rs` has the rules. Examples are test surface: no throwaway scripts there.
- **Goldens are regenerated by explicit tools**, never by tests, and only after the change is reviewed:
  - `brokkr run --features snapshot regenerate_save_golden` (never the archived `save_released_v*.bin`)
  - `brokkr run --release regenerate_corpus_cost -- tests/fixtures/corpus_cost.txt`
  - `brokkr run --debug regenerate_error_boundary -- tests/fixtures/error_boundary_transcript.txt`
  - `brokkr run --release regenerate_crmath_golden -- tests/fixtures/crmath_golden.txt` (needs `research/core-math`)
  - `python3 examples/parse/generate.py` for `examples/parse/large_source.lua`, `python3 scripts/crmath_tables.py --generate` (then `--check`) for the math tables
- The non-default test-infrastructure features are described beside their declarations in `Cargo.toml`.

## Benchmarks

Workloads are `[dellingr.workloads.*]` in `brokkr.toml`, each pairing a seconds-scale `bench/` file (wall-clock verdicts) with its ms-scale `examples/` counterpart (`--hotpath`/`--alloc` profiling); `examples/hotpath.rs` is the harness and documents its phases and output. `brokkr --help`, `brokkr results --help` and `brokkr sidecar --help` cover the rest.

```sh
brokkr dellingr --lua <W>                          # run once, store nothing
brokkr dellingr --lua <W> --bench                  # best of 3, stored
brokkr dellingr --lua <W> --hotpath                # function-level timing
brokkr dellingr --lua <W> --alloc                  # allocation tracking
brokkr dellingr --lua <W> --bench --commit <ref>   # old VM, current workload file
```

- **Every measurement of dellingr's own performance goes through `brokkr dellingr`**: never hyperfine, `time` or a hand-rolled loop. Register a workload first. The one exception is `scripts/bench.sh`, which only regenerates the README comparison table.
- **Never run benchmark or profiling commands in parallel.**
- **Interleave matched A/B cells** and compare medians with sign consistency across pairs; `--bench N` is best-of-N within one cell and cannot cancel drift. A same-day matched pair beats any historical number.
- **Suspect code layout before mechanism**: binaries whose executed path is identical have measured about 11% apart. Run a five-launch same-binary control before trusting a small delta.
- Never point instrumentation (`--hotpath`, `--alloc`, any `--features hotpath` build) at a seconds-scale `bench/` file: its event queue is unbounded and backlogs tens of GB.
- Read instrumented runs as distributions (call counts, % of total), never as absolute times. When a result's iteration walls disagree, don't read it as one number. Before calling a delta a regression, read `brokkr sidecar`: SETUP is the standalone footer, WARM is the verdict phase.
- Stored runs need a tree clean of source changes (`--force` runs anyway and stores nothing).
- Performance numbers in markdown include commit hash and hostname.
- The paired files keep the same kernel in the same proportions (`corpus::bench_scripts` checks the bytecode). Editing a registered file means updating its pin in `brokkr.toml` (`xxhsum -H2 <file>`), which deliberately cuts its stored history; brokkr refuses on hash drift.
- A registered `hotpath_file` opens with `-- Workload:`, `-- Probe:` or `-- Historical probe:`. Probes isolate one path on purpose; their ratios are not representative.
- New target: write `examples/{category}/{name}.lua` with `_bench()` and a footer printing `"{category}/{name}: " .. tostring(_bench()) ..`, then run `python3 scripts/example_list.py --write`. Register it in `brokkr.toml` to make it a brokkr workload.
- Measurement sites use `#[cfg_attr(feature = "hotpath", hotpath::measure)]`. **Never put one on a function that stays live across a native re-entry** (`call_unguarded`, `run_invocation`, `Frame::run` and the helpers between them): it grows every native level. `scripts/native_calls.py` rejects it there. For anything touching `Frame::run`, measure `brokkr test native_bytes_per_level` (dev and `--release`) before and after.

## Conventions

- `.scratch/` is gitignored throwaway space, wiped by every `brokkr check`; nothing that must be kept goes there.
- `target/` is a symlink to a shared cargo cache.
- `notes/optimizations.md` and `notes/todo.md` are working backlogs (perf and non-perf); items are deleted as they land. `notes/work.md` holds the current fix loop's plan, driven by `notes/fix-loop.md`.
- Some doc comments still mention `FleetCallbacks` etc. from the game project the crate was lifted out of.

## Document folders

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | How the thing is built and why: architecture, invariants, contracts, `performance.md` (measured numbers over time) | Durable and binding: what it says must be true. |
| `docs/` | How the thing is used: guides, CLI, the consumer-facing API | Durable and binding. |
| `notes/` | Work items, plans, research, analysis: things that will die | No truth guarantee. Nothing durable cites it. |

Dependencies point one way: `notes/` may cite `docs/` and `reference/`, but nothing durable (code comments included) cites `notes/`, so a code comment carries its full context. Root-level convention files (`AGENTS.md`, `README.md`, `CHANGELOG.md`, ...) stay at the root. Don't cite source line numbers in any of these folders; they drift.
