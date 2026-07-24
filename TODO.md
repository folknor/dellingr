# TODO.md

Forward-looking feature/refactor ideas. Not optimizations - those live in
[OPTIMIZATIONS.md](OPTIMIZATIONS.md). This is a working backlog; entries
get deleted as they ship or stop being worth tracking.

## Deferred forward-looking ideas

### Prune or index the `%p` identity map

What: `string.format("%p")` assigns deterministic ids from
`State.format_pointer_ids`, a `Vec<(Val, u64)>` probed by linear
scan. Entries are never removed, so every distinct value ever
formatted with `%p` keeps one entry for the State's lifetime (the
Val is deliberately not a GC root, so the object itself still
collects - only the entry lingers), and lookup cost grows linearly
with distinct `%p` usage.

Sketch: sweep the map during `gc_collect`, dropping entries whose
Val no longer resolves in the heap (the slotmap generation makes
dead entries detectable, and a dead Val can never be observed
again, so dropping is safe and deterministic). Independently, the
linear scan could become a deterministic keyed lookup.

Why deferred: cost budgets bound per-callback abuse, `%p` is a
niche conversion, and each entry is ~24 bytes. Only a long-lived
State that formats many distinct objects would notice.

Signal that would promote it: an embedder using `%p` for identity
logging over a long game session, or a profile showing the scan.

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
