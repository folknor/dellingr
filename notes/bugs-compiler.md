# Compiler findings: lexer, parser, codegen, verifier

Filed from the 2026-11-04 defect hunt (scopes `compiler-front` and
`compiler-back`). The verifier-admitted `FOR_LOOP` panic is filed in
`notes/bugs-runtime.md`, because its fix lands in the VM. Hunt reports are work
documents and may be wrong; verify before fixing.

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

## COMP-007 - Codegen reports u8 overflows as MultiValueTailMissingInstruction

Scope: `compiler-back`. Smell; unreachable today.

`codegen.rs` reports two u8 overflows as
`ParserFault::MultiValueTailMissingInstruction`: the argument count in
`call_args`, and the multi-value tail width in `explist_to_regs`. Neither is
about a missing instruction. The parser bounds both
(`bounded_u8(.., MAX_FIXED_ARGS ..)` and `check_multi_assign`); if either guard
regresses, the internal fault names the wrong cause.

## COMP-008 - Spurious size check on the \z escape

Scope: `compiler-front`. Unreachable; trivia.

`decode_string_escape` calls `check_decoded_string_growth(decoded.len())` before
every escape, including `\z`, which appends nothing. A literal already holding
exactly `MAX_STRING_BYTES` decoded bytes would fail on a trailing `\z`.
`MAX_SOURCE_BYTES == MAX_STRING_BYTES` makes it unreachable, which
`get_literal_string_contents` already notes for the whole `StringSizeExceeded`
branch.

## COMP-009 - Line-attribution differences from reference Lua (informational)

Scope: `compiler-front`. No contract broken; recorded so nobody rediscovers them
as bugs. All follow the "last token consumed" rule.

- Generic-`for` TFORCALL/TFORLOOP are stamped at the `do` line
  (`call_line = last_line()` after `expect(Do)`); reference stamps the explist's
  first line. An iterator-call error in a multi-line
  `for k, v in \n pairs(t) \n do` header points at the `do` line.
- In `function a.b.c()`, the intermediate `GET_FIELD a.b` is stamped at the
  following `.` (`parse_fndecl_table` pushes the field after `try_pop(Dot)`);
  the same `a.b.c` in an expression is stamped at `b`. Differs only when the
  declaration name spans lines.
- Numeric-`for` FORPREP errors (non-number control) are at the `do` line;
  reference reports the `for` line.

## COMP-010 - Lateral: numeral validation is split inconsistently between lexer and parser

Scope: `compiler-front`. Structural.

Hex numerals are validated eagerly in the lexer, twice (after the mantissa and
after the exponent, calling `parse_lua_numeral` both times). Decimal exponent
errors (`1e`, `1e+`) are deferred to `parse_expr_base`. Error timing therefore
differs by radix: `x = 0x1p` fails at lex time, `x = 1e` at parse time. A single
"lex the maximal numeral span, validate once" rule, as Lua 5.4's `read_numeral`
does, would make it uniform and would also give `1..2` Lua's "malformed number
near '1..2'" instead of the current two-token `1.` `.2` parse error.

## COMP-011 - Lateral: codegen runs before the terminator is checked

Scope: `compiler-front`. Harmless; undesigned error precedence.

`finish_chunk` runs codegen for a nested function before its `end` is consumed,
and for the main chunk before `<eof>` is checked. A codegen limit error
(`TooManyRegisters`, `JumpTooFar`) therefore wins over a syntax error on the
terminator, and `JumpTooFar` is located at the terminator via `error_here`, not
at the offending jump.

## COMP-012 - Lateral: decoded string contents travel through a keyed side table

Scope: `compiler-front`. Structural.

The lexer stores decoded string contents in
`literal_string_contents: Vec<(usize, LiteralStringContents)>`, keyed by token
start, and the parser retrieves them with a linear `position` plus `remove`.
That works because at most two tokens are buffered. A token payload
(`Token { kind, span, payload: Option<LiteralStringContents> }` or an owned
`TokenValue`) would remove the keyed side channel, the `MissingStringContents`
internal fault and the `tok.len < 2` sanity check.

## COMP-013 - Lateral: line-info length is checked twice, and the reason is unstated

Scope: `compiler-back`. Harmless.

`compiler::finalize_at_depth` checks `code.len() != line_info.len()` itself
(`CompilerFault::LineInfoDesynced`), and `validate_bytecode` checks it again
(`LineInfoLengthMismatch`). The first check exists so that
`validation_error`'s `debug_assert!(false)` does not fire for that case (a unit
test relies on it), but nothing in the code says so.

## COMP-014 - Lateral: the verifier does not require a root chunk to have zero upvalue descriptors

Scope: `compiler-back`. No reachable consequence found.

Compiled roots never have upvalue descriptors, and snapshot closures carry an
explicitly counted upvalue list (`validate_closures`). If a new path ever builds
a closure from a raw root `Bytecode` without supplying upvalues, `GET_UPVAL`
would index out of bounds.
