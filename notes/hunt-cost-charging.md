**The strongest opportunity is a full rewrite around statically priced execution regions.** Move accounting out of individual opcode handlers, execute affordable regions without meter updates, and recover exact charged prefixes only at exits. Pair that with an execution-owned meter that survives Lua calls and returns.

These are architectural changes with a credible throughput payoff. Simplifying the current batching arithmetic is useful, but leaves its main dependency chain intact.

This assessment is based only on code inspection. I read the charge sites, meter implementation, opcode metadata, execution and re-entry paths, snapshot accounting, relevant tests, and workload source. I made no changes and ran no builds, tests, or benchmarks.

**What the current implementation actually pays**

The dispatch lives in [frame.rs](/home/folk/Programs/dellingr/src/vm/frame.rs:266), rather than the named `eval*.rs` files. It already batches writes to the persistent meter. For each positively weighted opcode, however, it still:

- Reads the charge's weight and checks whether it is zero.
- Computes a saturating pending-cost sum.
- Compares that sum with the batching interval.
- Unless the interval check already succeeds, consults the optional finite budget and computes whether pending cost reaches it.
- Updates the pending accumulator or calls the settlement path.

Settlement then selects the meter mode and updates `used` and, for a finite budget, `remaining`.

This overhead applies to arithmetic, negation, table creation, table writes, and `SET_LIST`. Many other instructions are structurally free; their `fixed_cost!` invocation resolves to no charge. Thus the opportunity scales with **charged instruction density**, rather than total instruction count alone.

The current design amortizes persistent counter updates. It does not amortize the decision about whether an update is necessary. That decision remains intertwined with cheap opcode execution.

**1. Highest priority: replace opcode accounting with region accounting**

**Classification: full coherent rewrite. High conviction about the mechanism; unmeasured throughput gain.**

A straight-line region already determines its fixed charges before execution. The VM nevertheless reconstructs their running sum dynamically, instruction by instruction, every time it executes that region.

The necessary information largely exists:

- [Instr::static_units](/home/folk/Programs/dellingr/src/instr.rs:558) identifies fixed charges and closed `SET_LIST` element counts.
- The verifier already identifies [control-flow targets](/home/folk/Programs/dellingr/src/compiler/verify.rs:563).
- A State's cost model is immutable.
- [BytecodeRuntime](/home/folk/Programs/dellingr/src/vm.rs:192) already provides a State-specific bundle shared by a chunk's closures and active frames.
- Dispatch already maintains an authoritative instruction cursor and publishes it on escape.

I would turn those ingredients into an execution plan containing region boundaries, priced totals, and enough ordered charge information to reconstruct prefixes. Price it once for the State's model, alongside the existing runtime data.

On ordinary execution:

1. At region entry, determine whether its positive charges can all begin.
2. If so, execute through handlers containing **no per-opcode meter update or weight lookup**.
3. Account for the region once on completion.
4. On an early exit, account for the executed prefix instead.

This must change the execution structure. Adding a region table while retaining `fixed_cost!`, or testing an "already admitted" flag in every charged handler, preserves much of the problem. Region boundaries should be explicit execution boundaries or checkpoint entries. The ordinary handler path should have no runtime metering-mode branch.

Keep one source for opcode semantics, generating the ordinary and exact-accounting paths from it. Hand-maintaining two interpreters would create unnecessary correctness risk.

The exact budget rule is manageable. For positive charge amounts `c1 ... cn`, with positive remaining budget `R`, all charges may begin when:

```text
c1 + ... + c(n-1) < R
```

The final charge may cross zero. A simpler, conservative affordability test can send more edge cases through exact accounting without changing behavior.

For example, with charges `3, 5, 2` and budget `4`, the first two operations execute and charge `8`; the third fails. Rejecting the region at entry would be wrong. When a region is unaffordable, execute its prefix with exact charge checks, preserving intervening free operations and side effects.

The more consequential detail is **early exit accounting**:

- An arithmetic instruction that fails its type check has already incurred its charge.
- A blocked charge does not execute or add cost.
- An error in a free instruction still preserves preceding charges.
- An interrupt preserves the prefix through its existing safepoint.
- A metamethod call publishes only the prefix reached before entering the handler.

Do not precharge a whole region and later subtract its unused suffix from saturated counters. Saturation loses information. Preserve the entry accounting state and derive the actual prefix on escape.

For straight-line code, the cursor identifies that prefix. Store compact charge positions or prefix checkpoints; reconstruction belongs on exits, rather than every successful instruction.

Potential metamethod calls need side exits. A direct table access can remain inside the ordinary region. If resolution reaches a callback, publish the prefix, execute the callback, then admit the remaining suffix under the resulting meter state. Splitting every table access into its own region would sacrifice much of the opportunity.

Open `SET_LIST` needs separate treatment: its live count is dynamic, and the entire element charge is atomic. Closed `SET_LIST` can participate in static pricing. Native library charges remain outside the fixed region total.

**Why this could pay materially:** arithmetic expressions and table-update kernels currently serialize several accounting operations around each useful operation. Region accounting removes that chain between charged instructions. Its benefit should be strongest where multiple cheap charged operations occur between control-flow boundaries. The arithmetic and queue workload sources contain exactly that shape.

**Risks:** tiny regions can replace cheap accounting with expensive boundary handling; execution-plan metadata consumes cache; generated interpreter variants can increase code size; side exits and source attribution become more demanding. None of this requires changing instruction results, allocation order, RNG draws, or interrupt placement.

**Sibling request:** codegen should expose region structure and ordered charge provenance. Dispatch should support entering an admitted region without an extra metering test at each instruction. This crosses both scopes deliberately. Canonical serialized instructions can remain unchanged while the executable plan is rebuilt, preserving snapshot bytes.

**2. Give one dispatch invocation ownership of accounting across Lua frames**

**Classification: architectural rewrite, complementary to region accounting. High conviction that the current handoffs are unnecessary; payoff depends on call and iterator density.**

The pending counter already belongs to `Frame::run`, which drives multiple Lua frames. Nevertheless, [calls and returns flush it](/home/folk/Programs/dellingr/src/vm/frame.rs:719), even when control remains inside the same dispatch invocation.

There is a sharper example: [TFOR_CALL flushes eagerly](/home/folk/Programs/dellingr/src/vm/frame.rs:686), before discovering that the recognized `next` or `ipairs` path can complete [without invoking a callback](/home/folk/Programs/dellingr/src/vm/eval_control.rs:90).

Consequently, a loop such as:

```lua
for _, v in pairs(t) do
    sum = sum + v
end
```

can settle its one pending addition every iteration. The nominal batching interval offers little amortization there.

The stronger design is an invocation-owned accounting context:

- Lua-to-Lua calls, tail calls, and returns retain the same accounting owner.
- Recognized iterator fast paths retain it too.
- Region completion updates that local context.
- Before arbitrary Rust code executes, publish complete accounting to `State`.
- After Rust returns, reload it, because the callback may have charged work, reset the budget, or made nested calls.
- On escape, publish before constructing budget diagnostics or notifying host error callbacks.

The boundary should follow **actual independent access to the meter**, rather than the presence of a Lua call instruction or frame transition.

This also warrants changing helper interfaces. Today, `local_cost` travels through table and metamethod resolution so a deeper helper can flush it. The resolution machinery already distinguishes a direct result from a pending call. Make the pending-call outcome carry the execution continuation needed to synchronize accounting at that boundary. Keep meter ownership out of helpers that only perform direct work.

That is more substantial than moving one flush downward. It removes distributed responsibility for accounting publication and makes the execution driver responsible for its own ledger.

**Why this could pay materially:** it avoids short batches in method-heavy code, recursive Lua functions, and generic iteration. It also reduces the occasions on which local execution state becomes addressable shared state passed through helper chains.

**Risks:** Rust callbacks can inspect and reset accounting mid-run, and nested calls can fail while the callback continues. Every actual Rust entry must see complete accounting. The existing [mid-run reset contract](/home/folk/Programs/dellingr/src/vm.rs:553) makes reloading after return essential. Error notification ordering also matters.

This design must retain the current iterative Lua call stack. It needs no native recursion and must not replace existing stack-cap admission checks. Additional execution context should remain compact because dispatch stays live across native re-entry.

**3. A smaller alternative: cache the next settlement threshold**

**Classification: medium-value local change. Worth considering independently, but not a substitute for the region rewrite.**

The current loop repeatedly asks a question whose inputs usually have not changed: whether pending cost reaches either the batching interval or the remaining budget.

After settlement or return from native code, compute one threshold:

```text
count-only:       interval
finite budget:    min(interval, max(remaining, 0))
```

Then each positive opcode charge accumulates pending cost and compares against that threshold. Reaching it enters the existing admission and settlement path. At an exhausted budget, threshold zero routes the next positive charge to refusal; free operations still bypass it.

This removes the repeated optional-budget selection and saturating subtraction in [batch_needs_flush](/home/folk/Programs/dellingr/src/cost_meter.rs:199). A countdown representation can express the same design, with pending cost reconstructed when publication is necessary.

There is also avoidable generality in the production pending arithmetic. Pending cost remains below `64` between settlements; individual dispatch charges are bounded by `u32` weights and bounded element counts. Their addition does not require arbitrary-`u64` saturation. Persistent totals and raw host charges still do.

The limitation is fundamental: one accumulator dependency and boundary decision remain per positively charged instruction. This makes the existing design cheaper rather than removing its structural bottleneck. Actual load hoisting and register placement require compiled-code inspection before estimating a gain.

**4. Simplify persistent accounting to one authoritative counter**

**Classification: medium-value representation change, mainly supporting the larger redesign.**

`Budget` stores both `limit` and `remaining`, while `CostState` separately stores `used`. Yet [snapshot validation already enforces](/home/folk/Programs/dellingr/src/cost_meter.rs:240):

```text
remaining = limit.saturating_sub_unsigned(used)
```

For a configured finite budget, positive-charge admission can instead use:

```text
limit > 0 && used < limit as u64
```

Then update only `used`, saturating. Derive `remaining` when publishing it through the API or snapshot encoder.

This removes a counter update and a consistency obligation. It also gives the execution-owned ledger a simpler representation.

Preserve the configured-budget flag: a finite `i64::MAX` budget differs from count-only mode. Keep saturation and negative-limit behavior exact, and emit the existing snapshot fields unchanged.

I would not rank this as a major throughput project by itself. Persistent settlement is already amortized in favorable workloads. Its value grows when combined with eliminating the frequent handoffs described above.

**Findings and practical limits**

I found no definite correctness bug in the inspected charging paths. Several behaviors are important enough to make explicit:

- Batching already preserves instruction-level failure timing by settling at the budget edge. Replacing it with unconditional block precharging would regress correctness.
- Non-budget errors settle pending charges in [Frame::run](/home/folk/Programs/dellingr/src/vm/frame.rs:173). Prefix reconstruction must preserve this behavior.
- `Costing::Off` currently works by supplying zero effective weights; charged handlers still encounter the weight/zero decision. The region design can select genuinely accounting-free execution.
- Whole-function or loop-trip precharging is a weaker first target. Branches, callbacks, early failures, and interrupts make its exact-prefix recovery substantially harder. Known straight-line regions capture the cleaner opportunity.
- Cost-model construction, registry cleanup, and static analysis rendering are not meaningful targets for this scope.

The full region rewrite is my first choice, with invocation-owned accounting designed alongside it. The threshold change is the strongest local alternative. The key acceptance evidence would be identical outputs, errors, surviving state, charged prefixes, and snapshot bytes across budget boundaries, followed by throughput measurements on arithmetic, queue, method, and iterator workloads. No speedup percentage is justified from this read-only inspection.
