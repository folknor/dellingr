# Hunt: snapshot (save_state, verify, error, snapshot_format, rng)

Scope read: src/vm/save_state.rs, src/vm/save_state/{verify,error,integrity_tests}.rs,
src/snapshot_format.rs, src/vm/rng.rs, docs/save-state.md, plus the parts of
src/vm/table.rs, src/vm/global_env.rs, src/vm/vm.rs (bytecode caches,
quiescence), src/vm/table_ops.rs (format_pointer_id), src/lua_std/string.rs
(gmatch) and src/lua_std/basic.rs (getmetatable) that the snapshot paths ride on.

Findings are ordered by how much they matter for the stated contracts.

---

## 1. Rendered identities of environment objects are dropped even though the objects survive the load

**Claim broken:** docs/determinism.md "Object identity in output": `tostring`
and `%p` identities are "stable across a snapshot round trip"; docs/save-state.md
"What is saved": "State-local object identities that `tostring` and `%p` have
rendered". The save-side comment in `SaveBuilder::finish` justifies the filter
with "A heap value unreachable from saved state cannot survive the load", which
is false for environment objects.

**What the code does:** `SaveBuilder::finish` keeps a `format_pointer_ids` entry
only if `saved_reachable_pointer_value` resolves it *and* the value is in
`reachable_pointer_values` (the `Fn` case is exempt). `reachable_pointer_values`
is filled only by the value walk rooted at globals and environment deltas. An
`EnvObj` is in that set only if some saved value points at it.

Environment objects are rebuilt by `open_libs` on every standard load, so they
always survive, and some are reachable by scripts without any global:

- The shared string metatable (`string.metatable` token). `getmetatable("")`
  returns it; it has no `__metatable` protection (`install_string_metatable`
  in src/vm.rs; `raw_metafield` in basic.rs). No global or table refers to it,
  so the walk never reaches it.
- The `string` library table once the `string` global is shadowed or deleted.
  It is still reachable as `getmetatable("").__index`.

Repro sketch: `print(getmetatable(""))` prints `table: 0x1`. Save, load, run
`print(getmetatable(""))` again. The loaded State hands out a fresh id
(`next_format_pointer_id` was restored but the entry was not). The continuous
run prints `0x1`. That is a replay-visible divergence between a loaded State
and the run it came from. In the live State the entry is never pruned, because
the string metatable is a GC root (`mark_gc_roots`, vm.rs), so the save is the
only place the identity is lost.

The same gap covers strings that are reachable only through an environment
object the walk did not expand. Example: the keys of the `string` table once
`string` is deleted, enumerated through `pairs(getmetatable("").__index)` and
rendered with `%p`. The identity-string loop covers meta names and literals of
saved chunks, but not environment-table keys or values.

**Direction:** the "survives the load" set is everything `open_libs` rebuilds
plus the saved graph, not the saved graph alone. Seed the reachability walk
with every `env_tokens` object, expanded the same way as the existing
`Discard` expansion. Even simpler: keep every `EnvObj` and every string or
function reachable from an environment object. Tests should pin
`getmetatable("")` and a shadowed `string` table across a round trip.

---

## 2. Identity strings get arena ids in heap-address order (byte stability)

**Claim broken:** docs/save-state.md "Byte stability": "Two saves of the same
logical state are byte-identical" and "A loaded State and the continuous run it
came from keep saving identical bytes."

**What the code does:** in `SaveBuilder::finish`, after the graph walk:

```rust
for key in self.bytecode_ids.keys() {
    if let Some(entry) = self.state.bytecode_caches.get(key) {
        identity_strings.extend(entry.runtime.literals.iter().copied());
    }
}
```

`bytecode_ids` is a `BTreeMap<usize, u32>` keyed by `Arc::as_ptr(bytecode) as
usize`, so this loop runs in heap-address order. Each identity-bearing literal
the graph walk has not already reached gets its string-arena id from
`string_id` in that order. Those ids show up in the `strings` section and in
`format_pointer_ids` (`SavedVal::Str(id)`).

Two saved chunks are enough to trigger it. Each needs a string literal that
was rendered with `%p` (for example `string.format("%p", "abc")` in `f` and
`string.format("%p", "xyz")` in `g`) and is not otherwise stored. The order of
those two strings in the arena then follows the allocator's placement of the
two `Bytecode` Arcs. The order is generally the same within one process
history. It differs between the continuous run, where nested chunks are
allocated by codegen, and a loaded State, where chunks are allocated by
`build_bytecode_tree` in arena order. It can also differ across allocators and
hosts.

**Direction:** iterate chunks in arena-id order: walk `self.bytecode` by
index, or sort `bytecode_ids` by value. More structurally, nothing in the
encoder should iterate an address-keyed map. `bytecode_ids` is the only one
iterated today (`object_ids` is keyed by slotmap key, and `upvalue_ids` and
`string_ids` are lookups only). Make that a rule, or key the map by arena id.

---

## 3. A forged logical array length turns the next append into a host abort or panic

**Claims broken:** docs/limits.md "State-local growth": the logical array
length cap "growing past it panics, and a save beyond it is rejected on load".
The `MAX_ARRAY_LEN` doc in table.rs: "Snapshot loading rejects a larger length
rather than panicking." docs/save-state.md: saves are user-editable input. The
task framing also covers "anything a loaded snapshot can make the VM believe
that live execution never could".

**What the code does:** `validate_references` (verify.rs) bounds a saved
table's `array_len` only by `MAX_ARRAY_LEN` (2^53 - 2), independently of
`array_slots`. `rebuild_from_snapshot` then builds a table with `vlen = array_len`
and a window holding only the stored span. Live execution reaches a large
`vlen` only by paying for that many appends, and paying in storage unless a
FIFO drained them. A save of a few dozen bytes reaches it for free:

- `array_len = 2^52`, `array_slots = 1` (key 1 live). Any write to key
  `2^52 + 1` takes the append path. `prepare_append` sees `base + array.len()
  < vlen` and calls `self.array.resize(self.vlen - self.base, Val::Nil)`: about
  2^52 x 16 bytes. The allocator fails and Rust aborts the process
  (`handle_alloc_error`). The host gets no error.
- `array_len = MAX_ARRAY_LEN` with an empty window. The append reaches
  `grow_vlen`, whose `assert!` panics.

The forged save can carry the triggering bytecode itself, and the cost budget
does not charge the resize. A save that the load verifier accepts can
therefore kill the host on the first script write.

**Direction:** a table's logical length should be tied to what the save
carries. Two options:

- Bound `array_len` by the stored span end plus a slack the format can
  justify. Live tables reach a large `vlen` with a small window only through a
  FIFO, and their window sits at the tail, so one bound is
  `first_slot + array_slots <= array_len <= first_slot + array_slots + k`.
  Check that bound against what `snapshot_entries` can produce for a FIFO.
  Note that a live FIFO's dead prefix is not saved, so `first_slot` can be
  large; that case is fine.
- Make `prepare_append`'s gap fill fail-safe: treat a gap larger than the
  window as a window reset, not a resize. This is arguably right for live
  tables too, since nothing promises that the slots between `base + len` and
  `vlen` hold storage.

Separately, both `grow_vlen` asserts are a panic the host cannot catch, but
docs/limits.md documents that one.

---

## 4. "Reservations are capped at the remaining bytes" counts elements, not bytes

**Claim broken:** docs/save-state.md "Byte stability" and the module doc of
save_state.rs: the codec is bounds-checked "and reservations are capped at the
remaining bytes."

**What the code does:** `read_vec` reserves
`Vec::with_capacity(len.min(input.remaining()))`. That caps the element count
at the remaining byte count, so the reservation in bytes is
`remaining * size_of::<T>()`. For the bytecode arena `T = SavedBytecode`, which
is around 220 bytes (seven `Vec`s, an `Option<String>`, an
`Option<Arc<[u8]>>` and scalars). Objects are about 70 bytes and values
32 bytes. A forged save whose `Chunks` length prefix is `u32::MAX` reserves
about 220 times the file size before reading the first element. A 100 MB
input means about 22 GB. Under the default Linux overcommit heuristic, or the
repo's own `ulimit -v` test cap, that allocation fails and the process
aborts rather than returning `SnapshotError::DecodeError`.

The comment in `read_vec` ("each element consumes at least one byte") is
correct about the count. The documented claim is about memory, and the code
does not keep it.

**Direction:** cap by bytes. Either use
`len.min(remaining / min_encoded_size::<T>())` with a per-type minimum encoded
width, or skip the pre-reservation and let `push` grow the vector (amortised
doubling is bounded by what is actually decoded).

---

## 5. Load setup's rendered identities are silently replaced

**Claim broken:** the doc of `State::format_pointer_id` (table_ops.rs):
"a deterministic, state-local identity", and docs/determinism.md "Identities
are never reused." The verifier enforces `id < next_format_pointer_id`
precisely so that "the next fresh value [cannot] render the same identity as a
live one".

**What the code does:** `load_state` runs `setup` on a fresh State, and setup
can run Lua and render values. docs/save-state.md says a sealed setup "must
replay the initialization the saving State performed", so this is expected
use. `materialize_payload` then overwrites both `state.next_format_pointer_id`
and `state.format_pointer_ids` with the saved ones.

Consider a value that outlives setup and was rendered during it: an anchor
created there ("Anchors created there survive"), a setup-only global, or a
registered host function. That value rendered as some id `k` during setup.
After the load it either:

- renders as a different id (its entry is gone), or
- shares `k` with a saved value that was assigned `k` in the saving State.

Both ids came from the same counter start, so the second case is the norm,
not a corner case.

**Direction:** merge, not replace. Keep setup entries whose values survive,
re-key them above the saved counter, or reject or advance on collision. The
simplest consistent rule is that setup must not render identities (it is
initialization), and that rule could be enforced. As written, the code keeps
neither promise.

---

## 6. A cross-folding load fails outright when two folded functions were rendered or used as keys

**Claim broken:** docs/save-state.md "Rust functions": "a folded pair loads
back as the smaller id's function". The sentence is unconditional.

**What the code does:** the materialize pass repeats duplicate checks on
runtime values: table keys, environment delta keys, and `format_pointer_ids`
through `insert_runtime_key`. Take a save from a build where `f` and `g` are
distinct, such as a debug build (the golden fixture is regenerated in debug
because release folds), loaded into a build where they fold. The load fails
with `Duplicate { FormatPointerValue | TableKey, Materialized }` as soon as
both functions were printed or both are keys of one table. In that case the
pair does not "load back" at all. The code comments know this ("its failures
are the ones that catch those cases"), but the user-facing doc does not say
it.

**Direction:** either state in docs/save-state.md that such saves are
rejected, or resolve deterministically. For pointer ids, keep the smaller id's
entry. Table keys genuinely collide, so rejection is defensible there.

---

## Lateral and structural notes

- **Environment deltas carry a second, non-canonical array encoding.**
  `encode_env_deltas` calls `table.virtual_array()`, which allocates
  `vec![Nil; vlen]` and encodes every slot. A library table used as a FIFO
  (`table[#table+1] = x; table[head] = nil`) therefore makes every save
  O(total pushes ever), in both memory and bytes, while ordinary tables save
  O(live span). docs/save-state.md documents this, so it is not a defect, but
  it exists only because env deltas predate the canonical `(vlen, span)` form.
  Encoding the env array with the same `snapshot_entries` form would remove
  `virtual_array`, the `ArrayDisagreesWithUpserts` and
  `OrderDisagreesWithArray` cross-checks, and the materialize-side merge of
  setup slots beyond the saved range. Worth a rewrite of the delta array
  section. It changes the format, so bump `FORMAT_VERSION`.
- **The load-time collection claim is imprecise.** docs/save-state.md says
  "Allocation during load never runs GC. The only collection happens after
  all globals, deltas and setup anchors are in place." Setup runs inside
  `load_state` and can run Lua under the automatic policy, so collections can
  happen during load, before materialization. The doc means the
  materialization phase; it should say so.
- **`reset_in_flight` at the end of `materialize_payload` silently discards
  anything setup left on the value stack.** That is harmless, but nothing
  documents that setup's stack is not preserved. A host that pushes a value in
  setup and expects it after load gets an empty stack, with no error.
- **Error wording.** `EnvironmentCorruption::MetatableNotTable(Materialized)`
  renders as "saved environment metatable is not a table". Every other
  `Materialized` variant says "materialized ...", and this one reads like the
  `Saved` form. This is cosmetic (error.rs, `impl Display for
  EnvironmentCorruption`).
- **Misleading unregistered-function path from environment deltas.** When an
  unregistered function is reached only through an environment delta (for
  example `string.f = hostfn` with the `string` global deleted), `PathFinder`
  is rooted with `global = token`. The report then reads `global string ...`
  even though no global of that name holds it. This is diagnostic only.
- **rng.rs checked clean.** The SplitMix64 step, the 53-bit `random_f64`
  scale (`MAX_EXACT_INTEGER = 2^53`, so the result is in [0, 1)) and the
  rejection threshold `span.wrapping_neg() % span` are correct, and the
  doc-comment bounds hold. `math.random` charges cost and validates arguments
  before drawing, so a failed call advances no RNG state.
- **Gmatch-state verification is defense in depth.** The runtime re-checks
  every field it reads (`gmatch_state_index`, the compile-flag checks in
  string.rs), so a forged state table reachable by scripts cannot corrupt
  iteration. Nothing to report.
