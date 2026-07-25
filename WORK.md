# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #15, #16: work that is done but never charged

Per-opcode instruction-cost accounting is dellingr's reason to exist - the
README's pitch is bounded execution for a game tick. These two findings are
holes in that budget large enough to drive a script through.

**Verified together.** This script, run with `--limit 100000`:

```lua
local t = {}
table.move(t, 1, 200000, 1)          -- 200k table ops

local s = "x"
for i = 1, 16 do s = s .. s end      -- 65536-byte string
for i = 1, 200 do
  local _ = s:gsub("x", "y")         -- 200 passes over 65 KB
end
```

reports **`Cost used: 2`**. Hundreds of megabytes of real work, charged two.

### #15 (High) - `table.move` is unbounded and uncharged, and overflows

`src/lua_std/table.rs:255-307`. Charges a flat 1, then loops `e - f + 1` times
doing `get_table` + `set_table_raw`, both free. The range comes straight from
script arguments and is **not bounded by table size**, so nil reads and nil
writes still iterate. No allocation is required, so memory limits do not help
either - `table.move({}, 1, 2^30, 1)` is ~10^9 table operations for cost 1.

Separately, `e - f + 1` overflows `isize` at saturated extremes:
`table.move(t, -1e300, 1e300, 1)` gives `f = isize::MIN`, `e = isize::MAX`.
Panic in debug, silent wrap in release. Reference guards this with
"too many elements to move" (`f > 0 || e < LUA_MAXINTEGER + f`).

`table_sort` already does the right thing here - it charges `n.max(1)` **before**
running the comparator, with a comment explaining the contract. That is the
template.

### #16 (High) - pattern matching and string byte-work are entirely uncharged

There is no `consume_cost` anywhere in `src/lua_std/string.rs`,
`src/lua_std/string_format.rs`, or `src/patterns/`. Consequences:

- A script builds a large string with ~17 concat ops by repeated doubling,
  then every `gsub` / `find` / `match` / `upper` / `format("%s")` does O(n) or
  worse for ~0 charge, in a loop. That is the measurement above.
- Backtracking patterns are superpolynomial in *time* while the depth cap only
  bounds *recursion*: `"a*a*a*a*b"` against a long non-matching run of `a`s
  does O(n^k) `singlematch` work inside `max_expand` for one free call.
- `table.concat(t)` does `len` lookups and builds an arbitrarily large byte
  vector for cost 1 (this half is also C-D2).

`table.insert`/`remove` charging 1 for O(n) is already acknowledged in
`notes/optimizations.md`; the string and pattern side is tracked nowhere.

### The cost contract

From `table_sort`'s existing comment and the L18 convention: **charge before
the side effect**, so an exhausted budget blocks the work rather than letting
it complete and only then failing. `cost_remaining` is `i64` precisely so the
operation that crosses the boundary completes and the *next* costed op fails.

---

## Agreed plan - staged, this commit is stage 1 only

Fixing #16 changes what every string operation costs. There is no conversion
formula: the increase depends on string lengths, output expansion, match
success, captures and backtracking shape. That makes it a **cost-model version
bump**, not a bug fix - embedders must re-measure their tick budgets, and
snapshots persist cost counters so the save format is implicated too.

So it is staged, and this commit is stage 1:

- **Stage 1 (this commit):** `table.move` correctness and charging, plus the
  neutral cost-meter infrastructure that stage 2 needs. `table.move`
  undercharging is a plain bug - it already charged, just not proportionally.
- **Stage 2 (separate commit):** string, pattern, `format` and `concat`
  charging. This is the breaking half and is isolated so it can be held back or
  reverted before a release without disturbing stage 1.

### Stage 1: `table.move`

Arguments parse as exact `i64` via the existing `exact_i64` conversion, so
non-integral and out-of-range arguments fail cleanly rather than saturating
through `as isize`. Apply **both** reference guards, before any mutation or
charge:

- source span: "too many elements to move";
- destination end: "destination wrap around".

Reference does both up front (`ltablib.c`'s `tmove`), and doing so is what
removes the `e - f + 1` overflow: today `table.move(t, -1e300, 1e300, 1)` gives
`f = isize::MIN`, `e = isize::MAX` and panics in debug, silently wraps in
release and returns without moving anything.

Charging:

- **With an active finite budget:** charge one unit immediately before each
  source-lookup/destination-write pair, and stop before the first element whose
  charge finds the budget exhausted. A single up-front `consume_cost(count)` is
  *not* sufficient, because `consume_cost`'s contract lets an operation that
  starts with positive budget cross it and still complete - so a billion-unit
  charge would still run a billion iterations once.
- **Without a configured budget:** one `count.max(1)` charge before the loop,
  keeping the current tight loop and paying no per-element overhead.

A budget failure can therefore leave a partially moved table. That follows from
treating each element as a costed operation, and errors already kill the
callback. Both the forward and backward overlap paths must charge in their
actual copy order, so partial results stay deterministic.

### Stage 1: cost-meter infrastructure

- A count-only meter for the default/no-limit path.
- A finite-budget meter borrowing only `remaining` and `used`, with saturating
  counters matching `State::consume_cost`.
- A private flag recording whether a real budget was configured. **Do not infer
  this from `i64::MAX`** after earlier charges have already moved the counter.

Defined outside `vm` so stage 2 can pass it into the matcher without the
matcher ever seeing `State`.

### Stage 2 (recorded here, not implemented in this commit)

Unit: one per deterministic logical work item - one table element visited, one
byte processed or emitted, one matcher primitive. No hardware-inspired divisor;
existing costs are already semantic rather than cycle-calibrated (table
construction is one per element, `table.sort` is `n`, not `n log n`). Per
operation, `cost = max(1, sum of work units)`.

Charge points in the matcher, threaded via the meter: every `patt_match`
entry/continuation, every `singlematch`, every pattern byte inspected locating
or testing a bracket class, every subject byte inspected by `%b`, every byte
compared by a backreference, every literal-search comparison. Pattern
validation reserves `pattern.len()` up front. **Do not** batch as one-per-K -
that makes the stopping boundary depend on K and lets a pathological matcher
overshoot by K.

Per-function contractual work:

| Operation | Work |
|---|---|
| `string.len` | free, O(1) |
| `string.sub` | returned bytes |
| `string.upper/lower/reverse` | input length |
| `string.find` | pattern length + comparisons/steps + returned capture bytes |
| `string.match` | pattern length + steps + materialized result bytes |
| `string.gmatch` create | pattern validation length |
| each `gmatch` step | steps + materialized capture bytes |
| `string.gsub` | pattern + search work + replacement bytes examined + emitted bytes + captures materialized for callbacks |
| `string.format` | format bytes scanned + output bytes |
| `table.concat` | elements visited + output bytes including separators |

Both sides for constructors: a no-match `gsub` charges once to scan and once to
copy. Output buffers charge before mutating, since `gsub`/`format`/`concat`
output size is unknown at entry. Matcher work must be committed before a `gsub`
replacement function re-enters Lua.

Matcher failures need `MatchError::{Pattern, Budget}` so budget exhaustion
surfaces as `ErrorKind::BudgetExceeded` rather than being flattened into the
existing pattern `RuntimeError`.

Staging for stage 2: export a `COST_MODEL_VERSION`, release as the next pre-1.0
breaking minor, do **not** feature-gate a legacy mode (the old behaviour is the
hole, and compile-time modes fragment contractual costs), and bump/validate the
cost-model version in snapshots so old-unit counters cannot silently continue.

`analyze_cost` stays numerically unchanged - it cannot resolve data lengths or
even prove a call reaches the stdlib, since globals are mutable. Its docs and
the CLI output should say explicitly that it covers only statically knowable
bytecode charges and excludes runtime data-dependent native work. Pin that with
a test: `analyze_cost("return string.upper('abcd')") == 0` while execution
costs 4.

### Stage 1 tests

- `table.move` cost for empty, one-element and many-element ranges.
- Both overlap directions.
- Source-span overflow and destination wrap-around rejected with the reference
  messages, before any mutation.
- Budget zero performs no mutation at all.
- Budget 2 leaves a deterministic partial move, identical across runs.
- Extreme arguments (`-1e300`/`1e300`) produce a clean error in both debug and
  release rather than a panic or a silent no-op.
- Two fresh states running the same script report identical cost.

---

## Review findings to fix

Both range guards, the per-element stop-before-work charging, deterministic
partial mutation in both overlap directions, the explicit flag never being
inferred from `i64::MAX`, and `CostMeter`'s usability without `State` all
verified correct. Three defects remain.

### 1. Restored configured budgets silently become unconfigured

`save_state.rs:911` restores `cost_remaining`, `cost_budget` and `cost_used`,
but **not** `cost_budget_configured`. A fresh `State` starts with the flag
false, so after `load_state` a `table.move` takes the `CountOnly` path and
bypasses the remaining budget entirely.

Concretely: restore with two units remaining, move five elements - all five
complete, `cost_used` grows, and `cost_remaining` is still two. Restore with
*zero* remaining and the move still runs; only some later opcode notices.

`set_cost_budget` afterwards is not a workaround, because it resets usage and
remaining budget rather than continuing the persisted one.

I told the build session to defer this as a snapshot-format change. That was
wrong: it changes observable enforcement and breaks continuation of an
embedder's persisted budget, which is the guarantee the whole cost model
exists to provide. **Persist and restore the configured mode in stage 1**, with
whatever format version handling that requires.

### 2. Empty moves evade a configured budget

`table.rs:290`: the `count.max(1)` charge happens only on the no-budget path.
With a configured budget an empty range enters neither copy loop and so charges
**zero**, even when the budget is already exhausted.

So `table.move(t, 2, 1, 1)` now succeeds against a zero budget, where the old
flat-1 charge would have failed before doing anything. That is a regression the
existing empty-range cost test misses, because it only exercises an
unconfigured state.

Charge the `max(1)` minimum on both paths, and extend the test to cover a
configured budget.

### 3. The no-budget loop still branches per element

`table.rs:303` and `table.rs:316` keep an `if finite_budget` inside every
iteration, on the path that is supposed to be the tight budget-free loop. An
optimizer may unswitch it, but the implementation should not depend on that.
Hoist the branch: separate budgeted and unbudgeted loop variants.

### Superseded questions

1. What is the right charging model for the pattern matcher? Per `patt_match`
   invocation is the obvious hook, but backtracking means invocations are the
   thing that explodes, so that may be exactly right - or it may be so
   fine-grained that it costs more than the match. Per K `singlematch` steps is
   the alternative. Whichever, it needs to be charged against
   `State.cost_remaining` from inside `src/patterns/`, which currently has no
   access to `State` at all. How should that thread through without dragging
   the VM into the matcher?
2. What is the unit? Existing charges are per-opcode integers. Is one unit per
   byte scanned right, or should string work be charged at some ratio to
   opcode cost? Getting the ratio wrong makes previously-fine scripts start
   failing their budget, which is a compatibility break for the embedder. Say
   what the ratio should be and why.
3. Which string functions need length-proportional charges, and charged on
   input length, output length, or both? `gsub` can produce output far larger
   than its input.
4. For #15, is clamping `e` to something sane the right call in addition to
   the reference overflow check, or does the charge alone make it safe? If a
   script is charged 10^9 it will exhaust any real budget immediately, which
   might make clamping unnecessary.
5. **Does this break existing scripts?** This is the part I care about most.
   Adding charges where there were none can only *increase* measured cost, so
   any embedder currently running close to their budget will start failing.
   `analyze_cost` is also documented as "neither a lower nor an upper bound",
   but its relationship to runtime cost changes here. What is the migration
   story, and should any of this be feature-gated or staged?

Read `src/lua_std/string.rs`, `src/lua_std/table.rs`, `src/lua_std/string_format.rs`,
`src/patterns/`, `State::consume_cost` in `src/vm.rs`, and `analyze_cost` in
`src/lib.rs`.
