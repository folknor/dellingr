# Defect hunt: vm-heap (values, heap, tables, anchors, persistence)

Scope: `src/vm/object.rs`, `src/vm/lua_val.rs`, `src/vm/table.rs`,
`src/vm/table_ops.rs`, `src/vm/anchor.rs`, `src/vm/save_state.rs`,
`src/vm/save_state/verify.rs`. Some findings follow values out into
`vm.rs`, `eval.rs`, `eval_index.rs`, `eval_store.rs`, `lua_std/table.rs` and
`compiler/verify.rs`.

Findings are ordered by severity. Each one names the claim it breaks.

---

## 1. `table.sort` and `table.remove(t, 1)` are O(n^2) in host CPU but cost n and 1

**Claim broken:** the exact cost model. The cost budget is supposed to bound
the work a script can make the host do. The flat-cost comment in
`lua_std/table.rs` says the shift is "the same per-element work table.move
charges one per element for", which assumes linear work.

**`Table::set_array`** (the writeback for `table.sort`) first calls `remove`
on keys `1..=old_len`, which leaves one tombstone per key
(`dead_count = old_len`). It then calls `insert(i, v)` for each sorted value.
With `dead_count > 0`, every insert goes to `insert_into_map_with_tombstones`.
That finds the dead slot for key `i` and calls `map.shift_remove_index(index)`.
For a sequence the dead slot is near the front, so each call is O(n). The
writeback is therefore O(n^2). The sort charges `n` (`table_sort`'s
`consume_cost(n.max(1))`). Sorting 10^5 elements does about 10^10 element
moves for 10^5 cost.

**`Table::array_remove`** calls `ensure_map`. It then calls
`map.shift_remove(&pos)` and, for every `i in pos..len`,
`map.shift_remove(&(i+1))` followed by `map.insert(i, v)`. `shift_remove` is
O(n) and the removed key is always near the front, so `table.remove(t, 1)` is
O(n^2) at a flat cost of 1. A loop draining a queue with `table.remove(t, 1)`
is cubic in host time.

Side effects:
- `ensure_map` permanently promotes small inline tables to `IndexMap`.
- The re-append order breaks `Table::get`'s positional fast path
  (`get_index(n-1)`) for any table that has non-array keys before the array
  part.

**Fix:** treat the array part as the problem, not these two functions. Both
want a real array segment, a `Vec<Val>` for `1..=n` next to the hash part,
like reference Lua. `sort`, `insert`, `remove` and `#` would then be
memmove/slice operations. The positional fast path in `get`, the
`compute_array_len` binary search and the `cached_array_len` bookkeeping
would all go away. Until then, `set_array` should overwrite values in place
(the keys already exist) instead of removing and re-inserting.
`array_remove` should overwrite `t[i] = t[i+1]` in place and remove only the
last key.

## 2. `table.remove(t, 0)` / `table.remove({[0]=x})` returns nil and leaves `t[0]`

**Claim broken:** output matches reference Lua for implemented features.

`lua_std/table.rs` accepts `pos == 0` when `len == 0`, which matches the
reference argument check. `Table::array_remove` then does
`if pos > len || pos == 0 { return Val::Nil; }`. Both Lua 5.2 and 5.4 read and
return `t[pos]` and then set `t[pos] = nil`. Example:
`local t = {[0] = "x"}; print(table.remove(t))` prints `x` in reference Lua
and `nil` here, and `t[0]` survives. The same applies to
`table.remove(t, #t + 1)` when `t[#t+1]` is non-nil, which is impossible for
a border, so only the `pos == 0` case is reachable. No `-- DIFF:` marks this.

## 3. `tostring` mangles non-UTF-8 bytes returned by `__tostring`

**Claim broken:** reference output parity, and `bytes_with_tostring_meta`'s
documented purpose ("preserving arbitrary bytes returned by a `__tostring`
metamethod").

The `tostring` builtin (`lua_std/basic.rs`) calls
`State::to_string_with_meta`. That function does
`String::from_utf8_lossy(&self.bytes_with_tostring_meta(idx)?)` and then
`push_string`. A `__tostring` that returns `"\xff"` produces `"\u{FFFD}"`
(3 bytes). Reference Lua returns the raw byte. The byte-preserving helper
exists but the builtin bypasses it. `to_string_with_meta` is also the public
API, and its `String` return type forces the loss on hosts too.

## 4. `call_anchor` can insert the function into the caller's frame and leave it there on error

**Claim broken:** the anchor module's "errors leave the stack untouched"
discipline, and `call`'s documented "A fixed call with insufficient stack
values returns `InvalidStackIndex`".

`call_anchor` computes `insert_at = self.stack.len().checked_sub(n_args)`
against the absolute stack, not against `stack_bottom`. Inside a `RustFunc`
(`stack_bottom > 0`), passing `n_args > get_top()` inserts the function
below `stack_bottom`, into the Lua caller's registers. `call(Fixed(n))` then
fails its own `get_top() >= n+1` check and returns `InvalidStackIndex`. The
inserted value is never removed, so the caller's frame is shifted by one
slot. The check should be `n_args <= self.get_top()`, made before any
mutation.

## 5. Every `Table::clear_and_insert_entries` resets `version` to 0, so on load inline caches can return the wrong field

**Claim broken:** the table `version` contract ("Shape version for key/index
stability") that every field, method and string-method inline cache relies
on. `get_cached_field` / `get_cached_index_table_field` /
`get_cached_string_method` return `get_index(entry.index)` with no key check
when the versions match.

`clear_and_insert_entries` does `*self = Table::with_capacity(..)`. That
resets `version` to 0, and the re-inserts don't bump it because
`dead_count == 0`. During `load_state`, the env-delta `order` path rebuilds
an environment table (`math`, `string`, ...) in a different physical order
with version 0.

The `setup` closure runs before materialization. It can execute Lua and
create anchors, which are deliberately kept. If setup warmed an IC on that
table at version 0 (any unmodified library table is at version 0), the
cached index now points at a different key. A later call through the anchored
function returns the wrong library member silently.

Only the `table` token's fallback cache is dropped explicitly. Rebuilding
should bump the version past the old value (keep the old counter and
increment it) rather than reset it. More generally, a per-State monotonic
"shape epoch" would make version collisions impossible by construction.

## 6. `StringPool::collect` is O(dead x live): `IndexMap::shift_remove` per freed string

**Claim broken:** none on correctness. This is a large GC performance defect,
and the code comment justifying the structure is inaccurate.

For each removed string, `collect` calls `hash_index.shift_remove(&hash)`,
which shifts every later bucket. A collection that frees k strings out of n
costs O(k*n). String-heavy scripts (gsub/concat churn) hit this on every
cycle. The comment says the index is "Iterated only during GC sweep", but it
is never iterated at all. Insertion order is irrelevant, so `swap_remove` (or
one `retain` pass) is correct.

Related, about the structure as a whole: the pool computes an Fx hash, then
`IndexMap<u64, Vec<StringKey>>` re-hashes that u64 with SipHash
(`RandomState`). A `hashbrown::HashTable<StringKey>` keyed by the stored hash
would avoid both the second hash and the per-bucket `Vec` allocation.

## 7. Every `IndexMap` in the VM uses the default `RandomState` hasher

**Claim broken:** the letter of AGENTS.md's lint gate ("`RandomState` and
`DefaultHasher` are banned"; "Host state is banned"). The gate only catches
explicit uses. `IndexMap<Val, Val>` (table storage), `globals`,
`format_pointer_ids`, `StringPool::hash_index` and every other
`IndexMap::new()` all pick up `S = RandomState`, which is seeded from OS
entropy each run.

I found no observable output divergence, because iteration is insertion
order. Bucket layout and therefore probe counts do vary per process, so
performance is not reproducible. Every table lookup also pays SipHash. A
fixed-seed fast hasher (Fx/foldhash with a constant seed) via a crate-wide
type alias would meet the stated rule and speed up the hottest path in the
VM.

## 8. Snapshot verifier accepts duplicate table-template keys, which corrupts `Table::dead_count`

**Claim broken:** the load contract that "A saved bytecode program violates
VM structural invariants" is rejected with `InvalidBytecode`.

`compiler/verify.rs` only checks that template key ids are in range. The
parser guarantees uniqueness (`try_use_table_template` refuses
`template.contains(key_id)`), but a forged save can supply `[0, 0]`, or two
equal-content string literals `[0, 1]` that intern to one `StringPtr`.
`Table::with_template_keys` then builds a map with one entry and
`dead_count = 2`.

From there:
- `insert_into_map_with_tombstones` computes `map.len() - self.dead_count`.
  This underflows, which panics in debug or test builds and wraps in release.
- `init_at_index` and `remove` keep the phantom tombstone count. That
  suppresses compaction and skews `fallback_shape`.

The verifier should reject duplicate resolved template keys.
`with_template_keys` should set `dead_count = map.len()` instead of
`key_ids.len()`.

## 9. A saved table's metatable may resolve to a closure

**Claim broken:** a table's metatable is always a table. `set_metatable_of`
rejects anything else with a type error. `getmetatable` and every metamethod
lookup assume it.

`verify::validate_references` only range-checks the metatable `SavedVal`.
`materialize_payload` only checks that it decodes to `Val::Obj(_)`. The
`SavedMetatableDelta::Set` path has the same gap. A save whose metatable is
`Obj(id)` with `objects[id]` a `SavedObject::Closure` loads without error.
`getmetatable(t)` then returns a function, and `as_table_ref(mt)` silently
yields `None` in the metamethod paths. The fix is to require
`SavedObject::Table` (or an `EnvObj`) during verification.

## 10. `next_format_pointer_id` is not validated against the saved ids

**Claim broken:** `%p` / `tostring` identities are distinct per live value
(the "deterministic, state-local identity" in `format_pointer_id`).

`validate_pointer_ids` rejects duplicate saved ids. It does not require
`next_format_pointer_id` to be greater than all of them. A forged save with
`next_format_pointer_id = 0` and an existing entry with id 0 gives the next
fresh value the same rendered address as a live one. Saves produced by the
engine always satisfy the invariant, so this matters only for hostile or
corrupted input. The verifier claims to guard exactly that input.

## 11. Rust functions render as `<function>` and never get an identity

**Claim broken:** `lua_val.rs`'s stated reason for having no `Display`
("Rendering an object needs ... its deterministic identity"). Also
`format_pointer_id`'s own `debug_assert!`, which lists `Val::RustFn` as an
expected caller.

`bytes_with_default_string_coercion` only routes `Val::Obj` through
`format_pointer_id`. `tostring(print)` and `tostring(math.floor)` are
therefore both `<function>`, while Lua closures render as `function: 0x..`.
Two distinct builtins are indistinguishable by `tostring` but unequal under
`==`. Reference Lua gives them distinct `function: builtin: 0x..` /
`function: 0x..` strings. Either RustFns should get pointer ids, or the
`debug_assert!` and the identity comment should stop claiming they do.

## 12. Smaller items

- **`Table` tombstone keys are not GC-marked but stay in storage.**
  `mark_values` skips the key of a nil-valued slot, so a dead slot's
  `StringPtr`/`ObjectPtr` key is freed and dangles. Every current consumer
  only compares or hashes keys, which is safe with generational keys, or
  filters dead slots first (`entries`, `live_string_keys`). Any future code
  that walks raw slots and dereferences a key (`heap.get_string(key)`) will
  panic. This invariant ("dead-slot keys may dangle; never dereference them")
  is not written down anywhere. It should be a comment on `TableStorage`.
- **`array_insert` calls `compact_dead()` without bumping `version`.** The
  later `bump_version()` at the end of the function covers it, but only
  because nothing reads caches in between. That is fragile ordering:
  `compact_dead` says "The caller owns version changes".
- **`encode_env_deltas` is O(n^2) per environment table.** It uses linear
  `iter().any(...)` / `find(...)` across the baseline and live entries.
  Scripts can grow `string`/`math`/`table` without bound, so save time is
  quadratic in user additions to library tables.
- **`table_len` swallows a bad index and returns 0.** Every other public
  index API returns `InvalidStackIndex`. It takes `&self` and returns
  `usize`, so a host bug reads as "empty table".
- **Gremlin-class characters in comments.** `table.rs` uses `≤` (in
  "Small tables (≤4 entries)" and similar) and `→` in a test comment. Whether
  the gremlins scan covers these is unclear. They contradict the spirit of the
  "no strange Unicode" rule.
- **`GcHeap::collect` doc and `is_full` naming.** `Note: Caller must check
  is_full() and run GC if needed before calling` sits on the allocators, but
  `load_state`'s materialization deliberately ignores it, and the
  `alloc_table_with_capacity`/`with_template` variants lack the note. A typed
  "allocation without collection" API (a `NoGc` token) would make the
  load-time exception explicit instead of relying on comments.

## Checked and found sound

- `mark_gc_roots` covers stack, globals, builtins, runtime literals,
  transient roots, registry, table-library names and snapshot env tokens and
  baselines (through `GcHeap::mark`).
- Open upvalues are pool-marked in `gc_collect`.
- Executing closures are rooted via `transient_roots` in `call`.
- `format_pointer_ids` is pruned after a sweep.
- `Hash`/`Eq` for `Val` are consistent: -0.0 is normalized, NaN keys are
  rejected before hashing, and RustFn uses address for both.
- `sweep_bytecode_caches` identity keys cannot ABA, because each entry holds
  its `Arc<Bytecode>`.
- Inline/Map tombstone transitions keep `dead_count` correct for parser-made
  tables.
- Save-time quiescence covers every in-flight field I could find.
