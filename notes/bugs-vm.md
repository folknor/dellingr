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
metatable. This is sandbox-hostile in the same way as the (since-removed)
plain-table fallback to the global `table` library was, and the hunter
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

## VM-034 - `set_cost_budget` from inside a callback interacts with the dispatch loop's pending cost

Source: residue of the error-path cost-flush fix, from a resolution-loop fixer.

`set_cost_budget` resets `cost_used` to 0 while an outer `Frame::eval` still
holds unflushed batched opcode cost, which is then charged against the new
budget. So `cost_used` around a mid-run budget reset depends on the batching
interval, which the `COST_CHECK_INTERVAL` doc says it never does. The doc for
`set_cost_budget` does not say whether a mid-run reset is supported. Either
flush the pending batch before the reset or document mid-run resets as
unsupported.

## VM-035 - A snapshot load that fails partway through env deltas leaves earlier deltas applied

Source: lateral finding from a resolution-loop fixer (unverified whether the
State remains usable after a failed load).

Env deltas are applied to live library tables one at a time in
`src/vm/save_state.rs`. A corrupt delta that fails partway (the new
non-table-metatable check is one more way to fail there) leaves the earlier
deltas already applied. If a State is still usable after `load_state` returns
an error, that breaks the all-or-nothing reading of a rejected load.

## VM-036 - Re-adding a deleted hash key is O(table size) for an O(1) charge

Source: lateral finding from the resolution-loop fixer that gave tables an
array part.

Claim broken: the cost budget bounds host work (README "Budget").

`insert_into_map_with_tombstones` (`src/vm/table.rs`) reuses a key's dead slot
via `shift_remove_index`, which shifts every later entry. A loop like
`t.x = nil; t.x = v` on a large table therefore does linear host work per write
at a constant charge. The array part no longer goes through this path; hash keys
still do.
