The strongest opportunity is a rewrite of the host invocation and callback boundary: give execution a borrowed game context, marshal arguments directly into their execution slots, and decode results without routing every value through separate public stack operations.

I would prioritize that over optimizing `HostCallbacks`, anchor lookup, or the Rust function registry. Those are comparatively small mechanisms. The larger problem is that the current boundary makes the host repeatedly reconstruct information the VM already has: argument locations, types, table slots, result locations, and execution ownership.

This is a source-based assessment. I read the relevant implementation and tests, without reading project markdown or history, changing files, building, or executing anything. Payoff rankings below are hypotheses, not measured speedups.

| Priority | Opportunity | Form | Conviction |
|---|---|---|---|
| 1 | Direct invocation and typed callback frames | Full boundary rewrite, requiring support from the calls scope | High |
| 2 | Borrowed per-tick game context with split access to Lua arguments | Full embedding ownership rewrite, best combined with 1 | High architectural value; payoff depends on host integration |
| 3 | Prepared global and field access, including borrowed record decoding | Structural boundary change using existing table machinery | High for data-heavy callbacks |
| Conditional | Persistent backing for restricted environments | Full environment representation rewrite | Potentially large if used around every tick |
| Lower | Anchor churn, redundant validation, string conversion copies | Local changes | Real but narrower |

**1. Replace the stack-operation protocol with direct invocation and callback frames.**

The public protocol currently assembles a call through independent operations. Each numeric argument push checks usability and stack space. Calling an anchor then looks up the value, inserts it below the arguments, and enters the general call machinery. Results are normalized onto the stack, read through another public operation per value, then removed.

The relevant implementation is in [stack.rs](/home/folk/Programs/dellingr/src/vm/stack.rs:111), [anchor.rs](/home/folk/Programs/dellingr/src/vm/anchor.rs:341), and [eval.rs](/home/folk/Programs/dellingr/src/vm/eval.rs:182).

For a host that pushes two numbers, calls an anchor, reads one numeric result, and pops it, there are five public usability checks before considering anything the script calls back into. On x86-64, the floating-point environment check reaches an out-of-line control-register read. This is more than a repeated boolean branch. See [check_usable](/home/folk/Programs/dellingr/src/vm.rs:1493) and [read_control](/home/folk/Programs/dellingr/src/fp_env.rs:224).

The reverse boundary has a similar structure. A `RustFunc` receives only `&mut State` and returns a result count. Its author must rediscover arguments through stack indices and publish results by pushing them. Before invoking it, the VM removes the callee slot, shifting the arguments. Afterwards, it copies retained results down over discarded arguments and temporaries. See [RustFunc](/home/folk/Programs/dellingr/src/vm/lua_val.rs:23), [begin_call](/home/folk/Programs/dellingr/src/vm/eval.rs:438), and [invoke_rust_fn](/home/folk/Programs/dellingr/src/vm/eval.rs:660).

These costs occur once per boundary crossing, regardless of how little useful work the callback performs. That makes them especially relevant to entity scripts calling small game queries and commands.

The stronger design is an invocation interface whose implementation owns the whole transfer:

- A state-bound entry identifies the callee.
- A built-in argument encoder writes directly into the final call window.
- Execution returns a result span or decodes directly into the requested Rust result.
- One owner handles operand cleanup, result normalization, errors, and panic poisoning.

Conceptually, a host call could become:

```rust
let position = state.invoke(&mut tick_context, &entry, (entity_id, dt))?;
```

The important change is underneath that signature. A tuple wrapper that internally calls `push_number`, `call_anchor`, `to_number`, and `pop` retains most of the current cost.

For registered host functions, replace the requirement to manipulate `State` with a callback frame that supplies validated arguments and an explicit result destination. Simple callbacks should receive decoded scalars and borrowed string or table views, perform their game operation, and return values directly. Callbacks needing Lua re-entry can receive an execution capability with a deliberately broader contract.

That also creates a useful distinction between callbacks that need only game data and callbacks that need arbitrary VM access. Today every callback gets the latter interface.

The existing internals make this plausible. Lua admission already keeps arguments in their execution positions rather than copying them to a separate argument array. The dispatch loop already returns the first result location and count. The host boundary then normalizes those results through [collect_returns](/home/folk/Programs/dellingr/src/vm/eval.rs:1114). An outermost host invocation could decode from the returned span before destroying the frame, avoiding that relocation when the host only wants Rust values.

This proposal needs the calls scope to support a callee, argument span, and result destination as explicit operands. The host boundary should not have to materialize `[callee, arguments...]` merely to have the next stage remove or rearrange it.

There are substantial correctness risks:

- Keeping a dead callee slot physically present must not reduce the callback's usable stack capacity. The current removal deliberately frees that slot. A new convention needs accurate logical stack accounting.
- A result view must remain rooted until decoding finishes. Borrowed results must prevent incompatible stack mutation or re-entry.
- Closing upvalues must still precede destruction or relocation of their source slots.
- Typed decoding must preserve each binding's accepted inputs and exact errors, including missing versus nil arguments and method-call numbering. Automatically enforcing exact arity would change existing behavior.
- Floating-point checks can be consolidated across VM-controlled marshalling, but cannot simply be hoisted across arbitrary host code. A callback can change the environment before calling back into the VM.
- Opcode charges and callback-visible cost must remain settled at the existing semantic boundaries. Batching several entity invocations must preserve individual failure behavior.

My conviction is high that this is the right architectural direction. Its benefit will be greatest for short scripts and frequent small callbacks. A long compute-heavy Lua invocation will amortize most of it already.

**2. Move game access out of persistent `Any` storage and into a borrowed execution context.**

The ownership restriction is more consequential than the downcast itself.

`State` stores user data as `Option<Box<dyn Any + Send>>`, with `'static` accessors. Registered functions are plain function pointers. Consequently, a callback cannot directly receive a temporary borrow of the world's current tick state through the existing registration mechanism. See [user data](/home/folk/Programs/dellingr/src/vm.rs:818).

A host can own its game data inside `State`, but an engine driving many states often needs the opposite relationship: the world owns execution, and individual script states temporarily borrow selected world access. The present API can push such an integration toward shared ownership, interior mutability, moving owned context in and out, command collection, or copying tick data into Lua values.

Those extra mechanisms are integration consequences, not costs proven to exist in the supplied host. There is no mutex in `user_data_mut` itself, and removing one type check per callback is not the main prize.

There is a second restriction even when the data fits inside `State`: borrowing Lua string bytes through `to_bytes` borrows `State`, preventing a simultaneous mutable borrow through `user_data_mut`. That can force a host callback to copy a string merely to use it while updating game data. The current [raw-field conversions](/home/folk/Programs/dellingr/src/vm/raw_field.rs:162) explicitly return owned `Vec<u8>` and `String`.

The stronger design separates durable VM state from execution-time host access:

```text
durable State + borrowed tick context -> execution -> registered callback
                                          |
                              Lua argument views + game access
```

Registered callback entries retain their identity, diagnostic name, and executable thunk. The context arrives at invocation time and cannot escape it. An argument view can borrow the VM's heap while the callback independently mutates the borrowed game context.

This need not make the entire interpreter generic over the world's type. A typed registration layer can perform adaptation at the callback thunk. The essential requirement is a scoped lifetime and separate ownership, not a particular dispatch technique.

The VM already demonstrates the necessary borrowing approach internally. [with_string_args](/home/folk/Programs/dellingr/src/vm/string_arg.rs:72) splits access to strings, cost accounting, and interruption instead of borrowing all of `State` through one method interface. The embedding API could provide the analogous split between Lua arguments and game data.

This can remove copying and synchronization at every host query, potentially a larger gain than direct call marshalling. It also lets many states execute sequentially against one world borrow without constructing per-state ownership arrangements.

Re-entry is the main risk. A callback must not retain an exclusive world borrow and then recursively hand out another overlapping one through Lua. The execution API must express when access is suspended and reborrowed. Similarly, borrowed Lua data cannot remain exposed across arbitrary VM mutation unless its lifetime and immutability are guaranteed.

Immediate world changes must retain their current order and visibility. Replacing them with deferred commands is not a transparent implementation optimization.

I would implement this together with the first rewrite. Afterwards, if this works, broader conveniences for host services or binding generation can follow.

**3. Prepare repeated global and record access instead of resolving names and tables afresh.**

The host side does not benefit from much of the lookup machinery already available to bytecode.

For a non-builtin global, `get_global(name)` finds the interned string by its bytes, then reads the global table without a cache. The bytecode path has validated global probes and cached writes. Compare [read_name](/home/folk/Programs/dellingr/src/vm/global_env.rs:446) with [read_cached](/home/folk/Programs/dellingr/src/vm/global_env.rs:317).

For every `raw_field`, the implementation:

1. Resolves and validates the table argument.
2. Finds the field name in the string pool.
3. Looks up the field in the table.
4. Converts the value.

A callback reading ten fields repeats the table validation and name-resolution path ten times. String and byte fields additionally allocate owned results. See [raw_field](/home/folk/Programs/dellingr/src/vm/raw_field.rs:233).

General Rust table access is more elaborate: keys and values travel through stack operations and their associated checks. There are internal direct string-key helpers, but even exposing those unchanged would leave repeated interning and lookup in the hot path.

The stronger boundary is a prepared access plan:

- A global-binding descriptor caches a validated probe while continuing to read the current binding.
- A record view validates its table argument once.
- Field descriptors resolve names and retain validated slot information.
- Reads convert directly from values into scalars or borrowed bytes.
- Writes use direct slot stores when valid, with the existing semantic fallback when required.

The necessary primitives already exist: [find_slot](/home/folk/Programs/dellingr/src/vm/table.rs:1047), `read_slot`, `renew_slot`, `store_slot`, and global `HashProbe` operations. The bytecode field path in [eval_index.rs](/home/folk/Programs/dellingr/src/vm/eval_index.rs:16) demonstrates how table identity and slot validation work together.

This is a structural improvement because it moves repeated work from individual field operations to a prepared binding and a validated record access. It should not materialize a temporary map or vector of decoded fields.

There are two distinct workload cases:

- **Persistent entity tables:** caching slots per retained table can eliminate most repeated hashing.
- **Fresh option tables:** table-identity caches will miss repeatedly. Borrowed decoding and validating the receiver once still help, but equally spelled fields do not imply shared layouts. The existing `SlotRef` is not a cross-table shape identifier.

I would pursue this after, or within, the invocation rewrite. It is particularly compelling where callbacks exchange records rather than a few positional numbers.

The semantic risks are easy to underestimate:

- A cached global binding must observe rebinding. An anchored function deliberately retains a value; these are different operations.
- Raw field access must stay raw. Metamethod-aware access must preserve handler invocation and error behavior.
- Deletion, reinsertion, compaction, and environment changes must invalidate or renew the right tokens.
- Descriptors must not casually intern and permanently root every field name during preparation. Extra string lifetimes can affect later identity behavior.
- Record decoding must preserve the order in which validation errors become visible.

The write path must also use the existing mutation machinery so builtin mirrors and table bookkeeping remain coherent.

**The restricted-environment path deserves a separate, conditional rewrite.**

If the engine wraps each entity tick in `with_restricted_env`, this could move near the top of the list.

Every entry constructs a new `Table`, resolves the whitelist names, copies their values into it, and swaps it into the existing global table. Exit swaps the original contents back. Both swaps advance the table version, invalidating global probes and other caches naming that table. See [with_restricted_env](/home/folk/Programs/dellingr/src/vm.rs:1447) and [restrict/resume](/home/folk/Programs/dellingr/src/vm/global_env.rs:480).

That combines allocation, whitelist reconstruction, and repeated cache invalidation around the script's useful work.

A stronger design would make restricted contents a prepared ordered backing view with invocation-local writes, selected at entry and discarded at exit. Unchanged visible bindings could be shared rather than rebuilt. Cache validation would identify the active backing view instead of becoming cold solely because execution entered and left it.

This is a full representation rewrite. It cannot be implemented correctly as a second environment consulted only by global bytecodes: `_G`, previously retained aliases, raw table operations, and iteration must all observe the same active contents. Whitelist order, outer binding changes, nested restrictions, and discarded writes must remain exact.

The mechanism is clearly expensive; its importance is unknown without knowing whether the game uses this API per tick. If restriction is setup-only, it should not distract from the first three opportunities.

**The remaining local changes are useful, but I would keep their ranking lower.**

- **Avoid transient anchor ownership for temporary values.** Anchor insertion drains a mutex-protected release queue and clones an `Arc`; dropping queues the key under the mutex. Scoped values could remain in invocation-owned root storage and require an owned anchor only when they escape. However, ordinary anchor lookup takes no lock. Reusing one function anchor across ticks already avoids anchor creation and release costs. See [anchor registry](/home/folk/Programs/dellingr/src/vm/anchor.rs:176).

- **Remove avoidable string conversion intermediates.** `State::to_string` first obtains owned bytes, then constructs a Rust string. For an existing valid UTF-8 Lua string, this creates an unnecessary intermediate allocation and copy. `to_bytes` already supports borrowing positional strings; field decoding does not. See [to_string](/home/folk/Programs/dellingr/src/vm/stack.rs:334). Rendering numbers and metamethod results still needs its established byte-exact behavior.

- **Fuse validation with value extraction.** A host callback using `check_type` followed by `to_number` repeats validation and usability checks; `check_type` itself calls `try_typ`. A validated argument reader can produce the value in one pass. This belongs naturally in typed callback frames rather than becoming another independent stack API. See [check_type](/home/folk/Programs/dellingr/src/vm_aux.rs:42).

- **Reduce activation ownership work through the calls scope.** A root Lua invocation obtains a bytecode `Arc` for admission, then constructs a frame with cloned bytecode, runtime, and upvalue `Arc`s. The frame stack is already pooled, so claiming a fresh frame allocation per tick would be wrong. Consolidating activation ownership could remove reference-count operations, but this affects the general calling implementation. See [admitted_frame](/home/folk/Programs/dellingr/src/vm/eval.rs:926).

A few findings constrain how I would judge these changes. `HostCallbacks` handles printing and error notification, not ordinary game operations, so making that trait generic is unlikely to address the multiplier described here. Rust function dispatch is already an indexed function-pointer lookup, with names resolved for diagnostics rather than every successful call. Anchors also have an existing determinism hazard: cross-thread drops concurrent with execution can change collection survival, so a replacement must handle release timing deliberately.

Finally, the current benchmark harness's warm path reuses a function already rooted on the stack and invokes it with zero arguments and zero expected results. It does not exercise the substantial argument, record, result, and game-context traffic discussed above. See [run_warm_block](/home/folk/Programs/dellingr/src/bin/dellingr-bench.rs:370). A full boundary rewrite should be kept or reverted based on representative entity-tick traffic, including callbacks against game data; compute-heavy Lua workloads alone could largely miss its benefit.
