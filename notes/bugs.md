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

### 5. `Val` equality/hash for `RustFn` compares payload addresses, not function pointers (B-B1 + C-B1, two independent reports)

- **Locations:** `src/vm/lua_val.rs:178-195` (PartialEq), `:153-176` /
  `:169-172` (Hash), `:130, 145` (Debug/Display), `:105` (`to_string_with_heap`).
- **Cause:**

```rust
(RustFn(a), RustFn(b)) => {
    let x: *const RustFunc = a;   // a: &RustFunc -> pointer to the fn-ptr
    let y: *const RustFunc = b;   //                STORAGE inside the Val
    x == y
}
```

  `a`/`b` bind as `&RustFunc` (match ergonomics); coercing to
  `*const RustFunc` yields the address of the payload slot inside each `Val`,
  not the function's address. Two `Val`s holding the same function always
  live at different addresses (OP_EQUAL pops both operands into separate
  locals, `frame.rs:262-271`), so equality is effectively always false, and
  `Hash` hashes the payload address - also nondeterministic across runs.
- **Script-visible consequences:**
  - `print == print` evaluates false (reference: true); same for
    `local f = print; f == print`.
  - `rawequal(print, print)` (`src/vm/stack.rs:215-220`) is false.
  - Host functions are broken as table keys: `t[print] = 1; t[print]` is nil,
    and repeated assignment appends duplicate entries (inline scan and
    IndexMap probe both fail; probe hash differs from stored hash so Map
    lookups always miss).
  - `string.format("%p", fn)`: `format_pointer_id`
    (`src/vm/table_ops.rs:432-445`) probes with `*candidate == val`, never
    matches for RustFn, so every `%p` on the same host function mints a fresh
    id - defeats the deterministic-identity feature (tables unaffected), and
    is a leak-rate concern.
  - The method-IC validation `index_handler != entry.index_handler`
    (`eval_index.rs:299`) can never validate a cached `__index = <RustFn>`
    handler, so those callsites permanently miss (perf symptom of same bug).
- **Related inconsistency:** `Debug`/`Display` for `Val::RustFn` format
  `func: &RustFunc` with `:p` (the reference's address = payload slot), while
  `to_string_with_heap` (by value) prints the actual function address - so
  `tostring(print)` and a debug-printed error context show different
  "addresses" for the same value.
- **Note:** TODO.md's "Stable RustFunc identity" entry describes this code as
  hashing "by function-pointer address" - wrong; the bug is stronger than
  what TODO tracks. It does not even do that.
- **Repro:**

```lua
print(print == print)          -- dellingr: false, Lua: true
local t = {}
t[print] = 1
print(t[print])                -- dellingr: nil, Lua: 1
print(string.format("%p", print) == string.format("%p", print))
                               -- dellingr: false (two fresh ids)
```

- **Fix:** compare/hash the function pointer value itself:
  `std::ptr::fn_addr_eq(*a, *b)` (already used correctly in
  `eval_control.rs:138-143`) and `(*func as usize).hash(hasher)`. Cross-build
  instability of fn addresses is irrelevant in-process; the snapshot concern
  stays tracked in TODO.md.

### 7. Unbounded parser recursion: hostile source aborts the host process (A-A1)

- **Locations:** `src/compiler/parser/expr.rs` (`parse_expr -> parse_or ->
  ... -> parse_primary -> parse_prefix_exp -> parse_expr` via parens,
  `parse_unary -> parse_unary`, `parse_pow -> parse_unary -> parse_pow`),
  `stmt.rs:416-423` (`parse_statements -> parse_do`), `stmt.rs:503-547`
  (`parse_if_arm -> parse_else_or_elseif`), `table.rs` (`parse_table ->
  parse_table_entry -> parse_expr -> parse_table`).
- **Cause:** `nest_level: i32` tracks scope depth but is never bounded.
  Reference Lua rejects deep nesting with "chunk has too many syntax levels"
  (LUAI_MAXCCALLS, ~200). dellingr instead exhausts the native stack: a Rust
  stack overflow is an abort, killing the whole game process. Parser-side
  sibling of the L17 lexer comment-recursion fix - the lexer got hardened,
  the parser did not.
- **Repro (do not run in-process without expecting an abort):** 200k nested
  parens around `x`, or 200k of `do `, or 200k of `-` before a literal, or
  200k nested `{`.
- **Fix sketch:** a `syntax_depth: u32` on `Parser`, incremented in
  `parse_expr`, `parse_statements`, `parse_table`, `parse_prefix_extension`
  (recursive extension chains like `a.b.c.d...` also recurse), error
  `SyntaxError::TooManySyntaxLevels` past ~200-500. Cheap, matches reference.

### 8. Upvalue index truncation past 255: silent miscompile (A-A2)

- **Locations:** `src/compiler/parser/upvalue.rs:101-105` (`add_upvalue`:
  `self.upvalues.len() as u8`), `:7-12` (`find_upvalue`), `:69, 79, 90`
  (`create_parent_upvalue`).
- **Cause:** no cap on the per-function upvalue list, unlike `add_local`
  (parser.rs:85-95, capped at 255) and reference Lua (MAXUPVAL 255, "too many
  upvalues"). A function can legally reference more than 255 distinct outer
  names (200 locals in grandparent + 60 in parent, inner function referencing
  all 260): the 256th upvalue gets index 0 via `as u8` truncation and
  `Instr::get_upvalue(0)` silently reads the wrong variable. No error, no
  panic - a miscompiled program. `UpvalueDesc::Upvalue(idx)` is stored into
  `Bytecode.upvalues` with the truncated index, so the closure capture list
  itself is wrong and the snapshot codec faithfully persists the wrong
  program.
- **Repro sketch:** generated script - function A declares a1..a200, nested B
  declares b1..b60, inner C returns `a1+...+a200+b1+...+b60`; C's 256th
  captured name resolves to upvalue slot 0.
- **Fix:** mirror `add_local` - new `SyntaxError::TooManyUpvalues` when a list
  would exceed 255, in `add_upvalue` and both push sites in
  `create_parent_upvalue`.

### 9. `line_info` desyncs from `code`: wrong line numbers everywhere (A-A3)

- **Locations:** `parser.rs:386-389` (`Parser::push` appends in lockstep);
  desync sites: `expr.rs:272` and `expr.rs:327`
  (`self.chunk.code.remove(mark_idx)` - executed for EVERY plain fixed-arg
  call), plus pop-then-push adjustments leaving one stale extra entry each:
  `expr.rs:257-268`, `expr.rs:312-323` (vararg / tail-call arg adjustment),
  `stmt.rs:24-31` (return tail), `stmt.rs:371-388` (`adjust_multi_assign`).
- **Cause:** every post-hoc rewrite of `code` ignores `line_info`;
  `code.remove` leaves `line_info` one longer and misaligned for every
  instruction at or after `mark_idx`. Consumers index by pc:
  `vm/frame.rs:70` (`current_line`, stack traces), `vm.rs:488` (`host_print`
  line), `compiler.rs:263` (`assign_cache_slots` error line), and
  `save_state.rs` serializes the skewed vector. Skew accumulates one slot per
  call site; in call-heavy scripts, error traces report lines earlier than
  the actual error - the more code, the further off.
- **Repro:**

```lua
local function f() return 1 end
f() f() f() f() f() f() f() f() f() f()
local boom = nil + 1   -- reference reports this line; dellingr's trace
                       -- reports an earlier line (skew = ~10 slots)
```

  Or simply assert `bc.code.len() == bc.line_info.len()` after parsing
  `print(1)` - it fails today.
- **Fix options:** (a) route every code mutation through helpers that mirror
  the edit into `line_info` (pop_instr / remove_instr); (b) the OP_NOP
  restructure (optimizations.md #7), which removes `code.remove` entirely and
  makes the invariant structural. Either way add a debug_assert of equal
  lengths in `finalize`.

### 10. `load_state` performs zero bytecode validation; a forged save escalates to process panic (D-D2, hostile input)

- **Locations:** `src/vm/save_state.rs:645-709`
  (`materialize_bytecode`/`build_bytecode`), `src/instr.rs:350`
  (`Instr::from_raw`).
- **Cause:** the codec is carefully bounds-checked (length caps, `read_exact`,
  no over-reservation) but the semantic content is trusted; `Bytecode` is
  rebuilt directly from attacker-controlled fields with no verifier. Game
  saves are classic user-edited input, so this is reachable in the product's
  own use case. Concrete panic vectors (all ordinary safe-Rust panics, not
  UB, but they abort the host callback and are not catchable as `LoadError`):
  - code that runs off the end (no `OP_RETURN`): `Frame::get_instr` indexes
    `code[self.ip]` (`frame.rs:103-107`) - OOB panic.
  - `OP_PUSH_NUM`/`OP_PUSH_STRING`/`OP_CLOSURE` with an index beyond
    `number_literals`/`string_literals`/`nested` (`frame.rs:110-117`,
    `eval_index.rs:371`) - OOB panic.
  - `OP_GET_LOCAL`/`OP_SET_LOCAL` beyond the frame (`eval_index.rs:436-440`);
    `OP_GET_UPVALUE n` with no upvalues (`eval_index.rs:443`) - OOB panic.
  - `OP_GET_BUILTIN`/`OP_SET_BUILTIN` with slot >= `Builtin::COUNT` (19)
    (`eval_index.rs:414-423`) - OOB panic.
  - `OP_POP`/`OP_SWAP`/`OP_DUP` on an empty stack - `pop_val` expect / swap
    OOB.
  - forged `num_locals`/cache-slot counts inconsistent with cache-indexed
    instructions.
- Also forgeable as plain data: `cost_remaining`/`cost_budget`/`cost_used`
  (a save-file editor grants itself infinite budget) and `rng_state`. Those
  may be acceptable (saves are host-owned data), but the panic class is not,
  given the decoder otherwise promises graceful `LoadError`s (`CorruptArena`,
  `DecodeError`).
- **Fix sketch:** a linear verifier pass over each `SavedBytecode` at load:
  opcode in the known set, literal/nested/cache indices within declared
  pools, jump targets within `0..=code.len()` (simple per-instruction static
  check of `ip + sbx`; `Frame::jump` re-checks), code non-empty and
  terminated, upvalue descriptor indices within parent ranges,
  `num_locals`/`num_params` sane. Alternatively document loudly that save
  bytes are trusted input (weaker; contradicts the decoder's hostile-input
  posture).

### 11. `build_bytecode` recursion is unbounded; a forged save aborts via native stack overflow (D-D3, hostile input)

- **Location:** `src/vm/save_state.rs:657-709`.
- **Cause:** recursion through `nested` chunk ids has cycle detection
  (`visiting`) but no depth bound. A forged save can encode N chunks in a
  linear parent-child chain (a few dozen bytes each), so a ~10 MB file yields
  recursion hundreds of thousands of frames deep - native stack overflow,
  cannot be caught or returned as `LoadError`. Legitimate saves are bounded
  by compiler nesting limits, so an explicit depth cap (or an iterative
  post-order worklist, optimizations.md #5) costs nothing for real data.

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

### 13. Every pattern ending in an escaped percent (`%%`) is rejected as malformed (E-E2)

- **Location:** `src/patterns/luapat.rs:688` (`str_check`):
  `if at(sub(ms.p_end, 1)) == b'%' { return Err(... EndsWithPercent) }`.
- **Cause:** checks only the last byte, so valid patterns ending in the
  two-byte escape `%%` are rejected: `"%d+%%"`, `"%%"`, `"%%%%"`. Reference
  only rejects a genuinely dangling `%`. The runtime matcher (`classend`)
  handles `%%` correctly; only this eager pre-check misfires. The pre-check
  exists because `str_match_check`'s `L_ESC` arm (line 534: `let c = at(p)`)
  lacks its own bounds check and would read past the end on a trailing single
  `%`.
- **Repro:**

```lua
print(("50%"):gsub("%%", " percent"))  -- reference: "50 percent" 1; dellingr: error
print(("100%"):match("%d+%%"))         -- reference: "100%"; dellingr: error
```

- **Fix:** check trailing-percent parity (or add the bounds check in the loop
  and drop the pre-check).

### 14. Script-reachable panic: 32 captures including a position capture overflow the results array (E-E3)

- **Locations:** `src/patterns/luapat.rs:628-630` (validator's `()` branch
  just advances `p` - position captures not counted), `:300-302`
  (`start_capture` allows `level` to reach exactly 32 = LUA_MAXCAPTURES),
  `src/patterns/mod.rs:18` (`matches: [LuaCapture; 32]`), `:669`
  (`str_match` stores the whole match at `mm[0]`, captures into `mm[1..]`,
  len 31).
- **Cause:** a pattern with 32 `()` passes `str_check`; with `level == 32`,
  `push_captures` writes `mm[1..][31]` - index out of bounds, panic (not a
  Lua error).
- **Repro:**

```lua
-- 32 position captures:
print(("x"):match("()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()"))
```

- **Fix:** size the results array `LUA_MAXCAPTURES + 1`, and make the
  validator count position captures (also fixes #34a).

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

### 23. `tostring(fn)` leaks ASLR-dependent addresses: cross-run output nondeterminism (B-B7 + C-C1, two independent reports)

- **Locations:** `src/vm/lua_val.rs:105, 117`
  (`to_string_with_heap`/`to_bytes_with_heap` render RustFn as
  `<function: 0x...>` with the real function pointer).
- **Cause:** under PIE/ASLR that address changes between runs of the same
  binary, so `print(tostring(print))` differs across identical runs - and
  scripts can branch on the string (`tostring(print):find("7")`), so this is
  replay-visible, not cosmetic. Contradicts TODO.md's claim that "nothing
  observable depends on its stability". Tables already solved this:
  `ObjectPtr` Display prints the slotmap key (deterministic), and `%p` mints
  deterministic ids. Lua closures are fine (slotmap key rendering is
  history-deterministic).
- **Fix:** route function rendering through `format_pointer_id`-style
  deterministic ids (requires threading `&mut State` or pre-assigning ids at
  registration; also fixes the #5 Debug/Display inconsistency).

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

### 27. GC mark phase is recursive - deep structures overflow the Rust stack (C-A5; cross-noted by D)

- **Locations:** `src/vm/object.rs:355-386` (`GcHeap::mark` ->
  `mark_children` -> `Table::mark_values` -> `Val::mark_reachable` ->
  `GcHeap::mark`), `src/vm/table.rs:555-573`.
- **Cause:** recursion once per nesting level with no depth bound.
  `MAX_CALL_DEPTH`/`MAX_METAMETHOD_DEPTH` protect the interpreter; nothing
  protects the collector. A script builds a chain far deeper than the ~8 MB
  main-thread stack tolerates (roughly 5 frames per level).
- **Repro:**

```lua
local t = {}
for i = 1, 500000 do t = { t } end   -- cost ~2/iteration; auto-GC fires
-- during the loop and the mark phase recurses ~i deep -> stack overflow abort
```

- **Fix:** iterative marking with an explicit worklist (`Vec<ObjectPtr>` gray
  stack; optimizations.md #5). Also removes the `#[hotpath::measure]`
  recursion caveat for `mark`.

### 28. `SaveBuilder::encode_object` recursion is unbounded in data depth (D-D4, script-triggered host abort)

- **Location:** `src/vm/save_state.rs:313-351` (`encode_object` ->
  `encode_val` -> `encode_object`).
- **Cause:** recursion per nesting level of tables/closures. A script builds a
  deep chain cheaply (`local t = {} ; for i = 1, 200000 do t = {t} end ;
  g = t` costs ~2 per iteration, well inside normal budgets); the host then
  calls `save_state()` and overflows the native stack - script-controlled
  data kills the host during an API call typed to return
  `Result<_, SaveError>`. `GcHeap::mark`/`mark_children` share the recursive
  shape (see #27), so with auto-GC enabled the same chain usually aborts
  inside `gc_collect` even before the save is attempted. Both should move to
  an explicit worklist together (optimizations.md #5).

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

### 34. Pattern validator capture-count divergences (E-E4)

`src/patterns/luapat.rs`, `str_match_check`:

- a) Position captures are not counted (lines 628-630), so backreference
  validation is wrong when position captures precede a real capture:

```lua
print(("aa"):match("()(a)%2"))  -- reference: 1 "a"; dellingr: error "invalid capture index %2"
```

- b) Off-by-one ceiling: the validator increments `level` and then rejects
  `level >= 32` (lines 623-627), allowing only 31 captures, while the runtime
  (`start_capture`, line 302) and reference allow exactly 32. A
  32-normal-capture pattern is spuriously rejected with "too many captures".

### 35. Escaped uppercase non-class letters match the wrong character (E-E5)

- **Location:** `src/patterns/luapat.rs:171-191` (`match_class`).
- **Cause:** lowercases the class byte before the literal-comparison
  fallback (`let res = match class.to_ascii_lowercase() { ... lc => return
  lc == ch }`); reference compares the original byte in the default case
  (`default: return (cl == c);`). Affected:
  `%B %E %F %H %I %J %K %M %N %O %Q %R %T %V %Y` (uppercase letters whose
  lowercase is not a class letter), both bare and inside `[...]` classes.
  Naive "escape every char" pattern-quoting helpers produce exactly these.
- **Repro:**

```lua
print(("E"):match("%E"))  -- reference: "E"; dellingr: nil
print(("e"):match("%E"))  -- reference: nil; dellingr: "e"
```

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

### 55. TODO.md "Stable RustFunc identity for serialization" is stale (D-D9)

TODO.md:79-98 claims "dellingr doesn't ship a serialization story today" and
proposes registration-time stable ids. The snapshot feature ships exactly
this (`State::register_rust_fn`, `set_global_named_rust_fn`, dotted stdlib
ids, save-side `rust_fn_ids_by_addr`). Per the backlog convention the entry
should be deleted or rewritten to cover only the remaining sliver
(pointer-address rendering in `Display`/hash of `Val` - see #5 and #23).

### 56. Empty pattern is UB one call site away (E-E14, hardening)

`str_check` (`luapat.rs:681`) and `str_match` (`:655`) do `at(p)` on the
first pattern byte unconditionally; with an empty pattern that is an
out-of-bounds read of a dangling pointer. Every current stdlib call site
guards empty patterns before reaching the matcher, but the invariant is
implicit and undocumented. Add an explicit empty guard in
`LuaPattern::from_bytes_try` / `str_match`.

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
verified safe - only `line_info` is broken (#9). Repeat-until scoping (C29),
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
behavior); cost/rng values being host-forgeable is acceptable (saves are
host-owned; the integrity problem is the panic class, #10).

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

## Orchestrator notes (carried from the corner reports)

- #7, #8, #9, #17 deserve regression tests before/with any fix (#9 is
  testable as a pure invariant: `code.len() == line_info.len()` recursively
  after `finalize`).
- The A-corner P2/P3 items (#18, #19, #40-#43) are diff-testable against
  lua5.2/lua5.4 with small scripts; the CR/VT cases need byte-level fixtures
  (careful with editors normalizing line endings).
- E's verification list: (1) #36 assumes `check_type` errors on nil - check
  `vm_aux.rs`; (2) `table_remove_at(1, 0)` / `(1, len+1)` semantics vs
  reference's `t[0]` read/write edge (vm-side, was out of E's corner);
  (3) the E repros are diff-test ready, but #14 is a panic - keep it out of
  `examples/` until fixed or run_examples will abort; (4)
  `src/patterns/mod.rs:211` asserts the #12 divergence as expected behavior
  and will need updating with the fix.
- B ordered findings most-severe-first without labels; severities shown here
  for B items are the consolidator's placement of B's ordering, not new
  adjudication.
