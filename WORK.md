# WORK.md

Current work item. Optimization loop 3 of the measured sequence - the
headline: per-Bytecode State-level caches.

---

## Target: intern string literals once per (State, Bytecode), share RuntimeCaches across closures

### The two mechanisms, one infrastructure

**(a) Per-call literal interning.** `initialize_frame` (`eval.rs`, the
`#[hotpath::measure]`d function) re-interns EVERY string literal of a chunk
into `state.string_literals` on every call, and truncates them on return -
k intern-pool probes + pushes + GC-threshold checks per call, where k counts
every field name and string constant in the function. `get_string_constant`
then indexes `state.string_literals[frame.string_literal_start + id]`.

**(b) Per-closure RuntimeCaches.** `alloc_lua_fn` (`object.rs`) allocates a
fresh `Arc::new(RuntimeCaches::new(&bytecode))` per closure, so factory
patterns pay cold-IC warmup per produced closure. Pre-split, all closures of
one chunk shared caches.

Both want the same thing: state keyed by Bytecode identity, living on the
State, surviving across calls and closures.

### Measured evidence (2026-07-26, plantasjen)

- `calls/many_literals` (designed worst case: 32 literals, returns from the
  first branch): 42.9x lua5.5, the worst ratio in the suite by an order of
  magnitude. `--hotpath` (uuid 27aeab9d): `initialize_frame` is 86.0% of the
  run; `alloc_string` and `find_by_hash` each show 23,072,764 calls = exactly
  32 x 721,722 calls.
- Every call-heavy workload pays: `initialize_frame` shows 1.0-6.5% of wall
  across arithmetic/gc_churn/benchmark even with few literals.
- `alloc/closure`: `alloc_lua_fn` allocates 128 B x 510K closures (69% of the
  workload's process allocation); the three RuntimeCaches Vecs are part of
  that. `examples/calls/factory_closure.lua` exists to track (b).

### Working design hypothesis (challenge it)

Give `Closure` an `Arc<[Val]>` of its interned literals, resolved at
closure-creation time from a per-State map:

```rust
// on State
bytecode_caches: IndexMap<*const Bytecode, BytecodeCacheEntry>

struct BytecodeCacheEntry {
    bytecode: Arc<Bytecode>,      // pins identity; prevents ABA on the ptr key
    literals: Arc<[Val]>,         // interned once
    caches: Arc<RuntimeCaches>,   // shared by every closure of this Bytecode
}
```

`alloc_lua_fn` looks up by `Arc::as_ptr`; on miss it interns the chunk's
literals once and builds the entry. `Closure` carries `literals` + the shared
`caches`; `Frame` carries the same Arcs. `get_string_constant` becomes
`frame.literals[id]` - no offset plumbing. `initialize_frame` stops touching
`state.string_literals` entirely; the `string_literal_start` machinery and
the per-frame truncation disappear (check every reader:
`vm.rs`/`eval.rs`/`frame.rs`/`eval_store.rs`/`table_ops.rs`
`with_template_keys` reads `string_literals` with a start offset, and
`mark_gc_roots` roots the vec).

Open questions the plan must answer:

1. **Map hygiene / dead Bytecodes.** Entries must not accumulate forever in
   a long-lived State (the product runs per-tick callbacks; scripts can
   `load_string` repeatedly). Sweep during `gc_collect`: drop entries whose
   `Arc<Bytecode>` is uniquely held (`Arc::strong_count == 1` means only the
   cache still references the chunk - no closure, no frame, no nested
   parent). Is strong_count-based sweeping deterministic here?
   (`Bytecode.nested` children are `Arc<Bytecode>` held by parents - think
   through what keeps a nested chunk's count above 1.) Determinism of sweep
   ORDER matters only if dropping affects observable behavior - it should
   not, but argue it.
2. **GC rooting.** The interned literal `Val`s must be roots
   (`mark_gc_roots` is the single source of truth). The map itself is
   IndexMap (deterministic iteration). What roots literals between interning
   and map insertion inside `alloc_lua_fn`? (Interning can trigger GC via
   threshold checks - the current per-call path handles this by pushing into
   `state.string_literals` as it goes.)
3. **Allocation-order determinism.** Interning moves from call-time to
   closure-creation time. Allocation order remains a pure function of
   execution history (fine for replay), but heap/GC pressure timing shifts.
   Confirm nothing observable depends on WHEN literals intern (heap counts
   are host-visible via `heap_size`/`object_count` - the harness KVs will
   shift; that is measurement, not semantics).
4. **The top-level chunk.** `load_string` -> `State::call` executes the main
   chunk - does it go through `alloc_lua_fn` (so the entry exists before
   `_bench` closures are created), or does it need its own path?
5. **Snapshot save/load.** Closures serialize by chunk id + upvalues; on
   load, `alloc_lua_fn` rebuilds - entries repopulate naturally. But saved
   `RuntimeCaches` state? (Check: caches are runtime-only, not serialized -
   confirm.) And does the save walker read `string_literals` or
   `string_literal_start` anywhere?
6. **Shared-cache semantics (b).** ICs become shared across all closures of
   a Bytecode, not just recursive frames of one closure. Cache entries
   validate per-access (table ptr + version, globals_version), so stale
   entries self-heal - argue there is no correctness hole, only different
   warmup dynamics. Note the doc comment on `Closure` already says recursive
   calls share cache writes deliberately.
7. **`with_template_keys`** (`table.rs`) receives `string_literals` +
   `string_literal_start` from the constructor path - it must switch to the
   frame's literal slice. Find every other `string_literal_start` consumer.
8. **Cost.** No cost accounting changes anywhere: interning was never
   charged, and `cost_used` must stay byte-identical (regression-test it).

### Constraints (inline; sessions read nothing else)

- Read and write code only; no cargo/brokkr/test/bench commands - the
  orchestrator validates between steps.
- Determinism is a product requirement: no HashMap/HashSet (IndexMap or
  BTreeMap only), no unseeded RNG; identical source + identical call
  sequence must produce identical behavior and identical `cost_used`.
- Clippy denies warnings; `unwrap_used` denied outside `#[cfg(test)]`
  (`expect` with an invariant message allowed); `Result::ok()` banned;
  `dbg_macro`/`todo!` denied.
- GC discipline: root objects with `GcHeap::mark` / the `Markable` plumbing,
  never by pushing the worklist directly; `mark_gc_roots` stays the single
  source of truth; a `debug_assert` in `drain_mark_worklist` enforces
  colouring.
- The stack cap is a real invariant; any new push path must be checked or
  preflighted; a rejected operation must leave the stack, string pool and
  registry unchanged.
- Do not add `#[hotpath::measure]` to any function whose frame stays live
  across the recursive dispatch (`eval_closure` and its callers). Keep the
  existing annotations (`initialize_frame`, `alloc_lua_fn`,
  `get_string_constant`) so before/after distributions compare.
- Snapshot format must not change unless truly unavoidable - if it must,
  stop and say so instead of implementing it.
- `MAX_CALL_DEPTH` / stack-bloat: `Frame` size changes affect the recursion
  depth headroom - the `call_depth_exceeded_error` test recursing to 1000
  must still pass (Frame gains up to two Arcs and loses a usize; net small).

### Reading list

`src/vm/eval.rs` (`initialize_frame`, `eval_closure_frame`, literal
truncation sites), `src/vm/frame.rs` (Frame fields, `get_string_constant`
callers), `src/vm/eval_store.rs` (`get_string_constant`, template path),
`src/vm/object.rs` (`alloc_lua_fn`, `Closure`, GC mark), `src/vm.rs`
(`mark_gc_roots`, `string_literals` field, State construction),
`src/vm/table_ops.rs` + `src/vm/table.rs` (`with_template_keys`),
`src/vm/save_state.rs` (closure encode/decode, quiescence validation -
`validate_quiescent` checks `string_literals`!), `src/compiler.rs`
(`Bytecode` fields, `nested`), `src/lib.rs` (public API touchpoints).

### Deliverable

An implementation plan answering every numbered question, with file-by-file
shape, the sweep design, the rooting story, a test list (including: exact
cost regression; factory closures sharing warm ICs observably via bench not
semantics; snapshot round-trip; a load_string-in-a-loop map-hygiene test),
and bench predictions: `many_literals` should collapse (86% of its run is
the target), every call-heavy workload should move a little,
`factory_closure` tracks (b), `alloc/closure` must improve or hold (three
fewer Vec allocs per closure), nothing may regress, `cost_used` identical
everywhere.

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

The working hypothesis above is superseded on three points, each verified:
(1) `Arc::strong_count` liveness is UNSOUND - `Program` is public API and a
host-retained or multi-State-loaded Program keeps a dead entry alive
forever; sweep from actual State reachability instead. (2) A raw
`*const Bytecode` map key would break the compile-time `State: Send`
witness in `lib.rs` - key by pointer-derived `usize`, with the entry's own
`Arc<Bytecode>` pinning the address against ABA. (3) Bundle literals and
caches behind ONE Arc that REPLACES the existing `Closure.caches` Arc, so
per-call refcount traffic does not grow and `Frame` shrinks (it also loses
`string_literal_start`), protecting the 1000-deep recursion headroom.

### Core types

```rust
struct BytecodeRuntime {
    literals: Box<[Val]>,     // interned once per (State, Bytecode)
    caches: RuntimeCaches,    // shared by every closure of this Bytecode
}
struct BytecodeCacheEntry {
    bytecode: Arc<Bytecode>,  // pins the usize identity
    runtime: Arc<BytecodeRuntime>,
}
// State
bytecode_caches: IndexMap<usize, BytecodeCacheEntry>
```

`Closure.caches: Arc<RuntimeCaches>` becomes `Closure.runtime:
Arc<BytecodeRuntime>`; same for `Frame`. Literal access is
`frame.runtime.literals[id]` (a small `Frame::literal(id)` accessor).
`instr_get_global` keeps reading raw Bytecode bytes (it needs UTF-8 names,
not interned `Val`s).

### Map population (the only place literals intern)

In `push_closure` (and a snapshot-load variant, below), before
`alloc_lua_fn`: look up the entry by identity; on miss:

1. Prevalidate every literal's size BEFORE touching the heap (parsed
   Programs already pass this in the parser; the raw-Bytecode test path
   does not - a late failure must not leave a partially changed string
   pool).
2. Intern literals one at a time, appending each `Val` to
   `transient_roots.values` immediately (watermark pattern) so a
   GC triggered mid-population cannot sweep the earlier ones.
3. Build the `Box<[Val]>` + `RuntimeCaches`, insert the entry, truncate
   the transient-root watermark.

`alloc_lua_fn` takes the already-resolved `Arc<BytecodeRuntime>` instead of
constructing caches. A pending-identity guard (a small State-side list,
included in quiescence validation) covers the window between entry
insertion and the closure object existing, so the sweep cannot remove a
just-created entry.

### Sweep (State-local, in `gc_collect`)

After marking and draining the object worklist, before colours reset:
collect the Bytecode identities of every reachable Lua closure, plus every
`call_stack` entry's bytecode (defensive - covers the `eval_chunk` test
path), plus the pending list; traverse each live Bytecode's `nested` tree
and retain descendants too (a live factory keeps its child entry warm even
when no produced child closure currently exists). Membership via
`BTreeSet<usize>` (deterministic, no banned collections; pointer values
are process-local but used only for membership). Remove non-members in
IndexMap insertion order. Drop order is Lua-invisible (no destructor
semantics). Literals of a removed entry were already marked this cycle, so
they survive one extra collection and the NEXT sweep frees the strings -
bounded lag, accepted, in exchange for keeping `mark_gc_roots` the single
root authority.

### Rooting

`mark_gc_roots` marks every entry's `runtime.literals`. Additionally,
marking a reachable Lua closure marks `closure.runtime.literals`
transitively (object.rs), so closure GC-correctness does not depend on the
map invariant. IC pointers inside `RuntimeCaches` stay non-roots exactly as
today (non-owning; validation guards dereferences).

### Removal list (every `string_literal_start` consumer)

- `State.string_literals` field, its rooting line, and all three
  truncation/error paths + `initialize_frame`'s intern loop in `eval.rs` -
  `initialize_frame` becomes a pure move of Arcs/upvalues/varargs.
- `Frame::string_literal_start` + constructor arg + frame unit tests.
- `get_string_constant` (`eval_store.rs:530`) -> direct literal index.
- Template path: `eval_store.rs:90`, `new_table_with_template`
  (`table_ops.rs`), `GcHeap::alloc_table_with_template` (`object.rs`),
  `Table::with_template_keys` (`table.rs`) - pass a literal slice, no
  offset.
- Snapshot: drop `string_literals.is_empty()` from `validate_quiescent`,
  add the pending list to it; remove the obsolete
  `state.string_literals.clear()` (`save_state.rs:1184`); closure-shell
  loading resolves the shared runtime through a verified NO-GC population
  helper (the two-pass loader deliberately allocates shells without
  collection) and passes it to `alloc_lua_fn`. Binary format unchanged.
- `src/compiler.rs`: update the RuntimeCaches ownership doc comment
  (per-(State, Bytecode), still State-specific, never serialized).

### Semantics notes (documented, deliberate)

- Interning moves to load/closure-creation time: `string_count`,
  `heap_size`, `gc_should_run`, `gc_threshold` are host-observable and will
  read differently (earlier interning, possibly earlier auto-GC). No
  opcode, value, callback, or cost behavior changes. Harness KVs
  redistribute (parse/setup up a little, calls down).
- Shared ICs across closures of one Bytecode: correct because every cache
  family validates per access (globals_version / generational identity +
  table version / metatable + method-table version). Interleaved closures
  specialized to different receivers may thrash a monomorphic slot -
  measured, not assumed: the verdict includes an adversarial
  factory-polymorphic cell.

### Tests (all new or updated)

- Unit: two factory-produced closures share one runtime Arc; warming one
  populates the shared slot.
- Semantic: alternating factory closures over different tables, mutations,
  global swaps, explicit GC - results identical to reference expectations.
- Map hygiene: `load_string` loop with unique chunks, auto-GC disabled,
  explicit collect - map returns to baseline WHILE an external retained
  `Program` (and a second State loaded from it) still hold Bytecode Arcs;
  a second collect reclaims the one-cycle-lagged strings.
- Nested-parent retention: live factory retains child entry; dropping the
  factory then collecting removes both.
- Update `string_literals_are_unrooted_after_frame_exits`
  (`tests/gc_upvalues.rs:67`) to the new lifetime (cache sweep + one-cycle
  lag); keep the active-frame forced-GC test intact.
- Stack-cap: a rejected `load_string` leaves stack, object/string counts,
  cache map and registry unchanged.
- Exact-cost regression: hand-counted 12-cost source (two `{x=..}`
  templates + two calls each doing one field write and one two-field
  constructor), asserted cold AND warm, so neither population nor cache
  sharing ever charges.
- Snapshot: round-trip with multiple same-Bytecode closures and binary
  (non-UTF-8) literals; restored closures share one rebuilt runtime.
  Existing golden-bytes and wide-literal/template tests unchanged.
- `call_depth_exceeded_error` still passes (Frame got smaller).
- `State: Send` compile-time witness still compiles.

### Bench acceptance (orchestrator)

- `many_literals`: Amdahl ceiling ~7x on the 86% share - expect a large
  collapse, not reference parity.
- Call-heavy workloads: small wins; `factory_closure` (examples-only,
  via hotbench) improves; `alloc/closure` improves or holds.
- Adversarial factory-polymorphic cell watches shared-slot thrash.
- Full verdict suite: ANY aggregate regression is a design failure to
  investigate, not a tradeoff to accept. `cost_used` identical everywhere.
