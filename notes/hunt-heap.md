The biggest opportunity is a coherent rewrite of the value and heap layer. Compact values, typed object arenas, and collector metadata should be designed together. That has broader throughput potential than tuning individual allocation functions because it affects register traffic, table contents, object access, closure creation, and collection.

My next priorities would be string construction and closure ownership. Both currently introduce allocations and materialization that the VM's existing lifetime guarantees could eliminate.

These are source-based hypotheses, not measured speedups. I read source and dependency implementations only; I made no changes and ran no builds, tests, or benchmarks.

1. **Highest conviction, full architectural rewrite: compact values backed by typed arenas.**

   [`Val`](/home/folk/Programs/dellingr/src/vm/lua_val.rs:105) combines a discriminant with an eight-byte number or generational handle. Its expected size on a 64-bit target is 16 bytes; I did not measure its layout. That representation is copied throughout dispatch, register windows, table storage, varargs, and returns.

   Objects then go through a second layer of classification. `Val::Obj` identifies neither a table nor a closure. Access resolves the slotmap key and examines `RawObject`. Even `Val::typ` must dereference the heap for those values.

   The storage cost is particularly poor for closures: [`RawObject`](/home/folk/Programs/dellingr/src/vm/object.rs:224) reserves table-sized slots while storing closures behind a separate `Box`. Every closure therefore occupies a large arena slot plus its actual allocation.

   I would replace this with:

   - An eight-byte tagged value, using reserved NaN encodings for nonnumeric values.
   - Distinct table, closure, and string tags.
   - Separate typed arenas, with compact indices addressing their storage directly.
   - A structural distinction between strong values and weak handles. Strong values refer to objects kept alive by the root graph; weak caches carry generation metadata and validate it before producing a strong value.
   - Separate allocation and mark metadata, organized into pages with occupancy bitmaps.

   This avoids trying to squeeze the existing 64-bit slotmap key into a smaller payload. Generation protection remains necessary for weak references; it need not travel through every live register and table element.

   The collector should use those same arenas. The current [`sweep`](/home/folk/Programs/dellingr/src/vm/object.rs:771) calls `SlotMap::retain`; the dependency implementation iterates every historical slot, including vacant ones. Upvalue sweep similarly visits every slot ever created. A temporary allocation peak can therefore increase subsequent collection work indefinitely.

   Typed pages can instead enumerate occupied slots from compact metadata, sweep `occupied & !marked`, and omit empty payload pages from traversal. Marking can test a side bitmap before fetching an object's payload, eliminating the current object lookup to color it followed by another lookup when draining the worklist.

   **Why this is plausibly the largest win:** it reduces ubiquitous value traffic, removes closure slot waste, eliminates object-kind discovery through the heap, and makes collection operate over compact metadata. It helps scripts that allocate little as well as allocation-heavy ticks.

   **Risks:** this is a pervasive representation rewrite. Weak caches, builtin mirrors, identity maps, and snapshot traversal all need explicit treatment. NaN boxing also requires care: exact host-supplied NaN payloads cannot all coexist with tags in one word without an escape representation. Preserve those exceptional numeric encodings if required by the stated contract. Do not truncate generations or introduce a smaller reachable heap limit.

2. **High conviction, full construction-path rewrite: make string production feed final string storage.**

   The current ownership boundary throws away useful ownership.

   [`concat_values`](/home/folk/Programs/dellingr/src/vm/eval.rs:771) computes the output length, allocates a `Vec`, and fills it. It then passes a borrowed slice to `alloc_string`. On an interning miss, [`alloc_string_raw`](/home/folk/Programs/dellingr/src/vm/object.rs:640) allocates a separate boxed byte buffer and copies the completed output again.

   The same pattern appears when `string.format`, substitutions, case conversion, `table.concat`, and `tostring` pass owned buffers through `push_bytes`. Its `AsRef<[u8]>` interface discards ownership before the pool can use it.

   String access also resolves a generational slot containing a header whose bytes live in another allocation.

   I would redesign string storage and construction together:

   - Producers retain ownership through interning. On a miss, their completed buffer becomes the string's backing storage.
   - Short strings use pooled storage with bytes adjacent to their header, avoiding one allocator allocation per distinct short string.
   - Large or growing results retain their buffer allocation, including capacity metadata where necessary, instead of forcing everything through exact-sized `Box<[u8]>`.
   - Rendering writes into the destination builder where possible. Concat currently creates a collection of separately allocated numeric renderings before copying them into its output.
   - Canonical identity remains separate from backing-byte ownership, allowing physical storage changes without changing interning semantics.

   The existing [`range allocator`](/home/folk/Programs/dellingr/src/vm/object.rs:650) already demonstrates a useful property: a hit copies nothing and a miss copies only the selected range. The missing capability is transferring completed output storage into the pool.

   **Why high payoff:** every newly produced string currently pays for storage that already exists elsewhere. Removing the final allocation and copy directly benefits concat, formatting, conversion, and substitution workloads. Pooling short strings addresses allocator pressure that owned-buffer adoption alone leaves behind.

   **Risks:** preserve the exact collection point and root set at commitment. Some producers clear their arguments before interning; others retain them. Changing that can change string identities. Builders that survive callbacks must also remain valid across nested execution.

   I would keep canonical interning in this rewrite. Uninterned long strings and ropes require a much larger identity and accounting redesign: current equality uses string handles, and distinct-string counts determine future collection points. Ropes alone would also leave the full-content interning hash on the construction path.

3. **High conviction, full closure-lifetime rewrite: arena-owned closure records and captures, with State-owned executable records.**

   A captured closure currently passes through several allocations and ownership systems:

   - [`reg_closure`](/home/folk/Programs/dellingr/src/vm/registers.rs:477) allocates a temporary capture `Vec`.
   - [`alloc_lua_fn_raw`](/home/folk/Programs/dellingr/src/vm/object.rs:585) converts that into an `Arc` slice.
   - The closure itself gets a `Box`.
   - The boxed closure occupies a table-sized slotmap slot.
   - The closure owns separate `Arc`s for bytecode and runtime data.

   Calling it then resolves the closure again and clones all three owned handles into a [`Frame`](/home/folk/Programs/dellingr/src/vm/eval.rs:926). Captures are already shared without copying, which is good, but reference-counted ownership remains on every call.

   The VM already has the stronger lifetime fact needed to remove this: an executing Lua closure is rooted in its function slot. Frames do not need independently owned capture lists to protect them from collection.

   I would make a closure a compact arena record containing an executable-record ID and its capture references. Capture slots would be filled directly in their final storage. Size classes can cover common capture counts, with a general representation for wider closures.

   One State-owned executable record would hold the bytecode owner, runtime literals, caches, and links to nested executable records. Closures, frames, and activation records would carry IDs. Shared immutable `Program` bytecode can retain its outer `Arc` ownership without replicating ownership operations through every closure and call.

   This also removes a collection seam. Today, collection gathers active chunk identities into a newly allocated `BTreeSet`, recursively retains prototype trees, flags runtimes while tracing closures, then scans the runtime map to reconcile those sources. See [`LiveChunks`](/home/folk/Programs/dellingr/src/vm/object/collector.rs:76) and [`sweep_bytecode_caches`](/home/folk/Programs/dellingr/src/vm.rs:958).

   Executable records can carry collection epochs and direct child IDs. Active frames, pending construction, and traced closures mark those records directly, with an iterative work queue and no temporary ordered identity set.

   **Why high payoff:** this removes multiple per-closure allocations and atomic ownership operations from frequently called functions, while simplifying the connection between closure reachability and executable lifetime.

   **Risks:** retain capture aliasing exactly, including sibling closures sharing a mutable cell, scope exits, tail calls, and error cleanup. Snapshot serialization explicitly records shared upvalue identities, so copying apparently immutable captures by value would require additional work to reproduce the same saved graph.

   The existing one-collection grace for literals belonging to discarded runtime records must also survive. Removing it changes weak string identity behavior and allocation counts.

   Afterwards, if this works, compiler-proven immutable captures are a separate opportunity. They should not constrain this rewrite.

4. **Medium conviction, architectural collector change: trace reference-bearing storage instead of inspecting every value.**

   Even after fixing sweep, marking can dominate a State with large persistent tables.

   [`Table::mark_values`](/home/folk/Programs/dellingr/src/vm/table.rs:2590) scans every array value after the dead-prefix hint and every stored hash key/value. Numeric and boolean arrays incur a complete scan despite containing no heap edges. The collection threshold counts objects and distinct strings, not table payload, so a large scalar array can be repeatedly scanned as unrelated allocations trigger collection.

   I would add exact reference summaries to table storage: a count that skips an entirely reference-free region, plus reference bitmaps for mixed large arrays. The collector would visit only positions containing strings or objects.

   This needs integration with every write route, including cached stores, bulk construction, shifts, sort writeback, and snapshot loading. Existing mutation accounting such as [`write_window`](/home/folk/Programs/dellingr/src/vm/table.rs:1682) provides useful integration points, but the current paths are not sufficiently unified to make this a collector-only patch.

   **Why potentially material:** it changes marking work from inspecting all scalar payload to following actual references. That is attractive for persistent numeric game state.

   **Risks:** bookkeeping is paid on mutation, so highly mutable, reference-rich tables may lose throughput. Missing one update is a collection correctness bug. Tombstone keys remain strong edges and must stay represented. This is workload-dependent, with lower conviction than the first three rewrites.

Several smaller changes are worth doing, but they are medium-value local work:

| Change | Concrete opportunity |
|---|---|
| Fuse interner lookup and insertion | `HashTable::entry` is available. Current creation performs `find_by_hash` followed by `insert_unique`, probing again on misses. |
| Sweep strings through their content index | Current string collection allocates a removed-key vector, sweeps slots, then hashes each removed entry back into the index. `HashTable::retain` or `extract_if` can coordinate removal directly. |
| Trace each shared upvalue's contents once | `mark_slot` does not report whether a cell was already marked, so every closure sharing it rereads and marks its closed value. |
| Reuse collection scratch | The object worklist is reused; chunk-retention bookkeeping and the string removal vector are freshly allocated. |
| Retain anchor release-buffer capacity | `Registry::drain_released` takes and consumes the queued `Vec`, discarding its capacity. This matters with anchor churn, not ordinary value execution. |

I would not put anchor locking near the top of this investigation. Values are `Copy`, and normal instructions do not acquire that mutex. The more consequential ownership overhead is closure and frame ownership.

There are important observable behaviors that these changes must preserve:

- **An intern hit can currently trigger collection.** [`State::allocator`](/home/folk/Programs/dellingr/src/vm/object/collector.rs:91) checks the threshold before searching the pool. Looking up first and returning immediately on a hit can preserve an otherwise dead string and its `%p` identity.
- **Physical retention must not preserve logical string identity.** A nursery that leaves unreachable old strings in the interner can resurrect their identities. Deterministic nursery scheduling alone does not solve this.
- **Current roots include all retained register values and all cached runtime literals.** Removing apparently dead registers or dropping literals sooner can change collection results. Optimizing traversal while preserving that root set is straightforward; redefining liveness is a separate semantic change.
- **The exhaustive root inventory is useful.** [`for_each_gc_root`](/home/folk/Programs/dellingr/src/vm/inventory.rs:321) uses a generic visitor, not dynamic dispatch. Its abstraction is not the bottleneck; the volume of storage traversed and the separate lifetime reconciliation are.

For these reasons, I would start with the compact typed heap and faster exact full collection. An ordinary generational or incremental collector is not my leading throughput recommendation under this contract. Preserving the same reclamation decisions, weak identities, and next threshold requires more than adding a nursery or distributing marking work.

One contract discrepancy surfaced outside the performance ranking: raw NaN return bits have a weaker guarantee in the code than the question specifies. Arithmetic returns native `f64` results, and [`to_number`](/home/folk/Programs/dellingr/src/vm/stack.rs:323) exposes them directly. Snapshots canonicalize NaNs, while the replay tests [explicitly accept any NaN payload](/home/folk/Programs/dellingr/tests/integration/library/replay_goldens.rs:70). If bit-identical returned values includes NaN sign and payload, that guarantee is not currently established. A value-representation rewrite should handle this explicitly.
