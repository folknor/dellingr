# OPTIMIZATIONS.md

Forward-looking ideas: optimizations considered but not yet implemented, plus
notes on places where current code is deliberately conservative. This is a
working backlog, not a record of decisions. Delete entries as they ship, get
contradicted by new evidence, or stop being worth tracking.

Each entry: what, sketch, why-not-yet, signal that would change the calculus.

Entries tagged like (A-O1) came in through the 2026-07-24 corner audits
(A: front end, B: execution core, C: data plane, D: state/persistence/host,
E: stdlib/patterns), consolidated here after triage against the code on
2026-07-26; the audit's separate candidate file is gone. Bench ratios cited
below are from the 2026-07-27 README table refresh (commit abf7c7b,
plantasjen) unless noted.

---

## Investigate

### Residual ~2x in `strings/patterns` - decided not to chase (2026-07-25)

What: the 0.2.0-era README recorded `examples/strings/patterns.lua` at 30ms; it
measured 160ms on the same host before the gmatch memoization work and 65ms
after, so roughly 2.4x was recovered and roughly 2x remains unaccounted for.
The bench re-measured at 60ms on 2026-07-27, so the residual is stable, not
drifting.

Decision: not chasing it. The portion with an identified mechanism (per-iteration
subject recopy and pattern recompilation in `gmatch`) is already fixed. What is
left has no suspect attached, and bisecting it is open-ended work against a
bench whose absolute number is now 60ms. Relative deltas remain valid against an
inflated baseline, so this does not block measuring pattern-matcher
optimizations - it would only matter if the residual were a live cost that is
cheap to recover, which nothing currently suggests.

Signal that would reopen it: `strings/patterns` moving again without a
corresponding change to the pattern code, or a pattern-matcher optimization
landing with a much smaller win than its mechanism predicts (which would hint
that something else dominates the bench).

---

## Full coherent rewrites

Large enough to reshape whole subsystems. Each subsumes several smaller
entries below; check the cross-references before starting a subsumed item.

### Register-based codegen (A-O1; biggest throughput lever)

What: the parser emits a pure stack machine: `x = a + b` is GET_LOCAL,
GET_LOCAL, ADD, SET_LOCAL - four dispatches plus stack traffic; a register VM
does it in one ADD(dst, a, b). Reference Lua's 5.0 move from stack to register
VM is the single largest reason for the persistent gap on arithmetic/field
benches (`numerics/arithmetic` 2.6x, `fields/same_obj_read` 3.8x vs lua5.2).

Sketch: the 32-bit ABC encoding in `instr.rs` already has the operand room;
locals already live in fixed frame slots, which are registers in all but name.
The expression parser would grow a lua-style expdesc with delayed emission
(the existing `ExpDesc`/`PlaceExp` split is halfway there), and
`mark_call_base` / `dup` / `swap` gymnastics disappear.

Hard-constraint interactions: determinism unaffected; per-opcode cost
accounting survives but cost_used values re-base (fewer, fatter ops) - a
product decision to version, same as any replay-affecting change. Subsumes
the field-update fusion and GET/SET cache-slot-sharing entries below, which
become natural in a register IR.

### Flatten the interpreter into an explicit State-owned frame stack (B-O1)

What: today every Lua-to-Lua call recurses through Rust (`State::call` ->
`eval_closure` -> `eval_closure_inner` -> `Frame::eval`), with per-call costs
that are all consequences of the frame being a Rust stack local:

- `Closure` clone per call (`GcHeap::as_lua_function`, `object.rs`): three
  Arc refcount bumps since 2026-07-26 (upvalues became `Arc<[UpvalueRef]>`,
  commit eeb406a, killing the per-call heap allocation) - still a copy that
  a flattened loop would not make at all.
- Duplicate frame bookkeeping: `CallInfo` push (another `Arc<Bytecode>`
  clone) parallel to the `Frame` itself.
- `stack.remove(idx)` to extract the callee (O(args) memmove, `eval.rs:68`).
- varargs `drain(..).collect()` Vec per vararg call (`eval.rs:381`).
- (per-call string-literal interning was on this list until the per-Bytecode
  runtime cache shipped on 2026-07-26; the frame handoff is now pure Arc
  moves. The return-value drain-to-Vec left on 2026-07-26 too, commit
  a96cff4 - `eval.rs:508` now slides the results down in place.)

Sketch: a single dispatch loop over a State-owned `Vec<FrameState>` (bytecode
Arc, ip, base, vararg span, cache ptr) eliminates every item above, merges
`Frame` with `CallInfo` (stack traces read the real frames), and makes frames
visible to `mark_gc_roots` directly - the vararg/temporary rooting that today
rides on `transient_roots` watermarks becomes structural. Calls become "push
frame record, continue loop"; returns become "memmove rets down, pop frame
record". MAX_CALL_DEPTH becomes a plain length check, and the AGENTS.md
concern about Rust-stack bloat per recursion level (the hotpath-annotation
caveat) disappears.

Why deferred: highest-leverage rewrite in the execution core, and priced like
it. Call-heavy benches (`calls/*`, `benchmark`) are the ones sitting at
3.4-4.4x lua5.5. Pre-1.0, internal-only: `State::call`'s public signature can stay.
Subsumes the Closure-clone entry and the call-path micro cleanups below.

### 8-byte NaN-boxed `Val` (C-O7)

What: `Val` (`src/vm/lua_val.rs`) is 16 bytes (tag + payload). NaN-boxing
(payloads in the NaN space of an f64: slotmap keys are 64-bit but their useful
entropy fits 48-51 bits with an index/generation split; RustFn would need a
registry index rather than a raw pointer) halves stack/table/upvalue memory
traffic and makes `stack: Vec<Val>` copies twice as dense.

Why deferred: full-rewrite class - touches every `match` on `Val` - but it is
mechanical, determinism-neutral, and is the standard reason reference VMs beat
tagged-enum interpreters on memory-bound workloads (`tables/fill` and
`fields/same_obj_read` are the 2.6-3.8x-behind-lua5.2 benches). If judged too invasive, a cheaper
intermediate is boxing only `Table` storage entries more densely; but the full
version is where the payoff is.

Note the RustFn registry index is a cost of this item, not a shared
prerequisite: `Val::RustFn` compares and hashes by function address and
renders as a constant `<function>`, so nothing outside NaN-boxing needs an id.
Squeezing a raw `fn` pointer into the NaN payload is the part that would force
one.

---

## Architectural

### Shape-based polymorphic field IC

What: a per-callsite cache for `OP_GET_FIELD` keyed on (table_shape_id,
field_name) -> field_index. Catches the iteration pattern where many
distinct tables with the same logical structure are accessed at one site
(`for _, e in pairs(items) do sum = sum + e.id end`).

Sketch: each Table gets a `shape_id: u64` that updates when keys are
inserted or removed in non-shape-preserving ways. Shape IDs are interned -
two tables built by inserting the same keys in the same order share an ID.
The IC validates by shape_id equality instead of pointer equality.

Why deferred: shape interning is non-trivial. The dominant cost is on
table creation (computing the shape after each insert/remove), and
introducing it everywhere risks regressing table-fill paths for marginal
wins on iteration paths. The current monomorphic IC already catches
same-object access well.

Signal that would promote it: a real workload where polymorphic field
access dominates and `field_hits`-style benches show 3-5x slower than
`same_object_fields`. Currently they're within 3x and the bottleneck on
field_hits is the outer `items[i]` array access, not the field reads.

Lighter-weight variant worth weighing first - more honestly described as
a *key-position IC* than a shape IC, since it caches "this key is at
this ordinal IndexMap index" rather than recognizing table shapes.
Validation is a three-state ladder, not a single OR:

1. same table ptr + same `Table::version` - trust the cached index
   (this is what `get_cached_field` already does at frame.rs:785).
2. same ptr + bumped version, OR different ptr - re-read
   `tbl.get_index(idx)` and accept if the key at that index matches
   the cached key. On accept, refresh the cache.
3. otherwise - slow path.

Note that `Table::version` is bumped specifically when existing
key-to-index bindings may shift (`remove`, `array_insert`,
`array_remove`), not on tail-appending inserts or value-only updates,
since those leave existing indices stable. Cross-receiver validation
therefore *cannot* lean on version equality (different tables can both
have version 0 with completely different layouts); the cross-receiver
correctness guarantee comes from the key-at-index check alone.

What this catches: different tables that happen to lay out the same key
at the same ordinal (e.g. all `{id=…, value=…}` records constructed in
the same key order). What it doesn't catch: two valid layouts with the
same logical fields in different orders (`{id=…, value=…}` vs
`{value=…, id=…}` will thrash). If real workloads turn out polymorphic
across multiple layouts, a small N-way PIC is the next step rather than
a single-slot key-position cache.

How this might compare to the heavier shape-IC design depends heavily
on workload mix and isn't yet measured. One hypothesis worth testing:
in production scripts the bigger win comes from compile-time
record-shape prediction (see "Compile-time table-shape prediction"
below), which would make the runtime IC variant less load-bearing -
but this is unsupported by dellingr's own benches today.

Tried in 2026-05-06, reverted: dropping the early-exit on ptr mismatch
in `get_cached_field` and falling through to key-at-index validation
regressed both `same_obj_read` (+11% wall time) and `polymorphic` (+9%)
even with the cross-receiver branch extracted to a `#[cold]`
non-inlined fn. Two reasons it didn't pan out as written:

1. Polymorphic record-shaped tables (`{id=…, value=…}`, ≤4 fields)
   stay in `TableStorage::Inline`, whose slow path is already a
   1-2-entry pointer-compare scan. Cross-receiver validation
   (heap-deref + `get_index` + key cmp + `cache.set_field` write)
   is more work than that, so it loses on Inline.
2. Even with `#[cold]` extraction of the cross-receiver branch, the
   extra control-flow edge in `get_cached_field` shifts LLVM's
   inlining/register-allocation choices in the bytecode dispatch loop
   enough to slow the same-ptr fast path. Couldn't get the hot path
   to compile bit-exact-equal to the original.

Promotion would need either (a) a real workload where polymorphic
record-shape access is on Map-storage tables (>4 fields), where the
slow path's hash lookup is expensive enough to amortize the extra
work, or (b) a way to add the cross-receiver path without
perturbing the same-ptr branch's codegen.

### Array part for dense integer keys

What: a third `TableStorage` variant (`Array { values: Vec<Val> }`) for
tables with consecutive integer keys 1..N. Skips hashing entirely for
`arr[i]` access.

Sketch: when a fresh table receives integer keys 1, 2, 3, ... in order,
keep them in a Vec instead of an IndexMap. `get(Val::Num(n))` becomes a
range check + Vec index. Promote to `Map` on hash-key insert or sparse
integer insert (`arr[100] = x` when len < 100).

Why deferred: invasive. Touches every Table operation, including `next()`,
`pairs()` iteration, the metatable interactions, and the `set_at_index`
hot path. Real Lua VMs do this, but our `IndexMap` baseline is competitive
enough that the engineering cost has not earned itself.

First, a clarification the original sketch glossed over: there are
really two designs hiding under "array part," and most hidden costs
attach only to the heavier one.

- **Dense-only with one-way promotion** (the sketch above): an
  `Array(Vec<Val>)` storage variant for tables built from sequential
  integer keys, which promotes irreversibly to `Map` on the first
  sparse or non-integer write. After promotion, behavior is exactly
  today's. Iteration order can be preserved by materializing the map
  in insertion order during promotion. No permanent array/hash
  boundary, no `next(k)` boundary handoff.
- **Dual array+hash storage** (real Lua-style): both parts coexist
  for the lifetime of the table. `pairs()` iterates array-first, then
  hash; `next(k)` crosses the boundary. Rehash heuristics decide when
  to migrate keys between parts.

Several of the hidden costs below apply only to the dual-storage
variant; the dense-only one is genuinely cheaper.

Hidden costs that aren't obvious from the outside:

- Rehash / O(N) work and cost-accounting (both variants, mostly
  worsens an existing condition). A SET that triggers an array-hash
  migration does O(N) work invisible to `analyze_cost`. The
  blind-spot category isn't actually new: `Table::array_insert` /
  `array_remove` already do O(N) shifts charged as 1, `IndexMap`
  rehashes on capacity growth, and `lua_std::table.insert` /
  `table.remove` charge O(1) for O(N) work today. The array-part
  proposal extends that gap to plain `t[k] = v`, but doesn't create
  a new class of problem. The fix that doesn't require punching a
  hole through the table layer: have `Table::insert` return the
  work-done, and let the OP_SET caller charge the dynamic count -
  which is how `OP_SET_LIST` already handles its variable cost.
  `Table` doesn't need access to `State`.
- Iteration-order shift breaks replay compat (dual storage only).
  Switching from IndexMap insertion order to "array first, then hash"
  is observable via `pairs()`. This isn't a determinism break in the
  reference-Lua-spec sense (the spec leaves `pairs()` order
  unspecified); it's a *replay-compat* break against dellingr's
  self-imposed insertion-order contract. The right unblock is
  replay-versioning, which several other deferrable optimizations
  also want; framing this as an array-part-specific cost is
  misleading.
- `next(k)` continuation across the boundary (dual storage only).
  Distinct index ranges for array vs. hash and careful handoff at
  the boundary; reference VMs have shipped fixes here repeatedly.
- `#t` cost (mostly already paid). `compute_array_len` already does
  the doubling-then-bisect today; an array part *simplifies* `#t`
  for the array-only case (`return values.len()`) and only
  complicates the mixed case. Net code-size could go either way.
- Inline storage interaction (narrower than first written).
  `TableStorage::Inline` only loses to a dense `Array` variant for
  *small dense-integer-only* tables - record-shaped tables
  (`{x=…, y=…, z=…}`) stay in `Inline` regardless. And in the
  dense-int small case, `Inline` (no alloc, ≤4 compares) is
  plausibly already faster than `Vec<Val>` (heap alloc + bounds
  check), so the affected case may be the case where Inline wins
  on cycles anyway. A smallvec-style Array-with-inline-buffer
  would erase this concern entirely - whether that's worth the
  engineering is open.
- Dispatch-site / traversal-site multiplication (corrected from an
  earlier "GC barrier" framing). dellingr is stop-the-world
  mark-sweep with no write barriers, so the correct cost is
  match-arm proliferation across `TableStorage` dispatch sites
  (~8-10 functions: `mark_values`, `get`, `get_with_index`,
  `insert`, `remove`, `next`, `promote_to_map`, `ensure_map`, …),
  each gaining one arm per new variant. This is real engineering
  cost but has no GC-safety implication.

Signal that would promote it: `numeric_index` and `table_fill` becoming
the dominant slowness in a real game-script workload. The dynamic
numeric fast path in `Table::get` already covers loop-variable
integer indexing on Map storage; the full storage split is only
justified if that proves insufficient and the loss is specifically
on the per-allocation path rather than per-access. Note that integer
keys today are `Val::Num` (a stack-resident enum variant), not
separately allocated - so "boxing" isn't the cost mechanism. The
actual per-allocation cost in dellingr is the IndexMap's
bucket-and-entry storage growing at promotion-from-Inline; whether a
dense `Vec<Val>` beats that for typical workloads is unmeasured.

### Compiler-recognized field-update fusion

What: parse `t.x = t.x + n` into a single `OP_FIELD_ADD` that does
GET + ADD + SET in one instruction, eliminating dispatch overhead and
ideally sharing one cache slot across the read and write of the same
(receiver, key) pair.

Why deferred: requires parser-side pattern recognition, a new opcode, and
unifying cache slot management. The win compounds (one dispatch + one
cache lookup instead of three), but the complexity is parser-level, not
VM-level, which is where we've been working. Subsumed by register-based
codegen if that lands - fusion is a natural register-IR peephole.

Signal that would promote it: a workload where method bodies of the form
`self.field = self.field + ...` dominate, and the per-instruction
dispatch cost shows up in profiling. method_dispatch is now ~200us; if
we hit a floor below that, this is the natural next step.

### Compile-time table-shape prediction

This is two distinct work items that share a name; they have very
different ROIs and should be considered separately.

**(a) Pinned-shape constructor template / direct record build.**

Shipped first slice on 2026-05-11: table constructors with more than
four statically visible fields now emit `NewTablePresized(n)`, which
allocates map-backed storage at the final constructor size instead of
starting inline and promoting after the fourth insert. This is not the
full record-build design below, but it validates that constructor
allocation growth is worth caring about:
`examples/alloc/record_tables.lua` moved from 345.6ms to 277.9ms
against the pre-change binary on this host (~1.24x).

Shipped second slice on 2026-05-11: pure named-field constructors with
more than four unique keys now store a per-Chunk template of
interned-string ids, allocate map storage with those keys pre-installed,
and initialize values by pinned entry index. This avoids the repeated
key lookup on each value write, while falling back to `NewTablePresized`
for computed keys, array entries, vararg/spread entries, and duplicate
named keys. The measured win over capacity-only was small and noisy on
this host: `examples/alloc/record_tables.lua` moved from 264.4ms to
258.4ms in the cleaner run (~1.02x), with an earlier run at ~1.05x.
`examples/alloc/short_tables.lua` was effectively flat.

Remaining idea: a stronger `DUPTABLE`-style opcode would batch-fill a
pure named-field constructor at the end instead of preinstalling nil
placeholders and then overwriting by index. That would remove the
per-field bytecode dispatch and avoid the placeholder writes, while a
single table-build helper could replay normal `insert` semantics in
source order for nil values and duplicate keys.

Why deferred: the current parser emits each field write immediately.
Batch-filling means either delaying named-field writes while preserving
fallback behavior for mixed constructors, or adding a more general
builder instruction for values accumulated above the table on the VM
stack. That is a larger bytecode/stack-shape change than the pinned
write path, and the pinned path only proved a small remaining win after
capacity pre-sizing.

Caveat: if the template were to hold `Val::Str` or table objects
directly, closure marking / chunk rooting would need extension to keep
those reachable. Storing only literal-id ints in the template avoids
this entirely - the strings get interned at template-instantiation
time using the existing literal-string path.

**(b) Local-tracked shape inheritance (rewrite-the-analysis).**

What: callsites that read fields on a local known (statically) to hold
a pinned-shape table can skip the field IC's warmup and emit
`OP_GET_FIELD_AT(reg, key, expected_idx)` that goes directly to
`get_index(idx)` with a key-validate. Same machinery the integer-const
opcode would need for its fast path - they should share a sketch.

Why deferred (separately from a): dellingr's parser has no SSA, no
data-flow scaffolding, and no local-aliasing analysis. Tracking shape
inheritance "conservatively across reassignment, control flow, and
function boundaries" is build-the-analysis-from-scratch, not
add-a-pass. Reassignment, branch joins, and any escape into a
RustFunc/Lua-call/closure-capture/table-field-store would each give
up. An analysis that gives up frequently leaves most of the win on
the table.

Signal that would promote either: profiling showing first-iteration
miss cost dominates after the runtime field IC is sufficient for
steady state - short-loop workloads where the IC never warms up. (a)
is also worthwhile independently if benchmarks show construction cost
on record literals dominating their first-use cost - measure before
committing.

Whether the headline runtime-IC wins production game scripts more or
less than constructor pre-sizing is unmeasured for dellingr; the two
are complementary (cold/first-touch vs. warm/steady-state), not
substitutes.

### Move stdlib install from per-State to per-Engine

What: today `State::with_callbacks` -> `open_libs` re-runs the
standard library install on every new `State`, allocating each
RustFn entry into the `globals: IndexMap<String, Val>`. With the
`Engine` layer in place, the stdlib install can happen once on
`Engine::new` and `Engine::new_state` can clone the prepared
globals into the new State.

Sketch: `Engine` carries a frozen `stdlib_globals: IndexMap<String,
Val>` and a frozen `stdlib_builtins: [Val; Builtin::COUNT]` populated
once at construction. `new_state` clones both into the State.
Watch out: stdlib `Val`s currently include `Val::Str(StringPtr)` for
some entries, which are tied to one specific `StringPool`. Either
the stdlib stops baking string pointers (use `&'static [u8]` and
intern lazily on first use), or the Engine carries a shared
string pool that new States inherit.

Shipped smaller step on 2026-05-11: stdlib globals and module tables now
install Rust functions through direct `Val`/string-key insertion helpers
instead of pushing key/function pairs through the Lua stack and calling
the public table setters. This reduces the per-State setup work without
changing the ownership model. The process-level `examples/comments.lua`
signal is tiny and noisy on this host (roughly 495us to 487us in a
200-run hyperfine sample), so this does not remove the per-Engine idea.

Why deferred: not on the hot path. State construction is amortized
across however long the State lives; even at "one State per request
worker", the install cost is microseconds per request. The bigger
win would be at "one State per request" granularity, which isn't
the recommended embedding model.

Signal that would promote it: a profile showing State construction
dominating request-path latency, or a real "one State per request"
embedder asking.

---

## Front end / parse time

Measured context: `parse/large_source` (5000 generated lines) was 8.0x lua5.5
/ 7.3x lua5.2 before the finalize strip pass (commit bc99ae4) removed the
quadratic call-mark emission; it now measures 2.3x lua5.5 / 2.0x lua5.2, and
the section's remaining entries are no longer backed by an outlier ratio.
The suspected-quadratic parse candidates were never confirmed on a curve -
that still needs a second file size.

### Compare-and-branch fusion (A-O7)

What: `while i < n do` emits LESS (push bool) + BRANCH_FALSE (pop bool) -
two dispatches plus stack traffic per iteration of every hot loop condition.

Sketch: a peephole in the parser (or in `finalize`) fusing comparison +
branch into one opcode halves the dispatch on loop headers. Needs new
opcodes on the execution side. Subsumed by register-based codegen if that
lands.

---

## Stdlib / patterns

### gmatch: drop the Lua-side wrapper and table-backed iterator state (E-O2)

Partially shipped as of 2026-07-25 - the per-iteration subject/pattern
copies and pattern re-validation are gone (`memoize_gmatch_pattern`,
`gmatch_subject_matcher_and_cost_meter`); that was the bulk of the win
(`strings/patterns` 160ms -> 60ms).

What remains (`string.rs`): one Lua call into the compiled wrapper chunk
(`gmatch_wrapper()`, still a compiled Bytecode) + one RustFn call, plus
`get_table` lookups ("s", "pos") and one `set_table_raw` against the
table-backed iterator state, every iteration.

Explored 2026-09-03, and the original sketch understates the cost. The
generic-for protocol does pass `(state, control)` to the iterator, so a bare
RustFn works *inside a for loop* - but reference Lua also supports capturing
the iterator and calling it standalone (`local it = s:gmatch(p); it()`),
where it receives no arguments at all. The state therefore has to live
inside the callable, and dellingr's `RustFunc` is a plain fn pointer by
design (determinism, snapshot registry keyed by address). Dropping the
wrapper needs a new captured-state Rust callable - a `Val` variant or heap
object holding (fn, state) - which touches GC marking, Val size (and the
NaN-boxing plan), and snapshot encoding. That is rewrite-class, not a
stdlib cleanup; only worth doing as part of a broader Val/callable rework,
or if `strings/patterns` profiling shows the wrapper call dominating.

---

## IC extensions (same shape as existing ICs)

### Table-library fallback IC for the hit path (merged with B-O4/C-O5)

The miss-path half of this entry shipped 2026-07-26 (commit 11854a8): a
pristine-shape gate on `push_table_library_field` returns nil for
non-member keys without probing the library, and `bench/miss` moved 5.2s
to ~3.0s (worktree-matched interleaved pairs, plantasjen) with
`same_obj_read` and `cost_used` unmoved. What remains is the hit side:

What: when `tbl:insert(...)` etc. do fall through to the `table` library,
cache the resolved value at the call site - same shape as the shipped
string-method IC. The shipped gate's cached member-name pointers and
shape guard are reusable ingredients.

Why deferred: rarer pattern than string methods, and the fallback hit
path isn't a hot bench; the miss gate already removed the common cost
without any per-site cache.

Signal: a workload where `t:insert(...)`-style sugar fires in a tight
loop.

### Method IC: refresh "no method" entries on mutation

What: the method IC's "method_index = None" cache entry is currently
sticky - once a callsite is flagged as "no directly-cacheable method",
subsequent calls take the slow path forever, even if the receiver's
metatable or its `__index` table later gains the method.

Why deferred: the slow path returns the right answer, so this is a
performance pessimization, not a correctness issue. Refreshing requires
running the full validation chain (metatable identity check, __index
key/handler check, method-table version compare) on every call to a
non-cached site, which costs more than the slow path itself for cases
that genuinely have no resolvable method.

Signal: a workload that mutates index tables to add methods after first
access, where this miss path becomes hot. Vanishingly rare in practice.

### GET_FIELD slow-path cache repopulation after `__index` resolution

What: symmetrically to the SET case above - after `get_table_with_key`
resolves through `__index`, the resolved value lives somewhere
identifiable (the index table for the table-handler case). The method IC
already caches this. The direct-field cache could also populate when the
resolution happened to bottom out in a raw entry on a relevant table,
even though that entry wasn't on the receiver itself.

Why deferred: substantial overlap with the method IC, which already
handles the OOP case. The remaining cases (multi-level `__index` chains
without metatables in the middle) are uncommon.

---

## Compiler-side ideas

### GET/SET cache slot sharing

What: when the compiler can prove a `OP_GET_FIELD` and `OP_SET_FIELD`
refer to the same (receiver, key), assign them the same cache slot.
Saves a slot per such pair and shares warmup state.

Sketch: parser-time def-use analysis. For `self.count = self.count + 1`,
both the GET and SET refer to `self`'s `count`. They could share an
entry shape (table_ptr, version, index) since both lookups bottom out
in the same table at the same index. Subsumed by register-based codegen
if that lands.

Why deferred: requires DEF-USE analysis that the parser doesn't currently
do. The memory savings are small (one cache slot per pair = ~24 bytes per
shared site). The warmup-sharing benefit is one fewer slow-path call per
new pair, which is bounded.

Signal: profiling showing the parser is fast and we have headroom to add
analysis passes, OR a workload where many distinct (receiver, key) pairs
each get fewer than ~3 accesses (so warmup amortization matters).

---

## Snapshot path

Still unmeasured as a whole: snapshots are driven from Rust, not from Lua,
so benching them needs harness work rather than a new `.lua` file (tracked
in TODO.md's workload-registry entry).

---

## Micro / take-or-leave

- **Call-path cleanup worthwhile even if the frame-stack rewrite lands
  later (B-O5):** the return-value half shipped 2026-07-26 (commit
  a96cff4); `eval.rs:508` is now the in-place `drain`. What remains is the
  `State::call` fixed-arg path, which still does `stack.remove(idx)` to
  extract the callee (`eval.rs:68`) - an O(args) memmove per call. Avoid it
  by treating the callee slot as frame slot -1 (adjust `stack_bottom`);
  pairs naturally with the rewrite.
- **Dispatch micro-items, verify with asm/bench first (B-O6):** opcode space
  is sparse (0-25, 30-54, 60-63, 70-72); a dense renumbering (or
  `#[repr(u8)]` enum with a validated dense range) helps LLVM emit a single
  dense jump table without range holes. `get_instr` bounds-checks every
  fetch; with one-time validation that all jump targets are in-bounds at
  load/finalize time, the fetch could use a pointer/len cursor - only if
  profiles show it; keep panics over UB.
- **Table micro (C-O8):** remaining note only -
  `try_insert_table_direct`'s double probe is only on the
  metatable-present path; the no-metatable path is single-probe
  already - fine as is. (The `get_full` single-probe and the
  promote-to-8 capacity shipped 2026-09-03.)
- **Stdlib micro (E):** `string.format` should format into one output buffer
  instead of per-directive Vec round-trips (invasive across ~15 helper
  functions for a micro win; measure before bothering).
  `is_plain_lua_pattern` treats `-` as magic even though a `-` with no
  preceding class item at pattern start is literal; conservative is fine,
  just noting the fast path misses hyphenated plain needles like
  `"foo-bar"`.
