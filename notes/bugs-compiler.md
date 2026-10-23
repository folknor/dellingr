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

**Decided (user, 2026-10-23):** reject `_ENV` with a clear syntax error, and
list `\u{XXXX}` and `<const>`/`<close>` as absent.

**Fix direction:** list it under "Won't implement" or in compatibility.md.
Better, have the parser reject `_ENV` as a declared local, parameter or
assignment target with a clear syntax error, so a 5.2-style sandbox idiom fails
loudly instead of leaking to the globals.

**Same documentation gap, but these fail loudly:** Lua 5.4's `\u{XXXX}` escape
is rejected as "invalid escape sequence", and the local attributes `<const>` and
`<close>` are rejected as a syntax error. Neither is listed as absent from the
README's "Won't implement" list.

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

## CMP-011 - Register-limit errors are located at the end of the function; the assignment-offset check may now be redundant

Laterals from the first resolution wave, which made register exhaustion a typed
syntax error carrying the observed count and capped assignments at
`MAX_ASSIGNMENT_TARGETS` (255) targets.

- **Location.** The register-limit error is still located where `finish_chunk`
  reports it, at the end of the function, not at the statement or expression
  that ran out of registers. Every other limit error points at its source.
- **Redundant check.** `MAX_ASSIGNMENT_OFFSET` bounds `Field` and `Index`
  targets of a multiple assignment. With the target list capped at 255 the
  largest reachable offset is 254, so the check cannot fire; a `const`
  assertion now pins `MAX_ASSIGNMENT_TARGETS - 1 <= MAX_ASSIGNMENT_OFFSET` and
  the comment in `parse_assign` says so. Open: drop the check and its error
  kind, or keep both as a backstop.

## CMP-012 - Anonymous closures are stamped with the body's last token, and closure creation can fail

Lateral from the second resolution wave, which gave statement closures and
`VARARG` explicit line stamps. `ExprKind::Function`'s line is still the line of
the body's last token, not the `function` keyword. The hunt treated that as
invisible because a `CLOSURE` expression "cannot fault", but `push_closure`
runs `check_stack_space(1)` and `resolve_bytecode_runtime`, both fallible, so
`local f = function() ... end` near the stack cap reports its error at the
body's last line. Stamp the expression with the `function` keyword's line, as
the statement forms now are.
