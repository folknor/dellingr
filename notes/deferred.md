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

## BUG-011 - A literal held only by closure bytecode loses its `%p` identity across save/load

Source: found while interning metamethod names (880ff5a), agreed by a codex
review; no test yet.

`encode_bytecode` writes string literals as raw bytes and never registers them
in the save's `string_ids`, so `SaveBuilder::finish` drops the `%p` id of a
string that only a live closure's literal pool reaches. After load the same
literal formats with a fresh id. The fix should cover identity-bearing literals
of serialized closures, not every bytecode cache. Replay-visible; changes
snapshot contents for affected states.

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

## BUG-008 - Traceback frames copy the source name per frame

Narrowed in wave 2 to its growth residue (name limits are now enforced at
registration and compile time). `StackFrame.source` is still a fresh `String`
per frame per error, so a long source name times `MAX_CALL_DEPTH` frames is
copied on every error, while chunks already hold it as `Arc<str>`. Fix: make
`StackFrame.source` an `Arc<str>` (touches `vm/frame.rs` traceback
construction and existing tests).

## HYG-064 - `OP_NEW_TABLE_PRESIZED` never presizes the array part

Narrowed in wave 2. The parser encodes the total field count (positional plus
keyed) in `A`; `frame.rs` passes it to `new_table_with_capacity`, which presizes
only the hash map, so `{1, ..., 10}` reserves an unused 10-slot map and grows the
array by doubling. A fix needs separate positional and keyed counts (parser,
allocator, table storage, possibly `instr.rs`) and must keep `pairs` order, `#`
and snapshot bytes unchanged. A comment at the dispatch site records this.

## HYG-033 - Verifier and finalize cost scales badly with source size

Reported by the compiler hunter.

`verify_stack_discipline` clones `AbsState` (two marker stacks up to 200 entries)
into every reachable pc: O(code_len x marker depth) memory per chunk, bounded but
large for 16 MiB sources. `finalize` runs `validate_bytecode` per chunk and
recurses, so a source with many nested chunks pays it many times, in release, on
every `load`. Fix: share marker stacks or store them only at join points.

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
- `LuaPattern::new` is uncached for find, match and gsub; a script calling
  `s:find(p)` in a loop recompiles every time (uncharged, bounded). Sharing the
  gmatch cache would be a win without cost-model change.

## HYG-060 - Small persistence redundancies

Reported by the heap hunter.

- `retype_load_error` restates every `LoadError` variant to change the phantom
  `E`; split into `LoadError::Setup(E)` + `LoadError::Snapshot(SnapshotError)` with
  one `From`.
- `Encoder::enforce_field_limits` is `true` in production and `false` only in the
  `cfg(test)` constructor, so tests encode payloads production refuses and never
  exercise the encoder's own limit checks; build raw bytes for oversized-field
  decoding and drop the flag.
- `Table::clear_and_insert_entries` has one production caller in
  `materialize_payload`, which already falls back to `rebuild_from_snapshot` when
  the array is non-empty; merging deletes it and the `exact_shape` flag.
- `SavePayload::encoded_size_estimate` ignores bytecode size, which dominates
  script-heavy worlds; the buffer still grows.

## HYG-061 - Two hash functions for one string lookup

Reported by the heap hunter.

`det_hash.rs` calls itself the crate's only hash-map type and fixes foldhash's
seed, but `StringPool` hashes with a hand-rolled FxHash (`hash_string`, pinned by a
test, endian-defined) and stores the `u64`s in an `IndexMap<u64, _>` that foldhash
hashes again (`det_hash` says foldhash may change per target). Neither reaches
observable order. Fix: key by content with foldhash, or a no-op `BuildHasher` for
the `u64` map; drop the pin test's stability claim if nothing needs it. Related:
`det_hash` offers no `IndexSet` alias, pushing code to `IndexMap<K, ()>` and
`BTreeMap<_, ()>` sets (`SaveBuilder::reachable_pointer_values`,
`expanded_environment_objects`, `encode_env_deltas`' `live_keys`); add one.

## Comment or doc only

## HYG-015 - Constant folding contradicts `docs/budget.md` and leaves dead pool entries

Reported by the compiler hunter.

Folding `PushNum; PushNum; Add` (and `Negate` of a literal) removes a charge, so
`x = 60 * 1000` costs 0, `local k = 60; x = k * 1000` costs 1, and `2 ^ 10` costs
1 (pow never folded). `docs/budget.md` says naming an intermediate must not make
a script more expensive. Only `tests/cost_golden.rs` (`folded_literals`) pins it;
the only doc mention is `docs/determinism.md`'s note on `^`. Either document
folding as a deliberate exception or charge folded expressions (awkward:
per-chunk static "folded units").

Folding also leaves intermediates (`60`, `1000`, `5`, `-5`) in
`number_literals`: they count toward `MAX_LITERALS`, are serialized into
snapshots and become per-State `BytecodeRuntime.literals` slots. Fix: compact in
`finalize` (remap `PUSH_NUM` Bx); changes snapshot contents of identical source,
so needs a note but not a format bump.

## HYG-055 - Duplicate-key and disjointness validation runs twice on two key representations

Reported by the heap hunter.

The verifier checks duplicate saved keys, deleted/upserted disjointness and
order-contains-upserts via `SavedVal::table_key`; `materialize_payload`
repeats every check via `insert_runtime_key` on runtime `Val`s. The second pass is
genuinely needed (two `Fn` ids can resolve to one address under ICF, two env
tokens to one object), but neither site says why, and the reason strings differ
("duplicate deleted environment key" vs "duplicate materialized deleted key").
Keep only the runtime check for env deltas and pointer ids, or document both and
share a helper parameterised by key type.

## HYG-065 - Rust function identity is by address: an implicit contract across dispatch, snapshots and equality

Reported by the VM-core, heap and stdlib hunters.

- `eval_control.rs` fast-paths `TFORCALL` by comparing against `base_next` and
  `base_ipairs_iter` addresses (re-exported from `lua_std.rs` for that purpose);
  `base_unpack_values`/`table_unpack_values` keep separate callbacks because
  identity is by address; `active_rust_function_name` and snapshot reverse lookup
  are keyed by address.
- `Val::Hash` for `RustFn` hashes the address (ASLR-dependent, layout only; worth a
  comment so nobody "fixes" order with a hash-ordered map). `rust_fn_ids_by_addr`
  is a `BTreeMap<usize, _>` that would iterate in ASLR order; only probed today,
  nothing prevents iteration. `SaveBuilder::bytecode_ids` and `bytecode_caches`
  key by `Arc::as_ptr as usize`, lookup only.
- `register_rust_fn`'s "smallest id wins" for a shared address makes save bytes
  depend on whether the build folded functions (ICF): the same source built with
  and without LTO can write different `Fn` ids. Equality by address is
  script-visible (`f == g`) and differs under ICF; documented in
  `register_rust_fn`, not in `docs/compatibility.md`, `docs/determinism.md` or
  `docs/save-state.md`.

Fix: a `StdFn` tag or registry entry the dispatch matches on, so addresses are a
registry detail; an address-key newtype without `Ord` exposure; refuse to save a
function whose address has more than one registered id; document cross-build
behaviour. Enforceable by type.

## HYG-066 - Process-global state: `NEXT_STATE_ID`, gmatch wrapper `OnceLock`, scratch State in registry checks

Reported by the VM-core and stdlib hunters.

`anchor::NEXT_STATE_ID` is a process-global `AtomicU64`; not replay-visible
(Anchor's Debug is documented non-deterministic), but it makes State creation
order across threads observable through `Anchor`'s `Debug`/`Hash`. Removing `Hash`
from `Anchor`, or a per-State identity without a global, closes it.
`GMATCH_WRAPPER` is a process-wide `OnceLock` compiled lazily on the first gmatch
in the process, whichever State; wrap per State at `open_string`, or drop the
shim (it exists because `RustFunc` cannot carry upvalues).
`check_registry_compatible` builds a scratch `State::empty()` and, per its own
comment, advances the anchor counter as a side effect of a dry run; listing the
stdlib's (id, fn) pairs as data (HYG-007's slices) removes the scratch State.

## HYG-078 - ASCII character classes diverge from locale-following reference

Reported by the stdlib hunter.

`match_class` and `is_lua_whitespace` are fixed ASCII, so `%a` etc. are the same on
every host: a deliberate and correct divergence from reference (which follows
`setlocale`), worth listing in `docs/compatibility.md` if not already. The diff
gate only agrees with reference under a locale where `isalpha` is ASCII-only, so a
host with a Latin-1 locale could produce spurious diff failures on bytes
0x80..0xFF.

## HYG-096 - Exit-code vocabulary is an unstated cross-file assumption

Reported by the tooling hunter.

CLI always exits 1 (documented in `main.rs`; see BUG-006 for 101); the harness
uses 1 and 2; scripts use 1/2 for usage, 124 for timeout, 125 for a bad timeout;
the Python fallback maps signals to `128 - returncode`. `diff_test.sh` treats
dellingr status 124 as TIMEOUT, which holds only because the CLI never exits 124.
A comment in `diff_test.sh` citing `main.rs`'s exit policy would make it explicit.

## HYG-085 - Public error module exposes internal construction; `is_recoverable` has no consumer

Reported by the host-surface hunter.

`error::results_count` (an ABI validator) and `ErrorKind::argument_detail` are
`pub`; `Error::new`/`without_location`/`with_stack_trace` let a host fabricate a
stack trace. Some is needed by `RustFunc` authors; audit and make the rest
`pub(crate)` (`unreachable_pub` does not help since `error` is public).
`Error`/`ErrorKind`/`SyntaxError::is_recoverable` exist for a REPL that does not
exist; only tests call them. Keep with a doc saying it is for host REPLs, or drop.
Docs drift: `lib.rs` crate docs say callbacks "handle errors"; `on_error` is
notification only.

## Decisions for the user

## HYG-045 - Cleanup-on-error and panic-safety decided per site

Reported by the VM-core hunter.

Cleanup: `call_op` has 6 `truncate_call_frame` calls, `eval_closure_frame` 3
`cleanup_closure_frame`, `handle_index_metamethod_inner` and `index_string`
truncate temporaries by hand; a guard or `frame_scope` helper would make "every
exit restores" structural (measure frame bloat on the recursive path before
rejecting; a non-closure struct with plain-store drops adds no landing pad).
Panic safety: `eval.rs` says a State unwound through must be discarded, and the
main path has no guard, yet `with_restricted_env` (whose comment says it exists
"so an embedder that catches the panic and reuses the State" works),
`with_rooted_values` and the vararg path of `eval_closure_inner` use
`catch_unwind`/`resume_unwind`. A host reading `with_restricted_env` concludes
reuse is supported. Fix: a poison flag checked at public entry points (one branch
per host call, not per opcode, which answers `eval.rs`'s hot-path objection), or
remove the three guards. Enforceable by a test that panics in a RustFn under
`catch_unwind` and asserts the next `call` refuses.

## HYG-042 - Rust-function call ABI implemented twice and already diverged on error reporting

Reported by the VM-core hunter.

`eval.rs::call_op` (RustFn branch) and `eval_control.rs::instr_tfor_call_rust_fn`
each set `stack_bottom`, save/restore `active_rust_fn`, call, truncate and
restore on error, normalise the return count (pad nil / `rotate_right`), and
restore `stack_bottom`. `call_op` routes errors through `report_call_error`; the
tfor copy returns them raw and relies on the enclosing frame. Lateral: `call_op`
pads with nils when a RustFn reports more results than it pushed (`Ok(3)` after one
push gives `v, nil, nil`), a leniency not listed in `docs/compatibility.md`
(as far as the hunter checked); list it or make it an error. Fix: one
`invoke_rust_fn(f, call_base, nargs)`. Enforceable: afterwards
`active_rust_fn.replace` appears once.

## HYG-081 - `debug_parser`, `debug_vm` and `debug_gc` print straight to stdout

Reported by the compiler, VM-core and heap hunters.

`parser.rs` `parse_chunk` (`println!("Compiled chunk: {tmp_chunk:#?}")`),
`vm/frame.rs` (`println!("{inst:?}")`), `vm/object.rs::collect` print to stdout
under their features, bypassing `HostCallbacks` and interleaving with
`DefaultCallbacks::on_print`; an embedder's capture sees neither. The `debug_vm`
line omits ip, frame and source; `debug_gc` reports object counts only (no
strings, threshold, freed). Nothing in brokkr.toml, scripts or tests enables
`debug_vm`. Fix: one `trace!` implementation to stderr or a
`HostCallbacks::on_trace`/`debug` hook with ip and `bytecode.name`; or drop them
for hotpath instrumentation and a `--dump-bytecode` CLI flag. Enforceable:
`clippy::print_stdout`/`print_stderr` denied in the library, allowed in `main.rs`,
`examples/hotpath.rs`, the trace macro and the default print.
