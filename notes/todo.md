# todo.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[optimizations.md](optimizations.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

## Next up

### String cost model 9 is not yet approved (review before the next release)

The commit "One rule for string costs: cost model 9" (2026-11-14, the one
that added this section) replaced the per-function string charges with one
rule and bumped `COST_MODEL_VERSION` to 9. The user has not approved it and
is unsure about it. It shipped on an inferred ruling and then went further
than that ruling; nothing is released, so it can still be changed or
reverted, and any further change should fold into the same unreleased bump.

How it got here:

- The backlog entry it closed recorded a principle settled with the user on
  2026-11-05 (string work is charged only where it would otherwise be a proxy
  for work the budget prices) and a leaning, explicitly "not yet the
  decision": pattern work costs per primitive scanned, producing strings is
  free. It said to ask the user before implementing.
- Asked on 2026-11-14, the user replied "I thought we already ruled on
  strings". That was taken to mean the leaning. The user never said so in
  words.
- The implementer then went past the leaning, on an argument from its codex
  review partner.

What shipped (`docs/budget.md`, "String costs"):

- A string library call costs 1.
- `string.format` costs 1 per directive, at least 1.
- Pattern matching adds 1 per matcher step or substring-scan comparison.
- Bytes read and bytes produced are free, as for `..`.
- `table.concat` costs a flat 1.

Where that differs from the leaning: the leaning dropped only the charges for
bytes produced (`format`, `gsub` output, `table.concat`) and kept examination
charges. Model 9 also drops every charge for bytes examined: `find`, `match`,
`gmatch` and `gsub` no longer charge for the subject, the pattern, captures or
the replacement template, and only the matcher's own steps remain.

The argument behind the wider rule, as the implementer reported it (not
independently checked):

- Free structural code can already read any string: comparing with `<`
  against a known prefix identifies the next byte, and branching plus `..`
  rebuilds it. So a per-byte examination charge protects nothing.
- What a charge can still do is stop one library call from being a cheaper
  spelling of a priced operation (`math.abs` through `sub` of `tostring`); a
  flat 1 per call does that.
- `format` is per directive because one call can pack several roundings
  (`"%.0f%.0f"`).

Questions for the user:

- Is "bytes examined are free" acceptable, or should the subject and pattern
  charges come back? In the programming game this is the difference between a
  `find` over a 10 KB string costing its matcher steps only and costing at
  least 10 000.
- Is a flat 1 per library call the right floor, given that a long `sub`,
  `upper` or `reverse` now costs the same as a short one?
- Does the premise hold, that the `<` comparison trick makes per-byte charges
  pointless? It is slow in host terms and awkward to write; a player may still
  never do it, in which case the old charges were pricing something real.
- `string.format('%c', n)` produces any byte from a number, which is what the
  README's won't-implement `string.char` would do. It costs 1 per directive
  like any other, so it is not a cost bypass, but the `rep`/`byte`/`char`
  exclusion exists because those "could be used to work around restrictions
  the game wants to put on the user". Either `%c` is acceptable and the README
  should say why `char` is excluded while `%c` is not, or `%c` should be
  refused. This predates model 9; it was noticed during that work.

What a change of mind costs: the code and docs in that commit, plus the
regenerated `cost_golden.txt`, `corpus_cost.txt`, error-boundary transcript
and save golden, and the CHANGELOG "Changed (breaking)" entry. The `gmatch`
iterator state also lost its `compile_charged` flag in that commit, which a
return to a compile charge would need back.

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

Known laterals, pre-existing and deliberate (record, don't "fix" without a
cost-model decision):

- `table.insert` and `table.remove` charge their flat 1 before validating
  their arguments, so an argument error still costs 1. Since cost model 9,
  `table.concat` and `string.gsub` admit their charge after argument checks,
  so the table library is split on this.
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
- dellingr#7's close comment and commit 5b324b1 say `raw_field` takes "any
  fixed-width integer"; the trait doc now lists the actual set (8- to 64-bit,
  signed and unsigned).

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

The 2026-11-17 performance hunt (table-representation scope) reached the same
pathology plus a second one, and proposed separating numeric payload storage
from the logical array boundary. Fill `1..N` and clear `2..N-1`: two live
entries pin an N-wide window that iteration and GC scan. And the append policy
(`table.rs`) places by the historical `vlen`, so keys past the next append
position go to the hash part even when dense: filling `2..N` before `1` builds
a dictionary, then `append_growth` probes the run and `migrate_from_hash`
probes it again while moving it. Keep `vlen` as semantic history; keep
contiguous storage for dense runs and add segmented storage with occupancy
metadata for sparse ranges; let integer entries live in numeric storage before
they join the logical array, their iteration position staying in hash-order
history until the append rules advance `vlen`; migration then changes logical
membership and leaves tombstones without moving payloads; iteration and GC
skip empty regions. Risks: physical placement must never decide `pairs`
order; `#t` keeps `array_len`'s exact binary-search answer; snapshots
reconstruct today's canonical array span including interior nils and the
logical hash history; segmented access can slow dense reads, so the contiguous
form stays. High confidence in the pathologies, medium in their weight on
game throughput. (Performance, but filed here beside the matching entry.)

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

Also open (2026-11-17 performance hunt, host-boundary scope): the harness's
warm path (`run_warm_block`, `src/bin/dellingr-bench.rs`) reuses a function
already rooted on the stack and calls it with zero arguments and zero
expected results, so no registered workload exercises argument pushing,
result reading, record exchange or host callbacks against game data. A host
boundary rewrite would need a representative entity-tick bench to be kept or
reverted on; compute-heavy Lua workloads would largely miss its effect.

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
