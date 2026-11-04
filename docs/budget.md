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
  budget; without one, cost is counted only. `set_cost_budget` returns
  `ErrorKind::CostingOff`, changing nothing, on a State whose costing is off
  (see "Configuring costs").
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

Each opcode declares one charging rule in the opcode table (`OpcodeCharge` in
`src/instr.rs`): free, one unit of a `Charge` per execution, or, for
`SET_LIST` alone, one unit per element it stores. Dispatch and static analysis
both read that declaration. These rows give each opcode charge's static cost
under the default cost model, where every weight is 1; a row is a `Charge`,
and "none" covers every free opcode, `CALL` included. The table is checked
against every declared opcode by a unit test in `vm/frame.rs`.

| Charge | Operation | Default static cost |
| --- | --- | --- |
| none | | 0 |
| `Add` | `+` | 1 |
| `Sub` | `-` | 1 |
| `Mul` | `*` | 1 |
| `Div` | `/` | 1 |
| `Mod` | `%` | 1 |
| `Pow` | `^` | 1 |
| `Negate` | `unary -` | 1 |
| `NewTable` | `table constructor` | 1 |
| `TableWrite` | `table write` | 1 |
| `ArrayElement` | `table constructor array element` | encoded count |

A `SET_LIST` ending in a call or `...` has an open count, so static analysis
counts zero for it; at runtime it charges the live number of elements, all
at once.

## Free by design

- Structural constructs, for example `while true do end`.
- `..`: concatenation output bytes are uncharged; string size remains capped.
- `print`: host output is free by design.
- `select`: it moves its arguments once, as passing `...` along does.
- `unpack` and `table.unpack`: free however many elements they visit.
- `string.len`, `tostring`.
- Base-library operations `type`, `tonumber`, `rawget`, `rawequal`, `rawlen`,
  `next`, `pairs`, `ipairs` and its iterator, `getmetatable`, `setmetatable`,
  and `error`.

## Runaway loops are not the VM's problem

Structural code is free, so a loop that does only free work never exhausts a
budget: `while true do end`, a busy-wait such as `while unit.hp > 0 do end`, or
a loop whose increment was forgotten runs until the host stops it. This is
deliberate. dellingr has no instruction-count limit and no time limit, and
will not get one: nothing stops a script on its own. A host that wants to
stop one on demand triggers an interrupt (see "Stopping a script from the
host" below).

The games dellingr serves are played the way the old programming games were:
authors write scripts, publish them, and other players download them and run
them in a harness on their own machine. A script that hangs only hangs the
harness of the player who loaded it. That player restarts once and never loads
that script again. Its author gains nothing from releasing it, because a script
that hangs never plays and never wins, and the author would have seen it hang
the first time they ran it. There is no shared server whose tick a hanging
script could stall for other players.

The arguments for a limit, and why they don't hold here:

- *"The budget should bound running time."* It bounds the work a script does,
  as a game-design instrument (see the top of this page). It was never a
  running-time bound, and making loops cost something to turn it into one
  would break the rule that structural code is free.
- *"Costed work in the loop would end it anyway."* It does, and loops that only
  read, compare and call cost nothing by design. That changes nothing above:
  such a loop is a bug its author finds while testing.
- *"A hanging script is an attack on other players."* Only on a shared server.
  Here it is run locally by the player who chose to load it.
- *"Stock Lua can stop it with `lua_sethook`."* Matching reference Lua is not a
  goal in itself, and dellingr has no debug hooks at all. The hook exists for
  hosts that run untrusted code in a shared process, which these games don't.
- *"A clean error naming the line would help authors debug."* It would, but
  convenience while developing doesn't justify a limit in the dispatch loop and
  its contract.

Configurable costs (`Costing`) do not change this: loops stay free under every
model.

## Stopping a script from the host

Every State polls an `Interrupt`, a shareable flag the host can set from any
thread or from a signal handler (Ctrl-C in a bench runner, a stop button in an
editor). Running Lua fails with `ErrorKind::Interrupted` at its next
safepoint. It is not a limit: nothing sets it but the host, and it charges
nothing and changes no cost.

```rust
use dellingr::{Interrupt, State};

let stop = Interrupt::new();
let mut state = State::builder().interrupt(stop.clone()).build();
// From another thread, or a signal handler: stop.trigger();
// Once the host has handled the error: stop.reset();
```

- **Safepoints.** Every backward jump, every Lua function call admission
  (from Lua or from Rust), entry to every Rust callback (including library
  functions and Rust metamethod handlers), and the library work that could
  otherwise run without bound: pattern-matcher steps, plain substring search
  candidates, heap-sort steps, `string.format` directives, and the iterations
  of `table.concat`, `table.move`, the metamethod paths of `table.insert`,
  `table.remove` and `table.sort`, and the empty-pattern `string.gsub`.
  `vm/interrupt.rs` lists them.
- **Latency.** Nothing a script can repeat without bound avoids a safepoint,
  so the work between two polls is bounded by the VM's fixed limits, never by
  the script. It is a run of instructions with no backward jump and no call
  (bounded by code length and call depth, since returns do not poll), where
  each instruction or library step makes at most one linear pass over a
  string or a table's storage; plus any garbage collection, which traces the
  whole heap and visits the runtime of every loaded function, and now and
  then walks every inline-cache slot of that code (sized by what the host
  loaded, which no script can grow); plus the body of a host callback, which the VM cannot stop. A
  host callback that must be stoppable polls its own condition. This is a
  bound on work, not on elapsed time.
- **Errors.** `Interrupted` unwinds like any runtime error. An interrupted
  call admission is a refusal: the error is reported at the caller's call
  instruction and the callee never runs. Lua admission checks
  `CallDepthExceeded` before polling and reserves the callee's stack after
  polling. Rust callback entry polls before changing callback bookkeeping;
  argument preparation and callable resolution have already happened.
- **Sticky.** The VM only reads the flag. While it is set, every safepoint
  fails, including the first one of a later call, until the host calls
  `reset`. A Rust callback that swallows the error lets its Lua caller
  continue only to that caller's next safepoint. A reset while a script still
  runs can withdraw a request no safepoint has observed yet.
- **Sharing.** Clones share one flag. `StateBuilder::interrupt` gives a State
  a host-held handle, so one trigger stops every State built with it;
  `State::interrupt` returns the State's own handle and `State::set_interrupt`
  replaces it at any time, for example in a `load_state` setup. Snapshots do
  not save it.
- **Cost.** An interrupted operation has charged what it charged before the
  safepoint. Raw `table.move` prepays its whole range, so after an
  interruption `cost_used` can count copies it never made. Interrupted runs
  are not replayable, so this never reaches a replay.

## Native charges

Base library: `rawset` charges a flat 1, like a `TableWrite` opcode.

Math library: each function charges a flat 1: `abs`, `acos`, `asin`, `atan`,
`atan2`, `ceil`, `cos`, `deg`, `exp`, `floor`, `fmod`, `log`, `max`, `min`,
`modf`, `rad`, `random`, `sin`, `sqrt`, and `tan`.

String library:

- `sub`: output bytes, at least 1.
- `upper`, `lower`, `reverse`: input bytes processed, at least 1.
- `find`, `match`, `gsub`: `max(1, subject bytes + pattern bytes)` up front,
  plus `max(1, pattern bytes)`; scanning and matcher operations charge as they
  run. `find` charges returned capture bytes only on its Lua-pattern path;
  `match` and each `gmatch` result charge returned capture bytes. `gsub`
  charges capture bytes passed to a function replacement or used as a table
  lookup key. Position captures have no capture bytes.
  `gsub` also charges every byte copied into its result, including unmatched
  subject spans and whole matches kept by a nil or false function/table
  replacement. A string or number replacement template charges its original
  length once per applied match, in addition to the literal and capture bytes
  it emits. With a replacement limit of zero it charges the subject length.
- `gmatch`: `max(1, subject bytes + pattern bytes)` when the iterator is
  created; `max(1, pattern bytes)` on the iterator's first call that reaches
  the matcher, and matching work as it runs. A leading `^`, which `gmatch`
  matches as a literal byte, counts as the one byte it is. An empty pattern
  charges nothing after creation.
- `format`: format string bytes, at least 1, plus output bytes appended and
  input bytes consumed from string arguments. `%s` charges the full `tostring`
  rendering of any argument, including numbers and `__tostring` results,
  before precision truncation. Numeric conversions charge string input bytes
  they parse, and `%q` charges string input bytes it quotes. Output bytes,
  including padding, are charged separately.

Table library:

- `insert`, `remove`, `pack`: flat 1, however many elements they shift or
  store. This is a deliberate cost-model choice. The metamethod path charges
  the same. The metamethod paths for `insert` and `remove` reject a shift
  distance at or above `TABLE_SORT_MAX_LEN` with `SequenceShiftTooLarge`
  before any `__index` or `__newindex` shift access.
- `move`: 1 per element copied, with an empty range charging 1.
- `concat`: 1 per element visited, plus bytes consumed from the separator and
  element values, plus bytes of separators appended. An empty range with an
  empty separator charges 1.
- `sort`: the length of the original range (at least 1), charged before any
  comparator runs. A Lua comparator also incurs its normal execution cost, as
  do sequence-library metamethod handlers.

## The cost registry

Every costed operation is registered in `src/cost_model.rs` as a `Charge`:
each costed operator opcode (`+ - * / % ^`, unary `-`, table creation, table
write, and each array element a constructor stores) and each costed library
function. A charge site measures its work in the units listed above (one per
call, per byte, per element or per matcher step) and charges units times the
charge's weight, saturating. A library function's weight also covers the
matcher and substring-scan work it runs. The default model weighs every charge
1, which gives the charges this page lists. Free operations have no registry
entry, so no model can make them cost anything. `Charge::ALL` lists the
registry and `Charge::name` names each entry.

## Configuring costs

A State's costing is chosen when it is built and never changes:

```rust
use dellingr::{Charge, CostModel, Costing, State};

let model = CostModel::default()
    .with(Charge::Pow, 10)
    .with(Charge::StringFormat, 0);
let state = State::builder().costing(Costing::Metered(model)).build();
```

`State::new` is the default model. `State::costing` reports what a State was
built with.

- **Weights.** A `CostModel` holds one integer weight per `Charge`. Weight 1
  is the default; to make one operation relatively cheaper, raise the others.
  Weights are not fractions.
- **Zero weight.** A charge with weight 0 is free, exactly like an operation
  with no registry entry: a zero-weight opcode skips the budget check, so it
  runs even when the budget is spent. `CostModel::zero()` makes every
  registered charge free. Host charges through `consume_cost` and budgets still
  apply under it.
- **Off.** `Costing::Off` turns accounting off entirely. Every charge,
  including host `consume_cost`, is admitted without being counted;
  `cost_used` stays 0, `cost_remaining` is `i64::MAX`, and `set_cost_budget`
  is refused with `ErrorKind::CostingOff`, changing nothing. There is no
  budget to exhaust, so a script can run unbounded.
- **Host charges are raw.** `consume_cost` is never weighted.
- **Admission is unchanged by weights.** Each site keeps the granularity it
  has at weight 1. An atomic charge (an opcode, a flat library charge, a
  sort's up-front charge) completes when it crosses the budget, and the next
  one fails. A per-unit charge stops at the first unit that starts with no
  budget left: `table.move` copies exactly the elements it paid for, and a
  scan or pattern match stops part way. A unit that starts with budget left
  completes at its full weight, so the budget can be overshot by less than one
  unit's weight.
- **Static analysis.** `Program::analyze_cost_with(&model)` is
  `analyze_cost` under a model, which the analysis records
  (`CostAnalysis::model`). Unit counts are kept per `Charge` and do not depend
  on the model; costs are derived from them under it (see "Static analysis").

Snapshots save and restore the costing along with the counters (see
`save-state.md`).

## Flat charges are deliberate

The units each site measures are part of the cost model. Changing any of them
or a default weight, including making a free function charge, changes
`cost_used` for existing scripts and replays, so it needs a
`COST_MODEL_VERSION` bump. Do not "fix" a flat charge to be proportional.

## `COST_MODEL_VERSION`

Identifies the costs a source produces under any model: the units each charge
site measures, which charge the work is attributed to, how each charge is
admitted against a budget (atomic or per unit), the default weights, and
compiler emission.

- It moves whenever `cost_used` for identical source changes under the default
  model, and also when work moves from one charge to another or a charge's
  units or admission change, even if default costs stay the same: a
  non-default model prices the two charges differently.
- A cost fingerprint is the version plus the costing (`CostModel::iter` for a
  metered State, or Off). It is a valid key for stored cost fingerprints.
- Snapshots load only under an exact version match.
- The doc comment on the constant carries the per-version history.

`tests/fixtures/cost_golden.txt` pins source costs and fixed-input native
charges. The cost tests also check that every function registered in `_G`,
`math`, `string` and `table` has a call probe there; the documented free
functions are probed for a zero charge, and the `ipairs` probe also calls the
iterator it returns. Specialized tests read named pins (mostly cost deltas)
from the same fixture, so those values are versioned too.

Changing a fixture row requires a new version and an appended
(version, fingerprint) pair in `COST_MODEL_HISTORY` in
`tests/integration/cost/cost_golden.rs`. The test fails unless the last pair
is the current version with the current rows' fingerprint, and fails if those
rows match a fingerprint recorded under another version. The history is
append-only by convention: editing an existing pair in place would hide a cost
change under an old version, so a diff that touches one needs review.
The Unreleased changelog section must state "`COST_MODEL_VERSION` is N"
exactly once, with N the current version.
These probes cover fixed inputs; they do not prove that every possible
source keeps its cost. Changes outside their coverage still require review
under the version contract above.

## Static analysis

`analyze_cost` (and `dellingr --analyze`) sums each costed opcode once across
the main chunk and every nested function body, whether or not invoked. Loops
and branches count once. Call opcodes have no static charge, and the analyzer
does not inspect their callees, so all native library charges are excluded,
both flat and data-dependent. The estimate does not bound runtime cost or
estimate native-library cost.
`CostAnalysis::caveat()` returns the same user-facing explanation printed by
the CLI.

Each scope records how many units of each `Charge` its opcodes make
(`ScopeCost::units`), read from the same opcode declarations dispatch charges
by, so every operator is counted separately: a model that weighs `^` above `+`
shows where its cost comes from. The counts describe the program, not the
model. A scope's cost is derived from them by one rule, the sum of units times
weight over the charges, and the total adds the nested scopes' totals; every
product and sum saturates at `u64::MAX`. The analysis is read-only, so its
costs cannot drift from its counts and model.

The report names the model that priced it: `COST_MODEL_VERSION` and either
"every weight 1" or every weight that differs from 1. Its costed-operations
rows print `units x weight = cost` per charge with nonzero units, by the same
saturating rule, so a zero-weight charge still shows its units. The report
labels call counts as `Call instructions` and shows them per scope even when
the scope's cost is zero; calls are a statistic, not a charge. Anonymous
scopes are numbered among themselves, not among all sibling functions.
