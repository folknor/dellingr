# Hunt: compiler-back (codegen, bytecode format, verifier)

Scope read: `src/compiler.rs`, `src/compiler/codegen.rs`, `src/compiler/verify.rs`,
`src/instr.rs`. Followed values into `src/vm/frame.rs`, `src/vm/eval_control.rs`,
`src/vm/registers.rs`, `src/vm/table.rs`, `src/vm/save_state/verify.rs`,
`src/compiler/parser/{stmt,table}.rs` and `src/compiler/ast.rs`.

## Findings

### 1. Admitted bytecode can panic the VM at `FOR_LOOP` (verifier/VM contract gap)

`State::instr_for_loop` (`src/vm/eval_control.rs`) reads the three hidden
numeric-for registers with
`.as_num().expect("numeric for variable stays numeric after setup")` (and the
same for limit and step). That invariant comes from codegen: `FOR_PREP` writes
the registers, nothing writes them afterwards, and no jump lands in the body
without going through `FOR_PREP`. The verifier does not check any of this. For
`FOR_LOOP` it checks only `check_registers(pc, a, 4, frame)` and the jump target.

The claims this breaks:

- `verify.rs`, the dispatch-policy comment above `validate_bytecode`: "This does
  not prove runtime value types: constructor operands, **numeric loop values**,
  callable handlers and host/resource limits still need fallible runtime checks."
  `FOR_LOOP` does not do a fallible check. It panics.
- `docs/save-state.md`: "Saves are user-editable input." Snapshot bytecode goes
  through the same `validate_bytecode` (`vm/save_state/verify.rs`), so this input
  reaches the panic. `Table::init_at_index` and `Table::with_template_keys` already
  treat "forged snapshot bytecode" as a case that must stay sound, so this code
  path is the odd one out.
- `frame.rs` says a panic is acceptable only as "a verifier regression". This case
  is no regression: the verifier never claimed it, and the comment quoted above
  says the opposite.

Minimal admitted chunk: `code = [FOR_LOOP 0 +0, RETURN 0 0]`, `max_stack = 4`,
`line_info` of length 2. Every check passes: registers 0..4 are in the frame, the
target is 1, which is in range, and the last instruction is RETURN. Frame
admission sets the registers to nil, so the first `FOR_LOOP` panics. Under
unwind this poisons the State (`reference/error-reporting.md`, "Panics and
poisoning"). Under `panic = "abort"` it kills the host process.

Other admitted shapes that reach the same panic:
- a `JMP` into a loop body that skips `FOR_PREP`;
- a body instruction whose destination is `a..a+2`;
- an open producer (`VARARG`/`CALL` with all results) at a register below `a`,
  whose values spill over the hidden registers.

Compiled code is not affected. Hidden registers sit below `nactive`, and every
producer in a body sits at `freereg >= base + 4`.

Fix options:
- Make `instr_for_loop` fallible on a non-number: return an
  `InternalFault`-style error, as `reg_set_list` does for
  `ConstructorTableMissing`. The variant check already runs, so the only cost is
  a cold error arm.
- Or have the verifier prove the pairing: each `FOR_LOOP`'s target is
  `FOR_PREP + 1` with the same `A`, and that `FOR_PREP`'s exit is
  `FOR_LOOP + 1`. A single-region proof also has to rule out every write to
  `a..a+2` inside the region, including open-producer spills, so it is much more
  verifier code than the runtime check.

The runtime check is the right move. It also matches what the verifier comment
already says the VM does.

### 2. Jump-span limit is off by one in the backward direction (docs vs code)

`Generator::patch` (`codegen.rs`) rejects only
`offset > MAX_JUMP_OFFSET || offset < -MAX_JUMP_OFFSET - 1`, so it accepts a
backward offset of exactly `-(2^23)`. Two documents say otherwise:
- `instr.rs`, `MAX_JUMP_OFFSET`: "Largest jump offset magnitude ... the compiler
  rejects a jump at or past `2^23` instructions". The most negative accepted
  offset has magnitude `2^23`, one past the documented largest.
- `docs/limits.md`: "a branch or loop may span fewer than 2^23 instructions of
  one function's bytecode". A `while` loop whose back `JMP` has offset `-(2^23)`
  spans exactly `2^23` instructions.

Reachability: just barely, at `MAX_SOURCE_BYTES`. A loop body holding a
`{1,1,1,...}` constructor emits about one instruction per 2 source bytes, and
16 MiB of source gives about 2^23 instructions. Encoding and runtime are fine
with it (`frame.rs` `jump_accepts_the_most_negative_offset`), so nothing breaks
at runtime. It is a contract mismatch. Either make `patch` symmetric (reject
`offset < -MAX_JUMP_OFFSET`) or reword both docs to state the asymmetric range.
`codegen.rs` `jump_patching_rejects_offsets_past_the_encoding` tests only the
forward edge.

## Lateral notes (smells, not contract breaks)

- `codegen.rs` reports two u8 overflows as
  `ParserFault::MultiValueTailMissingInstruction`: the argument count in
  `call_args` and the multi-value tail width in `explist_to_regs`. Neither
  overflow is about a missing instruction. Both are unreachable today, because the
  parser bounds them (`bounded_u8(.., MAX_FIXED_ARGS ..)` and
  `check_multi_assign`). If either parser guard regresses, the internal fault will
  name the wrong cause.
- `compiler::finalize_at_depth` checks `code.len() != line_info.len()` itself
  (`CompilerFault::LineInfoDesynced`). `validate_bytecode` then checks the same
  thing again (`LineInfoLengthMismatch`). This is harmless. The first check exists
  so that `validation_error`'s `debug_assert!(false)` does not fire for that case
  (the unit test relies on that), but nothing in the code says so.
- The verifier does not require a root chunk to have zero upvalue descriptors.
  Compiled roots never have any, and snapshot closures carry an explicitly
  counted upvalue list (`validate_closures`), so I found no reachable
  consequence. If a new path ever builds a closure from a raw root `Bytecode`
  without supplying upvalues, `GET_UPVAL` would index out of bounds.

## Checked and found sound

I traced these and found no defect, so a follow-up can skip them:
- open-results protocol: producer and consumer adjacency, no jump into the
  consumer, `open_window_fits`, and the VM's `stack.len() - ra` arithmetic for
  RETURN, SET_LIST and CALL;
- register bounds for every opcode against the VM handlers;
- cache-slot policy parity between `assign_cache_slots` and the verifier,
  including global-cache exhaustion and uncached operands;
- template key distinctness;
- `INIT_FIELD_PINNED`'s unchecked D (the VM falls back to `insert`);
- `SET_LIST`'s unchecked batch C;
- backward-transfer safepoints: `FOR_PREP` and `TFOR_LOOP` are forced forward,
  and every other transfer polls through `Frame::branch`;
- codegen's operand-copy rule against `may_execute`. Global access is raw per
  `docs/compatibility.md`, so `Global` being non-executing is correct.
- register reservation against what each emitted instruction names;
- loop, `break` and CLOSE emission;
- the parser-side caps that keep fixed counts within `MAX_FIXED_ARGS` and
  `MAX_FIXED_RESULTS`.
