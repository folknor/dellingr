# Defect hunt: compiler front end

Scope: src/compiler.rs, src/compiler/{lexer,token,parser,ast}.rs,
src/compiler/parser/*, src/numeral.rs. Read-only review at a22d9c0; nothing
was built or run, so the repros below are traced by hand and not executed.

Findings come first, most severe first. Lateral notes and the areas I checked
and found sound follow after them.

---

## 1. Assignment target count is unbounded: user source reaches InternalError (release) or a panic (debug)

**Claims broken**

- reference/error-reporting.md: "`InternalError` ... always means a dellingr bug".
- docs/limits.md "Source limits": "Violations are reported as errors at load
  time, never silently truncated".
- README "Source limits": per-function ceilings "are reported as syntax errors
  rather than silently truncated".

**What happens**

`parse_assign` (parser/stmt.rs) collects any number of targets. The only
per-target bound is `MAX_ASSIGNMENT_OFFSET`, and it applies only to `Field`
and `Index` targets. `check_multi_assign` only fires when the last value is a
call or vararg. So `a, a, a, ..., a = 1`, with local, global, upvalue or
builtin targets, reaches codegen with an unbounded `targets.len()`.

`codegen::multi_assign` then calls `explist_to_regs(values, targets.len())`,
which needs one register per target:

- `let missing = (want - have) as u16;` silently truncates once `want - have`
  is at least 65536.
- `let src = values_base + index as u16;` truncates `index`. In debug builds
  it overflows and panics whenever `values_base > 0`.
- `reserve` fails with `ParserFault::RegisterLimit` once the frame reaches
  `OPEN_COUNT`. `finish_chunk` maps that to an internal fault.

The outcome depends on the target count:

- 65535 or 65536 targets: `reserve` overflows, giving
  `InternalError(Parser(RegisterLimit))`.
- 65537 or more targets: `missing` wraps to a small number and the store loop
  names registers past `max_stack`. The verifier catches this, so the result is
  `InternalError(CompilerValidation)`. Separately, `compiler.rs::validation_error`
  runs `debug_assert!(false, ...)`, so a debug-built host panics on this user
  input. With a local declared first (`values_base = 1`), codegen also panics
  earlier on the `u16` add overflow in debug.

The source is about 130 KB, well under `MAX_SOURCE_BYTES`. Reference Lua
rejects long target lists with a syntax error, because `restassign` checks
`nvars + nCcalls` against `LUAI_MAXCCALLS`.

**Repro (traced):** the target `a` repeated 65537 times, comma-separated, then
`= 1`.

**Fix direction**

- Bound the target list in the parser with a user-facing `SyntaxError`, as
  every other per-construct list already is. `MAX_EXPRESSIONS` is the natural
  cap, and it already bounds the value side.
- Separately, register exhaustion should be a user-facing limit error
  (reference Lua's "function or expression needs too many registers"), not a
  `ParserFault`. Codegen is the only stage that knows register pressure, so the
  parser cannot fully pre-check it. Today that pressure is bounded only because
  every other list happens to be capped. I estimate the worst case outside
  assignment at about 25k registers (about 255 per 2 syntax units), but nothing
  asserts that.
- Replace the `as u16` casts in `explist_to_regs` and `multi_assign` with
  checked conversions.

---

## 2. `function t.x() ... end` and `function t:m() ... end` stamp the store with the `end` line

**Claims broken**

- The project's own rule that an error message must not mislead the script
  author.
- ast.rs's `Stmt::Function { line }`, documented as "The store's stamp".
  Reference Lua deliberately sets this stamp to the definition line:
  `luaK_fixline(fs, line); /* definition "happens" in the first line */` in
  `funcstat`, in both 5.2 and 5.4.

**What happens**

`parse_fndecl_basic`, `parse_fndecl_table` and `parse_fndecl_method`
(parser/func.rs) all set `line: self.input.last_line()` after `parse_fndef`
has consumed `end`. `codegen::function_stmt` stamps the `SET_FIELD` with that
line. `SET_FIELD` is the instruction that faults ("attempt to index a nil
value") or calls `__newindex`, which makes it a traceback call site.

So in this program:

```lua
function cfg.handler()   -- line 1, cfg is nil
  ...                    -- 200 lines
end                      -- line 202
```

the error is reported at line 202 instead of line 1. `function_stmt` also
stamps the global and upvalue stores with this line through `self.line = line`;
those stores cannot fault.

**Fix direction:** record the `function` keyword's line in `parse_fndecl`, and
pass it down as the stamp.

---

## 3. `_ENV` is silently an ordinary name, an undocumented divergence that changes results

**Claims broken**

- docs/compatibility.md: "Every deliberate divergence from Lua 5.2 and 5.4. A
  difference not listed here is a bug. Features that are absent altogether are
  listed in the README under 'Won't implement'."

**What happens**

Name resolution (`parse_prefix_identifier` and `resolve_in`) has no `_ENV`
concept. In both reference versions, `local _ENV = t` (or an `_ENV` parameter
or upvalue) redirects every free name in its scope to `t`. In dellingr
`local _ENV = {}` declares an unused local, and later `x = 1` / `print(x)` keep
using the real globals.

The program compiles and runs with different results and no diagnostic. That
hurts the script author, which is the bar AGENTS.md sets for a defect. `_ENV`
appears nowhere in README, docs/ or reference/.

**Fix direction:** list it under "Won't implement" or in compatibility.md.
Better, make the parser reject `_ENV` as a declared local, parameter or
assignment target with a clear syntax error, so a 5.2-style sandbox idiom
fails loudly instead of silently leaking to the globals.

**Same documentation gap, but these fail loudly:**

- Lua 5.4's `\u{XXXX}` escape: rejected as "invalid escape sequence".
- Local attributes `<const>` and `<close>`: rejected as a syntax error.

Neither is listed as absent.

---

## 4. Stale or wrong statements about front-end limits

All of these are low severity. Each is a claim that does not match the code.

- **The dynamic-count sentinel comment in src/limits.rs is wrong.** The block
  above `MAX_LOCALS` says "Operand value 255 is reserved for dynamic argument
  and result counts". `MAX_FIXED_ARGS` is documented as "Maximum fixed
  arguments before the dynamic-count sentinel". docs/limits.md says "The
  dynamic-count sentinel is reserved".
  - Count operands are now 16 bits, and the sentinel is `instr::OPEN_COUNT`
    (`0xFFFF`), so 255 collides with nothing.
  - The stated reason for `MAX_FIXED_ARGS = 254` is therefore gone. The visible
    result is an asymmetry: `return v1, ..., v255` compiles (there is a test
    for it), and the runtime accepts 255 arguments (`MAX_ARGUMENTS = 255`). But
    `f(v1, ..., v255)` is rejected at parse time with `TooManyArguments`.
  - Either restate the reason (for example "kept to preserve the accepted
    program set") or lift the cap. Lifting it widens the accepted program set,
    so it is the maintainers' decision.
- **The 24-bit line comment in src/limits.rs describes code that does not
  exist.** It reads "Call sites pack lines into 24 bits; the compiler asserts
  this cap against that field's mask where the field is defined." I found no
  such packing, mask or assertion; the only 24-bit field is the jump offset.
  `MAX_SOURCE_BYTES` of 16 MiB would allow line numbers up to 2^24 + 1 anyway,
  so if such a field ever comes back, this cap would not protect it.
- **The README's parenthesis claim is wrong.** "Source limits" says "a nested
  parenthesis costs two levels ... Parentheses therefore nest about half as
  deep as other constructs." By docs/limits.md's own table (which matches the
  code):
  - table constructors and `if` bodies also cost 2, the same as parentheses;
  - calls and indexes cost 4, so they nest half as deep as parentheses;
  - only unary operators, `^` and `do`, loop or function bodies cost 1.
- **README and docs/limits.md name the wrong constant for field-store
  caching.** Both say field assignment sites past `MAX_TABLE_TEMPLATES` stop
  being inline-cached.
  - `SET_FIELD` caching is capped by `instr::MAX_CACHE_SLOTS`, in
    `Bytecode::assign_cache_slots`.
  - `MAX_TABLE_TEMPLATES` caps constructor templates; `try_use_table_template`
    falls back to ordinary construction past it.
  - Both happen to equal 255, so behaviour matches the text today. The
    sentence still describes the wrong mechanism, and it says nothing about the
    constructor fallback, which is what that constant actually controls.

---

## 5. A numeric `for` without a step can fail with "too many numbers" though the source adds no literal

Low severity. `parse_numeric_for_step` calls `find_or_add_number(1.0)` for the
implicit step. In a chunk whose number pool is full and lacks `1`,
`for i = a, b do ... end` fails with `TooManyNumbers`, although the user wrote
no new numeral.

Folding already treats a full pool as non-fatal: `try_fold_literal` documents
that "a full pool must leave otherwise valid source compilable". The implicit
step gets no such treatment. It is only reachable at 65536 distinct numerals,
so this is mostly a consistency note.

---

## Lateral notes (not defects by the definition above)

- **Codegen at the syntax budget is tested for only two tree shapes.**
  docs/limits.md promises "parsing at the syntax budget fits a 2 MiB thread in
  both settings", and compile includes codegen.
  - `deepest_nested_programs_compile_within_the_test_stack` drives only call
    chains and constructor chains to the deepest accepted depth.
  - `syntax_depth_limit_body` drives `do`, unary, `{`, `a[(` and `f(` to 10,000.
    That test fails at the budget, so it exercises the parser to full depth but
    never runs codegen on a full-depth tree.
  - Shapes whose codegen recursion has never run at the budget on the 2 MiB
    thread:
    - 199 nested `do`, `while`, `for` or `repeat` bodies;
    - about 99 nested `if` bodies;
    - about 197 chained `-` / `not`;
    - `^` chains;
    - about 98 parentheses.
  - `Generator::stmt` and `Generator::expr` are large `match` functions, and in
    the dev profile their frames are large. The parser's per-unit frames are
    also untested at the budget for `for`, `if` and `function`. `for` has the
    longest per-unit frame chain: `parse_statements_inner`, `push_statement`,
    `parse_for`, `parse_numeric_for`, then `parse_statements`.
  - The fix is cheap: an accepted-at-budget case per shape, compiled on
    `DEEP_PARSE_TEST_STACK_BYTES`.
- **Constructor flush order differs from reference.** Positional values are
  flushed in batches of 255 (reference uses 50), so `{v1, ..., v60, [1] = "x"}`
  ends with `t[1] == v1` in dellingr and `"x"` in reference Lua. The Lua manual
  leaves assignment order inside a constructor undefined, so I do not count this
  as a defect. The behaviour is deterministic and replay-stable.
- **Function-definition line differs.** `parse_fndef` records `defined_line`
  from the `(` token, not the `function` keyword, as reference does. This only
  matters for `function\nname()` layouts. The anonymous closure stamp
  (`ExprKind::Function.line`) is the line of the body's last token, not the
  `end` line. `CLOSURE` cannot fault, so this one is invisible today.
- **String-size error position.** `get_literal_string_contents` reports
  `StringSizeExceeded` at `error_here()`, the token after the string, not at
  the string token.

## Areas checked and found consistent

- **Hex numerals** (`numeral::parse_hex`): significand truncation with a sticky
  bit, the exponent clamp (`4*len + 2048` cannot change the result), carry into
  overflow at 2^1024, and the subnormal path (`shift` stays in `(b - 53, b]`,
  so `prefix << left` cannot overflow). All correct.
- **Decimal numerals:** the grammar is pre-validated before Rust's correctly
  rounded `f64::from_str`.
- **Constant folding:** runtime and folder share the arithmetic functions.
  NaN and -0 results are refused, which is necessary because
  `number_literal_key` merges the two zeros, and only +0 ever enters the pool.
  Left-associative chains fold only while the prefix is all-constant. `^` is
  never folded.
- **Upvalue resolution by name:** `resolve_in` is correct under shadowing in
  intermediate frames. Locals win, and an intermediate frame's suffix scope is
  frozen while a child is parsed. Capture logging lands in the owning frame, so
  block, loop, break and repeat-`until` close decisions are right. The redundant
  closes from outer blocks are harmless.
- **`Expr::may_execute`:** consistent with the absent arithmetic, comparison
  and concat metamethods. `GET_GLOBAL` and `GET_BUILTIN` cannot call out, and
  there is no `__gc`.
- **Table templates:** a nil named field leaves a tombstone. The runtime moves a
  re-added key to the back, so `pairs` order matches untemplated construction.
- **Lexer:** CR/LF pairing, the long-comment close scan, the deferred
  string-escape errors and the decimal/hex escape skip logic are all correct.
