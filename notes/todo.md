# todo.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[optimizations.md](optimizations.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

## Next up

### Housekeeping left from the 2026-10-22 loops

- libbeef 0.1.1 bugs the crmath test oracle works around, reported
  upstream on 2026-10-22: lifthrasiir/libbeef#2 (`pi` returns 0 from 688
  bits up, taking `atan` with it), #3 (power-of-two base `pow` truncates
  the exponent), #4 (`sin` panics on huge arguments), #5 (`to_f64`
  misrounds small inexact subnormals). When a release fixes them, bump the
  dev-dependency and drop the matching workarounds in
  `src/crmath/tests/oracle.rs` (and the `pow` note in Cargo.toml).
- Possible CORE-MATH bug: `is_exact` in pow.c measures the bit length of `m`
  rather than `m^y`. Ported as is; unobservable here (the sweeps pass).

### One rule for string costs (decide before the next release)

Goal: replace today's per-function string charges with one rule a script
author can state in a sentence, write that rule into `docs/budget.md`, and
make every string operation obey it. Any change in charges bumps
`COST_MODEL_VERSION` (and is replay-visible through `cost_used`); do it once,
before a release, and fold every string change into that one bump.

Why it matters: in the programming game (players' scripts compete per tick
under one budget) players optimise against the cost model. A rule they can
predict matters more than the exact numbers, and any free operation that can
stand in for a priced one will be found and used. In the RTS nobody will
notice either way.

#### The principle (settled with the user, 2026-11-05)

String work is charged only where it would otherwise be a proxy for work the
budget prices. This is the same reasoning the README has used since May for
excluding `string.rep`, `string.byte` and `string.char` ("could be used to
work around restrictions the game wants to put on the user"), and it sits on
the older rule that structural code is free so that authors are encouraged to
write more code, not less. The budget is a game-design instrument, not a CPU
meter (`docs/budget.md`): host work out of proportion to its charge is a
performance concern, not a reason to charge.

Applied, the leaning (not yet the decision) is: **pattern work costs per
primitive scanned; producing strings is free.**

- Pattern matching keeps its per-primitive charges. Matching is real
  computation (backtracking can be superpolynomial), and making it free would
  turn `find`/`gsub` into a free search-and-filter engine standing in for
  charged arithmetic and table work.
- Emitted bytes go free in `string.format`, `gsub` output and anything else
  that charges for bytes it produces, because `..` already produces bytes
  for free. Today a player avoids the `format` charge by concatenating by
  hand, which is exactly the kind of trick the rule must not leave open.

#### History: what was decided and what was only inferred

- 2026-05-04 READMEs (`0d75771`, `acdd010`): structural semantics free; the
  `rep`/`byte`/`char` exclusion as a proxy defence. Nothing about `..`.
- 2026-07-25 `6f9db93` (released as 0.4.0): before it, no string or pattern
  work was charged at all. Its plan, recoverable with
  `git show 6f9db93:WORK.md`, is headed "Agreed design" and set the unit as
  "bytes examined, bytes produced, elements visited", with the constraint
  "never bill a script for our implementation artifacts" and "legibility is
  never the more expensive option". That is where `format`, `gsub` and
  `table.concat` got charged for emitted bytes. The plan never mentions `..`,
  although its own motivation reads "a script could build a large subject
  with a handful of concatenations": it closed the work done on a big string,
  not the building of it.
- `docs/budget.md` "Free by design" lists "`..`: concatenation output bytes
  are uncharged". That line describes the status quo; the user never ruled
  on `..`. The `-S` search places the wording in the docs restructure for the
  0.4.0 release (`3170f83`, 2026-07-27), but that diff has not been read.

So the July design ("bytes produced cost") and today's leaning ("producing
strings is free") point opposite ways, and `..` is the loose end either way:

- Keep "bytes produced cost": `..` starts charging per output byte. Large
  change; ordinary string building gets expensive in both games.
- Adopt "producing strings is free": drop the output-byte charges from
  `format`, `gsub` and `table.concat` (check `upper`/`lower`/`reverse`/`sub`
  too) and keep only examination and pattern charges.

The user has not chosen between these yet. Ask before implementing.

#### Current charges to reconcile (from the earlier laterals list)

Read the code to confirm each before acting; these were recorded from
earlier reviews.

- `string.format` charges a `%s` argument's bytes when it is consumed and
  again when the result is appended; `%q` charges the argument, then the
  longer quoted output. A code comment says the double charge is intended.
- `string.find`, `match` and `gsub` charge the pattern bytes twice: inside
  `max(1, subject + pattern)` and again as `max(1, pattern)` (the July plan's
  "compilation reservation"). `gmatch` charges the sum at creation and the
  pattern again on the first iterator call; an empty `gmatch` pattern charges
  nothing after creation. Decide whether compiling a pattern is a priced step
  of its own; if it is, document it as one.
- A `gsub` string replacement template charges its length up front on each
  applied match (the scan for `%1` captures) and then every byte it emits, so
  a literal template `xyz` costs 6 per match while a function returning
  `xyz` costs 3 (`CALL` is free). Under "producing strings is free" both
  drop their output charge and the template keeps only its scan.

#### The proxy check to run on every free string operation

For each string operation the rule leaves free, ask whether a script can use
it in place of something charged. The known case: strings as a table
substitute. `t[i] = x` costs a `TableWrite`, while `s = s .. x` is free and
reading back may be cheap (`string.sub` charges its returned bytes, per the
July schedule; confirm). Each "write" copies the whole string, which is
host work the budget does not price, and the 16 MiB string cap bounds it, so
it may be self-limiting enough to accept. Record the verdict either way in
`docs/budget.md`.

#### Deliverables

1. The user's choice between the two directions above.
2. `docs/budget.md`: one stated rule for string costs, the per-function
   charges as consequences of it, and the proxy verdicts.
3. The code changes, one `COST_MODEL_VERSION` bump, regenerated
   `tests/fixtures/corpus_cost.txt` after review, CHANGELOG entry calling out
   the cost change as replay-visible.

### Housekeeping left from the `string.format` literal-run change (2026-11-05)

- AGENTS.md (Benchmarks, measurement sites) says to measure
  `brokkr test native_bytes_per_level` "dev and `--release`". `--release`
  forces release onto every sweep, so the debug-assertion-only sweeps
  (`instrumentation-faults`, `inline-cache-differential`,
  `cost-check-interval-differential`, `native-call-coverage`) fail to build
  and the run exits 1, though the release rows still print. The plain
  invocation already runs both profiles (`release-native-stack*` sweeps);
  reference/native-calls.md now says so. The AGENTS.md line is the owner's
  to reword.

### Housekeeping left from the `_G`-as-real-table change (2026-10-23)

- Several `bench/` verdict headers (arithmetic, pairs, fill, queue, mixed,
  patterns, literal_find, closure, record_tables) still say "see that file
  for the probe rationale", while their `examples/` partners are now
  `-- Workload:`. Rewording them re-pins the verdict files and cuts their
  stored history; do it the next time those workloads are re-pinned anyway.
- Strict-mode globals (`setmetatable(_G, {__index = ..., __newindex = ...})`)
  remain unavailable: `_G`'s metatable is protected and bare global access is
  raw. Supporting it would mean GET/SET_GLOBAL honouring `__index` and
  `__newindex`, a feature decision rather than a fix.

### Left from the configurable-costs, raw_field and naming work (2026-11-01)

Residuals that need a decision:

- Budget exhaustion kills the callback. Revisited 2026-11-05 against both
  target games and kept: in the user-scripted AI game (one budget per bug
  per tick) the kill is a fair game rule, and pausing across ticks would hand
  a script extra budget; in the RTS the budget is a safety net and waits are
  host callbacks dispatched from a Lua timers table (dellingr#3, closed), so
  each due callback is its own budgeted call. Before a release:
  - Document the kill as the intended outcome in `docs/budget.md`: the error
    kind a host matches on, which side effects of the killed call persist,
    and that the State is ready for the next call. Also say that a host
    exposes the remaining budget to scripts with its own host function over
    `State::cost_remaining()` (exact mid-run, since the batched opcode cost
    is flushed before any call leaves the dispatch loop); the VM adds no
    script-side API for it.
  (The separate question of runaway free loops is settled: see
  `docs/budget.md`, "Runaway loops are not the VM's problem".)
- Every metered snapshot stores all `Charge` weights, `4 * Charge::COUNT`
  bytes, even under the default model. A tag meaning "default model" would
  shrink every default save, at the cost of another format bump.

Known laterals, pre-existing and deliberate (record, don't "fix" without a
cost-model decision):

- The string-library charges (`format`, `find`/`match`/`gsub`/`gmatch`,
  `gsub` templates) moved to "One rule for string costs" above.
- `table.insert` and `table.remove` charge their flat 1 before validating
  their arguments, so an argument error still costs 1.
- `load_state` discards whatever setup charged or budgeted when it restores
  the saved counters; a host that wants a budget after loading sets it again.
- `State::raw_field` reads own fields only, so option tables built on an
  `__index` defaults metatable read their inherited fields as absent. A
  metamethod-honouring variant would be a separate operation with a
  native-call inventory entry; add it only if an embedder needs it.
- Argument errors from a plain Rust helper called directly by a callback
  (not dispatched through the VM) are attributed to that callback.

Cosmetic:

- Tracker comments with quoting damage that `bz` cannot edit: dellingr#3
  opens with "Won`t" and dellingr#6 writes `Send + static` for
  `Send + 'static`.

## Deferred forward-looking ideas

### Execution trace hook for a stepping debugger

What: a host-installed hook the VM calls on each new source line (or
instruction) with the line and `cost_remaining()`, so a programming-game
client can let the player step through a tick and watch the budget drain.
Nothing like it exists today: the host's only way in mid-run is the
`Interrupt`, which kills rather than pauses.

Preferred shape: record, then replay. The hook only appends `(line,
cost_remaining)` to a buffer during a normal run: it never blocks, never
re-enters Lua (so no native-call inventory entry or re-entry hazards) and
charges nothing (so tracing never changes the traced run). The client scrubs
the trace forwards and backwards; to inspect values at step N it reloads the
start-of-tick snapshot and reruns to N, which determinism makes exact. The
alternative, a blocking hook whose call hosts the debugger UI, avoids
suspension too but fires inside comparators and metamethods and gives no
backwards stepping.

Open: the hot-path cost of the check (feature flag or a separate traced
dispatch loop), line versus instruction granularity, and how a trace marks
nested native invocations.

Why deferred: purely additive and not replay-visible if it charges nothing,
so it does not gate a release.

Signal: work starting on the AI game's debug client.

### Observable GC and cache diagnostics

What: a collection counter on `GcHeap`, exposed as `State::gc_cycles()`
(not replay-visible, useful to embedders as a diagnostic), plus cache
residency, pattern compilation counts and slot identity through one
test-infrastructure feature or `#[doc(hidden)]` accessor usable from both unit
and integration tests. Today `State::pattern_compilations` is a `cfg(test)`
field visible only to unit tests (and makes the unit-test `State` a different
layout from production).

Why it matters: several tests exist to prove "a collection at point X does
not free Y" or "the cache was hit/evicted" but cannot assert the precondition
happened: the GC-rooting tests in `gc_upvalues.rs`, `table_metamethods.rs`,
`table_sort_metamethods.rs`, `src/vm/tests.rs`'s deep-chain test and
`string_charges.rs`'s gsub test; the gmatch cache-eviction and cache-hit cost
tests in `budget_and_iterator_charges.rs` and `string_charges.rs`; and
`string_metatable`'s slot-reuse tests. Each could then assert its
precondition, or construct the collision directly in a `vm/` unit test.

Why deferred: a new public API surface, which is a feature decision rather
than a test fix.

Signal: an embedder asking for GC diagnostics, or a GC rooting bug that one of
those tests should have caught.

### Sparse storage for the array part's stragglers

What: a table's array part stores one contiguous window of its logical
array (`src/vm/table.rs`), which handles FIFOs and cleared prefixes but not
an id-keyed registry (`entities[next_id] = e`, entities dying in any order)
where one long-lived low id keeps every slot up to the newest id stored.
Reference Lua rehashes such a table into its hash part, which would change
`pairs` order here. An order-preserving fix keeps the logical array as it is
and stores keys outside a dense window in a sorted side structure, so
`pairs` still visits array keys ascending; the window could then drop a
mostly-dead prefix even past a live straggler.

Signal: a game script holding a sparse long-lived id map, or a heap profile
showing an array window far larger than its live count.

### Grow the brokkr workload registry to the full candidate surface

What: `bench/` ports only the curated workloads registered in
`brokkr.d/bench.toml`.
The optimization backlogs name
more trackers that exist in `examples/` but have no seconds-scale
verdict port yet: the `calls/` family (`local`, `global`, `method`,
`method_cached`, `method_chain`, `vararg`, `fixedarg`, `general`,
`factory_closure` - the measurement surface for the frame-flattening
and closure-clone rewrites), `fields/polymorphic` / `same_obj_cached` /
`same_obj_write`, `tables/numeric_index` / `mixed`, `alloc/short_tables`,
`iter/ipairs`. Port with the same recipe (kernel + calibrated repeat
loop, ~100ms/call, footer 30) and register both pins in
`brokkr.d/bench.toml` (the bench/ port under `data.workloads.bench`, the
examples/ original under `data.workloads.hotpath`).

Also open: a seconds-scale parse
workload (`large_source` at a second, larger size), and a Rust-driven
snapshot bench (optimizations.md "Snapshot path").

The Rust-driven items here are no longer blocked on brokkr: a script bench
runs whatever binary its `[bench.*]` table builds, with any argv and params,
so each needs only dellingr-side code (a mode of `dellingr-bench` or a bin
of its own behind `bench-harness`) plus a bench table beside `wall`, `kernel`
and `alloc`. Mark its call as a hotpath stage to profile it.

Also open: a workload where one State holds a lot of loaded code and a small
live heap, so that per-collection work proportional to retained runtimes
(the runtime-cache walk, which since 2026-11-04 runs only every
`CACHE_PRUNE_INTERVAL` collections, and the per-runtime
`GcRoot::ChunkLiterals` visit) is what dominates. No registered workload loads
more than one chunk, so the change that put the cache prune on a cadence was
measured only on `gc_churn`. Scripts cannot load code, so this is a
Rust-driven bench that loads many Programs into one State.

Also open: State construction (`State::new`, `open_libs`) has no workload. The
harness reports `state_new_us`, one cold State per launch, so the change that
made the function table borrow the catalog's static ids (instead of copying
them per State) was read from interleaved single launches, not a stored
verdict. A Rust-driven bench building many States would measure what a host
building one State per script or per tick pays.

Why deferred: the registry is live with the curated workloads;
porting the rest before those have proven the recipe would be
premature batch work.

Signal that would promote it: active work starting on any candidate
whose tracker is still examples/-only.

### Typed `State<U>` for user-data

What: replace today's `Box<dyn Any + Send>` user-data slot with a
generic type parameter on `State`. `State<U>` carries a single `U:
Send + 'static` instead of erased `Any`, eliminating the downcast
on every access.

Sketch: `pub struct State<U = ()> { ..., user_data: U, ... }`.
`RustFunc<U> = fn(&mut State<U>) -> Result<u8>`. Stdlib functions
become generic (or stay tied to `State<()>`, with embedders writing
their own bridges). `Engine<U>` parameterized to match.

Why deferred: it infects every signature that touches `&mut State`,
including `RustFunc`, the host-callback trait, and every stdlib
function. The win over `Box<dyn Any + Send>` is one downcast per
access, which is microseconds at most. Not worth the cascading
generic churn pre-1.0 unless a concrete embedder pushes on it.

Signal that would promote it: a profile showing user-data downcasts
on the hot path, or a 1.0 API pass that lands a coherent generic
story across `State` / `Engine` / `RustFunc` / stdlib.
