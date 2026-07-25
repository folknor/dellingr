# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #13, #14, #34, #35, #56: local pattern-matcher defects

Five defects in `src/patterns/`, all local rather than structural. Grouped
because they touch one file and one test surface.

**Deliberately excluded from this loop:** #12 (matchdepth consumed by tail
calls) and #33 (`%f` loses left context because the stdlib re-slices the
subject). Both need the `str_match` rework - an `init` offset plus converting
tail continuations into a loop - which is most of the `optimizations.md` #4
rewrite. Doing them here would turn five small, individually verifiable fixes
into one large one.

All four testable findings verified by running against reference Lua 5.4.

### #14 (High) - script-reachable panic on 32 captures

**Verified:**

```lua
("x"):match("()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()")
```

gives `panicked at src/patterns/luapat.rs:498:21: index out of bounds: the len
is 31 but the index is 31`. A Lua script panics the host process rather than
raising a Lua error.

- `src/patterns/mod.rs:18` sizes `matches: [LuaCapture; 32]`, and `:669`
  stores the whole match at `mm[0]` with captures going into `mm[1..]`, so only
  31 capture slots exist.
- `start_capture` (`luapat.rs:300-302`) lets `level` reach exactly 32
  (`LUA_MAXCAPTURES`), and `push_captures` then writes `mm[1..][31]`.
- The validator's `()` branch (`luapat.rs:628-630`) just advances `p` without
  counting, so a pattern of 32 position captures passes `str_check`.

Fix: size the results array `LUA_MAXCAPTURES + 1`, and make the validator count
position captures. The latter also fixes #34a.

### #13 (High) - every pattern ending in `%%` is rejected

**Verified:** `("50%"):gsub("%%", " percent")` raises
`malformed pattern (ends with '%')`; reference returns `50 percent  1`. Same
for `("100%"):match("%d+%%")`, which reference answers `100%`.

`str_check` (`luapat.rs:688`) inspects only the final byte:

```rust
if at(sub(ms.p_end, 1)) == b'%' { return Err(... EndsWithPercent) }
```

so it cannot distinguish a dangling `%` from the two-byte escape `%%`. The
runtime matcher's `classend` handles `%%` correctly - only this eager
pre-check misfires. The pre-check exists because `str_match_check`'s `L_ESC`
arm (`:534`, `let c = at(p)`) has no bounds check of its own and would read
past the end on a genuinely trailing `%`.

Fix: check trailing-percent *parity*, or add the bounds check inside the loop
and drop the pre-check entirely. The second is tidier if it does not cost
anything.

### #34 (Medium) - validator capture-count divergences

a. Position captures are not counted (`luapat.rs:628-630`), so backreference
   validation is wrong when a position capture precedes a real one.
   **Verified:** `("aa"):match("()(a)%2")` raises `invalid capture index %2`;
   reference returns `1  a`.

b. Off-by-one ceiling: the validator increments `level` then rejects
   `level >= 32` (`:623-627`), permitting only 31 captures, while
   `start_capture` (`:302`) and reference both allow exactly 32. A pattern with
   32 ordinary captures is spuriously rejected as "too many captures".

Note (a) and #14 share a root cause and must be fixed consistently: after
counting position captures, the ceiling has to be right or valid patterns start
being rejected.

### #35 (Medium) - escaped uppercase non-class letters match the wrong character

`match_class` (`luapat.rs:171-191`) lowercases the class byte before the
literal-comparison fallback:

```rust
let res = match class.to_ascii_lowercase() { ... lc => return lc == ch };
```

Reference compares the original byte in the default case (`return (cl == c)`).

**Verified, and it is fully inverted:**

| pattern | subject | dellingr | reference |
|---|---|---|---|
| `%E` | `"E"` | nil | `E` |
| `%E` | `"e"` | `e` | nil |
| `%K` | `"K"` | nil | `K` |
| `[%N]` | `"N"` | nil | `N` |

Affects `%B %E %F %H %I %J %K %M %N %O %Q %R %T %V %Y` - the uppercase letters
whose lowercase is not a class letter - both bare and inside `[...]`. Naive
"escape every character" pattern-quoting helpers emit exactly these.

### #56 (Low, hardening) - empty pattern is one call site from an OOB read

`str_check` (`luapat.rs:681`) and `str_match` (`:655`) do `at(p)` on the first
pattern byte unconditionally. With an empty pattern that reads out of bounds
through a dangling pointer. Every current stdlib call site guards empty
patterns first, so it is not live - but the invariant is implicit and
undocumented, and this module is a raw-pointer transliteration of C where that
is exactly the kind of assumption that rots.

Fix: an explicit empty guard in `LuaPattern::from_bytes_try` / `str_match`.

### Found during review, not in the original audit

**A second script-reachable panic: `%0`.** Verified:
`("aa"):match("(a)%0")` gives
`panicked at src/patterns/errors.rs:41:56: attempt to add with overflow`.
`InvalidCaptureIndex(Some(-1))` is cast to `usize` before adding one. Release
happens to wrap and print `%0`, so this is debug-only in effect but wrong in
both.

**`%s` does not match vertical tab.** Verified against reference:

| expression | dellingr | reference 5.4 |
|---|---|---|
| `("\11"):match("%s")` | nil | matches |
| `("\11"):match("%S")` | matches | nil |
| `("\12"):match("%s")` | matches | matches |

`match_class` uses Rust's `is_ascii_whitespace`, which excludes 0x0B; C's
`isspace` includes it. Form feed (0x0C) is in both, so VT is the only
divergent byte. This is the same root cause as finding #42 (the lexer's
`consume_whitespace`), and `src/compiler/numeral.rs`'s `is_lua_whitespace`
already has the correct set.

Note the review initially concluded the remaining predicates agreed with
reference; that was checked and is wrong for `%s`/`%S` specifically.

---

## Agreed implementation plan

### Capture ceiling - get this exactly right

`LUA_MAXCAPTURES` stays **32**. It counts entries in `MatchState.capture`, and
both ordinary and position captures consume one. Reference's `start_capture`
rejects only when `level >= 32`, so capture 32 is valid and 33 is rejected.

dellingr's wrapper additionally stores the whole match at `matches[0]` with
explicit captures in `matches[1..=32]`, so it needs **33 slots**:

```text
LUA_MAXCAPTURES = 32
LUA_MAXMATCHES  = LUA_MAXCAPTURES + 1
```

Counting must use the same pre-increment rule as the runtime: reject when
`level >= LUA_MAXCAPTURES`, otherwise write capture `level` and increment.
**Never increment then test `>=`** - that is #34b.

Make `str_match` take `&mut [LuaCapture; LUA_MAXMATCHES]` so the capacity is
compile-time enforced rather than a comment.

### `src/patterns/luapat.rs`

- `match_class`: compare the **original** `class` byte in the default arm
  (`_ => return class == ch`), fixing #35.
- `match_class`: `b's'` must include vertical tab. Use the same set as
  `numeral.rs`'s `is_lua_whitespace` rather than `is_ascii_whitespace`.
- `str_match_check`:
  - bounds-check the `L_ESC` byte before dereferencing (this is what lets the
    eager pre-check go);
  - count **both** position and ordinary captures;
  - check capacity before indexing `capture` or `level_stack`;
  - allow the 31 -> 32 transition;
  - mark position captures `Position` immediately;
  - mark closed ordinary captures `Len(0)` - currently mis-tagged `Position`
    at `:637`, behaviour-neutral today because validation only tests
    "unfinished", but wrong;
  - return `InvalidPatternCapture` for an unmatched `)`, matching reference's
    "invalid pattern capture" rather than "no open capture".
- `str_check`: delete the final-byte `%` pre-check (#13). It only bought an
  O(1) rejection, masked the unsafe validator dereference, and gave worse
  diagnostics for things like an unterminated bracket ending in `%`. Runtime
  `classend` already does its own dangling-escape check, and the new bounds
  check runs during one-time validation, not matching.
- `str_check` / `str_match`: explicit empty-pattern handling (#56). An empty
  pattern matches `0..0` and returns one internal match.
- `match_capture`: `CapLen::Position` must yield `Ok(null())` - no match -
  rather than `NoCaptureLength`. Reference accepts `()%1` as a valid pattern
  that simply does not match, and fixing #34a would otherwise turn it into an
  error.
- `capture_to_close`: cast before subtracting (`self.level as isize - 1`).
  Validation currently prevents level zero reaching it, but the subtraction
  order is a latent panic.
- Drop `CapLen::size` once unused.

### `src/patterns/mod.rs`

Size `LuaPattern.matches` and its initializer with `LUA_MAXMATCHES`. **Do not**
touch `push_captures` in `lua_std/string.rs` - its `n - 1` correctly keeps the
whole-match bookkeeping slot internal.

### `src/patterns/errors.rs`

Format capture indices in signed space (`i16::from(idx) + 1`), fixing the `%0`
overflow panic. Remove `NoCaptureLength` and `NoOpenCapture` once their
replacements are in place.

### Out of scope

The validator has no counterpart in `lstrlib.c` - it is an eager dellingr
layer, and it will keep rejecting malformed suffixes that reference might never
execute (`"a%"` against `"b"`). Do not broaden this into removing it. #12 and
#33 stay out.

### Tests

`src/patterns/mod.rs` unit tests:

- `%%`, `%d+%%`, `%%%%`, while keeping the dangling-`%` rejection;
- `()(a)%2` returning `Position(0)` and `"a"`;
- exactly 32 position captures, exactly 32 ordinary captures, and a mixed 32;
- 33 of each returning `TooManyCaptures` **without indexing arrays**;
- every affected uppercase byte in `BEFHIJKMNOQRTVY`, bare and bracketed;
- vertical tab against `%s` and `%S`, plus form feed as the control;
- empty pattern matching `0..0` on empty and non-empty subjects;
- `()%1` returning no match rather than an error;
- `%0` formatting exactly as `invalid capture index %0`.

`tests/string_bytes.rs`: public-API regressions calling `string.match` with 32
position captures and 32 ordinary captures, verifying all 32 Lua results. This
is the host-panic regression that matters most.

`tests/error_handling.rs`: `%0` and unmatched-`)` producing clean
`RuntimeError`s with reference-compatible messages.

Extend `examples/pattern_result_handling.lua` with #13, #34a, #35, the VT case,
and the 32-capture boundary, so the differential gate covers them against both
Lua 5.2 and 5.4 automatically.

### Regression to watch

`src/patterns/mod.rs:211` (`runtime_match_errors_are_not_swallowed`, 201
literal `a`s) enshrines the #12 divergence as expected. Its pattern is
non-empty, capture-free and escape-free, so nothing here should touch it - but
run it explicitly and flag it if the outcome changes.
