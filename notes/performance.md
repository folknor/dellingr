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

A separate unsaved execution stream (derived from verified bytecode, for
fresh compiles and snapshot loads alike) is this entry's to build, not a
prerequisite landed ahead of it: the comment on `BytecodeRuntime` in
`src/vm.rs` records why. Its case here is that fused forms stay out of the
persisted opcode set and out of what snapshot admission must accept. It owes
what the hunt named as constraints: admission keeps the logical frame
reservation and refusal point (`eval.rs` call admission plus
`FRAME_HEADROOM`); values held only in native locals or in a shorter-lived
register change GC roots and so later `%p` rendering; and a fused op that can
fail or run out of budget at either constituent must identify the
constituent reached and its charged prefix. `CallInfo.ip` can stay canonical
if the executor records the canonical call site at the boundary. PERF-014
and region accounting need not wait on such a stream: proof
metadata keyed by canonical pc, an IR emitting existing opcodes, or canonical
block boundaries priced per State are each candidate shapes for them.

**Numeric-for loop regions.** The per-iteration tag check of the three hidden
`FOR_LOOP` controls is not removable on its own: a verifier proof is invisible
to the `Val` payload match in the canonical handler, and admitted bytecode
must never reach a panic (comments at `instr_for_loop` and the verifier's
`FOR_LOOP` arm). Its payoff comes only here, as a typed region that runs a
whole proven loop with the controls, induction value and accumulators as
`f64` locals: `bench/arithmetic`'s `for i = 1, 50000 do sum = sum + i * 2.5 -
i / 3.0 end` drops five dispatches per iteration, the control tag checks, the
duplicate induction write and the boxed temporaries. The proof that selects
the region (canonical handler as fallback for anything it does not cover)
must show the successful `FOR_PREP` edge dominates it, cover every register
write in it including open-result spills, exclude writes through an open
upvalue a child closure took on a control slot, and account for reads of any
slot whose write is elided, after the loop included. Numeric-only locals add
no GC roots and no snapshot fields (saves are quiescent). A guarded exact
countdown is sound for step exactly `1.0` with finite integral endpoints
within 2^53 - 1: count `limit - start` in `i64`, keep the original `f64` start
for the first iteration (`-0.0`), poll at the canonical `FOR_LOOP` pc before
publishing the next value, and leave a reassigned visible variable alone; all
other inputs take the general float path, so docs/compatibility.md's f64 loop
semantics hold.

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

Raised by the codegen hunter (its second rewrite).

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
analysis. The host boundary does not need these operands: `State::invoke`
pushes the callee below its arguments, which already is a Lua callee's final
window (reason at `vm/invoke.rs`).

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
many captures immutable. Feeds the typed fused forms of PERF-002, its
numeric-for loop regions included.

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
  and heap hunter's typed arenas, with the table hunter's
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
