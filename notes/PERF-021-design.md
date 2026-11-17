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
11. **W** is a tuning constant in `src/limits/tuning.rs`. Candidates are
    chosen from a measured slot-scan cost against an object mark and sweep,
    and decided on retained garbage and pause tails across these shapes:
    scalar slots, hash pairs, duplicate references, small-object churn, and
    payload-carrying churn.

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
