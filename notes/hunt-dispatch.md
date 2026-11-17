I would rewrite the hot executor around fused operations and an explicit register window. That is the strongest opportunity in this scope. The current structure repeatedly returns to instruction dispatch and materializes values between operations that could execute together.

This is a source-based ranking. I read the implementation and relevant callers, verifier, code generation, snapshot code and workload sources. I did not build, execute, inspect git history or read project Markdown. The expected gains below are hypotheses, not measurements.

| Priority | Opportunity | Classification | Conviction |
|---|---|---|---|
| 1 | Fused execution instructions with temporary elimination | Full architectural rewrite | High |
| 2 | Verifier-backed register executor with explicit slow exits | Full architectural rewrite, complementary to 1 | High |
| 3 | Smaller runtime values | Full representation rewrite | Medium, potentially large payoff |
| 4 | Threaded dispatch with guaranteed tail transfers | Full dispatch rewrite | Medium |
| Afterwards | Constant operand forms and specialized comparisons | Local instruction-set changes | High confidence, smaller ceiling |

Several useful optimizations are already present. The instruction cursor and frame base are local to the running activation. Arithmetic has separate RR/RK/KR opcodes and directly selected arithmetic functions. Conditional comparisons already fuse comparison and branch. Register helpers generally operate in place. Charge declarations resolve at compile time, and `native_call!` adds no production dispatch wrapper. These are not remaining opportunities.

1. **Make fused operations the execution unit, with intermediate values held in native locals.**

   The central bottleneck is visible in [the arithmetic handlers](/home/folk/Programs/dellingr/src/vm/frame.rs:321) and [the dispatch loop](/home/folk/Programs/dellingr/src/vm/frame.rs:350): every arithmetic instruction independently fetches a word, selects a handler, addresses operands, checks their types and writes a tagged result to `State.stack`.

   For an expression such as:

   ```lua
   sum = sum + i * 2 - 1
   ```

   the execution representation still separates multiplication, addition and subtraction. Their temporary results pass through VM registers. The compiler already relocates destinations and avoids some moves, but that cannot remove the intervening dispatches or generally forward values across an unknown next opcode.

   I would introduce a lowered execution ISA containing fused handlers for useful expression and access patterns. The example above should execute through one handler that keeps its intermediate numbers as `f64` locals, preserving the three arithmetic operations and their rounding order.

   The important extension is beyond arithmetic. Consider:

   ```lua
   total = total + entity.health
   ```

   Currently, [the field handler](/home/folk/Programs/dellingr/src/vm/registers.rs:117) writes the field value into a VM register; arithmetic subsequently reads it back. A fused field-read-and-add handler can use the existing [own-field cache probe](/home/folk/Programs/dellingr/src/vm/eval_index.rs:76), consume the returned value directly, and write only the required destination. Similar opportunities exist for field updates, indexed reads followed by arithmetic, and short arithmetic chains.

   This does not require a replacement table implementation. The code already exposes the useful lower-level primitives. The missed opportunity is composing those primitives across instruction boundaries.

   The redesign needs a real lowering pass with control-flow targets, operand dependencies, capture information and temporary liveness. Codegen should emit, or supply enough information to derive, fused execution forms. A vector of decoded micro-operations interpreted individually would retain the main bottleneck. I would use a finite family of fused handlers implemented or generated in Rust.

   The payoff is plausibly high because each fusion can remove several costs simultaneously: dispatches, instruction-pointer updates, operand decoding, temporary stores and reloads, and repeated type checks where the fused handler establishes a valid numeric path. The mixed workload sources contain arithmetic chains and field-read accumulations, so this is broader than an empty-loop optimization.

   The principal risk is preserving the exact executed prefix. A fused handler must retain each constituent operation's charge and error position. It cannot charge the whole expression upfront or reassociate its arithmetic. Guards that fail should resume the original semantics before committing effects; exits after partial execution need the correct intermediate state and original instruction position. Captured locals, allocation and metamethod calls create materialization boundaries.

   **I would make this the main rewrite.** Afterwards, if it works, broader instruction-description machinery would be a follow-up.

2. **Give the hot executor a verified register window instead of repeatedly accessing the whole mutable State.**

   The verifier establishes register, literal and jump bounds in [bytecode admission](/home/folk/Programs/dellingr/src/compiler/verify.rs:476). Execution nevertheless uses ordinary checked indexing:

   ```rust
   code[*ip]
   state.stack[base + operand]
   frame.bytecode.number_literals[index]
   frame.runtime.literals[index]
   ```

   Those are Rust bounds-checked operations. The verifier's conclusions are not expressed as compiler-visible proofs. Source alone does not establish the exact retained machine checks, but arbitrary decoded operands prevent assuming that optimization removes them all.

   More fundamentally, the same loop mixes small register operations with calls into methods receiving `&mut State`. Those methods can resize the stack, allocate, collect or re-enter Lua. That structure makes keeping stack addressing and operand state consistently resident across instructions harder.

   I would split execution into a hot register executor and a boundary driver:

   - Enter the executor with validated code, a frame-relative register window, constant bases and the narrow pieces of runtime state it needs.
   - Use admission-backed unchecked access inside that boundary, with the verification capability covering every producer, including snapshot loads.
   - Execute non-growing operations and suitable direct table fast paths through narrow borrows.
   - Return an action to the driver before stack reshaping, allocation or re-entry requires unrestricted State access.
   - Reacquire the register window after that action completes.

   This is more substantial than replacing indexing expressions with `get_unchecked`. It gives the executor an enforceable lifetime during which its addressing assumptions hold. It also provides the foundation for retaining intermediate values in fused handlers.

   The existing code helps: field probes and global-cache operations already have paths that neither collect nor re-enter. They can be composed into the hot executor without taking the complete slow-path machinery with them.

   The risks are concrete. A stack pointer cannot survive arbitrary callback execution or vector growth. A long-lived mutable register slice cannot overlap unrestricted mutable State access. Open-result producer/consumer pairs temporarily change the stack length, and the verifier explicitly permits that protocol. These need dedicated handling rather than treating every instruction boundary as a full frame window.

   Native stack layout also matters: returning from the hot executor before re-entry could reduce the native state held across nested invocations, but the boundary driver must remain small. Lua calls must continue switching heap-held frames without native recursion.

   This and opportunity 1 are complementary parts of one engine redesign. Their gains should not be added as independent estimates.

3. **Reduce the size and handling cost of runtime values, but treat this as a heap-handle rewrite.**

   [The runtime value](/home/folk/Programs/dellingr/src/vm/lua_val.rs:106) is a tagged Rust enum containing numeric values and generational handles. It is the tag-plus-payload representation carried through register loads, moves, writes, helper arguments and results.

   A one-word representation could reduce register traffic and cache footprint substantially. Unlike fusion, it would also help instructions and helper boundaries that still require materialized values.

   However, "use NaN boxing" is not a complete proposal here. Object and string values contain generational slotmap keys. Those identities do not automatically fit in a NaN-box payload. Truncating their generations would invalidate lifetime and weak-cache assumptions.

   A coherent redesign would combine compact values with a deliberate strong-reference representation: compact arena identifiers in live values, with full generation information retained where weak references and reuse validation require it. That reaches into object access, caches, roots and identity handling. An additional handle-table lookup could erase part of the operand-path saving, so the arena and access path need designing together.

   There is also a numeric complication worth flagging: snapshots canonicalize NaNs in [their numeric encoder](/home/folk/Programs/dellingr/src/vm/save_state.rs:1663), while the host's numeric stack API returns the stored `f64`. Snapshot canonicalization alone is not evidence that every numeric boundary can discard NaN payload bits.

   The payoff ceiling is large, but my confidence is below the first two opportunities because this trades simpler value movement against changed tagging, handle access and numeric handling. I would first obtain unboxed numeric intermediates inside fused handlers, then assess the remaining materialized-value cost.

   This is a full representation rewrite. A wrapper around the existing enum would not deliver it.

4. **Consider threaded dispatch after reducing the number of dispatches.**

   [The executor uses a central opcode match](/home/folk/Programs/dellingr/src/vm/frame.rs:354). A threaded design could give handlers distinct dispatch sites and potentially improve prediction of opcode transitions. An execution stream containing handler targets could also remove opcode-to-handler selection.

   That is a plausible opportunity, but source does not tell us whether branch prediction, instruction-pointer dependencies or operand traffic dominates the current machine code. Threading still dispatches once per execution instruction; fusion reduces how often dispatch is necessary.

   The coherent version requires guaranteed tail transfers between handlers, a compact calling convention, and a small set of live execution variables. Ordinary function-pointer calls returning to a central loop add call/return mechanics. Recursive handler calls without a guaranteed tail transfer violate the stack requirement.

   Handler addresses must remain internal execution data, separate from Lua function identity and serialized bytecode. Every supported compiler/target combination needs the actual constant-stack guarantee.

   I would put this behind the fused register executor in priority. It could be valuable, especially if dispatch remains dominant, but it is the least defensible major rewrite to promise from source inspection alone.

The 64-bit encoding deserves redesign as part of the execution ISA, but I would not make narrower words the lead optimization. [Its extraction functions](/home/folk/Programs/dellingr/src/instr.rs:747) are simple shifts and truncations. The larger structural cost is repeatedly executing the surrounding machinery.

A compact runtime encoding with common narrow forms and explicit wide forms is reasonable. It must preserve large operands: [the register limit](/home/folk/Programs/dellingr/src/limits.rs:108) is much wider than eight bits, as are literal indices. `MAX_FRAME_SLOTS` limits parameters plus locals; it does not imply every temporary register fits in a byte. Field instructions also need cache information, and comparison branches need two operands plus a target. A universal 32-bit replacement would either lose supported programs or move complexity into extensions.

I would design encoding around the fused handlers and their operand needs. I would avoid expanding every instruction into a large decoded record: saving shifts while multiplying the instruction working set is an unattractive trade without evidence.

The smaller changes I would prioritize afterwards are:

- **Constant comparison and table operand forms.** [Comparison codegen](/home/folk/Programs/dellingr/src/compiler/codegen.rs:1031) materializes both operands in registers. Even `if n < 2` needs a constant load before the already-fused comparison branch. Constant comparisons, literal indexed accesses and literal field stores can remove entire instructions and temporary values.
- **Opcode-specific comparison paths.** [The comparison helper](/home/folk/Programs/dellingr/src/vm/registers.rs:450) takes a runtime `CompareOp` and expresses ordering through `partial_cmp`, target ordering and negation. Direct numeric comparison handlers could simplify this path, while retaining string/error fallbacks and exact NaN behavior. The remaining machine-level selection overhead needs checking before claiming a gain.
- **Execution-bound constants and cache references.** [BytecodeRuntime](/home/folk/Programs/dellingr/src/vm.rs:194) already provides the natural binding point. Lowered execution operands can directly identify their constants and cache storage, avoiding repeated optional cache-slot decoding and family indexing. Keep large mutable cache records out of the instruction stream.
- **Specialized numeric-loop controls.** [FOR_LOOP](/home/folk/Programs/dellingr/src/vm/eval_control.rs:49) reloads and checks three tagged controls each iteration. Unboxed controls are plausible where analysis proves their isolation. Snapshot-admitted code can skip setup or overwrite controls, so specialization needs a proof or fallback. Converting the loop to an integer trip count would change floating-point behavior.

`Builtin` is already a useful fast path, not a major structural omission. [Builtin reads](/home/folk/Programs/dellingr/src/vm/global_env.rs:411) use a mirror guarded by the global table's mutation epoch. The more interesting extension is fusing uses of that result, such as a builtin-table field read. Any specialization must still observe rebinding and mutation through `_G`.

Three findings materially constrain the rewrites:

- **Snapshots contain raw bytecode words.** [The serializer](/home/folk/Programs/dellingr/src/vm/save_state.rs:1476) saves instructions, cache counts, frame size and line metadata. Preserving snapshot bytes requires retaining or reconstructing that canonical representation while executing a different one.
- **Register elimination can change stack-cap failures.** [Call admission](/home/folk/Programs/dellingr/src/vm/eval.rs:896) reserves the original frame footprint plus headroom. A physically smaller executor must preserve the logical reservation and refusal point.
- **Materialization boundaries include GC and captured state.** Values held only in native locals are absent from the current root inventory. Before allocation or re-entry, the executor must restore the required register/root state, including retention that could affect observable identities. Backward-transfer interrupt polls and partial effects before errors must also remain correctly placed.

I did not establish a correctness bug in the inspected paths. The strongest finding is architectural: execution still requires a stack materialization and another interpreter turn between many tightly connected operations. Removing those boundaries, supported by a register executor that can exploit admission guarantees, is where I would spend the aggressive rewrite effort.
