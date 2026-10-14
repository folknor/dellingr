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

## HYG-010 - Runtime limits are private constants, restated as numbers in prose, with no injection point and no list

Reported by the vm, surface and stdlib hunters.

`MAX_CALL_DEPTH = 1000` and `MAX_STACK_SIZE = 1_000_000` (vm.rs, private),
`MAX_METAMETHOD_DEPTH = 200` (vm/metamethod.rs, private), `MAX_SYNTAX_DEPTH`
(`pub(crate)`), `GMATCH_PATTERN_CACHE_ENTRIES`/`_BYTES` (vm.rs),
`COST_CHECK_INTERVAL` (frame.rs), and the parser's 254/255 literals. Only
`MAX_STRING_BYTES` is public. README restates "200 levels", "1000 frames",
"1,000,000 values" and "16 MiB" (twice); AGENTS.md restates 1000 and
1_000_000; error.rs doc comments say "runs 1000 frames deep"; an eval.rs
comment says "MAX_CALL_DEPTH = 1000". The error variants
`CallDepthExceeded`, `StackOverflow` and `MetamethodDepthExceeded` carry no
`limit` field, unlike `StringSizeExceeded { size, limit }`.

No injection point: `call_depth_exceeded_error` has to recurse a real 1000
frames on a big-stack thread, the stack-cap tests fill a million slots, and
public types in error.rs (`PrefixLocation::TraceFrame`, `ScriptError`) are
shaped around debug-build stack size at depth 1000.

Fix: a `pub mod limits` (or `State::limits()`) re-exporting every limit; give
every limit error a `limit` field; reword prose to cite the name. A per-State
`Limits` struct (defaulted, fixed at construction for determinism, recorded by
save/restore) would let tests use small caps and the game tune per script.
Enforceable: a test that every `const MAX_*` in `src/` is re-exported; a
doctest asserting README numbers equal the consts.

## HYG-013 - Capacity hints and headroom are hand-counted, and some are already off

Reported by the vm and stdlib hunters.

`stack` 256, `call_stack` 64, `transient_roots` 64, table `INLINE_CAPACITY 4`,
`reserve_stdlib_capacity` (`globals.reserve(Builtin::COUNT + 4)`,
`heap.reserve(8, 96)` - VM magic numbers describing the library's shape),
`new_table_with_capacity(24)` for math (22 entries registered), `(10)` for
string (10), `(8)` for table (7). `STDLIB_SETUP_HEADROOM = 8` (lua_std.rs) is
justified in prose ("four slots") and checked by nothing. Nothing lists the
tunables. Fix: per-library `const FUNCTIONS: &[(&str, RustFunc)]` with
capacities from `.len()`; named consts in one tuning section; a test that the
stack high-water mark during `open_libs` stays within the headroom.

## HYG-014 - The table-library fast path silently disables itself if the function count changes

Reported by the stdlib hunter.

`TableLibraryFallbackCache.names: [Val; 7]` (vm.rs), and
`capture_table_library_fallback` does `live_string_keys().try_into()`; any
other count sets the cache to `None` with no failure and no log. The count's
owner is `open_table`. Fix: derive the length from table.rs's function list
(`[Val; TABLE_FUNCTIONS.len()]`), making the coupling a compile error; test
that the fallback is `Some` after `open_libs`.

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

## HYG-024 - Aborts on caller-controlled input

Reported by the compiler and vm hunters; the compiler half is fixed by a 16 MiB
source cap.

- Public API (vm): `check_any`, `check_type`, `is_none_or_nil`
  `assert!(arg_number != 0)` (vm_aux.rs); `Val`'s `Hash` impl hard-asserts
  non-NaN (lua_val.rs), safe only while every insertion path pre-rejects NaN;
  `insert`/`remove`/`replace` accept whatever `convert_idx` accepts and are
  otherwise unvalidated.
- Enforcement idea (vm): `clippy::panic` / `clippy::manual_assert` scoped to the
  public API.

# State, ordering and ambient dependencies

## HYG-034 - Several invariants rest on call ordering in one function

Reported by the vm, surface and stdlib hunters.

- The gmatch cache `gmatch_patterns: IndexMap<StringPtr, _>` (vm.rs) is safe
  only because `gc_collect`, the sole path that frees strings, clears it first. A
  second collection entry calling `heap.collect()` directly (object.rs tests do)
  would leave keys aliasing a new string at a reused slot, running a different
  pattern. "memoize must precede match" in
  `gmatch_subject_matcher_and_cost_meter` is the same shape. Fix: key by content
  or move the clear into sweep; a typestate token from memoize to match.
- `with_restricted_env` swaps `globals`/`builtins` and bumps `globals_version`
  but does not refresh `table_library_fallback`; safe because the cache compares
  the table ObjectPtr, an argument that lives in the cache's doc comment.
- `State::open_libs` calls `lua_std::open_libs` then
  `capture_table_library_fallback`; other construction paths
  (`with_callbacks`) must remember the pair. Make it one function.
- `pending_bytecode_caches` bridges a GC that may run between
  `resolve_bytecode_runtime` and `alloc_lua_fn` in `push_closure`; correctness
  rests on push/pop order in that function.
- `load_state` clears `registry`, runs `setup`, then `materialize_payload`,
  whose allocators must "never run GC" (module doc) with no type or assertion. A
  `debug_assert` on a GC counter or a `NoGc` token would make it structural.
- `env_tokens` (captured at construction) and `builtins` are two writers of
  "which object is `math`"; if a host calls public `open_libs` twice they
  diverge. Check who refreshes `env_tokens`.
- `validate_quiescent` lists ten fields by hand; a new in-flight vector would
  not be added. Put "is quiescent" on `State` next to the fields and
  `debug_assert!` it at `call`/`eval` entry and exit.

## HYG-036 - Panic safety via `catch_unwind` is half a policy

Reported by the vm hunter.

`catch_unwind` is applied in `with_restricted_env`, `with_rooted_values` and
vararg frames, but explicitly not on the main Lua call path (an eval.rs comment
says the State must be discarded after an unwind). A panic anywhere under a Lua
call already leaves the State inconsistent, so the partial guarantees buy
nothing. Either poison the State on unwind (a flag checked on entry) and drop
the per-site guards, or make it uniform.

## HYG-037 - Constant folding of `^` writes the host's libm result into bytecode

Reported by the compiler hunter.

`parse_pow` folds with `f64::powf`, which calls the platform math library and
is not guaranteed bit-identical across targets or libm versions. The VM uses the
same function at runtime, so one host agrees with itself, but snapshots carry
compiled bytecode, so a replay on another host mixes host A's folded constant
with host B's runtime `powf`. `pow` needs one owned implementation (for example a
pure-Rust libm port) used by both, or `^` should not be folded; `+ - * / %` are
IEEE exact. Related fold-ownership point: each fold operator is paired with its
function in the parser (`parse_addition`, `parse_multiplication`, `parse_pow`)
and again in `vm/frame.rs`, guarded only by a comment; an
`arith_fn(opcode) -> fn(f64, f64) -> f64` in numeral.rs would be the single
path.

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

## HYG-041 - Process-global state shared across States

Reported by the vm, compiler and stdlib hunters.

`anchor::NEXT_STATE_ID` is a process-global `AtomicU64` (only Anchor identity
and Debug; documented in anchor.rs). `GMATCH_WRAPPER` (lua_std/string.rs) is a
`static OnceLock<Arc<Bytecode>>` compiled once per process; sound because
`Bytecode` is immutable and caches are per State, but its `source` is `None`, so
tracebacks through `gmatch` name no chunk, and its `Arc` identity is shared
across States, which matters if snapshot or identity logic keys on the pointer.
The hunters found the RNG properly owned and `format_pointer_ids` per-State
deterministic.

## HYG-042 - Native stack depth is an unstated host requirement

Reported by the vm hunter. Already tracked in notes/todo.md ("Depth limits are
not stack-safe in unoptimized builds"); recorded here for the hunt's account.

`MAX_CALL_DEPTH = 1000` is safe only on a large enough native stack. An
embedder calling from a small-stack thread (a 2 MiB worker, or musl's 128 KiB
default) gets an abort, not `CallDepthExceeded`. Nothing detects or documents
the required stack size. Document a minimum and/or make the depth limit
injectable (HYG-010). BUG-004 is the same failure mode with no limit at all.

## HYG-043 - Unbounded or quadratic growth in the VM and registries

Reported by the vm, surface and compiler hunters.

- `format_pointer_ids` (a Vec with linear probe) is pruned only in
  `gc_collect`; with `gc_disable_auto` it grows without bound and each `%p` /
  `tostring` of a table is O(n). Use an `IndexMap<Val, u64>` and/or prune
  independently.
- `open_upvalues` `find_or_create_upvalue` is a linear scan plus sorted
  `insert`, O(n) per capture; bounded by the stack cap but quadratic.
- `Registry` (anchors) is embedder-controlled and unbounded, and `heap_size()`
  does not count it.
- `rust_fns_by_id` and `rust_fn_ids_by_addr` never shrink; no unregister.
- Swept `bytecode_caches` entries keep literals alive one extra GC
  (documented).
- Every nested function clones `source: Option<String>` (`parse_chunk`); make it
  `Option<Arc<str>>`.

# Rules duplicated inside the compiler

## HYG-070 - `ArgDetail` is chosen by matching message text

Reported by the wave 4 reviewer; typed `ErrorKind::argument_detail`
constructors now exist in error.rs.

The stdlib still goes through the text-matching `bad_argument` path:
`lua_std.rs` `exact_integer_value`, two `math.random` branches, basic.rs
`tonumber`/`rawlen`/`select`, string.rs `gsub`, table position validation and
string_format.rs `format_arg_error`. Convert them, then drop the text matching.

## HYG-073 - Some execution errors bypass the on_error reporting boundaries

Reported by the wave 5 error-kinds fixer; unverified.

Stack preflight failures and result-padding failures are raised outside the
three boundaries that notify `on_error`, so a host may see the error returned
without the callback firing. The wave 5 reviewer confirms `eval_closure_frame`
returns early on `check_stack_space` and vararg `pop` failures without
`report_frame_error`. Related: the private `reported` flag lives on the `Error`
value, so a host that stores an error from one `call` and returns the same value
from a later `RustFunc` gets no `on_error` for the second escape; undocumented.

## HYG-074 - Pattern and compiler-internal errors are still built ad hoc

Reported by the wave 5 reviewer; residue of the mislabelled-error-kinds entry.

- gmatch, match, find and gsub map `PatternError` to
  `RuntimeError(err.to_string())` at four sites in string.rs, shedding
  structure.
- The compiler builds internal errors several ways: `compiler.rs`
  `internal_error` (no location), `error_at(InternalError, 0)` in parser.rs
  (renders a false line 1), `finalize`, and a new "outer literal index stack
  empty" `error_at(..., 0)`. One helper per kind.

## HYG-071 - Error-message function names depend on the build

Reported by the wave 4 reviewer.

`active_rust_function_name` matches the running callback by function-pointer
address against globals, then falls back to the snapshot registry, which exists
only with `feature = "snapshot"`. LTO or identical-code folding can merge two
callbacks, and a generic `check_type` inside `math.*` is named in a snapshot
build but anonymous otherwise. Deterministic within one binary, but replay logs
compared across builds or feature sets can differ in error text. Document it in
reference, or name callbacks at registration instead of by address.

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

- `bytes_coerce` is a pure alias of `bytes_with_default_string_coercion`.
- String size check duplicated: `check_string_size` (vm.rs) and an inline `len >
  MAX_STRING_BYTES` building the same error in save_state.rs;
  `resolve_bytecode_runtime_no_gc` skips the check (relies on load-time
  validation).
- `ErrorKind::InvalidJump` is constructed only in vm/frame.rs `jump` (and its
  test), reachable only if verification has a bug; an `InternalError` in
  disguise with its own public variant.
- The commented-out alternative in `SyntaxError::is_recoverable` (`//
  matches!(self, Self::UnclosedString | Self::UnexpectedEof)`) records an open
  REPL question; move it to notes/todo.md or delete it.
- `Box<dyn HostCallbacks + Send>` in `Engine::new_state_with_callbacks` and
  `State::load_state` restates a bound `HostCallbacks: Send` already has.
- `base_unpack_values` (basic.rs) and `table_unpack_values` (table.rs) are
  one-line wrappers around `unpack_values`; if they exist for distinct fn
  addresses in the snapshot registry, say so in a comment.
- `LuaPattern::matches_bytes` is `cfg(test)` only (documented, fine).
