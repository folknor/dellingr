# todo.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[optimizations.md](optimizations.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

## Next up

### Run the crmath contract on a second platform (do first)

What: `src/crmath/` (c2932f8) promises bit-identical transcendentals and
`^` on every host, but every run so far was x86-64 with FMA on plantasjen,
built with `target-cpu=native` (set globally in `~/.cargo/config.toml`).
Two code paths have never executed: the run-time-detected no-FMA x86 path
(it is only compile-checked) and aarch64, where `crmath::fma` is `mul_add`
lowering to `fmadd`. wasm has not run either. The soft `soft_fma` itself is
exercised by the forced-FMA tests, but `tests/fixtures/crmath_golden.txt`
only proves cross-platform agreement when it runs somewhere else.

Sketch: run the crmath tests in a baseline `x86-64` build (override the
global `target-cpu=native` for that run), then the golden test on aarch64
(qemu user-mode or real hardware). The ignored release sweeps
(`brokkr test sweep_ --release`, about 7M CORE-MATH worst cases) are the
stronger check where time allows.

Signal: any disagreement is a replay divergence, the exact bug HYG-067
removed.

### Housekeeping left from the 2026-10-22 loops

- libbeef 0.1.1 bugs the crmath test oracle works around, possibly worth
  reporting upstream. Outward-facing; the user deferred it on 2026-10-22.
  If it is picked up: confirm each still reproduces on the latest release,
  draft one issue per bug with a minimal reproduction, and post only after
  the user approves the text. `BigFloat::pi`
  returns 0 and high-precision `atan` returns 0 with it; the power-of-two
  base `pow` shortcut truncates the exponent (`4^0.25` gives 1); `sin` on
  huge arguments panics (its Ziv loop never converges); `to_f64` mishandles
  the bottom of the subnormal range.
- Syntax-error wording still short of reference: at end of input the parser
  always reports `unexpected <eof>`; lexer errors name no `near` token; an
  unclosed bracket on a later line lacks `(to close ... at line N)`.
- `Error::library_function` is set only for `table.sort`; other library
  errors carry their function inside the `LibraryError` variant. Decide
  whether the accessor should cover them or go.
- `docs/compatibility.md` gaps, all matching 5.4 or deliberate but unlisted
  (the "Position arguments" section of the `src/lua_std.rs` module doc has
  the rules): non-integer positions raise where 5.2 truncates; `gsub` with
  negative `n` replaces nothing where 5.2 replaces all; `gmatch`'s `init`
  is 5.4-only; `unpack` and `concat` raise across +/-2^53 where 5.4 returns
  nils. The page's `move` +/-2^53 wording is also inaccurate: a single exact
  key outside the range is accepted.
- The ignored crmath sweeps read CORE-MATH's worst-case files from an
  unpinned local checkout under `research/`; only the c2932f8 commit message
  records the upstream commit (`0ce0cbc`). Pin it somewhere the sweeps or
  `reference/deterministic-math.md` can check.
- The `State::set_user_data` doc example is still marked `ignore`.
- Possible CORE-MATH bug: `is_exact` in pow.c measures the bit length of `m`
  rather than `m^y`. Ported as is; unobservable here (the sweeps pass).

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

Also open: a math workload (no registered workload isolates the cost of
`crmath` transcendentals or `^`; `bench/benchmark.lua` calls `math.sin` in
one section only, so c2932f8's cost is unmeasured), a seconds-scale parse
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
