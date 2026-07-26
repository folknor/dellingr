# WORK.md

Current work item. Optimization loop 8: the parse-time cluster - three
front-end costs with one measurement surface (`large_source` parse_us).

---

## Targets (one loop, three independent fixes)

### (1) Token-stamped line numbers - kills the quadratic (A-O2)

`update_line` (`parser.rs`, the small fn near the helpers) calls the
lexer's `line_and_col`, a LINEAR walk of the whole `linebreaks` vec, at
every statement start and every `expect()`. Parse time is therefore
O(statements x lines); a 10k-line chunk does tens of millions of window
compares inside `parse_str`, a `#[hotpath::measure]`d function. Measured:
`parse/large_source` (5000 generated lines) at 8.0x lua5.5 - the worst
ratio outside the since-fixed literal probe (2026-07-25 README, b4ae38e).

Fix: the lexer knows the current line when it produces a token - stamp
`Token` with `line: u32` (check `Token`'s layout for padding; it carries
`typ`, `start`, `len` today - verify) and make `update_line` a field copy.
Keep `line_and_col` for error rendering, and make THAT binary-searchable
via `partition_point` (the linebreaks vec is sorted) so error paths win
too. Audit every `line_and_col`/`line_and_column` caller.

### (2) `mem::take` instead of two full Bytecode clones per nested function (A-O3)

`parse_chunk` does `outer_chunks.push(self.chunk.clone())` and later
`let tmp_chunk = self.chunk.clone()` - deep copies of code, literal
pools, line_info and nested Arcs of the partially built outer chunk and
the finished inner chunk, immediately overwritten. `std::mem::take` /
`std::mem::replace` make both O(1). Cost today is O(enclosing-chunk size)
per nested function definition. (Line numbers shifted since the audit -
locate by the two `self.chunk.clone()` hits in `parser.rs`.)

### (3) Zero-allocation identifiers in the lexer (A-O5, lexer half only)

`lex_word` builds a fresh `String` per identifier/keyword token, used
only for keyword matching, then discarded (the parser re-slices the
source). Match on the `&source[token_start..pos]` slice instead - one
allocation per token removed. The parser-side half of A-O5 (`locals:
Vec<(String, i32)>` etc. becoming `&'a str` borrows) is OUT OF SCOPE for
this loop: it touches lifetime plumbing across the whole parser for a
smaller win - keep the entry in OPTIMIZATIONS.md, narrowed.

### Constraints (inline; sessions read nothing else)

- Read/write code only; no cargo/brokkr/test/bench commands.
- BYTECODE OUTPUT MUST BE BYTE-IDENTICAL: identical code, literals,
  line_info, error positions (line AND column) for every input. The
  parser bytecode-shape tests, the golden fixtures, the diff gate, and
  every error-message test are the oracle - none may change. This is a
  pure-speed loop.
- `cost_used` identical (parsing charges nothing - keep it so).
- Determinism; clippy strict; `unwrap_used` denied outside tests;
  `Result::ok()` banned; no HashMap/HashSet.
- Token size changes affect lexer-internal memory only (tokens are
  transient); note any size growth.
- Keep `#[hotpath::measure]` annotations on the parse path
  (`parse_str_named`, `next_token`, `update_line` if annotated) so
  before/after distributions compare.

### Open questions for the reviewer

1. Does the lexer actually know the line at token-production time, or
   does it only track byte positions and compute lines lazily? Read
   `lexer.rs` and state the true cheapest way to stamp tokens (a running
   line counter incremented on newline consumption is the classic shape;
   verify how `linebreaks` is built and whether comments/long strings
   complicate the counter).
2. `update_line` feeds `current_line` which feeds instruction emission
   (`line_info`). Confirm stamping changes NO line_info values - the
   stamped line must equal what `line_and_col(pos).0` returned for the
   same token position, including tricky cases: multi-line strings,
   comments before statements, CRLF handling (the lexer was aligned with
   reference Lua on newlines in 1db7f75 - do not disturb that).
3. Is `line_and_column` (public-ish error path) the same walk? Both it
   and `line_and_col` should become `partition_point` searches - confirm
   the vec's sortedness invariant and the exact tie-breaking (a position
   ON a newline byte).
4. For (2): after `mem::take(&mut self.chunk)`, the parser continues
   filling a fresh default chunk - verify every field the taken chunk
   carried is either restored or deliberately reset, especially
   `source` (name), `num_params`, `is_vararg`, and the upvalue
   bookkeeping around nested-function entry/exit.
5. For (3): confirm keyword matching is the String's only use, and
   whether `describe_token` / error paths re-slice the source
   independently (they appear to - `input.substring(..)`).

### Deliverable

Implementation plan answering the questions with file-by-file shape and
a test list (line-number oracle across multi-line strings/comments/CRLF;
error-position pins; a nested-function-heavy fixture proving (2) changes
nothing observable; existing corpus as the byte-identity oracle), plus
bench prediction: `large_source` parse_us drops materially (the
quadratic term dies); nothing else moves; the second data point for the
suspected quadratic (TODO.md's larger generated size) can finally be
taken cheaply afterward.

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

Corrections to the problem statement, verified: `Token.line: u32` does NOT
fit padding - Token grows ~16 -> 24 bytes on 64-bit (transient lexer
memory; document, accept). `TokenStream::line_and_column` is a wrapper;
the one linear walk is `Lexer::line_and_col`. TWO additional parse-path
callers exist beyond `update_line`: the call-opening line lookups in
`expr.rs` (ordinary + method calls, packed into `CallSite` and emitted via
`push_at_line`) - both must switch to the stamped token line or the hot
path keeps positional searches.

### Line stamping (the equivalence is proven, implement exactly this shape)

`Lexer` has no line counter, but `linebreaks.len()` IS the current
one-based line: `next_char` appends a line start per logical newline (CR,
LF, CRLF, LFCR handled; comments, `\z` gaps and escaped physical newlines
all consume through `next_char`; long strings unsupported). The correct
stamp: after consuming leading whitespace and setting `tok_start`, capture
`tok_line = linebreaks.len() as u32` BEFORE consuming the token body -
capturing after would be wrong for `\z`/escaped-newline strings. Stamp
ordinary AND EOF tokens. Storing on `Token` is required because `peek2`
lookahead can advance the lexer lines ahead of consumption.

Equivalence: for a token at byte `s`, every line start `<= s` is already
recorded and none later is, so `linebreaks.len()` at stamp time equals the
old `line_and_col(s).0` - newlines inside the token body have offsets
`> s` and never affected the old lookup either. `line_info` is therefore
byte-identical.

### Error-path search

Replace `line_and_col`'s `windows(2)` walk with the exact-equivalent
binary search: `partition_point(|&start| start <= pos) - 1`, returning
`(idx + 1, pos - linebreaks[idx] + 1)`. The `<=` predicate is the
tie-break: a pos equal to a recorded start is the new line at column 1; a
pos on the newline byte(s) belongs to the preceding line. Columns stay
byte-based. Keep positional lookup ONLY in the two error paths
(`Lexer::error_at`, `Parser::error_at`).

### `parse_chunk` ownership (field table verified; follow this sequence)

1. Validate + compute `num_params` first (preserve failure behavior).
2. Clone only `self.chunk.source` (parent and child deliberately share
   the source name string - the ONE remaining clone, of one Option
   String).
3. `outer_chunks.push(mem::take(&mut self.chunk))`.
4. Set the fresh child's `source`, `is_vararg`, `num_params`.
5. Compile as today; fill `self.chunk.upvalues` at completion.
6. `mem::replace` restores the popped outer chunk while taking the
   finished child.

`locals`/`upvalues` bookkeeping (outer_locals/outer_upvalues, recursive
descriptor additions to parent entries) is separate parser state and
untouched. Error-inside-child behavior unchanged (parser discarded
either way).

### `lex_word`

Consume the same continuation characters, then
`keyword_match(&self.source[tok_start..self.pos])` - no String. Token
type/start/len/line/lexer-pos identical; the parser re-slices the source
independently (`get_text`, `describe_token`).

### Consumers

- `update_line` (keep its annotation) becomes a `Token.line` field copy;
  callers (`expect`, statement start) pass the token.
- The two `expr.rs` call-line sites use `peek()?.line`.
- Update exhaustive Token destructurings with `..`.

### Docs

OPTIMIZATIONS.md: remove the shipped A-O2/A-O3 entries; narrow A-O5 to
the parser-side owned-names half (and fix its stale "fits padding"
wording).

### Tests (characterization pins written with hand-derived expectations)

- Lexer token-line oracle: LF, CR, CRLF, LFCR, repeated newlines, EOF
  after newline, short + leveled long comments, `\z`, escaped physical
  newlines, `peek2` lookahead crossing lines.
- `line_and_col` boundary pins: both bytes of CRLF/LFCR and the first
  byte after; pos equal to a line start.
- Recursive `line_info` fixtures: multiline strings, comments, ordinary
  + method calls, all newline encodings.
- Error line AND column pins: lexer errors, unexpected token after
  comments, escape errors, the ambiguous line-start-paren error, errors
  on/after newline sequences.
- BYTE-IDENTITY ORACLES (already in-tree; any change is a regression,
  never an expected update): `save_golden_current.bin` byte stability
  (contains nested closures - it IS the pre-change fixture), both legacy
  fixtures, all parser bytecode-shape tests, the compiler corpus, the
  diff gate, static-cost pins, `cost_used` pins.
- Confirm existing hotpath annotations remain.

### Bench acceptance (orchestrator, post-rsync)

`large_source` parse_us falls materially (quadratic term dies + chunk
clones + identifier allocs gone); everything else unchanged beyond
noise; then collect TODO.md's larger generated size as the second curve
point.
