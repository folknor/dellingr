# Hunt: vm-exec

Scope: `src/vm.rs`, `src/vm/{eval,eval_control,eval_index,eval_store,frame,stack,metamethod}.rs`,
`src/cost_meter.rs`, `src/limits.rs`, `src/error.rs`. Findings are ordered by severity. I found
these by reading the code and did not run anything. Every finding names the claim it breaks.

---

## 1. HIGH - A field miss on a plain table reads from the global `table` library, and errors when `table` is not a table

**Where:** `eval_index.rs` `instr_get_field` -> `push_table_library_field` /
`push_table_library_field_slow`.

**What happens:** `GET_FIELD` (`t.k`, `t:k(...)`) on a table first looks for the key in the table
itself. If the key is missing and the table has no metatable, or its `__index` chain yields nil, the
read falls through to `get_global("table")` and looks the key up there. This has three
consequences:

- **Wrong values.** `local t = {} print(t.insert)` prints a function where reference Lua prints
  `nil`. The same applies to `t.remove`, `t.sort`, `t.concat`, `t.unpack` and every other
  table-library name. It also applies to objects whose class `__index` lacks the name. So
  `if obj.remove then obj:remove() end` calls `table.remove(obj)`. Through the `_G` proxy (its
  `__index` function returns nil, so the fallback runs), `_G.insert` is also non-nil.
- **Inconsistent indexing.** `t["insert"]` (`GET_TABLE`), `rawget`, the host `State::get_table` and
  `pairs` all see nil, while `t.insert` sees a function. So `t.k` and `t["k"]` stop being the same
  expression.
- **Errors on ordinary code.** If the global `table` is not a table, every field miss on any plain
  table raises an error. The VM pushes `get_global("table")`, then `get_table_with_key` on the
  non-table returns `TypeError::TableIndex`. Two ways to get there:
  - The script does `table = nil` (or `table = 5`). `invalidate_table_library_fallback_rebind` drops
    the cache, so every miss takes the slow path, and `local t = {} if t.x then end` fails with
    "attempt to index a nil value".
  - The host calls `State::with_restricted_env` with a whitelist that omits `"table"`. The Table
    builtin becomes Nil, so the fast-path check `builtins[Table] == cache.table` fails, and every
    plain-table field miss inside the sandbox errors.

**Claim broken:** the output should match reference Lua for implemented features, with
divergences marked `-- DIFF:`. No example marks this divergence, and neither the README nor
`reference/` mentions it. The only place it is described is the header of
`examples/fields/miss.lua` ("exists only to support `t:insert(...)` sugar"). The error case also
breaks `with_restricted_env`'s documented purpose: a restricted environment should hide globals,
not make indexing fail.

**Direction:** delete the fallback. If the `t:insert` sugar is a product requirement, document it
in the README as a DIFF and make it never error: a missing or non-table `table` global should mean
"no fallback". The probe bench `fields/miss` and the `table_library_fallback` cache machinery
exist only to make this fallback cheap, so both disappear with it.

---

## 2. MEDIUM - A runtime error discards up to `COST_CHECK_INTERVAL - 1` charged units

**Where:** `frame.rs` `Frame::eval`, where `local_cost` is a stack local.

**What happens:** costed opcodes add to `local_cost`, which is flushed only:
- at the interval,
- when the budget is about to run out,
- before `OP_CALL`, `OP_TFOR_CALL` or a metamethod call, and
- at `OP_RETURN`.

Any other `Err` leaving `eval` throws the pending amount away. Examples are an arithmetic type
error, indexing nil, a stack overflow or a string-size error. For example,
`local a, b = 1, nil  local x = a + a  local y = a + b` charges 2 units, but `cost_used()` after
the error reports 0. The amount lost depends on `limits::COST_CHECK_INTERVAL` (64).

**Claim broken:** "exact cost model (charged at runtime)". The `Frame::COST_CHECK_INTERVAL` doc
promises that the batching interval does not change `cost_used`, but it guarantees this only at
`BudgetExceeded`. Hosts that bill a killed callback's `cost_used()` against a tick budget
undercharge, and the amount depends on a batching constant rather than on the script.
`tests/cost_golden.rs` pins only successful runs, so nothing catches this.

**Fix:** flush `local_cost` on the error exit of `eval`. Wrap the loop body, or handle it in
`eval_closure_frame`'s `Err` arm by passing `local_cost` out. Flushing on error is a plain
`saturating_add` with no admission check, so the error path stays cold.

---

## 3. MEDIUM - A host `call(ArgCount::Dynamic, ..)` inside a callback consumes the Lua caller's pending call base

**Where:** `eval.rs` `State::call`, `ArgCount::Dynamic` arm.

**What happens:** `OP_MARK_CALL_BASE` pushes an absolute base, then evaluates the arguments. Those
arguments can call a `RustFunc`, for example `print(g())` where `g` is a host function. If `g`
calls `state.call(ArgCount::Dynamic, ..)`, `vararg_call_bases.pop()` succeeds and returns the
outer frame's base, which lies below `g`'s `stack_bottom`. The only check is `checked_sub` against
the absolute stack length. The call then removes the caller's callee slot (`print`) out from under
the Lua frame and calls it with arguments that span both frames. Later, the outer frame's own
Dynamic `OP_CALL` fails with "no vararg_call_base set" or uses the wrong base.

**Claims broken:**
- `call`'s own doc says "Dynamic calls are bytecode-internal and return an error if used by a
  host". That holds only when no Lua frame has a pending base.
- The comment on `OP_MARK_CALL_BASE` names exactly this hazard ("would treat a caller-owned slot
  as this call's callee") and validates against `stack_bottom` at the marker. The consumer never
  repeats that check, even though `stack_bottom` changes in between.

**Fix:** in the Dynamic arm, reject `base < self.stack_bottom` without popping. A cleaner design
is not to expose Dynamic through the public `ArgCount` at all: give the bytecode its own internal
entry point, so `call` takes only fixed counts.

---

## 4. MEDIUM - `State::set_global` pops below the current frame and can panic

**Where:** `vm.rs` `set_global`, which calls `pop_val()` directly.

**What happens:**
- `pop_val` ignores `stack_bottom`. A `RustFunc` that calls `state.set_global("x")` with no
  visible values pops the top value of the calling Lua frame, such as a temporary or a local.
- Back in `call`, `num_ret_actual = self.get_top()` then computes
  `stack.len() - stack_bottom` with `stack.len() < stack_bottom`. That is a usize underflow: a
  panic in debug builds and a huge count in release builds.
- With an empty backing stack, `set_global` panics with "VM bug: pop from empty stack".

**Claim broken:** AGENTS.md says the public stack API is fallible, and that a rejected operation
must leave the stack unchanged. Every sibling API (`pop`, `set_table_raw`, `concat`, `call`)
validates against the visible frame; `set_global` is the gap. Its signature is infallible, so it
cannot even report the misuse.

**Fix:** return `Result<()>` and check `get_top() >= 1` first.

**Doc nit:** its doc comment reads "Instr::pop()s a value", which looks like a find-and-replace
accident.

---

## 5. LOW - A zero `for` step silently skips the loop

**Where:** `eval_control.rs` `check_numeric_for_condition`.

**What happens:** `for i = 1, 10, 0 do ... end` runs zero iterations. Lua 5.4 raises "'for' step
is zero", and Lua 5.2 loops forever. dellingr matches neither.

**Claim broken:** output should match reference or be marked `-- DIFF:`. No example covers a zero
step, and the README does not mention it. The choice may be deliberate, since an infinite free
loop is a budget hole, but it should be documented or turned into the 5.4 error.

---

## 6. LOW - Runtime 255-value caps on Lua returns and dynamic calls are documented as Rust-callback limits

**Where:** `error.rs` `results_count`; `limits::MAX_RESULTS` and `MAX_ARGUMENTS`.

**What happens:** `results_count` ("Validate a result count against the Rust callback ABI") is
also applied in `eval_closure_frame` to every Lua `return f()` / `return ...` with
`RetCount::All`. `call`'s Dynamic arm caps `f(table.unpack(t))` at 255 arguments in the same way.
So `return table.unpack(t)` with 256 or more elements raises `TooManyResults` in pure Lua code.

**Claim broken:**
- `MAX_RESULTS` is documented only as "Maximum number of values a Rust callback can return".
- README "Runtime limits" lists only the fixed-count compile-time caps.
- Reference Lua accepts these programs.

**Fix:** either document the runtime cap for Lua frames, or widen the internal counts to `usize`.
The `u8` exists only because of the bytecode operand encoding, which these dynamic paths do not
need.

---

## 7. LOW - Partial-failure leftovers on host-facing metamethod paths

- `metamethod.rs` `handle_index_metamethod_inner` and `handle_newindex_metamethod_inner` (function
  handlers) push handler, table, key and value, then call `Frame::flush_local_cost(..)?`. When
  this is reached from bytecode, a flush failure leaves those 3-4 temporaries on the stack. That
  is harmless today because the frame unwinds. The ordering is still inverted relative to the
  "preflight before any mutation" comment directly above it. Flushing before the pushes would
  make the whole path strictly all-or-nothing.
- The host `State::get_table` pops the key before `get_table_with_key`, so a type error (a
  non-table at `i`) consumes the key. The same is true of `get_table_raw`, and of `table_next` on
  `InvalidKey`. These are less strict than the "rejected operation leaves the stack unchanged"
  wording, but that wording only covers stack-growth rejections, so this is a smell rather than a
  broken contract.

---

## 8. LOW - The host `get_table` disagrees with the `t[k]` opcode on strings

`State::get_table` goes through `get_table_with_key`, which raises `TableIndex(string)` for a
string receiver. `OP_GET_TABLE` / `OP_GET_FIELD` resolve string receivers through the `string`
library. The name and doc ("As in Lua, this function may trigger a metamethod for the index
event") promise `t[k]` semantics, so the host sees a different result than the script for
`("abc")["upper"]`.

---

## 9. LOW - `State::concat(n < 2)` reports the wrong kind of error

`concat` rejects `n < 2` with
`ArgError { arg_number: 1, func_name: "concat", detail: TypeMismatch, expected: None }`, which
renders as "bad argument #1 to 'concat' (value expected)". The problem is a count, not a type or
missing value, and argument #1 is not a Lua argument at all. `ArgDetail::TypeMismatch`'s own doc
("Type or presence mismatch, described by expected and received") does not fit.
`InvalidStackIndex` or `OutOfBounds` would be accurate.

---

## 10. LOW - `Error` Display can splice the selected frame's line with the innermost error's column

`error.rs` `impl Display for Error`: when the selected `prefix_frame` has a line but no source, it
prints `"{frame.line}:{self.column}"`. The comment above insists on taking "the source and the line
from the SAME record", but the column still comes from the innermost error. With
`error(msg, 2)` (`TraceFrame(1)`) and an unnamed chunk, the output pairs the caller's line with the
callee's column. Column is always 0 for runtime errors today, so the visible effect is a spurious
`:0`. It is still the splice the comment warns against.

---

## Observations (not defects)

- **String methods depend on the global `string`.** `get_string_table_field` resolves string
  methods through the *current global* `string`. In a `with_restricted_env` sandbox without
  `"string"`, `s:upper()` raises "attempt to index a nil value", whereas reference Lua uses the
  string metatable. The function's doc states this shape, so it is not a defect, but it has the
  same sandbox-hostile shape as finding 1 and is worth a deliberate decision.
- **`__index`/`__newindex` function handlers count against `MAX_METAMETHOD_DEPTH` (200).**
  `__call` function handlers deliberately do not; the comment in `call` explains that guarding them
  "would cap legitimate recursion". A memoising `__index` that recurses through its own table
  (`fib[300]`) therefore stops at depth 200 even though every level is a real Lua frame. Reference
  5.4 also stops near 200 (C-stack), so this is consistent with the reference, but it is not
  consistent with the stated `__call` rationale.
- **Conservative `__call` preflight.** The `__call` path in `call` preflights 2 slots after
  already removing the callable, so it needs only +1 net. A call exactly at the stack cap is
  rejected even though it would fit. This is harmless.
- **`..` charges no cost.** `OP_CONCAT` charges nothing for the bytes it emits, while README
  "Budget" says data-dependent string work "bytes examined or emitted" is charged (for the string
  and table *libraries*). This is probably deliberate, since `..` is an operator and not a library
  call, but a `s = s .. s` doubling loop is bounded only by `MAX_STRING_BYTES`, at the price of
  one `OP_CONCAT`. Worth a sentence in the README either way.
- **Cost meter.** I verified `CostMeter`, `cost_batch_needs_flush` and the `add_cost!` admission
  sequence against the unit-at-a-time semantics, and they are exact at `BudgetExceeded`.
- **GC rooting.** Rooting across `get_table_with_key`, `set_table_with_key`,
  `protected_index_key`, `concat_helper`, `push_closure` / `resolve_bytecode_runtime` and the
  `call` paths looked sound. Every value popped before an allocation is re-pushed or
  transient-rooted.
- **Field caches.** Field inline caches cannot return a tombstoned nil: `Table::get_index`
  filters dead slots, so a nil-deleted key falls through to `__index` correctly.
