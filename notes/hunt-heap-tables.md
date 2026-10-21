# Hygiene hunt: heap, tables and persistence

Scope: `src/vm/object.rs`, `src/vm/table.rs`, `src/vm/table_ops.rs`,
`src/vm/save_state.rs`, `src/vm/save_state/verify.rs`, `src/vm/rng.rs`,
`src/det_hash.rs`, with excursions into `vm.rs` (GC driver, registration),
`lua_val.rs`, `lua_std/string.rs`, `lua_std.rs`, `frame.rs`.

Findings are unranked. Each says what enforcement the fix could have.
Existing enforcement read first: `clippy.toml` bans unseeded RNG, host
env/time, std hash maps and random hashers; `Cargo.toml [lints]` denies the
usual clippy set plus `unreachable_pub`/`unused`; `src/lib.rs` denies
`unwrap_used`/`float_cmp` outside tests; `brokkr check` adds the gremlins scan
and the `large_source` generator check. Nothing in that set touches the
duplication classes below - every finding here is enforced today only by
comments of the form "keep in step with".

---

## 1. One value, one owner

### 1.1 The "integer key in `1..=n`" rule is spelled at least seven times, in three dialects

The rule that decides whether a number key belongs in the array part:

- `Table::array_slot` (`table.rs`): `n >= 1.0 && n <= len as f64 && n == n.floor()`.
- `Table::is_array_append_key`: `*n == (len + 1) as f64`.
- `Table::rebuild` (snapshot): `n.to_bits() == ((slot + 1) as f64).to_bits()`.
- `runtime_array_slot` (`save_state.rs`): `n >= 1.0 && n <= len && n.fract().to_bits() << 1 == 0`.
- `verify.rs` `validate_environment_deltas`: the same `fract` spelling twice
  (once against `array.len()`, once against `array.len() + 1`), plus a
  `to_bits()` append-key test and a `to_bits()` order test.
- `verify.rs` `validate_references`: `SavedVal::Num(((i + 1) as f64).to_bits())`
  equality twice (slot key, append key).
- `verify.rs` `gmatch_integer_field`: a fourth variant (non-negative integer).

They have already diverged in form: `floor` equality vs `fract` bit test vs
bit-exact equality. The bit-exact form rejects `-0.0`-free spellings only
because array keys are always `>= 1`, so today they agree on every input -
but nothing says they must, and the `floor` form compares a `usize` cast that
the others do not. The verifier is the place where a disagreement becomes a
"valid save rejected" or "forged save accepted into a shape `Table` would
never produce" bug.

Fix: one `fn array_index(n: f64, len: usize) -> Option<usize>` (and
`is_append_key`) owned by `table.rs`, operating on `f64` so the verifier can
call it on `f64::from_bits(bits)`. Enforceable by structure (verifier imports
it, as it already imports `normalize_table_key`) and by a text rule in
`brokkr check` grepping for `fract().to_bits()` outside `table.rs`.

### 1.2 `Val -> TableKeyInput` mapping written three times; NaN/nil key check four times

`normalize_table_key` exists precisely to be the single owner (its doc says
so), but its *input* mapping is re-spelled at each caller:
`Table::insert`, `save_state::runtime_table_key`, `verify::resolve_key`, and
`Val::hash` in `lua_val.rs` (which calls it with `()` payloads and then has to
`panic!`/`unreachable!` on branches the type admits). Independently, the
nil/NaN key refusal is hand-written again in `Table::hash_get`,
`Table::get_with_index`, `Table::next_with_index`,
`Table::next_from_matching_index`, and `State::set_table_raw` (which
duplicates the check `Table::insert` performs one line later, only to get a
located error - see 4.1).

Fix: `Val::table_key(&self) -> Result<NormalizedTableKey<StringPtr, ObjectPtr,
usize>, InvalidTableKey>` on `Val`, and a `SavedVal` twin in the verifier.
Enforceable structurally: make `TableKeyInput` private to `table.rs`.

### 1.3 gmatch iterator ids and state-table field names spelled twice across the crate

`lua_std/string.rs` defines `GMATCH_ITER_ID` / `GMATCH_EMPTY_ITER_ID`, but
`verify.rs` re-spells them as literals `"string.gmatch.iter"` and
`"string.gmatch.empty_iter"`. The state-table field names `"pos"`, `"len"`,
`"s"`, `"p"` are literals in `string.rs` (`"pos"` five times, `"len"` twice)
and again as `b"pos"`, `b"len"`, `b"s"`, `b"p"` in `verify.rs`. The verifier
is therefore a second, unlinked description of the gmatch state layout; a
rename in `string.rs` makes every saved gmatch iterator unloadable (or, worse,
silently unvalidated if the id rename makes `iterator_kind` stay `None`, so
the closure is skipped as "ordinary").

Fix: move the ids and field names into one `gmatch_state` module both sides
import - or, better, stop storing iterator state in a Lua table whose shape the
verifier must reverse-engineer (see 9.4). Enforceable by structure (shared
consts) plus a test that saves a live gmatch iterator and reloads it (one may
exist; the verifier's own tests build payloads by hand with the literals,
which is the duplication again).

### 1.4 Metamethod event names are string literals at every dispatch site

`"__index"` (`metamethod.rs`, `eval_index.rs` twice, `vm.rs`, `basic.rs`),
`"__newindex"` (`metamethod.rs`, `basic.rs`), `"__call"` (`eval.rs`),
`"__tostring"` (`table_ops.rs` twice, once as `b"__tostring"` for
`find_string`, once as `"__tostring"` for `alloc_string`), `"__metatable"`
(`basic.rs` twice), `"__len"` (`table.rs` stdlib loop). There is a `Builtin`
enum for well-known globals but no equivalent for events. A typo compiles and
silently never dispatches.

Fix: a `MetaEvent` enum with `as_bytes()`, and ideally pre-interned event
strings held as roots (the `table_ops.rs` comment that "pre-interning moves
that allocation and can change reuse order" shows the current allocation
point is being preserved as if it were contract - see 7.2). Enforceable by a
text rule banning `"__[a-z]+"` literals outside the enum's module.

### 1.5 Snapshot field limits live in `save_state.rs`, disagree with the limits they shadow, and are not checked at the point of entry

`save_state.rs` defines `MAX_ENGINE_VERSION_BYTES`, `MAX_SAVE_IDENTIFIER_BYTES`
(4 KiB), `MAX_ENVIRONMENT_TOKEN_BYTES`, `MAX_CHUNK_NAME_BYTES` (4 KiB),
`MAX_CHUNK_SOURCE_BYTES` (256 KiB), plus diagnostic-rendering caps.
`limits.rs` says it is the home of "fixed resource and encoding limits" and
that "the snapshot format and verifier validate against these values
directly" - but these do not live there, so the stated set of limits is
incomplete.

More importantly, nothing upstream enforces them:

- `register_rust_fn` accepts an id of any length; an id over 4 KiB registers
  fine and makes every later `save_state` fail with
  `TooLarge { what: "function id" }`.
- `load_string`/compile accept a source name of any length; `Bytecode.source`
  over 256 KiB makes saves fail with `TooLarge { "chunk source" }`.
- `Bytecode.name` (function name, derived from source identifiers, bounded
  only by `MAX_SOURCE_BYTES` = 16 MiB) over 4 KiB makes saves fail with
  `TooLarge { "chunk name" }`. A script can therefore make its own host unable
  to save by writing `function aaaa...aaaa() end`.

These are refusals hours later on the save path instead of at registration or
compile. Fix: move all codec limits into `limits.rs`; enforce id length in
`register_rust_fn`, source-name length in the compile entry points, and either
cap or truncate `Bytecode.name` in the parser (it is diagnostic only).
Enforceable by test: compile/register at limit+1 must fail, and a property
that anything compilable is savable.

### 1.6 `MAX_PATH_GLOBAL_BYTES` truncation applied twice per segment

`SaveBuilder::push_path` truncates every `PathSegment::Global` to
`MAX_PATH_GLOBAL_BYTES`, and all eight call sites (`finish`,
`encode_env_deltas` x7) already call `truncate_utf8(.., MAX_PATH_GLOBAL_BYTES)`
before constructing the segment. One of the two is dead; the call-site copies
are the ones to delete. Enforceable structurally: `PathSegment::global(&str)`
constructor, private variant field.

### 1.7 The save-time "in-flight State" inventory and the load-time reset are two hand-maintained lists

`State::validate_quiescent` enumerates 12 fields that must be empty/zero;
`materialize_payload` ends by clearing 14 (it clears
`transient_roots.values` and `transient_roots.suspended_envs` separately where
the validator calls `transient_roots.is_empty()`). The validator's comment
says "keep the full snapshot-only inventory here ... whenever adding new
in-flight State data", i.e. the invariant is maintained by memory across two
functions in two files (and `State::new`, a third writer). A new in-flight
field added to `State` and to neither list is a save that captures a
half-call, or a load that inherits setup's leftovers.

Fix: group the in-flight fields into one `InFlight` struct on `State` with
`is_quiescent()` and `reset()`; both sites call the struct. Enforceable by the
type: a field added to `InFlight` is automatically covered, and a destructuring
`let InFlight { a, b, .. }` without `..` in `is_quiescent` makes forgetting a
compile error.

### 1.8 `SavedBytecode` mirrors `Bytecode` field-for-field, four times

`SavedBytecode` (struct), the `Bytecode -> SavedBytecode` conversion in
`encode_bytecode`, the placeholder literal pushed before recursion (all 15
fields spelled out as zeros), `build_bytecode_node` (the reverse), and the
`BytecodeView` impl in `verify.rs`. `SavedUpvalueDesc` likewise mirrors
`UpvalueDesc`. Adding a field to `Bytecode` compiles fine everywhere except
where a struct literal is exhaustive - the placeholder and the two
conversions catch it, `BytecodeView` does not. Fix: have the codec
encode/decode `Bytecode` directly (raw `u32` code is already what `Instr`
holds; number literals go through `saved_num_bits`), and make verify run on a
decoded `Bytecode` through the same `BytecodeView` the compiler-side verifier
already uses. That deletes `SavedBytecode` and its three conversions.
Enforceable by structure.

### 1.9 `retype_load_error` restates every `LoadError` variant field by field

It exists only to change the phantom `E` parameter. Exhaustiveness makes it
safe today, but it is one more site per new variant. Fix: split
`LoadError<E>` into `LoadError::Setup(E)` plus `LoadError::Snapshot(SnapshotError)`
so decode/verify/materialize return `SnapshotError` and lift with one `From`.
Enforceable by type.

### 1.10 Stdlib capacity constants are hand counts

`lua_std.rs`: `MATH_LIBRARY_CAPACITY = 22`, `STRING_LIBRARY_CAPACITY = 10`;
`vm.rs`: `STDLIB_HEAP_TABLES = 6`, `STDLIB_INTERNED_TABLE_KEYS = 41`,
`STDLIB_NON_BUILTIN_GLOBALS = 1`. `TABLE_LIBRARY_CAPACITY` is derived from
`TABLE_FUNCTIONS.len()` - the right pattern, applied to one library of three.
I did not recount the others, so whether they have drifted is unverified;
drift is only a performance effect. Fix: derive all from registration arrays,
or a debug assertion at the end of `open_libs` comparing `heap.object_count()`
/ `string_count()` with the constants. Enforceable by that assertion in
tests.

### 1.11 Default RNG seed `0` stated in two places

`vm.rs` constructs `VmRng::seed_from_u64(0)` and AGENTS.md/docs state
"default seed 0". Harmless; a `VmRng::DEFAULT_SEED` const would make the doc
claim checkable. `seed_from_u64` and `from_state` are also byte-identical
constructors distinguished only by name and a feature gate.

---

## 2. Values nobody can find, change, or trust

### 2.1 GC policy constants live in `object.rs`, unlisted, with a test-only injection point

`GC_MIN_THRESHOLD = 20`, `GC_INITIAL_THRESHOLD`, `GC_THRESHOLD_GROWTH_FACTOR =
2` sit at the top of the heap module; `limits.rs` (the module that claims to
answer "what are the tunables") does not mention them, and no doc in `docs/`
describes collection policy at all (`gc_set_threshold`, `gc_disable_auto` are
documented only in rustdoc). `GcHeap::with_threshold` is `#[cfg(test)]`, so
the policy is injectable for heap unit tests but not for `State`.

Consequences worth knowing:

- The initial threshold (20) is below the stdlib's own footprint (6 tables +
  41 interned keys), so every `State::new` runs automatic collections during
  `open_libs`. Harmless, but surprising, and it makes construction cost depend
  on the policy constant.
- Host-set thresholds are one-shot: every `collect` overwrites `Automatic(_)`
  with `survivors * 2`. `gc_set_threshold`'s rustdoc says so, but see 2.2.

### 2.2 `load_state` silently discards the host's GC configuration

`load_state` runs the host `setup` closure, then `materialize_payload`, which
ends with `state.gc_collect()`. If setup called `gc_set_threshold(N)`, that
collection replaces `Automatic(N)` with `survivors * 2`; only `Disabled`
survives. The threshold is also not part of the snapshot, so a host cannot
restore it except by setting it again *after* `load_state` returns. Nothing
says this. Fix: document it in `docs/save-state.md`, or make GC policy an
explicit `State` config (initial threshold, growth factor, min) that
collection reads rather than overwrites. Enforceable by a test: set a
threshold in setup, load, assert it.

### 2.3 `Encoder::enforce_field_limits` has one production value

`with_capacity` sets `true`; the only `false` constructor is `#[cfg(test)]
Encoder::new`. Tests therefore encode payloads production would refuse, which
lets the decoder's limit checks be tested - but it also means the encoder's
own limit enforcement is never what those tests exercise. Better: tests build
raw bytes for oversized-field decoding, and the flag goes. Enforceable by
deletion.

---

## 3. One channel, one implementation

### 3.1 `debug_gc` prints straight to stdout from library code

`GcHeap::collect` under `feature = "debug_gc"` does `println!`, bypassing
`HostCallbacks` (which owns `on_print`). In an embedded game host stdout may
be nothing, or the game's own console. It also reports object counts only,
not strings, threshold, or freed counts, so it cannot answer "why did this
collection run". Fix: route through a debug hook on `HostCallbacks`, or emit
via the same mechanism the hotpath harness uses for counters. Enforceable by
`clippy::print_stdout` denied for the library (allowed in `main.rs` and
examples), which would also catch the `debug_parser`/`debug_vm` siblings.

### 3.2 Snapshot diagnostics are good; index context is not uniform

`LoadError::CorruptArena { reason, index }` names an index, but which arena
the index belongs to varies by site and is not recorded: in
`validate_environment_deltas`, duplicate keys report `key_index` with no
delta index; the order checks report `delta_index` for some failures and
`key_index` for others; `validate_metatable` reports `0` when the value is
neither `Obj` nor `EnvObj` (index meaningless). `materialize_payload` does the
same (`key_index` vs `delta_index` for the same delta). An operator reading
"corrupt save arena at index 3: duplicate deleted environment key" cannot tell
which table. Fix: make `CorruptArena` carry a typed location
(`Object(u32) | EnvDelta { token, entry } | Global(name) | ...`). Enforceable
by type.

---

## 4. Errors

### 4.1 `Table` errors carry no location

`Table::insert` returns `Error::new(TypeError::TableKeyNil, 0, 0)` /
`TableKeyNan` with line 0, column 0, no source; `Table::rebuild` returns
`InternalError` the same way. Callers that propagate with `?`
(`set_table_str_key_value`, `set_list`, and the dispatch paths through
`table_ops`) hand the operator an error that names no line. `set_table_raw`
works around this by pre-checking the key itself (1.2) so it can call
`self.type_error`. Fix: `Table` returns `InvalidTableKey` (its own small
enum, which already exists) and the `State` layer converts with location. That
also removes `Error`/`ErrorKind` as a dependency of the table module.
Enforceable structurally: `table.rs` cannot import `Error`.

### 4.2 Materialization discards the underlying reason

`rebuild_from_snapshot(..).map_err(|_| corrupt_arena("saved table entries are
invalid", idx))` and the five `table.insert(..).map_err(|_| ..)` sites in the
env-delta path throw away which rule failed (wrong array key, hash entry
belongs in array, nil key, NaN key). With 4.1's typed error these could carry
the reason through.

### 4.3 Process aborts on invariant paths that a host can reach

- `Val::hash` `panic!`s on a NaN number. All current callers filter NaN first
  (the comment enumerates them), so this is an invariant, but it is a panic in
  a `Hash` impl, i.e. reachable from any future `IndexMap<Val, _>` user who
  did not read that comment. `format_pointer_id` only `debug_assert!`s its
  input. A `TableKey` newtype that can only be built through
  `normalize_table_key` and is the only thing that implements `Hash` would make
  the bad input unrepresentable.
- `GcHeap::get`/`get_mut`/`StringPool::get` `expect` on a stale pointer. That
  is the intended use-after-free tripwire, but `table.rs`'s tombstone note
  ("the key of a dead slot may dangle ... must never dereference it") means
  any new raw-slot walker that forgets `filter` aborts the host. The note is
  the only enforcement.
- `Table::array_insert` and `set_array` `expect` on insert; fine, integer keys
  cannot fail.

### 4.4 `table_len` swallows a bad index as `0`

Documented as crate-internal with callers that pre-validate, but
`table_insert_at` / `table_remove_at` call it with the *original* `table_idx`
after `table_operand_below_top` validated it - two resolutions of the same
index, one of which may silently return 0. Pass the resolved `idx` instead and
drop the fallback. Enforceable by signature (`table_len(resolved: usize)`).

---

## 5. Policy invented per call site

### 5.1 Duplicate-key and disjointness validation runs twice, on two key representations

The verifier checks duplicate saved keys, deleted/upserted disjointness, and
order-contains-upserts using `resolve_key` on `SavedVal`s.
`materialize_payload` repeats every one of those checks with
`insert_runtime_key` on runtime `Val`s. The second pass is genuinely needed -
two different `Fn` ids may resolve to one address (aliasing/ICF, which
`register_rust_fn` explicitly allows), and two env tokens could resolve to one
object - so this is a forced duplicate. But the reason is not written down at
either site, and the two copies use different reason strings ("duplicate
deleted environment key" vs "duplicate materialized deleted key"). Fix: keep
only the runtime check (it is strictly stronger) for env deltas and pointer
ids, or record why both exist. If both stay, a shared helper parameterised by
the key type keeps them in step.

### 5.2 "Allocate without collecting" is a per-call-site convention

Every `GcHeap::alloc_*` carries the same four-line doc ("Caller must check
`is_full()` ... except snapshot materialization"), and `State::alloc_table`,
`alloc_table_with_capacity`, `new_table_with_template` each call
`maybe_collect()` by hand. `object.rs` explains why there is no token
(the root set lives in `State`), but the result is that the collector's
safety rests on every new allocation site remembering one of two rules.
Structural fix: `GcHeap` allocators become private to a `State`-level
allocator that always collects, plus a separate `NoGcAllocator<'a>` borrowed
from `&mut GcHeap` that only `materialize_payload` can construct (lifetime
proves no `State` access during its use). Enforceable by visibility.

### 5.3 `GcHeap::collect` is reachable without the rest of the collection protocol

A full collection in `State::gc_collect` is: clear gmatch cache, mark roots,
mark open-upvalue slots, drain, sweep bytecode caches, sweep heap, sweep
upvalue pool, prune `format_pointer_ids`. `GcHeap::collect` is `pub(super)`
and its comment says `gc_collect` "is the only production caller" - an
invariant held by comment. `mark_gc_roots` is documented as "the single source
of truth for what constitutes a GC root", but open upvalue slots and
`pending_bytecode_caches` are rooted in `gc_collect`, not there. Fix: make
`gc_collect` the only path (heap `collect` private to a `collector` module
that also owns the side sweeps), and fold the open-upvalue and pending-cache
roots into `mark_gc_roots` so the "single source" claim is true.

---

## 6. Ambient dependencies

Clean within scope: no clock, environment, or OS randomness. Two softer
items:

- `Val::RustFn` hashes by function address and `rust_fn_ids_by_addr` /
  `SaveBuilder::bytecode_ids` / `bytecode_caches` key by
  `Arc::as_ptr as usize`. `det_hash.rs` correctly says this affects only
  layout, never order, and all those maps are used for lookup only. Holds
  today; the guarantee rests on nobody iterating them. `BTreeMap<usize, _>`
  keyed by address *would* iterate in ASLR order - `rust_fn_ids_by_addr` is
  exactly that type. It is only probed, but nothing prevents iteration.
  A newtype key without `Ord` exposure (or a lint-like test) would.
- `register_rust_fn`'s "smallest id wins" rule makes save bytes depend on
  whether the release build folded two functions (ICF). The comment admits it.
  This is host-build-dependent replay data; see 10.2.

---

## 7. Shared state whose safety is not structural

### 7.1 `Table` invariants are kept by many writers that each re-derive `dead_count` and `version`

`dead_count` is adjusted by `insert` (inline branch), `hash_remove`,
`init_at_index` (both directions, with a comment about forged bytecode),
`compact_dead`, `with_template_keys`; `version` is bumped by `bump_version`
and, in `insert_into_map_with_tombstones`, by a hand-inlined
`self.version.set(..)` because of a borrow. `rebuild` resets it to a computed
`next_version`. The shape rules in the `Table` doc comment are the contract
and nothing checks them. Fix: a `debug_assert_invariants()` (dead_count equals
the count of nil slots; no live hash entry for keys `1..=array.len()+1`)
called at the end of every mutator under `cfg(debug_assertions)`; cheap to
add, and it turns the doc comment into a checked one, just as
`drain_mark_worklist`'s assertion did for `mark`.

### 7.2 GC schedule and string-slot reuse are being preserved as if they were replay contract

Comments in `vm.rs` (`sweep_bytecode_caches`: "would ... change string-slot
reuse and future collection thresholds") and `table_ops.rs`
(`metatable_tostring_field`: "Pre-interning moves that allocation and can
change reuse order") treat allocation order and collection timing as
something that must not change. I found nothing in scope that makes either
observable: `pairs` order is insertion-based, pointer identities are
counters, snapshot ids are first-encounter order, and `ObjectPtr`-keyed
`BTreeMap`s are only probed or re-keyed by token. If they are contract, it
belongs in `docs/determinism.md` with the reason; if not, these comments are
freezing code for nothing (and blocking 1.4's pre-interning). Worth an
explicit decision.

---

## 8. Unbounded growth

### 8.1 The array part never shrinks, so queue-style tables grow with history

`Table` doc: "The array never shrinks outside a snapshot rebuild." A FIFO
written the obvious Lua way (`q[tail] = v; tail = tail + 1` / `q[head] = nil;
head = head + 1`) keeps appending at `array.len() + 1` while clearing the
front, so the `Vec<Val>` grows by one slot per push forever. Effects:

- Memory: 16 bytes per historical push, uncharged.
- Host time: `next_live_from(Position::Array(0))` scans the dead prefix, so
  every `pairs(q)`/`next(q)` start is O(total pushes). Charge is flat, so this
  is host work out of proportion to its charge (`docs/budget.md` calls that a
  performance concern).
- Snapshots: `snapshot_entries` emits one `(key, Nil)` entry per array slot,
  so save size grows with the queue's history, not its contents, and the
  verifier and materializer walk all of them.

The no-shrink rule exists so `pairs` can assign nil to visited keys and
caches keep slot indices. A shrink at GC time or at snapshot (trim trailing
nils, or rebase a dead prefix into the hash part when it exceeds half the
array) preserves both if done only when no traversal can be mid-flight -
which is exactly true at save time. At minimum the snapshot could save only
up to the last live slot plus the `#t`-relevant shape, though `#` semantics
on trailing nils would need care (it is replay-visible, so any change must be
called out).

### 8.2 `SaveBuilder::breadcrumbs` holds one path node per encoded value

Every key, value, metatable and upvalue pushes a `Breadcrumb` (with a
`String` for globals), purely so `UnregisteredFunction` can render a path.
For a large world that is a second copy of the graph's size held for the
whole save. `encode_env_deltas` additionally pushes a fresh root breadcrumb
(with a newly truncated copy of the token) for every single key and value of
every env table. Fix: record nothing on the happy path; on failure, re-walk
from the roots to find the first path to the offending function. Errors are
rare, saves are frequent.

### 8.3 `format_pointer_ids` is bounded by live values, but only at GC

Pruned in `gc_collect`; with automatic GC disabled (`gc_disable_auto`) and a
script that `tostring`s fresh tables in a loop, it grows with allocation, as
does the heap itself. Proportional, not a leak - noted for completeness.

### 8.4 Upvalue pool `slots`/`colors` never shrink

Free-listed and reused, so bounded by peak, not total. Fine; noted.

---

## 9. Code that is no longer load-bearing

### 9.1 Call-site `truncate_utf8` before `PathSegment::Global` (see 1.6)

Dead because `push_path` truncates again. Evidence: `push_path` is the only
consumer of `PathSegment` values constructed in `finish`/`encode_env_deltas`.

### 9.2 `StringPtr`'s `Display` impl

Prints a process-local slotmap key ("string: StringKey(..)"). I found no
formatting of a bare `StringPtr` in the scope's code; if nothing else uses it,
it is dead and, worse, a non-deterministic rendering waiting to be used in a
message. Verify with a removal + `brokkr check`.

### 9.3 `GC_INITIAL_THRESHOLD`

Defined as `= GC_MIN_THRESHOLD` and used once. A second name for one value,
suggesting a past where they differed. Either give it a real role (2.1) or
drop it.

### 9.4 `Table::clear_and_insert_entries` has one production caller

Used once in `materialize_payload` (env reorder with empty array part) and in
tests; its sibling `rebuild_from_snapshot` covers the general case and the
same call site already falls back to it when the array is non-empty. Merging
the two branches deletes `clear_and_insert_entries` and the `exact_shape`
flag.

### 9.5 `BTreeMap<_, ()>` used as sets

`SaveBuilder::reachable_pointer_values`, `expanded_environment_objects`,
`encode_env_deltas`' `live_keys: DetMap<Val, ()>`. Not dead, but `BTreeSet`
says the same thing. `det_hash` offers no `IndexSet` alias, which pushes code
toward `IndexMap<K, ()>`; add one.

---

## 10. Where production code assumes it runs

### 10.1 Two hash functions, one documented as "the only one"

`det_hash.rs` calls itself "the crate's only hash-map type" (true) and fixes
foldhash's seed. But `StringPool` has its own hand-rolled FxHash
(`hash_string`, pinned by a test) and then stores those `u64`s in an
`IndexMap<u64, _>` that foldhash hashes again. Two hash functions for one
lookup; the Fx one is endian-defined (`from_le_bytes`), foldhash's is not
(`det_hash` says it may change per target). Neither reaches observable order
today. Fix: either key the string index by `Box<[u8]>` content directly with
foldhash (one hash), or use a no-op `BuildHasher` for the `u64` map. If the
pinned Fx hash has no consumer that needs pinning, drop the pin test's claim
of stability.

### 10.2 Save bytes depend on the producing binary's code layout

Through `register_rust_fn`'s smallest-id-for-shared-address rule, two builds
of the same source (LTO vs not) can write different `Fn` ids for the same
logical state. The format already accepts cross-version loads only with
exact `FORMAT_VERSION`, but nothing tells a host that the same version, built
differently, can produce different bytes or different `==` behaviour after
load. Worth a line in `docs/save-state.md`; structurally, refusing to
serialise a function whose address has more than one registered id (instead of
silently choosing one) would turn a host build quirk into a refusal at save.

### 10.3 `Val::RustFn` identity across processes

Hashing by address is layout-only (documented). Equality by address is
script-visible (`f == g`) and differs under ICF - documented in
`register_rust_fn`, not in `docs/compatibility.md` or `docs/determinism.md`.

### 10.4 No writes outside owned locations in scope

The scope does no filesystem I/O. `save_state` returns bytes; the host owns
persistence. Nothing to report.

---

## Lateral findings

- **`OP_NEW_TABLE_PRESIZED` sizes only the hash part.** `frame.rs` passes the
  instruction's `A` to `new_table_with_capacity`, and `Table::with_capacity`
  allocates an `IndexMap` of that capacity; the array part is never presized.
  If `A` counts positional items (I did not confirm what the parser stores),
  `{1, 2, ..., 10}` allocates a 10-slot hash map it never uses and grows the
  array by doubling. Worth checking what the parser emits.
- **`next_live_from` dead-prefix scan** (8.1) also affects plain tables that
  had their low keys cleared, e.g. `t[1] = nil` on a 100k-element array makes
  every `pairs` start skip 1 slot - cheap - but clearing the first half makes
  it skip 50k.
- **`VmRng` is `Copy`.** Copying replay-critical RNG state by accident (a
  `let rng = state.rng;` followed by draws) silently forks the stream. Dropping
  `Copy`/`Clone` (keeping `state()`/`from_state()` for snapshots) makes that
  impossible. Enforceable by type.
- **`Table::with_template_keys` comment** says duplicate keys "would otherwise
  leave phantom tombstones" - the defensive count is right, but a duplicate
  template key reaching runtime means the bytecode verifier is wrong; a
  `debug_assert_eq!(map.len(), key_ids.len())` would surface it rather than
  absorb it.
- **`SavePayload::encoded_size_estimate`** ignores bytecode size, which for
  script-heavy worlds dominates; minor, the buffer still grows.
- **`string_tostring_handler`** uses `find_string(b"__tostring")` (no alloc)
  while `metatable_tostring_field` uses `alloc_string("__tostring")` (allocates,
  may GC). Same lookup, two allocation policies, chosen per call site; the
  comment justifies the second by preserving allocation order (7.2).
