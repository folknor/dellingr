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

Related: numerals now validate entirely in the lexer (`lex_numeral`), and the
parser converts the same bytes again, with `ParserFault::InvalidNumberToken`
for a lexer that broke its promise. A token payload would let the lexer hand
over the converted value and remove both the second conversion and that
fault; the numeral change deliberately left the payload shape to this entry.
