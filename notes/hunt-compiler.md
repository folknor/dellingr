# Compiler defect hunt

Scope: `src/compiler/**`, `src/compiler.rs` (finalize/strip/cache slots), `src/numeral.rs`, `src/instr.rs`.
All findings come from reading the code. The run attempts were denied, so none of the
repro scripts below were executed. The traces are given step by step so they can be
checked against a build.

## 1. HIGH - `break` inside a nested function binds to the enclosing function's loop (wrong code or panic)

Claims broken: "Errors kill the callback by design" / errors are catchable rather than
panics (README, finalize's own comment: "a VM panic ... is precisely the failure mode this
crate refuses"), and "output matching reference Lua". Lua 5.2 and 5.4 both reject the
program at compile time: `break` outside a loop.

`Parser::loop_breaks` belongs to the whole parser. `parse_chunk` (parser.rs) saves and resets
`chunk`, `literal_indices`, `fold_barrier`, `locals` and `upvalues` for each nested function,
but **not** `loop_breaks`. Inside a function literal that sits within a loop:

- `add_break` (parser.rs) finds the *outer* function's `LoopContext`. It emits
  `CloseUpvalues(outer_close_slot)` + `Jump(0)` into the **nested** chunk's code and records
  the nested chunk's code index in the **outer** loop's `break_jumps`. It does not raise
  `BreakOutsideLoop`.
- In the nested function, `Jump(0)` falls through, so the `break` silently does nothing.
- `exit_loop` on the outer loop then runs `patch_jump(break_idx, loop_end, ..)` against the
  **outer** chunk, using an index that belongs to a different code vector:
  - If that index is `>=` the outer chunk's current length, `self.chunk.code[from] = ...`
    panics with an index out of bounds. Repro (outer loop body is short, inner function is long):
    ```lua
    local n = 0
    while n < 3 do
      n = n + 1
      local f = function()
        local a = 1 local b = 2 local c = 3 local d = 4 local e = 5 local g = 6 local h = 7
        break
      end
    end
    ```
    Trace: the nested `break`'s Jump lands at index 15, and the outer chunk is about 13
    instructions long when `exit_loop` runs.
  - Otherwise it overwrites an arbitrary outer instruction with a `Jump` to the loop end. In
    ```lua
    local n = 0
    while true do n = n + 1; local f = function() break end; if n > 2 then break end end
    ```
    outer index 1 (the `SetLocal` of `local n = 0`) becomes a jump. In release, the verifier
    usually rejects the result as an `InternalError` ("operand stack state disagrees at
    control-flow join"). In debug, the `debug_assert!(false)` in `finalize` panics. If the
    clobbered instruction is stack-neutral, the verifier can accept the result and the program
    is silently miscompiled.
- The `close_slot` copied into the nested chunk is an outer frame slot. If the nested function
  has no closures, stripping hides this. Otherwise it can fail the verifier's
  `close-upvalues level is out of range` check, or close the wrong level.

This applies to `while`, `repeat`, numeric `for` and generic `for` alike. No test covers it:
nothing in `src/`, `tests/` or `examples/` contains `function() break`.

Fix: treat loop context as per-function state. Take/restore `loop_breaks` in `parse_chunk`
the same way as `locals`. The cleaner fix is to group every per-function field
(`chunk`, `literal_indices`, `fold_barrier`, `locals`, `upvalues`, `loop_breaks`, and arguably
`nest_level`) into one `FuncState` struct that is pushed and popped as a unit. The current
shape needs four parallel `outer_*` stacks and leaves room to forget a field, which is how
this bug happened. Add a regression test at the `parse_str` level for each loop kind.

## 2. MEDIUM - Long `elseif` chains and long suffix chains hit the syntax-depth cap; reference Lua accepts them

Claim broken: README "Runtime limits": the syntax budget is described as "following the
native-stack safeguard in reference Lua's `LUAI_MAXCCALLS`". It does not follow that
safeguard here, because reference Lua does not recurse for these constructs.

- `parse_else_or_elseif` calls `parse_if_arm`, and each arm calls `enter_syntax_level` before
  recursing into the next arm. An `if` with about 200 `elseif` arms therefore fails with
  `chunk has too many syntax levels`. Reference `test_then_block` loops, so it accepts any
  number of arms. Long elseif dispatch chains are common in generated or data-driven game
  scripts.
- `parse_prefix_extension` recurses once per suffix (`.f`, `[k]`, `(args)`, `:m()`), with an
  `enter_syntax_level` each time. A builder chain such as `obj:a():b():c()...` or a deep field
  path with about 200 suffixes is rejected. Reference `suffixedexp` loops.

Both recursions are tail positions and can be rewritten as loops, which also saves native
stack. The README's line "Other constructs consume entries according to their recursive
parser calls" accurately describes the code. The claim that the budget follows reference
Lua does not hold.

## 3. LOW - An unterminated long comment is accepted silently

`Lexer::skip_comment`: inside `--[[` / `--[==[`, reaching EOF just returns (`None => return`),
so the rest of the file disappears into the comment and the program compiles. Reference Lua
(5.2 and 5.4) raises `unfinished long comment`. A missing `]]` therefore makes code
after it disappear silently instead of producing a diagnostic. This breaks the
"matches reference Lua" claim, and no `-- DIFF:` marks it.

## 4. LOW - Whether `(` counts as "at line start" depends on comments

`LParenLineStart` is set only when `consume_whitespace` consumed a newline immediately before
the `(`. A newline consumed by a line comment does not count:

- `f\n(g)()` gives a `LParenLineStart` error (the 5.2 "ambiguous syntax" behaviour), but
- `f -- note\n(g)()` compiles as the call `f(g)()` (the 5.4 behaviour), and
- `f\n-- note\n(g)()` also compiles as a call, because the loop iteration that follows the
  comment resets `starts_line` from the whitespace after the comment only.

The same two-line program therefore means different things depending on whether a comment
sits between the lines. Whichever rule is intended (error or call), it should be decided by
"is the `(` on a later line than the previous token". Comparing token `line` stamps in the
parser does this, and the lexer's whitespace bookkeeping is not needed.

## 5. LOW (doc drift) - `fold_result_allowed` describes the literal pool's dedup incorrectly

The comment says `find_or_add_number` "dedups with `==`, which conflates 0.0 and -0.0". It
actually dedups through `number_literal_key`, a bit-pattern key that deliberately folds
signed zeroes together and gives NaN no key. The conclusion (never fold to -0.0 or NaN) still
holds. The stated mechanism is stale, so the comment should cite `number_literal_key`.

## Checked and found sound (so they need not be re-audited)

- Constant folding: the operands are always the two trailing `PushNum`s of the current binary
  op. Every lhs shape that ends in `PushNum` without being a lone literal (`and`/`or`) is
  fenced by `fold_barrier`. Backward targets (while/repeat/for) never land on a second
  operand. `^` is not folded. NaN and -0.0 results are refused. A full literal pool falls back
  to the unfolded instruction.
- Comparison+branch fusion: when `and`/`or` patches its target to the would-be branch slot,
  `fold_barrier` blocks the fusion. NaN handling for `<=`/`>=` via `(Ordering, negate)` returns
  false on `partial_cmp == None`, which is correct.
- Multi-assignment receiver offsets (`num_lvals - i - 1`) stay correct for every mix of local,
  field and index targets, and evaluation order matches 5.4 (`a, a = 1, 2` gives 1).
- Tail expansion (`open_tail`) only fires on a bare call or vararg, and parenthesised forms
  truncate.
- Upvalue resolution through `outer_locals`/`outer_upvalues` is correct, given that outer
  scopes are frozen while a nested function is parsed.
- `strip_dead_instructions`: the remap is correct for removed targets, and dropping
  `CloseUpvalues` is sound only because closures in *this* chunk are the only way to open an
  upvalue on its frame.
- Cache-slot assignment and the verifier's independent restatement agree on every sentinel
  (GET_GLOBAL 255, SET_GLOBAL and TFOR_CALL biased 0, GET_FIELD/SET_FIELD 255).
- Hex-float parsing (`numeral.rs`): rounding, subnormals and saturation of huge exponents
  are correct. `lua_modulo` matches 5.4's `luai_nummod` including infinities and signed zero.
- Lexer number boundaries (`1..2`, `5..x`, `3e`) all end in a syntax error, as in reference
  Lua.

## Smaller observations

- `parse_numeric_for` patches `ForPrep` by direct assignment, not through `patch_jump`, so
  `fold_barrier` is not raised to its target. This is harmless today because the target is
  always a statement start. If folding ever looks further back than two instructions, it will
  break silently. Routing every forward patch through `patch_jump` would remove the
  special case.
- The `if`/`else` arm emits an unconditional `Jump` even when the arm ended in `return`. The
  dead code is never verified for stack discipline (it is unreachable) but still costs a
  little bytecode size. This is cosmetic.
