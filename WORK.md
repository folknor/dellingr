# WORK.md

Current work item. Optimization loop 7: one finalize-time instruction-strip
pass, two consumers.

---

## Target: strip provably dead instructions in `finalize` with a single correct jump remap

### The two consumers

**(a) CLOSE_UPVALUES in closure-free functions (A-O9).** Scope exits emit
OP_CLOSE_UPVALUES unconditionally (`parser.rs` `level_down` + the loop
parsers) - including once per iteration inside for-loop bodies. If a
function body creates no closures (no OP_CLOSURE in its code - note
`chunk.nested` non-empty does NOT imply a closure is created in THIS
function's code path... verify: nested chunks only exist for OP_CLOSURE
sites, so "no OP_CLOSURE emitted" is the precise condition), no upvalue
over its locals can ever be open, and every CLOSE_UPVALUES in it is a
guaranteed-no-op dispatch. Measured: `close_upvalues` shows 1.5M-4.6M
calls on loop-heavy workloads at 2.2-4.0% of instrumented wall (pairs,
composite, factory_closure); the uninstrumented cost is a call + an
empty-list check per loop iteration, everywhere.

**(b) `Vec::remove` in call emission (A-O4).** Every plain call currently
does `remove_instr` -> `code.remove(mark_idx)` (`parser.rs:446-448`,
called from the expression parser's call emission): an O(n) tail shift
per call plus the matching `line_info` shift, making call-heavy chunks
quadratic-ish in emission. The pragmatic fix from the backlog: emit the
mark unconditionally, and when the call turns out not to need it, REWRITE
it to a nop instead of removing it - then let the shared strip pass
delete all nops at finalize time with one remap.

### The infrastructure

A `strip_dead_instructions` pass in `finalize` (`src/compiler.rs`),
running BEFORE `assign_cache_slots` and before verification:

1. Build the removal set: every OP_NOP (new opcode, see below), plus -
   when the chunk contains no OP_CLOSURE - every OP_CLOSE_UPVALUES.
2. Compute the pc remap (prefix sums of removals).
3. Rewrite every jump-carrying instruction's offset: OP_JUMP,
   OP_BRANCH_FALSE, OP_BRANCH_TRUE_KEEP, OP_BRANCH_FALSE_KEEP,
   OP_FOR_PREP, OP_FOR_LOOP, OP_TFOR_LOOP - enumerate from `instr.rs` /
   the verifier's successor logic, not from memory; any opcode the
   verifier treats as a control transfer must be covered. Offsets are
   relative (sBx / signed), so the new offset is
   remap(target) - remap(source_next) style arithmetic - define it
   precisely against the existing jump semantics (ip points at NEXT
   instruction; offsets are applied to that).
4. Drop the removed instructions from `code` AND `line_info` together.
5. Reject (or keep unstripped) any pathological case rather than emitting
   wrong offsets: a jump whose new offset would not fit i16 cannot occur
   from shrinking, but assert it anyway.

**OP_NOP**: a new opcode, executed as a no-op in dispatch (it should never
survive finalize for compiler-produced code, but saved/forged bytecode is
its own world - see below). The verifier must accept it (stack-neutral,
all operands reserved-zero); `analyze_cost` treats it as free; dispatch
executes it as nothing. This keeps old-binary-reads-new-save... actually
NO: a nop that never survives finalize never enters a save. But a FORGED
save may contain OP_NOP - the verifier and dispatch must handle it
anyway. State explicitly whether compiler output can ever retain a nop
(it must not - assert in finalize).

### Order-of-operations question (answer explicitly)

`assign_cache_slots` bakes slot indices positionally; the verifier
validates slot streams in instruction order; the stack-discipline pass
walks jumps. Stripping must happen BEFORE slot assignment and
verification so both see the final code. Confirm `finalize`'s current
pass order allows this, and that `remove_instr`'s existing careful
`line_info` handling has no other callers left after (b) - if the mark
rewrite covers every `remove_instr` use, delete `remove_instr`.

### Semantics and cost

- CLOSE_UPVALUES stripping is observable ONLY through timing - the op is
  free (verify), and closing never does anything in a closure-free
  function. Argue there is no path where an upvalue over this function's
  locals exists without an OP_CLOSURE in its code (host API? varargs?
  metamethods? - none capture locals, but verify).
- `cost_used` must be byte-identical: CLOSE_UPVALUES and the stripped
  marks are free ops (verify both in `frame.rs` dispatch and
  `analyze_cost`), so removal changes no charges.
- Iteration/replay: bytecode SHAPE changes (fewer instructions), which
  changes nothing observable - but `analyze_cost` outputs (ScopeCost own
  totals) may shift if it counts per-instruction anything (verify what
  analyze_cost reports for free ops).
- Snapshot: saved bytecode is post-finalize, so saves carry stripped
  code; old saves with unstripped CLOSE_UPVALUES remain valid (dispatch
  still executes them). FORMAT_VERSION untouched. Forged saves with
  OP_NOP now pass known_opcode - deliberate, dispatch handles it.
- The golden fixture WILL change again (stripped code in saved chunks) -
  regenerate current, keep legacy (same procedure as loop 4).

### Parser-side change for (b)

At the two `remove_instr` call sites in the expression parser, replace
removal with an in-place rewrite of the mark instruction to OP_NOP.
Nothing else about call emission changes; jump targets recorded before
the mark stay valid because nothing shifts at parse time anymore (that is
the entire point). Any parser bookkeeping that stored indices relative to
post-removal positions must be audited - the removal previously SHIFTED
later indices, so removing the removal changes subsequent index
arithmetic; read `expr.rs`/`parser.rs` call emission thoroughly (this is
where the 5ff9a27 "mark the call base after evaluating the callee"
history lives - understand it before touching).

### Constraints (inline; sessions read nothing else)

- Read/write code only; no cargo/brokkr/test/bench commands.
- Determinism; identical `cost_used`; no HashMap/HashSet; clippy strict;
  `unwrap_used` denied outside tests; `Result::ok()` banned.
- The stack-discipline verifier + finalize debug_assert + the full
  example corpus are the safety net for remap bugs - extend the corpus
  test if any dynamic shape (marker stacks!) interacts with stripping:
  OP_MARK_CALL_BASE is exactly the instruction being nopped in (b), and
  the abstract interpreter tracks its marker stack - a nop that used to
  be a mark must NOT reach the verifier (it is stripped first), but a
  forged save CAN contain OP_NOP anywhere; define its verifier transfer
  as stack-neutral no-op.
- Keep `#[hotpath::measure]` on `close_upvalues` (its call counts
  dropping to ~frame-exit-only is the measurement of success).

### Deliverable

Implementation plan: the strip pass algorithm with exact remap math, the
OP_NOP opcode spec (encoding, dispatch, verifier transfer, analyze_cost),
the parser call-emission rewrite with the index-arithmetic audit, pass
ordering in finalize, test list (remap correctness across every jump
shape incl. backward jumps and jump-over-stripped-regions; closure-free
vs closure-containing functions; forged OP_NOP saves; golden fixture
split; exact-cost pins; parse-time complexity guard for (b) if
measurable), and bench predictions (`pairs`/`benchmark`/`gc_churn` gain a
little from (a); `large_source` parse_us gains from (b); nothing
regresses; `cost_used` identical).

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

Two corrections to the problem statement, both verified: the CURRENT golden
fixture does NOT change (the golden program contains no strippable
instruction - `make` has OP_CLOSURE so its CLOSEs stay, and nops never
survive finalize; do NOT regenerate either fixture), and `gc_churn`'s
closure-producing kernel keeps its per-iteration CLOSE (only closure-free
functions strip). Also verified: 5ff9a27's mark placement (after callee,
before args) is load-bearing - the mark must execute before argument
evaluation because varargs/all-results calls make the height dynamic; the
parser index audit is clean (all recorded indices stay stable once nothing
shifts at parse time); `remove_instr` has exactly two callers and dies;
the complete transfer set is JUMP, BRANCH_FALSE, BRANCH_TRUE_KEEP,
BRANCH_FALSE_KEEP, FOR_PREP, FOR_LOOP, TFOR_LOOP (TFOR_PREP is not a
transfer); no path opens an upvalue over a frame's locals without that
frame executing OP_CLOSURE (find_or_create_upvalue's only caller is
instr_closure; frame return/error safety closes remain untouched);
CLOSE_UPVALUES and MARK_CALL_BASE are free in both dispatch and
analyze_cost, so cost_used/own_cost/total_cost stay identical - only the
public `ScopeCost::instructions` count decreases (assert exactly that).

### 1. OP_NOP

`OP_NOP = 26` (next unused operandless slot) in `instr.rs`; `Instr::nop()`
constructor; Debug prints `Nop`. Dispatch: empty free arm in `frame.rs`.
Verifier: known opcode, operandless reserved-zero, stack-neutral transfer,
fallthrough successor. analyze_cost: free. Finalized compiler output must
never contain one (debug-asserted below); a forged v6 save containing a
zero-operand nop becomes accepted and executable - deliberate.

### 2. Parser rewrite (consumer b)

Both fixed-call `remove_instr(mark_idx)` sites in
`parser/expr.rs` (~line 265 region) become
`self.chunk.code[mark_idx] = Instr::nop();` - `line_info[mark_idx]` stays
until finalize. Dynamic calls keep their real marker. Delete
`remove_instr` (`parser.rs:446`).

### 3. `strip_dead_instructions(&mut Bytecode) -> Result<()>` in compiler.rs

- `has_closure = code contains OP_CLOSURE`; removable = every OP_NOP, plus
  every OP_CLOSE_UPVALUES when `!has_closure`.
- Boundary map, length n+1: `map[p]` = retained old instructions with
  index < p.
- For each retained transfer at old pc `s` (the seven opcodes above):
  `old_next = s + 1; old_target = old_next + old_sBx;`
  `new_sBx = map[old_target] - map[old_next]`. A removed target's map
  value is exactly the next retained instruction - correct fallthrough
  semantics for removed no-ops.
- Validate old target in `0..n`, new target within retained code, i16
  conversion (shrinkage cannot grow a valid jump, but a failed conversion
  still returns an internal compiler error, never a wrong offset).
  Preserve the A operand on the three loop transfers.
- Compact `code` + `line_info` with one removal mask.

### 4. finalize ordering

assert code/line_info aligned -> strip -> assert aligned again ->
debug-assert no OP_NOP remains -> assign_cache_slots -> validate_bytecode
-> recurse into nested chunks (recursion stays in finalize; while there,
fix the stale `assign_cache_slots` comment claiming it recurses itself).

### 5. Snapshots

Format 6 untouched. Compiler saves carry stripped code, never OP_NOP. Old
saves with CLOSE_UPVALUES stay valid (dispatch support remains). Neither
golden fixture is touched.

### Tests

- Strip unit tests: all seven transfer opcodes; forward AND backward
  jumps; removal before both endpoints / strictly between / at the
  target; consecutive removals; mixed nop+close regions; A preserved on
  loop transfers; line_info pairing preserved; out-of-range and failed
  remap return errors.
- Parser/finalize: raw parser output has nops for fixed normal AND
  method calls; dynamic vararg/all-results calls retain MARK_CALL_BASE;
  finalized trees contain no nops; closure-free nested functions contain
  no CLOSE; functions with OP_CLOSURE retain theirs; parent/child chunks
  independent; keep the 5ff9a27 method-receiver/nested-dynamic corpus
  cases.
- Upvalue correctness: closures capturing loop-body locals still close
  across block/if/numeric-for/generic-for/while/repeat/break exits; keep
  `block_upvalue_closes_before_slot_reuse` and friends green.
- Verifier/snapshot: forged `[NOP, RETURN]` loads, executes, costs zero;
  nop with nonzero operands rejected; forged `[CLOSE_UPVALUES, RETURN]`
  (old-style) still accepted; saved compiler output contains neither.
- Cost: pin runtime `cost_used` for a closure-free loop, a dynamic call,
  and a closure-capture case; pin analyze_cost `own_cost`/`total_cost`;
  assert ONLY `instructions` decreases.
- No timing-sensitive parse test - the structural guard is "no
  `Vec::remove` left in the parser"; the orchestrator measures
  `large_source` parse_us.

### Bench acceptance (orchestrator)

`pairs`/`benchmark` small gains; `factory_closure`'s loop CLOSE strips
(its kernel calls `mk` but contains no OP_CLOSURE itself); `gc_churn`
holds (kernel keeps CLOSE); `large_source` parse_us improves; nothing
regresses; `cost_used` identical everywhere.
