# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #18, #19, #40, #41, #42, #43: lexer and parser conformance

Six small divergences, all in `src/compiler/lexer.rs` and
`src/compiler/parser.rs`. Grouped because they share a file, a test surface,
and a theme: dellingr's lexer accepts or rejects things reference Lua does not.

Two verified by running; the rest are byte-level or established by reading.

### #40 (Low) - `break;` is rejected

**Verified:** `while true do break; end` gives `2:20: 'end' expected near ';'`.
Reference parses it fine (it went on to fail at a later line). Valid in 5.1,
5.2 and 5.4.

`parser.rs:538-542`: after `break`, `parse_statements` exits without consuming
an optional `;` - compare `parse_return` (`stmt.rs:40`), which does - and
without allowing further statements. So `while true do break print(1) end` is
also rejected, though that is valid dead code in reference.

The trailing-semicolon case is the one real scripts hit. Minimal fix:
`self.input.try_pop(TokenType::Semi)?;` after `add_break()`.

### #41 (Low) - a numeral touching a letter is only rejected for hex digits

**Verified:** `print(3or 4)` prints `3` in dellingr; reference errors
`malformed number near '3o'`.

`lexer.rs` `lex_exponent:465-468` and the hex tail check at `403-405` reject a
trailing letter only when `is_ascii_hexdigit()`. Reference rejects **any**
alphanumeric touching a numeral. Same class: `1e5or 2`, and `0x5rad` which is
already codified in `test_lexer07`.

Fix: reject `is_ascii_alphanumeric()` (and `_`) immediately after a numeral.

### #42 (Low) - vertical tab is not treated as whitespace

`lexer.rs consume_whitespace` (264-276) uses `is_ascii_whitespace`, which
excludes VT (0x0B); C's `isspace` includes it. So reference accepts VT between
tokens and after `\z`, and dellingr raises `InvalidCharacter`. The parser-side
`\z` skip (`parser.rs:339`) has the same problem.

This is the exact sibling of the pattern-matcher `%s` bug fixed in the previous
loop, and `src/numeral.rs`'s `is_lua_whitespace` - now `pub(crate)` - already
has the correct set. Reuse it in both places rather than adding a third
definition.

### #18 (Medium) - bare CR inside string literals

`lexer.rs` `lex_string` (353-368) rejects only `'\n'` inside a string, so
`"a<CR>b"` compiles and embeds a raw 0x0D byte; reference treats both CR and LF
as "unfinished string".

`parser.rs` `get_literal_string_contents:343` maps escape `\<LF>` to `'\n'`,
but `\<CR>` falls into the `_ =>` arm and errors `InvalidEscapeSequence`.
Reference maps `\<CR>`, `\<CR><LF>` and `\<LF><CR>` all to a single `'\n'`;
dellingr errors on the first and produces two bytes `\n\r` for the last.

Any script written or transferred with CR or CRLF line endings inside string
literals diverges.

Fix: treat `'\r'` like `'\n'` in `lex_string` for the unescaped case, and
accept `\<CR>`, `\<CR><LF>`, `\<LF><CR>` as one `'\n'` in the escape decoder.

### #19 (Medium) - `--[=[ ... ]=]` leveled long comments are misparsed

`lexer.rs skip_comment` (210-236) recognises only `--[[`. A leveled opener
`--[=[` falls through to the single-line branch, so only the first line is
skipped and the comment **body is lexed as live source**. Reference skips the
whole block.

```lua
--[=[
this line is a comment in reference Lua
]=]
print("ok")
```

Reference prints `ok`; dellingr tries to parse line 2 as code - usually a
confusing syntax error, but if the body happens to be valid Lua it would
execute.

Given long *strings* already produce a dedicated `LongStringUnsupported` error
at `[=` / `[[`, the consistent fix is to error on `--[=` too. Supporting levels
in the comment skipper is the alternative - comments are not on the README's
"Won't implement" list and `--[[` already works.

Related, lower priority: an unfinished `--[[` at EOF is silently accepted
(`skip_comment` returns on None, the lexer emits EOF); reference errors
`unfinished long comment`.

### #43 (Low) - the ambiguous-call check is keyed to LF only

`consume_whitespace` sets `starts_line` only on `'\n'` (`lexer.rs:270`). A file
with bare-CR line endings never produces `LParenLineStart`, so the deliberate
"ambiguous function call" error silently does not fire: `f<CR>(g)` parses as a
call while `f<LF>(g)` errors. After the L17 bare-CR line-counting fix this is
the one remaining LF-only assumption in the lexer.

Fix: set `ret = true` for the bare-CR branch too.

---

## Agreed implementation plan

Two qualifications from review. **#41 is a Lua 5.4 tightening, not universal
conformance** - 5.2 accepts `3or`. Take 5.4's behaviour, since rejecting glued
tokens is safer, but document the 5.2 difference in the test. And **#43
enforces dellingr's own ambiguity policy** rather than reference behaviour:
both references accept `f<LF>(g)`. Fixing it makes the deliberate rejection
consistent across line endings, which is the point.

### Seventh bug, found during review

`skip_comment` terminates a short `--` comment only on LF. Verified: under CR
line endings `-- comment<CR>print("ok")` is swallowed entirely by dellingr,
while both references print `ok`. Same newline cleanup; include it.

### #19 - support leveled long comments, do not error

Long comments are lexical trivia, not the deliberately omitted long-*string*
value feature. Supporting them adds no value type, allocation facility, opcode
or cost-model concern, and the lexer can scan them deterministically in linear
time with no allocation. It also removes the genuinely dangerous current
behaviour where apparently-commented code becomes executable. Long strings stay
rejected; that boundary is unaffected.

Unfinished-long-comment diagnostics are deliberately **out of scope** - that
needs an error-policy decision (`UnexpectedEof` versus a new variant) and
closed leveled comments can be fixed independently.

### #40 - allow statements after `break` too, not just the semicolon

In `parse_statements_inner` (`parser.rs:566`), consume `break`, call
`add_break()`, then continue the statement loop; the existing `Semi` arm
already handles one or many semicolons. Following bytecode is emitted but
unreachable, because `add_break()` already emitted an unconditional jump
patched to the loop exit. Simpler and more consistent than special-casing one
semicolon. `return` staying terminal is not a contradiction - it has separate
grammar and lowering, and reference also accepts ordinary statements after
`break`.

### `src/compiler/lexer.rs`

- Import `crate::numeral::is_lua_whitespace`.
- `consume_whitespace`: use the Lua predicate instead of `is_ascii_whitespace`
  (guard the char-to-byte conversion with `is_ascii()`), and set `starts_line`
  for **either** CR or LF (#42, #43).
- `next_char`: treat CRLF and LFCR each as one logical newline, deferring the
  first member and recording the line start after the second.
- `lex_string`: reject unescaped CR exactly like unescaped LF; after an escaped
  physical CR or LF, consume an immediately following opposite newline byte so
  CRLF and LFCR stay one escape token (#18).
- `lex_full_number` and `lex_exponent`: reject a trailing ASCII alphanumeric or
  `_` (#41).
- `skip_comment`: after the first `[`, count consecutive `=` and recognise a
  long comment only when another `[` follows; scan for `]`, the same count of
  `=`, then `]`. A failed opener probe stays an ordinary single-line comment,
  since the consumed characters are already comment text. Level-zero `--[[`
  keeps working (#19). Also terminate short `--` comments on CR as well as LF
  (the seventh bug).
- Keep `[=[...]=]` long *strings* routed to `LongStringUnsupported`.

### `src/compiler/parser.rs`

- `get_literal_string_contents`: use `is_lua_whitespace` for `\z`; separate the
  named `\n` escape from physical escaped newlines; map escaped CR and LF both
  to `b'\n'`; when the next byte forms CRLF or LFCR, advance past it so the
  pair yields exactly one byte.
- `parse_statements_inner`: drop the terminating `break Ok(())` from the
  `TokenType::Break` arm.

No change needed in `src/numeral.rs`.

### Testing strategy for the byte-level cases

**Use Rust string escapes in unit tests; do not put raw CR/VT bytes in checked
-in files.** The Rust source stays ordinary ASCII while the compiled test value
holds the exact bytes:

```rust
let cr_source = "return \"a\\\r\nb\"";
let vt_source = "return \"a\\z\x0b b\"";
```

Add `as_bytes()` assertions for `b'\r'`, `b'\n'` and `0x0b` where useful, so
the fixture shape is itself pinned. Keep CR/VT cases out of `examples/`.
`tests/string_bytes.rs` is **not** a precedent here - its gremlins exclusion is
for literal Unicode test data, not line endings - and no new exclusion is
needed.

### Corpus impact

No example relies on #19 or #41, but **`test_lexer07` (`lexer.rs:654`)
explicitly asserts that `0x5rad` lexes as a hex token followed by an
identifier**. It must be inverted, not preserved. `examples/comments.lua` uses
only level-zero `--[[`. Occurrences like `"1_0"` in `tonumber` tests are inside
Lua string literals and unaffected.

What changes for previously-compiling source: #18 rejects bare-CR strings,
newly accepts escaped CR/CRLF, and changes escaped-LFCR output; #19 can change
behaviour where a "comment" body was being executed; #40 only *adds* accepted
programs and leaves ordinary `break` bytecode unchanged; #41 rejects 5.2-style
adjacency; #42 newly accepts VT and changes `\z` contents; #43 rejects bare-CR
continuations dellingr previously accepted.

### Tests

Lexer unit module:

- `bare_cr_in_short_string_is_unclosed`
- `vertical_tab_is_lua_whitespace`
- `bare_cr_marks_lparen_as_line_start`
- `leveled_long_comments_use_exact_matching_level` - levels 0, 1, 2 plus a
  mismatched inner delimiter
- `short_comment_ends_at_cr`
- replace `test_lexer07` with `numeral_identifier_adjacency_is_malformed`
  covering `3or`, `1e5or`, `3_name`, `0x5rad`, asserting `SyntaxError::BadNumber`
- keep the existing valid hex-float tests

`src/compiler/parser/tests.rs`:

- `literal_string_normalizes_escaped_physical_newlines` - bare CR, CRLF, LFCR
  all decoding to `b"a\nb"`
- `literal_string_z_skips_vertical_tab` - assert `b"ab"`, not merely that it
  compiles
- `break_allows_semicolons_and_following_statements` - `break;`, repeated
  semicolons, and a dead assignment after `break`
- extend `test33` to assert the same `LParenLineStart` error and `(2, 1)`
  location for LF, CR and CRLF

Differential coverage:

- extend `examples/comments.lua` with a leveled comment whose body would print
  or mutate state if executed
- extend `examples/loops.lua` with `break;` followed by a dead assignment, then
  assert the assignment did not run

### Superseded questions

1. For #19, error on `--[=` or support levels? Erroring is consistent with the
   long-string decision, but a leveled comment is a much more ordinary thing to
   find in real source than a leveled long string, and the current behaviour -
   silently executing the comment body if it parses - is the worst of the three
   options. Recommend one.
2. #18, #42 and #43 all need byte-level fixtures containing raw CR and VT.
   `notes/bugs.md` warns that editors normalise line endings. What is the
   robust way to write these tests in this repo - build the source as a Rust
   byte string in a unit test rather than an `examples/` file? Note
   `tests/string_bytes.rs` is already excluded from the gremlins scan, which
   suggests a precedent.
3. Do any of these change what currently-valid programs compile to? #41 and #19
   are both *accepts-more* divergences being tightened, so they can only reject
   things that previously compiled. Is there anything in `examples/` or the
   test corpus that relies on the current leniency?
4. #40's second half - statements after `break` - is valid dead code in
   reference. Worth fixing, or is rejecting it a defensible divergence to
   document instead? I lean towards fixing the semicolon and documenting the
   dead-code case, but say if that is inconsistent.

Read `src/compiler/lexer.rs`, `src/compiler/parser.rs`'s `parse_statements` and
`get_literal_string_contents`, and `src/numeral.rs`'s `is_lua_whitespace`.
