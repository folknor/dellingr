# Compiler findings: lexer, parser, codegen, verifier

Filed from the 2026-11-04 defect hunt (scopes `compiler-front` and
`compiler-back`). Hunt reports are work documents and may be wrong; verify
before acting.

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

## COMP-010 - Numeral validation is split between the lexer and the parser

Scope: `compiler-front`. Structural. Re-observed by a fix wave.

Hex numerals are validated in the lexer, twice: after the mantissa and again
after the exponent, calling `parse_lua_numeral` both times. Decimal exponent
errors (`1e`, `1e+`) are found later, in `parse_expr_base`. Error timing
therefore depends on the radix: `x = 0x1p` fails while lexing, `x = 1e` while
parsing. `1..2` lexes as the two tokens `1.` and `.2` and produces a parse
error, where Lua 5.4 reports "malformed number near '1..2'". Since a fix wave,
numerals cut off at the end of the source raise `UnfinishedNumber` from the
parser side; that path is part of the same split.

Approaches raised so far (not exhaustive): lex the maximal numeral span and
validate it once, as Lua 5.4's `read_numeral` does.

## COMP-012 - Decoded string contents travel through a keyed side table

Scope: `compiler-front`. Structural.

The lexer stores decoded string contents in
`literal_string_contents: Vec<(usize, LiteralStringContents)>`, keyed by token
start, and the parser retrieves them with a linear `position` plus `remove`.
This works because at most two tokens are buffered. The arrangement brings with
it the `MissingStringContents` internal fault and a `tok.len < 2` sanity check.

Approaches raised so far (not exhaustive): carry the decoded contents on the
token itself (for example `Token { kind, span, payload:
Option<LiteralStringContents> }`, or an owned `TokenValue`).
