# Cost budget

The budget is a game-design instrument, not a CPU meter. Charges are design
decisions. Flat and free charges are often deliberate, and structural
semantics are free on purpose: extracting a helper, naming an intermediate or
any other legible refactor must not make a script more expensive. Host work
out of proportion to its charge is a performance concern, not a cost-model
defect.

There is one deliberate exception: constant folding. The compiler evaluates
`+ - * / %` over two number literals, and negation of a literal, at compile
time, so `60 * 1000` costs nothing while `local k = 60; x = k * 1000` is
charged for the multiplication. `^` is never folded and is always charged, as
is any operation whose result would be NaN or negative zero, or that finds the
literal pool full.
Folding is a compile-time optimization the cost model does not undo; the
operand literals also stay in the chunk's literal pool as unused entries.

## Mechanics

- `cost_used` counts everything charged. `set_cost_budget` sets a finite
  budget; without one, cost is counted only.
- The counter is signed. The operation that crosses the budget completes; the
  next costed operation fails.
- Opcode charges are batched in the dispatch loop and checked every
  `limits::COST_CHECK_INTERVAL` static units. Opcode work still pending when a
  runtime error ends a frame is charged first, so `cost_used` after a failed
  callback counts every executed costed opcode.
- `set_cost_budget` also zeroes `cost_used`. Calling it from a Rust callback
  mid-run is supported: pending opcode cost is flushed before every call out
  of the dispatch loop, so the reset never races the batch.
- Loading a snapshot restores the saved counters. Call `set_cost_budget`
  afterwards.
- Stack-cap enforcement charges nothing.

## Static opcode charges

These rows describe the units returned by `Instr::static_units()`. The table
is checked against every declared opcode by a unit test in `vm/frame.rs`.

| `OpcodeCost` | Units from `static_units()` |
| --- | --- |
| `Free` | 0 |
| `Call` | 0 |
| `Arithmetic` | 1 |
| `Negation` | 1 |
| `TableCreation` | 1 |
| `TableWrite` | 1 |
| `ArrayElements` | encoded count |

A `SET_LIST` ending in a call or `...` has an open count, so static analysis
counts zero for it; at runtime it charges the live number of elements.

## Free by design

- Structural constructs, for example `while true do end`.
- `..`: concatenation output bytes are uncharged; string size remains capped.
- `print`: host output is free by design.
- `select`: it moves its arguments once, as passing `...` along does.
- `unpack` and `table.unpack`: free however many elements they visit.
- `string.len`, `tostring`.

## Native charges

Math library: each function charges a flat 1: `abs`, `acos`, `asin`, `atan`,
`atan2`, `ceil`, `cos`, `deg`, `exp`, `floor`, `fmod`, `log`, `max`, `min`,
`modf`, `rad`, `random`, `sin`, `sqrt`, and `tan`.

String library:

- `sub`: output bytes, at least 1.
- `upper`, `lower`, `reverse`: input bytes processed, at least 1.
- `find`, `match`, `gsub`: `max(1, subject bytes + pattern bytes)` up front,
  plus `max(1, pattern bytes)`; scanning and matcher operations charge as they
  run. `gsub` also charges replacement text bytes. With a replacement limit
  of zero it charges the subject length.
- `gmatch`: `max(1, subject bytes + pattern bytes)` when the iterator is
  created; `max(1, pattern bytes)` on the iterator's first call that reaches
  the matcher, and matching work as it runs. A leading `^`, which `gmatch`
  matches as a literal byte, counts as the one byte it is. An empty pattern
  charges nothing after creation.
- `format`: format string bytes, at least 1, plus bytes appended to the output
  and bytes of string arguments consumed.

Table library:

- `insert`, `remove`, `pack`: flat 1, however many elements they shift or
  store. This is a deliberate cost-model choice. The metamethod path charges
  the same.
- `move`: 1 per element copied, with an empty range charging 1.
- `concat`: 1 per element visited, plus bytes consumed from the separator and
  element values, plus bytes of separators appended. An empty range with an
  empty separator charges 1.
- `sort`: the length of the original range (at least 1), charged before any
  comparator runs. A Lua comparator also incurs its normal execution cost, as
  do sequence-library metamethod handlers.

## Flat charges are deliberate

Native costs are per-function literals, not a rule table. The numbers are part
of the cost model. Changing any of them, including making a free function
charge, changes `cost_used` for existing scripts and replays, so it needs a
`COST_MODEL_VERSION` bump. Do not "fix" a flat charge to be proportional.

## `COST_MODEL_VERSION`

Identifies the `cost_used` that a given source produces: opcode charges,
native charges and compiler emission together.

- It moves whenever `cost_used` for identical source changes.
- It is a valid key for stored cost fingerprints.
- Snapshots load only under an exact match.
- The doc comment on the constant carries the per-version history.

## Static analysis

`analyze_cost` (and `dellingr --analyze`) sums each costed opcode once across
the main chunk and every nested function body, whether or not invoked. Loops
and branches count once. Call opcodes have no static charge, and the analyzer
does not inspect their callees, so all native library charges are excluded,
both flat and data-dependent. The estimate does not bound runtime cost or
estimate native-library cost.
`CostAnalysis::caveat()` returns the same user-facing explanation printed by
the CLI.

The report labels call counts as `Call instructions` and shows them per scope
even when the scope's cost is zero. Anonymous scopes are numbered among
themselves, not among all sibling functions.
