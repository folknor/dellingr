# Bug hunt 2026-07-24 - Corner A: Front end

Scope: lexing, parsing, codegen, expression descriptors, numeral parsing,
compiled chunk structure, cache-slot allocation.
Files: `src/compiler.rs`, `src/compiler/`, `src/numeral.rs`.
Method: code reading only. All repros are written down, not executed.

Severity legend: P1 = crash / silent miscompile, P2 = wrong observable
behavior vs reference Lua on supported constructs, P3 = accepts/rejects
programs differently from reference (compile-time only), P4 = minor.

---

## P1 findings

### A1. Unbounded parser recursion: hostile source aborts the host process

The parser recurses on nesting with no depth guard anywhere:

- expressions: `parse_expr -> parse_or -> ... -> parse_primary ->
  parse_prefix_exp -> parse_expr` (parens), `parse_unary -> parse_unary`,
  `parse_pow -> parse_unary -> parse_pow` (`src/compiler/parser/expr.rs`)
- statements: `parse_statements -> parse_do -> parse_statements`
  (`stmt.rs:416-423`), `parse_if_arm -> parse_else_or_elseif ->
  parse_if_arm` (`stmt.rs:503-547`)
- tables: `parse_table -> parse_table_entry -> parse_expr -> parse_table`
  (`table.rs`)

`nest_level: i32` tracks scope depth but is never bounded. Reference Lua
rejects deep nesting with "chunk has too many syntax levels"
(LUAI_MAXCCALLS, ~200). dellingr instead exhausts the native stack:
a Rust stack overflow is an abort, killing the whole game process.
This is the parser-side sibling of the L17 lexer comment-recursion fix -
the lexer got hardened, the parser did not.

Repro (do not run in-process without expecting an abort):

```lua
-- 200k of these, then "x", then 200k closing parens:
-- print((((((((((( ... x ... ))))))))))
```
or 200k of `do `, or 200k of `-` before a literal, or 200k nested `{`.

Fix sketch: a `syntax_depth: u32` on `Parser`, incremented in
`parse_expr`, `parse_statements`, `parse_table`, `parse_prefix_extension`
(recursive extension chains like `a.b.c.d...` also recurse), error
`SyntaxError::TooManySyntaxLevels` past ~200-500. Cheap, matches
reference behavior.

### A2. Upvalue index truncation past 255: silent miscompile

`src/compiler/parser/upvalue.rs` has no cap on the per-function upvalue
list, unlike `add_local` (parser.rs:85-95, capped at 255) and reference
Lua (MAXUPVAL 255, "too many upvalues" error):

- `add_upvalue` (upvalue.rs:101-105): `let idx = self.upvalues.len() as u8;`
- `find_upvalue` (upvalue.rs:7-12): `position(...).map(|i| i as u8)`
- `create_parent_upvalue` (upvalue.rs:69, 79, 90):
  `self.outer_upvalues[parent_idx].len() as u8`

A function can legally reference more than 255 distinct outer names
(e.g. 200 locals in grandparent A + 60 locals in parent B, inner C
referencing all 260): the 256th upvalue gets index 0 via `as u8`
truncation, and `Instr::get_upvalue(0)` silently reads the wrong
variable. No error, no panic - a miscompiled program.

Also affected: `UpvalueDesc::Upvalue(idx)` stored into
`Bytecode.upvalues` with the truncated index, so the closure capture
list itself is wrong, and the snapshot codec will faithfully persist the
wrong program.

Repro sketch (generated script):

```lua
local a1, a2 = 1, 2  -- ... through a200, in function A
-- function B nested in A declares b1..b60
-- function C nested in B: return a1+a2+...+a200+b1+...+b60
-- C's 256th captured name resolves to upvalue slot 0 -> wrong value
```

Fix: mirror `add_local` - error (new `SyntaxError::TooManyUpvalues`)
when a list would exceed 255, in `add_upvalue` and both push sites in
`create_parent_upvalue`.

### A3. `line_info` desyncs from `code`: wrong line numbers everywhere

`Parser::push` (parser.rs:386-389) appends to `code` and `line_info` in
lockstep, but every post-hoc rewrite of `code` ignores `line_info`:

- `expr.rs:272` and `expr.rs:327`: `self.chunk.code.remove(mark_idx)` -
  executed for EVERY plain fixed-arg call (the common case). Removes one
  instruction, leaves `line_info` untouched: `line_info` is now one
  longer than `code` and misaligned for every instruction at or after
  `mark_idx`.
- pop-then-push adjustments, each leaving one stale extra entry:
  `expr.rs:257-268`, `expr.rs:312-323` (vararg / tail-call arg
  adjustment), `stmt.rs:24-31` (return tail), `stmt.rs:371-388`
  (`adjust_multi_assign`).

Consumers index `line_info` by pc: `vm/frame.rs:70` (`current_line`,
stack traces), `vm.rs:488` (`host_print` line), `compiler.rs:263`
(`assign_cache_slots` error line), and `save_state.rs` serializes the
skewed vector. Skew accumulates one slot per call site, so in any
call-heavy script, runtime error traces report lines that are earlier
than the actual error - the more code, the further off.

Repro (inspection or behavior):

```lua
local function f() return 1 end
f() f() f() f() f() f() f() f() f() f()
local boom = nil + 1   -- reference reports this line; dellingr's trace
                       -- reports an earlier line (skew = ~10 slots)
```
Or simply assert `bc.code.len() == bc.line_info.len()` after parsing
`print(1)` - it fails today.

Fix options: (a) route every code mutation through helpers that mirror
the edit into `line_info` (pop_instr / remove_instr); (b) the A4/O2
restructure below, which removes `code.remove` entirely and makes the
invariant structural. Either way, add a debug_assert of equal lengths in
`finalize`.

---

## P2 findings

### A4. `for` control expressions see the loop variable (scoping bug)

Both for-loop parsers bring the hidden control slots AND the visible
loop variables into scope BEFORE parsing the control expressions:

- numeric: `stmt.rs:230-244` - `add_local("")` x3, `add_local(name)`,
  then `parse_expr()` for start/stop/step.
- generic: `stmt.rs:291-316` - hidden x3 + all visible names added,
  then `parse_explist()` for the iterator expressions.

In Lua, control expressions are evaluated in the enclosing scope; the
loop variable is only in scope in the body. Because `find_last_local`
picks the newest binding, any name collision resolves to the fresh
(nil or stale) loop-var slot:

```lua
local i = 5
for i = i, 7 do print(i) end
-- reference: prints 5 6 7
-- dellingr: start expr reads the new slot (nil) -> runtime error
--           "'for' initial value must be a number"

local t = {10, 20}
for _, t in ipairs(t) do print(t) end
-- reference: prints 10 20
-- dellingr: ipairs(t) reads the new nil slot -> bad-argument error
```

Worse, slot reuse can make it silently wrong rather than an error: after
an earlier scope used the same slot index, the stale value is read as
the start value (no error, wrong iteration bounds).

Fix: parse the control expressions first, then add the hidden + visible
locals (the emitted slot arithmetic already targets
`locals.len()`-relative slots, so record the base index before parsing
and add the locals after).

### A5. Bare CR inside string literals (sibling of the L17 CR fixes)

The L16/L17 hardening covered CR in line counting but not in strings:

- `lexer.rs lex_string` (353-368) rejects only `'\n'` inside a string.
  Reference Lua treats both CR and LF as "unfinished string". dellingr
  compiles `"a<CR>b"` and embeds a raw 0x0D byte.
- `parser.rs get_literal_string_contents:343` maps escape `\<LF>` to
  `'\n'`, but `\<CR>` falls into the `_ =>` arm: InvalidEscapeSequence
  error. Reference Lua maps `\<CR>`, `\<CR><LF>`, and `\<LF><CR>` all to
  a single `'\n'`.
- `\<LF><CR>` in dellingr produces the two bytes `\n\r` (the CR is
  copied literally); reference produces one `'\n'`.

Any script written or transferred with CR / CRLF / mixed line endings
inside string literals diverges. Fix: in `lex_string`, treat `'\r'` like
`'\n'` (unfinished string) for the unescaped case; in the escape
decoder, accept `\<CR>` / `\<CR><LF>` / `\<LF><CR>` as one `'\n'`.

### A6. `--[=[ ... ]=]` leveled long comments misparsed

`lexer.rs skip_comment` (210-236) recognizes only `--[[`. A leveled
opener `--[=[` (any `--[=...=[`) falls through to the single-line
branch, so only the first line is skipped and the comment BODY is then
lexed as live source. Reference Lua 5.2/5.4 skips the whole block.

```lua
--[=[
this line is a comment in reference Lua
]=]
print("ok")
```
Reference prints `ok`; dellingr attempts to parse line 2 as code
(usually a confusing syntax error; if the body happens to be valid Lua
it would execute). Given long strings already produce a dedicated
LongStringUnsupported error at `[=`/`[[`, the consistent fix is to
error on `--[=` as well (or support levels in the comment skipper -
comments are not on the "Won't implement" list, `--[[` already works).

Related, lower priority: an unfinished `--[[` at EOF is silently
accepted (skip_comment returns on None, lexer emits EOF); reference Lua
errors "unfinished long comment". Accepts-more divergence.

---

## P3 findings (compile-time acceptance divergences)

### A7. `break;` and code after `break` are rejected

`parser.rs:538-542`: after `break`, `parse_statements` exits the loop
without consuming an optional `;` (compare `parse_return`, which does:
`stmt.rs:40`) and without allowing further statements.

- `while true do break; end` -> "'end' expected near ';'" in dellingr;
  valid in Lua 5.1/5.2/5.4.
- `while true do break print(1) end` -> rejected; valid (dead code) in
  5.2/5.4.

The trailing-semicolon case is the one real scripts hit. Minimal fix:
`self.input.try_pop(TokenType::Semi)?;` after `add_break()`.

### A8. Numeral-touching-letter only rejected for hex-digit letters

`lexer.rs lex_exponent:465-468` and the hex tail check (403-405) reject
a trailing letter only if it `is_ascii_hexdigit()`. Reference Lua
rejects ANY alphanumeric touching a numeral. So `3and 4` errors in both
(a is a hex digit), but:

- `print(3or 4)` -> dellingr prints 3; reference: "malformed number near
  '3o'".
- `1e5or 2`, `0x5rad` (the latter codified in `test_lexer07`) - same
  class.

Accepts-more, silently. Fix: reject `is_ascii_alphanumeric()` (and `_`)
after a numeral instead of `is_ascii_hexdigit()`.

### A9. Vertical tab is not whitespace

`lexer.rs consume_whitespace` (264-276) uses `is_ascii_whitespace`,
which excludes VT (0x0B); C `isspace` includes it, so reference Lua
accepts VT between tokens and after `\z`. dellingr: InvalidCharacter.
Same for the parser-side `\z` skip (`parser.rs:339`). One-line predicate
fix in both places (space, \t, \n, \v, \f, \r - `numeral.rs
is_lua_whitespace` already has the correct set; reuse it).

### A10. LParenLineStart ambiguity check keyed to LF only

`consume_whitespace` sets `starts_line` only on `'\n'` (lexer.rs:270).
A file using bare-CR line endings never produces `LParenLineStart`, so
the (intentional, tested) "ambiguous function call" error silently does
not fire for CR files: `f<CR>(g)` parses as a call while `f<LF>(g)`
errors. After the L17 bare-CR line-counting fix, this is the one
remaining LF-only assumption in the lexer. Set `ret = true` for the
bare-CR branch too.

### A11. Capacity ceilings likely to bite data-heavy game scripts

All compile-time rejections (correct given the encoding), but far below
reference Lua and plausible in real init/data scripts:

- 255 array entries per table constructor (`table.rs:158-160`,
  TooManyTableFields). Reference Lua handles millions by flushing
  SETLIST every 50 entries. dellingr could do the same with periodic
  `set_list(k)` flushes and no encoding change - see O8.
- 255 distinct string literals per function - shared pool for string
  literals, global names, AND field names (`parser.rs:253-271`).
  A single chunk with 255+ distinct identifiers/strings fails
  (TooManyStrings). `{x1=1, ..., x300=300}` is enough.
- 255 distinct number literals per function (TooManyNumbers).
- 255 `t.f = v` SET_FIELD sites per function
  (`compiler.rs:259-270`, TooManyFieldAssignments) - the cache-slot
  byte (C) is the limiter, not the instruction itself. Instead of
  erroring, sites past 255 could emit cache-slot sentinel 255 = "not
  cached" and take the slow path; capacity stops being a hard limit.
  (Widening the encoding is execution-corner turf; the sentinel is
  pure codegen.)
- Cosmetic: >65535 GET_FIELD sites errors as `InternalError` rather
  than a SyntaxError (`compiler.rs:247-255`); inconsistent with the
  SET_FIELD path just below it.

### A12. `num_locals` over-counts in functions with params + sibling scopes

`add_local` (parser.rs:85-95) grows `num_locals` whenever
`locals.len() > num_locals`, but params are pushed without updating
`num_locals` (parser.rs:477-479). With P params, sibling scopes
re-trigger growth: `function f(a, b) do local x end do local y end end`
ends with num_locals = 2 though peak non-param locals is 1. Never
under-counts (safe), but each call pushes that many extra nils
(`vm/eval.rs:346`) and consumes stack headroom. Fix: track
`num_locals = max(num_locals, locals.len() - num_params)`.

---

## Verified clean (checked, no finding)

- Determinism: the whole front end is Vec-scan based (no HashMap, no
  entropy, no platform-dependent behavior). Decimal literals go through
  Rust's correctly-rounded `str::parse::<f64>` (locale-independent,
  bit-stable); hex literals through the in-crate `numeral.rs` parser.
  Identical source -> identical Bytecode, byte for byte.
- `numeral.rs`: round-to-nearest-even with sticky bits, subnormal and
  overflow paths, exponent saturation bounds - all checked against the
  IEEE semantics; consistent with the C30/L16 tests. No issue found.
  `-0.0`/NaN literal-pool collapse is unreachable: the parser never
  constant-folds, so literals are always non-negative and non-NaN.
- `Instr::call(ArgCount::Fixed(255))` in the re-emission paths
  (expr.rs, stmt.rs) looks wrong but round-trips correctly: 255 encodes
  back to Dynamic. Fragile-but-correct; a `from_u8` round-trip would
  read better.
- `code.remove(mark_idx)` index safety (jump offsets, break-jump
  indices, table-template instr indices, tail-call indices): all
  recorded indices point before the removal site or move with it;
  relative jumps have both endpoints on the same side. Only `line_info`
  is broken (A3).
- Repeat-until scoping (C29), close-upvalue emission at scope/break
  boundaries, multi-assign RHS/LHS ordering, `...` restriction to
  vararg functions, method-call desugaring stack discipline, escape
  decoding for `\ddd`/`\xXX`/`\z` bounds: checked, correct.
- `assign_cache_slots` bounds: global cache <= 256 (fits u16), field
  cache guarded, set-field guarded. Slot assignment is deterministic
  (instruction order).

---

## Optimization opportunities

Ordered by expected impact. O1 is a full coherent rewrite; the rest are
local-to-moderate changes.

### O1. Register-based codegen (full rewrite, biggest throughput lever)

The parser emits a pure stack machine: `x = a + b` is GET_LOCAL,
GET_LOCAL, ADD, SET_LOCAL - four dispatches plus stack traffic; a
register VM does it in one ADD(dst, a, b). Reference Lua's 5.0 move from
stack to register VM is the single largest reason for the persistent
1.6-3x gap on arithmetic/field benches (`numerics/arithmetic` 2.8x,
`fields/same_obj_read` 2.5x vs lua5.2). The 32-bit ABC encoding in
`instr.rs` already has the operand room; locals already live in fixed
frame slots, which are registers in all but name. The expression parser
would grow a lua-style expdesc with delayed emission (the existing
`ExpDesc`/`PlaceExp` split is halfway there), and `mark_call_base` /
`dup` / `swap` gymnastics disappear.

Interactions with the hard constraints: determinism is unaffected;
per-opcode cost accounting survives but cost_used values re-base
(fewer, fatter ops) - a product decision to version, same as any
replay-affecting change. This also subsumes several OPTIMIZATIONS.md
entries (field-update fusion, GET/SET cache-slot sharing become natural
in a register IR).

### O2. Token-stamped line numbers (kills quadratic parse behavior)

`update_line` (parser.rs:392-395) calls `line_and_col`, which is a
linear walk of the whole `linebreaks` vec (lexer.rs:489-505). It runs at
every statement start and every `expect()`. Parse time is therefore
O(statements x lines) - quadratic for large scripts; a 10k-line chunk
does tens of millions of window compares inside `parse_str`, which is a
measured hotpath. Fix: the lexer already knows the current line when it
produces a token - stamp `Token` with `line: u32` (fits existing
padding) and make `update_line` a field copy; keep `line_and_col`
(binary-searchable via `partition_point`, the vec is sorted) only for
error rendering. This also provides the correct infrastructure to fix
A3 for good.

### O3. `parse_chunk` clones the whole Bytecode twice per function

parser.rs:462 (`outer_chunks.push(self.chunk.clone())`) and
parser.rs:487 (`let tmp_chunk = self.chunk.clone()`) deep-copy code,
literal pools, line_info, and nested Arcs of the partially built outer
chunk and the finished inner chunk, then immediately overwrite the
originals. `std::mem::take` / `std::mem::replace` make both O(1). Cost
today is O(enclosing-chunk size) per nested function definition -
top-level files with many functions pay repeatedly.

### O4. Stop using `Vec::remove` in call emission

Every plain call does `code.remove(mark_idx)` (expr.rs:272, 327): O(n)
tail shift per call, so call-heavy chunks are quadratic-ish in
emission, and it is the root cause of A3. Options: (a) introduce OP_NOP
and patch the mark in place (O(1), keeps line_info aligned; one extra
cheap dispatch on calls that needed the mark removed - or strip nops in
`finalize` with a jump-offset remap done once, correctly, in one
place); (b) restructure so the mark is only emitted once the arg list
is known to need it (requires buffered args or a pre-scan). (a) is the
pragmatic fix.

### O5. Zero-allocation identifiers and names

- `lex_word` (lexer.rs:472-485) builds a fresh `String` for every
  identifier/keyword token, used only for `keyword_match`, then thrown
  away (the parser re-slices the source anyway). Match on the
  `&source[tok_start..pos]` slice instead: one alloc per token removed
  on the front-end hot path.
- `locals: Vec<(String, i32)>`, `upvalues: Vec<(String, ...)>`, and
  namelists allocate a String per declaration (parser.rs:89,
  upvalue.rs:103, stmt.rs:294). The parser already carries `'a`; these
  can be `&'a str` borrows of the source.

### O6. Constant folding (with reference Lua's guards)

There is no folding at all: `local ms = 60 * 1000` multiplies at
runtime on every execution, `-5` is PUSH_NUM + NEGATE per hit, and both
charge cost. Fold literal arithmetic/unary at parse time with lcode.c's
guards (refuse to fold results that are NaN or 0.0, exactly to avoid
the -0.0 literal-pool collapse noted above; `find_or_add_number` dedups
with `==` which conflates 0.0/-0.0). Note: folding changes cost_used
for identical source - same replay-versioning consideration as O1.

### O7. Compare-and-branch fusion

`while i < n do` emits LESS (push bool) + BRANCH_FALSE (pop bool) -
two dispatches plus stack traffic per iteration of every hot loop
condition. A peephole in the parser (or in `finalize`) fusing
comparison + branch into one opcode halves the dispatch on loop
headers. Needs execution-corner cooperation for the new opcodes.

### O8. Flush SET_LIST periodically to lift the 255-entry constructor cap

Emit `set_list(k)` every k <= 255 pending array values and reset the
counter (reference Lua flushes every 50). Removes the A11 constructor
limit with no encoding change and bounds constructor stack growth.
Interleaved named fields keep working if the `init_field`/`init_index`
offset tracks pending-only entries (it already does).

### O9. Skip CLOSE_UPVALUES in closure-free functions

`level_down` and every loop parser emit CLOSE_UPVALUES unconditionally
at scope exits (parser.rs:369-383; stmt.rs:259, 340, 441-443, 479-481) -
including once per iteration inside numeric/generic for bodies. If a
function body creates no closures (`chunk.nested` empty and no
OP_CLOSURE emitted), no upvalue over its locals can ever be open, and
every CLOSE_UPVALUES in it is a guaranteed no-op paying a dispatch.
A `finalize` pass can prove this per-Bytecode and drop/nop them. Most
game-script hot loops are closure-free; this removes a per-iteration
dispatch from all of them.

---

## Notes for the orchestrator

- A1, A2, A3, A4 deserve regression tests before/with any fix (A3 is
  testable as a pure invariant: `code.len() == line_info.len()`
  recursively after `finalize`).
- A5-A10 are diff-testable against lua5.2/lua5.4 with small scripts;
  the CR/VT cases need byte-level fixtures (careful with editors
  normalizing line endings).
- Nothing in this corner touches determinism or the cost-charge-before-
  side-effect contract; no findings there.
