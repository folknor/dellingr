# Bugs: VM runtime, heap, persistence and host API

Filed from the defect hunt at b83eab8, from three scopes: vm-exec (`src/vm.rs`,
`src/vm/eval*.rs`, `frame.rs`, `stack.rs`, `metamethod.rs`, `cost_meter.rs`,
`limits.rs`, `error.rs`), vm-heap (`object.rs`, `lua_val.rs`, `table.rs`,
`table_ops.rs`, `anchor.rs`, `save_state/**`) and embedding (`lib.rs`,
`host.rs`, `main.rs`, `rng.rs`, the public `State` surface). Where one finding
came from more than one scope, the entry says so. None of the hunters ran
anything.

```
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
```

## VM-001 - A field miss on a plain table reads from the global `table` library, and errors when `table` is not a table

Source: vm-exec (hunter's label: HIGH).

Where: `eval_index.rs` `instr_get_field` -> `push_table_library_field` /
`push_table_library_field_slow`.

`GET_FIELD` (`t.k`, `t:k(...)`) on a table first looks up the key in the table.
If the key is missing and there is no metatable, or the `__index` chain yields
nil, it falls through to `get_global("table")` and looks the key up there.

- **Wrong values.** `local t = {} print(t.insert)` prints a function, where
  reference prints `nil`. The same goes for `remove`, `sort`, `concat`,
  `unpack` and every other table-library name, and for objects whose class
  `__index` lacks the name. So `if obj.remove then obj:remove() end` calls
  `table.remove(obj)`. Through the `_G` proxy (its `__index` function returns
  nil, so the fallback runs), `_G.insert` is also non-nil.
- **Inconsistent indexing.** `t["insert"]` (`GET_TABLE`), `rawget`, the host
  `State::get_table` and `pairs` all see nil, while `t.insert` sees a function.
  So `t.k` and `t["k"]` are no longer the same expression.
- **Errors on ordinary code.** If the global `table` is not a table, every field
  miss on any plain table errors: the VM pushes `get_global("table")`, and
  `get_table_with_key` on a non-table returns `TypeError::TableIndex`. Two ways
  to get there:
  - The script does `table = nil` (or `= 5`).
    `invalidate_table_library_fallback_rebind` drops the cache, so every miss
    takes the slow path, and `local t = {} if t.x then end` fails with "attempt
    to index a nil value".
  - The host uses `State::with_restricted_env` with a whitelist that omits
    `"table"`. The Table builtin becomes Nil, the fast-path check
    `builtins[Table] == cache.table` fails, and every plain-table field miss in
    the sandbox errors.

Claim broken: output matches reference Lua except for divergences marked
`-- DIFF:`. No example marks this, and neither the README nor `reference/`
mentions it. The only description is the header of `examples/fields/miss.lua`
("exists only to support `t:insert(...)` sugar"). The error case also breaks
`with_restricted_env`'s purpose: restricting the environment should hide
globals, not make indexing fail.

Hunter's direction: delete the fallback. If the `t:insert` sugar is a product
requirement, document it in the README as a DIFF and make it never error, so a
missing or non-table `table` global means "no fallback". The `fields/miss` probe
bench and the `table_library_fallback` cache machinery exist only to make the
fallback cheap, and both go with it.

## VM-002 - A runtime error discards pending batched opcode cost

Source: vm-exec (MEDIUM) and embedding (finding 6). Both describe the same
mechanism.

Where: `frame.rs` `Frame::eval`, where `local_cost` is a stack local.

Costed opcodes add to `local_cost`, which is flushed only at the
`COST_CHECK_INTERVAL` (64), near budget exhaustion, before `OP_CALL`,
`OP_TFOR_CALL` or a metamethod call, and at `OP_RETURN`. Any other `Err`
leaving `eval` discards the pending amount, up to 63 units of already-executed
work. Examples are an arithmetic type error, indexing nil, a stack overflow, a
string-size error, an `error()` reached without a flush, or a concat failure.

Repros given:
- `local a, b = 1, nil  local x = a + a  local y = a + b` charges 2 units, but
  `cost_used()` after the error reports 0 (vm-exec).
- `local a = 1 + x; local b = nil .. 1` with `x = 2` reports `cost_used() == 0`
  even though the add ran (embedding).

Claims broken:
- The exact cost model.
- `cost_used()`: "total cost consumed since the last budget reset".
- `COST_MODEL_VERSION`: "moves whenever `cost_used` for identical source
  changes". Changing the public `limits::COST_CHECK_INTERVAL` changes
  `cost_used` for erroring scripts without a version bump.
- The `Frame::COST_CHECK_INTERVAL` doc promises the interval does not change
  `cost_used`. It argues this only for `BudgetExceeded`.

Consequences: hosts that bill a killed callback's `cost_used()` undercharge, by
an amount that depends on a batching constant rather than on the script. The CLI
prints the undercounted `Cost used` for every failing script, and a snapshot
taken after a killed callback persists the undercount. `tests/cost_golden.rs`
pins only successful runs.

Related (embedding): `set_cost_budget` called from inside a callback resets
`cost_used` to 0 while the outer `Frame::eval` still holds unflushed
`local_cost`, which is then charged against the new budget. So `cost_used`
around a mid-run budget reset also depends on batching. The doc does not say
whether mid-run resets are supported.

Direction (both hunters): flush `local_cost` on `eval`'s error exit, either with
a guard in `eval` or by passing it out to `eval_closure_frame`'s / the
`report_frame_error` caller's `Err` arm. Use a plain `saturating_add` with no
admission check, so the error path stays cold. The failing opcode was already
admitted and executed, so charging it is consistent with "the action that pushes
you over budget completes".

## VM-006 - Host table and metatable methods consume their operands before they fail

Source: embedding (finding 4) and vm-exec (finding 7, second bullet). vm-exec
read the README sentence as covering only stack-growth rejections, and so called
this a smell rather than a broken contract. Embedding argues that `table_next`
breaks the sentence under either reading.

Claim broken (embedding's reading): README "a rejected operation leaves the
stack exactly as it was", and the same statement in AGENTS.md for the host API.

- `table_next` pops the key and then runs `check_stack_space(2)`. On
  `StackOverflow` the key is gone, so a host iteration loop can neither retry nor
  resume. The same happens on `InvalidKey` and on a non-table type error.
- `get_table` and `get_table_raw` pop the key before the type check and before
  `__index` dispatch. Indexing a non-table, or an `__index` error, loses the key.
- `set_table_raw` pops value and key, then fails on a non-table or on
  `t.insert` (nil or NaN key). Both operands are lost.
- `set_metatable_of` pops the metatable, then fails on a non-table target or a
  non-table metatable.
- `table_insert_at` pops the value, then fails on a non-table.

`set_metatable_of` and `set_table_raw` carry comments about validating "before
popping", but they validate only the stack shape, not the types.

Direction: peek, validate everything, and pop only on commit, as
`concat_helper` already does ("Intern before truncating").

Structural direction (embedding, covering this entry, VM-007 and the
since-fixed `set_global` / `call_anchor` / host `Dynamic` call cases):
the host API is a C-API-style set of stack-twiddling methods, each
re-implementing "validate, then pop" by hand, and the same methods double as
stdlib internals with stdlib-only preconditions. Split it into a narrow, fully
validated host facade (frame-relative indexing, peek-then-commit, no `Dynamic`)
and `pub(crate)` internals (`table_*_at`, `table_sort`, unvalidated
`set_global`, the dynamic call).

## VM-007 - `table_insert_at`, `table_remove_at` and `table_sort` are public but trust stdlib-validated inputs

Source: embedding (finding 5).

Claim broken: `table_insert_at`'s name and doc ("Inserts a value into a table at
a position, shifting elements"). Its doc admits "`pos` has been validated by
table.insert", but an embedder has no such validator.

`Table::array_insert` loops over `pos..=len` and then writes `carry` at `len+1`:
- For `pos > len + 1` the loop is empty, and the value lands at `len+1`, not at
  `pos`.
- For `pos == 0` the value is written at key `0`, and the old `t[0]` is shifted
  into `t[1]`.

`table_remove_at` silently returns nil for out-of-range positions, which at
least matches its stdlib caller. `table_sort`'s "uses the function at stack index
2 as comparator" is a hidden coupling to the stdlib calling convention. Its doc
says "Returns the array length for cost charging", but it already calls
`consume_cost` itself, so a host following the doc charges twice.

Direction: either validate `1 <= pos <= len+1` and return an error, or make these
helpers `pub(crate)`.

## VM-008 - `table.sort` writeback and `table.remove(t, 1)` are O(n^2) host work for a cost of n and 1

Source: vm-heap (finding 1) and stdlib (finding 1). The stdlib hunter's account
of the related `table.insert`, `unpack`, `pack` and `select` charges is in
STD-006.

Claims broken: the exact cost model. README "Budget": "Data-dependent native work
in the string, pattern, and table libraries is charged: ... table elements
visited". The flat-cost comment in `lua_std/table.rs` says the shift is "the
same per-element work table.move charges one per element for" and "a mid-array
call shifts every later element one key at a time", both of which assume linear
work.

- **`Table::set_array`** (the `table.sort` writeback) first calls `remove` on
  keys `1..=old_len`, leaving `dead_count = old_len` tombstones. It then calls
  `insert(i, v)` per sorted value. With `dead_count > 0`, every insert goes
  through `insert_into_map_with_tombstones`, which finds key `i`'s dead slot near
  the front and calls `map.shift_remove_index(index)`, an O(n) operation. The
  writeback is O(n^2), while `table_sort` charges `consume_cost(n.max(1))`.
  Sorting 10^5 elements is about 10^10 element moves for 10^5 cost.
- **`Table::array_remove`** calls `ensure_map`, then `map.shift_remove(&pos)`,
  then, for every `i in pos..len`, `map.shift_remove(&(i+1))` and
  `map.insert(i, v)`. `shift_remove` is O(map size), and each reinsert appends at
  the end, so the next key to remove is near the front again. One
  `table.remove(t, 1)` is O(n^2) at a flat cost of 1 (`consume_cost(1)`). A queue
  idiom (`table.insert(q, x)` / `table.remove(q, 1)`) on 1000 elements does about
  10^6 map shifts per call, and draining it is cubic in host time. This is a
  budget bypass.
- `table.remove(t)` (pop last) is O(entries after key `len` in insertion order),
  not O(1), whenever hash keys were inserted after the array keys.

Side effects:
- `ensure_map` permanently promotes small inline tables to `IndexMap` storage on
  their first `table.remove`.
- Every remove moves the shifted keys to the end, so `pairs` order changes. It
  stays deterministic but changes for no reason.
- The re-append order breaks `Table::get`'s positional fast path
  (`get_index(n-1)`) for any table with non-array keys before the array part.

Direction (both hunters): the root cause is that there is no array part. All
integer keys live in one `IndexMap`, which also explains the O(n)
`array_insert` and `get_array`/`set_array` hashing through `table.sort`. A real
Lua-style array segment (`Vec<Val>` for `1..=n` next to the hash part) would turn
`sort`/`insert`/`remove`/`#` into memmove/slice operations, keep `pairs` order
stable, and remove the positional fast path in `get`, the `compute_array_len`
binary search and the `cached_array_len` bookkeeping. Interim fixes:
`set_array` overwrites values in place (the keys already exist), and
`array_remove` overwrites `t[i] = t[i+1]` in place and removes only the last
key. If the flat cost is kept, the host work must at least be linear.

Constraint carried over from the `notes/todo.md` entry this replaces: the
insertion order of surviving keys is replay-visible through `pairs()`, so any
fix has to pin down what that order becomes and treat a change to it as a
determinism-visible change. `array_insert` should be checked for the same
quadratic shape.

## VM-009 - `table.remove` with `pos == 0` returns nil and leaves `t[0]`

Source: vm-heap (finding 2) and stdlib (finding 6).

Claim broken: output matches reference Lua. No `-- DIFF:` marks it.

`lua_std/table.rs` accepts `pos == 0` when `len == 0`, which matches the
reference argument check. `Table::array_remove` then does
`if pos > len || pos == 0 { return Val::Nil; }`. Lua 5.2 and 5.4 `tremove` read
and return `t[pos]` and then set `t[pos] = nil`, even when `size == 0` and
`pos == 0`. So `local t = {[0] = "x"}; print(table.remove(t))` prints `x` in
reference and `nil` here, and `t[0]` survives. (`table.remove(t, #t + 1)` with a
non-nil `t[#t+1]` would diverge in the same way, but that is impossible for a
border, so only `pos == 0` is reachable.)

## VM-010 - `tostring` mangles non-UTF-8 bytes returned by `__tostring`

Source: vm-heap (finding 3).

Claims broken: reference output parity, and the documented purpose of
`bytes_with_tostring_meta` ("preserving arbitrary bytes returned by a
`__tostring` metamethod").

The `tostring` builtin (`lua_std/basic.rs`) calls `State::to_string_with_meta`,
which does `String::from_utf8_lossy(&self.bytes_with_tostring_meta(idx)?)` and
then `push_string`. A `__tostring` returning `"\xff"` produces `"\u{FFFD}"`
(3 bytes), where reference returns the raw byte. The byte-preserving helper
exists, but the builtin bypasses it. `to_string_with_meta` is also public API,
and its `String` return type forces the same loss on hosts.

## VM-011 - Rebuilding a table in `clear_and_insert_entries` resets `version` to 0, so on load an inline cache can return the wrong field

Source: vm-heap (finding 5).

Claim broken: the table `version` contract ("Shape version for key/index
stability"), which every field, method and string-method inline cache relies on.
`get_cached_field`, `get_cached_index_table_field` and
`get_cached_string_method` return `get_index(entry.index)` without checking the
key when the versions match.

`clear_and_insert_entries` does `*self = Table::with_capacity(..)`, which resets
`version` to 0, and the re-inserts do not bump it because `dead_count == 0`.
During `load_state`, the env-delta `order` path rebuilds an environment table
(`math`, `string`, ...) in a different physical order, still at version 0. The
`setup` closure runs before materialization, can execute Lua, and can create
anchors, which are deliberately kept. If setup warmed an IC on that table at
version 0 (any unmodified library table is at version 0), the cached index now
points at a different key. A later call through the anchored function silently
returns the wrong library member. Only the `table` token's fallback cache is
dropped explicitly.

Direction: a rebuild keeps the old counter and bumps it past the old value
instead of resetting it. More generally, a per-State monotonic "shape epoch"
would make version collisions impossible by construction.

## VM-013 - Every `IndexMap` in the VM uses the default `RandomState` hasher

Source: vm-heap (finding 7).

Claim broken: the letter of AGENTS.md's lint gate ("`RandomState` and
`DefaultHasher` are banned"; "Host state is banned"). The gate only catches
explicit uses. `IndexMap<Val, Val>` (table storage), `globals`,
`format_pointer_ids`, `StringPool::hash_index` and every other `IndexMap::new()`
default to `S = RandomState`, which is seeded from OS entropy on each run.

The hunter found no observable output divergence, because iteration follows
insertion order. Bucket layout, and therefore probe counts, varies per process,
so performance is not reproducible. Every table lookup also pays for SipHash.

Direction: a crate-wide type alias using a fixed-seed fast hasher (Fx or foldhash
with a constant seed), which meets the stated rule and speeds up the VM's
hottest path.

## VM-014 - Snapshot verifier accepts duplicate table-template keys, which corrupts `Table::dead_count`

Source: vm-heap (finding 8).

Claim broken: the load contract that a saved bytecode program violating VM
structural invariants is rejected with `InvalidBytecode`.

`compiler/verify.rs` only range-checks template key ids. The parser guarantees
uniqueness (`try_use_table_template` refuses `template.contains(key_id)`), but a
forged save can supply `[0, 0]`, or two equal-content string literals `[0, 1]`
that intern to one `StringPtr`. `Table::with_template_keys` then builds a map
with one entry and `dead_count = 2`. From there:
- `insert_into_map_with_tombstones` computes `map.len() - self.dead_count`,
  which underflows: a panic in debug and test builds, a wrap in release.
- `init_at_index` and `remove` carry the phantom tombstone count, which
  suppresses compaction and skews `fallback_shape`.

Direction: the verifier rejects duplicate resolved template keys, and
`with_template_keys` sets `dead_count = map.len()` rather than
`key_ids.len()`.

## VM-015 - A saved table's metatable may resolve to a closure

Source: vm-heap (finding 9).

Claim broken: a table's metatable is always a table. `set_metatable_of` rejects
anything else with a type error, and `getmetatable` and every metamethod lookup
assume it.

`verify::validate_references` only range-checks the metatable `SavedVal`, and
`materialize_payload` only checks that it decodes to `Val::Obj(_)`. The
`SavedMetatableDelta::Set` path has the same gap. A save whose metatable is
`Obj(id)` with `objects[id]` a `SavedObject::Closure` loads without error.
`getmetatable(t)` then returns a function, and `as_table_ref(mt)` silently yields
`None` in the metamethod paths.

Direction: require `SavedObject::Table` (or an `EnvObj`) during verification.

## VM-016 - `next_format_pointer_id` is not validated against the saved ids

Source: vm-heap (finding 10).

Claim broken: `%p` / `tostring` identities are distinct per live value (the
"deterministic, state-local identity" in `format_pointer_id`).

`validate_pointer_ids` rejects duplicate saved ids, but does not require
`next_format_pointer_id` to exceed all of them. A forged save with
`next_format_pointer_id = 0` and an existing entry with id 0 gives the next fresh
value the same rendered address as a live one. Engine-produced saves always
satisfy the invariant, so this matters only for hostile or corrupted input. That
is exactly the input the verifier claims to guard against.

## VM-017 - Rust functions render as `<function>` and never get an identity

Source: vm-heap (finding 11) and stdlib (finding 15, first bullet).

Claims broken: `lua_val.rs`'s stated reason for having no `Display` ("Rendering
an object needs ... its deterministic identity"). `format_pointer_id`'s own
`debug_assert!`, which lists `Val::RustFn` as an expected caller. Reference
output parity.

`bytes_with_default_string_coercion` only routes `Val::Obj` through
`format_pointer_id`. So `tostring(print)`, `tostring(math.floor)` and
`string.format("%s", print)` all render as `<function>` (`RUST_FN_DISPLAY`),
while `%p` on the same value yields a `0x..` identity and Lua closures render as
`function: 0x..`. Two distinct builtins are indistinguishable by `tostring` but
unequal under `==`. Reference gives distinct `function: builtin: 0x..` /
`function: 0x..` strings.

Direction: either give RustFns pointer ids in
`bytes_with_default_string_coercion`, or stop the `debug_assert!` and the
identity comment from claiming that they have them.

## VM-018 - A zero `for` step silently skips the loop

Source: vm-exec (finding 5, LOW). The stdlib hunter reports that the README's
"Compatibility divergences" section lists the zero-step `for`. The vm-exec
hunter reports that the README does not mention it. The two readings disagree.

Where: `eval_control.rs` `check_numeric_for_condition`.

`for i = 1, 10, 0 do ... end` runs zero iterations. Lua 5.4 raises "'for' step is
zero", and Lua 5.2 loops forever. dellingr matches neither.

Claim broken (vm-exec): output matches reference or is marked `-- DIFF:`. No
example covers a zero step. The behaviour may be deliberate, since an infinite
free loop is a budget hole, but if so it needs documenting; otherwise it should
become the 5.4 error.

## VM-019 - Runtime 255-value caps on Lua returns and dynamic calls are documented as Rust-callback limits

Source: vm-exec (finding 6, LOW).

Where: `error.rs` `results_count`; `limits::MAX_RESULTS` and `MAX_ARGUMENTS`.

`results_count` ("Validate a result count against the Rust callback ABI") is also
applied in `eval_closure_frame` to every Lua `return f()` / `return ...` with
`RetCount::All`. `call`'s Dynamic arm caps `f(table.unpack(t))` at 255 arguments
the same way. So `return table.unpack(t)` with 256 or more elements raises
`TooManyResults` in pure Lua code.

Claims broken: `MAX_RESULTS` is documented only as "Maximum number of values a
Rust callback can return". README "Runtime limits" lists only the fixed-count
compile-time caps. Reference Lua accepts these programs.

Direction: either document the runtime cap for Lua frames, or widen the internal
counts to `usize`. The `u8` exists for the bytecode operand encoding, which these
dynamic paths do not need.

## VM-020 - Metamethod function handlers flush cost after pushing their temporaries

Source: vm-exec (finding 7, first bullet).

`metamethod.rs` `handle_index_metamethod_inner` and
`handle_newindex_metamethod_inner` (function handlers) push the handler, table,
key and value, and only then call `Frame::flush_local_cost(..)?`. When reached
from bytecode, a flush failure leaves 3-4 temporaries on the stack. That is
harmless today because the frame unwinds, but the ordering contradicts the
"preflight before any mutation" comment directly above it. Flushing before the
pushes makes the path strictly all-or-nothing.

## VM-021 - `ArgCount::Fixed(255)` is accepted by `call` but collides with the `Dynamic` sentinel

Source: embedding (smaller item).

`call` accepts `ArgCount::Fixed(255)` (`MAX_ARGUMENTS = 255`), but that value
cannot round-trip through `to_u8`, because 255 is the `Dynamic` sentinel. This is
harmless today because host calls never encode. The public `State::call` now
rejects `Dynamic` at runtime (bytecode uses an internal `call_op`), but hosts
can still name `ArgCount::Dynamic`. Both point to a separate host-facing
argument type with neither a sentinel nor `Dynamic`.

## VM-022 - The host `get_table` disagrees with the `t[k]` opcode on string receivers

Source: vm-exec (finding 8, LOW).

`State::get_table` goes through `get_table_with_key`, which raises
`TableIndex(string)` for a string receiver. `OP_GET_TABLE` / `OP_GET_FIELD`
resolve string receivers through the `string` library. The method's name and doc
("As in Lua, this function may trigger a metamethod for the index event") promise
`t[k]` semantics, so for `("abc")["upper"]` the host sees a different result
from the script.

## VM-023 - `State::concat(n < 2)` reports the wrong kind of error

Source: vm-exec (finding 9, LOW).

`concat` rejects `n < 2` with
`ArgError { arg_number: 1, func_name: "concat", detail: TypeMismatch, expected: None }`,
which renders as "bad argument #1 to 'concat' (value expected)". The problem is a
count, not a type or a missing value, and argument #1 is not a Lua argument.
`ArgDetail::TypeMismatch`'s own doc ("Type or presence mismatch, described by
expected and received") does not fit. `InvalidStackIndex` or `OutOfBounds` would
be accurate.

## VM-024 - `Error` Display can pair the selected frame's line with the innermost error's column

Source: vm-exec (finding 10, LOW).

In `error.rs` `impl Display for Error`, when the selected `prefix_frame` has a
line but no source, it prints `"{frame.line}:{self.column}"`. The comment above
insists on taking "the source and the line from the SAME record", but the column
still comes from the innermost error. With `error(msg, 2)` (`TraceFrame(1)`) and
an unnamed chunk, the output pairs the caller's line with the callee's column.
Runtime errors always have column 0 today, so the visible effect is a spurious
`:0`, but it is the exact splice the comment warns against.

## VM-025 - `table_len` swallows a bad index and returns 0

Source: vm-heap (smaller items).

Every other public index API returns `InvalidStackIndex`. `table_len` takes
`&self` and returns `usize`, so a host bug reads as "empty table".

## VM-026 - Smaller heap items: dangling tombstone keys, `array_insert` version ordering, quadratic env-delta encoding, allocator GC note

Source: vm-heap (smaller items).

- **Tombstone keys are not GC-marked but stay in storage.** `mark_values` skips
  the key of a nil-valued slot, so a dead slot's `StringPtr`/`ObjectPtr` key is
  freed and dangles. Every current consumer only compares or hashes keys (safe
  with generational keys) or filters dead slots first (`entries`,
  `live_string_keys`). Future code that walks raw slots and dereferences a key
  (`heap.get_string(key)`) will panic. The invariant ("dead-slot keys may dangle;
  never dereference them") is not written down and belongs as a comment on
  `TableStorage`.
- **`array_insert` calls `compact_dead()` without bumping `version`.** The later
  `bump_version()` at the end of the function covers it, but only because nothing
  reads caches in between. That ordering is fragile, since `compact_dead` says
  "The caller owns version changes".
- **`encode_env_deltas` is O(n^2) per environment table**, through linear
  `iter().any(...)` / `find(...)` across the baseline and live entries. Scripts
  can grow `string`/`math`/`table` without bound, so save time is quadratic in
  user additions to library tables.
- **`GcHeap` allocator note and `is_full` naming.** The allocators carry "Note:
  Caller must check is_full() and run GC if needed before calling", but
  `load_state`'s materialization deliberately ignores it, and the
  `alloc_table_with_capacity` / `with_template` variants lack the note. A typed
  "allocation without collection" API (a `NoGc` token) would make the load-time
  exception explicit instead of comment-borne.
- **Non-ASCII characters in `table.rs` comments.** `≤` ("Small tables (≤4
  entries)" and similar) and `→` in a test comment. The hunter was unsure whether
  the gremlins scan covers them. Either way they contradict the spirit of the
  banned-Unicode rule.

## VM-027 - Concat charges nothing for the bytes it emits

Source: vm-exec (observations; flagged by the hunter as probably deliberate).

`OP_CONCAT` charges nothing for the bytes it emits, while README "Budget" says
data-dependent string work ("bytes examined or emitted") is charged for the
string and table libraries. `..` is an operator, not a library call, but a
`s = s .. s` doubling loop is bounded only by `MAX_STRING_BYTES` at the price of
one `OP_CONCAT` per step. The hunter recommends a README sentence either way.

## VM-028 - `__index`/`__newindex` function handlers count against `MAX_METAMETHOD_DEPTH` while `__call` handlers do not

Source: vm-exec (observations; flagged as not a defect against reference).

The comment in `call` explains that guarding `__call` handlers "would cap
legitimate recursion". `__index`/`__newindex` function handlers still count
against `MAX_METAMETHOD_DEPTH` (200), so a memoising `__index` that recurses
through its own table (`fib[300]`) stops at depth 200 even though every level is
a real Lua frame. Reference 5.4 also stops near 200 (C stack), so the behaviour
matches the reference, but it contradicts the stated `__call` rationale.

Also noted: the `__call` path in `call` preflights 2 slots after already removing
the callable, though it needs only +1 net, so a call exactly at the stack cap is
rejected even though it would fit. The hunter called this harmless.

## VM-029 - Embedding-surface smaller items: `ScopeCost` labels, anonymous scope numbering, README `Anchor` bounds

Source: embedding (smaller items).

- **`ScopeCost` display**: `function_calls` counts only `OP_CALL`, not
  `OP_TFOR_CALL` or metamethod calls, yet the summary labels it "Function calls".
  `fmt_indent` never prints a per-scope call count, so the per-scope breakdown
  and the totals show different fields.
- **Anonymous scope names** use `anonymous #{i+1}`, where `i` indexes all nested
  chunks, named ones included. Two anonymous functions with a named one between
  them become `#1` and `#3`.
- **README vs anchor doc**: the README lists `Anchor` as `Copy + Send`, while the
  anchor module doc says `Copy + Send + Sync + 'static`. They do not contradict
  each other, but the README understates the guarantee.

## VM-030 - String methods resolve through the current global `string`

Source: vm-exec (observations; flagged by the hunter as not a defect because the
function's doc states it). Related to, but a different mechanism from, STD-012
(strings have no metatable).

`get_string_table_field` resolves string methods through the current global
`string`. In a `with_restricted_env` sandbox without `"string"`, `s:upper()`
raises "attempt to index a nil value", whereas reference Lua uses the string
metatable. This is sandbox-hostile in the same way as VM-001, and the hunter
recommends making a deliberate decision about it.

## VM-031 - Globals set to nil are never removed from `globals`

Source: stdlib (finding 7, related smells; the hunter flagged it as outside
stdlib scope).

`_G.k = nil` (or any global set to nil) inserts a `Nil` entry into `globals`
through `set_global_value_owned` and never removes it. A script cycling through
generated global names grows the map without bound.

## VM-032 - `call_anchor` errors bypass `State::error`, unlike `call`

Source: lateral finding from a resolution-loop fixer.

`call_anchor` (`src/vm/anchor.rs`) builds its errors with
`Error::without_location`, so they skip `self.error(...)`, while `State::call`
reports through `self.error`. The two host entry points therefore report
failures inconsistently: an anchor-call error carries no location and may not
reach the host's `on_error` notification. The wave reviewer read it more
broadly: the preflight errors of direct `State::call` also bypass `on_error`,
because `State::error` only constructs an error and does not notify. The
changelog describes `on_error` as firing for every exit.

## VM-033 - `from_bytes_try` can no longer fail, but its `Result` and the gmatch compile-error cache remain

Source: lateral finding from a resolution-loop fixer.

Pattern validation is now lazy (the first malformed point compiles to an
`Item::Fail` raised at match time), so `patterns::from_bytes_try` always
returns `Ok`. It keeps its `Result` signature only so callers outside the
patterns module kept compiling. `src/vm.rs` (around `memoize_gmatch_pattern`)
still caches a compile error that cannot occur. Make the compile infallible and
delete the dead error path.
