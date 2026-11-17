The biggest opportunity is to stop representing repeated records as independent ordered dictionaries. I would make that rewrite. It can remove work from construction, field access across many objects, and GC together.

The other substantial opportunities are separating dictionary lookup from iteration history, and separating numeric storage from the historical array boundary. Those are coherent rewrites too, but their payoff is more workload dependent.

These are architectural hypotheses from code inspection, including the pinned IndexMap, hashbrown, and foldhash implementations. I made no changes and ran no builds, tests, or benchmarks.

1. **High-conviction rewrite: shared record shapes with per-instance values.**

   A constructor template currently saves field-write lookups, but it does not save constructing the dictionary. [with_template_keys](/home/folk/Programs/dellingr/src/vm/table.rs:625) allocates an IndexMap and inserts every key with a nil value for every new instance. [Pinned initialization](/home/folk/Programs/dellingr/src/vm/registers.rs:333) subsequently fills those slots.

   Consequently, thousands of identically shaped objects each own the same keys, cached hashes, and hash index. Small records avoid allocations but still duplicate keys and search them linearly. Templates currently apply only to unique, entirely named constructors larger than four fields, as selected in [parser/table.rs](/home/folk/Programs/dellingr/src/compiler/parser/table.rs:54).

   There is a second structural limitation: the existing own-field cache identifies a particular table, rather than a layout shared by tables. Iterating through an array of identical records therefore defeats that cache. The code in [fields/polymorphic.lua](/home/folk/Programs/dellingr/examples/fields/polymorphic.lua:1) exercises exactly this access pattern.

   **The redesign:** materialize each constructor's layout once in State-local runtime data. A record instance holds a shape identity and densely stored values. The shared shape owns the ordered names and their lookup index. Known constructor writes address value offsets directly.

   Small records should hold values inline without repeating their keys. Larger records should allocate a value block, without allocating and populating an independent hash index. Structural mutations can convert a record into dictionary storage, carrying its existing history with it. A general dynamic shape-transition system is a follow-up, afterwards, if this works.

   The table interface should expose a shape identity and a field-offset token valid across all instances of that shape. That lets the lookup-paths scope implement a field hit as shape validation followed by a value load. Changing caches belongs to that sibling scope, but providing this interface belongs here.

   This removes several costs together:

   - Repeated key hashing and map population during construction.
   - Per-instance key and index storage.
   - Repeated lookup when a call site visits different objects with the same layout.
   - Repeated tracing of identical name lists during GC, provided shared shapes are marked once.

   There is also a footprint opportunity. [TableStorage](/home/folk/Programs/dellingr/src/vm/table.rs:334) embeds four complete key/value pairs in its inline variant. Every table must accommodate that variant, including pure arrays and large dictionaries. [RawObject](/home/folk/Programs/dellingr/src/vm/object.rs:224) stores Table inline, so its size also affects closure slots. I would redesign these layouts together around compact record values and optional numeric or dictionary storage. Simply boxing the existing Table would exchange footprint for another allocation and pointer chase.

   **The main risk is history, not field lookup.** Existing large templates reserve nil-valued keys; ordinary small constructors do not reserve a key merely because its initializer is nil. Deletion and reinsertion can change order, and snapshots distinguish storage kinds. A shared layout must describe where a field *could* live without accidentally declaring that it has an existing traversal-control slot.

   Shape conversion must preserve these distinctions, not reconstruct a table from its live fields alone. Shared names also need GC ownership when a record outlives its creating closure. The creating bytecode's literal roots are insufficient by themselves.

   This is the strongest broad-throughput hypothesis because it removes dictionaries from a common class of game objects, rather than making each dictionary operation marginally cheaper.

2. **Medium-conviction full rewrite: own the dictionary's lookup index and iteration history separately.**

   IndexMap already provides a competent foundation: SwissTable probing, a dense insertion-order entry vector, and cached hashes for rehashing. Replacing it with another ordinary ordered map is unlikely to change much.

   The structural mismatch is how dellingr preserves deleted slots. A deleted key remains indexed so `next(t, deleted_key)` can resume. When that key is reinserted, [insert_into_map_with_tombstones](/home/folk/Programs/dellingr/src/vm/table.rs:2005) renames the old slot to a synthetic `Vacant(index)` key and appends the real key.

   The dependency implementation makes that rename expensive: `replace_index` checks uniqueness, removes the previous hash-index entry, and inserts the replacement. Dellingr then inserts the real key again. Synthetic vacant positions occupy the hash index even though no Lua query should find them.

   **The redesign:** use a table-owned ordered slot vector and a hashbrown `HashTable` indexing actual keys. Preserve dead real keys in the index while they remain valid controls. Keep vacant historical positions only in the ordered vector.

   Reinsertion then becomes:

   - Find the existing dead key.
   - Append its new ordered slot.
   - Mark its old ordered position vacant.
   - Change the existing hash-index entry to reference the new position.

   The lookup index does not need to delete the real key, insert a fictitious key, and insert the real key again. Its occupancy tracks indexed keys rather than all historical positions.

   This is supported by APIs already present locally. Hashbrown provides `find_entry`, mutable occupied entries, `entry`, `insert_unique`, and fallible reservation. The string interner already demonstrates using a separate hash index in [object.rs](/home/folk/Programs/dellingr/src/vm/object.rs:990).

   Stable compaction would still occur at the existing logical thresholds. It would compact the ordered history and rebuild the index. Global tables retain their different revival and retention policies.

   **Why it could pay:** delete/reinsert churn currently performs several index operations per logical write and increases index occupancy with synthetic entries. Removing that work is a concrete improvement for mutable registries, sets, and job tables. The gain is smaller for dictionaries that mostly read or overwrite live values: the existing implementation already handles those paths reasonably directly.

   **The risks:** exact compaction timing must remain a function of the old logical slot counts, rather than the new index occupancy. Otherwise deleted-control validity, snapshot contents, and subsequent reinsertion behavior change.

   Cache tokens should identify logical slots under an epoch, rather than expose hashbrown bucket positions. Bucket indices have a narrower validity contract than the current slots and can change during insertion or resizing.

   I would pursue this as a churn-focused rewrite. I would not claim that it automatically accelerates every hash read: an index-to-entry dependency remains, and IndexMap already has cached hashes and efficient probing.

3. **Medium-conviction full rewrite: separate numeric payload storage from the logical array boundary.**

   The current array window is effective for an ordinary FIFO. It can drop a dead prefix and keeps absolute array-slot tokens valid.

   Its two remaining structural limitations are more substantial than the queue case.

   First, the window stores every position between its live endpoints. Fill keys `1..N`, then clear `2..N-1`: two live entries still pin a window spanning N values. Iteration and GC scan that span. A permanent low-key sentinel beside an advancing queue similarly prevents prefix reclamation.

   Second, [the append policy](/home/folk/Programs/dellingr/src/vm/table.rs:1586) determines physical placement from the historical `vlen`. Positive integer keys beyond the next append position use hash storage even when they form a dense range. Filling `2..N` before writing `1` builds a dictionary, then incurs a migration pass. [append_growth](/home/folk/Programs/dellingr/src/vm/table.rs:1740) probes the run to reserve it; [migrate_from_hash](/home/folk/Programs/dellingr/src/vm/table.rs:1943) probes it again while removing and copying its values.

   **The redesign:** keep `vlen` as semantic history, but let numeric payloads use storage chosen independently of it.

   I would retain contiguous storage for dense sequences and add segmented storage with occupancy metadata for sparse ranges. Integer entries can physically occupy numeric storage before they belong to the logical array. Their iteration position remains in the logical hash-order history until the existing append rules advance `vlen`.

   Migration then changes logical membership and leaves the required historical tombstones; it does not need to move every payload from hash entries into a Vec. Occupancy metadata can discover adjacent live numeric keys without repeated generic hashing.

   For sparse historical arrays, iteration skips empty regions and GC visits allocated regions instead of walking every nil between distant live values.

   **Why it could pay:** it removes hash construction and migration for dense but out-of-order numeric population, and prevents repeated traversal and storage costs from tracking historical span rather than useful contents.

   **The risks are substantial.** Physical numeric placement must never determine `pairs` order. A key beyond `vlen` still belongs at its historical hash-order position, even if its value lives in a numeric page.

   Likewise, `#t` must retain the current exact answer. [array_len](/home/folk/Programs/dellingr/src/vm/table.rs:1565) uses a particular binary search when the logical tail is nil. Replacing it with highest-live-key tracking, cardinality, or another valid Lua border would change results.

   Snapshot encoding must reconstruct the current canonical array span, including interior nils, and preserve logical hash history. Sparse physical storage does not authorize smaller snapshot contents.

   Finally, segmented access adds machinery that can slow dense reads. Keeping a direct contiguous form is essential. I have high confidence in the identified pathologies, but medium confidence in their importance to aggregate game throughput without workload measurements.

There are also several worthwhile local changes. These should stay distinct from the rewrites:

- **Track live hash keys that can actually migrate.** This is the strongest local opportunity. [append](/home/folk/Programs/dellingr/src/vm/table.rs:1785) takes its slow path whenever *any* hash entry is live. A string field therefore forces every append through migration-related checks. Typically, `append_growth` performs an unsuccessful numeric lookup and `migrate_from_hash` performs another unsuccessful lookup. A counter for eligible positive integral hash keys would let mixed tables append without this work. [list_hash_cannot_migrate](/home/folk/Programs/dellingr/src/vm/table.rs:1985) currently substitutes a bounded scan, which also rejects bulk extension when the hash slot count exceeds the remaining batch size. Maintain the counter through insertion, deletion, initialization, migration, and snapshot loading.

- **Return nil directly for logical array keys outside the window.** [get](/home/folk/Programs/dellingr/src/vm/table.rs:866) and `get_with_index` fall through to the hash part when `array_slot` fails. For an integer in `1..=vlen` outside the physical window, the answer is already known: no live hash entry can exist there. This is targeted at missing reads in trimmed arrays. The extra classification should stay off the common successful array path.

- **Reserve fixed positional constructors across batches.** `reg_set_list` already reads the VM stack directly, and [set_list](/home/folk/Programs/dellingr/src/vm/table.rs:2243) already bulk-extends plain batches. There is no temporary list materialization to eliminate. The remaining opportunity is carrying a fixed constructor's total positional capacity to avoid repeated growth across batches. Reservation must leave writes, expression evaluation, logical growth, and cost admission at their existing operations.

- **Cache the exact computed border between occupancy changes.** Repeated length queries on an unchanged holey array repeat the binary search. A lazy cache invalidated by nil/non-nil transitions and logical growth can preserve the exact answer. Dense-tail length is already constant time, so this is a secondary opportunity.

Two findings particularly constrain implementation.

**Iteration is already better than a superficial reading suggests.** Generic-for has direct `next` and `ipairs` paths in [eval_control.rs](/home/folk/Programs/dellingr/src/vm/eval_control.rs:165). The pairs cursor avoids resolving the previous key on every step. IndexMap's iterator forwards `nth` to a slice iterator, so `.skip(hash_start)` does not rescan the prefix. The remaining iteration costs are hole scanning, tombstone scanning, and entry footprint.

**Snapshots expose representation history.** [snapshot_hash_slots](/home/folk/Programs/dellingr/src/vm/table.rs:1287) saves dead controls, vacant positions, and a map-storage marker. The snapshot tests also establish that a dead key remains a strong GC reference. Preserving live values and current pairs order is insufficient: each rewrite needs to preserve the old logical slot history and its transitions.

One robustness inconsistency is worth flagging: array reservations translate failure into `TableError::GrowthFailed`, while hash creation and growth use infallible IndexMap allocation APIs. The fallible table-growth boundary is therefore asymmetric.

My implementation priority would be the shared-record rewrite, with the eligible-numeric-key counter as an independently valuable local change. Dictionary history separation comes next for mutation-heavy workloads. Adaptive numeric storage is the larger response to sparse and out-of-order tables. All three can preserve existing opcode charges and admission points; none requires changing Lua call nesting or introducing callback re-entry.
