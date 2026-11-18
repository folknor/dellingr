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

**Landed part** (`src/vm/numeric_loop.rs`, reasoning in its module comment
and at `State::try_numeric_leaf`; numbers in `reference/performance.md`):
- Numeric-for loop regions over an `f64` mirror, keyed by the canonical
  `FOR_PREP` pc and selected by an entry guard on every register read before
  written. Their bodies take arithmetic, `UNM`, `MOVE`, `LOAD_NUM` and
  loop-invariant field reads (`GET_UPVAL r; GET_FIELD r r k`, and `GET_FIELD`
  on a register the body never writes), each field read once at entry as a
  raw present number.
- Numeric leaves: a function whose code up to its first `RETURN` is that
  arithmetic over its parameters, returning one value, runs inside
  `call_from_lua` without an activation when its arguments are numbers.
  Every admission check is made first and declines rather than fails. This
  was the argument-guarded specialization the call-site hunt asked for; the
  tag test it named was never the cost, the activation was.
- Loop regions calling numeric leaves through a global or upvalue
  (`GET_GLOBAL`/`GET_UPVAL r`, then `CALL r` with fixed arguments and one
  result). The callee is read once at entry: the per-iteration identity
  check this entry once asked for is unnecessary, since nothing the region
  or a leaf runs can rebind it. Each call polls as its admission would and
  hands off to canonical dispatch on a set flag or a possibly refused leaf
  charge.
- Field leaves: a leaf body may also read and store string-keyed fields of
  a table parameter or an upvalue table and may return nothing (the method
  shape `self.count = self.count + n`), resolved to live slots at entry
  (`State::resolve_sites`). bench/method 3.1 s to 1.7 s. Fusing
  `GET_FIELD; ADD; SET_FIELD` in the dispatch loop or as a persisted opcode
  was weighed and passed over: both keep the activation, which is the cost.

What the landed work taught: canonical-pc metadata with the canonical
instructions as the fallback needs no position map or execution stream;
deferred register writes are exact once the first checkpoint has published
every written slot; free constituents let a fused read sit inside the
per-operation charge path with no refusal between its halves. The exact `i64`
countdown was not built: the f64 add and compare are not where the loop's time
went.

**Residue:** loop bodies with a compare-branch, a store, or a call to
anything but a numeric leaf (nested `f(g(x))`, a method call, open
arguments or results) still run canonically (no registered workload has a
numeric-for body that is a compare-branch plus region operations only; every
one with an `if` also calls or touches a table). Straight-line chains in functions that are not leaves (fib's `n - 1`,
`n - 2`, `+` around its calls) still dispatch per instruction. Field updates
outside a leaf (in a function that branches or calls, or in a loop body
such as object_fields' `u.x = u.x + u.speed` beside `units[i]` and a method
call) still dispatch; inside a loop region a store would need slot identity
hoisted and every value it can reach reloaded, not the region's read-once
fields, and a region still calls neither a field leaf nor a method. Each
needs a workload whose loop or call the landed forms do not already take.
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

Raised by the codegen, dispatch and lookup hunters. Predicate lowering, the
literal compare-and-branch forms, `t["k"]` on the field path and the fused
generic `for` step have landed. Fusing a literal load into its consumer
(`t[1]`, and by extension `t.k = 0`, `local b = x < 2`, `2 < n`) was built,
measured flat and refused; the reasoning and numbers are at `Generator::suffix`
and `cond_jump` in `codegen.rs`. Each remaining item waits for a workload that
shows it.

**Bottleneck:**
- A dynamic string key (`t[name]`) uses uncached `GET_TABLE`. Every registered
  dynamic-string-key site is polymorphic (`rekey`, `fill`'s 3000-key store,
  `object_fields`), so a last-key cache tied to the receiver would only miss;
  `fill`'s one monomorphic read runs once per fresh table. If a workload shows
  hot monomorphic dynamic reads across receivers of one layout, the shape is a
  cached `KeyedIndex` checked with `value_if_key` against the live slot, not a
  field plan: the field cache trusts one key per slot with no key compare.
- An ordering comparison that must jump when true (`or`'s left operand, a
  `not` in a condition) is a compare-branch over a `JMP`; the extra dispatch
  runs only when the comparison holds. A branch-when-true family (RR and RK
  orderings, eight opcodes) keeps every register; no registered workload has
  a hot site.
- The register-register comparison arms decode `op.ordering()` and run
  `partial_cmp` inside the inlined `compare_vals`, where the number-literal arm
  matches the operator directly. Whether that survives optimization needs the
  assembly and a hot RR site; none is registered (`rust_boundary`'s `less` is
  the nearest).

**Classification:** local changes, smaller ceiling; workload-gated.

## PERF-015 - Inherited routes retire together on any route write

Raised by the lookup hunter; the writer-side route epoch landed
(`RouteLookupCacheEntry`, `caches.rs`; numbers in
`reference/performance.md`, "Inherited routes under a writer-side
epoch"). The cold self-index reuse was refused (reason above the loop in
`walk_index`).

The terminal table is no longer marked, and a warm cached SET_FIELD
overwrite of a live slot leaves routes standing
(`GcHeap::overwrite_live_slot`; numbers in `reference/performance.md`,
"Route-preserving writes to class tables").

Residue: every other mutable handout of a link or intermediate table
still retires every route, including live overwrites that could not
change one: an uncached `store_field` overwrite (a polymorphic store
site), SET_TABLE with a string key, `rawset`, host writes, and the
global table's `write_cached` (only when a route passes through `_G`).
Covering them means classifying mutations instead of handouts: a
conservative default on the editor plus explicit proven-overwrite
entry points, so no caller can bypass retirement by accident. The
measured thrash on a two-table route was about 12% of the warm call
(355 to 312 us); none of the remaining paths is in a registered
workload's loop.

**Classification:** rewrite, unmeasured.

## PERF-042 - Host field reads pay admission and argument resolution per field

Raised while landing `FieldName` slot hints for `State::raw_field`.

**Bottleneck (hypothesis):** a callback decoding N fields of one table calls
`raw_field` N times, and each call runs `check_usable` (an out-of-line
MXCSR read for host code on x86-64), checks the argument index and resolves
the table. With descriptors the lookups are cheap; the `host_records`
workload still spends about 150 ns of process wall per eight-field decode
(6.7 s over about 46 million decodes, bygg, Lua call overhead included);
how much of it is per-field admission is unmeasured.

**Redesign:** a sealed batch read, `raw_fields::<(f64, u32, &str)>(arg,
(&X, &PLAYER, &KIND))`, resolving the table and checking the environment
once, converting in tuple order and reporting the first failing field with
the scalar read's error. Sound within one call: no host code runs between
the reads (the per-read check exists because host code may change the
floating-point environment between separate calls). The tuple machinery has
a precedent in `State::invoke`'s sealed tuples.

**Measure on:** `host_records`, scalar against batch in one binary.

**Classification:** local, unmeasured.

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
  harness warm path exercises no Rust-to-Lua argument or result traffic.
