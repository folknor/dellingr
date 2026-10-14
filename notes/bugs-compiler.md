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

## CMP-007 - `if` arms ending in `return` still emit an unconditional `Jump`

Hunter's label: smaller observation (cosmetic).

The arms of an `if` are now parsed in a loop in `parse_if_chain`
(`parser/stmt.rs`), which is where the dead jump would be dropped.

An `if`/`else` arm emits an unconditional `Jump` even when it ended in `return`.
The dead jump is unreachable and is never checked for stack discipline, but it
still adds bytecode.
