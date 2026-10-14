# Hunt: embedding surface

Scope: `src/lib.rs`, `src/host.rs`, `src/main.rs`, `src/vm/rng.rs`, and the
public `State` methods, measured against README and `reference/`. There is
no `docs/` folder.

Findings are ordered by severity. Each one names the claim it breaks.

## 1. `State::call(ArgCount::Dynamic, ..)` from a host steals an outer frame's call base

Claim broken: the doc comment on `State::call` (`src/vm/eval.rs`) says
"Dynamic calls are bytecode-internal and return an error if used by a host".
The stack-cap and frame-isolation invariants in AGENTS.md are also broken.

What the code does: the `Dynamic` arm pops `vararg_call_bases` and only fails
when that stack is empty. When Lua runs `f(1, g())` and `g` is a `RustFunc`,
`OP_MARK_CALL_BASE` has already pushed `f`'s base before `g` runs. If `g`
calls `state.call(ArgCount::Dynamic, ..)`, it pops `f`'s base, which lies below
`g`'s `stack_bottom`. It then does `self.stack.remove(base)` on a caller-owned
slot and calls `f` with the caller's operands as arguments. When the outer
`OP_CALL` later runs, it finds its base gone. It then gets either an
`InternalError` or a different stale base.

`call_anchor` already rejects `Dynamic` explicitly, and its comment describes
this exact hazard. `call` itself does not. Fix: reject `Dynamic` at the public
entry point, splitting an internal `call_dynamic` out of the dispatch loop.
Alternatively, at least require `base >= self.stack_bottom`. Better still, take
`Dynamic` out of the public `ArgCount` entirely: it is a bytecode encoding
detail and should not appear in the host signature.

## 2. `set_global` is infallible and pops below the frame, or panics

Claim broken: AGENTS.md says the host API is fallible and a rejected operation
leaves the stack unchanged. Every sibling method (`anchor`, `set_table_raw`,
`set_metatable_of`) carries a comment saying the host must get an error rather
than reach the panicking `pop_val`.

What the code does: `State::set_global` (`src/vm.rs`) calls `self.pop_val()`
with no visibility check.
- With an empty stack at the top level, it panics with "VM bug: pop from empty
  stack".
- Inside a `RustFunc` whose visible frame is empty (`get_top() == 0`) while the
  caller's frame holds values, it silently pops a value from the **caller's Lua
  frame** and installs it as the global. This corrupts the running Lua function.

Fix: return `Result<()>` and validate with `at_index(-1)` before popping.

## 3. `call_anchor` checks argument count against the absolute stack, then mutates before failing

Claim broken: README "Runtime limits" says a rejected operation leaves the
stack exactly as it was. The Anchor docs say misuse surfaces as errors, not
silent corruption.

What the code does (`src/vm/anchor.rs`): it computes
`insert_at = self.stack.len().checked_sub(n_args)`. That is the absolute
length, not `get_top()`. Inside a `RustFunc` with fewer than `n_args` visible
values, `insert_at` lands below `stack_bottom`. The function is
`stack.insert`ed into the caller's frame, shifting a caller value into the
visible frame. `call(Fixed(n))` then fails its `get_top()` check with
`InvalidStackIndex`, and the stack is left mutated: the anchored value is now
inside the caller's frame. Fix: validate `n_args <= get_top()` before inserting.

## 4. Host table methods consume operands before they fail

Claim broken: README says "a rejected operation leaves the stack exactly as it
was", and that the `push_*` / `new_table` / `get_global` host API respects it.
AGENTS.md says the same for every growth path. Whichever way the README
sentence is read, `table_next` violates it literally.

- `table_next` pops the key and then runs `check_stack_space(2)`. On
  `StackOverflow` the key is gone, so a host iteration loop cannot retry or
  resume. The same happens on `InvalidKey` and on a non-table type error.
- `get_table` and `get_table_raw` pop the key before the type check and before
  the `__index` dispatch. If the operation fails (indexing a non-table, an
  `__index` error), the key is gone.
- `set_table_raw` pops value and key, then fails on a non-table or on
  `t.insert` (nil or NaN key). Both operands are lost.
- `set_metatable_of` pops the metatable, then fails on a non-table target or a
  non-table metatable.
- `table_insert_at` pops the value, then fails on a non-table.

`set_metatable_of` and `set_table_raw` have comments about validating "before
popping", but they validate only the stack shape, not the types. Fix: peek,
validate everything, and pop only on commit. This is the same discipline that
`concat_helper` already follows ("Intern before truncating").

## 5. `table_insert_at` is public but trusts an unvalidated `pos`

Claim broken: the method's own name and doc ("Inserts a value into a table at a
position, shifting elements"). The doc admits "`pos` has been validated by
table.insert", but an embedder calling it has no such validator.

`Table::array_insert` loops `pos..=len` and then writes `carry` at `len+1`.
- For `pos > len + 1`, the loop is empty and the value lands at `len+1`, not
  `pos`.
- For `pos == 0`, the value is written at key `0` and the old `t[0]` is shifted
  into `t[1]`.

Either validate `1 <= pos <= len+1` and return an error, or make these
stdlib-internal helpers `pub(crate)`. The same applies to `table_remove_at`
(it silently returns nil for out-of-range positions, which at least matches its
stdlib caller) and to `table_sort`.

`table_sort`'s "uses the function at stack index 2 as comparator" is a hidden
coupling to the stdlib calling convention. Its doc also says "Returns the array
length for cost charging", but it already calls `consume_cost` itself. A host
following the doc would charge twice.

## 6. Pending batched opcode cost is dropped when a frame errors

Claims broken:
- `cost_used()`: "total cost consumed since the last budget reset".
- `COST_MODEL_VERSION`: "moves whenever `cost_used` for identical source
  changes".
- The `COST_CHECK_INTERVAL` doc in `frame.rs`, which says the interval does
  not change `cost_used`. It argues this only for `BudgetExceeded`.

What the code does: `Frame::eval` accumulates fixed opcode charges in
`local_cost` and flushes on `OP_CALL`, `OP_TFOR_CALL`, `OP_RETURN`,
metamethod dispatch, every `COST_CHECK_INTERVAL` (64) units, or near budget
exhaustion. Any other error exits via `?` (a type error in arithmetic, an
`error()` call reached without a flush, a table-index error, `concat` failure)
and discards up to 63 already-executed arithmetic or table-write units.

For example, `local a = 1 + x; local b = nil .. 1` with `x = 2` reports
`cost_used() == 0` after the failure, even though the add ran. The CLI prints
this undercounted `Cost used` on every failing script. A snapshot taken after a
killed callback persists the undercount. Changing the public
`limits::COST_CHECK_INTERVAL` changes `cost_used` for erroring scripts without
a `COST_MODEL_VERSION` bump.

Fix: flush `local_cost` on the error path, either through a guard in `eval` or
in `report_frame_error`'s caller. The ADD has already been admitted and
executed, so charging it is consistent with "the action that pushes you over
budget completes".

## 7. Smaller items

- **CLI `-` argument** (`src/main.rs`): `parse_args` special-cases `"-"` so it
  is not rejected as an unknown option, which is the conventional stdin
  spelling. It then passes `"-"` to `fs::read_to_string` as a file named `-`.
  Nothing documents stdin support, so this is not a broken claim, but the
  special case suggests an intent the code does not carry out. Either read
  stdin or drop the special case.
- **`set_cost_budget` from inside a callback** resets `cost_used` to 0 while
  the outer `Frame::eval` still holds unflushed `local_cost`. That pending cost
  is then charged against the new budget. Combined with finding 6, `cost_used`
  around a mid-run budget reset depends on batching. The doc does not say
  whether mid-run resets are supported.
- **`ScopeCost` display**: `function_calls` counts only `OP_CALL`, not
  `OP_TFOR_CALL` (iterator calls) or metamethod calls, yet the summary labels
  it "Function calls". `fmt_indent` never prints a per-scope call count, so the
  per-scope breakdown and the totals disagree on which fields they show. This is
  cosmetic.
- **Anonymous scope names**: they use `anonymous #{i+1}`, where `i` indexes
  *all* nested chunks, named ones included. Two anonymous functions with a named
  one between them become `#1` and `#3`.
- **`math.random(m, n)` precision**: the result goes `i64` to `f64`. For spans
  outside +/-2^53 it silently rounds, so results can fall outside `[m, n]` after
  rounding (for example near `i64::MAX`). Reference 5.4 returns integers here.
  Whether to call this a defect depends on `arg_integer` accepting such
  magnitudes. It is worth one check against the math contract in
  `src/lua_std/math.rs`.
- **README vs anchor doc**: the README lists `Anchor` as `Copy + Send`, while
  the anchor module doc says `Copy + Send + Sync + 'static`. They do not
  contradict each other, but the README understates the guarantee.
- **The `ArgCount`/`RetCount` public encoding**: `ArgCount::Fixed(255)` is
  accepted by `call` (`MAX_ARGUMENTS = 255`) but cannot round-trip through
  `to_u8`, because 255 is the `Dynamic` sentinel. This is harmless today
  because host calls never encode. Together with finding 1, it argues for a
  separate host-facing argument type with no sentinel and no `Dynamic`.

## Structural recommendation

Findings 2 through 5 share a root cause. The host API is a C-API-style set of
stack-twiddling methods, each re-implementing "validate, then pop" by hand. The
same `State` methods also serve as stdlib internals with stdlib-only
preconditions. Splitting the surface into a narrow, fully validated host facade
and `pub(crate)` internals would close the class rather than the instances. The
facade would cover frame-relative indexing, peek-then-commit and no `Dynamic`.
The internals would be `table_*_at`, `table_sort`, `set_global` without
validation, and the dynamic call.
