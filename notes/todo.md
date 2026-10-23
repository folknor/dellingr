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

- The header comments of `examples/globals/write.lua` and
  `examples/calls/global.lua` still describe the old global store (IndexMap
  lookups, no SET_GLOBAL inline cache). Both are hash-pinned workloads, so
  rewording them means re-hashing their `brokkr.toml` pins.
- The README's `globals/write` ratio row predates the change and may be stale.
- The compiler verifier still requires GET/SET_GLOBAL names to be UTF-8,
  although the runtime keys the global table with the interned literal and no
  longer needs it. Either drop the check or say why it stays.
- `SnapshotField::GlobalName` now only serves the name-length error in
  `set_global_named_rust_fn`; fold it or rename it to what it covers.
- Strict-mode globals (`setmetatable(_G, {__index = ..., __newindex = ...})`)
  remain unavailable: `_G`'s metatable is protected and bare global access is
  raw. Supporting it would mean GET/SET_GLOBAL honouring `__index` and
  `__newindex`, a feature decision rather than a fix.

## Deferred forward-looking ideas

### `\u{XXX}` string escapes

What: Lua 5.3+ accepts `\u{XXX}` UTF-8 escapes in string literals; dellingr's
lexer does not, and nothing documents the gap (tests write UTF-8 as `\x`
bytes). Either add the escape or list it under the README's won't-implement
or divergences sections.

Signal: a script author reaching for it.

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

Why deferred: the registry is live with the curated workloads;
porting the rest before those have proven the recipe would be
premature batch work.

Signal that would promote it: active work starting on any candidate
whose tracker is still examples/-only.

### Configurable per-category cost weights

What: let library consumers set their own cost per category rather than
the fixed unit prices. Some embedders might want arithmetic to cost less
than allocations, or native string work to cost more than bytecode.

The cost model has two halves, and weights would have to cover both:

- Bytecode: the eval loop charges 1 per arithmetic op, negation, table
  creation, table write and array element, accumulated locally and
  flushed to the state every `COST_CHECK_INTERVAL` units (or sooner when
  the budget is nearly spent). Calls, reads, comparisons and control flow
  are free. These are the costed categories `analyze_cost` enumerates;
  its `function_calls` and `instructions` counters are informational and
  carry no cost.
- Native: the string, pattern, table and math
  libraries charge data-dependent work - bytes examined or emitted,
  matcher primitives, table elements visited - through `CostMeter` and
  `State::consume_cost`, some per unit, some batched via
  `consume_units`.

Sketch: `State::set_cost_weights(weights: CostWeights)`, one field per
category across both halves, default all 1 so current behaviour is
unchanged. Bytecode weights can be folded into the per-op charge before
it hits the local accumulator; native weights scale the unit count at
each `CostMeter` call site. `consume_units`' contract (exactly the
outcome of `n` unit charges) needs restating for weighted units.

Weights change what a remaining budget means, so they are part of the
cost model's identity. `save_state` writes `COST_MODEL_VERSION` and
refuses a mismatch on load; a snapshot resumed under different weights
would be the same hazard, so the weights (or a digest of them) have to
be persisted and checked alongside the version.

Why deferred: not user-requested by the current consumer; adds a
multiply to every costed op on the eval-loop hot path and to every
native charge site; complicates `cost_used` interpretation across
configurations and snapshots. Worth doing once there's a concrete
second consumer with different cost-budget needs.

Signal that would promote it: a real embedder asking for non-uniform
weights, or a benchmarking case where the uniform-cost model
materially misrepresents the actual VM work.

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
