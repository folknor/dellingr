**I would rewrite the activation machinery.** The strongest opportunity is to remove duplicated activation ownership and make VM-owned callbacks use the same execution engine as ordinary Lua calls. A separate native calling convention should then remove the stack protocol paid by small library functions.

These are source-based performance hypotheses, not measured speedups. Their likely value differs by workload:

| Priority | Change | Classification | Likely payoff |
|---|---|---|---|
| 1 | One activation record, without per-call Arc ownership | Full rewrite, highest conviction | Broad benefit to ordinary Lua calls |
| 2 | VM-owned continuations for metamethods, iterators and library callbacks | Full rewrite, high conviction about the bottleneck | Potentially large on callback-heavy scripts |
| 3 | Native argument/result windows instead of the host stack protocol | Full boundary rewrite | Strong candidate for cheap library calls |
| 4 | Selective initialization and fewer window materializations | Calling-convention rewrite, more conditional payoff | Wide frames and vararg-heavy code |

Ordinary Lua arguments already occupy the callee's parameter window, and Lua CALL already stays within one dispatch loop. The opportunity is in everything wrapped around that transfer.

**The first rewrite should make an activation one execution object with one lifetime.**

Today each Lua activation has both a state-owned [CallInfo](/home/folk/Programs/dellingr/src/vm.rs:109) and an invocation-owned [Frame](/home/folk/Programs/dellingr/src/vm/frame.rs:29). Both carry bytecode ownership and an instruction position. Admission and retirement update two vectors, a separate depth counter, the current stack bottom, and the vararg arena.

The direct path also resolves the same closure twice:

1. [lua_function_bytecode](/home/folk/Programs/dellingr/src/vm/eval.rs:914) looks through the heap and clones its bytecode Arc.
2. Admission stores that owner in CallInfo.
3. [admitted_frame](/home/folk/Programs/dellingr/src/vm/eval.rs:926) looks through the function slot and heap again, then clones bytecode, runtime and upvalue-list Arcs.

That is **four Arc clones and four matching drops per activation**, including two owners of the same bytecode. This happens during calls to already-created closures, so it is directly within the dominant cost being optimized.

On return, [return_to_caller](/home/folk/Programs/dellingr/src/vm/eval.rs:1034) retires both records, restores the caller's bottom, decodes its previous CALL instruction for the result policy, adjusts the results, and restores its physical register extent.

The stronger design is one executor-owned activation stack. An activation contains its execution descriptor, cursor, register base, vararg ownership, chain accounting and return continuation. Diagnostics and GC inspect this same stack. An invocation is a stopping boundary within it, rather than a separate frame vector paired with another global record vector.

Resolve the closure once and retain a non-owning execution view for the activation. The closure already owns its immutable bytecode, runtime and upvalue list, and its function slot ordinarily roots it. The [boxed Closure representation](/home/folk/Programs/dellingr/src/vm/object.rs:233) also offers stable storage while the closure remains alive. This makes removing activation-level reference counting plausible without replacing all program ownership with another mechanism.

The execution/control storage should be separated from the mutable VM services it calls. That lets execution borrow immutable descriptors while manipulating activation records by index. Frame-vector references must end before pushes or re-entry can invalidate them.

This is high conviction because it removes unconditional work from essentially every Lua activation: redundant ownership operations, a repeated heap lookup, and two record lifecycles. Merely merging the structs while retaining all four Arc owners would capture only part of the opportunity.

The main risks are lifetime correctness and observable retirement timing. In particular, a Rust tail callee can collect the retired caller's closure while its diagnostic frame still exists; [the tests explicitly exercise this](/home/folk/Programs/dellingr/tests/integration/runtime/tail_calls.rs:36). The rewrite needs separate lifetimes for executable closure state and the diagnostic information retained during that tail call. Keeping the old closure rooted longer would change behavior. Simply substituting raw pointers for Arcs would be unsafe.

**The second rewrite should turn VM-owned callbacks into continuations on that executor.**

The current flat calling convention applies to ordinary CALL, but several other Lua-to-Lua transfers travel through Rust:

- [index_call](/home/folk/Programs/dellingr/src/vm/registers.rs:146), newindex_call and len_call invoke call_unguarded.
- A general [generic-for iterator](/home/folk/Programs/dellingr/src/vm/eval_control.rs:110) does the same.
- Each [custom sort comparison](/home/folk/Programs/dellingr/src/vm/table_ops.rs:900) builds a call window and invokes call_unguarded.
- string.gsub replacement functions repeat this protocol per replacement.

Consequently, a Lua iterator or metamethod pays native-depth accounting, call-boundary machinery, a fresh dispatch invocation, entry-result collection and boundary teardown. [run_invocation](/home/folk/Programs/dellingr/src/vm/eval.rs:996) reuses its frame vectors, so this is not necessarily an allocation per callback. It is nevertheless an entire nested execution lifecycle per callback.

The existing metamethod resolver already exposes the useful seam. [IndexOutcome::Call](/home/folk/Programs/dellingr/src/vm/metamethod.rs:43) means resolution has finished and a handler window exists. The current caller then enters Lua synchronously from Rust.

Instead, suspend the operation in the executor:

- An index continuation says where to put the handler's result and which metamethod units to release.
- A newindex continuation releases its units and resumes the caller.
- An iterator continuation projects results into loop-variable registers.
- A sort continuation retains heap-sort phase, indices and operands, and requests the next comparison.

A Lua handler then pushes an ordinary activation onto the same execution stack. Its return completes the continuation. There is no nested Frame::run invocation for these VM-owned operations.

For table.sort, this means rewriting the sort driver as a resumable operation. An adapter that still synchronously calls call_unguarded for every comparison preserves the expensive structure. The same applies to callback-taking string operations. Afterwards, if this works, sharing continuation utilities across more operations is a follow-up.

This could matter substantially on real scripts: iterator calls repeat per loop element, index handlers can mediate ordinary game-object access, and sort comparisons repeat throughout the sort. Small handlers currently have little useful work over which to amortize the invocation boundary.

There are important compatibility obligations:

- Preserve the **logical** native-depth limit for eliminated re-entry paths. Allowing formerly failing recursion to succeed changes script-visible errors.
- Preserve exact comparison order and sort writeback timing. Comparators can mutate state, print, draw RNG values or fail.
- Preserve metamethod-depth ownership until the same operation completes.
- Preserve where errors are located and reported. call_unguarded reports errors before returning them to Rust code that may handle them.
- Retain interrupt polls in resumable native loops and at function admissions.

Arbitrary host Rust functions can still synchronously re-enter Lua. That requires an execution boundary. VM-owned iterator and metamethod calls do not inherently require one.

**The third rewrite should stop making library functions implement the host's mutable-stack protocol.**

The native ABI is currently [fn(&mut State) -> Result<u8>](/home/folk/Programs/dellingr/src/vm/lua_val.rs:23). Arguments are callback-visible stack entries; results are the top reported values.

That convention causes work before and after even a tiny library body:

- [begin_call](/home/folk/Programs/dellingr/src/vm/eval.rs:438) removes the function slot, shifting its arguments down.
- invoke_rust_fn changes stack-bottom and active-function state.
- The callback accesses arguments through relative stack APIs.
- It clears or reshapes that stack and pushes its results.
- [invoke_rust_fn](/home/folk/Programs/dellingr/src/vm/eval.rs:660) validates the reported count and potentially copies the result suffix over arguments and temporaries.
- The caller adjusts result count and restores its register extent.

The [math library wrappers](/home/folk/Programs/dellingr/src/lua_std/math.rs:55) make the mismatch concrete: a one-number operation performs argument checking, another indexed read, set_top and push_number around its arithmetic.

Introduce a native call context with explicit argument indices/count and a result destination or result writer. A one-value function can return a value directly; a multi-result function can return a rooted span or write through the result interface. Arguments remain where the caller prepared them. Relative host-stack indexing and top-suffix result interpretation disappear from ordinary library calls.

The registry can retain exactly the same RustFnId identities while associating entries with different execution forms: trusted leaf library functions, resumable library operations, and arbitrary host functions. The native ABI change does not require changing Lua equality, rendered identities, registration order or saved function identifiers.

There is already an internal capability worth using: [RustFnId::is_library](/home/folk/Programs/dellingr/src/vm/rust_fn.rs:54). check_usable uses it to skip repeated floating-point-environment reads inside library functions. However, invoke_rust_fn still checks the environment after every successful native return, including trusted leaf math functions.

A leaf entry that cannot execute host code can avoid arbitrary-host return machinery. Entries that can call HostCallbacks or host functions still need the real boundary checks. Catalog membership alone is insufficient because print is also a catalog entry.

I would make this a real ABI rewrite, including the library adapters. Wrapping the existing push/pop implementations in a new context would leave most of the materialization intact.

The payoff is strongest for frequently called, cheap functions whose useful work is smaller than this protocol. This does not imply that catch_unwind allocates on successful calls; the opportunity is the combined boundary bookkeeping, argument relocation, indexed API work and result normalization.

The largest semantic trap is the stack cap. Removing the function slot currently gives Rust callbacks one more usable slot. Starting callbacks at idx + 1 changes which pushes succeed. The new context must preserve logical capacity and exact overflow errors independently of physical placement. It must also preserve method argument numbering, result-count validation, poisoning and callback frame isolation.

**The fourth opportunity is to separate logical register semantics from physical stack initialization.**

The value vector currently serves several roles at once: register storage, active stack extent, native argument/result area and GC root list.

At CALL, [call_window](/home/folk/Programs/dellingr/src/vm/frame.rs:820) truncates the caller's register suffix after the arguments. [Admission](/home/folk/Programs/dellingr/src/vm/eval.rs:827) then nil-fills the callee's entire remaining register window. A fixed-result return resizes back to the caller's full top and nil-fills the restored suffix again.

Thus a small call can pay initialization proportional to callee frame width and the caller suffix it temporarily replaced. Across repeated calls inside a wide function, those writes can exceed the actual argument/result traffic.

The stronger calling convention separates:

- Backing storage capacity.
- Logical stack height used for limit behavior.
- Register ranges holding observable values.
- Initialization and GC-root state.

Derive an initialization plan from compiler/admission dataflow. Initialize missing parameters and registers that can be read before being assigned; do not clear every temporary that execution will overwrite first. On return, restore the caller's logical extent without necessarily writing nil across its entire suffix.

This should use derived execution metadata rather than altering canonical saved bytecode. Activation metadata is transient, but snapshots can contain bytecode, so changing the persisted instruction stream would violate the stated snapshot contract.

The difficult part is that stale storage must remain semantically nil and must not become an extra GC root. [The collector currently visits the whole live stack slice](/home/folk/Programs/dellingr/src/vm/inventory.rs:365). Retaining old values physically while scanning them all would retain objects longer. Conversely, clearing fewer roots than the current execution semantics expose can collect objects earlier. Both are observable through host callbacks, subsequent execution and snapshots.

The verifier also does not establish general definite initialization. An admitted snapshot instruction can read an otherwise untouched register and currently gets nil. The optimization needs a proof for each omitted initialization, or a representation that supplies nil semantics without the physical write. Rejecting previously admitted bytecode would change correctness.

Varargs fit this redesign, but their payoff is more conditional. [collect_varargs](/home/folk/Programs/dellingr/src/vm/eval.rs:944) copies extras into a shared arena; [reg_vararg](/home/folk/Programs/dellingr/src/vm/registers.rs:502) copies them back into registers when requested. Retaining incoming extras as a rooted span could remove the entry copy, but locals need distinct storage and named parameters may then need moving. Hidden varargs also currently live outside the capped value stack. This needs a complete layout and accounting design, not simply leaving extra arguments in place.

I have more confidence in the ownership and invocation rewrites than in the net benefit of initialization maps. The latter trade bulk writes for metadata and control work; real frame shapes determine which wins.

There are also worthwhile **local changes**, with a smaller ceiling:

- **Project fixed results once.** collect_returns moves every returned value before finish_call discards the surplus. Copy only the requested prefix, after validating the full actual count and closing upvalues. Discarded results must still be computed, charged and validated. This mainly helps multi-result functions consumed as zero or one result.
- **Replace tail activations directly.** The current [tail replacement](/home/folk/Programs/dellingr/src/vm/eval.rs:390) admits another record, pops it, assigns it over the caller, then drops and rebuilds the execution frame. A direct replacement can remove bookkeeping while retaining the caller's diagnostic state until admission succeeds. General tail calls can still require argument movement.
- **Assemble callable-object receiver prefixes once.** [begin_callable_call](/home/folk/Programs/dellingr/src/vm/eval.rs:491) inserts each __call receiver separately, shifting existing arguments repeatedly. Resolving the receiver prefix before one final placement could help deep chains with many arguments. Direct function calls bypass this path, so it is secondary.

Two incidental findings deserve flagging. The comparator sort path holds its working array plus two additional full-array copies for rooting: a temporary roots vector and the rooted_values copy in [table_sort](/home/folk/Programs/dellingr/src/vm/table_ops.rs:827). A state-owned resumable sort could expose its working array directly to GC, removing that materialization as part of the continuation rewrite.

Also, [the comment above unpack_values](/home/folk/Programs/dellingr/src/lua_std.rs:463) still describes 255 as RetCount's All sentinel. [The actual encoding](/home/folk/Programs/dellingr/src/instr.rs:134) uses the 16-bit OPEN_COUNT sentinel and permits fixed 255. That is a stale source comment; I did not establish a runtime defect there.
