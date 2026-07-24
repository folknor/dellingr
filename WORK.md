# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Target #6: `t[k] = nil` during `pairs` traversal

### The bug

Reference Lua explicitly permits clearing the field you are currently sitting
on during a traversal:

```lua
local t = { a = 1, b = 2, c = 3, d = 4, e = 5 }
for k in pairs(t) do t[k] = nil end
print(next(t) == nil)   -- reference: true
```

This is the filter-in-place idiom and it is common in game scripts.

dellingr physically removes the entry, so the control key the iterator carries
no longer exists in the table:

- `Table::remove` (`src/vm/table.rs:381-412`), inline arm: shifts later entries
  down over the hole and decrements `len` (lines 394-397).
- Map arm: `IndexMap::shift_remove` (line 405), which likewise shifts every
  later entry down.

Then `Table::next(&control)` cannot find the control key. **As of the previous
commit** that returns `TableNext::InvalidKey`, and the loop dies with
`invalid key to 'next'`. Before that commit it silently ended the loop early
(leaving `b`..`e` in the table above). So the current failure is loud rather
than silent, but the idiom is still broken, and it is now broken in a way that
kills the callback.

### Why it is not just a `next` bug

The information `next` needs is destroyed by `remove` before `next` is ever
called. Any fix has to change what removal leaves behind, or record where the
removed key used to sit.

### Constraints that shape the fix

- **Determinism is a product requirement.** Iteration order is insertion order
  today and must stay exactly that, including after removals and re-insertions.
  Replays depend on it.
- `Table::next`, `get`, and `insert` are all `#[hotpath::measure]`d and sit
  under `pairs` / field access. The common path must not get slower.
- `Table::version()` guards the inline caches in `eval_index.rs` (6 sites) and
  `eval_store.rs` (3 sites). Anything that changes an existing entry's *index*
  must bump the version, or those caches read the wrong slot.
- `mark_values` (`table.rs:555+`) walks entries for GC. Anything retained
  after removal must not silently keep dead objects reachable forever.
- `array_len` / the border binary search (`table.rs:280-299`) call `get` and
  treat `Val::Nil` as absent.
- `array_insert` / `array_remove` build on `remove` and on index positions.

---

## Agreed implementation plan: tombstones

A removed-key memo was considered and rejected: it only ever fixes whichever
key was removed last, and generalizing it to the full case requires retaining
every removed key plus successor chains, which is a worse tombstone
implementation that still pays for physical deletion.

### Representation

`Val::Nil` in an occupied value slot means "logically absent". This needs no
new `Val` variant, no bitmap, and no change to `TableStorage`: Lua tables never
semantically store nil, `get` already returns a stored nil as absence, and
templated constructors already prepopulate nil-valued entries (`table.rs:86`).

- Add `dead_count: usize` to `Table`. Zero in `Default` and `with_capacity`.
- In `with_template_keys`, initialize it to `key_ids.len()`: those reserved nil
  slots start out logically absent.
- `TableStorage` is unchanged.
- `dead_count` lives on `Table`, not on `TableStorage`, so `promote_to_map` /
  `ensure_map` do not need to carry it across a `mem::take` of `self.storage`.

### Liveness contract on indexed accessors

This is what makes the "no version bump on removal" rule safe, so it is not
optional. Every path that reaches an entry *by index* rather than by key must
hide or refuse dead slots:

- `get_with_index` -> `None` for a dead value. Callers: `eval_index.rs`
  (string-method cache population, direct field lookup, metatable `__index`
  lookup, method-table lookup), `eval_store.rs` (SET_FIELD cache population,
  direct-set lookup).
- `get_index` -> `None` for a dead value. Callers: `eval_index.rs:141, 173,
  298, 320`, `eval_store.rs:303`.
- `set_at_index` -> return `false` if the target slot is dead. Callers:
  `eval_store.rs:316` (version-matched cached write), `eval_store.rs:348`
  (direct indexed write after key lookup). **Without this, a warm SET_FIELD
  cache resurrects a deleted field in place.**
- `entries` (snapshot-only, consumed by `save_state.rs:325`) -> emit live
  entries only. Its `enumerate()` numbers the returned vector, not physical
  slots, so nothing else in the codec changes. Load goes through
  `clear_and_insert_entries`, which reinserts by key.
- `mark_values` -> mark neither key nor value for a dead slot.
- `next` -> the one deliberate exception. It must still *locate* a dead key
  (that is the whole point) but must skip dead slots when producing a pair.

New `init_at_index(index, key, value) -> bool` for templated construction:
validates the reserved key, leaves nil values dead, decrements `dead_count`
when a non-nil value activates the slot. Its sole caller is
`instr_init_field_pinned`, which today reaches for the cache-oriented
`get_index` / `set_at_index` at `eval_store.rs:111, 114` and must be switched
over. It is deliberately not available to caches or general writes.

`get`, `array_len`, `compute_array_len`, `get_array`, `set_array`,
`array_insert`, and `array_remove` are key-based and need no liveness logic;
`get`'s numeric Map shortcut at `table.rs:144` already validates the physical
key and returns nil for a dead value.

### `remove`

- Locate the key as today.
- If the value is already nil, return `None`.
- Otherwise replace **only the value** with `Val::Nil`, increment `dead_count`,
  return the previous value.
- Do not shift entries. Do not bump `version`.
- Keep the existing `cached_array_len` invalidation.

### `insert`

Nil assignment still routes to `remove`. Otherwise:

- `dead_count == 0`: exactly the current update/append path, same hash-probe
  count. This is the hot path and must not regress.
- Updating a **live** existing key: in place, no compaction, no bump.
- Inserting a key that is **distinct from every tombstone**: append. Leave the
  tombstones alone.
- Reinserting a **tombstoned** key: `shift_remove` that one dead slot,
  decrement `dead_count`, bump `version`, append at the back. O(1) when the
  tombstone is already last; the O(N) shift lands here instead of on the
  preceding removal.
- Only when tombstones are dense - deterministic threshold
  `dead_count >= live_count`, evaluated on a non-nil insertion - do a full
  stable compaction, bump `version` once, then append.
- Inline storage may compact whenever space or reinsertion needs it; the work
  is bounded by 4 slots.

**Do not** revive a tombstone in place below the threshold. That is the one
variant that breaks ordering. All three cases above must produce the same live
insertion order as today's eager physical removal:

- Inline: `a, b, c` -> remove and reinsert `b` -> `a, c, b`.
- Map: `a, b, c, d, e` -> remove and reinsert `c` -> `a, b, d, e, c`.

### `compact_dead(&mut self) -> bool`

- Inline: stable-pack live pairs toward index 0, clear the tail, update `len`.
- Map: `retain` non-nil values (`IndexMap::retain` preserves relative order).
- Reset `dead_count` to 0. Do not shrink Map capacity.
- Returns whether it compacted.
- **Does not bump `version` itself** - the caller does. Compaction moves live
  entries' indices, so a caller that compacts without bumping corrupts every
  index cache. Exactly one bump per compact-then-insert.

### Version policy

- Removal: no bump. No live `(key, index)` binding moves, and the liveness
  contract above means no cache can act on a dead slot.
- Live value update: no bump (unchanged).
- Append with no compaction: no bump (unchanged).
- Compaction, or reinsertion of a tombstoned key: exactly one bump.
- `array_insert` / `array_remove`: `compact_dead` first (no bump), then their
  existing single structural bump.
- Template initialization: no bump; reserved indices do not move.

### Compaction timing

Compaction is triggered only by script mutation history - never by GC or
allocation pressure. GC-triggered compaction would make valid iteration depend
on allocation timing, which breaks determinism. Do not compact in `gc_collect`.

Inserting a previously absent field during traversal may compact and invalidate
an old control key. That is acceptable: reference Lua already leaves
add-during-traversal undefined. Pure deletion and value updates stay supported,
which is the case this finding is about.

Add `debug_assert_eq!(self.dead_count, 0)` to `promote_to_map` and
`ensure_map`. Both should only ever be reached with no tombstones present
(inline compacts its bounded 4 slots first, which may avoid promotion
entirely; the array helpers compact before calling `ensure_map`).

### `next`

- Nil control: scan from slot 0 to the first **live** entry.
- Non-nil control: may match a live or a dead key. Continue from its physical
  slot, skipping consecutive dead entries.
- A key that was never present stays `InvalidKey`. NaN stays `InvalidKey`.

### Tests

`src/vm/table.rs` unit tests:

- Removed controls advance correctly, inline and map.
- Multiple adjacent tombstones are skipped.
- Never-present controls still `InvalidKey`.
- Removal leaves `version` unchanged; compaction/reinsert bumps it exactly once.
- `get_with_index` / `get_index` hide dead slots; `set_at_index` refuses to
  resurrect one.
- Reinserted keys move to the back, both storage modes.

New `tests/table_iteration.rs`:

- The exact filter-in-place repro, for a <= 4-entry table and a > 4-entry table.
- Delete the current key *and another key* before calling `next` - the case a
  last-removal memo could not handle.
- Exact insertion order after remove-then-reinsert.
- Traversal with collectable table keys, forcing GC between the deletion and
  the next iterator call.

`tests/metamethod_errors.rs`:

- Warm a read IC, delete the cached field, verify `__index` runs.
- Warm a write IC, delete that field, verify a later assignment goes through
  `__newindex` instead of reviving the dead slot.

`tests/gc_upvalues.rs`:

- Keep the containing table alive, tombstone object keys and object values,
  force GC, assert the object count returns to baseline.

`tests/save_state.rs`:

- Save a table containing tombstones, reload, verify only live entries survive
  and that reinsertion still appends at the back.

Existing invalid-control tests in `tests/error_handling.rs` and templated-nil
tests in `tests/table_constructors.rs` stay as regression coverage.

### Validation

`brokkr check`, then `./diff_test.sh`, then hotbench comparisons on
`iter/pairs`, `fields/same_obj_read`, `fields/same_obj_write`, `tables/fill`.
Expected profile: `get` unchanged, one predictable zero-tombstone branch in
`insert` and `next`, slower scanning only after deletions.
