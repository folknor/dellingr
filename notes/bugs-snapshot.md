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

# Snapshot defects: save, load, identity, replay

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `snapshot` and
`tables`). The hunters read the code statically and built or ran nothing, so
every repro here is traced by hand, not executed. The `snapshot` hunter found
rng.rs clean: the SplitMix64 step, the 53-bit `random_f64` scale and the
rejection threshold are correct, and `math.random` validates and charges before
drawing.

## SNP-001 - A cleared hash key can resume `next` in a continuous run but not after save/load (replay divergence)

From the `tables` hunter.

**Claims broken**

- docs/determinism.md "`pairs` order": "Snapshots preserve it, so a loaded state
  continues with the same order".
- docs/save-state.md "Byte stability": "A loaded State and the continuous run it
  came from keep saving identical bytes".
- AGENTS.md lists snapshot contents and `pairs` order as replay-visible.
- Lua's own `next` contract: "you may clear existing fields" during a traversal.
  dellingr implements that with hash tombstones: `Table::next_with_index` and
  `next_from_matching_index` accept a dead slot as the control key ("A dead
  control slot remains valid, matching Lua's `next` behaviour here").

**Mechanism.** A cleared hash entry stays in the table as a tombstone
(`hash_remove`). `next(t, k)` for that cleared `k` finds the tombstone's index
and continues from it. Snapshot save keeps only live entries
(`snapshot_entries` -> `live_hash_entries`), and `rebuild_from_snapshot`
re-inserts only those. The loaded table therefore has no tombstones, and
`next(t, k)` for the same cleared key returns `InvalidKey` ("invalid key to
'next'"). The callback dies, while the continuous run goes on. Nothing compacts
tombstones between callbacks unless a new key is inserted, which is already
undefined during a traversal. A pure "clear as you go" cursor therefore works
indefinitely in the continuous run and breaks on the first resume after a load.

This is legal Lua and a natural pattern for budgeted per-tick game work:

```lua
work = {}  -- string keys, so hash part
for i = 1, 10 do work["job" .. i] = i end
cursor = nil
function tick()                -- one call per frame
  local k = next(work, cursor) -- continuous run: fine, tombstone resumes
  if k == nil then cursor = nil return end
  work[k] = nil                -- clearing an existing field: allowed
  cursor = k                   -- cursor key held in a global, so it stays alive
end
-- save after a tick, load, tick() -> "invalid key to 'next'"
```

**Template variant.** `{a = nil, b = 1}` creates a dead template slot for `a`
(`with_template_keys` pre-inserts every key as nil). In the continuous run
`next(t, "a")` returns `("b", 1)`; after a load it raises. Reference 5.4 also
raises here, because it never inserts a nil-valued key, so the continuous run is
the lenient one and only the snapshot boundary exposes it.

**Secondary effect.** Tombstone keys are not marked (`mark_values` marks only
live slots). Whether a cleared key whose only reference was the tombstone can
still resume a traversal therefore depends on whether a collection freed and
re-interned it, which ties the result to collection timing. A loaded State's
collection timing differs from the continuous run's. See also SNP-002.

**Fix options**

- Make the dead slots part of the saved shape, the way environment deltas
  already save the whole logical array. Save the hash part as slots with a dead
  marker, so the loaded table has the same tombstones and the same compaction
  schedule. Canonical bytes would then depend on history the same way the array
  length already does. save-state.md's "never where its stored window happens to
  sit" applies to the window, not to tombstones, so the doc would need a
  sentence on this.
- Or decide that a traversal cursor naming a cleared key does not survive a
  save: say so in save-state.md ("Not saved: ...") and compatibility.md, and make
  the continuous run refuse it too, so both runs agree. That means either never
  resuming from a tombstone, which breaks the `for k in pairs(t) do t[k] = nil
  end` idiom unless the tfor cursor alone carries it, or clearing tombstones at a
  deterministic, save-aligned point.

The hunter favours the first option, as the one that keeps both Lua's `next`
contract and the replay contract.

## SNP-002 - Tombstone keys are a weak reference that collection never prunes

From the `tables` hunter. Lower severity; practically unreachable.

**Claim at issue.** `val_is_live` in src/vm/object.rs notes that handles kept
across allocations "can alias one, since generations wrap; that is why weak side
tables ... are pruned with this at every collection". A tombstone's key is
exactly such a weak handle (the `TableStorage` doc: "the key of a dead ... slot
may dangle"), and it is never pruned.

**Mechanism.** After a slot's generation wraps, a new object can compare equal
to a dead tombstone key, and `next(t, newobj)` would then resume the traversal
instead of raising "invalid key to 'next'". The `insert` and `get` paths stay
correct, because re-add semantics apply. It takes 2^31 reuses of one arena slot.

**Direction.** It is the one weak table the stated pruning rule does not cover.
If SNP-001 is fixed by saving tombstones, decide at the same time whether
tombstones keep their keys alive (mark them) or are pruned at collection.

## SNP-003 - Rendered identities of environment objects are dropped though the objects survive the load

From the `snapshot` hunter.

**Claims broken**

- docs/determinism.md "Object identity in output": `tostring` and `%p`
  identities are "stable across a snapshot round trip".
- docs/save-state.md "What is saved": "State-local object identities that
  `tostring` and `%p` have rendered".
- The save-side comment in `SaveBuilder::finish` justifies the filter with "A
  heap value unreachable from saved state cannot survive the load", which is
  false for environment objects.

**Mechanism.** `SaveBuilder::finish` keeps a `format_pointer_ids` entry only if
`saved_reachable_pointer_value` resolves it and the value is in
`reachable_pointer_values` (the `Fn` case is exempt). `reachable_pointer_values`
is filled only by the value walk rooted at globals and environment deltas, so an
`EnvObj` is in it only if some saved value points at it.

Environment objects are rebuilt by `open_libs` on every standard load, so they
always survive, and some are reachable by scripts without any global:

- The shared string metatable (`string.metatable` token). `getmetatable("")`
  returns it, and it has no `__metatable` protection (`install_string_metatable`
  in src/vm.rs; `raw_metafield` in basic.rs). No global or table refers to it, so
  the walk never reaches it.
- The `string` library table once the `string` global is shadowed or deleted. It
  is still reachable as `getmetatable("").__index`.

**Repro sketch.** `print(getmetatable(""))` prints `table: 0x1`. Save, load, and
run `print(getmetatable(""))` again. The loaded State hands out a fresh id
(`next_format_pointer_id` was restored, but the entry was not), while the
continuous run prints `0x1`. That is a replay-visible divergence between a
loaded State and the run it came from. In the live State the entry is never
pruned, because the string metatable is a GC root (`mark_gc_roots`, vm.rs), so
the save is the only place the identity is lost.

The same gap covers strings reachable only through an environment object the
walk did not expand: for example the keys of the `string` table once `string` is
deleted, enumerated through `pairs(getmetatable("").__index)` and rendered with
`%p`. The identity-string loop covers meta names and literals of saved chunks,
but not environment-table keys or values.

**Direction.** The set that survives the load is everything `open_libs` rebuilds
plus the saved graph, not the saved graph alone. Seed the reachability walk with
every `env_tokens` object, expanded the same way as the existing `Discard`
expansion. Simpler still: keep every `EnvObj` and every string or function
reachable from an environment object. Tests should pin `getmetatable("")` and a
shadowed `string` table across a round trip.

## SNP-004 - Identity strings get arena ids in heap-address order (byte stability)

From the `snapshot` hunter.

**Claim broken.** docs/save-state.md "Byte stability": "Two saves of the same
logical state are byte-identical" and "A loaded State and the continuous run it
came from keep saving identical bytes."

**Mechanism.** In `SaveBuilder::finish`, after the graph walk:

```rust
for key in self.bytecode_ids.keys() {
    if let Some(entry) = self.state.bytecode_caches.get(key) {
        identity_strings.extend(entry.runtime.literals.iter().copied());
    }
}
```

`bytecode_ids` is a `BTreeMap<usize, u32>` keyed by
`Arc::as_ptr(bytecode) as usize`, so this loop runs in heap-address order. Each
identity-bearing literal the graph walk has not already reached gets its
string-arena id from `string_id` in that order. Those ids appear in the
`strings` section and in `format_pointer_ids` (`SavedVal::Str(id)`).

Two saved chunks are enough. Each needs a string literal that was rendered with
`%p` (for example `string.format("%p", "abc")` in `f` and
`string.format("%p", "xyz")` in `g`) and is not otherwise stored. The order of
those two strings in the arena then follows the allocator's placement of the two
`Bytecode` Arcs. Within one process history the order is generally the same. It
differs between the continuous run, where codegen allocates nested chunks, and a
loaded State, where `build_bytecode_tree` allocates chunks in arena order. It
can also differ across allocators and hosts.

**Direction.** Iterate chunks in arena-id order: walk `self.bytecode` by index,
or sort `bytecode_ids` by value. More structurally, nothing in the encoder should
iterate an address-keyed map. `bytecode_ids` is the only one iterated today
(`object_ids` is keyed by slotmap key, and `upvalue_ids` and `string_ids` are
lookups only). Make that a rule, or key the map by arena id.

## SNP-005 - A forged logical array length turns the next append into a host abort or panic

From the `snapshot` hunter.

**Claims broken**

- docs/limits.md "State-local growth": for the logical array length cap,
  "growing past it panics, and a save beyond it is rejected on load".
- The `MAX_ARRAY_LEN` doc in table.rs: "Snapshot loading rejects a larger length
  rather than panicking."
- docs/save-state.md treats saves as user-editable input.

**Mechanism.** `validate_references` (verify.rs) bounds a saved table's
`array_len` only by `MAX_ARRAY_LEN` (2^53 - 2), independently of `array_slots`.
`rebuild_from_snapshot` then builds a table with `vlen = array_len` and a window
holding only the stored span. Live execution reaches a large `vlen` only by
paying for that many appends, and paying in storage unless a FIFO drained them.
A save of a few dozen bytes reaches it for free:

- `array_len = 2^52`, `array_slots = 1` (key 1 live). Any write to key
  `2^52 + 1` takes the append path. `prepare_append` sees
  `base + array.len() < vlen` and calls
  `self.array.resize(self.vlen - self.base, Val::Nil)`: about 2^52 x 16 bytes.
  The allocator fails and Rust aborts the process (`handle_alloc_error`). The
  host gets no error.
- `array_len = MAX_ARRAY_LEN` with an empty window. The append reaches
  `grow_vlen`, whose `assert!` panics.

The forged save can carry the triggering bytecode itself, and the cost budget
does not charge the resize. A save the load verifier accepts can therefore kill
the host on the first script write.

**Direction.** Tie a table's logical length to what the save carries:

- Bound `array_len` by the stored span end plus a slack the format can justify.
  Live tables reach a large `vlen` with a small window only through a FIFO, and
  their window sits at the tail, so one bound is
  `first_slot + array_slots <= array_len <= first_slot + array_slots + k`. Check
  that bound against what `snapshot_entries` can produce for a FIFO. A live
  FIFO's dead prefix is not saved, so `first_slot` can be large; that case is
  fine.
- Or make `prepare_append`'s gap fill fail-safe: treat a gap larger than the
  window as a window reset, not a resize. This is arguably right for live tables
  too, since nothing promises that the slots between `base + len` and `vlen` hold
  storage.

Both `grow_vlen` asserts are a panic the host cannot catch, but docs/limits.md
documents that one.

## SNP-006 - "Reservations are capped at the remaining bytes" counts elements, not bytes

From the `snapshot` hunter.

**Claim broken.** docs/save-state.md "Byte stability" and the module doc of
save_state.rs: the codec is bounds-checked "and reservations are capped at the
remaining bytes."

**Mechanism.** `read_vec` reserves `Vec::with_capacity(len.min(input.remaining()))`.
That caps the element count at the remaining byte count, so the reservation in
bytes is `remaining * size_of::<T>()`. For the bytecode arena,
`T = SavedBytecode` is around 220 bytes (seven `Vec`s, an `Option<String>`, an
`Option<Arc<[u8]>>` and scalars). Objects are about 70 bytes and values 32
bytes. A forged save whose `Chunks` length prefix is `u32::MAX` reserves about
220 times the file size before reading the first element: a 100 MB input means
about 22 GB. Under the default Linux overcommit heuristic, or the repo's own
`ulimit -v` test cap, that allocation fails and the process aborts instead of
returning `SnapshotError::DecodeError`.

The comment in `read_vec` ("each element consumes at least one byte") is right
about the count. The documented claim is about memory, and the code does not keep
it.

**Direction.** Cap by bytes: either `len.min(remaining / min_encoded_size::<T>())`
with a per-type minimum encoded width, or skip the pre-reservation and let `push`
grow the vector (amortised doubling is bounded by what is actually decoded).

## SNP-007 - Identities rendered during load setup are silently replaced

From the `snapshot` hunter.

**Claims broken**

- The doc of `State::format_pointer_id` (table_ops.rs): "a deterministic,
  state-local identity".
- docs/determinism.md: "Identities are never reused." The verifier enforces
  `id < next_format_pointer_id` precisely so that "the next fresh value [cannot]
  render the same identity as a live one".

**Mechanism.** `load_state` runs `setup` on a fresh State, and setup can run Lua
and render values. docs/save-state.md says a sealed setup "must replay the
initialization the saving State performed", so this is expected use.
`materialize_payload` then overwrites both `state.next_format_pointer_id` and
`state.format_pointer_ids` with the saved ones.

Take a value that outlives setup and was rendered during it: an anchor created
there ("Anchors created there survive"), a setup-only global, or a registered
host function. It rendered as some id `k` during setup. After the load it either
renders as a different id (its entry is gone), or shares `k` with a saved value
that was assigned `k` in the saving State. Both ids came from the same counter
start, so the second case is the norm, not a corner case.

**Direction.** Merge instead of replacing: keep setup entries whose values
survive, re-key them above the saved counter, or reject or advance on collision.
The simplest consistent rule is that setup must not render identities, since it
is initialization, and that rule could be enforced. As written, the code keeps
neither promise.

## SNP-008 - A cross-folding load fails outright when two folded functions were rendered or used as keys

From the `snapshot` hunter.

**Claim broken.** docs/save-state.md "Rust functions": "a folded pair loads back
as the smaller id's function". The sentence is unconditional.

**Mechanism.** The materialize pass repeats duplicate checks on runtime values:
table keys, environment delta keys, and `format_pointer_ids` through
`insert_runtime_key`. Take a save from a build where `f` and `g` are distinct,
such as a debug build (the golden fixture is regenerated in debug because
release folds), and load it into a build where they fold. The load fails with
`Duplicate { FormatPointerValue | TableKey, Materialized }` as soon as both
functions were printed, or both are keys of one table. In that case the pair does
not "load back" at all. The code comments know this ("its failures are the ones
that catch those cases"); the user-facing doc does not say it.

**Direction.** Either state in docs/save-state.md that such saves are rejected,
or resolve deterministically: for pointer ids, keep the smaller id's entry. Table
keys genuinely collide, so rejection is defensible there.

## SNP-009 - Environment deltas carry a second, non-canonical array encoding

From the `snapshot` hunter, raised as a structural note outside the defect bar.

`encode_env_deltas` calls `table.virtual_array()`, which allocates
`vec![Nil; vlen]` and encodes every slot. A library table used as a FIFO
(`table[#table+1] = x; table[head] = nil`) therefore makes every save O(total
pushes ever), in both memory and bytes, while ordinary tables save O(live span).
docs/save-state.md documents this, so it is not a defect, but it exists only
because env deltas predate the canonical `(vlen, span)` form. Encoding the env
array in the same `snapshot_entries` form would remove `virtual_array`, the
`ArrayDisagreesWithUpserts` and `OrderDisagreesWithArray` cross-checks, and the
materialize-side merge of setup slots beyond the saved range. The hunter
considers it worth a rewrite of the delta array section. It changes the format,
so it needs a `FORMAT_VERSION` bump.

## SNP-010 - The load-time collection claim is imprecise

From the `snapshot` hunter, raised outside the defect bar.

docs/save-state.md says "Allocation during load never runs GC. The only
collection happens after all globals, deltas and setup anchors are in place."
Setup runs inside `load_state` and can run Lua under the automatic policy, so
collections can happen during load, before materialization. The doc means the
materialization phase and should say so.

## SNP-011 - Small load-path observations: setup's stack, error wording, misleading delta path

From the `snapshot` hunter, raised outside the defect bar.

- **Setup's stack is discarded without a word.** `reset_in_flight` at the end of
  `materialize_payload` drops anything setup left on the value stack. That is
  harmless, but nothing documents that setup's stack is not preserved. A host
  that pushes a value in setup and expects it after load gets an empty stack and
  no error.
- **Error wording.** `EnvironmentCorruption::MetatableNotTable(Materialized)`
  renders as "saved environment metatable is not a table". Every other
  `Materialized` variant says "materialized ...", and this one reads like the
  `Saved` form. Cosmetic (error.rs, `impl Display for EnvironmentCorruption`).
- **Misleading unregistered-function path from environment deltas.** When an
  unregistered function is reached only through an environment delta (for
  example `string.f = hostfn` with the `string` global deleted), `PathFinder` is
  rooted with `global = token`. The report then reads `global string ...` even
  though no global of that name holds it. Diagnostic only.
