# Hunt: tables (src/vm/table.rs, src/vm/table_ops.rs)

Scope: the table implementation (array window, hash part, tombstones,
iteration, border, templates, metatables) and the table host API and
`__index`/`__newindex` chains it feeds. Static read only; nothing was built or
run. The repros below come from reading the code. Each one should become a
test before anyone acts on it.

Overall, the core storage held up under a close read. The version-bump rules
cover every path that changes which key sits at a hash index: Map re-add,
inline shift, both compactions and rebuild; promotion keeps indices. The
tagged array indices survive moves of the window. The `pairs` cursor checks
the key, not the version, which is enough because each key has at most one
slot. `dead_count` stays exact on every path, and `init_at_index`'s fallback to
`insert` stays correct when a runtime `[k]` field has already vacated or
compacted a template slot. The findings below are where the code and a stated
contract part ways.

---

## 1. A cleared hash key can still resume `next` in a continuous run, but not after save/load (replay divergence)

**Claim broken.** docs/determinism.md "`pairs` order": "Snapshots preserve
it, so a loaded state continues with the same order". docs/save-state.md
"Byte stability": "A loaded State and the continuous run it came from keep
saving identical bytes". AGENTS.md lists snapshot contents and `pairs` order
as replay-visible. Lua's own `next` contract applies too: "you may clear
existing fields" during a traversal. dellingr implements that contract with
hash tombstones (`Table::next_with_index` and `next_from_matching_index`
accept a dead slot as the control key: "A dead control slot remains valid,
matching Lua's `next` behaviour here").

**What the code does.** A cleared hash entry stays in the table as a
tombstone (`hash_remove`). `next(t, k)` for that cleared `k` finds the
tombstone's index and continues from it. Snapshot save only keeps live
entries (`snapshot_entries` -> `live_hash_entries`), and
`rebuild_from_snapshot` re-inserts only those. So the loaded table has no
tombstones, and `next(t, k)` for the same cleared key returns `InvalidKey`
("invalid key to 'next'"). The callback dies, while the continuous run goes
on. Nothing compacts tombstones between callbacks unless a *new* key is
inserted, and that is already undefined during a traversal. A pure
"clear as you go" cursor therefore works indefinitely in the continuous run
and breaks on the first resume after a load.

A pattern that is legal Lua and natural for budgeted per-tick game work:

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

Template tables have a variant. `{a = nil, b = 1}` creates a dead template
slot for `a` (`with_template_keys` pre-inserts every key as nil). In the
continuous run `next(t, "a")` returns `("b", 1)`; after a load it raises.
Reference 5.4 also raises here, because it never inserts a nil-valued key.
So the continuous run is the lenient one, and only the snapshot boundary
exposes it.

A secondary effect: tombstone keys are not marked (`mark_values` marks only
live slots). Whether a cleared key whose only reference was the tombstone can
still resume a traversal therefore depends on whether a collection freed and
re-interned it, which ties the result to collection timing. A loaded State's
collection timing differs from the continuous run's.

**Fix options.**
- Make the dead slots part of the saved shape, the way environment deltas
  already save the whole logical array. Save the hash part as slots with a
  dead marker, so the loaded table has the same tombstones (and the same
  compaction schedule). Canonical bytes would then depend on history the
  same way the array length already does. The save-state doc's "never where
  its stored window happens to sit" applies to the window, not to
  tombstones, so the doc would need a sentence on this.
- Or decide that a traversal cursor naming a cleared key does not survive a
  save. Say so in save-state.md ("Not saved: ...") and compatibility.md, and
  make the continuous run refuse it too, so both runs agree. That means
  either never resuming from a tombstone, which breaks the
  `for k in pairs(t) do t[k] = nil end` idiom unless the tfor cursor alone
  carries it, or clearing tombstones at a deterministic, save-aligned point.

The first option is the one that keeps both Lua's `next` contract and the
replay contract.

---

## 2. Host `get_table` drops the key on a refusal that never enters the handler

**Claim broken.** The doc comment on `get_table_with_key_tracking_call`
(src/vm/metamethod.rs): "records the commit point for host reads: only
entering a user function can consume the key on failure". `State::get_table`
docs: "Validation and dispatch preflight failures leave the stack
unchanged". AGENTS.md: `get_table_unguarded` "restores the key only before
handler entry".

**What the code does.** `handle_index_metamethod_inner` (function and RustFn
arms) and `call_string_index_handler` (src/vm/eval_index.rs) set
`*handler_called = true` *before* `call_unguarded`. `call_unguarded` can
still refuse without entering anything:

- `native_call_depth >= MAX_NATIVE_CALL_DEPTH` (`NativeCallDepthExceeded`);
- `begin_call` -> `admit_lua_call`: `MAX_CALL_DEPTH`, and the callee frame's
  `check_stack_space(grow)` (`StackOverflow`).

On each of these, `fail_call` clears the three operands, and
`get_table_unguarded` sees `handler_called` and does not push the key back.
Two `StackOverflow` refusals of the same `get_table` call then leave
different stacks:

- 0-2 free slots: the refusal comes from `check_stack_space(3)`, which runs
  before the flag is set. The key is preserved, which is what
  `tests/host_table_api_atomicity.rs::get_table_preserves_the_key_on_dispatch_preflight_failures`
  asserts.
- 3 free slots but fewer than the handler's `max_stack`: the refusal comes
  from the callee-frame admission. The key is consumed.

The same split applies to a host `get_table` made from a Rust callback that
is already at the native depth limit.

**Fix.** Set the commit flag where the callee is actually entered, not
before the call. One way is for `call_unguarded` to report whether it got
past admission, for example a separate admission phase whose refusals carry
an "operands untouched" outcome. The cleaner structural move is a
`try_call_preflight` that runs every refusal check (native depth, Lua depth,
callee stack) before anything is pushed. Then "preflight failure" means one
thing for every host entry point, not just `get_table`.

---

## 3. An all-dead array window is not released at "an array write outside it" when the dead-prefix hint lags; it grows instead

**Claim broken.** docs/limits.md, memory section: "an all-dead window is
released at the next append or array write outside it, so a FIFO ... keeps
storage proportional to its backlog". The doc comment on
`write_outside_window` says the same: "An all-dead window restarts at `key`".

**What the code does.** "All dead" is tested as `self.lead == self.array.len()`.
`lead` is only a lower bound. It advances by at most `LEAD_CATCH_UP` (8) per
nil write *at* the hint, and not at all for nil writes above it. Clearing a
window from the top down leaves `lead` at about 9 however long the window is.
`write_outside_window` does no catch-up of its own, so it takes the extend
branch, and the downward pad uses `len - lead` as its "live span" estimate:

```lua
local q = {}
for i = 1, 2000 do q[i] = i end
for i = 1, 1000 do q[i] = nil end      -- hint tracks: lead = 1000
q[2001] = true                          -- append drops the prefix: base = 1000, window keys 1001..2001
for i = 2001, 1001, -1 do q[i] = nil end -- top-down clear: lead ends at 9, window is all nil
q[1] = true                             -- outside the window, so it should restart there;
                                        -- instead splices 1000 nils: window = 2001 slots, 1 live
```

Scale the numbers up and a single SET charge doubles a large all-nil
allocation that the doc says is released. That is host work out of
proportion to its charge, and a direct contradiction of the documented
release rule. The append path has the same blind spot: an all-nil window
with a lagging hint is kept and pushed onto. limits.md's "after catching up
within a bounded number of pushes" partly covers that case, though the bound
is O(lag / 15) pushes, not a constant. The outside-window write has no
catch-up at all.

**Fix.** Replace the hint with an exact live count of the window, a
`window_live: usize` maintained on every window write (nil to live, live to
nil). That makes "all dead" O(1) and exact. `lead` can stay as a
prefix-skip accelerator for `next`/`mark_values`, but release decisions
should never depend on it. The downward pad in `write_outside_window` should
then use the live count, not `len - lead`.

---

## 4. determinism.md's `pairs` order rule leaves out migration, so it predicts the wrong order

**Claim broken.** docs/determinism.md "`pairs` order": "A table's array part
(keys `1..n`, grown by appending `n + 1`) is visited first, in key order.
Every other key follows in insertion order." This is the replay contract for
`pairs`.

**What the code does.** Growing the array also *migrates* any live hash
entries for the following keys into the array (`migrate_from_hash`). A key
that was inserted into the hash part before some other key can therefore end
up visited ahead of it:

```lua
local t = {}
t.x = 1
t[3] = "c"   -- not an append (n = 0): hash part, inserted after x
t[1] = "a"   -- append
t[2] = "b"   -- append, then key 3 migrates into the array
for k in pairs(t) do print(k) end
-- code: 1 2 3 x     the doc's rule predicts: 1 2 x 3
```

The code is self-consistent and matches the `Table` struct doc ("after which
any live hash entries for the following keys migrate into the array"). The
durable doc a script author or replay tool reads is the one that is wrong.
Add the migration rule there: "appending `n + 1` also moves keys
`n + 2, n + 3, ...` that are already present into the array part".

---

## 5. Lower-severity items

### 5a. `set_metatable_of` rejects a bad metatable as "attempt to index a X value"

`State::set_metatable_of` (table_ops.rs) returns
`TypeError::TableIndex(other.typ())` when the value on top is neither a table
nor nil. It returns the same error when the target is not a table, through
`table_operand_below_top`. For a host passing a number as the metatable, the
message reads "attempt to index a number value", but nothing was indexed.
Scripts never see this, because `setmetatable` type-checks argument 2 first.
For the host it is the "misleading message" case AGENTS.md counts as a
defect. It needs its own error kind, for example "metatable must be a table
or nil".

### 5b. Tombstone keys are a weak reference that collection never prunes

src/vm/object.rs `val_is_live` notes that handles kept across allocations
"can alias one, since generations wrap; that is why weak side tables ... are
pruned with this at every collection". A tombstone's key is exactly such a
weak handle (the `TableStorage` doc: "the key of a dead ... slot may
dangle"), and it is never pruned. After a slot's generation wraps, a new
object can compare equal to a dead tombstone key. `next(t, newobj)` would
then resume the traversal instead of raising "invalid key to 'next'". The
`insert`/`get` paths stay correct, because re-add semantics apply. It takes
2^31 reuses of one arena slot, so this is practically unreachable, but it is
the one weak table the stated pruning rule does not cover. If finding 1 is
fixed by saving tombstones, decide at the same time whether tombstones should
keep their keys alive (mark them) or be pruned at collection.

---

## Lateral findings (outside the scope, or performance)

### L1. A single miss through `__index` pins a GET_FIELD callsite to the slow path for good

`get_index_table_field_direct` (eval_index.rs) returns `None` at once
whenever the callsite's cached method entry has `method_index == None`.
`set_method` is only called further down the same function, so the entry
never gets replaced. The miss is never revalidated against the method
table's version, and it is not keyed on the receiver's metatable either. One
miss therefore sends every later read at that callsite to
`get_table_with_key`:

```lua
local Base = {}
local o = setmetatable({}, {__index = Base})
local function get(x) return x.greet end
get(o)              -- miss is cached as "method_index = None"
Base.greet = print
for i = 1, 1e6 do get(o) end   -- always the slow path, for every receiver
```

Results stay correct; the cost is performance. The same goes for a
polymorphic callsite where one receiver's class lacks the field. The
`Some(None)` "known miss" arms in `get_cached_index_table_field` cannot be
reached from its only caller. Either revalidate negative entries (the
version check is already there) and let them repopulate, or stop caching
misses.

### L2. `try_insert_table_direct` looks the key up twice

For a table with a metatable, it calls `tbl.get(&key)` and then `insert`,
which repeats the lookup. This is a performance note only.

### L3. The doc comments on `debug_assert_invariants` say more than the code checks

The O(1) branch for big maps uses the message "dead_count must equal the
number of nil-valued hash slots" for a `<=` bound, and the lead check covers
only the last 64 slots of the hinted prefix. Both are deliberate for speed.
The messages and doc comments should say so, so a failure is not misread.
