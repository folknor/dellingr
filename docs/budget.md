# Cost budget

The budget is a game-design instrument, not a CPU meter. Charges are design
decisions. Flat and free charges are often deliberate, and structural
semantics are free on purpose: extracting a helper, naming an intermediate or
any other legible refactor must not make a script more expensive. Host work
out of proportion to its charge is a performance concern, not a cost-model
defect.

## Mechanics

- `cost_used` counts everything charged. `set_cost_budget` sets a finite
  budget; without one, cost is counted only.
- The counter is signed. The operation that crosses the budget completes; the
  next costed operation fails.
- Opcode charges are batched in the dispatch loop and checked every
  `limits::COST_CHECK_INTERVAL` static units. Opcode work still pending when a
  runtime error ends a frame is charged first, so `cost_used` after a failed
  callback counts every executed costed opcode.
- Loading a snapshot restores the saved counters. Call `set_cost_budget`
  afterwards.
- Stack-cap enforcement charges nothing.

## Free by design

- Structural constructs, for example `while true do end`.
- `select`: it moves its arguments once, as passing `...` along does.
- `unpack` and `table.unpack`: free however many elements they visit.
- `string.len`, `tostring`.

## Native charges

Math library: every function charges a flat 1.

String library, in bytes:

- `sub`, `upper`, `lower`, `reverse`: bytes in the result or processed (at
  least 1).
- `find`, `match`, `gsub`, `gmatch`: subject length plus pattern length (at
  least 1) up front.
  - `find`, `match` and `gsub` also charge the pattern bytes a second time.
  - Matcher primitives, including backtracking steps, charge as they run.
  - `gsub` also charges replacement text bytes. With a replacement limit of zero
    it charges the subject length.
  - `gmatch` charges pattern compilation once per iterator.
- `format`: format string length (at least 1), plus bytes appended to the
  output, plus bytes of string arguments consumed.

Table library:

- `insert`, `remove`, `pack`: flat 1, however many elements they shift or
  store. This is a deliberate cost-model choice. The metamethod path charges
  the same.
- `move`: 1 per element copied.
- `concat`: 1 per element visited, plus bytes copied, plus separator bytes.
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
and branches count once. The result is neither a runtime lower nor upper bound
and excludes data-dependent native work.

The report labels call counts as `Call instructions` and shows them per scope
even when the scope's cost is zero. Anonymous scopes are numbered among
themselves, not among all sibling functions.
