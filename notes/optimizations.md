# optimizations.md

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

### Per-instruction weight of the register dispatch loop (re-measured 2026-11-25)

Measured on bygg (Zen 5, 9950X3D2) at 4495fca with the 8-byte `Val`.
`bench/closed_mode` (canonical dispatch of upvalue, arithmetic and branch
ops): about 55 machine instructions and 9 cycles per VM op at an IPC near 6,
branch misses negligible. The loop is issue-bound, so retired instructions
translate into cycles.

Residue: the code slice's pointer and length are still reloaded from spill
slots at the dispatch head; nothing shown recovers them. The other weight
sources have their own entries below: register bounds checks, "Cost
accounting on every arithmetic op" (the pending batch is an escaped `&mut`
slot loaded and stored per charge), and `GET_UPVAL` indirection.

### Register access bounds checks

Measured 2026-11-25 on bygg, 4495fca, matched interleaved `--commit` pairs,
3 rounds: making every value-stack index unchecked (throwaway newtype over
`State.stack`) took closed_mode 4364 to 3782 ms (-13%), the composite
`benchmark` -4%, wide_sweep -6%, key_sweep -5%, each 3 of 3; arithmetic flat
(its hot loop is a typed region). Instructions on closed_mode 86.1e9 to
72.6e9. This overturns the 2026-10 Zen 3 refusal.

`unsafe` unchecked access is refused: the verifier would become a
memory-safety boundary for snapshot files. A padded stack with fixed register
windows was tried and lost (`State.stack` in `src/vm.rs`). Untried safe shapes, each judged only by a matched A/B against the plain
`Vec`: a single checked maximum-index guard on a multi-operand fast path
(`SET_TABLE`, `GET_TABLE`) with safe indexing of the same slice, adopted only
if release asm drops the subordinate checks; and a register borrow kept
across heap-only fast paths (a raw `GET_TABLE` hit writing its result
through the borrow that read its inputs). Medium each; check the asm first.

### Cost accounting on every arithmetic op

Every positive dispatch charge goes through `Frame::accrue` (one rule:
settle at `next >= flush_at`, else keep pending). Generic statically priced
straight-line regions were refused (2026-11-25; reasoning at `Frame::accrue`):
outside typed loop regions, `arithmetic`'s hot `fib` holds one charge per
run between calls. Two unmeasured residues remain:

- By-value batch: `pending` lives on the stack (`add (%rsp)` / `mov
  %rdx,(%rsp)` per charge in release) because its address escapes into the
  outlined helpers. The store/load chain gates the next charge's threshold
  branch, so it is not shown hidden. Threading the batch by value through
  every `local_cost` helper (returning it with errors) needs a matched
  `--commit` A/B on `arithmetic`/`method` and native-stack rows before and
  after. The old "3.6% error-path flush" figure is no estimate: its pair was
  unmatched (live tree against `--commit`), its commit also removed a field
  cache, and the code shape has since moved (one invocation-owned batch,
  settled on every exit).
- Template runs: `Frame::template_init_run`'s pinned writes are the one
  straight-line multi-charge run found (`static_records`: eight writes). A
  read-only prescan summing the run's weights could admit it once when all
  but the last fit strictly under the room, falling back to per-write
  charging; positions stay observable-identical. Unmeasured.

### `GET_UPVAL` indirection

What: canonical `GET_UPVAL` and `SET_UPVAL` index the activation's capture
slice (selected once per `run_frame`), then the pool slot, then branch on
open or closed. Typed regions take number upvalue reads and, without
calls, number upvalue writes (`closed_tally`), so what still dispatches
per instruction is a loop the region declines for another reason
(`closed_mode`, a string-mode test, is the registered workload): stores
other than number fields of a record, non-number branches, calls other
than leaves or invariant-receiver methods, an upvalue write beside any
call, and a captured leaf called on a captured argument, `f(a)` (the
argument's load hoists the callee as a number, so `CALL f` refuses).

Sketch: widen region coverage rather than the canonical arms. `f(a)` needs
the pending loads kept per register (each resolved at first use as a
number or a callee). An upvalue write beside a leaf call could be taken
when the leaf's own captures are proven disjoint from the written cells at
entry (by `UpvalueRef`), with the leaf's capture hoists then still exact.
A closed-cell fast path in the canonical arm was refused (reason at
`State::upvalue_value`). Hoisting the read into a register at compile time
is refused (codegen.rs header: it drops scratch register overwrites that
free dead values).

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

### Call path after the frame-stack flattening (B-O1 remainder)

Lua-to-Lua calls run in one dispatch loop over a per-invocation frame stack
since 27ee714. What B-O1 listed and is still open:

- Admission probes. On the ordinary direct Lua call that reaches frame
  admission, the closure is looked up three times:
  `publish_cost_unless_lua_callee`, `classify_callee` (which clones the
  bytecode `Arc` for `CallInfo`) and `admitted_frame` (which clones the
  runtime `Arc`, and the capture list's `Arc` only with two or more
  captures). An eligible numeric leaf returns before classification, and a
  host entry skips the cost probe. Frames no longer hold their own bytecode
  `Arc` (they read it through the runtime). Consolidating the probes must
  keep the cost-publication decision and the admission GC checkpoint, and a
  heap borrow cannot span that checkpoint.
- One activation stack (the `CallInfo`/frame merge). `CallInfo` parallels the
  frame stacks with a bytecode `Arc` and a cursor for tracebacks, `print`
  locations and the `ActiveChunk` root. The blocker is not tracebacks but
  borrows: `eval_batched` holds `&Frame` into the invocation's vector across
  every native re-entry, so a nested invocation cannot push onto it. Route
  (2026-11-25 spar): split resolution from re-entry. A resolver that only
  produces a value stays in the loop; one that needs a call returns it (the
  shape `IndexOutcome::Call` already has), the cursor is published, the frame
  borrow ends, and the driver runs the callee with full `&mut State` on the
  same activation vector, the entry index being the unwind boundary, and
  reacquires the frame by index. The unified activation would use its
  runtime for the active-chunk root and diagnostics, and a retired activation
  under a Rust tail callee keeps its runtime without rooting its closure.
  Payoff (one `Arc` pair and a duplicated cursor per call, diagnostics and GC
  on one stack) against the helper API split is the open question. Refused
  shape: frames borrowing descriptors from a State-owned arena by id, since a
  borrow cannot span a collection that mutates or reclaims the arena; an id
  design would need a different reclamation scheme.
- Continuations: `index_call`, `newindex_call` and `len_call`
  (`registers.rs`), the generic-for Lua iterator fallback (`eval_control.rs`),
  each custom sort comparison (`table_ops.rs`) and `gsub` function
  replacements each run a whole nested invocation lifecycle (native-depth
  accounting, boundary machinery, fresh dispatch, entry-result collection,
  teardown), not an allocation, since `run_invocation` reuses its vectors.
  `IndexOutcome::Call` (`metamethod.rs`) is already the seam. Suspend the
  operation instead: an index continuation knows where the result goes and
  which metamethod units to release; newindex releases units and resumes; an
  iterator continuation projects results into loop registers; a sort
  continuation keeps heap-sort phase, indices and operands. The handler is
  then an ordinary activation on the same stack. Sort must become a resumable
  driver: an adapter still calling `call_unguarded` per comparison keeps the
  cost. Obligations: keep the logical native-depth limit for the eliminated
  re-entry paths (letting formerly failing recursion succeed changes
  script-visible errors); exact comparison order and sort writeback timing;
  metamethod-depth ownership until the operation completes; where errors are
  located and reported; interrupt polls in resumable loops and at admissions.
  Arbitrary host Rust functions still need a real boundary. The codegen and
  table-and-math-libraries hunters raised the iterator and sort halves
  independently. Measured against the claim "the nested invocation is the
  cost" on bygg (de2a001, release): a 2000-element comparator sort, 37746
  comparisons, cost about 1.5 s per probe run where the same number of plain
  Lua-to-Lua calls of the same comparator cost about 1.2 s beyond the loop.
  The re-entry plus all the native heap work is at most ~20 percent of a
  comparison, so the sort half was not pursued (reasoning at `table_sort`).
  That number bounds only the comparator: the first step here is a probe of
  a tiny `__index` handler and a Lua iterator against equivalent plain Lua
  calls, then `rust_boundary` (registered at both scales; it runs both paths)
  for whether the saving shows in a mixed kernel. Design points settled in
  the 2026-11-25 spar: the caller instruction at `ip - 1` decodes where
  results go (`GET_FIELD`/`GET_TABLE`/`LEN` write the first result or nil to
  `R[A]`; `SELF` likewise without replaying its `R[A+1]` write; `SET_*`
  discard; `TFOR_CALL` projects into the loop registers from `R[A+3]`, and a
  fused `TFOR_LOOP` must be the real next instruction), each after validating
  the full returned count. The resolver's metamethod units and the logical
  native-depth increment need an outstanding-call owner of their own: the
  record's `chain` field is wrong, since `tail_call_from_lua` releases it
  before the tail callee runs, while today the units are held until the
  whole handler call returns. Keep the exit order too: the handler's cost
  batch settles before result validation, a callee error is reported while
  the callee still supplies source and traceback, and a refused admission
  is reported at the caller opcode.

Measurement lesson worth keeping: on plantasjen (Zen 3) the first flattened
version moved frames and records by value and lost 14-22% to store-forwarding
stalls although callgrind instruction counts were within 1-4% of the
recursive path; check `vm/eval.rs` comments before moving a `Frame` or
`CallInfo` whole. Callgrind instruction-level counts plus objdump of the hot
function located those stalls. `perf` works on that host only after
`kernel.perf_event_paranoid` is lowered for the boot (the distribution
default blocks unprivileged sampling); with it, `perf record` cycles plus
`perf annotate` of `Frame::run` attribute stalls to instructions directly.

Native-stack follow-up: `MAX_NATIVE_CALL_DEPTH` is 150, not reference Lua's
200, because 200 did not fit the envelope of the time, 2 MiB in the dev
profile. Since the envelope split it is 4 MiB in dev and 2 MiB in release
(docs/limits.md), which 200 fits on the per-level sizes measured then
(about 11 KB dev, 2.3 KB release, plantasjen). What holds it at 150 now is
demand, not stack: raising it is replay-visible, so it waits for a script
that legitimately nests calls made from Rust past 150.

### 8-byte NaN-boxed `Val` (C-O7)

Residue: typed arenas with distinct table and closure tags (the 2026-11-17
hunt's other half). `Val` is now an 8-byte NaN-boxed word, but its `Obj`
tag names neither a table nor a closure, so access resolves the handle and
examines `RawObject`, and `Val::typ` dereferences the heap. Two tags are
spare for it.

---

## Architectural

### Field-site position hints: residue

GET_FIELD and SELF read the site's cached position in any table receiver as
a key-checked hint (`State::get_current_cached_field`, `Table::string_value_at`;
the measurements and the refused shape-id IC are recorded there). What is
left:

- One position per site. A site whose receivers alternate between layouts
  that hold the key at different positions misses on every switch and
  refills from the keyed lookup. N positions per site cover the same hits a
  per-site set of N shape guards would, with no table field; each failed
  guess reads another slot of the receiver, where a shape guard would
  compare an already-loaded id. Promote on a workload with persistent
  alternating-offset misses.
- SET_FIELD keeps the same-table entry only. A cross-receiver store hint
  must prove what `GcHeap::overwrite_live_slot` requires (a live, non-nil
  overwrite of a key other than `__index` through a token valid for the
  current receiver), so it is not the read hint reused. Coverage only: the
  read hint holds a position, never a value, so it is sound without it.
- `bench/polymorphic` and `examples/fields/polymorphic.lua` (2 fields, a
  numeric `for`) now run entirely in a typed loop region, so their header's
  "per-callsite IC never hits" no longer describes them; `wide_sweep` is the
  workload that exercises the dispatch hint.
- The host hunter noted fresh option tables built by hosts get nothing from
  identity caches; the position hint now serves them when they share a
  layout.

### Compiler-recognized field-update fusion

Field updates in method bodies run as field leaves without an activation
(`vm/numeric_loop.rs`), straight-line or branching forward on number
comparisons and field truthiness up to the first `RETURN`; that takes the
activation, every dispatch and (from a region over records) the per-site
lookup misses, more than a fused opcode would. Residue: the same update
inside a function that calls, or returns early, or reads a field only on
a path the receiver lacks it on (every site is resolved on every path, so
such a call declines). There an `OP_FIELD_ADD` (or next-opcode fusion in
the `GET_FIELD` arm) would save two of an adjacent update's three
dispatches; most updates are not adjacent (`self.x = self.x + self.vx * dt`
puts a read and a `MUL` between the read and the add). It owes the
canonical register state and cursor when the add succeeds and the store's
`TableWrite` charge is refused, and must prove the receiver register still
holds the same table at the store.

Signal that would promote it: a workload whose hot non-leaf function
(one that calls) does such updates, with the dispatches showing in
profiling.

### Branching leaves: open costs and declines (2026-11-24)

- The straight-line region runner (`State::run_passes`) declines a loop
  calling a leaf with branches, since it charges whole leaf weights; each
  call still runs as a leaf. Following paths there cost `global` and
  `method` 15-25% in a first build, so it would need its own
  instantiation.
- Every field site is resolved on every path, so a leaf whose skipped path
  reads a field the receiver lacks declines the call.

### Shared record layouts: the storage side

Residue of the compile-time table-shape prediction entry. The constructor
side landed as an execution-side fusion (`Frame::template_init_run`: the
leading run of static loads and pinned writes after a `NEW_TABLE_TEMPLATE`
runs in one call); the batch-fill `DUPTABLE` form, a literal-prefilling
bytecode plan and a compile-time `GET_FIELD_AT` were declined, with the
reasons at `template_init_run` and `State::get_current_cached_field`.

What remains open (2026-11-17 table hunter's lead rewrite): materialize each
constructor's layout once in State-local runtime data, an instance holding a
shape identity and dense values, the shape owning ordered names and the
lookup index; structural mutation converts to dictionary storage carrying its
history. Today every template instance copies the prototype's slot vector and
clones its hashbrown index: `examples/alloc/record_tables.lua` allocates
416 B per eight-field record, all in `reg_new_table_template` (alloc
profile, bygg, 8b07d97), the index one of the two backing allocations.

Why deferred: field reads no longer need it (the key-checked position hint),
so the payoff is allocation and footprint only. Resident bytes, including
the index, pace collection and `gc_score` reports real backing capacity, so
dropping per-instance index bytes is a versioned, changelogged pacing change,
or the accounting must charge a fictional footprint that follows the old
mutation history. A distinct shaped storage variant need not slow ordinary
Map probes, but conversion must keep reserved nil keys, vacant positions and
compaction and reinsertion behaviour, and layout keys must stay rooted by
surviving instances after the creating runtime is gone. Fusing pinned writes
after a dynamic value (`ADD_RK` then `INIT_FIELD_PINNED` in
`record_tables`) is a separate execution-side fusion that needs a witness
that the register still holds the constructed table.

### Share the stdlib install across States

What: today `StateBuilder::build` runs `open_libs` for every new
`State`, building each library table and the global table (`_G`, an
ordinary heap table) from scratch. There is no longer an `Engine` to
hold a prepared copy, so sharing the install needs a new owner: a
prepared template the builder clones from.

Sketch: build the standard environment once into a template and give
new States a copy of it. Watch out: the library tables, `_G` and the
interned key strings all live in one State's heap, so a copy has to
rebuild them in the new heap (or the heap has to support cloning), and
the environment-table record (`GlobalEnv::env_tables`, with each table's
slot policy) is captured right after `open_libs` and must match the copy.

Shipped smaller step on 2026-05-11: stdlib globals and module tables now
install Rust functions through direct `Val`/string-key insertion helpers
instead of pushing key/function pairs through the Lua stack and calling
the public table setters. This reduces the per-State setup work without
changing the ownership model. The process-level `examples/comments.lua`
signal is tiny and noisy on this host (roughly 495us to 487us in a
200-run hyperfine sample), so this does not remove the shared-install idea.

Why deferred: not on the hot path. State construction is amortized
across however long the State lives; even at "one State per request
worker", the install cost is microseconds per request. The bigger
win would be at "one State per request" granularity, which isn't
the recommended embedding model.

Signal that would promote it: a profile showing State construction
dominating request-path latency, or a real "one State per request"
embedder asking. The `host --scenario state_new` bench (construction plus
teardown, about 5 us per State on bygg, 2026-11-24, dirty tree at 8b98de6)
is where a change here is measured.

---

## Stdlib / patterns

### Growing-producer residue after the reusable result buffer (2026-11-25)

`string.format`, `gsub` and `table.concat` now build in the State's
reusable result buffer and decide storage once at the push
(`reference/performance.md`, "Growing producers reuse one result
buffer"). Left open:

- `format_literals` is slower across that change (median 506 to 530 ms,
  0 of 5 faster), and was slower by the same amount across the stack-storage
  change before it, although its executed path is unchanged: allocation and
  per-function call counts on its hotpath file are identical in both arms.
  Read as layout, unconfirmed (no five-launch same-binary control run).
  `patterns` (+0.9%, 1 of 5 faster) and `mixed` (+0.5%, 0 of 5) are inside
  the floor; `mixed`'s counts are identical too.
- A long result that hits an existing string drops its buffer; the
  post-policy probe could hand the unused `Vec` back for reuse (within the
  same capacity cap). A pre-policy hit shortcut would move the collection
  point and is not an option. Unmeasured. `patterns` is the probe: its
  three long results (the two `gsub(log, ...)` calls and the last `gsub`
  in `replace`) repeat their content every kernel call but are not
  retained across calls, so whether they hit after the allocation policy
  runs is the first count to take, by site, beside buffers actually reused
  and alloc/realloc/free calls. The 2026-11-24 owned-interning pair on this
  workload does not implicate the hit path (reference/performance.md,
  "Code-alignment recipe sweeps").

---

## IC extensions (same shape as existing ICs)

### Method IC: three or more receiver metatables at one canonical site

Each allocated GET_FIELD or SELF slot holds two one-level method entries
(two-way LRU, keyed by receiver metatable identity) beside the field, route
and string-method cells; `size_of::<FieldLookupCacheSlot>()` was 184 bytes
at 77954a3, 80 of them the method entries. The SELF-only family split is
refused (reason at `FieldLookupCacheSlot`). The registered method kernels
(`method`, `method_polymorphic`) now run in typed loop regions, whose
per-run cache keys up to eight metatables, so they no longer exercise this
IC; no registered workload has a canonical site cycling three or more
one-level receiver metatables.

The case that would: a canonical site (a host callback `tick(obj)` doing
`obj:step()`, or a shared accessor for a prototype default) cycling three
or more metatables, including per-instance metatables that share one
`__index` table, which exhaust identity-keyed entries though the route is
the same. First shape to try: a hit-only key-checked position pair after the
receiver's raw miss (the remembered `__index` position on the live
metatable still holds `__index`, its value is a table, and the remembered
position there still holds the site's key with a non-nil value; any failed
check falls through), the cross-receiver own-field hint applied twice. If
positions are stable but incompatible, a bounded overflow consulted only
after the inline entries miss. A State-global (metatable, key) backstop is
untested, not refused.

Signal: a workload of that kind with measured replacement misses, or a
memory profile where field cache slots matter.

Note: caching misses was tried and dropped. An exact absence proof (no hash
slot for the key, unchanged version and slot count) is sound but validating
it reads as many tables as the uncached miss, so it saved nothing on
`bench/method_chain.lua`. The repeated direct attempt that cost it its time
is gone since the register-native rewrite (each table read once per
lookup; 85.3 to 64.7 ms warm, plantasjen, df80d81 against the rewrite).

### GET_FIELD slow-path cache repopulation after `__index` resolution

What: after `State::resolve_index` resolves through `__index`, the resolved value lives somewhere
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

---

## Host boundary and per-State costs

### Tree expansion during closure tracing

From the spar on the runtime reconcile (2026-11-24): a reached runtime with
nested prototypes could expand its tree into the live-chunk set when closure
tracing first flags it, removing the discovery pass over the runtime map in
`sweep_bytecode_caches`. Pending identities must stay out until tracing
finishes. It moves work into the closure tracer and leaves the literal-root
visit and the map `retain`, so it pays only on states whose retained
runtimes mostly have nested prototypes; no workload has that shape.

### `ipairs` regions: eager record bind, three or more variables (2026-11-24)

A typed `ipairs` region whose body uses the value as a record binds it at
the pass's first operation (`OpKind::Bind`), so a value that is no table
hands the rest of the loop back even on a pass whose path never touches
it (`if i ~= 2 then s = s + u.x end` over `{ {x=1}, false, {x=3} }`), and
the key-1 entry rule means no retry. A guard at the first consuming
operation on each path would take such mixed arrays; it needs a per-path
"bound" fact the solver lacks. No workload holds such an array. Loops with
three or more variables are still declined (the extra ones are nil every
step).

---

## Snapshot path

Benched by the `snapshot` scenarios (`brokkr bench snapshot --scenario
snapshot_save` or `snapshot_load`): a 2000-unit world graph saves in about
2 ms and loads (this build's own save, teardown included) in about 2.8 ms on
bygg (2026-11-24, ms-scale run, dirty tree at 8b98de6). No profile of either
path is recorded yet; nothing here is a candidate until one is.

---

## Micro / take-or-leave

- **Global stores inline an array-part branch they never take:**
  `GlobalEnv::write_cached` is `inline(always)` into SET_GLOBAL and
  SET_BUILTIN and drags in `set_at_index`'s array-part branch, although a
  global's slot is always in the hash part. A hash-only store variant might
  trim `Frame::run` further. Mind the `bench/write` lesson from 2026-10-23:
  removing a never-taken slow branch from this inlined helper made the steady
  state 14% slower, so measure before trusting the idea.

- **Rust-callee `stack.remove(idx)` (B-O5), rejected:** the return-value
  half shipped 2026-07-26 (commit a96cff4). Keeping the callee as frame
  slot -1 to skip the O(args) memmove in `begin_call` was rejected: the
  contiguous Rust callback ABI may use every slot up to the global stack
  cap, so a retained callee would refuse a callback push one slot earlier
  (`rust_call_releases_callee_slot_before_callback_push` in
  tests/integration/host/host_call_preflight.rs). Moving the callee elsewhere still shifts
  the arguments; see the comment at the remove in src/vm/eval.rs.
- **`string.format` cached plan (deferred, 2026-11-24):** `perf record` of
  release `bench/format_literals.lua` (bygg, b85864e) put about half of
  `format`'s own samples (about 10% of the run) on the bytewise scan for the
  next `%`, directive parsing a few percent of `format`, and about 15% of the
  run in allocator growth of the output. The original word-at-a-time scan
  and a bounded capacity hint on the first literal run cut about 25% of the
  cycles in the same profile, and `format`'s
  own time had no hot spot left (wall, bygg, two matched `--commit` pairs in
  both orders against b85864e: 646, 643 to 524, 520 ms; measured with the
  word scan, before `memchr` replaced it unmeasured). Re-argued and refused
  again on 2026-11-25: a plan cannot size `%s` output before its
  `__tostring` runs, so it cannot remove the output buffer, and that buffer
  is now the State's reusable result buffer anyway. A plan cache keyed by template
  identity would now save only the parse and the per-run scans. Reopen only if a
  warm, directive-dense workload with cheap arguments profiles the parse as
  meaningful; `--hotpath kernel` cannot answer it (`format` and
  `format_argument` are native-call inventory entries and carry no
  measurement site), so use `perf`. A plan would also need a byte bound
  on construction (a 16 MiB `%%` template is about eight million escape
  events, and a malformed suffix must not change the error a call raises). A
  cache needs
  invalidation on collection (a `StringPtr` is reused; the pattern cache is
  the precedent), must survive nested `__tostring` calls that can evict it,
  and must keep today's event order (missing argument before parse, parse
  and validation before the charge, charge before rendering, a poll per
  `%`). Residual allocation: `%s` of a non-string without a
  string-returning handler (`TostringRendering::Bytes`).
