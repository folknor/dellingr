# performance.md

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

Findings of the 2026-11-17 performance hunt: ten codex hunters
(`write-sol-high`), one per scope (dispatch, cost charging, calls, table
representation, lookup paths, heap/GC, strings and patterns, table/math/basic
libraries, codegen output, host boundary). Every account is source-only: no
hunter built, ran or measured anything, and every payoff below is a
hypothesis. Where a mechanism already had an entry in `optimizations.md` or
`todo.md`, this hunt's account was added there instead of here; those entries
are listed at the end.

"Rewrite" and "local" are the hunters' own classifications.

---

## PERF-001 - Derived execution plan separate from canonical bytecode

Raised as a constraint by the dispatch, cost-charging, lookup and codegen
hunters; each of their rewrites needs it.

**Bottleneck:** snapshots serialize raw instruction words, literal pools,
templates, frame size, capture descriptors, cache counts and line
information (`save_state.rs`, chunk serializer). Any in-place bytecode
optimization (fusion, new operand forms, removed moves, region markers)
changes snapshot bytes even when execution is equal, and changing emitted
lookup instructions changes saved bytecode.

**Redesign:** keep a canonical serialized program and derive a separate,
unsaved optimized execution plan from it. Both freshly compiled programs and
loaded snapshots enter the optimizer. `BytecodeRuntime` (`src/vm.rs`) is the
lifecycle precedent, though immutable plans can be shared independently of
State-local caches. Constants used only by optimized execution stay out of the
canonical literal pools.

**Constraints the plan must carry:** admission reserves the original frame
footprint plus headroom against the global stack cap (`eval.rs` call
admission), so a physically smaller frame must keep the logical reservation
and refusal point. Stack slots are GC roots and rendered pointer identities
are weak, so shortening a value's lifetime can change later identity
rendering. Source-operation and logical-state information has to be carried
through lowering rather than reconstructed after fusion.

**Classification:** enabling rewrite; prerequisite for PERF-002, PERF-005,
PERF-014 and the region accounting appended to `optimizations.md`.

## PERF-002 - Fused execution ops with intermediates in native locals

Raised by the dispatch hunter (its lead rewrite) and the codegen hunter
(typed numeric execution inside its PERF-014 optimizer).

**Bottleneck:** every arithmetic instruction independently fetches, selects a
handler, addresses operands, checks types and writes a tagged result to
`State.stack` (`frame.rs` arithmetic handlers and dispatch loop). For
`sum = sum + i * 2 - 1` the codegen already uses constant operands and writes
the result straight to `sum`, so no move problem is left: what remains is
three dispatches, repeated tag handling and intermediate stack writes. For
`total = total + entity.health`, `reg_get_field` writes the field into a
register and the add reads it back.

**Redesign:** a lowered execution ISA with a finite family of fused handlers
(arithmetic chains, field-read-and-add, field updates, indexed read then
arithmetic), keeping intermediates as `f64` locals and composing existing
primitives such as the own-field cache probe (`eval_index.rs`). Needs a real
lowering pass with control-flow targets, operand dependencies, capture
information and temporary liveness. A vector of decoded micro-ops interpreted
one by one would keep the bottleneck. The codegen hunter adds that a compiler
can prove the induction value and an accumulator numeric through a loop, so
the fused forms can skip tag tests.

**Payoff:** removes dispatches, ip updates, operand decode, temporary
store/reload and repeated type checks together; the mixed workloads contain
arithmetic chains and field-read accumulations.

**Risks:** each constituent keeps its own charge and error position; no
upfront charging of a whole expression, no reassociation, no FMA. Failed
guards resume original semantics before effects commit; partial exits need
the intermediate state and original instruction position. Captured locals,
allocation and metamethod calls are materialization boundaries; values held
only in native locals are absent from the root inventory, so they must be
restored before allocation or re-entry. Backward-transfer interrupt polls stay
placed.

**Classification:** rewrite, dispatch hunter's highest conviction. Subsumes
`optimizations.md` "Compiler-recognized field-update fusion". Afterwards, if
it works, broader instruction-description machinery is a follow-up.

## PERF-003 - Threaded dispatch with guaranteed tail transfers

Raised by the dispatch hunter.

**Bottleneck:** one central opcode match (`frame.rs`). Threading could give
handlers distinct dispatch sites and better opcode-transition prediction, and
an execution stream of handler targets would remove opcode-to-handler
selection.

**Redesign:** handlers transferring by guaranteed tail call, a compact calling
convention and a small set of live execution variables. Handler addresses stay
internal execution data, out of Lua function identity and serialized
bytecode.

**Risks:** source cannot tell whether prediction, ip dependencies or operand
traffic dominate. Ordinary function-pointer calls back to a loop add
call/return; recursive handler calls without guaranteed tail transfer break
the flat native stack, and every compiler/target combination needs the real
constant-stack guarantee. Threading still dispatches once per instruction;
fusion (PERF-002) reduces how often.

**Classification:** rewrite, medium conviction, ranked behind PERF-002 and the
register window. The hunter also says the 64-bit encoding should be redesigned
around the fused handlers' operand needs, not narrowed first: the register
limit (`limits.rs`) and literal indices exceed eight bits, `MAX_FRAME_SLOTS`
does not bound temporaries, and a universal 32-bit word would lose programs or
move complexity into extensions.

## PERF-004 - Remaining constant operand and loop-step forms

Raised by the codegen, dispatch and lookup hunters. Predicate lowering,
number/string/nil literal compare-and-branch forms, `t["k"]` routed to the
field path and the rotated generic `for` (TFOR_LOOP as the polled back edge)
have landed; this is what was left.

**Bottleneck:**
- A dynamic string key (`t[name]`) uses uncached `GET_TABLE` even when the
  site sees one interned key every time.
- Numeric-literal indexing (`t[1]`), literal values in field stores
  (`t.k = 0`) and value-form comparisons (`local b = x < 2`) still load the
  literal into a register.
- An ordering comparison that must jump when true (`or`'s left operand, a
  `not` in a condition) is a compare-branch over a `JMP`: two dispatches.
- A comparison with a literal on the left (`2 < n`) still loads it; it is
  not swapped, because the type error names the operands in source order.
- `TFOR_CALL` and `TFOR_LOOP` are still two dispatches per iteration.
  Fusing them touches the `TFOR_CALL` arm, whose frame is live across native
  re-entry (reference/performance.md "Shared vararg arena").
- The register-register comparison arm still goes through `compare_vals`;
  only the number-literal arm has an inline IEEE fast path.

**Redesign:** a guarded per-site key cache for dynamic string keys reusing a
field plan; literal operand forms for the remaining sites where a workload
shows them; a fused generic-for step only with `native_bytes_per_level`
measured before and after.

**Risks:** cache slots are per family and capped at 255 per function; the
frame-size history of the `TFOR_CALL` arm.

**Classification:** local changes, smaller ceiling.

## PERF-005 - Numeric-for controls rechecked every iteration

Raised by the dispatch and codegen hunters.

**Bottleneck:** `FOR_LOOP` (`eval_control.rs`) reloads and tag-checks three
hidden control registers each iteration. The compiler knows normal source
cannot overwrite them, but the verifier proves bounds, not setup dominance or
control-register integrity, and snapshot-admitted code can skip setup or
overwrite controls.

**Redesign:** stronger dataflow validation proving control-register
integrity for optimized execution, unboxed controls and direction-specific
loop forms; the visible loop variable shares the induction value only when
reassignment and capture analysis allow.

**Risks:** needs a proof or fallback for snapshot-admitted bytecode.
Converting the loop to an integer trip count changes floating-point
behaviour.

**Classification:** local (dispatch hunter), part of the PERF-014 rewrite
(codegen hunter).

## PERF-006 - Accounting owned by the invocation, not flushed at every frame transition

Raised by the cost-charging hunter.

**Bottleneck:** the pending counter belongs to `Frame::run`, which drives many
Lua frames, yet calls and returns flush it (`frame.rs`) even when control stays
in the same dispatch invocation. `TFOR_CALL` flushes eagerly before discovering
that the recognized `next`/`ipairs` path completes without a callback
(`eval_control.rs`), so `for _, v in pairs(t) do sum = sum + v end` settles its
one pending add every iteration. `local_cost` also travels through table and
metamethod resolution so a deeper helper can flush it.

**Redesign:** an invocation-owned accounting context. Lua-to-Lua calls, tail
calls, returns and recognized iterator fast paths keep the same owner.
Publish complete accounting to `State` before arbitrary Rust runs, reload it
after (the callback may have charged, reset the budget or made nested calls),
and publish on escape before budget diagnostics or host error callbacks. The
boundary follows actual independent access to the meter. Make the
resolver's pending-call outcome carry the continuation that synchronizes
accounting, and keep meter ownership out of helpers that only do direct work.

**Payoff:** avoids short batches in method-heavy code, recursive Lua and
generic iteration; fewer occasions where local execution state becomes shared
state passed through helpers.

**Risks:** the mid-run reset contract (`vm.rs`) makes reload after Rust
essential; error notification ordering. The extra context must stay compact,
since dispatch stays live across native re-entry. Retain the iterative call
stack and stack-cap admission.

**Classification:** rewrite, complementary to region accounting (appended to
`optimizations.md` "Cost accounting on every arithmetic op").

## PERF-008 - Rust library calls go through the host stack protocol

Raised by the calls hunter (its third rewrite) and the host-boundary hunter
(the callback half of its first rewrite).

**Bottleneck:** the native ABI is `fn(&mut State) -> Result<u8>`
(`lua_val.rs`). Before a tiny body, `begin_call` removes the function slot and
shifts the arguments; `invoke_rust_fn` changes stack-bottom and active-function
state; the callback reads arguments through relative stack APIs, clears or
reshapes the stack and pushes results; `invoke_rust_fn` validates the count and
copies the result suffix over arguments and temporaries; the caller adjusts
result count and restores its extent. The math wrappers (`math.rs`) do
argument checking, another indexed read, `set_top` and `push_number` around
one arithmetic operation. `invoke_rust_fn` checks the floating-point
environment after every successful return, including trusted leaf math
functions, although `RustFnId::is_library` (`rust_fn.rs`) already lets
`check_usable` skip repeated reads inside library functions. Host callbacks
using `check_type` then `to_number` validate twice.

**Redesign:** a native call context with explicit argument indices/count and a
result destination or writer: one-value functions return a value, multi-result
functions return a rooted span. Arguments stay where the caller put them. The
registry keeps the same `RustFnId` identities but associates execution forms:
trusted leaf library functions, resumable library operations, arbitrary host
functions. Leaf entries that cannot run host code skip arbitrary-host return
machinery (catalog membership alone is not enough: `print` is a catalog
entry). The host hunter adds: simple host callbacks receive decoded scalars and
borrowed string/table views and return values directly; callbacks that need
Lua re-entry get a broader execution capability. Wrapping today's push/pop
implementations in a context would keep most of the materialization.

**Risks:** the stack cap. Removing the function slot gives callbacks one more
usable slot today (`optimizations.md` records the retained-callee variant as
rejected for this reason); the new context must keep logical capacity and
exact overflow errors independent of physical placement. Also method argument
numbering, missing-versus-nil arguments, result-count validation, poisoning,
callback frame isolation. FP checks can be consolidated over VM-controlled
marshalling but not hoisted across host code.

**Classification:** rewrite.

## PERF-009 - Heap-writing library functions still enter as callbacks

Raised by the table-and-math-libraries hunter. Narrowed: every `math`
function, `type`, `rawequal`, `rawlen`, one-argument `tonumber` and `rawget`
are now library leaves executed in call dispatch by function identity
(`vm/leaf.rs`; `library_leaves` workload 6.3 s to 3.4 s on bygg).

**Residue:** `rawset`, append `table.insert`, `table.remove` and `select`
still pay the callback ABI. They write the heap (growth, allocation, write
barriers, metatable checks) or produce open results, so a leaf must reproduce
more mid-operation refusal points than the pure set did; the leaf contract in
`vm/leaf.rs` (decline before any side effect, then the callback's order of
poll, charge, work, slot checks and writes) applies unchanged.
`pairs`/`ipairs` loops are already special-cased in generic `for`.

**Not pursued:** dedicated guarded opcodes (Luau FASTCALL style). Over the
identity path they would only skip the call-site record, the call window and
one out-of-line call; the cost flush must stay (folding the library charge
into batched opcode cost would move a budget failure), and they would change
persisted opcode encodings.

**Classification:** rewrite, medium conviction.

## PERF-010 - Register window nil-filled on every call and return

Raised by the calls hunter.

**Bottleneck:** at CALL `call_window` (`frame.rs`) truncates the caller's
suffix after the arguments; admission (`eval.rs`) nil-fills the callee's whole
remaining window; a fixed-result return resizes back to the caller's top and
nil-fills the restored suffix again. A small call pays initialization in
proportion to callee frame width and the caller suffix it replaced. The value
vector serves as register storage, active extent, native argument/result area
and GC root list at once. Varargs are copied into a shared arena
(`collect_varargs`) and copied back on use (`reg_vararg`).

**Redesign:** separate backing capacity, logical height for limit behaviour,
observable register ranges, and initialization/GC-root state. Derive an
initialization plan from compiler/admission dataflow: initialize missing
parameters and registers readable before assignment only; restore the
caller's logical extent without writing nil across it. Use derived execution
metadata, not the persisted instruction stream. Varargs could stay as a rooted
span, but locals need distinct storage and hidden varargs live outside the
capped stack today, so this needs a full layout and accounting design.

**Risks:** stale storage must stay semantically nil and must not become an
extra GC root (the collector visits the whole live stack slice,
`inventory.rs`); clearing fewer roots can collect objects earlier; both are
observable. The verifier does not establish definite initialization, and
snapshot bytecode can read an untouched register and get nil today.

**Classification:** calling-convention rewrite, lower conviction than the
activation and continuation rewrites; real frame shapes decide whether the
metadata beats the bulk writes.

## PERF-012 - Calls with independent callee, argument and result locations, then inlining

Raised by the codegen hunter (its second rewrite); the host-boundary hunter
asks the calls scope for the same explicit operands.

**Bottleneck:** call generation (`codegen.rs`) puts callee and arguments in a
fresh consecutive block and the result starts at the same base, so
`sum = f(sum)` copies locals into the window and moves the result back. The
runtime enforces it: the call window truncates the stack after the arguments,
the callee uses the window as its base, and return closes captures and slides
results into the function slot before the caller's assignment move. The
verifier requires adjacent open producers and consumers. The upside of today's
convention is that argument slots become parameter slots, so a naive
gather-into-buffer call would trade copies for copies.

**Redesign:** a call describes the callee, the argument values or ranges, and
the result destinations. Caller storage survives independently of the callee
window; producers coalesce with parameter storage where possible and other
local arguments are copied during admission, not by `MOVE` dispatches; fixed
results land in their final destinations after captures close; dynamic result
lists are explicit ranges. Rewrite admission, return continuations and the
open-results protocol together. Then exploit known callees (immutable local
function bindings) and inline small proven helpers (`local function add(a, b)
return a + b end`), which exposes the body to the caller's type and allocation
analysis. The host boundary wants the same: no materialized
`[callee, arguments...]` that the next stage removes or rearranges.

**Risks:** inlined calls keep logical call metadata: depth and stack-cap
refusal where the original call would refuse, function-entry interruption,
tracebacks, error and print locations naming the logical callee, captures,
varargs, result adjustment, tail calls. Lua calls stay on the explicit frame
stack; a Rust call per compiled Lua function would break the flat native
stack. Closure creation and literal initialization stay until separately
proven removable.

**Classification:** rewrite, high conviction.

## PERF-014 - Whole-function optimizing compiler

Raised by the codegen hunter (its lead rewrite).

**Bottleneck:** storage is committed too early. The parser assigns physical
local slots, the AST carries them, and the generator walks statements once
with declaration-ordered locals and a LIFO temporary allocator (`codegen.rs`,
`ast.rs`, `parser.rs` function finalization). `Val` descriptors delay some
decisions within an expression but stop at statement boundaries, and there is
no dataflow or register-allocation pass. Consequences: no constant or type
propagation through locals and joins; aliases occupy distinct slots; multiple
assignment always stages in fresh registers then stores (`a, b = a + 1, b + 1`
emits two arithmetic ops and two moves); every upvalue read is a new
`GET_UPVAL`; repeated pure computations stay separate; no loop-invariant
treatment; captures have one conservative `captured` property. `may_execute`
(`ast.rs`) treats every suffix chain conservatively, and operand preparation
treats every captured local as writable by later code.

**Redesign:** a CFG with SSA values and explicit effects (values with known
types/constants; reads and writes of mutable cells including upvalues;
operations that can call Lua or Rust; operations that can fail, allocate or
expose state; original charge sites, source locations and interrupt points),
then propagation, copy coalescing, CSE, loop analysis and destination
allocation before instruction selection. A nested-function analysis can prove
many captures immutable. Feeds the typed fused forms of PERF-002 and the
numeric-for proof of PERF-005.

**Risks:** a dead result can still owe a charge or type error; hoisting cannot
hoist a charge or a failure; an eliminated computation keeps the charged
prefix under every cost model; captured values reflect the completed prefix
when an error closes upvalues; unboxed values materialize at calls, escapes and
diagnostic boundaries.

**Classification:** rewrite, the codegen hunter's highest-conviction broad
investment. `optimizations.md` "Compile-time table-shape prediction" (b)
defers on the absence of exactly this analysis.

## PERF-015 - Inherited reads re-resolve every table on the path

Raised by the lookup hunter; narrowed after a build and measurement.

Residue: an inherited read through more than one handler table walks it
in full each time (`walk_index`, `metamethod.rs`); the method cache
covers only a raw hit in the first handler table. A validate-on-hit route
cache for deeper chains was built and measured and did not pay: each hit
still resolves every table on the path to compare its live links, so it
saves only the keyed scans (numbers at `MethodLookupCacheEntry`,
`caches.rs`). The access-plan rewrite with a dependency registry beneath
every table writer is refused on the same measurement: the one-level hit
is already near its floor (`resolve_index` about 12 ns warm on
`examples/calls/method.lua`), and the receiver's own-key absence still
needs its raw probe without shared layouts across tables, which no entry
proposes.

What could still pay, untested:

- A writer-side witness with constant work per mutation instead of
  dependency lists: mark tables when a route is filled through them, and
  move one epoch on any mutation of a marked table (raw ops, host writes,
  `set_metatable`, cached global writes and other bypasses included). A
  hit checks the epoch, guards the receiver, and reads the terminal slot
  live. A write to one marked prototype invalidates unrelated routes;
  ordinary instance writes do not. Baseline workloads: `method_chain`
  (two-key classes) and `method_chain_wide` (map-storage classes), 2749
  and 3111 ms wall at 8f6a6bb on bygg.
- The self-index idiom (`C.__index = C`) resolves the same table handle
  twice in the cold walk: once as the receiver's metatable, again as the
  first handler. Reusing the borrowed table when the handler equals the
  metatable removes one heap resolution per inherited read, with no cache.

**Classification:** local (second bullet), rewrite (first), unmeasured.

## PERF-016 - Global bindings as authoritative cells

Raised by the lookup hunter.

**Bottleneck:** ordinary globals probe the global table per bytecode; builtins
add a value mirror validated by one heap mutation epoch that advances on any
mutable access to `_G` even when no builtin changes; cached non-builtin writes
have a special bypass; every ordinary heap mutation meets the watched-object
comparison (`global_env.rs`, `GcHeap::get_mut` in `object.rs`). It is a
coherence structure imposed by keeping the authoritative value in a movable
table layout while wanting direct builtin access elsewhere.

**Redesign:** assign referenced global names State-local binding IDs at
runtime linking, store values in a dense active-environment array, have
bytecode use the IDs and `_G` lookup map names to the same cells. `_G` keeps
its numeric entries, metatable behaviour, insertion history, tombstones and
sealing; reserving an ID for an absent name creates no logical entry.
Restricted environments exchange active values and logical contents keeping
`_G` identity. Requires breaking the context-free table access abstraction for
global storage, or a heap-aware storage view; another mirror would keep the
problem.

**Payoff and ranking:** direct reads for arbitrary game globals, no mirror
validation, no watched-object check in generic heap mutation. Ranked below
PERF-015 because today's builtin hit is already an epoch compare and array read
and ordinary global hits avoid hashing; extra indirection or costlier generic
`_G` operations could lose. `src/globals.rs` is source-occurrence metadata,
not a runtime stage.

**Classification:** rewrite, medium conviction. Relates to the
`optimizations.md` micro item "Keyed global writes invalidate every builtin
mirror entry", which it would remove.

## PERF-017 - Local lookup-path changes

Raised by the lookup hunter.

Residue: SET_FIELD stores, `__newindex` terminal tables and host `set_table`
now carry their lookup's slot into the write (`Table::store_string_key`
reports the slot a string insert wrote). Still two probes: an absent key on
a receiver that has a metatable, outside SET_FIELD's string path. `reg_set_table`
(`try_insert_table_direct` misses with `get_with_index`, then
`store_absent_key` inserts) and `store_new_key`'s non-string insert look the
key up again inside `Table::insert`. Closing it needs an insert that takes
the miss's probe, which `__newindex` dispatch between the probe and the
insert must not invalidate. The hash part now owns its index
(`vm/table/hash_slots.rs`, a hashbrown `HashTable` beside the slot
vector), so a miss could carry an insertion hint validated by the version
and slot count, as `HashProbe` already does for absence. Unmeasured.

(The 255-per-family cache cap was weighed and kept; the reasoning sits at
`assign_cache_slots`. Key-checked position reuse across receivers is in
`optimizations.md` "Shape-based polymorphic field IC"; literal string keys
already take the field path; dynamic string-key sites are in PERF-004.)

**Classification:** local.

## PERF-019 - Local table-storage changes

Raised by the table-representation hunter.

The migratable-key counter (`hash_migratable`), the probe-free nil answer for
array keys outside the window, and the all-dead `#t` shortcut landed; a border
cache was declined at `array_len`. What remains:

- **Reserve fixed positional constructors across batches.** Only constructors
  with more than `MAX_TABLE_BATCH_SIZE` (255) positional fields have a second
  `SET_LIST`; geometric growth then copies about N slots and over-allocates.
  Carry the fixed total on the first `SET_LIST` (after that batch's fields are
  evaluated, not at `NEW_TABLE`) as a capacity-only hint: never through
  `prepare_list_append` (which moves the window and fills nil tails), separate
  from `vlen`, and discarded on overflow or reservation failure so no visible
  effect or error moves. Charging and admission stay per batch; migration and
  computed keys may still outgrow it, and `reset_window` after an all-nil
  first batch shrinks it away (keeping it is a separate decision).
- **O(1) `#t` for stack-shaped tables.** Push/pop leaves a structural nil tail,
  so every `#t` binary-searches `1..=vlen`. A maintained proof that the live
  array keys are exactly `1..=array_live` would answer `array_live` (the only
  border, so not replay-visible); tail push/pop keeps the proof, any other
  mutation drops it to the search, and load recomputes it.

**Classification:** local.

## PERF-036 - One-capture closures still allocate a capture list

Raised while closing the closure-allocation entry. A capturing closure now
costs one allocation, its `Arc<[UpvalueRef]>` capture list (`reg_closure` in
`registers.rs`). An empty / one inline `UpvalueRef` / shared slice
representation in both `Closure` and `Frame` would remove it for the
one-capture case (the `closure` workload's shape), with the same cell
identity. Select the representation once on entry to `run_frame` and borrow
the inline word as a one-element slice, so `GET_UPVAL`/`SET_UPVAL` keep
indexing a slice. The cost is a wider `Frame` written per call and a slice
kept live in dispatch; measure `native_bytes_per_level` and the call-heavy
workloads against the allocation saved.

**Classification:** experiment.

## PERF-037 - `CLOSURE` finds its runtime by a pointer-keyed map lookup

Raised while closing the closure-allocation entry. `reg_closure` resolves the
nested chunk's runtime through `State::bytecode_caches` keyed by bytecode
address (a hash lookup per closure created). Direct child indices on the
parent runtime (a per-runtime vector of the nested runtimes, filled lazily,
since eager bundles would intern literals the continuous State never
interned and change `%p` identities and save membership) would make it an
index. Entry lifetime is still decided by the cache sweep, so a child slot
must not keep a dropped runtime alive or name a stale one.

**Classification:** experiment.

## PERF-025 - Callback gsub replacements copy the returned value and captures

Raised by the strings hunter. The string and number replacement half landed
(`gsub_with_string`: one borrow for the whole scan, replacement read in
place, no template, no plan cache); what remains is the callback half.

**Bottleneck:** `append_function_replacement` and the table arm of
`append_gsub_replacement` (`string.rs`) copy the returned string into a
temporary `Vec` (`bytes_with_default_string_coercion(-1)`) before appending
it, and `gsub_lua_pattern` still copies the matcher's captures into a `Vec`
per match for those paths.

**Redesign:** append the returned bytes straight from the stack slot before
popping it, and hand the callback paths the published captures without the
copy. Keep the matcher-live-across-callback frame cost in view
(notes/optimizations.md, "Lua-pattern gsub keeps the matcher live across the
replacement call"): any new local must not grow the re-entry cycle.

**Risks:** callback effects survive a later failure; the copy is per match
against a Lua call per match, so the payoff is small and unmeasured.

**Classification:** local, low conviction.

## PERF-029 - Certified fast phase for transcendental math

Raised by the table-and-math-libraries hunter.

**Bottleneck:** the kernels already have cheap approximation, rounding
certification and accurate fallback, with FMA selected once at kernel entry,
so there is no per-FMA selection to remove; `sqrt`, `floor`, `ceil` and `abs`
have no algorithmic headroom (their gain is PERF-009). Cosine still does range
reduction and double-double reconstruction through `cos_fast`
(`crmath/cos.rs`).

**Redesign:** a cheaper certified fast phase for common game-domain inputs,
with the existing kernels as fallback, worth it only if a sound error interval
certifies the binary64 answer often enough. A joint sine/cosine kernel sharing
reduction and reconstruction, certifying each output separately, where scripts
compute both for one angle; compiler fusion would have to keep identity
checks, evaluation order and both charge points.

**Risks:** new bounds and certification work; cannot be justified by visual
accuracy or small ULP error. Payoff depends on input distributions.

**Classification:** numerical rewrite, lower conviction; after PERF-009.

## PERF-031 - Host calls into Lua assembled from independent stack operations

Raised by the host-boundary hunter (the invocation half of its lead rewrite;
the callback half is PERF-008).

**Bottleneck:** each numeric argument push checks usability and stack space;
calling an anchor looks up the value, inserts it below the arguments and enters
the general call machinery; results are normalized onto the stack
(`collect_returns`), read through another public operation each, then popped
(`stack.rs`, `anchor.rs`, `eval.rs`). Pushing two numbers, calling, reading one
number and popping makes five public usability checks, and on x86-64 the
floating-point environment check is an out-of-line control-register read
(`check_usable` in `vm.rs`, `read_control` in `fp_env.rs`).

**Redesign:** an invocation interface owning the whole transfer: a state-bound
entry identifies the callee, a built-in argument encoder writes into the final
call window, execution returns a result span or decodes into the requested Rust
result, and one owner handles operand cleanup, result normalization, errors and
panic poisoning (conceptually `state.invoke(&mut tick_context, &entry,
(entity_id, dt))`). An outermost invocation decodes from the returned span
before the frame is destroyed. A tuple wrapper over `push_number`,
`call_anchor`, `to_number` and `pop` would keep most of the cost. Needs the
explicit callee, argument-span and result-destination operands of PERF-012.

**Risks:** result views stay rooted until decoding ends and block
incompatible mutation or re-entry; upvalues close before their source slots
are destroyed or moved; typed decoding keeps accepted inputs and exact errors
(no automatic exact-arity enforcement); FP checks consolidate only over
VM-controlled marshalling; charges and callback-visible cost settle at today's
boundaries, and batching several entity invocations keeps individual failure
behaviour.

**Classification:** rewrite, high conviction; greatest for short scripts and
frequent small callbacks.

## PERF-032 - Borrowed per-tick game context for host callbacks

Raised by the host-boundary hunter.

**Bottleneck:** user data is `Option<Box<dyn Any + Send>>` with `'static`
accessors (`vm.rs`) and registered functions are plain function pointers, so a
callback cannot receive a temporary borrow of the world's tick state. An
engine where the world owns execution is pushed toward shared ownership,
interior mutability, moving context in and out, command collection or copying
tick data into Lua (integration consequences, not costs proven in a host).
Even with data inside `State`, borrowing Lua string bytes through `to_bytes`
borrows `State` and blocks `user_data_mut`, forcing a copy; `raw_field`
conversions (`raw_field.rs`) return owned `Vec<u8>`/`String`. The downcast
itself is not the prize.

**Redesign:** durable `State` plus a borrowed tick context passed at
invocation; callback entries keep identity, diagnostic name and thunk; an
argument view borrows the VM heap while the callback mutates the context. A
typed registration layer adapts at the thunk, so the interpreter need not be
generic over the world type. `with_string_args` (`string_arg.rs`) already
splits strings, cost accounting and interruption this way.

**Payoff:** removes copying and synchronization at every host query, possibly
more than PERF-031; lets many States run sequentially against one world
borrow.

**Risks:** re-entry: an exclusive world borrow must be suspended and
reborrowed explicitly across Lua re-entry; borrowed Lua data cannot outlive
arbitrary VM mutation; world changes keep their order and visibility
(deferred commands are not transparent). Implement with PERF-031.

**Classification:** rewrite, high architectural value; payoff depends on host
integration. Related to `todo.md` "Typed `State<U>` for user-data", which
addresses only the downcast.

## PERF-033 - Host global and field access resolves names and tables afresh

Raised by the host-boundary hunter.

Narrowed: borrowed `&str`/`&[u8]` field reads landed; global-binding
descriptors and a record view were refused (reasons at `read_name` and
`raw_field`). What remains is unmeasured.

**Bottleneck (hypothesis):** each `raw_field` hashes the field name, probes the
string pool and probes the table. A callback decoding many scalar fields of
consistently laid-out records (more than the inline capacity, so a map probe)
pays all three per field per call.

**Redesign:** a prepared field descriptor holding the name's bytes plus a
`Cell` slot-index hint, kept in user data (`user_data` hands out `&T`). A hit
reads the key at the hinted index and compares its bytes with the name; a miss
takes today's lookup and refreshes the hint. Sound across States, snapshot
loads and fresh tables without rooting the name. `check_usable` stays per
read.

**Precondition:** a registered host-traffic workload (the bench harness
registers no Rust function today) that shows the lookups. Without it the
saving is one short hash and two probes per field.

**Classification:** micro, low conviction until measured.

## PERF-034 - Restricted environment rebuilt on every entry

Raised by the host-boundary hunter.

**Bottleneck:** each `with_restricted_env` entry (`vm.rs`) builds a new
`Table`, resolves the whitelist names, copies their values in and swaps it into
the global table; exit swaps back. Both swaps advance the table version,
invalidating global probes and other caches naming it (`restrict`/`resume`,
`global_env.rs`).

**Redesign:** restricted contents as a prepared ordered backing view with
invocation-local writes, selected at entry and discarded at exit; unchanged
visible bindings shared, not rebuilt; cache validation names the active
backing view rather than going cold on entry and exit. It cannot be a second
environment consulted only by global bytecodes: `_G`, retained aliases, raw
operations and iteration all see the same active contents. Whitelist order,
outer binding changes, nested restrictions and discarded writes stay exact.

**Classification:** full representation rewrite, conditional: near the top if
the game restricts around every tick, irrelevant if restriction is setup-only.

## PERF-038 - Sweep visits vacant slot-map slots up to the high-water mark

Raised by the resident-payload pacing spar. Verified by reading the code. Not
measured.

**Bottleneck:** `GcHeap::collect` sweeps with `SlotMap::retain`, which visits
every slot up to the map's historical high-water mark, occupied or not. A heap
that once held K objects pays K slot visits on every later collection, even
when few objects are live and collections come every few dozen allocations.
Weighted pacing adds a route to a high-water mark: a large live string buys
headroom for many small objects that then die together.

**Redesign:** sweep only occupied slots, freeing in the existing slot order so
free-list reuse and the version stepping `CACHE_PRUNE_INTERVAL` relies on stay
the same. Related: the paged sweep in `optimizations.md` "8-byte NaN-boxed
`Val`".

**Risks:** free-list order is replay-visible through `%p` ids.

**Classification:** local collector change.

---

## Accounts added to existing entries

This hunt's account of these mechanisms went into the entry already filed for
them:

- `optimizations.md` "Register access bounds checks": dispatch hunter's
  verified register window with explicit slow exits.
- `optimizations.md` "Cost accounting on every arithmetic op": cost-charging
  hunter's region accounting.
- `optimizations.md` "Call path after the frame-stack flattening": calls
  hunter's single activation record and VM-owned continuations, with the heap,
  host and codegen hunters' overlapping points.
- `optimizations.md` "8-byte NaN-boxed `Val`": dispatch hunter's compact value
  and heap hunter's typed arenas and paged sweep, with the table hunter's
  `TableStorage` footprint point.
- `optimizations.md` "Shape-based polymorphic field IC": lookup and table
  hunters' shared layouts, and the lookup hunter's key-checked position reuse.
- `optimizations.md` "Compile-time table-shape prediction": codegen hunter's
  constructor plans.
- `optimizations.md` "Owned result buffers are copied again when interned":
  heap and strings hunters' owned-buffer adoption and short-string pooling.
- `optimizations.md` "gmatch: drop the Lua-side wrapper and table-backed
  iterator state": strings hunter's native iterator, failed-search exhaustion
  and pattern-cache sizing.
- `optimizations.md` "Stdlib micro (E)": strings hunter's cached format plan.
- `todo.md` "Sparse storage for the array part's stragglers": table hunter's
  numeric storage separated from the logical array boundary.
- `todo.md` "Grow the brokkr workload registry": host hunter's note that the
  harness warm path exercises no host traffic.
