The biggest opportunity is to stop executing hot standard-library functions as ordinary Rust callbacks. The second is a full rewrite of sort execution and buffer ownership. I would pursue those before replacing the deterministic math kernels.

These are conclusions from source inspection, not measured speedups. I read the scoped implementations and their supporting execution, storage, GC, formatting and test code. I did not build, run, modify files, or read project Markdown or history.

1. **Highest priority: make hot library functions native operations of the interpreter. Full architectural rewrite; high conviction.**

   Consider [`math.floor`](/home/folk/Programs/dellingr/src/lua_std/math.rs:127). Its useful computation is `x.floor()`. Around that, the implementation charges cost, checks the argument type through the auxiliary API, reads the argument through another checked API, clears the callback stack, and pushes a checked result.

   Before and after that wrapper, [`begin_call`](/home/folk/Programs/dellingr/src/vm/eval.rs:438) removes the function slot and relocates arguments, while [`invoke_rust_fn`](/home/folk/Programs/dellingr/src/vm/eval.rs:660) handles callback bookkeeping, panic containment, floating-point environment validation and result processing. The dispatch loop also flushes its local cost before calling out.

   That machinery is reasonable for arbitrary host code. It is structurally disproportionate for `floor`, `sqrt`, `abs`, `type`, `rawequal`, and ordinary numeric `tonumber`. `math.random` and append-form `table.insert` also have small enough useful bodies for this overhead to matter.

   **The stronger design:** identify the actual library function value at call execution and execute its operation directly against the Lua register window. Read validated operands once; write results directly into their destination; keep cost accounting inside the running interpreter.

   I would start with identity-based handling inside call dispatch, covering aliases such as `local floor = math.floor`. Dedicated guarded instructions can then eliminate more operand movement and dispatch work at suitable call sites. This crosses into dispatch and codegen, and potentially bytecode verification and static cost analysis. It does not require a redesign of the generic host callback ABI, which belongs to the calls scope.

   Function identity must be checked after evaluating the callee and arguments. Recognizing the spelling `math.floor` is insufficient: libraries and globals are mutable, and argument evaluation can itself mutate them.

   The initial set should be:

   - Unary arithmetic: `floor`, `ceil`, `sqrt`, `abs`, `deg`, `rad`.
   - Small fixed-arity operations: `fmod`, `modf`, common `min`/`max` forms.
   - `random`, preserving the existing generator and range mapping.
   - `type`, `rawequal`, numeric `tonumber`, `rawlen`, `rawget`, `rawset`.
   - Append-form `table.insert`, then ordinary `remove`, `select`, `pairs` and `ipairs` construction.

   **Why this is plausibly the largest payoff:** it removes an entire execution boundary from millions of calls whose useful work is tiny. Optimizing the arithmetic inside these wrappers cannot compete with removing the wrapper and handoffs.

   **Main risks:** exact error construction and attribution, method-call argument numbering, extra and missing arguments, variable result counts, tail calls, stack-cap failures and charge ordering. For example, math wrappers charge before validating arguments. An intrinsic must preserve that ordering and must not combine charges in a way that moves a budget failure to another operation.

   Removing physical temporary slots also requires preserving any corresponding observable stack-cap refusal. Keep interrupt admission checks and validate the floating-point environment whenever control returns from host code. Internal specialization must preserve existing function identities and snapshot encoding.

2. **Second priority: replace sort's ownership and execution model. Full coherent rewrite; high conviction.**

   Sort has two substantial bottlenecks, both visible in [`table_sort`](/home/folk/Programs/dellingr/src/vm/table_ops.rs:779).

   First, a raw comparator sort creates three additional value copies: the detached sorting array, a temporary roots vector, and the copy appended to `in_flight.rooted_values`. The roots exist because the mutable sorting buffer is outside the collector's ownership model.

   Second, every comparison pushes a call window and enters [`call_unguarded`](/home/folk/Programs/dellingr/src/vm/eval.rs:182). For Lua comparators, that starts a nested invocation, obtains a separate frame stack, dispatches the comparator, collects its return and restores the boundary. This happens throughout the `O(n log n)` comparison sequence.

   The metamethod path has another expensive representation seam: [`sort_through_metamethods`](/home/folk/Programs/dellingr/src/lua_std/table.rs:662) materializes a scratch Lua table. Heap sort then performs its internal accesses through table lookup and insertion, although the scratch data is a private indexed buffer.

   **The stronger design:** make a sort activation own one GC-traced mutable value buffer and an explicit heap-sort continuation.

   - Raw input is copied once into that buffer.
   - Metamethod input is read, in the existing order, directly into it.
   - Comparisons against Lua functions suspend the sort activation and push the comparator onto the current dispatch loop's frame stack.
   - Comparator return resumes the saved sift decision.
   - Successful completion performs the existing raw or metamethod writeback.

   This removes the scratch table, duplicate root copies, repeated nested invocation boundaries and the accessor structure imposed by supporting both vectors and scratch tables. Nested sorts need separate activation-owned buffers. A live Rust slice borrow must not survive a comparator call.

   Afterwards, if this works, other library callbacks could use the same continuation mechanism. That generalization should follow the sort rewrite, not dictate its design.

   **Why high payoff:** collection sorting each tick multiplies both comparison-entry overhead and buffer traffic. This redesign attacks both. It also lets the collector trace the actual mutable owner instead of requiring a second representation solely for rooting.

   **Preserve the existing heap-sort decision sequence.** A user comparator can print, mutate tables, draw randomness, fail or return inconsistent answers. Changing comparison order changes the program. Preserve argument order, comparator truthiness, the read phase and the write phase.

   Raw sorts must also retain detached semantics: comparator writes to the source may be overwritten at final writeback, while entries beyond the original range survive. Sorting the source directly during callbacks would change observable behavior.

   **Main risks:** GC rooting through nested callbacks, error unwinding, traceback attribution, partial metamethod writes, stack admission, and existing depth errors. Flattening physical execution must not silently remove an existing script-visible native-depth refusal; equivalent logical accounting may still be necessary. Sort state belongs in execution-owned storage, keeping Lua-to-Lua native stack usage flat.

3. **Give ordinary default sorts a separate storage-native implementation. Targeted structural change; high conviction.**

   The no-comparator path cannot call Lua or collect during comparisons, yet it still clones through [`get_array`](/home/folk/Programs/dellingr/src/vm/table.rs:2423) and writes every element back through general `Table::insert` in [`set_array`](/home/folk/Programs/dellingr/src/vm/table.rs:2439).

   For a homogeneous sequence of numbers or strings, that staging is unnecessary. Split the table-storage and string-pool borrows, establish that the range can be sorted without a comparison error, and run a specialized version of the same heap sort directly on its storage. Preserve the table's structural metadata, iteration behavior and snapshot shape.

   Mixed or otherwise fallible ranges should retain detached sorting. The current implementation leaves the raw source unchanged when comparison fails; an in-place sort that discovers a bad value halfway through would violate that behavior.

   **Why worthwhile:** numeric collections avoid allocation, input copying, per-comparison tag dispatch and general insertion during writeback. This requires cooperation from the table-representation scope, but does not require replacing table storage.

   **Do not equate "no comparator" with "any sort algorithm is safe."**

   - Positive and negative zero compare equal but render differently.
   - NaNs make `<` a partial ordering and their positions remain observable.
   - Mixed-type comparison errors depend on which operands are compared first.

   Changing algorithms is defensible only for a range proven to have a unique observable sorted result, such as distinct non-NaN numbers. Establishing that condition has a cost. Preserving heap-sort decisions is the stronger default.

   Interrupted in-place sorts also need deliberate treatment: the current detached path leaves raw storage untouched until writeback. That must be considered alongside the requirement to keep sort interruptible.

4. **Replace sequence-library stack choreography with direct kernels, especially concat. Coherent subsystem rewrite; high conviction about the waste, workload-dependent payoff.**

   [`table.concat`](/home/folk/Programs/dellingr/src/lua_std/table.rs:231) currently processes each element through:

   `numeric key -> stack -> general index resolution -> stack value -> owned byte vector -> output vector`

   String elements are copied into temporary vectors before being copied into the joined result. The finished vector then goes through `push_bytes`, whose borrowed-byte interning API copies it again on an intern miss.

   There is already a useful precedent in [`string_arg.rs`](/home/folk/Programs/dellingr/src/vm/string_arg.rs): rooted strings can be borrowed where they lie, with field splitting allowing other state to remain usable. Concat is not taking advantage of that approach.

   **The stronger design:** implement concat as a sequence reader feeding an output builder.

   - For ordinary tables, read values directly without synthesizing stack keys.
   - Append string bytes directly from the pool.
   - Render numbers directly into the destination or a bounded formatting buffer.
   - Intern the completed owned buffer by transferring ownership on a miss.
   - For metamethod reads, retain the current sequential resolution order, then append each returned value without an intermediate owned string copy.

   This crosses into the strings scope for owned-buffer interning and formatting. Build concat around its actual needs first.

   Do not bulk-prefetch metamethod results or reorder validation through a sizing pass. `__len`, `__index`, invalid-element errors and output-size errors have observable order. Concat must still evaluate length when an explicit end is supplied, and admit its charge at the current point.

   The same direct-kernel approach applies elsewhere:

   - **Unpack:** ordinary ranges can read directly into return slots instead of pushing a key and invoking general lookup for every result.
   - **Pack:** the existing `Table::set_list` capability can replace per-argument key/value pushes and raw stores, while preserving positional nils and the `"n"` field.
   - **Insert/remove:** validate the table, length and position once, then perform the storage operation once.

   Append insert currently computes the border in the library, validates it again in the host-facing method, and computes it again in `array_insert`. Its actual append already uses the table's storage machinery. Repeated validation and call seams are the opportunity; a new shifting algorithm is not.

   **Main risks:** stack-cap error ordering, metatable mutation during handlers, GC rooting and exact partial effects. A fast ordinary-table path should use event-specific conditions: `__newindex` need not disqualify a read-only operation, while handlers that can mutate metatables require re-evaluation at the appropriate boundaries.

5. **For math itself, keep the current exact kernels initially. A replacement fast phase is a lower-conviction architectural opportunity.**

   The kernels already have the important structure: cheap approximation, rounding certification and accurate fallback. Hardware FMA is selected once at kernel entry, and the selected helpers can inline within that kernel. There is no per-FMA runtime selection bottleneck to remove.

   `sqrt`, `floor`, `ceil` and `abs` have little algorithmic headroom. Their major opportunity is item 1.

   The more credible mathematical rewrite is a cheaper **certified fast phase** for common game-domain transcendental inputs, retaining the existing kernels as fallback. For example, cosine currently performs range reduction and double-double reconstruction through [`cos_fast`](/home/folk/Programs/dellingr/src/crmath/cos.rs:75). A cheaper approximation is useful only if a sound error interval certifies the final binary64 answer often enough to repay its work.

   This would be a numerical algorithm rewrite, with new bounds and targeted certification work. It cannot be justified by visual accuracy or a small ULP error.

   A joint sine/cosine kernel is another coherent possibility where scripts repeatedly compute both for the same angle: share reduction and reconstruction, then certify each output separately. Compiler fusion would need to preserve function identity checks, evaluation order and both original charge points. It cannot blindly compute both results before the second call is admitted.

   **Why lower priority:** these kernels already avoid expensive accurate phases when certification succeeds. There is no comparable obvious materialization seam to remove. Payoff depends on actual input distributions and transcendental usage. Rewriting high-precision fallback arithmetic first would optimize uncommon work.

6. **Medium-value local changes: exact RNG range mapping and rendering.**

   In [`random_range_i64`](/home/folk/Programs/dellingr/src/vm/rng.rs:57), each nondegenerate range recomputes `2^64 mod span`, then maps accepted draws with `raw % span`. Repeated game ranges make cached span metadata or exact reciprocal-based remainder computation plausible improvements.

   Preserve the exact rejection predicate and exact remainder. A multiply-high range mapper generally changes which draws are accepted and which results they produce, so it violates the required stream. Singleton ranges currently consume no draw. Preserve that too.

   This is a local arithmetic optimization, potentially material for random-heavy scripts, not a reason to redesign SplitMix64 or batch draws ahead of execution.

   Rendering has another concrete seam. [`format_g14`](/home/folk/Programs/dellingr/src/vm/lua_val.rs:69) first formats scientific notation to determine the decimal exponent, then often formats the number again in fixed notation, allocating intermediate strings. A single conversion into a bounded buffer with exact `%.14g` selection and trimming would benefit numeric `tostring` and concat. The existing integral fast path already handles many game values, so the payoff is concentrated in nonintegral rendering.

   [`tostring_rendering`](/home/folk/Programs/dellingr/src/vm/table_ops.rs:1013) can also copy a string returned by `__tostring` into bytes and then re-intern it. Returning the rooted string value directly would remove that round trip.

   Keep negative zero, rounding carries, exponent spelling, non-UTF-8 bytes and pointer-identity assignment order exact. Avoid permanent caches that introduce new strong roots merely to accelerate `type` or `tostring`; those can affect collection and snapshot state.

Two areas are already ahead of the obvious suggestions. Generic `for` recognizes the catalog identities of `next` and the ipairs iterator and executes them directly; `pairs` traversal also has an indexed cursor cache. Those loop bodies are not fresh callback-elimination opportunities. `select` already removes its prefix in one bulk move, so repeated removal is not a remaining quadratic problem.

I found two correctness concerns worth flagging separately:

- **RNG rejection sampling has no interrupt poll.** Its data-dependent retry loop neither polls nor receives an interrupt handle. Expected retries are small, but that is insufficient for the host-stoppable contract. This needs a safepoint within rejection processing.
- **Unconditional correct rounding is not fully established by the final fallback paths.** [`atan2_accurate`](/home/folk/Programs/dellingr/src/crmath/atan2.rs:417) discards its final error bound and rounds the approximation. [`pow`](/home/folk/Programs/dellingr/src/crmath/pow.rs:890) also returns its approximation when the final boundary test remains unresolved. I found no concrete failing input; these are explicit certification gaps. Any correction producing different bits would need to be treated as a replay-visible change.

My implementation order would be direct library execution, then activation-owned resumable sorting, then direct sequence kernels. Those remove substantial work imposed by the current architecture. Replacing correctly-rounded transcendental kernels should follow only when those structural costs are gone and the remaining kernel work warrants it.
