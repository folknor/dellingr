# Hunt: compiler-front (lexer, parser, ast, token, numeral)

Scope read in full: `src/compiler/lexer.rs`, `token.rs`, `parser.rs`,
`parser/{expr,func,state,stmt,table,upvalue}.rs`, `ast.rs`, `src/numeral.rs`,
plus the codegen consumers of the loop/close fields and the contract docs
(README, `docs/compatibility.md`, `docs/limits.md`, `docs/budget.md`,
`docs/determinism.md`, `reference/error-reporting.md`).

Summary: I found no wrong-result bug in the front end itself. Upvalue
resolution, the capture log and close decisions, constant folding, the
numeral parser, string escape decoding, CR/LF line counting and the
syntax-depth accounting all hold up under the edge cases I traced. The
defects are mostly gaps between `docs/compatibility.md` ("A difference not
listed here is a bug") and what the lexer/numeral code actually does, plus a
traceback-line inconsistency and some docs that have gone stale.

---

## F1. `%` follows Lua 5.4's fmod definition; compatibility.md does not list the 5.2 divergence (medium)

`numeral::lua_modulo` (shared by the VM and the constant folder) computes
`fmod(a, b)` and then corrects the sign: that is Lua 5.4's `luai_nummod`. Lua
5.2 defines `a % b` as `a - floor(a/b)*b`. The two give results that differ
visibly in printed output for ordinary inputs:

- `1 % 0.1`: 5.2 gives `0` (because `1/0.1` rounds to exactly 10), while
  5.4 and dellingr give `0.1` (`0.09999999999999995`).
- `5 % math.huge`: 5.2 gives `nan`, 5.4 and dellingr give `5`.
  `-5 % math.huge`: 5.2 `nan`, dellingr `inf`.

Broken claim: `docs/compatibility.md` opens with "Every deliberate divergence
from Lua 5.2 and 5.4 ... A difference not listed here is a bug". Where the
doc picks 5.4 over 5.2 elsewhere it says so (`gsub` negative `n`, integer
arguments, `error` prefixes), but it says nothing about `%`. The fix is a
line under "Numbers", unless the 5.2 definition was the one intended.
Changing the semantics would be replay-visible and would also change folded
literals.

## F2. `goto` is an ordinary identifier (medium-low)

`token.rs` `KEYWORDS` has no `goto`, so the lexer returns it as `Identifier`.
As a result:

- `local goto = 1`, `goto = 5` and `t.goto` as a variable all compile. Lua
  5.2 and 5.4 reject all three because `goto` is reserved in both. This is an
  accepted-program divergence, and `docs/compatibility.md` does not list it.
  The README's "Won't implement: `goto`/labels" says the feature is absent.
  It does not say the name becomes free.
- A script author who writes `goto continue` gets `syntax error near
  'continue'`: the parser reads `goto` as a global-variable expression
  statement. `::continue::` gives `unexpected symbol near ':'`. Neither
  message says that `goto` is unsupported. Compare `_ENV`, which has a
  dedicated "not supported" error (`SyntaxError::UnsupportedEnvironmentName`),
  and long strings (`LongStringUnsupported`), which are handled the same way
  for the same reason.

Broken claims: compatibility.md's "not listed here is a bug". The project's
own rule that a misleading message is a defect for the script author also
applies. Two possible fixes: reserve `goto` and give it an "unsupported"
error, as with `_ENV` and long strings, or document that the name is free.

## F3. Other unlisted 5.2/5.4 picks made by the lexer/parser/numeral code (low)

Each of these is a deliberate choice of one reference over the other, and
each is recorded only in a test comment or the CHANGELOG, not in
`docs/compatibility.md`:

- **Numeral glued to a letter (`3or 4`, `1e5or`)** is rejected
  (`lexer.rs` `lex_exponent` / hex branch, `is_identifier_continue`). Lua 5.2
  accepts `3or 4`. Recorded only in the `numeral_identifier_adjacency_is_malformed`
  test comment and CHANGELOG ("tracking 5.4").
- **`(` on a new line continues a call** (`f\n(g)`; `parse_prefix_suffix`).
  Lua 5.2 rejects it with "ambiguous syntax (function call x new
  statement)". The test `a_new_line_paren_continues_the_call` says "as in
  reference Lua", which holds only for 5.4.
- **Hex numerals without `.`/`p` do not wrap.** `0x10000000000000000` is
  `1.8446744073709552e19` and `0xffffffffffffffff` is `1.8e19`. In Lua 5.4
  they wrap to `0` and `-1` ("if the value overflows, it wraps around"). The
  same goes for `tonumber("0xffffffffffffffff")` through
  `parse_lua_numeral`. compatibility.md lists the wrap difference for
  `tonumber(s, base)` only. "Numbers are f64 only" arguably implies it, but
  the doc spells out the analogous `tonumber(s, base)` case and not this one.
- **Lateral (CLI, `src/main.rs`):** `lua script.lua` skips a first line that
  starts with `#` (a shebang), but `dellingr script.lua` reports
  `invalid character '#'`/`unexpected symbol`. `docs/cli.md` does not
  mention it.

## F4. `defined_line` is the `(` line, not the `function` keyword's (low)

`parser/func.rs` `parse_fndef`: `let defined_line = self.input.peek()?.line;`
runs after the `function` keyword and the name have been consumed, so it
stamps the line of the parameter list's `(`. Meanwhile:

- the closure-creation stamp for a function expression is the `function`
  keyword's line (`ExprKind::Function { line: tok.line }` in
  `parse_expr_base`), and CHANGELOG says "`function` statements and
  expressions [record] their `function` keyword's" line;
- reference Lua's `linedefined` is the `function` keyword's line in both 5.2
  and 5.4;
- `reference/error-reporting.md` says an unnamed function is shown as
  `function <source:line>` "with the line it was defined on".

For `local f = function\n(x) ... end`, the traceback label says line 2 while
the CLOSURE instruction is stamped line 1. The value is saved in snapshots
(`Bytecode::defined_line`), so a fix is replay/snapshot-visible and has to be
called out as such.

## F5. `parse_fndef` documents and returns a stamp that nobody uses (smell)

`parse_fndef` is documented as returning "the new function's nested-chunk
index and the stamp of its closure creation". The second value,
`self.input.last_line()` taken just before `end`, is discarded by every
caller (`let (nested, _) = ...` in five places). The real closure stamps come
from elsewhere (`tok.line` of `function`, the statement's `line`). The dead
return value contradicts the stamping rule actually in use. Similarly, the
`bool` from `parse_statements` ("whether the block's own last statement was a
`return`") is ignored by every caller; codegen recomputes it from
`block.stmts.last()` in `if_stmt`. Both can go.

## F6. Stale or wrong doc comments in scope (low)

- `TokenStream::line_and_column` and `Lexer::line_and_col`: "Returns the
  current position of the `TokenStream`/`Lexer`". Both actually map a given
  byte offset to (line, column).
- `TokenStream::pos`: "Returns how many bytes have been read". It returns
  the start of the lookahead token when one is buffered, and otherwise the
  offset just past the last consumed token. As a consequence, `error_here`
  points at two different places depending on lookahead state. After
  `break` (outside a loop) or `...` (outside a vararg function), the error
  column is just past the offending token rather than at it.
- `ast.rs` `Stmt::While::body_close`: "...at the end of each iteration and
  before each break". Codegen uses `break_close`, not `body_close`, for
  breaks (`codegen.rs` `Stmt::Break` reads `LoopGen::break_close`).
- `ast.rs` module doc: "walking the tree recursively never goes deeper than
  the parser's charged syntax depth". The uncharged precedence levels (`Or`,
  `And`, `Compare`, `Concat`, two `Arith` levels) nest between charged
  `parse_expr`/`parse_unary` levels, so tree depth is a constant multiple of
  charged depth, not bounded by it. The real promise is that tree depth
  mirrors the parser's own native recursion. The wording should say that.
- `error.rs` `SyntaxError::TooManyArguments` is documented as "A
  fixed-arity call uses the dynamic-argument sentinel". The parser raises it
  for more than `MAX_FIXED_ARGS` fixed arguments (`checked_fixed_arg_count`).
- `SyntaxError::TooManyTableFields` ("More fields in a single table
  constructor than the VM accepts") vs `docs/limits.md` "Constructors have no
  overall entry limit". The parser raises it when the `u16` batch counter
  overflows (`parse_table`). That needs about 16.7M positional entries,
  which the 16 MiB source cap makes unreachable, so in practice the doc is
  right and the error variant is dead. The doc and the variant should agree.

## F7. `Error::is_recoverable` misses two EOF cases it claims to cover (low)

`is_recoverable` is documented as "those that appending more source text can
fix". Two cases that appending does fix are reported unrecoverable:

- A numeral cut off at end of input (`x = 1e`, `x = 0x`, `x = 0x1p`) is
  `BadNumber`. `1e` + `5` is valid.
- A short string cut off at end of input (`print("abc`) is `UnclosedString`.
  The code comment deliberately excludes it because the same kind also
  covers a string broken by a newline, which cannot be fixed. The lexer
  knows which case it is (the `None` arm vs the newline arm of
  `lex_string`), so the two could be told apart.

This matters only for host REPLs, but the method exists solely for them.

## F8. Line-attribution observations (no contract broken; informational)

These follow the "last token consumed" rule but differ from reference Lua.
They are recorded here so nobody rediscovers them as bugs:

- Generic-`for` TFORCALL/TFORLOOP are stamped at the `do` line
  (`call_line = last_line()` after `expect(Do)`). Reference stamps the
  explist's first line. An iterator-call error in a multi-line `for k, v in
  \n pairs(t) \n do` header points at the `do` line.
- In `function a.b.c()`, the intermediate `GET_FIELD a.b` is stamped at the
  following `.` (`parse_fndecl_table` pushes the field after
  `try_pop(Dot)`). The same `a.b.c` in an expression is stamped at `b`. This
  differs only when the declaration name spans lines.
- Numeric-`for` FORPREP errors (non-number control) are at the `do` line.
  Reference reports the `for` line.

## F9. Spurious size check on `\z` (unreachable; trivia)

`decode_string_escape` calls `check_decoded_string_growth(decoded.len())`
before every escape, including `\z`, which appends nothing. A literal
already holding exactly `MAX_STRING_BYTES` decoded bytes would fail on a
trailing `\z`. `MAX_SOURCE_BYTES == MAX_STRING_BYTES` makes this
unreachable, which `get_literal_string_contents` already notes for the whole
`StringSizeExceeded` branch.

---

## Structural observations (not defects)

- **Decoded string side table.** The lexer stores decoded string contents
  in `literal_string_contents: Vec<(usize, LiteralStringContents)>`, keyed by
  token start, and the parser fishes them out with a linear `position` +
  `remove`. That works because at most two tokens are buffered. A token
  payload (`Token { kind, span, payload: Option<LiteralStringContents> }` or
  an owned `TokenValue`) would remove the keyed side channel, the
  `MissingStringContents` internal fault and the `tok.len < 2` sanity check.
- **Numeral validation is split inconsistently.** Hex numerals are validated
  eagerly in the lexer (twice: after the mantissa and after the exponent,
  calling `parse_lua_numeral` both times). Decimal exponent errors (`1e`,
  `1e+`) are deferred to `parse_expr_base`. Error timing therefore differs
  by radix: `x = 0x1p` fails at lex time, `x = 1e` at parse time. A single
  "lex the maximal numeral span, validate once" rule, as Lua 5.4's
  `read_numeral` does, would make that uniform and would also give `1..2`
  Lua's "malformed number near '1..2'" instead of the current two-token
  `1.` `.2` parse error.
- **`error_here` has no anchored position.** Many errors are built with
  `error_here()`, whose position depends on whether a lookahead token
  happens to be buffered (see F6). Passing the relevant token (or span)
  explicitly, as `err_near`/`error_at` already do, would make every
  diagnostic position deterministic by construction rather than by parse
  state.
- **Codegen runs before the terminator is checked.** `finish_chunk` runs
  codegen for a nested function before its `end` is consumed, and for the
  main chunk before `<eof>` is checked. A codegen limit error
  (`TooManyRegisters`, `JumpTooFar`) therefore wins over a syntax error on
  the terminator. `JumpTooFar` is located at the terminator via
  `error_here`, not at the offending jump. This is harmless, but the error
  precedence comes from where finishing happens and was not designed.

## Checked and found sound

- Upvalue resolution by name across intermediate frames (enclosing frames
  cannot change while an inner frame is open). The capture log records
  captures in the owning frame, and `LocalDecl::captured` is final before
  codegen reads it.
- Block, loop, break and repeat-until close decisions, including a capture
  inside the `until` condition and captures in nested blocks.
- Constant folding: left-associative chain folding only while every operand
  so far is a literal; NaN, `-0` and a full pool are refused; signed-zero
  pool keys cannot collide, because no `-0` ever enters the pool.
- `parse_hex` rounding, saturation and subnormal paths. `parse_decimal`
  validates before handing off to Rust's correctly rounded parser, so
  `inf`/`nan` spellings never get through.
- CR/LF/LFCR line counting through whitespace, comments, escaped newlines
  and `\z`. Long-comment level matching. Escape decoding offsets.
- Syntax-depth charging: every recursive entry is charged, matching the
  `docs/limits.md` table. `resolve_in` recursion is bounded by
  `MAX_CHUNK_NESTING`.
- Table constructor batching, tail expansion, and template duplicate-key
  fallback.
