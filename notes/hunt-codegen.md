# Hunt: codegen, admission verifier, instruction encoding, disassembly

Scope: `src/compiler/codegen.rs`, `src/compiler/verify.rs`, `src/instr.rs`,
`src/disassemble.rs`, following values into the parser, the dispatch loop and
snapshot admission where the question led there. Read-only pass, no builds or
tests run.

Summary: register allocation, the captured-local copy rule, the open-results
protocol and the verifier's static proofs held up under every case traced (see
"Checked and sound" at the end). Two real gaps were found, both at the edges:
an unbounded multiple-assignment target list that ends in an internal fault (or
a debug panic), and faultable instructions that carry another statement's line.
The other items are small mislabels.

## 1. Too many multiple-assignment targets: an InternalError, a debug panic and silent `u16` truncation

**What happens.** The parser bounds a multiple assignment's *Field/Index*
targets by position (`MAX_ASSIGNMENT_OFFSET`) and bounds the expandable tail
(`check_multi_assign`, only when the last value is a call or `...`). Nothing
bounds the number of plain Local/Upval/Global/Builtin targets
(`parse_assign` in `parser/stmt.rs` pushes targets in a loop with no limit and
no syntax-depth charge). Codegen then needs one fresh register per target:

- `multi_assign` calls `explist_to_regs(values, targets.len())`.
- `explist_to_regs` computes `let missing = (want - have) as u16;` and
  `self.freereg = base + want as u16;`, both silent truncations.
- `multi_assign` stores from `src = values_base + index as u16`, also a
  silent truncation.

What `a, a, ..., a = 1` with N targets does at the top level, where
`nactive = 0` (the source for N = 65537 is about 131 KB, well under
`MAX_SOURCE_BYTES`):

| N | Outcome |
|---|---|
| up to 65534 | Compiles. The frame has about N registers. |
| 65535, 65536 | `reserve` fails with `CodegenError::Internal(ParserFault::RegisterLimit)`. `finish_chunk` turns that into `InternalError`. |
| 65537 | `missing` truncates to 0. `reserve(0)` succeeds and emits `LOAD_NIL 1 0`. The verifier rejects it with `CountOperandInvalid`. |
| 65538 and up | `missing` and `src` wrap. The stores name registers past `max_stack`. The verifier rejects them with `RegisterOutOfRange`. |

The verifier stops the wrapped bytecode from running, so nothing is silently
miscompiled. But both verifier rejections go through
`compiler.rs::validation_error`, which runs `debug_assert!(false, "{error}")`.
In a debug build (the test profile) they panic instead of returning an error.
In release they come back as `InternalError(CompilerValidation)`.

**Claims broken.**
- `reference/error-reporting.md`: "`InternalError` ... always means a dellingr
  bug". This is ordinary oversized source, and it gets the same report as a
  compiler bug.
- `docs/limits.md` "Source limits": "Violations are reported as errors at load
  time". Every other source limit gets a typed `SyntaxError` (`TooManyLocals`,
  `TooManyExpressions`, `AssignmentOffsetTooLarge`, ...). This one has no limit
  kind at all.
- `ParserFault::RegisterLimit` is listed among internal faults ("A function
  needs more registers than an operand can name"), but it can be reached from
  source. It is a limit, not an invariant.
- AGENTS.md / `docs/limits.md`: limit errors carry the observed size and the
  limit. This one carries neither, and is located at the end of the function
  (`self.input.pos()` in `finish_chunk`), not at the statement.

**Fix direction.** Bound the target list at parse time with a typed syntax
error, at the parse event where the targets are collected. A natural bound is
`MAX_EXPRESSIONS`, or applying `MAX_ASSIGNMENT_OFFSET` to every target and not
just Field/Index ones; either is far below the register ceiling. Separately,
make codegen's register exhaustion a typed limit error rather than a
`ParserFault`, and replace the three `as u16` conversions in
`explist_to_regs` / `multi_assign` with checked ones, so a future path to the
ceiling cannot wrap. Register pressure from nesting is bounded today by
`MAX_SYNTAX_DEPTH`: about 99 levels at about 256 registers each stays near
25k. That bound holds by arithmetic, not by a check, so a typed error at
`reserve` is the robust backstop. Fixing this means a source that is rejected
today stops being rejected, which changes no replay.

## 2. Faultable instructions emitted without a line stamp report the previous statement's line

**What happens.** `Generator::emit` stamps an instruction with `self.line`,
the last value any `emit_at` set. `generate`'s back-fill only repairs leading
zeros. So an instruction emitted with plain `emit` inherits whatever was
stamped last, which can belong to an earlier statement. Two of these
instructions can fail at runtime:

- **`OP_VARARG` with all values.** `ExprKind::Vararg` has no line in the tree,
  and codegen emits `Instr::vararg` with `emit`. `reg_vararg` with
  `RetCount::All` runs `check_stack_space(available)?` and can raise
  `StackOverflow`. Example:

  ```lua
  local function f(...)
    print("x")   -- line 2: the CALL stamps 2
    return ...   -- line 3: VARARG inherits 2, RETURN stamps 3
  end
  ```

  An overflow at the `...` is reported at line 2.
- **`OP_CLOSURE` in function statements.** `Stmt::LocalFunction` carries no
  line at all. `function_stmt` emits the closure with `emit` on every path and
  stamps only the following store (`self.line = line` comes after the closure
  in the Upval/Global/Builtin arm, and `emit_at` is used only for the
  `SET_FIELD` in the Field arm). `push_closure` runs `check_stack_space(1)?`,
  then `resolve_bytecode_runtime(..)?`, both fallible. A function body whose
  frame top lands at `MAX_STACK_SIZE` can hit this: frames can be thousands
  of registers, so a modest recursion depth gets there. The error then names
  the line of whatever statement preceded `function g() ... end` /
  `local function g() ... end`. (`ExprKind::Function` *is* stamped, so
  `g = function() end` is correct. Only the statement forms are wrong.)

**Claim broken.** `src/compiler/ast.rs` module doc: the tree "records ... the
line stamp of every instruction that can fault or name a call site".
`Vararg` and `LocalFunction` carry no stamp, and `Stmt::Function`'s stamp is
applied to the store but not to the closure. The traceback line
(`Frame::current_line` reads `line_info[ip - 1]`) is the visible effect.

**Fix direction.** Give `ExprKind::Vararg` and `Stmt::LocalFunction` a line,
and emit both closures and `VARARG` with `emit_at`. More structurally, the
sticky `self.line` is the root problem: any new faultable opcode emitted with
plain `emit` silently inherits a neighbour's line. Making `emit` take the line
explicitly, or giving every tree node that emits code a stamp, would turn this
whole class into a compile-time obligation instead of a convention.

## 3. Minor: `ValidationReason` variants that name the wrong defect

`ValidationReason` is public through `dellingr::snapshot`
(`InvalidBytecodeReason::Validation`), so its variants are the host's
diagnosis of a rejected save.

- `OP_TFOR_CALL` with `b > 255` is reported as `TforCallWithoutResults`
  ("TFOR_CALL requires at least one result"). The real defect is a count too
  large to be a `u8` (`verify.rs`, the `b == 0 || b > u16::from(u8::MAX)`
  arm).
- `max_stack == OPEN_COUNT` is reported as `FrameTooSmall` ("frame reserves
  fewer registers than its locals"). The real defect is a frame too large to
  be encodable.

Neither lets bad bytecode through; only the reason is wrong. The variant names
and Display strings are their own claims, and here they do not match the
defect.

## Lateral observations (outside the scope, noticed on the way)

- `vm/registers.rs::reg_vararg`, `RetCount::All` arm: the order is
  `check_window(dst)?`, then `self.stack.truncate(dst)`, then
  `check_stack_space(available)?`. When the preflight refuses, the live
  frame's registers above `dst` are already gone. Today that is harmless:
  `unwind_invocation` owns the failed frame and errors kill the callback. It
  is the one stack-growth site in this area that changes the stack before the
  check that can refuse it. Preflighting `available - (frame.top - dst)` first
  (or preflighting before the truncate) would make it a clean refusal, which
  is what the fault-injection sweep's "surviving operands" check is built to
  expect.
- `disassemble.rs` prints `local_slots={num_locals}`. `num_locals` counts
  *non-parameter* locals ("the most non-parameter locals the function has in
  scope at once"), so for a function with parameters the label undercounts
  what most readers would take "local slots" to mean. This is a diagnostic
  label only, with no format guarantee.
- Unreachable codegen faults are labelled with unrelated `ParserFault`
  variants: `multi_assign` uses `LocalWithoutDeclaration` for a missing place
  register, and `return_stmt` uses `EmptyMultiAssignment` for a
  more-than-255-values return. Neither can fire today, but if one ever does,
  the report will name the wrong invariant.

## Checked and sound

These looked suspicious and were followed through to the end. None is a
defect.

**Captured-local copies.**
- `operand_before` copies only captured locals. Within an expression, only a
  closure can write a local: there is no assignment expression, and debug and
  metamethod writes go through upvalues. The `captured` flag is final at
  codegen, because the whole function is parsed first and `resolve_in` marks
  the owning decl for captures at any depth.
- Every expression with a later sibling that may run code was traced:
  arithmetic, comparison and compare-branch chains, the index suffix, keyed
  constructor items, single Field/Index assignment, call, method call, concat,
  explist, numeric-for controls, and return lists. Each either copies the
  operand or materialises a `Reloc` before the sibling runs.
- `ExprKind::may_execute` covers every opcode that can reach script code:
  index (`__index`), `#` (`__len`) and calls. Comparisons and concat have no
  metamethods by design.

**Register allocation.**
- No temporary leaks at statement end were found. The release-mode risk would
  be a drift between the parser's local registers and codegen's `freereg`, and
  every free path checked keeps the two in step.
- The "everything above a call or concat or constructor block is dead"
  invariant that `call_window`'s truncate relies on holds for logical `dst`,
  pending constructor values, keyed keys, place operands and SELF pairs.

**Open-results protocol.**
- Codegen always emits a producer immediately before its consumer.
- No jump can target a consumer: targets are statement or loop starts, where a
  producer can be first but a consumer cannot.
- The verifier's adjacency check, its no-entering-jump check and
  `open_window_fits` together guarantee `stack.len() >= first` for every
  consumer's `len - first` computation in `frame.rs`.

**Verifier against the dispatch loop.**
- Every operand the dispatch loop indexes without a check is proven at
  admission:
  - register ranges (`SELF`'s two, the for-loop four, `TFOR_CALL`'s `3 + b`,
    and fixed call results);
  - literal, template, nested and upvalue indices;
  - builtin slots;
  - cache slots per family, with the count equalities;
  - jump targets.
- `INIT_FIELD_PINNED`'s unverified D entry is safe, because
  `Table::init_at_index` re-checks the key at that entry and falls back to
  `insert`.
- `SET_LIST`'s unverified batch is only arithmetic on keys.
- The child-capture check (`Local(slot) < num_params + num_locals`) is sound,
  because the hidden for-loop registers are declared locals and count in
  `num_locals`.

**Cache-slot policy.** `Bytecode::assign_cache_slots` and the verifier's
restated policy agree on every case, including exhausted names and SET slots
that interleave with a name's first GET.

**Encoding.**
- The jump offset encoding round-trips at both 24-bit extremes, and `patch`
  rejects anything wider.
- Fixed counts cannot alias `OPEN_COUNT`, and the asserts in `to_operand` are
  unreachable from parsed source, because `check_call_width` bounds fixed
  arguments at 254 including the receiver.
- The reserved-bit masks match each layout's use of D and J.
