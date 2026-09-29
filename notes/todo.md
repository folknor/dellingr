# todo.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[optimizations.md](optimizations.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

## Deferred forward-looking ideas

### Grow the brokkr workload registry to the full candidate surface

What: `bench/` currently ports the 15 curated benches (+ `large_source`
registered against its examples/ file). The optimization backlogs name
more trackers that exist in `examples/` but have no seconds-scale
verdict port yet: the `calls/` family (`local`, `global`, `method`,
`method_cached`, `method_chain`, `vararg`, `fixedarg`, `general`,
`factory_closure` - the measurement surface for the frame-flattening
and closure-clone rewrites), `fields/polymorphic` / `same_obj_cached` /
`same_obj_write`, `tables/numeric_index` / `mixed`, `alloc/short_tables`,
`iter/ipairs`. Port with the same recipe (kernel + calibrated repeat
loop, ~100ms/call, footer 30) and register both pins (the bench/ port
as `file`, the examples/ original as `hotpath_file`).

Also open: a seconds-scale parse workload (`large_source` at a second,
larger size), and a Rust-driven snapshot bench
(optimizations.md "Snapshot path" - needs harness support, not a .lua
file).

Why deferred: the registry is live with the 16 curated workloads;
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
- Native (cost model version 2): the string, pattern, table and math
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
