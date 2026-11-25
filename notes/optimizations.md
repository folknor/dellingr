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
sources: register bounds checks have their own entry below; the pending
cost batch's escaped `&mut` slot is settled (a register batch was built,
measured and refused at `Frame::accrue`); `GET_UPVAL` indirection is settled
(capture slice selected once per activation in `run_frame`, closed-cell
fast path refused at `State::upvalue_value`, region widenings refused at
`Layout::load` and `Layout::finish`).

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

## Full coherent rewrites

Large enough to reshape whole subsystems. Each subsumes several smaller
entries below; check the cross-references before starting a subsumed item.

### Call path after the frame-stack flattening (B-O1 remainder)

Lua-to-Lua calls run in one dispatch loop over a per-invocation frame stack
since 27ee714. `CallInfo` names its code by runtime-cache key and admission
copies the callee's layout instead of cloning an `Arc`, so a Lua call keeps
one `Arc` pair (the frame's runtime); the `CallInfo`/frame merge is not a
standalone item any more (reasoning in the `Frame` doc). What is still open:

- Continuations. A handler or iterator run as an activation of the current
  invocation needs its frame on that invocation's frame vector and a
  continuation owner; the `CallInfo` stack can stay separate. Sites: `index_call`, `newindex_call` and `len_call`
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
left is one position per site. A site whose receivers alternate between
layouts that hold the key at different positions misses on every switch and
refills from the keyed lookup. N positions per site cover the same hits a
per-site set of N shape guards would, with no table field; each failed
guess reads another slot of the receiver, where a shape guard would compare
an already-loaded id. The same alternating shape is the only one where a
`GET_FIELD`/`SET_FIELD` pair on one receiver and key would gain from
sharing a cell (the read's refill would warm the store); the reasoning is at
`Bytecode::assign_cache_slots`. No registered workload has it: promote
either on one with persistent alternating-offset misses. A cross-receiver
store hint is refused on cost, not soundness (reasoning at
`State::store_field`).

### Branching leaves: field sites resolved on every path (2026-11-25)

Every field site of a leaf is resolved before it runs, so a leaf whose
skipped path reads a field the receiver lacks declines the call. A leaf
runs whole or not at all, so only sites that every path reaches before
its first store can be resolved lazily: mark a failed one unavailable and
fail the leaf at the `CALL` when a run reaches it, its result, path and
charges discarded. No workload has such a leaf.

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

---

## Host boundary and per-State costs

### `ipairs` regions: eager record bind (2026-11-25)

A typed `ipairs` region whose body uses the value as a record binds it at
the pass's first operation (`OpKind::Bind`), so a value that is no table
hands the rest of the loop back even on a pass whose path never touches
it (`if active[i] ~= 0 then s = s + u.x end` over records mixed with
`false`), and the key-1 entry rule means no retry. The per-path fact a
lazy bind needs exists: `Facts.rmust`. A bind at each consumer reached
without it also needs the yielded register's stack value to be the
step's (refuse intervening writes or call clears), must exit at the
consumer's first instruction before its charge (not at the body start,
which would replay the prefix), and must sit at branch targets that land
on a consumer. The self-guarded tombstone idiom (`if e then ... e.x end`)
additionally needs a region op for a bare `TEST` of a register, which no
region admits today. No workload holds such an array.

---

## Snapshot path

Benched by the `snapshot` scenarios (`brokkr bench snapshot --scenario
snapshot_save` or `snapshot_load`). The first profile (perf, bygg,
2026-11-25) put about a quarter of save in a reachability set that repeated
the id maps, and about a third of load in per-table duplicate-key sets built
from copied string bytes, run twice. Both were removed, and save now takes
about 0.8 ms and load about 1.5 ms of a 2000-unit world (teardown included).
What a perf profile of that build still shows, none of it measured as a
change yet:

- **Save:** `finish_object` spends about 16% converting each table's
  `(Option<SavedVal>, Option<SavedVal>)` pending slots into the final pairs,
  a second pass over every entry. Writing the saved pairs in place, with
  "unfilled" tracked some other way, would remove it. About 5.5% is a
  `reserve_rehash` of an `IndexMap<usize, Arc<BytecodeRuntime>>` (the
  `bytecode_caches` shape) inside the measured loop. Find out where that
  map grows before treating it as save cost. Id-map probes and `string_id`
  are each under 5%.
- **Load:** the time is spread out. Decode of `SavedVal` and `SavedObject`
  is about 17%, `rebuild_from_snapshot` 5%, `validate_table_shape` 5%, and
  the remaining duplicate-key sets of tables past eight hash keys under 4%
  each. No single site dominates.

---

## Micro / take-or-leave

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
