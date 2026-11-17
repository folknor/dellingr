**The strongest opportunity is to replace instance-specific lookup caches with shared-layout access plans, including resolved routes through metatables.** The current design accelerates repeated access to one object, but leaves substantial work in the entity loops you care about.

The first two recommendations below form one coherent architectural rewrite. Global binding storage is a separate, lower-confidence rewrite. These are source-based payoff hypotheses; I made no changes and ran no builds, tests or measurements.

1. **Cache layouts across receivers, rather than individual table identities. Full rewrite; highest conviction.**

   Own-field entries contain exactly one `table: ObjectPtr` and one `SlotRef`. Reads and writes reject the entry whenever the next receiver is a different table. Caches belong to the bytecode runtime, so different closures and every entity executing the same method share that single entry. See [caches.rs](/home/folk/Programs/dellingr/src/vm/caches.rs:158), [eval_index.rs](/home/folk/Programs/dellingr/src/vm/eval_index.rs:76) and [eval_store.rs](/home/folk/Programs/dellingr/src/vm/eval_store.rs:78).

   Consequently, this ordinary pattern defeats the own-field cache even when every entity has the same layout:

   ```lua
   function update(entity)
       entity.health = entity.health - entity.damage
   end

   for i = 1, #entities do
       update(entities[i])
   end
   ```

   Each access site remembers the preceding entity. The next entity falls into keyed lookup, and reads enter the outlined resolver. Increasing the number of cached *objects* only postpones this failure.

   **The redesign:** make the common access plan describe a layout and a slot:

   - Guard the receiver's shared layout identity.
   - Load or update the designated live slot.
   - Resolve generically only when the layout differs or the slot's current nil status requires metamethod handling.

   A small polymorphic cache should distinguish layouts, with a shared lookup structure for sites seeing many layouts. It should not grow into a linear list of entity identities.

   The table-representation scope needs to supply shared layout identities and slots whose meaning is guaranteed by those identities. Its current `version` is insufficient: it is a per-table slot-stability counter, starts identically on unrelated tables, and does not change on ordinary additions. Two tables with equal versions need not have remotely similar layouts. See [table.rs](/home/folk/Programs/dellingr/src/vm/table.rs:445).

   Constructor templates are an existing source of layout knowledge. They already describe ordered field names and initialize pinned positions, but lookup does not exploit their common origin across instances. The stronger design would recognize equivalent layouts beyond a single constructor site, including layouts reached by ordinary mutations. See [with_template_keys](/home/folk/Programs/dellingr/src/vm/table.rs:625).

   **Why this is plausibly the biggest win:** it removes keyed lookup and resolver entry from normal reads and updates across entire entity populations. The improvement scales with executed accesses, rather than object construction or cache allocation.

   **Risks:** layout proliferation, mutation overhead and invalid layout assumptions. Nil needs particular care. A layout can prove that a field has no slot, but a reserved slot containing nil needs a live nil check unless presence changes also change the guarded identity. Cache metadata must remain weak and must not retain otherwise dead objects or strings.

2. **Cache the complete lookup route, with mutation-driven invalidation. Full rewrite; high conviction, especially for methods and inheritance.**

   A warm inherited read currently still does considerable dependent work:

   - Fail the receiver's own-field cache.
   - Search the receiver's raw contents.
   - Obtain its metatable.
   - Search the two method-cache entries.
   - Read and key-check the metatable's `__index` slot.
   - Compare its current handler.
   - Validate and read the destination method slot.

   Only the final own-field hit is handled by the dispatch-side fast path. Inherited hits enter `resolve_index`. The method cache covers a raw hit in the first handler table; deeper inheritance and inherited misses keep walking. Delegated writes have no corresponding resolved-route cache. See [reg_get_field](/home/folk/Programs/dellingr/src/vm/registers.rs:117), [cached_method_at](/home/folk/Programs/dellingr/src/vm/eval_index.rs:135) and [walk_index](/home/folk/Programs/dellingr/src/vm/metamethod.rs:164).

   **The redesign:** replace the separate own-field, method and string-method ladders with access plans that describe the resolved action. Examples include:

   - Read an own slot.
   - Read a slot in a particular prototype.
   - Return nil after a proven terminal miss.
   - Update a live slot in a delegated destination.
   - Continue to a function handler with the correct receiver and depth requirement.

   For inherited access, the receiver guard must prove both the applicable metatable and the absence of an overriding own value. Shared layouts make that proof inexpensive. A reserved own slot may require one nil check.

   Register dependencies on the shared prototype route when constructing the plan. Relevant mutations invalidate the plan immediately: installing an overriding field, deleting a terminal field, replacing `__index` or `__newindex`, changing an intermediate metatable, or moving a referenced slot. The hot read should check a route-validity token and access the terminal slot, rather than revalidate every intermediate table.

   This should move work toward mutations of shared prototypes, which are plausibly much rarer than entity reads. It should **not** use one VM-wide mutation epoch: routine entity writes would destroy the benefit. Ordinary receivers should be guarded through their layouts and current slot values, without registering dependencies on every entity.

   Metamethod representation belongs inside this redesign. [MetaNames](/home/folk/Programs/dellingr/src/vm/meta_name.rs:62) already avoids interning event names on each lookup, but event access remains ordinary table lookup or key-checked slot access. Metatables should expose direct event descriptors, including explicit absence and mutation notification. Tables first used as metatables can acquire this metadata lazily. The string metatable can use the same mechanism instead of its separate validation path.

   The mutation contract must live beneath all writers. Bytecode stores, raw operations, host writes and pinned initialization cannot each remember to invalidate independently. This requires changing the table mutation interface, not adding another cache layer around the existing callers.

   **Why the payoff is high:** for `self:update()` and inherited properties, it removes the repeated receiver search and most of the serial pointer chasing. It also makes multi-level inheritance, optional inherited fields and delegated writes eligible for acceleration.

   **Risks:** invalidation completeness and excessive dependency fanout. Live-to-live replacement of a terminal value can remain cheap if the plan reads its slot live; changing presence or routing must invalidate. Function handler *results* must never be cached.

   Depth behavior must remain explicit in the plan. Skipping two intermediate lookups does not erase their metamethod-depth requirements. A valid plan can use its precomputed requirement when sufficient depth remains; insufficient headroom can take the original resolver to preserve the exact error and precedence. Handler invocation remains the calls scope's responsibility.

   I would implement recommendations 1 and 2 as the architectural experiment, benchmark the complete change, then keep or revert it. Adding more special cases to the present method cache has a substantially lower ceiling.

3. **Make global bindings directly addressable authoritative cells. Separate full rewrite; medium conviction.**

   Ordinary globals use per-bytecode probes into the global table. Builtins add a value mirror validated by a single heap mutation epoch. That epoch advances on generic mutable access to `_G`, even when the operation does not change a builtin. Cached non-builtin global writes have a special bypass. Every ordinary heap mutation also encounters the watched-object comparison. See [global_env.rs](/home/folk/Programs/dellingr/src/vm/global_env.rs:317) and [GcHeap::get_mut](/home/folk/Programs/dellingr/src/vm/object.rs:498).

   This is a coherence structure imposed by storing the authoritative value inside a movable table layout while wanting direct builtin access elsewhere.

   **The redesign:** assign referenced global names State-local binding IDs during runtime linking. Store their authoritative values in a dense active-environment array. Bytecode accesses use those IDs directly. `_G` lookup maps names to the same cells; it does not maintain another copy.

   `_G` still needs its current numeric entries, metatable behavior, insertion history, tombstones and sealing semantics. Those determine enumeration and serialization. Reserving a binding ID for an absent name must not create a logical table entry.

   Restricted environments would exchange the active binding values and logical table contents while preserving `_G` identity. The binding-ID namespace must remain coherent across both directions of that exchange.

   This requires breaking the context-free table access abstraction for global storage, or replacing it with a heap-aware storage view. That is a justified part of the rewrite. Maintaining another array mirror would preserve most of the current problem.

   **Payoff:** arbitrary game globals get direct indexed reads, builtin reads lose mirror validation, and builtin coherence no longer requires a watched-object check throughout generic heap mutation.

   **Why I rank it below the object rewrite:** the current builtin hit is already an epoch comparison and array read. Ordinary global hits already avoid hashing. A new cell representation could lose through added indirection or more expensive generic `_G` operations. The strongest case is global-heavy game scripts, rather than builtin-heavy loops alone.

   `src/globals.rs` is source-occurrence metadata, not a runtime lookup stage. Optimizing that inventory would not address this cost.

4. **The strongest local changes are useful, but have narrower ceilings.**

   **Reuse key-checked positions across different receivers.** The table already exposes `find_keyed` and `value_if_key`, yet own-field caches insist on table identity. A cached position can be tried against another receiver by checking that its slot actually contains the site's interned key. Similar entity layouts would then avoid hashing without requiring shared shapes. Writes need a corresponding checked update operation. See [table.rs](/home/folk/Programs/dellingr/src/vm/table.rs:1190).

   This is my highest-value local hypothesis. Its guard is weaker than a layout guard and reads the key on every hit, but it attacks the actual cross-entity failure. It must use the key-checked operation; applying a `SlotRef` to another table because its version happens to match would be unsound.

   **Carry lookup results through stores instead of searching again.** On an absent field, `store_field` searches, `store_new_key` eventually inserts, and `store_field` searches again to fill the cache. In `walk_newindex`, a terminal existing entry is read with `get`, then written through `insert`, which locates it again. See [store_field](/home/folk/Programs/dellingr/src/vm/metamethod.rs:366) and [walk_newindex](/home/folk/Programs/dellingr/src/vm/metamethod.rs:473).

   A table operation should return a mutation result containing the resulting slot. Existing-slot resolution should carry that slot directly into the write. An absent insertion position can survive the no-callback path, but cannot be retained across user code that may mutate the table. This removes real stage seams, although it primarily benefits misses, insertions and delegated writes.

   **Give indexed string accesses the same specialization opportunities.** `t["health"]` is parsed and emitted as indexed access, and `GET_TABLE` has no lookup cache. A stable runtime string key also cannot use the field cache. See [codegen.rs](/home/folk/Programs/dellingr/src/compiler/codegen.rs:1065) and [reg_get_table](/home/folk/Programs/dellingr/src/vm/registers.rs:169).

   Literal string indexing can use a field access plan; dynamic sites can guard their current interned string key and reuse a layout plan. Numeric array access should retain its direct path. This has meaningful value for data-driven game code, but should not burden fast numeric indexing.

   **Remove the static cache coverage cliff.** Cache assignment stops after 255 entries per family, in instruction order. A hot site later in a large function can therefore stay uncached regardless of execution frequency. Runtime access metadata can be indexed independently of the serialized cache operand. See [assign_cache_slots](/home/folk/Programs/dellingr/src/compiler.rs:100).

There are several contract details that materially shape these designs.

**Snapshot compatibility is stricter than preserving table values and `pairs` order.** The current serializer includes dead slots, vacant positions and an Inline/Map storage marker. A replacement representation must reproduce that history in its serialization view. Likewise, changing emitted lookup instructions changes saved bytecode. Runtime specialization can retain the original serialized instructions and source mapping while executing different access plans. See [snapshot_hash_slots](/home/folk/Programs/dellingr/src/vm/table.rs:1287).

Method lookup timing also matters. The compiler emits `SELF` before evaluating explicit arguments. In `obj:m(change_methods())`, the lookup has already selected its value before `change_methods` runs. A redesigned method path must preserve that ordering and the original receiver. See [method_call](/home/folk/Programs/dellingr/src/compiler/codegen.rs:1130).

All access plans must preserve existing charges, budget-failure placement, error attribution and stack admission. A delegated store still carries the original store's charge. Cached handler routes must keep the existing call boundary and interrupt checks. Lookup dependency processing must remain bounded or poll if it introduces an unbounded loop. None of these changes requires native recursion for Lua calls.

I found two concrete testing weaknesses worth flagging:

- The two method-cache mutation tests use separate `t:f()` sites before and after mutation. They verify the results, but those particular tests do not exercise revalidation of a warmed entry. Other route tests provide overlapping coverage. See [field_cache_mutation.rs](/home/folk/Programs/dellingr/tests/integration/runtime/field_cache_mutation.rs:197).
- The cache differential compares printed output, total cost and rendered errors. It discards return values and does not compare snapshot bytes or subsequent state behavior, so it is narrower than the observable contract specified here. See [cache_differential.rs](/home/folk/Programs/dellingr/src/vm/cache_differential.rs:32).

Finally, bytecode field keys are already interned, successful reads already go directly into registers, and the common lookup path does not materialize strings or stack operands. `raw_field` does hash its host-provided name before table lookup, but that is a separate host access path. The dominant opportunity is avoiding repeated searches and repeated proofs across structurally equivalent objects.
