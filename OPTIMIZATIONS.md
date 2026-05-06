# OPTIMIZATIONS.md

Forward-looking ideas: optimizations considered but not yet implemented, plus
notes on places where current code is deliberately conservative. This is a
working backlog, not a record of decisions. Delete entries as they ship, get
contradicted by new evidence, or stop being worth tracking.

Each entry: what, sketch, why-not-yet, signal that would change the calculus.

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
VM-level, which is where we've been working.

Signal that would promote it: a workload where method bodies of the form
`self.field = self.field + ...` dominate, and the per-instruction
dispatch cost shows up in profiling. method_dispatch is now ~200us; if
we hit a floor below that, this is the natural next step.

### Compile-time table-shape prediction

This is two distinct work items that share a name; they have very
different ROIs and should be considered separately.

**(a) Pre-sized constructor (shippable in a small parser PR).**

What: when a constructor is statically a sequence of named-key
assignments (no computed keys, no `[expr] = …` entries, no spreads),
the parser emits a `DUPTABLE`-style opcode that allocates the table at
exact final IndexMap capacity with the keys pre-installed in their
final positions. The current parser emits `NEW_TABLE` followed by
per-field `INIT_FIELD`, so the win is avoiding per-key inserts +
hash-grow during construction.

Sketch: a per-Chunk template stores the key list as interned-string
ids (not `Val` payloads, to avoid GC complications - see below).
Runtime allocates `Table::with_pinned_shape(template_keys)` which
prepopulates the IndexMap with `Nil` values at the template positions;
subsequent SET-by-pinned-index overwrites in place.

Why deferred: small but real change to bytecode + Table API. Hasn't
been measured against current `NEW_TABLE` + `INIT_FIELD` cost.

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

---

## IC extensions (same shape as existing ICs)

### Table-library fallback IC

What: when `tbl:insert(...)`, `tbl:concat(...)` etc. fall through from
direct lookup to the `table` global library, cache the resolved value at
the call site.

Sketch: same shape as the string method IC above, but for the `table`
global instead of `string`.

Why deferred: rarer pattern than string methods in current code, and the
fallback path itself isn't a hot bench.

Signal: same as string method IC - a workload where these fallbacks fire
in a tight loop.

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
in the same table at the same index.

Why deferred: requires DEF-USE analysis that the parser doesn't currently
do. The memory savings are small (one cache slot per pair = ~24 bytes per
shared site). The warmup-sharing benefit is one fewer slow-path call per
new pair, which is bounded.

Signal: profiling showing the parser is fast and we have headroom to add
analysis passes, OR a workload where many distinct (receiver, key) pairs
each get fewer than ~3 accesses (so warmup amortization matters).
