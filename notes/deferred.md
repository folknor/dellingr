# Deferred findings

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

Entries moved out of the bug and hygiene documents (hunt at 6c795d1)
before the resolution loop, judged on their face rather than verified. They keep
their IDs; cross-references to other IDs point back into those two documents.
Entries are moved whole, so some carry a small part that is fixable as written;
the category names the part that kept them out of the loop.

## Widens the project

## BUG-010 - CLI accepts `-` as a positional, then reads a file literally named `-`

Source: host-surface hunter.

`-` is excluded from the unknown-option check (the conventional stdin spelling)
but is then read as a path. Either implement stdin or reject `-` with a message;
`docs/cli.md` says nothing about it.

## HYG-079 - Host boundary assumes UTF-8: lossy `print`, UTF-8-only source

Reported by the stdlib and host-surface hunters.

`print` converts to `&str` lossily because `HostCallbacks::print` takes `&str`;
scripts can print arbitrary bytes and the host sees U+FFFD, and the 3x expansion
preflight in `print` and `error` exists only because of it. `load_string(&str)` is
the only load API, so the CLI's `fs::read_to_string` refuses non-UTF-8 sources
(a clean refusal naming the path) although Lua strings are bytes; non-UTF-8 paths
become the chunk name via `to_string_lossy`, silently degrading the identifier.
Fix pre-1.0: byte-exact `on_print(&[u8])` and a `load_bytes`.

## HYG-035 - Thin wrapper layers and a facade with one bool of payload

Reported by the compiler and host-surface hunters.

`Parser::describe_token_type` returns `typ.spelling()`; `parse_fndef`,
`parse_fndef_named`, `parse_fndef_method` forward to `parse_fndef_with_params`;
two layers of `parse_str`/`parse_str_named` (`compiler::` wraps `parser::`).
`parser::parse_str` exists separately only so tests can see pre-`finalize`
output. On the host side, `Engine::compile`/`compile_named` never reference
`self` (Engine's only state is `install_stdlib: bool`); `analyze_cost(source)` and
`Engine::analyze_cost(&program)` are two entries to one thing;
`State::load_string(_named)` is a third compile path. Fix: one
`compile(source, name)` (test helper for pre-finalize output); either drop
`Engine` for `Program::compile` + `State::new`/`raw`, or move real configuration
(budget defaults, RNG seed, callbacks factory) into it.

## HYG-067 - libm transcendental functions and `powf` make replay output host-dependent

Reported by the compiler, stdlib and host-surface hunters.

`math.sin/cos/tan/asin/acos/atan/atan2/exp/log` call `f64` methods lowering to
platform libm (the module doc admits last-ULP differences); `vm/frame.rs`
`OP_POW` calls `f64::powf`. The compiler carefully refuses to fold `^` so
bytecode carries no host result, but runtime `^` still produces host bits. A
replay recorded on glibc x86-64 and replayed on aarch64 or musl can diverge once a
script branches on such a result. `numeral.rs`'s `2_f64.powi` is exact for powers
of two but still an intrinsic. The clippy ban list covers clock, env and RNG, not
this. Fix: a fixed pure-Rust implementation (e.g. `libm` crate software paths or
core-math ports) owned by `numeral.rs`/`math.rs`, plus `disallowed-methods` for
`f64::sin`, `powf`, `powi`, `exp` and kin; or `docs/determinism.md` states plainly
that cross-platform replay is not guaranteed for these. Enforceable by clippy.

## HYG-068 - Native stack size is an implicit configuration value

Reported by the compiler, VM-core and host-surface hunters.

`MAX_CALL_DEPTH` plus `MAX_METAMETHOD_DEPTH` assume enough native stack; on a
small-stack embedder thread deep recursion aborts instead of returning
`CallDepthExceeded`. Documented in `docs/limits.md` but undetected (no
stack-remaining probe such as `stacker`/`psm`, no per-State override). The
`eval.rs` rules (no `catch_unwind`, no hotpath on the recursive path) show
per-frame bytes are already load-bearing. `call_depth_exceeded_error` enforces it
only at the test thread's stack size. Test thread stacks are now named constants
(`DEEP_PARSE_TEST_STACK_BYTES`, `DEEP_RECURSION_TEST_STACK_BYTES`), but frames
outgrowing them still fail as an abort, not an assertion. What remains is
detection or configurability: a stack-remaining probe or a per-State
native-stack setting, which widens the embedding surface.

## Replay- or cost-visible changes

## HYG-058 - Array part never shrinks, so queue-style tables grow with history

Reported by the heap hunter.

A FIFO written the obvious way (`q[tail] = v` / `q[head] = nil`) appends at
`array.len() + 1` while clearing the front, growing the `Vec<Val>` by one slot per
push forever: 16 bytes per historical push, uncharged; `next_live_from` scans the
dead prefix, so every `pairs`/`next` start is O(total pushes) for a flat charge;
`snapshot_entries` emits one `(key, Nil)` per array slot, so save size and
verify/materialize work grow with history. Clearing the first half of a large
array has the same effect on traversal. The no-shrink rule protects `pairs`
assigning nil and cached slot indices; shrinking only when no traversal can be
mid-flight (save time, or GC) preserves both. `#` on trailing nils is
replay-visible, so any change must be called out.

## HYG-001 - Cache-slot operand encoding has four owners and two sentinel conventions

Reported by the compiler and VM-core hunters.

- `GET_GLOBAL`/`GET_FIELD`/`SET_FIELD` use 255 for "uncached"; the VM relies on
  `caches.x.get(255)` returning `None` because no cache vector reaches 256
  entries, which nothing asserts. `frame.rs` passes a literal `u8::MAX` for
  `SET_FIELD_AT` to mean "no cache".
- `SET_GLOBAL`/`TFOR_CALL` bias by one (0 = uncached), decoded with
  `checked_sub(1)` in `eval_store.rs::instr_set_global` and
  `eval_control.rs::instr_tfor_call_next`. `SET_FIELD_AT` with A=0 means "not yet
  rewritten". All persisted.
- Each rule is spelled in the `instr.rs` constructors, `assign_cache_slots`, the
  verifier (`compiler/verify.rs`) and the VM decode sites. `RuntimeCaches::new`
  recomputes the TFOR cursor count a third way (max C operand) because storing a
  fourth count would change the snapshot format.
- The verifier copy is a deliberate independent check, and `finalize` runs
  `validate_bytecode` on every compile, so drift fails every compile (the
  verify.rs comment undersells this by naming only the corpus test). The VM
  decode copy has no checker.
- Related sentinel copies: `OP_VARARG` checks `n == u8::MAX` for "all values" in
  `frame.rs` and `verify.rs` instead of `RetCount::from_u8`; `ArgCount`/`RetCount`
  panic messages hard-code "below 255" beside `limits::MAX_FIXED_ARGS = u8::MAX -
  1` and `MAX_ARGUMENTS = u8::MAX`.

Fix: typed operand decoders on `Instr` paired with the encoders
(`global_get_slot(self) -> Option<u8>` etc.) used by VM, verifier and finalizer;
at the next format bump unify on one convention (biased, 0 = uncached) and store
the TFOR count beside the others, retiring the recomputation. Format the panic
strings from the const. Enforceable: decoders make raw decodes stand out; a grep
for `== u8::MAX` / `inst.a()` near cache opcodes in `src/vm/`; a verifier test
pinning cache vectors below the sentinel.

## HYG-074 - Anchor and empty-pattern handling decided in several places; gmatch rewrite charges an extra unit

Reported by the stdlib hunter.

`luapat::Compiler::compile` decides `anchored` from the first byte; `gsub`
decides again from `pattern.first() == Some(&b'^')`; `gmatch` rewrites a leading
`^` to `%^` by byte surgery to reuse the compiler. The rewrite makes
`charge_bytes` see `len + 1`, one extra replay-visible cost unit per iterator from
an implementation detail. Empty-pattern fast paths in find, match, gsub and
gmatch are four special cases with their own return shapes. Fix:
`CompiledPattern::is_anchored()`; an `AnchorMode::{Honour, Literal}` compiler
flag for gmatch (removes the rewrite and the extra unit, a cost change to call
out); decide once whether the empty fast paths are needed (`str_match` already
handles an empty item list).

## Unmeasured performance

## HYG-045 - Error-path cleanup decided per site

Reported by the VM-core hunter; the panic-safety half shipped as the poison
flag. Remaining: `call_op` restores its frame with several
`truncate_call_frame` calls, `eval_closure_frame` with several
`cleanup_closure_frame` calls, and `handle_index_metamethod_inner` and
`index_string` truncate temporaries by hand. A guard or `frame_scope` helper
would make "every exit restores" structural. Not done because it cannot be
a Drop guard (the guard would hold `&mut State` while the body uses it), so
it is either a closure helper - the frame bloat `call_op` already rejected
for `with_rooted_value` on the recursive path - or a plain struct that every
`Ok` exit must disarm by hand, which keeps the per-site count it was meant to
remove. Revisit only with a native-stack-per-level measurement of the
recursive path showing the closure form is frame-neutral.

## HYG-049 - Unbounded or super-linear VM growth paths

Reported by the VM-core and heap hunters.

- `bytecode_caches`: each `load_string` adds an `Arc<Bytecode>` entry swept only by
  GC; with `gc_disable_auto()` repeated loads grow without bound. Same for
  `format_pointer_ids` (documented), pruned only in `gc_collect`. Neither appears
  in `docs/limits.md`'s growth list (not verified).
- `open_upvalues` uses sorted `Vec::insert`: bounded by `MAX_STACK_SIZE`, but O(n)
  per insert, so capturing many locals in a deep frame is quadratic, uncharged (a
  performance concern per the budget doc).
- RustFn re-entrancy (a RustFn calling `state.call` on another RustFn) recurses on
  the native stack without touching `call_depth`; `docs/limits.md`'s "active Lua
  calls" is accurate but easy to misread as bounding all calls.
- Upvalue pool `slots`/`colors` never shrink; free-listed, bounded by peak. Noted.

## HYG-077 - Small stdlib duplications and unreachable checks

Reported by the stdlib hunter.

- `tostring` and `string.format("%s")` each implement "a string bypasses
  `__tostring` unless the shared string metatable has one"; `error` implements the
  opposite (always bypass). One `State::tostring_bytes(idx)` should own the first.
- Lua-index normalisation: `string.rs` has `lua_start_index`/`lua_end_index`,
  `table.rs` has per-function rules (matching 5.4's quirks), `select` its own.
  The rules really differ, but nothing documents them in one place.
- `_G.__index`/`__newindex` handlers check args 2 and 3 via `check_any_named`;
  the VM always passes them and scripts cannot reach the handlers (`__metatable =
  false`), so the checks are unreachable from Lua (a host could call them). Noted
  so nobody cites them as precedent.

## Comment or doc only

## Decisions for the user
