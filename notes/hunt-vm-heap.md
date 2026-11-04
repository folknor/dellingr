# Hunt: vm-heap

Scope: `src/vm/object.rs`, `src/vm/object/collector.rs`, `src/vm/table.rs`,
`src/vm/table_ops.rs`, `src/vm/raw_field.rs`, `src/vm/lua_val.rs`,
`src/vm/caches.rs`, `src/vm/global_env.rs`, `src/det_hash.rs`,
`src/vm/metamethod.rs`, `src/vm/meta_name.rs`. Followed values into
`eval_index.rs`, `eval_store.rs`, `registers.rs`, `eval_control.rs` (tfor
cursor), `inventory.rs` (roots/cleanup), `vm.rs` (GC policy, global API) and
`compiler/verify.rs` (cache-slot ownership).

Overall: the core is in good shape. The table window/tombstone machinery,
the cache validation rules and the GC root/weak-table protocol held up under
every adversarial sequence I traced. The findings below are one replay-visible
doc/code contradiction, two narrow contract gaps, one stale doc, and some
structural notes.

---

## Findings

### 1. "A cleared global keeps its place in `pairs(_G)`" is false once the sealed table compacts (doc vs code, replay-visible order)

Claims:
- `docs/compatibility.md`, Globals and metatables: "In `pairs(_G)`, a global
  set to nil and later redefined keeps its earlier place, where an ordinary
  table moves a re-added key to the end."
- `docs/limits.md`, State-local growth: "Clearing and redefining the same
  global reuses its slot in every phase."
- `global_env.rs` `slot_policy`: "A global keeps its slot (and its place in
  `pairs(_G)`) however often it is cleared and set, as a binding always has".

Code: after `seal_globals`, the global table's policy is
`revive_in_place: true, retain_dead: false`. A tombstone only revives in
place while its slot exists. Any insertion of a *new* key goes through
`Table::insert_into_map_with_tombstones`, which compacts when
`dead_count >= live_count` (`compact_dead`, which a sealed policy permits).
Compaction removes the cleared global's slot; redefining it then appends at
the back.

Sequence (sealed): `a = 1; b = 1; a = nil; c = 1; a = 2` gives
`pairs(_G)` order `b, c, a` (plus whatever setup installed), not `a, b, c`.
Before sealing (`retain_dead: true`) the claim holds.

The behaviour is deterministic, so replays are not broken. The defect is that
three documents state an unconditional guarantee about replay-visible order
that only holds until the next compaction. Either the docs need the
qualification ("until a later new global compacts the cleared slot, after
`seal_globals`"), or the policy should keep string-keyed tombstones of the
global table for good (which is what `retain_dead` does before sealing, and
what limits.md says sealing exists to stop). That trade-off is the user's
call; the current wording is wrong either way.

### 2. `array_insert` can fail its growth reservation after it has mutated the table

Claims:
- `table.rs` `reserve_array`: "Growth paths call this before touching the
  logical length or removing migrated hash keys, so a refusal leaves the table
  logically unchanged."
- `table.rs` `reserve_outside_window`: "Reserve the storage
  `write_outside_window(key, ..)` will need once the window has grown by
  `extra` pushed slots."
- `docs/save-state.md`: an array growth that cannot be reserved "raises the
  ordinary `table growth failed` error, which kills the callback and leaves
  the table unchanged." `table_ops.rs` `table_insert_at`: "Growth failure
  also leaves the value on the stack" (it does) next to "leaves the stack and
  table unchanged" for its other rejections.

Code: in `Table::array_insert`, the structural case (`len == vlen`) with
`pos <= base` and a live value reserves with
`reserve_outside_window(pos, extra)`, computed from the *current*
`array_live`. It then pushes, rotates and runs `migrate_from_hash`, which
pulls `extra - 1` *live* values in and raises `array_live`. The final
`write_outside_window(pos, value)` calls `reserve_outside_window(pos, 0)`
again with the larger `array_live`, so its downward padding
`pad = min(pos - 1, array_live.div_ceil(2))` can be larger than the one
reserved for. If that second reservation needs more capacity and the
allocator refuses, `GrowthFailed` comes back after `vlen` grew, elements were
shifted up and hash keys were migrated, but before the value was written:
`table.insert(t, pos, v)` half-applied.

Concrete shape: keys 1..100 set, 1..60 cleared, append 101 (drops the dead
prefix, base 60, 41 live), live hash keys 103 and 104, then
`table.insert(t, 50, v)`: reserved pad 21, needed pad 22.

Practical impact is small (only an allocator refusal at exactly that point
exposes it; otherwise the extra growth just succeeds unreserved), but it is a
real hole in the "reserve, then mutate" discipline, and the helper's own doc
comment is what misleads: it accounts for the pushed slots' count but not
that migrated slots are live. Fix by reserving with the post-migration live
count (`array_live + extra - 1` in the structural case), or by reserving
inside a single helper that takes both the pushed count and the live delta.

### 3. Automatic GC threshold that saturates becomes permanently `Disabled`

Claims: `docs/limits.md` GC policy: "After an automatic collection, the next
threshold is `max(live_allocations * growth_factor, minimum_threshold)`,
using saturating multiplication." `vm.rs` `gc_set_policy` doc repeats it.

Code: `GcHeap::collect` recomputes only while the threshold is
`GcThreshold::Automatic`, and builds the new one with
`GcThreshold::automatic(...)`, which maps a value of `usize::MAX` to
`GcThreshold::Disabled`. A saturating product (large `growth_factor`) therefore
switches the State out of automatic mode. Every later collection, explicit
`gc_collect` included, skips the recomputation, so when the live count later
drops the threshold never comes back to `max(live * growth, minimum)`. Only
`gc_set_auto_threshold`/`gc_set_policy` recovers.

The conflation is between "automatic, threshold currently unreachable" and
"automatic collection disabled by the host". Low severity (needs a huge growth
factor), but the documented formula is not what runs. Keep `Automatic(usize::MAX)`
as a distinct state (only `initial_threshold == usize::MAX` and host calls
should produce `Disabled`).

### 4. `docs/determinism.md` "Error labels" is stale (outside scope, found while checking names)

`docs/determinism.md`, Error labels: "Generic Rust callbacks checked with
`State::check_any` or `State::check_type` get a label from the current global
bindings or, with the `snapshot` feature, from the registered function id.
The label can therefore differ between feature sets or builds, and when
globals are rebound."

Since 912c2d5, `State::active_rust_function_name` returns the entry's own
name, fixed at `add_rust_fn`/`register_rust_fn` time ("not discovered from
globals"), and `reference/error-reporting.md` says the same ("Nothing is
discovered at error time. The name does not depend on which globals..."). The
determinism.md bullet list contradicts both and should be rewritten to the
creation-time naming rule.

---

## Smells and latent hazards (not defects today)

- **Silent drop on a vacant cached global slot.** `GlobalEnv::write_cached`
  and `GlobalEnv::write` ignore `Table::write_slot`'s `false` (vacant or
  missing slot) and report the store done. Unreachable today because the
  global table revives in place and never creates `Vacant` keys, and
  `hash_slot_of` never returns one. But a future policy change or a load path
  that admits `(nil, nil)` slots into the global table would turn a global
  store into a no-op with no error. A `debug_assert!` on the return, or making
  `write_slot` infallible for revive-in-place maps, would close it.
- **Release-mode upvalue pool is not defensive.** `UpvaluePool::mark_slot`
  only `debug_assert!`s that a slot is not `Free`. In release, marking a freed
  slot flips it to `Reachable`, the next sweep sets it to `Unmarked` while it
  is still on the free list, and a later sweep can push it a second time: two
  allocations would then share one slot. Only reachable through a root-set
  bug, but the failure mode is silent upvalue aliasing rather than a panic,
  unlike the heap's `get` which fails loudly on a stale key.
- **`GcHeap::get_mut` carries a hidden side effect.** Every mutable access to
  any object compares against `watched` and bumps the global epoch, so the
  builtin mirror is invalidated by `_G.x = 1`, `rawset(_G, ...)` or any table
  library call on `_G`, even for non-builtin keys. Correct, but the coupling
  between a generic accessor and one table's cache lives in `object.rs`, and
  the one bypass (`watched_table_unmarked`) relies on a `builtin` bit cached
  by the caller. A `GlobalEnv`-owned mutation API would make the epoch rule
  local.
- **`Table::rebuild_from_snapshot` is not atomic on error.** It replaces
  `*self` with an empty shell (version reset to 0, metatable cleared) before
  validating array keys and hash keys. Callers are load paths that discard
  the State on failure, so nothing observes it, but the version comment
  ("must move past every value an inline cache could have captured") only
  holds on the success path.
- **Template tombstones are compacted by the first unrelated keyed insert.**
  `Table::with_template_keys` makes every template key a tombstone
  (`dead_count == map.len()`), so the first `INIT_INDEX`/non-pinned insert
  into a template table (live count 0, dead count n) compacts all the
  reserved slots and bumps the version; the later `INIT_FIELD_PINNED` writes
  then miss `init_at_index` and fall back to keyed inserts. Correct result,
  but it defeats the template's purpose for constructors that mix computed
  keys before named ones. Worth checking codegen's emission order.

---

## Structural observations

- **Cache validation is re-implemented at every consumer.** The rule "same
  table, same version, slot live; else re-read the key at the cached index and
  refresh" exists separately in `get_cached_field`, `get_current_cached_field`,
  `cached_method_at`, `get_cached_string_method` and `try_set_field_cached`,
  and the global variant (`Present`/`Absent` + slot count) in
  `GlobalEnv::read_cached`/`write_cached`. Soundness depends on table.rs
  invariants that are only written as prose (which operations bump the
  version, that `Vacant(i)` always sits at index `i`, that inline keys are
  unique). A `Table`-owned typed slot reference (`SlotRef { version, index }`
  with `Table::read_slot(&SlotRef, key)` / `write_slot(..)`) would put the
  invariant and its only consumers in one file and let the five copies
  collapse to calls. Cache entries also do not record their key: correctness
  rests on the verifier's one-site-per-slot rule (`compiler/verify.rs`), which
  is checked, but it is a cross-module dependency worth naming at the cache
  definitions.
- **Per-collection cost scales with code, not garbage.** Every collection
  walks every cache slot of every live runtime (`drop_dead_cache_entries`)
  and walks the whole object slot map a second time
  (`reachable_lua_bytecodes`). With many loaded chunks and small heaps this
  dominates. Marking runtimes reachable during the mark phase (closures
  already visit `closure.runtime`) and pruning only those would fold the
  second walk into the first.
- **The heap is a single non-incremental mark-sweep with an allocation-count
  trigger.** Fine for determinism; just noting that object count, not bytes,
  drives it, so one large table counts as one allocation (documented in
  limits.md).

---

## Checked and found sound

- Table shape invariants under every mutation path: dead-prefix hint (`lead`)
  lower bound through `array_remove`, `array_insert` rotation,
  `write_outside_window` downward growth, `drop_dead_prefix`; `array_live`
  accounting; `dead_count` in inline shift-out, map vacate, revive, template
  init, `write_slot`; `mark_values` skipping `array[..lead]` is safe.
- Fallible growth ordering in `append`/`append_slow`, `set_list` bulk and
  per-key paths, `array_remove` (its padding reservation over-estimates, so
  the second reservation never grows), `write_outside_window` from `insert`.
- `next`/`pairs`: dead controls stay valid until an insertion compacts;
  array controls resume regardless of window moves; tfor cursor revalidates
  by key; vacant keys are never yielded.
- Version bumps cover every slot reorder (inline shift-out, map vacate,
  compaction in insert/migrate, rebuild, environment swap via
  `advance_version_past`); inline-to-map promotion and `set_slot_policy`
  preserve indices without a bump. `Absent(count)` proofs hold.
- Method cache: proven-hit-only, validated by the metatable's `__index` slot
  key and value, so it never changes the answer or the depth units taken.
  String method cache: validated against the live string metatable shape.
- `__index`/`__newindex` resolvers return every depth unit on every error
  path and push their windows only after cost flush and stack preflight.
- GC: roots, weak tables (patterns, `%p` ids) and runtime caches pruned right
  after the sweep before any allocation; string pool index removal never
  rehashes with dead keys; upvalue slots reachable through running closures'
  function slots.
- `det_hash`: no std `HashMap`/`HashSet` anywhere; only `IndexMap`/`IndexSet`
  with the fixed seed and the never-iterated interner index.
- `number_to_string` fast path and `%.14g` emulation (exponent boundaries,
  rounding into the next decade, `-0`, NaN).
- `raw_field` integer range checks, `-0.0`, and the `find_string` shortcut.
