# PERF-021 design: pace collections by resident payload

Settled by spar with codex (write-sol-high), 2026-11-17, at c02940e.

## Premise, verified

`Table::mark_values` scans `array[lead..]` and every inline or map slot, and a
scalar slot has no edge. `GcHeap::is_full` compares objects plus distinct
strings with the threshold, and `collect` recomputes the threshold from the
same count. With L live allocations and one live N-slot table, every
collection scans N slots and collections are about L allocations apart, so
mark work per allocation is about N/L, with no bound. The premise holds.

## Rulings

1. **Reference summaries are refused** (the entry's proposal). Every array
   write route would have to keep them up to date. A missed update under-counts,
   and a live object then goes unmarked and is freed. They add cost to the
   array write paths, which measurements have shown to be sensitive, and they
   only help scalar arrays. A large array of references, or many duplicate
   references, still gets rescanned on every collection. The reasoning is
   recorded at `Table::mark_values`.
2. **Mark-time pacing alone is refused.** In this design the threshold is
   credited from the slots scanned. A live scalar table then gives about
   `growth * N / W` more object allocations of headroom, and every one of
   them can carry N slots of dead payload, so retention grows as `N^2 / W`.
3. **Collections pace by a weighted resident score**: objects + strings +
   resident_bytes / W. Automatic mode only. A Manual threshold, Disabled mode,
   `object_count` and `string_count` keep their present units. The threshold
   after a collection is computed from the same score. Saturation stays
   `Automatic(usize::MAX)`. Growth factors 0 and 1 need a stated domain:
   floor division must not let an intern hit collect on every attempt.
4. **Resident bytes measure backing capacity.** They cover array `Vec`
   capacity, the `HashSlots` entry vector capacity and the index's
   `allocation_size()`, and string `Box<[u8]>` length. Inline entries are part
   of the object and cost nothing extra. A charge is made where backing is
   created or grows. A credit is made only for capacity that is actually
   returned. Hash compaction keeps its backing and earns no credit. Sweep
   debits each dead table's capacity and each dead string's length.
5. **Plumbing is a split-borrow table editor from `GcHeap`.** It carries
   `&mut Table` and `&mut` resident bytes. Growth operations take the editor
   and apply deltas at the backing operations. Validated overwrites
   (`set_at_index`, `overwrite_live_prefix`) keep their path, which touches no
   capacity. There is no per-table field and no atomic. Whole-table ownership
   is accounted separately. A restricted environment is charged when it is
   installed. A suspended outgoing backing stays charged. Resume credits only
   the discarded restricted backing. Constructors and snapshot rebuild account
   for backing when they publish or replace it.
6. **The meter only schedules collections.** A missed charge is a timing or
   policy defect, never unsound, as long as no credit subtracts an unmatched
   charge. Debug collections check the meter against a capacity census that
   includes suspended environments.
7. **Trigger.** The allocator gate stays. Automatic pressure is also serviced
   at Lua GC checkpoints: taken back-edges, and rooted call admission
   including tail calls. Back-edges alone miss tail recursion that allocates
   nothing. Each checkpoint is already a legal collection point, because the
   next instruction could allocate. The audited sites (SET_LIST, template
   init, metamethod admission, sort comparators) have complete roots.
   Allocator-gate-only was refused because staged create-then-grow defeats
   it: Theta(N/W) empty tables are created first, then grown and dropped one
   at a time with no gate.
8. **Hot poll cost stays at one load and one branch.** When growth crosses a
   byte watermark, the State's poll source switches from the shared Interrupt
   to a private flag that is always set. The cold path honours the real
   Interrupt first, then rechecks pressure. The watermark is refreshed after a
   collection and whenever counts or policy change. The generic Interrupt
   poll stays cancellation-only. Native growth loops (`table.move`) opt in at
   audited iteration boundaries. The default string sort must not opt in,
   because it holds slices borrowed from the string pool.
9. **What is bounded.** Weighted resident <= threshold + all growth between
   two checkpoints. One sparse write can reserve a large gap, and a host
   callback can grow backing repeatedly before it returns. Table scan work is
   bounded per unit of incoming weighted payload, not per object allocation:
   if a live N-byte table sits beside churn of N-byte temporaries, holding
   dead storage to O(N) has to mean scanning N per O(1) temporaries. A single
   pause is not shortened.
10. **Replay-visible.** Collection timing moves in every State that has table
    or string payload. CHANGELOG callout. docs/limits.md ("Garbage collection
    policy", "State-local growth") and docs/determinism.md ("Garbage
    collection timing") are rewritten. docs/save-state.md: the threshold stays
    host configuration, and a loaded State can derive a different score than a
    continuous one, because saves trim nil array ends.
11. **W = 256 bytes**, `GC_RESIDENT_BYTES_PER_UNIT` in
    `src/limits/tuning.rs` (amended at link 3, spar with codex
    write-sol-high). W is an exchange rate between payload and objects, not
    a measured cost ratio: about one heap object's footprint (16 `Val`
    slots), so the score reads as resident memory in object-sized units. It
    sets a tradeoff, not correctness: at growth g a cycle admits about
    (g-1) * (live bytes + W per live object) of new payload, and a scalar
    table is rescanned about W/16 slots per unit of headroom. The measurement
    campaign the original ruling asked for was refused: slot checks,
    already-marked references and slot-map sweep steps have different costs
    and access patterns, so a probe ratio would pick a point on one machine
    for a constant that has to be a fixed literal anyway. Never `size_of`:
    object layout differs between debug and release, and timing is
    replay-visible. Host-configurable W was refused (a new replay-visible
    host input nobody asked for). Known tradeoff, accepted: a large string
    that survives a sweep buys headroom that persists after it dies, so its
    own reclamation can wait a full cycle; that is proportional pacing, not a
    reason to move W.
12. **Growth factors 0 and 1.** The next threshold is
    `max(live * g, minimum, live + 1)`, saturating. The guarantee is only
    one unit of headroom for a score below `usize::MAX`: an unchanged heap
    (a rooted intern hit right after a collection) does not collect again.
    One allocation can still consume it.

## Link plan

1. **Baseline workload** (small). Register a workload at both scales
   (AGENTS.md "Benchmarks"): a live scalar array of about 10^6 slots plus
   small-object churn, together with a payload-churn variant. Store wall
   baselines at HEAD.
2. **Resident meter and editor** (large). Add `ResidentBytes` on `GcHeap` and
   the split-borrow editor, and charge and credit every backing site in
   `table.rs` and `hash_slots.rs`. Also covered: environments, snapshot
   rebuild, the string pool (miss and sweep), the sweep debit, and the debug
   census. The trigger does not change yet. Nothing is replay-visible.
3. **Weighted threshold at the allocator gate** (medium). Score formula,
   growth-factor domain, policy tests (`tests/integration/runtime/gc_policy.rs`,
   object.rs threshold tests), W from link 1's shapes. Replay-visible:
   CHANGELOG and the docs listed in ruling 10.
4. **GC checkpoints** (medium-large). Watermark, private poll flag, back-edge
   and call-admission servicing, the `table.move` opt-in, and a test of the
   staged create-then-grow case with retention bounded. Add a
   `reference/native-calls.toml` entry only if a new path into Lua appears
   (none is expected). Measure `native_bytes_per_level` before and after if
   `Frame::run` is touched.
5. **Measure** (small). Interleaved A/B on the link 1 workloads plus
   `gc_churn`, `fill`, `queue`, `list_literals` and `rekey`. Write the results
   with commit and host to reference/performance.md.
6. **Green agent.** `brokkr fmt`, then `brokkr check` to green, then
   CHANGELOG, then remove the PERF-021 entry and this document, then commit.

### Link 1 implementation

Both workload pairs retain the same dense array of 1,000,000 numeric slots.
Each kernel reads its last slot, keeping the array reachable across calls.
`gc_scalar_churn` allocates 2,048 two-element tables per kernel.
`gc_payload_churn` allocates 128 tables and fills each with 1,024 numeric
slots. The first shape emphasizes allocation frequency and repeated scans.
The second adds backing growth and dead payload while keeping the live scan
the same. Both examples run one kernel for the profiling scale. The wall
wrappers repeat the respective kernels with identical bytecode.

The wall baselines ran serially on bygg with the unchanged VM at
`5c2cd0766`. All three iterations of each workload agreed to the reported
precision:

| Workload | Best wall of 3 | Brokkr run |
|---|---:|---|
| `gc_scalar_churn` | 12.2 s | `40bffd8be80f43618abac2d0cdac2ed1` |
| `gc_payload_churn` | 1.7 s | `651d68c066a7461c921654b9bbadc677` |

`brokkr check` passed all 15 sweeps. The example list and corpus cost
fixture include both profiling scripts. No design decision was handed back.

The session has read-only access to .git, so the link is uncommitted and
Brokkr saved both runs as dirty. Remaining work (small): commit link 1 on
main with Git write access before editing the VM, and rerun the two wall
arms from that clean revision to store comparable baselines. Use that
pre-meter revision for link 5's matched A/B cells.

### Link 2 implementation

The heap now counts array vector capacity, hash slot vector capacity and
index allocation size, and interned string payload lengths. The table editor
passes the meter to array reservations and shrinks, hash reservations and slot
growth, and inline-to-map promotion. Existing reservation preflights cover
the subsequent array pushes, batch extend, and downward splice. Map
compaction keeps its backing and makes no debit. Slot-only stores and default
sort writeback do not update the meter.

Constructors and successful snapshot rebuilds account for complete backing at
publication. A failed rebuild keeps the previous table and its charge. A
restricted environment builds against a local meter until installation. The
heap keeps the outgoing environment's charge across suspension, and releases
the discarded backing on resume or when resetting suspended state. String
pool hits do not update the meter. Sweep debits dead table backing and string
payloads without changing their sweep order.

Debug collection compares the meter with a census both before and after
sweep. The first comparison includes garbage and catches growth accounting
errors even when the affected object dies in that sweep. The second checks
the sweep's debits. Both include all suspended environments. Existing tests
now also check string byte totals across hits and sweeps, and collect again
after both restricted environments resume.

The collection threshold and trigger still use the existing allocation count.
Link 2 has no replay-visible change and hands back no design decision.

`brokkr check` passed all 15 sweeps. Link 2 is complete and remains
uncommitted on main because the session has read-only access to .git. The
clean baseline reruns listed under link 1 are still pending.

### Link 3 implementation

The handback asked for a probe campaign to choose W. It was settled instead
by reasoning (ruling 11) and W = 256. `GcHeap::is_full` compares the
pacing score (`pacing_score`: objects + strings + resident / W) in
Automatic mode and the allocation count under a Manual threshold;
`GcHeap::collect` derives the next Automatic threshold from the score with
the ruling 12 floor. `State::gc_score` exposes the score; `heap_size`,
`object_count` and `string_count` keep their units. Policy tests in
`tests/integration/runtime/gc_policy.rs` cover payload in the score, the
Manual threshold ignoring it, and growth factors 0 and 1. docs/limits.md,
docs/determinism.md, docs/save-state.md, the `State` API docs and
CHANGELOG carry the replay-visible change.

Lateral, filed in notes/performance.md: the sweep visits every slot-map
slot up to its high-water mark, so a heap that once held many objects keeps
paying for vacant slots on every later collection.

Links 1 to 3 were committed together, so link 5 has no clean pre-meter
revision in the tree's history after the fact: use 5c2cd07 as the A side.
