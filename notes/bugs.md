# bugs.md

Defects turned up by the hygiene hunt over the compiler, vm, surface, stdlib
and tooling scopes. Hunter reports, not verified findings: each may be wrong.

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

## BUG-001 - `cost_remaining()` drifts differently for bytecode and native cost when no budget is set

Reported by the vm, surface and stdlib hunters.

`State::consume_cost` (vm.rs) always decrements `cost_remaining`, whether or
not a budget is configured. `CostMeter` (cost_meter.rs) branches on
`cost_budget_configured`, and its `CountOnly` arm bumps `cost_used` and does
not touch `remaining`. On an unbudgeted State, bytecode cost and the callers of
`consume_cost` (math.rs, `table.insert`/`remove`/`pack`) lower
`cost_remaining()` from `i64::MAX`, while string, format, concat and
`table.move` cost (through `cost_meter().consume`) does not. So `math.sin`
moves `cost_remaining()` and `string.upper` does not. The resulting value
matches neither `budget - used` nor "unbounded", and `save_state` persists it
(`cost_remaining` in `SavePayload`). The `consume` doc says it behaves "like
`State::consume_cost`", which is true of the gate but not of the counter.

The budget rule has three implementations: `State::consume_cost`,
`CostMeter::consume`/`consume_units`, and the `add_cost!` macro in
`Frame::eval` (vm/frame.rs), which peeks
`cost_remaining().saturating_sub_unsigned(next) <= 0` to decide when to flush.
Two more copies of the meter selection exist: `cost_meter()` and the
destructured copy in `gmatch_subject_matcher_and_cost_meter` (vm.rs) each
restate "configured -> finite_budget else count_only". `BudgetExceeded` is
built in both `consume_cost` and `budget_exceeded_error`. In the stdlib,
`charge_cost` is defined three times with identical bodies (string.rs,
table.rs, string_format.rs) and `table.move` inlines it five more times.

Proposed fix: one gate. `State::consume_cost` delegates to
`self.cost_meter().consume(cost)`. Better, collapse `cost_remaining`,
`cost_budget` and `cost_budget_configured` into one
`Budget { Unbounded, Finite { budget, remaining } }` owned by cost_meter.rs, so
an unconfigured budget has no `remaining` to drift. Add one
`State::charge(cost)` for the library and make `CostMeter::consume` visible
only to cost_meter/vm. Enforceable by type and visibility. A unit test that
`cost_remaining()` changes identically on both paths pins it.

## BUG-002 - gmatch argument errors report an absolute stack index as the argument number

Reported by the vm and stdlib hunters.

`memoize_gmatch_pattern` and `gmatch_subject_matcher_and_cost_meter` (vm.rs)
build `ArgError { arg_number: idx as isize + 1 }` where `idx` comes from
`convert_idx`, an absolute stack index. Any non-empty stack below the frame
makes the reported argument number wrong. `push_bytes_from_stack_range` and
`to_bytes` (vm/stack.rs) correctly use the caller's relative index. The root
cause is that the "expected string at index" `ArgError` is hand-built at four
sites (vm.rs x2, stack.rs x2); one `expect_string_at(idx)` helper fixes it.

The stdlib hunter adds that these paths are reachable only through an internal
ordering mistake or a doctored save, and that they should be `InternalError`,
as the two neighbouring checks already are, rather than presenting as a script
argument error.

## BUG-003 - Concat and `tostring` render numbers by different rules

Reported by the vm hunter.

`concat_helper` (vm/eval.rs) renders `fract() == 0 && abs < 1e15` as
`num as i64`, otherwise `{num}`. `Val::to_bytes_with_heap` (vm/lua_val.rs) uses
`n.to_string()`. They have diverged: `-0.0 .. ""` yields `"0"`, while
`tostring(-0.0)` yields `"-0"`. One number-to-string function should exist,
read by stdlib `tostring` and `string.format` too. Enforce with a property test
that `x .. ""` equals `tostring(x)` for all finite and special f64 values.

## BUG-012 - A host `State::call` on a cyclic `__call` table may not fire `on_error`

Reported by the fixer that guarded `__call` depth.

A host calling a self-referential `__call` table directly through `State::call`
now gets `MetamethodDepthExceeded`, but if no Lua frame or Rust function is
entered along the way, `on_error` may never fire. The `on_error` doc calls it
the single source of telemetry.

## BUG-006 - gmatch charges pattern-compile cost only on a cache miss, so cost depends on GC schedule and save/load

Reported by the stdlib hunter, by reading; the hunter says to test before
acting.

`memoize_gmatch_pattern` (vm.rs) charges `pattern_len.max(1)` only on a cache
miss. The cache (`gmatch_patterns`) is cleared on every `gc_collect`, evicts
FIFO at `GMATCH_PATTERN_CACHE_ENTRIES = 256`, and is not saved. Consequences:

- Two runs of the same script with different `gc_set_threshold` settings, or
  different allocation history before the loop, can charge different cost for
  the same gmatch iteration. README "Determinism" promises same script and
  inputs give the same cost per build; the GC threshold is a host knob, not an
  input the README names.
- A game saved with a live gmatch iterator charges compile cost again on its
  first resumed iteration, so a resumed replay's cost diverges from an
  uninterrupted one.

Fix: charge compile cost per gmatch iterator (a "compiled" flag in the
iterator's saved state table) regardless of a VM cache hit. Test: same script
with GC forced between iterations vs not, assert equal `cost_used`.

## BUG-010 - Out-of-range argument errors are `RuntimeError`, not `ArgError`

Reported by the stdlib hunter; narrowed by a fixer.

`tonumber` (base out of range), `select` (index out of range) and `rawlen`
("table or string expected") now say the right thing, through a shared
formatter in lua_std.rs, but as `RuntimeError`: `ArgError` cannot carry a
custom detail. The fix is the `ArgDetail` enum from the hygiene entry on "bad
argument" owners, after which these become `ArgError` again.

## BUG-011 - `get_table` panics when the host passes the key's own index

Reported by the vm hunter.

`get_table` (vm/table_ops.rs) does `assert!(idx != self.stack.len() - 1)`. A
host passing the key's own index panics instead of getting
`InvalidStackIndex`. The hunter lists this with the other public-API aborts
(see the hygiene entry on aborts on embedder-controlled input).
