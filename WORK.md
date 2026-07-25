# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Target #16: pattern matching, string byte-work and `table.concat` are uncharged

A deliberate **breaking cost-model change**. The crate is far from 1.0 and the
decision to take the break is made. Ships as 0.4.0.

### Verified, by execution, not reading

- 200 `gsub` passes over a 131,072-byte string: `Cost used: 0`.
- `"a*a*a*a*b"` against 30 non-matching `a` bytes: `Cost used: 0`.
- `table.concat` over 10,000 ten-byte elements: total 10,002 - table creation 1,
  10,000 table writes, and exactly **1** for the concat itself.

There is no `consume_cost` or `cost_meter` call anywhere in
`src/lua_std/string.rs`, `src/lua_std/string_format.rs` or `src/patterns/`.

---

## THE GOVERNING PRINCIPLE - read this before writing any charge point

The budget is a game-design instrument, not a CPU meter. Its consumer subtracts
from a per-gametick allowance for scripts written by players, including
children. The README's Budget section states the intent: do not penalise
structural semantics, because users should be encouraged to write *more* code,
not less.

The operative consequence, which governs every decision below:

> **Never bill a script for our implementation artifacts.**
> Cost is what the script asked for, measured in units a player could name -
> bytes examined, bytes produced, elements visited. Not what our code happened
> to do to deliver it.

Two corollaries, both testable:

1. **Refactoring-invariance.** The same work costs the same however it is
   written. Extracting a helper, naming an intermediate, adding a guard clause,
   or using a capture instead of a bare pattern must not change the bill.
2. **Bill proportional work; fix superlinear artifacts.** Work proportional to
   what the script asked for gets charged. Work that is superlinear *only*
   because of how we store or re-derive things gets **removed, not billed**.

This is why `len` and `sub` must stop cloning the whole subject, and why
`gmatch_iter` must stop recopying the subject and recompiling the pattern on
every iteration: a full `gmatch` scan runs O(n) iterations, so billing that
copy would charge the single most legible way to walk words **quadratically in
the subject**. Declining to charge it is equally unacceptable - it would leave
an uncharged O(n^2) hole of exactly the kind this finding is about. So the
artifact goes away.

Legibility is never the more expensive option. If a charge point cannot satisfy
that, it is measuring us rather than the script, and it comes out.

---

## Agreed design

### Unit

One per deterministic logical work item: one table element visited, one byte
processed or emitted, one matcher primitive. No hardware-inspired divisor -
existing costs are already semantic (table construction is one per element,
`table.sort` is `n`, not `n log n`). Per charged native operation,
`cost = max(1, work)`. `string.len` is the sole zero-cost exception.

### Matcher charge points - per primitive, no batching

Settled against the alternative of batching within provably-linear scans: once
`singlematch` itself charges, there is nothing left to batch, and per-primitive
charging has **zero overshoot**. Correctness of the bound beats the branch.

- One unit at the top of every `match_at` loop iteration, before
  `items.get(item)` (`luapat.rs:460`, where the reserved-for-cost comment sits).
- One unit at the start of every `singlematch`, including out-of-range attempts.
- One unit per subject byte inspected by `match_balance`, including the opening
  test.
- One unit per byte compared by a backreference - replace the slice equality at
  `luapat.rs:521` with an explicit charged loop.
- One unit per comparison in literal search (`find_subslice`).

Charging only recursive entries is insufficient: long literal runs and every
depth-free continuation do unbounded work inside one invocation. Charging only
`singlematch` is insufficient: zero-width captures, frontiers, anchors, `%b`
and backreferences do work without it.

**The bound holds.** Every unbounded matcher loop either consumes directly or
re-enters `match_at`, which consumes before continuing. With remaining budget
`B`, at most `B` further primitives complete. `max_expand` cannot hide
combinatorial work because every backtracking candidate re-enters a charged
`match_at`.

**Captures must not pay for recursion.** `CaptureStart`/`CaptureEnd`/
`PositionCapture` recurse where a bare pattern iterates. They are charged the
same one unit per `match_at` entry as any other continuation, plus the bytes
they actually materialize - never a surcharge for the recursion itself. Pin it:
`(%a+)` and `%a+` cost the same modulo materialized capture bytes.

### Compilation

The matcher is **compiled** - `Compiler` interns 256-bit `ByteClass` bitsets, so
there is no runtime pattern-byte scan for bracket classes. `pattern.len()` is
the semantic compilation reservation, charged once per pattern the script
supplies. It is deliberately *not* an exact count of internal compiler steps
(the fixed 256-byte class expansion, `BTreeMap` comparisons) - those are
artifacts.

### Error channel

```rust
// patterns/errors.rs
pub(crate) enum MatchError {
    Pattern(PatternError),
    BudgetExceeded,
}
impl From<PatternError> for MatchError { .. }
```

`PatternError` and `LuaPattern::from_bytes_try(&[u8]) -> Result<_, PatternError>`
stay exactly as they are. The compile-time / match-time error split is contract
and is asserted by tests. Budget exhaustion surfaces only from matching, and
maps to `state.budget_exceeded_error()` (`ErrorKind::BudgetExceeded`), never
flattened into the pattern `RuntimeError` path. One conversion helper in
`string.rs`, used by all call sites.

### Meter threading

Per-call matcher context, **not** stored in `LuaPattern` - that would tie a
compiled pattern to borrowed `State` counters and stop `gsub` releasing the
borrow before re-entering Lua.

```rust
struct MatchState<'data, 'meter, 'cost> {
    subject: &'data [u8],
    pattern: &'data CompiledPattern,
    meter: &'meter mut CostMeter<'cost>,
    captures: [Capture; LUA_MAXCAPTURES],
}

pub(crate) fn matches_bytes_from(&mut self, subject: &[u8], init: usize,
    meter: &mut CostMeter<'_>) -> Result<bool, MatchError>;
pub(super) fn str_match(subject: &[u8], pattern: &CompiledPattern, init: usize,
    out: &mut [LuaCapture; LUA_MAXMATCHES], meter: &mut CostMeter<'_>)
    -> Result<usize, MatchError>;
```

`singlematch`, `match_balance`, `recurse`, `max_expand`, `min_expand`,
`match_at` and `captures` all become fallible over `MatchError`.

Callers reserve materialization/compilation cost *before* `from_bytes_try`,
scope `state.cost_meter()` around only the match, drop it, then translate.

### Refusal boundary

Errors kill the callback by design; side effects already committed are not
rolled back and must not be. The required guarantee is narrower: **a partially
built result is never returned, and a refused charge never advances iterator
state.**

- Charge capture materialization *before* pushing any capture value.
- Charge replacement bytes *before* scanning them.
- Charge emitted bytes *before* growing or appending to a result buffer.
- Finish and drop the matcher meter scope *before* any table lookup or function
  invocation that re-enters Lua.
- Never `push_bytes(result)` until every emitted byte is charged.
- `gmatch_iter` writes `pos` before pushing captures (`string.rs:713`); all
  capture-byte charges must be preflighted before that write, or a refusal
  consumes a match without returning it.

### Charge schedule

| Function | Charged work |
|---|---|
| `len` | **Zero.** Must stop cloning; read the length from the borrow |
| `sub` | Returned bytes, min 1. Must stop cloning the whole source |
| `upper`/`lower`/`reverse` | Input length, min 1 |
| `find`/`match` | Subject + pattern materialization, compilation, literal or matcher steps, returned capture bytes |
| `gmatch` creation | Subject/pattern materialization + `max(1, pattern.len())` compilation reservation. **Do not validate here** |
| `gmatch` iteration | Matcher steps + returned capture bytes **only**. No recopy charge, no recompile charge |
| `gsub` | Subject/pattern materialization, search/matcher work, replacement bytes examined, capture arguments materialized, emitted bytes |
| `format` | Format bytes scanned, argument bytes examined (incl. truncated `%s` and numeric-string parsing), emitted bytes |
| `table.concat` | Separator materialization, elements visited, emitted element and separator bytes |

Both sides for constructors: a no-match `gsub` charges once to scan and once to
copy.

Two corrections to the original sketch, both from reading:

1. **`gmatch` validation stays deferred.**
   `tests/string_bytes.rs:349` (`deferred_pattern_validation_keeps_existing_call_timing`)
   pins that `string.gmatch("abc", "%")` returns a function rather than
   erroring. It says nothing about recompiling, so validation defers to the
   first *iteration* while compilation still happens once.
2. **`format bytes + output bytes` is incomplete.** `string.format("%.0s", huge)`
   clones and scans the whole argument while emitting nothing, and numeric
   conversions parse arbitrarily long string arguments. Those input bytes need
   charges.

### Versioning

```rust
// cost_meter.rs
pub const COST_MODEL_VERSION: u16 = 2;   // 1 = the old implicit model
// lib.rs
pub use cost_meter::COST_MODEL_VERSION;
```

Snapshots persist `cost_remaining`, `cost_budget` and `cost_used`, so an old
counter under new units is invalid. In `save_state.rs`: bump `FORMAT_VERSION`
5 -> 6, write `COST_MODEL_VERSION` immediately after it, require strict equality
on both before decoding payload or running setup, add
`LoadError::UnsupportedCostModelVersion`. Old snapshots **fail to load**; they
must not load with reset counters, which would be a budget bypass. The README
save-state section already documents version rejection, so no new prose there.

No feature-gated legacy mode - the old behaviour is the hole, and compile-time
modes fragment a contractual cost.

### `analyze_cost` stays numerically unchanged

It cannot resolve data lengths, or even prove a call reaches the stdlib, since
globals are mutable. Document that explicitly on the function and in the CLI
`--analyze` output. Pin it: `analyze_cost("return string.upper('abcd')") == 0`
while execution adds exactly 4.

---

## Phases - validate between each, do not run them as one session

Phase boundaries are compile-clean. `brokkr fmt` + `brokkr check` between.

**Phase 1 - matcher core.** `cost_meter.rs` (`COST_MODEL_VERSION`, keep
`consume` inline), `patterns/errors.rs` (`MatchError`), `patterns/luapat.rs`
(meter in `MatchState`, all charge points, charged backref loop, fallible
helpers), `patterns/mod.rs` (signatures, `MatchError` export, update unit tests
with a `CountOnly` meter, add exact finite-budget tests for `%b`, backrefs,
outer search, greedy/minimal expansion, pathological nested greedies).

**Phase 2 - artifact removal.** `len` reads length from a borrow instead of
`string_or_number_bytes`. `sub` slices instead of cloning. `gmatch_iter` stops
recopying the subject and recompiling: a `State` helper hands back a subject
borrow and a `CostMeter` from disjoint fields in one call, captures push via an
index+range helper rather than an external slice borrow, and the compiled
pattern is memoized. No charge points yet.

**Phase 3 - stdlib charging.** `string.rs` helpers (`charge_cost`, charged
`find_subslice`, `append_bytes` taking `&mut State`, capture preflight helpers,
the `MatchError` conversion helper) then each function per the schedule.
`string_format.rs` (`append_output`, argument-byte charges in `format_argument`
and `number_argument`). `table.rs` `concat` (drop the flat 1; separator
materialization; per-element charge before each lookup under a finite budget,
one reservation under `CountOnly`; exact emitted bytes before each append).

**Phase 4 - versioning, docs, tests, release.** `save_state.rs` header gate and
`tests/save_state.rs` cases (format and cost-model mismatch independently,
rejection before setup, counter round-trip on match). `lib.rs`/`main.rs` export
and docs. New `tests/string_cost.rs`. README Budget rewrite + perf table
refresh. CHANGELOG. `Cargo.toml`/`Cargo.lock` to 0.4.0.

## Tests that enforce the principle

These are not optional extras; they are what stops the principle being prose.

- `string.len` on a large string adds **0**.
- `(%a+)` costs `%a+` plus a **constant independent of subject size**. Measured:
  the delta is 600 over 100 calls at a 23-byte subject and 600 over 100 calls at
  a 1472-byte subject - flat 6 per call, 0.4% of the call cost. Capture items
  are real pattern items the script wrote and cost O(1) each per match; what the
  principle forbids is a surcharge that *scales*, so pin the constancy, not
  equality. A test that asserts the delta is unchanged across two subject sizes
  is the one with teeth.
- An N-iteration `gmatch` over a subject costs O(N x steps), **not**
  O(N x (subject + pattern)) - assert the total for a full scan is linear in
  the subject, not quadratic.
- The same work split across an extracted helper costs what the inlined version
  did.
- Exact basic charges for every affected function; `upper("")`, empty `sub`,
  `format("")` and empty-range `concat` each add exactly 1.
- Matcher refusal surfaces `ErrorKind::BudgetExceeded`.
- Pathological backtracking stops at exactly the configured budget.
- No partial `gsub` result; replacement-callback side effects persist despite a
  later refusal.
- `gmatch` refusal does not advance iterator position.
- State reuse after refusal; under `snapshot`, quiescent after each refusal.
- `table.concat` element, separator, output and empty-range accounting.
- `CountOnly` determinism across repeated runs.

## Benchmarks to report after phase 3

`./hotbench.sh strings/mixed --runs 20` and
`./hotbench.sh strings/patterns --runs 20`. Baseline on this host:
`strings/mixed` 405 us warm, cost 2,705; `strings/patterns` 63 us warm, cost 39.
Report median and spread, percentage regression against the pre-change commit,
`warm_avg_cost` churn, and `CountOnly` versus a very large finite budget. The
two existing targets cannot isolate the per-primitive increment, so add a
focused literal-search and a pathological-backtracking case.
