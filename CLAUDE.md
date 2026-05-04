# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

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
./diff_test.sh                # compares examples/ output against lua5.2 and lua5.4 (must be on PATH)
./test_limited.sh <cmd>       # run <cmd> with a 2GB virtual-memory cap (for stress tests)
```

Debug-print feature flags: `--features debug_parser`, `debug_vm`, `debug_gc`.

MSRV is `1.92`. Edition 2024.

## Lint gate (don't disable, fix the code)

`src/lib.rs` denies a long list of clippy lints; `clippy.toml` adds disallowed methods/types. The non-obvious ones:

- **`unwrap_used` is denied outside `#[cfg(test)]`.** Use `?`, `expect("reason that explains why this can't fail")`, or proper error handling. The string in `expect` should explain the invariant, not just describe the call.
- **`Result::ok()` is banned.** It silently discards errors. Use `?` or handle the error.
- **`HashMap` / `HashSet` are banned.** Iteration order is non-deterministic. Use `IndexMap` (already a dep) or `BTreeMap` / `BTreeSet`.
- **`rand::rng` / `rand::thread_rng` are banned.** Unseeded RNG breaks determinism. Use `state.rng` (a `StdRng` seeded via `set_rng_seed`, default seed 0). `allow-invalid = true` is set on these entries so the gate doesn't trip on toolchains where the path moves.
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
- `rng: StdRng` seeded via `set_rng_seed`

`MAX_CALL_DEPTH = 1000`, `MAX_STACK_SIZE = 1_000_000`.

**GC roots**: `vm::mark_gc_roots` is the *single source of truth* for what's reachable. Any allocator that may trigger GC must use this same root set. Closed upvalues are reached transitively via the closures that hold them, not as a separate root set - there's an explicit comment about this (`vm.rs` / `vm/object.rs`).

**Stack indexing**: Rust callbacks (`RustFunc = fn(&mut State) -> Result<u8>`) use **1-based** indexing; Lua bytecode internally uses 0-based. `vm_aux.rs` and the lua_std modules show the 1-based pattern.

**Standard library**: `src/lua_std/{basic,math,string,table}.rs`, opened by `lua_std::open_libs(state)` from `State::with_callbacks`. Every `open_*` function pushes builtin functions onto the stack and uses `set_global` / `set_table_raw` to install them. `_G` exists but is wired through a metatable that proxies to `state.globals` (see `basic.rs`); it is not a real table.

**Cost model**: opcodes charge in `vm/eval.rs`'s dispatch; `analyze_cost` (`src/lib.rs`) walks bytecode statically and produces a `ScopeCost` tree (own + nested totals). The README's "Budget" section flags that structural ops like `while true do end` are intentionally free.

**Patterns**: Lua-pattern matching is delegated to the `lua-patterns` crate (not a custom impl). `string.rs` has helpers `is_plain_lua_pattern` (fast path) and `gsub_replacement` (string / table / function replacements with Lua's `%0..%9` capture syntax).

## Tests

- `tests/run_examples.rs` - runs every `examples/*.lua` via `cargo run` and fails on nonzero exit OR if the output contains the substring `: false`. Examples that print test results follow the convention `print("test name: " .. tostring(condition))`. Adding a new `examples/foo.lua` that prints `: false` will break this test.
- `tests/error_handling.rs`, `gc_upvalues.rs`, `gsub_errors.rs`, `metamethod_errors.rs`, `rustfn_error.rs` - focused integration suites.
- `tests/diff_test.rs` is a Rust harness; `diff_test.sh` is the differential shell script that compares output against `lua5.2` and `lua5.4`. Mark intentional divergences with a `-- DIFF: <reason>` comment in the example. `benchmark.lua`, `stress_*.lua`, and `upvalue_stress.lua` are skipped by the diff script.
- Unit tests live alongside their modules (e.g. `src/vm/object.rs` has GC tests).

## Project conventions worth knowing

- Examples in `examples/` are part of the test surface - don't add throwaway scripts there.
- The CLI prints `Cost used: N` after each run; `diff_test.sh` filters this line out before comparing.
- The crate was lifted out of a game project (originally extracted from `fcomm2`); some doc comments still mention `FleetCallbacks` etc. as illustrative examples.
- `target/` is a symlink to a shared cargo cache.

## Multi-Agent Orchestration

**Do NOT use worktree isolation for parallel agents.** Worktrees create merge conflicts that silently drop agent work. Instead, launch agents in the same tree with strict file ownership - zero overlap.

**Why no worktrees:** Worktrees let agents work on diverged snapshots. When merging back, `git checkout --ours/--theirs` drops code, conflict markers get missed, and features end up "existing but not wired" - types/functions created but never connected to bytecode dispatch, the standard library, or call sites. This has happened in long sessions and was only caught by a rigorous 3-pass audit.

**Agent coordination rules:**

- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file FIRST. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr check`, `brokkr test`, `cargo`, or `./diff_test.sh`. The orchestrator validates between agents.
- Include `CLAUDE.md` (and any other top-level docs they'll need, e.g. `LLM.md`) in every agent's required reading.

**Audit protocol:**

- Do not trust agent claims of completion. Verify existence + wiring + behavior.
- Use the 3-pass audit structure: domain-specific verification, then cross-cutting reconciliation (does the new instruction actually dispatch? is the new builtin actually installed by `open_libs`?), then editorial normalization.
- Any discrepancies doc should contain only current gaps, not historical records. Remove resolved items entirely.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of CLAUDE.md rules. They wrote them, so they know them.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not suggest saving things to memory. Durable context belongs in CLAUDE.md or the relevant docs, not in per-session memory files - this project is developed across several hosts and users, and memory does not transfer between them; CLAUDE.md does.

### Bash rules

- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`. Exception: piping into `review` is allowed (writing scratch prompt files is wasteful).
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.
- Never run `git` with `-C <path>`. Run `git` from the current working directory.

### git commit rules

- Never commit markdown changes alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files if dirty.
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.
