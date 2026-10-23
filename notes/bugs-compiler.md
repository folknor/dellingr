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

# Compiler defects: front end, codegen, verifier, encoding

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `frontend`,
`codegen`, `vm-core`, `surface`). The hunters read the code statically and built
or ran nothing, so every repro here is traced by hand, not executed.

## CMP-001 - Unbounded multiple-assignment target count reaches InternalError, a debug panic and silent u16 truncation

Reported independently by the `frontend` and `codegen` hunters. Their accounts
are merged here.

**Claims broken**

- reference/error-reporting.md: "`InternalError` ... always means a dellingr
  bug". Oversized but otherwise ordinary source gets the same report as a
  compiler bug.
- docs/limits.md "Source limits": "Violations are reported as errors at load
  time, never silently truncated". The README's "Source limits" also says
  per-function ceilings "are reported as syntax errors rather than silently
  truncated". Every other source limit gets a typed `SyntaxError`
  (`TooManyLocals`, `TooManyExpressions`, `AssignmentOffsetTooLarge`, ...);
  this one has no limit kind.
- `ParserFault::RegisterLimit` is listed among internal faults ("A function
  needs more registers than an operand can name"), but source can reach it. It
  is a limit, not an invariant.
- Limit errors are supposed to carry the observed size and the limit. This one
  carries neither, and is located at the end of the function
  (`self.input.pos()` in `finish_chunk`), not at the statement.

**Mechanism**

`parse_assign` (parser/stmt.rs) collects any number of targets in a loop with
no limit and no syntax-depth charge. The only per-target bound,
`MAX_ASSIGNMENT_OFFSET`, applies only to `Field` and `Index` targets.
`check_multi_assign` fires only when the last value is a call or vararg. So
`a, a, ..., a = 1` with Local, Upval, Global or Builtin targets reaches codegen
with an unbounded `targets.len()`.

`codegen::multi_assign` calls `explist_to_regs(values, targets.len())`, which
needs one fresh register per target:

- `let missing = (want - have) as u16;` and `self.freereg = base + want as u16;`
  truncate silently.
- `multi_assign` stores from `src = values_base + index as u16`, which also
  truncates silently. In debug it overflows and panics whenever
  `values_base > 0` (a local declared first).
- `reserve` fails with `ParserFault::RegisterLimit` once the frame reaches
  `OPEN_COUNT`, and `finish_chunk` maps that to an internal fault.

Outcome by target count N, at the top level (`nactive = 0`). The source for
N = 65537 is about 130 KB, well under `MAX_SOURCE_BYTES`.

| N | Outcome |
|---|---|
| up to 65534 | Compiles, with a frame of about N registers. |
| 65535, 65536 | `reserve` fails with `CodegenError::Internal(ParserFault::RegisterLimit)`, which becomes `InternalError`. |
| 65537 | `missing` truncates to 0. `reserve(0)` succeeds and emits `LOAD_NIL 1 0`. The verifier rejects it with `CountOperandInvalid`. |
| 65538 and up | `missing` and `src` wrap, and the stores name registers past `max_stack`. The verifier rejects them with `RegisterOutOfRange`. |

The verifier stops the wrapped bytecode from running, so nothing is silently
miscompiled. But both verifier rejections go through
`compiler.rs::validation_error`, which runs `debug_assert!(false, "{error}")`.
A debug-built host (the test profile) therefore panics on this user input; in
release it comes back as `InternalError(CompilerValidation)`.

Reference Lua rejects long target lists with a syntax error, because
`restassign` checks `nvars + nCcalls` against `LUAI_MAXCCALLS`.

**Repro (traced):** the target `a` repeated 65537 times, comma-separated, then
`= 1`.

**Fix direction**

- Bound the target list at parse time with a typed `SyntaxError`, at the parse
  event that collects the targets. `MAX_EXPRESSIONS`, which already bounds the
  value side, is the natural cap. Applying `MAX_ASSIGNMENT_OFFSET` to every
  target would also work. Either is far below the register ceiling.
- Make register exhaustion in codegen a typed, user-facing limit error
  (reference Lua's "function or expression needs too many registers") instead
  of a `ParserFault`. Only codegen knows register pressure, so the parser
  cannot fully pre-check it. Today pressure from nesting is bounded only by
  arithmetic: about 99 levels of `MAX_SYNTAX_DEPTH` at about 256 registers each
  is near 25k (the frontend hunter estimates about 255 per 2 syntax units).
  Nothing asserts that bound, so a typed error at `reserve` is the robust
  backstop.
- Replace the three `as u16` conversions in `explist_to_regs` and
  `multi_assign` with checked ones.

The codegen hunter notes the fix only rejects source that already fails today,
so no replay changes.

## CMP-002 - `function t.x() ... end` and `function t:m() ... end` stamp the store with the `end` line

From the `frontend` hunter.

**Claims broken**

- The project's rule that an error message must not mislead the script author.
- ast.rs's `Stmt::Function { line }`, documented as "The store's stamp".
  Reference Lua deliberately stamps the definition line
  (`luaK_fixline(fs, line); /* definition "happens" in the first line */` in
  `funcstat`, in both 5.2 and 5.4).

**Mechanism.** `parse_fndecl_basic`, `parse_fndecl_table` and
`parse_fndecl_method` (parser/func.rs) all set `line: self.input.last_line()`
after `parse_fndef` has consumed `end`. `codegen::function_stmt` stamps the
`SET_FIELD` with that line. `SET_FIELD` is the instruction that faults
("attempt to index a nil value") or calls `__newindex`, so it is a traceback
call site.

```lua
function cfg.handler()   -- line 1, cfg is nil
  ...                    -- 200 lines
end                      -- line 202
```

The error is reported at line 202, not line 1. `function_stmt` also stamps the
global and upvalue stores with this line through `self.line = line`; those
stores cannot fault.

**Fix direction:** record the `function` keyword's line in `parse_fndecl` and
pass it down as the stamp.

Related observations from the same hunter, outside the defect bar:
`parse_fndef` records `defined_line` from the `(` token rather than the
`function` keyword, which matters only for `function\nname()` layouts. The
anonymous closure stamp (`ExprKind::Function.line`) is the line of the body's
last token, not the `end` line; `CLOSURE` as an expression cannot fault, so
this is invisible today. See CMP-003 for the closure statement forms.

## CMP-003 - Faultable instructions emitted without a line stamp report the previous statement's line

From the `codegen` hunter.

**Claim broken.** The module doc of `src/compiler/ast.rs`: the tree "records
... the line stamp of every instruction that can fault or name a call site".
`Vararg` and `LocalFunction` carry no stamp, and `Stmt::Function`'s stamp is
applied to the store but not to the closure. The visible effect is the
traceback line, which `Frame::current_line` reads from `line_info[ip - 1]`.

**Mechanism.** `Generator::emit` stamps an instruction with `self.line`, the
last value any `emit_at` set. `generate`'s back-fill repairs only leading zeros.
An instruction emitted with plain `emit` therefore inherits whatever was
stamped last, which can belong to an earlier statement. Two such instructions
can fail at runtime:

- **`OP_VARARG` with all values.** `ExprKind::Vararg` has no line in the tree,
  and codegen emits `Instr::vararg` with `emit`. `reg_vararg` with
  `RetCount::All` runs `check_stack_space(available)?` and can raise
  `StackOverflow`.

  ```lua
  local function f(...)
    print("x")   -- line 2: the CALL stamps 2
    return ...   -- line 3: VARARG inherits 2, RETURN stamps 3
  end
  ```

  An overflow at the `...` is reported at line 2.
- **`OP_CLOSURE` in function statements.** `Stmt::LocalFunction` carries no
  line. `function_stmt` emits the closure with `emit` on every path and stamps
  only the following store (`self.line = line` comes after the closure in the
  Upval/Global/Builtin arm, and `emit_at` is used only for the `SET_FIELD` in
  the Field arm). `push_closure` runs `check_stack_space(1)?` and then
  `resolve_bytecode_runtime(..)?`, both fallible. Frames can be thousands of
  registers, so a modest recursion depth can put a frame top at
  `MAX_STACK_SIZE`. The error then names the line of whatever statement came
  before `function g() ... end` or `local function g() ... end`. The expression
  form `g = function() end` is stamped correctly.

**Fix direction.** Give `ExprKind::Vararg` and `Stmt::LocalFunction` a line,
and emit closures and `VARARG` with `emit_at`. The structural root is the
sticky `self.line`: any new faultable opcode emitted with plain `emit` silently
inherits a neighbour's line. Making `emit` take the line explicitly, or giving
every tree node that emits code a stamp, turns this whole class into a
compile-time obligation instead of a convention.

## CMP-004 - `_ENV` is silently an ordinary name, an undocumented divergence that changes results

From the `frontend` hunter.

**Claim broken.** docs/compatibility.md: "Every deliberate divergence from Lua
5.2 and 5.4. A difference not listed here is a bug. Features that are absent
altogether are listed in the README under 'Won't implement'."

**Mechanism.** Name resolution (`parse_prefix_identifier` and `resolve_in`) has
no `_ENV` concept. In both reference versions, `local _ENV = t` (or an `_ENV`
parameter or upvalue) redirects every free name in its scope to `t`. In
dellingr, `local _ENV = {}` declares an unused local, and a later `x = 1` or
`print(x)` still uses the real globals. The program compiles and runs with
different results and no diagnostic, which hurts the script author. `_ENV`
appears nowhere in the README, docs/ or reference/.

**Fix direction:** list it under "Won't implement" or in compatibility.md.
Better, have the parser reject `_ENV` as a declared local, parameter or
assignment target with a clear syntax error, so a 5.2-style sandbox idiom fails
loudly instead of leaking to the globals.

**Same documentation gap, but these fail loudly:** Lua 5.4's `\u{XXXX}` escape
is rejected as "invalid escape sequence", and the local attributes `<const>` and
`<close>` are rejected as a syntax error. Neither is listed as absent. See
LIB-008 in notes/bugs-stdlib.md for the same gap in the library surface.

## CMP-005 - Stale "255 sentinel" rationale for the fixed-count limits

Reported independently by the `frontend`, `vm-core` and `surface` hunters.

**Claims at issue.** These statements give a reason that is no longer true:

- src/limits.rs, the comment above `MAX_LOCALS` / `MAX_FIXED_ARGS`: "Operand
  value 255 is reserved for dynamic argument and result counts."
- The doc comments on `MAX_FIXED_ARGS` ("before the dynamic-count sentinel")
  and `MAX_FIXED_RESULTS` ("before the all-results sentinel").
- docs/limits.md: "The dynamic-count sentinel is reserved."
- src/instr.rs: "The fixed-count limits are exactly the counts below the shared
  255 sentinel of the runtime count types". `ArgCount` and `RetCount` have no
  sentinel value.
- `ArgCount`'s doc: "Only instruction encoding rejects the sentinel alias", and
  the comment block under it: "runtime counts never pass through `to_u8`, which
  asserts against the alias". There is no `to_u8` on `ArgCount`; the encoder is
  `to_operand`, which asserts `n <= MAX_FIXED_ARGS`.

**What is true.** Count operands are 16 bits, and the open count is
`instr::OPEN_COUNT` (`0xFFFF`). Operand value 255 is an ordinary fixed count:
`RetCount::to_operand` encodes `Fixed(255)` as 255 for `RETURN`. The 254 caps
still hold, but for a different reason. The `vm-core` and `surface` hunters
attribute them to the `u8` host count types (`ArgCount::Fixed` is a `u8`) plus
the `__call` receiver, which can add one argument to 254. The public `limits`
docs therefore tell a host about a reserved operand that does not exist.

**Visible asymmetry (frontend hunter).** `return v1, ..., v255` compiles (there
is a test for it), and the runtime accepts 255 arguments
(`MAX_ARGUMENTS = 255`), but `f(v1, ..., v255)` is rejected at parse time with
`TooManyArguments`.

**Fix direction:** restate the reason (for example the `u8` host count types and
the `__call` receiver, or "kept to preserve the accepted program set"), or lift
the cap. Lifting it widens the accepted program set, which is the maintainers'
decision.

## CMP-006 - Other stale or wrong statements about compiler limits

From the `frontend` and `vm-core` hunters. Low severity; each is a claim that
does not match the code.

- **The 24-bit line comment in src/limits.rs describes code that does not
  exist.** It reads "Call sites pack lines into 24 bits; the compiler asserts
  this cap against that field's mask where the field is defined." The frontend
  hunter found no such packing, mask or assertion; the only 24-bit field is the
  jump offset. `MAX_SOURCE_BYTES` of 16 MiB allows line numbers up to
  2^24 + 1 anyway, so if such a field ever came back, this cap would not
  protect it.
- **`MAX_FRAME_SLOTS`: "Maximum frame slots encoded by bytecode."**
  (vm-core hunter.) Registers are 16-bit, so 255 is a compiler and verifier
  policy cap, not an encoding limit. The comment in `find_or_create_upvalue`
  (src/vm/eval.rs) that bounds the open-upvalue shift by it is still correct,
  because the verifier enforces it.
- **The README's parenthesis claim is wrong.** "Source limits" says "a nested
  parenthesis costs two levels ... Parentheses therefore nest about half as
  deep as other constructs." By docs/limits.md's own table, which matches the
  code: table constructors and `if` bodies also cost 2, the same as
  parentheses; calls and indexes cost 4, so they nest half as deep as
  parentheses; only unary operators, `^` and `do`, loop or function bodies cost
  1.
- **README and docs/limits.md name the wrong constant for field-store
  caching.** Both say field assignment sites past `MAX_TABLE_TEMPLATES` stop
  being inline-cached. `SET_FIELD` caching is actually capped by
  `instr::MAX_CACHE_SLOTS` in `Bytecode::assign_cache_slots`, while
  `MAX_TABLE_TEMPLATES` caps constructor templates (`try_use_table_template`
  falls back to ordinary construction past it). Both equal 255, so behaviour
  matches the text today, but the sentence describes the wrong mechanism and
  omits the constructor fallback, which is what that constant controls.

## CMP-007 - `ValidationReason` variants that name the wrong defect

From the `codegen` hunter. Minor.

`ValidationReason` is public through `dellingr::snapshot`
(`InvalidBytecodeReason::Validation`), so its variants are the host's diagnosis
of a rejected save. The variant names and Display strings are claims, and two
do not match the defect they report:

- `OP_TFOR_CALL` with `b > 255` is reported as `TforCallWithoutResults`
  ("TFOR_CALL requires at least one result"). The real defect is a count too
  large for a `u8` (the `b == 0 || b > u16::from(u8::MAX)` arm in verify.rs).
- `max_stack == OPEN_COUNT` is reported as `FrameTooSmall` ("frame reserves
  fewer registers than its locals"). The real defect is a frame too large to be
  encodable.

Neither lets bad bytecode through; only the stated reason is wrong.

Related, from the same hunter and outside the defect bar: unreachable codegen
faults are labelled with unrelated `ParserFault` variants. `multi_assign` uses
`LocalWithoutDeclaration` for a missing place register, and `return_stmt` uses
`EmptyMultiAssignment` for a return of more than 255 values. Neither can fire
today, but if one ever does, the report names the wrong invariant.

## CMP-008 - A numeric `for` without a step can fail with "too many numbers" though the source adds no literal

From the `frontend` hunter. Low severity; mostly a consistency note.

`parse_numeric_for_step` calls `find_or_add_number(1.0)` for the implicit step.
In a chunk whose number pool is full and lacks `1`, `for i = a, b do ... end`
fails with `TooManyNumbers`, although the author wrote no new numeral. Folding
already treats a full pool as non-fatal: `try_fold_literal` documents that "a
full pool must leave otherwise valid source compilable". The implicit step gets
no such treatment. It is reachable only at 65536 distinct numerals.

## CMP-009 - Codegen at the syntax budget is tested for only two tree shapes

From the `frontend` hunter, raised as a lateral observation outside the defect
bar.

docs/limits.md promises "parsing at the syntax budget fits a 2 MiB thread in
both settings", and compiling includes codegen.

- `deepest_nested_programs_compile_within_the_test_stack` drives only call
  chains and constructor chains to the deepest accepted depth.
- `syntax_depth_limit_body` drives `do`, unary, `{`, `a[(` and `f(` to 10,000.
  That test fails at the budget, so it exercises the parser to full depth but
  never runs codegen on a full-depth tree.
- Shapes whose codegen recursion has never run at the budget on the 2 MiB
  thread: 199 nested `do`, `while`, `for` or `repeat` bodies; about 99 nested
  `if` bodies; about 197 chained `-` / `not`; `^` chains; about 98
  parentheses.
- `Generator::stmt` and `Generator::expr` are large `match` functions with
  large frames in the dev profile. The parser's per-unit frames are also
  untested at the budget for `for`, `if` and `function`. `for` has the longest
  per-unit frame chain: `parse_statements_inner`, `push_statement`, `parse_for`,
  `parse_numeric_for`, then `parse_statements`.

**Fix direction:** an accepted-at-budget case per shape, compiled on
`DEEP_PARSE_TEST_STACK_BYTES`.

## CMP-010 - Small front-end and disassembly observations

Raised as lateral observations outside the defect bar.

- **Constructor flush order differs from reference** (frontend hunter).
  Positional values are flushed in batches of 255 (reference uses 50), so
  `{v1, ..., v60, [1] = "x"}` ends with `t[1] == v1` in dellingr and `"x"` in
  reference Lua. The Lua manual leaves assignment order inside a constructor
  undefined, and the behaviour is deterministic and replay-stable, so the
  hunter does not count it as a defect.
- **String-size error position** (frontend hunter).
  `get_literal_string_contents` reports `StringSizeExceeded` at `error_here()`,
  the token after the string, not at the string token.
- **`local_slots` label in disassembly** (codegen hunter). `disassemble.rs`
  prints `local_slots={num_locals}`. `num_locals` counts non-parameter locals
  ("the most non-parameter locals the function has in scope at once"), so for a
  function with parameters the label undercounts what most readers take "local
  slots" to mean. Diagnostic label only, with no format guarantee.
