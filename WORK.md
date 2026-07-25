# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Target #59: saved bytecode stack discipline is not verified

The last item in `notes/bugs.md`, filed under deferred hardening. #16 shipped
in 0.4.0; this is the only thing left.

### The promise, and the gap

`load_state` promises that malformed save structure "is rejected with a
`LoadError`; it cannot trigger an indexing, stack-underflow, or
recursive-traversal panic during load". The recursive-traversal half is
already true - marking and save encoding are both iterative, so a deep decoded
table graph no longer overflows during the `gc_collect()` at the end of
materialization. The stack-underflow half is **not**.

Save files are user-editable input. The README says so explicitly and tells
hosts to reset the cost budget after loading a save. A forged save is therefore
untrusted input reaching the bytecode interpreter.

### What is verified today, verified by reading

`src/compiler/verify.rs` (442 lines) is the phase 1 verifier, reached from
`save_state/verify.rs:305` via `verify_payload`, and it runs before
materialization. It checks, per chunk:

- opcode is known (`known_opcode`)
- operand indices are in range (`check_index`) - number/string literals, table
  templates, nested chunk ids, upvalue ids, local slots
- inline-cache slot bounds (`check_slots`) for global/field/set-field caches
- jump targets land inside the code and are not negative-out-of-range
- reserved operand fields are zero
- `line_info` length agreement

`save_state/verify.rs` then checks the nested-chunk graph (acyclic, depth
within `MAX_SYNTAX_DEPTH`, child upvalue descriptors valid against the parent),
closures, values, references, environment deltas and pointer ids.

**There is no notion of stack height anywhere in either file.** Grep for
`stack`, `height` or `discipline` in `src/compiler/verify.rs` returns nothing.

### What that leaves reachable

Forged-but-structurally-valid code can still underflow or index outside the
current frame. From the finding, with the sites it names:

- `pop_val` underflow
- `DUP`'s `.last().expect(..)`
- `SWAP`'s `len - 1` / `len - 2`
- `CONCAT A`'s `len - A`
- fixed `RETURN n` while locating return values
- `RETURN RetCount::All`'s `stack.len() - frame_base`
- direct local accesses and loop-local ranges below the frame base
- a later `get_top()` call below `stack_bottom`
- open upvalues left pointing at popped frame slots

Cited sites: `src/vm/eval.rs:235,407,434`, `eval_index.rs:436`,
`eval_store.rs:454`, plus the dispatch paths for `DUP`, `SWAP`,
`MARK_CALL_BASE`, table initialization, field/table stores, the numeric and
generic loop helpers, fixed `SET_LIST`, and `GET_TABLE`.

### What the verifier has to prove

Abstract interpretation over each chunk's code with:

- an abstract operand-stack height per program point
- agreement at every CFG join (both edges of every branch, and every jump
  target) - a disagreement is a rejection, not a merge
- the vararg-call marker stack (`MARK_CALL_BASE` / `CALL` with
  `ArgCount::Dynamic`) and the table-constructor marker stack
  (`OP_NEW_TABLE_TRACKED` / dynamic `SET_LIST`), each balanced along every path
  and agreeing at joins
- dynamic result counts (`RetCount::All`, `ArgCount::Dynamic`), which make the
  height unknown until the matching marker pops - the abstract domain needs a
  representation for this rather than a single integer
- height never dropping below the frame base, and local slot accesses always
  within `num_params + num_locals`
- `MAX_STACK_SIZE` is **not** this verifier's job. The #62 work made every
  growth path checked or preflighted at runtime, so the obligation here is
  absence of underflow, valid ranges, balanced markers and join agreement -
  not a height ceiling.

### The hard constraint

**It needs compiler-corpus proof before it may reject saves.** A verifier that
rejects bytecode the real compiler emits turns every affected save into an
unloadable file. The corpus is `examples/*.lua` plus every test fixture; the
gate is that the verifier accepts every chunk the compiler produces for all of
them, including nested functions, before it is allowed to fail a load.

That suggests landing it in two stages: compute and check in a mode that cannot
reject (assert in tests, accept in production), prove it over the corpus, then
turn on rejection. Decide whether that staging is worth it or whether corpus
coverage in tests is sufficient proof to enable rejection directly.

### Open questions for the reviewer

1. **Is the abstract domain a plain integer height, or does it need a symbolic
   component?** `RetCount::All` and `ArgCount::Dynamic` make the height depend
   on runtime values between a marker and its consumer. Does tracking "height
   relative to the innermost marker" suffice, or is a richer domain needed?
   Name the opcodes that make this hard.
2. **Is the CFG reducible enough for a single pass?** The compiler emits
   structured control flow only - there is no `goto`. Can the verifier work in
   one forward pass with a worklist over jump targets, or does it need
   iteration to a fixed point? If one pass suffices, what property of the
   emitted code guarantees it, and does the verifier have to *check* that
   property rather than assume it, given the input is forged?
3. **What exactly must hold at a join?** Equal height is the obvious rule, but
   the marker stacks also have to agree in depth *and* in the base each marker
   records. Is equality the right rule, or does anything the compiler emits
   legitimately join at different heights?
4. **Which of the listed panic sites are actually reachable** given phase 1
   already runs? Some may already be excluded by operand-range checks. A
   demonstrated repro for at least one is worth more than the list.
5. **Cost.** This runs per load, over every chunk. What is the complexity, and
   does it need a bound on total code size so that verification itself cannot
   become the denial of service?
6. **Is `MAX_CALL_DEPTH` or anything else in scope**, or is underflow plus
   local-range plus marker balance genuinely the whole obligation?

### Constraints

- Rejection must produce `LoadError::InvalidBytecode { .. }` with the chunk
  index and offending instruction, matching the existing `invalid(..)` shape.
- Verification runs **before** materialization, like the rest of
  `verify_payload`.
- Determinism: identical input rejects or accepts identically.
- `unwrap_used` denied outside `#[cfg(test)]`; `Result::ok()` banned;
  `HashMap`/`HashSet` banned; clippy denies warnings.
- No new cost charges. Verification is host-side load work, not script work.
- Do not weaken any existing phase 1 check to make the new one fit.

### Reading

`src/compiler/verify.rs`, `src/vm/save_state/verify.rs`, `src/instr.rs`
(encoding, `ArgCount`/`RetCount` sentinels), `src/vm/frame.rs` (the dispatch
loop and what each opcode does to the stack), `src/vm/eval.rs`,
`src/vm/eval_control.rs`, `src/vm/eval_index.rs`, `src/vm/eval_store.rs`,
`src/vm/save_state.rs` (load path and existing rejection tests around lines
1860-2160), `README.md` (save-state section).

---

## Agreed plan

### Frame model

`stack_bottom` points at param 0; params+locals occupy `num_params + num_locals`
slots; the operand region begins at `frame_base = stack_bottom + num_params +
num_locals` (`eval.rs:462`). **Height** is the operand count above `frame_base`,
0 at pc 0. `pop_val` panics only when the whole `Vec` is empty, but any pop at
height 0 silently consumes a local or a caller's slot - both are the corruption
class #59 names. The main chunk (`stack_bottom == 0`, no locals) turns it into
the hard panic.

A leaked marker is also caller-corrupting: `eval_closure` (`eval.rs:322-331`)
truncates `vararg_call_bases` / `table_constructor_bases` **only on error**, so
a chunk returning Ok with a live marker poisons the caller's next dynamic call.
"Both marker stacks empty at every `OP_RETURN`" is a hard obligation.

### Domain

A plain integer is not enough:

```rust
enum Height { Known(u32), Dyn { floor: u32 } }
```

relative to `frame_base`, plus two abstract marker stacks (`Vec<u32>` of Known
bases). A floor suffices because every panic site is a pop or a `len - k`
subtraction.

`Dyn` producers: `OP_CALL` with `B == 255` (`RetCount::All`), `OP_VARARG` with
`A == 255`. Consumers: `OP_CALL` with `A == 255`, `OP_SET_LIST` with `A == 0`,
`OP_RETURN` with `A == 255`.

Restrictions keeping the domain small, all corpus-checkable: `MARK_CALL_BASE`
and `NEW_TABLE_TRACKED` require `Known`; all control-transfer opcodes and
`RETURN Fixed(n)` require `Known`, so `Dyn` cannot flow into a jump target.

### Control flow - single forward worklist pass, no fixed point

The join rule is **exact equality**, not a lattice merge, so a state is written
once and every later edge into it must equal it. Terminates in O(edges)
regardless of whether the forged CFG is reducible or adversarial. Equality *is*
the checked property - nothing is assumed about compiler output.

- `states: Vec<Option<AbsState>>` indexed by pc; worklist `Vec<usize>`.
- Seed pc 0 with `{ Known(0), [], [] }`.
- Successors: fall-through, plus the already-range-checked jump target; both
  for conditionals; target only for `OP_JUMP`; none for `OP_RETURN`.
- Join must agree on height variant+value **and** both marker stacks
  element-wise including each recorded base - depth alone is insufficient,
  because `SET_LIST 0` / `CALL Dynamic` use the base to compute post-height.
- Unreachable pcs stay `None` and are accepted: dispatch starts at pc 0 and
  traverses the same edges, so unverified code is unexecutable. Existing phase 1
  operand checks still cover it. This avoids rejecting dead code.

### Markers

- `MARK_CALL_BASE`: require `Known(h)`, `h >= 1`; push `h - 1`.
- `CALL A == 255`: pop (empty -> reject); require `floor >= base + 1`.
- `NEW_TABLE_TRACKED`: require `Known(h)`; push `h`; height `h + 1`.
- `SET_LIST A == 0`: pop (empty -> reject); require `floor >= base + 1`; height
  `Known(base + 1)`.
- Continuous invariant: after every instruction, `floor >= top_live_marker_base
  + 1` for each stack.
- Cap each abstract stack at `MAX_SYNTAX_DEPTH` - without it a forged chunk of N
  trackers then N branches makes join comparisons O(N^2).

### Placement

Private `verify_stack_discipline(view: &impl BytecodeView)` in
`src/compiler/verify.rs`, called as the **last** statement of
`validate_bytecode`, so every existing check runs first unweakened and the
transfer function may rely on already-validated operands (`CONCAT >= 2`,
`MARK_CALL_BASE A == 1`, `TFOR_CALL B >= 1`, jump targets in range) - no
`unwrap` anywhere. Errors use the existing `err(Some(pc), reason)`, so
`save_state/verify.rs:305` maps them to `LoadError::InvalidBytecode` unchanged.
`BytecodeView` needs no new methods. Height arithmetic in `u32` with
`checked_add`/`checked_sub`, overflow rejects.

### Staging: none. Land computation and rejection together

`finalize` (`src/compiler.rs:349-354`) already runs `validate_bytecode` on every
compiled chunk and `debug_assert!`s on failure - accept-in-release,
assert-in-tests. Extending it turns the whole existing suite into the corpus
proof the day the check lands: a false rejection fails CI as a debug assertion
before it can fail a load. The only hard-rejecting path is the load path, which
is exactly where rejection is wanted. Add one explicit corpus test; the
two-stage flag machinery buys nothing and costs a release plus dormant-code
risk.

### Complexity

O(code_len) visits + O(edges) joins, each join O(marker depth), capped at
`MAX_SYNTAX_DEPTH`. No new total-code-size bound needed - each instruction is 4
payload bytes, so `code_len` is already linear in the save size. The
marker-depth cap is the one bound that must be added.

### Tests

Rejection (alongside the existing forged-bytecode tests,
`save_state.rs:1860-2160`), each asserting `LoadError::InvalidBytecode` with the
expected chunk and instruction:

1. `[POP, RETURN 0]` - the host-panic repro, reject at pc 0
2. `[SWAP, RETURN 0]` and `[DUP, RETURN 0]`
3. `[PUSH_NIL, CONCAT 2, RETURN 0]` - height 1 < 2
4. `[RETURN Fixed(1)]` - return values exceed height
5. Join mismatch: `[PUSH_BOOL, BRANCH_FALSE +1, PUSH_NIL, RETURN 0]`
6. Back-edge mismatch: a loop that grows height per iteration
7. `CALL(Dynamic, Fixed 1)` with no `MARK_CALL_BASE`
8. `SET_LIST(0)` with no `NEW_TABLE_TRACKED`
9. `[PUSH_NIL, MARK_CALL_BASE 1, RETURN 0]` - live marker at return
10. `MARK_CALL_BASE` at height 0
11. `CALL(Fixed 0, All)` then `BRANCH_FALSE` - branch at `Dyn`
12. `SET_FIELD_AT` / `SET_TABLE` / `INIT_INDEX` with offset below height
13. `[SET_LOCAL 0, ...]` with `num_locals = 1` at height 0

Corpus acceptance: a test iterating every `examples/**/*.lua`, compiling with
`compiler::parse_str`, walking the `nested` tree recursively, asserting
`validate_bytecode` passes on every chunk. Must exercise the dynamic shapes
(`MARK_CALL_BASE` / `CALL Dynamic` / `NEW_TABLE_TRACKED` / `SET_LIST 0` /
`RETURN All`). The `debug_assert!` at `compiler.rs:350` makes every other test
an implicit corpus case for free.
