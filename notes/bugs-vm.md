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

## VM-037 - Snapshots carry the producing host's NaN bits

Source: lateral finding from the resolution-loop fixer that made
`string.format` print NaN unsigned.

Claim at stake: determinism across hosts. `0/0`, `math.sqrt(-1)` and similar
produce the hardware default NaN, which has the sign bit set on x86-64 and clear
on aarch64. Script output no longer shows the sign: `tostring` prints `NaN`,
`string.format` now prints an unsigned `nan`, `%q` prints `(0/0)`, and NaN never
compares equal and cannot be a table key. But `save_state.rs` stores
`SavedVal::Num` as `to_bits`, so a NaN computed at runtime is saved with the
producing host's bits. Two hosts running the same script therefore produce
different snapshot bytes, although the restored States behave identically. (The
fixer also blamed folded literals; the wave reviewer reports that the parser
already refuses to fold a NaN result, so only runtime values are affected.) If
snapshots must be byte-identical across hosts (for example, hashed for replay
verification), canonicalise NaN when saving.

## VM-038 - A Rust function reachable only through host state loses its rendered identity across save and load

Source: lateral finding from the resolution-loop fixer that gave Rust functions
`tostring` identities.

`tostring(f)` and `%p` render a Rust function as `function: 0x<id>`, with the id
held in `format_pointer_ids`, which snapshots save. The save walk reaches Rust
functions through globals and environment tables. One reachable only from host
state (a value the host holds and passes back in) is not walked, so its id is
dropped on save, and after load it gets a fresh id. The same script then renders
the same function differently before and after a save. `%p` has the same gap.

## VM-040 - The lexer counts `\r\n\r` as one line break instead of two

Source: lateral finding from a resolution-loop reviewer.

The newline handling in `src/compiler/lexer.rs` (the line-break consumer) folds
`\r\n\r` into one break. Reference Lua's `inclinenumber` treats `\r\n` as one
break and the trailing `\r` as a second. Line numbers in error messages and
stack traces after such a sequence are off by one, and the line-based ambiguous
`(` rule sees the wrong line.

## VM-041 - `print` can allocate about three times its input before rejecting oversized output

Source: lateral finding from a resolution-loop reviewer.

`print` (`src/lua_std/basic.rs`) converts each argument to UTF-8 lossily before
checking the size of the result. Each invalid byte expands to the 3-byte
U+FFFD, so 16 MiB of invalid bytes allocate about 48 MiB before the expanded
output is rejected against the string-size limit. The cap is meant to bound host
memory per operation.

## VM-039 - Lexer errors reuse generic variants for an unterminated long comment and an ambiguous call

Source: lateral finding from the resolution-loop fixer that made both of them
errors.

An unterminated `--[[` now fails with `SyntaxError::UnexpectedEof` at the
comment's opener, where reference Lua says `unfinished long comment`. A `(` on a
later line than the previous token fails with `LParenLineStart`, which displays
as "ambiguous function call" without mentioning the newline that makes it
ambiguous. A dedicated `UnfinishedLongComment` variant and a message naming the
line break (both in `src/error.rs`) would match the reference more closely and
point users at the cause.
