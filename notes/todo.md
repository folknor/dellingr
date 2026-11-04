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

- Budget exhaustion kills the callback. The user wants to revisit whether
  running out of budget should do something else; not decided. (The separate
  question of runaway free loops is settled: see `docs/budget.md`, "Runaway
  loops are not the VM's problem".)
- Every metered snapshot stores all `Charge` weights, `4 * Charge::COUNT`
  bytes, even under the default model. A tag meaning "default model" would
  shrink every default save, at the cost of another format bump.
- `COST_MODEL_VERSION` stayed 8 while its documented meaning widened to
  cover attribution and admission for every model. Judged right because
  default costs and behaviour are unchanged; revisit if anyone keyed stored
  fingerprints on the old meaning.

Known laterals, pre-existing and deliberate (record, don't "fix" without a
cost-model decision):

- `string.format` charges a `%s` argument's bytes when it is consumed and
  again when the result is appended; `%q` charges the argument, then the
  longer quoted output. A code comment says the double charge is intended.
- `string.find`, `match` and `gsub` charge the pattern bytes twice: inside
  `max(1, subject + pattern)` and again as `max(1, pattern)`. `gmatch`
  charges the sum at creation and the pattern again on the first iterator
  call. An empty `gmatch` pattern charges nothing after creation.
- A `gsub` string replacement template charges its length up front on each
  applied match and then charges every byte it emits, so a literal template
  `xyz` costs 6 per match while a function returning `xyz` costs 3 output
  bytes. `docs/budget.md` now states the units; whether the asymmetry is
  intended is open, and removing it would bump `COST_MODEL_VERSION`.
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

What: `bench/` ports only the curated benches registered in brokkr.toml.
The optimization backlogs name
more trackers that exist in `examples/` but have no seconds-scale
verdict port yet: the `calls/` family (`local`, `global`, `method`,
`method_cached`, `method_chain`, `vararg`, `fixedarg`, `general`,
`factory_closure` - the measurement surface for the frame-flattening
and closure-clone rewrites), `fields/polymorphic` / `same_obj_cached` /
`same_obj_write`, `tables/numeric_index` / `mixed`, `alloc/short_tables`,
`iter/ipairs`. Port with the same recipe (kernel + calibrated repeat
loop, ~100ms/call, footer 30) and register both pins (the bench/ port
as `file`, the examples/ original as `hotpath_file`).

Also open: a seconds-scale parse
workload (`large_source` at a second, larger size), and a Rust-driven
snapshot bench
(optimizations.md "Snapshot path" - needs harness support, not a .lua
file).

Also open: a workload where one State holds a lot of loaded code and a small
live heap, so that per-collection work proportional to retained runtimes
(the runtime-cache walk, which since 2026-11-04 runs only every
`CACHE_PRUNE_INTERVAL` collections, and the per-runtime
`GcRoot::ChunkLiterals` visit) is what dominates. No registered workload loads
more than one chunk, so the change that put the cache prune on a cadence was
measured only on `gc_churn`. Scripts cannot load code, so this also needs a
Rust-driven harness that loads many Programs into one State.

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
