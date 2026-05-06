# TODO.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[OPTIMIZATIONS.md](OPTIMIZATIONS.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

---

### Configurable per-category cost weights

What: let library consumers set their own cost per opcode category
(arithmetic, table_writes, function_calls, ...) rather than the
hardcoded `cost = 1` per costed op. Some embedders might want
arithmetic to cost less than allocations, or vice versa.

Sketch: `State::set_cost_weights(weights: CostWeights)` where
`CostWeights` is a struct with one `u32` field per category that the
existing `analyze_cost` already enumerates (arithmetic, negation,
table_creation, table_writes, array_elements, ...). The eval loop
multiplies by the configured weight when charging cost. Default
weights all = 1, preserving current behavior.

Why deferred: not user-requested by the current consumer; adds a
multiply per costed op (hit on the eval-loop hot path); complicates
`cost_used` interpretation across configurations. Worth doing once
there's a concrete second consumer with different cost-budget needs.

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

### Pin StringPool's hasher

What: `StringPool::hash_string` (`src/vm/object.rs:474-479`) uses
`std::collections::hash_map::DefaultHasher`, whose internal algorithm
is documented as "not specified, subject to change." It's currently
SipHash with a fixed key, so deterministic across hosts today, but a
future stdlib release could silently change bucket order.

Sketch: replace with an explicit hasher under our control -
`siphasher` with a fixed key, `ahash` with a fixed seed, or `fnv`.
The hasher is internal to string interning; bucket order isn't
program-visible (interning compares by content, `src/vm/object.rs:494-503`),
so the determinism contract holds today by accident, not by design.

Why deferred: not load-bearing yet. The contract is preserved
without us doing anything.

Signal that would promote it: a stdlib release changing
`DefaultHasher`'s algorithm, OR a future change that makes
`StringPool` bucket iteration program-visible (e.g. a debug API).

### Stable `RustFunc` identity for serialization

What: `RustFunc` `Val`s render and hash by function-pointer address
(`src/vm/lua_val.rs:105, 169`). Function-pointer addresses aren't
stable across builds or across hosts, so any feature that
serializes a `Val` containing a `RustFn` (replay, snapshot,
cross-process IPC) will fail byte-for-byte determinism even if
nothing else has changed.

Sketch: assign each registered RustFunc a stable ID at registration
time (e.g. via a registry on `Engine`), and render / hash by ID.
Embedders register their host functions through an `Engine` API
that returns a `RustFunc` handle carrying both the fn pointer and
the ID.

Why deferred: dellingr doesn't ship a serialization story today.
The pointer-address rendering is fine for in-process use; nothing
observable depends on its stability.

Signal that would promote it: someone wanting deterministic replay
across hosts, or wanting to snapshot/restore VM state across
process restarts.
