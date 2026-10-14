# hygiene.md

Hygiene findings from the hunt over the compiler, vm, surface, stdlib and
tooling scopes: values with more than one owner, rules implemented more than
once, and assumptions about the host. Hunter reports, not verified findings:
each may be wrong.

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

# Opcode and encoding ownership

## HYG-003 - "Uncached" has two encodings

Reported by the compiler hunter; narrowed by a fixer, who documented the
encoding contract at the instr.rs, `assign_cache_slots` and verifier sites.

`GET_GLOBAL` and `GET_FIELD`/`SET_FIELD` use 255 for "uncached"; `SET_GLOBAL`
and `TFOR_CALL` use a +1 bias with 0 for "uncached". Snapshots store raw
instruction words, so unifying them behind a `CacheSlot` newtype needs a save
format bump plus coordinated changes in compiler, verifier, save_state and the
runtime decoders (`eval_index.rs`, `eval_store.rs`). Worth doing alongside the
next format bump.

## HYG-007 - Cache-slot allocation and the tfor-cursor count are each derived twice

Reported by the compiler hunter. Both copies are deliberate; recorded with
what keeps them in step.

- `assign_cache_slots` counts `tfor_cursor_len` and discards it;
  `RuntimeCaches::new` recomputes it as `max(C)` over the code, because a
  fourth count field would change the snapshot format. The verifier's
  sequential slot check keeps them in step.
- The verifier restates the cache-slot allocation algorithm exactly
  (first-use order for globals, instruction order for fields and set-fields),
  held by `compiler_examples_pass_stack_discipline_verification`. Any policy
  change (for example sharing GET_FIELD slots by name) must change both.

# Limits and tunables

## HYG-010 - Runtime limits have no injection point

Reported by the vm, surface and stdlib hunters; the public limits surface,
`limit` fields on the limit errors and name-citing prose are done. Pattern
recursion and capture constants in patterns/luapat.rs and the save-state field
caps are not yet on the public surface.

No injection point: `call_depth_exceeded_error` has to recurse a real 1000
frames on a big-stack thread, and the stack-cap tests fill a million slots. A
per-State `Limits` struct (defaulted, fixed at construction for determinism,
recorded by save/restore) would let tests use small caps and the game tune per
script.

## HYG-016 - Line numbers have two widths

Reported by the compiler hunter.

`CallSite` saturates call lines at 2^24, while `line_info` is `u32`. In a
source longer than 16M lines, a call's traceback line freezes while
neighbouring instructions report the true line. Use one width or one
saturation rule.

# Cost model

## HYG-018 - Per-function native cost rules are scattered literals, inconsistent with each other and with the README

Reported by the stdlib hunter.

README "Budget" says data-dependent native work in string, pattern and table
libraries is charged. The rules are per-closure literals:

- `find`, `match`, `gsub` charge `(s + p).max(1)` and then `p.max(1)` again
  (pattern bytes twice); `gmatch` charges `(s + p)` at construction and
  `p.max(1)` at first iteration via `memoize_gmatch_pattern` (vm.rs) - a fourth
  rule, owned by the VM.
- `string.len` free; `sub` charges output bytes but not a number-to-string
  coercion; `upper`/`lower`/`reverse` charge input bytes.
- `table.insert`/`remove` charge a flat 1 regardless of shift (unless
  `table_insert_at` charges internally, splitting the rule across library and
  VM); `table.pack` charges 1 for any argument count; `unpack` (both spellings)
  charges nothing for up to 255 `get_table` calls, each of which may invoke
  `__index`.
- `print` builds up to a 16 MiB message free; `tonumber` parses up to a 16 MiB
  string free (including `parse_integer_with_base`, an unbounded f64
  accumulation); `tostring`, `select`, `next`, `ipairs` free.
- math.rs says "costs 1" on every function: 20 copies of one constant.

Nothing answers "what does each stdlib function cost", and `analyze_cost`
cannot see any of it. Fix: a declarative registration table per library
(`&[LibFn { name, func, cost: CostRule }]` with `Flat(1)`, `PerByte`,
`PerElement`) and a doc-tested reference table generated from it. Enforceable:
a test that every registration has a cost rule; a cost-snapshot test per
function (fixed inputs, exact `cost_used`).

# Errors and diagnostics

# State, ordering and ambient dependencies

## HYG-036 - Panic safety via `catch_unwind` is half a policy

Reported by the vm hunter.

`catch_unwind` is applied in `with_restricted_env`, `with_rooted_values` and
vararg frames, but explicitly not on the main Lua call path (an eval.rs comment
says the State must be discarded after an unwind). A panic anywhere under a Lua
call already leaves the State inconsistent, so the partial guarantees buy
nothing. Either poison the State on unwind (a flag checked on entry) and drop
the per-site guards, or make it uniform.

## HYG-039 - Saves embed the crate version, and function identity depends on the host build's code folding

Reported by the surface hunter.

`save_state` embeds `env!("CARGO_PKG_VERSION")`, so identical worlds saved by
0.4.0 and 0.4.1 differ in bytes; the module doc's "two saves of the same logical
state are byte-identical" holds only within one crate version, which matters to
a host that content-hashes saves. Make it optional or document it in the claim.
`register_rust_fn` keys the reverse map on `func as usize`; the comment notes ICF
folding under release LTO but not the consequence: two distinct host functions
folded to one address both save as the smallest id, so after load in a build
that does not fold them, a value that was `b` comes back as `a` and Lua `==`
between them changes across save/load. Document it or refuse when two ids share
an address.

## HYG-040 - The stdlib-or-not decision has two owners

Reported by the surface hunter.

`Engine { install_stdlib }` decides it for `new_state`; `State::load_state`
ignores any Engine and decides from the save's `has_standard_environment` flag.
An `Engine::raw()` user loading a stdlib save gets a stdlib State with no
refusal. If intended, `load_state` should live on `Engine` and check or override
the flag explicitly.

# Rules duplicated inside the compiler

## HYG-072 - Table key normalisation now runs on insert and hash

Reported by the wave 4 reviewer; perf, unmeasured.

`Table::insert` builds a `TableKeyInput` and runs the full normaliser to reject
nil and NaN, and `Hash for Val` routes numbers through it. Both should inline to
the old code; check with the next `tables/*` bench.

## HYG-060 - Tool directories are ignored wholesale

Reported by the tooling hunter; the dead ignore entries are fixed.

`.agents`, `.codex` and `.claude` are ignored wholesale, so
`.claude/settings.local.json` changes are invisible to review (possibly
intended). `.review.toml` holds only an `[_audit] id`, is read by nothing in the
tree (the external `review` tool's state), and is listed in Cargo.toml
`exclude`.

# Output channel and lints

## HYG-048 - Library code writes to stdout, and the debug features are never compiled by the gate

Reported by the compiler, vm, surface and tooling hunters.

`debug_parser` prints `println!("Compiled chunk: ...")` in `parse_chunk` - the
raw pre-finalize chunk with NOPs and unassigned cache slots, not the shipped
bytecode. `debug_vm` prints every instruction (vm/frame.rs) and `debug_gc`
prints in vm/object.rs (two sites), all to stdout, interleaving with script
`print` output and bypassing `HostCallbacks`. `brokkr.toml` sweeps only
`default` and `snapshot`, so `debug_parser`, `debug_vm`, `debug_gc`, `hotpath`
and `hotpath-alloc` are never built by `brokkr check`; nothing outside one
AGENTS.md line exercises the debug features, which the tooling hunter reads as
a sign they are dead.

`HostCallbacks::on_print`'s default calls `println!` from library code, and
`DefaultCallbacks` (documented "suitable for CLI tools") is what
`Engine::new_state()` and `State::new()` hand every embedder, so a library
consumer that forgets to override writes to the process's stdout.
`DefaultCallbacks` is an empty impl existing only because the trait has default
bodies. GC collections, budget exhaustion and depth-limit hits produce no
host-visible event besides the error; there is no GC callback.

Fix: route dumps through a host hook, or delete the debug features and add a
`Bytecode` disassembler test helper; make `on_print` required or a no-op
default, with main.rs supplying a `StdoutCallbacks`. Enforceable:
`clippy::print_stdout` / `print_stderr` denied crate-wide, allowed in main.rs
and at feature-gated sites; `[[check]]` entries per feature (a clippy-only pass
would do) or delete the features.

## HYG-051 - `hotpath` is a non-optional dependency of the library

Reported by the tooling hunter.

`hotpath = "0.28.6"` is unconditional and the features only toggle
`hotpath/hotpath`, so every downstream consumer (the game) compiles hotpath and
its dependency tree for no-op macros. Make it `optional = true` with a
`cfg_attr` shim, or record why not. Enforceable: a `cargo tree -e normal` check
in the gate. Related: `update_line` in the parser carries
`#[hotpath::measure]` on a one-line setter called per token, inflating the
parser's profile distribution.

# Tooling and test harness

## HYG-053 - The differential test accepts either reference version

Reported by the tooling hunter; the rest of the entry is fixed.

A match against either lua5.2 or lua5.4 passes the whole file, so a regression
  toward 5.2 behaviour in a 5.4-intended feature is invisible; per-file `-- REF:
  5.4` pinning would sharpen it.

## HYG-055 - CLI exit codes are ad hoc

Reported by the tooling hunter; the output, usage and argument-handling parts
are fixed (table-driven parser in main.rs, summary on stderr).

`exit(1)` at seven sites in main.rs for usage, IO, parse and runtime errors
  alike; `exit(0)` for help; hotpath.rs uses 2 for usage and 1 otherwise;
  hotbench.sh 2 for usage; diff_test.sh 1. A caller cannot tell "script failed"
  from "invoked wrong" or "budget exhausted", which the game host cares about.
  Fix: an `ExitCode` enum per binary with distinct usage, IO, compile, runtime
  and budget codes, `main` returning it. Enforceable: ban
  `std::process::exit` in main.rs and `examples/` via `disallowed-methods`.

## HYG-056 - The bench harness has residue

Reported by the tooling hunter; the rest is fixed.

- The marker protocol (`<us> NAME`, `<us> @k=v`) is defined here and parsed in
  brokkr, with nothing keeping them in step (shared protocol crate or
  brokkr-side schema test).
- The plain-pattern classifier test re-spells the magic-byte literal instead of
  iterating luapat's constant, so it does not tie the fast path to the grammar.

## HYG-057 - The bench/examples kernel pairing is unenforced

Reported by the tooling hunter; the rest is fixed (bench.sh reads brokkr.toml,
one reference-Lua array, `generate.py --check`).

Hash pins are enforced; the "same kernel in the same proportions" pairing
between `bench/X` and its examples/ counterpart is not. A test reading
brokkr.toml could check both files define `_bench` and that their per-call
`cost` is in a fixed ratio. A fixer notes brokkr.toml declares no expected
ratios and the composite and parse pairs are scaled differently. The README's
`benchmark` row may still describe the old `examples/benchmark.lua` kernel.

# Dead code and stale docs

## HYG-063 - Dead or redundant code in the VM, surface and stdlib

Reported by the vm, surface and stdlib hunters.

- The commented-out alternative in `SyntaxError::is_recoverable` (`//
  matches!(self, Self::UnclosedString | Self::UnexpectedEof)`) records an open
  REPL question; move it to notes/todo.md or delete it.
- `LuaPattern::matches_bytes` is `cfg(test)` only (documented, fine).
