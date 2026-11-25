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
  arithmetic over its parameters, returning one value or none, runs inside
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
- Branches and indexed reads in regions without calls: forward number
  compare-branches and `JMP`s, and raw element reads of a table read once
  at entry, exiting to canonical dispatch at a non-number element
  (`State::run_control_region`). bench/cull 1.28 s to 0.77 s.
- Method calls and field leaves in straight-line regions: `SELF` on a
  loop-invariant receiver, field leaves, calls keeping no result; method,
  route and field slots resolved once at entry, declined when a leaf store
  key meets a key the region resolved (`State::resolve_callees`).
  bench/method 1.6 s to 0.54 s, method_chain_counter 1.5 s to 0.57 s;
  global about 5% slower, read as layout (reference/performance.md).
- Records in regions without calls: a `GET_TABLE` whose destination is
  next used as a field receiver reads a table element per iteration, whose
  number fields the body reads and stores raw, each store resolving its
  live slot before its `TableWrite` charge and exiting to canonical
  dispatch when the key is not live; a site's slot is reused across
  records of one layout by key-checked position. bench/store_loop 4.2 s to
  1.7 s on bygg.
- Method calls on records, calls beside branches, nested loops and number
  upvalue reads in regions (`State::run_control_region`): bench/method_polymorphic
  2.1 s to 1.4 s, object_fields 3.4 s to 3.0 s, store_loop 1.7 s to 1.5 s;
  cull flat with its outer loop now a region (reference/performance.md).
- Truth tests of record fields (`if u.alive then`, `if not u.dead then`):
  `GET_FIELD d r k; TEST d` on a record runs as one region operation that
  writes the live value of any type to `d` on the stack and drops `d`'s
  mirror slot from publication until a number write (`Layout::record_test`);
  a key not live in the record exits at the `GET_FIELD`. bench/alive_update
  1482 to 1379 ms (3 of 3 matched pairs, bygg, 0288cac5 against the
  landing build); store_loop 1461 to 1484 ms in the same pairs with an
  unchanged executed path, read as layout.

What the landed work taught: canonical-pc metadata with the canonical
instructions as the fallback needs no position map or execution stream;
deferred register writes are exact once the first checkpoint has published
every written slot; free constituents let a fused read sit inside the
per-operation charge path with no refusal between its halves. The exact `i64`
countdown was not built: the f64 add and compare are not where the loop's time
went. After the NaN-boxed `Val`, a record region is only a few percent faster
than canonical dispatch of the same kernel (alive_update 7%): record-shape
residue is worth taking for loops that already run as regions, less as a
headline win on its own.

**Residue:** loop bodies storing anything but a number into a live field
of a record (a store to an invariant receiver, a dynamic-key
`SET_TABLE`), a non-number branch (a truth test of anything but a record
field present raw in its record, so a field the record lacks or inherits
exits; a string or nil compare), a
call to anything but a numeric leaf (nested `f(g(x))`, open arguments or
results, a method that calls or returns early), a
method the record holds itself or finds through a function `__index`, or
more than eight classes at one site still run canonically. A string read
into a register used as a key (column_key's `cs[c]`) stops a region that
holds it at that read; the loop inside it, which reads `us[i][key]`, is a
region. A number key on a record (`m[i][j]`) still declines: the record
read by a key register takes only an entry-checked string. Straight-line chains in functions that
are not leaves (fib's `n - 1`, `n - 2`, `+` around its calls) still
dispatch per instruction. object_fields' remaining time is its
construction pass (`{}`, `new_unit` calls, `SET_TABLE`), which no region
takes.
One stable shape for a two-op fusion: after a handler, test the actual next
opcode and enter its body directly inside the dispatch loop. That is a
subset of what threaded dispatch was after (a predecessor-specific path for
one chosen successor), without its native-stack hazard (threading itself is
refused at `run_frame`). The
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

**Classification:** rewrite, dispatch hunter's highest conviction. Afterwards, if
it works, broader instruction-description machinery is a follow-up.

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
- The heap hunter's compact value and typed arenas both landed; the table
  hunter's `TableStorage` footprint point (every table slot sized by the
  four-pair inline variant) is unfiled residue, and
  `optimizations.md` "Per-kind arenas keep their high-water storage" is
  the arenas' follow-up.
- `optimizations.md` "Field-site position hints: residue": the lookup
  hunter's shared layouts (landed as GET_FIELD's key-checked position hint;
  the shape-id IC refused); the table hunter's shared layouts sit under
  "Shared record layouts: the storage side".
- `optimizations.md` "Shared record layouts: the storage side": codegen
  hunter's constructor plans (the leading static run landed as an
  execution-side fusion; the storage-side layouts remain).
- `optimizations.md` "`string.format` cached plan": strings hunter's
  cached format plan, deferred after the scan and capacity changes and
  refused again once the output buffer became reusable.
- `todo.md` "Sparse storage for the array part's stragglers": table hunter's
  numeric storage separated from the logical array boundary.
- `optimizations.md` "Rust-to-Lua call forms": host hunter's note that the
  harness warm path exercises no Rust-to-Lua argument or result traffic; the
  `invoke_typed` and `invoke_stack` scenarios now carry it.
