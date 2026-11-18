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

**Landed part: numeric-for loop regions** (`src/vm/numeric_loop.rs`). A
numeric `for` whose body is straight-line arithmetic, `UNM`, `MOVE` and
`LOAD_NUM` runs as a typed region keyed by its canonical `FOR_PREP` pc,
selected by an entry guard on every register read before written (no static
proof, no stream, no persisted opcode). `bench/arithmetic` went from 2.9 s to
2.0 s wall on bygg. What it taught the residue: canonical-pc metadata with the
canonical instructions left in place as the fallback needs no position map;
deferred register writes are exact once the first checkpoint has published
every written slot (stale numbers root nothing); an iteration's charges can be
added at once when their sum stays below the flush threshold, with a
per-operation path otherwise. The exact `i64` countdown was not built: the
f64 add and compare are not where the loop's time went.

**Residue:** everything outside such loops. Loop bodies with a compare-branch,
an upvalue read, a field read or a call still run canonically (an `if` inside
the body is the next most common shape); straight-line chains outside loops
(fib's `SUB_RK; CALL` gives nothing, `sum + i * 2 - 1` in a loop with a call
does); field-read-and-add (`same_obj_read`: `GET_UPVAL` + `GET_FIELD` + `ADD`
per read) and field updates. None of these has had its benefit established;
each needs a workload whose loop the region does not already take.

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

## PERF-006 - Generic-for iterator steps still settle the pending batch

Raised by the cost-charging hunter; narrowed when its call and return half
landed.

**Landed:** the pending batch is owned by the `Frame::run` invocation. Lua
calls, Lua tail calls and returns into a Lua caller carry it forward (Lua
admission and return never touch the meter); `call_from_lua` and
`tail_call_from_lua` publish it only when the callee is not a Lua function,
and `Frame::run` settles it on every exit. The rule: pending is zero whenever
code outside the dispatch loop runs. No cost, prefix or golden changed.
Interleaved wall pairs on bygg against b9becfb: method 3.2 and 3.3 s to 3.1 s
in both pairs, arithmetic and tail_call flat (no same-binary control run).

**Residue:** `TFOR_CALL` still flushes eagerly before its `next`/`ipairs` fast
paths, so `for _, v in pairs(t) do sum = sum + v end` settles its one pending
add every iteration. Deferring that flush into the iterator helper or behind a
probe was measured to grow release `Frame::run` (reference/performance.md
"Shared vararg arena"); a retry belongs with the PERF-004 residue's fused
generic-for step, measured the same way.

**Not pursued:** removing `local_cost` from the metamethod resolvers. The flush
must still precede every handler call, so it would only move one level up into
the `reg_*` helpers, which keep the parameter; no meter write is saved. A
dispatch-local cached threshold was measured neutral under PERF-007.

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
A leaf need not take the callback's argument relocation: `table.pack` can
keep its arguments in place, run the callback's `StackSpace(1)` check at the
same logical length, put the new table in the callee slot and truncate after,
saving one argument move per call that grows with arity (reasoning at the
remove in `begin_call`). The same shape fits any heap-writing leaf here.

**Not pursued:** dedicated guarded opcodes (Luau FASTCALL style). Over the
identity path they would only skip the call-site record, the call window and
one out-of-line call; the cost flush must stay (folding the library charge
into batched opcode cost would move a budget failure), and they would change
persisted opcode encodings.

**Classification:** rewrite, medium conviction.

## PERF-012 - Call-site residue: CALL then MOVE continuation, argument-guarded helper specialization

Raised by the codegen hunter (its second rewrite). Narrowed after a spar: an
encoding with independent callee, argument and result locations and splicing
proven helpers into the caller were refused (reasoning at `Generator::call`
in `codegen.rs`). Argument slots already are parameter slots; a result
destination drops a store that is a GC root; exact inlining rebuilds a Lua
activation. The per-activation overhead every Lua call pays (one record instead
of `Frame` plus `CallInfo`, the per-call `Arc` clones) is the
`optimizations.md` call-path entry, not this one.

**Residue 1, `CALL b .. 1; MOVE x b` continuation:** a derived continuation,
selected from admitted pairs (no opcode, no save-byte change), could finish
the return as today (full result-count check, captures closed, caller top
restored with the value at `b`), then write `x` and resume past the MOVE.
Both stores stay; only the MOVE dispatch goes. Must use the actual next
instruction, never assignment syntax (`x, y = f(), x`), and keep the
canonical MOVE for any other incoming edge. One dispatch per call: unmeasured.

**Residue 2, argument-guarded specialization:** a verified prototype could
carry a derived numeric plan selected by guards on its arguments (`x` proven
a number skips the tag test in `return x + 1`), and a typed numeric-for
region could then extend across such a call. Every charge, refusal point,
entry poll and root stays canonical; a leaf body that cannot collect, call
out or capture may skip the physical activation on success and materialize
the logical callee and cursor only on its failure path. Belongs with the
typed-region work (PERF-002, PERF-014); the plan is designed by that consumer.

**Classification:** rewrite, residue unmeasured.

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
many captures immutable. Feeds the typed fused forms of PERF-002; the
numeric-for regions that landed select by an entry guard instead, so a proof
would widen them only to bodies the guard cannot cover.

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
