# Bug hunt 2026-07-24 - Corner E: Stdlib and patterns

Auditor: read-only pass over `src/lua_std.rs`, `src/lua_std/{basic,math,string,string_format,table}.rs`,
`src/patterns/{mod,luapat,errors}.rs`. Supporting reads (verification only): `src/vm.rs`,
`src/vm/{rng,stack,eval,eval_control,frame}.rs`, README/OPTIMIZATIONS/TODO.

All repros are written down, not executed. Reference behavior claims are against Lua 5.2/5.4
C sources (lstrlib.c, lbaselib.c, ltablib.c, lmathlib.c) from memory; each repro is a
one-liner suitable for `diff_test.sh`.

Legend: severity is my ranking within this corner. CONFIRMED = verified by direct code
reading; PLAUSIBLE = needs one fact checked outside my corner.

---

## A. High severity

### E1. CONFIRMED - matchdepth is consumed by tail calls and never reset between attempts; benign patterns die with "pattern too complex"

`src/patterns/luapat.rs`. `patt_match` decrements `matchdepth` on entry (line 351) and only
restores it on the fall-through paths. Every tail-style `return self.patt_match(...)` skips the
restore: line 391 (`%b` continuation), 413 (`%f` continuation), 421 (backref continuation), and in
`patt_default_match` lines 440 (`*`/`?`/`-` accept-empty), 455 (`?` else-branch), 473 (item with no
suffix). Reference C uses `goto init` for exactly these transitions, so tail continuation is
depth-free there; 5.2 even asserts `matchdepth == MAXCCALLS` before every anchor attempt.

Two distinct failure modes:

1. Pattern length: a chain of N non-suffixed items costs N depth. Any pattern with more than
   ~198 sequential items errors "pattern too complex"; reference matches patterns of arbitrary
   length (depth there only counts real recursion). The test
   `src/patterns/mod.rs:211` (`runtime_match_errors_are_not_swallowed`, 201 literal `a`s)
   enshrines this divergent behavior as expected.
2. Leak across the scan loop: `str_match` (line 661) never resets `matchdepth` (or `level`)
   between anchor positions, and each failed attempt that partially matched leaks depth equal to
   its tail-chain length. A subject with ~200 partial matches kills the call.

Repro (mode 2, no string.rep needed):

```lua
local s = ""
for i = 1, 250 do s = s .. "a" end
print(s:find("a%d"))      -- reference: nil; dellingr: error "pattern too complex"
-- realistic shape: subject containing many "id" prefixes, pattern "id=(%d+)"
```

Fix sketch: rewrite the tail transitions as a loop (`s = ...; p = ...; continue`), mirroring the
C `goto init`, and reset `matchdepth = MAXCCALLS; level = 0` at the top of each `str_match`
attempt. See O1.

### E2. CONFIRMED - every pattern ending in an escaped percent (`%%`) is rejected as malformed

`src/patterns/luapat.rs:688` (`str_check`):

```rust
if at(sub(ms.p_end, 1)) == b'%' { return Err(... EndsWithPercent) }
```

This checks only the last byte, so valid patterns ending in the two-byte escape `%%` are
rejected: `"%d+%%"`, `"%%"`, `"%%%%"`. Reference only rejects a genuinely dangling `%`.
The runtime matcher (`classend`) handles `%%` correctly; only this eager pre-check misfires.

Repro:

```lua
print(("50%"):gsub("%%", " percent"))  -- reference: "50 percent" 1; dellingr: error
print(("100%"):match("%d+%%"))        -- reference: "100%"; dellingr: error
```

The pre-check exists because `str_match_check`'s `L_ESC` arm (line 534: `let c = at(p)`)
lacks its own bounds check and would read past the end on a trailing single `%`. Fix: check
trailing-percent *parity* (or add the bounds check in the loop and drop the pre-check).

### E3. CONFIRMED - script-reachable panic: 32 captures including a position capture overflow the results array

Three interacting facts:

- The validator does not count position captures at all: `src/patterns/luapat.rs:628-630`
  (the `()` branch just advances `p`). So a pattern with 32 `()` passes `str_check`.
- The runtime `start_capture` (line 300) allows `level` to reach exactly 32
  (`LUA_MAXCAPTURES`).
- `LuaPattern.matches` is `[LuaCapture; 32]` (`src/patterns/mod.rs:18`), and `str_match`
  stores the whole match at `mm[0]` then captures into `mm[1..]` (len 31, line 669). With
  `level == 32`, `push_captures` writes `mm[1..][31]` -> index out of bounds -> panic.

Repro (panic, not error):

```lua
-- 32 position captures:
print(("x"):match("()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()"))
```

Fix: size the results array `LUA_MAXCAPTURES + 1`, and make the validator count position
captures (also fixes E4a).

### E12. CONFIRMED - table.move: unbounded uncharged work (budget bypass) and integer overflow

`src/lua_std/table.rs:255-307`. `table.move` charges cost 1, then loops
`count = e - f + 1` times doing `get_table` + `set_table_raw` regardless of table contents
(nil reads/writes still iterate). `count` is not bounded by any table length:

```lua
table.move({}, 1, 1e15, 2)   -- cost charged: 1; work: ~1e15 iterations (host hang)
```

This defeats the cost budget, which is the product feature. (Reference Lua also loops, but
reference has no budget to protect.) Additionally `e - f + 1` with saturated extremes
(`table.move(t, -1e300, 1e300, 1)` gives `f = isize::MIN`, `e = isize::MAX`) overflows
`isize`: panic in debug builds, silent wrap in release. Reference guards with
"too many elements to move" (`f > 0 || e < LUA_MAXINTEGER + f`).

Fix: charge per element (or per element in chunks) *before* the copy, and add the reference
overflow argcheck.

### E13. CONFIRMED - cost-model gap: pattern matching and string byte-work are entirely uncharged

There is not a single `consume_cost` in `src/lua_std/string.rs`, `string_format.rs`, or
`src/patterns/`. Consequences:

- A script can build a ~131k-char string with ~17 concat ops (repeated doubling), then each
  `gsub`/`find`/`match`/`upper`/`format("%s")` call does O(n) or worse work for ~0 charged
  cost, repeatable in a loop.
- Backtracking patterns are superpolynomial in *time* while the depth cap only bounds
  *recursion*: `("a*a*a*a*b")` against a long non-matching subject of `a`s does O(n^k)
  `singlematch` work inside `max_expand` for a single costed-at-0 call.
- `table.concat` charges 1 for O(total bytes). (`table.insert`/`remove` charging 1 for O(n)
  is already acknowledged in OPTIMIZATIONS.md; the string/pattern side is not tracked
  anywhere.)

Fix sketch: thread a charge hook into the matcher (charge per `patt_match` invocation or per
K `singlematch` steps against `State.cost_remaining`), and charge length-proportional cost
in `gsub`/`sub`/`upper`/`lower`/`reverse`/`format`/`concat` before doing the work (cost
contract: charge BEFORE the side effect).

---

## B. Medium severity

### E7. CONFIRMED - `%f` frontier loses left context because stdlib re-slices the subject

The matcher's `src_init` is the start of the *slice* it is handed, and `%f` treats
slice-start as string-start (`previous = '\0'`, luapat.rs:403-407). Reference keeps
`src_init` at the true beginning of the subject even when matching from `init` or resuming
gmatch/gsub mid-string. dellingr re-slices everywhere: `gsub` loop (`&s[pos..]`,
string.rs:575), `gmatch_iter` (`&s[pos..]`, string.rs:712), `find`/`match` with init
(`&s[init..]`, string.rs:320/416).

Repro:

```lua
print(("abcd"):gsub("%f[%w]%w", "X"))   -- reference: "Xbcd" 1; dellingr: "XXXX" 4
print(("ab"):find("%f[%a]%a", 2))       -- reference: nil;    dellingr: 2 2
```

Fix (structural, see O1): give `str_match` an `init` offset and keep the full subject, like
reference `prepstate`. This also deletes all the `base +` capture-offset arithmetic in
string.rs.

### E4. CONFIRMED - validator capture-count divergences

`src/patterns/luapat.rs`, `str_match_check`:

a) Position captures are not counted (lines 628-630), so backreference validation is wrong
   when position captures precede a real capture:

```lua
print(("aa"):match("()(a)%2"))  -- reference: 1 "a"; dellingr: error "invalid capture index %2"
```

b) Off-by-one ceiling: the validator increments `level` and *then* rejects `level >= 32`
   (lines 623-627), so it allows only 31 captures, while the runtime (`start_capture`,
   line 302) and reference allow exactly 32. A 32-normal-capture pattern is spuriously
   rejected with "too many captures".

### E5. CONFIRMED - escaped uppercase non-class letters match the wrong character

`src/patterns/luapat.rs:171-191` (`match_class`) lowercases the class byte *before* the
literal-comparison fallback:

```rust
let res = match class.to_ascii_lowercase() {
    ...
    lc => return lc == ch,   // BUG: compares the lowercased byte
};
```

Reference compares the *original* byte in the default case (`default: return (cl == c);`).
Affected: `%B %E %F %H %I %J %K %M %N %O %Q %R %T %V %Y` (uppercase letters whose lowercase
is not a class letter), both bare and inside `[...]` classes. Naive "escape every char"
pattern-quoting helpers produce exactly these.

```lua
print(("E"):match("%E"))  -- reference: "E"; dellingr: nil
print(("e"):match("%E"))  -- reference: nil; dellingr: "e"
```

### E10. CONFIRMED - explicit `nil` is rejected for optional stdlib arguments

Reference `luaL_opt*` treats nil as "absent". The prevailing dellingr pattern
`if num_args >= k { check_type(k, ...) }` errors on explicit nil instead. Affected:
`string.find` init (string.rs:268 - breaks the very common plain-find idiom
`s:find(pat, nil, true)`), `string.sub` j, `string.match` init, `string.gsub` n,
`table.concat` sep/i/j, `table.sort` comp, `table.unpack`/`unpack` i/j, `table.insert` pos
(3-arg form with nil pos errors differently than reference), `select` is fine.
Notably `tonumber` base and `table.remove` pos already do it correctly
(`state.typ(k) != LuaType::Nil` guard), so the codebase has the right pattern applied
inconsistently.

```lua
print(("a.b"):find(".", nil, true))  -- reference: 1 1; dellingr: bad-argument error
```

### E8. CONFIRMED - table.concat's `len == 0` short-circuit ignores an explicit range

`src/lua_std/table.rs:224`: `if i > j || len == 0 { return "" }`. Reference only defaults
`j` from `#t`; an explicit `i..j` range is honored regardless of the border.

```lua
local t = {}; t[2] = "x"
print(table.concat(t, "", 2, 2))  -- reference: "x"; dellingr: ""
```

Also: `i`/`j` (and `table.unpack`'s, see E9) go through `as usize`, so negative values
saturate to 0 instead of addressing negative indices like reference does.

### E9. CONFIRMED - unpack / table.unpack truncate negative start indices to 0

`src/lua_std/basic.rs:241` and `src/lua_std/table.rs:121`: `state.to_number(2)? as usize`
saturates negatives to 0. `unpack(t, -2, 2)` returns 3 values `t[0],t[1],t[2]` instead of
reference's 5 values `t[-2]..t[2]` (wrong count *and* wrong values). The 255-result cap
itself is a deliberate protocol limit (verified: RustFunc return counts are plain numbers in
`vm/eval.rs:102-119`, no sentinel collision at exactly 255).

### E11. CONFIRMED - math.modf(±inf) returns NaN fractional part

`src/lua_std/math.rs:323`: `x.fract()` is `x - x.trunc()`, which is `inf - inf = NaN` for
infinite inputs. Reference (both 5.2's C `modf` and 5.4's explicit `n == ip` test) returns
0.0.

```lua
print(math.modf(math.huge))  -- reference: inf 0.0 (5.2: "inf 0"); dellingr: inf nan
```

---

## C. Low severity / notes

### E14. CONFIRMED (hardening) - empty pattern is UB one call site away

`str_check` (luapat.rs:681) and `str_match` (:655) do `at(p)` on the first pattern byte
unconditionally; with an empty pattern that is an out-of-bounds read of a dangling pointer.
Every *current* stdlib call site guards empty patterns before reaching the matcher, but the
invariant is implicit and undocumented. Add an explicit empty guard in
`LuaPattern::from_bytes_try` / `str_match`.

### E15. CONFIRMED - tonumber divergences (src/lua_std/basic.rs:18-40, 159-215)

- `+` sign accepted with an explicit base: `tonumber("+ff", 16)` -> 255; reference -> nil
  (reference only skips `-`).
- With a base, arg 1 must be a string *type*; reference coerces numbers:
  `tonumber(10, 16)` -> 16 in reference, error in dellingr.

### E16. Assorted minor divergences (all CONFIRMED at the code level, cosmetic-to-small)

- `math.random(m, n)` with `m > n` reports "bad argument #1"; reference says `#2`.
- `math.log(x, base)` is always `ln/ln`; 5.4 special-cases base 2/10 (`log(8,2)` is exactly
  3.0 there, ~3.0000000000000004-class noise here). Covered in spirit by the README's
  transcendental caveat, but this one is *within* dellingr's control.
- String library rejects number arguments where reference coerces (`string.len(42)`,
  `("x"):sub` receivers are fine, but `string.sub(123, 1)` etc. error). One systematic
  decision should be recorded either way.
- `error()` ignores the level argument and adds no position prefix (arguably deliberate
  without pcall; noting for completeness).
- `string.format("%u")` accepted; Lua 5.4 removed `%u`. Harmless leniency, but the module
  claims the 5.4 contract - decide deliberately.
- `string.format("%p")` prints `(null)` for non-collectable values; glibc-based reference
  prints `(nil)`. `%p` output is inherently divergent (deterministic ids vs addresses), so
  only relevant to diff-test cosmetics.
- `_G` proxy stringifies keys (`_G[1]` aliases `_G["1"]`; non-string keys go through
  `to_string`), unlike reference's real table. Inherent to the proxy design; recording so
  it is a decision, not an accident.

### Verified non-findings (so the next auditor doesn't re-derive them)

- `VmRng::random_range_i64` uses i128; no overflow from `math.random` extremes.
- RustFunc returning exactly 255 results does not collide with the `RetCount::All` sentinel
  (`vm/eval.rs` treats the callee count numerically).
- gmatch's leading-`^` escaping matches reference (gmatch treats `^` literally).
- gsub's empty-match advancement, anchored gsub, `$`/`^` anchors at `init`, `[]]`/`[^]]`
  class closing, `%b((` validation, `()` position-capture positions with `init` bases: all
  line up with reference.
- string_format: directive length/width caps, flag validation per conversion, `#o`/`#x`
  prefix edge cases, `%g` significant-digit selection and zero-stripping, zero-padding after
  `0x` for `%a`, `%q` quoting table (incl. 3-digit escapes before digits), `%c` mod-256,
  integer-representation errors - all consistent with the 5.4 contract as far as static
  reading can tell. Rust's round-half-even float formatting matches glibc's.

---

## D. Optimization opportunities (structural first)

### O1. Full coherent rewrite of `src/patterns/luapat.rs` (recommended; fixes E1/E3/E4/E7/E14 structurally)

The module is a raw-pointer transliteration of C with three recently-patched bug clusters
and four more found above. A rewrite buys correctness and speed at once:

- Safe index arithmetic (`usize` offsets into `&[u8]`) instead of `CPtr`; eliminates the
  `unsafe` derefs and the empty-pattern UB class entirely. With slices + indices LLVM
  bounds-checks mostly fold; the C-shaped pointer code is not load-bearing for perf.
- Loop-based tail transitions (the C `goto init`) instead of Rust tail calls: fixes the
  matchdepth semantics (E1) *and* removes real call overhead per pattern item.
- `init` offset parameter (reference `prepstate` shape): subject is passed once, whole;
  fixes `%f` (E7), deletes the `base` arithmetic and the slice re-derivations in string.rs.
- Pre-compiled pattern: one pass producing an item list (item kind, class range in the
  pattern, suffix, precomputed `classend`). Today `classend` re-scans the class bytes for
  every item at every subject position tried by `str_match`'s scan loop - O(pattern) per
  position. Precompiling makes the scan loop O(1) per item and folds the validator
  (`str_check`) and the compiler into one pass with one authoritative capture count
  (fixing the E3/E4 validator/runtime skew by construction).
- A cost hook (charge per K match steps into `State`) lands naturally here (E13).

### O2. gmatch: drop the Lua-side wrapper and table-backed iterator state

Current per-iteration cost (`src/lua_std/string.rs:11-26, 431-471, 688-732`): one Lua call
into the compiled wrapper chunk + one RustFn call, three `get_table` lookups ("s", "p",
"pos") + one `set_table_raw`, two full `to_vec` copies of subject and pattern, and a full
pattern re-validation (`from_bytes_try`) - every iteration. A generic-for iterator already
receives `(state, control)`, so a RustFunc can be the iterator directly; hold the iterator
state (subject Val, compiled pattern, pos) in a Rust-side object (heap object variant or
registry anchor) instead of a Lua table of strings. This is an order-of-magnitude reduction
in per-iteration work for `for w in s:gmatch(...)` loops. The wrapper/table design mainly
serves the snapshot feature; keep a serializable representation for that (the state is just
(string, string, number)).

### O3. gsub: precompile the replacement template

`append_gsub_replacement` re-fetches and re-copies the replacement bytes
(`to_bytes_coerce(3)?.into_owned()`) and re-parses `%N` escapes for every match
(string.rs:139-214). Parse the template once per gsub call into
`Vec<Segment{Literal(range) | Capture(n)}>`; per-match work becomes pure byte appends.

### O4. Plain find/gsub: replace the naive subslice scan

`find_subslice` (string.rs:37-45) is `windows().position` - O(n*m) with no skip loop. A
memchr-based first-byte skip (or full two-way/memmem) is deterministic and typically
several-fold faster on long subjects; it serves `string.find` plain path and the plain-gsub
loop.

### O5. Stop copying subject/pattern per call

`find`/`match`/`gsub`/`gmatch` each do `to_bytes(...)?.to_vec()` of subject and pattern.
With O1's init-offset API, matching can borrow the interned bytes directly: the subject and
pattern Vals sit at stack indices 1-2 for the whole call, so they are GC roots; only
replacement paths that can run Lua (function/table repl in gsub) genuinely need an owned
copy (Lua code can trigger GC/interning reallocation - verify StringPool stability before
borrowing across allocation; otherwise copy only in those two modes).

### Micro (worth batching into any touch of these files)

- `string.format`: format into one output buffer instead of per-directive `Vec` round-trips.
- `table.pack`: `for _ in 0..num_args { state.remove(1) }` is O(n^2) stack shuffling; a
  single rotate/truncate does it.
- `is_plain_lua_pattern` treats `-` as magic even though a `-` with no preceding class item
  at pattern start is literal; conservative is fine, just noting the fast path misses
  hyphenated plain needles like `"foo-bar"`.

---

## E. For the orchestrator: quick verification list

1. E10 assumes `check_type(k, T)` errors on nil (very high confidence from `ArgError`
   plumbing, but I did not read `vm_aux.rs`).
2. `table_remove_at(1, 0)` / `(1, len+1)` semantics vs reference's `t[0]` read/write edge
   (vm-side function, out of corner).
3. The repro scripts above are diff-test ready; `E1`/`E3` should be added as regression
   examples once fixed (E3 is a panic - keep it out of `examples/` until fixed or the
   run_examples harness will abort).
4. `src/patterns/mod.rs:211` (`runtime_match_errors_are_not_swallowed`) asserts the E1
   divergence as expected behavior and will need updating with the fix.
