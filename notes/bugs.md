# Bug hunt 2026-07-24 - consolidated findings

Consolidated and deduplicated from five independent read-only corner audits:
A (front end), B (execution core), C (data plane), D (state/persistence/host),
E (stdlib/patterns). Nothing here has been verified or executed - all evidence
is from code reading and every repro is written down but unrun. Severity is
the auditors' own ranking; where two corners disagreed the higher ranking won
and both are noted. Original corner finding IDs are kept in parentheses.
Findings reported independently by two corners are marked as such - treat
that as a confidence signal, not a verification.

Structural fixes for several of these live in [optimizations.md](optimizations.md);
cross-references below use its item numbers.

Numbering is stable: fixed items are deleted outright and their numbers are
never reused, so gaps in the sequence are expected and cross-references stay
valid. Fix history lives in git.

---

## High severity

### 12. Pattern matcher: matchdepth consumed by tail calls and never reset between attempts (E-E1)

- **Locations:** `src/patterns/luapat.rs:351` (decrement on entry); tail-style
  `return self.patt_match(...)` paths that skip the restore: line 391 (`%b`
  continuation), 413 (`%f` continuation), 421 (backref continuation), and in
  `patt_default_match` lines 440 (`*`/`?`/`-` accept-empty), 455 (`?`
  else-branch), 473 (item with no suffix); `str_match` (line 661) never
  resets `matchdepth` (or `level`) between anchor positions.
- **Cause:** reference C uses `goto init` for these transitions, so tail
  continuation is depth-free there; 5.2 even asserts
  `matchdepth == MAXCCALLS` before every anchor attempt. Two failure modes:
  1. Pattern length: a chain of N non-suffixed items costs N depth; any
     pattern with more than ~198 sequential items errors "pattern too
     complex" (reference matches arbitrary-length patterns). The test at
     `src/patterns/mod.rs:211` (`runtime_match_errors_are_not_swallowed`,
     201 literal `a`s) enshrines the divergence as expected.
  2. Leak across the scan loop: each failed anchor attempt that partially
     matched leaks depth equal to its tail-chain length; a subject with ~200
     partial matches kills the call.
- **Repro (mode 2):**

```lua
local s = ""
for i = 1, 250 do s = s .. "a" end
print(s:find("a%d"))      -- reference: nil; dellingr: error "pattern too complex"
-- realistic shape: subject with many "id" prefixes, pattern "id=(%d+)"
```

- **Fix sketch:** rewrite the tail transitions as a loop
  (`s = ...; p = ...; continue`), mirroring C's `goto init`, and reset
  `matchdepth = MAXCCALLS; level = 0` at the top of each `str_match` attempt.
  The luapat rewrite (optimizations.md #4) fixes this structurally.

### 15. `table.move`: unbounded uncharged work (budget bypass) and integer overflow (C-D1 + E-E12, two independent reports)

- **Location:** `src/lua_std/table.rs:255-307`.
- **Cause:** charges a flat 1, then loops `count = e - f + 1` times doing
  `get_table` + `set_table_raw` (both free) regardless of table contents (nil
  reads/writes still iterate); the range comes straight from script arguments
  and is not bounded by table size. No allocation is needed (reads of an
  empty table, writes of nil are removals), so memory caps do not save the
  tick budget - this defeats the cost budget, which is the product feature.
  Additionally `e - f + 1` with saturated extremes
  (`table.move(t, -1e300, 1e300, 1)` gives `f = isize::MIN`,
  `e = isize::MAX`) overflows `isize`: panic in debug builds, silent wrap in
  release. Reference guards with "too many elements to move"
  (`f > 0 || e < LUA_MAXINTEGER + f`).
- **Repro:**

```lua
table.move({}, 1, 2^30, 1)   -- ~10^9 table ops, cost charged: 1
table.move({}, 1, 1e15, 2)   -- host hang for cost 1
```

- **Fix:** charge `count.max(1)` up front, exactly like `table_sort` does (its
  L18 comment is the template), add the reference overflow argcheck, and
  consider clamping `e` to a sane bound.

### 16. Cost-model gap: pattern matching and string byte-work are entirely uncharged (E-E13; table.concat also C-D2)

- **Locations:** no `consume_cost` anywhere in `src/lua_std/string.rs`,
  `src/lua_std/string_format.rs`, or `src/patterns/`;
  `src/lua_std/table.rs:194-250` (`table.concat` charges 1 for O(j - i) work
  and arbitrarily large output).
- **Cause / consequences:**
  - A script can build a ~131k-char string with ~17 concat ops (repeated
    doubling), then each `gsub`/`find`/`match`/`upper`/`format("%s")` call
    does O(n) or worse work for ~0 charged cost, repeatable in a loop.
  - Backtracking patterns are superpolynomial in time while the depth cap
    only bounds recursion: `"a*a*a*a*b"` against a long non-matching subject
    of `a`s does O(n^k) `singlematch` work inside `max_expand` for a single
    costed-at-0 call.
  - `table.concat(t)` over a large array does `len` lookups and builds an
    arbitrarily large byte vector for cost 1.
  (`table.insert`/`remove` charging 1 for O(n) is already acknowledged in
  OPTIMIZATIONS.md; the string/pattern side is not tracked anywhere.)
- **Fix sketch:** thread a charge hook into the matcher (charge per
  `patt_match` invocation or per K `singlematch` steps against
  `State.cost_remaining`), and charge length-proportional cost in
  `gsub`/`sub`/`upper`/`lower`/`reverse`/`format`/`concat` before doing the
  work (cost contract: charge BEFORE the side effect).

---

## Medium severity

### 17. `for` control expressions see the loop variable (scoping bug) (A-A4)

- **Locations:** numeric: `stmt.rs:230-244` (`add_local("")` x3,
  `add_local(name)`, then `parse_expr()` for start/stop/step); generic:
  `stmt.rs:291-316` (hidden x3 + all visible names added, then
  `parse_explist()`).
- **Cause:** in Lua, control expressions are evaluated in the enclosing scope;
  the loop variable is only in scope in the body. `find_last_local` picks the
  newest binding, so any name collision resolves to the fresh (nil or stale)
  loop-var slot. Worse, slot reuse can make it silently wrong rather than an
  error: after an earlier scope used the same slot index, the stale value is
  read as the start value (wrong iteration bounds, no error).
- **Repro:**

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

- **Fix:** parse the control expressions first, then add the hidden + visible
  locals (the emitted slot arithmetic already targets `locals.len()`-relative
  slots, so record the base index before parsing and add the locals after).

### 18. Bare CR inside string literals (A-A5, sibling of the L17 CR fixes)

- **Locations:** `lexer.rs` `lex_string` (353-368) rejects only `'\n'` inside
  a string; `parser.rs` `get_literal_string_contents:343` maps escape `\<LF>`
  to `'\n'` but `\<CR>` falls into the `_ =>` arm (InvalidEscapeSequence).
- **Cause / divergences:** reference Lua treats both CR and LF as "unfinished
  string"; dellingr compiles `"a<CR>b"` and embeds a raw 0x0D byte. Reference
  maps `\<CR>`, `\<CR><LF>`, and `\<LF><CR>` all to a single `'\n'`; dellingr
  errors on `\<CR>` and produces the two bytes `\n\r` for `\<LF><CR>`. Any
  script written or transferred with CR / CRLF / mixed line endings inside
  string literals diverges.
- **Fix:** in `lex_string`, treat `'\r'` like `'\n'` (unfinished string) for
  the unescaped case; in the escape decoder, accept `\<CR>` / `\<CR><LF>` /
  `\<LF><CR>` as one `'\n'`.

### 19. `--[=[ ... ]=]` leveled long comments misparsed (A-A6)

- **Location:** `lexer.rs skip_comment` (210-236) recognizes only `--[[`.
- **Cause:** a leveled opener `--[=[` (any `--[=...=[`) falls through to the
  single-line branch, so only the first line is skipped and the comment BODY
  is lexed as live source. Reference 5.2/5.4 skips the whole block.

```lua
--[=[
this line is a comment in reference Lua
]=]
print("ok")
```

  Reference prints `ok`; dellingr attempts to parse line 2 as code (usually a
  confusing syntax error; if the body happens to be valid Lua it would
  execute). Given long strings already produce a dedicated
  LongStringUnsupported error at `[=`/`[[`, the consistent fix is to error on
  `--[=` as well (or support levels in the comment skipper - comments are not
  on the "Won't implement" list, `--[[` already works).
- **Related, lower priority:** an unfinished `--[[` at EOF is silently
  accepted (skip_comment returns on None, lexer emits EOF); reference errors
  "unfinished long comment". Accepts-more divergence.

### 20. NaN comparisons: `<=` and `>=` evaluate true (B-B3 + C-B3, two independent reports)

- **Locations:** `src/vm/eval_store.rs:484-509` (`eval_compare` maps
  `partial_cmp -> None` to `Ordering::Equal`), `src/vm/frame.rs:277-278`
  (`<=` implemented as negated `>`, `>=` as negated `<`).
- **Cause:** for NaN operands the comparison result is `Equal`, which is not
  `Greater`/`Less`, so the negated forms return true. Reference: every
  ordered comparison involving NaN is false. `<`/`>` are unaffected (Equal
  matches neither target).
- **Repro:**

```lua
print((0/0) <= 1)   -- dellingr: true, Lua: false
print((0/0) >= 1)   -- dellingr: true, Lua: false
print(1 <= 0/0)     -- dellingr: true, Lua: false
print((0/0) < 1)    -- both: false (only <=/>= are wrong)
```

- **Fix:** on `partial_cmp() == None`, push false unconditionally (before the
  negate step), or compute `<=` as a first-class comparison instead of
  `!(>)`.

### 21. Floored-modulo formula produces NaN for infinite divisor (B-B5)

- **Location:** `src/vm/frame.rs:328-333` (`OP_MOD` computes
  `a - (a/b).floor() * b`).
- **Cause:** with `b = inf`: `a/b = 0`, `0 * inf = NaN`. Reference (5.2 and
  5.4 `luai_nummod`) is fmod-based: `1 % math.huge == 1.0`,
  `-1 % math.huge == inf`. The formula is also less exact than `fmod` for
  large finite operands (rounding in `a/b` can flip the floor).
- **Repro:**

```lua
print(1 % (1/0))    -- dellingr: nan,  Lua: 1.0
print(-1 % (1/0))   -- dellingr: nan,  Lua: inf
```

- **Fix:** implement as reference does:
  `m = a.rem(b) (fmod); if m != 0 && (m < 0) != (b < 0) { m += b }`.

### 22. `instr_tfor_call_rust_fn` truncates the result count with `as u8` (B-B6, host-API only)

- **Location:** `src/vm/eval_control.rs:266`
  (`let num_ret_actual = self.get_top() as u8;`).
- **Cause:** if a host-provided iterator RustFunc leaves more than 255 values
  on its frame, the cast wraps (256 -> 0), the `Greater` arm pushes spurious
  nils, and the subsequent `results_start`/`truncate` bookkeeping leaves
  hundreds of stray values on the stack inside the loop frame - silent stack
  corruption instead of the "too many results" error. The sibling path in
  `State::call` (`src/vm/eval.rs:102-117`) does this correctly in `usize`.
  Not reachable from stdlib iterators; requires a host RustFunc used as a
  generic-for iterator.
- **Fix:** mirror the `usize` comparison from `State::call`.

### 24. `OP_CONCAT` is free: exponential memory growth at near-zero cost (B-B8 + C-D3, two independent reports; design-review flag)

- **Locations:** `src/vm/frame.rs:304-307` (charges nothing),
  `src/vm/eval.rs:228-263` (`concat_helper` does O(total bytes) work and
  allocation).
- **Cause:** `s = s .. s` in a free `while true` loop doubles memory every
  iteration with zero cost charged - ~34 iterations is a 16 GB attempt, ~40
  exceeds any realistic host memory. The in-code comment declares string ops
  free, and README's Budget section concedes structural freebies, but that
  argument is about time on control flow; exponential allocation is a
  different failure domain (OOM kill of the host rather than idling a tick
  budget).
- **Repro:**

```lua
local s = "x"
for i = 1, 34 do s = s .. s end   -- 16 GB attempt, total charged cost ~34
```

- **Recommendation:** charge concat proportional to output length (like
  SET_LIST charges per element - dynamic charging is already in the design
  vocabulary), or a max-string-length cap. Decision belongs to the cost-model
  owner.

### 26. `table.insert(t, pos, v)` reverses `pairs` order and is O(N^2) (C-B4)

- **Location:** `src/vm/table.rs:410-433` (`Table::array_insert`).
- **Cause:** shifts by `shift_remove(key i)` + `insert(key i+1)` from high to
  low; each re-insert appends at the END of the IndexMap, so shifted keys end
  up in reverse order, followed by the newly inserted key. (`array_remove`
  happens to preserve order because its forward loop re-appends in ascending
  key order.) Also O(N^2): each `shift_remove` is O(tail) in IndexMap, and
  there are O(N) of them - a single `table.insert(t, 1, v)` on a 50k-element
  table is ~10^9 memmoves for cost 1 (extends the OPTIMIZATIONS.md
  "O(N) shifts charged as 1" entry, which understates it).
- **Repro:**

```lua
local t = { 10, 20, 30 }
table.insert(t, 1, 99)
for k, v in pairs(t) do print(k, v) end
-- reference 5.2/5.4: 1 99 / 2 10 / 3 20 / 4 30
-- dellingr:          4 30 / 3 20 / 2 10 / 1 99
```

- **Related cache nit:** for a non-sequence (`t = {1,2,3, [5]=5}`, border 3),
  `array_insert` unconditionally sets `cached_array_len = len + 1 = 4`, but
  after the shift `t[5]` is non-nil so 4 is not a border; `#t` returns a
  non-border value until invalidated. Safest: only trust `Some(len + 1)` when
  `get(len + 2)` is nil, else `set(None)`.
- **Fix:** value-rotation rewrite (optimizations.md #11) fixes order and cost
  together; charge the shifted count (as `table_sort` does).

### 30. User mutations inside environment tables are silently dropped by save/load (D-D5, round-trip fidelity)

- **Location:** `src/vm/save_state.rs:303-310`.
- **Cause:** when the walker meets an `ObjectPtr` present in `env_tokens` it
  emits an `EnvObj` token and never walks the table's entries. So
  `math.myconst = 42`, `string.trim = function(s) ... end`,
  `table.foo = {...}` (ordinary Lua idiom - extending library tables) survive
  in the live State but vanish on save/load: the tokens resolve to freshly
  rebuilt pristine libraries. Values reachable ONLY through an env table
  (that `table.foo` subtable) are dropped entirely, with no `SaveError` and
  no diagnostic. README's snapshot section ("globals, reachable
  tables/closures/upvalues/strings ... are persisted") does not carve this
  out; the module doc only covers the reverse direction (new build adding
  `math.foo`).
- **Fix options:** (a) at save time, diff each env table against a
  capture-time pristine snapshot (entry list captured alongside `env_tokens`)
  and persist the delta, replayed on load after `open_libs`; (b) cheaper:
  detect a modified env table (its `version()` differs from capture time, or
  entry-count/name diff) and fail fast or surface it in `SaveDiagnostics`,
  plus document the limitation. Doing nothing silently loses user state.

### 31. `%p` identity state is not persisted; identities collide across save/load (D-D6)

- **Locations:** `src/vm.rs:164-168` (`format_pointer_ids`,
  `next_format_pointer_id`) absent from `SavePayload`
  (`save_state.rs:198-210`).
- **Cause:** after a load the counter restarts at 1 while strings produced by
  `string.format("%p", x)` before the save can persist in saved globals. A
  new object formatted after load can render byte-identical to a different
  pre-save object's `%p` string, breaking the uniqueness the deterministic
  `%p` ids exist to provide; an uninterrupted run diverges from a
  save/load-interrupted run (replay-affecting if scripts branch on `%p`
  output).
- **Fix:** persist `next_format_pointer_id` and the id entries whose `Val`s
  are reachable in the payload (dead entries can be dropped - consistent with
  the TODO.md pruning sketch), or document that `%p` identities are
  per-process and never comparable across a load.

### 32. `table.sort` with a comparator does O(N^2) comparator calls charged N (C-D5)

- **Location:** `src/vm/table_ops.rs:304-326`.
- **Cause:** bubble sort always runs the full N(N-1)/2 rounds (no early-exit
  swap flag; each round re-calls even when sorted). The comparator call is
  free by design and a trivial `return a < b` body charges ~0, so
  `table.sort` on 10k elements is ~5*10^7 full call-machinery round trips for
  cost 10^4. Fix via the sort rewrite (optimizations.md #12).

### 33. `%f` frontier loses left context because stdlib re-slices the subject (E-E7)

- **Locations:** matcher treats slice-start as string-start
  (`previous = '\0'`, `luapat.rs:403-407`); re-slice sites: `gsub` loop
  (`&s[pos..]`, `string.rs:575`), `gmatch_iter` (`&s[pos..]`,
  `string.rs:712`), `find`/`match` with init (`&s[init..]`,
  `string.rs:320/416`).
- **Cause:** reference keeps `src_init` at the true beginning of the subject
  even when matching from `init` or resuming gmatch/gsub mid-string.
- **Repro:**

```lua
print(("abcd"):gsub("%f[%w]%w", "X"))   -- reference: "Xbcd" 1; dellingr: "XXXX" 4
print(("ab"):find("%f[%a]%a", 2))       -- reference: nil;    dellingr: 2 2
```

- **Fix (structural, optimizations.md #4):** give `str_match` an `init`
  offset and keep the full subject, like reference `prepstate`. Also deletes
  all the `base +` capture-offset arithmetic in string.rs.

### 36. Explicit `nil` is rejected for optional stdlib arguments (E-E10)

- **Cause:** reference `luaL_opt*` treats nil as "absent"; the prevailing
  dellingr pattern `if num_args >= k { check_type(k, ...) }` errors on
  explicit nil instead. Affected: `string.find` init (`string.rs:268` -
  breaks the very common plain-find idiom `s:find(pat, nil, true)`),
  `string.sub` j, `string.match` init, `string.gsub` n, `table.concat`
  sep/i/j, `table.sort` comp, `table.unpack`/`unpack` i/j, `table.insert`
  pos (3-arg form with nil pos errors differently than reference); `select`
  is fine. Notably `tonumber` base and `table.remove` pos already do it
  correctly (`state.typ(k) != LuaType::Nil` guard), so the codebase has the
  right pattern applied inconsistently.
- **Repro:**

```lua
print(("a.b"):find(".", nil, true))  -- reference: 1 1; dellingr: bad-argument error
```

- **Caveat from the auditor:** assumes `check_type(k, T)` errors on nil (very
  high confidence from `ArgError` plumbing, but `vm_aux.rs` was not read).

### 37. `table.concat`'s `len == 0` short-circuit ignores an explicit range; negative range values saturate (E-E8)

- **Location:** `src/lua_std/table.rs:224`
  (`if i > j || len == 0 { return "" }`).
- **Cause:** reference only defaults `j` from `#t`; an explicit `i..j` range
  is honored regardless of the border. Also: `i`/`j` (and `table.unpack`'s,
  see #38) go through `as usize`, so negative values saturate to 0 instead of
  addressing negative indices like reference does.
- **Repro:**

```lua
local t = {}; t[2] = "x"
print(table.concat(t, "", 2, 2))  -- reference: "x"; dellingr: ""
```

### 38. `unpack` / `table.unpack` truncate negative start indices to 0 (E-E9)

- **Locations:** `src/lua_std/basic.rs:241`, `src/lua_std/table.rs:121`
  (`state.to_number(2)? as usize` saturates negatives to 0).
- **Cause:** `unpack(t, -2, 2)` returns 3 values `t[0],t[1],t[2]` instead of
  reference's 5 values `t[-2]..t[2]` (wrong count AND wrong values). The
  255-result cap itself is a deliberate protocol limit (verified: RustFunc
  return counts are plain numbers in `vm/eval.rs:102-119`, no sentinel
  collision at exactly 255).

### 39. `math.modf(+-inf)` returns NaN fractional part (E-E11)

- **Location:** `src/lua_std/math.rs:323` (`x.fract()` =
  `x - x.trunc()` = `inf - inf` = NaN for infinite inputs).
- **Cause:** reference (5.2's C `modf`, 5.4's explicit `n == ip` test)
  returns 0.0.
- **Repro:**

```lua
print(math.modf(math.huge))  -- reference: inf 0.0 (5.2: "inf 0"); dellingr: inf nan
```

---

## Low severity

### 40. `break;` and code after `break` are rejected (A-A7)

`parser.rs:538-542`: after `break`, `parse_statements` exits without
consuming an optional `;` (compare `parse_return`, `stmt.rs:40`) and without
allowing further statements. `while true do break; end` -> "'end' expected
near ';'" (valid in 5.1/5.2/5.4); `while true do break print(1) end` ->
rejected (valid dead code in 5.2/5.4). The trailing-semicolon case is the one
real scripts hit. Minimal fix: `self.input.try_pop(TokenType::Semi)?;` after
`add_break()`.

### 41. Numeral-touching-letter only rejected for hex-digit letters (A-A8)

`lexer.rs lex_exponent:465-468` and the hex tail check (403-405) reject a
trailing letter only if `is_ascii_hexdigit()`. Reference rejects ANY
alphanumeric touching a numeral: `print(3or 4)` prints 3 in dellingr,
"malformed number near '3o'" in reference; `1e5or 2`, `0x5rad` (codified in
`test_lexer07`) same class. Fix: reject `is_ascii_alphanumeric()` (and `_`)
after a numeral.

### 42. Vertical tab is not whitespace (A-A9)

`lexer.rs consume_whitespace` (264-276) uses `is_ascii_whitespace`, which
excludes VT (0x0B); C `isspace` includes it, so reference accepts VT between
tokens and after `\z`; dellingr: InvalidCharacter. Same for the parser-side
`\z` skip (`parser.rs:339`). One-line predicate fix in both places
(`numeral.rs is_lua_whitespace` already has the correct set; reuse it).

### 43. LParenLineStart ambiguity check keyed to LF only (A-A10)

`consume_whitespace` sets `starts_line` only on `'\n'` (lexer.rs:270). A file
using bare-CR line endings never produces `LParenLineStart`, so the
intentional "ambiguous function call" error silently does not fire for CR
files: `f<CR>(g)` parses as a call while `f<LF>(g)` errors. After the L17
bare-CR line-counting fix, this is the one remaining LF-only assumption in
the lexer. Set `ret = true` for the bare-CR branch too.

### 44. Capacity ceilings likely to bite data-heavy game scripts (A-A11 + B-B10)

All compile-time rejections (correct given the encoding), but far below
reference and plausible in real init/data scripts:

- 255 array entries per table constructor (`table.rs:158-160`,
  TooManyTableFields). Reference handles millions by flushing SETLIST every
  50 entries; dellingr could do the same with periodic `set_list(k)` flushes
  and no encoding change (optimizations.md #19).
- 255 distinct string literals per function - shared pool for string
  literals, global names, AND field names (`parser.rs:253-271`,
  TooManyStrings). `{x1=1, ..., x300=300}` is enough to fail.
- 255 distinct number literals per function (TooManyNumbers).
- 255 `t.f = v` SET_FIELD sites per function (`compiler.rs:259-270`,
  TooManyFieldAssignments; also B-B10) - the cache-slot byte (C operand) is
  the limiter, not the instruction. Instead of erroring, sites past 255
  could emit a cache-slot sentinel = "not cached" and take the slow path;
  capacity stops being a hard limit. Note from B: today every index < 255
  resolves to a slot, so the sentinel needs to be an out-of-range value
  (e.g. keep 255 reserved as "no cache"); `instr_set_field` already
  tolerates `cache = None` via `caches.set_field_lookup.get(idx)`.
- Cosmetic: >65535 GET_FIELD sites errors as `InternalError` rather than a
  SyntaxError (`compiler.rs:247-255`); inconsistent with the SET_FIELD path
  just below it.

### 45. `num_locals` over-counts in functions with params + sibling scopes (A-A12)

`add_local` (parser.rs:85-95) grows `num_locals` whenever
`locals.len() > num_locals`, but params are pushed without updating
`num_locals` (parser.rs:477-479). With P params, sibling scopes re-trigger
growth: `function f(a, b) do local x end do local y end end` ends with
num_locals = 2 though peak non-param locals is 1. Never under-counts (safe),
but each call pushes that many extra nils (`vm/eval.rs:346`) and consumes
stack headroom. Fix: track `num_locals = max(num_locals, locals.len() -
num_params)`.

### 46. Stack-trace line staleness for non-OP_CALL invocations (B-B11)

Only `OP_CALL` refreshes `call_info.ip` (`src/vm/frame.rs:206-213`). Calls
made by `OP_TFOR_CALL`, `__index`/`__newindex`/`__len`/`__tostring`
metamethod dispatch, and `table.sort` comparators leave the caller's
`CallInfo.ip` pointing at the previous `OP_CALL`, so error tracebacks report
the wrong caller line for errors raised inside iterators and metamethods.
Fix: refresh `ip` in those dispatch sites too (or derive it from the live
`Frame` at trace-build time).

### 47. `set_top` growth bypasses `MAX_STACK_SIZE`; assert-vs-Result inconsistency in the stack API (D-D7 + B-B12)

`src/vm/stack.rs:25-57`. `set_top(i)` with a large positive `i` pushes nils
in a loop with no `check_stack_space`, so a host (or buggy RustFunc) can
request `set_top(isize::MAX)` and OOM the process, bypassing the documented
1M-value cap enforced everywhere else. Also inconsistent error contract on
the same surface: `set_top` and `pop` `assert!` (panic) on misuse while
`insert`, `remove`, `replace`, `push_value` return
`ErrorKind::InvalidStackIndex`. Given error paths must leave State
consistent and reusable, the panicking forms are the odd ones out. Suggest
`check_stack_space` in the growth arm and converting the asserts to
`InvalidStackIndex` errors pre-1.0.

### 48. Default `table.sort` silently orders mixed/incomparable types (C-B5)

`src/vm/table_ops.rs:328-346`: without a comparator, numbers sort before
strings and anything else compares `Equal`. Reference raises "attempt to
compare number with string" (and errors on tables/booleans).
`table.sort({ 1, "a", 2 })` succeeds in dellingr; `table.sort({ {}, {} })`
is a no-op "sorted". Since "errors kill the callback" is the product stance,
silently succeeding is the divergence. Fix inside the comparator closure:
return a type error (needs `sort_by` restructured to a fallible sort or a
pre-scan; folds into optimizations.md #12).

### 49. `print`/`tostring` accept a non-string `__tostring` result (C-B7)

`to_string_with_meta` (`src/vm/table_ops.rs:363-392`) stringifies whatever
`__tostring` returns; its sibling `bytes_with_tostring_meta` (396-429)
correctly errors "'__tostring' must return a string". Reference errors in
both. `print` and the `tostring` builtin use the permissive one
(`lua_std/basic.rs:132, 223`).
`print(setmetatable({}, { __tostring = function() return {} end }))` errors
in reference, prints "object: ..." in dellingr. Fix: fold
`to_string_with_meta` over `bytes_with_tostring_meta` (one code path, one
behavior).

### 50. `__index` bottoming out in a string value errors instead of chaining (C-B8)

`handle_index_metamethod_inner` (`src/vm/metamethod.rs:88-137`) accepts only
table/function/RustFn handlers; a string `__index` (legal in reference,
where indexing re-dispatches on the string) raises a type error. Exotic;
note-only given the "no string metatable" design, but the error message
("attempt to index a table value" via `typ_simple`) is doubly wrong.

### 51. Error messages misreport functions as tables (C-B9)

`Val::typ_simple` (`src/vm/lua_val.rs:94`) maps every `Obj` to
`LuaType::Table` "for display purposes". Indexing a Lua function
(`local f = function() end; return f.x`) reports "attempt to index a *table*
value". Cosmetic, but user-facing and visible in diff tests against
reference messages.

### 52. Script `error()` raises `ErrorKind::InternalError` (D-D10, error taxonomy)

`src/lua_std/basic.rs:140-147` implements the `error` builtin with
`ErrorKind::InternalError(message)`. `src/error.rs:97-99` documents
`InternalError` as "corrupt bytecode or VM bug ... report these as bugs" and
renders it "internal error: <msg>". So `error("oops")` surfaces as an
internal VM bug ("0:0: internal error: oops") - wrong taxonomy for host
telemetry (an embedder filtering `InternalError` for crash reporting gets
user-raised errors), and diverges from reference's `input:LINE: oops`.
`RuntimeError` (or a dedicated `ScriptError` variant carrying the position
prefix) is the right shape. Spans error.rs and basic.rs.

### 53. Anchors created inside the `load_state` setup closure are silently invalidated (D-D11)

`src/vm/save_state.rs:599`: `materialize_payload` ends with
`state.registry.clear()`, which runs AFTER the host's `setup` closure
(`save_state.rs:459`). A host that pushes a value and anchors it during
setup gets a handle that is stale the moment `load_state` returns, with no
error (later use returns `InvalidAnchor`). Either run `registry.clear()`
before `setup`, or document that setup-time anchors do not survive.
(Clearing exists to drop anchors inherited from `State::with_callbacks`;
there are none in a fresh State, so moving the clear earlier is free.)

### 54. MSRV / version doc mismatches (D-D8)

`Cargo.toml:5` says `rust-version = "1.97"`; README badge (line 5) and
AGENTS.md both say 1.92. One is wrong. (README's `dellingr = "0.2"` snippet
also trails the crate's 0.3.0, minor.)

### 57. `tonumber` divergences (E-E15)

`src/lua_std/basic.rs:18-40, 159-215`:

- `+` sign accepted with an explicit base: `tonumber("+ff", 16)` -> 255;
  reference -> nil (reference only skips `-`).
- With a base, arg 1 must be a string type; reference coerces numbers:
  `tonumber(10, 16)` -> 16 in reference, error in dellingr.

### 58. Assorted minor divergences and design-confirmation notes

From E (E-E16, all confirmed at code level, cosmetic-to-small):

- `math.random(m, n)` with `m > n` reports "bad argument #1"; reference says
  `#2`.
- `math.log(x, base)` is always `ln/ln`; 5.4 special-cases base 2/10
  (`log(8,2)` is exactly 3.0 there, ~3.0000000000000004-class noise here).
  Within dellingr's control despite the README transcendental caveat.
- String library rejects number arguments where reference coerces
  (`string.len(42)`, `string.sub(123, 1)` error). One systematic decision
  should be recorded either way.
- `error()` ignores the level argument and adds no position prefix (arguably
  deliberate without pcall).
- `string.format("%u")` accepted; Lua 5.4 removed `%u`. Harmless leniency,
  but the module claims the 5.4 contract - decide deliberately.
- `string.format("%p")` prints `(null)` for non-collectable values;
  glibc-based reference prints `(nil)`. `%p` output is inherently divergent;
  diff-test cosmetics only.
- `_G` proxy stringifies keys (`_G[1]` aliases `_G["1"]`; non-string keys go
  through `to_string`), unlike reference's real table. Inherent to the proxy
  design; recorded so it is a decision, not an accident.

From B (B-B12, robustness notes):

- `Frame::jump` (`frame.rs:83-99`) accepts `ip == code.len()`; the next
  `get_instr` would panic on the OOB fetch. Unreachable with
  compiler-emitted bytecode (every chunk ends with OP_RETURN), but the bound
  should be `<` for defense in depth.
- Numeric `for` with step 0 skips the loop (`eval_control.rs:316-326`).
  Matches 5.2 for ascending ranges, silently diverges from 5.2's infinite
  loop for descending ranges and from 5.4's "'for' step is zero" error.
  Looks deliberate - worth one line in README's divergence notes if so.
- Arithmetic does not coerce numeric strings (`"10" + 1` is a type error;
  reference yields 11; `eval_float_float`/`pop_num`,
  `eval_store.rs:511-530`). Concat DOES coerce numbers to strings. If the
  strictness is deliberate (it reads that way), it belongs on the README
  "Won't implement" list; today it's an undocumented divergence in an
  implemented feature area.

From C (latent hazards and nits):

- C-E2: `alloc_string` runs the `is_full` check and potential full
  collection even when the string is already interned - a hot loop touching
  only existing strings can pay a whole mark+sweep at the threshold boundary
  with zero reclaimable garbage; threshold then doubles. Amortized-fine;
  noted to preempt "GC runs with nothing to collect" confusion in profiles.
- C-E3: upvalue pool never frees slots (documented). A closed upvalue whose
  closure died retains a stale `Val` forever; never marked, never read again
  (refs only flow from closures) - a bounded leak, not a safety issue. But
  see optimizations.md #21: the "VMs have short lifetimes" justification
  contradicts the long-lived-State story the snapshot feature implies.
- C-E4: `call_anchor`'s `insert_at` check only guards against underflowing
  the whole stack, not against inserting below `stack_bottom`; a host that
  lies about `args` can corrupt a caller frame's slots. Suggest
  `checked_sub` against `get_top()` instead of `stack.len()`.
- C-F1: `t.foo` on a table without the field falls back to the `table`
  library (`instr_get_field` -> `push_table_library_field`,
  `eval_index.rs:37-39, 350-363`), so `({}).insert == table.insert`.
  Deliberate, but note the asymmetry: `t["insert"]` via `OP_GET_TABLE` does
  NOT fall back, so `t.insert ~= t["insert"]`. Worth one README line if it
  is contract; also a perf drag (optimizations.md #10).
- C-F2: `_G` proxying, `pairs(_G)` iterating the near-empty proxy, and
  globals never physically removed (nil stored instead) are consistent with
  the documented design; index-based global ICs stay valid because `globals`
  is append-only and existing-key inserts keep indices.

From D (CLI nits): multiple filename args - last silently wins; negative
`--limit` accepted and immediately exhausts. Both arguably fine.

---

## Coverage with no finding (merged from all five corners)

**A (front end):** determinism - the whole front end is Vec-scan based (no
HashMap, no entropy, no platform-dependent behavior); decimal literals via
Rust's correctly-rounded `str::parse::<f64>`, hex via in-crate `numeral.rs`;
identical source -> identical Bytecode byte for byte. `numeral.rs`
round-to-nearest-even with sticky bits, subnormal/overflow paths, exponent
saturation all consistent with IEEE semantics and the C30/L16 tests.
`Instr::call(ArgCount::Fixed(255))` in re-emission paths round-trips
correctly (255 encodes back to Dynamic) - fragile-but-correct; a `from_u8`
round-trip would read better. `code.remove(mark_idx)` index safety (jump
offsets, break-jump indices, table-template indices, tail-call indices) all
verified safe. Repeat-until scoping (C29),
close-upvalue emission at scope/break boundaries, multi-assign ordering,
`...` restriction, method-call desugaring, escape decoding bounds: correct.
`assign_cache_slots` bounds and deterministic slot assignment: correct.

**B (execution core):** budget boundary - `add_cost!` batching
(`frame.rs:147-159`) plus `consume_cost` (`vm.rs:329-339`) enforces exactly
"the op that crosses the boundary completes; the next costed op fails",
including flushes before OP_CALL/OP_TFOR_CALL/OP_RETURN and metamethod
invocations. One soft edge: up to 63 accumulated cost is dropped (never
added to `cost_used`) when a frame errors mid-batch - reporting accuracy
only (also noted by C). Dynamic SET_LIST sentinel is count==0 (not 255);
`analyze_cost` adds 0 for dynamic constructors, consistent with the runtime
minimum; base validation and error-path watermark truncation of
`vararg_call_bases`/`table_constructor_bases` hold. 255 ceilings (Dynamic
args, RetCount::All, `__call` prepend, `unpack`/`select`) all error cleanly;
`__call`-chain recursion bounded. Cache-slot aliasing: `finalize` rewrites
every GET_GLOBAL/GET_FIELD/SET_FIELD with a real slot index, so plain
constructors' implicit index 0 never reaches the runtime.
`with_restricted_env` swaps are honored by all three IC families via
`globals_version`, restore-under-panic via catch_unwind. `mark_gc_roots`
otherwise sound for the execution core (active closures and scoped temporaries
via `transient_roots`, per-frame literals via `string_literals`, metamethod
key/val protection pushes present).

**C (data plane):** `StringPool.hash_index` uses IndexMap keyed by pinned
FxHash values; iteration only during sweep; insertion-order semantics keep
it honest. Anchor `state_id` from a process atomic is documented and never
script-observable. Registry/slotmap iteration orders are
insert/release-history deterministic. Recently-fixed items re-scrutinized:
interned strings counting toward the GC threshold (`allocation_count`,
`is_full`, `collect` recompute) is coherent; the `usize::MAX`
auto-GC-disabled sentinel is correctly preserved across explicit
collections, and `saturating_mul(2).max(20)` cannot produce the sentinel for
any real heap size. `table_sort` charges `n.max(1)` BEFORE the comparator
runs (correct per the L18 contract). Anchors: registry correctly in the root
set; generational keys + state_id make stale/cross-state handles error
cleanly; `anchor()` validates before popping.

**D (state/persistence/host):** saturating `consume_cost` correct
(`saturating_sub_unsigned`/`saturating_add`; covered by
`consume_cost_saturates_large_host_charges`); the eval-loop batcher flushes
eagerly so batching cannot defer the boundary. Empty-state snapshot
round-trip correct (`has_standard_environment` false path,
`State::empty_with_callbacks` on load, no env tokens). on_error for
host-direct RustFunc failures fires exactly once (`call_depth == 0 &&
stack_trace.is_empty()` guard); Lua-frame path fires in `eval_closure_inner`.
Panic-safe restricted-env restoration is correct as far as it goes (but see
#1). Documented contracts: math determinism doc matches code; snapshot
versioning matches (FORMAT_VERSION strict equality is the gate, crate
version read-and-discarded); `analyze_cost` "neither lower nor upper bound"
doc matches. `VmRng` fully deterministic, pinned by test; seed 0 default
documented; `random_range_i64` degenerate-range and bias behavior documented
and sane. Save output deterministic (BTreeMaps + traversal order +
insertion-order globals; aliased fn addresses resolve to the
lexicographically-smallest id independent of registration order). Decoder
reservation capping (`len.min(remaining)`) defangs forged length prefixes;
memory linear in input size. `validate_quiescent` covers all transient
stacks/counters; eval_closure watermarks (L8) keep the State quiescent after
killed callbacks. Non-findings: anchored-only values not serialized is
documented (`SaveDiagnostics::anchor_count`); `rust_fn_ids_by_addr`
collapsing ICF-folded functions is safe (identical code, identical
behavior); cost/rng values are host-forgeable, so hosts that load
user-editable saves must reset the cost budget after loading.

**E (stdlib/patterns):** `VmRng::random_range_i64` uses i128, no overflow
from `math.random` extremes. RustFunc returning exactly 255 results does not
collide with the `RetCount::All` sentinel. gmatch's leading-`^` handling
matches reference (treats `^` literally). gsub empty-match advancement,
anchored gsub, `$`/`^` anchors at `init`, `[]]`/`[^]]` class closing,
`%b((` validation, `()` position-capture positions with `init` bases: all
line up with reference. string_format: directive length/width caps, flag
validation per conversion, `#o`/`#x` prefixes, `%g` significant-digit
selection and zero-stripping, zero-padding after `0x` for `%a`, `%q` quoting
table (incl. 3-digit escapes before digits), `%c` mod-256,
integer-representation errors - all consistent with the 5.4 contract as far
as static reading can tell. Rust's round-half-even float formatting matches
glibc's.

---

## Deferred hardening

### 59. Saved bytecode stack discipline is not verified (phase 2)

- **Locations:** `src/vm/eval.rs:235,407,434`, `eval_index.rs:436`,
  `eval_store.rs:454`, and the bytecode dispatch paths for `DUP`, `SWAP`,
  `MARK_CALL_BASE`, table initialization, field/table stores, numeric and
  generic loop helpers, fixed `SET_LIST`, and `GET_TABLE`.
- **Cause:** phase 1 validates bytecode structure, operands, cache layout, and
  nesting, but deliberately does not perform abstract operand-stack dataflow
  or marker-stack/CFG-join verification. Forged code can therefore still
  underflow `pop_val`, `DUP`'s `.last().expect`, `SWAP`'s `len - 1`/`len - 2`,
  `CONCAT A`'s `len - A`, fixed `RETURN n` while locating return values,
  `RETURN RetCount::All`'s `stack.len() - frame_base`, direct local accesses,
  loop-local ranges, a later `get_top()` call below `stack_bottom`, or open
  upvalues after malformed code has popped frame slots.
- **Fix sketch:** add the deferred stack-discipline verifier with abstract
  stack heights, vararg-call and table-constructor marker stacks, dynamic
  result counts, and agreement at CFG joins. It needs compiler-corpus proof
  before it may reject saves.
- **Status:** this is now the *only* thing standing between phase 1 and the
  stated promise ("malformed save structure is rejected with a `LoadError`; it
  cannot trigger an indexing, stack-underflow, or recursive-traversal panic
  during load"). The recursive-GC half of that caveat is gone: marking and save
  encoding are both iterative, so a deep decoded table graph no longer
  overflows during the `gc_collect()` at the end of materialization.

## Orchestrator notes (carried from the corner reports)

- #17 deserves a regression test before/with any fix.
- The A-corner P2/P3 items (#18, #19, #40-#43) are diff-testable against
  lua5.2/lua5.4 with small scripts; the CR/VT cases need byte-level fixtures
  (careful with editors normalizing line endings).
- E's verification list: (1) #36 assumes `check_type` errors on nil - check
  `vm_aux.rs`; (2) `table_remove_at(1, 0)` / `(1, len+1)` semantics vs
  reference's `t[0]` read/write edge (vm-side, was out of E's corner);
  (3) `src/patterns/mod.rs:211` asserts the #12 divergence as expected
  behavior and will need updating with the fix.
- B ordered findings most-severe-first without labels; severities shown here
  for B items are the consolidator's placement of B's ordering, not new
  adjudication.
