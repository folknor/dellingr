# Bug hunt 2026-07-24 - Corner C: Data plane

Auditor scope: GC heap + string interning, tables and table ops, index/store
dispatch (`__index`/`__newindex`/`__call`/`__len`), anchors. Files:
`src/vm/object.rs`, `src/vm/table.rs`, `src/vm/table_ops.rs`,
`src/vm/eval_index.rs`, `src/vm/eval_store.rs`, `src/vm/metamethod.rs`,
`src/vm/anchor.rs`. Root-set completeness (`mark_gc_roots` in `src/vm.rs`)
traced end to end. Supporting evidence read from `src/vm.rs`, `src/vm/eval.rs`,
`src/vm/eval_control.rs`, `src/vm/frame.rs`, `src/vm/stack.rs`,
`src/vm/lua_val.rs`, `src/lua_std/{basic,table,math,string}.rs`.

All findings are from reading code only; repros are written down, not executed.
Ranked most severe first within each section.

---

## A. Panics / memory-safety-class (script- or host-reachable aborts)

### A1. NaN table key panics the process via `next(t, 0/0)`

- `src/vm/lua_val.rs:162` - `Hash for Val` contains
  `assert!(!n.is_nan(), "Cannot use NaN as table key")` (a hard assert, so it
  fires in release).
- `Table::get` / `get_with_index` / `insert` all guard NaN keys at entry
  (`src/vm/table.rs:122, 152, 298`), but `Table::next` does NOT
  (`src/vm/table.rs:512`). On Map storage it calls `map.get_index_of(key)`,
  which hashes the key.
- The `next` builtin (`src/lua_std/basic.rs:77`) passes any script-supplied key
  straight through `State::table_next` (`src/vm/table_ops.rs:155`) into
  `Table::next`.

Repro (script only, no host cooperation):

```lua
local t = {}
for i = 1, 5 do t[i] = i end   -- >4 entries forces Map storage
next(t, 0/0)                    -- assert -> panic -> host process aborts
```

Reference Lua raises "invalid key to 'next'"; dellingr aborts the process.
Fix: NaN-guard (and arguably a missing-key error, see C6) at the top of
`Table::next` or in `table_next`.

### A2. `with_restricted_env` lets the saved environment escape the GC root set

- `src/vm.rs:614-654`: `with_restricted_env` swaps `self.globals` and
  `self.builtins` into locals (`saved_globals`, `saved_builtins`) and installs
  whitelist-only copies.
- `mark_gc_roots` (`src/vm.rs:69-89`) marks only the CURRENT
  `globals`/`builtins`. If `f(self)` executes script that allocates enough to
  trigger GC (initial threshold is 20; any nontrivial script will), every
  library table not on the whitelist (`math`, `string`, `table`, `_G` and its
  metatable, plus any user globals) is unreachable and gets swept.
- After the restore, `self.builtins`/`self.globals` contain dangling
  `ObjectPtr`s. The next access (`math.floor(1)`) hits
  `GcHeap::get`'s `expect("Invalid ObjectPtr: ...")` (`src/vm/object.rs:221`)
  and panics.

Repro (host API):

```rust
let mut state = State::new();
state.with_restricted_env(&["print"], |s| {
    s.load_string("local t = {} for i = 1, 2000 do t[i] = {} end").unwrap();
    s.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
});
state.load_string("print(math.floor(1.5))").unwrap();
state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap(); // panic
```

This is exactly the "values that can escape mark_gc_roots" class. Fix options:
mark `saved_globals`/`saved_builtins` as roots for the duration (e.g. park them
in a State field that `mark_gc_roots` covers - a `Vec` of shadowed
environments would also make nesting safe), or disable auto-GC during
restricted execution (worse: unbounded heap growth).

### A3. `#t` with a `__len` metatable: receiver unrooted across `alloc_string`

- `src/vm/eval_store.rs:146-190` (`instr_length`): the operand is popped
  (`let val = self.pop_val()`), then, on the metatable path, the code calls
  `self.alloc_string("__len")` at line 168. `State::alloc_string`
  (`src/vm.rs:657-665`) runs `gc_collect()` whenever `heap.is_full()`, even
  when the string is already interned.
- Between the pop and the metamethod call, neither the table nor its metatable
  is rooted anywhere (the metatable is only reachable through the table). If
  the length operand is a temporary, GC collects both, and the very next line
  (`self.heap.as_table_ref(mt_ptr)`) panics with the use-after-free expect; if
  the metatable happens to survive, the later `self.stack.push(val)` reinstates
  a dangling pointer that the `__len` body then dereferences.
- Contrast with the same file's neighbors that get this right:
  `metamethod.rs:49-52` and `:170-173` push key/val around `alloc_string`
  exactly to protect them, and `eval.rs`'s `__call` path parks `func_val` in
  `active_call_roots` around its `alloc_string("__call")`.

Repro sketch (script; hits when an allocation boundary lands on the `#`):

```lua
for i = 1, 10000 do
  local n = #setmetatable({}, { __len = function() return 0 end })
end
```

Each iteration performs several counted allocations (table, metatable,
closure, string check), so `is_full()` eventually becomes true precisely at
the `alloc_string("__len")` call. Fix: push `val` back on the stack before the
`alloc_string`, pop after (same pattern as `metamethod.rs`).

### A4. `table.sort` comparator runs with the array detached from the root set

- `src/vm/table_ops.rs:276-358`: `table_sort` copies the array portion into a
  local `Vec<Val> arr` (`t.get_array()`), then, in the comparator branch, calls
  arbitrary Lua code (`self.call(...)`) in a loop while `arr` is held only in
  Rust. `arr` is not reachable from `mark_gc_roots`.
- A comparator that removes elements from the table and allocates enough to
  trigger GC gets the not-currently-passed elements collected. The next bubble
  step pushes a dangling `Val` as a comparator argument (heap access inside
  the comparator panics), and `set_array` at the end can write dangling
  pointers back into the table (deferred use-after-free panic).

Repro:

```lua
local t = {}
for i = 1, 20 do t[i] = { v = i } end
table.sort(t, function(a, b)
  for k in pairs(t) do t[k] = nil end   -- drop the table's own references
  for i = 1, 200 do local _ = {} end    -- churn until GC fires
  return a.v < b.v
end)
```

Fix: sort in place (see O2), or root the detached array (e.g. park it on the
VM stack, or an `active_call_roots`-style side vector) for the duration.

### A5. GC mark phase is recursive - deep structures overflow the Rust stack

- `GcHeap::mark` -> `mark_children` -> `Table::mark_values` ->
  `Val::mark_reachable` -> `GcHeap::mark` (`src/vm/object.rs:355-386`,
  `src/vm/table.rs:555-573`) recurses once per nesting level with no depth
  bound. `MAX_CALL_DEPTH`/`MAX_METAMETHOD_DEPTH` protect the interpreter, but
  nothing protects the collector.
- A script can build a linked chain far deeper than the ~8 MB main-thread
  stack tolerates (roughly 5 frames per level):

```lua
local t = {}
for i = 1, 500000 do t = { t } end   -- cost ~2/iteration; auto-GC fires
-- during the loop and the mark phase recurses ~i deep -> stack overflow abort
```

Fix: iterative marking with an explicit worklist (`Vec<ObjectPtr>` gray
stack). Also removes the `#[hotpath::measure]` recursion caveat for `mark`.

---

## B. Correctness vs reference Lua (supported subset)

### B1. `Val` equality/hash for `RustFn` compares the ADDRESS OF THE PAYLOAD, not the function

- `src/vm/lua_val.rs:184-188` (PartialEq):

```rust
(RustFn(a), RustFn(b)) => {
    let x: *const RustFunc = a;   // address of the payload inside *self*
    let y: *const RustFunc = b;   // address of the payload inside *other*
    x == y
}
```

  `a`/`b` bind as `&RustFunc` (references into the two `Val`s being compared),
  so the coercion produces pointers to the enum payload slots, not the
  function addresses. Two `Val`s holding the same function compare unequal
  whenever they live at different addresses - which is always the case for
  `OP_EQUAL` (both operands are popped into separate locals, `frame.rs:262-266`)
  and for `raw_equal` (`stack.rs:215-220`).
- `src/vm/lua_val.rs:169-172` (Hash) has the same defect: it hashes the payload
  address. Consequences:
  - `print == print` evaluates to `false` (reference Lua: `true`).
  - `rawequal(print, print)` is `false`.
  - Rust functions are unusable as table keys: `t[print] = 1; t[print]` is
    `nil`, and repeated assignment creates duplicate entries (the Inline-scan
    and IndexMap probes never find an equal key).
  - The method-IC validation `index_handler != entry.index_handler`
    (`eval_index.rs:299`) can never validate a cached `__index = <RustFn>`
    handler, so those callsites permanently miss (perf symptom of same bug).
  - The `%p` identity map probe (`table_ops.rs:437`, `*candidate == val`)
    never matches for RustFn values, so every `%p` on the same Rust function
    mints a new id - both a leak-rate and a determinism-of-output concern.
- Note `TODO.md` ("Stable RustFunc identity...") believes this code hashes
  "by function-pointer address"; it does not even do that.

Fix: compare/hash the function pointer value itself, e.g.
`std::ptr::fn_addr_eq(*a, *b)` (already used correctly in
`eval_control.rs:138`) and `(*func as usize).hash(hasher)`. If clippy's
fn-address lint complains, the `fn_addr_eq` helper is the blessed form.
(Cross-build instability of fn addresses is irrelevant in-process; the
snapshot concern stays tracked in TODO.md.)

### B2. Removing the current key during `pairs` terminates iteration early

- Reference Lua explicitly permits clearing the field being iterated
  (`t[k] = nil` inside a `pairs` loop). dellingr's `Table::remove`
  (`src/vm/table.rs:376-405`) physically removes the entry
  (`shift_remove` / inline shift), and `Table::next(&key)`
  (`src/vm/table.rs:512-548`) then fails to find `key` at all -> returns
  `(nil, nil)` -> the loop ends after the first deletion.
- Both the `pairs` fast path (`eval_control.rs:186-207`,
  `instr_tfor_call_next`) and the `next` builtin route through `Table::next`,
  so the standard idiom breaks:

```lua
local t = { a = 1, b = 2, c = 3, d = 4, e = 5 }
for k in pairs(t) do t[k] = nil end
print(next(t) == nil)   -- reference: true; dellingr: false (b..e survive)
```

This is a high-impact divergence for game scripts (filter-in-place loops).
Fix direction: dead-key semantics - on remove, keep the key with a hidden
tombstone until a safe compaction point (insert of a colliding key, explicit
rebuild, or GC), with `get`/`insert`/`mark_values`/`pairs` skipping
tombstones; or have `next` fall back to a per-table "last removed key ->
successor index" memo (version-checked). Whatever the mechanism, it must stay
deterministic and preserve insertion order for untouched keys.

### B3. `NaN <= x` and `NaN >= x` evaluate to `true`

- `src/vm/eval_store.rs:484-509` (`eval_compare`) maps incomparable pairs to
  `Ordering::Equal` via `partial_cmp(..).unwrap_or(Equal)`. `frame.rs:277-278`
  implements `<=` as `!(a > b)` (`eval_compare(Greater, negate=true)`) and
  `>=` as `!(a < b)`.
- With NaN: `cmp` becomes `Equal`, `Equal == Greater` is false, negation makes
  the result TRUE. Reference Lua: every order comparison involving NaN is
  false.

```lua
print(0/0 <= 1)   -- dellingr: true; lua5.2/5.4: false
print(1 >= 0/0)   -- dellingr: true; lua5.2/5.4: false
```

Fix: treat `partial_cmp() == None` as "always false regardless of negate"
(return false before the negate step). `<`/`>` are unaffected (Equal matches
neither target).

### B4. `table.insert(t, pos, v)` reverses `pairs` order

- `Table::array_insert` (`src/vm/table.rs:410-433`) shifts by
  `shift_remove(key i)` + `insert(key i+1)` from high to low. Each re-insert
  appends at the END of the IndexMap, so the shifted keys end up in reverse
  order, followed by the newly inserted key:

```lua
local t = { 10, 20, 30 }
table.insert(t, 1, 99)
for k, v in pairs(t) do print(k, v) end
-- reference 5.2/5.4: 1 99 / 2 10 / 3 20 / 4 30
-- dellingr:          4 30 / 3 20 / 2 10 / 1 99
```

  (`array_remove` happens to preserve order because its forward loop re-appends
  in ascending key order.)
- Same routine is also O(N^2): each `shift_remove` is O(tail) in IndexMap, and
  there are O(N) of them - see O1, which fixes both the order and the cost.
- Related cache nit: for a non-sequence (`t = {1,2,3, [5]=5}` where the border
  is 3), `array_insert` unconditionally sets `cached_array_len = len + 1 = 4`,
  but after the shift `t[5]` is non-nil so 4 is not a border; `#t` then
  returns a non-border value until invalidated. Edge case, but the cache write
  is simply wrong for tables where `t[len+1]` was occupied; safest is to only
  trust `Some(len + 1)` when `get(len + 2)` is nil, else `set(None)`.

### B5. Default `table.sort` silently orders mixed/incomparable types

- `src/vm/table_ops.rs:328-346`: without a comparator, numbers sort before
  strings and anything else compares `Equal`. Reference Lua raises "attempt to
  compare number with string" (and errors on tables/booleans).

```lua
table.sort({ 1, "a", 2 })   -- reference: error; dellingr: succeeds
table.sort({ {}, {} })      -- reference: error; dellingr: no-op "sorted"
```

Since "errors kill the callback" is the product stance, silently succeeding is
the divergence here, not the erroring. Fix inside the comparator closure:
return a type error (needs the `sort_by` restructured to a fallible sort or a
pre-scan).

### B6. `next(t, k)` with a key not present in the table returns nil instead of erroring

- Reference Lua: "invalid key to 'next'". dellingr's `Table::next` returns
  `(nil, nil)`, which is indistinguishable from end-of-iteration
  (`src/vm/table.rs:512-548`). Low priority, but it also masks B2.

### B7. `print`/`tostring` accept a non-string `__tostring` result

- `to_string_with_meta` (`src/vm/table_ops.rs:363-392`) stringifies whatever
  `__tostring` returns; its sibling `bytes_with_tostring_meta` (line 396-429)
  correctly errors with "'__tostring' must return a string". Reference Lua
  errors in both. `print` and the `tostring` builtin use the permissive one
  (`lua_std/basic.rs:132, 223`).

```lua
print(setmetatable({}, { __tostring = function() return {} end }))
-- reference: error; dellingr: prints "object: ..."
```

Fix: fold `to_string_with_meta` over `bytes_with_tostring_meta` (one code
path, one behavior).

### B8. `__index` bottoming out in a string value errors instead of chaining

- `handle_index_metamethod_inner` (`src/vm/metamethod.rs:88-137`) accepts only
  table/function/RustFn handlers; a string `__index` (legal in reference Lua,
  where indexing re-dispatches on the string) raises a type error. Exotic;
  note-only given the "no string metatable" design, but the error message
  ("attempt to index a table value" via `typ_simple`) is doubly wrong.

### B9. Error messages misreport functions as tables

- `Val::typ_simple` (`src/vm/lua_val.rs:94`) maps every `Obj` to
  `LuaType::Table` "for display purposes". Indexing a Lua function
  (`local f = function() end; return f.x`) reports "attempt to index a
  *table* value". Cosmetic, but it shows up in user-facing errors and in
  diff tests against reference messages.

---

## C. Determinism

### C1. `tostring` on a Rust function leaks ASLR addresses into script-visible output

- `to_string_with_heap` / `to_bytes_with_heap` (`src/vm/lua_val.rs:105, 117`)
  format `RustFn` as `<function: {func:p}>` - the real function address.
  Two identical runs of the same script on the same host produce different
  output under ASLR, and scripts can branch on the string
  (`tostring(print):find("7")`), so this is replay-visible, not just cosmetic.
- `TODO.md`'s "Stable RustFunc identity" entry claims "nothing observable
  depends on its stability" - that is not true for `tostring`. The
  `format_pointer_ids` mechanism (deterministic `%p`) already exists; routing
  the `tostring`/`print` rendering of RustFn through the same deterministic id
  (requires threading `&mut State` or pre-assigning ids at registration) would
  close it. Lua closures are fine (slotmap key rendering is
  history-deterministic).

### C2. Verified determinism-clean (no findings)

- `StringPool.hash_index` uses `IndexMap` keyed by pinned FxHash values;
  iteration only during sweep; insertion-order semantics keep it honest.
- Anchor `state_id` comes from a process atomic (nondeterministic) but is
  documented and never script-observable.
- Registry/slotmap iteration orders are insert/release-history deterministic.
- Recently-fixed items re-scrutinized: interned strings counting toward the GC
  threshold (`allocation_count`, `is_full`, `collect` recompute) is coherent;
  the `usize::MAX` auto-GC-disabled sentinel is correctly preserved across
  explicit collections, and `saturating_mul(2).max(20)` cannot accidentally
  produce the sentinel for any real heap size.

---

## D. Cost-model integrity

### D1. `table.move` performs unbounded uncharged work

- `src/lua_std/table.rs:255-307` charges a flat 1, then loops `e - f + 1`
  times doing `get_table` + `set_table_raw` (both free). The range comes
  straight from script arguments and is not bounded by table size:

```lua
table.move({}, 1, 2^30, 1)   -- ~10^9 table ops, cost charged: 1
```

  No allocation is needed (reads of an empty table, writes of nil are
  removals), so memory caps do not save the tick budget. Fix: charge
  `count.max(1)` up front, exactly like `table_sort` does (its L18 comment is
  the template), and consider clamping `e` to a sane bound.

### D2. `table.concat` charges 1 for O(j - i) work and output

- `src/lua_std/table.rs:194-250`: `table.concat(t)` over a large array does
  `len` lookups and builds an arbitrarily large byte vector for cost 1. Charge
  per element (and/or per output byte) before the loop.

### D3. `OP_CONCAT` is free, so string memory grows exponentially at near-zero cost

- `frame.rs:304-307` deliberately makes concatenation free;
  `concat_helper` (`eval.rs:228-263`) then allocates the full result:

```lua
local s = "x"
for i = 1, 34 do s = s .. s end   -- 16 GB attempt, total charged cost ~34
```

  The `while true do end` free-structural-ops trade-off is documented in the
  README, but that argument is about *time* on control flow; this one converts
  ~34 cost into gigabytes of allocation, which is a different failure domain
  (OOM kill of the host). Recommendation: keep short concats free if desired,
  but charge `total_len / K` for some K (the SET_LIST per-element precedent
  shows dynamic charging is already in the design vocabulary).

### D4. `table.insert`/`table.remove` are charged 1 for O(N^2)/O(N) work

- `OPTIMIZATIONS.md` already tracks this class but understates it as "O(N)
  shifts charged as 1": `array_insert` is actually O(N^2) because each of the
  O(N) `shift_remove`s is itself O(tail) in IndexMap (`src/vm/table.rs:410`).
  A single `table.insert(t, 1, v)` on a 50k-element table is ~10^9 memmoves
  for cost 1. O1 below reduces the work to O(N); the residual O(N)-charged-1
  gap then matches the already-tracked entry (fix by charging the shifted
  count, as `table_sort` does).

### D5. `table.sort` with a comparator does O(N^2) comparator calls charged N

- Bubble sort (`table_ops.rs:304-326`) always runs the full N(N-1)/2 rounds
  (no early-exit swap flag, and each round re-calls even when sorted). The
  comparator call itself is free by design and a trivial `return a < b` body
  charges ~0, so `table.sort` on 10k elements = ~5*10^7 full call-machinery
  round trips for cost 10^4. See O2.

### D6. Charging-order spot checks (clean)

- `table_sort` charges `n.max(1)` BEFORE the comparator runs or the table is
  mutated (correct per the L18 contract).
- `add_cost!` batching in `frame.rs` respects the budget boundary: the flush
  condition `cost_remaining - next <= 0` forces an immediate `consume_cost`,
  which errors before the op only when the budget was already exhausted -
  matching "the op that pushes you over completes; the next costed op fails".
- Minor: `local_cost` accumulated but not yet flushed is dropped on error
  paths, so `cost_used` can undercount by <64 on a killed callback.
  Harmless given errors kill the callback.

---

## E. GC / heap smaller notes

- E1 (latent hazard pattern): `set_table_str_key_value`
  (`src/vm/table_ops.rs:37-55`) interns `name` (which can run GC) while the
  `val` parameter is held unrooted. Every current caller passes `RustFn`/`Num`
  (verified in `lua_std/{basic,math,table,string}.rs` install paths), so it is
  not live today, but the signature invites a future use-after-free: any
  caller passing a fresh `Val::Obj`/`Val::Str` loses it if the intern
  triggers a collection. Either root `val` on the stack inside the helper or
  document the invariant on the function.
- E2: `alloc_string` runs the `is_full` check and potential full collection
  even when the string is already interned. Correct, but it means a hot loop
  that only touches existing strings can still pay a whole mark+sweep at the
  threshold boundary with zero reclaimable garbage; the threshold then doubles
  and the loop continues. Behavior is amortized-fine; noting to preempt
  "GC runs with nothing to collect" confusion in profiles.
- E3: upvalue pool never frees slots (documented). A closed upvalue whose
  closure died retains a stale `Val` forever; it is never marked and never
  read again (refs only flow from closures), so it is a bounded leak, not a
  safety issue. Fine as designed.
- E4: anchors: registry is correctly in the root set; generational keys +
  `state_id` make stale/cross-state handles error cleanly; `anchor()`
  validates before popping. No findings. One nit: `call_anchor`'s
  `insert_at` check only guards against underflowing the whole stack, not
  against inserting below `stack_bottom`; a host that lies about `args` can
  corrupt a caller frame's slots. Suggest `checked_sub` against
  `get_top()` instead of `stack.len()`.

---

## F. Boundary observations (design-confirmation, not bugs)

- F1: `t.foo` on a table without the field falls back to the `table` library
  (`instr_get_field` -> `push_table_library_field`, `eval_index.rs:37-39,
  350-363`), so `({}).insert == table.insert`. Deliberate (OPTIMIZATIONS.md
  "Table-library fallback IC" presupposes it), but note the asymmetry:
  `t["insert"]` via `OP_GET_TABLE` does NOT fall back (`instr_get_table` has
  no library fallback), so `t.insert ~= t["insert"]`. Worth one line in the
  README if it is contract; it also means every missed field read on a
  metatable-less table pays a `table`-library probe (see O5).
- F2: `_G` proxying, `pairs(_G)` iterating the near-empty proxy table, and
  globals never being physically removed (nil stored instead) are consistent
  with the documented `_G` design; index-based global ICs stay valid because
  `globals` is append-only and existing-key inserts keep indices.

---

## G. Optimization opportunities

Ordered by expected real-throughput impact; each is labeled local vs rewrite.
None of these are already tracked in OPTIMIZATIONS.md/TODO.md except where
noted as extending a tracked entry.

### O1. Rewrite `array_insert`/`array_remove` as value rotation (local; also fixes B4/D4)

Instead of `shift_remove(key i)` + re-insert (O(N^2), order-scrambling),
rotate VALUES across the existing dense key range: the keys `pos..=len` stay
at their current indices; only their values move by one. Sketch: resolve the
index of each integer key once (`get_index_of`), then walk the dense prefix
with `set_at_index`, ending with one real insert/remove at the boundary key.
O(N) with no rehashing, preserves insertion order exactly (reference-Lua
iteration order for sequences), and the shifted count is available to charge
(D4). This is independent of and much cheaper than the tracked
"Array part for dense integer keys" storage split, and remains worthwhile
even if that lands later.

### O2. Replace `table_sort` wholesale (local-to-module rewrite; fixes A4/B5/D5)

Sort in place over the table's dense prefix (or over a rooted scratch region
on the VM stack) with a deterministic O(N log N) algorithm:

- comparator path: bottom-up merge sort (stable, deterministic comparison
  sequence, no recursion) calling the Lua comparator; charge
  `n*ceil(log2 n)` or charge per comparison as they happen.
- default path: same algorithm with a fallible primitive comparator that
  errors on mixed/incomparable types (B5).

Sorting in place (values re-read from the table between comparator calls, or
scratch rooted) removes the detached-Vec GC hazard (A4) without a new root
mechanism. Reference Lua's introsort comparison order is not observable to
conforming comparators, so algorithm choice is free as long as it is
deterministic; document that comparator side effects observe a different
(but fixed) comparison sequence than C Lua.

### O3. Stop cloning `Closure` on every Lua call (local, hot path)

`State::call` -> `Val::as_lua_function` -> `GcHeap::as_lua_function`
(`src/vm/object.rs:237-242`) deep-clones the `Closure`, including
`upvalues: Vec<UpvalueRef>`, on every single Lua->Lua call. For closures with
captured upvalues that is a heap allocation per call; for all calls it is
avoidable copying. Options, in increasing invasiveness:

- change `Closure.upvalues` to `Arc<[UpvalueRef]>` (clone becomes a refcount
  bump; `RuntimeCaches` and `Bytecode` are already Arcs, making the whole
  clone trivially cheap);
- or restructure `eval_closure` to take `ObjectPtr` and borrow the closure
  from the heap per access (bigger borrow-model change; the Arc variant gets
  ~all the win for a 3-line diff).

Benches `calls/*` should move; `alloc/closure` guards against regression.

### O4. Version-validated cursor for `pairs` (local; pairs hot path)

`instr_tfor_call_next` (`eval_control.rs:186`) calls `Table::next(&control)`,
which does a full hash lookup (`get_index_of`) of the control key on every
iteration - `pairs` over an N-entry Map table is N hash probes. The TFOR fast
path already special-cases `base_next` by fn address, so it can carry a hidden
numeric cursor: cache `(table_ptr, version, index)` in the loop's control area
(or a per-frame side slot); on each step, if ptr+version match, step
`get_index(index + 1)` directly; on mismatch, fall back to the key-based
`next`. Deterministic (same order as today), invisible to scripts, and it
composes with the B2 fix (a tombstone-aware `get_index` walk skips dead
entries without hashing). `iter/pairs` is already a headline bench (88ms,
2.1x lua5.5) - this removes the dominant per-step cost.

### O5. Compile-time table-library-name flag for GET_FIELD misses (local)

Every nil field read on a metatable-less table (`if t.flag then ...`) goes
through `push_table_library_field`: a `get_global("table")` plus a full
`get_table_with_key` on the library table (`eval_index.rs:37-39`). The field
id is a compile-time string literal, so the parser can precompute one bit per
field literal: "is one of the 7 table-library names". Misses on all other
names return nil immediately with zero lookups. The tracked "Table-library
fallback IC" entry optimizes the HIT case; this kills the much more common
miss case and is simpler (no cache invalidation at all - the name set is
static).

### O6. Iterative GC mark with explicit gray stack (local; fixes A5)

Direct fix for A5; also makes `mark` a flat loop that the hotpath
annotations measure honestly, and opens the door to reusing one scratch
`Vec<ObjectPtr>` across collections (zero allocation marking).

### O7. Full rewrite candidate: 8-byte NaN-boxed `Val`

`Val` is currently 16 bytes (tag + f64). NaN-boxing (payloads in the NaN
space of an f64: slotmap keys are 64-bit but their useful entropy fits 48-51
bits with an index/generation split; RustFn would need a registry index
rather than a raw pointer - which B1/C1 want anyway) halves stack/table/
upvalue memory traffic and makes `stack: Vec<Val>` copies twice as dense.
This is the "full coherent rewrite" class: it touches every `match` on `Val`,
but it is mechanical, determinism-neutral, and is the standard reason
reference VMs beat tagged-enum interpreters on memory-bound workloads
(`tables/fill`, `fields/*` are exactly the 3-4x-behind benches). Prerequisite:
the RustFn-registry-id change (B1/C1 fixes), which is independently needed.
If NaN-boxing is judged too invasive, a cheaper intermediate is boxing only
`Table` storage entries more densely; but the full version is where the
payoff is.

### O8. Micro (take or leave)

- `Table::get_with_index` on Map does `get_index_of` + `get_index` (two
  probes); IndexMap's `get_full` does it in one.
- `try_insert_table_direct` does `get(&key)` then `insert` (two probes) on
  the no-metatable... metatable-present path only; the no-metatable path is
  single-probe already. Fine.
- `promote_to_map` allocates capacity `INLINE_CAPACITY + 1 = 5`, guaranteeing
  a rehash almost immediately for growing tables; promoting straight to 8
  avoids one rehash on the common grow-past-inline path.

---

## Summary table

| # | Sev | Class | One-liner |
|---|-----|-------|-----------|
| A1 | high | panic | `next(t, 0/0)` hits NaN hash assert, aborts process |
| A2 | high | GC/panic | `with_restricted_env` saved env escapes root set, swept, dangling builtins |
| A3 | high | GC/panic | `#t` with `__len`: receiver unrooted across `alloc_string` GC |
| B1 | high | correctness | RustFn Eq/Hash compare payload addresses; `print == print` is false |
| B2 | high | correctness | `t[k] = nil` inside `pairs` ends iteration early |
| A4 | med | GC/panic | `table.sort` comparator runs with array outside root set |
| B3 | med | correctness | `NaN <= x` / `NaN >= x` evaluate true |
| B4 | med | correctness | `table.insert(t, pos, v)` reverses `pairs` order (and is O(N^2)) |
| A5 | med | abort | recursive GC mark overflows stack on deep nesting |
| D1 | med | cost | `table.move` unbounded uncharged work |
| D3 | med | cost | free `OP_CONCAT` allows exponential memory for ~0 cost |
| D2 | med | cost | `table.concat` charges 1 for O(n) work |
| D5 | med | cost/perf | bubble-sort comparator path O(N^2) charged N |
| C1 | med | determinism | `tostring(print)` output varies run-to-run under ASLR |
| B5 | low | correctness | default sort accepts incomparable types silently |
| B7 | low | correctness | `__tostring` non-string result accepted by print/tostring |
| B6 | low | correctness | `next` with invalid key returns nil, no error |
| B8/B9 | low | correctness | `__index`-string chaining; functions reported as "table" in errors |
| E1 | low | latent | `set_table_str_key_value` val unrooted across intern |
| E4 | low | host API | `call_anchor` can insert below caller's stack_bottom |
