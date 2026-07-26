# WORK.md

Current work item. Optimization loop 6.

---

## Target: return values move in place instead of through a per-call Vec

### The mechanism

`eval_closure_frame` (`src/vm/eval.rs`) ends every successful Lua call with:

1. `collect_return_values(ret_start)` - `stack.drain(ret_start..).collect()`
   into a fresh `Vec<Val>` (one heap allocation per returning call),
2. `close_upvalues(self.stack_bottom)`,
3. `stack.truncate(self.stack_bottom)`,
4. `stack.extend(ret_vals)`.

Measured: 16 B x 1,116,493 returns = 17.0 MB, 79.8% of `arithmetic`'s
process allocation (`--alloc` uuid 93903c8e); `collect_return_values` shows
1.5-3.2% of instrumented wall on call-heavy workloads.

### The change

Reorder and collapse steps 1-4 into one in-place move:

1. `close_upvalues(self.stack_bottom)` FIRST - it reads the frame's local
   slots, which sit below `ret_start` and are untouched by the move; today
   it runs after the drain, and the drain only removes values above the
   locals, so the reordering observes the same slot contents.
2. `self.stack.drain(self.stack_bottom..ret_start);` - removes the frame's
   params/locals/operands beneath the returns in ONE memmove, leaving the
   returns starting at `stack_bottom`. No allocation, no second pass.

`collect_return_values` and its `#[hotpath::measure]` point disappear (the
allocation it existed to attribute no longer exists). `collect_varargs`
STAYS - varargs genuinely need an owned Vec (they live in the Frame).

### Correctness arguments to verify, not assume

- Open upvalues of the frame point at slots in `stack_bottom..ret_start`;
  none can point at the return-value slots (returns are transient operands
  above the locals) - confirm by reading `find_or_create_upvalue` callers
  and the slot math.
- The error paths (`frame.eval` Err, RetCount::All overflow) do not collect
  returns and keep their existing close/truncate ordering - untouched.
- `stack_bottom` restore and `call_stack.pop()` ordering unchanged.
- `Vec::drain` on a range, dropped immediately, compiles to a memmove of
  the tail; no iterator is consumed. (The drain guard moves the tail on
  drop.)
- Stack cap: net effect is strictly shrinking (len decreases by
  `ret_start - stack_bottom`); no new push paths.

### Constraints

- Read/write code only for any session involved; orchestrator validates.
- `cost_used` byte-identical (nothing here charges).
- Iteration/GC: between close and drain there is no allocation, so no GC
  can observe the intermediate state.
- Clippy denies warnings; the usual lint gate.

### Verdict plan (orchestrator)

`arithmetic` A/B interleaved worktree pairs vs a4811f9 (expect small
single-digit wall win at best; primary verdict is `--alloc`:
`collect_return_values`' 17.0 MB on arithmetic collapses to ~0, leaving
process allocation near the 13.7 KB floor). `benchmark` + `closure`
guards. Cost fingerprints identical.
