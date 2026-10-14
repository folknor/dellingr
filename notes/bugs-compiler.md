# Bugs: compiler front end

Filed from the defect hunt at b83eab8 (scope: `src/compiler/**`, `src/numeral.rs`,
`src/instr.rs`). The hunter could not run anything; every repro below is a
reading-derived trace, not an observed run.

```
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
```

## CMP-001 - `break` inside a nested function binds to the enclosing function's loop

Hunter's label: HIGH.

Claims broken: errors are errors, not panics (README; `finalize`'s own comment
that a VM panic "is precisely the failure mode this crate refuses"), and output
matching reference Lua. Lua 5.2 and 5.4 both reject the program at compile time
with `break` outside a loop.

`Parser::loop_breaks` is parser-wide state. `parse_chunk` (parser.rs) saves and
resets `chunk`, `literal_indices`, `fold_barrier`, `locals` and `upvalues` for
each nested function, but not `loop_breaks`. For a function literal inside a
loop:

- `add_break` finds the outer function's `LoopContext`. It emits
  `CloseUpvalues(outer_close_slot)` + `Jump(0)` into the nested chunk and
  records the nested chunk's code index in the outer loop's `break_jumps`. It
  never raises `BreakOutsideLoop`.
- In the nested function, `Jump(0)` falls through, so the `break` silently does
  nothing.
- The outer loop's `exit_loop` then calls `patch_jump(break_idx, loop_end, ..)`
  on the outer chunk with an index from a different code vector:
  - If the index is past the outer chunk's length, `self.chunk.code[from] = ...`
    panics with an index out of bounds. Repro (short outer body, long inner
    function; the nested Jump lands at index 15 and the outer chunk is about 13
    instructions long when `exit_loop` runs):
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
  - Otherwise it overwrites an arbitrary outer instruction with a jump to the
    loop end. In
    `local n = 0 while true do n = n + 1; local f = function() break end; if n > 2 then break end end`
    outer index 1 (the `SetLocal` of `local n = 0`) becomes a jump. In release
    the verifier usually rejects the result as an `InternalError` ("operand
    stack state disagrees at control-flow join"). In debug, `finalize`'s
    `debug_assert!(false)` panics. If the clobbered instruction is stack-neutral,
    the verifier can accept the result, and the program is silently miscompiled.
- The `close_slot` copied into the nested chunk is an outer frame slot. If the
  nested function has no closures, stripping hides this. Otherwise it can fail
  the verifier's `close-upvalues level is out of range` check, or close the wrong
  level.

This affects `while`, `repeat`, numeric `for` and generic `for`. No test covers
it.

Hunter's direction: make loop context per-function state. Either take/restore
`loop_breaks` in `parse_chunk`, or, more cleanly, group `chunk`,
`literal_indices`, `fold_barrier`, `locals`, `upvalues`, `loop_breaks` and
arguably `nest_level` into one `FuncState` pushed and popped as a unit. The
current four parallel `outer_*` stacks are how a field got forgotten. Add a
`parse_str`-level regression test per loop kind.

## CMP-002 - Long `elseif` chains and long suffix chains hit the syntax-depth cap; reference Lua accepts them

Hunter's label: MEDIUM.

Claim broken: README "Runtime limits" describes the syntax budget as "following
the native-stack safeguard in reference Lua's `LUAI_MAXCCALLS`". Reference Lua
does not recurse for these constructs, so the budget does not follow it here.

- `parse_else_or_elseif` calls `parse_if_arm`, and each arm calls
  `enter_syntax_level` before recursing into the next arm. An `if` with about
  200 `elseif` arms fails with `chunk has too many syntax levels`. Reference
  `test_then_block` loops. Long elseif dispatch chains are common in generated
  or data-driven game scripts.
- `parse_prefix_extension` recurses once per suffix (`.f`, `[k]`, `(args)`,
  `:m()`), each with an `enter_syntax_level`. A builder chain
  `obj:a():b():c()...` or a field path with about 200 suffixes is rejected.
  Reference `suffixedexp` loops.

Both recursions are in tail position and can become loops, which also saves
native stack. The README line "Other constructs consume entries according to
their recursive parser calls" describes the code accurately. The claim that the
budget follows reference Lua does not.

## CMP-003 - An unterminated long comment is accepted silently

Hunter's label: LOW.

Claim broken: output matching reference Lua. No `-- DIFF:` marks it.

`Lexer::skip_comment`: inside `--[[` / `--[==[`, EOF just returns
(`None => return`), so the rest of the file disappears into the comment and the
program compiles. Lua 5.2 and 5.4 raise `unfinished long comment`. A missing
`]]` silently drops the code after it instead of producing a diagnostic.

## CMP-004 - Whether `(` counts as "at line start" depends on intervening comments

Hunter's label: LOW.

Claim broken: one program, one meaning. The same two lines are parsed
differently depending on whether a comment sits between them.

`LParenLineStart` is set only when `consume_whitespace` consumed a newline
immediately before the `(`. A newline consumed by a line comment does not count:

- `f\n(g)()` gives a `LParenLineStart` error (the 5.2 "ambiguous syntax"
  behaviour).
- `f -- note\n(g)()` compiles as the call `f(g)()` (the 5.4 behaviour).
- `f\n-- note\n(g)()` also compiles as a call, because the loop iteration after
  the comment resets `starts_line` from the whitespace after the comment only.

Hunter's direction: whichever rule is intended (error or call), decide it by
whether the `(` is on a later line than the previous token, for example by
comparing token `line` stamps in the parser, not through the lexer's whitespace
bookkeeping.

## CMP-005 - `fold_result_allowed` comment describes the literal pool's dedup incorrectly

Hunter's label: LOW (doc drift).

Claim broken: the comment's description of `find_or_add_number`.

The comment says `find_or_add_number` "dedups with `==`, which conflates 0.0 and
-0.0". It actually dedups through `number_literal_key`, a bit-pattern key that
deliberately folds signed zeroes together and gives NaN no key. The conclusion
(never fold to -0.0 or NaN) still holds. The stated mechanism is stale, and the
comment should cite `number_literal_key`.

## CMP-006 - `parse_numeric_for` patches `ForPrep` outside `patch_jump`

Hunter's label: smaller observation (latent).

`parse_numeric_for` patches `ForPrep` by direct assignment rather than through
`patch_jump`, so `fold_barrier` is not raised to its target. This is harmless
today because the target is always a statement start. If folding ever looks
further back than two instructions, it will break silently. Routing every
forward patch through `patch_jump` removes the special case.

## CMP-007 - `if` arms ending in `return` still emit an unconditional `Jump`

Hunter's label: smaller observation (cosmetic).

An `if`/`else` arm emits an unconditional `Jump` even when it ended in `return`.
The dead jump is unreachable and is never checked for stack discipline, but it
still adds bytecode.
