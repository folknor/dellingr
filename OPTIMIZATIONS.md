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

Signal that would promote it: `numeric_index` and `table_fill` becoming
the dominant slowness in a real game-script workload. Currently both are
in the 400-800us range and not flagged as bottlenecks.

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
