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
(typed numeric execution inside its whole-function optimizer, refused as a
compiler pass; the reasoning is at the `codegen.rs` header).

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
if the executor records the canonical call site at the boundary. Region
accounting need not wait on such a stream: proof
metadata keyed by canonical pc, an IR emitting existing opcodes, or canonical
block boundaries priced per State are each candidate shapes for it.

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
per read; 6 of the loop's 19 dispatches are `GET_UPVAL` of one capture,
and a fused `GET_UPVAL`+`GET_FIELD` that still reads the cell and stores the
scratch register at each site removes them with every root kept; compiler-side
caching of the capture was refused, see the `codegen.rs` header) and field
updates. None of these has had its benefit established;
each needs a workload whose loop the region does not already take.
One stable shape for a two-op fusion: after a handler, test the actual next
opcode and enter its body directly inside the dispatch loop. That is the
predecessor-specific dispatch threaded dispatch was after, without its
native-stack hazard (threading itself is refused at `run_frame`). The
dispatch hunter's encoding note also lands here: redesign the 64-bit word
around fused handlers' operand needs rather than narrowing it first.

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

**Redesign.** Replace the central opcode match with handlers that tail-call
the next handler. Distinct dispatch sites give the branch predictor a site
for each predecessor handler. An internal execution stream of handler targets
would also remove opcode-to-handler selection while keeping handler addresses
out of Lua function identity and serialized bytecode.

**Deferred** until Rust's `become` guarantees tail calls on every supported
toolchain, including the MSRV set by `rust-version` in `Cargo.toml`, and
works in dev builds without growing the native stack per instruction.
Commit `2d322db` recorded that `become` was unstable and that ordinary
handler calls survive in dev builds. A single surviving call on a handler
cycle grows the native stack as instructions execute. A handler-pointer table
called from a loop keeps one shared indirect jump site and adds a call and
return per opcode.

**Reopen conditions.** The `run_frame` comment records per-ISA assembly as
another way to guarantee tail transfers. Either implementation requires a fresh
measurement attributing branch misses to the dispatch branch before work
begins. The stable-Rust route that tests the actual next opcode and enters
its handler directly lives under PERF-002.

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
typed-region work (PERF-002); the plan is designed by that consumer.

**Classification:** rewrite, residue unmeasured.

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

## PERF-041 - A map re-add probes the index twice

Raised while closing the double lookup of stores through metatables.

**Bottleneck (hypothesis):** re-adding a tombstoned key into map storage
(`insert_into_map_with_tombstones`, non-compacting branch) finds the key's
index entry with `map.find`, then `HashSlots::relocate` probes the hashbrown
index again to retarget that entry at the new back slot. Nothing between
the two touches the index (the slot vector grows, the index does not
reserve), so the first probe's bucket (`HashTable::find_bucket_index`,
`get_bucket_mut` in the pinned hashbrown) could be carried into the
retarget. The `rekey` workload is this path. Unmeasured.

**Classification:** local.

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
